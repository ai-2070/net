//! Entity identity inside org credentials: a 32-byte Ed25519
//! verifying key.
//!
//! Core's `identity/entity.rs` half that the wire types need — the
//! id newtype, its two public derivations and `verify_strict` — with
//! all cryptography **reused** from [`crate::identity`]: verification
//! is [`crate::identity::verify_entity_signature`], `node_id` is
//! [`crate::identity::node_id_for_entity`], and signing rides
//! [`crate::identity::EntityKeypair`] (which the proof module takes
//! by reference). What is new here is only the wire newtype and its
//! serde shape.
//!
//! `EntityId` is **not** a bearer secret: `PartialEq` is deliberately
//! the derived non-constant-time one, and `Ord` is lexicographic byte
//! order — the canonical ordering revocation floor maps key on.

use ed25519_dalek::VerifyingKey;

use crate::identity::{hex_lower, node_id_for_entity, unhex, verify_entity_signature};

/// A 32-byte Ed25519 verifying key: the entity's identity.
///
/// `Ord` is the lexicographic byte order; it doubles as the canonical
/// ordering for persisted floor maps (`BTreeMap<(OrgId, EntityId),
/// u32>` in [`crate::org::revocation::RevocationFacts`]).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId(pub [u8; 32]);

impl EntityId {
    /// Create an `EntityId` from raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the 32-byte representation.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The entity's `origin_hash`: keyed BLAKE2s-MAC of the public key
    /// under `net-origin-v1`, truncated to 8 bytes LE.
    ///
    /// The value every packet this identity publishes is stamped
    /// with; the mesh's reverse index maps it back to the node id.
    /// Same derivation as core's `EntityId::origin_hash` and as
    /// [`crate::identity::EntityKeypair::origin_hash`] — the scalar
    /// crosses the wire, so both sides must agree. (The node id half
    /// of the pair is [`crate::identity::node_id_for_entity`], reused
    /// through [`Self::node_id`].)
    pub fn origin_hash(&self) -> u64 {
        use blake2::{
            digest::{consts::U32, KeyInit, Mac},
            Blake2sMac,
        };
        let Ok(mut mac) = <Blake2sMac<U32> as KeyInit>::new_from_slice(b"net-origin-v1") else {
            // Unreachable: BLAKE2s accepts any key up to 32 bytes and
            // the label is a constant.
            unreachable!("BLAKE2s accepts any key up to 32 bytes; the label is a constant")
        };
        Mac::update(&mut mac, &self.0);
        let out = mac.finalize().into_bytes();
        u64::from_le_bytes(out[..8].try_into().unwrap())
    }

    /// The mesh node id derived from this entity key.
    ///
    /// [`crate::identity::node_id_for_entity`] — the one derivation,
    /// reused so a leaf can never announce one identity and publish
    /// under another.
    #[inline]
    pub fn node_id(&self) -> u64 {
        node_id_for_entity(&self.0)
    }

    /// Verify a 64-byte Ed25519 signature over `message` against this
    /// entity.
    ///
    /// `verify_strict` via
    /// [`crate::identity::verify_entity_signature`], matching core's
    /// `EntityId::verify`: the lax `verify` admits the malleable
    /// `(R, S + L)` variant, and org objects are compared and cached
    /// on their signed bytes. A malformed key is classified
    /// [`EntityError::InvalidPublicKey`] and a bad signature
    /// [`EntityError::InvalidSignature`], exactly as core does.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> Result<(), EntityError> {
        VerifyingKey::from_bytes(&self.0).map_err(|_| EntityError::InvalidPublicKey)?;
        verify_entity_signature(&self.0, message, signature)
            .map_err(|_| EntityError::InvalidSignature)
    }
}

impl core::fmt::Debug for EntityId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EntityId({}...)", hex_lower(&self.0[..8]))
    }
}

// Hex string when human-readable (JSON config), raw bytes otherwise —
// mirroring core's `EntityId` impls so the two identity kinds read
// identically in every serialized form. Exactly one byte layout.
impl serde::Serialize for EntityId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex_lower(&self.0))
        } else {
            serializer.serialize_bytes(&self.0)
        }
    }
}

impl<'de> serde::Deserialize<'de> for EntityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            unhex(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("entity_id must be 32 bytes"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(EntityId(arr))
    }
}

/// Failures from entity-identity operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityError {
    /// The 32 bytes are not a valid Ed25519 point.
    InvalidPublicKey,
    /// Signature verification failed.
    InvalidSignature,
    /// A signing operation was attempted on a public-only keypair.
    ///
    /// Core's `EntityKeypair::try_sign` vocabulary. The leaf's
    /// [`crate::identity::EntityKeypair`] is always signing-capable
    /// and cannot produce this, but the variant is kept so the error
    /// vocabulary matches core's one-for-one.
    ReadOnly,
}

impl core::fmt::Display for EntityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidPublicKey => write!(f, "invalid entity public key"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::ReadOnly => write!(f, "keypair is public-only"),
        }
    }
}

impl std::error::Error for EntityError {}
