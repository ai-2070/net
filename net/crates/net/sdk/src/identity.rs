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
    EntityError, EntityId, EntityKeypair, IdentityState, IdentityStateError, OriginStamp,
    PermissionToken, TokenCache, TokenError, TokenScope, IDENTITY_STATE_SIZE,
    IDENTITY_STATE_VERSION, MAX_TOKEN_TTL_SECS, TOKEN_CLOCK_SKEW_SECS_RECOMMENDED,
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
    /// This issuer's credential epoch. Stamped onto every token this
    /// identity issues or delegates.
    ///
    /// Not behind an `Arc`, and deliberately not mutable: rotating
    /// produces a *new* `Identity` for the same key
    /// ([`Identity::at_generation`]). A stale clone can therefore still
    /// mint at `N - 1` after a rotation to `N`. That is an availability
    /// failure — verifiers past floor `N` reject those tokens — not an
    /// authority bypass, and it is the cheaper mistake than having a
    /// clone in another thread silently change what a caller is signing.
    generation: u32,
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
    ///
    /// **Key-only restoration resets the issuer generation to zero and
    /// is not sufficient to restore an issuer after generation
    /// rotation.** The seed carries no epoch, so an issuer that has
    /// rotated to generation `N` and comes back through this path
    /// starts minting at `0` again — below its own published floor, so
    /// every token it signs is rejected, and it cannot climb back
    /// without knowing `N`. Use [`Self::to_state_bytes`] /
    /// [`Self::from_state_bytes`] for anything that rotates.
    pub fn to_bytes(&self) -> [u8; 32] {
        *self.keypair.secret_bytes()
    }

    /// Load a previously-serialized identity. Expects exactly 32
    /// bytes — the ed25519 seed — otherwise returns
    /// [`TokenError::InvalidFormat`].
    ///
    /// Same caveat as [`Self::to_bytes`]: this restores the key, not
    /// the issuer. Generation comes back as zero.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TokenError> {
        if bytes.len() != 32 {
            return Err(TokenError::InvalidFormat);
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(bytes);
        Ok(Self::from_seed(seed))
    }

    /// This issuer's current credential epoch.
    ///
    /// Every token [`Self::issue_token`] and the delegation builders
    /// produce carries this value, and a verifier rejects it once its
    /// `RevocationRegistry` floor for this entity exceeds it.
    ///
    /// Unlinked deliberately: `crate::revocation` uses the type without
    /// re-exporting it, and the path that does re-export it
    /// (`crate::delegation`) is feature-gated, so a link here resolves
    /// under some feature sets and not others.
    pub fn issuer_generation(&self) -> u32 {
        self.generation
    }

    /// The same key at a later generation.
    ///
    /// Returns a *new* `Identity`; this one is unchanged, and so is
    /// every other clone of it. Rotation is therefore explicit at the
    /// call site rather than something a background thread can do to a
    /// caller mid-issuance.
    ///
    /// `next == issuer_generation()` is accepted and idempotent, so
    /// re-applying a persisted generation on restart is not an error.
    /// Going backwards is rejected — it would re-mint at an epoch the
    /// issuer has already retired.
    ///
    /// # Rotation order
    ///
    /// 1. construct the generation-`N` identity here;
    /// 2. persist [`Self::to_state_bytes`] atomically and durably;
    /// 3. distribute verifier floor `N`;
    /// 4. start issuing from the returned identity.
    ///
    /// Never publish floor `N` before step 2 completes. A crash between
    /// them leaves an issuer that has announced a floor it has no
    /// durable state to satisfy, and it cannot mint anything a verifier
    /// will accept — permanent self-revocation, recoverable only by
    /// rotating the key.
    pub fn at_generation(&self, next: u32) -> Result<Self, IdentityStateError> {
        let generation = IdentityState::check_rotation(self.generation, next)?;
        Ok(Self {
            keypair: self.keypair.clone(),
            cache: self.cache.clone(),
            generation,
        })
    }

    /// Serialize the full issuer state: version, seed, generation.
    ///
    /// **Secret material** — these bytes contain the ed25519 signing
    /// seed, exactly like [`Self::to_bytes`]. Encrypt at rest, and
    /// write atomically (temp file + rename, or whatever your vault
    /// offers): a torn write here is an issuer that cannot come back.
    ///
    /// The token cache is not included; it is runtime state, and its
    /// entries are other issuers' grants rather than this issuer's.
    pub fn to_state_bytes(&self) -> [u8; IDENTITY_STATE_SIZE] {
        IdentityState {
            seed: *self.keypair.secret_bytes(),
            generation: self.generation,
        }
        .to_bytes()
    }

    /// Restore an issuer — key *and* generation — from
    /// [`Self::to_state_bytes`].
    ///
    /// This is the restart path for anything that rotates. A restored
    /// identity mints at the generation it was persisted with, so it
    /// satisfies the floor it published before going down.
    pub fn from_state_bytes(bytes: &[u8]) -> Result<Self, IdentityStateError> {
        let state = IdentityState::from_bytes(bytes)?;
        Ok(Self {
            keypair: Arc::new(EntityKeypair::from_bytes(state.seed)),
            cache: Arc::new(TokenCache::new()),
            generation: state.generation,
        })
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
    /// minimum non-born-expired TTL), and a `ttl` longer than
    /// [`MAX_TOKEN_TTL_SECS`] is soft-clamped down to that ceiling.
    /// Both keep this infallible surface non-panicking: `try_issue`
    /// rejects an over-long TTL with `TokenError::TtlTooLong`, which
    /// the `.expect()` below would otherwise turn into a process
    /// abort. In debug builds a `debug_assert!` fires so either misuse
    /// surfaces in tests; in release the SDK keeps a non-panicking
    /// surface for callers that may receive an out-of-range value from
    /// upstream configuration. Callers that need to *reject* these at
    /// the boundary should use [`Self::try_issue_token`], which returns
    /// `TokenError::ZeroTtl` / `TokenError::TtlTooLong`.
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
        debug_assert!(
            ttl.as_secs() <= MAX_TOKEN_TTL_SECS,
            "Identity::issue_token called with ttl > MAX_TOKEN_TTL_SECS ({MAX_TOKEN_TTL_SECS}s); \
             release builds soft-clamp to the ceiling, but the call site is likely a bug"
        );
        let effective_ttl = if ttl.is_zero() {
            Duration::from_secs(1)
        } else {
            // Clamp to the issuance ceiling so the infallible wrapper
            // can't panic on the `TtlTooLong` that `try_issue` returns
            // past `MAX_TOKEN_TTL_SECS`.
            Duration::from_secs(ttl.as_secs().min(MAX_TOKEN_TTL_SECS))
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
        PermissionToken::try_issue_with_generation(
            &self.keypair,
            self.generation,
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
            // Key-only construction has no epoch to restore. See
            // `to_bytes` for why that matters after a rotation.
            generation: 0,
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

    /// Security-review follow-up: a `ttl` past `MAX_TOKEN_TTL_SECS`
    /// used to reach `try_issue`, which (after audit H3) returns
    /// `TokenError::TtlTooLong` — and the infallible wrapper's
    /// `.expect()` would have turned that into a process abort. The
    /// wrapper now soft-clamps down to the ceiling, mirroring the
    /// zero-TTL soft-clamp. Release-gated like its zero-TTL sibling
    /// because the `debug_assert!` fires under `cargo test`.
    #[cfg(not(debug_assertions))]
    #[test]
    fn issue_token_over_long_ttl_soft_clamps_in_release() {
        let id = Identity::generate();
        let subject = Identity::generate();
        let channel = ChannelName::new("long-ttl-soft-clamp").unwrap();
        let token = id.issue_token(
            subject.entity_id().clone(),
            crate::TokenScope::PUBLISH,
            &channel,
            // 10x the ceiling — the old saturating path would have
            // produced a near-immortal token; clamp caps it.
            Duration::from_secs(MAX_TOKEN_TTL_SECS * 10),
            0,
        );
        assert!(
            token.not_after < u64::MAX,
            "clamped TTL must not saturate not_after"
        );
        assert!(
            token.verify().is_ok(),
            "clamped TTL must produce a verify-ok token"
        );
        assert!(
            token.is_valid().is_ok(),
            "clamped TTL must be live at issue time"
        );
    }

    /// Companion to the above: in debug builds the over-long soft-clamp
    /// fires `debug_assert!` so the misuse surfaces in tests.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "MAX_TOKEN_TTL_SECS")]
    fn issue_token_over_long_ttl_debug_asserts() {
        let id = Identity::generate();
        let subject = Identity::generate();
        let channel = ChannelName::new("long-ttl-debug").unwrap();
        let _ = id.issue_token(
            subject.entity_id().clone(),
            crate::TokenScope::PUBLISH,
            &channel,
            Duration::from_secs(MAX_TOKEN_TTL_SECS * 10),
            0,
        );
    }

    /// The fallible surface rejects an over-long TTL with
    /// `TokenError::TtlTooLong` rather than clamping — the boundary
    /// path FFI bindings route through.
    #[test]
    fn try_issue_token_over_long_ttl_returns_ttl_too_long() {
        let id = Identity::generate();
        let subject = Identity::generate();
        let channel = ChannelName::new("long-ttl-fallible").unwrap();
        let err = id
            .try_issue_token(
                subject.entity_id().clone(),
                crate::TokenScope::PUBLISH,
                &channel,
                Duration::from_secs(MAX_TOKEN_TTL_SECS + 1),
                0,
            )
            .unwrap_err();
        assert!(
            matches!(err, TokenError::TtlTooLong),
            "expected TtlTooLong, got {err:?}"
        );
    }
}
