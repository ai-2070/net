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

//! # The verb facade (OSDK)
//!
//! Beyond the canonical re-exports, this module is a thin **verb layer** over
//! the closed OA substrate — it never admits, and its local checks only ever
//! refuse to send:
//!
//! ```ignore
//! let org = mesh.org(credentials)?;                       // bind
//! let customer: Customer = org.call("customer.read", &request).await?;
//!
//! mesh.serve_org("customer.read", OrgAccess::Granted, handler)?;
//! ```
//!
//! Five top-level concepts: [`OrgCredentials`], [`OrgClient`], `OrgAccess`,
//! `OrgCaller`, and [`OrgSdkError`] (whose public domain enums
//! [`OrgCredentialError`] and [`OrgDiscoveryError`] accompany it). The
//! canonical types live in [`types`]; the low-level
//! [`OrgProofIntent`](types::OrgProofIntent) seam stays available and unchanged
//! for advanced callers.

pub mod types;
pub use types::*;

mod credentials;
mod error;
mod lease;
pub use credentials::OrgCredentials;
pub use error::{OrgCredentialError, OrgDiscoveryError, OrgSdkError};
pub(crate) use lease::OrgAudienceLeases;

mod client;
pub use client::OrgClient;

#[cfg(test)]
mod tests;

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
