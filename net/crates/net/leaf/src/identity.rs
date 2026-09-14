//! The leaf's identity: Ed25519 entity key + X25519 Noise static.
//!
//! The identity *itself*, the two derivations every other node keys
//! on, the fingerprint leader election names its lock after, and the
//! secret-at-rest value ([`IdentitySecrets`]) that both the custodial
//! path and the encrypted-storage path produce. The storage itself —
//! IndexedDB under a non-extractable WebCrypto AES-GCM key — is
//! `crate::storage`, because it needs the browser; everything here
//! is natively testable.
//!
//! **The derivations are not ours to choose.** `node_id` and
//! `origin_hash` are keyed BLAKE2s-MAC over the Ed25519 public key
//! with the domain labels `net-node-id-v1` and `net-origin-v1`, and
//! the mesh's reverse index (`mesh.rs::origin_hash_to_node`) maps the
//! `origin_hash` a leaf stamps on every packet back to its node id.
//! A leaf that derived either differently would announce one identity
//! and publish under another.
//!
//! **What the origin is, stated honestly** (plan §8): the scalars are
//! not readable from IndexedDB in the clear and the wrapping key
//! cannot be exported, but that does **not** make the scalars
//! non-extractable. WebCrypto decryption returns plaintext to the
//! calling context, and wasm is not a security boundary against
//! same-origin JavaScript. The trust boundary is the origin; XSS on
//! the origin owns the identity. A host application that needs
//! stronger custody injects a keypair
//! ([`IdentitySecrets::from_hex`], the same shape as
//! `MeshNodeConfig::entity_keypair`).

use blake2::{
    digest::{consts::U32, KeyInit, Mac},
    Blake2sMac,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use net_wire::crypto::StaticKeypair;
use zeroize::Zeroize;

use crate::error::{LeafError, Result};

/// Domain label for the `node_id` derivation.
const NODE_ID_LABEL: &[u8] = b"net-node-id-v1";

/// Domain label for the `origin_hash` derivation.
const ORIGIN_LABEL: &[u8] = b"net-origin-v1";

/// Domain label for the identity fingerprint the Web Lock and the
/// follower `BroadcastChannel` are named after.
///
/// Its own label, not a truncation of the entity id: the lock name is
/// visible to every script on the origin, and a name that *was* a
/// prefix of the entity id would invite a reader to treat it as one.
/// A domain-separated MAC names the identity without being any wire
/// field.
const FINGERPRINT_LABEL: &[u8] = b"net-leaf-fingerprint-v1";

/// The schema family of the at-rest plaintext, without its version
/// digit.
///
/// Split out so a record written by a future build can be reported as
/// "this build speaks v1" rather than as foreign data. The obvious
/// implementation compares the whole magic first and calls a v2 blob
/// corrupt, which is the misleading answer to the one migration
/// question a future reader will actually have.
const IDENTITY_BLOB_PREFIX: &[u8] = b"net-leaf-identity-v";

/// The version digit this build writes and reads.
const IDENTITY_BLOB_VERSION: u8 = b'1';

/// The versioned plaintext the identity vault encrypts.
///
/// The magic is both the schema tag inside the plaintext and the
/// AES-GCM additional data, so a ciphertext minted for some other
/// record in the same database cannot be decrypted into an identity,
/// and a future schema is refused rather than misread.
pub const IDENTITY_BLOB_MAGIC: &[u8] = b"net-leaf-identity-v1";

/// The exact length of an encoded identity blob.
pub const IDENTITY_BLOB_LEN: usize = IDENTITY_BLOB_MAGIC.len() + 64;

/// Keyed BLAKE2s-MAC of `public` under `label`, all 32 bytes.
fn keyed_mac(label: &[u8], public: &[u8; 32]) -> [u8; 32] {
    // `new_from_slice` rejects only keys longer than 32 bytes, and
    // every label is a short compile-time constant.
    let Ok(mut mac) = <Blake2sMac<U32> as KeyInit>::new_from_slice(label) else {
        // Unreachable for the constant labels above. Returning a
        // fixed value rather than panicking would mint a colliding
        // identity, so the branch is expressed as a hard failure of
        // the only kind available here: it cannot happen, and the
        // constants are right there to check.
        unreachable!("BLAKE2s accepts any key up to 32 bytes; the labels are constants")
    };
    Mac::update(&mut mac, public);
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// Keyed BLAKE2s-MAC of `public` under `label`, truncated to the
/// leading 8 bytes as a little-endian `u64`.
///
/// `EntityId::blake2s_hash` in the core, reproduced because the value
/// crosses the wire and both sides must agree. The parity test below
/// pins the two derivations against values computed from the same
/// primitive.
fn keyed_u64(label: &[u8], public: &[u8; 32]) -> u64 {
    let out = keyed_mac(label, public);
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

    /// The identity fingerprint the Web Lock and the follower
    /// `BroadcastChannel` are named after: 32 lowercase hex
    /// characters of the domain-separated MAC over the entity public
    /// key.
    ///
    /// Stable for the life of the identity and derived from the
    /// *public* half only, so naming a lock never involves a secret.
    /// Half a MAC is 128 bits, which is not a collision risk between
    /// the handful of identities one origin ever holds; the name is a
    /// scope, not a credential.
    pub fn fingerprint(&self) -> String {
        hex_lower(&keyed_mac(FINGERPRINT_LABEL, self.entity.entity_id())[..16])
    }
}

/// The two scalars, and the only thing the identity vault ever
/// encrypts.
///
/// Separate from [`LeafIdentity`] on purpose: this is the *secret at
/// rest*, so it has an encoding, a length, a version tag and a `Drop`
/// that overwrites it, while `LeafIdentity` is the live object with
/// no way to read a scalar back out. The custodial path and the
/// storage path both produce one of these and nothing else — that is
/// what makes them one surface instead of two
/// ([`IdentitySecrets::from_hex`] and
/// [`IdentitySecrets::decode`] are the two constructors, and
/// [`IdentitySecrets::into_identity`] is the single exit).
pub struct IdentitySecrets {
    entity: [u8; 32],
    noise: [u8; 32],
}

impl core::fmt::Debug for IdentitySecrets {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never the scalars. The whole point of the type is that they
        // do not escape by accident.
        f.write_str("IdentitySecrets(<redacted>)")
    }
}

impl Drop for IdentitySecrets {
    fn drop(&mut self) {
        // Best effort and honestly labelled: `zeroize` uses a
        // volatile write and a compiler fence, so this scalar is gone
        // from *this* allocation. It says nothing about copies the
        // JavaScript heap made before wasm ever saw the bytes — see
        // the module header on what the origin boundary is.
        self.entity.zeroize();
        self.noise.zeroize();
    }
}

impl IdentitySecrets {
    /// Generate both scalars from the platform CSPRNG
    /// (`crypto.getRandomValues` in the browser).
    pub fn generate() -> Result<Self> {
        let mut entity = [0u8; 32];
        let mut noise = [0u8; 32];
        getrandom::fill(&mut entity)
            .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
        getrandom::fill(&mut noise)
            .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
        Ok(Self { entity, noise })
    }

    /// Custodial injection, the `MeshNodeConfig::entity_keypair`
    /// shape: the host holds the entity scalar and optionally the
    /// Noise static.
    ///
    /// A custodial entity key with no Noise half gets a fresh Noise
    /// static, because the Noise static is a session key the mesh
    /// learns from the *signed* announcement — the identity is the
    /// Ed25519 half.
    pub fn from_hex(entity_hex: &str, noise_hex: Option<&str>) -> Result<Self> {
        let entity = thirty_two(entity_hex, "entitySecretHex")?;
        let noise = match noise_hex {
            Some(hex) => thirty_two(hex, "noiseSecretHex")?,
            None => {
                let mut noise = [0u8; 32];
                getrandom::fill(&mut noise)
                    .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
                noise
            }
        };
        Ok(Self { entity, noise })
    }

    /// The versioned plaintext: magic, entity scalar, Noise scalar.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(IDENTITY_BLOB_LEN);
        out.extend_from_slice(IDENTITY_BLOB_MAGIC);
        out.extend_from_slice(&self.entity);
        out.extend_from_slice(&self.noise);
        out
    }

    /// Parse a plaintext blob.
    ///
    /// Four distinct refusals, in this order, because they mean four
    /// different things to whoever reads the message:
    ///
    /// 1. **Too short to identify** — the record is corrupt, and
    ///    nothing further can be said about it.
    /// 2. **A known family, an unknown version** — a record written
    ///    by a later build. Reported as "this build speaks v1", which
    ///    is the answer a migration needs; comparing the whole magic
    ///    first would have called it foreign instead.
    /// 3. **A foreign tag** — not our schema at all.
    /// 4. **The right tag, the wrong length** — our schema,
    ///    truncated or padded.
    ///
    /// Nothing is read out of a blob that failed any of them: reading
    /// 64 bytes from the middle of someone else's record would mint an
    /// identity from their data.
    pub fn decode(blob: &[u8]) -> Result<Self> {
        if blob.len() < IDENTITY_BLOB_MAGIC.len() {
            return Err(LeafError::Identity(format!(
                "stored identity is {} bytes, too short to carry a schema tag",
                blob.len()
            )));
        }
        if blob.starts_with(IDENTITY_BLOB_PREFIX) {
            let version = blob[IDENTITY_BLOB_PREFIX.len()];
            if version != IDENTITY_BLOB_VERSION {
                return Err(LeafError::Identity(format!(
                    "the stored identity is v{}; this build speaks v{}",
                    version as char, IDENTITY_BLOB_VERSION as char
                )));
            }
        } else {
            return Err(LeafError::Identity(
                "stored identity carries a foreign schema tag".into(),
            ));
        }
        if blob.len() != IDENTITY_BLOB_LEN {
            return Err(LeafError::Identity(format!(
                "stored identity is {} bytes, expected {IDENTITY_BLOB_LEN}",
                blob.len()
            )));
        }
        let scalars = &blob[IDENTITY_BLOB_MAGIC.len()..];
        let mut out = Self {
            entity: [0u8; 32],
            noise: [0u8; 32],
        };
        out.entity.copy_from_slice(&scalars[..32]);
        out.noise.copy_from_slice(&scalars[32..]);
        Ok(out)
    }

    /// Build the live identity. The scalars are copied into the
    /// keypairs and this value's own copies are overwritten when it
    /// drops.
    pub fn into_identity(self) -> LeafIdentity {
        LeafIdentity::from_secrets(EntityKeypair::from_secret(self.entity), self.noise)
    }

    /// The two scalars as hex, for the one caller that needs them:
    /// handing the identity to `LeafNode::connect`, whose options
    /// object takes `entitySecretHex` / `noiseSecretHex`.
    ///
    /// **Why this exists and why it is not a hole.** A new leader
    /// re-bootstraps with the *same* identity, and the only way in is
    /// the same custodial option a host application uses. So the
    /// storage path converts to exactly the shape the custodial path
    /// arrives in — that is what makes them one surface. It does not
    /// weaken anything this crate claimed: WebCrypto decryption
    /// already returned these bytes to this context, and the module
    /// header says plainly that the origin is the trust boundary. A
    /// caller that wants the scalars never to become strings holds
    /// custody itself and never calls the storage path at all.
    pub fn to_hex_pair(&self) -> (String, String) {
        (hex_lower(&self.entity), hex_lower(&self.noise))
    }
}

/// A 32-byte scalar from hex, named in the error so a host that
/// passed the wrong field knows which one.
fn thirty_two(hex: &str, field: &str) -> Result<[u8; 32]> {
    unhex(hex)?
        .try_into()
        .map_err(|_| LeafError::Identity(format!("{field} must be 32 bytes of hex")))
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

    /// The at-rest format is the one thing in §8 that can silently
    /// corrupt a user's identity, and it has no native equivalent to
    /// diff against — so it is pinned here, on the side of the line
    /// that does not need a browser.
    #[test]
    fn the_at_rest_blob_round_trips_and_names_the_scalars_it_carries() {
        let secrets = IdentitySecrets::from_hex(&"11".repeat(32), Some(&"22".repeat(32)))
            .expect("two valid scalars");
        let blob = secrets.encode();
        assert_eq!(blob.len(), IDENTITY_BLOB_LEN);
        assert!(blob.starts_with(IDENTITY_BLOB_MAGIC));

        let restored = IdentitySecrets::decode(&blob).expect("its own encoding");
        let before = secrets.into_identity();
        let after = restored.into_identity();
        // The identity, not the bytes: the derivations are what the
        // mesh keys on, and a blob that round-tripped into a
        // different node id would be worse than one that failed.
        assert_eq!(before.node_id(), after.node_id());
        assert_eq!(before.origin_hash(), after.origin_hash());
        assert_eq!(before.entity().entity_id(), after.entity().entity_id());
        assert_eq!(before.noise().public_key(), after.noise().public_key());
        assert_eq!(before.fingerprint(), after.fingerprint());
    }

    /// Four refusals, four different messages. The version case is
    /// the one a future build actually hits, and it must not be
    /// reported as foreign data.
    #[test]
    fn a_stored_blob_is_refused_four_distinguishable_ways() {
        let good = IdentitySecrets::from_hex(&"33".repeat(32), Some(&"44".repeat(32)))
            .expect("valid")
            .encode();

        let too_short = IdentitySecrets::decode(&good[..8])
            .expect_err("a blob too short to carry a tag must be refused");
        assert!(
            too_short
                .to_string()
                .contains("too short to carry a schema tag"),
            "unexpected message: {too_short}"
        );

        let mut v2 = good.clone();
        v2[IDENTITY_BLOB_PREFIX.len()] = b'2';
        let wrong_version =
            IdentitySecrets::decode(&v2).expect_err("a v2 record must be refused by a v1 build");
        assert!(
            wrong_version
                .to_string()
                .contains("is v2; this build speaks v1"),
            "a future version must be named as one, not called foreign: {wrong_version}"
        );

        let mut foreign = good.clone();
        foreign[..4].copy_from_slice(b"oops");
        let foreign_err =
            IdentitySecrets::decode(&foreign).expect_err("a foreign tag must be refused");
        assert!(
            foreign_err.to_string().contains("foreign schema tag"),
            "unexpected message: {foreign_err}"
        );

        let mut truncated = good.clone();
        truncated.truncate(IDENTITY_BLOB_LEN - 1);
        let length_err = IdentitySecrets::decode(&truncated)
            .expect_err("our schema at the wrong length must be refused");
        assert!(
            length_err.to_string().contains("expected"),
            "unexpected message: {length_err}"
        );

        let mut padded = good;
        padded.push(0);
        assert!(
            IdentitySecrets::decode(&padded).is_err(),
            "extra trailing bytes must be refused, not ignored"
        );
    }

    /// The custodial path is the same surface as the storage one: one
    /// value type, one exit. A custodial entity key with no Noise half
    /// still produces a usable identity, because the Noise static is a
    /// session key the mesh learns from the signed announcement.
    #[test]
    fn custodial_injection_is_deterministic_in_the_half_that_names_the_node() {
        let entity_hex = "55".repeat(32);
        let a = IdentitySecrets::from_hex(&entity_hex, None)
            .expect("an entity key alone is enough")
            .into_identity();
        let b = IdentitySecrets::from_hex(&entity_hex, None)
            .expect("an entity key alone is enough")
            .into_identity();
        assert_eq!(
            a.node_id(),
            b.node_id(),
            "the node id derives from the Ed25519 half alone, so it must not \
             move when the Noise half is freshly generated"
        );
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(
            a.noise().public_key(),
            b.noise().public_key(),
            "a generated Noise half must actually be fresh"
        );

        assert!(
            IdentitySecrets::from_hex("abcd", None).is_err(),
            "a short entity scalar must be refused, not padded"
        );
        assert!(
            IdentitySecrets::from_hex(&entity_hex, Some("abcd")).is_err(),
            "a short Noise scalar must be refused"
        );
        let named = IdentitySecrets::from_hex("zz", None).expect_err("not hex");
        assert!(
            named.to_string().contains("not hex"),
            "the refusal must say what was wrong: {named}"
        );
    }

    /// The lock name must be stable, identity-specific, and derived
    /// from the public half — naming a lock must never involve a
    /// secret, and two identities must not contend for one lock.
    #[test]
    fn the_fingerprint_is_a_stable_domain_separated_name() {
        let one = LeafIdentity::from_secrets(EntityKeypair::from_secret([7u8; 32]), [8u8; 32]);
        let same = LeafIdentity::from_secrets(EntityKeypair::from_secret([7u8; 32]), [9u8; 32]);
        let other = LeafIdentity::from_secrets(EntityKeypair::from_secret([6u8; 32]), [8u8; 32]);

        assert_eq!(one.fingerprint().len(), 32);
        assert!(one.fingerprint().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            one.fingerprint(),
            same.fingerprint(),
            "the fingerprint names the identity, which is the Ed25519 half"
        );
        assert_ne!(
            one.fingerprint(),
            other.fingerprint(),
            "two identities must not contend for one lock"
        );
        assert!(
            !one.entity().entity_id_hex().starts_with(&one.fingerprint()),
            "the fingerprint must not be a prefix of the entity id, or a \
             reader will treat the lock name as one"
        );
    }

    #[test]
    fn a_debug_print_of_the_at_rest_secrets_carries_neither_scalar() {
        let secrets =
            IdentitySecrets::from_hex(&"11".repeat(32), Some(&"22".repeat(32))).expect("valid");
        let text = format!("{secrets:?}");
        assert!(
            !text.contains("11") && !text.contains("22"),
            "the at-rest secrets leaked into Debug: {text}"
        );
    }
}
