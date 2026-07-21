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

use super::types::{CoarseAdmissionReason, OrgId};

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
