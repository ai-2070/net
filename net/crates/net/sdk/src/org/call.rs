//! OSDK S1 — `org.call`: the caller verb.
//!
//! One method, no options object. The SDK owns the proof TTL, grant matching,
//! provider selection, provider pinning, the retry prohibition, the codec, and
//! the timeout; an advanced caller who needs to tune any of those already has
//! the low-level [`OrgProofIntent`](super::types::OrgProofIntent) seam.
//!
//! ```text
//! derive capability            for_tag("nrpc:<service>")
//! → private verified discovery owner plane (SameOrg) + granted planes
//! → classify by plane          same-org ⇒ no grant; cross-org ⇒ grant required
//! → exact grant matching       the complete authority relation, INVOKE
//! → deterministic selection    lowest provider EntityId
//! → canonical OrgProofIntent   all nine fields
//! → exact-target protected call  core call() pins/mints/digests/signs
//! → coarse denial decoding     0x0009 → OrgSdkError::AdmissionDenied
//! ```
//!
//! # Two caller verbs, two discovery planes
//!
//! [`OrgClient::call`] discovers ONLY the private planes — the verbs are
//! symmetric: `serve_org` emits privately, `call` discovers privately.
//!
//! [`OrgClient::call_exported`] is the public-plane counterpart
//! (SUBNET_AUTH_SDK_PLAN.md §3.6): a subnet-exported registration announces
//! plaintext, so its callers derive the authority relation from the VERIFIED
//! owner projection sampled with the candidate in one fold snapshot
//! ([`MeshNode::public_owned_service_providers`]). Everything after discovery —
//! credential currency, dispatcher scope, grant matching, deterministic
//! selection, the canonical proof, the no-retry rule — is the same shared
//! pipeline, deliberately not forked. The name is `call_exported`, not
//! `call_subnet`: the caller invokes a publicly discoverable organization
//! service and neither names nor joins a subnet; the subnet is provider-local
//! execution authority.
//!
//! [`MeshNode::public_owned_service_providers`]: net::adapter::net::MeshNode::public_owned_service_providers
//!
//! # Never a second attempt
//!
//! A signed proof is never resent and the facade never retries: the replay guard
//! is volatile and keyed on `(caller, call_id)`, so every attempt must be a
//! fresh call id and a fresh signature. Cross-call idempotency is the
//! application's.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};

use net::adapter::net::behavior::org_admission::CoarseAdmissionReason;
use net::adapter::net::behavior::org_cold_plan::{
    OrgColdAuthority, OrgColdDiscovery, OrgColdRefusal,
};
use net::adapter::net::behavior::org_scoped_store::PrivateCapabilityProvider;
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};

use super::error::{hex32, hex_capability, OrgCredentialError, OrgDiscoveryError, OrgSdkError};
use super::types::{CapabilityAuthorityId, OrgCapabilityGrant, OrgProofIntent};
use super::OrgClient;
use crate::mesh_rpc::Codec;

/// The wire status a provider's admission denial carries (OA2-E2).
const RPC_STATUS_ADMISSION_DENIED: u16 = 0x0009;

/// How a selected provider is authorized — derived from the discovery plane and
/// the org relation, never chosen by the caller.
///
/// The grant is boxed because it is by far the larger variant (a signed grant
/// with its discovery binding), and the same-org arm carries nothing: an
/// unboxed enum would make every candidate in the selection vector pay the
/// cross-org size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Same organization: the provider's owner org IS the acting org. No
    /// capability grant is attached — admission refuses an unexpected one.
    SameOrg,
    /// Cross-organization: authorized by exactly one held grant.
    Granted(Box<OrgCapabilityGrant>),
}

/// A discovered provider plus the plane that produced it.
struct Candidate {
    provider: EntityId,
    owner_org: net::adapter::net::behavior::org::OrgId,
    same_org: bool,
}

/// A discovered provider this credential set is authorized to invoke
/// `capability` on, carrying the authority relation and current direct
/// reachability. The whole pre-network authority decision, factored out of
/// selection (OLB-1) so `plan` — and later the sensed selector — composes over
/// it rather than re-deriving it.
///
/// Internal only; nothing here is re-exported.
#[derive(Debug, Clone)]
pub(crate) struct AuthorizedOrgCandidate {
    /// The provider entity to target.
    pub(crate) provider: EntityId,
    /// The provider's owner organization as it rides the proof: the acting org
    /// for [`Mode::SameOrg`], the grant issuer for [`Mode::Granted`].
    pub(crate) provider_owner_org: net::adapter::net::behavior::org::OrgId,
    /// How invoking this provider is authorized.
    pub(crate) mode: Mode,
    /// Whether a live direct session to this provider exists right now
    /// (OA2-E0.3: protected RPC is direct-session-only). Annotated here, never
    /// a filter on authorization.
    pub(crate) direct: bool,
    /// The capability being invoked.
    pub(crate) capability: CapabilityAuthorityId,
}

/// The outcome of one cold-plan derivation over one capture (OLB-2B.3d-pre).
///
/// `Superseded` is not an error: nothing was sent, nothing was signed, and the
/// caller re-derives from a fresh capture. It is a value rather than a bool so a
/// superseded attempt cannot be mistaken for "no candidates".
#[derive(Debug)]
pub(crate) enum PlanAttempt {
    /// The captured authority still held, so exactly one proof intent exists.
    ///
    /// Boxed for the reason [`Mode::Granted`] is: the intent carries the whole
    /// credential set, and the superseded arm carries a `usize`.
    Minted(Box<OrgProofIntent>),
    /// The captured authority moved before the mint. Carries the count this
    /// derivation examined, so the eventual refusal reports a real number.
    Superseded {
        /// Private candidates examined before authority filtering.
        considered: usize,
    },
}

impl OrgClient {
    /// Call a protected service (OSDK §2).
    ///
    /// Discovers privately, selects one authorized provider, mints a canonical
    /// request-bound proof, and issues one exact-target call.
    ///
    /// Errors distinguish local refusal ([`OrgSdkError::Credentials`],
    /// [`OrgSdkError::Discovery`] — nothing was sent) from provider refusal
    /// ([`OrgSdkError::AdmissionDenied`]) and transport
    /// ([`OrgSdkError::Rpc`]).
    pub async fn call<Req, Resp>(&self, service: &str, request: &Req) -> Result<Resp, OrgSdkError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = Codec::Json.encode(request).map_err(|e| RpcError::Codec {
            direction: net::adapter::net::mesh_rpc::CodecDirection::Encode,
            message: format!("org call encode: {e}"),
        })?;

        let reply = self.call_bytes(service, Bytes::from(body)).await?;

        Codec::Json.decode(&reply).map_err(|e| {
            OrgSdkError::Rpc(RpcError::Codec {
                direction: net::adapter::net::mesh_rpc::CodecDirection::Decode,
                message: format!("org call decode: {e}"),
            })
        })
    }

    /// [`call`](Self::call) without the codec — bytes in, bytes out (OSDK-L R1).
    ///
    /// The typed verb IS this plus JSON, so there is one authority path and the
    /// typed layer is provably just marshaling. Exists because language
    /// bindings cannot cross an FFI boundary with a generic: `call<Req, Resp>`
    /// is unwrappable by napi, PyO3, or cgo, and this is what they call.
    ///
    /// Every guarantee of the typed verb holds here unchanged: private-only
    /// discovery, inferred admission mode, exact grant matching, deterministic
    /// selection, one canonical request-bound proof, and no retry.
    pub async fn call_bytes(&self, service: &str, request: Bytes) -> Result<Bytes, OrgSdkError> {
        // `0` deadline = the facade's default; `0` token = uncancellable.
        self.call_bytes_deadline(service, request, 0, 0).await
    }

    /// [`call_bytes`](Self::call_bytes) with execution control — a deadline and
    /// a pre-reserved cancel token (OSDK-L §D6a).
    ///
    /// This is the seam the C ABI's `net_org_call` reaches so a Go `Call(ctx,
    /// ..)` can carry a real deadline and cancel a call **in flight**, rather
    /// than only abandoning its own wait while an authorized side effect keeps
    /// executing. Neither argument is an authorization input: they select no
    /// provider, no grant, and no authority — the `plan()` decision is byte-for-
    /// byte identical to `call_bytes`. `deadline_ms == 0` means the facade
    /// default; `cancel_token == 0` means uncancellable. Reserve the token with
    /// [`reserve_cancel_token`](Self::reserve_cancel_token) BEFORE calling.
    ///
    /// `#[doc(hidden)]` — applications use `call`/`call_bytes`; execution control
    /// is a binding concern, exposed for the cancellable C ABI only.
    #[doc(hidden)]
    pub async fn call_bytes_deadline(
        &self,
        service: &str,
        request: Bytes,
        deadline_ms: u64,
        cancel_token: u64,
    ) -> Result<Bytes, OrgSdkError> {
        let intent = self.plan(service)?;
        let provider = intent.provider.clone();

        let mut opts = CallOptions {
            org_proof_intent: Some(intent),
            ..CallOptions::default()
        };
        // Execution control only — never an authority input.
        if deadline_ms > 0 {
            opts.deadline = Some(Instant::now() + Duration::from_millis(deadline_ms));
        }
        if cancel_token != 0 {
            opts.cancel_token = Some(cancel_token);
        }

        // The core call mints the call id, computes the canonical request
        // digest, signs the proof, appends exactly one admission header, and
        // pins `peer_entity_id(target) == intent.provider` before sending.
        let reply = self
            .node
            .call(provider.node_id(), service, request, opts)
            .await
            .map_err(map_rpc_error)?;

        Ok(reply.body)
    }

    /// Call a subnet-exported service (SUBNET_AUTH_SDK_PLAN.md §3.6).
    ///
    /// Discovers on the PUBLIC plane through the verified ownership
    /// projection, derives the same-org / granted relation from the
    /// verified owner, selects deterministically, mints the same canonical
    /// request-bound proof as [`call`](Self::call), and sends exactly once.
    ///
    /// Deliberately `call_exported`, not `call_subnet`: this invokes a
    /// publicly discoverable organization service. The caller presents
    /// organization authority only — it never supplies a subnet
    /// credential, an export binding, or any subnet coordinate, it never
    /// joins the provider's subnet, and it receives no provider-local
    /// subnet context. Whether the export exists is decided provider-side,
    /// per call, against the provider's own live authority; provider-side
    /// authority movement surfaces as one coarse
    /// [`AdmissionDenied`](OrgSdkError::AdmissionDenied) and is NEVER
    /// retried by the facade — a signed proof is never resent, and
    /// rediscovery/retry policy belongs to the application.
    pub async fn call_exported<Req, Resp>(
        &self,
        service: &str,
        request: &Req,
    ) -> Result<Resp, OrgSdkError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = Codec::Json.encode(request).map_err(|e| RpcError::Codec {
            direction: net::adapter::net::mesh_rpc::CodecDirection::Encode,
            message: format!("org call_exported encode: {e}"),
        })?;

        let reply = self.call_exported_bytes(service, Bytes::from(body)).await?;

        Codec::Json.decode(&reply).map_err(|e| {
            OrgSdkError::Rpc(RpcError::Codec {
                direction: net::adapter::net::mesh_rpc::CodecDirection::Decode,
                message: format!("org call_exported decode: {e}"),
            })
        })
    }

    /// [`call_exported`](Self::call_exported) without the codec — bytes in,
    /// bytes out. The typed verb IS this plus JSON; language bindings call
    /// this.
    pub async fn call_exported_bytes(
        &self,
        service: &str,
        request: Bytes,
    ) -> Result<Bytes, OrgSdkError> {
        self.call_exported_bytes_deadline(service, request, 0, 0)
            .await
    }

    /// [`call_exported_bytes`](Self::call_exported_bytes) with execution
    /// control — a deadline and a pre-reserved cancel token, the same
    /// binding seam contract as
    /// [`call_bytes_deadline`](Self::call_bytes_deadline): neither argument
    /// is an authorization input, and the `plan_exported()` decision is
    /// byte-for-byte identical to `call_exported_bytes`.
    ///
    /// `#[doc(hidden)]` — applications use `call_exported`; execution
    /// control is a binding concern.
    #[doc(hidden)]
    pub async fn call_exported_bytes_deadline(
        &self,
        service: &str,
        request: Bytes,
        deadline_ms: u64,
        cancel_token: u64,
    ) -> Result<Bytes, OrgSdkError> {
        let intent = self.plan_exported(service)?;
        let provider = intent.provider.clone();

        let mut opts = CallOptions {
            org_proof_intent: Some(intent),
            ..CallOptions::default()
        };
        // Execution control only — never an authority input.
        if deadline_ms > 0 {
            opts.deadline = Some(Instant::now() + Duration::from_millis(deadline_ms));
        }
        if cancel_token != 0 {
            opts.cancel_token = Some(cancel_token);
        }

        // One send, ever: the core call mints the call id, digests, signs,
        // pins the peer entity, and the facade never launches a second
        // attempt on any outcome.
        let reply = self
            .node
            .call(provider.node_id(), service, request, opts)
            .await
            .map_err(map_rpc_error)?;

        Ok(reply.body)
    }

    /// Reserve a cancel token from this client's node for a subsequent
    /// [`call_bytes_deadline`](Self::call_bytes_deadline) (OSDK-L §D6a).
    ///
    /// Reserve BEFORE the call so a cancel that races the call's registration is
    /// still delivered — the doctrine [`MeshNode::reserve_cancel_token`] already
    /// establishes. Scoped to this client's node so the substrate's per-mesh
    /// `CancelRegistry` stays the single source of truth.
    ///
    /// `#[doc(hidden)]` — a binding-only execution-control seam.
    ///
    /// [`MeshNode::reserve_cancel_token`]: net::adapter::net::MeshNode::reserve_cancel_token
    #[doc(hidden)]
    pub fn reserve_cancel_token(&self) -> u64 {
        self.node.reserve_cancel_token()
    }

    /// Cancel the one in-flight call bound to `token` (OSDK-L §D6a). Idempotent;
    /// a no-op for `0` or a token no call reserved. It never launches a second
    /// attempt — a signed proof is never resent (the facade's no-retry rule).
    ///
    /// `#[doc(hidden)]` — a binding-only execution-control seam.
    #[doc(hidden)]
    pub fn cancel(&self, token: u64) {
        self.node.cancel(token);
    }

    /// Everything `call` does before touching the network: the coherent
    /// authority/discovery capture, the stage-3 temporal recheck, mode
    /// classification, exact grant matching, deterministic selection, the final
    /// coherent authority comparison, and the canonical proof intent.
    ///
    /// Split out so the whole authority decision is witnessable without a
    /// provider: `call` is exactly this plus encode → `MeshNode::call` → decode.
    ///
    /// **The coherent cold plan** (OLB-2B.3d-pre,
    /// `docs/internal/plans/OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md` §10). Every
    /// step below reads ONE captured observation of the node's private-discovery
    /// authority — one instant, one revocation view, one consumer-grant view,
    /// one scoped-store critical section — instead of re-sampling per credential
    /// and per plane. Then, before the proof exists, it compares that authority
    /// identity again and re-derives rather than minting under an identity that
    /// has moved.
    ///
    /// The loop is bounded, and it is not a retry of anything: nothing has been
    /// sent, no proof has been signed, and no provider has been contacted. A
    /// re-derivation happens only on node-mediated authority movement (an
    /// authority or store installation, a floor raise, a poison transition, a
    /// consumer-grant install/remove/replacement), never on announcement
    /// traffic — captured rows are values, already filtered per row.
    ///
    /// Exhausting the attempts is a LOCAL refusal reported as
    /// [`OrgDiscoveryError::NoAuthorizedProvider`] with the last derivation's
    /// considered count: the plan examined that many candidates and could not
    /// establish an authorized provider under one coherent authority. It never
    /// falls through to a send under a superseded capture.
    pub(crate) fn plan(&self, service: &str) -> Result<OrgProofIntent, OrgSdkError> {
        let capability = CapabilityAuthorityId::for_tag(&nrpc_tag(service));
        self.plan_over(&capability, || self.capture_private(&capability))
    }

    /// [`Self::plan`]'s bounded loop over an injectable capture.
    ///
    /// `pub(crate)` with the capture as a parameter because the loop's own
    /// properties — that exhaustion refuses locally with the last considered
    /// count, that the budget is bounded, and that each refusal class maps to the
    /// existing vocabulary — are otherwise only reachable by racing authority
    /// movement against three consecutive derivations.
    pub(crate) fn plan_over(
        &self,
        capability: &CapabilityAuthorityId,
        mut capture: impl FnMut() -> Result<OrgColdDiscovery, OrgColdRefusal>,
    ) -> Result<OrgProofIntent, OrgSdkError> {
        let mut considered = 0usize;
        for _ in 0..COLD_PLAN_ATTEMPTS {
            let capture = match capture() {
                Ok(capture) => capture,
                Err(refusal) => return Err(cold_refusal_error(capability, refusal, considered)),
            };
            match self.plan_attempt(capability, &capture)? {
                PlanAttempt::Minted(intent) => return Ok(*intent),
                PlanAttempt::Superseded { considered: seen } => considered = seen,
            }
        }
        Err(OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex_capability(capability),
            considered,
        }
        .into())
    }

    /// ONE derivation over ONE capture: candidates, selection, the final
    /// coherent authority comparison, and — only if that comparison holds — the
    /// derivation's result.
    ///
    /// The comparison sits between selection and the RELEASE of the result
    /// deliberately (design §10): the rows, the grant matching and the chosen
    /// provider all rest on the captured authority, so a moved authority
    /// invalidates the whole derivation rather than just its last step.
    ///
    /// **HOLD-3 (independent review, 2026-08-29): that includes the NEGATIVE
    /// outcomes.** The derivation is computed into a value FIRST, and the
    /// comparison gates it: `?` on candidate derivation or selection would have
    /// let a stale `NoAuthorizedProvider`, `ProviderNotDirect`,
    /// `AmbiguousCapabilityGrant` or credential refusal escape from a view the
    /// node had already superseded — a wrong exact refusal, reported with
    /// authority, and outside the bounded re-derivation budget. Movement can
    /// CAUSE all four: a removed consumer grant empties a plane, a raised floor
    /// retracts the only direct provider, and an installed grant can make two
    /// grants match at once.
    ///
    /// When the capture is still current the exact error is preserved verbatim.
    pub(crate) fn plan_attempt(
        &self,
        capability: &CapabilityAuthorityId,
        capture: &OrgColdDiscovery,
    ) -> Result<PlanAttempt, OrgSdkError> {
        // Derive into a VALUE — never `?` — so the comparison below decides
        // whether this derivation may speak at all.
        let (candidates, considered) = self.derive_captured(capability, capture);
        let derived = candidates.and_then(|candidates| {
            self.select_candidate(capability, &candidates, considered)
                .map(|candidate| self.intent_for(candidate))
        });
        if !self.node.org_cold_authority_is_current(capture.authority()) {
            return Ok(PlanAttempt::Superseded { considered });
        }
        derived.map(|intent| PlanAttempt::Minted(Box::new(intent)))
    }

    /// [`Self::plan`] over the public exported plane — the same selection rule,
    /// the same captured instant, the same final comparison, and the same
    /// negative-outcome gating applied to
    /// [`Self::authorized_exported_candidates`].
    ///
    /// The capture is the AUTHORITY half only: exported candidates come from the
    /// plaintext fold, so there is no private plane to query. The temporal and
    /// authority coherence is identical.
    pub(crate) fn plan_exported(&self, service: &str) -> Result<OrgProofIntent, OrgSdkError> {
        let capability = CapabilityAuthorityId::for_tag(&nrpc_tag(service));
        self.plan_exported_over(&capability, service, || self.node.org_cold_authority())
    }

    /// [`Self::plan_exported`]'s bounded loop over an injectable capture — the
    /// exported twin of [`Self::plan_over`], for the same reason.
    pub(crate) fn plan_exported_over(
        &self,
        capability: &CapabilityAuthorityId,
        service: &str,
        mut capture: impl FnMut() -> Result<OrgColdAuthority, OrgColdRefusal>,
    ) -> Result<OrgProofIntent, OrgSdkError> {
        let mut considered = 0usize;
        for _ in 0..COLD_PLAN_ATTEMPTS {
            let authority = match capture() {
                Ok(authority) => authority,
                Err(refusal) => return Err(cold_refusal_error(capability, refusal, considered)),
            };
            match self.plan_exported_attempt(capability, service, &authority)? {
                PlanAttempt::Minted(intent) => return Ok(*intent),
                PlanAttempt::Superseded { considered: seen } => considered = seen,
            }
        }
        Err(OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex_capability(capability),
            considered,
        }
        .into())
    }

    /// ONE exported derivation over ONE authority capture — the exported twin of
    /// [`Self::plan_attempt`], including its negative-outcome gating (HOLD-3).
    pub(crate) fn plan_exported_attempt(
        &self,
        capability: &CapabilityAuthorityId,
        service: &str,
        authority: &OrgColdAuthority,
    ) -> Result<PlanAttempt, OrgSdkError> {
        let (candidates, considered) = self.derive_exported(capability, service, authority);
        let derived = candidates.and_then(|candidates| {
            self.select_candidate(capability, &candidates, considered)
                .map(|candidate| self.intent_for(candidate))
        });
        if !self.node.org_cold_authority_is_current(authority) {
            return Ok(PlanAttempt::Superseded { considered });
        }
        derived.map(|intent| PlanAttempt::Minted(Box::new(intent)))
    }

    /// The shared selection rule (OA2-E0.3): org-protected RPC is
    /// direct-session-only, so select the first authorized provider
    /// (deterministic order) with a live direct session; if some are
    /// authorized but none is directly reachable, tell the caller which of
    /// the two it hit.
    ///
    /// Returns the CHOSEN CANDIDATE rather than a proof intent: the cold plan's
    /// final coherent authority comparison sits between selection and the mint
    /// (design §10), so selection must not be the thing that mints.
    fn select_candidate<'a>(
        &self,
        capability: &CapabilityAuthorityId,
        candidates: &'a [AuthorizedOrgCandidate],
        considered: usize,
    ) -> Result<&'a AuthorizedOrgCandidate, OrgSdkError> {
        if let Some(candidate) = candidates.iter().find(|c| c.direct) {
            return Ok(candidate);
        }
        if let Some(candidate) = candidates.first() {
            return Err(OrgDiscoveryError::ProviderNotDirect {
                provider: candidate.provider.clone(),
            }
            .into());
        }
        Err(OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex_capability(capability),
            considered,
        }
        .into())
    }

    /// One coherent capture of the private planes for `capability`, over exactly
    /// the audiences this credential set holds DISCOVER on.
    ///
    /// The grant ids are derived here, in HELD-GRANT ORDER, and the capture
    /// answers per grant id in that same order — so the discovery order the
    /// authority pipeline depends on is the facade's, not the node's.
    pub(crate) fn capture_private(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Result<OrgColdDiscovery, OrgColdRefusal> {
        let discover_grant_ids: Vec<[u8; 32]> = self
            .grants
            .iter()
            .filter(|g| &g.capability == capability && g.permits_discover())
            .map(|g| g.grant_id)
            .collect();
        self.node
            .org_cold_discovery(capability, &discover_grant_ids)
    }

    /// The authorized candidate set: which discovered providers this credential
    /// set may invoke `capability` on, each annotated with its authority
    /// relation and current direct reachability, in deterministic order.
    ///
    /// Authority and reachability stay distinct in meaning even though both now
    /// ride the candidate: no transport state can make an unauthorized provider
    /// eligible or an authorized one ineligible — `direct` is annotated here,
    /// never a filter. Selection (`plan`, and later the sensed selector)
    /// composes over this set. Returns the ordered candidates and how many
    /// private candidates were considered (the count `NoAuthorizedProvider`
    /// reports).
    ///
    /// Takes its own coherent capture. `plan` does NOT go through here — it
    /// keeps its capture so the final comparison can name the exact authority the
    /// candidates were derived under.
    ///
    /// Compiled only where its callers are (the `cortex`-gated call witnesses):
    /// after OLB-2B.3d-pre the ONE production entry into the authority decision
    /// is `plan`, and a seam kept alive by an `allow(dead_code)` would claim a
    /// production consumer that does not exist.
    #[cfg(all(test, feature = "cortex"))]
    pub(crate) fn authorized_candidates(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Result<(Vec<AuthorizedOrgCandidate>, usize), OrgSdkError> {
        let capture = self
            .capture_private(capability)
            .map_err(|refusal| cold_refusal_error(capability, refusal, 0))?;
        self.authorized_captured_candidates(capability, &capture)
    }

    /// The candidate derivation over an already-captured observation — the
    /// PURE half: no clock sample, no store query, no authority read.
    #[cfg(all(test, feature = "cortex"))]
    fn authorized_captured_candidates(
        &self,
        capability: &CapabilityAuthorityId,
        capture: &OrgColdDiscovery,
    ) -> Result<(Vec<AuthorizedOrgCandidate>, usize), OrgSdkError> {
        let (candidates, considered) = self.derive_captured(capability, capture);
        candidates.map(|candidates| (candidates, considered))
    }

    /// [`Self::authorized_captured_candidates`] as a VALUE plus the count
    /// discovery examined — the shape [`Self::plan_attempt`] needs.
    ///
    /// The count rides beside the result rather than inside the `Ok` arm because
    /// it is a property of DISCOVERY, which succeeded even when authorization
    /// then refused: an ambiguity examined its candidate. A credential or
    /// authority refusal precedes discovery and therefore examined none (HOLD-3).
    fn derive_captured(
        &self,
        capability: &CapabilityAuthorityId,
        capture: &OrgColdDiscovery,
    ) -> (Result<Vec<AuthorizedOrgCandidate>, OrgSdkError>, usize) {
        // Stage 3 of the validity contract: the credentials backing EVERY call,
        // at the captured instant.
        if let Err(refusal) = self.check_current_at(capture.now_secs()) {
            return (Err(refusal.into()), 0);
        }
        // Per-call authority currentness. Bind proved this relation once; a call
        // is where it must still hold, and a plan against another org's authority
        // would search private state this credential set cannot own.
        //
        // Fail-closed rather than reachable today: `install_node_authority`
        // refuses replacement by a different owner org and there is no
        // uninstall, so a bound client's authority cannot change org. No witness
        // claims the end-to-end transition; the capture-level refusals are
        // witnessed directly.
        if capture.authority_org() != self.acting_org {
            return (
                Err(OrgCredentialError::NodeAuthorityOrgMismatch {
                    authority_org: capture.authority_org(),
                    membership_org: self.acting_org,
                }
                .into()),
                0,
            );
        }
        if !self.dispatcher.covers_capability(capability) {
            return (
                Err(OrgCredentialError::DispatcherScopeExcludesCapability {
                    capability: hex_capability(capability),
                }
                .into()),
                0,
            );
        }

        let discovered = self.discover_private_captured(capability, capture);
        let considered = discovered.len();
        (
            self.authorize_discovered(capability, discovered, capture.now_secs()),
            considered,
        )
    }

    /// The exported-plane counterpart of the private derivation
    /// (SUBNET_AUTH_SDK_PLAN.md §3.6): candidates come from the public
    /// verified-ownership query instead of the private planes; the
    /// credential checks and the whole authority pipeline are the SAME
    /// code, deliberately not forked.
    ///
    /// Derives over an already-captured authority observation, so the exported
    /// path shares the private path's one instant and one authority identity —
    /// and, like [`Self::derive_captured`], yields a VALUE plus the count so the
    /// final comparison can gate a refusal (HOLD-3).
    fn derive_exported(
        &self,
        capability: &CapabilityAuthorityId,
        service: &str,
        authority: &OrgColdAuthority,
    ) -> (Result<Vec<AuthorizedOrgCandidate>, OrgSdkError>, usize) {
        if let Err(refusal) = self.check_current_at(authority.now_secs()) {
            return (Err(refusal.into()), 0);
        }
        if authority.authority_org() != self.acting_org {
            return (
                Err(OrgCredentialError::NodeAuthorityOrgMismatch {
                    authority_org: authority.authority_org(),
                    membership_org: self.acting_org,
                }
                .into()),
                0,
            );
        }
        if !self.dispatcher.covers_capability(capability) {
            return (
                Err(OrgCredentialError::DispatcherScopeExcludesCapability {
                    capability: hex_capability(capability),
                }
                .into()),
                0,
            );
        }

        let discovered = self.discover_public_owned(service);
        let considered = discovered.len();
        (
            self.authorize_discovered(capability, discovered, authority.now_secs()),
            considered,
        )
    }

    /// Phases 1–3 of the authority pipeline, shared verbatim by the
    /// private and exported paths — grant matching and proof-relevant
    /// classification must not fork per discovery plane.
    ///
    /// `now_secs` is the plan's captured instant: every grant window in phase 1
    /// is evaluated at exactly it, so two candidates can never be authorized
    /// against two different "now"s (OLB-2B.3d-pre).
    fn authorize_discovered(
        &self,
        capability: &CapabilityAuthorityId,
        discovered: Vec<Candidate>,
        now_secs: u64,
    ) -> Result<Vec<AuthorizedOrgCandidate>, OrgSdkError> {
        // Phase 1 — authority construction in DISCOVERY order. Grant matching
        // (and its `AmbiguousCapabilityGrant` error) must run in this order, so
        // the first independently-ambiguous discovered candidate is the one that
        // surfaces, exactly as before the factoring. `direct` is a placeholder
        // here; it is annotated in phase 3 so reachability is never observed in
        // discovery order.
        let mut candidates: Vec<AuthorizedOrgCandidate> = Vec::new();
        for candidate in discovered {
            // `provider_owner_org` is derived exactly as the proof needs it: the
            // acting org for same-org, the grant issuer for granted — never the
            // raw record's owner org.
            let (mode, provider_owner_org) = if candidate.same_org {
                (Mode::SameOrg, self.acting_org)
            } else {
                match self.match_invoke_grant(capability, &candidate, now_secs)? {
                    Some(grant) => {
                        let issuer_org = grant.issuer_org;
                        (Mode::Granted(Box::new(grant)), issuer_org)
                    }
                    None => continue,
                }
            };
            candidates.push(AuthorizedOrgCandidate {
                provider: candidate.provider,
                provider_owner_org,
                mode,
                direct: false,
                capability: *capability,
            });
        }
        // Phase 2 — deterministic, load-blind ordering. A stable choice is
        // debuggable, and spreading load is a policy this stage has no basis to
        // invent — sensed selection arrives above this layer in OLB-3.
        candidates.sort_by(|a, b| a.provider.as_bytes().cmp(b.provider.as_bytes()));
        // Phase 3 — annotate OA2-E0.3 direct reachability in SORTED order, the
        // exact order the pre-factoring selection loop queried sessions in.
        // Discovery order is not provider-EntityId order across the owner and
        // grant planes, so sampling before the sort could, under concurrent
        // session churn, observe reachability in a different order and select a
        // different provider. Reachability is annotated, never a filter: reading
        // providers after the first direct one cannot change which earlier
        // candidate `plan` selects.
        for candidate in &mut candidates {
            candidate.direct = self
                .node
                .peer_entity_id(candidate.provider.node_id())
                .as_ref()
                == Some(&candidate.provider);
        }
        Ok(candidates)
    }

    /// Assemble the canonical nine-field proof intent for a chosen candidate.
    /// Pure construction — the authority decision already happened in
    /// [`Self::plan_attempt`], and its result is released only after the final
    /// currentness comparison there.
    pub(crate) fn intent_for(&self, candidate: &AuthorizedOrgCandidate) -> OrgProofIntent {
        OrgProofIntent {
            caller: self.caller.clone(),
            membership: self.membership.clone(),
            dispatcher: self.dispatcher.clone(),
            capability_grant: match &candidate.mode {
                Mode::SameOrg => None,
                Mode::Granted(grant) => Some((**grant).clone()),
            },
            acting_org: self.acting_org,
            provider_owner_org: candidate.provider_owner_org,
            provider: candidate.provider.clone(),
            capability: candidate.capability,
            proof_ttl_secs: DEFAULT_PROOF_TTL_SECS,
        }
    }

    /// The public exported plane, as one candidate list: every plaintext
    /// `nrpc:<service>` candidate carrying a verified owner projection,
    /// candidate and owner from ONE fold snapshot. Same-org is derived by
    /// comparing the VERIFIED owner org against the acting org — an
    /// unsigned or unowned public candidate never reaches authority
    /// construction because the query cannot return one.
    ///
    /// No DISCOVER right is involved: the announcement is public. The
    /// grant question (INVOKE) is [`Self::match_invoke_grant`]'s, shared
    /// with the private path.
    fn discover_public_owned(&self, service: &str) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        for p in self.node.public_owned_service_providers(service) {
            push_unique(
                &mut out,
                Candidate {
                    same_org: p.owner_org == self.acting_org,
                    provider: p.provider,
                    owner_org: p.owner_org,
                },
            );
        }
        out
    }

    /// The two private planes of ONE capture, in one candidate list.
    ///
    /// Owner-plane records are same-org by construction (ingest requires the
    /// envelope's owner org to be this node's own); granted-plane records come
    /// from grants this client holds DISCOVER on. The plane order — owner first,
    /// then held grants in held order — and the dedup rule are unchanged: an
    /// owner-plane duplicate wins, so a provider visible on both planes is
    /// classified same-org exactly as before.
    ///
    /// Pure over the capture (OLB-2B.3d-pre): no query, no clock, no lock. The
    /// grant loop still walks `self.grants` rather than the capture's rows, so
    /// the facade's own grant order decides discovery order.
    fn discover_private_captured(
        &self,
        capability: &CapabilityAuthorityId,
        capture: &OrgColdDiscovery,
    ) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();

        for c in capture.owner_providers() {
            push_unique(
                &mut out,
                Candidate {
                    provider: c.provider.clone(),
                    owner_org: c.owner_org,
                    same_org: true,
                },
            );
        }

        for grant in &self.grants {
            if &grant.capability != capability || !grant.permits_discover() {
                continue;
            }
            for c in capture.granted_providers(&grant.grant_id) {
                let PrivateCapabilityProvider {
                    provider,
                    owner_org,
                    ..
                } = c;
                let same_org = *owner_org == self.acting_org;
                push_unique(
                    &mut out,
                    Candidate {
                        provider: provider.clone(),
                        owner_org: *owner_org,
                        same_org,
                    },
                );
            }
        }
        out
    }

    /// The complete authority relation for invoking `capability` on this
    /// candidate: grantee org, issuer org, capability, INVOKE, target scope, and
    /// a current window — evaluated with the provider's OWN predicates
    /// (`permits_invoke`, `GrantTargetScope::covers`,
    /// `is_valid_at_with_skew`), never a reimplementation.
    ///
    /// The window is evaluated at the plan's captured instant rather than at a
    /// fresh sample per grant per candidate, which is what let one plan mix
    /// grants that were never simultaneously valid (OLB-2B.3d-pre).
    ///
    /// Zero matches is not an error here (another candidate may match);
    /// ambiguity is, and is never resolved silently.
    fn match_invoke_grant(
        &self,
        capability: &CapabilityAuthorityId,
        candidate: &Candidate,
        now_secs: u64,
    ) -> Result<Option<OrgCapabilityGrant>, OrgSdkError> {
        let mut matches: Vec<&OrgCapabilityGrant> = self
            .grants
            .iter()
            .filter(|g| {
                g.grantee_org == self.acting_org
                    && g.issuer_org == candidate.owner_org
                    && &g.capability == capability
                    && g.permits_invoke()
                    && g.target_scope
                        .covers(&candidate.provider, Some(&candidate.owner_org))
                    && g.is_valid_at_with_skew(now_secs, self.skew_secs).is_ok()
            })
            .collect();

        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.remove(0).clone())),
            _ => Err(OrgCredentialError::AmbiguousCapabilityGrant {
                capability: hex_capability(capability),
                grant_ids: matches.iter().map(|g| hex32(&g.grant_id)).collect(),
            }
            .into()),
        }
    }
}

/// The shared proof TTL (`MAX_ORG_PROOF_TTL_SECS`). Owned by the SDK: it must be
/// long enough to survive one network round trip and short enough that a
/// captured proof is worthless, and there is no per-call knowledge that improves
/// on the substrate's frozen value.
const DEFAULT_PROOF_TTL_SECS: u64 = net::adapter::net::behavior::org_call::MAX_ORG_PROOF_TTL_SECS;

/// How many times the cold plan re-derives from a fresh capture when the
/// authority it derived under moved before the intent was minted
/// (OLB-2B.3d-pre).
///
/// Bounded rather than looped, matching `MeshNode::sample_routing_authority`'s
/// discipline: authority movement is node-mediated and rare, so exhausting the
/// attempts means the node is genuinely churning and the honest answer is a
/// local refusal. Never a retry of an ATTEMPTED call — no proof has been signed
/// and nothing has been sent at the point this loops.
const COLD_PLAN_ATTEMPTS: usize = 3;

/// Map a capture refusal onto the facade's EXISTING local vocabulary — both arms
/// are refusals where nothing was sent.
///
/// No new error kind: the cross-language error vocabulary is frozen with its
/// golden fixture, and neither condition is a new KIND of failure. A node with no
/// installed authority is exactly `NodeAuthorityRequired` (the bind-time
/// refusal, now also checked per call); an authority view that could not be
/// observed coherently established no authorized provider, which is what
/// `NoAuthorizedProvider` says, with the count of candidates the last derivation
/// examined.
fn cold_refusal_error(
    capability: &CapabilityAuthorityId,
    refusal: OrgColdRefusal,
    considered: usize,
) -> OrgSdkError {
    match refusal {
        OrgColdRefusal::NoNodeAuthority => OrgCredentialError::NodeAuthorityRequired.into(),
        OrgColdRefusal::IncoherentAuthority => OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex_capability(capability),
            considered,
        }
        .into(),
    }
}

/// The capability tag an nRPC service registers under.
fn nrpc_tag(service: &str) -> String {
    format!("nrpc:{service}")
}

/// Keep one entry per provider — the same provider can surface on both planes
/// (owner-private and under a grant) without becoming two candidates.
fn push_unique(out: &mut Vec<Candidate>, candidate: Candidate) {
    if out.iter().any(|c| c.provider == candidate.provider) {
        return;
    }
    out.push(candidate);
}

/// Decode a provider admission denial into the facade's own variant; everything
/// else stays transport/server error.
///
/// The body is the single coarse reason byte (OA2-E2). A body that does not
/// decode maps to the least-informative bucket rather than an
/// error-about-an-error — the caller still learns it was denied.
fn map_rpc_error(e: RpcError) -> OrgSdkError {
    match &e {
        RpcError::ServerError { status, .. } if *status == RPC_STATUS_ADMISSION_DENIED => {
            let coarse = admission_reason_of(&e).unwrap_or(CoarseAdmissionReason::Denied);
            OrgSdkError::AdmissionDenied(coarse)
        }
        _ => OrgSdkError::Rpc(e),
    }
}

/// The coarse reason carried by a `0x0009` response, if it decodes.
///
/// `emit_admission_denial` ships the reason as a one-byte BODY; the caller-side
/// `RpcError::ServerError` renders that body lossily into `message`, so the byte
/// is recovered from the message's single char rather than re-read from the
/// wire.
fn admission_reason_of(e: &RpcError) -> Option<CoarseAdmissionReason> {
    let RpcError::ServerError { message, .. } = e else {
        return None;
    };
    let mut chars = message.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let byte = u8::try_from(u32::from(c)).ok()?;
    CoarseAdmissionReason::from_wire(byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly how the provider ships a denial: status `0x0009`, no headers,
    /// and a one-byte body carrying the coarse reason.
    fn denial(coarse: CoarseAdmissionReason) -> RpcError {
        RpcError::ServerError {
            status: RPC_STATUS_ADMISSION_DENIED,
            message: String::from_utf8(vec![coarse.to_wire()]).expect("coarse bytes are ascii"),
            headers: vec![],
        }
    }

    /// Every coarse reason round-trips into the facade's own variant — the byte
    /// OA2-E2 put on the wire finally has a caller-side consumer.
    #[test]
    fn every_coarse_reason_decodes_from_an_admission_denial() {
        for coarse in [
            CoarseAdmissionReason::Denied,
            CoarseAdmissionReason::NotSupported,
            CoarseAdmissionReason::Unavailable,
        ] {
            match map_rpc_error(denial(coarse)) {
                OrgSdkError::AdmissionDenied(got) => assert_eq!(got, coarse),
                other => panic!("expected AdmissionDenied, got {other:?}"),
            }
        }
    }

    /// An undecodable body still reports a denial — the least-informative
    /// bucket, never an error about the error. A caller must not be told
    /// "transport failed" when the provider refused it.
    #[test]
    fn an_undecodable_denial_body_falls_back_to_denied() {
        let e = RpcError::ServerError {
            status: RPC_STATUS_ADMISSION_DENIED,
            message: "<3 bytes of non-utf8 body>".to_string(),
            headers: vec![],
        };
        match map_rpc_error(e) {
            OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Denied) => {}
            other => panic!("expected AdmissionDenied(Denied), got {other:?}"),
        }
    }

    /// A reason byte outside the known set is still a denial, not a decode
    /// failure — a provider that learns a new bucket cannot make old callers
    /// misreport the outcome.
    #[test]
    fn an_unknown_reason_byte_is_still_a_denial() {
        let e = RpcError::ServerError {
            status: RPC_STATUS_ADMISSION_DENIED,
            message: String::from_utf8(vec![0x7F]).expect("ascii"),
            headers: vec![],
        };
        match map_rpc_error(e) {
            OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Denied) => {}
            other => panic!("expected AdmissionDenied(Denied), got {other:?}"),
        }
    }

    /// Any other server status stays a transport/server error — the facade
    /// never manufactures an admission denial.
    #[test]
    fn other_server_errors_are_not_admission_denials() {
        let e = RpcError::ServerError {
            status: 0x8001,
            message: "handler said no".to_string(),
            headers: vec![],
        };
        match map_rpc_error(e) {
            OrgSdkError::Rpc(RpcError::ServerError { status, .. }) => assert_eq!(status, 0x8001),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }
}
