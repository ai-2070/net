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
//!   need invalidation — a hit is always correct.
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
//! sections are short (one HashMap probe + one VecDeque touch
//! + arithmetic).

use std::collections::HashMap;
use std::collections::VecDeque;

/// Default byte cap when constructing via
/// [`TreeNodeCache::new`]. 64 MiB ≈ 13 K nodes at the typical
/// ~5 KiB postcard-encoded per-node size.
pub const DEFAULT_TREE_NODE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// In-process LRU cache of `TreeNode` postcard-encoded bytes.
/// Keyed by BLAKE3 hash; bytes-bounded eviction.
///
/// Caller wraps this in a `Mutex` for shared access. Single-
/// owner hot loops can use it unsynchronised.
#[derive(Debug)]
pub struct TreeNodeCache {
    entries: HashMap<[u8; 32], Vec<u8>>,
    /// Access order, most-recently-used at the back. On every
    /// hit, the key moves to the back. Eviction pops from the
    /// front until total bytes is under the cap.
    order: VecDeque<[u8; 32]>,
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
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            cap_bytes,
            hits: 0,
            misses: 0,
        }
    }

    /// Look up `hash`. On hit, returns a clone of the cached
    /// bytes (the caller decodes them as a [`super::blob_tree::TreeNode`]) and
    /// promotes the entry to most-recently-used. On miss,
    /// returns `None`.
    pub fn get(&mut self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        if self.cap_bytes == 0 {
            self.misses = self.misses.saturating_add(1);
            return None;
        }
        if let Some(bytes) = self.entries.get(hash) {
            self.hits = self.hits.saturating_add(1);
            let cloned = bytes.clone();
            // Promote to MRU. O(N) over `order` but bounded by
            // entry count ≈ cap_bytes / avg_node_bytes ≈ 13 K
            // at defaults — comfortable for a per-fetch call.
            if let Some(pos) = self.order.iter().position(|k| k == hash) {
                if let Some(k) = self.order.remove(pos) {
                    self.order.push_back(k);
                }
            }
            Some(cloned)
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    /// Insert `(hash, bytes)`. If `bytes.len() > cap_bytes`,
    /// the insert is rejected silently (a single oversize
    /// entry can't dominate the cache). Otherwise evicts LRU
    /// entries until the cap is satisfied, then inserts.
    /// Re-inserting an existing hash refreshes the access time
    /// without double-counting bytes.
    pub fn insert(&mut self, hash: [u8; 32], bytes: Vec<u8>) {
        if self.cap_bytes == 0 || bytes.len() > self.cap_bytes {
            return;
        }
        // Re-insert: drop the old entry's accounting first.
        if let Some(old) = self.entries.remove(&hash) {
            self.bytes = self.bytes.saturating_sub(old.len());
            if let Some(pos) = self.order.iter().position(|k| k == &hash) {
                self.order.remove(pos);
            }
        }
        // Evict LRU entries until we have room.
        while self.bytes.saturating_add(bytes.len()) > self.cap_bytes {
            let Some(victim) = self.order.pop_front() else {
                break; // shouldn't happen — cap_bytes >= bytes.len() but defensive.
            };
            if let Some(evicted) = self.entries.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(evicted.len());
            }
        }
        // Insert + promote.
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.entries.insert(hash, bytes);
        self.order.push_back(hash);
    }

    /// Number of entries currently cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff no entries are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total bytes currently cached.
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
        self.order.clear();
        self.bytes = 0;
        self.hits = 0;
        self.misses = 0;
    }
}

impl Default for TreeNodeCache {
    fn default() -> Self {
        Self::new()
    }
}

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
        assert_eq!(c.misses(), 1);
        assert_eq!(c.hits(), 0);
        assert_eq!(c.hit_ratio(), 0.0);
    }

    #[test]
    fn insert_then_get_hits() {
        let mut c = TreeNodeCache::new();
        let bytes = vec![0xAA; 1024];
        c.insert(h(1), bytes.clone());
        assert_eq!(c.get(&h(1)).as_deref(), Some(bytes.as_slice()));
        assert_eq!(c.hits(), 1);
        assert_eq!(c.misses(), 0);
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 1024);
    }

    #[test]
    fn insert_evicts_lru_to_satisfy_cap() {
        let mut c = TreeNodeCache::with_capacity_bytes(4096);
        // Three 2-KiB entries — at the third insert the first
        // evicts to satisfy the 4-KiB cap.
        c.insert(h(1), vec![0xA1; 2048]);
        c.insert(h(2), vec![0xA2; 2048]);
        assert_eq!(c.bytes(), 4096);
        c.insert(h(3), vec![0xA3; 2048]);
        // h(1) was LRU → evicted; h(2) and h(3) remain.
        assert!(c.get(&h(1)).is_none());
        assert!(c.get(&h(2)).is_some());
        assert!(c.get(&h(3)).is_some());
        assert!(c.bytes() <= 4096);
    }

    #[test]
    fn get_promotes_to_mru() {
        let mut c = TreeNodeCache::with_capacity_bytes(4096);
        c.insert(h(1), vec![0xA1; 2048]);
        c.insert(h(2), vec![0xA2; 2048]);
        // Access h(1) — promotes it to MRU.
        let _ = c.get(&h(1));
        // Insert h(3) — h(2) is now LRU → evicts.
        c.insert(h(3), vec![0xA3; 2048]);
        assert!(c.get(&h(1)).is_some(), "promoted entry survives");
        assert!(c.get(&h(2)).is_none(), "LRU after promotion evicts");
        assert!(c.get(&h(3)).is_some());
    }

    #[test]
    fn reinsert_refreshes_access_without_double_counting_bytes() {
        let mut c = TreeNodeCache::with_capacity_bytes(4096);
        c.insert(h(1), vec![0xA1; 2048]);
        c.insert(h(1), vec![0xB1; 2048]); // re-insert same key
        assert_eq!(c.bytes(), 2048, "re-insert must not double-count bytes");
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(&h(1)).unwrap(), vec![0xB1; 2048]);
    }

    #[test]
    fn oversize_entry_is_silently_rejected() {
        let mut c = TreeNodeCache::with_capacity_bytes(4096);
        // 8 KiB entry vs 4 KiB cap.
        c.insert(h(1), vec![0xAA; 8192]);
        assert!(c.is_empty(), "oversize entry must not be admitted");
        assert!(c.get(&h(1)).is_none());
    }

    #[test]
    fn zero_cap_disables_caching() {
        let mut c = TreeNodeCache::with_capacity_bytes(0);
        c.insert(h(1), vec![0xAA; 1024]);
        assert!(c.is_empty());
        assert!(c.get(&h(1)).is_none());
        // Misses still tracked for observability.
        assert_eq!(c.misses(), 1);
    }

    #[test]
    fn hit_ratio_reports_correctly() {
        let mut c = TreeNodeCache::new();
        c.insert(h(1), vec![0xAA; 128]);
        let _ = c.get(&h(1)); // hit
        let _ = c.get(&h(1)); // hit
        let _ = c.get(&h(2)); // miss
        let _ = c.get(&h(3)); // miss
        assert_eq!(c.hits(), 2);
        assert_eq!(c.misses(), 2);
        assert!((c.hit_ratio() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn clear_resets_everything() {
        let mut c = TreeNodeCache::new();
        c.insert(h(1), vec![0xAA; 128]);
        let _ = c.get(&h(1));
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.bytes(), 0);
        assert_eq!(c.hits(), 0);
        assert_eq!(c.misses(), 0);
    }

    #[test]
    fn many_inserts_stays_within_cap() {
        let mut c = TreeNodeCache::with_capacity_bytes(64 * 1024);
        // 200 × 1 KiB inserts; cap is 64 KiB, so at most 64
        // entries survive at any moment.
        for i in 0..200u8 {
            c.insert(h(i), vec![i; 1024]);
            assert!(
                c.bytes() <= 64 * 1024,
                "byte total {} exceeded cap after insert {}",
                c.bytes(),
                i
            );
        }
        assert!(c.len() <= 64);
    }
}
