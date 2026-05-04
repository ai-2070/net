//! Cryptographic entity identity for Net nodes.
//!
//! An entity is identified by its ed25519 public key. The identity is
//! independent of network addresses — an entity can migrate across nodes.
//! The u64 node IDs used in swarm/routing are derived from the public key.

use blake2::{
    digest::{consts::U32, Mac},
    Blake2sMac,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// Entity identity — a 32-byte ed25519 public key.
///
/// This is the canonical identity for a node in the mesh. All other
/// identifiers (node_id, origin_hash) are derived from this.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EntityId(pub [u8; 32]);

impl EntityId {
    /// Create an EntityId from raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Get the raw public key bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derive the 8-byte origin hash for application-layer
    /// accounting (DashMap keys, `CausalLink`, `EventMeta`,
    /// migration registries). The per-packet
    /// `NetHeader::origin_hash` stays u32 (routing fast-path);
    /// callers there downcast `kp.origin_hash() as u32`.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        let hash = self.blake2s_hash(b"net-origin-v1");
        u64::from_le_bytes(hash[0..8].try_into().unwrap())
    }

    /// Derive the 8-byte node ID for swarm/routing.
    ///
    /// This replaces arbitrary u64 node IDs with cryptographically derived
    /// ones, binding node identity to the entity keypair.
    #[inline]
    pub fn node_id(&self) -> u64 {
        let hash = self.blake2s_hash(b"net-node-id-v1");
        u64::from_le_bytes(hash[0..8].try_into().unwrap())
    }

    /// Get the ed25519 verifying key for signature verification.
    pub fn verifying_key(&self) -> Result<VerifyingKey, EntityError> {
        VerifyingKey::from_bytes(&self.0).map_err(|_| EntityError::InvalidPublicKey)
    }

    /// Verify a signature against this entity's public key.
    ///
    /// Uses `verify_strict` so that signature malleability (the
    /// `(R, S + L)` variant permitted by the lax `verify`) is
    /// rejected. Callers cache tokens / announcements keyed on the
    /// signed bytes; accepting malleated variants would let the
    /// same logical message appear under two different byte
    /// encodings and bypass content-hash dedup.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), EntityError> {
        let vk = self.verifying_key()?;
        vk.verify_strict(message, signature)
            .map_err(|_| EntityError::InvalidSignature)
    }

    /// Compute BLAKE2s-MAC hash of the public key with a domain label.
    fn blake2s_hash(&self, label: &[u8]) -> [u8; 32] {
        let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(label)
            .expect("BLAKE2s accepts variable-length keys");
        Mac::update(&mut mac, &self.0);
        let result = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }
}

impl std::fmt::Debug for EntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EntityId({})", hex_short(&self.0))
    }
}

impl std::fmt::Display for EntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex_short(&self.0))
    }
}

// Hex-encoded JSON round-trip (to match the `Signature64` pattern
// on `CapabilityAnnouncement`); raw bytes for non-human-readable
// formats. Keeps the announcement wire format self-describing.
impl serde::Serialize for EntityId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex::encode(self.0))
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
        if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            let bytes = hex::decode(&hex_str).map_err(serde::de::Error::custom)?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("entity_id must be 32 bytes"));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(EntityId(arr))
        } else {
            let bytes = <Vec<u8>>::deserialize(deserializer)?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("entity_id must be 32 bytes"));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(EntityId(arr))
        }
    }
}

/// Entity keypair — ed25519 signing key + public identity.
///
/// This is the root of trust for a node. The signing key must be
/// kept secret. The `EntityId` (public key) is freely shareable.
///
/// # Full vs. public-only
///
/// Most keypairs carry both halves: a `SigningKey` (32-byte secret
/// seed) plus the derived `EntityId`. The migration-target path
/// may instead receive a **public-only** keypair — same `entity_id`
/// and `origin_hash`, but with the signing half absent. Public-only
/// keypairs satisfy every `entity_id` / `origin_hash` / `node_id`
/// query the mesh does routinely, but refuse to sign. This exists
/// because a daemon that migrated under "transport_identity = false"
/// (see `DAEMON_IDENTITY_MIGRATION_PLAN.md`) reaches the target with
/// its public identity intact but no private material — the plan's
/// deliberate trade-off for workloads that don't need post-migration
/// signing capability.
///
/// Callers that may receive a public-only keypair should use
/// [`Self::try_sign`] / [`Self::is_read_only`]; [`Self::sign`]
/// panics on a public-only keypair because most call sites own a
/// freshly generated keypair and a silent "signed with zeros"
/// fallback would be worse than a panic.
pub struct EntityKeypair {
    /// `None` marks a public-only keypair that was transferred without
    /// its private half (or whose private half has been explicitly
    /// [zeroized](Self::zeroize)). `Some(_)` is the normal path.
    signing_key: Option<SigningKey>,
    entity_id: EntityId,
    /// Cached origin_hash.
    origin_hash: u64,
    /// Cached node_id (computed once)
    node_id: u64,
}

impl EntityKeypair {
    /// Generate a new random keypair.
    ///
    /// `getrandom::fill` failure is a fatal condition for an
    /// identity layer that issues secret keys (predictable bytes
    /// produce a forgeable ed25519 secret), so the safe response
    /// is to terminate the process rather than unwind. We use
    /// `std::process::abort()` instead of `expect`/`panic!` because
    /// `abort` does not unwind and is `extern "C"`-safe — these
    /// helpers are reachable from the FFI bindings under
    /// `ffi/mesh.rs`, where unwinding through an `extern "C"` frame
    /// is undefined behaviour.
    pub fn generate() -> Self {
        let mut rng_bytes = [0u8; 32];
        if let Err(e) = getrandom::fill(&mut rng_bytes) {
            eprintln!(
                "FATAL: EntityKeypair::generate getrandom failure ({e:?}); aborting to avoid weak ed25519 secret"
            );
            std::process::abort();
        }
        let signing_key = SigningKey::from_bytes(&rng_bytes);
        // Zeroize secret material — volatile write prevents optimizer elision
        for byte in rng_bytes.iter_mut() {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        Self::from_signing_key(signing_key)
    }

    /// Create from an existing ed25519 signing key.
    pub fn from_signing_key(signing_key: SigningKey) -> Self {
        let verifying_key = signing_key.verifying_key();
        let entity_id = EntityId::from_bytes(verifying_key.to_bytes());
        let origin_hash = entity_id.origin_hash();
        let node_id = entity_id.node_id();
        Self {
            signing_key: Some(signing_key),
            entity_id,
            origin_hash,
            node_id,
        }
    }

    /// Create from raw secret key bytes (32 bytes).
    pub fn from_bytes(secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&secret);
        Self::from_signing_key(signing_key)
    }

    /// Create a public-only keypair — an `EntityId` without its
    /// signing half. Sign attempts return [`EntityError::ReadOnly`]
    /// (via [`Self::try_sign`]) or panic (via [`Self::sign`]).
    ///
    /// Used by the migration-target path when a caller opts out of
    /// private-key transport; the daemon keeps its public identity
    /// (so `origin_hash` stays stable and the causal chain continues)
    /// but cannot sign new capability announcements or mint new
    /// permission tokens from the target.
    pub fn public_only(entity_id: EntityId) -> Self {
        let origin_hash = entity_id.origin_hash();
        let node_id = entity_id.node_id();
        Self {
            signing_key: None,
            entity_id,
            origin_hash,
            node_id,
        }
    }

    /// Get the entity identity (public key).
    #[inline]
    pub fn entity_id(&self) -> &EntityId {
        &self.entity_id
    }

    /// Get the cached origin hash. The per-packet
    /// `NetHeader::origin_hash` callers downcast via `as u32`.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.origin_hash
    }

    /// Get the cached node ID for swarm/routing.
    #[inline]
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// `true` iff this keypair has no signing half. Public-only
    /// keypairs survive every `entity_id` / `origin_hash` query but
    /// return [`EntityError::ReadOnly`] from [`Self::try_sign`] and
    /// [`Self::try_secret_bytes`].
    #[inline]
    pub fn is_read_only(&self) -> bool {
        self.signing_key.is_none()
    }

    /// Sign a message. Panics on a public-only keypair; callers
    /// that might hold one must use [`Self::try_sign`] instead.
    ///
    /// # Panics
    ///
    /// If this keypair is public-only (see [`Self::is_read_only`]).
    #[inline]
    pub fn sign(&self, message: &[u8]) -> Signature {
        match &self.signing_key {
            Some(sk) => sk.sign(message),
            None => panic!(
                "EntityKeypair::sign called on public-only keypair — use try_sign \
                 or check is_read_only() before signing",
            ),
        }
    }

    /// Fallible sign. Returns [`EntityError::ReadOnly`] when this
    /// keypair is public-only; otherwise delegates to the ed25519
    /// signing path.
    #[inline]
    pub fn try_sign(&self, message: &[u8]) -> Result<Signature, EntityError> {
        self.signing_key
            .as_ref()
            .map(|sk| sk.sign(message))
            .ok_or(EntityError::ReadOnly)
    }

    /// Get the raw secret key bytes. Panics on a public-only
    /// keypair; callers that might hold one must use
    /// [`Self::try_secret_bytes`].
    ///
    /// Handle with care — this is the root secret.
    ///
    /// # Panics
    ///
    /// If this keypair is public-only (see [`Self::is_read_only`]).
    pub fn secret_bytes(&self) -> &[u8; 32] {
        match &self.signing_key {
            Some(sk) => sk.as_bytes(),
            None => panic!(
                "EntityKeypair::secret_bytes called on public-only keypair — use \
                 try_secret_bytes or check is_read_only()",
            ),
        }
    }

    /// Fallible `secret_bytes`. Returns [`EntityError::ReadOnly`]
    /// for public-only keypairs.
    pub fn try_secret_bytes(&self) -> Result<&[u8; 32], EntityError> {
        self.signing_key
            .as_ref()
            .map(|sk| sk.as_bytes())
            .ok_or(EntityError::ReadOnly)
    }

    /// Zeroize the signing half in place, converting this keypair
    /// into public-only. The `entity_id` / `origin_hash` / `node_id`
    /// remain available; further `sign` / `secret_bytes` calls go
    /// down the `try_*` / read-only path.
    ///
    /// Called by the migration-source handler after
    /// `ActivateAck` arrives from the target — the source no longer
    /// needs to sign on behalf of this daemon, and holding the key
    /// longer than necessary widens the dual-custody window beyond
    /// the plan's invariant. Idempotent; second call is a no-op.
    pub fn zeroize(&mut self) {
        // `ed25519_dalek::SigningKey` carries `ZeroizeOnDrop`, so
        // dropping the value we take out of `self.signing_key` runs
        // the zeroize. The compiler-inserted drop is sufficient; no
        // explicit `Zeroize::zeroize` needed (and indeed upstream
        // does not derive the bare `Zeroize` trait on `SigningKey`
        // in 2.x). Double-zeroize is safe because `Option::take`
        // leaves `None` behind.
        let _ = self.signing_key.take();
    }
}

impl Clone for EntityKeypair {
    fn clone(&self) -> Self {
        match &self.signing_key {
            Some(sk) => Self::from_signing_key(sk.clone()),
            None => Self::public_only(self.entity_id.clone()),
        }
    }
}

impl std::fmt::Debug for EntityKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secret_marker = if self.signing_key.is_some() {
            "[REDACTED]"
        } else {
            "[PUBLIC-ONLY]"
        };
        f.debug_struct("EntityKeypair")
            .field("entity_id", &self.entity_id)
            .field("origin_hash", &format!("{:08x}", self.origin_hash))
            .field("node_id", &format!("{:016x}", self.node_id))
            .field("secret", &secret_marker)
            .finish()
    }
}

/// Errors from entity identity operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityError {
    /// Public key bytes are not a valid ed25519 point.
    InvalidPublicKey,
    /// Signature verification failed.
    InvalidSignature,
    /// Operation requires the signing half of the keypair, but this
    /// keypair is public-only — either constructed via
    /// [`EntityKeypair::public_only`] or zeroized via
    /// [`EntityKeypair::zeroize`].
    ReadOnly,
}

impl std::fmt::Display for EntityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPublicKey => write!(f, "invalid public key"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::ReadOnly => write!(f, "keypair is public-only; signing is not available"),
        }
    }
}

impl std::error::Error for EntityError {}

/// Format first 8 bytes as hex for debug display.
fn hex_short(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
        + "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generate() {
        let kp1 = EntityKeypair::generate();
        let kp2 = EntityKeypair::generate();

        // Different keypairs produce different identities
        assert_ne!(kp1.entity_id(), kp2.entity_id());
        assert_ne!(kp1.origin_hash(), kp2.origin_hash());
        assert_ne!(kp1.node_id(), kp2.node_id());
    }

    #[test]
    fn test_keypair_from_bytes_deterministic() {
        let secret = [0x42u8; 32];
        let kp1 = EntityKeypair::from_bytes(secret);
        let kp2 = EntityKeypair::from_bytes(secret);

        assert_eq!(kp1.entity_id(), kp2.entity_id());
        assert_eq!(kp1.origin_hash(), kp2.origin_hash());
        assert_eq!(kp1.node_id(), kp2.node_id());
    }

    #[test]
    fn test_sign_verify() {
        let kp = EntityKeypair::generate();
        let message = b"hello, mesh";

        let signature = kp.sign(message);
        assert!(kp.entity_id().verify(message, &signature).is_ok());
    }

    #[test]
    fn test_verify_wrong_message() {
        let kp = EntityKeypair::generate();
        let signature = kp.sign(b"correct message");

        assert_eq!(
            kp.entity_id().verify(b"wrong message", &signature),
            Err(EntityError::InvalidSignature)
        );
    }

    #[test]
    fn test_verify_wrong_key() {
        let kp1 = EntityKeypair::generate();
        let kp2 = EntityKeypair::generate();
        let message = b"hello";

        let signature = kp1.sign(message);
        assert_eq!(
            kp2.entity_id().verify(message, &signature),
            Err(EntityError::InvalidSignature)
        );
    }

    #[test]
    fn test_origin_hash_nonzero() {
        let kp = EntityKeypair::generate();
        // origin_hash should be non-zero (with overwhelming probability)
        // and consistent with entity_id derivation
        assert_eq!(kp.origin_hash(), kp.entity_id().origin_hash());
    }

    #[test]
    fn test_node_id_nonzero() {
        let kp = EntityKeypair::generate();
        assert_eq!(kp.node_id(), kp.entity_id().node_id());
    }

    #[test]
    fn test_clone_preserves_identity() {
        let kp = EntityKeypair::generate();
        let kp2 = kp.clone();

        assert_eq!(kp.entity_id(), kp2.entity_id());
        assert_eq!(kp.origin_hash(), kp2.origin_hash());

        // Cloned keypair can sign and the original can verify
        let sig = kp2.sign(b"test");
        assert!(kp.entity_id().verify(b"test", &sig).is_ok());
    }

    #[test]
    fn test_entity_id_display() {
        let kp = EntityKeypair::generate();
        let display = format!("{}", kp.entity_id());
        // Should be hex prefix + "..."
        assert!(display.ends_with("..."));
        assert!(display.len() > 4);
    }

    // ---- Public-only + zeroize (Stage 2 of DAEMON_IDENTITY_MIGRATION_PLAN) ----

    #[test]
    fn public_only_preserves_identity_queries() {
        let full = EntityKeypair::generate();
        let public = EntityKeypair::public_only(full.entity_id().clone());
        assert_eq!(public.entity_id(), full.entity_id());
        assert_eq!(public.origin_hash(), full.origin_hash());
        assert_eq!(public.node_id(), full.node_id());
        assert!(public.is_read_only());
        assert!(!full.is_read_only());
    }

    #[test]
    fn public_only_try_sign_returns_read_only() {
        let public = EntityKeypair::public_only(EntityKeypair::generate().entity_id().clone());
        let err = public.try_sign(b"payload").expect_err("must refuse");
        assert_eq!(err, EntityError::ReadOnly);
    }

    #[test]
    fn public_only_try_secret_bytes_returns_read_only() {
        let public = EntityKeypair::public_only(EntityKeypair::generate().entity_id().clone());
        let err = public.try_secret_bytes().expect_err("must refuse");
        assert_eq!(err, EntityError::ReadOnly);
    }

    #[test]
    #[should_panic(expected = "public-only keypair")]
    fn public_only_sign_panics() {
        let public = EntityKeypair::public_only(EntityKeypair::generate().entity_id().clone());
        let _ = public.sign(b"payload");
    }

    #[test]
    #[should_panic(expected = "public-only keypair")]
    fn public_only_secret_bytes_panics() {
        let public = EntityKeypair::public_only(EntityKeypair::generate().entity_id().clone());
        let _ = public.secret_bytes();
    }

    #[test]
    fn zeroize_converts_full_to_public_only() {
        let mut kp = EntityKeypair::generate();
        let entity_before = kp.entity_id().clone();
        assert!(!kp.is_read_only());
        // Sanity: signing works pre-zeroize.
        let sig = kp.try_sign(b"pre").expect("full keypair signs");
        assert!(entity_before.verify(b"pre", &sig).is_ok());

        kp.zeroize();
        assert!(kp.is_read_only());
        assert_eq!(kp.entity_id(), &entity_before);
        assert_eq!(
            kp.try_sign(b"post")
                .expect_err("post-zeroize signing must fail"),
            EntityError::ReadOnly,
        );
    }

    #[test]
    fn zeroize_is_idempotent() {
        let mut kp = EntityKeypair::generate();
        kp.zeroize();
        kp.zeroize();
        assert!(kp.is_read_only());
    }

    #[test]
    fn try_sign_on_full_keypair_matches_sign() {
        let kp = EntityKeypair::generate();
        let try_sig = kp.try_sign(b"m").expect("try_sign");
        let plain_sig = kp.sign(b"m");
        // Ed25519 is deterministic — identical inputs produce
        // identical signatures.
        assert_eq!(try_sig.to_bytes(), plain_sig.to_bytes());
    }

    #[test]
    fn clone_of_public_only_is_public_only() {
        let public = EntityKeypair::public_only(EntityKeypair::generate().entity_id().clone());
        let cloned = public.clone();
        assert!(cloned.is_read_only());
        assert_eq!(cloned.entity_id(), public.entity_id());
    }
}
