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
