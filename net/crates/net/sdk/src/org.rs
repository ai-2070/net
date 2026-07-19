//! Organization capability-auth surface (OA-1, scaffolded
//! ownership) — re-exports of the core org authority primitives so
//! operators and daemon authors import from `net_sdk::org::*`
//! instead of reaching into the core crate.
//!
//! See `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md`. OA-1 ships
//! belonging only: an [`OrgMembershipCert`] proves which single
//! organization owns a node and feeds the fold's `owner_org`
//! discovery projection after ingest verification. Nothing here
//! grants invocation authority — admission (OA-2) is a separate,
//! per-call proof.
//!
//! Typical operator flow (mirrors the `net-mesh` CLI):
//!
//! 1. `OrgKeypair::generate()` — offline org root (`net org keygen`).
//! 2. [`OrgMembershipCert::try_issue`] for each node
//!    (`net org issue-cert`).
//! 3. [`NodeAuthority::adopt`] on the node (`net node adopt`) —
//!    provisions `owner-membership.json`, `owner-audience.key`,
//!    and `revocation-state.json`.
//! 4. At startup, [`NodeAuthority::open`] self-verifies LOUDLY and
//!    the store is installed via
//!    `MeshNode::install_org_revocation_store`.
//! 5. Revocation: [`OrgRevocationBundle::try_issue`] with raised
//!    floors (`net org issue-floors`), applied through
//!    [`OrgRevocationStore::apply_bundle`] — monotone and
//!    restart-durable (a lower floor never rolls back).

pub use net::adapter::net::behavior::org::{
    OrgError, OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle, MAX_ORG_CERT_TTL_SECS,
    MAX_REVOCATION_FLOORS_PER_BUNDLE, ORG_CERT_SIG_DOMAIN, ORG_CERT_TTL_SECS_RECOMMENDED,
    ORG_FLOORS_SIG_DOMAIN,
};

/// OA-2 admission grants (§2.1–2.2) — the offline-issued authorization
/// credentials that back a protected call, so grant tooling imports from
/// `net_sdk::org::*` instead of reaching into the core crate. A
/// [`OrgDispatcherGrant`] (A→S) empowers a dispatcher entity to act for its org
/// over a capability; a [`OrgCapabilityGrant`] (B→A) empowers a grantee org to
/// discover and/or invoke a capability on B-owned providers, keyed by
/// [`CapabilityAuthorityId`] and scoped by [`GrantRights`] / [`GrantTargetScope`].
///
/// A DISCOVER grant additionally mints a fresh [`OrgAudienceSecret`] — the raw
/// discovery key — which NEVER transits the wire: the signed grant carries only
/// its [`GrantedDiscoveryBinding`] (a 32-byte [`audience_key_commitment`]), and
/// `OrgAudienceSecret` is deliberately non-serializable (stored 0600 out of
/// band, delivered to B's publishers and A's consumers). None of these grant
/// INVOCATION authority — that is the per-call proof the provider verifies.
pub use net::adapter::net::behavior::org_grant::{
    audience_key_commitment, CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope,
    GrantedDiscoveryBinding, OrgAudienceSecret, OrgCapabilityGrant, OrgDispatcherGrant,
    AUDIENCE_COMMIT_CONTEXT, CAPABILITY_AUTHORITY_CONTEXT, MAX_ORG_GRANT_TTL_SECS,
    ORG_AUDIENCE_SECRET_VERSION, ORG_CAPABILITY_GRANT_SIG_DOMAIN, ORG_DISPATCHER_GRANT_SIG_DOMAIN,
};

/// The clock-skew ceiling org certificate verification enforces
/// (`OrgError::ClockSkewTooLarge` above it) — the token module's
/// constant, re-exported beside the cert API it gates.
pub use net::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS;

pub use net::adapter::net::behavior::org_authority::{
    authority_dir, NodeAuthority, NodeAuthorityConfig, OrgAuthorityError, OwnerAudienceCredential,
    NODE_AUTHORITY_CONFIG_VERSION, OWNER_AUDIENCE_FILE, OWNER_MEMBERSHIP_FILE,
    REVOCATION_STATE_FILE,
};

pub use net::adapter::net::behavior::org_revocation::{
    OrgRevocationError, OrgRevocationState, OrgRevocationStore, ORG_REVOCATION_STATE_VERSION,
};

#[cfg(test)]
mod oa2_grant_sdk_reexport {
    //! OA2-F1: the OA-2 grant primitives are reachable AND constructible through
    //! the SDK facade (`net_sdk::org::*`) — not just the OA-1 belonging types.
    //! Named via `super::` (i.e. `crate::org`), so this only compiles if the
    //! re-exports exist.
    use super::{
        audience_key_commitment, CapabilityAuthorityId, DispatcherScope, GrantRights,
        GrantTargetScope, OrgCapabilityGrant, OrgDispatcherGrant, OrgKeypair,
    };
    use net::adapter::net::identity::EntityKeypair;

    #[test]
    fn grants_issue_through_the_sdk_facade() {
        let org_a = OrgKeypair::from_bytes([0x11u8; 32]);
        let org_b = OrgKeypair::from_bytes([0x22u8; 32]);
        let dispatcher = EntityKeypair::generate().entity_id().clone();
        let cap = CapabilityAuthorityId::for_tag("nrpc:svc");

        // A→S dispatcher grant.
        OrgDispatcherGrant::try_issue(&org_a, dispatcher, DispatcherScope::Exact(cap), 3600)
            .expect("dispatcher grant issues through the SDK facade");

        // B→A capability grant WITH discover → mints an audience secret whose
        // commitment matches the in-grant binding (the raw key never leaves it).
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &org_b,
            org_a.org_id(),
            cap,
            GrantRights::INVOKE.union(GrantRights::DISCOVER),
            GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
            3600,
        )
        .expect("capability grant issues through the SDK facade");
        let secret = secret.expect("a DISCOVER grant mints an audience secret");
        let binding = grant.discovery.as_ref().expect("discover binding present");
        assert!(secret.matches_binding(binding));
        assert_eq!(
            binding.key_commitment,
            audience_key_commitment(secret.discovery_key()),
        );

        // INVOKE-only → no audience secret, no binding.
        let (invoke_only, none) = OrgCapabilityGrant::try_issue(
            &org_b,
            org_a.org_id(),
            cap,
            GrantRights::INVOKE,
            GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
            3600,
        )
        .expect("invoke-only grant issues");
        assert!(none.is_none(), "INVOKE-only mints no audience secret");
        assert!(invoke_only.discovery.is_none());
    }
}
