//! The org proof and admission authority (OA-1/OA-2), ported to run
//! inside a leaf.
//!
//! # Why this exists here at all
//!
//! The authority lives in the core's tokio-linked crate
//! (`behavior/{org,org_grant,org_call,org_admission}.rs`), which a
//! browser cannot build. This is a **faithful port**, not a second
//! implementation with opinions: every wire layout, every signed
//! transcript and every acceptance check is byte-identical to core,
//! so a proof minted by a browser verifies on a native provider and
//! vice versa. The module names and type names mirror core's exactly
//! — one vocabulary across both crates.
//!
//! # The four adaptations (the port MUST NOT weaken authority)
//!
//! 1. **Time is a parameter.** Core reads `SystemTime::now()` — which
//!    compiles on `wasm32` and then panics. Every function here takes
//!    its `now` explicitly (`now_unix_secs` / `now_unix_ns` for wall
//!    clock, `now_mono_ms` for the replay guard's monotonic
//!    timeline); nothing in `src/org/` reads a clock.
//! 2. **Randomness is a parameter.** Core calls `getrandom::fill` and
//!    `std::process::abort()` on failure. Issue functions take the
//!    bytes instead (`nonce: u64`, `grant_id: [u8; 32]`,
//!    `audience_random: Option<[u8; 64]>`) — the host owns entropy,
//!    and no path here can abort a tab.
//! 3. **Revocation arrives as facts.** Core's
//!    `org_revocation.rs` is a filesystem store with `Condvar`s; the
//!    verify-time query surface it actually serves the admission
//!    engine is [`revocation::RevocationFacts`] — merged raise-only
//!    floors plus the publish epoch and store health, fed over the
//!    leaf's control channel.
//! 4. **Single-threaded interior mutability.** The replay guard and
//!    failure limiter use `core::cell::RefCell` / `Cell` instead of
//!    `parking_lot` — the leaf has one thread and no runtime.
//!
//! # One ed25519 implementation
//!
//! Org roots and entity keys are the same primitive, so everything
//! signs through [`crate::identity::EntityKeypair`] and verifies
//! through [`crate::identity::verify_entity_signature`] (`verify_strict`
//! — the lax `verify` admits the malleable `(R, S + L)` variant).
//! The [`entity::EntityId`] layer wraps those; it re-implements no
//! cryptography.

/// The hard clock-skew ceiling, in seconds (300 = 5 minutes).
///
/// Core's `identity/token.rs` constant, ported at the same value
/// because it is load-bearing in two places: every credential window
/// check refuses a larger tolerance
/// ([`cert::OrgError::ClockSkewTooLarge`]), and the replay guard
/// retains a used call binding for **this** allowance past proof
/// expiry regardless of the caller's smaller `skew_secs` — widening
/// skew must never re-open an already-used proof.
pub const MAX_TOKEN_CLOCK_SKEW_SECS: u64 = 300;

pub mod admission;
pub mod cert;
pub mod digest;
pub mod entity;
pub mod grant;
pub mod proof;
pub mod replay;
pub mod revocation;

pub use admission::{
    verify_org_admission, Admitted, AdmissionContext, AdmissionDenied, CoarseAdmissionReason,
    OrgAdmission,
};
pub use cert::{
    OrgError, OrgFloor, OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle,
    ORG_CERT_SIG_DOMAIN, ORG_CERT_TTL_SECS_RECOMMENDED, ORG_FLOORS_SIG_DOMAIN,
    MAX_ORG_CERT_TTL_SECS, MAX_REVOCATION_FLOORS_PER_BUNDLE,
};
pub use digest::{org_request_digest, ORG_RPC_REQUEST_DIGEST_CONTEXT};
pub use entity::{EntityError, EntityId};
pub use grant::{
    audience_key_commitment, CapabilityAuthorityId, DispatcherScope, GrantedDiscoveryBinding,
    GrantRights, GrantTargetScope, OrgAudienceSecret, OrgCapabilityGrant, OrgDispatcherGrant,
    AUDIENCE_COMMIT_CONTEXT, CAPABILITY_AUTHORITY_CONTEXT, ORG_AUDIENCE_SECRET_VERSION,
    ORG_CAPABILITY_GRANT_SIG_DOMAIN, ORG_DISPATCHER_GRANT_SIG_DOMAIN, MAX_ORG_GRANT_TTL_SECS,
};
pub use proof::{
    check_proof_expiry_at, CallBinding, OrgCallProof, OrgStreamCallProof, RpcCallShape,
    StreamCallBinding, MAX_ORG_CALL_PROOF_BYTES, MAX_ORG_PROOF_TTL_SECS, ORG_ADMISSION_HEADER,
    ORG_CALL_BINDING_CONTEXT, ORG_STREAM_CALL_BINDING_CONTEXT, STREAM_CALL_KIND_CLIENT_STREAMING,
    STREAM_CALL_KIND_DUPLEX, STREAM_CALL_KIND_SERVER_STREAMING,
};
pub use replay::{
    AdmissionFailureLimiter, AdmissionRateLimitConfig, AdmissionReplayConfig, AdmissionReplayGuard,
    ReplayConfigError, ReplayOutcome, ReplayPrincipal, DEFAULT_MAX_FAILED_ADMISSIONS_PER_PEER,
    DEFAULT_MAX_RATE_LIMITED_PEERS, DEFAULT_MAX_REPLAY_ENTRIES,
    DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER, DEFAULT_MAX_REPLAY_ENTRIES_PER_EXTERNAL_ORG,
    DEFAULT_OWNER_RESERVED_REPLAY_ENTRIES, DEFAULT_FAILED_ADMISSION_REFILL_PER_SEC,
};
pub use revocation::RevocationFacts;
