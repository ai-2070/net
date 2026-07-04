//! Real X25519 sealed-box implementation of the forwarded-context seal/open
//! seam (`MCP_CREDENTIAL_FORWARDING_PLAN.md` **Phase 2**).
//!
//! Fills [`ForwardedContextSealer`] / [`ForwardedContextOpener`] with anonymous
//! sealing to a destination node's X25519 key. The construction mirrors the
//! core's proven identity-envelope sealed box (`identity/envelope.rs`), so the
//! crypto is reuse, not invention:
//!
//! ```text
//! ephemeral_sk ← random 32 bytes (X25519, zeroize-on-drop)
//! ephemeral_pk ← x25519_base(ephemeral_sk)
//! shared       ← x25519(ephemeral_sk, recipient_pub)
//! key          ← BLAKE2s-MAC(shared; "net-mcp-forward-key")[..32]
//! nonce        ← BLAKE2s-MAC(ephemeral_pk || recipient_pub; "net-mcp-forward-nonce")[..24]
//! ciphertext   ← XChaCha20Poly1305(key, nonce, header_map, AAD = ctx.canonical_aad())
//! SealedContext.ciphertext ← ephemeral_pk (32) || ciphertext
//! ```
//!
//! Only the recipient's private key recovers `shared`, so only the destination
//! can open. Every non-secret field is bound as AEAD associated data (the
//! golden-vectored [`canonical_aad`]), so a tampered envelope — a redirected
//! `sealed_to`, a swapped `caller_origin`, a stretched `expires_at` — fails the
//! AEAD tag. A fresh ephemeral key per seal makes `(key, nonce)` unique without
//! any shared counter.
//!
//! **Key distribution is out of scope here.** The sealer takes the recipient's
//! public key and the opener holds the local private key as constructor inputs;
//! wiring those to mesh identity (a node's X25519 key derived from its ed25519
//! identity, learned from its announcement) is the remaining integration.
//!
//! [`canonical_aad`]: ForwardedContext::canonical_aad

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use blake2::{
    digest::{consts::U32, Mac},
    Blake2sMac,
};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305,
};
use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

use super::context::ForwardedContext;
use super::header::{ForwardedHeaderValue, HeaderName, MAX_HEADER_VALUE_LEN};
use super::seal::{
    ForwardedContextOpener, ForwardedContextSealer, OpenError, SealError, SealedContext,
};

/// Domain separator for the AEAD key derivation.
const KDF_DOMAIN_KEY: &[u8] = b"net-mcp-forward-key";
/// Domain separator for the AEAD nonce derivation.
const KDF_DOMAIN_NONCE: &[u8] = b"net-mcp-forward-nonce";
/// Length of the ephemeral X25519 public key prefixing the ciphertext.
const EPH_PK_LEN: usize = 32;

/// Seals a forwarded context to a destination node's X25519 public key.
#[derive(Debug, Clone)]
pub struct X25519SealedBoxSealer {
    recipient_pub: [u8; 32],
}

impl X25519SealedBoxSealer {
    /// A sealer that encrypts to `recipient_pub` (the destination node's X25519
    /// public key).
    pub fn to_recipient(recipient_pub: [u8; 32]) -> Self {
        Self { recipient_pub }
    }
}

#[async_trait]
impl ForwardedContextSealer for X25519SealedBoxSealer {
    async fn seal(&self, ctx: &ForwardedContext) -> Result<SealedContext, SealError> {
        ctx.validate()?;

        // Ephemeral X25519 keypair. `X25519Secret` (zeroize feature) wipes
        // itself on drop; scrub the raw seed too.
        let mut eph_seed = [0u8; 32];
        getrandom::fill(&mut eph_seed)
            .map_err(|e| SealError::Backend(format!("getrandom failed: {e}")))?;
        let eph_sk = X25519Secret::from(eph_seed);
        volatile_zero(&mut eph_seed);
        let eph_pk = X25519Pub::from(&eph_sk);

        let recipient_pk = X25519Pub::from(self.recipient_pub);
        let shared = eph_sk.diffie_hellman(&recipient_pk);
        // Refuse a low-order / identity recipient key. A non-contributory shared
        // secret is the all-zero point regardless of our ephemeral key, so the
        // derived AEAD key would be a public constant and any passive observer
        // could recompute it and decrypt the bearer token. Reject before
        // deriving a key or even materializing the plaintext header copy, so the
        // early-return path holds no secret to scrub.
        if !shared.was_contributory() {
            return Err(SealError::Backend(
                "recipient forwarding key is low-order and was rejected".into(),
            ));
        }

        let mut plaintext = serialize_headers(ctx);
        let mut key = derive_key(shared.as_bytes(), KDF_DOMAIN_KEY);
        let nonce = derive_nonce(eph_pk.as_bytes(), &self.recipient_pub);
        let aad = ctx.canonical_aad();

        let aead = XChaCha20Poly1305::new((&key).into());
        let ciphertext = aead
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::Backend("AEAD seal failed".into()));
        // Scrub the derived key (a function of the shared secret) and the
        // plaintext header copy regardless of the encrypt outcome.
        volatile_zero(&mut key);
        volatile_zero(&mut plaintext);
        let ciphertext = ciphertext?;

        let mut sealed_ct = Vec::with_capacity(EPH_PK_LEN + ciphertext.len());
        sealed_ct.extend_from_slice(eph_pk.as_bytes());
        sealed_ct.extend_from_slice(&ciphertext);
        Ok(SealedContext::from_context(ctx, sealed_ct))
    }
}

/// Opens a sealed context with the local node's X25519 private key.
pub struct X25519SealedBoxOpener {
    secret: X25519Secret,
}

impl X25519SealedBoxOpener {
    /// An opener holding the local node's X25519 private key.
    pub fn new(secret: X25519Secret) -> Self {
        Self { secret }
    }

    /// An opener from raw 32-byte X25519 secret material.
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        Self {
            secret: X25519Secret::from(bytes),
        }
    }

    /// This opener's X25519 public key — the value a caller seals to.
    pub fn public_key(&self) -> [u8; 32] {
        X25519Pub::from(&self.secret).to_bytes()
    }
}

/// Redacted — never prints the private key.
impl fmt::Debug for X25519SealedBoxOpener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("X25519SealedBoxOpener")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ForwardedContextOpener for X25519SealedBoxOpener {
    async fn open(
        &self,
        sealed: &SealedContext,
        expected_destination: &str,
        now_unix: u64,
    ) -> Result<ForwardedContext, OpenError> {
        // (1) never touch the ciphertext of a blob sealed to someone else.
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
        // (3) decrypt. `ciphertext = ephemeral_pk (32) || aead_ct`.
        if sealed.ciphertext.len() < EPH_PK_LEN {
            return Err(OpenError::BindingFailed);
        }
        let (eph_pk_bytes, ct) = sealed.ciphertext.split_at(EPH_PK_LEN);
        let eph_arr: [u8; 32] = eph_pk_bytes
            .try_into()
            .map_err(|_| OpenError::BindingFailed)?;
        let eph_pk = X25519Pub::from(eph_arr);
        let recipient_pub = X25519Pub::from(&self.secret).to_bytes();

        let shared = self.secret.diffie_hellman(&eph_pk);
        let mut key = derive_key(shared.as_bytes(), KDF_DOMAIN_KEY);
        let nonce = derive_nonce(&eph_arr, &recipient_pub);
        let aad = sealed.aad();

        let aead = XChaCha20Poly1305::new((&key).into());
        let plaintext = aead.decrypt((&nonce).into(), Payload { msg: ct, aad: &aad });
        volatile_zero(&mut key);
        let mut plaintext = plaintext.map_err(|_| OpenError::BindingFailed)?;

        // (4) parse + reconstruct with the AUTHENTICATED declared list, then
        // validate — a decrypted header set that disagrees with the declared
        // names, or any other broken invariant, fails here.
        let headers = deserialize_headers(&plaintext);
        volatile_zero(&mut plaintext);
        let headers =
            headers.ok_or_else(|| OpenError::Backend("malformed sealed header payload".into()))?;

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

// --- header-map codec ------------------------------------------------------

/// Serialize the sealed header map as `put(name) put(value) ...`, where `put`
/// is a 4-byte big-endian length prefix + bytes.
fn serialize_headers(ctx: &ForwardedContext) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in ctx.headers() {
        put(&mut out, name.as_str().as_bytes());
        put(&mut out, value.expose());
    }
    out
}

/// Parse the header map back. Returns `None` on any structural problem — the
/// AEAD already authenticated the bytes, so a parse failure means a
/// version/format mismatch, not an attack.
fn deserialize_headers(mut buf: &[u8]) -> Option<BTreeMap<HeaderName, ForwardedHeaderValue>> {
    let mut map = BTreeMap::new();
    while !buf.is_empty() {
        let name = take(&mut buf)?;
        let value = take(&mut buf)?;
        let name = HeaderName::parse(std::str::from_utf8(name).ok()?).ok()?;
        // Reject an oversized value *before* copying it. `ForwardedHeaderValue::new`
        // enforces the same cap, but only after `to_vec` — a crafted field could
        // force a large allocation first. `value` is a borrow into the (bounded)
        // plaintext, so checking its length allocates nothing.
        if value.len() > MAX_HEADER_VALUE_LEN {
            return None;
        }
        let value = ForwardedHeaderValue::new(value.to_vec()).ok()?;
        map.insert(name, value);
    }
    Some(map)
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn take<'a>(buf: &mut &'a [u8]) -> Option<&'a [u8]> {
    if buf.len() < 4 {
        return None;
    }
    let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    // Compare as `n > remaining` rather than `4 + n > len`: `buf.len() >= 4`
    // here so the subtraction can't underflow, and this avoids a `4 + n` wrap
    // on 32-bit targets that would let an over-long prefix slip past into a
    // panicking `split_at`. Fail closed (None) on a malformed prefix.
    if n > buf.len() - 4 {
        return None;
    }
    let (field, rest) = buf[4..].split_at(n);
    *buf = rest;
    Some(field)
}

// --- KDF + scrub (mirrors identity/envelope.rs) ----------------------------

/// BLAKE2s-MAC keyed with a domain `label`, over a 32-byte input, truncated to
/// 32. The domain-separated key-derivation primitive shared by the AEAD key
/// derivation here and the forwarding-keypair derivation in
/// [`super::keys`](super::keys).
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; the domain labels are short compile-time constants"
)]
pub(crate) fn derive_key(shared: &[u8; 32], label: &[u8]) -> [u8; 32] {
    let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(label)
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut mac, shared);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// BLAKE2s-MAC keyed with [`KDF_DOMAIN_NONCE`], over `eph_pk || recipient_pk`,
/// truncated to the 24-byte XChaCha nonce.
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; KDF_DOMAIN_NONCE is a short compile-time constant"
)]
fn derive_nonce(eph_pk: &[u8; 32], recipient_pk: &[u8; 32]) -> [u8; 24] {
    let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(KDF_DOMAIN_NONCE)
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut mac, eph_pk);
    Mac::update(&mut mac, recipient_pk);
    let result = mac.finalize().into_bytes();
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&result[..24]);
    nonce
}

/// Best-effort scrub with `write_volatile` so the compiler can't elide it.
/// Shared with [`super::keys`], which scrubs its derived forwarding secret.
pub(crate) fn volatile_zero(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, mutable reference for the
        // duration of this call — all `write_volatile` requires.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::DEFAULT_TTL_SECS;

    fn keypair() -> ([u8; 32], X25519Secret) {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let sk = X25519Secret::from(seed);
        let pk = X25519Pub::from(&sk).to_bytes();
        (pk, sk)
    }

    fn hv(s: &str) -> ForwardedHeaderValue {
        ForwardedHeaderValue::new(s.as_bytes().to_vec()).unwrap()
    }

    fn sample(dest: &str, issued_at: u64) -> ForwardedContext {
        let mut headers = BTreeMap::new();
        headers.insert(
            HeaderName::parse("Authorization").unwrap(),
            hv("Bearer s3cret"),
        );
        headers.insert(HeaderName::parse("X-Tenant-Id").unwrap(), hv("acme"));
        ForwardedContext::new(
            dest,
            "origin-caller",
            "github.create_issue",
            "inv-1",
            issued_at,
            issued_at + DEFAULT_TTL_SECS,
            [9u8; 16],
            headers,
        )
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[tokio::test]
    async fn round_trips_and_hides_values_in_ciphertext() {
        let (pk, sk) = keypair();
        let ctx = sample("node-dest", 1_000);
        let sealed = X25519SealedBoxSealer::to_recipient(pk)
            .seal(&ctx)
            .await
            .unwrap();

        // The real cipher: no cleartext value in the sealed bytes.
        assert!(!contains(&sealed.ciphertext, b"Bearer"));
        assert!(!contains(&sealed.ciphertext, b"s3cret"));

        let opened = X25519SealedBoxOpener::new(sk)
            .open(&sealed, "node-dest", 1_010)
            .await
            .unwrap();
        assert_eq!(
            opened
                .header(&HeaderName::parse("Authorization").unwrap())
                .unwrap()
                .expose(),
            b"Bearer s3cret"
        );
        assert_eq!(
            opened
                .header(&HeaderName::parse("X-Tenant-Id").unwrap())
                .unwrap()
                .expose(),
            b"acme"
        );
    }

    #[tokio::test]
    async fn only_the_intended_recipient_can_open() {
        let (pk, _sk) = keypair();
        let (_pk2, sk2) = keypair(); // a different key
        let sealed = X25519SealedBoxSealer::to_recipient(pk)
            .seal(&sample("node-dest", 1_000))
            .await
            .unwrap();
        let err = X25519SealedBoxOpener::new(sk2)
            .open(&sealed, "node-dest", 1_010)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenError::BindingFailed));
    }

    #[tokio::test]
    async fn refuses_a_low_order_recipient_key() {
        // An all-zero X25519 public key is a small-order point: the shared
        // secret is non-contributory (the identity), so the derived key would be
        // a public constant. The sealer must refuse rather than emit a blob any
        // observer could decrypt.
        let err = X25519SealedBoxSealer::to_recipient([0u8; 32])
            .seal(&sample("node-dest", 1_000))
            .await
            .unwrap_err();
        assert!(matches!(err, SealError::Backend(_)), "low-order key must be rejected");
    }

    #[tokio::test]
    async fn tampering_a_bound_field_fails_the_open() {
        let (pk, sk) = keypair();
        let mut sealed = X25519SealedBoxSealer::to_recipient(pk)
            .seal(&sample("node-dest", 1_000))
            .await
            .unwrap();
        // Redirect the caller — the recomputed AAD no longer matches the tag.
        sealed.caller_origin = "attacker-origin".to_string();
        let err = X25519SealedBoxOpener::new(sk)
            .open(&sealed, "node-dest", 1_010)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenError::BindingFailed));
    }

    #[tokio::test]
    async fn tampering_issued_at_fails_the_open() {
        // issued_at is bound in the AAD, so forging the issue time (which the
        // opener otherwise trusts when reconstructing the context) breaks the tag.
        let (pk, sk) = keypair();
        let mut sealed = X25519SealedBoxSealer::to_recipient(pk)
            .seal(&sample("node-dest", 1_000))
            .await
            .unwrap();
        sealed.issued_at = 0;
        let err = X25519SealedBoxOpener::new(sk)
            .open(&sealed, "node-dest", 1_010)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenError::BindingFailed));
    }

    #[tokio::test]
    async fn refuses_wrong_destination_and_expiry_before_crypto() {
        let (pk, sk) = keypair();
        let sealed = X25519SealedBoxSealer::to_recipient(pk)
            .seal(&sample("node-dest", 1_000))
            .await
            .unwrap();
        let opener = X25519SealedBoxOpener::new(sk);
        assert!(matches!(
            opener.open(&sealed, "node-other", 1_010).await.unwrap_err(),
            OpenError::WrongDestination { .. },
        ));
        assert!(matches!(
            opener
                .open(&sealed, "node-dest", 1_000 + DEFAULT_TTL_SECS)
                .await
                .unwrap_err(),
            OpenError::Expired { .. },
        ));
    }

    #[tokio::test]
    async fn opener_public_key_is_what_the_sealer_seals_to() {
        let (_pk, sk) = keypair();
        let opener = X25519SealedBoxOpener::new(sk);
        let pk = opener.public_key();
        let sealed = X25519SealedBoxSealer::to_recipient(pk)
            .seal(&sample("node-dest", 1_000))
            .await
            .unwrap();
        assert!(opener.open(&sealed, "node-dest", 1_010).await.is_ok());
        // Debug never leaks the secret.
        assert!(!format!("{opener:?}").contains("secret"));
    }

    #[test]
    fn take_fails_closed_on_an_overlong_length_prefix() {
        // A prefix claiming more bytes than remain must return None, never
        // panic — including the case where `4 + n` would wrap on 32-bit.
        let mut overlong: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0x01];
        assert!(take(&mut overlong).is_none());
        // A well-formed field still parses and advances the cursor.
        let mut ok: &[u8] = &[0, 0, 0, 3, b'a', b'b', b'c'];
        assert_eq!(take(&mut ok), Some(&b"abc"[..]));
        assert!(ok.is_empty());
    }

    #[test]
    fn deserialize_rejects_an_oversized_value() {
        // A value field over the per-value cap is rejected before it's copied.
        let mut blob = Vec::new();
        put(&mut blob, b"authorization");
        put(&mut blob, &vec![b'x'; MAX_HEADER_VALUE_LEN + 1]);
        assert!(deserialize_headers(&blob).is_none());

        // A within-cap field still round-trips.
        let mut ok = Vec::new();
        put(&mut ok, b"x-tenant-id");
        put(&mut ok, b"acme");
        assert_eq!(deserialize_headers(&ok).unwrap().len(), 1);
    }
}
