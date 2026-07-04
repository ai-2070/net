//! Sealed forwarded-context wire object + the seal/open crypto seam
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` **Phase 2**, spec + seam).
//!
//! [`ForwardedContext`] is the in-memory object; [`SealedContext`] is what
//! actually travels: the authenticated **cleartext envelope** (destination,
//! caller, capability, invocation, declared names, expiry, nonce) plus the
//! **encrypted header payload**. The header values live only inside
//! `ciphertext`; every other field is cleartext and bound into the AEAD's
//! associated data ([`SealedContext::aad`], byte-identical to the source's
//! [`ForwardedContext::canonical_aad`]). So a relay sees no value, and a
//! tampered envelope field fails the open.
//!
//! ## The seam
//!
//! [`ForwardedContextSealer`] and [`ForwardedContextOpener`] are the crypto
//! boundary. The actual AEAD — sealed to the destination node's key, per the
//! plan — is deliberately **not** implemented here: the SDK exposes no sealing
//! primitive yet, and picking one (a crypto crate vs. an SDK primitive over the
//! core's in-tree XChaCha20-Poly1305) is a decision, not a default. This module
//! defines the shapes, the fixed open-order invariants, and a plumbing-level
//! conformance test; the cipher slots in behind the traits.
//!
//! ## Open order (fixed, cheapest-and-safest first)
//!
//! An opener must, in order: (1) refuse a context **not sealed to this
//! destination** (cleartext check — never touch the ciphertext of a blob meant
//! for someone else), (2) refuse an **expired** context (cleartext TTL — the
//! backstop for a misbehaving invocation-id cache), (3) **decrypt** with
//! AAD = [`SealedContext::aad`], which authenticates every cleartext field at
//! once (tamper ⇒ [`OpenError::BindingFailed`]), and (4) **validate** the
//! decrypted context, which catches decrypted header names that disagree with
//! the authenticated declared list. Steps 1–2 are cleartext, so a replayed or
//! misdirected blob dies before any crypto.

use std::collections::BTreeSet;

use async_trait::async_trait;

use super::context::{canonical_aad_bytes, ContextError, ForwardedContext};
use super::header::HeaderName;

/// The sealed, on-the-wire form of a forwarded context.
///
/// Construct from a validated [`ForwardedContext`] plus the AEAD ciphertext via
/// [`Self::from_context`]; a [`ForwardedContextSealer`] is the thing that
/// produces the ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedContext {
    /// Destination node id the payload is sealed to.
    pub sealed_to: String,
    /// The AEAD-verified caller origin.
    pub caller_origin: String,
    /// The capability id the forward authorizes.
    pub capability_id: String,
    /// The specific invocation this context is bound to.
    pub invocation_id: String,
    /// The declared, in-the-clear header names (bound into the AAD).
    pub declared_names: BTreeSet<HeaderName>,
    /// Unix-seconds issue time.
    pub issued_at: u64,
    /// Unix-seconds expiry.
    pub expires_at: u64,
    /// Single-use AEAD nonce.
    pub nonce: [u8; 16],
    /// AEAD ciphertext of the sealed header map, bound to [`Self::aad`]. Opaque
    /// here — the cipher is the sealer's concern; nothing else may read it.
    pub ciphertext: Vec<u8>,
}

impl SealedContext {
    /// Wrap a validated context's cleartext envelope around `ciphertext`. The
    /// caller (a sealer) must have produced `ciphertext` by encrypting the
    /// header map under `ctx.canonical_aad()`.
    pub fn from_context(ctx: &ForwardedContext, ciphertext: Vec<u8>) -> Self {
        Self {
            sealed_to: ctx.sealed_to.clone(),
            caller_origin: ctx.caller_origin.clone(),
            capability_id: ctx.capability_id.clone(),
            invocation_id: ctx.invocation_id.clone(),
            declared_names: ctx.declared_names.clone(),
            issued_at: ctx.issued_at,
            expires_at: ctx.expires_at,
            nonce: ctx.nonce,
            ciphertext,
        }
    }

    /// The associated-data bytes the ciphertext is bound to — recomputed from
    /// this envelope, byte-identical to the source's
    /// [`ForwardedContext::canonical_aad`]. Any altered cleartext field changes
    /// these bytes, so the AEAD open fails.
    pub fn aad(&self) -> Vec<u8> {
        canonical_aad_bytes(
            &self.sealed_to,
            &self.caller_origin,
            &self.capability_id,
            &self.invocation_id,
            self.issued_at,
            self.expires_at,
            &self.nonce,
            &self.declared_names,
        )
    }

    /// Whether the context has expired at `now_unix` (the TTL backstop).
    pub fn is_expired(&self, now_unix: u64) -> bool {
        now_unix >= self.expires_at
    }
}

/// A failure sealing a context.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// The context failed validation and must not be sealed.
    #[error("context is invalid and cannot be sealed: {0}")]
    Invalid(#[from] ContextError),
    /// The sealer's crypto/key backend failed. Never contains a value.
    #[error("sealer backend error: {0}")]
    Backend(String),
}

/// A failure opening a sealed context, in the order an opener checks them.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The context is sealed to a different destination — never decrypted.
    #[error("context is sealed to {sealed_to:?}, not this destination {expected:?}")]
    WrongDestination {
        /// Who the context was sealed to.
        sealed_to: String,
        /// This destination's id.
        expected: String,
    },
    /// The context's TTL has elapsed.
    #[error("forwarded context expired at {expires_at} (now {now})")]
    Expired {
        /// Unix-seconds expiry.
        expires_at: u64,
        /// The time the check ran.
        now: u64,
    },
    /// AEAD authentication failed — tampered envelope, wrong key, or a replayed
    /// blob whose cleartext no longer matches its ciphertext binding.
    #[error("sealed context failed authentication (tampered or wrong key)")]
    BindingFailed,
    /// The decrypted context is structurally invalid (e.g. the decrypted header
    /// names disagree with the authenticated declared list).
    #[error("opened context is malformed: {0}")]
    Malformed(#[from] ContextError),
    /// The opener's crypto/key backend failed. Never contains a value.
    #[error("opener backend error: {0}")]
    Backend(String),
}

/// Seals a [`ForwardedContext`] to a destination node's key. The caller side of
/// forwarding (Phase 2 injection) calls this after [`resolve_secret_send`] has
/// materialized the values and a context is assembled.
///
/// [`resolve_secret_send`]: super::resolve_secret_send
#[async_trait]
pub trait ForwardedContextSealer: Send + Sync {
    /// Validate and seal `ctx`, producing the wire form. Implementations
    /// encrypt the header map under `ctx.canonical_aad()` and wrap via
    /// [`SealedContext::from_context`].
    async fn seal(&self, ctx: &ForwardedContext) -> Result<SealedContext, SealError>;
}

/// Opens a [`SealedContext`] at the destination. Implementations MUST follow the
/// fixed order documented on the module: destination check → TTL → decrypt →
/// validate, refusing before any crypto whenever a cleartext check fails.
#[async_trait]
pub trait ForwardedContextOpener: Send + Sync {
    /// Open a context that must be sealed to `expected_destination`, at time
    /// `now_unix`. Returns the decrypted, validated context, or the first
    /// invariant that failed.
    async fn open(
        &self,
        sealed: &SealedContext,
        expected_destination: &str,
        now_unix: u64,
    ) -> Result<ForwardedContext, OpenError>;
}

#[cfg(test)]
mod tests {
    //! The tests exercise the seam and its invariants through an **insecure
    //! passthrough** sealer/opener that stands in for real AEAD: it stores the
    //! header map in the clear and embeds the AAD it was sealed under, so the
    //! opener can prove destination-binding, TTL, and tamper (AAD-mismatch)
    //! defenses at the plumbing level without a cipher. It is test-only and
    //! never leaves this module.

    use super::*;
    use crate::forward::{ForwardedHeaderValue, DEFAULT_TTL_SECS};
    use std::collections::BTreeMap;

    // --- tiny length-prefixed codec for the passthrough "ciphertext" ---------

    fn put(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }

    fn take<'a>(buf: &mut &'a [u8]) -> Option<&'a [u8]> {
        if buf.len() < 4 {
            return None;
        }
        let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if buf.len() < 4 + n {
            return None;
        }
        let (field, rest) = buf[4..].split_at(n);
        *buf = rest;
        Some(field)
    }

    /// Passthrough sealer: `ciphertext = put(aad) || put(name) put(value) ...`.
    struct PassthroughSealer;

    #[async_trait]
    impl ForwardedContextSealer for PassthroughSealer {
        async fn seal(&self, ctx: &ForwardedContext) -> Result<SealedContext, SealError> {
            ctx.validate()?;
            let mut blob = Vec::new();
            put(&mut blob, &ctx.canonical_aad());
            for (name, value) in ctx.headers() {
                put(&mut blob, name.as_str().as_bytes());
                put(&mut blob, value.expose());
            }
            Ok(SealedContext::from_context(ctx, blob))
        }
    }

    /// Passthrough opener enforcing the fixed order.
    struct PassthroughOpener;

    #[async_trait]
    impl ForwardedContextOpener for PassthroughOpener {
        async fn open(
            &self,
            sealed: &SealedContext,
            expected_destination: &str,
            now_unix: u64,
        ) -> Result<ForwardedContext, OpenError> {
            // (1) never touch a blob sealed to someone else.
            if sealed.sealed_to != expected_destination {
                return Err(OpenError::WrongDestination {
                    sealed_to: sealed.sealed_to.clone(),
                    expected: expected_destination.to_string(),
                });
            }
            // (2) cleartext TTL backstop.
            if sealed.is_expired(now_unix) {
                return Err(OpenError::Expired {
                    expires_at: sealed.expires_at,
                    now: now_unix,
                });
            }
            // (3) "decrypt": parse, and authenticate by comparing the embedded
            // AAD to the one recomputed from the (possibly tampered) envelope.
            let mut buf = sealed.ciphertext.as_slice();
            let embedded_aad = take(&mut buf).ok_or(OpenError::BindingFailed)?;
            if embedded_aad != sealed.aad().as_slice() {
                return Err(OpenError::BindingFailed);
            }
            let mut headers = BTreeMap::new();
            while !buf.is_empty() {
                let name = take(&mut buf).ok_or(OpenError::BindingFailed)?;
                let value = take(&mut buf).ok_or(OpenError::BindingFailed)?;
                let name = HeaderName::parse(std::str::from_utf8(name).map_err(|_| {
                    OpenError::Backend("non-utf8 header name in sealed payload".into())
                })?)
                .map_err(|_| OpenError::BindingFailed)?;
                let value = ForwardedHeaderValue::new(value.to_vec())
                    .map_err(|_| OpenError::BindingFailed)?;
                headers.insert(name, value);
            }
            // (4) reconstruct with the AUTHENTICATED declared list, then
            // validate — a decrypted header set that disagrees with the
            // declared names fails here.
            let mut ctx = ForwardedContext::new(
                sealed.sealed_to.clone(),
                sealed.caller_origin.clone(),
                sealed.capability_id.clone(),
                sealed.invocation_id.clone(),
                sealed.issued_at,
                sealed.expires_at,
                sealed.nonce,
                headers,
            );
            ctx.declared_names = sealed.declared_names.clone();
            ctx.validate()?;
            Ok(ctx)
        }
    }

    fn hv(s: &str) -> ForwardedHeaderValue {
        ForwardedHeaderValue::new(s.as_bytes().to_vec()).unwrap()
    }

    fn sample_context(dest: &str, issued_at: u64) -> ForwardedContext {
        let mut headers = BTreeMap::new();
        headers.insert(HeaderName::parse("Authorization").unwrap(), hv("Bearer t0ken"));
        headers.insert(HeaderName::parse("X-Tenant-Id").unwrap(), hv("acme"));
        ForwardedContext::new(
            dest,
            "origin-caller",
            "github.create_issue",
            "inv-1",
            issued_at,
            issued_at + DEFAULT_TTL_SECS,
            [7u8; 16],
            headers,
        )
    }

    #[tokio::test]
    async fn seal_open_round_trips() {
        let ctx = sample_context("node-dest", 1_000);
        let sealed = PassthroughSealer.seal(&ctx).await.unwrap();
        // The ciphertext carries no value in the clear beyond the passthrough's
        // stand-in — the real sealer encrypts it; here we only assert the open
        // recovers the values through the seam.
        let opened = PassthroughOpener
            .open(&sealed, "node-dest", 1_010)
            .await
            .unwrap();
        assert_eq!(
            opened
                .header(&HeaderName::parse("Authorization").unwrap())
                .unwrap()
                .expose(),
            b"Bearer t0ken"
        );
        assert_eq!(opened.caller_origin, "origin-caller");
    }

    #[tokio::test]
    async fn open_refuses_a_context_sealed_elsewhere() {
        let sealed = PassthroughSealer
            .seal(&sample_context("node-A", 1_000))
            .await
            .unwrap();
        let err = PassthroughOpener
            .open(&sealed, "node-B", 1_010)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenError::WrongDestination { .. }));
    }

    #[tokio::test]
    async fn open_refuses_an_expired_context() {
        let ctx = sample_context("node-dest", 1_000); // expires at 1_000 + TTL
        let sealed = PassthroughSealer.seal(&ctx).await.unwrap();
        let err = PassthroughOpener
            .open(&sealed, "node-dest", 1_000 + DEFAULT_TTL_SECS)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenError::Expired { .. }));
    }

    #[tokio::test]
    async fn tampering_a_cleartext_field_fails_the_open() {
        // Replay/redirect defense: flip the caller_origin on the sealed blob.
        // The recomputed AAD no longer matches the embedded one → BindingFailed.
        let mut sealed = PassthroughSealer
            .seal(&sample_context("node-dest", 1_000))
            .await
            .unwrap();
        sealed.caller_origin = "attacker-origin".to_string();
        let err = PassthroughOpener
            .open(&sealed, "node-dest", 1_010)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenError::BindingFailed));
    }

    #[tokio::test]
    async fn aad_is_byte_identical_to_the_source_context() {
        let ctx = sample_context("node-dest", 1_000);
        let sealed = PassthroughSealer.seal(&ctx).await.unwrap();
        assert_eq!(sealed.aad(), ctx.canonical_aad(), "one AAD layout, shared");
    }
}
