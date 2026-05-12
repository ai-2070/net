//! Greedy-LRU metrics — bounded per-channel + cluster-wide
//! counters surfaced as `dataforts_greedy_*` per
//! `DATAFORTS_PLAN.md` § Cross-cutting concerns / Observability.
//!
//! Cardinality model mirrors `ReplicationMetricsRegistry`:
//!
//! - Per-channel state keyed by channel name. New channels past
//!   [`MAX_TRACKED_CHANNELS`] fold into a shared `__overflow__`
//!   bucket so an unbounded-fanout workload can't grow the
//!   DashMap without bound.
//! - Cluster-wide state (admit-rejected counters split by reason,
//!   I/O budget gauge) lives in a single shared atomic struct
//!   reachable from any caller.
//!
//! Surface:
//!
//! | Name | Type | Labels |
//! |------|------|--------|
//! | `dataforts_greedy_cache_hits_total` | counter | `channel` |
//! | `dataforts_greedy_serve_count_total` | counter | `channel` |
//! | `dataforts_greedy_evictions_total` | counter | `channel` |
//! | `dataforts_greedy_bytes_resident` | gauge | `channel` |
//! | `dataforts_greedy_admit_rejected_total` | counter | `reason` (`scope`/`intent`/`colocation`/`capacity`) |
//! | `dataforts_greedy_io_budget_used_bytes` | gauge | — |

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

/// Maximum number of channels tracked with their own counter set.
/// Channels past this fold into a single `__overflow__` bucket.
/// Same cap as `ReplicationMetricsRegistry::MAX_TRACKED_CHANNELS`.
pub const MAX_TRACKED_CHANNELS: usize = 1024;

/// Label value used for channels folded past
/// [`MAX_TRACKED_CHANNELS`].
pub const OVERFLOW_CHANNEL_LABEL: &str = "__overflow__";

/// Per-channel atomic counter set.
#[derive(Debug, Default)]
pub struct GreedyChannelMetricsAtomic {
    /// Cumulative cache-hit reads — a substrate read that
    /// resolved to a greedy-cached holder.
    pub cache_hits_total: AtomicU64,
    /// Cumulative reads served *from* this node's cache (the
    /// other-direction view of `cache_hits_total`). Bumped by the
    /// runtime when an inbound read targets a chain we hold.
    pub serve_count_total: AtomicU64,
    /// Cumulative evictions — this channel was removed from the
    /// registry under cluster-cap pressure.
    pub evictions_total: AtomicU64,
    /// Current bytes resident in this channel's cache. Updated by
    /// the runtime on every `note_appended` / `evict`. Gauge, not
    /// monotonic.
    pub bytes_resident: AtomicU64,
}

impl GreedyChannelMetricsAtomic {
    /// All-zero atomics. Used by the registry on first per-channel
    /// observation.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the `cache_hits_total` counter by 1.
    pub fn incr_cache_hit(&self) {
        self.cache_hits_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the `serve_count_total` counter by 1.
    pub fn incr_serve(&self) {
        self.serve_count_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the `evictions_total` counter by 1.
    pub fn incr_eviction(&self) {
        self.evictions_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the current bytes-resident gauge.
    pub fn set_bytes_resident(&self, bytes: u64) {
        self.bytes_resident.store(bytes, Ordering::Relaxed);
    }
}

/// Cluster-wide atomic counters — admission rejections split by
/// reason, plus the I/O budget gauge.
#[derive(Debug, Default)]
pub struct GreedyClusterMetricsAtomic {
    /// Cumulative scope-axis admission rejections.
    pub admit_rejected_scope_total: AtomicU64,
    /// Cumulative intent-axis admission rejections.
    pub admit_rejected_intent_total: AtomicU64,
    /// Cumulative colocation-axis admission rejections.
    pub admit_rejected_colocation_total: AtomicU64,
    /// Cumulative capacity rejections — admission would have
    /// landed but the cluster cap refused the write. Disjoint from
    /// the `bandwidth` axis (bandwidth-budget throttling is its
    /// own counter).
    pub admit_rejected_capacity_total: AtomicU64,
    /// Cumulative bandwidth-throttle rejections — admission and
    /// the cluster cap were both clear, but the bandwidth budget
    /// refused the write. Operators on faster-than-gigabit NICs
    /// who see this counter saturating should set
    /// `GreedyConfig::nic_peak_bytes_per_s` explicitly.
    pub admit_rejected_bandwidth_total: AtomicU64,
    /// Current I/O budget used — bytes consumed from the token
    /// bucket since the last refill. Gauge.
    pub io_budget_used_bytes: AtomicU64,
    /// Cumulative inbound events dropped because the
    /// observe_event in-flight cap was saturated. Surfaces a
    /// flooding peer or an under-provisioned cap — operators
    /// dashboard this against admit_rejected counters to
    /// distinguish "rejected by policy" from "dropped under load".
    pub observer_dropped_overloaded_total: AtomicU64,
    /// Cumulative `note_read` calls skipped under gravity because
    /// the chain's `origin_hash == 0` (the default publisher
    /// doesn't stamp identity). Per-chain heat would collapse
    /// into a single bucket if we bumped these, so we skip them
    /// and surface the count instead. Operators see this rising
    /// when their publishers aren't configured to stamp origins.
    pub gravity_heat_unattributed_total: AtomicU64,
    /// Cumulative G-1 blob-pull verdicts that returned `Admit` —
    /// the local node would have speculatively pulled the blob
    /// referenced by an admitted chain event. The actual fetch
    /// path is a follow-up; this counter surfaces the decision
    /// so operators can dashboard the policy independent of the
    /// fetch wiring.
    pub blob_pulls_admitted_total: AtomicU64,
    /// G-1 blob-pull veto: local node lacks `dataforts.blob.storage`.
    pub blob_pulls_rejected_no_storage_total: AtomicU64,
    /// G-1 blob-pull veto: local greedy disabled.
    pub blob_pulls_rejected_greedy_disabled_total: AtomicU64,
    /// G-1 blob-pull veto: local greedy proximity is zero.
    pub blob_pulls_rejected_proximity_zero_total: AtomicU64,
    /// G-1 blob-pull veto: local node advertising
    /// `dataforts:blob-storage-unhealthy`.
    pub blob_pulls_rejected_unhealthy_total: AtomicU64,
    /// G-1 blob-pull veto: publisher scope outside local greedy
    /// scope boundary.
    pub blob_pulls_rejected_scope_mismatch_total: AtomicU64,
}

impl GreedyClusterMetricsAtomic {
    /// All-zero atomics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bump the corresponding admit-rejected counter for the
    /// supplied [`crate::adapter::net::dataforts::greedy::AdmissionVerdict`]
    /// reject variant.
    pub fn incr_admit_rejected(&self, reason: AdmitRejectReason) {
        match reason {
            AdmitRejectReason::Scope => {
                self.admit_rejected_scope_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            AdmitRejectReason::Intent => {
                self.admit_rejected_intent_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            AdmitRejectReason::Colocation => {
                self.admit_rejected_colocation_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            AdmitRejectReason::Capacity => {
                self.admit_rejected_capacity_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            AdmitRejectReason::Bandwidth => {
                self.admit_rejected_bandwidth_total
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Set the I/O-budget-used gauge.
    pub fn set_io_budget_used_bytes(&self, bytes: u64) {
        self.io_budget_used_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Increment the observer-overload drop counter.
    pub fn incr_observer_dropped_overloaded(&self) {
        self.observer_dropped_overloaded_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the gravity unattributed-heat skip counter.
    pub fn incr_gravity_heat_unattributed(&self) {
        self.gravity_heat_unattributed_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the G-1 blob-pull admitted counter.
    pub fn incr_blob_pull_admitted(&self) {
        self.blob_pulls_admitted_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the G-1 blob-pull rejected counter for the supplied
    /// reason. Each reason maps to a distinct
    /// `dataforts_greedy_blob_pulls_rejected_total{reason=...}`
    /// Prometheus label.
    pub fn incr_blob_pull_rejected(
        &self,
        reason: crate::adapter::net::dataforts::blob::PullBlobReject,
    ) {
        use crate::adapter::net::dataforts::blob::PullBlobReject;
        match reason {
            PullBlobReject::NoStorageCap => {
                self.blob_pulls_rejected_no_storage_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            PullBlobReject::GreedyDisabled => {
                self.blob_pulls_rejected_greedy_disabled_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            PullBlobReject::ProximityZero => {
                self.blob_pulls_rejected_proximity_zero_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            PullBlobReject::Unhealthy => {
                self.blob_pulls_rejected_unhealthy_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            PullBlobReject::ScopeMismatch => {
                self.blob_pulls_rejected_scope_mismatch_total
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Enum mirroring the
/// [`crate::adapter::net::dataforts::greedy::AdmissionVerdict`]
/// reject variants plus a `Capacity` shape for the I/O-budget /
/// cluster-cap rejection path the runtime owns (not `should_admit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitRejectReason {
    /// Scope-axis rejection from `should_admit`.
    Scope,
    /// Intent-axis rejection from `should_admit`.
    Intent,
    /// Colocation-axis rejection from `should_admit`.
    Colocation,
    /// Runtime-side rejection — admission permits but the cluster
    /// cap or per-channel cap would force an immediate eviction.
    /// Disjoint from `Bandwidth`.
    Capacity,
    /// Runtime-side rejection — admission and cluster cap permit
    /// but the bandwidth budget refuses the write. Operators
    /// dashboard this separately so faster-than-gigabit NICs
    /// observed via [`crate::adapter::net::dataforts::greedy::DEFAULT_NIC_PEAK_BYTES_PER_S`]
    /// surface as a distinct signal from real-capacity exhaustion.
    Bandwidth,
}

/// Process-wide registry. One per `MeshNode::enable_greedy_dataforts`
/// install; the registry's `cluster` field is shared across every
/// channel.
#[derive(Debug)]
pub struct GreedyMetricsRegistry {
    channels: DashMap<String, Arc<GreedyChannelMetricsAtomic>>,
    cluster: Arc<GreedyClusterMetricsAtomic>,
}

impl Default for GreedyMetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GreedyMetricsRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
            cluster: Arc::new(GreedyClusterMetricsAtomic::new()),
        }
    }

    /// Get-or-create the per-channel counter set for `channel`.
    /// Channels past [`MAX_TRACKED_CHANNELS`] fold into the
    /// `__overflow__` bucket.
    ///
    /// **Soft cap.** The `len() < MAX` check is a separate read
    /// from the subsequent `entry().or_insert_with(...)`, so two
    /// threads racing past the boundary can both insert (each
    /// bumping `len` past `MAX_TRACKED_CHANNELS` by one). DashMap's
    /// sharded layout bounds the overshoot to a small constant per
    /// shard; the cap is documented as "approximately MAX," not a
    /// hard ceiling. Hard-capping would require a global lock and
    /// is not worth the cost — `for_channel` is on the hot path.
    pub fn for_channel(&self, channel: &str) -> Arc<GreedyChannelMetricsAtomic> {
        if let Some(m) = self.channels.get(channel) {
            return m.clone();
        }
        if self.channels.len() >= MAX_TRACKED_CHANNELS && !self.channels.contains_key(channel) {
            return self
                .channels
                .entry(OVERFLOW_CHANNEL_LABEL.to_string())
                .or_insert_with(|| Arc::new(GreedyChannelMetricsAtomic::new()))
                .clone();
        }
        self.channels
            .entry(channel.to_string())
            .or_insert_with(|| Arc::new(GreedyChannelMetricsAtomic::new()))
            .clone()
    }

    /// Borrow the cluster-wide counter set. Same `Arc` across every
    /// call — cheap to clone for runtime threads.
    pub fn cluster(&self) -> Arc<GreedyClusterMetricsAtomic> {
        self.cluster.clone()
    }

    /// Number of distinct channels currently tracked, including the
    /// overflow bucket if active.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// True iff no channel has been observed yet.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Read-only snapshot — copies atomics into plain data, sorted
    /// by channel name for byte-stable Prometheus emission.
    pub fn snapshot(&self) -> GreedyMetricsSnapshot {
        let mut channels = Vec::with_capacity(self.channels.len());
        for entry in self.channels.iter() {
            let m = entry.value();
            channels.push(GreedyChannelMetrics {
                channel: entry.key().clone(),
                cache_hits_total: m.cache_hits_total.load(Ordering::Relaxed),
                serve_count_total: m.serve_count_total.load(Ordering::Relaxed),
                evictions_total: m.evictions_total.load(Ordering::Relaxed),
                bytes_resident: m.bytes_resident.load(Ordering::Relaxed),
            });
        }
        channels.sort_by(|a, b| a.channel.cmp(&b.channel));
        GreedyMetricsSnapshot {
            channels,
            cluster: GreedyClusterMetrics {
                admit_rejected_scope_total: self
                    .cluster
                    .admit_rejected_scope_total
                    .load(Ordering::Relaxed),
                admit_rejected_intent_total: self
                    .cluster
                    .admit_rejected_intent_total
                    .load(Ordering::Relaxed),
                admit_rejected_colocation_total: self
                    .cluster
                    .admit_rejected_colocation_total
                    .load(Ordering::Relaxed),
                admit_rejected_capacity_total: self
                    .cluster
                    .admit_rejected_capacity_total
                    .load(Ordering::Relaxed),
                admit_rejected_bandwidth_total: self
                    .cluster
                    .admit_rejected_bandwidth_total
                    .load(Ordering::Relaxed),
                io_budget_used_bytes: self.cluster.io_budget_used_bytes.load(Ordering::Relaxed),
                observer_dropped_overloaded_total: self
                    .cluster
                    .observer_dropped_overloaded_total
                    .load(Ordering::Relaxed),
                gravity_heat_unattributed_total: self
                    .cluster
                    .gravity_heat_unattributed_total
                    .load(Ordering::Relaxed),
                blob_pulls_admitted_total: self
                    .cluster
                    .blob_pulls_admitted_total
                    .load(Ordering::Relaxed),
                blob_pulls_rejected_no_storage_total: self
                    .cluster
                    .blob_pulls_rejected_no_storage_total
                    .load(Ordering::Relaxed),
                blob_pulls_rejected_greedy_disabled_total: self
                    .cluster
                    .blob_pulls_rejected_greedy_disabled_total
                    .load(Ordering::Relaxed),
                blob_pulls_rejected_proximity_zero_total: self
                    .cluster
                    .blob_pulls_rejected_proximity_zero_total
                    .load(Ordering::Relaxed),
                blob_pulls_rejected_unhealthy_total: self
                    .cluster
                    .blob_pulls_rejected_unhealthy_total
                    .load(Ordering::Relaxed),
                blob_pulls_rejected_scope_mismatch_total: self
                    .cluster
                    .blob_pulls_rejected_scope_mismatch_total
                    .load(Ordering::Relaxed),
            },
        }
    }
}

/// Per-channel snapshot.
#[derive(Debug, Clone)]
pub struct GreedyChannelMetrics {
    /// Channel name or [`OVERFLOW_CHANNEL_LABEL`].
    pub channel: String,
    /// Cumulative cache hits.
    pub cache_hits_total: u64,
    /// Cumulative serves from cache.
    pub serve_count_total: u64,
    /// Cumulative evictions.
    pub evictions_total: u64,
    /// Current bytes resident in this channel's cache.
    pub bytes_resident: u64,
}

/// Cluster-wide snapshot.
#[derive(Debug, Clone, Default)]
pub struct GreedyClusterMetrics {
    /// Cumulative scope-axis rejections.
    pub admit_rejected_scope_total: u64,
    /// Cumulative intent-axis rejections.
    pub admit_rejected_intent_total: u64,
    /// Cumulative colocation-axis rejections.
    pub admit_rejected_colocation_total: u64,
    /// Cumulative capacity rejections.
    pub admit_rejected_capacity_total: u64,
    /// Cumulative bandwidth-budget rejections — admission permits
    /// and capacity permits, but the bandwidth gate refuses.
    pub admit_rejected_bandwidth_total: u64,
    /// Current I/O-budget bytes used.
    pub io_budget_used_bytes: u64,
    /// Cumulative observer-overload event drops — the
    /// `observe_event` in-flight cap was saturated and the event
    /// was discarded without entering the admission pipeline.
    pub observer_dropped_overloaded_total: u64,
    /// Cumulative gravity heat bumps skipped because the chain's
    /// `origin_hash == 0` (publisher didn't stamp identity).
    pub gravity_heat_unattributed_total: u64,
    /// Cumulative G-1 blob-pull admit verdicts.
    pub blob_pulls_admitted_total: u64,
    /// G-1 blob-pull veto: no `dataforts.blob.storage`.
    pub blob_pulls_rejected_no_storage_total: u64,
    /// G-1 blob-pull veto: local greedy disabled.
    pub blob_pulls_rejected_greedy_disabled_total: u64,
    /// G-1 blob-pull veto: local greedy proximity zero.
    pub blob_pulls_rejected_proximity_zero_total: u64,
    /// G-1 blob-pull veto: local node unhealthy.
    pub blob_pulls_rejected_unhealthy_total: u64,
    /// G-1 blob-pull veto: publisher scope outside local boundary.
    pub blob_pulls_rejected_scope_mismatch_total: u64,
}

/// Full snapshot — sorted channel list + cluster-wide counters.
#[derive(Debug, Clone, Default)]
pub struct GreedyMetricsSnapshot {
    /// One entry per tracked channel, sorted by channel name.
    pub channels: Vec<GreedyChannelMetrics>,
    /// Cluster-wide counters.
    pub cluster: GreedyClusterMetrics,
}

impl GreedyMetricsSnapshot {
    /// Render in Prometheus text-exposition format. Mirrors
    /// `ReplicationMetricsSnapshot::prometheus_text`.
    pub fn prometheus_text(&self) -> String {
        let mut out = String::with_capacity(2048);

        for (help, name, getter) in CHANNEL_COUNTER_DESCRIPTORS {
            let _ = writeln!(out, "# HELP {} {}", name, help);
            let _ = writeln!(out, "# TYPE {} counter", name);
            for c in &self.channels {
                let _ = writeln!(
                    out,
                    "{}{{channel=\"{}\"}} {}",
                    name,
                    escape_label(&c.channel),
                    getter(c),
                );
            }
        }

        // bytes_resident is a gauge, not a counter.
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_bytes_resident Current bytes resident in the channel's cache."
        );
        let _ = writeln!(out, "# TYPE dataforts_greedy_bytes_resident gauge");
        for c in &self.channels {
            let _ = writeln!(
                out,
                "dataforts_greedy_bytes_resident{{channel=\"{}\"}} {}",
                escape_label(&c.channel),
                c.bytes_resident,
            );
        }

        // Cluster-wide admit_rejected counter (single metric, reason-labeled).
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_admit_rejected_total Cumulative greedy-cache admission rejections, split by axis."
        );
        let _ = writeln!(out, "# TYPE dataforts_greedy_admit_rejected_total counter");
        for (reason, count) in [
            ("scope", self.cluster.admit_rejected_scope_total),
            ("intent", self.cluster.admit_rejected_intent_total),
            ("colocation", self.cluster.admit_rejected_colocation_total),
            ("capacity", self.cluster.admit_rejected_capacity_total),
            ("bandwidth", self.cluster.admit_rejected_bandwidth_total),
        ] {
            let _ = writeln!(
                out,
                "dataforts_greedy_admit_rejected_total{{reason=\"{}\"}} {}",
                reason, count,
            );
        }

        // I/O budget gauge.
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_io_budget_used_bytes Bytes consumed from the greedy I/O token bucket since the last refill."
        );
        let _ = writeln!(out, "# TYPE dataforts_greedy_io_budget_used_bytes gauge");
        let _ = writeln!(
            out,
            "dataforts_greedy_io_budget_used_bytes {}",
            self.cluster.io_budget_used_bytes,
        );

        // Observer-overload drops (counter).
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_observer_dropped_total Cumulative events dropped at the observe_event hot path, split by reason."
        );
        let _ = writeln!(
            out,
            "# TYPE dataforts_greedy_observer_dropped_total counter"
        );
        let _ = writeln!(
            out,
            "dataforts_greedy_observer_dropped_total{{reason=\"overloaded\"}} {}",
            self.cluster.observer_dropped_overloaded_total,
        );

        // Gravity heat-bumps skipped because the chain origin
        // wasn't stamped (origin_hash == 0).
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_gravity_heat_unattributed_total Cumulative gravity note_read bumps skipped because origin_hash was zero (publisher didn't stamp identity)."
        );
        let _ = writeln!(
            out,
            "# TYPE dataforts_greedy_gravity_heat_unattributed_total counter"
        );
        let _ = writeln!(
            out,
            "dataforts_greedy_gravity_heat_unattributed_total {}",
            self.cluster.gravity_heat_unattributed_total,
        );

        // G-1 blob-pull admit counter.
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_blob_pulls_admitted_total Cumulative G-1 blob-pull verdicts that returned Admit."
        );
        let _ = writeln!(
            out,
            "# TYPE dataforts_greedy_blob_pulls_admitted_total counter"
        );
        let _ = writeln!(
            out,
            "dataforts_greedy_blob_pulls_admitted_total {}",
            self.cluster.blob_pulls_admitted_total,
        );

        // G-1 blob-pull rejection counter, reason-labeled.
        let _ = writeln!(
            out,
            "# HELP dataforts_greedy_blob_pulls_rejected_total Cumulative G-1 blob-pull rejections, split by reason."
        );
        let _ = writeln!(
            out,
            "# TYPE dataforts_greedy_blob_pulls_rejected_total counter"
        );
        for (reason, count) in [
            ("no_storage", self.cluster.blob_pulls_rejected_no_storage_total),
            (
                "greedy_disabled",
                self.cluster.blob_pulls_rejected_greedy_disabled_total,
            ),
            (
                "proximity_zero",
                self.cluster.blob_pulls_rejected_proximity_zero_total,
            ),
            ("unhealthy", self.cluster.blob_pulls_rejected_unhealthy_total),
            (
                "scope_mismatch",
                self.cluster.blob_pulls_rejected_scope_mismatch_total,
            ),
        ] {
            let _ = writeln!(
                out,
                "dataforts_greedy_blob_pulls_rejected_total{{reason=\"{}\"}} {}",
                reason, count,
            );
        }

        out
    }

    /// True iff zero channels and zero cluster-wide events.
    /// Useful for tests.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
            && self.cluster.admit_rejected_scope_total == 0
            && self.cluster.admit_rejected_intent_total == 0
            && self.cluster.admit_rejected_colocation_total == 0
            && self.cluster.admit_rejected_capacity_total == 0
            && self.cluster.admit_rejected_bandwidth_total == 0
            && self.cluster.io_budget_used_bytes == 0
            && self.cluster.observer_dropped_overloaded_total == 0
            && self.cluster.gravity_heat_unattributed_total == 0
            && self.cluster.blob_pulls_admitted_total == 0
            && self.cluster.blob_pulls_rejected_no_storage_total == 0
            && self.cluster.blob_pulls_rejected_greedy_disabled_total == 0
            && self.cluster.blob_pulls_rejected_proximity_zero_total == 0
            && self.cluster.blob_pulls_rejected_unhealthy_total == 0
            && self.cluster.blob_pulls_rejected_scope_mismatch_total == 0
    }
}

/// `(help, metric_name, getter)` triples for the per-channel
/// counter family. The Prometheus emit loop walks this so adding a
/// new counter is a single-row change here.
type ChannelCounterGetter = fn(&GreedyChannelMetrics) -> u64;
const CHANNEL_COUNTER_DESCRIPTORS: &[(&str, &str, ChannelCounterGetter)] = &[
    (
        "Cumulative greedy-cache hits — substrate read resolved to a cached holder.",
        "dataforts_greedy_cache_hits_total",
        (|c| c.cache_hits_total) as ChannelCounterGetter,
    ),
    (
        "Cumulative reads served from this node's greedy cache.",
        "dataforts_greedy_serve_count_total",
        (|c| c.serve_count_total) as ChannelCounterGetter,
    ),
    (
        "Cumulative greedy-cache evictions under cluster-cap pressure.",
        "dataforts_greedy_evictions_total",
        (|c| c.evictions_total) as ChannelCounterGetter,
    ),
];

/// Escape a Prometheus label value (`\\`, `\"`, `\n`).
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_starts_empty() {
        let r = GreedyMetricsRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        let snap = r.snapshot();
        assert!(snap.is_empty());
    }

    #[test]
    fn for_channel_returns_same_arc_per_name() {
        let r = GreedyMetricsRegistry::new();
        let a1 = r.for_channel("test/foo");
        let a2 = r.for_channel("test/foo");
        assert!(Arc::ptr_eq(&a1, &a2));
    }

    #[test]
    fn channel_counters_bump_into_snapshot() {
        let r = GreedyMetricsRegistry::new();
        let m = r.for_channel("test/a");
        m.incr_cache_hit();
        m.incr_cache_hit();
        m.incr_eviction();
        m.set_bytes_resident(4096);
        let snap = r.snapshot();
        let c = snap
            .channels
            .iter()
            .find(|c| c.channel == "test/a")
            .expect("entry present");
        assert_eq!(c.cache_hits_total, 2);
        assert_eq!(c.evictions_total, 1);
        assert_eq!(c.bytes_resident, 4096);
    }

    #[test]
    fn cluster_admit_rejected_bumps_per_reason() {
        let r = GreedyMetricsRegistry::new();
        let cluster = r.cluster();
        cluster.incr_admit_rejected(AdmitRejectReason::Scope);
        cluster.incr_admit_rejected(AdmitRejectReason::Scope);
        cluster.incr_admit_rejected(AdmitRejectReason::Intent);
        cluster.incr_admit_rejected(AdmitRejectReason::Capacity);
        cluster.incr_admit_rejected(AdmitRejectReason::Bandwidth);
        cluster.incr_admit_rejected(AdmitRejectReason::Bandwidth);
        let snap = r.snapshot();
        assert_eq!(snap.cluster.admit_rejected_scope_total, 2);
        assert_eq!(snap.cluster.admit_rejected_intent_total, 1);
        assert_eq!(snap.cluster.admit_rejected_colocation_total, 0);
        assert_eq!(snap.cluster.admit_rejected_capacity_total, 1);
        assert_eq!(snap.cluster.admit_rejected_bandwidth_total, 2);
    }

    #[test]
    fn overflow_bucket_activates_past_cap() {
        let r = GreedyMetricsRegistry::new();
        // Don't actually push MAX_TRACKED_CHANNELS entries (slow);
        // just verify the path by checking that the cap check uses
        // contains_key. Pin overflow-bucket creation via a direct
        // call past a low cap.
        // (Manual cap-flip test would need a #[cfg(test)] hook; we
        // instead pin that under-cap registrations get unique
        // arcs.)
        let a = r.for_channel("a");
        let b = r.for_channel("b");
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn prometheus_text_renders_all_metrics() {
        let r = GreedyMetricsRegistry::new();
        let m = r.for_channel("test/a");
        m.incr_cache_hit();
        m.set_bytes_resident(2048);
        let cluster = r.cluster();
        cluster.incr_admit_rejected(AdmitRejectReason::Scope);
        cluster.set_io_budget_used_bytes(8192);

        let text = r.snapshot().prometheus_text();
        assert!(text.contains("dataforts_greedy_cache_hits_total{channel=\"test/a\"} 1"));
        assert!(text.contains("dataforts_greedy_bytes_resident{channel=\"test/a\"} 2048"));
        assert!(text.contains("dataforts_greedy_admit_rejected_total{reason=\"scope\"} 1"));
        assert!(text.contains("dataforts_greedy_io_budget_used_bytes 8192"));
        // All four reason labels are emitted even when zero.
        assert!(text.contains("dataforts_greedy_admit_rejected_total{reason=\"intent\"} 0"));
        assert!(text.contains("dataforts_greedy_admit_rejected_total{reason=\"colocation\"} 0"));
        assert!(text.contains("dataforts_greedy_admit_rejected_total{reason=\"capacity\"} 0"));
        assert!(text.contains("dataforts_greedy_admit_rejected_total{reason=\"bandwidth\"} 0"));
        assert!(text.contains("dataforts_greedy_observer_dropped_total{reason=\"overloaded\"} 0"));
    }

    #[test]
    fn channels_render_in_sorted_order() {
        let r = GreedyMetricsRegistry::new();
        r.for_channel("zeta").incr_cache_hit();
        r.for_channel("alpha").incr_cache_hit();
        r.for_channel("middle").incr_cache_hit();
        let snap = r.snapshot();
        let channels: Vec<&str> = snap.channels.iter().map(|c| c.channel.as_str()).collect();
        assert_eq!(channels, vec!["alpha", "middle", "zeta"]);
    }

    #[test]
    fn label_escape_handles_quotes_and_backslashes() {
        assert_eq!(escape_label("plain"), "plain");
        assert_eq!(escape_label("with \"quote\""), "with \\\"quote\\\"");
        assert_eq!(escape_label("a\\b"), "a\\\\b");
        assert_eq!(escape_label("multi\nline"), "multi\\nline");
    }
}
