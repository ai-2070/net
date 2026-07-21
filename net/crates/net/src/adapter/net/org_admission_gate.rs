//! OA2-E1 §2.4a — the cortex/mesh admission gate glue.
//!
//! The behavior-layer admission engine
//! ([`org_admission`](super::behavior::org_admission)) verifies a
//! decoded [`OrgCallProof`]
//! against the provider's own facts, deliberately WITHOUT importing
//! the cortex RPC payload types. This module is the thin bridge the
//! mesh gate uses: it computes the canonical request digest the proof
//! binds, from the cortex [`RpcRequestPayload`], so the same digest
//! function is shared by the provider gate and (in E2) the caller's
//! proof-intent builder. A divergence between the two would fail
//! every legitimate call CLOSED — safe, and caught by the admit
//! witness.

use std::sync::Arc;

use super::behavior::admission_clock::ClockSample;
use super::behavior::org::OrgId;
use super::behavior::org_admission::{AdmissionDenied, OrgAdmission};
use super::behavior::org_authority::NodeAuthority;
use super::behavior::org_call::{OrgCallProof, ORG_ADMISSION_HEADER};
use super::behavior::org_revocation::{OrgRevocationState, OrgRevocationStore};
use super::cortex::{RpcCodecError, RpcHeader, RpcRequestPayload};
use super::identity::EntityId;
use super::mesh::MeshNode;

/// blake3 `derive_key` context for the canonical org-RPC request
/// digest (E1.7). Distinct, versioned domain string so a future wire
/// change gets a new context and cannot collide with an old digest.
pub const ORG_RPC_REQUEST_DIGEST_CONTEXT: &str = "net-org-rpc-request-v1";

/// The canonical request digest an [`OrgCallProof`] binds (§2.4 call
/// binding). One shared definition (verdict §8) — never a second
/// hand-written concatenation codec:
///
/// 1. drop EVERY exact `net-org-admission` header (the proof itself
///    rides one of these; a request must not bind the proof carrying
///    it, and a provider strips them all before hashing);
/// 2. PRESERVE the relative order of every remaining header;
/// 3. re-encode with [`RpcRequestPayload`]'s existing canonical wire
///    encoder — this binds service, deadline, flags, every remaining
///    header (in order, with multiplicity), and the body length +
///    bytes automatically;
/// 4. `blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, encoded)`.
///
/// Header ORDER is bound, NOT canonicalized away (Kyra E1 audit): the
/// application receives the original ordered `Vec<RpcHeader>` and
/// existing parsers are order-sensitive (trace extraction is
/// last-duplicate-wins; stream-window parsing is first-duplicate-wins),
/// so the proof must bind the exact sequence the handler interprets.
/// Sorting here would let `[("x","allow"),("x","deny")]` and its
/// reverse sign the same digest while delivering different meaning.
///
/// Both the provider (verifying `ctx.request_digest`) and the caller
/// (E2, minting the proof) call THIS function over the SAME finalized
/// request, so a mismatch is impossible for a well-formed call and a
/// tampered body/header set/order fails the binding.
pub fn org_request_digest(req: &RpcRequestPayload) -> Result<[u8; 32], RpcCodecError> {
    // R2-1 (Kyra addendum): validate the FINALIZED request — the exact
    // bytes the caller signs and the provider decodes, proof headers
    // INCLUDED — before stripping. Stripping first would let a
    // structurally invalid finalized request (33 exact proof headers
    // over `MAX_RPC_HEADERS`, or an oversized proof-header value) reduce
    // to a valid canonical after the strip and hash `Ok`, so the proof
    // would bind a request no honest party could have put on the wire.
    req.validate_wire_bounds()?;
    // Strip the admission headers ONLY; the relative order of every
    // other header is preserved exactly as the application will see it.
    let headers: Vec<RpcHeader> = req
        .headers
        .iter()
        .filter(|(name, _)| name != ORG_ADMISSION_HEADER)
        .cloned()
        .collect();

    let canonical = RpcRequestPayload {
        service: req.service.clone(),
        deadline_ns: req.deadline_ns,
        flags: req.flags,
        headers,
        // `Bytes` clone is a refcount bump, not a copy.
        body: req.body.clone(),
    };
    // AV-7 item 7: refuse an over-cap request rather than hash a
    // release-truncated, ambiguous encoding. encode_into's length
    // prefixes are `as u8`/`as u16`/`as u32` casts guarded only by
    // debug_assert, so an oversized publicly-constructed request could
    // otherwise round-trip to a colliding digest.
    canonical.validate_wire_bounds()?;
    let mut encoded = Vec::with_capacity(canonical.encoded_len());
    canonical.encode_into(&mut encoded);
    Ok(blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, &encoded))
}

/// A cheap fingerprint of the provider's admission-relevant security
/// view (E1.4 §9.5): which node authority + revocation store are
/// installed, the store's floor-publish generation, and whether the
/// store is poisoned right now.
///
/// The gate captures one stamp BEFORE running
/// [`verify_org_admission`](super::behavior::org_admission::verify_org_admission)
/// and recomputes it inside the engine's §9.5 hook. A mismatch — a
/// floor raised (generation bumped), the authority was swapped
/// (`authority_ptr` changed), or the store was poisoned — means the
/// floor snapshot the proof was verified against is no longer live,
/// so the stale decision is denied `AuthorityChanged` BEFORE it can
/// consume a replay slot or run the handler. The comparison is
/// distinct from the OA-1 send seqlock (which stamps the announce
/// path) though structurally analogous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionStamp {
    /// `Arc::as_ptr` of the installed `NodeAuthority` (0 = none).
    pub authority_ptr: usize,
    /// `Arc::as_ptr` of the installed `OrgRevocationStore` (0 = none).
    pub store_ptr: usize,
    /// The store's floor-publish generation — bumps on every floor
    /// publish (under the reload lock), so a floor raise changes the
    /// stamp even when the same store `Arc` stays installed.
    pub store_generation: u64,
    /// Whether the active store is poisoned as of this capture.
    pub poisoned: bool,
}

impl AdmissionStamp {
    /// `true` iff `self` (captured before verification) still equals
    /// `current` (recomputed at §9.5) AND the store is not poisoned —
    /// i.e. the security view the proof was verified against is still
    /// live. Any change, or a now-poisoned store, is a stale view.
    pub fn is_current(&self, current: &AdmissionStamp) -> bool {
        self == current && !current.poisoned
    }
}

/// Recompute the provider's admission stamp against LIVE state
/// (E1.4 §9.5). A single read of each field — used only to detect
/// CHANGE relative to a previously-captured stamp, so it does not
/// need the consistent floors/generation pairing that
/// [`verify_provider_authority`] performs. `0` pointers mean the
/// authority / store is no longer installed.
pub fn capture_admission_stamp(mesh: &MeshNode) -> AdmissionStamp {
    let authority = mesh.node_authority();
    let store = mesh.org_revocation_store();
    let authority_ptr = authority
        .as_ref()
        .map_or(0, |a| Arc::as_ptr(a) as *const () as usize);
    let (store_ptr, store_generation, poisoned) = store.as_ref().map_or((0, 0, false), |s| {
        (
            Arc::as_ptr(s) as *const () as usize,
            // Publication-barriered (Kyra E1 review): a bare
            // `publish_generation()` could read an old generation
            // while a floor publish holds `live.write()` mid-swap, so
            // a stale stamp would compare "unchanged". The barriered
            // read serializes against the swap.
            s.barriered_generation(),
            s.is_poisoned(),
        )
    });
    AdmissionStamp {
        authority_ptr,
        store_ptr,
        store_generation,
        poisoned,
    }
}

/// The provider's own facts for one admission, captured at CALL time
/// (E1.3). Registration-time authority is not usable authority: a
/// provider whose owner cert has since expired, whose store is
/// poisoned, or whose authority was uninstalled cannot admit.
pub struct ProviderFacts {
    /// This provider's entity (P).
    pub provider: EntityId,
    /// This provider's PROVEN owner org (B) — from the installed
    /// authority scaffold, never fold state.
    pub provider_owner_org: OrgId,
    /// The clock-skew tolerance persisted in the provider's authority
    /// config — the SAME tolerance the ceremony accepted, applied to
    /// every wall-clock check in the admission (caller credentials +
    /// proof freshness), read from the authority verified above.
    pub skew_secs: u64,
    /// The floor snapshot the admission is verified against, paired
    /// with the generation recorded in `stamp`.
    pub floors: Arc<OrgRevocationState>,
    /// The security-view fingerprint (§9.5) matching `floors`. The
    /// gate re-checks this against [`capture_admission_stamp`] after
    /// verification and before the replay insert.
    pub stamp: AdmissionStamp,
    /// The installed authority Arc, PINNED for the admission's
    /// lifetime (Kyra E1 audit — ABA). `stamp.authority_ptr` is a raw
    /// address; without retaining the Arc, a replace/drop/realloc
    /// cycle could reuse that address for a DIFFERENT authority and
    /// make the §9.5 pointer comparison false-match "unchanged".
    /// Holding the Arc keeps the address occupied until the stability
    /// check completes.
    _authority: Arc<NodeAuthority>,
    /// The installed store Arc, pinned for the same ABA reason.
    _store: Arc<OrgRevocationStore>,
}

/// Live provider self-verification (E1.3, verdict §5). For every
/// protected admission, as a call-time prerequisite:
///
/// - a node authority AND a revocation store must be installed;
/// - the store must not be poisoned;
/// - the authority's owner cert must pass `self_verify` against the
///   CURRENT floor snapshot (binds this node, temporally valid, its
///   generation at/above the floor).
///
/// Any failure is [`AdmissionDenied::ProviderAuthorityUnavailable`]
/// and the handler stays dark. On success returns the four provider
/// facts the admission engine needs plus the security stamp matching
/// the captured floors.
pub fn verify_provider_authority(
    mesh: &MeshNode,
    clock: &ClockSample,
) -> Result<ProviderFacts, AdmissionDenied> {
    let authority = mesh
        .node_authority()
        .ok_or(AdmissionDenied::ProviderAuthorityUnavailable)?;
    let store = mesh
        .org_revocation_store()
        .ok_or(AdmissionDenied::ProviderAuthorityUnavailable)?;
    if store.is_poisoned() {
        return Err(AdmissionDenied::ProviderAuthorityUnavailable);
    }
    // Publication-barriered floors + generation (Kyra E1 review): both
    // read under one `live.read()`, so the pair is consistent and the
    // generation cannot lag an in-progress floor swap.
    let (floors, store_generation) = store.snapshot_with_generation();
    let provider = mesh.entity_id().clone();
    // Live self-verify against the ONE admission ClockSample (AV-6
    // item 6): an expired / below-floor / foreign-bound owner cert
    // fails here even though it verified at registration, and it reads
    // the SAME instant the caller-credential checks will, so no
    // wall-clock step can open a window between provider and caller
    // verification.
    authority
        .config
        .self_verify_at(&provider, &floors, clock.wall_secs())
        .map_err(|_| AdmissionDenied::ProviderAuthorityUnavailable)?;
    // A poison could have raced in after the check above; deny rather
    // than admit against a durability-uncertain store.
    if store.is_poisoned() {
        return Err(AdmissionDenied::ProviderAuthorityUnavailable);
    }
    let stamp = AdmissionStamp {
        authority_ptr: Arc::as_ptr(&authority) as *const () as usize,
        store_ptr: Arc::as_ptr(&store) as *const () as usize,
        store_generation,
        // Verified non-poisoned above; a poison arriving after this
        // point is caught by the §9.5 recheck via the live stamp.
        poisoned: false,
    };
    let provider_owner_org = authority.owner_org();
    let skew_secs = authority.config.verification_skew_secs;
    Ok(ProviderFacts {
        provider,
        provider_owner_org,
        skew_secs,
        floors,
        stamp,
        // Pin the exact Arcs the stamp fingerprints, so their
        // addresses cannot be reused under a §9.5 recheck (ABA).
        _authority: authority,
        _store: store,
    })
}

/// The visibility of a registered capability (E1.1). Emission projects by
/// visibility (Kyra OA3 ruling): `Public` → plaintext CAP-ANN; the two scoped
/// forms → an encrypted `ScopedCapabilityAnnouncement` ONLY, never plaintext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityVisibility {
    /// Announced in the clear, discoverable by anyone (v0.4 default).
    Public,
    /// An internal private capability of the node's OWN org. Emitted ONLY as an
    /// encrypted `ScopedCapabilityAnnouncement` under the owner audience (the
    /// reserved zero `grant_id` sentinel) — never in a plaintext CAP-ANN.
    /// Invocation still requires [`OrgAdmission::OwnerDelegated`]; the capability
    /// enters the local self-fold for `has_local_capability` but never the wire
    /// in the clear (OA3-4b1).
    OwnerScoped,
    /// A cross-org private capability. Emitted ONLY as an encrypted
    /// `ScopedCapabilityAnnouncement` under a grant audience — never plaintext —
    /// one envelope per active provider grant record (OA3-4b2). Invocation still
    /// requires [`OrgAdmission::CrossOrgGranted`] admission; the capability enters
    /// the local self-fold for `has_local_capability` but never the wire in the
    /// clear. A granted service registered BEFORE a matching grant is installed is
    /// locally dispatchable but undiscoverable (fail-closed) until installing the
    /// grant wakes a coherent reannouncement.
    GrantedAudience,
}

/// The provider-local application veto (E1.1/E1.6, verdict §7). Runs
/// LAST in the admission order, seeing only the VERIFIED proof, and
/// returns `true` to admit. Legacy public services carry a trivial
/// `|_| true`; a protected registration supplies a real one.
pub type OrgProviderPolicy = Arc<dyn Fn(&OrgCallProof) -> bool + Send + Sync>;

/// The immutable registration record captured by a serve bridge
/// (E1.1, verdict §1/§2/§7/§12). ONE truth per registration — the
/// policy is captured WITH the handler, not looked up in a separate
/// name→policy map, so there is never an unknown-policy fallback.
///
/// Fields are PRIVATE (Kyra E1 audit): a registration is built only
/// through the validated [`Self::public`] / [`Self::protected`]
/// constructors, which structurally enforce the legal shapes, and is
/// read-only thereafter through the accessors.
#[derive(Clone)]
pub struct RegisteredRpcService {
    registration_id: u64,
    service: Arc<str>,
    visibility: CapabilityVisibility,
    admission: OrgAdmission,
    provider_policy: OrgProviderPolicy,
    /// Test-only (review-7 RED negative control): when set, the protected
    /// dispatch bypasses ONLY `verify_org_admission` for this registration. Never
    /// selectable from a production constructor; compiled out entirely without
    /// `cfg(test)`, so a shipping build cannot carry the bypass.
    #[cfg(test)]
    red_witness_disabled: bool,
}

/// An invalid [`RegisteredRpcService`] construction (Kyra E1 audit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegisteredServiceError {
    /// [`RegisteredRpcService::protected`] was given
    /// [`OrgAdmission::PublicAuthenticated`] — a protected service
    /// must carry an org-protected admission mode.
    #[error(
        "protected registration requires an org-protected admission mode, not PublicAuthenticated"
    )]
    PublicAdmissionNotProtected,
}

impl RegisteredRpcService {
    /// A legacy PUBLIC registration: `Public` visibility,
    /// [`OrgAdmission::PublicAuthenticated`], and a trivial allow-all
    /// policy — so every live public handler still carries a policy
    /// (never an unknown-policy fallback).
    pub fn public(registration_id: u64, service: Arc<str>) -> Self {
        Self {
            registration_id,
            service,
            visibility: CapabilityVisibility::Public,
            admission: OrgAdmission::PublicAuthenticated,
            provider_policy: Arc::new(|_| true),
            #[cfg(test)]
            red_witness_disabled: false,
        }
    }

    /// A PROTECTED registration: `Public` visibility (E1 — OA-3 adds
    /// the rest), an org-protected `admission` mode, and an EXPLICIT
    /// `provider_policy`. Rejects [`OrgAdmission::PublicAuthenticated`]
    /// structurally — a protected service cannot be built with the
    /// public mode.
    pub fn protected(
        registration_id: u64,
        service: Arc<str>,
        admission: OrgAdmission,
        provider_policy: OrgProviderPolicy,
    ) -> Result<Self, RegisteredServiceError> {
        if matches!(admission, OrgAdmission::PublicAuthenticated) {
            return Err(RegisteredServiceError::PublicAdmissionNotProtected);
        }
        Ok(Self {
            registration_id,
            service,
            visibility: CapabilityVisibility::Public,
            admission,
            provider_policy,
            #[cfg(test)]
            red_witness_disabled: false,
        })
    }

    /// An OWNER-SCOPED registration (OA3-4b1): `OwnerScoped` visibility (emitted
    /// only as an encrypted owner-audience announcement, never plaintext),
    /// [`OrgAdmission::OwnerDelegated`] (internal invocation authority), and an
    /// EXPLICIT `provider_policy`. The service enters the local self-fold so
    /// `has_local_capability` admits it, but its tag never rides a plaintext
    /// broadcast.
    pub fn owner_scoped(
        registration_id: u64,
        service: Arc<str>,
        provider_policy: OrgProviderPolicy,
    ) -> Self {
        Self {
            registration_id,
            service,
            visibility: CapabilityVisibility::OwnerScoped,
            admission: OrgAdmission::OwnerDelegated,
            provider_policy,
            #[cfg(test)]
            red_witness_disabled: false,
        }
    }

    /// A GRANTED-AUDIENCE registration (OA3-4b2): a cross-org private capability.
    /// `GrantedAudience` visibility (emitted only as an encrypted grant-audience
    /// announcement, never plaintext), [`OrgAdmission::CrossOrgGranted`] invocation
    /// authority (identical invoke gate to a protected cross-org service — the two
    /// differ ONLY in discovery visibility), and an EXPLICIT `provider_policy`. The
    /// service enters the local self-fold so `has_local_capability` admits it, but
    /// its tag never rides a plaintext broadcast; it becomes discoverable only once
    /// a matching provider grant is installed.
    pub fn granted(
        registration_id: u64,
        service: Arc<str>,
        provider_policy: OrgProviderPolicy,
    ) -> Self {
        Self {
            registration_id,
            service,
            visibility: CapabilityVisibility::GrantedAudience,
            admission: OrgAdmission::CrossOrgGranted,
            provider_policy,
            #[cfg(test)]
            red_witness_disabled: false,
        }
    }

    /// Test-only (review-7 RED negative control): `true` iff this registration
    /// was marked to bypass ONLY `verify_org_admission`. Compiled out of
    /// production — a shipping build has no way to observe or set the flag.
    #[cfg(test)]
    pub(crate) fn red_witness_admission_disabled(&self) -> bool {
        self.red_witness_disabled
    }

    /// Test-only (review-7 RED negative control): mark this registration so the
    /// protected dispatch bypasses ONLY the org-admission engine. Reachable only
    /// from the `#[cfg(test)]` serve seam, never a production constructor.
    #[cfg(test)]
    pub(crate) fn with_red_witness_disabled(mut self) -> Self {
        self.red_witness_disabled = true;
        self
    }

    /// The generation token (E0.1) this registration owns.
    pub fn registration_id(&self) -> u64 {
        self.registration_id
    }

    /// The service name this registration serves.
    pub fn service(&self) -> &Arc<str> {
        &self.service
    }

    /// The announcement visibility — `Public` for legacy/protected services,
    /// `OwnerScoped` for an owner-scoped registration (OA3-4b1), `GrantedAudience`
    /// for a granted-audience registration (OA3-4b2).
    pub fn visibility(&self) -> CapabilityVisibility {
        self.visibility
    }

    /// The admission mode bound at registration.
    pub fn admission(&self) -> OrgAdmission {
        self.admission
    }

    /// The application veto (step 11). `|_| true` for public.
    pub fn provider_policy(&self) -> &OrgProviderPolicy {
        &self.provider_policy
    }
}

impl std::fmt::Debug for RegisteredRpcService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredRpcService")
            .field("registration_id", &self.registration_id)
            .field("service", &self.service)
            .field("visibility", &self.visibility)
            .field("admission", &self.admission)
            .field("provider_policy", &"<fn>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn req(headers: Vec<RpcHeader>, body: &[u8]) -> RpcRequestPayload {
        RpcRequestPayload {
            service: "oa2-echo".to_string(),
            deadline_ns: 1_700_000_000_000_000_000,
            flags: 0,
            headers,
            body: Bytes::copy_from_slice(body),
        }
    }

    fn h(name: &str, value: &[u8]) -> RpcHeader {
        (name.to_string(), value.to_vec())
    }

    /// Well-formed fixtures always encode; unwrap for the equality
    /// tests. The fallible surface (AV-7 item 7) is exercised by
    /// `digest_refuses_over_cap_requests`.
    fn digest(req: &RpcRequestPayload) -> [u8; 32] {
        org_request_digest(req).expect("well-formed fixture digests")
    }

    /// AV-7 item 7: an over-cap request is REFUSED (release-safe) rather
    /// than silently truncated into a collision-prone digest — one
    /// witness per length ceiling the codec's `u8`/`u16`/`u32` prefixes
    /// enforce. Runs in release too, where `encode_into` would truncate
    /// instead of `debug_assert`.
    #[test]
    fn digest_refuses_over_cap_requests() {
        use super::super::cortex::{
            MAX_RPC_BODY_LEN, MAX_RPC_HEADERS, MAX_RPC_HEADER_NAME_LEN, MAX_RPC_HEADER_VALUE_LEN,
            MAX_RPC_SERVICE_NAME_LEN,
        };
        let mut over_service = req(vec![], b"x");
        over_service.service = "s".repeat(MAX_RPC_SERVICE_NAME_LEN + 1);
        assert!(matches!(
            org_request_digest(&over_service),
            Err(RpcCodecError::TooLarge {
                field: "service",
                ..
            })
        ));
        let too_many = req(
            (0..=MAX_RPC_HEADERS)
                .map(|i| h(&format!("h{i}"), b"v"))
                .collect(),
            b"x",
        );
        assert!(matches!(
            org_request_digest(&too_many),
            Err(RpcCodecError::TooLarge {
                field: "headers",
                ..
            })
        ));
        let over_name = req(
            vec![h(&"n".repeat(MAX_RPC_HEADER_NAME_LEN + 1), b"v")],
            b"x",
        );
        assert!(matches!(
            org_request_digest(&over_name),
            Err(RpcCodecError::TooLarge {
                field: "header name",
                ..
            })
        ));
        let over_value = req(vec![h("k", &vec![0u8; MAX_RPC_HEADER_VALUE_LEN + 1])], b"x");
        assert!(matches!(
            org_request_digest(&over_value),
            Err(RpcCodecError::TooLarge {
                field: "header value",
                ..
            })
        ));
        let over_body = req(vec![], &vec![0u8; MAX_RPC_BODY_LEN + 1]);
        assert!(matches!(
            org_request_digest(&over_body),
            Err(RpcCodecError::TooLarge { field: "body", .. })
        ));
        // A well-formed request at the ceiling still digests fine.
        assert!(org_request_digest(&req(vec![h("k", b"v")], b"body")).is_ok());
    }

    /// R2-1 (Kyra addendum): the FINALIZED request — proof headers
    /// INCLUDED — is validated BEFORE stripping. A finalized request
    /// that is structurally invalid ONLY in its proof headers (which
    /// the digest strips) must still be refused, not reduce to a valid
    /// canonical and hash `Ok`.
    #[test]
    fn digest_refuses_over_cap_finalized_requests() {
        use super::super::cortex::{MAX_RPC_HEADERS, MAX_RPC_HEADER_VALUE_LEN};
        // 33 exact proof headers (> MAX_RPC_HEADERS) — every one is
        // stripped, so the canonical would be empty and pass; the
        // finalized request must be refused first.
        let many_proof = req(
            (0..=MAX_RPC_HEADERS)
                .map(|_| h(ORG_ADMISSION_HEADER, b"p"))
                .collect(),
            b"x",
        );
        assert!(matches!(
            org_request_digest(&many_proof),
            Err(RpcCodecError::TooLarge {
                field: "headers",
                ..
            })
        ));
        // An oversized proof-header VALUE — stripped from the canonical,
        // so only finalized validation catches it.
        let big_proof = req(
            vec![h(
                ORG_ADMISSION_HEADER,
                &vec![0u8; MAX_RPC_HEADER_VALUE_LEN + 1],
            )],
            b"x",
        );
        assert!(matches!(
            org_request_digest(&big_proof),
            Err(RpcCodecError::TooLarge {
                field: "header value",
                ..
            })
        ));
        // A single proof header at the ceiling still digests fine.
        assert!(org_request_digest(&req(vec![h(ORG_ADMISSION_HEADER, b"ok")], b"body")).is_ok());
    }

    /// Header ORDER is BOUND (Kyra E1 audit): two requests with the
    /// same duplicate header in opposite orders — which order-sensitive
    /// parsers interpret differently — must sign DIFFERENT digests.
    #[test]
    fn digest_binds_header_order() {
        let allow_then_deny = req(vec![h("x", b"allow"), h("x", b"deny")], b"body");
        let deny_then_allow = req(vec![h("x", b"deny"), h("x", b"allow")], b"body");
        assert_ne!(
            digest(&allow_then_deny),
            digest(&deny_then_allow),
            "reversed duplicate headers must not collide",
        );
        // A plain reordering of distinct headers also changes it.
        let abc = req(vec![h("a", b"1"), h("b", b"2"), h("c", b"3")], b"body");
        let bac = req(vec![h("b", b"2"), h("a", b"1"), h("c", b"3")], b"body");
        assert_ne!(digest(&abc), digest(&bac));
    }

    /// The admission header is stripped before hashing — so the proof
    /// (which rides that header) never binds itself, and adding /
    /// removing it leaves the digest unchanged WHILE the relative
    /// order of the surrounding headers is preserved.
    #[test]
    fn digest_ignores_admission_header_and_preserves_surrounding_order() {
        let bare = req(vec![h("x", b"1"), h("y", b"2")], b"body");
        // Proof header interleaved between x and y: stripping it must
        // leave x-before-y intact, matching `bare`.
        let with_proof = req(
            vec![
                h("x", b"1"),
                h(ORG_ADMISSION_HEADER, b"opaque-proof-bytes"),
                h("y", b"2"),
            ],
            b"body",
        );
        assert_eq!(digest(&bare), digest(&with_proof));

        // Even MULTIPLE admission headers are all stripped, order kept.
        let with_two = req(
            vec![
                h(ORG_ADMISSION_HEADER, b"p1"),
                h("x", b"1"),
                h(ORG_ADMISSION_HEADER, b"p2"),
                h("y", b"2"),
            ],
            b"body",
        );
        assert_eq!(digest(&bare), digest(&with_two));
    }

    /// Duplicate non-admission headers ARE bound — dropping one
    /// changes the digest (multiplicity matters).
    #[test]
    fn digest_binds_header_multiplicity() {
        let one = req(vec![h("x", b"1")], b"body");
        let two = req(vec![h("x", b"1"), h("x", b"1")], b"body");
        assert_ne!(digest(&one), digest(&two));
    }

    /// Body, service, deadline, and flags all change the digest.
    #[test]
    fn digest_binds_request_fields() {
        let base = req(vec![], b"body");
        let base_d = digest(&base);

        assert_ne!(base_d, digest(&req(vec![], b"other")));

        let mut svc = req(vec![], b"body");
        svc.service = "different".to_string();
        assert_ne!(base_d, digest(&svc));

        let mut dl = req(vec![], b"body");
        dl.deadline_ns += 1;
        assert_ne!(base_d, digest(&dl));

        let mut fl = req(vec![], b"body");
        fl.flags = 1;
        assert_ne!(base_d, digest(&fl));
    }

    /// KC9 — RegisteredRpcService is built only through the validated
    /// constructors, which structurally enforce the legal shapes.
    #[test]
    fn registered_service_constructors_enforce_shape() {
        let svc: Arc<str> = Arc::from("oa2-echo");

        // public(): Public + PublicAuthenticated + a (non-null) policy.
        let pubreg = RegisteredRpcService::public(7, svc.clone());
        assert_eq!(pubreg.registration_id(), 7);
        assert_eq!(&**pubreg.service(), "oa2-echo");
        assert_eq!(pubreg.visibility(), CapabilityVisibility::Public);
        assert_eq!(pubreg.admission(), OrgAdmission::PublicAuthenticated);

        // protected() accepts the org-protected modes...
        for mode in [OrgAdmission::OwnerDelegated, OrgAdmission::CrossOrgGranted] {
            let reg = RegisteredRpcService::protected(9, svc.clone(), mode, Arc::new(|_| true))
                .expect("protected mode accepted");
            assert_eq!(reg.admission(), mode);
            assert_eq!(reg.visibility(), CapabilityVisibility::Public);
        }

        // ...and REFUSES PublicAuthenticated as a protected mode.
        assert_eq!(
            RegisteredRpcService::protected(
                9,
                svc.clone(),
                OrgAdmission::PublicAuthenticated,
                Arc::new(|_| true),
            )
            .err(),
            Some(RegisteredServiceError::PublicAdmissionNotProtected),
        );

        // owner_scoped() (OA3-4b1): OwnerScoped visibility + OwnerDelegated.
        let own = RegisteredRpcService::owner_scoped(11, svc.clone(), Arc::new(|_| true));
        assert_eq!(own.visibility(), CapabilityVisibility::OwnerScoped);
        assert_eq!(own.admission(), OrgAdmission::OwnerDelegated);

        // granted() (OA3-4b2): GrantedAudience visibility + CrossOrgGranted. The
        // invoke gate matches a CrossOrgGranted protected service; only the
        // discovery VISIBILITY differs (private grant-audience vs public).
        let granted = RegisteredRpcService::granted(13, svc.clone(), Arc::new(|_| true));
        assert_eq!(granted.visibility(), CapabilityVisibility::GrantedAudience);
        assert_eq!(granted.admission(), OrgAdmission::CrossOrgGranted);
    }

    /// The admission stamp is "current" only against an identical,
    /// non-poisoned stamp — any field change, or a poisoned store,
    /// reads as a stale view (E1.4 §9.5).
    #[test]
    fn admission_stamp_currency() {
        let base = AdmissionStamp {
            authority_ptr: 0x1000,
            store_ptr: 0x2000,
            store_generation: 7,
            poisoned: false,
        };
        assert!(base.is_current(&base), "identical, unpoisoned → current");

        // Floor rose (generation bumped) → stale.
        let mut gen_bumped = base;
        gen_bumped.store_generation = 8;
        assert!(!base.is_current(&gen_bumped));

        // Authority swapped → stale.
        let mut swapped = base;
        swapped.authority_ptr = 0x9999;
        assert!(!base.is_current(&swapped));

        // Store replaced → stale.
        let mut store_swapped = base;
        store_swapped.store_ptr = 0x9999;
        assert!(!base.is_current(&store_swapped));

        // Same identity but now poisoned → stale.
        let mut poisoned = base;
        poisoned.poisoned = true;
        assert!(!base.is_current(&poisoned));
    }

    /// A fixed fixture, with a multi-header + duplicate-header layout,
    /// hashing to a specific service/deadline/flags/body.
    fn golden_fixture() -> RpcRequestPayload {
        RpcRequestPayload {
            service: "oa2-echo".to_string(),
            deadline_ns: 1_700_000_000_000_000_000,
            flags: 0,
            headers: vec![
                h("content-type", b"application/json"),
                h("x-idempotency", b"k1"),
                // duplicate header, order-significant
                h("x-tag", b"a"),
                h("x-tag", b"b"),
                // admission headers must be stripped, not hashed
                h(ORG_ADMISSION_HEADER, b"opaque"),
            ],
            body: bytes::Bytes::from_static(b"hello"),
        }
    }

    /// Golden: a LITERAL, hard-coded 32-byte digest (Kyra E1 audit) —
    /// pinned so a cross-language caller (or the E2 Rust caller) must
    /// reproduce it byte-for-byte over the wire. It is NOT recomputed
    /// with this same implementation, so it actually catches an
    /// encoding / context / ordering drift. A change here is a wire
    /// break and must bump `ORG_RPC_REQUEST_DIGEST_CONTEXT`.
    #[test]
    fn digest_golden_is_literal_and_stable() {
        const GOLDEN: [u8; 32] = [
            0xce, 0x89, 0x3f, 0xa7, 0x73, 0x10, 0x92, 0x8e, 0x5b, 0xa7, 0x5d, 0x2b, 0xe3, 0x3a,
            0x66, 0xb1, 0x8c, 0x0e, 0xae, 0x77, 0x90, 0xe1, 0xaa, 0xdf, 0x52, 0x26, 0xc7, 0x62,
            0xac, 0x6f, 0x70, 0xbb,
        ];
        let got = digest(&golden_fixture());
        assert_eq!(got, GOLDEN, "wire digest drifted: {got:02x?}");
        assert_ne!(got, [0u8; 32]);
        // Reversing the duplicate x-tag headers changes the digest —
        // the golden binds their order.
        let mut reversed = golden_fixture();
        reversed.headers.swap(2, 3);
        assert_ne!(digest(&reversed), GOLDEN);
    }
}
