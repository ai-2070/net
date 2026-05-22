//! `TreeNodeCache` — per-node LRU cache for v0.3 manifest tree
//! walks.
//!
//! `MeshBlobAdapter::fetch_range` walks the [`super::blob_tree::TreeNode`] tree
//! lazily for every range query. Without caching, two adjacent
//! range reads on the same blob fetch the root + every spanning
//! internal node twice. Pinning the recently-walked nodes in an
//! in-process LRU absorbs that re-fetch — typical workload
//! (sequential reads, locality-clustered random reads) sees a
//! >90% cache hit ratio after warmup.
//!
//! # Sizing
//!
//! The cache is **byte-bounded**, not entry-bounded, so the
//! memory budget is operator-set in MiB rather than tied to a
//! per-deployment node-shape distribution. Default 64 MiB ≈
//! 13 K nodes at the ~5 KiB postcard-encoded per-node size that
//! a fanout-128 leaf or internal node lands at.
//!
//! # Semantics
//!
//! - **Content-addressed**: keyed on `[u8; 32]` BLAKE3 hashes,
//!   which are immutable by construction. Cache entries never
//!   need invalidation against tampering — a hit is always
//!   integrity-correct (insert sites in
//!   `MeshBlobAdapter::walk_tree_range` re-hash bytes before
//!   inserting). Operational invalidation against chunk-store
//!   deletes lives at the caller (`delete_chunk` and `sweep_gc`
//!   call `remove`).
//! - **LRU eviction**: bytes-bounded; on insert past the cap,
//!   the least-recently-used entry evicts until the cap is
//!   satisfied. A single entry larger than the cap is rejected
//!   (caller falls back to direct fetch).
//! - **Read-through**: callers `get(&hash)` then `insert(hash,
//!   bytes)` on miss. The cache doesn't drive fetches itself.
//!
//! # Thread safety
//!
//! The struct is `!Sync`; callers wrap it in
//! `parking_lot::Mutex<TreeNodeCache>`. Per-call critical
//! sections are O(1) — `lru::LruCache` backs the entry table
//! with a doubly-linked list so promotion on hit is a constant-
//! time pointer flip.

use bytes::Bytes;
use lru::LruCache;
use std::num::NonZeroUsize;

/// Default byte cap when constructing via
/// [`TreeNodeCache::new`]. 64 MiB ≈ 13 K nodes at the typical
/// ~5 KiB postcard-encoded per-node size.
pub const DEFAULT_TREE_NODE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// In-process LRU cache of `TreeNode` postcard-encoded bytes.
/// Keyed by BLAKE3 hash; bytes-bounded eviction.
///
/// Caller wraps this in a `Mutex` for shared access. Single-
/// owner hot loops can use it unsynchronised.
pub struct TreeNodeCache {
    /// Backing LRU map. `lru::LruCache` tracks insertion + access
    /// order via an internal doubly-linked list keyed on a
    /// `usize` entry index, so promotion / eviction are O(1).
    /// We allocate it with `usize::MAX` entry capacity and gate
    /// eviction by our own byte tally instead — the entry count
    /// is incidental for a byte-bounded cache.
    entries: LruCache<[u8; 32], Bytes>,
    /// Running total of cached byte payloads. Maintained on
    /// every insert / remove so the cap check stays O(1) per
    /// insert.
    bytes: usize,
    /// Byte cap. Insert past this triggers LRU eviction.
    cap_bytes: usize,
    /// Hit counter (lifetime). Operators can graph
    /// `hits / (hits + misses)` for cache effectiveness.
    hits: u64,
    /// Miss counter (lifetime).
    misses: u64,
}

impl std::fmt::Debug for TreeNodeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeNodeCache")
            .field("len", &self.entries.len())
            .field("bytes", &self.bytes)
            .field("cap_bytes", &self.cap_bytes)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish()
    }
}

impl TreeNodeCache {
    /// Construct a cache with the default 64 MiB cap.
    pub fn new() -> Self {
        Self::with_capacity_bytes(DEFAULT_TREE_NODE_CACHE_BYTES)
    }

    /// Construct a cache with an explicit byte cap. `cap_bytes
    /// == 0` disables caching — every `get` misses, every
    /// `insert` is a no-op. Useful for ablation testing or for
    /// callers that want explicit cache disable.
    pub fn with_capacity_bytes(cap_bytes: usize) -> Self {
        Self {
            // `unbounded()` because we drive eviction by our own
            // byte accounting, not by lru's entry-count cap.
            entries: LruCache::unbounded(),
            bytes: 0,
            cap_bytes,
            hits: 0,
            misses: 0,
        }
    }

    /// Look up `hash`. On hit, returns a [`Bytes`] clone of the
    /// cached entry (`Bytes::clone` is one atomic refcount bump,
    /// not a memcpy of the node payload) and promotes the entry
    /// to most-recently-used. On miss, returns `None`.
    pub fn get(&mut self, hash: &[u8; 32]) -> Option<Bytes> {
        if self.cap_bytes == 0 {
            self.misses = self.misses.saturating_add(1);
            return None;
        }
        // `LruCache::get` is the documented promote-to-MRU
        // operation; it returns `Option<&V>` and updates the
        // internal linked list in O(1).
        match self.entries.get(hash) {
            Some(bytes) => {
                self.hits = self.hits.saturating_add(1);
                Some(bytes.clone())
            }
            None => {
                self.misses = self.misses.saturating_add(1);
                None
            }
        }
    }

    /// Insert `(hash, bytes)`. If `bytes.len() > cap_bytes`,
    /// the insert is rejected silently (a single oversize
    /// entry can't dominate the cache). Otherwise evicts LRU
    /// entries until the cap is satisfied, then inserts.
    /// Re-inserting an existing hash refreshes the access time
    /// without double-counting bytes.
    ///
    /// **Crate-private.** Cache integrity (a hit returns bytes
    /// that hash to the key) depends on every insert site having
    /// already verified `blake3(&bytes) == hash`. The miss path in
    /// `walk_tree_range` does this re-verification (see
    /// `MeshBlobAdapter::walk_tree_range`); restricting `insert`
    /// to `pub(crate)` ensures no external caller can poison the
    /// cache with a (hash, mismatched_bytes) pair. If a future
    /// commit needs to expose insert publicly, it MUST first hash-
    /// validate the bytes argument internally.
    pub(crate) fn insert(&mut self, hash: [u8; 32], bytes: Bytes) {
        if self.cap_bytes == 0 || bytes.len() > self.cap_bytes {
            return;
        }
        // Re-insert: drop the old entry's accounting first. lru's
        // `put` returns the displaced value, so we adjust the
        // byte tally in one step.
        let new_len = bytes.len();
        if let Some(prev) = self.entries.put(hash, bytes) {
            self.bytes = self.bytes.saturating_sub(prev.len());
        }
        self.bytes = self.bytes.saturating_add(new_len);
        // Evict LRU entries (front of the linked list) until the
        // running byte total fits the cap. `pop_lru` is O(1).
        while self.bytes > self.cap_bytes {
            match self.entries.pop_lru() {
                Some((_, evicted)) => {
                    self.bytes = self.bytes.saturating_sub(evicted.len());
                }
                None => break, // defensive: cap_bytes >= bytes.len() at entry
            }
        }
    }

    /// Number of entries currently cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff no entries are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current total byte payload across all cached entries.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Configured byte cap.
    pub fn cap_bytes(&self) -> usize {
        self.cap_bytes
    }

    /// Lifetime hit counter.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Lifetime miss counter.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Hit ratio in `[0.0, 1.0]`, or `0.0` when no accesses
    /// have happened. Useful for operator dashboards.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits.saturating_add(self.misses);
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Drop every entry. Resets hit / miss counters.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.hits = 0;
        self.misses = 0;
    }

    /// Drop a single entry by hash. Used by the chunk-delete /
    /// GC sweep paths to keep the cache in sync with the on-disk
    /// chunk store: a manifest node whose chunk file just went
    /// away must not survive in cache, otherwise subsequent
    /// fetch_range walks descend through the cached node and
    /// only discover the missing chunks at the leaf, confusing
    /// operator error attribution. Cache integrity (bytes hash
    /// to key) is preserved either way — this fix is for error-
    /// path clarity, not for soundness.
    ///
    /// No-op if the hash isn't cached.
    pub fn remove(&mut self, hash: &[u8; 32]) {
        if let Some(bytes) = self.entries.pop(hash) {
            self.bytes = self.bytes.saturating_sub(bytes.len());
        }
    }
}

impl Default for TreeNodeCache {
    fn default() -> Self {
        Self::new()
    }
}

// `LruCache::unbounded()` is the entry-count-unbounded constructor;
// importing `NonZeroUsize` is unused now but kept for future
// callers who want to construct an entry-count-bounded cache for
// tests.
#[allow(dead_code)]
fn _force_import_nonzero(_: NonZeroUsize) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn empty_cache_misses() {
        let mut c = TreeNodeCache::new();
        assert!(c.get(&h(1)).is_none());
        assert_eq!(c.hits(), 0);
        assert_eq!(c.misses(), 1);
    }

    #[test]
    fn insert_and_get_round_trip() {
        let mut c = TreeNodeCache::with_capacity_bytes(1024);
        c.insert(h(1), Bytes::from(vec![1, 2, 3]));
        let got = c.get(&h(1)).unwrap();
        assert_eq!(got.as_ref(), &[1u8, 2, 3]);
        assert_eq!(c.hits(), 1);
        assert_eq!(c.misses(), 0);
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 3);
    }

    #[test]
    fn cap_zero_disables_cache() {
        let mut c = TreeNodeCache::with_capacity_bytes(0);
        c.insert(h(1), Bytes::from(vec![1, 2, 3]));
        assert!(c.get(&h(1)).is_none());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn oversize_single_entry_rejected() {
        let mut c = TreeNodeCache::with_capacity_bytes(2);
        c.insert(h(1), Bytes::from(vec![1, 2, 3])); // 3 > cap=2 → rejected
        assert!(c.get(&h(1)).is_none());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let mut c = TreeNodeCache::with_capacity_bytes(10);
        c.insert(h(1), Bytes::from(vec![0; 4]));
        c.insert(h(2), Bytes::from(vec![0; 4]));
        // Touch h(1) so it moves to MRU.
        let _ = c.get(&h(1));
        // Insert pushes us over the cap (4+4+4=12 > 10);
        // h(2) is LRU and should evict.
        c.insert(h(3), Bytes::from(vec![0; 4]));
        assert!(c.get(&h(1)).is_some(), "h(1) was touched, stays cached");
        assert!(c.get(&h(2)).is_none(), "h(2) was LRU, evicted");
        assert!(c.get(&h(3)).is_some());
    }

    #[test]
    fn re_insert_replaces_value_without_double_counting() {
        let mut c = TreeNodeCache::with_capacity_bytes(100);
        c.insert(h(1), Bytes::from(vec![0; 10]));
        c.insert(h(1), Bytes::from(vec![0; 20]));
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 20);
    }

    #[test]
    fn clear_resets_state() {
        let mut c = TreeNodeCache::with_capacity_bytes(100);
        c.insert(h(1), Bytes::from(vec![0; 10]));
        let _ = c.get(&h(1));
        c.clear();
        assert_eq!(c.len(), 0);
        assert_eq!(c.bytes(), 0);
        assert_eq!(c.hits(), 0);
        assert_eq!(c.misses(), 0);
    }

    #[test]
    fn remove_drops_one_entry_only() {
        let mut c = TreeNodeCache::with_capacity_bytes(100);
        c.insert(h(1), Bytes::from(vec![0; 10]));
        c.insert(h(2), Bytes::from(vec![0; 20]));
        c.remove(&h(1));
        assert!(c.get(&h(1)).is_none());
        assert!(c.get(&h(2)).is_some());
        assert_eq!(c.bytes(), 20);
    }

    #[test]
    fn remove_unknown_hash_is_noop() {
        let mut c = TreeNodeCache::with_capacity_bytes(100);
        c.insert(h(1), Bytes::from(vec![0; 10]));
        c.remove(&h(99));
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 10);
    }

    #[test]
    fn hit_ratio_reflects_ratio() {
        let mut c = TreeNodeCache::with_capacity_bytes(100);
        c.insert(h(1), Bytes::from(vec![0; 5]));
        let _ = c.get(&h(1)); // hit
        let _ = c.get(&h(2)); // miss
        let _ = c.get(&h(1)); // hit
        assert!((c.hit_ratio() - (2.0 / 3.0)).abs() < 1e-9);
    }
}
