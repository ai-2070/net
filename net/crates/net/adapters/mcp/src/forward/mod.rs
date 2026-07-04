//! Forwarded invocation context — the spec-only foundation for opt-in,
//! filtered, deny-by-default credential & header forwarding
//! (`docs/plans/MCP_CREDENTIAL_FORWARDING_PLAN.md`, **Phase 0**).
//!
//! # Posture
//!
//! Net's default is credential locality: secrets live on the machine that owns
//! the tool and never transit. Forwarding inverts that for services that only
//! understand bearer auth — a **tagged concession**, not a headline feature.
//! Replayable secrets re-enter transit, so every default here is hostile and
//! both ends must opt in. Preference order stays: provider-held credentials >
//! Net delegation/identity > forwarded credentials.
//!
//! # What Phase 0 is (and is not)
//!
//! This module is **spec only**. It defines the object, its canonical
//! encoding, the policy schema, the secret wrapper type, the risk tag, and the
//! never-for-stdio doctrine — and it *forwards nothing*. There is no sealing,
//! no injection, no secret store, no wire path. It exists so later bridge work
//! can't smuggle in "just forward `Authorization`" under deadline: every route
//! to a forwarded value has to go through these types, and every one of them
//! is hostile by default.
//!
//! - [`ForwardedContext`] — the `net.invoke.forwarded_context@1` object plus
//!   its [`canonical_aad`](ForwardedContext::canonical_aad) binding and
//!   [`validate`](ForwardedContext::validate) rules.
//! - [`ForwardedHeaderValue`] — the secret wrapper: redacted, unserializable,
//!   exposed only at the injection boundary.
//! - [`HeaderName`] — canonicalized, classified header names.
//! - [`ForwardingConfig`] / [`AcceptPolicy`] — caller-side and destination-side
//!   deny-by-default policy, with a [`DenialLevel`] that names the gate that
//!   refused (never a value).
//! - [`WrapTransport`] / [`resolve_injection`] — the never-for-stdio guard, and
//!   [`RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS`] for honest labeling.
//! - [`ForwardingStore`] (**Phase 1**) — the persistent caller-side *policy*
//!   store and its redaction-safe [`ForwardingAudit`]. It records destination
//!   bindings, never secret values; the value backend (keychain / encrypted
//!   store) and the CLI verbs (`net secret`, `net security audit`) build on it.
//! - [`SecretBackend`] / [`resolve_secret_send`] (**Phase 1**, value side) — the
//!   value-storage seam and the resolver that applies policy *before* touching
//!   the backend and materializes a value only as a [`ForwardedHeaderValue`].
//!   [`InMemorySecretBackend`] is the ephemeral default; persistent backends
//!   plug in behind the trait.
//!
//! # Threat model (honest section)
//!
//! Defends against: prompt-injected sends to the wrong provider (destination
//! binding in the AAD), relay observation (values never leave the sealed
//! payload / never enter the AAD), replay (invocation + expiry binding),
//! stealth acceptance (accept-lists + auto-tagging), and value leakage via
//! logs / `Debug` / serialization (the wrapper type). Does **not** defend
//! against: a destination leaking a header after injection, an upstream
//! logging `Authorization`, a user deliberately granting a secret to a
//! malicious provider, or a compromised endpoint machine.
//!
//! # Where this lives
//!
//! The object rides in the MCP adapter for Phase 0 because the bridge is the
//! pressure point the plan exists to hold. If native (non-MCP) capabilities
//! later need forwarding (plan Phase 2), these types promote to a shared crate
//! unchanged — they take no MCP dependency.

mod context;
mod header;
mod policy;
mod secret;
mod store;
mod target;

pub use context::{ContextError, ForwardedContext};
pub use header::{
    ForwardedHeaderValue, HeaderError, HeaderName, MAX_FORWARDED_HEADERS, MAX_HEADER_VALUE_LEN,
    MAX_TOTAL_FORWARDED_BYTES,
};
pub use policy::{
    AcceptError, AcceptPolicy, AllowList, DenialLevel, ForwardingConfig, PlainHeaderPolicy,
    ProviderScope, SecretPolicy, SendGrant,
};
pub use secret::{
    resolve_secret_send, InMemorySecretBackend, ResolveError, SecretBackend, SecretBackendError,
};
pub use store::{ForwardingAudit, ForwardingStore, Grant, GrantKind, StoreError};
pub use target::{
    forwarding_supported, resolve_injection, risk_tags_for_accept_policy, ForwardingUnsupported,
    InjectionTarget, WrapTransport, RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS,
};

/// The forwarded-context object's type name, without version.
pub const OBJECT_TYPE: &str = "net.invoke.forwarded_context";

/// The forwarded-context object's schema version.
pub const OBJECT_VERSION: u32 = 1;

/// The fully-qualified, versioned object tag (`net.invoke.forwarded_context@1`).
/// Bound into the canonical AAD as a domain separator.
pub const OBJECT_TAG: &str = "net.invoke.forwarded_context@1";

/// Default forwarded-context TTL. Short by design: sealed bearer material is
/// never meant to be valid at rest, and expiry is the backstop for the day an
/// invocation-id cache misbehaves.
pub const DEFAULT_TTL_SECS: u64 = 30;

/// Hard cap on a forwarded-context TTL. A context asking for more is refused
/// by [`ForwardedContext::validate`].
pub const MAX_TTL_SECS: u64 = 300;
