//! Greedy-LRU cache registry — per-channel state + cluster-wide
//! LRU index. Pure data structure; the runtime owns I/O.
//!
//! Layout:
//!
//! ```text
//! entries: HashMap<ChannelName, GreedyCacheEntry { file, last_read, bytes, lru_pos }>
//! lru:     BTreeMap<u64, ChannelName>   // leftmost == oldest (smallest lru_pos)
//! ```
//!
//! Two indexes so the runtime can: (a) look up by channel name in
//! O(1), (b) find the LRU victim in O(log n), (c) move-to-front
//! on read hits in O(log n).
//!
//! The LRU is keyed on a monotonically-increasing counter rather
//! than `(Instant, ChannelName)` because `ChannelName` doesn't
//! impl `Ord` — and the counter has the side benefit that "touch
//! twice in the same Instant" still produces a strict ordering
//! (newer touch sorts last).
//!
//! Byte accounting is caller-driven — the runtime calls
//! [`Self::note_appended`] with the payload's byte count after a
//! successful `RedexFile::append`. Per-channel retention is
//! enforced inside the `RedexFile` (`with_retention_max_bytes`);
//! the registry's `bytes` count is an upper bound that may drift
//! above actual disk usage after retention trim. That's fine for
//! the cluster-eviction decision — operator-visible behavior is
//! "evict slightly sooner than strictly necessary," never the
//! other direction.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::redex::RedexFile;

/// One per-channel cache entry.
pub struct GreedyCacheEntry {
    /// The cache file. Heap-only by default; persistent opt-in via
    /// the runtime's config. Caller constructed via
    /// `Redex::open_file(channel, RedexFileConfig::default()
    ///     .with_retention_max_bytes(per_channel_cap_bytes))`.
    pub file: RedexFile,
    /// Last time the cache was read by an upstream consumer.
    /// Updated by [`GreedyCacheRegistry::touch`] on every cache
    /// hit; LRU ordering itself is driven by `lru_pos`.
    pub last_read: Instant,
    /// Bytes appended to this cache file since registration.
    /// Upper bound on retained bytes (retention may evict).
    pub bytes: u64,
    /// Most-recently-observed `origin_hash` for the chain this
    /// cache entry holds. Set on first cache-write and refreshed
    /// on each subsequent observation. Used by the data-gravity
    /// layer to key heat counters per origin (`heat:<hex>=<rate>`
    /// matches the chain's `causal:<hex>` advertisement). Zero
    /// if no event has landed yet (cache file opened but no
    /// observation).
    pub origin_hash: u64,
    /// Monotonic LRU position. Higher = more recent. The
    /// registry's `lru` BTreeMap keys on this so two channels
    /// touched in the same `Instant` still order deterministically.
    lru_pos: u64,
}

impl std::fmt::Debug for GreedyCacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GreedyCacheEntry")
            .field("bytes", &self.bytes)
            .field("last_read", &self.last_read)
            .field("lru_pos", &self.lru_pos)
            .finish_non_exhaustive()
    }
}

/// One evicted entry. Carries the `origin_hash` alongside the
/// channel name so the runtime can issue
/// `ChainTagSink::withdraw_chain` without a follow-up lookup.
/// `origin_hash == 0` means "no event landed in the cache before
/// eviction" — runtime skips the withdraw in that case (there is
/// nothing announced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvictedEntry {
    /// Channel name whose cache entry was removed.
    pub channel: ChannelName,
    /// `origin_hash` recorded on the cache entry at eviction time;
    /// `0` if no event landed before eviction.
    pub origin_hash: u64,
}

/// Outcome of an evict-to-fit pass. Carries enough info for the
/// caller to withdraw the chain announcement that registered each
/// cache entry.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EvictionSweep {
    /// Channels removed from the cache, in eviction order
    /// (oldest first).
    pub evicted: Vec<EvictedEntry>,
}

impl EvictionSweep {
    /// True iff no channel was evicted.
    pub fn is_empty(&self) -> bool {
        self.evicted.is_empty()
    }

    /// Number of channels evicted this sweep.
    pub fn len(&self) -> usize {
        self.evicted.len()
    }

    /// Iterate evicted channel names.
    pub fn channels(&self) -> impl Iterator<Item = &ChannelName> + '_ {
        self.evicted.iter().map(|e| &e.channel)
    }
}

/// Cluster-wide cache registry. Holds every channel's
/// [`GreedyCacheEntry`] + an LRU index keyed on a monotonic
/// counter for deterministic ordering.
#[derive(Debug)]
pub struct GreedyCacheRegistry {
    entries: HashMap<ChannelName, GreedyCacheEntry>,
    /// LRU index. Leftmost = smallest `lru_pos` = least-recently
    /// touched channel.
    lru: BTreeMap<u64, ChannelName>,
    /// Next LRU position to assign. Monotonic across upserts +
    /// touches; saturating to `u64::MAX` would mean ~`u64::MAX`
    /// touches, which is unreachable in any realistic deployment.
    next_lru_pos: u64,
    total_bytes: u64,
    total_cap_bytes: u64,
}

impl GreedyCacheRegistry {
    /// Build an empty registry with the cluster-wide cap.
    pub fn new(total_cap_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            lru: BTreeMap::new(),
            next_lru_pos: 0,
            total_bytes: 0,
            total_cap_bytes,
        }
    }

    /// Number of cached channels.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True iff zero channels are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total bytes appended across every cached channel. Upper
    /// bound on disk usage (retention may evict).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Resync every cached entry's byte count against the
    /// authoritative `RedexFile::retained_bytes` reading. The
    /// registry's per-entry `bytes` counter is monotonic — `append`
    /// adds, eviction subtracts, but retention trim inside the
    /// `RedexFile` doesn't propagate back. Long-running hot,
    /// retention-trimmed channels see `entry.bytes` drift
    /// arbitrarily above what's actually on disk; eventually the
    /// cluster-cap budget reads "full" while disk reads near-empty
    /// and every admission false-rejects. Periodic resync re-anchors
    /// `entry.bytes` (and `total_bytes`) on the substrate's view.
    ///
    /// O(n) over cached channels; intended for a periodic background
    /// task (e.g. heartbeat-aligned) not the per-event hot path.
    pub fn resync_bytes_from_files(&mut self) {
        let mut new_total: u64 = 0;
        for entry in self.entries.values_mut() {
            let on_disk = entry.file.retained_bytes();
            entry.bytes = on_disk;
            new_total = new_total.saturating_add(on_disk);
        }
        self.total_bytes = new_total;
    }

    /// Cluster-wide cap.
    pub fn total_cap_bytes(&self) -> u64 {
        self.total_cap_bytes
    }

    /// True iff `channel` has a cache entry.
    pub fn contains(&self, channel: &ChannelName) -> bool {
        self.entries.contains_key(channel)
    }

    /// Borrow a channel's cache entry.
    pub fn get(&self, channel: &ChannelName) -> Option<&GreedyCacheEntry> {
        self.entries.get(channel)
    }

    /// True iff any cached entry records `origin_hash` as its
    /// chain identity. Used by the colocation gate to resolve
    /// `metadata.colocate-with[-strict]` hints against locally-held
    /// chains. O(n) over cached channels — colocation hints are
    /// expected to be sparse, but for very large caches a future
    /// slice may want a reverse index.
    pub fn contains_origin(&self, origin_hash: u64) -> bool {
        if origin_hash == 0 {
            return false;
        }
        self.entries
            .values()
            .any(|e| e.origin_hash == origin_hash)
    }

    /// Iterate over cached channel names.
    pub fn channels(&self) -> impl Iterator<Item = &ChannelName> + '_ {
        self.entries.keys()
    }

    fn allocate_lru_pos(&mut self) -> u64 {
        let pos = self.next_lru_pos;
        self.next_lru_pos = self.next_lru_pos.saturating_add(1);
        pos
    }

    /// Register a new channel's cache file. If the channel is
    /// already registered, replace the file reference and refresh
    /// the LRU position (idempotent registration on reopen). The
    /// previous entry's `bytes` are subtracted from `total_bytes`
    /// and the new entry's count starts at zero — otherwise reopens
    /// would accumulate phantom cluster usage that no `evict` could
    /// ever drain.
    pub fn upsert(&mut self, channel: ChannelName, file: RedexFile, now: Instant) {
        let new_pos = self.allocate_lru_pos();
        if let Some(prev) = self.entries.get_mut(&channel) {
            let old_pos = prev.lru_pos;
            // Subtract the previous entry's bytes from total_bytes.
            // The new file's append count starts at zero — what's
            // on disk is governed by RedexFile retention, not by
            // the registry's accounting.
            self.total_bytes = self.total_bytes.saturating_sub(prev.bytes);
            prev.bytes = 0;
            prev.file = file;
            prev.last_read = now;
            prev.lru_pos = new_pos;
            self.lru.remove(&old_pos);
            self.lru.insert(new_pos, channel);
            return;
        }
        self.lru.insert(new_pos, channel.clone());
        self.entries.insert(
            channel,
            GreedyCacheEntry {
                file,
                last_read: now,
                bytes: 0,
                origin_hash: 0,
                lru_pos: new_pos,
            },
        );
    }

    /// Record the `origin_hash` for `channel`. Used by the
    /// data-gravity layer to map cache entries back to the chain
    /// identifier carried in `heat:<hex>=<rate>` wire tags.
    /// No-op if the channel isn't registered.
    pub fn set_origin_hash(&mut self, channel: &ChannelName, origin_hash: u64) {
        if let Some(entry) = self.entries.get_mut(channel) {
            entry.origin_hash = origin_hash;
        }
    }

    /// Bump `last_read` for `channel` to `now`. No-op if the
    /// channel isn't registered. Moves the channel to the head of
    /// the LRU queue (front == newest).
    pub fn touch(&mut self, channel: &ChannelName, now: Instant) {
        if !self.entries.contains_key(channel) {
            return;
        }
        let new_pos = self.allocate_lru_pos();
        let entry = self
            .entries
            .get_mut(channel)
            .expect("just checked contains_key");
        let old_pos = entry.lru_pos;
        entry.last_read = now;
        entry.lru_pos = new_pos;
        self.lru.remove(&old_pos);
        self.lru.insert(new_pos, channel.clone());
    }

    /// Account `payload_bytes` newly appended to `channel`'s cache.
    /// Updates the running total and runs cluster-cap enforcement.
    /// Returns the channels evicted to keep
    /// `total_bytes <= total_cap_bytes`.
    ///
    /// Writes do NOT promote the channel's LRU position — the
    /// cache exists to make future *reads* cheap, so LRU ordering
    /// is keyed on read recency. A channel actively being
    /// written-to-but-never-read evicts before one that's quietly
    /// being read; that matches the consumer-focused framing in
    /// `DATAFORTS_PLAN.md` § Phase 1. Use [`Self::touch`] from the
    /// read path to refresh recency.
    ///
    /// If `channel` isn't registered, the call is a no-op and
    /// returns an empty [`EvictionSweep`].
    pub fn note_appended(
        &mut self,
        channel: &ChannelName,
        payload_bytes: u64,
        _now: Instant,
    ) -> EvictionSweep {
        let Some(entry) = self.entries.get_mut(channel) else {
            return EvictionSweep::default();
        };
        entry.bytes = entry.bytes.saturating_add(payload_bytes);
        self.total_bytes = self.total_bytes.saturating_add(payload_bytes);

        self.evict_until_under_cap()
    }

    /// Evict a channel by name. Returns the removed entry so the
    /// caller can withdraw the chain announcement.
    pub fn evict(&mut self, channel: &ChannelName) -> Option<GreedyCacheEntry> {
        let entry = self.entries.remove(channel)?;
        self.lru.remove(&entry.lru_pos);
        self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
        Some(entry)
    }

    /// Update the cluster-wide cap. Runs eviction if the new cap
    /// is smaller than current usage. Returns the eviction sweep.
    pub fn set_total_cap_bytes(&mut self, new_cap: u64) -> EvictionSweep {
        self.total_cap_bytes = new_cap;
        self.evict_until_under_cap()
    }

    /// Evict the LRU entry. Returns the removed channel + entry.
    fn evict_oldest(&mut self) -> Option<(ChannelName, GreedyCacheEntry)> {
        let (_, channel) = self.lru.iter().next()?;
        let channel = channel.clone();
        let entry = self.evict(&channel)?;
        Some((channel, entry))
    }

    /// Evict oldest entries until `total_bytes <= total_cap_bytes`.
    fn evict_until_under_cap(&mut self) -> EvictionSweep {
        let mut evicted = Vec::new();
        while self.total_bytes > self.total_cap_bytes {
            let Some((channel, entry)) = self.evict_oldest() else {
                break;
            };
            evicted.push(EvictedEntry {
                channel,
                origin_hash: entry.origin_hash,
            });
        }
        EvictionSweep { evicted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::redex::{Redex, RedexFileConfig};
    use std::time::Duration;

    fn cn(s: &str) -> ChannelName {
        ChannelName::new(s).unwrap()
    }

    fn open_file(redex: &Redex, name: &str, cap_bytes: u64) -> RedexFile {
        redex
            .open_file(
                &cn(name),
                RedexFileConfig::default().with_retention_max_bytes(cap_bytes),
            )
            .expect("open cache file")
    }

    #[test]
    fn new_registry_is_empty() {
        let r = GreedyCacheRegistry::new(1024);
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
        assert_eq!(r.total_bytes(), 0);
        assert_eq!(r.total_cap_bytes(), 1024);
    }

    #[test]
    fn upsert_registers_channel() {
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now = Instant::now();
        let file = open_file(&redex, "test/a", 10_000);
        reg.upsert(cn("test/a"), file, now);
        assert_eq!(reg.len(), 1);
        assert!(reg.contains(&cn("test/a")));
        let entry = reg.get(&cn("test/a")).unwrap();
        assert_eq!(entry.last_read, now);
        assert_eq!(entry.bytes, 0);
    }

    #[test]
    fn upsert_is_idempotent_on_reopen() {
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now1 = Instant::now();
        reg.upsert(cn("test/a"), open_file(&redex, "test/a", 10_000), now1);
        let now2 = now1 + Duration::from_secs(1);
        reg.upsert(cn("test/a"), open_file(&redex, "test/a", 10_000), now2);
        assert_eq!(reg.len(), 1, "reopen must not duplicate the entry");
        assert_eq!(reg.get(&cn("test/a")).unwrap().last_read, now2);
    }

    #[test]
    fn touch_updates_lru_position() {
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let base = Instant::now();
        // A is oldest, B is middle, C is newest.
        reg.upsert(cn("test/a"), open_file(&redex, "test/a", 10_000), base);
        reg.upsert(
            cn("test/b"),
            open_file(&redex, "test/b", 10_000),
            base + Duration::from_secs(1),
        );
        reg.upsert(
            cn("test/c"),
            open_file(&redex, "test/c", 10_000),
            base + Duration::from_secs(2),
        );
        // Touch A — it should now be newest.
        reg.touch(&cn("test/a"), base + Duration::from_secs(3));
        // The LRU's first entry should now be B (oldest).
        let oldest = reg.lru.values().next().unwrap();
        assert_eq!(*oldest, cn("test/b"));
    }

    #[test]
    fn note_appended_tracks_bytes() {
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now = Instant::now();
        reg.upsert(cn("test/a"), open_file(&redex, "test/a", 10_000), now);
        let sweep = reg.note_appended(&cn("test/a"), 500, now);
        assert!(sweep.is_empty());
        assert_eq!(reg.total_bytes(), 500);
        assert_eq!(reg.get(&cn("test/a")).unwrap().bytes, 500);
    }

    #[test]
    fn note_appended_on_missing_channel_is_noop() {
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now = Instant::now();
        let sweep = reg.note_appended(&cn("missing"), 1024, now);
        assert!(sweep.is_empty());
        assert_eq!(reg.total_bytes(), 0);
    }

    #[test]
    fn cluster_cap_triggers_lru_eviction() {
        let redex = Redex::new();
        // Cluster cap = 1 KiB; per-channel cap larger so retention
        // doesn't kick in — we want the cluster eviction path.
        let mut reg = GreedyCacheRegistry::new(1024);
        let base = Instant::now();
        reg.upsert(cn("a"), open_file(&redex, "a", 10_000), base);
        reg.upsert(
            cn("b"),
            open_file(&redex, "b", 10_000),
            base + Duration::from_secs(1),
        );
        reg.upsert(
            cn("c"),
            open_file(&redex, "c", 10_000),
            base + Duration::from_secs(2),
        );

        // Fill A to 600 bytes, B to 600 → total 1200 > 1024 cap.
        let sweep_a = reg.note_appended(&cn("a"), 600, base + Duration::from_secs(3));
        assert!(sweep_a.is_empty());
        let sweep_b = reg.note_appended(&cn("b"), 600, base + Duration::from_secs(4));
        // After B's append, A is oldest (LRU); evict A.
        let names: Vec<_> = sweep_b.channels().cloned().collect();
        assert_eq!(names, vec![cn("a")]);
        assert!(!reg.contains(&cn("a")));
        assert_eq!(reg.total_bytes(), 600);
    }

    #[test]
    fn explicit_evict_drops_entry_and_bytes() {
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now = Instant::now();
        reg.upsert(cn("a"), open_file(&redex, "a", 10_000), now);
        reg.note_appended(&cn("a"), 5_000, now);
        let entry = reg.evict(&cn("a")).expect("evict returns entry");
        assert_eq!(entry.bytes, 5_000);
        assert!(!reg.contains(&cn("a")));
        assert_eq!(reg.total_bytes(), 0);
    }

    #[test]
    fn shrinking_cap_runs_eviction_immediately() {
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(10_000);
        let base = Instant::now();
        reg.upsert(cn("a"), open_file(&redex, "a", 10_000), base);
        reg.upsert(
            cn("b"),
            open_file(&redex, "b", 10_000),
            base + Duration::from_secs(1),
        );
        reg.note_appended(&cn("a"), 4_000, base + Duration::from_secs(2));
        reg.note_appended(&cn("b"), 4_000, base + Duration::from_secs(3));
        assert_eq!(reg.total_bytes(), 8_000);

        // Shrink cap to 3000 — A is oldest (older LRU pos than B), evict first.
        let sweep = reg.set_total_cap_bytes(3000);
        // A's 4000-byte share evicts first. After that total_bytes
        // drops to 4000 which is still > 3000, so B evicts too.
        let names: Vec<_> = sweep.channels().cloned().collect();
        assert_eq!(names, vec![cn("a"), cn("b")]);
        assert!(reg.is_empty());
    }

    #[test]
    fn touch_on_read_promotes_channel_past_silent_peers() {
        // Writes don't promote LRU position; reads do. Pin that a
        // touched channel survives eviction pressure that a silent
        // peer would have absorbed.
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1024);
        let base = Instant::now();
        // A is upserted first (oldest), B second.
        reg.upsert(cn("a"), open_file(&redex, "a", 10_000), base);
        reg.upsert(
            cn("b"),
            open_file(&redex, "b", 10_000),
            base + Duration::from_secs(1),
        );
        // Both grow to half the cap.
        reg.note_appended(&cn("a"), 500, base + Duration::from_secs(2));
        reg.note_appended(&cn("b"), 400, base + Duration::from_secs(3));
        // Read A — touch promotes it past B.
        reg.touch(&cn("a"), base + Duration::from_secs(4));
        // Push the cluster over cap. B is now the oldest; B evicts.
        let sweep = reg.note_appended(&cn("b"), 200, base + Duration::from_secs(5));
        let names: Vec<_> = sweep.channels().cloned().collect();
        assert_eq!(names, vec![cn("b")]);
        assert!(reg.contains(&cn("a")));
    }

    #[test]
    fn eviction_sweep_carries_origin_hash_for_withdraw() {
        // Cluster cap forces eviction; the sweep must surface the
        // evicted entries' origin_hash values so the runtime can
        // issue `withdraw_chain` without a follow-up lookup.
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1024);
        let base = Instant::now();
        reg.upsert(cn("a"), open_file(&redex, "a", 10_000), base);
        reg.upsert(
            cn("b"),
            open_file(&redex, "b", 10_000),
            base + Duration::from_secs(1),
        );
        reg.set_origin_hash(&cn("a"), 0xAAAA_AAAA_AAAA_AAAA);
        reg.set_origin_hash(&cn("b"), 0xBBBB_BBBB_BBBB_BBBB);

        reg.note_appended(&cn("a"), 600, base + Duration::from_secs(2));
        let sweep = reg.note_appended(&cn("b"), 600, base + Duration::from_secs(3));
        assert_eq!(sweep.len(), 1, "A should evict");
        let evicted = &sweep.evicted[0];
        assert_eq!(evicted.channel, cn("a"));
        assert_eq!(evicted.origin_hash, 0xAAAA_AAAA_AAAA_AAAA);
    }

    #[test]
    fn resync_bytes_from_files_anchors_total_on_substrate_view() {
        // Registry tracks monotonic appends; RedexFile retention is
        // separate and runs via `sweep_retention()`. After enough
        // appends + a sweep, on-disk bytes < registry's count.
        // Resync re-anchors on the substrate's authoritative view.
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now = Instant::now();
        let per_channel_cap = 2048u64;
        reg.upsert(cn("test/a"), open_file(&redex, "test/a", per_channel_cap), now);

        // Drive registry past the per-channel cap.
        for _ in 0..20 {
            let payload = vec![0u8; 1024];
            let file = reg.get(&cn("test/a")).unwrap().file.clone();
            file.append(&payload).unwrap();
            reg.note_appended(&cn("test/a"), payload.len() as u64, now);
        }
        let pre_resync_bytes = reg.total_bytes();
        assert!(
            pre_resync_bytes >= 20 * 1024,
            "registry must have accumulated monotonic bytes; got {}",
            pre_resync_bytes,
        );

        // Trigger substrate-side retention trim.
        let file = reg.get(&cn("test/a")).unwrap().file.clone();
        file.sweep_retention();

        // Resync — total drops to actual on-disk usage.
        reg.resync_bytes_from_files();
        let post_resync_bytes = reg.total_bytes();
        assert!(
            post_resync_bytes < pre_resync_bytes,
            "resync must reduce drift (pre {} > post {})",
            pre_resync_bytes,
            post_resync_bytes,
        );
        // Entry's byte count was clamped too.
        assert_eq!(reg.get(&cn("test/a")).unwrap().bytes, post_resync_bytes);
    }

    #[test]
    fn upsert_on_reopen_subtracts_old_bytes_from_total() {
        // Without subtraction on reopen, total_bytes accumulates
        // phantom usage that no evict path can drain. Pin the fix:
        // a reopen of the same channel zeroes the entry's byte
        // count and removes its prior contribution from the
        // cluster total.
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1_000_000);
        let now = Instant::now();
        reg.upsert(cn("test/a"), open_file(&redex, "test/a", 10_000), now);
        reg.note_appended(&cn("test/a"), 500, now);
        assert_eq!(reg.total_bytes(), 500);
        assert_eq!(reg.get(&cn("test/a")).unwrap().bytes, 500);

        // Reopen — both entry.bytes and total_bytes must reset.
        reg.upsert(
            cn("test/a"),
            open_file(&redex, "test/a", 10_000),
            now + Duration::from_secs(1),
        );
        assert_eq!(reg.total_bytes(), 0, "reopen must subtract old bytes");
        assert_eq!(reg.get(&cn("test/a")).unwrap().bytes, 0);
    }

    #[test]
    fn eviction_sweep_origin_hash_zero_when_unset() {
        // An entry that never had an event landed has origin_hash =
        // 0. The runtime treats this as "nothing announced" and
        // skips the withdraw, but the cache must still surface the
        // value (rather than synthesizing a phantom hash).
        let redex = Redex::new();
        let mut reg = GreedyCacheRegistry::new(1024);
        let base = Instant::now();
        reg.upsert(cn("a"), open_file(&redex, "a", 10_000), base);
        reg.upsert(
            cn("b"),
            open_file(&redex, "b", 10_000),
            base + Duration::from_secs(1),
        );
        reg.note_appended(&cn("a"), 600, base + Duration::from_secs(2));
        let sweep = reg.note_appended(&cn("b"), 600, base + Duration::from_secs(3));
        assert_eq!(sweep.len(), 1);
        assert_eq!(sweep.evicted[0].origin_hash, 0);
    }
}
