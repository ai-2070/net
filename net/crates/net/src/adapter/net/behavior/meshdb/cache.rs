//! Phase F result cache — single-node LRU keyed on the
//! plan-byte hash + the capability-index version.
//!
//! # Design (per locked Phase F decisions)
//!
//! - **Global cache version**, not per-query classification:
//!   any mutation to the local `CapabilityIndex` bumps a
//!   monotonic version counter; cache lookups compare the
//!   stored version against the current one and miss on any
//!   divergence (pull-based invalidation). The cost is high
//!   invalidation churn on busy capability surfaces; the fix
//!   for that is `bypass_cache` or
//!   [`CachePolicy::Permanent`] per query, not softening the
//!   bump.
//!
//! - **`TimeBound { ttl }` is the default policy** (5 s,
//!   mirroring the locked-decision-#2 join watermark).
//!   Callers that know their query is over a closed substrate
//!   range — `At(chain, seq)`, `Between(chain, a..b)` with
//!   `b ≤ current_tip` — may pass
//!   [`CachePolicy::Permanent`] to cache the result until
//!   LRU evicts it.
//!
//! - **`bypass_cache`** on [`super::executor::ExecuteOptions`]
//!   skips both lookup and writeback. For diagnostics,
//!   authoritative reads, and the "force TTL to zero" hack
//!   that this flag exists specifically to obviate.
//!
//! # Wire-key shape
//!
//! Plans are byte-identical-deterministic (locked decision
//! #1). We postcard-encode the plan, run [`DefaultHasher`]
//! (std-internal; not algorithm-stable across Rust releases,
//! but stable within a binary) over the bytes, and pair the
//! `u64` digest with the capability version. Hashing keeps
//! the key small (16 bytes) regardless of plan size; cache
//! lookups stay O(1).
//!
//! Not every plan is postcard-encodable — `Filter` /
//! `Discovered` carry [`PredicateWire`](super::query::PredicateWire)
//! which is `#[serde(tag = "kind")]` and falls outside
//! postcard's supported subset. `for_plan` returns
//! `Option<CacheKey>` so the executor can treat encode
//! failures as a transparent cache bypass rather than a
//! panic.
//!
//! # LRU mechanics
//!
//! Hand-rolled: a `HashMap<CacheKey, Node>` carrying an
//! intrusive doubly-linked list of keys. Eviction trips on
//! either entry count (default `LRU_MAX_ENTRIES`) or
//! approximate byte size (default `LRU_MAX_BYTES`). Per a
//! Phase F locked decision: 1024 entries / 256 MiB.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::planner::ExecutionPlan;
use super::query::ResultRow;

/// Cache policy attached to an [`super::executor::ExecuteOptions`].
/// Callers either know their query's result is permanent under
/// substrate semantics (`At` / closed `Between`) and pass
/// `Permanent`, or accept that the result represents a
/// snapshot of moving state and want `TimeBound` freshness.
///
/// The default is `TimeBound { ttl: 5s }`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CachePolicy {
    /// Hold until LRU eviction. No staleness deadline beyond
    /// capability-index-version mismatch.
    Permanent,
    /// TTL-bounded; entry expires after `ttl` regardless of
    /// the capability-index version.
    TimeBound {
        /// Wall-clock duration after which the entry is treated
        /// as stale.
        ttl: Duration,
    },
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self::TimeBound {
            ttl: Duration::from_secs(5),
        }
    }
}

/// Composite cache lookup key: plan-hash + capability version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// SipHash digest of the plan's postcard byte form.
    pub plan_hash: u64,
    /// `CapabilityIndex::version()` snapshot at plan time.
    pub capability_version: u64,
}

impl CacheKey {
    /// Hash a plan into a cache key for the given capability
    /// version. Per locked decision #1, plans are byte-identical-
    /// deterministic, so encode + hash is stable across runs.
    ///
    /// Returns `None` when the plan contains a node postcard
    /// can't encode (currently: any `Filter` / `Discovered`
    /// node, because `PredicateWire` uses `#[serde(tag)]`).
    /// Callers treat that as a transparent cache bypass.
    pub fn for_plan(plan: &ExecutionPlan, capability_version: u64) -> Option<Self> {
        use std::collections::hash_map::DefaultHasher;
        let bytes = postcard::to_allocvec(plan).ok()?;
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        Some(Self {
            plan_hash: hasher.finish(),
            capability_version,
        })
    }
}

/// One cached query result.
#[derive(Clone, Debug)]
pub struct CachedResult {
    /// Materialized rows in stream order.
    pub rows: Vec<ResultRow>,
    /// When the entry was inserted. Combined with the policy's
    /// `ttl` to decide expiry.
    pub inserted_at: Instant,
    /// The policy under which this entry was inserted.
    pub policy: CachePolicy,
}

impl CachedResult {
    /// Whether the entry has aged past its TTL. `Permanent`
    /// never expires by time.
    pub fn is_expired(&self) -> bool {
        match self.policy {
            CachePolicy::Permanent => false,
            CachePolicy::TimeBound { ttl } => self.inserted_at.elapsed() >= ttl,
        }
    }

    /// Approximate in-memory byte size: payload bytes + a
    /// fixed per-row overhead. Used to enforce
    /// `LRU_MAX_BYTES`.
    fn approx_bytes(&self) -> u64 {
        let row_overhead: u64 = 64; // ResultRow header bytes (origin + seq + Vec header)
        self.rows
            .iter()
            .map(|r| r.payload.len() as u64 + row_overhead)
            .sum::<u64>()
    }
}

/// Pluggable cache trait. The hot path is `get` / `insert`;
/// `invalidate_all` is tooling-only (the version-bump path
/// pull-invalidates without touching this).
pub trait ResultCache: Send + Sync {
    /// Look up an entry. Returns `Some` only when the entry is
    /// present AND unexpired AND its `capability_version`
    /// matches `key.capability_version` (the latter is
    /// enforced by the key itself — entries inserted under a
    /// different version simply don't collide).
    fn get(&self, key: &CacheKey) -> Option<CachedResult>;

    /// Insert a fresh entry. Replaces any prior entry at the
    /// same key; trips LRU eviction if the cache is full.
    fn insert(&self, key: CacheKey, result: CachedResult);

    /// Drop every entry. Used by tests + explicit operator
    /// flushes; not on the hot path.
    fn invalidate_all(&self);

    /// Current number of entries. For metrics / tests.
    fn len(&self) -> usize;

    /// Whether the cache holds no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// LRU max-entries bound. Phase F locked decision: 1024 keeps
/// the working set small enough to fit comfortably alongside
/// the executor's working memory.
pub const LRU_MAX_ENTRIES: usize = 1024;

/// LRU max-bytes bound. Phase F locked decision: 256 MiB
/// covers most catalog / blob-index introspection results
/// without letting one runaway query OOM the executor.
pub const LRU_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// In-memory LRU cache. Hand-rolled to avoid the `lru` crate
/// dependency — the substrate already has intrusive lists in
/// several places and the implementation is ~80 lines.
pub struct LruResultCache {
    inner: Mutex<LruInner>,
}

struct LruInner {
    /// Key → node-id mapping. Nodes live in `nodes`.
    by_key: HashMap<CacheKey, usize>,
    /// Doubly-linked list nodes. Indexed by `usize` so the
    /// `prev` / `next` cells don't need Arc<Mutex> contortions.
    nodes: Vec<LruNode>,
    /// MRU end of the list.
    head: Option<usize>,
    /// LRU end of the list.
    tail: Option<usize>,
    /// Free list of vacated node indices.
    free: Vec<usize>,
    /// Total approximate bytes across all entries.
    total_bytes: u64,
    /// Maximum entries.
    max_entries: usize,
    /// Maximum approximate bytes.
    max_bytes: u64,
}

struct LruNode {
    key: CacheKey,
    value: CachedResult,
    prev: Option<usize>,
    next: Option<usize>,
    bytes: u64,
}

impl Default for LruResultCache {
    fn default() -> Self {
        Self::new(LRU_MAX_ENTRIES, LRU_MAX_BYTES)
    }
}

impl LruResultCache {
    /// Construct an LRU with the given bounds. Either bound
    /// being exceeded triggers eviction of the LRU end until
    /// both are satisfied.
    pub fn new(max_entries: usize, max_bytes: u64) -> Self {
        Self {
            inner: Mutex::new(LruInner {
                by_key: HashMap::new(),
                nodes: Vec::new(),
                head: None,
                tail: None,
                free: Vec::new(),
                total_bytes: 0,
                max_entries,
                max_bytes,
            }),
        }
    }
}

impl ResultCache for LruResultCache {
    fn get(&self, key: &CacheKey) -> Option<CachedResult> {
        let mut g = self.inner.lock().unwrap();
        let idx = *g.by_key.get(key)?;
        if g.nodes[idx].value.is_expired() {
            // Lazy eviction: drop the entry and miss. Hot
            // path stays cheap; we don't sweep proactively.
            g.detach_and_drop(idx);
            return None;
        }
        // Move-to-head (MRU).
        g.move_to_head(idx);
        Some(g.nodes[idx].value.clone())
    }

    fn insert(&self, key: CacheKey, result: CachedResult) {
        let mut g = self.inner.lock().unwrap();
        let bytes = result.approx_bytes();
        // Replace existing entry at this key, if any.
        if let Some(&idx) = g.by_key.get(&key) {
            let old_bytes = g.nodes[idx].bytes;
            g.total_bytes = g.total_bytes.saturating_sub(old_bytes);
            g.nodes[idx].value = result;
            g.nodes[idx].bytes = bytes;
            g.total_bytes = g.total_bytes.saturating_add(bytes);
            g.move_to_head(idx);
            g.evict_until_within_bounds();
            return;
        }
        let prev_head = g.head;
        let idx = g.alloc_node(LruNode {
            key,
            value: result,
            prev: None,
            next: prev_head,
            bytes,
        });
        if let Some(h) = g.head {
            g.nodes[h].prev = Some(idx);
        }
        g.head = Some(idx);
        if g.tail.is_none() {
            g.tail = Some(idx);
        }
        g.by_key.insert(key, idx);
        g.total_bytes = g.total_bytes.saturating_add(bytes);
        g.evict_until_within_bounds();
    }

    fn invalidate_all(&self) {
        let mut g = self.inner.lock().unwrap();
        g.by_key.clear();
        g.nodes.clear();
        g.head = None;
        g.tail = None;
        g.free.clear();
        g.total_bytes = 0;
    }

    fn len(&self) -> usize {
        self.inner.lock().unwrap().by_key.len()
    }
}

impl LruInner {
    fn alloc_node(&mut self, node: LruNode) -> usize {
        if let Some(idx) = self.free.pop() {
            self.nodes[idx] = node;
            idx
        } else {
            self.nodes.push(node);
            self.nodes.len() - 1
        }
    }

    fn detach(&mut self, idx: usize) {
        let (prev, next) = (self.nodes[idx].prev, self.nodes[idx].next);
        match prev {
            Some(p) => self.nodes[p].next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.nodes[n].prev = prev,
            None => self.tail = prev,
        }
        self.nodes[idx].prev = None;
        self.nodes[idx].next = None;
    }

    fn detach_and_drop(&mut self, idx: usize) {
        let key = self.nodes[idx].key;
        let bytes = self.nodes[idx].bytes;
        self.detach(idx);
        self.by_key.remove(&key);
        self.total_bytes = self.total_bytes.saturating_sub(bytes);
        self.free.push(idx);
    }

    fn move_to_head(&mut self, idx: usize) {
        if self.head == Some(idx) {
            return;
        }
        self.detach(idx);
        self.nodes[idx].prev = None;
        self.nodes[idx].next = self.head;
        if let Some(h) = self.head {
            self.nodes[h].prev = Some(idx);
        }
        self.head = Some(idx);
        if self.tail.is_none() {
            self.tail = Some(idx);
        }
    }

    fn evict_until_within_bounds(&mut self) {
        while self.by_key.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(tail) = self.tail else { break };
            self.detach_and_drop(tail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::query::SeqNum;
    use super::*;
    use std::thread::sleep;

    fn make_rows(n: usize) -> Vec<ResultRow> {
        (0..n)
            .map(|i| ResultRow {
                origin: i as u64,
                seq: SeqNum(i as u64),
                payload: vec![0u8; 8],
            })
            .collect()
    }

    fn make_result(rows: Vec<ResultRow>, policy: CachePolicy) -> CachedResult {
        CachedResult {
            rows,
            inserted_at: Instant::now(),
            policy,
        }
    }

    fn key(plan: u64, version: u64) -> CacheKey {
        CacheKey {
            plan_hash: plan,
            capability_version: version,
        }
    }

    #[test]
    fn default_policy_is_timebound_5s() {
        assert_eq!(
            CachePolicy::default(),
            CachePolicy::TimeBound {
                ttl: Duration::from_secs(5)
            }
        );
    }

    #[test]
    fn permanent_entries_never_expire_by_time() {
        let r = make_result(vec![], CachePolicy::Permanent);
        assert!(!r.is_expired());
    }

    #[test]
    fn timebound_entries_expire_after_ttl() {
        let r = CachedResult {
            rows: vec![],
            inserted_at: Instant::now() - Duration::from_millis(50),
            policy: CachePolicy::TimeBound {
                ttl: Duration::from_millis(10),
            },
        };
        assert!(r.is_expired());
    }

    #[test]
    fn lru_round_trips_a_simple_insert_then_get() {
        let cache = LruResultCache::default();
        let k = key(1, 1);
        cache.insert(k, make_result(make_rows(3), CachePolicy::Permanent));
        let got = cache.get(&k).expect("hit");
        assert_eq!(got.rows.len(), 3);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn lru_miss_on_unknown_key() {
        let cache = LruResultCache::default();
        assert!(cache.get(&key(42, 0)).is_none());
    }

    #[test]
    fn lru_miss_on_version_mismatch_by_construction() {
        // CacheKey embeds capability_version; entries inserted
        // under one version simply don't collide with another.
        let cache = LruResultCache::default();
        cache.insert(key(1, 1), make_result(vec![], CachePolicy::Permanent));
        assert!(cache.get(&key(1, 2)).is_none());
        assert!(cache.get(&key(1, 1)).is_some());
    }

    #[test]
    fn lru_expired_entries_miss_and_are_dropped_lazily() {
        let cache = LruResultCache::default();
        let k = key(1, 0);
        let stale = CachedResult {
            rows: vec![],
            inserted_at: Instant::now() - Duration::from_millis(50),
            policy: CachePolicy::TimeBound {
                ttl: Duration::from_millis(10),
            },
        };
        cache.insert(k, stale);
        assert!(cache.get(&k).is_none());
        assert_eq!(cache.len(), 0, "expired entry dropped on miss");
    }

    #[test]
    fn lru_evicts_least_recently_used_when_entry_bound_trips() {
        let cache = LruResultCache::new(2, u64::MAX);
        cache.insert(key(1, 0), make_result(make_rows(1), CachePolicy::Permanent));
        cache.insert(key(2, 0), make_result(make_rows(1), CachePolicy::Permanent));
        // Touch key=1 so key=2 is LRU.
        let _ = cache.get(&key(1, 0));
        cache.insert(key(3, 0), make_result(make_rows(1), CachePolicy::Permanent));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&key(1, 0)).is_some());
        assert!(cache.get(&key(2, 0)).is_none(), "evicted as LRU");
        assert!(cache.get(&key(3, 0)).is_some());
    }

    #[test]
    fn lru_evicts_when_byte_bound_trips() {
        // 100-byte budget; each row ~72 bytes (64 overhead + 8 payload).
        let cache = LruResultCache::new(usize::MAX, 100);
        cache.insert(key(1, 0), make_result(make_rows(1), CachePolicy::Permanent));
        assert_eq!(cache.len(), 1);
        cache.insert(key(2, 0), make_result(make_rows(1), CachePolicy::Permanent));
        // Bound forces one eviction.
        assert!(cache.len() <= 1);
    }

    #[test]
    fn lru_replace_at_same_key_updates_bytes_in_place() {
        let cache = LruResultCache::new(8, 10_000);
        let k = key(1, 0);
        cache.insert(k, make_result(make_rows(1), CachePolicy::Permanent));
        cache.insert(k, make_result(make_rows(5), CachePolicy::Permanent));
        assert_eq!(cache.len(), 1);
        let got = cache.get(&k).unwrap();
        assert_eq!(got.rows.len(), 5);
    }

    #[test]
    fn invalidate_all_drops_every_entry() {
        let cache = LruResultCache::default();
        for i in 0..5 {
            cache.insert(key(i, 0), make_result(make_rows(1), CachePolicy::Permanent));
        }
        assert_eq!(cache.len(), 5);
        cache.invalidate_all();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn key_for_plan_is_deterministic() {
        // Same plan + same version -> same hash. Pinned because
        // locked decision #4's cache-key contract depends on this.
        use super::super::planner::{CostEstimate, OperatorNode, OperatorPlan};
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::LatestRead {
                    origin: 0xABCD_EF01,
                },
                target_nodes: vec![1, 2, 3],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let k1 = CacheKey::for_plan(&plan, 7).unwrap();
        let k2 = CacheKey::for_plan(&plan, 7).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn key_for_plan_differs_on_version_change() {
        use super::super::planner::{CostEstimate, OperatorNode, OperatorPlan};
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::LatestRead { origin: 0x01 },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let a = CacheKey::for_plan(&plan, 1).unwrap();
        let b = CacheKey::for_plan(&plan, 2).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn key_for_plan_handles_filter_plans_without_panicking() {
        // `Filter` carries `PredicateWire`, whose
        // `PredicateNodeWire` uses `#[serde(tag = "kind")]`.
        // Postcard *encodes* internally-tagged enums fine
        // (the failure mode is on decode); cache hashing only
        // touches encode, so we just need a stable u64 here.
        // The Option return type is the future-proof contract
        // for any plan variant that becomes un-encodable
        // (the cache transparently bypasses rather than
        // panicking).
        use super::super::planner::{CostEstimate, OperatorNode, OperatorPlan};
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::TagKey;
        use crate::adapter::net::behavior::TaxonomyAxis;
        let pred = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Software, "any"),
            value: "v".to_string(),
        };
        let inner = OperatorNode {
            operator: OperatorPlan::LatestRead { origin: 0x01 },
            target_nodes: vec![],
            cost: CostEstimate::default(),
        };
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::Filter {
                    input: Box::new(inner),
                    predicate: pred.to_wire(),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        // Today's wire encoding succeeds; the key is stable
        // across runs (deterministic-plan contract). The
        // option-shape is the load-bearing piece — see
        // `for_plan` doc-comment.
        let k = CacheKey::for_plan(&plan, 0).expect("filter plan is encodable today");
        assert_eq!(k, CacheKey::for_plan(&plan, 0).unwrap());
    }

    #[test]
    fn ttl_expiry_is_observable_through_get() {
        let cache = LruResultCache::default();
        let k = key(1, 0);
        let entry = make_result(
            make_rows(1),
            CachePolicy::TimeBound {
                ttl: Duration::from_millis(15),
            },
        );
        cache.insert(k, entry);
        assert!(cache.get(&k).is_some());
        sleep(Duration::from_millis(25));
        assert!(cache.get(&k).is_none());
    }
}
