//! Identity envelope — encrypted daemon-keypair transport.
//!
//! When a daemon migrates, its ed25519 private key has to travel
//! from source to target: the target must sign capability
//! announcements + mint permission tokens under the same
//! `entity_id` the source did, and `origin_hash` stability requires
//! the same underlying seed. Shipping the seed in plaintext (even
//! inside a Noise session) is unacceptable — any middlebox that
//! logs post-decryption payloads would see the key.
//!
//! `IdentityEnvelope` wraps the seed under the target's X25519
//! static public key and attests the wrapping with an ed25519
//! signature from the source's node key. The target verifies the
//! attestation before unsealing, which rejects envelopes that were
//! retargeted to a key an attacker controls.
//!
//! # Sealed-box construction
//!
//! Mirrors libsodium's `crypto_box_seal` shape but substitutes
//! XChaCha20-Poly1305 (already in-tree) for XSalsa20-Poly1305, so
//! we don't have to pull in a second AEAD:
//!
//! ```text
//! ephemeral_sk ← random 32 bytes
//! ephemeral_pk ← x25519_base(ephemeral_sk)
//! shared       ← x25519(ephemeral_sk, target_static_pub)
//! key          ← BLAKE2s-MAC(shared; "net-identity-envelope")[..32]
//! nonce        ← BLAKE2s-MAC(ephemeral_pk || target_static_pub;
//!                            "net-identity-nonce")[..24]
//! ciphertext   ← XChaCha20Poly1305(key, nonce, seed)      (48 bytes)
//! sealed_seed  ← ephemeral_pk (32) || ciphertext (48)     (80 bytes)
//! ```
//!
//! Nonce derivation is deterministic from public material — safe
//! because the ephemeral keypair is single-use and the key is
//! freshly derived per envelope, so `(key, nonce)` is unique per
//! envelope.
//!
//! # Attestation
//!
//! Ed25519 signature from the source node's keypair over:
//!
//! ```text
//! target_static_pub (32) || chain_link.to_bytes() (24)
//! ```
//!
//! Binding the signed transcript to both the target pubkey *and* a
//! specific causal-chain position rejects two attacks at once: a
//! middlebox retargeting the envelope to an attacker-controlled
//! seal key, and a replay of an older envelope at a later chain
//! position under a different migration.
//!
//! # Wire layout (208 bytes fixed)
//!
//! ```text
//! target_static_pub: 32 bytes   (X25519 pubkey — seal recipient)
//! sealed_seed:       80 bytes   (ephemeral_pk || XChaCha ciphertext+tag)
//! signer_pub:        32 bytes   (ed25519 pubkey — source's node key)
//! signature:         64 bytes   (ed25519 over target_static_pub || chain_link)
//! ```

use blake2::{
    digest::{consts::U32, Mac},
    Blake2sMac,
};
use bytes::{Buf, BufMut};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305,
};
use ed25519_dalek::{Signature, VerifyingKey};
use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

use super::entity::{EntityError, EntityKeypair};
use crate::adapter::net::state::causal::{CausalLink, CAUSAL_LINK_SIZE};

/// Wire-format version byte stamped at the head of every
/// serialized `IdentityEnvelope`. Producers always emit this
/// value; readers reject any other byte. Without it, `open`
/// would have to try v1 AAD then fall back to v0 (empty) AAD on
/// failure — doubling AEAD CPU per probe of legitimate v0
/// envelopes during a rolling upgrade. The wire-bump cycle that
/// landed `IDENTITY_ENVELOPE_VERSION = 1` drops v0 support
/// entirely (no backwards compat — the migration cliff is
/// documented in the project release notes).
pub const IDENTITY_ENVELOPE_VERSION: u8 = 1;

/// Fixed wire size of a serialized `IdentityEnvelope`. Bumped
/// from 208 to 209 in the wire-bump to make room for the
/// leading version byte.
pub const IDENTITY_ENVELOPE_SIZE: usize = 1 + 32 + 80 + 32 + 64;

/// Domain separator for the sealed-box AEAD key derivation.
const KDF_DOMAIN_KEY: &[u8] = b"net-identity-envelope";
/// Domain separator for nonce derivation.
const KDF_DOMAIN_NONCE: &[u8] = b"net-identity-nonce";

/// The ed25519 seed is 32 bytes; the sealed payload is the seed
/// plus the AEAD's 16-byte Poly1305 tag plus a 32-byte ephemeral
/// pubkey.
const SEED_LEN: usize = 32;
const TAG_LEN: usize = 16;
const EPH_PK_LEN: usize = 32;

/// Errors from envelope sealing / unsealing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The attestation signature did not verify against the
    /// transcript `target_static_pub || chain_link`.
    InvalidAttestation,
    /// `crypto_box_seal_open` failed — either the target X25519
    /// private key doesn't match the envelope's target pubkey, or
    /// the ciphertext has been tampered with.
    SealOpenFailed,
    /// Decrypted seed produced a keypair whose `origin_hash` does
    /// not match the expected value. Surfaces at the migration
    /// layer, not at the primitive — the primitive returns the
    /// keypair and the caller cross-checks.
    OriginHashMismatch,
    /// Source's `signer_pub` is not a valid ed25519 point.
    InvalidSignerKey,
    /// Attempted to seal with a public-only source keypair (no
    /// signing half). The envelope needs an attestation signature;
    /// a public-only caller can't produce one.
    SourceReadOnly,
    /// Wire-format version byte at the head of the envelope is
    /// not [`IDENTITY_ENVELOPE_VERSION`]. Either the bytes were
    /// produced by a pre-`v1` peer (the rolling-upgrade cliff
    /// documented in the audit-#102 wire bump) or the bytes are
    /// not an `IdentityEnvelope` at all.
    UnknownVersion {
        /// The first byte we read.
        got: u8,
        /// What we expected.
        expected: u8,
    },
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAttestation => {
                write!(f, "identity envelope: attestation signature invalid")
            }
            Self::SealOpenFailed => write!(
                f,
                "identity envelope: seal_open failed (wrong target key or tampered ciphertext)"
            ),
            Self::OriginHashMismatch => write!(
                f,
                "identity envelope: decrypted seed's origin_hash does not match expected"
            ),
            Self::InvalidSignerKey => {
                write!(
                    f,
                    "identity envelope: signer_pub is not a valid ed25519 point"
                )
            }
            Self::SourceReadOnly => write!(
                f,
                "identity envelope: source keypair is public-only; cannot attest"
            ),
            Self::UnknownVersion { got, expected } => write!(
                f,
                "identity envelope: unknown wire version {got:#04x} (expected {expected:#04x})"
            ),
        }
    }
}

impl std::error::Error for EnvelopeError {}

impl From<EntityError> for EnvelopeError {
    fn from(e: EntityError) -> Self {
        match e {
            EntityError::InvalidPublicKey => Self::InvalidSignerKey,
            EntityError::InvalidSignature => Self::InvalidAttestation,
            EntityError::ReadOnly => Self::SourceReadOnly,
        }
    }
}

/// Encrypted + attested daemon-keypair transport.
///
/// Constructed on the source side during `TakeSnapshot`, rides
/// inside `StateSnapshot::identity_envelope`, unsealed on the target
/// during `restore_snapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityEnvelope {
    /// X25519 public key the payload is sealed to — the target
    /// node's static key from the Noise session.
    pub target_static_pub: [u8; 32],
    /// `crypto_box_seal`-style output: 32-byte ephemeral pubkey
    /// concatenated with the 48-byte XChaCha20-Poly1305 ciphertext
    /// (32-byte seed + 16-byte tag).
    pub sealed_seed: [u8; 80],
    /// Source node's ed25519 public key. Target uses this to verify
    /// `signature` before unsealing.
    pub signer_pub: [u8; 32],
    /// Ed25519 signature over `target_static_pub (32) ||
    /// chain_link.to_bytes() (24)`. Binds the envelope to a specific
    /// recipient and a specific migration point.
    pub signature: [u8; 64],
}

impl IdentityEnvelope {
    /// Seal `source_kp`'s private seed to `target_static_pub`, and
    /// attest with `source_kp.sign` over
    /// `target_static_pub || chain_link.to_bytes()`.
    ///
    /// # Errors
    ///
    /// [`EnvelopeError::SourceReadOnly`] if `source_kp` is
    /// public-only — the attestation needs its signing half.
    #[expect(
        clippy::expect_used,
        reason = "XChaCha20Poly1305 with a freshly-derived key+nonce cannot fail on a 32-byte msg, and try_sign on a full keypair (checked above via source_kp.try_sign requirement) cannot fail"
    )]
    pub fn new(
        source_kp: &EntityKeypair,
        target_static_pub: [u8; 32],
        chain_link: &CausalLink,
    ) -> Result<Self, EnvelopeError> {
        let mut seed = source_kp
            .try_secret_bytes()
            .map_err(EnvelopeError::from)?
            .to_owned();

        // Ephemeral X25519 keypair. `StaticSecret` is zeroize-on-drop,
        // so `eph_sk` is wiped as soon as the function returns.
        //
        // Aborts on `getrandom` failure rather than
        // panic-unwinding through the FFI boundary; same
        // rationale as `EntityKeypair::generate`. A predictable
        // X25519 ephemeral secret defeats the envelope's forward
        // secrecy, so termination is the only safe response.
        let mut rng_bytes = [0u8; 32];
        if let Err(e) = getrandom::fill(&mut rng_bytes) {
            eprintln!(
                "FATAL: IdentityEnvelope::seal getrandom failure ({e:?}); aborting to avoid weak X25519 ephemeral"
            );
            std::process::abort();
        }
        let eph_sk = X25519Secret::from(rng_bytes);
        volatile_zero(&mut rng_bytes);
        let eph_pk = X25519Pub::from(&eph_sk);
        let target_pk = X25519Pub::from(target_static_pub);

        let shared = eph_sk.diffie_hellman(&target_pk);
        let mut key = derive_key(shared.as_bytes(), KDF_DOMAIN_KEY);
        let nonce = derive_nonce(eph_pk.as_bytes(), &target_static_pub);

        // Bind `chain_link` to the AEAD via AAD so a tampered link
        // breaks BOTH the attestation signature (already covered)
        // AND the AEAD tag. With `aad: &[]` the chain_link would
        // be bound only to the signature — an attacker who can
        // swap the on-the-wire chain_link for a different one (and
        // re-attest) wouldn't also break the AEAD, narrowing the
        // verification surface.
        let aad_bytes = chain_link.to_bytes();
        let aead = XChaCha20Poly1305::new((&key).into());
        let ciphertext = aead
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &seed,
                    aad: &aad_bytes,
                },
            )
            .expect("XChaCha20Poly1305 encrypt with fresh key+nonce cannot fail on 32-byte msg");
        debug_assert_eq!(ciphertext.len(), SEED_LEN + TAG_LEN);

        // Wipe the local copy of the seed — the AEAD has already
        // consumed it, and we hold the ciphertext only from here on.
        // The source keypair retains its own seed inside
        // `EntityKeypair`; this wipe is about the short-lived `to_owned`
        // copy we made for the `Payload.msg`.
        volatile_zero(&mut seed);
        // Derived AEAD key: also a function of a secret (the shared
        // DH output). Scrub so the stack frame doesn't retain it.
        volatile_zero(&mut key);

        let mut sealed_seed = [0u8; 80];
        sealed_seed[..EPH_PK_LEN].copy_from_slice(eph_pk.as_bytes());
        sealed_seed[EPH_PK_LEN..].copy_from_slice(&ciphertext);

        let transcript = attestation_transcript(&target_static_pub, chain_link);
        let sig = source_kp
            .try_sign(&transcript)
            .expect("try_sign on a full keypair produced above must not fail");

        Ok(Self {
            target_static_pub,
            sealed_seed,
            signer_pub: *source_kp.entity_id().as_bytes(),
            signature: sig.to_bytes(),
        })
    }

    /// Verify the attestation and unseal the sealed seed, returning
    /// a fresh full [`EntityKeypair`] reconstructed from the seed.
    ///
    /// `expected_signer_pub`, when `Some`, asserts the envelope was
    /// produced by the named source identity. The check fires
    /// BEFORE any cryptographic work — an attacker who can inject
    /// a substituted envelope built from THEIR keypair (with
    /// `target_static_pub` set correctly to the actual target)
    /// no longer reaches signature verification or AEAD decrypt
    /// when the caller knows which source they expected. Pass
    /// `None` for the legacy "primitive returns the keypair,
    /// caller cross-checks" pattern; the existing snapshot path
    /// uses the post-decrypt `kp.entity_id() != snapshot.entity_id`
    /// cross-check (`state/snapshot.rs::open_identity_envelope`),
    /// so passing `None` there is sound. New call sites should
    /// pass `Some` whenever the expected source identity is known
    /// up front.
    ///
    /// # Errors
    ///
    /// - [`EnvelopeError::InvalidSignerKey`] if `signer_pub` is not
    ///   a valid ed25519 point, OR (when `expected_signer_pub` is
    ///   `Some`) if the envelope's `signer_pub` doesn't match.
    /// - [`EnvelopeError::InvalidAttestation`] if the attestation
    ///   signature does not verify against the transcript built from
    ///   `target_static_pub || chain_link`.
    /// - [`EnvelopeError::SealOpenFailed`] if the XChaCha AEAD fails
    ///   (wrong target key, tampered ciphertext, tampered chain_link
    ///   AAD post-fix, etc.).
    pub fn open(
        &self,
        target_static_priv: &X25519Secret,
        chain_link: &CausalLink,
        expected_signer_pub: Option<&[u8; 32]>,
    ) -> Result<EntityKeypair, EnvelopeError> {
        // Step 0: early-reject if the caller knows which source
        // identity they expected and this envelope's `signer_pub`
        // doesn't match. Constant-time-ish compare not strictly
        // needed (the field is public and an attacker can already
        // inspect it), but avoiding the cryptographic work below
        // for every wrong envelope is the load-bearing benefit.
        if let Some(expected) = expected_signer_pub {
            if &self.signer_pub != expected {
                return Err(EnvelopeError::InvalidSignerKey);
            }
        }

        // Step 1: verify the attestation. We do this BEFORE
        // unsealing so a tampered envelope can't get anywhere near
        // the decryption path.
        let transcript = attestation_transcript(&self.target_static_pub, chain_link);
        let verifying_key = VerifyingKey::from_bytes(&self.signer_pub)
            .map_err(|_| EnvelopeError::InvalidSignerKey)?;
        let sig = Signature::try_from(&self.signature[..])
            .map_err(|_| EnvelopeError::InvalidAttestation)?;
        verifying_key
            .verify_strict(&transcript, &sig)
            .map_err(|_| EnvelopeError::InvalidAttestation)?;

        // Step 2: the receiver's X25519 pubkey derived from its
        // private key must match the envelope's `target_static_pub`.
        // If it doesn't, the caller handed us the wrong private key
        // (or the envelope was retargeted after the attestation was
        // computed). Fail closed rather than let the XChaCha AEAD
        // silently produce garbage.
        let derived_target_pub = X25519Pub::from(target_static_priv);
        if derived_target_pub.as_bytes() != &self.target_static_pub {
            return Err(EnvelopeError::SealOpenFailed);
        }

        // Step 3: seal_open.
        let (eph_pk_bytes, ct) = self.sealed_seed.split_at(EPH_PK_LEN);
        #[expect(
            clippy::unwrap_used,
            reason = "split_at(EPH_PK_LEN) where EPH_PK_LEN == 32; <[u8; 32]>::try_from(&[u8] of length 32) is infallible"
        )]
        let eph_pk = X25519Pub::from(<[u8; 32]>::try_from(eph_pk_bytes).unwrap());
        let shared = target_static_priv.diffie_hellman(&eph_pk);
        let mut key = derive_key(shared.as_bytes(), KDF_DOMAIN_KEY);
        let nonce = derive_nonce(eph_pk.as_bytes(), &self.target_static_pub);

        // AAD must match what `seal` used so the AEAD tag binds
        // the chain_link to the ciphertext. A tampered link will
        // fail the signature check above AND the AEAD tag here.
        //
        // The wire-bump that landed `IDENTITY_ENVELOPE_VERSION = 1`
        // makes the AAD deterministic — there's no fallback path
        // here. Without the version byte, the reader would try v1
        // AAD then fall back to v0 (empty) AAD on failure,
        // doubling AEAD CPU per legitimate-v0-replay probe during
        // a rolling upgrade. With the version byte, v0 envelopes
        // are rejected at `from_bytes`'s version check, so by the
        // time we reach this AEAD attempt, the AAD is known
        // unambiguous.
        let aad = chain_link.to_bytes();
        let aead = XChaCha20Poly1305::new((&key).into());
        let mut seed_vec = match aead.decrypt((&nonce).into(), Payload { msg: ct, aad: &aad }) {
            Ok(v) => v,
            Err(_) => {
                // Scrub the derived AEAD `key` BEFORE returning
                // Err so the key (a function of the shared DH
                // output — sensitive material) doesn't sit on
                // the stack until natural drop. `[u8; 32]`'s
                // default Drop does NOT zeroize, so an early
                // return via `?` would leak it.
                volatile_zero(&mut key);
                return Err(EnvelopeError::SealOpenFailed);
            }
        };
        if seed_vec.len() != SEED_LEN {
            // Even on a length-mismatch error, scrub the buffer
            // before dropping — it holds (partial) decrypted secret
            // material regardless of length.
            volatile_zero(&mut seed_vec);
            volatile_zero(&mut key);
            return Err(EnvelopeError::SealOpenFailed);
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&seed_vec);
        // AEAD returned an owned `Vec<u8>` holding the decrypted seed.
        // Its `Drop` does NOT zeroize — `alloc::Vec` frees the backing
        // storage without scrubbing, so a later allocation could
        // observe the seed bytes in reused heap memory. Wipe through
        // a volatile write before drop runs. Length-only wipe is
        // enough because XChaCha20Poly1305::decrypt returns a tight
        // Vec (len == capacity == SEED_LEN on the happy path we
        // validated above).
        volatile_zero(&mut seed_vec);

        // The derived ed25519 public key MUST match `signer_pub` —
        // otherwise the sender sealed a seed that doesn't correspond
        // to the identity they attested with. Fail closed.
        let kp = EntityKeypair::from_bytes(seed);
        // Wipe the local copy of the seed; `kp` owns its own. Do
        // this before the signer_pub check so an early-return on
        // mismatch doesn't leave secret material on the stack.
        volatile_zero(&mut seed);
        volatile_zero(&mut key);
        if kp.entity_id().as_bytes() != &self.signer_pub {
            return Err(EnvelopeError::InvalidAttestation);
        }

        Ok(kp)
    }

    /// Serialize to its fixed 209-byte wire layout. First byte is
    /// [`IDENTITY_ENVELOPE_VERSION`]. Producer always stamps the
    /// current version; readers reject any other byte via
    /// [`Self::from_bytes`].
    pub fn to_bytes(&self) -> [u8; IDENTITY_ENVELOPE_SIZE] {
        let mut buf = [0u8; IDENTITY_ENVELOPE_SIZE];
        let mut cursor = &mut buf[..];
        cursor.put_u8(IDENTITY_ENVELOPE_VERSION);
        cursor.put_slice(&self.target_static_pub);
        cursor.put_slice(&self.sealed_seed);
        cursor.put_slice(&self.signer_pub);
        cursor.put_slice(&self.signature);
        buf
    }

    /// Deserialize from bytes. Returns `None` if the input is
    /// not exactly [`IDENTITY_ENVELOPE_SIZE`] bytes OR if the
    /// leading version byte isn't [`IDENTITY_ENVELOPE_VERSION`].
    /// Trailing bytes are an error because a short envelope is
    /// indistinguishable from a truncation, and a long envelope
    /// would swallow data the parent frame expects to consume
    /// next.
    ///
    /// Without the version byte, the reader would have to try v1
    /// AAD then fall back to v0 (empty AAD) on AEAD failure —
    /// doubled CPU per legitimate-v0-replay probe. The version
    /// byte is the deterministic selector; the v0 fallback path
    /// is gone.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() != IDENTITY_ENVELOPE_SIZE {
            return None;
        }
        if data[0] != IDENTITY_ENVELOPE_VERSION {
            return None;
        }
        let mut cursor = &data[1..];
        let mut target_static_pub = [0u8; 32];
        cursor.copy_to_slice(&mut target_static_pub);
        let mut sealed_seed = [0u8; 80];
        cursor.copy_to_slice(&mut sealed_seed);
        let mut signer_pub = [0u8; 32];
        cursor.copy_to_slice(&mut signer_pub);
        let mut signature = [0u8; 64];
        cursor.copy_to_slice(&mut signature);
        Some(Self {
            target_static_pub,
            sealed_seed,
            signer_pub,
            signature,
        })
    }
}

// ---- helpers --------------------------------------------------------

/// Transcript bytes signed by the source and verified by the target:
/// `target_static_pub (32) || chain_link.to_bytes() (CAUSAL_LINK_SIZE)`.
///
/// Width follows `CAUSAL_LINK_SIZE` so a wire-format change to
/// the causal link doesn't require a hand-edited length here.
fn attestation_transcript(
    target_static_pub: &[u8; 32],
    chain_link: &CausalLink,
) -> [u8; 32 + CAUSAL_LINK_SIZE] {
    let mut out = [0u8; 32 + CAUSAL_LINK_SIZE];
    out[..32].copy_from_slice(target_static_pub);
    out[32..].copy_from_slice(&chain_link.to_bytes());
    out
}

/// Domain-separated key derivation. We already use BLAKE2s-MAC
/// elsewhere in the identity layer (for `origin_hash` / `node_id`);
/// reusing it keeps the primitive surface minimal.
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; label slices are always short labels"
)]
fn derive_key(shared: &[u8; 32], label: &[u8]) -> [u8; 32] {
    let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(label)
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut mac, shared);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Scrub a byte slice with `write_volatile` so the compiler can't
/// elide the wipe. Centralized so every secret-bearing buffer in
/// this module uses the same idiom — missing a site has already
/// bitten us once (see Cubic-AI P1 on `new` + `open`), and a helper
/// makes future sites easier to audit.
///
/// `Vec<u8>`: the iteration bound is `len()`, not `capacity()`. The
/// AEAD returns a tight buffer on the happy path, so this is
/// sufficient; callers that know the Vec has excess capacity should
/// truncate first or use a different primitive.
fn volatile_zero(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        // SAFETY: `byte` is a valid mutable reference for the
        // lifetime of this call, which is all `write_volatile` needs.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

/// Deterministic nonce: BLAKE2s-MAC keyed with a domain label,
/// input = `eph_pk || target_pk`. Truncated to 24 bytes for the
/// XChaCha nonce.
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; KDF_DOMAIN_NONCE is a short compile-time-constant label"
)]
fn derive_nonce(eph_pk: &[u8; 32], target_pk: &[u8; 32]) -> [u8; 24] {
    let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(KDF_DOMAIN_NONCE)
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut mac, eph_pk);
    Mac::update(&mut mac, target_pk);
    let result = mac.finalize().into_bytes();
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&result[..24]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::state::causal::CausalLink;

    fn fresh_x25519() -> (X25519Secret, [u8; 32]) {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let sk = X25519Secret::from(seed);
        let pk = X25519Pub::from(&sk);
        (sk, *pk.as_bytes())
    }

    fn chain_link_at(seq: u64) -> CausalLink {
        CausalLink {
            origin_hash: 0xDEAD_BEEF,
            horizon_encoded: 0,
            sequence: seq,
            parent_hash: 0,
        }
    }

    fn raw_fixture() -> IdentityEnvelope {
        IdentityEnvelope {
            target_static_pub: [0xAA; 32],
            sealed_seed: [0xBB; 80],
            signer_pub: [0xCC; 32],
            signature: [0xDD; 64],
        }
    }

    // ---- Wire format ----

    #[test]
    fn wire_size_is_209_bytes() {
        // Wire bump: 208 → 209 (one leading version byte).
        assert_eq!(IDENTITY_ENVELOPE_SIZE, 209);
        assert_eq!(raw_fixture().to_bytes().len(), 209);
    }

    #[test]
    fn first_byte_is_version_marker() {
        let bytes = raw_fixture().to_bytes();
        assert_eq!(bytes[0], IDENTITY_ENVELOPE_VERSION);
        assert_eq!(IDENTITY_ENVELOPE_VERSION, 1);
    }

    #[test]
    fn from_bytes_rejects_unknown_version() {
        let mut bytes = raw_fixture().to_bytes();
        bytes[0] = 0; // pre-#102 v0 wire shape (no version byte)
        assert!(
            IdentityEnvelope::from_bytes(&bytes).is_none(),
            "post-#102 reader must reject the v0 shape; rolling-upgrade compat is gone"
        );
        bytes[0] = 0xFF;
        assert!(IdentityEnvelope::from_bytes(&bytes).is_none());
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let env = raw_fixture();
        let bytes = env.to_bytes();
        let decoded = IdentityEnvelope::from_bytes(&bytes).expect("roundtrip");
        assert_eq!(decoded, env);
    }

    #[test]
    fn from_bytes_rejects_truncated() {
        let env = raw_fixture();
        let bytes = env.to_bytes();
        assert!(IdentityEnvelope::from_bytes(&bytes[..208]).is_none());
        assert!(IdentityEnvelope::from_bytes(&[]).is_none());
    }

    #[test]
    fn from_bytes_rejects_trailing_garbage() {
        let env = raw_fixture();
        let mut bytes = env.to_bytes().to_vec();
        bytes.push(0xFF);
        assert!(IdentityEnvelope::from_bytes(&bytes).is_none());
    }

    // ---- Seal / open ----

    #[test]
    fn seal_open_roundtrip() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(7);

        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        let opened = env.open(&target_sk, &link, None).expect("open");

        assert_eq!(opened.entity_id(), source.entity_id());
        assert_eq!(opened.origin_hash(), source.origin_hash());
        // Opened keypair can actually sign — proves we recovered a
        // working signing half, not just the public bytes.
        let sig = opened.sign(b"post-open");
        assert!(source.entity_id().verify(b"post-open", &sig).is_ok());
    }

    #[test]
    fn seal_open_rejects_tampered_signature() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(1);

        let mut env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        env.signature[0] ^= 0xFF;

        assert_eq!(
            env.open(&target_sk, &link, None).expect_err("must reject"),
            EnvelopeError::InvalidAttestation,
        );
    }

    #[test]
    fn seal_open_rejects_tampered_ciphertext() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(1);

        let mut env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        // Flip a bit inside the ciphertext (past the ephemeral
        // pubkey).
        env.sealed_seed[40] ^= 0xFF;

        assert_eq!(
            env.open(&target_sk, &link, None).expect_err("must reject"),
            EnvelopeError::SealOpenFailed,
        );
    }

    #[test]
    fn seal_open_rejects_wrong_target_key() {
        let source = EntityKeypair::generate();
        let (_, target_pk) = fresh_x25519();
        let (different_sk, _) = fresh_x25519();
        let link = chain_link_at(1);

        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        // `different_sk` is not the private key matching `target_pk`
        // — opening must refuse before even trying the AEAD.
        assert_eq!(
            env.open(&different_sk, &link, None)
                .expect_err("must reject"),
            EnvelopeError::SealOpenFailed,
        );
    }

    #[test]
    fn seal_open_rejects_replay_at_different_chain_link() {
        // The attestation transcript binds to the chain_link; a
        // replay of the same envelope at a different migration point
        // must fail.
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(7);

        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        let later_link = chain_link_at(8);
        assert_eq!(
            env.open(&target_sk, &later_link, None)
                .expect_err("replay at later link must reject"),
            EnvelopeError::InvalidAttestation,
        );
    }

    /// When the caller passes `Some(expected)`, an
    /// envelope built by a different source identity is rejected
    /// EARLY (before any cryptographic work) with
    /// `EnvelopeError::InvalidSignerKey`. Pre-fix the primitive
    /// accepted any well-formed envelope and relied on the caller
    /// to cross-check post-decrypt — a substituted envelope from
    /// an attacker's keypair (with `target_static_pub` set
    /// correctly) reached the AEAD decrypt path.
    #[test]
    fn seal_open_with_expected_signer_pub_rejects_substituted_envelope() {
        let attacker = EntityKeypair::generate();
        let expected = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(1);

        // Attacker builds a perfectly-valid envelope to the actual
        // target, but using THEIR keypair as the signer.
        let env = IdentityEnvelope::new(&attacker, target_pk, &link).expect("seal");

        // Caller knows it expected `expected`, not `attacker`.
        let err = env
            .open(&target_sk, &link, Some(expected.entity_id().as_bytes()))
            .expect_err("substituted envelope must be rejected with expected_signer_pub");
        assert_eq!(err, EnvelopeError::InvalidSignerKey);
    }

    /// `expected_signer_pub == None` preserves the legacy "primitive
    /// returns the keypair, caller cross-checks" pattern — pins
    /// the contract so callers like `state::snapshot::open_identity_envelope`
    /// (which has its own post-decrypt cross-check on `entity_id`)
    /// keep working.
    #[test]
    fn seal_open_with_none_expected_preserves_legacy_behavior() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(1);

        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        let opened = env
            .open(&target_sk, &link, None)
            .expect("legacy None path must still succeed");
        assert_eq!(opened.entity_id(), source.entity_id());
    }

    /// `expected_signer_pub == Some(matching)` succeeds — pins the
    /// happy-path so a future tightening of the early-reject
    /// (e.g. constant-time-compare drift) can't lock out
    /// legitimate callers.
    #[test]
    fn seal_open_with_matching_expected_signer_pub_succeeds() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(1);

        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");
        let opened = env
            .open(&target_sk, &link, Some(source.entity_id().as_bytes()))
            .expect("matching expected_signer_pub must succeed");
        assert_eq!(opened.entity_id(), source.entity_id());
    }

    #[test]
    fn seal_open_rejects_retargeted_envelope() {
        // Attacker-in-the-middle scenario: source sealed to
        // `target_pk_a` and attested against it. Attacker rewrites
        // `target_static_pub` to a key they control and re-seals
        // the ciphertext themselves — but the attestation still
        // covers the *original* target pubkey, so verification fails.
        let source = EntityKeypair::generate();
        let (_target_sk_a, target_pk_a) = fresh_x25519();
        let (target_sk_b, target_pk_b) = fresh_x25519();
        let link = chain_link_at(1);

        let mut env = IdentityEnvelope::new(&source, target_pk_a, &link).expect("seal");
        // Attacker rewrites the target pubkey field only.
        env.target_static_pub = target_pk_b;

        assert_eq!(
            env.open(&target_sk_b, &link, None)
                .expect_err("must reject"),
            EnvelopeError::InvalidAttestation,
        );
    }

    #[test]
    fn new_refuses_public_only_source() {
        let source = EntityKeypair::public_only(EntityKeypair::generate().entity_id().clone());
        let (_, target_pk) = fresh_x25519();
        let link = chain_link_at(1);

        let err = IdentityEnvelope::new(&source, target_pk, &link).expect_err("must refuse");
        assert_eq!(err, EnvelopeError::SourceReadOnly);
    }

    /// Rolling-upgrade compatibility from v0 (pre-version-byte)
    /// was REMOVED in this wire-bump. A hand-built v0 envelope
    /// (or any 208-byte payload that would have been a v0
    /// envelope) is now rejected at `from_bytes`'s version-byte
    /// check — the v0 AEAD fallback path that used to double CPU
    /// per legitimate-v0-replay probe is gone.
    ///
    /// This test pins that the reader rejects any 208-byte input
    /// AND any 209-byte input whose first byte is not
    /// [`IDENTITY_ENVELOPE_VERSION`] = 1.
    #[test]
    fn open_rejects_pre_wire_bump_v0_envelope() {
        let env = raw_fixture();
        let v1_bytes = env.to_bytes();
        // 208 bytes (pre-bump shape): rejected on length.
        assert!(IdentityEnvelope::from_bytes(&v1_bytes[1..]).is_none());
        // 209 bytes with version=0: rejected on version byte.
        let mut v0_shape = v1_bytes;
        v0_shape[0] = 0;
        assert!(IdentityEnvelope::from_bytes(&v0_shape).is_none());
    }

    /// Pin that v1 envelopes open with a single AEAD attempt —
    /// no fallback path remains.
    #[test]
    fn open_accepts_v1_envelope_on_first_try() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(7);

        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal v1");
        let opened = env
            .open(&target_sk, &link, None)
            .expect("v1 envelope must open without fallback");
        assert_eq!(opened.entity_id(), source.entity_id());
    }

    /// CR-5: tampering with the chain_link MUST still be caught,
    /// even with the v0 fallback in place. Pre-CR-5 the v1 AAD
    /// binding was the only AEAD-level chain_link defense; post-fix
    /// the v0 fallback could in principle let an attacker who
    /// forces v0 semantics smuggle a chain_link change past the
    /// AEAD. This is fine because the SIGNATURE check (which
    /// happens BEFORE the AEAD attempt) ALSO binds the chain_link
    /// — a tampered link breaks the signature regardless of which
    /// AEAD path runs. This test pins that defense.
    #[test]
    fn open_rejects_tampered_chain_link_under_v0_fallback() {
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(7);
        let tampered_link = chain_link_at(8);

        // Build a real v1 envelope.
        let env = IdentityEnvelope::new(&source, target_pk, &link).expect("seal");

        // Caller passes a different chain_link than the source
        // signed over. Signature verification fires FIRST and
        // rejects with InvalidAttestation — the v0 fallback never
        // runs because we don't reach AEAD.
        let err = env
            .open(&target_sk, &tampered_link, None)
            .expect_err("tampered chain_link must reject regardless of AEAD path");
        assert_eq!(err, EnvelopeError::InvalidAttestation);
    }

    #[test]
    fn opened_keypair_matches_signer_pub() {
        // The opened keypair's public half must equal the envelope's
        // `signer_pub`. Tampering with `signer_pub` (without
        // re-signing) trips `InvalidAttestation` first; tampering
        // with the sealed seed (such that decryption produces a
        // valid-but-different keypair) trips AEAD first. This test
        // is the belt-and-braces assertion that the round-trip
        // invariant holds on the happy path.
        let source = EntityKeypair::generate();
        let (target_sk, target_pk) = fresh_x25519();
        let link = chain_link_at(42);

        let env = IdentityEnvelope::new(&source, target_pk, &link).unwrap();
        let opened = env.open(&target_sk, &link, None).unwrap();
        assert_eq!(opened.entity_id().as_bytes(), &env.signer_pub);
    }
}
