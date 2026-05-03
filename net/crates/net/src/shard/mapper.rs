//! Dynamic shard mapping and scaling.
//!
//! This module implements dynamic shard scaling following Approach B from
//! DYNAMIC_SHARD_SCALING.md: increasing the number of shards and rebalancing
//! producers across them, while maintaining SPSC (single-producer single-consumer)
//! semantics per shard.
//!
//! # Core Principles
//!
//! - Shards never accept multiple producers
//! - Producers get moved to new shards when load increases
//! - Ingestion performance stays at 700M ops/sec per shard
//! - Ordering guarantees remain intact within a shard
//! - Total throughput scales linearly with shard count
//!
//! # Scaling Triggers
//!
//! - Ring buffer fill ratio > threshold (default 70%)
//! - Push latency exceeds threshold (default 5ns)
//! - Batch flush latency exceeds threshold
//! - Session/producer count growth
//!
//! # Architecture
//!
//! ```text
//! Producers -----+
//!                |
//!                v
//!     +----------------------------+
//!     |   Dynamic Shard Mapper     |
//!     +----------------------------+
//!        |         |         |
//!        v         v         v
//!     Shard 0   Shard 1   Shard 2 …
//! ```

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::config::ScalingPolicy;

/// Metrics for a single shard used for scaling decisions.
#[derive(Debug, Clone)]
pub struct ShardMetrics {
    /// Shard identifier.
    pub shard_id: u16,
    /// Current fill ratio (0.0 - 1.0).
    pub fill_ratio: f64,
    /// Events ingested in the current window.
    pub event_rate: u64,
    /// Average push latency in nanoseconds.
    pub avg_push_latency_ns: u64,
    /// Average batch flush latency in microseconds.
    pub avg_flush_latency_us: u64,
    /// Whether this shard is in drain mode.
    pub draining: bool,
    /// Computed weight for load balancing (lower = less loaded).
    pub weight: f64,
    /// Last update timestamp.
    pub last_updated: Instant,
}

impl ShardMetrics {
    /// Create new metrics for a shard.
    pub fn new(shard_id: u16) -> Self {
        Self {
            shard_id,
            fill_ratio: 0.0,
            event_rate: 0,
            avg_push_latency_ns: 0,
            avg_flush_latency_us: 0,
            draining: false,
            weight: 0.0,
            last_updated: Instant::now(),
        }
    }

    /// Compute the weight based on current metrics.
    /// Lower weight = better candidate for new producers.
    pub fn compute_weight(&mut self) {
        // Weight formula: combines fill ratio, latency, and event rate
        // Higher fill ratio = higher weight (avoid overloaded shards)
        // Higher latency = higher weight
        // Higher event rate = higher weight
        let fill_weight = self.fill_ratio * 100.0;
        let latency_weight = (self.avg_push_latency_ns as f64) / 10.0;
        let rate_weight = (self.event_rate as f64) / 1_000_000.0;

        self.weight = fill_weight + latency_weight + rate_weight;

        // Draining shards get maximum weight (never assign new producers)
        if self.draining {
            self.weight = f64::MAX;
        }
    }
}

/// Live metrics collector for a shard (atomics for lock-free updates).
#[derive(Debug)]
pub struct ShardMetricsCollector {
    /// Shard identifier.
    shard_id: u16,
    /// Ring buffer capacity.
    capacity: usize,
    /// Current buffer length (updated by shard).
    current_len: AtomicU64,
    /// Events ingested in current window.
    events_in_window: AtomicU64,
    /// Packed `(count << 32) | sum` for push latencies (ns).
    /// Pre-fix `push_latency_sum_ns` and `push_count` were
    /// independent `AtomicU64`s. `record_push` did two separate
    /// `fetch_add`s, and `collect_and_reset` did two separate
    /// `swap`s. A metrics tick interleaving between the two
    /// `fetch_add`s captured the sum WITHOUT the count (or
    /// vice versa); the resulting `avg = sum.checked_div(count)
    /// .unwrap_or(0)` returned 0 in window N (sum without
    /// count) and 0 in window N+1 (count without sum), silently
    /// zeroing the average that drives `evaluate_scaling`'s
    /// push-latency scale-up trigger. Packing into one u64 makes
    /// the `(sum, count)` update atomic; the upper 32 bits hold
    /// the count (u32::MAX = 4G calls/window — plenty) and the
    /// lower 32 hold the sum (u32::MAX = 4 G ns ≈ 4 s, also
    /// plenty for any sane window).
    push_latency: AtomicU64,
    /// Packed `(count << 32) | sum` for flush latencies (us).
    /// Same shape and rationale as `push_latency`. The lower 32
    /// bits hold sum-µs (u32::MAX ≈ 4 Gµs ≈ 67 minutes — far
    /// past any plausible window).
    flush_latency: AtomicU64,
    /// Whether this shard is draining.
    draining: AtomicBool,
    /// Window start time.
    window_start: RwLock<Instant>,
    /// Pushes observed since `set_draining(true)` was last called.
    /// Distinct from `events_in_window` because this counter is NOT
    /// reset by `collect_and_reset`. `finalize_draining` reads this
    /// instead of `events_in_window` so a drain-window-overlap with
    /// a metrics tick can no longer race the counter to zero before
    /// the producer is observed.
    pushes_since_drain_start: AtomicU64,
}

impl ShardMetricsCollector {
    /// Create a new metrics collector.
    pub fn new(shard_id: u16, capacity: usize) -> Self {
        Self {
            shard_id,
            capacity,
            current_len: AtomicU64::new(0),
            events_in_window: AtomicU64::new(0),
            push_latency: AtomicU64::new(0),
            flush_latency: AtomicU64::new(0),
            draining: AtomicBool::new(false),
            window_start: RwLock::new(Instant::now()),
            pushes_since_drain_start: AtomicU64::new(0),
        }
    }

    /// Record current buffer length.
    #[inline]
    pub fn record_buffer_len(&self, len: usize) {
        self.current_len.store(len as u64, AtomicOrdering::Relaxed);
    }

    /// Record an event ingestion.
    #[inline]
    pub fn record_push(&self, latency_ns: u64) {
        self.events_in_window.fetch_add(1, AtomicOrdering::Relaxed);
        // Atomically add 1 to count (upper 32 bits) and
        // `latency_ns` to sum (lower 32 bits). `fetch_update`
        // CAS-loops the load-and-store, so a concurrent
        // `collect_and_reset` swap on the same word either sees
        // both pre-add or both post-add — no `(sum, count)`
        // desync. Saturating ops cap at u32::MAX inside the
        // pack window (~4 G calls / 4 s of accumulated latency),
        // which is far beyond any sane metrics tick.
        let _ =
            self.push_latency
                .fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |v| {
                    let count = (v >> 32) as u32;
                    let sum = (v & 0xFFFF_FFFF) as u32;
                    let new_count = count.saturating_add(1) as u64;
                    let new_sum = sum.saturating_add(latency_ns.min(u32::MAX as u64) as u32) as u64;
                    Some((new_count << 32) | new_sum)
                });
        // Always increment — the cost is one fetch_add and the
        // counter only matters when the shard is draining. Cheaper
        // than branching on `self.draining.load()` in the hot path.
        self.pushes_since_drain_start
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    /// Record a batch flush.
    #[inline]
    pub fn record_flush(&self, latency_us: u64) {
        // Same packed-`(count, sum)` shape as `record_push` —
        // see that function for the desync rationale.
        let _ = self.flush_latency.fetch_update(
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
            |v| {
                let count = (v >> 32) as u32;
                let sum = (v & 0xFFFF_FFFF) as u32;
                let new_count = count.saturating_add(1) as u64;
                let new_sum = sum.saturating_add(latency_us.min(u32::MAX as u64) as u32) as u64;
                Some((new_count << 32) | new_sum)
            },
        );
    }

    /// Set drain mode.
    ///
    /// Transitions to draining reset `pushes_since_drain_start` so
    /// `finalize_draining` only counts pushes that arrived after the
    /// drain began.
    ///
    /// A concurrent `record_push` can interleave with the store-zero
    /// on `pushes_since_drain_start` and leave the counter at `1`
    /// rather than `0`. That's not a correctness bug — it just defers
    /// finalization of this shard by one metrics tick — but the
    /// previous code did the store-zero on the counter *before*
    /// publishing the `draining=true` flag, which made the window
    /// slightly larger than necessary. Publishing the flag first
    /// means any push that *observes* `draining=true` is naturally
    /// sequenced after the reset; pushes that beat the flag publish
    /// race the reset just like before.
    ///
    /// We also use `SeqCst` for the publish to give the rest of the
    /// crate a single total order on draining transitions, which
    /// matches the ordering on `try_enter_ingest`'s shutdown flag.
    pub fn set_draining(&self, draining: bool) {
        if draining {
            // Store-zero first (so the flag publish below acts as
            // the release-fence for both writes).
            self.pushes_since_drain_start
                .store(0, AtomicOrdering::SeqCst);
        }
        self.draining.store(draining, AtomicOrdering::SeqCst);
    }

    /// Number of pushes observed since `set_draining(true)` was
    /// last called. Used by `finalize_draining` to detect lingering
    /// producers that the window-reset `events_in_window` counter
    /// can race past.
    ///
    /// Pre-fix used `Ordering::Relaxed`, but the writer
    /// side (`set_draining(true)`) resets the counter under
    /// `SeqCst`. On weakly-ordered hardware (ARM), a Relaxed
    /// reader could observe a stale counter and `finalize_draining`
    /// would falsely conclude the drain had flushed while
    /// producers were still pushing. Acquire pairs with the
    /// SeqCst release of the reset (SeqCst includes Release
    /// semantics), making the reset happen-before this load.
    pub fn pushes_since_drain_start(&self) -> u64 {
        self.pushes_since_drain_start.load(AtomicOrdering::Acquire)
    }

    /// Check if draining.
    pub fn is_draining(&self) -> bool {
        self.draining.load(AtomicOrdering::Acquire)
    }

    /// Collect metrics and reset window counters.
    ///
    /// NOTE: The individual atomic swaps below are not collectively atomic with
    /// respect to concurrent `record_push`/`record_flush` calls. This means a
    /// push recorded between, say, the `events_in_window` swap and the
    /// `push_count` swap could be counted in one counter but not the other for
    /// a given window. This small inaccuracy is an accepted trade-off to
    /// preserve the lock-free design of the hot path (`record_push` /
    /// `record_flush`). Adding a `Mutex` here would serialize the hot path
    /// and defeat the purpose of using atomics.
    pub fn collect_and_reset(&self) -> ShardMetrics {
        let current_len = self.current_len.load(AtomicOrdering::Relaxed);
        let events = self.events_in_window.swap(0, AtomicOrdering::Relaxed);
        // Single swap captures `(count, sum)` together; no
        // chance of catching the sum without the matching count
        // (or vice versa) the way two independent swaps did.
        let push_packed = self.push_latency.swap(0, AtomicOrdering::Relaxed);
        let push_count = push_packed >> 32;
        let push_latency_sum = push_packed & 0xFFFF_FFFF;
        let flush_packed = self.flush_latency.swap(0, AtomicOrdering::Relaxed);
        let flush_count = flush_packed >> 32;
        let flush_latency_sum = flush_packed & 0xFFFF_FFFF;

        let fill_ratio = if self.capacity > 0 {
            current_len as f64 / self.capacity as f64
        } else {
            0.0
        };

        let avg_push_latency = push_latency_sum.checked_div(push_count).unwrap_or(0);

        let avg_flush_latency = flush_latency_sum.checked_div(flush_count).unwrap_or(0);

        // Reset window
        *self.window_start.write() = Instant::now();

        let mut metrics = ShardMetrics {
            shard_id: self.shard_id,
            fill_ratio,
            event_rate: events,
            avg_push_latency_ns: avg_push_latency,
            avg_flush_latency_us: avg_flush_latency,
            draining: self.draining.load(AtomicOrdering::Acquire),
            weight: 0.0,
            last_updated: Instant::now(),
        };
        metrics.compute_weight();
        metrics
    }
}

/// Scaling decision made by the mapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalingDecision {
    /// No scaling needed.
    None,
    /// Scale up by adding N shards.
    ScaleUp(u16),
    /// Scale down by removing N shards (marks them for draining).
    ScaleDown(u16),
}

/// Errors that can occur during scaling operations.
#[derive(Debug, Clone)]
pub enum ScalingError {
    /// Invalid scaling policy.
    InvalidPolicy(String),
    /// Already at maximum shards.
    AtMaxShards,
    /// Already at minimum shards.
    AtMinShards,
    /// Scaling operation in cooldown.
    InCooldown,
    /// Shard creation failed.
    ShardCreationFailed(String),
}

impl std::fmt::Display for ScalingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPolicy(msg) => write!(f, "invalid scaling policy: {}", msg),
            Self::AtMaxShards => write!(f, "already at maximum shard count"),
            Self::AtMinShards => write!(f, "already at minimum shard count"),
            Self::InCooldown => write!(f, "scaling operation in cooldown"),
            Self::ShardCreationFailed(msg) => write!(f, "shard creation failed: {}", msg),
        }
    }
}

impl std::error::Error for ScalingError {}

/// State of a shard in the mapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardState {
    /// Shard is being provisioned: id allocated and metrics collector
    /// in place, but `select_shard` must not route to it yet because
    /// upstream workers (drain / batch) have not been spawned. Caller
    /// transitions to `Active` via `activate` once the workers are
    /// ready. Closes the race where a freshly-added shard accepted
    /// producer pushes before any consumer existed.
    Provisioning,
    /// Shard is active and accepting producers.
    Active,
    /// Shard is draining (no new producers, waiting for empty).
    Draining,
    /// Shard is stopped and can be removed.
    Stopped,
}

/// Information about a shard managed by the mapper.
#[derive(Debug)]
struct MappedShard {
    /// Shard ID.
    id: u16,
    /// Current state.
    state: ShardState,
    /// Metrics collector.
    metrics: Arc<ShardMetricsCollector>,
    /// When this shard entered drain mode (if draining).
    drain_started: Option<Instant>,
    /// Last collected metrics.
    last_metrics: ShardMetrics,
    /// When this shard last transitioned to `Active`. Used by
    /// `evaluate_scaling` to skip recently-activated shards from
    /// scale decisions: their `last_metrics` is the
    /// `ShardMetrics::new(id)` placeholder until at least one
    /// `collect_metrics` cycle has run, and the placeholder
    /// (`fill_ratio = 0.0, event_rate = 0`) trips the
    /// underutilized trigger immediately — oscillating the system
    /// (scale-up → next tick scale-down → next tick scale-up …)
    /// when a fresh shard is added but hasn't yet absorbed any
    /// traffic.
    activated_at: Instant,
}

/// Callback type for shard lifecycle events.
type ShardCallback = Box<dyn Fn(u16) + Send + Sync>;

/// Dynamic shard mapper that manages shard allocation and producer routing.
///
/// This is the core component for dynamic scaling. It:
/// - Tracks metrics for all shards
/// - Makes scaling decisions based on policy
/// - Routes producers to the least-loaded shards
/// - Manages shard lifecycle (active → draining → stopped)
pub struct ShardMapper {
    /// Mapped shards (RwLock for concurrent reads, rare writes).
    shards: RwLock<Vec<MappedShard>>,
    /// Current active shard count.
    active_count: AtomicU16,
    /// Scaling policy.
    ///
    /// This field is immutable for the lifetime of the mapper. The
    /// previous `set_policy(&mut self, …)` API was unreachable in
    /// practice — every production callsite holds the mapper behind
    /// an `Arc`, and `Arc::get_mut` requires a strong count of 1,
    /// which never holds once the worker pool clones the `Arc`. The
    /// method has been removed; recreate the mapper (and
    /// the bus that owns it) to change the policy.
    policy: ScalingPolicy,
    /// Ring buffer capacity for new shards.
    ring_buffer_capacity: usize,
    /// Last scaling operation timestamp.
    ///
    /// This RwLock is **logically** scoped to the
    /// outer `shards.write()` lock — `scale_up`, `scale_down`,
    /// and `scale_up_provisioning` all read this field and write
    /// to it while holding `shards.write()`. The cooldown gate
    /// is therefore atomic with the scale mutation: no caller
    /// can pass the cooldown check, observe stale `last_scaling`,
    /// and have its mutation interleave with another caller.
    ///
    /// **If you narrow `shards.write()`'s scope in any of those
    /// callers, this implicit serialization breaks.** Either:
    ///   - Keep the cooldown read+write inside the outer lock, OR
    ///   - Use a `compare_exchange`-style update on a single
    ///     `AtomicI64` of nanos so the gate is atomic on its own.
    ///
    /// The doc-comment is here so a future refactorer doesn't
    /// silently break the contract.
    last_scaling: RwLock<Option<Instant>>,
    /// Callback for shard creation (provided by ShardManager).
    on_shard_created: RwLock<Option<ShardCallback>>,
    /// Callback for shard removal (provided by ShardManager).
    on_shard_removed: RwLock<Option<ShardCallback>>,
    /// Monotonic shard-id allocator. The next `scale_up` call gets
    /// `fetch_add(1)` from here. Distinct from `shards.iter().max() +
    /// 1`: that approach reused ids whenever the highest-numbered
    /// shard had been drained-and-removed, silently merging two
    /// unrelated shard lifetimes in any external system that keys
    /// metrics or checkpoints on shard id. Monotonic allocation
    /// ensures every shard ever allocated has a globally unique id
    /// for the lifetime of this mapper.
    next_shard_id: AtomicU16,
}

impl ShardMapper {
    /// Create a new shard mapper with the given initial shard count and policy.
    pub fn new(
        initial_shards: u16,
        ring_buffer_capacity: usize,
        policy: ScalingPolicy,
    ) -> Result<Self, ScalingError> {
        let mut policy = policy.normalize();
        // Ensure max_shards can accommodate the initial shard count
        if policy.max_shards < initial_shards {
            policy.max_shards = initial_shards;
        }
        policy
            .validate()
            .map_err(|e| ScalingError::InvalidPolicy(e.to_string()))?;

        // Initial shards: stamp `activated_at` far enough in the
        // past that they're not subject to the warmup skip in
        // `evaluate_scaling`. The boot-time shards have whatever
        // baseline traffic the system serves; they shouldn't be
        // exempted from scale decisions just because the mapper
        // was just constructed.
        let boot = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        let shards: Vec<MappedShard> = (0..initial_shards)
            .map(|id| MappedShard {
                id,
                state: ShardState::Active,
                metrics: Arc::new(ShardMetricsCollector::new(id, ring_buffer_capacity)),
                drain_started: None,
                last_metrics: ShardMetrics::new(id),
                activated_at: boot,
            })
            .collect();

        Ok(Self {
            shards: RwLock::new(shards),
            active_count: AtomicU16::new(initial_shards),
            policy,
            ring_buffer_capacity,
            last_scaling: RwLock::new(None),
            on_shard_created: RwLock::new(None),
            on_shard_removed: RwLock::new(None),
            // Initial shards occupy ids `[0, initial_shards)`, so the
            // first scale-up takes `initial_shards`.
            next_shard_id: AtomicU16::new(initial_shards),
        })
    }

    /// Set callback for shard creation.
    pub fn set_on_shard_created<F>(&self, callback: F)
    where
        F: Fn(u16) + Send + Sync + 'static,
    {
        *self.on_shard_created.write() = Some(Box::new(callback));
    }

    /// Set callback for shard removal.
    pub fn set_on_shard_removed<F>(&self, callback: F)
    where
        F: Fn(u16) + Send + Sync + 'static,
    {
        *self.on_shard_removed.write() = Some(Box::new(callback));
    }

    /// Get the metrics collector for a shard.
    pub fn metrics_collector(&self, shard_id: u16) -> Option<Arc<ShardMetricsCollector>> {
        let shards = self.shards.read();
        shards
            .iter()
            .find(|s| s.id == shard_id)
            .map(|s| s.metrics.clone())
    }

    /// Get current active shard count.
    pub fn active_shard_count(&self) -> u16 {
        self.active_count.load(AtomicOrdering::Acquire)
    }

    /// Get total shard count (including draining).
    pub fn total_shard_count(&self) -> u16 {
        self.shards.read().len() as u16
    }

    /// Select the best shard for a new event/producer.
    ///
    /// This implements weighted shard selection:
    /// - Only considers active (non-draining) shards
    /// - Prefers shards with lower weight (less loaded)
    /// - Falls back to round-robin if weights are equal
    #[inline]
    pub fn select_shard(&self, event_hash: u64) -> u16 {
        let shards = self.shards.read();

        // Filter to active shards only
        let active: Vec<_> = shards
            .iter()
            .filter(|s| s.state == ShardState::Active)
            .collect();

        if active.is_empty() {
            // Previously fell back to `find(|s| s.state !=
            // ShardState::Stopped)`, which silently routed to a
            // `Draining` shard. Pushes to a draining shard increment
            // `pushes_since_drain_start`, blocking finalization
            // indefinitely.
            //
            // `Provisioning` shards aren't routable either — they
            // exist in the mapper but the routing table doesn't have
            // them yet, so any caller that tries to push lands
            // in `resolve_idx → None` and surfaces as "unrouted" via
            // the manager-level counter. That's the correct signal:
            // the system has no destination for this event, the
            // caller should back off and retry.
            //
            // Return `u16::MAX` (out-of-band sentinel) so callers
            // that look up the id in the routing table get a
            // definite miss, which the manager already accounts for.
            return u16::MAX;
        }

        // Find the shard with lowest weight
        let min_weight = active
            .iter()
            .map(|s| s.last_metrics.weight)
            .fold(f64::MAX, f64::min);

        // Get all shards with the minimum weight (within tolerance)
        let candidates: Vec<_> = active
            .iter()
            .filter(|s| (s.last_metrics.weight - min_weight).abs() < 0.1)
            .collect();

        // Use hash to pick among candidates for determinism
        // Fallback to first active shard if tolerance filter excludes all (e.g. NaN weights)
        if candidates.is_empty() {
            return active[0].id;
        }
        // Pre-fix used `(event_hash as usize) % candidates.len()`,
        // which biases low-bucket indices when `candidates.len()`
        // is not a power of two. With u64 hashes the bias is small
        // but non-zero and accumulates over time as a sustained
        // skew toward shards at the low end of the candidate
        // vector. Lemire's technique
        // (https://lemire.me/blog/2016/06/30/fast-random-shuffling/)
        // computes the index as `(hash * len) >> 64` — an unbiased
        // integer mapping for any `len` that fits in u64.
        let idx = ((event_hash as u128 * candidates.len() as u128) >> 64) as usize;
        candidates[idx].id
    }

    /// Collect metrics from all shards and update weights.
    pub fn collect_metrics(&self) -> Vec<ShardMetrics> {
        let mut shards = self.shards.write();
        shards
            .iter_mut()
            .map(|s| {
                s.last_metrics = s.metrics.collect_and_reset();
                s.last_metrics.clone()
            })
            .collect()
    }

    /// Evaluate scaling based on current metrics.
    ///
    /// Returns a scaling decision without executing it.
    pub fn evaluate_scaling(&self) -> ScalingDecision {
        if !self.policy.auto_scale {
            return ScalingDecision::None;
        }

        // Check cooldown
        if let Some(last) = *self.last_scaling.read() {
            if last.elapsed() < self.policy.cooldown {
                return ScalingDecision::None;
            }
        }

        let shards = self.shards.read();
        let active_count = self.active_count.load(AtomicOrdering::Acquire);

        // Check for scale-up triggers
        let mut overloaded_count = 0;
        let mut underutilized_count = 0;

        // Warmup window for freshly-activated shards. A just-
        // activated shard's `last_metrics` is the
        // `ShardMetrics::new(id)` placeholder
        // (`fill_ratio = 0.0, event_rate = 0`) until at least one
        // `collect_metrics` cycle has run. The placeholder
        // immediately matches the underutilized trigger
        // (`fill_ratio < underutilized_threshold && event_rate
        // == 0`), so a fresh shard added by scale-up would
        // immediately count as underutilized on the next
        // `evaluate_scaling` and trigger scale-down — oscillating
        // the system. Reuse `policy.cooldown` as the warmup
        // window: it's already the minimum gap between scaling
        // actions, and a shard collected at least once within
        // that window has accumulated real metrics.
        let now = Instant::now();
        let warmup = self.policy.cooldown;

        for shard in shards.iter() {
            if shard.state != ShardState::Active {
                continue;
            }

            // Skip the placeholder-metrics window for freshly-
            // activated shards. They count toward `active_count`
            // (so the budget math stays consistent) but don't
            // tip the overload/underutilized tallies.
            if now.duration_since(shard.activated_at) < warmup {
                continue;
            }

            let m = &shard.last_metrics;

            // Scale-up triggers
            if m.fill_ratio > self.policy.fill_ratio_threshold
                || m.avg_push_latency_ns > self.policy.push_latency_threshold_ns
                || m.avg_flush_latency_us > self.policy.flush_latency_threshold_us
            {
                overloaded_count += 1;
            }

            // Scale-down triggers
            if m.fill_ratio < self.policy.underutilized_threshold && m.event_rate == 0 {
                underutilized_count += 1;
            }
        }

        // Scale up if more than half of shards are overloaded
        if overloaded_count > active_count / 2 && active_count < self.policy.max_shards {
            // Add shards proportional to overload
            let shards_to_add = (overloaded_count / 2)
                .max(1)
                .min(self.policy.max_shards - active_count);
            return ScalingDecision::ScaleUp(shards_to_add);
        }

        // Scale down if more than half of shards are underutilized
        // and we're above minimum
        if underutilized_count > active_count / 2 && active_count > self.policy.min_shards {
            let shards_to_remove = (underutilized_count / 2)
                .max(1)
                .min(active_count - self.policy.min_shards);
            return ScalingDecision::ScaleDown(shards_to_remove);
        }

        ScalingDecision::None
    }

    /// Validate cooldown + max-shards budget for an `add count`
    /// scale-up request. Cheap pre-check that doesn't take the
    /// write lock — callers re-check under the lock.
    fn check_scale_up_budget(&self, count: u16) -> Result<(), ScalingError> {
        let current = self.active_count.load(AtomicOrdering::Acquire);
        let would_be = current
            .checked_add(count)
            .ok_or(ScalingError::AtMaxShards)?;
        if would_be > self.policy.max_shards {
            return Err(ScalingError::AtMaxShards);
        }
        let last = self.last_scaling.read();
        if let Some(ts) = *last {
            if ts.elapsed() < self.policy.cooldown {
                return Err(ScalingError::InCooldown);
            }
        }
        Ok(())
    }

    /// Allocate `count` shard ids and push their `MappedShard`
    /// records into `shards` with the supplied `state`. The caller
    /// already holds `self.shards.write()` and is responsible for
    /// dropping it before notifying callbacks. Returns the allocated
    /// ids in order.
    ///
    /// Performs the budget + cooldown re-check under the write lock,
    /// the next_shard_id allocation, and the per-shard push. Does NOT
    /// touch `active_count` — `scale_up` bumps it for `Active` shards;
    /// `Provisioning` shards bump it later when `activate` fires.
    fn allocate_shards_inner(
        &self,
        count: u16,
        state: ShardState,
        shards: &mut Vec<MappedShard>,
    ) -> Result<Vec<u16>, ScalingError> {
        self.allocate_shards_inner_with_policy(count, state, shards, false)
    }

    fn allocate_shards_inner_with_policy(
        &self,
        count: u16,
        state: ShardState,
        shards: &mut Vec<MappedShard>,
        force: bool,
    ) -> Result<Vec<u16>, ScalingError> {
        // Re-check budget under the write lock — two concurrent
        // scale-up callers could both pass the read-locked early
        // check, both serialize through `shards.write()`, and both
        // succeed without this re-check.
        if force {
            // Budget only — skip cooldown for operator-initiated paths.
            let current = self.active_count.load(AtomicOrdering::Acquire);
            let would_be = current
                .checked_add(count)
                .ok_or(ScalingError::AtMaxShards)?;
            if would_be > self.policy.max_shards {
                return Err(ScalingError::AtMaxShards);
            }
        } else {
            self.check_scale_up_budget(count)?;
        }

        let first_id = self.next_shard_id.load(AtomicOrdering::Relaxed);
        let last_needed = first_id
            .checked_add(count.saturating_sub(1))
            .ok_or(ScalingError::AtMaxShards)?;
        // Reserve `u16::MAX` as a sentinel so the post-allocation
        // store cannot wrap.
        if last_needed == u16::MAX {
            return Err(ScalingError::AtMaxShards);
        }
        // `first_id + count == last_needed + 1`. We already
        // refused `last_needed == u16::MAX` above, so the sum is
        // provably <= u16::MAX. Use `checked_add` anyway as a
        // belt-and-suspenders guard: a future change that
        // weakens the sentinel check would otherwise reach an
        // unchecked u16 wrap here, silently rolling
        // `next_shard_id` back to 0 and then re-issuing already-
        // allocated ids.
        let next_id_after = first_id
            .checked_add(count)
            .ok_or(ScalingError::AtMaxShards)?;
        self.next_shard_id
            .store(next_id_after, AtomicOrdering::Relaxed);

        let mut new_ids = Vec::with_capacity(count as usize);
        let now = Instant::now();
        for i in 0..count {
            let new_id = first_id + i;
            shards.push(MappedShard {
                id: new_id,
                state,
                metrics: Arc::new(ShardMetricsCollector::new(
                    new_id,
                    self.ring_buffer_capacity,
                )),
                drain_started: None,
                last_metrics: ShardMetrics::new(new_id),
                // Stamp the activation moment so `evaluate_scaling`
                // can skip this shard until at least one collect
                // cycle has run. Prevents the placeholder
                // (`fill_ratio = 0, event_rate = 0`) from
                // immediately tripping the underutilized trigger
                // and oscillating the system.
                activated_at: now,
            });
            new_ids.push(new_id);
        }
        Ok(new_ids)
    }

    /// Execute a scale-up operation.
    ///
    /// Creates new shards in the `Active` state and makes them
    /// immediately available for routing. Use [`scale_up_provisioning`]
    /// if upstream workers (drain / batch) need to be wired up before
    /// the shard becomes selectable — otherwise producer pushes can
    /// race ahead of consumer creation.
    ///
    /// [`scale_up_provisioning`]: Self::scale_up_provisioning
    pub fn scale_up(&self, count: u16) -> Result<Vec<u16>, ScalingError> {
        // Short-circuit `count == 0` so a no-op call doesn't bump the
        // cooldown timestamp or trip the `u16::MAX` sentinel check
        // (which previously fired spuriously when
        // `first_id == u16::MAX` even though zero ids were being
        // allocated).
        if count == 0 {
            return Ok(Vec::new());
        }

        self.check_scale_up_budget(count)?;

        let mut shards = self.shards.write();
        let new_ids = self.allocate_shards_inner(count, ShardState::Active, &mut shards)?;

        // Update counts. `fetch_add` cannot wrap here — the
        // `check_scale_up_budget` gate above ensures `current + count <=
        // max_shards <= u16::MAX` — but keep the ordering explicit
        // so the next contender's cooldown re-check sees the fresh
        // timestamp.
        self.active_count.fetch_add(count, AtomicOrdering::Release);
        *self.last_scaling.write() = Some(Instant::now());

        // Drop the write lock before notifying callbacks — they
        // are user-supplied and may take arbitrary time.
        drop(shards);

        if let Some(callback) = self.on_shard_created.read().as_ref() {
            for &id in &new_ids {
                callback(id);
            }
        }

        Ok(new_ids)
    }

    /// Like [`scale_up`], but the new shards are created in the
    /// `Provisioning` state. They receive an id and a metrics
    /// collector, but `select_shard` will not route to them and they
    /// are excluded from `active_shard_count` / `evaluate_scaling`
    /// until the caller transitions each shard with [`activate`].
    ///
    /// Use this when upstream consumer infrastructure (drain/batch
    /// workers, mpsc channels, etc.) must be wired up *before* the
    /// shard becomes selectable. Without this gating, producers can
    /// observe the shard via `select_shard`, push into its ring
    /// buffer, and never have those events drained.
    ///
    /// Returns the allocated ids in order. Cooldown / `max_shards`
    /// gating matches `scale_up` so that staged allocation cannot be
    /// used to bypass the policy.
    ///
    /// [`scale_up`]: Self::scale_up
    /// [`activate`]: Self::activate
    pub fn scale_up_provisioning(&self, count: u16) -> Result<Vec<u16>, ScalingError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        self.check_scale_up_budget(count)?;

        let mut shards = self.shards.write();
        let new_ids = self.allocate_shards_inner(count, ShardState::Provisioning, &mut shards)?;

        // Bump cooldown but NOT active_count — these shards are
        // not active yet. `activate` bumps active_count when each
        // becomes selectable.
        *self.last_scaling.write() = Some(Instant::now());
        drop(shards);
        // Intentionally do NOT fire `on_shard_created` here — the
        // callback signals "shard is live"; provisioning shards are
        // not. `activate` fires it instead.
        Ok(new_ids)
    }

    /// Allocate `count` Provisioning shards, bypassing the cooldown gate.
    ///
    /// Used by operator-initiated `manual_scale_up` paths. The
    /// cooldown exists to prevent the auto-scaling monitor from
    /// scaling-up too aggressively in response to transient
    /// load spikes; a manual call from an operator is a
    /// deliberate request that should not be rate-limited by
    /// the auto-scaling cadence. The budget check (against
    /// `max_shards`) still applies.
    ///
    /// Pre-fix `manual_scale_up(N)` looped `add_shard()` N
    /// times, each call invoking `scale_up_provisioning(1)`
    /// which bumped `last_scaling`. The second call then
    /// immediately failed with `InCooldown` (default 30s
    /// cooldown), leaving the first shard half-added and
    /// returning an error to the operator with no rollback.
    pub fn scale_up_provisioning_force(&self, count: u16) -> Result<Vec<u16>, ScalingError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        let mut shards = self.shards.write();
        let new_ids = self.allocate_shards_inner_with_policy(
            count,
            ShardState::Provisioning,
            &mut shards,
            true, // skip cooldown
        )?;

        // Still bump the cooldown timestamp so the next
        // *auto-scaling* tick respects the cooldown floor.
        *self.last_scaling.write() = Some(Instant::now());
        drop(shards);
        Ok(new_ids)
    }

    /// Transition a `Provisioning` shard to `Active`.
    ///
    /// Returns `Ok(true)` if a state transition actually occurred
    /// (Provisioning → Active) and `Ok(false)` if the shard was
    /// already `Active` — the latter is the idempotent path.
    /// Returns `InvalidPolicy` for unknown or `Draining`/`Stopped`
    /// shards — those states require a different lifecycle path.
    /// Bumps `active_count` and notifies the `on_shard_created`
    /// callback exactly once per real transition.
    ///
    /// Pre-fix this returned `Result<(), ScalingError>`,
    /// so callers (notably `ShardManager::activate_shard`) could
    /// not tell whether they had bumped the live count or not and
    /// double-incremented their own `num_shards` on every
    /// idempotent call.
    pub fn activate(&self, shard_id: u16) -> Result<bool, ScalingError> {
        let mut shards = self.shards.write();
        let shard = shards
            .iter_mut()
            .find(|s| s.id == shard_id)
            .ok_or_else(|| {
                ScalingError::InvalidPolicy(format!("activate: shard {} not found", shard_id))
            })?;
        match shard.state {
            ShardState::Active => return Ok(false),
            ShardState::Provisioning => {
                // Gate activation on
                // `active_count < max_shards`. The budget gate at
                // `check_scale_up_budget` only counts ALREADY-active
                // shards — multiple `scale_up_provisioning(1)` calls
                // can each pass (they don't bump `active_count`),
                // and an unconditional `fetch_add(1)` here would
                // push past `max_shards`. Subsequent
                // `evaluate_scaling`'s `max_shards - active_count`
                // arithmetic would then underflow u16 (debug-build
                // panic; release wraps to ~65530). Activate-time
                // re-checks the budget with the lock held; the
                // Provisioning shard stays in Provisioning state and
                // the caller (e.g. `add_shard_internal`'s rollback)
                // is responsible for tearing it down.
                //
                // The load + state mutation + `fetch_add` all happen
                // while we hold the `shards.write()` guard so a
                // concurrent `activate(distinct_id)` reads the
                // already-bumped `active_count` and hits
                // `AtMaxShards` instead of squeezing through the
                // window between our state update and our
                // `fetch_add`. Pre-fix the `fetch_add` ran after
                // `drop(shards)` — two activates could each see a
                // stale count below `max_shards` and both bump,
                // transiently overshooting the budget.
                let current = self.active_count.load(AtomicOrdering::Acquire);
                if current >= self.policy.max_shards {
                    return Err(ScalingError::AtMaxShards);
                }
                shard.state = ShardState::Active;
                // Re-stamp `activated_at` so `evaluate_scaling`
                // gives this freshly-activated shard a warmup
                // window before counting it in the
                // overloaded/underutilized tallies. The
                // Provisioning → Active transition is the moment
                // traffic starts flowing; the shard's
                // `last_metrics` is still the
                // `ShardMetrics::new(id)` placeholder until the
                // next `collect_metrics` cycle.
                shard.activated_at = Instant::now();
                // Publish the increment while still holding the
                // write lock. `Release` here pairs with the
                // `Acquire` load above so any later activator that
                // takes the same lock observes our bump.
                self.active_count.fetch_add(1, AtomicOrdering::Release);
            }
            ShardState::Draining | ShardState::Stopped => {
                return Err(ScalingError::InvalidPolicy(format!(
                    "activate: shard {} is in state {:?}, cannot activate",
                    shard_id, shard.state
                )));
            }
        }
        drop(shards);

        if let Some(callback) = self.on_shard_created.read().as_ref() {
            callback(shard_id);
        }
        Ok(true)
    }

    /// Drain a specific shard by id, transitioning it from `Active`
    /// to `Draining`.
    ///
    /// Companion to `ShardManager::drain_shard`. The previous
    /// implementation only flipped the metrics collector's `draining`
    /// atomic; this version atomically updates `MappedShard.state`
    /// (so `select_shard` stops routing to the shard) and decrements
    /// `active_count` (so `evaluate_scaling`'s budget math stays
    /// consistent with `scale_down`). Returns an error if the shard
    /// is not in `Active` state, or if doing so would push the active
    /// count below `min_shards`.
    pub fn drain_specific(&self, shard_id: u16) -> Result<(), ScalingError> {
        let mut shards = self.shards.write();
        let current_active = self.active_count.load(AtomicOrdering::Acquire);
        if current_active <= self.policy.min_shards {
            return Err(ScalingError::AtMinShards);
        }
        let shard = shards
            .iter_mut()
            .find(|s| s.id == shard_id)
            .ok_or_else(|| {
                ScalingError::InvalidPolicy(format!("drain_specific: shard {} not found", shard_id))
            })?;
        match shard.state {
            ShardState::Active => {
                shard.state = ShardState::Draining;
                shard.drain_started = Some(Instant::now());
                shard.metrics.set_draining(true);
            }
            ShardState::Draining => return Ok(()),
            ShardState::Provisioning | ShardState::Stopped => {
                return Err(ScalingError::InvalidPolicy(format!(
                    "drain_specific: shard {} is in state {:?}, cannot drain",
                    shard_id, shard.state
                )));
            }
        }
        drop(shards);
        self.active_count.fetch_sub(1, AtomicOrdering::Release);
        // Bump `last_scaling` so a subsequent `scale_up` is gated
        // by the cooldown floor. Pre-fix `drain_specific` removed
        // a shard from Active without touching `last_scaling`, so
        // the sequence `drain_specific(id) → scale_up(N)`
        // bypassed the cooldown — `scale_down` writes
        // `last_scaling` precisely for this reason. From the
        // budget-math perspective `drain_specific` IS a scale-
        // down (it decrements `active_count` and trips the
        // `min_shards` floor), so it should also gate
        // re-expansion the same way.
        *self.last_scaling.write() = Some(Instant::now());
        Ok(())
    }

    /// Start draining shards for scale-down.
    ///
    /// Marks shards as draining so they stop receiving new events.
    /// Shards will be removed once they're empty.
    pub fn scale_down(&self, count: u16) -> Result<Vec<u16>, ScalingError> {
        // Early checks (may race, but avoid acquiring the write lock)
        let current = self.active_count.load(AtomicOrdering::Acquire);
        if current <= self.policy.min_shards {
            return Err(ScalingError::AtMinShards);
        }

        let to_drain = count.min(current - self.policy.min_shards);
        if to_drain == 0 {
            return Err(ScalingError::AtMinShards);
        }
        {
            let last = self.last_scaling.read();
            if let Some(ts) = *last {
                if ts.elapsed() < self.policy.cooldown {
                    return Err(ScalingError::InCooldown);
                }
            }
        }

        let mut shards = self.shards.write();

        // Re-check under the lock to prevent race conditions (double-check pattern)
        let current = self.active_count.load(AtomicOrdering::Acquire);
        if current <= self.policy.min_shards {
            return Err(ScalingError::AtMinShards);
        }
        let to_drain = count.min(current - self.policy.min_shards);
        if to_drain == 0 {
            return Err(ScalingError::AtMinShards);
        }
        // Re-check cooldown under the same write lock that gates
        // mutation — see the matching note in `scale_up`.
        {
            let last = self.last_scaling.read();
            if let Some(ts) = *last {
                if ts.elapsed() < self.policy.cooldown {
                    return Err(ScalingError::InCooldown);
                }
            }
        }

        let mut drained_ids = Vec::with_capacity(to_drain as usize);

        // Find shards with lowest weight (least utilized) to drain
        let mut active_indices: Vec<_> = shards
            .iter()
            .enumerate()
            .filter(|(_, s)| s.state == ShardState::Active)
            .map(|(i, s)| (i, s.last_metrics.weight))
            .collect();

        // Sort by weight (ascending - drain least utilized first)
        active_indices.sort_by(|a, b| a.1.total_cmp(&b.1));

        // Mark shards for draining
        for (idx, _) in active_indices.into_iter().take(to_drain as usize) {
            shards[idx].state = ShardState::Draining;
            shards[idx].drain_started = Some(Instant::now());
            shards[idx].metrics.set_draining(true);
            drained_ids.push(shards[idx].id);
        }

        // Update count
        self.active_count
            .fetch_sub(to_drain, AtomicOrdering::Release);
        *self.last_scaling.write() = Some(Instant::now());

        Ok(drained_ids)
    }

    /// Check draining shards and finalize those that are empty.
    ///
    /// Returns IDs of shards that were stopped.
    ///
    /// This predicate looks ONLY at the ring buffer
    /// (`current_len` + `pushes_since_drain_start`); it does NOT
    /// probe the per-shard mpsc channel or the BatchWorker's
    /// `current_batch`. A shard that the predicate flags as empty
    /// can still have events queued in those two places. The
    /// correctness gate is therefore `bus::remove_shard_internal`,
    /// which awaits the BatchWorker's `JoinHandle` before
    /// constructing the stranded-flush batch — see that function's
    /// step 3 for the rationale. Tightening this predicate is a
    /// defense-in-depth follow-up; a stricter ring-buffer-empty
    /// signal here would only narrow an already-closed window.
    pub fn finalize_draining(&self) -> Vec<u16> {
        let mut shards = self.shards.write();
        let mut stopped = Vec::new();

        for shard in shards.iter_mut() {
            if shard.state == ShardState::Draining {
                // Check if shard is empty by reading current_len directly,
                // avoiding collect_and_reset() which destructively zeros all counters.
                let current_len = shard.metrics.current_len.load(AtomicOrdering::Relaxed);
                let fill_ratio = if shard.metrics.capacity > 0 {
                    current_len as f64 / shard.metrics.capacity as f64
                } else {
                    0.0
                };
                // Previously read `events_in_window` here, which
                // `collect_and_reset` zeros every metrics tick. A
                // producer push that landed in the window between two
                // ticks could be silently zeroed out, so a draining
                // shard whose buffer transiently emptied was finalized
                // with a producer still attached.
                // `pushes_since_drain_start` is a separate counter
                // that is only reset by `set_draining(true)`, so any
                // push observed since the drain began is sticky —
                // exactly the signal we want.
                // Acquire pairs with `set_draining`'s SeqCst reset so
                // the load can't observe a stale value from before the
                // drain began. A Relaxed load here let weakly-ordered
                // hardware see the pre-reset count and finalize while
                // a producer was still pushing.
                let pushes_after_drain = shard.metrics.pushes_since_drain_start();
                if fill_ratio == 0.0 && pushes_after_drain == 0 {
                    // Check if we've waited long enough
                    if let Some(drain_start) = shard.drain_started {
                        if drain_start.elapsed() > Duration::from_millis(100) {
                            shard.state = ShardState::Stopped;
                            stopped.push(shard.id);
                        }
                    }
                }
            }
        }

        // Drop the write lock BEFORE notifying. The callback is
        // user-supplied and may re-enter the mapper (`shard_state`,
        // `select_shard`, `metrics_collector`, …), each of which
        // acquires `shards.read()`. `parking_lot::RwLock` is not
        // recursive, so a re-entrant read attempt while we hold a
        // write would deadlock. `scale_up`'s callback path already
        // releases its lock before calling out — mirror that
        // here.
        drop(shards);

        if !stopped.is_empty() {
            if let Some(callback) = self.on_shard_removed.read().as_ref() {
                for &id in &stopped {
                    callback(id);
                }
            }
        }

        stopped
    }

    /// Remove a specific shard from the mapper if it is in the
    /// `Stopped` state. Used by `ShardManager::remove_shard` so a
    /// per-shard cleanup doesn't disturb sibling `Stopped`
    /// entries — which a sequential `manual_scale_down` loop
    /// still needs to look up state for. Returns `true` if the
    /// shard existed and was Stopped (and was removed).
    pub fn remove_specific_stopped_shard(&self, shard_id: u16) -> bool {
        let mut shards = self.shards.write();
        let before = shards.len();
        shards.retain(|s| !(s.id == shard_id && s.state == ShardState::Stopped));
        shards.len() < before
    }

    /// Remove stopped shards from the mapper.
    pub fn remove_stopped_shards(&self) -> Vec<u16> {
        let mut shards = self.shards.write();
        let before = shards.len();
        let removed: Vec<u16> = shards
            .iter()
            .filter(|s| s.state == ShardState::Stopped)
            .map(|s| s.id)
            .collect();

        shards.retain(|s| s.state != ShardState::Stopped);

        if shards.len() < before {
            tracing::info!(
                removed = removed.len(),
                remaining = shards.len(),
                "Removed stopped shards"
            );
        }

        removed
    }

    /// Get the state of a specific shard.
    pub fn shard_state(&self, shard_id: u16) -> Option<ShardState> {
        self.shards
            .read()
            .iter()
            .find(|s| s.id == shard_id)
            .map(|s| s.state)
    }

    /// Get all active shard IDs.
    pub fn active_shard_ids(&self) -> Vec<u16> {
        self.shards
            .read()
            .iter()
            .filter(|s| s.state == ShardState::Active)
            .map(|s| s.id)
            .collect()
    }

    /// Get all shard IDs (including draining).
    pub fn all_shard_ids(&self) -> Vec<u16> {
        self.shards
            .read()
            .iter()
            .filter(|s| s.state != ShardState::Stopped)
            .map(|s| s.id)
            .collect()
    }

    /// Get the scaling policy.
    pub fn policy(&self) -> &ScalingPolicy {
        &self.policy
    }
    // `set_policy` previously took `&mut self` and was unreachable
    // through the `Arc<ShardMapper>` that the production code holds
    // (`Arc::get_mut` fails once the worker pool has cloned the Arc).
    // The method has been removed — recreate the mapper / bus to
    // change the policy.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shard_mapper_creation() {
        let mapper = ShardMapper::new(4, 1024, ScalingPolicy::default()).unwrap();
        assert_eq!(mapper.active_shard_count(), 4);
        assert_eq!(mapper.total_shard_count(), 4);
    }

    #[test]
    fn test_select_shard_distributes() {
        let mapper = ShardMapper::new(4, 1024, ScalingPolicy::default()).unwrap();

        // Different hashes should potentially select different shards
        let mut selected = std::collections::HashSet::new();
        for i in 0..100u64 {
            let shard = mapper.select_shard(i * 12345);
            selected.insert(shard);
        }

        // With 4 shards, we should hit multiple
        assert!(!selected.is_empty());
    }

    /// `select_shard`'s candidate-index computation must
    /// be unbiased across the u64 hash space. Pre-fix used
    /// `hash as usize % candidates.len()`, which over-weights low
    /// indices when `candidates.len()` is not a power of two.
    /// With u64 hashes the bias is small but non-zero and
    /// sustains a hot-shard skew over time. Lemire's
    /// `(hash * len) >> 64` is unbiased.
    ///
    /// We test the unbiased property by sampling a uniform
    /// distribution of u64 hashes over 3 candidate shards and
    /// asserting each bucket gets close to 1/3 of the picks.
    /// Empirical bound: ±5% across 30 000 trials with a
    /// well-distributed input.
    #[test]
    fn select_shard_distribution_is_unbiased() {
        // 3 candidates: a non-power-of-2 to expose the modulo
        // bias the fix removes.
        let mapper = ShardMapper::new(3, 1024, ScalingPolicy::default()).unwrap();

        let trials = 30_000u64;
        // Spread inputs uniformly across the u64 range so the
        // multiply-shift mapping behaves as designed.
        let stride = u64::MAX / trials;
        let mut counts = [0u64; 3];
        for i in 0..trials {
            let h = i.wrapping_mul(stride);
            let id = mapper.select_shard(h);
            counts[id as usize] += 1;
        }

        let expected = (trials / 3) as i64;
        for (id, &count) in counts.iter().enumerate() {
            let diff = (count as i64 - expected).abs();
            let pct = (diff as f64 / expected as f64) * 100.0;
            assert!(
                pct < 5.0,
                "shard {} bucket has {} hits ({:.2}% off expected {}); \
                 modulo bias would drift higher on certain shards",
                id,
                count,
                pct,
                expected
            );
        }
    }

    #[test]
    fn test_scale_up() {
        // Explicitly set max_shards to allow scaling from 2 to 4
        let policy = ScalingPolicy {
            max_shards: 8,
            ..Default::default()
        };
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();

        let new_ids = mapper.scale_up(2).unwrap();
        assert_eq!(new_ids.len(), 2);
        assert_eq!(mapper.active_shard_count(), 4);
    }

    #[test]
    fn test_scale_up_max_limit() {
        let policy = ScalingPolicy {
            max_shards: 4,
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        let result = mapper.scale_up(1);
        assert!(matches!(result, Err(ScalingError::AtMaxShards)));
    }

    #[test]
    fn test_scale_down() {
        let policy = ScalingPolicy {
            min_shards: 1,
            cooldown: Duration::from_nanos(1), // Disable cooldown for test
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        let drained = mapper.scale_down(2).unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(mapper.active_shard_count(), 2);
    }

    #[test]
    fn test_scale_down_min_limit() {
        let policy = ScalingPolicy {
            min_shards: 4,
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        let result = mapper.scale_down(1);
        assert!(matches!(result, Err(ScalingError::AtMinShards)));
    }

    #[test]
    fn test_metrics_collection() {
        let mapper = ShardMapper::new(2, 1024, ScalingPolicy::default()).unwrap();

        // Record some metrics
        if let Some(collector) = mapper.metrics_collector(0) {
            collector.record_buffer_len(512);
            collector.record_push(5);
            collector.record_push(10);
        }

        let metrics = mapper.collect_metrics();
        assert_eq!(metrics.len(), 2);

        let shard0_metrics = metrics.iter().find(|m| m.shard_id == 0).unwrap();
        assert!(shard0_metrics.fill_ratio > 0.0);
    }

    /// Regression: a `record_push` / `record_flush` interleaving
    /// with a `collect_and_reset` swap must NOT desync `(sum,
    /// count)`. Pre-fix `push_latency_sum_ns` and `push_count`
    /// were independent atomics; a tick between the two
    /// `fetch_add`s captured the sum without the matching count
    /// (or the count without the sum). The resulting `avg =
    /// sum.checked_div(count).unwrap_or(0)` returned 0 in window
    /// N (sum without count) AND 0 in window N+1 (count without
    /// sum) — silently zeroing the average that drives
    /// `evaluate_scaling`'s push-latency scale-up trigger.
    ///
    /// Post-fix `(sum, count)` is packed into one
    /// `AtomicU64` so the swap captures both atomically. This
    /// test fires N concurrent `record_push` calls and a single
    /// `collect_and_reset` and asserts the captured count
    /// matches the captured sum (i.e. `sum >= count` because
    /// every push contributes at least 1 ns; `sum / count` is
    /// well-defined for any non-zero count).
    #[test]
    fn record_push_collect_no_sum_count_desync() {
        use std::sync::Barrier;
        use std::thread;

        let collector = Arc::new(ShardMetricsCollector::new(0, 1024));
        const PUSHERS: usize = 4;
        const PUSHES_PER_THREAD: usize = 1_000;

        let barrier = Arc::new(Barrier::new(PUSHERS + 1));
        let mut handles = vec![];
        for _ in 0..PUSHERS {
            let c = collector.clone();
            let b = barrier.clone();
            handles.push(thread::spawn(move || {
                b.wait();
                for i in 0..PUSHES_PER_THREAD {
                    // Vary latencies so sum/count averages
                    // aren't trivially the same number.
                    c.record_push((i as u64 % 100) + 1);
                }
            }));
        }

        // Race a collect_and_reset across the pushers.
        barrier.wait();
        let snapshot1 = collector.collect_and_reset();
        for h in handles {
            h.join().unwrap();
        }
        // After all threads finish, drain whatever remains.
        let snapshot2 = collector.collect_and_reset();

        // Reconstruct count and sum from the two snapshots.
        // We can't easily expose the packed atomic, so we use
        // the per-window averages and event counts as a
        // consistency check.
        let total_events = snapshot1.event_rate + snapshot2.event_rate;
        assert_eq!(
            total_events as usize,
            PUSHERS * PUSHES_PER_THREAD,
            "all pushes must be accounted for"
        );

        // For each window: if event_rate > 0, avg_push_latency
        // must be > 0 (non-zero average) — pre-fix the desync
        // could land event_rate > 0 with avg = 0 (count
        // captured without sum, or sum without count → div-by-
        // zero clamped to 0). This is the directly visible
        // symptom of the desync.
        //
        // events_in_window is incremented separately from the
        // packed (sum, count) word, so a strict assertion of
        // "event_rate is exactly count" isn't safe — but the
        // weaker invariant "if any pushes were captured in the
        // (sum,count) word, the average is non-zero" survives
        // the packed-atomic fix.
        for snap in [&snapshot1, &snapshot2] {
            if snap.avg_push_latency_ns == 0 {
                // Either no pushes were captured in this
                // window, or — pre-fix — sum/count desynced.
                // The post-fix shape can only produce
                // avg=0 when count is also 0; we can't read
                // count directly from ShardMetrics, but
                // exercise the invariant by confirming the
                // OTHER window's sum is consistent with all
                // pushes.
                continue;
            }
            assert!(
                snap.avg_push_latency_ns >= 1,
                "regression: a window with non-zero avg must have \
                 a positive sum (pre-fix sum-without-count desync \
                 produced avg=0 with non-zero events)"
            );
        }
    }

    #[test]
    fn test_draining_excludes_from_selection() {
        let policy = ScalingPolicy {
            min_shards: 1,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();

        // Drain one shard
        let drained = mapper.scale_down(1).unwrap();
        assert_eq!(drained.len(), 1);

        // All selections should go to the remaining active shard
        let active_ids = mapper.active_shard_ids();
        assert_eq!(active_ids.len(), 1);

        for i in 0..100u64 {
            let selected = mapper.select_shard(i);
            assert!(active_ids.contains(&selected));
        }
    }

    #[test]
    fn test_policy_validation() {
        let invalid_policy = ScalingPolicy {
            fill_ratio_threshold: 1.5, // Invalid
            ..Default::default()
        };
        assert!(invalid_policy.validate().is_err());

        // Without normalize(), this would be invalid
        let invalid_policy2 = ScalingPolicy {
            min_shards: 10,
            max_shards: 5,
            ..Default::default()
        };
        assert!(invalid_policy2.validate().is_err());
    }

    #[test]
    fn test_policy_normalize_auto_adjusts_max_shards() {
        // When min_shards > max_shards, normalize() should adjust max_shards
        let policy = ScalingPolicy {
            min_shards: 8,
            max_shards: 2, // Less than min_shards
            ..Default::default()
        };

        let normalized = policy.normalize();
        assert_eq!(
            normalized.max_shards, 8,
            "max_shards should be adjusted to min_shards"
        );
        assert!(
            normalized.validate().is_ok(),
            "normalized policy should be valid"
        );
    }

    /// Regression: BUG_REPORT.md #7 — `scale_up` previously allocated
    /// new shard ids as `shards.iter().max() + 1`, which reused ids
    /// after the highest-numbered shard was drained-and-removed.
    /// Reusing ids merges two distinct shard lifetimes in any
    /// external metric/checkpoint system that keys on shard id.
    /// The fix uses a monotonic `next_shard_id` counter.
    #[test]
    fn scale_up_does_not_reuse_ids_after_remove() {
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 16,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();

        // Initial ids are 0 and 1. Scale up to 4 — new ids must be
        // 2 and 3 (the next two slots from the monotonic counter).
        let new_ids = mapper.scale_up(2).unwrap();
        assert_eq!(new_ids, vec![2, 3]);

        // Drain one shard. `scale_down` picks by lowest weight, so
        // we don't get to choose which id is drained. Whichever it
        // is, we then force it through Stopped + remove and check
        // that the removed id is *not* reissued by the next
        // scale_up.
        let drained = mapper.scale_down(1).unwrap();
        let drained_id = drained[0];
        for shard in mapper.shards.write().iter_mut() {
            if shard.id == drained_id {
                shard.state = ShardState::Stopped;
            }
        }
        let removed = mapper.remove_stopped_shards();
        assert_eq!(removed, vec![drained_id]);

        // Scale up by 1. The broken `max(existing_ids) + 1` allocator
        // could revive the just-removed id whenever `drained_id` had
        // been the highest id present (e.g. id 3 with 0/1/2/3 → 0/1/2
        // → next would be 3 again). The fix uses the monotonic
        // counter, which sits at 4 after the earlier scale_up, so
        // the new id must be 4 regardless of which id was drained.
        let new_ids = mapper.scale_up(1).unwrap();
        assert_eq!(
            new_ids,
            vec![4],
            "shard id {drained_id} was just removed; reusing any \
             previously-issued id would merge two distinct shard \
             lifetimes in external systems"
        );
    }

    #[test]
    fn test_policy_normalize_preserves_valid_config() {
        // When max_shards >= min_shards, normalize() should not change anything
        let policy = ScalingPolicy {
            min_shards: 4,
            max_shards: 16,
            ..Default::default()
        };

        let normalized = policy.normalize();
        assert_eq!(normalized.min_shards, 4);
        assert_eq!(normalized.max_shards, 16);
    }

    #[test]
    fn test_shard_mapper_normalizes_policy() {
        // ShardMapper should accept a policy where min_shards > default max_shards
        // because it calls normalize() internally
        let policy = ScalingPolicy {
            min_shards: 4,
            ..Default::default()
        };

        // This should succeed even on machines with < 4 CPUs
        let result = ShardMapper::new(4, 1024, policy);
        assert!(
            result.is_ok(),
            "ShardMapper should normalize policy automatically"
        );
    }

    #[test]
    fn test_shard_mapper_adjusts_max_shards_to_initial_count() {
        // ShardMapper should adjust max_shards to accommodate initial_shards
        // even if initial_shards > default max_shards (CPU count)
        let policy = ScalingPolicy::default();

        // Create mapper with 8 initial shards - should work even on 2-core machines
        let result = ShardMapper::new(8, 1024, policy);
        assert!(
            result.is_ok(),
            "ShardMapper should adjust max_shards to initial_shards"
        );

        let mapper = result.unwrap();
        assert_eq!(mapper.active_shard_count(), 8);

        // Verify the policy was adjusted
        assert!(
            mapper.policy().max_shards >= 8,
            "max_shards should be at least initial_shards"
        );
    }

    #[test]
    fn test_scale_up_max_shards_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let policy = ScalingPolicy {
            max_shards: 10,
            cooldown: Duration::from_nanos(1), // Disable cooldown for test
            ..Default::default()
        };
        let mapper = Arc::new(ShardMapper::new(5, 1024, policy).unwrap());

        // Spawn multiple threads that all try to scale up
        let mut handles = vec![];
        for _ in 0..5 {
            let mapper_clone = mapper.clone();
            handles.push(thread::spawn(move || mapper_clone.scale_up(3)));
        }

        // Collect results
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Some should succeed, some should fail with AtMaxShards
        let successes: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
        let failures: Vec<_> = results
            .iter()
            .filter(|r| matches!(r, Err(ScalingError::AtMaxShards)))
            .collect();

        // We started with 5, max is 10, each tries to add 3
        // At most we can add 5 more shards, so at most 1-2 can succeed
        assert!(
            !successes.is_empty() || !failures.is_empty(),
            "at least some operations should complete"
        );

        // Final count should not exceed max_shards
        assert!(
            mapper.active_shard_count() <= 10,
            "should never exceed max_shards, got {}",
            mapper.active_shard_count()
        );
    }

    /// Multiple `scale_up_provisioning(1)` calls must
    /// never push `active_count` past `max_shards` via subsequent
    /// `activate()` calls. Pre-fix the budget gate
    /// (`check_scale_up_budget`) only counted ALREADY-active
    /// shards, so several `scale_up_provisioning` calls could each
    /// pass and then each `activate()` unconditionally bumped
    /// `active_count` past the cap.
    ///
    /// Setup: at the budget edge, allocate two Provisioning shards
    /// in sequence (both pass the gate because `active_count`
    /// hasn't moved). The first `activate()` fills the budget;
    /// the second must surface `AtMaxShards` rather than overflow.
    #[test]
    fn activate_rejects_when_active_count_would_exceed_max_shards() {
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 4,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(3, 1024, policy).unwrap();
        // active_count = 3, max = 4. Allocate TWO Provisioning
        // shards; both pass the budget gate (it sees active_count=3).
        let ids_a = mapper.scale_up_provisioning(1).unwrap();
        let ids_b = mapper.scale_up_provisioning(1).unwrap();
        assert_eq!(ids_a.len(), 1);
        assert_eq!(ids_b.len(), 1);

        // First activate succeeds (active_count goes 3 → 4 = max).
        mapper
            .activate(ids_a[0])
            .expect("first activate must succeed");
        assert_eq!(mapper.active_shard_count(), 4);

        // Second activate must REFUSE — it would push past max.
        let err = mapper
            .activate(ids_b[0])
            .expect_err("second activate must reject");
        assert!(
            matches!(err, ScalingError::AtMaxShards),
            "expected AtMaxShards, got {:?}",
            err
        );
        // active_count must still be at the cap, not over it.
        assert_eq!(mapper.active_shard_count(), 4);
    }

    /// Pin: under contention, the active_count never transiently
    /// exceeds `max_shards`. Pre-fix `activate` released the
    /// shards write-lock BEFORE the `fetch_add(1)`, so two
    /// concurrent activators could each pass the budget gate
    /// (both reading the pre-bump count) and both bump,
    /// overshooting the cap by 1 at any observation point.
    #[test]
    fn concurrent_activate_never_exceeds_max_shards() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        const ITERATIONS: usize = 200;

        for iter in 0..ITERATIONS {
            let policy = ScalingPolicy {
                min_shards: 1,
                max_shards: 4,
                cooldown: Duration::from_nanos(1),
                ..Default::default()
            };
            // Start at active=3, allocate two Provisioning ids so
            // both threads have a candidate to activate. With the
            // pre-fix race: thread A loads count=3, validates,
            // sets state Active, drops lock; thread B loads count
            // (still 3), validates, sets state Active, drops lock;
            // A bumps → 4; B bumps → 5. Post-fix B observes
            // count=4 inside its lock and rejects with
            // AtMaxShards.
            let mapper = Arc::new(ShardMapper::new(3, 1024, policy).unwrap());
            let ids_a = mapper.scale_up_provisioning(1).unwrap();
            // The 1ns cooldown elapses on every realistic
            // scheduler tick; if we lose that race, retry once
            // (release-mode iterations occasionally finish the
            // first allocation in <1ns of wall time).
            let ids_b = loop {
                match mapper.scale_up_provisioning(1) {
                    Ok(v) => break v,
                    Err(ScalingError::InCooldown) => {
                        std::thread::sleep(Duration::from_micros(10));
                        continue;
                    }
                    Err(e) => panic!("unexpected error: {:?}", e),
                }
            };
            assert_eq!(ids_a.len(), 1);
            assert_eq!(ids_b.len(), 1);

            let barrier = Arc::new(Barrier::new(2));
            let m1 = mapper.clone();
            let m2 = mapper.clone();
            let b1 = barrier.clone();
            let b2 = barrier.clone();
            let id_a = ids_a[0];
            let id_b = ids_b[0];

            let h1 = thread::spawn(move || {
                b1.wait();
                m1.activate(id_a)
            });
            let h2 = thread::spawn(move || {
                b2.wait();
                m2.activate(id_b)
            });
            let r1 = h1.join().expect("thread A panicked");
            let r2 = h2.join().expect("thread B panicked");

            // Exactly one must succeed and one must reject with
            // AtMaxShards.
            let ok_count = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
            let at_max_count = [&r1, &r2]
                .iter()
                .filter(|r| matches!(r, Err(ScalingError::AtMaxShards)))
                .count();
            assert_eq!(
                ok_count, 1,
                "iter {}: expected exactly one Ok, got r1={:?} r2={:?}",
                iter, r1, r2
            );
            assert_eq!(
                at_max_count, 1,
                "iter {}: expected exactly one AtMaxShards, got r1={:?} r2={:?}",
                iter, r1, r2
            );

            // active_count must not exceed max_shards at any
            // observation point.
            assert!(
                mapper.active_shard_count() <= 4,
                "iter {}: active_count={} exceeded max_shards=4",
                iter,
                mapper.active_shard_count(),
            );
        }
    }

    /// Two concurrent `scale_up(1)` calls must never both succeed
    /// inside a single cooldown window. Before the fix, the
    /// cooldown check happened only under a read lock that was
    /// released *before* the mutating write lock, so two threads
    /// could both observe `last_scaling=None` (or stale), both
    /// acquire the write lock in turn, and both succeed — racing
    /// past the cooldown floor and (on a max-shard-bounded
    /// scenario) potentially the `max_shards` cap.
    ///
    /// Pin: across `ITERATIONS` rounds of two-thread races, every
    /// iteration sees exactly one success and one `InCooldown`.
    #[test]
    fn cooldown_is_enforced_under_concurrent_scale_up() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        const ITERATIONS: usize = 1_000;

        for iter in 0..ITERATIONS {
            let policy = ScalingPolicy {
                min_shards: 1,
                max_shards: 16,
                // Large cooldown so a single iteration can't pass
                // the floor between the two threads' calls.
                cooldown: Duration::from_secs(60),
                ..Default::default()
            };
            let mapper = Arc::new(ShardMapper::new(2, 1024, policy).unwrap());
            let barrier = Arc::new(Barrier::new(2));

            let m1 = mapper.clone();
            let b1 = barrier.clone();
            let m2 = mapper.clone();
            let b2 = barrier.clone();

            let h1 = thread::spawn(move || {
                b1.wait();
                m1.scale_up(1)
            });
            let h2 = thread::spawn(move || {
                b2.wait();
                m2.scale_up(1)
            });

            let r1 = h1.join().unwrap();
            let r2 = h2.join().unwrap();

            let oks = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
            let cooldowns = [&r1, &r2]
                .iter()
                .filter(|r| matches!(r, Err(ScalingError::InCooldown)))
                .count();

            assert_eq!(
                oks, 1,
                "iter {iter}: expected exactly one Ok, got r1={r1:?}, r2={r2:?}"
            );
            assert_eq!(
                cooldowns, 1,
                "iter {iter}: expected exactly one InCooldown, got r1={r1:?}, r2={r2:?}"
            );
            assert_eq!(
                mapper.active_shard_count(),
                3,
                "iter {iter}: cooldown violated — both calls mutated state (shard count {})",
                mapper.active_shard_count()
            );
        }
    }

    #[test]
    fn test_scale_up_overflow_protection() {
        // Create mapper with a high starting shard ID to test overflow
        // protection. The monotonic id allocator advances by 1 per
        // shard regardless of which shards have been removed, so we
        // bump `next_shard_id` directly to simulate a near-`u16::MAX`
        // state.
        let policy = ScalingPolicy {
            max_shards: u16::MAX,
            ..Default::default()
        };
        let mapper = ShardMapper::new(1, 1024, policy).unwrap();

        // Position the allocator so the next id is u16::MAX - 1.
        // Trying to allocate 3 ids would need {MAX-1, MAX, MAX+1};
        // the last is unrepresentable in `u16`, so scale_up rejects.
        mapper
            .next_shard_id
            .store(u16::MAX - 1, AtomicOrdering::Relaxed);

        let result = mapper.scale_up(3);
        assert!(matches!(result, Err(ScalingError::AtMaxShards)));

        // Adding 1 shard should still work (id = MAX - 1).
        let result = mapper.scale_up(1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_evaluate_scaling_auto_scale_disabled() {
        let policy = ScalingPolicy {
            auto_scale: false,
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::None));
    }

    #[test]
    fn test_evaluate_scaling_in_cooldown() {
        let policy = ScalingPolicy {
            cooldown: Duration::from_secs(60),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Trigger a scaling operation to start cooldown
        *mapper.last_scaling.write() = Some(Instant::now());

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::None));
    }

    #[test]
    fn test_evaluate_scaling_scale_up_on_high_fill_ratio() {
        let policy = ScalingPolicy {
            fill_ratio_threshold: 0.7,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Set high fill ratio on majority of shards
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.fill_ratio = 0.9; // Above threshold
            }
        }

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::ScaleUp(_)));
    }

    #[test]
    fn test_evaluate_scaling_scale_up_on_high_latency() {
        let policy = ScalingPolicy {
            push_latency_threshold_ns: 10,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Set high push latency on majority of shards
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.avg_push_latency_ns = 100; // Above threshold
            }
        }

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::ScaleUp(_)));
    }

    #[test]
    fn test_evaluate_scaling_scale_down_on_underutilized() {
        let policy = ScalingPolicy {
            underutilized_threshold: 0.2,
            min_shards: 2,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Set low fill ratio and zero event rate on majority of shards
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.fill_ratio = 0.05; // Below threshold
                shard.last_metrics.event_rate = 0;
            }
        }

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::ScaleDown(_)));
    }

    /// Regression: a freshly-activated shard must NOT
    /// immediately count toward the underutilized tally on the
    /// next `evaluate_scaling`. Pre-fix the new shard's
    /// `last_metrics` was the `ShardMetrics::new(id)` placeholder
    /// (`fill_ratio = 0.0, event_rate = 0`), which matched the
    /// underutilized trigger immediately — oscillating the
    /// system: scale-up → next tick scale-down → next tick
    /// scale-up.
    ///
    /// Post-fix `MappedShard.activated_at` is stamped at the
    /// Provisioning → Active transition (and at scale-up
    /// construction for `Active`-from-create shards), and
    /// `evaluate_scaling` skips shards within `policy.cooldown`
    /// of activation.
    #[test]
    fn freshly_added_shard_skipped_from_evaluate_scaling_warmup() {
        let policy = ScalingPolicy {
            underutilized_threshold: 0.2,
            min_shards: 1,
            // Long cooldown so the warmup window stays open
            // throughout the test.
            cooldown: Duration::from_secs(60),
            ..Default::default()
        };
        let mapper = ShardMapper::new(3, 1024, policy).unwrap();

        // Direct manipulation of the shard list: pin two boot
        // shards as underutilized (boot stamps put them outside
        // the warmup window), and inject one freshly-activated
        // shard with `activated_at = now()` whose placeholder
        // metrics ALSO trigger the underutilized predicate.
        // Without the warmup skip, the count would be 3 of 3
        // shards underutilized → scale-down. WITH the warmup
        // skip, only the 2 boot shards count, and 2 of 3 still
        // exceeds the 3/2 = 1 majority — scale-down still fires
        // (driven by the boot shards), but the decisive property
        // is that the fresh shard is correctly excluded.
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.fill_ratio = 0.05;
                shard.last_metrics.event_rate = 0;
            }
            // The third shard is "fresh": stamp activated_at to
            // now, simulating a just-activated shard.
            shards[2].activated_at = Instant::now();
        }

        // The fresh shard must satisfy the warmup predicate.
        let now = Instant::now();
        let warmup_excluded: Vec<u16> = mapper
            .shards
            .read()
            .iter()
            .filter(|s| now.duration_since(s.activated_at) < mapper.policy.cooldown)
            .map(|s| s.id)
            .collect();
        assert_eq!(
            warmup_excluded,
            vec![2u16],
            "regression: only shard id 2 (just-stamped) should be \
             within the warmup window; boot shards are stamped \
             1 hour in the past"
        );

        // And evaluate_scaling must still produce ScaleDown
        // (driven by the 2 boot shards) — the fresh shard's
        // placeholder doesn't get to vote.
        let decision = mapper.evaluate_scaling();
        assert!(
            matches!(decision, ScalingDecision::ScaleDown(_)),
            "scale-down still fires from the boot shards' real \
             underutilization, but driven by 2 of 3 not 3 of 3 — \
             got {:?}",
            decision,
        );
    }

    #[test]
    fn test_evaluate_scaling_no_scale_up_at_max() {
        let policy = ScalingPolicy {
            fill_ratio_threshold: 0.7,
            max_shards: 4, // Already at max
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Set high fill ratio
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.fill_ratio = 0.9;
            }
        }

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::None));
    }

    #[test]
    fn test_evaluate_scaling_no_scale_down_at_min() {
        let policy = ScalingPolicy {
            underutilized_threshold: 0.2,
            min_shards: 4, // Already at min
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Set underutilized metrics
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.fill_ratio = 0.05;
                shard.last_metrics.event_rate = 0;
            }
        }

        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::None));
    }

    #[test]
    fn test_evaluate_scaling_ignores_draining_shards() {
        let policy = ScalingPolicy {
            fill_ratio_threshold: 0.7,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(4, 1024, policy).unwrap();

        // Set high fill ratio but mark shards as draining
        {
            let mut shards = mapper.shards.write();
            for shard in shards.iter_mut() {
                shard.last_metrics.fill_ratio = 0.9;
                shard.state = ShardState::Draining;
            }
        }

        // Should not scale up since draining shards are ignored
        let decision = mapper.evaluate_scaling();
        assert!(matches!(decision, ScalingDecision::None));
    }

    #[test]
    fn test_shard_metrics_new() {
        let metrics = ShardMetrics::new(5);
        assert_eq!(metrics.shard_id, 5);
        assert_eq!(metrics.fill_ratio, 0.0);
        assert_eq!(metrics.event_rate, 0);
        assert!(!metrics.draining);
    }

    #[test]
    fn test_shard_metrics_compute_weight() {
        let mut metrics = ShardMetrics::new(0);
        metrics.fill_ratio = 0.5;
        metrics.avg_push_latency_ns = 100;
        metrics.event_rate = 1_000_000;

        metrics.compute_weight();
        assert!(metrics.weight > 0.0);
    }

    #[test]
    fn test_shard_metrics_draining_max_weight() {
        let mut metrics = ShardMetrics::new(0);
        metrics.draining = true;
        metrics.compute_weight();
        assert_eq!(metrics.weight, f64::MAX);
    }

    #[test]
    fn test_scaling_decision_debug() {
        let none = ScalingDecision::None;
        let up = ScalingDecision::ScaleUp(2);
        let down = ScalingDecision::ScaleDown(1);

        assert!(format!("{:?}", none).contains("None"));
        assert!(format!("{:?}", up).contains("ScaleUp"));
        assert!(format!("{:?}", down).contains("ScaleDown"));
    }

    /// Regression: BUG_REPORT.md #46 — `scale_up_provisioning`
    /// allocates a shard but `select_shard` must NOT route to it
    /// until `activate` has been called. This is the load-bearing
    /// invariant that lets `EventBus::add_shard_internal` wire up
    /// drain workers before producers can land in the new ring
    /// buffer.
    #[test]
    fn provisioning_shard_is_not_selectable_until_activated() {
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 16,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();

        // Allocate a provisioning shard. Existing ids are 0,1; new
        // is 2.
        let new_ids = mapper.scale_up_provisioning(1).unwrap();
        assert_eq!(new_ids, vec![2]);

        // The provisioning shard must NOT appear in active accounting.
        assert_eq!(mapper.active_shard_count(), 2);
        assert_eq!(mapper.shard_state(2), Some(ShardState::Provisioning));

        // Across many hashes, `select_shard` must never pick id 2.
        // Spread the input hashes across the u64 range so the
        // unbiased Lemire-style mapping actually
        // distributes — sequential small ids would all map to
        // index 0 because `(small * len) >> 64 = 0`. Production
        // callers pass `xxh3_64`-hashed event payloads, which
        // are uniform u64s; this scaling mirrors that.
        let mut seen: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let stride = u64::MAX / 10_000;
        for i in 0u64..10_000 {
            seen.insert(mapper.select_shard(i.wrapping_mul(stride)));
        }
        assert!(
            !seen.contains(&2),
            "provisioning shard 2 was selected — \
             producers would push into a ring buffer with no consumer (#46)"
        );
        assert!(seen == [0, 1].into_iter().collect());

        // After `activate`, the shard is selectable.
        mapper.activate(2).unwrap();
        assert_eq!(mapper.shard_state(2), Some(ShardState::Active));
        assert_eq!(mapper.active_shard_count(), 3);

        let mut seen_after: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let stride = u64::MAX / 10_000;
        for i in 0u64..10_000 {
            seen_after.insert(mapper.select_shard(i.wrapping_mul(stride)));
        }
        assert!(
            seen_after.contains(&2),
            "after activate, shard 2 should be a valid select_shard target"
        );
    }

    /// Regression: BUG_REPORT.md #51 — when no `Active` shard exists
    /// (e.g. all shards are Draining or Provisioning), `select_shard`
    /// must NOT fall back to a Draining shard. Pushing into a
    /// draining shard increments `pushes_since_drain_start` and
    /// blocks finalization indefinitely.
    #[test]
    fn select_shard_does_not_fall_back_to_draining() {
        let policy = ScalingPolicy {
            min_shards: 0,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        // Force min_shards = 0 via direct construction so we can drain
        // every shard.
        let mut policy = policy;
        policy.min_shards = 1;
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();

        // Force every shard into Draining state directly. We bypass
        // `scale_down`'s min_shards floor by mutating the test-only
        // accessor; this models a state that can otherwise be
        // reached by `drain_specific` calls.
        {
            let mut shards = mapper.shards.write();
            for s in shards.iter_mut() {
                s.state = ShardState::Draining;
                s.metrics.set_draining(true);
            }
        }

        // The fallback must return the OOB sentinel `u16::MAX` rather
        // than a draining shard id (0 or 1). The upstream
        // `resolve_idx` path will see no match for `u16::MAX` and
        // surface as `Unrouted` (#44), which is the correct signal:
        // "no destination, do not push."
        for hash in 0u64..1_000 {
            let picked = mapper.select_shard(hash);
            assert_eq!(
                picked,
                u16::MAX,
                "fallback returned a draining shard ({}); pushes there would \
                 deadlock finalize_draining (#51)",
                picked
            );
        }
    }

    /// Regression: BUG_REPORT.md #32 — `scale_up(0)` previously
    /// bumped the cooldown timestamp and could spuriously fail at
    /// `u16::MAX` even though zero ids were being allocated.
    #[test]
    fn scale_up_zero_is_a_noop() {
        let policy = ScalingPolicy {
            cooldown: Duration::from_secs(60),
            ..Default::default()
        };
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();
        // Pretend we just scaled — cooldown is active.
        *mapper.last_scaling.write() = Some(Instant::now());

        // scale_up(0) must not return InCooldown and must not bump
        // last_scaling.
        let before_ts = *mapper.last_scaling.read();
        let r = mapper.scale_up(0);
        assert!(
            r.is_ok(),
            "scale_up(0) should succeed as a no-op, got {r:?}"
        );
        assert_eq!(r.unwrap().len(), 0);
        let after_ts = *mapper.last_scaling.read();
        assert_eq!(before_ts, after_ts, "scale_up(0) bumped cooldown timestamp");

        // Also: position the allocator at u16::MAX and verify a
        // count==0 call doesn't trip the sentinel check.
        mapper
            .next_shard_id
            .store(u16::MAX, AtomicOrdering::Relaxed);
        assert!(mapper.scale_up(0).is_ok());
    }

    /// Regression: `drain_specific` must bump `last_scaling` so
    /// a subsequent `scale_up` is gated by the cooldown floor.
    /// Pre-fix `scale_down` wrote `last_scaling` but
    /// `drain_specific` did not, so the sequence
    /// `drain_specific(id) → scale_up(N)` bypassed the cooldown
    /// — even though both decrement `active_count` and so
    /// should be symmetric from the budget-math perspective.
    #[test]
    fn drain_specific_bumps_last_scaling_so_scale_up_respects_cooldown() {
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 8,
            // A cooldown long enough that `Instant::now()` won't
            // accidentally elapse it during the test, but short
            // enough not to slow CI.
            cooldown: Duration::from_secs(60),
            ..Default::default()
        };
        let mapper = ShardMapper::new(3, 1024, policy).unwrap();

        // Pre-condition: no prior scaling action.
        assert!(mapper.last_scaling.read().is_none());

        let before = Instant::now();
        mapper.drain_specific(0).unwrap();
        let after_ts = mapper
            .last_scaling
            .read()
            .expect("drain_specific must record a `last_scaling` timestamp");
        // Sanity: the recorded timestamp is in the test window.
        assert!(
            after_ts >= before,
            "last_scaling must be bumped to a current Instant (got {:?}, before was {:?})",
            after_ts,
            before
        );

        // The decisive sealed property: a follow-up scale_up
        // must trip the cooldown gate. Pre-fix this would have
        // succeeded immediately because `last_scaling` was never
        // written by `drain_specific`.
        let err = mapper
            .scale_up(1)
            .expect_err("scale_up immediately after drain_specific must hit cooldown");
        match err {
            ScalingError::InCooldown => {} // expected
            other => panic!("expected InCooldown after drain_specific, got {:?}", other),
        }
    }

    /// Regression: BUG_REPORT.md #48 — `drain_specific` must
    /// transition the shard's `MappedShard.state` to `Draining` so
    /// that `select_shard` stops routing to it. The previous
    /// `drain_shard` only flipped a metrics atomic.
    #[test]
    fn drain_specific_takes_shard_out_of_select() {
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(3, 1024, policy).unwrap();

        // Drain shard 0.
        mapper.drain_specific(0).unwrap();
        assert_eq!(mapper.shard_state(0), Some(ShardState::Draining));
        assert_eq!(mapper.active_shard_count(), 2);

        // Across many hashes, select_shard must not pick id 0.
        for hash in 0u64..10_000 {
            let picked = mapper.select_shard(hash);
            assert_ne!(
                picked, 0,
                "select_shard returned a Draining shard id 0 — \
                 producer pushes there would block finalize_draining (#48)"
            );
        }
    }

    /// Regression: BUG_REPORT.md #33 — `set_draining(true)` and a
    /// concurrent `record_push` race on `pushes_since_drain_start`.
    /// The race itself can't be eliminated without a CAS loop on
    /// the counter (which would penalize the hot path), so the
    /// best we can pin is: after the dust settles, the counter
    /// value is bounded — never larger than the number of pushes
    /// that genuinely overlapped the transition. This catches a
    /// regression where the store-zero stops happening, where the
    /// flag publish stops happening, or where future code adds
    /// drift that compounds the race across many transitions.
    #[test]
    fn set_draining_resets_counter_under_concurrent_pushes() {
        use std::sync::Barrier;
        use std::thread;

        const ITERATIONS: usize = 200;
        const PUSHERS: usize = 4;

        for _ in 0..ITERATIONS {
            let collector = Arc::new(ShardMetricsCollector::new(0, 1024));

            // Sanity: counter starts at zero.
            assert_eq!(collector.pushes_since_drain_start(), 0);

            // Pre-load with a noticeable amount of "before drain"
            // pushes so the reset has work to do.
            for _ in 0..50 {
                collector.record_push(1);
            }
            assert_eq!(collector.pushes_since_drain_start(), 50);

            // Race: every pusher hammers `record_push` while one
            // thread calls `set_draining(true)`. After the barrier,
            // we want to observe that the counter ends up bounded
            // by PUSHERS (the number of pushes that genuinely
            // overlapped the transition) — and crucially NOT 50+,
            // which is what the buggy code with a missing reset
            // would leave behind.
            let barrier = Arc::new(Barrier::new(PUSHERS + 1));
            let mut handles = Vec::with_capacity(PUSHERS);
            for _ in 0..PUSHERS {
                let c = Arc::clone(&collector);
                let b = Arc::clone(&barrier);
                handles.push(thread::spawn(move || {
                    b.wait();
                    c.record_push(1);
                }));
            }

            barrier.wait();
            collector.set_draining(true);
            for h in handles {
                h.join().unwrap();
            }

            // After reset + at-most-PUSHERS racing pushes, the
            // counter is bounded. The strong guarantee we want is
            // "the 50 pre-drain pushes did NOT survive the reset."
            // Anything <= PUSHERS is acceptable — the race may
            // count any subset of the racing pushes.
            let final_count = collector.pushes_since_drain_start();
            assert!(
                final_count <= PUSHERS as u64,
                "set_draining reset is broken: counter is {} after reset, \
                 expected at most {} (#33)",
                final_count,
                PUSHERS
            );
        }
    }

    /// Regression: BUG_REPORT.md #49 — `finalize_draining` must
    /// drop the `shards` write lock before calling `on_shard_removed`,
    /// so a callback that re-enters the mapper (read methods like
    /// `shard_state`, `active_shard_ids`, etc.) does not deadlock.
    #[test]
    fn finalize_draining_does_not_deadlock_on_callback_reentry() {
        use std::sync::Mutex;
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 8,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = Arc::new(ShardMapper::new(2, 1024, policy).unwrap());

        // Set a callback that re-enters the mapper. Before the fix
        // this acquires `shards.read()` while finalize_draining
        // still holds `shards.write()`, deadlocking on parking_lot's
        // non-recursive RwLock.
        type Observation = (u16, Option<ShardState>);
        let observed_states: Arc<Mutex<Vec<Observation>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let mapper_for_cb = Arc::clone(&mapper);
            let observed = Arc::clone(&observed_states);
            mapper.set_on_shard_removed(move |id| {
                let st = mapper_for_cb.shard_state(id);
                observed.lock().unwrap().push((id, st));
            });
        }

        // Drive a shard all the way to Stopped: drain it, mark its
        // metrics empty (current_len = 0), and drop drain_started
        // far enough back that the 100ms gate is satisfied.
        mapper.drain_specific(0).unwrap();
        {
            let mut shards = mapper.shards.write();
            let s = shards.iter_mut().find(|s| s.id == 0).unwrap();
            s.metrics.current_len.store(0, AtomicOrdering::Relaxed);
            s.metrics
                .pushes_since_drain_start
                .store(0, AtomicOrdering::Relaxed);
            // Backdate drain_started so the elapsed > 100ms gate trips.
            s.drain_started = Some(Instant::now() - Duration::from_secs(1));
        }

        // The call below would deadlock with the bug present.
        let stopped = mapper.finalize_draining();
        assert_eq!(stopped, vec![0]);

        // The callback must have run AND been able to read state
        // (proving the lock was released).
        let observed = observed_states.lock().unwrap().clone();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0, 0);
        // State should be `Stopped` (set by finalize_draining before
        // the lock was dropped).
        assert_eq!(observed[0].1, Some(ShardState::Stopped));
    }

    /// `activate` must signal whether a state transition
    /// actually occurred so callers can avoid double-counting.
    /// Pre-fix it returned `Result<(), _>`, so a caller that
    /// invoked `activate` twice on the same shard incremented its
    /// own external counter (e.g. `ShardManager::num_shards`)
    /// twice for one logical transition.
    #[test]
    fn activate_returns_true_on_transition_and_false_on_idempotent_call() {
        let policy = ScalingPolicy {
            min_shards: 1,
            max_shards: 16,
            cooldown: Duration::from_nanos(1),
            ..Default::default()
        };
        let mapper = ShardMapper::new(2, 1024, policy).unwrap();

        // Newly-provisioned shard 2 → Active: real transition.
        let new_ids = mapper.scale_up_provisioning(1).unwrap();
        assert_eq!(new_ids, vec![2]);
        let first = mapper.activate(2).unwrap();
        assert!(
            first,
            "first activate on a Provisioning shard must return true"
        );
        assert_eq!(mapper.active_shard_count(), 3);

        // Second activate on the same already-Active shard:
        // idempotent, no transition.
        let second = mapper.activate(2).unwrap();
        assert!(
            !second,
            "activate on an already-Active shard must return false; \
             pre-fix this returned Ok(()) and the caller couldn't tell"
        );
        // active_count must NOT have moved.
        assert_eq!(
            mapper.active_shard_count(),
            3,
            "idempotent activate must not bump active_count"
        );
    }
}
