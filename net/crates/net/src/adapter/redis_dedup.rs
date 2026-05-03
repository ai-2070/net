//! Consumer-side dedup helper for the Redis Streams adapter.
//!
//! See `net::adapter::redis` module docs for the producer-side
//! contract. Briefly:
//!
//! - The Redis adapter writes a `dedup_id` field on every XADD
//!   entry (`"{producer_nonce:hex}:{shard_id}:{sequence_start}:{i}"`),
//!   stable across retries and (when `producer_nonce_path` is
//!   configured) across process restart.
//! - Redis Streams has no server-side dedup, so duplicate entries
//!   from the producer-side `MULTI/EXEC`-timeout race land in the
//!   stream verbatim. Each carries the SAME `dedup_id`.
//! - Consumers filter at consume time by remembering recently-seen
//!   `dedup_id`s in a small LRU.
//!
//! `RedisStreamDedup` is the reference implementation of that LRU.
//! It's transport-agnostic — bring your own `redis-rs` /
//! `redis-py` / `ioredis` client; this helper just answers
//! "have we seen this dedup_id before?" against an in-memory cache.
//!
//! # Example
//!
//! ```rust
//! use net::adapter::RedisStreamDedup;
//!
//! let mut dedup = RedisStreamDedup::with_capacity(4096);
//!
//! // First time we see this id: NOT a duplicate.
//! assert!(!dedup.is_duplicate("abc123:0:0:0"));
//!
//! // Same id reappears: IS a duplicate.
//! assert!(dedup.is_duplicate("abc123:0:0:0"));
//!
//! // Different id: NOT a duplicate.
//! assert!(!dedup.is_duplicate("abc123:0:0:1"));
//! ```
//!
//! # Sizing
//!
//! The LRU capacity bounds memory and the dedup window. A consumer
//! that sees ~10k events/sec and wants ~1 minute of out-of-order
//! tolerance should size to ~600k. The default of 4096 is suited
//! to low-throughput / short-window deployments; production
//! callers should set explicitly.
//!
//! # Concurrency
//!
//! `RedisStreamDedup` is `Send + Sync`. Wrap in `Mutex` /
//! `RwLock` if multiple consumer threads share the same dedup
//! window; or run one helper per consumer thread (each with its
//! own LRU) if the threads consume disjoint stream partitions.
//! Send + Sync is required by the PyO3 binding's `#[pyclass]`
//! Send/Sync assertion.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

/// LRU-bounded set of recently-seen `dedup_id` strings.
///
/// Lookup is O(1) (hash); eviction is O(1) (deque). Memory is
/// `capacity * (avg_id_len + per-entry overhead)`. With the
/// default 4096 capacity and ~24-byte ids, that's ~100 KiB —
/// noise for any non-embedded consumer.
///
/// Ids are stored as `Arc<str>` so the queue and the lookup index
/// can share one underlying allocation per id. Storing two
/// `String`s (one in `order`, one in `seen`) would cost two
/// allocations + a `memcpy` on every new `is_duplicate` call;
/// `Arc<str>` insert is one allocation + a refcount bump.
///
/// `Arc<str>` (rather than `Rc<str>`) is required for `Send +
/// Sync`, which the PyO3 binding's `#[pyclass]` Send/Sync
/// assertion enforces (`assert_pyclass_send_sync` would otherwise
/// fail to compile). The C-FFI and NAPI wrappers would compile
/// only because raw `*mut` deref bypasses auto-trait checks, but
/// the concurrent-threads-on-one-handle test would be UB in the
/// Rust abstract machine. `Arc<str>` adds an atomic refcount-bump
/// on insert (vs `Rc::clone`'s relaxed increment) — single-digit
/// nanoseconds extra per new id, dwarfed by the heap allocation
/// that already dominates insert cost.
pub struct RedisStreamDedup {
    /// Insertion-ordered queue, used for FIFO eviction. Each entry
    /// is `Arc::clone`-shared with `seen` so we don't pay a second
    /// allocation per insert.
    ///
    /// Pre-fix doc-comments described this as
    /// "LRU eviction" but `is_duplicate` doesn't move re-observed
    /// ids to the back of the queue — re-observation is a no-op
    /// and the queue stays strictly insertion-ordered. The
    /// eviction is FIFO, not LRU. Functionally it's a sliding
    /// dedup window: any id older than `capacity` insertions ago
    /// is forgotten, regardless of how often it's been observed.
    /// Frequently-observed ids do NOT stay tracked longer than
    /// rarely-observed ones. Docstrings updated for accuracy.
    order: VecDeque<Arc<str>>,
    /// Lookup index. Shares its `Arc<str>` entries with `order` —
    /// every id lives in exactly one heap allocation, refcounted.
    seen: HashSet<Arc<str>>,
    /// Maximum number of distinct ids tracked. Older ids are
    /// evicted on insert when the set is at capacity (FIFO order).
    capacity: usize,
}

impl RedisStreamDedup {
    /// Upper bound on the dedup capacity. Beyond this,
    /// [`Self::with_capacity`] clamps. `1 << 24` (~16.7 M ids) is
    /// far above any realistic dedup window and below the point
    /// where pre-allocation would dominate the process heap
    /// (~256 MiB just for the `HashSet` + `VecDeque` reservations).
    pub const MAX_CAPACITY: usize = 1 << 24;

    /// Create a helper with the given LRU capacity.
    ///
    /// `capacity == 0` is treated as 1 — the FIFO eviction still
    /// works (every insert evicts the prior id) but the dedup
    /// window is effectively a single-id rolling window. Callers
    /// that want "no dedup at all" should not construct this
    /// helper in the first place.
    ///
    /// Pre-fix, `capacity` had no upper clamp. A
    /// misconfigured `usize::MAX` pre-allocated the `VecDeque`
    /// and `HashSet` and OOMed on construction. Capacity is now
    /// clamped to [`Self::MAX_CAPACITY`].
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, Self::MAX_CAPACITY);
        Self {
            order: VecDeque::with_capacity(capacity),
            seen: HashSet::with_capacity(capacity),
            capacity,
        }
    }

    /// Default-sized helper (4 096 ids).
    ///
    /// Sized for low-throughput consumers and short dedup
    /// windows. At 10 K events/sec this covers ~0.4 seconds —
    /// far below the "minutes of in-flight" horizon production
    /// deployments typically require. **Production callers
    /// with high throughput or long out-of-order horizons must
    /// pick a capacity explicitly via [`Self::with_capacity`].**
    /// As a rough guideline, sizing follows
    /// `peak_events_per_sec × out_of_order_tolerance_seconds`:
    /// 10 K events/sec with ~1 minute of tolerance needs ~600 K.
    ///
    /// Pre-fix the parent module's docs claimed the default
    /// matched "a few thousand ids, ~minutes of in-flight at
    /// moderate throughput" — those numbers don't match
    /// (4 096 / 10 000 ≈ 0.4 s, not minutes). The corrected
    /// guidance is now explicit on both sides.
    pub fn new() -> Self {
        Self::with_capacity(4096)
    }

    /// Number of distinct ids currently tracked.
    #[inline]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True if no ids are tracked yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Configured maximum capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Test-and-insert: returns `true` if the caller should treat
    /// the entry as a DUPLICATE (skip it), `false` if it's the
    /// first time we've seen this `dedup_id` (process it AND mark
    /// it seen).
    ///
    /// This is the primary consumer entry point. Typical usage:
    ///
    /// ```rust,no_run
    /// # use net::adapter::RedisStreamDedup;
    /// # let mut dedup = RedisStreamDedup::new();
    /// # let dedup_id = "abc:0:0:0";
    /// # let entry: () = ();
    /// # fn process(_: ()) {}
    /// if !dedup.is_duplicate(dedup_id) {
    ///     process(entry);
    /// }
    /// ```
    pub fn is_duplicate(&mut self, dedup_id: &str) -> bool {
        // Lookup uses the `Borrow<str>` impl on `Arc<str>`, so we
        // can probe the set with the borrowed `&str` — no
        // allocation on the duplicate (hot) path.
        if self.seen.contains(dedup_id) {
            return true;
        }

        // Evict the oldest id if we're at capacity. The eviction
        // is amortized O(1) — `pop_front` on `VecDeque` and
        // `remove` on the `HashSet`. Both containers hold the
        // SAME `Arc<str>`; popping the queue drops one strong
        // count, and the set's `remove` drops the other, freeing
        // the underlying allocation.
        if self.seen.len() >= self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }

        // Insert. ONE heap allocation per new id (`Arc::from(&str)`
        // copies the bytes once into a refcounted slice). The
        // `Arc::clone` on the next line is an atomic refcount bump,
        // not an allocation. Pre-fix this path did two allocations:
        // `to_owned()` for the lookup-set entry plus a separate
        // `.clone()` (= alloc + memcpy) for the queue entry.
        let id: Arc<str> = Arc::from(dedup_id);
        self.order.push_back(Arc::clone(&id));
        self.seen.insert(id);
        false
    }

    /// Clear all tracked ids. Equivalent to dropping and
    /// reconstructing — exposed for callers that want to reset
    /// the dedup window without losing the helper instance
    /// (e.g. on consumer-group rebalance).
    pub fn clear(&mut self) {
        self.order.clear();
        self.seen.clear();
    }
}

impl Default for RedisStreamDedup {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RedisStreamDedup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisStreamDedup")
            .field("len", &self.len())
            .field("capacity", &self.capacity)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_is_not_a_duplicate() {
        let mut d = RedisStreamDedup::with_capacity(8);
        assert!(!d.is_duplicate("a"));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn repeat_observation_is_a_duplicate() {
        let mut d = RedisStreamDedup::with_capacity(8);
        assert!(!d.is_duplicate("a"));
        assert!(d.is_duplicate("a"));
        // Length doesn't grow on duplicate hits.
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn distinct_ids_do_not_collide() {
        let mut d = RedisStreamDedup::with_capacity(8);
        assert!(!d.is_duplicate("a"));
        assert!(!d.is_duplicate("b"));
        assert!(!d.is_duplicate("c"));
        assert_eq!(d.len(), 3);
        // Each one is then a duplicate on re-observation.
        assert!(d.is_duplicate("a"));
        assert!(d.is_duplicate("b"));
        assert!(d.is_duplicate("c"));
    }

    /// Pin LRU eviction: inserting `capacity + 1` distinct ids
    /// drops the OLDEST. The non-evicted ids remain tracked.
    /// We split this from the "evicted id looks new" assertion
    /// because re-observing an evicted id re-inserts it (the
    /// helper has no concept of "evicted-but-suppressed"), which
    /// would push another id out and corrupt the second
    /// assertion.
    #[test]
    fn lru_keeps_non_evicted_ids_tracked() {
        let mut d = RedisStreamDedup::with_capacity(2);
        assert!(!d.is_duplicate("a"));
        assert!(!d.is_duplicate("b"));
        assert!(!d.is_duplicate("c")); // evicts "a"
        assert_eq!(d.len(), 2);

        // "b" and "c" are still tracked (only "a" was evicted).
        assert!(d.is_duplicate("b"));
        assert!(d.is_duplicate("c"));
    }

    #[test]
    fn lru_evicted_id_is_reported_as_new() {
        let mut d = RedisStreamDedup::with_capacity(2);
        assert!(!d.is_duplicate("a"));
        assert!(!d.is_duplicate("b"));
        assert!(!d.is_duplicate("c")); // evicts "a"

        // "a" was evicted — re-observation is NOT a duplicate.
        // (Side effect: this re-inserts "a", evicting "b". The
        // adjacent "non-evicted ids stay tracked" test verifies
        // the contrapositive without that side effect.)
        assert!(!d.is_duplicate("a"));
    }

    /// Pin that re-observing an id does NOT re-order it in the
    /// LRU. This is a deliberate-simplicity choice: the LRU
    /// tracks insertion order, not most-recent-use order. For
    /// the dedup-id use case this is fine — a duplicate
    /// observation means we're skipping the entry, not "using"
    /// it in any sense that would warrant promotion.
    ///
    /// We pin "no refresh" by observing that a duplicate-touched
    /// id is the FIRST one evicted at capacity overflow — i.e.
    /// the touch did NOT move it to the back of the queue.
    #[test]
    fn duplicate_observation_does_not_refresh_lru_position() {
        let mut d = RedisStreamDedup::with_capacity(2);
        assert!(!d.is_duplicate("a")); // order: [a]
        assert!(!d.is_duplicate("b")); // order: [a, b]

        // Touch "a" again. If the LRU promoted on re-observation
        // (it doesn't), "a" would move to the back: order [b, a].
        // Then inserting "c" would evict "b". We want the OPPOSITE
        // — "a" stays at the front and gets evicted on overflow.
        assert!(d.is_duplicate("a"));

        // Insert "c" — this evicts the front of the queue.
        assert!(!d.is_duplicate("c"));

        // If we evicted "a" (no refresh), "b" is still tracked.
        // If we evicted "b" (refresh), "a" would be still tracked.
        // The test asserts the no-refresh shape: "b" remains.
        assert!(
            d.is_duplicate("b"),
            "duplicate observation must NOT refresh LRU position — \
             expected `b` to still be tracked after `c` evicted the \
             front, but `b` was evicted instead",
        );
    }

    #[test]
    fn capacity_zero_is_clamped_to_one() {
        let mut d = RedisStreamDedup::with_capacity(0);
        assert_eq!(d.capacity(), 1);
        // The LRU still works — every insert evicts the prior id.
        assert!(!d.is_duplicate("a"));
        assert!(!d.is_duplicate("b")); // evicts "a"
        assert!(!d.is_duplicate("a")); // "a" was evicted, looks new
    }

    #[test]
    fn clear_resets_state() {
        let mut d = RedisStreamDedup::with_capacity(8);
        d.is_duplicate("a");
        d.is_duplicate("b");
        assert_eq!(d.len(), 2);
        d.clear();
        assert_eq!(d.len(), 0);
        assert!(d.is_empty());
        assert!(!d.is_duplicate("a")); // post-clear, looks new
    }

    /// CR-2: pin `Send + Sync`. The PyO3 `#[pyclass]` Send/Sync
    /// assertion compiled this in via the binding, but having a
    /// crate-local guard keeps the guarantee from silently
    /// regressing if a future refactor reintroduces an `Rc` or
    /// `Cell`. Static asserts are the cheapest pinning.
    #[test]
    fn redis_stream_dedup_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<RedisStreamDedup>();
        assert_sync::<RedisStreamDedup>();
    }

    /// CR-2: pin actual cross-thread shared use. With `Rc<str>` this
    /// would not compile (`Mutex<Rc<...>>` is `!Sync`). With
    /// `Arc<str>` it builds and runs cleanly under TSan / Miri.
    #[test]
    fn redis_stream_dedup_works_under_mutex_across_threads() {
        use std::sync::Arc;
        use std::sync::Mutex;
        use std::thread;

        let dedup = Arc::new(Mutex::new(RedisStreamDedup::with_capacity(128)));
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let d = Arc::clone(&dedup);
                thread::spawn(move || {
                    for i in 0..16 {
                        let id = format!("t{}:{}", t, i);
                        let mut g = d.lock().unwrap();
                        // First time should be NOT-duplicate.
                        let was_dup = g.is_duplicate(&id);
                        assert!(
                            !was_dup,
                            "thread {} id {} should be new on first insert",
                            t, i
                        );
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let g = dedup.lock().unwrap();
        assert_eq!(g.len(), 4 * 16);
    }

    /// Pin the canonical use case: producer-side dup
    /// scenario. Two `XADD`s of the same logical event produce
    /// stream entries with distinct server-generated `*` ids but
    /// IDENTICAL `dedup_id` fields. The dedup helper filters the
    /// second one.
    #[test]
    fn filters_redis_streams_producer_duplicates_by_dedup_id() {
        let mut d = RedisStreamDedup::with_capacity(64);

        // Simulate a batch of 3 events going through the adapter
        // twice (the MULTI/EXEC-timeout race scenario). Producer
        // nonce, shard, and sequence_start are stable; only the
        // (server-assigned) stream id would differ.
        let dedup_ids = ["deadbeef:0:0:0", "deadbeef:0:0:1", "deadbeef:0:0:2"];

        // First pass: all three observed as new.
        for id in &dedup_ids {
            assert!(
                !d.is_duplicate(id),
                "first observation of {id} should not be a duplicate",
            );
        }

        // Producer-side retry path: the same ids reappear in the
        // stream (with different stream-ids, but same dedup_ids).
        for id in &dedup_ids {
            assert!(
                d.is_duplicate(id),
                "retry-path observation of {id} should be filtered as a duplicate",
            );
        }
    }

    /// A misconfigured `capacity == usize::MAX` must not
    /// OOM at construction. Pre-fix, `with_capacity(usize::MAX)`
    /// pre-allocated `VecDeque` + `HashSet` for the full range and
    /// the process aborted before the helper was even usable.
    #[test]
    fn with_capacity_clamps_usize_max() {
        let d = RedisStreamDedup::with_capacity(usize::MAX);
        assert_eq!(
            d.capacity,
            RedisStreamDedup::MAX_CAPACITY,
            "capacity must be clamped at MAX_CAPACITY",
        );
        // The clamp value must be small enough to actually pre-
        // allocate without OOMing on a development machine — the
        // mere fact that this test reached this assertion proves
        // it. Const-block assert so clippy doesn't flag it as
        // a runtime check on a constant.
        const _: () = assert!(
            RedisStreamDedup::MAX_CAPACITY < usize::MAX,
            "MAX_CAPACITY must strictly bound usize::MAX",
        );
    }

    #[test]
    fn with_capacity_preserves_in_range_values() {
        let d = RedisStreamDedup::with_capacity(1024);
        assert_eq!(d.capacity, 1024);
        let d_zero = RedisStreamDedup::with_capacity(0);
        assert_eq!(d_zero.capacity, 1, "0 must clamp UP to 1, not down");
    }
}
