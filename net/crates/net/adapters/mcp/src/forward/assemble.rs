//! Caller-side assembly of a [`ForwardedContext`] from resolved sends
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` **Phase 2**, caller side).
//!
//! The caller pipeline is **resolve → assemble → seal**:
//!
//! 1. [`resolve_secret_send`](super::resolve_secret_send) turns a policy-allowed
//!    secret ref into a `(HeaderName, ForwardedHeaderValue)` — one per ref.
//! 2. [`ForwardedContextBuilder`] collects those (plus any allowed plain
//!    headers) for one destination + invocation, stamps a fresh random nonce
//!    and a short expiry, and validates.
//! 3. A [`ForwardedContextSealer`](super::ForwardedContextSealer) seals the
//!    result to the destination node's key.
//!
//! The builder generates the single-use nonce itself (via `getrandom`) so a
//! caller can't forget it, and takes `issued_at` as an argument rather than
//! reading a clock — that keeps it testable and leaves the one time source at
//! the call site.

use std::collections::BTreeMap;

use super::context::{ContextError, ForwardedContext};
use super::header::{ForwardedHeaderValue, HeaderName};
use super::{DEFAULT_TTL_SECS, MAX_TTL_SECS};

/// Why assembling a context failed.
#[derive(Debug, thiserror::Error)]
pub enum AssembleError {
    /// No headers were added — there is nothing to forward.
    #[error("no headers to forward")]
    Empty,
    /// The OS RNG failed while generating the nonce.
    #[error("nonce generation failed: {0}")]
    Rng(String),
    /// The assembled context failed validation.
    #[error(transparent)]
    Context(#[from] ContextError),
}

/// Builds a [`ForwardedContext`] for one destination + invocation.
///
/// Bind the destination, caller origin, capability, and invocation up front
/// (they're what the seal authenticates), add resolved headers, then
/// [`build`](Self::build). Not `Clone` — it holds secret values, which the
/// wrapper type deliberately makes uncloneable.
#[derive(Debug)]
pub struct ForwardedContextBuilder {
    sealed_to: String,
    caller_origin: String,
    capability_id: String,
    invocation_id: String,
    ttl_secs: u64,
    headers: BTreeMap<HeaderName, ForwardedHeaderValue>,
}

impl ForwardedContextBuilder {
    /// Start a builder for a forward to `sealed_to`, on behalf of
    /// `caller_origin`, for `capability_id` and this `invocation_id`. TTL
    /// defaults to [`DEFAULT_TTL_SECS`].
    pub fn new(
        sealed_to: impl Into<String>,
        caller_origin: impl Into<String>,
        capability_id: impl Into<String>,
        invocation_id: impl Into<String>,
    ) -> Self {
        Self {
            sealed_to: sealed_to.into(),
            caller_origin: caller_origin.into(),
            capability_id: capability_id.into(),
            invocation_id: invocation_id.into(),
            ttl_secs: DEFAULT_TTL_SECS,
            headers: BTreeMap::new(),
        }
    }

    /// Override the TTL. Clamped to `[1, MAX_TTL_SECS]` at
    /// [`build`](Self::build) — sealed bearer material is never meant to live
    /// long, so an over-long request is trimmed, not honored.
    pub fn ttl_secs(mut self, ttl_secs: u64) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    /// Add a resolved header. A later add of the same (canonical) name replaces
    /// the earlier value.
    pub fn header(mut self, name: HeaderName, value: ForwardedHeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Whether any header has been added.
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Finalize: stamp a fresh random nonce, set `expires_at = issued_at + ttl`
    /// (TTL clamped to `[1, MAX_TTL_SECS]`), and validate. `issued_at` is unix
    /// seconds from the caller's clock.
    pub fn build(self, issued_at: u64) -> Result<ForwardedContext, AssembleError> {
        if self.headers.is_empty() {
            return Err(AssembleError::Empty);
        }
        let ttl = self.ttl_secs.clamp(1, MAX_TTL_SECS);
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| AssembleError::Rng(e.to_string()))?;
        let ctx = ForwardedContext::new(
            self.sealed_to,
            self.caller_origin,
            self.capability_id,
            self.invocation_id,
            issued_at,
            issued_at.saturating_add(ttl),
            nonce,
            self.headers,
        );
        ctx.validate()?;
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::{
        ForwardedContextOpener, ForwardedContextSealer, X25519SealedBoxOpener,
        X25519SealedBoxSealer,
    };

    fn hv(s: &str) -> ForwardedHeaderValue {
        ForwardedHeaderValue::new(s.as_bytes().to_vec()).unwrap()
    }

    fn name(s: &str) -> HeaderName {
        HeaderName::parse(s).unwrap()
    }

    #[test]
    fn builds_a_valid_context_with_expiry_and_nonce() {
        let ctx = ForwardedContextBuilder::new("node-dest", "origin", "github.issues", "inv-1")
            .header(name("Authorization"), hv("Bearer t"))
            .build(1_000)
            .unwrap();
        assert_eq!(ctx.sealed_to, "node-dest");
        assert_eq!(ctx.expires_at, 1_000 + DEFAULT_TTL_SECS);
        assert!(ctx.declared_names.contains(&name("authorization")));
    }

    #[test]
    fn empty_builder_is_rejected() {
        let err = ForwardedContextBuilder::new("d", "o", "c", "i")
            .build(1)
            .unwrap_err();
        assert!(matches!(err, AssembleError::Empty));
    }

    #[test]
    fn ttl_is_clamped_to_the_maximum() {
        let ctx = ForwardedContextBuilder::new("d", "o", "c", "i")
            .ttl_secs(MAX_TTL_SECS * 10)
            .header(name("X-Trace-Id"), hv("abc"))
            .build(0)
            .unwrap();
        assert_eq!(ctx.ttl_secs(), MAX_TTL_SECS, "over-long TTL is trimmed");
    }

    #[test]
    fn each_build_stamps_a_fresh_nonce() {
        let mk = || {
            ForwardedContextBuilder::new("d", "o", "c", "i")
                .header(name("X-Trace-Id"), hv("abc"))
                .build(0)
                .unwrap()
                .nonce
        };
        assert_ne!(mk(), mk(), "nonces must be unique per assembly");
    }

    #[tokio::test]
    async fn assemble_seal_open_round_trips() {
        // The whole caller→destination path with the real cipher.
        let opener = X25519SealedBoxOpener::from_secret_bytes([3u8; 32]);
        let ctx = ForwardedContextBuilder::new("node-dest", "origin", "github.issues", "inv-1")
            .header(name("Authorization"), hv("Bearer s3cret"))
            .header(name("X-Tenant-Id"), hv("acme"))
            .build(1_000)
            .unwrap();
        let sealed = X25519SealedBoxSealer::to_recipient(opener.public_key())
            .seal(&ctx)
            .await
            .unwrap();
        let opened = opener.open(&sealed, "node-dest", 1_005).await.unwrap();
        assert_eq!(
            opened.header(&name("Authorization")).unwrap().expose(),
            b"Bearer s3cret"
        );
    }
}
