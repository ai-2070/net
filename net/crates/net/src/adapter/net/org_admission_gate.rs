//! OA2-E1 §2.4a — the cortex/mesh admission gate glue.
//!
//! The behavior-layer admission engine
//! ([`org_admission`](super::behavior::org_admission)) verifies a
//! decoded [`OrgCallProof`](super::behavior::org_call::OrgCallProof)
//! against the provider's own facts, deliberately WITHOUT importing
//! the cortex RPC payload types. This module is the thin bridge the
//! mesh gate uses: it computes the canonical request digest the proof
//! binds, from the cortex [`RpcRequestPayload`], so the same digest
//! function is shared by the provider gate and (in E2) the caller's
//! proof-intent builder. A divergence between the two would fail
//! every legitimate call CLOSED — safe, and caught by the admit
//! witness.

use std::sync::Arc;

use super::behavior::org::OrgId;
use super::behavior::org_admission::{AdmissionDenied, OrgAdmission};
use super::behavior::org_call::{OrgCallProof, ORG_ADMISSION_HEADER};
use super::behavior::org_revocation::OrgRevocationState;
use super::cortex::{RpcHeader, RpcRequestPayload};
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
pub fn org_request_digest(req: &RpcRequestPayload) -> [u8; 32] {
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
    let mut encoded = Vec::with_capacity(canonical.encoded_len());
    canonical.encode_into(&mut encoded);
    blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, &encoded)
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
    /// The floor snapshot the admission is verified against, paired
    /// with the generation recorded in `stamp`.
    pub floors: Arc<OrgRevocationState>,
    /// The security-view fingerprint (§9.5) matching `floors`. The
    /// gate re-checks this against [`capture_admission_stamp`] after
    /// verification and before the replay insert.
    pub stamp: AdmissionStamp,
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
pub fn verify_provider_authority(mesh: &MeshNode) -> Result<ProviderFacts, AdmissionDenied> {
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
    // Live self-verify: an expired / below-floor / foreign-bound
    // owner cert fails here even though it verified at registration.
    authority
        .config
        .self_verify(&provider, &floors)
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
    Ok(ProviderFacts {
        provider,
        provider_owner_org: authority.owner_org(),
        floors,
        stamp,
    })
}

/// The visibility of a registered capability (E1.1). E1 protected
/// registration accepts ONLY [`Self::Public`]; `OwnerScoped` /
/// `GrantedAudience` are deferred to OA-3, where the announcement
/// state machine lands, and a protected registration that requested
/// them is loudly refused until then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityVisibility {
    /// Announced in the clear, discoverable by anyone (v0.4 default).
    Public,
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
#[derive(Clone)]
pub struct RegisteredRpcService {
    /// The generation token (E0.1) this registration owns.
    pub registration_id: u64,
    /// The service name this registration serves.
    pub service: Arc<str>,
    /// Announcement visibility — MUST be `Public` in E1.
    pub visibility: CapabilityVisibility,
    /// The admission mode bound at registration.
    pub admission: OrgAdmission,
    /// The application veto (step 11). `|_| true` for public.
    pub provider_policy: OrgProviderPolicy,
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

    /// Header ORDER is BOUND (Kyra E1 audit): two requests with the
    /// same duplicate header in opposite orders — which order-sensitive
    /// parsers interpret differently — must sign DIFFERENT digests.
    #[test]
    fn digest_binds_header_order() {
        let allow_then_deny = req(vec![h("x", b"allow"), h("x", b"deny")], b"body");
        let deny_then_allow = req(vec![h("x", b"deny"), h("x", b"allow")], b"body");
        assert_ne!(
            org_request_digest(&allow_then_deny),
            org_request_digest(&deny_then_allow),
            "reversed duplicate headers must not collide",
        );
        // A plain reordering of distinct headers also changes it.
        let abc = req(vec![h("a", b"1"), h("b", b"2"), h("c", b"3")], b"body");
        let bac = req(vec![h("b", b"2"), h("a", b"1"), h("c", b"3")], b"body");
        assert_ne!(org_request_digest(&abc), org_request_digest(&bac));
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
        assert_eq!(org_request_digest(&bare), org_request_digest(&with_proof));

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
        assert_eq!(org_request_digest(&bare), org_request_digest(&with_two));
    }

    /// Duplicate non-admission headers ARE bound — dropping one
    /// changes the digest (multiplicity matters).
    #[test]
    fn digest_binds_header_multiplicity() {
        let one = req(vec![h("x", b"1")], b"body");
        let two = req(vec![h("x", b"1"), h("x", b"1")], b"body");
        assert_ne!(org_request_digest(&one), org_request_digest(&two));
    }

    /// Body, service, deadline, and flags all change the digest.
    #[test]
    fn digest_binds_request_fields() {
        let base = req(vec![], b"body");
        let base_d = org_request_digest(&base);

        assert_ne!(base_d, org_request_digest(&req(vec![], b"other")));

        let mut svc = req(vec![], b"body");
        svc.service = "different".to_string();
        assert_ne!(base_d, org_request_digest(&svc));

        let mut dl = req(vec![], b"body");
        dl.deadline_ns += 1;
        assert_ne!(base_d, org_request_digest(&dl));

        let mut fl = req(vec![], b"body");
        fl.flags = 1;
        assert_ne!(base_d, org_request_digest(&fl));
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
        let got = org_request_digest(&golden_fixture());
        assert_eq!(got, GOLDEN, "wire digest drifted: {got:02x?}");
        assert_ne!(got, [0u8; 32]);
        // Reversing the duplicate x-tag headers changes the digest —
        // the golden binds their order.
        let mut reversed = golden_fixture();
        reversed.headers.swap(2, 3);
        assert_ne!(org_request_digest(&reversed), GOLDEN);
    }
}
