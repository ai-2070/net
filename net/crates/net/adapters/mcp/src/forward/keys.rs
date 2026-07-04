//! Forwarding key derivation — a **domain-separated** X25519 keypair from a
//! node's ed25519 identity (`MCP_CREDENTIAL_FORWARDING_PLAN.md` **Phase 2**,
//! key distribution, option 2).
//!
//! A node's forwarding sealing key is derived from its ed25519 identity seed
//! (`Identity::to_bytes()`) through a domain-separated KDF, so it is **distinct
//! from the signing key**: the identity key is never used for key exchange, and
//! forwarding gets its own key without a second secret to store. Because the
//! derivation is one-way, the forwarding *public* key can't be recovered from
//! the ed25519 public key — a node **publishes** its forwarding public key in
//! its announcement, and a caller seals to the value it learns there.
//!
//! Only the local node derives here (it needs its own keypair — the public half
//! to publish, the secret half to open). A caller sealing to a destination uses
//! the destination's *published* public key directly; it derives nothing.
//!
//! The derivation reuses the same BLAKE2s-MAC primitive as the AEAD KDF
//! ([`super::aead`]), keyed with a forwarding-specific domain label. x25519's
//! scalar clamping happens at Diffie-Hellman time, so the raw KDF output is a
//! sound `StaticSecret` input.

use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

use super::aead::{derive_key, X25519SealedBoxOpener};

/// Domain separator for the forwarding X25519 secret derivation. Versioned so a
/// future rotation of the derivation is unambiguous.
const FORWARD_X25519_DOMAIN: &[u8] = b"net-mcp-forward-x25519-v1";

/// A node's forwarding keypair, derived from its ed25519 identity seed.
///
/// The public half is announced (callers seal to it); the secret half backs an
/// [`X25519SealedBoxOpener`] for inbound contexts. Not `Clone`/`Debug`-verbose:
/// it holds secret key material.
pub struct ForwardingKeypair {
    secret: X25519Secret,
    public: [u8; 32],
}

impl ForwardingKeypair {
    /// Derive the forwarding keypair from the node's 32-byte ed25519 identity
    /// seed (`net_sdk::Identity::to_bytes()`). Deterministic: the same seed
    /// always yields the same forwarding keypair, so a node's published public
    /// key is stable across restarts.
    pub fn from_ed25519_seed(seed: &[u8; 32]) -> Self {
        let secret_bytes = derive_key(seed, FORWARD_X25519_DOMAIN);
        let secret = X25519Secret::from(secret_bytes);
        let public = X25519Pub::from(&secret).to_bytes();
        Self { secret, public }
    }

    /// The forwarding **public** key — the value a node publishes in its
    /// announcement and a caller seals to.
    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    /// Consume into an opener for inbound sealed contexts (destination side).
    pub fn into_opener(self) -> X25519SealedBoxOpener {
        X25519SealedBoxOpener::new(self.secret)
    }
}

/// Redacted — never prints the secret; shows only the public key length marker.
impl std::fmt::Debug for ForwardingKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardingKeypair")
            .field("public_key", &"<32 bytes>")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::{
        ForwardedContextBuilder, ForwardedContextOpener, ForwardedContextSealer, ForwardedHeaderValue,
        HeaderName, X25519SealedBoxSealer,
    };

    #[test]
    fn derivation_is_deterministic_and_seed_specific() {
        let seed = [7u8; 32];
        let a = ForwardingKeypair::from_ed25519_seed(&seed);
        let b = ForwardingKeypair::from_ed25519_seed(&seed);
        assert_eq!(a.public_key(), b.public_key(), "same seed → same key");

        let other = ForwardingKeypair::from_ed25519_seed(&[8u8; 32]);
        assert_ne!(
            a.public_key(),
            other.public_key(),
            "different seed → different key"
        );
    }

    #[test]
    fn forwarding_key_is_not_the_raw_seed() {
        // Domain separation: the derived secret is a KDF of the seed, not the
        // seed (nor a naive copy) — the forwarding key is distinct from the
        // identity key material it comes from.
        let seed = [42u8; 32];
        let kp = ForwardingKeypair::from_ed25519_seed(&seed);
        assert_ne!(kp.public_key(), seed, "derived key must not echo the seed");
    }

    #[test]
    fn debug_never_leaks_the_secret() {
        let kp = ForwardingKeypair::from_ed25519_seed(&[1u8; 32]);
        let dbg = format!("{kp:?}");
        assert!(!dbg.contains("secret"));
        assert!(dbg.contains("public_key"));
    }

    #[tokio::test]
    async fn seal_to_published_key_opens_with_the_derived_opener() {
        // The real distribution shape: the destination derives its keypair and
        // "publishes" the public key; a caller seals to that; the destination
        // opens with its derived opener.
        let dest_seed = [5u8; 32];
        let dest_kp = ForwardingKeypair::from_ed25519_seed(&dest_seed);
        let published_pubkey = dest_kp.public_key();

        let ctx = ForwardedContextBuilder::new("node-dest", "origin", "github.issues", "inv-1")
            .header(
                HeaderName::parse("Authorization").unwrap(),
                ForwardedHeaderValue::new(b"Bearer k3y".to_vec()).unwrap(),
            )
            .build(1_000)
            .unwrap();
        let sealed = X25519SealedBoxSealer::to_recipient(published_pubkey)
            .seal(&ctx)
            .await
            .unwrap();

        // The destination reconstructs its opener from the same seed.
        let opener = ForwardingKeypair::from_ed25519_seed(&dest_seed).into_opener();
        let opened = opener.open(&sealed, "node-dest", 1_005).await.unwrap();
        assert_eq!(
            opened
                .header(&HeaderName::parse("Authorization").unwrap())
                .unwrap()
                .expose(),
            b"Bearer k3y"
        );
    }
}
