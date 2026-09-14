//! The leaf's identity: Ed25519 entity key + X25519 Noise static.
//!
//! §8's storage half (IndexedDB under a non-extractable WebCrypto
//! AES-GCM key) and leader election are a separate slice; what lives
//! here is the identity *itself*, the two derivations every other
//! node keys on, and the custodial-injection path — the same shape
//! as `MeshNodeConfig::entity_keypair`.
//!
//! **The derivations are not ours to choose.** `node_id` and
//! `origin_hash` are keyed BLAKE2s-MAC over the Ed25519 public key
//! with the domain labels `net-node-id-v1` and `net-origin-v1`, and
//! the mesh's reverse index (`mesh.rs::origin_hash_to_node`) maps the
//! `origin_hash` a leaf stamps on every packet back to its node id.
//! A leaf that derived either differently would announce one identity
//! and publish under another.
//!
//! **What the origin is, stated honestly** (plan §8): wasm is not a
//! security boundary against same-origin JavaScript, and WebCrypto
//! decryption returns plaintext to the calling context. The trust
//! boundary is the origin; XSS on the origin owns the identity. A
//! host application that needs stronger custody injects a keypair.

use blake2::{
    digest::{consts::U32, KeyInit, Mac},
    Blake2sMac,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use net_wire::crypto::StaticKeypair;

use crate::error::{LeafError, Result};

/// Domain label for the `node_id` derivation.
const NODE_ID_LABEL: &[u8] = b"net-node-id-v1";

/// Domain label for the `origin_hash` derivation.
const ORIGIN_LABEL: &[u8] = b"net-origin-v1";

/// Keyed BLAKE2s-MAC of `public` under `label`, truncated to the
/// leading 8 bytes as a little-endian `u64`.
///
/// `EntityId::blake2s_hash` in the core, reproduced because the value
/// crosses the wire and both sides must agree. The parity test below
/// pins the two derivations against values computed from the same
/// primitive.
fn keyed_u64(label: &[u8], public: &[u8; 32]) -> u64 {
    // `new_from_slice` rejects only keys longer than 32 bytes, and
    // both labels are short compile-time constants.
    let Ok(mut mac) = <Blake2sMac<U32> as KeyInit>::new_from_slice(label) else {
        // Unreachable for the two constant labels above. Returning 0
        // rather than panicking would mint a colliding identity, so
        // the branch is expressed as a hard failure of the only kind
        // available in a `-> u64` signature: it cannot happen, and
        // the constants are right here to check.
        unreachable!("BLAKE2s accepts any key up to 32 bytes; the labels are constants")
    };
    Mac::update(&mut mac, public);
    let out = mac.finalize().into_bytes();
    let mut head = [0u8; 8];
    head.copy_from_slice(&out[..8]);
    u64::from_le_bytes(head)
}

/// The Ed25519 half: who this node *is*.
pub struct EntityKeypair {
    signing: SigningKey,
    public: [u8; 32],
    node_id: u64,
    origin_hash: u64,
}

impl core::fmt::Debug for EntityKeypair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // No secret half in the formatter, ever.
        f.debug_struct("EntityKeypair")
            .field("node_id", &format_args!("{:#018x}", self.node_id))
            .field("entity_id", &self.entity_id_hex())
            .finish()
    }
}

impl EntityKeypair {
    /// Generate a fresh identity from the platform CSPRNG.
    ///
    /// In the browser that is `crypto.getRandomValues` via
    /// `getrandom`'s `wasm_js` backend — the opt-in S0a §6.2 found
    /// every `getrandom` major in this graph needs.
    pub fn generate() -> Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
        Ok(Self::from_secret(secret))
    }

    /// Custodial injection: build from a caller-held scalar.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&secret);
        let public = signing.verifying_key().to_bytes();
        Self {
            signing,
            node_id: keyed_u64(NODE_ID_LABEL, &public),
            origin_hash: keyed_u64(ORIGIN_LABEL, &public),
            public,
        }
    }

    /// The 32-byte Ed25519 public key — the entity id.
    #[inline]
    pub fn entity_id(&self) -> &[u8; 32] {
        &self.public
    }

    /// The entity id as lowercase hex, which is how the capability
    /// announcement carries it.
    pub fn entity_id_hex(&self) -> String {
        hex_lower(&self.public)
    }

    /// The mesh node id derived from the public key.
    #[inline]
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// The origin hash stamped on every packet this node publishes.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.origin_hash
    }

    /// Sign `message`.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }
}

/// Verify a 64-byte Ed25519 signature over `message` against an
/// entity's public key.
///
/// `verify_strict`, matching `EntityId::verify`: the lax `verify`
/// admits the malleable `(R, S + L)` variant, which would let one
/// logical announcement appear under two byte encodings and defeat
/// any dedup keyed on the signed bytes.
pub fn verify_entity_signature(
    entity_id: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
) -> Result<()> {
    let vk = VerifyingKey::from_bytes(entity_id)
        .map_err(|_| LeafError::Identity("not a valid Ed25519 public key".into()))?;
    vk.verify_strict(message, &Signature::from_bytes(signature))
        .map_err(|_| LeafError::Identity("signature did not verify".into()))
}

/// Everything a leaf needs to be a node: its entity key and its
/// Noise static keypair.
///
/// Two keys, not one: the entity key signs announcements and
/// `0x0D02` envelopes, the X25519 static is what a peer runs NKpsk0
/// against. The announcement carries the X25519 *public* half
/// (`noise_pubkey`) authenticated by the Ed25519 signature, which is
/// the §5 Layer 1 key-discovery path — key discovery, not key
/// agreement.
pub struct LeafIdentity {
    entity: EntityKeypair,
    noise: StaticKeypair,
}

impl core::fmt::Debug for LeafIdentity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LeafIdentity")
            .field("entity", &self.entity)
            .field("noise_pubkey", &hex_lower(self.noise.public_key()))
            .finish()
    }
}

impl LeafIdentity {
    /// Generate both halves from the platform CSPRNG.
    pub fn generate() -> Result<Self> {
        let entity = EntityKeypair::generate()?;
        let mut x_secret = [0u8; 32];
        getrandom::fill(&mut x_secret)
            .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
        Ok(Self::from_secrets(entity, x_secret))
    }

    /// Custodial injection of both halves.
    pub fn from_secrets(entity: EntityKeypair, noise_secret: [u8; 32]) -> Self {
        let secret = x25519_dalek::StaticSecret::from(noise_secret);
        let public = x25519_dalek::PublicKey::from(&secret);
        Self {
            entity,
            noise: StaticKeypair::from_keys(noise_secret, *public.as_bytes()),
        }
    }

    /// The entity key.
    #[inline]
    pub fn entity(&self) -> &EntityKeypair {
        &self.entity
    }

    /// The Noise static keypair.
    #[inline]
    pub fn noise(&self) -> &StaticKeypair {
        &self.noise
    }

    /// This node's id.
    #[inline]
    pub fn node_id(&self) -> u64 {
        self.entity.node_id()
    }

    /// This node's origin hash.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.entity.origin_hash()
    }
}

/// Lowercase hex of a byte slice. The one encoding the announcement's
/// `entity_id` and `signature` fields use.
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((b & 0x0F) as u32, 16).unwrap_or('0'));
    }
    s
}

/// Parse lowercase-or-uppercase hex into bytes.
pub fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(LeafError::Identity("odd-length hex".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| LeafError::Identity(format!("not hex: {:?}", &s[i..i + 2])))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivations must be the keyed-BLAKE2s ones the mesh's
    /// reverse index inverts. Computed here from the primitive
    /// independently of `keyed_u64`, so a change to either the label
    /// or the truncation fails.
    #[test]
    fn node_id_and_origin_hash_are_the_meshs_keyed_blake2s_derivations() {
        let kp = EntityKeypair::from_secret([9u8; 32]);
        let public = *kp.entity_id();

        for (label, got) in [
            (b"net-node-id-v1".as_slice(), kp.node_id()),
            (b"net-origin-v1".as_slice(), kp.origin_hash()),
        ] {
            let mut mac = <Blake2sMac<U32> as KeyInit>::new_from_slice(label).expect("mac");
            Mac::update(&mut mac, &public);
            let digest = mac.finalize().into_bytes();
            let expect = u64::from_le_bytes(digest[..8].try_into().expect("8 bytes"));
            assert_eq!(got, expect, "derivation for {label:?} drifted");
        }
        assert_ne!(
            kp.node_id(),
            kp.origin_hash(),
            "the two labels must produce different values, or the \
             domain separation is not doing its job"
        );
    }

    #[test]
    fn a_signature_verifies_and_a_tampered_message_does_not() {
        let kp = EntityKeypair::from_secret([3u8; 32]);
        let sig = kp.sign(b"announcement transcript");
        verify_entity_signature(kp.entity_id(), b"announcement transcript", &sig)
            .expect("its own signature must verify");
        assert!(
            verify_entity_signature(kp.entity_id(), b"announcement transcripT", &sig).is_err(),
            "one flipped bit must fail verification"
        );

        let other = EntityKeypair::from_secret([4u8; 32]);
        assert!(
            verify_entity_signature(other.entity_id(), b"announcement transcript", &sig).is_err(),
            "another entity's key must not verify this signature"
        );
    }

    #[test]
    fn the_noise_static_public_half_is_the_x25519_derivation() {
        let id = LeafIdentity::from_secrets(EntityKeypair::from_secret([1u8; 32]), [2u8; 32]);
        let expect = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([2u8; 32]));
        assert_eq!(id.noise().public_key(), expect.as_bytes());
        assert_eq!(id.noise().private, [2u8; 32]);
    }

    #[test]
    fn hex_round_trips_and_the_entity_id_is_sixty_four_characters() {
        let kp = EntityKeypair::from_secret([0xAB; 32]);
        let hex = kp.entity_id_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(unhex(&hex).expect("round trip"), kp.entity_id().to_vec());
        assert!(unhex("abc").is_err(), "odd-length hex must be refused");
        assert!(unhex("zz").is_err(), "non-hex must be refused");
    }

    #[test]
    fn a_debug_print_never_contains_the_secret_scalar() {
        let id = LeafIdentity::from_secrets(EntityKeypair::from_secret([0x11; 32]), [0x22; 32]);
        let text = format!("{id:?}");
        assert!(
            !text.contains(&"22".repeat(32)),
            "the Noise secret leaked into Debug: {text}"
        );
        assert!(
            !text.contains(&"11".repeat(32)),
            "the entity secret leaked into Debug: {text}"
        );
    }
}
