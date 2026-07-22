//! OSDK §5 — the facade error hierarchy.
//!
//! Four domains, not a flattened implementation map. A caller branches on
//! whether it lacked authority locally, could not find an authorized provider,
//! was denied by the provider, or hit transport:
//!
//! ```text
//! Credentials(..)      local — nothing was sent
//! Discovery(..)        local — nothing was sent
//! AdmissionDenied(..)  remote — the provider's admission engine refused
//! Rpc(..)              transport / non-admission server error
//! ```
//!
//! The nested local enums are detailed because they never leave this process.
//! The REMOTE reason deliberately stays the coarse three-bucket wire value: a
//! precise denial reason would be a credential oracle (OA2-E2), so the provider
//! ships one byte and the detailed [`AdmissionDenied`] stays provider-side audit
//! only.
//!
//! [`AdmissionDenied`]: net::adapter::net::behavior::org_admission::AdmissionDenied

use net::adapter::net::behavior::org_grant_registry::GrantAudienceInstallError;
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::RpcError;
use thiserror::Error;

use super::types::{CapabilityAuthorityId, CoarseAdmissionReason, OrgId};

/// Every failure the org facade can produce.
#[derive(Debug, Error)]
pub enum OrgSdkError {
    /// The local credential set could not authorize this call — assembly,
    /// binding, matching, or a validity window. Nothing was sent.
    #[error("org credentials: {0}")]
    Credentials(#[from] OrgCredentialError),

    /// No provider could be found that this credential set is authorized to
    /// call. Nothing was sent.
    #[error("org discovery: {0}")]
    Discovery(#[from] OrgDiscoveryError),

    /// The provider's admission engine refused the call (`RpcStatus 0x0009`).
    /// The reason is the coarse wire bucket by design — see the module docs.
    #[error("org admission denied by provider: {0:?}")]
    AdmissionDenied(CoarseAdmissionReason),

    /// Transport failure, or a server error that is not an admission denial.
    #[error("rpc: {0}")]
    Rpc(#[from] RpcError),
}

/// The wire prefix every org error kind carries (OSDK-L R3).
///
/// Mirrors `ERR_NRPC_PREFIX` — the house pattern for crossing an FFI boundary
/// is a stable prefixed string that each language re-parses into its own error
/// type, pinned by a golden fixture.
pub const ERR_ORG_PREFIX: &str = "org:";

/// The four canonical domains, plus the parser fallback.
///
/// The domain is the load-bearing fact: it says WHERE the refusal happened.
/// `Credentials` and `Discovery` mean nothing was sent; `AdmissionDenied` means
/// a provider's admission engine evaluated and refused the request; `Rpc` means
/// transport or a non-admission server failure.
///
/// [`Unclassified`](Self::Unclassified) exists so a binding that meets a kind
/// it does not know **never impersonates one of the four** — reporting
/// `admission_denied` for an unparsed string would assert a remote evaluation
/// that may never have happened. It is an internal compatibility failure, not
/// an admission result, and a binding that emits it in CI has a vocabulary out
/// of sync with this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgErrorDomain {
    /// Local — the credential set could not authorize the call.
    Credentials,
    /// Local — no authorized provider could be found.
    Discovery,
    /// Remote — the provider's admission engine refused.
    AdmissionDenied,
    /// Transport, or a non-admission server error.
    Rpc,
    /// Parser / ABI fallback. Never produced by Rust; only by a binding whose
    /// vocabulary disagrees with this build.
    Unclassified,
}

impl OrgErrorDomain {
    /// The stable wire token. Frozen — a rename is a breaking ABI change that
    /// must fail the cross-language fixture before it reaches a release.
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Credentials => "credentials",
            Self::Discovery => "discovery",
            Self::AdmissionDenied => "admission_denied",
            Self::Rpc => "rpc",
            Self::Unclassified => "unknown",
        }
    }

    /// Parse a domain token. `None` means this build does not know it — the
    /// caller maps that to [`Unclassified`](Self::Unclassified).
    pub fn from_wire(token: &str) -> Option<Self> {
        Some(match token {
            "credentials" => Self::Credentials,
            "discovery" => Self::Discovery,
            "admission_denied" => Self::AdmissionDenied,
            "rpc" => Self::Rpc,
            "unknown" => Self::Unclassified,
            _ => return None,
        })
    }

    /// Whether a refusal in this domain means nothing left this process.
    ///
    /// The single most useful question a caller asks, and the one a
    /// misclassification would answer wrongly.
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Credentials | Self::Discovery)
    }
}

/// Parse a wire string into its domain and kind — the reference implementation
/// every binding mirrors (OSDK-L X1).
///
/// Anything that does not match `org:<domain>:<kind>` with a domain this build
/// knows yields [`OrgErrorDomain::Unclassified`] and `None`. That is the whole
/// point: a binding meeting an unfamiliar vocabulary must say so, never guess a
/// domain — reporting `admission_denied` for an unparsed string would assert
/// that a request reached a provider and its admission engine evaluated it.
pub fn parse_org_wire(wire: &str) -> (OrgErrorDomain, Option<&str>) {
    let Some(rest) = wire.strip_prefix(ERR_ORG_PREFIX) else {
        return (OrgErrorDomain::Unclassified, None);
    };
    // `domain:kind[: detail]` — the kind runs to the next colon, and the
    // detail (if any) is human-facing and never parsed for semantics.
    let mut parts = rest.splitn(3, ':');
    let (Some(domain), Some(kind)) = (parts.next(), parts.next()) else {
        return (OrgErrorDomain::Unclassified, None);
    };
    match OrgErrorDomain::from_wire(domain) {
        // `unknown` is a fallback classification, not something a peer asserts.
        Some(OrgErrorDomain::Unclassified) | None => (OrgErrorDomain::Unclassified, None),
        Some(d) if kind.is_empty() => {
            let _ = d;
            (OrgErrorDomain::Unclassified, None)
        }
        Some(d) => (d, Some(kind)),
    }
}

impl OrgSdkError {
    /// The domain this error belongs to.
    pub fn domain(&self) -> OrgErrorDomain {
        match self {
            Self::Credentials(_) => OrgErrorDomain::Credentials,
            Self::Discovery(_) => OrgErrorDomain::Discovery,
            Self::AdmissionDenied(_) => OrgErrorDomain::AdmissionDenied,
            Self::Rpc(_) => OrgErrorDomain::Rpc,
        }
    }

    /// The stable kind token within the domain.
    pub fn wire_kind(&self) -> &'static str {
        match self {
            Self::Credentials(e) => e.wire_kind(),
            Self::Discovery(e) => e.wire_kind(),
            Self::AdmissionDenied(reason) => match reason {
                CoarseAdmissionReason::Denied => "denied",
                CoarseAdmissionReason::NotSupported => "not_supported",
                CoarseAdmissionReason::Unavailable => "unavailable",
            },
            // The nested nRPC kind is already a frozen vocabulary; reuse it
            // rather than minting a second name for the same condition.
            Self::Rpc(e) => rpc_wire_kind(e),
        }
    }

    /// The single source of the `org:` wire vocabulary (OSDK-L R3).
    ///
    /// Every binding parses exactly this, and
    /// `tests/cross_lang_org/error_vectors.json` is generated from it, so a
    /// kind rename fails five suites instead of silently diverging one.
    ///
    /// Shape: `org:<domain>:<kind>[: <detail>]`. The detail is human-facing
    /// and MUST NOT be parsed for semantics — and it never carries credential
    /// material, because the local variants render only ids and the remote one
    /// renders only a coarse bucket.
    pub fn to_wire(&self) -> String {
        let (domain, kind) = (self.domain().as_wire(), self.wire_kind());
        match self {
            // The remote bucket is deliberately reasonless beyond the bucket:
            // a precise reason would be a credential oracle (OA2-E2).
            Self::AdmissionDenied(_) => format!("{ERR_ORG_PREFIX}{domain}:{kind}"),
            other => format!("{ERR_ORG_PREFIX}{domain}:{kind}: {other}"),
        }
    }
}

/// The frozen nRPC kind for an `org:rpc:` error, reusing that vocabulary.
fn rpc_wire_kind(e: &RpcError) -> &'static str {
    match e {
        RpcError::NoRoute { .. } => "no_route",
        RpcError::Timeout { .. } => "timeout",
        RpcError::ServerError { .. } => "server_error",
        RpcError::Transport(_) => "transport",
        RpcError::Codec { direction, .. } => match direction {
            net::adapter::net::mesh_rpc::CodecDirection::Encode => "codec_encode",
            net::adapter::net::mesh_rpc::CodecDirection::Decode => "codec_decode",
        },
        RpcError::CapabilityDenied { .. } => "capability_denied",
        RpcError::Cancelled => "cancelled",
    }
}

/// Local credential failures — assembly (`OrgCredentials::new`), binding
/// (`Mesh::org`), and per-call matching. The three stages are noted per variant
/// because which stage refused tells an operator where to look.
#[derive(Debug, Error)]
pub enum OrgCredentialError {
    // ---- bind: the private-discovery identity relation ----
    /// BIND. The mesh was built without an explicit `.identity(..)`, so it runs
    /// on a generated ephemeral keypair. Organization membership binds to a
    /// durable cryptographic entity; the facade refuses rather than signing
    /// proofs with a key that disappears on restart.
    #[error(
        "the mesh has no configured durable identity — build it with \
         MeshBuilder::identity(..) before binding org credentials"
    )]
    PersistentIdentityRequired,

    /// BIND. No node authority is installed (`net node adopt`). Consumer
    /// audience installation and owner-private discovery both require it, so a
    /// bind without it would search private state that can never exist and
    /// report a misleading "no authorized provider" later.
    #[error("no node authority is installed — adopt this node before binding org credentials")]
    NodeAuthorityRequired,

    /// BIND. The installed node authority belongs to a different organization
    /// than the membership being bound.
    #[error(
        "node authority owner org {authority_org:?} does not match the membership org \
         {membership_org:?}"
    )]
    NodeAuthorityOrgMismatch {
        /// The org that owns this node.
        authority_org: OrgId,
        /// The org named by the membership certificate.
        membership_org: OrgId,
    },

    /// BIND. The membership certificate vouches for a different entity than
    /// this mesh's identity. The provider's TOFU member binding would refuse
    /// this proof remotely; the facade refuses before signing.
    #[error("membership names {credential:?} but this mesh's identity is {expected:?}")]
    MemberBindingMismatch {
        /// This mesh's entity id.
        expected: EntityId,
        /// The entity the membership vouches for.
        credential: EntityId,
    },

    // ---- construction: structure + signatures ----
    /// CONSTRUCTION. A credential's signature did not verify against the org id
    /// it names.
    #[error("{credential} signature is invalid: {source}")]
    SignatureInvalid {
        /// Which credential failed (`membership`, `dispatcher grant`, or
        /// `capability grant <hex>`).
        credential: String,
        /// The underlying verification error.
        source: net::adapter::net::behavior::org::OrgError,
    },

    /// CONSTRUCTION. The dispatcher grant empowers a different entity than the
    /// membership vouches for; admission step 7 requires they be the same
    /// caller.
    #[error("dispatcher grant empowers {dispatcher:?} but the membership names {member:?}")]
    DispatcherBindingMismatch {
        /// The entity the dispatcher grant empowers.
        dispatcher: EntityId,
        /// The entity the membership vouches for.
        member: EntityId,
    },

    /// CONSTRUCTION. Membership and dispatcher grant name different orgs;
    /// admission requires they agree on the acting org.
    #[error(
        "membership org {membership_org:?} and dispatcher grant org {dispatcher_org:?} disagree"
    )]
    ActingOrgMismatch {
        /// The org named by the membership certificate.
        membership_org: OrgId,
        /// The org named by the dispatcher grant.
        dispatcher_org: OrgId,
    },

    /// CONSTRUCTION. A capability grant names a different grantee org than the
    /// acting org — this wallet holds only grants issued TO its own org.
    #[error("capability grant {grant_id} is issued to {grantee_org:?}, not the acting org")]
    GrantNotForActingOrg {
        /// Hex grant id.
        grant_id: String,
        /// The org the grant names as grantee.
        grantee_org: OrgId,
    },

    /// CONSTRUCTION. Two capability grants share a grant id.
    #[error("duplicate capability grant id {grant_id}")]
    DuplicateGrant {
        /// Hex grant id.
        grant_id: String,
    },

    /// CONSTRUCTION. An audience secret matches none of the held grants — the
    /// grant id or the key commitment disagrees. A wrong or stale secret never
    /// sits silently in the credential set.
    #[error("audience secret for grant {grant_id} matches no held grant")]
    AudienceSecretMismatch {
        /// Hex grant id the secret claims.
        grant_id: String,
    },

    // ---- bind: operational installability ----
    /// BIND. A DISCOVER grant could not be installed into the node's consumer
    /// audience registry — it is expired, lacks DISCOVER, carries no discovery
    /// binding, conflicts with a different record under the same id, or the
    /// registry is at capacity.
    #[error("capability grant {grant_id} is not installable: {source}")]
    AudienceInstallRefused {
        /// Hex grant id.
        grant_id: String,
        /// The canonical registry refusal.
        source: GrantAudienceInstallError,
    },

    /// BIND. An audience-secret file could not be loaded — it does not exist,
    /// is not a regular file, is reachable through an ancestor another account
    /// can write, is group/other-readable, carries an untrusted Windows ace, is
    /// the wrong length, or failed to decode.
    ///
    /// The detail names the path and the refusal only; nothing derived from key
    /// bytes reaches it.
    #[error("audience secret file {path} was refused: {detail}")]
    AudienceSecretFile {
        /// The rejected path.
        path: String,
        /// The canonical loader's refusal, rendered.
        detail: String,
    },

    // ---- call: temporal + matching ----
    /// CALL. A credential's validity window does not contain the call clock. A
    /// long-lived client crosses expiry, so this is re-checked per call rather
    /// than only at construction.
    #[error("{credential} is not currently valid: {source}")]
    NotCurrentlyValid {
        /// Which credential expired.
        credential: String,
        /// The underlying window error.
        source: net::adapter::net::behavior::org::OrgError,
    },

    /// CALL. The dispatcher grant's scope does not cover the invoked
    /// capability.
    #[error("the dispatcher grant does not cover capability {capability}")]
    DispatcherScopeExcludesCapability {
        /// Hex capability authority id.
        capability: String,
    },

    /// CALL. No held capability grant satisfies the complete authority relation
    /// for the selected provider (grantee org, issuer org, capability, INVOKE,
    /// target scope, validity window).
    #[error("no capability grant authorizes capability {capability} on the selected provider")]
    MissingCapabilityGrant {
        /// Hex capability authority id.
        capability: String,
    },

    /// CALL. More than one held grant satisfies the relation. The facade never
    /// picks silently — remove the ambiguity, or use the low-level
    /// `OrgProofIntent` seam to name the grant explicitly.
    #[error(
        "capability {capability} is authorized by {} overlapping grants ({grant_ids:?}) — \
         remove the ambiguity or use the low-level OrgProofIntent seam",
        grant_ids.len()
    )]
    AmbiguousCapabilityGrant {
        /// Hex capability authority id.
        capability: String,
        /// Hex ids of every grant that matched.
        grant_ids: Vec<String>,
    },
}

/// Local discovery failures — the facade found nothing it is authorized to
/// call. Nothing was sent.
#[derive(Debug, Error)]
pub enum OrgDiscoveryError {
    /// No eligible provider. `considered` counts the verified private
    /// candidates seen before authority filtering, which distinguishes "nothing
    /// was discovered at all" (0) from "providers exist but this credential set
    /// cannot call them".
    #[error(
        "no authorized provider for capability {capability} ({considered} private \
         candidate(s) considered)"
    )]
    NoAuthorizedProvider {
        /// Hex capability authority id.
        capability: String,
        /// How many verified private candidates were examined.
        considered: usize,
    },

    /// A provider was discovered but has no direct authenticated session.
    /// Org-protected RPC is direct-session-only in v1 (OA2-E0.3): a relayed
    /// protected request is denied at the provider, so the facade does not
    /// send one.
    #[error("provider {provider:?} has no direct session — protected calls are direct-only")]
    ProviderNotDirect {
        /// The provider that could not be reached directly.
        provider: EntityId,
    },
}

impl OrgCredentialError {
    /// The stable kind token. Frozen with the fixture (OSDK-L R3).
    pub fn wire_kind(&self) -> &'static str {
        match self {
            Self::PersistentIdentityRequired => "persistent_identity_required",
            Self::NodeAuthorityRequired => "node_authority_required",
            Self::NodeAuthorityOrgMismatch { .. } => "node_authority_org_mismatch",
            Self::MemberBindingMismatch { .. } => "member_binding_mismatch",
            Self::SignatureInvalid { .. } => "signature_invalid",
            Self::DispatcherBindingMismatch { .. } => "dispatcher_binding_mismatch",
            Self::ActingOrgMismatch { .. } => "acting_org_mismatch",
            Self::GrantNotForActingOrg { .. } => "grant_not_for_acting_org",
            Self::DuplicateGrant { .. } => "duplicate_grant",
            Self::AudienceSecretMismatch { .. } => "audience_secret_mismatch",
            Self::AudienceInstallRefused { .. } => "audience_install_refused",
            Self::AudienceSecretFile { .. } => "audience_secret_file",
            Self::NotCurrentlyValid { .. } => "not_currently_valid",
            Self::DispatcherScopeExcludesCapability { .. } => {
                "dispatcher_scope_excludes_capability"
            }
            Self::MissingCapabilityGrant { .. } => "missing_capability_grant",
            Self::AmbiguousCapabilityGrant { .. } => "ambiguous_capability_grant",
        }
    }
}

impl OrgDiscoveryError {
    /// The stable kind token. Frozen with the fixture (OSDK-L R3).
    pub fn wire_kind(&self) -> &'static str {
        match self {
            Self::NoAuthorizedProvider { .. } => "no_authorized_provider",
            Self::ProviderNotDirect { .. } => "provider_not_direct",
        }
    }
}

/// Hex-format a 32-byte id for error text (grant ids and capability authority
/// ids are public values; this never touches secret material).
pub(crate) fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Hex-format a capability authority id.
///
/// Only the call verb (`call.rs`, `cortex`-gated) formats a capability id for an
/// error message, so this is dead in a `net`-without-`cortex` build — the same
/// conditional-use shape as `OrgClient::node`.
#[cfg_attr(not(feature = "cortex"), allow(dead_code))]
pub(crate) fn hex_capability(capability: &CapabilityAuthorityId) -> String {
    hex32(capability.as_bytes())
}

#[cfg(test)]
mod wire_vocabulary_tests {
    //! OSDK-L R3 — the `org:` vocabulary is the bindings' contract, so these
    //! pin its shape, its exhaustiveness, and the one property a
    //! misclassification would destroy.

    use super::*;

    #[test]
    fn the_domain_token_round_trips_and_says_where_the_refusal_happened() {
        for d in [
            OrgErrorDomain::Credentials,
            OrgErrorDomain::Discovery,
            OrgErrorDomain::AdmissionDenied,
            OrgErrorDomain::Rpc,
            OrgErrorDomain::Unclassified,
        ] {
            assert_eq!(OrgErrorDomain::from_wire(d.as_wire()), Some(d));
        }
        // The load-bearing distinction: local means nothing was sent.
        assert!(OrgErrorDomain::Credentials.is_local());
        assert!(OrgErrorDomain::Discovery.is_local());
        assert!(!OrgErrorDomain::AdmissionDenied.is_local());
        assert!(!OrgErrorDomain::Rpc.is_local());
        // Unknown must NOT claim to be local either — it claims nothing.
        assert!(!OrgErrorDomain::Unclassified.is_local());
    }

    /// An unknown token yields `None` so a binding maps it to `Unclassified`
    /// rather than guessing a domain.
    #[test]
    fn an_unknown_domain_token_is_not_silently_coerced() {
        assert_eq!(OrgErrorDomain::from_wire("admission"), None);
        assert_eq!(OrgErrorDomain::from_wire(""), None);
        assert_eq!(OrgErrorDomain::from_wire("credentials "), None);
    }

    #[test]
    fn every_error_renders_the_prefixed_three_part_shape() {
        let e = OrgSdkError::Credentials(OrgCredentialError::NodeAuthorityRequired);
        let wire = e.to_wire();
        assert!(
            wire.starts_with("org:credentials:node_authority_required"),
            "{wire}"
        );
        assert_eq!(e.domain(), OrgErrorDomain::Credentials);

        let e = OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider {
            capability: "ab".repeat(32),
            considered: 3,
        });
        assert!(
            e.to_wire()
                .starts_with("org:discovery:no_authorized_provider"),
            "{}",
            e.to_wire()
        );
    }

    /// The remote bucket carries the bucket and NOTHING else — a precise
    /// reason would be a credential oracle (OA2-E2).
    #[test]
    fn an_admission_denial_renders_only_its_coarse_bucket() {
        for (reason, token) in [
            (CoarseAdmissionReason::Denied, "denied"),
            (CoarseAdmissionReason::NotSupported, "not_supported"),
            (CoarseAdmissionReason::Unavailable, "unavailable"),
        ] {
            let wire = OrgSdkError::AdmissionDenied(reason).to_wire();
            assert_eq!(wire, format!("org:admission_denied:{token}"));
            // No trailing detail at all.
            assert_eq!(wire.matches(':').count(), 2, "{wire}");
        }
    }

    /// `org:rpc:` reuses the frozen nRPC kinds rather than minting a second
    /// name for the same condition.
    #[test]
    fn the_rpc_domain_reuses_the_nrpc_kind_vocabulary() {
        let e = OrgSdkError::Rpc(RpcError::Timeout { elapsed_ms: 5 });
        assert!(
            e.to_wire().starts_with("org:rpc:timeout"),
            "{}",
            e.to_wire()
        );

        let e = OrgSdkError::Rpc(RpcError::Codec {
            direction: net::adapter::net::mesh_rpc::CodecDirection::Decode,
            message: "x".into(),
        });
        assert!(
            e.to_wire().starts_with("org:rpc:codec_decode"),
            "{}",
            e.to_wire()
        );
    }

    /// Every credential and discovery kind is distinct — a duplicated token
    /// would make two conditions indistinguishable to every binding at once.
    #[test]
    fn kind_tokens_are_unique_within_their_domain() {
        let credential_kinds = [
            OrgCredentialError::PersistentIdentityRequired.wire_kind(),
            OrgCredentialError::NodeAuthorityRequired.wire_kind(),
            OrgCredentialError::DuplicateGrant {
                grant_id: String::new(),
            }
            .wire_kind(),
            OrgCredentialError::MissingCapabilityGrant {
                capability: String::new(),
            }
            .wire_kind(),
            OrgCredentialError::AmbiguousCapabilityGrant {
                capability: String::new(),
                grant_ids: vec![],
            }
            .wire_kind(),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for k in credential_kinds {
            assert!(seen.insert(k), "duplicate credential kind token: {k}");
            // Tokens are snake_case ASCII so every language can match them.
            assert!(
                k.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "non-portable kind token: {k}"
            );
        }
    }
}
