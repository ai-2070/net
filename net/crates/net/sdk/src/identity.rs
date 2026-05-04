//! Identity handle — keypair + token cache.
//!
//! Built once at node start, handed to [`crate::NetBuilder::identity`]
//! or [`crate::MeshBuilder::identity`]. Owns the ed25519 signing key;
//! the transport borrows it for `OriginStamp` derivation, event
//! signing, and token-gated subscribe checks.
//!
//! `Identity` is cheap to clone (both the keypair and the token cache
//! are held behind `Arc`). Clone and share between threads freely.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//! use net_sdk::{Identity, TokenScope};
//! use net_sdk::ChannelName;
//!
//! // Two entities — a publisher issuing a subscribe grant to a
//! // subscriber it trusts.
//! let publisher = Identity::generate();
//! let subscriber = Identity::generate();
//!
//! let channel = ChannelName::new("sensors/temp").unwrap();
//! let token = publisher.issue_token(
//!     subscriber.entity_id().clone(),
//!     TokenScope::SUBSCRIBE,
//!     &channel,
//!     Duration::from_secs(300),
//!     0, // delegation depth — 0 disallows re-delegation
//! );
//!
//! // Full round-trip: signature verifies against the issuer's key,
//! // install stores it in the subscriber's cache, lookup returns it.
//! assert!(token.verify().is_ok());
//! subscriber.install_token(token.clone()).unwrap();
//! let cached = subscriber.lookup_token(subscriber.entity_id(), &channel);
//! assert!(cached.is_some());
//! ```
//!
//! # Persistence
//!
//! Treat the bytes from [`Identity::to_bytes`] as secret material —
//! they're the 32-byte ed25519 seed. Typical flow: generate once on
//! first run, write-encrypted to disk (or a vault / enclave / k8s
//! secret), reload with [`Identity::from_bytes`] on every subsequent
//! start. The SDK never touches a hardcoded path — where the bytes
//! live is the caller's call.

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::channel::ChannelName;

// Re-export of core identity primitives so users can import directly
// from `net_sdk::identity::*` instead of reaching into the core crate.
pub use net::adapter::net::identity::{
    EntityError, EntityId, EntityKeypair, OriginStamp, PermissionToken, TokenCache, TokenError,
    TokenScope,
};

/// Caller-owned identity bundle: one ed25519 keypair + one token
/// cache.
///
/// See the [module docs](self) for generation / persistence / issuance
/// semantics.
#[derive(Clone, Debug)]
pub struct Identity {
    keypair: Arc<EntityKeypair>,
    cache: Arc<TokenCache>,
}

impl Identity {
    /// Generate a fresh ed25519 identity.
    ///
    /// Use once at first-run; persist the returned bytes via
    /// [`Self::to_bytes`] and reload with [`Self::from_bytes`] on
    /// subsequent runs. Every call to `generate()` produces a *new*
    /// entity id — don't call it on every startup unless you actually
    /// want a fresh identity (you almost never do).
    pub fn generate() -> Self {
        Self::from_keypair(EntityKeypair::generate())
    }

    /// Load from a caller-owned 32-byte ed25519 seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self::from_keypair(EntityKeypair::from_bytes(seed))
    }

    /// Serialize the identity as its 32-byte seed. Token cache entries
    /// are runtime-only and not serialized — reinstall any long-lived
    /// grants via [`Self::install_token`] after reloading.
    pub fn to_bytes(&self) -> [u8; 32] {
        *self.keypair.secret_bytes()
    }

    /// Load a previously-serialized identity. Expects exactly 32
    /// bytes — the ed25519 seed — otherwise returns
    /// [`TokenError::InvalidFormat`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TokenError> {
        if bytes.len() != 32 {
            return Err(TokenError::InvalidFormat);
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(bytes);
        Ok(Self::from_seed(seed))
    }

    /// Ed25519 public key. 32 bytes.
    pub fn entity_id(&self) -> &EntityId {
        self.keypair.entity_id()
    }

    /// Derived 64-bit hash used in packet headers (`OriginStamp`).
    pub fn origin_hash(&self) -> u64 {
        self.keypair.origin_hash()
    }

    /// Derived 64-bit node id used for routing / addressing.
    pub fn node_id(&self) -> u64 {
        self.keypair.node_id()
    }

    /// Sign arbitrary bytes. Typically used by the transport to sign
    /// `CapabilityAnnouncement`s; exposed here so callers can sign
    /// their own out-of-band messages with the same identity.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.keypair.sign(message).to_bytes()
    }

    /// Issue a scoped permission token to `subject`.
    ///
    /// Short TTLs + periodic re-issuance is the designed v1 answer to
    /// revocation — a [`PermissionToken`] has no CRL lookup. Pick
    /// TTLs that match how long you'd tolerate a compromised token
    /// being valid.
    ///
    /// `delegation_depth = 0` disallows re-delegation (subject cannot
    /// mint further tokens from this one).
    ///
    /// `ttl == Duration::ZERO` is soft-clamped to 1 second (the
    /// minimum non-born-expired TTL). In debug builds a
    /// `debug_assert!` fires so the misuse surfaces in tests; in
    /// release the SDK keeps a non-panicking surface for callers
    /// that may receive a zero-duration value from upstream
    /// configuration. Callers that need to *reject* zero TTLs at
    /// the boundary should use [`Self::try_issue_token`], which
    /// returns `TokenError::ZeroTtl`.
    pub fn issue_token(
        &self,
        subject: EntityId,
        scope: TokenScope,
        channel: &ChannelName,
        ttl: Duration,
        delegation_depth: u8,
    ) -> PermissionToken {
        debug_assert!(
            !ttl.is_zero(),
            "Identity::issue_token called with Duration::ZERO; \
             release builds soft-clamp to 1s, but the call site is likely a bug"
        );
        let effective_ttl = if ttl.is_zero() {
            Duration::from_secs(1)
        } else {
            ttl
        };
        self.try_issue_token(subject, scope, channel, effective_ttl, delegation_depth)
            .expect("Identity::issue_token: invalid input (use try_issue_token for fallible)")
    }

    /// Fallible variant of [`Self::issue_token`].
    ///
    /// Returns [`TokenError::ZeroTtl`] when `ttl ==
    /// Duration::ZERO`. Pre-fix this minted a born-expired token
    /// — every receiver rejected it as `Expired` and the issuer
    /// learned about the misuse only by reading log lines on the
    /// receiver side.
    pub fn try_issue_token(
        &self,
        subject: EntityId,
        scope: TokenScope,
        channel: &ChannelName,
        ttl: Duration,
        delegation_depth: u8,
    ) -> Result<PermissionToken, TokenError> {
        PermissionToken::try_issue(
            &self.keypair,
            subject,
            scope,
            channel.hash(),
            ttl.as_secs(),
            delegation_depth,
        )
    }

    /// Install a token received from another issuer — typically a
    /// delegated subscribe / publish grant. The signature is verified
    /// on insert; an invalid token returns
    /// [`TokenError::InvalidSignature`].
    pub fn install_token(&self, token: PermissionToken) -> Result<(), TokenError> {
        self.cache.insert(token)
    }

    /// Look up a cached token by `(subject, channel)`. Sub-microsecond
    /// (DashMap-backed). Returns `None` if no exact-channel token is
    /// cached; the transport's wildcard fallback is handled separately
    /// by [`TokenCache::check`].
    pub fn lookup_token(
        &self,
        subject: &EntityId,
        channel: &ChannelName,
    ) -> Option<PermissionToken> {
        self.cache.get(subject, channel.hash())
    }

    /// Shared reference to the underlying keypair. Used by the mesh
    /// builder to hand the keypair to `MeshNode::new`; most callers
    /// don't need this directly.
    pub fn keypair(&self) -> &Arc<EntityKeypair> {
        &self.keypair
    }

    /// Shared reference to the underlying token cache. Used by the
    /// transport to check subscribe authorizations; most callers
    /// don't need this directly.
    pub fn token_cache(&self) -> &Arc<TokenCache> {
        &self.cache
    }

    fn from_keypair(kp: EntityKeypair) -> Self {
        Self {
            keypair: Arc::new(kp),
            cache: Arc::new(TokenCache::new()),
        }
    }
}

// NOTE: `Identity` deliberately does NOT implement `Default`.
// Returning a fresh random keypair from `default()` would be a
// footgun — any `unwrap_or_default()` or `#[derive(Default)]` on a
// struct containing `Identity` would silently spin up a throwaway
// identity, bypassing the explicit `generate()` / `from_seed()`
// constructors where the docs warn about secret-material handling.
// Callers who want a random identity must call
// [`Identity::generate`] directly; callers restoring from a seed
// call [`Identity::from_seed`].

#[cfg(test)]
mod tests {
    use super::*;

    /// `Identity::issue_token` previously routed through
    /// `try_issue_token(...).expect(...)`, which blew up the
    /// process on `Duration::ZERO` (because `try_issue` returns
    /// `TokenError::ZeroTtl`). The current behaviour soft-clamps
    /// to a 1-second TTL (with a `debug_assert!` to surface the
    /// misuse in tests). Release builds therefore mint a
    /// short-but-valid token instead of process-aborting.
    ///
    /// The `debug_assert!` fires under `cargo test`, so we
    /// exercise the soft-clamp via `release` semantics by
    /// `#[cfg]`-gating off of `debug_assertions`. The assertion
    /// itself is covered by a separate `#[should_panic]` test
    /// below.
    #[cfg(not(debug_assertions))]
    #[test]
    fn issue_token_zero_duration_soft_clamps_in_release() {
        let id = Identity::generate();
        let subject = Identity::generate();
        let channel = ChannelName::new("zero-ttl-soft-clamp").unwrap();
        let token = id.issue_token(
            subject.entity_id().clone(),
            crate::TokenScope::PUBLISH,
            &channel,
            Duration::ZERO,
            0,
        );
        assert!(
            token.verify().is_ok(),
            "soft-clamped 1s TTL must produce a verify-ok token"
        );
        assert!(
            token.is_valid().is_ok(),
            "soft-clamped 1s TTL must be live at issue time"
        );
    }

    /// Companion to the above: in debug builds the soft-clamp
    /// fires `debug_assert!` so the misuse surfaces in tests.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "Duration::ZERO")]
    fn issue_token_zero_duration_debug_asserts() {
        let id = Identity::generate();
        let subject = Identity::generate();
        let channel = ChannelName::new("zero-ttl-debug").unwrap();
        let _ = id.issue_token(
            subject.entity_id().clone(),
            crate::TokenScope::PUBLISH,
            &channel,
            Duration::ZERO,
            0,
        );
    }

    /// `try_issue_token` is the explicit fallible surface — must
    /// reject `Duration::ZERO` with `TokenError::ZeroTtl` rather
    /// than soft-clamping. This is the path FFI bindings route
    /// through; an attempt to mint a zero-TTL token there should
    /// surface as an error to the caller, not be silently
    /// remediated.
    #[test]
    fn try_issue_token_zero_duration_returns_zero_ttl() {
        let id = Identity::generate();
        let subject = Identity::generate();
        let channel = ChannelName::new("zero-ttl-fallible").unwrap();
        let err = id
            .try_issue_token(
                subject.entity_id().clone(),
                crate::TokenScope::PUBLISH,
                &channel,
                Duration::ZERO,
                0,
            )
            .unwrap_err();
        assert!(
            matches!(err, TokenError::ZeroTtl),
            "expected ZeroTtl, got {err:?}"
        );
    }
}
