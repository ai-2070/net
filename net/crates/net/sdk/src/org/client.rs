//! OSDK §2 — [`OrgClient`] and the binding verb [`Mesh::org`].
//!
//! Binding is where a credential set stops being data and becomes an operating
//! capability: it pins the set to a durable mesh identity and an installed node
//! authority, and leases the consumer audiences private discovery needs.
//!
//! S0 lands the binding relation and the lease. The call verb (`org.call`)
//! lands in S1.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::MeshNode;

use super::credentials::OrgCredentials;
use super::error::{hex32, OrgCredentialError, OrgSdkError};
use super::lease::AudienceLeaseGuard;
use super::types::{OrgCapabilityGrant, OrgDispatcherGrant, OrgId, OrgMembershipCert};
use crate::mesh::Mesh;

/// A credential set bound to a live mesh — the caller half of the org facade.
///
/// Obtained from [`Mesh::org`]. Cloning shares one audience lease rather than
/// taking a second reference, so clones are free and dropping a clone never
/// withdraws another clone's ingest authority.
#[derive(Clone)]
pub struct OrgClient {
    /// The node this client calls through. Used by the call verb, which rides
    /// the nRPC surface.
    #[cfg_attr(not(feature = "cortex"), allow(dead_code))]
    pub(crate) node: Arc<MeshNode>,
    /// The mesh's durable identity — signs every proof this client mints.
    pub(crate) caller: Arc<EntityKeypair>,
    pub(crate) membership: OrgMembershipCert,
    pub(crate) dispatcher: OrgDispatcherGrant,
    pub(crate) grants: Vec<OrgCapabilityGrant>,
    pub(crate) acting_org: OrgId,
    /// Clock-skew tolerance from the installed authority — the same tolerance
    /// the provider applies, so local temporal checks agree with remote ones.
    pub(crate) skew_secs: u64,
    /// Dropped with the last clone; releases the consumer-audience references.
    pub(crate) _lease: Arc<AudienceLeaseGuard>,
}

impl OrgClient {
    /// The organization this client acts for.
    pub fn acting_org(&self) -> OrgId {
        self.acting_org
    }

    /// The entity this client calls as (the mesh's durable identity).
    pub fn caller(&self) -> &net::adapter::net::identity::EntityId {
        self.caller.entity_id()
    }

    /// The held cross-org capability grants.
    pub fn grants(&self) -> &[OrgCapabilityGrant] {
        &self.grants
    }

    /// The membership certificate this client calls under.
    pub fn membership(&self) -> &OrgMembershipCert {
        &self.membership
    }

    /// The dispatcher grant this client calls under.
    pub fn dispatcher(&self) -> &OrgDispatcherGrant {
        &self.dispatcher
    }

    /// Stage-3 of the validity contract: are the credentials that back EVERY
    /// call currently within their validity windows?
    ///
    /// `call` performs this itself (plus the selected grant), so this is for
    /// callers that want to check before committing to work — a long-lived
    /// client crosses expiry, and a bound client is not a permanently valid
    /// one. Uses the installed authority's skew tolerance, so it agrees with
    /// the provider's own window arithmetic.
    pub fn check_current(&self) -> Result<(), OrgCredentialError> {
        // `is_valid_with_skew` samples the canonical wall clock the credential
        // family uses; taking our own `SystemTime` here would be a second clock
        // that could disagree with the one the windows were minted against.
        self.membership
            .is_valid_with_skew(self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "membership".to_string(),
                source,
            })?;
        self.dispatcher
            .is_valid_with_skew(self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "dispatcher grant".to_string(),
                source,
            })?;
        Ok(())
    }
}

impl std::fmt::Debug for OrgClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgClient")
            .field("acting_org", &self.acting_org)
            .field("caller", self.caller.entity_id())
            .field("grants", &self.grants.len())
            .finish()
    }
}

impl OrgClient {
    /// Bind a credential set to a NODE — the one implementation of the bind
    /// pipeline (OSDK-L N).
    ///
    /// [`Mesh::org`] delegates here, and so does every language binding: the
    /// Node, Python, and Go/C surfaces hold `Arc<MeshNode>` rather than an SDK
    /// [`Mesh`], and neither fabricating a throwaway `Mesh` per bind nor
    /// pinning a permanent one is acceptable — the first makes
    /// `Mesh::from_node_arc` an accidental binding adapter, the second adds a
    /// permanent `Arc<MeshNode>` that blocks shutdown.
    ///
    /// `#[doc(hidden)]` because applications should use [`Mesh::org`]; this is
    /// the binding seam, not a second public way to do the same thing. There is
    /// exactly one authority pipeline and both doors reach it.
    ///
    /// Refuses unless the complete private-discovery identity relation holds:
    ///
    /// 1. the node's identity was EXPLICITLY configured — org membership binds
    ///    to a durable cryptographic entity, never a generated ephemeral
    ///    keypair whose entity id changes on restart;
    /// 2. a node authority is installed — consumer-audience installation and
    ///    owner-private discovery both require it, so binding without one would
    ///    search private state that can never exist;
    /// 3. that authority's owner org is the membership's org;
    /// 4. the membership vouches for THIS node's entity (the provider's TOFU
    ///    member binding would refuse otherwise — fail before signing).
    ///
    /// Then each DISCOVER grant's audience is leased into the node's consumer
    /// registry. A grant that cannot currently be installed (expired, no
    /// discovery binding, conflicting, registry full) fails the bind loudly
    /// rather than leaving a client that silently discovers nothing.
    ///
    /// The lease is released when the last clone of the returned client drops.
    #[doc(hidden)]
    pub fn bind_node(
        node: Arc<MeshNode>,
        credentials: OrgCredentials,
    ) -> Result<Self, OrgSdkError> {
        // Node metadata, not an authority decision — but the facade's contract
        // is that org credentials bind to a durable entity, and a generated
        // fallback identity is not one.
        if !node.has_configured_identity() {
            return Err(OrgCredentialError::PersistentIdentityRequired.into());
        }
        let authority = node
            .node_authority()
            .ok_or(OrgCredentialError::NodeAuthorityRequired)?;

        let authority_org = authority.owner_org();
        if authority_org != credentials.acting_org() {
            return Err(OrgCredentialError::NodeAuthorityOrgMismatch {
                authority_org,
                membership_org: credentials.acting_org(),
            }
            .into());
        }
        if credentials.member() != node.entity_id() {
            return Err(OrgCredentialError::MemberBindingMismatch {
                expected: node.entity_id().clone(),
                credential: credentials.member().clone(),
            }
            .into());
        }

        let acting_org = credentials.acting_org();
        let (membership, dispatcher, grants, secrets) = credentials.into_parts();

        // Pair each secret with its grant. Construction proved every secret
        // matches exactly one held grant, so this loses nothing; a grant with no
        // secret (INVOKE-only, or DISCOVER whose key was not supplied) simply
        // installs no audience.
        let mut pairs = Vec::with_capacity(secrets.len());
        for secret in secrets {
            let Some(grant) = grants.iter().find(|g| secret.matches_grant(g)) else {
                // Unreachable: `OrgCredentials::new` rejects an unmatched secret.
                continue;
            };
            pairs.push((grant.clone(), secret));
        }

        // The lease registry lives on the NODE, so every wrapper over this node
        // shares one refcount per grant id.
        let grant_ids = node
            .acquire_consumer_audience_leases(pairs)
            .map_err(|(id, source)| OrgCredentialError::AudienceInstallRefused {
                grant_id: hex32(&id),
                source,
            })?;

        Ok(OrgClient {
            caller: node.entity_keypair_arc(),
            membership,
            dispatcher,
            grants,
            acting_org,
            skew_secs: authority.config.verification_skew_secs,
            _lease: Arc::new(AudienceLeaseGuard::new(node.clone(), grant_ids)),
            node,
        })
    }
}

impl Mesh {
    /// Bind an organization credential set to this mesh (OSDK §1).
    ///
    /// Thin delegation to [`OrgClient::bind_node`], which documents the
    /// complete relation this refuses on. One pipeline, two doors.
    pub fn org(&self, credentials: OrgCredentials) -> Result<OrgClient, OrgSdkError> {
        OrgClient::bind_node(self.node().clone(), credentials)
    }
}
