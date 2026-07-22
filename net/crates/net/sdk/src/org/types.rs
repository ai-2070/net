//! The canonical OA type spine (OSDK) — re-exports of the core organization
//! authority primitives.
//!
//! Nothing here is invented by the SDK: these are the types the substrate signs,
//! verifies, and admits on. The facade ([`OrgCredentials`](super::OrgCredentials),
//! [`OrgClient`](super::OrgClient), …) is a verb layer over exactly these.
//!
//! Every name is ALSO re-exported at `net_sdk::org::*`, so both
//! `net_sdk::org::OrgMembershipCert` and `net_sdk::org::types::OrgMembershipCert`
//! resolve — the flat paths predate the module split and stay valid.

/// OA-1 belonging: org root keys, membership certificates, and revocation-floor
/// bundles.
///
/// Note [`OrgError`] is the canonical **issuance / verification** error of these
/// primitives. The facade's own error is the distinct
/// [`OrgSdkError`](super::OrgSdkError) — this name is not shadowed.
pub use net::adapter::net::behavior::org::{
    OrgError, OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle, MAX_ORG_CERT_TTL_SECS,
    MAX_REVOCATION_FLOORS_PER_BUNDLE, ORG_CERT_SIG_DOMAIN, ORG_CERT_TTL_SECS_RECOMMENDED,
    ORG_FLOORS_SIG_DOMAIN,
};

/// OA-2 admission grants (§2.1–2.2) — the offline-issued authorization
/// credentials that back a protected call. A [`OrgDispatcherGrant`] (A→S)
/// empowers a dispatcher entity to act for its org over a capability; a
/// [`OrgCapabilityGrant`] (B→A) empowers a grantee org to discover and/or invoke
/// a capability on B-owned providers, keyed by [`CapabilityAuthorityId`] and
/// scoped by [`GrantRights`] / [`GrantTargetScope`].
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
/// (`OrgError::ClockSkewTooLarge` above it) — the token module's constant,
/// re-exported beside the cert API it gates.
pub use net::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS;

/// The node-side authority scaffold `net node adopt` provisions: membership,
/// owner-audience credential, and the persisted revocation state.
pub use net::adapter::net::behavior::org_authority::{
    authority_dir, NodeAuthority, NodeAuthorityConfig, OrgAuthorityError, OwnerAudienceCredential,
    NODE_AUTHORITY_CONFIG_VERSION, OWNER_AUDIENCE_FILE, OWNER_MEMBERSHIP_FILE,
    REVOCATION_STATE_FILE,
};

/// Membership revocation: signed floor bundles merged into a restart-durable
/// maxima store.
pub use net::adapter::net::behavior::org_revocation::{
    OrgRevocationError, OrgRevocationState, OrgRevocationStore, ORG_REVOCATION_STATE_VERSION,
};

/// OA-2 admission results and modes.
///
/// [`Admitted`] is the provider-verified fact set a protected handler receives —
/// the facade's [`OrgCaller`](super::OrgCaller) is an exact projection of it.
/// [`OrgAdmission`] is the canonical mode enum ([`OrgAccess`](super::OrgAccess)
/// is its human-facing facade name). [`CoarseAdmissionReason`] is the
/// three-bucket wire reason a denial carries; the detailed
/// [`AdmissionDenied`](net::adapter::net::behavior::org_admission::AdmissionDenied)
/// stays provider-side audit only, deliberately, so denials are not a credential
/// oracle.
// `OrgAccess` / `OrgCaller` are the `cortex`-gated serve-verb facade names; this
// doc is always compiled, so the links are live-but-unresolvable in a
// `net`-without-`cortex` build. Relax the check ONLY there — a `cortex` build
// (the default and CI's `--features full`) resolves and fully checks them.
#[cfg_attr(not(feature = "cortex"), allow(rustdoc::broken_intra_doc_links))]
pub use net::adapter::net::behavior::org_admission::{
    Admitted, CoarseAdmissionReason, OrgAdmission,
};

/// Registration visibility: `Public` plaintext CAP-ANN vs the two ENCRYPTED-only
/// private forms. [`OrgAccess`](super::OrgAccess) selects the private form
/// implicitly (access implies visibility); this enum is the canonical name the
/// low-level serve APIs take.
// `super::OrgAccess` is `cortex`-gated (see above); relax the link check only in
// a `net`-without-`cortex` doc build.
#[cfg_attr(not(feature = "cortex"), allow(rustdoc::broken_intra_doc_links))]
pub use net::adapter::net::org_admission_gate::CapabilityVisibility;

/// Why installing a grant audience was refused — surfaced by the facade when a
/// DISCOVER grant cannot be installed at bind time.
pub use net::adapter::net::behavior::org_grant_registry::GrantAudienceInstallError;

/// The low-level caller escape hatch (OA2-E2). Everything
/// [`OrgClient`](super::OrgClient) does is expressible by hand through this on
/// [`CallOptions`](net::adapter::net::mesh_rpc::CallOptions) — the facade builds
/// it for you, and advanced callers that need an exact provider, a specific
/// grant, or an unusual TTL keep using it directly.
pub use net::adapter::net::mesh_rpc::OrgProofIntent;
