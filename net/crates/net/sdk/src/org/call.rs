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
//! # Private-only
//!
//! Discovery consults ONLY the private planes. Searching the plaintext plane
//! would need a public ownership projection, provenance ranking, and support for
//! registrations `serve_org` never creates; a protected-but-publicly-discoverable
//! service stays a low-level concern on both sides. The verbs are symmetric:
//! `serve_org` emits privately, `org.call` discovers privately.
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

    /// Everything `call` does before touching the network: capability
    /// derivation, the stage-3 temporal recheck, private discovery, mode
    /// classification, exact grant matching, deterministic selection, and the
    /// canonical proof intent.
    ///
    /// Split out so the whole authority decision is witnessable without a
    /// provider: `call` is exactly this plus encode → `MeshNode::call` → decode.
    pub(crate) fn plan(&self, service: &str) -> Result<OrgProofIntent, OrgSdkError> {
        let capability = CapabilityAuthorityId::for_tag(&nrpc_tag(service));
        let (authorized, considered) = self.authorized_targets(&capability)?;

        // OA2-E0.3: org-protected RPC is direct-session-only. A relayed
        // protected request is denied at the provider, so an authorized but
        // indirectly-reachable provider is not eligible — and the caller is told
        // which of the two it hit.
        let mut indirect: Option<EntityId> = None;
        for (provider, mode) in authorized {
            if self.node.peer_entity_id(provider.node_id()).as_ref() == Some(&provider) {
                return Ok(self.intent_for(capability, provider, mode));
            }
            indirect.get_or_insert(provider);
        }
        if let Some(provider) = indirect {
            return Err(OrgDiscoveryError::ProviderNotDirect { provider }.into());
        }
        Err(OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex_capability(&capability),
            considered,
        }
        .into())
    }

    /// The pure authority decision: which discovered providers may this
    /// credential set invoke `capability` on, in deterministic order.
    ///
    /// Separated from reachability so the authority logic is exactly what it
    /// looks like — no transport state can make an unauthorized provider
    /// eligible or an authorized one ineligible. Returns the ordered targets and
    /// how many private candidates were considered.
    pub(crate) fn authorized_targets(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Result<(Vec<(EntityId, Mode)>, usize), OrgSdkError> {
        // Stage 3 of the validity contract: the credentials backing EVERY call.
        self.check_current()?;
        if !self.dispatcher.covers_capability(capability) {
            return Err(OrgCredentialError::DispatcherScopeExcludesCapability {
                capability: hex_capability(capability),
            }
            .into());
        }

        let candidates = self.discover_private(capability);
        let considered = candidates.len();

        let mut targets: Vec<(EntityId, Mode)> = Vec::new();
        for candidate in candidates {
            let mode = if candidate.same_org {
                Mode::SameOrg
            } else {
                match self.match_invoke_grant(capability, &candidate)? {
                    Some(grant) => Mode::Granted(Box::new(grant)),
                    None => continue,
                }
            };
            targets.push((candidate.provider, mode));
        }
        // Deterministic and load-blind on purpose: a stable choice is
        // debuggable, and spreading load is a policy the facade has no basis to
        // invent.
        targets.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        Ok((targets, considered))
    }

    /// Assemble the canonical nine-field proof intent. Pure construction — the
    /// authority decision already happened.
    pub(crate) fn intent_for(
        &self,
        capability: CapabilityAuthorityId,
        provider: EntityId,
        mode: Mode,
    ) -> OrgProofIntent {
        OrgProofIntent {
            caller: self.caller.clone(),
            membership: self.membership.clone(),
            dispatcher: self.dispatcher.clone(),
            capability_grant: match &mode {
                Mode::SameOrg => None,
                Mode::Granted(grant) => Some((**grant).clone()),
            },
            acting_org: self.acting_org,
            provider_owner_org: match &mode {
                Mode::SameOrg => self.acting_org,
                Mode::Granted(grant) => grant.issuer_org,
            },
            provider,
            capability,
            proof_ttl_secs: DEFAULT_PROOF_TTL_SECS,
        }
    }

    /// The two private planes, in one candidate list. Owner-plane records are
    /// same-org by construction (ingest requires the envelope's owner org to be
    /// this node's own); granted-plane records come from grants this client
    /// holds DISCOVER on.
    fn discover_private(&self, capability: &CapabilityAuthorityId) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();

        for c in self.node.owner_private_capability_providers(capability) {
            push_unique(
                &mut out,
                Candidate {
                    provider: c.provider,
                    owner_org: c.owner_org,
                    same_org: true,
                },
            );
        }

        for grant in &self.grants {
            if &grant.capability != capability || !grant.permits_discover() {
                continue;
            }
            for c in self.node.granted_capability_providers(&grant.grant_id) {
                let PrivateCapabilityProvider {
                    provider,
                    owner_org,
                    ..
                } = c;
                let same_org = owner_org == self.acting_org;
                push_unique(
                    &mut out,
                    Candidate {
                        provider,
                        owner_org,
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
    /// (`permits_invoke`, `GrantTargetScope::covers`, `is_valid_with_skew`), never
    /// a reimplementation.
    ///
    /// Zero matches is not an error here (another candidate may match);
    /// ambiguity is, and is never resolved silently.
    fn match_invoke_grant(
        &self,
        capability: &CapabilityAuthorityId,
        candidate: &Candidate,
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
                    && g.is_valid_with_skew(self.skew_secs).is_ok()
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
