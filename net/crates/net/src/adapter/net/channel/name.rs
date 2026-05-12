//! Named typed channels for Net.
//!
//! Channels are hierarchical named endpoints (e.g. `"sensors/lidar/front"`).
//! Two hashes are derived from the name via xxh3:
//!
//! - **Canonical [`ChannelHash`] (`u32`)** — used as the substrate-wide key
//!   for auth (`AuthGuard`, `PermissionToken`), config (`ChannelConfigMap`),
//!   storage (`RedexFile`), and metrics. ~4B buckets; birthday-collision
//!   threshold ~65 K channels per process is well above realistic deployment
//!   sizes, so this key is treated as collision-free in fast paths.
//! - **Wire `u16`** — the fast-path hint stamped on every outgoing packet
//!   header for wire-speed filtering by forwarding nodes. 65 K buckets;
//!   routine collisions at scale. Mirrors the
//!   `origin_hash: u64 canonical → u32 wire` precedent in the protocol
//!   layer: per-packet width is fixed by the 64-byte header budget, and
//!   wire-side collisions are benign (only affect filter precision, not
//!   ACL or storage decisions, since those key on the canonical hash).

use dashmap::DashMap;
use xxhash_rust::xxh3::xxh3_64;

/// Maximum channel name length in bytes.
pub const MAX_NAME_LEN: usize = 255;

/// Substrate-wide canonical hash for a [`ChannelName`].
///
/// 32-bit. Used as the canonical key for ACL, storage, and config decisions.
/// Distinct from the wire `u16` hash on `NetHeader::channel_hash`, which is
/// a per-packet fast-path filter hint and may collide; the canonical
/// `ChannelHash` is what auth and storage decisions must key on.
pub type ChannelHash = u32;

/// A validated channel name.
///
/// Names are hierarchical with `/` separators. Valid characters are
/// alphanumeric, `-`, `_`, `.`, and `/`. Names must not be empty,
/// start or end with `/`, or contain `//`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ChannelName(String);

impl ChannelName {
    /// Create a new channel name, validating the format.
    pub fn new(name: &str) -> Result<Self, ChannelError> {
        Self::validate(name)?;
        Ok(Self(name.to_string()))
    }

    /// Get the name as a string slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compute the canonical [`ChannelHash`] (32-bit) for the name.
    ///
    /// This is the substrate-wide key used for ACL, storage, and config
    /// decisions. Collision-resistant at realistic deployment scale
    /// (~65 K channels before birthday-collision threshold).
    #[inline]
    pub fn hash(&self) -> ChannelHash {
        channel_hash(&self.0)
    }

    /// Compute the wire `u16` channel hash for the Net header fast-path.
    ///
    /// The hint stamped on every outgoing packet — fast to compare but only
    /// 65,536 buckets, so it has routine collisions at mesh scale.
    /// Control-plane and storage authorization key on
    /// [`ChannelName::hash`] (canonical `u32`), not on this wire hint.
    #[inline]
    pub fn wire_hash(&self) -> u16 {
        wire_channel_hash(&self.0)
    }

    /// Get the number of path segments.
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }

    /// Check if this name is a prefix of another (for wildcard subscriptions).
    pub fn is_prefix_of(&self, other: &ChannelName) -> bool {
        if self.0.len() >= other.0.len() {
            return self.0 == other.0;
        }
        other.0.starts_with(&self.0) && other.0.as_bytes()[self.0.len()] == b'/'
    }

    fn validate(name: &str) -> Result<(), ChannelError> {
        if name.is_empty() {
            return Err(ChannelError::Empty);
        }
        if name.len() > MAX_NAME_LEN {
            return Err(ChannelError::TooLong(name.len()));
        }
        if name.starts_with('/') || name.ends_with('/') {
            return Err(ChannelError::InvalidFormat(
                "must not start or end with '/'".into(),
            ));
        }
        if name.contains("//") {
            return Err(ChannelError::InvalidFormat("must not contain '//'".into()));
        }
        for ch in name.chars() {
            if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '/') {
                return Err(ChannelError::InvalidChar(ch));
            }
        }
        // Reject segments that are path-traversal tokens. Channel
        // names are also used as on-disk directory path segments in
        // the `redex-disk` feature; `..` would escape the persistent
        // base directory, `.` would alias the current directory and
        // shadow siblings. Rejecting these at name-construction time
        // keeps every downstream path-use safe by construction.
        for seg in name.split('/') {
            if seg == "." || seg == ".." {
                return Err(ChannelError::InvalidFormat(format!(
                    "segment {:?} is reserved",
                    seg
                )));
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ChannelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChannelName({:?})", self.0)
    }
}

impl std::fmt::Display for ChannelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Compute the canonical [`ChannelHash`] (32-bit) from a name string.
///
/// Uses xxh3_64 truncated to 32 bits. This is the substrate-wide canonical
/// key for ACL, storage, and config — collision-resistant at realistic
/// deployment scale.
#[inline]
pub fn channel_hash(name: &str) -> ChannelHash {
    xxh3_64(name.as_bytes()) as u32
}

/// Compute the wire `u16` channel hash from a name string.
///
/// Uses xxh3_64 truncated to 16 bits, consistent with the existing
/// `stream_id_from_key` pattern in the routing module. Used only for the
/// `NetHeader::channel_hash` fast-path hint; ACL/storage decisions must
/// use [`channel_hash`] (canonical `u32`).
#[inline]
pub fn wire_channel_hash(name: &str) -> u16 {
    xxh3_64(name.as_bytes()) as u16
}

/// A channel identifier: name + cached canonical hash.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ChannelId {
    name: ChannelName,
    hash: ChannelHash,
}

impl ChannelId {
    /// Create a new channel ID.
    pub fn new(name: ChannelName) -> Self {
        let hash = name.hash();
        Self { name, hash }
    }

    /// Create from a raw name string.
    pub fn parse(name: &str) -> Result<Self, ChannelError> {
        Ok(Self::new(ChannelName::new(name)?))
    }

    /// Get the channel name.
    #[inline]
    pub fn name(&self) -> &ChannelName {
        &self.name
    }

    /// Get the cached canonical [`ChannelHash`] (32-bit).
    ///
    /// Used as the substrate-wide key for auth, storage, and config.
    #[inline]
    pub fn hash(&self) -> ChannelHash {
        self.hash
    }

    /// Get the wire `u16` hash for stamping the `NetHeader` fast-path.
    ///
    /// Derived from the canonical hash by truncation; the wire `u16` is a
    /// fast-path filter hint that may collide, while [`ChannelId::hash`]
    /// (canonical `u32`) is the ACL/storage key.
    #[inline]
    pub fn wire_hash(&self) -> u16 {
        self.hash as u16
    }
}

impl std::fmt::Debug for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChannelId({}, {:08x})", self.name, self.hash)
    }
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Registry of channels, tracking name-to-hash mappings.
///
/// Detects canonical-hash collisions at creation time. Forwarding nodes only
/// see the wire `u16` hash; the registry resolves wire-side ambiguity via
/// [`Self::get_by_wire_hash`].
pub struct ChannelRegistry {
    /// Canonical hash -> list of channels with that hash (rare at u32).
    by_hash: DashMap<ChannelHash, Vec<ChannelId>>,
    /// Wire `u16` hash -> list of channels with that wire bucket
    /// (routine collisions at scale; used for receive-side disambig).
    by_wire_hash: DashMap<u16, Vec<ChannelId>>,
    /// Name -> channel ID (for fast lookup by name)
    by_name: DashMap<String, ChannelId>,
}

impl ChannelRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            by_hash: DashMap::new(),
            by_wire_hash: DashMap::new(),
            by_name: DashMap::new(),
        }
    }

    /// Register a channel. Returns the ChannelId and whether a canonical-hash
    /// collision was detected with an existing channel.
    pub fn register(&self, name: &str) -> Result<(ChannelId, bool), ChannelError> {
        let id = ChannelId::parse(name)?;
        let name_key = name.to_string();

        // Hold the by_hash entry guard while inserting into by_name.
        // This ensures both maps are updated atomically from the perspective
        // of concurrent register/remove calls.
        let mut hash_entry = self.by_hash.entry(id.hash()).or_default();
        let collision = !hash_entry.is_empty();

        match self.by_name.entry(name_key) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                // Drop hash_entry guard before returning — don't leave
                // a dangling entry if the name already existed.
                return Err(ChannelError::AlreadyExists(name.to_string()));
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                hash_entry.push(id.clone());
                self.by_wire_hash
                    .entry(id.wire_hash())
                    .or_default()
                    .push(id.clone());
                vacant.insert(id.clone());
            }
        }

        Ok((id, collision))
    }

    /// Look up a channel by name.
    pub fn get(&self, name: &str) -> Option<ChannelId> {
        self.by_name.get(name).map(|r| r.clone())
    }

    /// Look up all channels with a given canonical hash (may be multiple if
    /// the rare u32 collision occurs).
    pub fn get_by_hash(&self, hash: ChannelHash) -> Vec<ChannelId> {
        self.by_hash
            .get(&hash)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Look up all channels with a given wire `u16` hash (routinely multiple
    /// due to wire-bucket collisions at scale). Used by receive-side dispatch
    /// to disambiguate the wire fast-path hint into canonical channels.
    pub fn get_by_wire_hash(&self, wire_hash: u16) -> Vec<ChannelId> {
        self.by_wire_hash
            .get(&wire_hash)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Remove a channel by name.
    ///
    /// Holds the `by_hash` entry guard while removing from `by_name` to
    /// prevent interleaved register/remove from leaving stale entries.
    pub fn remove(&self, name: &str) -> Option<ChannelId> {
        // Look up the id first to get the hash for locking
        let id = self.by_name.get(name)?.clone();

        // Hold by_hash guard while removing from both maps
        if let Some(mut hash_entry) = self.by_hash.get_mut(&id.hash()) {
            hash_entry.retain(|c| c.name().as_str() != name);
        }
        if let Some(mut wire_entry) = self.by_wire_hash.get_mut(&id.wire_hash()) {
            wire_entry.retain(|c| c.name().as_str() != name);
        }

        if self.by_name.remove(name).is_some() {
            Some(id)
        } else {
            None
        }
    }

    /// Number of registered channels.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ChannelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelRegistry")
            .field("channels", &self.by_name.len())
            .field("hash_buckets", &self.by_hash.len())
            .finish()
    }
}

/// Errors from channel operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// Channel name is empty.
    Empty,
    /// Channel name exceeds maximum length.
    TooLong(usize),
    /// Invalid character in channel name.
    InvalidChar(char),
    /// Invalid name format.
    InvalidFormat(String),
    /// Channel already exists.
    AlreadyExists(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "channel name is empty"),
            Self::TooLong(len) => write!(f, "channel name too long ({} > {})", len, MAX_NAME_LEN),
            Self::InvalidChar(ch) => write!(f, "invalid character '{}' in channel name", ch),
            Self::InvalidFormat(msg) => write!(f, "invalid channel name format: {}", msg),
            Self::AlreadyExists(name) => write!(f, "channel '{}' already exists", name),
        }
    }
}

impl std::error::Error for ChannelError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        assert!(ChannelName::new("sensors").is_ok());
        assert!(ChannelName::new("sensors/lidar").is_ok());
        assert!(ChannelName::new("sensors/lidar/front").is_ok());
        assert!(ChannelName::new("control.v2").is_ok());
        assert!(ChannelName::new("my-channel_1").is_ok());
    }

    #[test]
    fn test_invalid_names() {
        assert_eq!(ChannelName::new(""), Err(ChannelError::Empty));
        assert!(matches!(
            ChannelName::new("/leading"),
            Err(ChannelError::InvalidFormat(_))
        ));
        assert!(matches!(
            ChannelName::new("trailing/"),
            Err(ChannelError::InvalidFormat(_))
        ));
        assert!(matches!(
            ChannelName::new("double//slash"),
            Err(ChannelError::InvalidFormat(_))
        ));
        assert_eq!(
            ChannelName::new("has space"),
            Err(ChannelError::InvalidChar(' '))
        );
        assert_eq!(
            ChannelName::new("has@symbol"),
            Err(ChannelError::InvalidChar('@'))
        );
    }

    #[test]
    fn test_regression_rejects_path_traversal_segments() {
        // Regression: channel names are used as on-disk directory
        // segments in the redex-disk feature. Names like
        // `a/../../etc/target` previously passed validation (only
        // `.` and `/` chars) and would escape the base dir. Reject
        // any `..` or `.` segment at name-construction time.
        assert!(matches!(
            ChannelName::new("a/../etc"),
            Err(ChannelError::InvalidFormat(_))
        ));
        assert!(matches!(
            ChannelName::new(".."),
            Err(ChannelError::InvalidFormat(_))
        ));
        assert!(matches!(
            ChannelName::new("."),
            Err(ChannelError::InvalidFormat(_))
        ));
        assert!(matches!(
            ChannelName::new("sensors/./front"),
            Err(ChannelError::InvalidFormat(_))
        ));
        // Names with `.` inside a segment (not as a whole segment)
        // are still valid — e.g. "control.v2".
        assert!(ChannelName::new("control.v2").is_ok());
        assert!(ChannelName::new("a.b/c.d").is_ok());
    }

    #[test]
    fn test_name_too_long() {
        let long_name = "a".repeat(256);
        assert!(matches!(
            ChannelName::new(&long_name),
            Err(ChannelError::TooLong(256))
        ));

        let max_name = "a".repeat(255);
        assert!(ChannelName::new(&max_name).is_ok());
    }

    #[test]
    fn test_hash_deterministic() {
        let h1 = channel_hash("sensors/lidar");
        let h2 = channel_hash("sensors/lidar");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_differs() {
        let h1 = channel_hash("sensors/lidar");
        let h2 = channel_hash("control/estop");
        // Not guaranteed to differ for all inputs, but should for these
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_channel_id() {
        let id = ChannelId::parse("sensors/lidar").unwrap();
        assert_eq!(id.name().as_str(), "sensors/lidar");
        assert_eq!(id.hash(), channel_hash("sensors/lidar"));
    }

    #[test]
    fn test_depth() {
        assert_eq!(ChannelName::new("a").unwrap().depth(), 1);
        assert_eq!(ChannelName::new("a/b").unwrap().depth(), 2);
        assert_eq!(ChannelName::new("a/b/c/d").unwrap().depth(), 4);
    }

    #[test]
    fn test_is_prefix_of() {
        let parent = ChannelName::new("sensors").unwrap();
        let child = ChannelName::new("sensors/lidar").unwrap();
        let grandchild = ChannelName::new("sensors/lidar/front").unwrap();
        let unrelated = ChannelName::new("control/estop").unwrap();

        assert!(parent.is_prefix_of(&child));
        assert!(parent.is_prefix_of(&grandchild));
        assert!(child.is_prefix_of(&grandchild));
        assert!(!child.is_prefix_of(&parent));
        assert!(!parent.is_prefix_of(&unrelated));

        // Self is prefix of self
        assert!(parent.is_prefix_of(&parent));
    }

    #[test]
    fn test_registry_basic() {
        let reg = ChannelRegistry::new();

        let (id, collision) = reg.register("sensors/lidar").unwrap();
        assert!(!collision);
        assert_eq!(reg.len(), 1);

        let found = reg.get("sensors/lidar").unwrap();
        assert_eq!(found.hash(), id.hash());
    }

    #[test]
    fn test_registry_duplicate() {
        let reg = ChannelRegistry::new();
        reg.register("sensors/lidar").unwrap();

        assert_eq!(
            reg.register("sensors/lidar").unwrap_err(),
            ChannelError::AlreadyExists("sensors/lidar".to_string())
        );
    }

    #[test]
    fn test_registry_remove() {
        let reg = ChannelRegistry::new();
        reg.register("sensors/lidar").unwrap();
        assert_eq!(reg.len(), 1);

        let removed = reg.remove("sensors/lidar");
        assert!(removed.is_some());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("sensors/lidar").is_none());
    }

    #[test]
    fn test_registry_get_by_hash() {
        let reg = ChannelRegistry::new();
        let (id, _) = reg.register("sensors/lidar").unwrap();

        let results = reg.get_by_hash(id.hash());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name().as_str(), "sensors/lidar");
    }

    #[test]
    fn test_canonical_hash_is_u32_and_wire_is_u16() {
        // The canonical hash is u32 (4 bytes); the wire hash is u16
        // (2 bytes) and equals the low 16 bits of the canonical hash.
        let name = "sensors/lidar";
        let canonical: ChannelHash = channel_hash(name);
        let wire: u16 = wire_channel_hash(name);
        assert_eq!(canonical as u16, wire);
        // Width assertions.
        assert_eq!(std::mem::size_of::<ChannelHash>(), 4);
        assert_eq!(std::mem::size_of_val(&wire), 2);
    }

    #[test]
    fn test_registry_disambiguates_wire_hash() {
        // Two channels that may share a u16 wire bucket (high probability
        // with crafted input) must be uniquely separable by the canonical
        // u32 hash. With random inputs we can't reliably force a u16
        // collision, so this test exercises the wire-hash lookup API
        // for the non-colliding case and asserts both lookup paths agree.
        let reg = ChannelRegistry::new();
        let (id_a, _) = reg.register("sensors/lidar").unwrap();
        let (id_b, _) = reg.register("control/estop").unwrap();

        // by_wire_hash returns the right channel for each wire bucket.
        let by_wire_a = reg.get_by_wire_hash(id_a.wire_hash());
        assert!(by_wire_a
            .iter()
            .any(|c| c.name().as_str() == "sensors/lidar"));
        let by_wire_b = reg.get_by_wire_hash(id_b.wire_hash());
        assert!(by_wire_b
            .iter()
            .any(|c| c.name().as_str() == "control/estop"));

        // by_hash (canonical) returns exactly one channel per registered
        // hash — collisions at u32 require ~65 K channels to become
        // probable, so two registered channels are practically guaranteed
        // distinct.
        assert_eq!(reg.get_by_hash(id_a.hash()).len(), 1);
        assert_eq!(reg.get_by_hash(id_b.hash()).len(), 1);
    }
}
