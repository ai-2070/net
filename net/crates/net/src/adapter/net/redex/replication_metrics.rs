//! Replication metrics — Phase H scaffolding for
//! `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §11.
//!
//! Defines the seven `dataforts_replication_*` metric shapes the
//! `ReplicationCoordinator` (Phase C) emits, plus a bounded per-
//! channel registry and Prometheus text renderer. Lands ahead of
//! the coordinator so Phase C wires call-sites directly into the
//! frozen metric names — drift between plan §11 and the emitted
//! wire is caught here at scaffolding time, not late in Phase C
//! review.
//!
//! Cardinality model:
//!
//! - Registry is keyed by channel name (string). New channels past
//!   [`MAX_TRACKED_CHANNELS`] fold into a shared `__overflow__`
//!   bucket so an unbounded fanout (e.g. a workload that creates
//!   one channel per request) can't grow the DashMap without
//!   bound. Mirrors the `RpcMetricsRegistry` bounded-cardinality
//!   shape established for the nRPC surface.
//! - Per-channel state is a struct of `AtomicU64`s — read-mostly
//!   hot path is the `tail_lag_seconds` gauge that the heartbeat
//!   ack loop updates once per `heartbeat_ms`.
//!
//! Metrics surface:
//!
//! | Name | Type | Labels |
//! |------|------|--------|
//! | `dataforts_replication_lag_seconds` | gauge | `channel`, `role` |
//! | `dataforts_replication_sync_bytes_total` | counter | `channel` |
//! | `dataforts_leader_changes_total` | counter | `channel` |
//! | `dataforts_replication_under_capacity_total` | counter | `channel` |
//! | `dataforts_replication_skip_ahead_total` | counter | `channel` |
//! | `dataforts_replication_election_thrash_total` | counter | `channel` |
//! | `dataforts_replication_witness_withdrawals_total` | counter | `channel` |
//!
//! Lag gauge is split by `role` so a single channel's
//! leader-side-view-of-lag and replica-side-view-of-lag are
//! observable independently.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

/// Maximum number of channels tracked with their own counter set.
/// Channels past this fold into a single `__overflow__` bucket.
/// The cap mirrors `RpcMetricsRegistry::MAX_TRACKED_SERVICES` shape
/// — generous enough that real deployments rarely hit it; small
/// enough that a misbehaving caller emitting unique channel names
/// can't blow up memory.
pub const MAX_TRACKED_CHANNELS: usize = 1024;

/// Label value used for channels folded past
/// [`MAX_TRACKED_CHANNELS`].
pub const OVERFLOW_CHANNEL_LABEL: &str = "__overflow__";

/// Sentinel for "tail lag not yet observed." Encoded as `u64::MAX`
/// because legitimate lag values fit comfortably below this — the
/// gauge surfaces as `NaN` in [`ReplicationMetricsSnapshot`] when
/// no heartbeat has landed yet, so Prometheus skips emitting the
/// metric until the first observation.
const LAG_NOT_OBSERVED: u64 = u64::MAX;

/// Atomic per-channel counter set. All fields use relaxed ordering
/// — the metrics path is observability-only; lost updates under
/// extreme contention are acceptable.
#[derive(Debug)]
pub struct ChannelMetricsAtomic {
    /// Leader-side view of the channel's tail lag, in microseconds.
    /// `u64::MAX` sentinel = "not yet observed."
    pub leader_lag_micros: AtomicU64,
    /// Replica-side view of the channel's tail lag, in microseconds.
    /// `u64::MAX` sentinel = "not yet observed."
    pub replica_lag_micros: AtomicU64,
    /// Cumulative bytes shipped via `SYNC_RESPONSE`.
    pub sync_bytes_total: AtomicU64,
    /// Cumulative leader elections completed.
    pub leader_changes_total: AtomicU64,
    /// Cumulative times the channel tripped `UnderCapacity` policy.
    pub under_capacity_total: AtomicU64,
    /// Cumulative times a replica skipped instead of replaying a
    /// gap above `skip_threshold`.
    pub skip_ahead_total: AtomicU64,
    /// Cumulative elections triggered within 30 s of the previous
    /// one — saturation indicator.
    pub election_thrash_total: AtomicU64,
    /// Cumulative witness `Mesh::withdraw_chain` calls issued by
    /// this node when it observed a leadership transition.
    pub witness_withdrawals_total: AtomicU64,
}

impl Default for ChannelMetricsAtomic {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelMetricsAtomic {
    /// All counters zeroed; both lag gauges set to the
    /// "not-yet-observed" sentinel.
    pub fn new() -> Self {
        Self {
            leader_lag_micros: AtomicU64::new(LAG_NOT_OBSERVED),
            replica_lag_micros: AtomicU64::new(LAG_NOT_OBSERVED),
            sync_bytes_total: AtomicU64::new(0),
            leader_changes_total: AtomicU64::new(0),
            under_capacity_total: AtomicU64::new(0),
            skip_ahead_total: AtomicU64::new(0),
            election_thrash_total: AtomicU64::new(0),
            witness_withdrawals_total: AtomicU64::new(0),
        }
    }

    /// Record the leader's current view of `tail_seq` lag.
    pub fn record_leader_lag(&self, lag: std::time::Duration) {
        let micros = u64::try_from(lag.as_micros()).unwrap_or(u64::MAX - 1);
        let stored = if micros == LAG_NOT_OBSERVED {
            LAG_NOT_OBSERVED - 1
        } else {
            micros
        };
        self.leader_lag_micros.store(stored, Ordering::Relaxed);
    }

    /// Record the replica's current view of `tail_seq` lag.
    pub fn record_replica_lag(&self, lag: std::time::Duration) {
        let micros = u64::try_from(lag.as_micros()).unwrap_or(u64::MAX - 1);
        let stored = if micros == LAG_NOT_OBSERVED {
            LAG_NOT_OBSERVED - 1
        } else {
            micros
        };
        self.replica_lag_micros.store(stored, Ordering::Relaxed);
    }

    /// Bump the cumulative-bytes counter on a successful
    /// `SYNC_RESPONSE` ship.
    pub fn incr_sync_bytes(&self, bytes: u64) {
        self.sync_bytes_total.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bump the leader-changes counter on every completed election.
    pub fn incr_leader_change(&self) {
        self.leader_changes_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the under-capacity counter on every `UnderCapacity`
    /// policy trip.
    pub fn incr_under_capacity(&self) {
        self.under_capacity_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the skip-ahead counter whenever a replica chooses to
    /// skip rather than replay a gap above `skip_threshold`.
    pub fn incr_skip_ahead(&self) {
        self.skip_ahead_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the election-thrash counter when an election fires
    /// within 30 s of the previous one. The hysteresis-tripping
    /// logic lives in the coordinator (Phase C); this metric is
    /// the operator-facing escape valve.
    pub fn incr_election_thrash(&self) {
        self.election_thrash_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the witness-withdrawal counter when this node speaks
    /// the "previous leader is gone" fact via
    /// `Mesh::withdraw_chain`.
    pub fn incr_witness_withdrawal(&self) {
        self.witness_withdrawals_total
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// Per-channel registry. Bounded by [`MAX_TRACKED_CHANNELS`];
/// channels past the cap fold into a shared `__overflow__` bucket.
///
/// `for_channel(name)` returns the `Arc<ChannelMetricsAtomic>` the
/// coordinator bumps on the hot path — single DashMap get; falls
/// back to entry-API insert on first access for a channel.
pub struct ReplicationMetricsRegistry {
    channels: DashMap<String, Arc<ChannelMetricsAtomic>>,
}

impl Default for ReplicationMetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicationMetricsRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    /// Get-or-create the per-channel counter set for `channel`.
    /// Channels past [`MAX_TRACKED_CHANNELS`] fold into the
    /// `__overflow__` bucket.
    pub fn for_channel(&self, channel: &str) -> Arc<ChannelMetricsAtomic> {
        if let Some(m) = self.channels.get(channel) {
            return m.clone();
        }
        // Cap check BEFORE the entry-API insert: if we're at the
        // limit, fold this call into the overflow bucket. The
        // overflow bucket itself counts as one slot and is created
        // lazily on first overflow — net: at most cap+1 entries.
        if self.channels.len() >= MAX_TRACKED_CHANNELS && !self.channels.contains_key(channel) {
            return self
                .channels
                .entry(OVERFLOW_CHANNEL_LABEL.to_string())
                .or_insert_with(|| Arc::new(ChannelMetricsAtomic::new()))
                .clone();
        }
        self.channels
            .entry(channel.to_string())
            .or_insert_with(|| Arc::new(ChannelMetricsAtomic::new()))
            .clone()
    }

    /// Read-only snapshot — copies the atomic counters into plain
    /// data. Suitable for a per-scrape Prometheus pull; allocation
    /// cost is one `Vec` entry per active channel.
    pub fn snapshot(&self) -> ReplicationMetricsSnapshot {
        let mut channels = Vec::with_capacity(self.channels.len());
        for entry in self.channels.iter() {
            let m = entry.value();
            channels.push(ChannelMetrics {
                channel: entry.key().clone(),
                leader_lag_seconds: load_lag(&m.leader_lag_micros),
                replica_lag_seconds: load_lag(&m.replica_lag_micros),
                sync_bytes_total: m.sync_bytes_total.load(Ordering::Relaxed),
                leader_changes_total: m.leader_changes_total.load(Ordering::Relaxed),
                under_capacity_total: m.under_capacity_total.load(Ordering::Relaxed),
                skip_ahead_total: m.skip_ahead_total.load(Ordering::Relaxed),
                election_thrash_total: m.election_thrash_total.load(Ordering::Relaxed),
                witness_withdrawals_total: m.witness_withdrawals_total.load(Ordering::Relaxed),
            });
        }
        // Stable order — the snapshot's serialized form (and
        // Prometheus text emission) is keyed to it.
        channels.sort_by(|a, b| a.channel.cmp(&b.channel));
        ReplicationMetricsSnapshot { channels }
    }

    /// True if `channel` has been observed (or is the overflow
    /// bucket). Test-only helper; production code shouldn't need it.
    #[cfg(test)]
    pub fn contains(&self, channel: &str) -> bool {
        self.channels.contains_key(channel)
    }

    /// Number of distinct channels currently tracked, including the
    /// overflow bucket if it's been activated.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// True iff no channel has been observed yet.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }
}

/// Read-side per-channel snapshot. `leader_lag_seconds` /
/// `replica_lag_seconds` carry `None` when the heartbeat ack loop
/// hasn't yet observed a value for that side; Prometheus emission
/// skips the gauge for that role when the value is `None`.
#[derive(Debug, Clone)]
pub struct ChannelMetrics {
    /// Channel name (or [`OVERFLOW_CHANNEL_LABEL`] for the bucket).
    pub channel: String,
    /// Leader-side view of `tail_seq` lag in seconds. `None` until
    /// the first heartbeat-driven observation lands.
    pub leader_lag_seconds: Option<f64>,
    /// Replica-side view of `tail_seq` lag in seconds. `None` until
    /// the first heartbeat-driven observation lands.
    pub replica_lag_seconds: Option<f64>,
    /// Cumulative `SYNC_RESPONSE` payload bytes shipped.
    pub sync_bytes_total: u64,
    /// Cumulative completed leader elections.
    pub leader_changes_total: u64,
    /// Cumulative `UnderCapacity` policy trips.
    pub under_capacity_total: u64,
    /// Cumulative skip-ahead replica catchups (gaps above
    /// `skip_threshold`).
    pub skip_ahead_total: u64,
    /// Cumulative elections within 30 s of the previous one.
    pub election_thrash_total: u64,
    /// Cumulative witness `Mesh::withdraw_chain` calls.
    pub witness_withdrawals_total: u64,
}

/// Read-side snapshot of every tracked channel, sorted by channel
/// name. The sorted order is load-bearing for Prometheus text
/// emission (and for byte-stable test goldens).
#[derive(Debug, Clone, Default)]
pub struct ReplicationMetricsSnapshot {
    /// One entry per tracked channel, sorted by channel name.
    pub channels: Vec<ChannelMetrics>,
}

impl ReplicationMetricsSnapshot {
    /// Render in Prometheus text-exposition format. Mirrors the
    /// shape `RpcMetricsRegistry::prometheus_text` uses for the
    /// nRPC surface: a HELP + TYPE block per metric, then one line
    /// per channel.
    ///
    /// Lag gauges are emitted as `..._seconds` per Prometheus
    /// convention (Counter / Gauge units always SI base — seconds
    /// here, bytes for `_bytes_total`). Lag values that haven't yet
    /// been observed are omitted entirely (no `NaN` lines).
    pub fn prometheus_text(&self) -> String {
        let mut out = String::with_capacity(2048);

        // dataforts_replication_lag_seconds (gauge, role-labeled)
        out.push_str(
            "# HELP dataforts_replication_lag_seconds Replica's tail_seq lag behind leader, per role.\n",
        );
        out.push_str("# TYPE dataforts_replication_lag_seconds gauge\n");
        for c in &self.channels {
            if let Some(secs) = c.leader_lag_seconds {
                let _ = writeln!(
                    out,
                    "dataforts_replication_lag_seconds{{channel=\"{}\",role=\"leader\"}} {}",
                    escape_label(&c.channel),
                    format_seconds(secs),
                );
            }
            if let Some(secs) = c.replica_lag_seconds {
                let _ = writeln!(
                    out,
                    "dataforts_replication_lag_seconds{{channel=\"{}\",role=\"replica\"}} {}",
                    escape_label(&c.channel),
                    format_seconds(secs),
                );
            }
        }

        // Single-label counters share the same emit pattern.
        for (help, name, getter) in COUNTER_DESCRIPTORS {
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

        out
    }

    /// Look up the snapshot row for `channel` — `None` if the
    /// channel hasn't been observed (or has folded into the
    /// overflow bucket).
    pub fn channel(&self, channel: &str) -> Option<&ChannelMetrics> {
        self.channels.iter().find(|c| c.channel == channel)
    }

    /// Aggregate every counter across every channel. Useful for
    /// quick smoke tests / overall liveness checks in tests.
    pub fn totals(&self) -> HashMap<&'static str, u64> {
        let mut totals = HashMap::new();
        let mut bump = |k: &'static str, v: u64| {
            *totals.entry(k).or_insert(0) += v;
        };
        for c in &self.channels {
            bump("sync_bytes_total", c.sync_bytes_total);
            bump("leader_changes_total", c.leader_changes_total);
            bump("under_capacity_total", c.under_capacity_total);
            bump("skip_ahead_total", c.skip_ahead_total);
            bump("election_thrash_total", c.election_thrash_total);
            bump("witness_withdrawals_total", c.witness_withdrawals_total);
        }
        totals
    }
}

/// Counter descriptor — `(help, prom_name, getter)`. Drives the
/// uniform Prometheus emission loop.
type CounterGetter = fn(&ChannelMetrics) -> u64;
const COUNTER_DESCRIPTORS: &[(&str, &str, CounterGetter)] = &[
    (
        "Cumulative bytes shipped via SYNC_RESPONSE.",
        "dataforts_replication_sync_bytes_total",
        |c| c.sync_bytes_total,
    ),
    (
        "Number of leader elections completed.",
        "dataforts_leader_changes_total",
        |c| c.leader_changes_total,
    ),
    (
        "Times the channel hit UnderCapacity policy.",
        "dataforts_replication_under_capacity_total",
        |c| c.under_capacity_total,
    ),
    (
        "Times a replica skipped instead of replaying a large gap.",
        "dataforts_replication_skip_ahead_total",
        |c| c.skip_ahead_total,
    ),
    (
        "Elections triggered within 30 s of the previous one (saturation indicator).",
        "dataforts_replication_election_thrash_total",
        |c| c.election_thrash_total,
    ),
    (
        "Times a peer replica issued a witness Mesh::withdraw_chain for a deposed leader's tag.",
        "dataforts_replication_witness_withdrawals_total",
        |c| c.witness_withdrawals_total,
    ),
];

fn load_lag(atomic: &AtomicU64) -> Option<f64> {
    let raw = atomic.load(Ordering::Relaxed);
    if raw == LAG_NOT_OBSERVED {
        None
    } else {
        // f64 conversion is lossless up to 2^53; lag values in
        // microseconds with that ceiling represent ~285 years of
        // lag, which is enough for any operator's lag budget.
        Some(raw as f64 / 1_000_000.0)
    }
}

/// Format an `f64` seconds value with up to 6 digits past the
/// decimal — matches Prometheus's standard double-precision
/// emission.
fn format_seconds(secs: f64) -> String {
    // `{:.6}` would always emit six decimals; the trimming below
    // strips trailing zeros for compact output without losing
    // precision.
    let mut s = format!("{:.6}", secs);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// Escape `s` for use inside a Prometheus label value.
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn registry_starts_empty() {
        let reg = ReplicationMetricsRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn for_channel_is_idempotent() {
        let reg = ReplicationMetricsRegistry::new();
        let a = reg.for_channel("payments");
        let b = reg.for_channel("payments");
        assert!(Arc::ptr_eq(&a, &b), "same channel must return same Arc");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn distinct_channels_get_distinct_counters() {
        let reg = ReplicationMetricsRegistry::new();
        let a = reg.for_channel("payments");
        let b = reg.for_channel("refunds");
        a.incr_leader_change();
        b.incr_leader_change();
        b.incr_leader_change();
        let snap = reg.snapshot();
        let p = snap.channel("payments").expect("payments row");
        let r = snap.channel("refunds").expect("refunds row");
        assert_eq!(p.leader_changes_total, 1);
        assert_eq!(r.leader_changes_total, 2);
    }

    #[test]
    fn counters_increment_in_isolation() {
        let reg = ReplicationMetricsRegistry::new();
        let m = reg.for_channel("c1");
        m.incr_sync_bytes(1024);
        m.incr_sync_bytes(512);
        m.incr_leader_change();
        m.incr_under_capacity();
        m.incr_skip_ahead();
        m.incr_election_thrash();
        m.incr_election_thrash();
        m.incr_witness_withdrawal();
        let snap = reg.snapshot();
        let c = snap.channel("c1").expect("c1 row");
        assert_eq!(c.sync_bytes_total, 1536);
        assert_eq!(c.leader_changes_total, 1);
        assert_eq!(c.under_capacity_total, 1);
        assert_eq!(c.skip_ahead_total, 1);
        assert_eq!(c.election_thrash_total, 2);
        assert_eq!(c.witness_withdrawals_total, 1);
    }

    #[test]
    fn lag_starts_unobserved_then_records() {
        let reg = ReplicationMetricsRegistry::new();
        let m = reg.for_channel("c1");
        let snap = reg.snapshot();
        let c = snap.channel("c1").unwrap();
        assert_eq!(c.leader_lag_seconds, None);
        assert_eq!(c.replica_lag_seconds, None);

        m.record_leader_lag(Duration::from_millis(250));
        m.record_replica_lag(Duration::from_micros(500));
        let snap = reg.snapshot();
        let c = snap.channel("c1").unwrap();
        assert!(
            (c.leader_lag_seconds.unwrap() - 0.25).abs() < 1e-9,
            "leader lag should be 0.25s; got {:?}",
            c.leader_lag_seconds
        );
        assert!(
            (c.replica_lag_seconds.unwrap() - 0.0005).abs() < 1e-9,
            "replica lag should be 0.0005s; got {:?}",
            c.replica_lag_seconds
        );
    }

    #[test]
    fn snapshot_sorts_by_channel_name() {
        let reg = ReplicationMetricsRegistry::new();
        // Insert in non-sorted order; snapshot must emit alphabetical.
        for name in ["zebra", "alpha", "delta", "bravo"] {
            reg.for_channel(name);
        }
        let snap = reg.snapshot();
        let names: Vec<&str> = snap.channels.iter().map(|c| c.channel.as_str()).collect();
        assert_eq!(names, vec!["alpha", "bravo", "delta", "zebra"]);
    }

    #[test]
    fn overflow_folds_past_cap() {
        let reg = ReplicationMetricsRegistry::new();
        // Fill to the cap (each call inserts a distinct channel).
        for i in 0..MAX_TRACKED_CHANNELS {
            reg.for_channel(&format!("channel-{i}"));
        }
        assert_eq!(reg.len(), MAX_TRACKED_CHANNELS);

        // The (cap+1)-th channel folds into the overflow bucket.
        let overflow = reg.for_channel("late-comer");
        overflow.incr_leader_change();
        // Same call from a different name routes to the same bucket.
        let overflow2 = reg.for_channel("another-late-comer");
        overflow2.incr_leader_change();

        assert!(reg.contains(OVERFLOW_CHANNEL_LABEL));
        let snap = reg.snapshot();
        let bucket = snap.channel(OVERFLOW_CHANNEL_LABEL).expect("overflow row");
        assert_eq!(bucket.leader_changes_total, 2);
        // Original channels still tracked under their own keys.
        let original = snap.channel("channel-0").expect("channel-0 row");
        assert_eq!(original.leader_changes_total, 0);
    }

    #[test]
    fn previously_seen_channel_skips_overflow_after_cap() {
        let reg = ReplicationMetricsRegistry::new();
        // Pre-seed a channel before filling.
        reg.for_channel("known").incr_leader_change();
        for i in 0..MAX_TRACKED_CHANNELS - 1 {
            reg.for_channel(&format!("channel-{i}"));
        }
        // We're at the cap; another `for_channel("known")` call must
        // route to the existing entry, not the overflow bucket.
        let known = reg.for_channel("known");
        known.incr_leader_change();
        let snap = reg.snapshot();
        let c = snap.channel("known").expect("known row");
        assert_eq!(c.leader_changes_total, 2);
        assert!(snap.channel(OVERFLOW_CHANNEL_LABEL).is_none());
    }

    #[test]
    fn prometheus_text_emits_every_metric() {
        let reg = ReplicationMetricsRegistry::new();
        let m = reg.for_channel("payments/settlements");
        m.incr_sync_bytes(2048);
        m.incr_leader_change();
        m.incr_leader_change();
        m.incr_under_capacity();
        m.incr_skip_ahead();
        m.incr_election_thrash();
        m.incr_witness_withdrawal();
        m.record_leader_lag(Duration::from_millis(125));
        m.record_replica_lag(Duration::from_secs(2));
        let text = reg.snapshot().prometheus_text();

        // Every metric name appears.
        for name in [
            "dataforts_replication_lag_seconds",
            "dataforts_replication_sync_bytes_total",
            "dataforts_leader_changes_total",
            "dataforts_replication_under_capacity_total",
            "dataforts_replication_skip_ahead_total",
            "dataforts_replication_election_thrash_total",
            "dataforts_replication_witness_withdrawals_total",
        ] {
            assert!(
                text.contains(name),
                "metric {name} missing from emission: {text}"
            );
        }

        // Channel label is present + value lines match the recorded
        // counters.
        assert!(text.contains("channel=\"payments/settlements\""));
        assert!(text.contains(
            "dataforts_replication_sync_bytes_total{channel=\"payments/settlements\"} 2048"
        ));
        assert!(text.contains("dataforts_leader_changes_total{channel=\"payments/settlements\"} 2"));

        // Lag gauge carries both roles when both are observed.
        assert!(text.contains("role=\"leader\""));
        assert!(text.contains("role=\"replica\""));

        // HELP + TYPE blocks per metric.
        let help_lines = text.matches("# HELP ").count();
        let type_lines = text.matches("# TYPE ").count();
        assert_eq!(help_lines, 7, "expected 7 HELP lines, got {help_lines}");
        assert_eq!(type_lines, 7, "expected 7 TYPE lines, got {type_lines}");
    }

    #[test]
    fn prometheus_text_omits_unobserved_lag_roles() {
        // Only the leader-side lag is recorded; emission must
        // include the leader line and OMIT the replica line entirely
        // (no NaN, no zero — the role just isn't reported yet).
        let reg = ReplicationMetricsRegistry::new();
        let m = reg.for_channel("c1");
        m.record_leader_lag(Duration::from_millis(10));
        let text = reg.snapshot().prometheus_text();
        assert!(text.contains("role=\"leader\""));
        assert!(
            !text.contains("role=\"replica\""),
            "unobserved replica lag must not emit: {text}",
        );
    }

    #[test]
    fn prometheus_text_escapes_label_quotes_and_backslashes() {
        let reg = ReplicationMetricsRegistry::new();
        reg.for_channel(r#"weird/name"with"quotes\and\slashes"#)
            .incr_leader_change();
        let text = reg.snapshot().prometheus_text();
        // Both `"` and `\` must be escaped.
        assert!(
            text.contains(r#"channel=\"weird/name\\\"with\\\"quotes\\\\and\\\\slashes\""#)
                || text.contains(r#"channel="weird/name\"with\"quotes\\and\\slashes""#)
        );
    }

    #[test]
    fn totals_aggregates_across_channels() {
        let reg = ReplicationMetricsRegistry::new();
        let a = reg.for_channel("a");
        let b = reg.for_channel("b");
        a.incr_sync_bytes(100);
        b.incr_sync_bytes(200);
        a.incr_leader_change();
        b.incr_leader_change();
        b.incr_leader_change();
        let snap = reg.snapshot();
        let totals = snap.totals();
        assert_eq!(totals.get("sync_bytes_total").copied(), Some(300));
        assert_eq!(totals.get("leader_changes_total").copied(), Some(3));
    }

    #[test]
    fn lag_record_saturating_on_oversize_duration() {
        // A Duration that overflows `u64::try_from(as_micros())`
        // would otherwise panic via `unwrap`; we saturate to a safe
        // sentinel-adjacent value to keep the metrics path
        // panic-free.
        let reg = ReplicationMetricsRegistry::new();
        let m = reg.for_channel("c1");
        // Duration::MAX overflows u64 micros; record_leader_lag
        // must not panic.
        m.record_leader_lag(Duration::MAX);
        let snap = reg.snapshot();
        let c = snap.channel("c1").unwrap();
        assert!(
            c.leader_lag_seconds.is_some(),
            "lag must be Some even at saturation"
        );
    }
}
