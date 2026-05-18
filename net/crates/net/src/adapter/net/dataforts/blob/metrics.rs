//! Prometheus-shaped counters + gauges for the mesh-native blob
//! adapter. Per the plan (`docs/plans/DATAFORTS_BLOB_STORAGE_PLAN.md`
//! § 6 / § 11), every adapter + node exposes:
//!
//! - `dataforts_blobs_stored_total{adapter}` — `store` /
//!   `store_stream` success count.
//! - `dataforts_blobs_fetched_total{adapter}` — `fetch` /
//!   `fetch_range` success count.
//! - `dataforts_blob_bytes_stored_total{adapter}` — bytes accepted
//!   by `store` (size of the stored payload).
//! - `dataforts_blob_gc_swept_total{adapter}` — number of blobs
//!   removed by the GC sweep.
//! - `dataforts_blob_gc_pending_total{adapter}` — current count
//!   of zero-refcount entries waiting on the retention floor.
//!   Snapshotted on demand.
//! - `dataforts_blob_disk_used_bytes{adapter}` — bytes the
//!   adapter currently holds locally. Updated by the operator's
//!   heartbeat-cadence disk-pressure update.
//! - `dataforts_blob_disk_capacity_bytes{adapter}` — operator-
//!   configured cap (`MeshBlobAdapter::with_disk_capacity`).
//!
//! Atomic counters live behind `AtomicU64::Relaxed` — the relaxed
//! ordering is correct because the operator-facing read is a
//! snapshot, not a coherent multi-counter view.
//!
//! `dataforts_blob_replication_lag_ms` + `bytes_replicated_total`
//! land alongside the cross-node replication wiring in PR-5; this
//! module ships the local-only counters today.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Per-adapter atomic counter registry. Cheap to clone
/// (`Arc<MetricsInner>`); intended to be shared across the
/// adapter's hot-path methods + the operator-facing snapshot
/// reader.
#[derive(Clone, Debug, Default)]
pub struct BlobMetrics {
    inner: Arc<MetricsInner>,
}

#[derive(Debug, Default)]
struct MetricsInner {
    blobs_stored_total: AtomicU64,
    blobs_fetched_total: AtomicU64,
    bytes_stored_total: AtomicU64,
    gc_swept_total: AtomicU64,
    disk_used_bytes: AtomicU64,
    disk_capacity_bytes: AtomicU64,
    // --- v0.3 active-overflow counter family ---
    // Operators dashboard `pushes_admitted_total` against
    // `push_errors_total` to spot send-side failure rates;
    // the per-reason `rejected_*` counters break out the
    // receive-side admission verdict. `high_water_triggered`
    // / `low_water_cleared` show the hysteresis state-machine
    // transitions; `active` is a 0/1 gauge for "is the
    // controller actively shedding right now?".
    overflow_pushes_admitted_total: AtomicU64,
    overflow_push_errors_total: AtomicU64,
    overflow_pushed_bytes_total: AtomicU64,
    overflow_rejected_no_target_total: AtomicU64,
    overflow_rejected_no_storage_cap_total: AtomicU64,
    overflow_rejected_not_participating_total: AtomicU64,
    overflow_rejected_sender_not_overflowing_total: AtomicU64,
    overflow_rejected_unhealthy_total: AtomicU64,
    overflow_rejected_scope_mismatch_total: AtomicU64,
    overflow_rejected_insufficient_disk_total: AtomicU64,
    overflow_high_water_triggered_total: AtomicU64,
    overflow_low_water_cleared_total: AtomicU64,
    // 0/1 gauge — set by the tick driver after each tick.
    // Doesn't decay; the next tick re-asserts the current
    // state. Operators dashboarding `overflow_active` see
    // the live hysteresis state.
    overflow_active: AtomicU64,
    // Disk-usage ratio × 1000 (so `850` = 0.85). Stored as
    // u64 to share the atomic shape with other gauges;
    // operators format-rendering divide by 1000 on the way
    // out. Set by the tick driver at the end of each tick.
    overflow_disk_ratio_x1000: AtomicU64,
}

impl BlobMetrics {
    /// Construct an empty registry. Counters start at zero;
    /// gauges (`disk_*_bytes`) start at zero and are filled in
    /// by the operator's heartbeat-cadence update.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner::default()),
        }
    }

    /// Snapshot the registry. Returns a [`BlobMetricsSnapshot`]
    /// — point-in-time copy of every counter / gauge. The
    /// `gc_pending_total` field comes from the caller (the
    /// refcount table tracks zero-refcount entries; metrics
    /// isn't authoritative for that).
    pub fn snapshot(&self) -> BlobMetricsSnapshot {
        let inner = &self.inner;
        BlobMetricsSnapshot {
            blobs_stored_total: inner.blobs_stored_total.load(Ordering::Relaxed),
            blobs_fetched_total: inner.blobs_fetched_total.load(Ordering::Relaxed),
            bytes_stored_total: inner.bytes_stored_total.load(Ordering::Relaxed),
            gc_swept_total: inner.gc_swept_total.load(Ordering::Relaxed),
            disk_used_bytes: inner.disk_used_bytes.load(Ordering::Relaxed),
            disk_capacity_bytes: inner.disk_capacity_bytes.load(Ordering::Relaxed),
            overflow: OverflowMetricsSnapshot {
                pushes_admitted_total: inner.overflow_pushes_admitted_total.load(Ordering::Relaxed),
                push_errors_total: inner.overflow_push_errors_total.load(Ordering::Relaxed),
                pushed_bytes_total: inner.overflow_pushed_bytes_total.load(Ordering::Relaxed),
                rejected_no_target_total: inner
                    .overflow_rejected_no_target_total
                    .load(Ordering::Relaxed),
                rejected_no_storage_cap_total: inner
                    .overflow_rejected_no_storage_cap_total
                    .load(Ordering::Relaxed),
                rejected_not_participating_total: inner
                    .overflow_rejected_not_participating_total
                    .load(Ordering::Relaxed),
                rejected_sender_not_overflowing_total: inner
                    .overflow_rejected_sender_not_overflowing_total
                    .load(Ordering::Relaxed),
                rejected_unhealthy_total: inner
                    .overflow_rejected_unhealthy_total
                    .load(Ordering::Relaxed),
                rejected_scope_mismatch_total: inner
                    .overflow_rejected_scope_mismatch_total
                    .load(Ordering::Relaxed),
                rejected_insufficient_disk_total: inner
                    .overflow_rejected_insufficient_disk_total
                    .load(Ordering::Relaxed),
                high_water_triggered_total: inner
                    .overflow_high_water_triggered_total
                    .load(Ordering::Relaxed),
                low_water_cleared_total: inner
                    .overflow_low_water_cleared_total
                    .load(Ordering::Relaxed),
                active: inner.overflow_active.load(Ordering::Relaxed) != 0,
                disk_ratio: inner.overflow_disk_ratio_x1000.load(Ordering::Relaxed) as f64 / 1000.0,
            },
        }
    }

    /// Increment `dataforts_blobs_stored_total` by 1 and bump
    /// `dataforts_blob_bytes_stored_total` by `size`. Called from
    /// the adapter's `store` success path; the helper bundles
    /// both bumps so they atomic-add in lockstep (operators
    /// reading `count` + `bytes` see consistent ratios).
    pub fn record_store(&self, size: u64) {
        self.inner
            .blobs_stored_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .bytes_stored_total
            .fetch_add(size, Ordering::Relaxed);
    }

    /// Increment `dataforts_blobs_fetched_total` by 1.
    pub fn record_fetch(&self) {
        self.inner
            .blobs_fetched_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Increment `dataforts_blob_gc_swept_total` by `count`.
    /// Called once per sweep with the size of the deleted set
    /// (often 0 if nothing to sweep).
    pub fn record_gc_swept(&self, count: u64) {
        self.inner
            .gc_swept_total
            .fetch_add(count, Ordering::Relaxed);
    }

    /// Set `dataforts_blob_disk_used_bytes` to `bytes`. Caller
    /// invokes on the heartbeat cadence (default 5 s).
    pub fn set_disk_used_bytes(&self, bytes: u64) {
        self.inner.disk_used_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Set `dataforts_blob_disk_capacity_bytes` to `bytes`. Caller
    /// sets once at adapter construction; subsequent operator
    /// updates land here too.
    pub fn set_disk_capacity_bytes(&self, bytes: u64) {
        self.inner
            .disk_capacity_bytes
            .store(bytes, Ordering::Relaxed);
    }

    /// Apply a [`BlobOverflowTickReport`] to the overflow
    /// counter family. The tick driver calls this once per
    /// tick — bumps the per-reason counters by their report
    /// deltas, sets the `active` + `disk_ratio` gauges from
    /// the post-tick state, and records hysteresis
    /// transitions (high-water trigger / low-water clear).
    ///
    /// Transitions: `high_water_triggered_total` bumps when
    /// `!was_active_at_start && is_active_at_end`;
    /// `low_water_cleared_total` bumps when
    /// `was_active_at_start && !is_active_at_end`. The two
    /// counters together let operators count distinct
    /// "overflow episodes" (every trigger paired with its
    /// eventual clear).
    ///
    /// [`BlobOverflowTickReport`]: super::overflow::BlobOverflowTickReport
    pub fn record_overflow_tick(&self, report: &super::overflow::BlobOverflowTickReport) {
        let inner = &self.inner;
        inner
            .overflow_pushes_admitted_total
            .fetch_add(report.admitted, Ordering::Relaxed);
        inner
            .overflow_push_errors_total
            .fetch_add(report.push_errors, Ordering::Relaxed);
        inner
            .overflow_pushed_bytes_total
            .fetch_add(report.pushed_bytes, Ordering::Relaxed);
        inner
            .overflow_rejected_no_target_total
            .fetch_add(report.rejected_no_target, Ordering::Relaxed);
        // Hysteresis transitions: count distinct edge events,
        // not steady-state ticks. Repeated active-during ticks
        // don't bump either counter; only the edge does.
        if !report.was_active_at_start && report.is_active_at_end {
            inner
                .overflow_high_water_triggered_total
                .fetch_add(1, Ordering::Relaxed);
        }
        if report.was_active_at_start && !report.is_active_at_end {
            inner
                .overflow_low_water_cleared_total
                .fetch_add(1, Ordering::Relaxed);
        }
        // Gauges: post-tick state.
        inner.overflow_active.store(
            if report.is_active_at_end { 1 } else { 0 },
            Ordering::Relaxed,
        );
        // Clamp ratio to `[0.0, 10.0]` then scale by 1000.
        // 10.0 is a generous ceiling — `disk_used > disk_total`
        // shouldn't happen but defends against an operator
        // misconfiguring `set_disk_capacity_bytes(small)` with
        // an already-large `disk_used`. Defend against `f64::NAN`
        // by replacing with 0 — `(NaN * 1000.0) as u64` casts to
        // 0 in Rust today but the spec is unsettled and a future
        // change would silently corrupt the gauge.
        let raw = report.disk_ratio_at_end;
        let ratio = if raw.is_nan() {
            0.0
        } else {
            raw.clamp(0.0, 10.0)
        };
        inner
            .overflow_disk_ratio_x1000
            .store((ratio * 1000.0) as u64, Ordering::Relaxed);
    }

    /// Increment a per-reason rejection counter by 1. Called
    /// from the receive-side handler when admission rejects;
    /// the sender-side `push_errors_total` aggregates send-side
    /// failures (RPC transport + non-`Accepted` acks). The two
    /// surfaces are complementary — a sender observes the
    /// receiver's rejection through `push_errors`, and the
    /// receiver records the same event through this method, so
    /// dashboards on both sides see matching volumes.
    pub fn record_overflow_reject(&self, reason: super::admission::OverflowReject) {
        use super::admission::OverflowReject as R;
        let counter = match reason {
            R::NoStorageCap => &self.inner.overflow_rejected_no_storage_cap_total,
            R::NotParticipating => &self.inner.overflow_rejected_not_participating_total,
            R::SenderNotOverflowing => &self.inner.overflow_rejected_sender_not_overflowing_total,
            R::Unhealthy => &self.inner.overflow_rejected_unhealthy_total,
            R::ScopeMismatch => &self.inner.overflow_rejected_scope_mismatch_total,
            R::InsufficientDisk => &self.inner.overflow_rejected_insufficient_disk_total,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Point-in-time snapshot of every adapter counter / gauge. The
/// adapter takes one via [`BlobMetrics::snapshot`] when an
/// operator scrapes; the snapshot decouples the scrape format
/// (Prometheus text / OTel / JSON) from the atomic-counter
/// layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BlobMetricsSnapshot {
    /// `dataforts_blobs_stored_total{adapter}`.
    pub blobs_stored_total: u64,
    /// `dataforts_blobs_fetched_total{adapter}`.
    pub blobs_fetched_total: u64,
    /// `dataforts_blob_bytes_stored_total{adapter}`.
    pub bytes_stored_total: u64,
    /// `dataforts_blob_gc_swept_total{adapter}`.
    pub gc_swept_total: u64,
    /// `dataforts_blob_disk_used_bytes{adapter}`.
    pub disk_used_bytes: u64,
    /// `dataforts_blob_disk_capacity_bytes{adapter}`.
    pub disk_capacity_bytes: u64,
    /// v0.3 active-overflow counter family.
    pub overflow: OverflowMetricsSnapshot,
}

/// Point-in-time snapshot of the v0.3 active-overflow counter
/// family. Carried inside [`BlobMetricsSnapshot::overflow`];
/// operators emit the body via the parent's
/// [`BlobMetricsSnapshot::to_prometheus_text`] which includes
/// these counters.
///
/// `PartialEq` (not `Eq`) because `disk_ratio` is `f64`. Other
/// fields are pure counters / gauges where `Eq` would otherwise
/// hold.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OverflowMetricsSnapshot {
    /// `dataforts_blob_overflow_pushes_admitted_total{adapter}`.
    /// Counter: per successful send-side push (ack == Accepted).
    pub pushes_admitted_total: u64,
    /// `dataforts_blob_overflow_push_errors_total{adapter}`.
    /// Counter: per send-side failure (any non-Accepted ack +
    /// RPC transport errors).
    pub push_errors_total: u64,
    /// `dataforts_blob_overflow_pushed_bytes_total{adapter}`.
    /// Counter: sum of `size_bytes` across successful pushes.
    pub pushed_bytes_total: u64,
    /// `dataforts_blob_overflow_rejected_no_target_total{adapter}`.
    /// Counter: tick computed a cold-hash candidate but no
    /// overflow-enabled peer was reachable for it.
    pub rejected_no_target_total: u64,
    /// `dataforts_blob_overflow_rejected_total{adapter,reason="no_storage_cap"}`.
    /// Counter: per receive-side admission rejection on the
    /// `NoStorageCap` reason.
    pub rejected_no_storage_cap_total: u64,
    /// `…{reason="not_participating"}`. Local node doesn't
    /// carry `dataforts.blob.overflow`.
    pub rejected_not_participating_total: u64,
    /// `…{reason="sender_not_overflowing"}`. Sender doesn't
    /// carry `dataforts.blob.overflow`.
    pub rejected_sender_not_overflowing_total: u64,
    /// `…{reason="unhealthy"}`. Local node advertising
    /// `dataforts:blob-storage-unhealthy`.
    pub rejected_unhealthy_total: u64,
    /// `…{reason="scope_mismatch"}`. Sender's scope is outside
    /// the local gravity scope.
    pub rejected_scope_mismatch_total: u64,
    /// `…{reason="insufficient_disk"}`. Local `disk_free_gb`
    /// insufficient for the chunk.
    pub rejected_insufficient_disk_total: u64,
    /// `dataforts_blob_overflow_high_water_triggered_total{adapter}`.
    /// Counter: per `false → true` hysteresis transition.
    pub high_water_triggered_total: u64,
    /// `dataforts_blob_overflow_low_water_cleared_total{adapter}`.
    /// Counter: per `true → false` hysteresis transition.
    pub low_water_cleared_total: u64,
    /// `dataforts_blob_overflow_active{adapter}`. Gauge `0/1`:
    /// `1` while the controller is actively shedding.
    pub active: bool,
    /// `dataforts_blob_overflow_disk_ratio{adapter}`. Gauge
    /// `0.0..=1.0` typically (clamped to `[0.0, 10.0]`
    /// internally as defense against misconfiguration). Set
    /// to `disk_ratio_at_end` after each tick.
    pub disk_ratio: f64,
}

impl BlobMetricsSnapshot {
    /// Render as Prometheus text. Concatenates to the per-Redex
    /// scrape body via `MeshBlobAdapter::prometheus_text`. The
    /// `gc_pending_total` field comes from the refcount table —
    /// pass it in as a separate argument so this struct stays
    /// snapshottable from `BlobMetrics` alone.
    pub fn to_prometheus_text(&self, adapter_id: &str, gc_pending_total: u64) -> String {
        // Operator-supplied `adapter_id` is interpolated into
        // Prometheus label values. Escape per the text-exposition
        // spec (`\\`, `\"`, `\n`) so a `--adapter-id 'evil"\n#bogus'`
        // input can't inject fake metric lines / labels.
        let label = escape_prometheus_label(adapter_id);
        let label = label.as_str();
        let mut out = String::new();
        out.push_str(&format!(
            "# HELP dataforts_blobs_stored_total Successful blob stores.\n\
             # TYPE dataforts_blobs_stored_total counter\n\
             dataforts_blobs_stored_total{{adapter=\"{}\"}} {}\n",
            label, self.blobs_stored_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blobs_fetched_total Successful blob fetches.\n\
             # TYPE dataforts_blobs_fetched_total counter\n\
             dataforts_blobs_fetched_total{{adapter=\"{}\"}} {}\n",
            label, self.blobs_fetched_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_bytes_stored_total Bytes accepted by store.\n\
             # TYPE dataforts_blob_bytes_stored_total counter\n\
             dataforts_blob_bytes_stored_total{{adapter=\"{}\"}} {}\n",
            label, self.bytes_stored_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_gc_swept_total Blobs removed by GC sweep.\n\
             # TYPE dataforts_blob_gc_swept_total counter\n\
             dataforts_blob_gc_swept_total{{adapter=\"{}\"}} {}\n",
            label, self.gc_swept_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_gc_pending Zero-refcount blobs waiting on retention floor.\n\
             # TYPE dataforts_blob_gc_pending gauge\n\
             dataforts_blob_gc_pending{{adapter=\"{}\"}} {}\n",
            label, gc_pending_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_disk_used_bytes Bytes the adapter currently holds.\n\
             # TYPE dataforts_blob_disk_used_bytes gauge\n\
             dataforts_blob_disk_used_bytes{{adapter=\"{}\"}} {}\n",
            label, self.disk_used_bytes
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_disk_capacity_bytes Operator-configured disk cap.\n\
             # TYPE dataforts_blob_disk_capacity_bytes gauge\n\
             dataforts_blob_disk_capacity_bytes{{adapter=\"{}\"}} {}\n",
            label, self.disk_capacity_bytes
        ));
        // v0.3 active-overflow counter family.
        let o = &self.overflow;
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_pushes_admitted_total Successful overflow pushes (Accepted ack).\n\
             # TYPE dataforts_blob_overflow_pushes_admitted_total counter\n\
             dataforts_blob_overflow_pushes_admitted_total{{adapter=\"{}\"}} {}\n",
            label, o.pushes_admitted_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_push_errors_total Send-side overflow failures (non-Accepted ack + transport errors).\n\
             # TYPE dataforts_blob_overflow_push_errors_total counter\n\
             dataforts_blob_overflow_push_errors_total{{adapter=\"{}\"}} {}\n",
            label, o.push_errors_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_pushed_bytes_total Bytes pushed via overflow (sum of size_bytes on Accepted).\n\
             # TYPE dataforts_blob_overflow_pushed_bytes_total counter\n\
             dataforts_blob_overflow_pushed_bytes_total{{adapter=\"{}\"}} {}\n",
            label, o.pushed_bytes_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_rejected_no_target_total Tick computed a cold candidate but no overflow-enabled peer was reachable.\n\
             # TYPE dataforts_blob_overflow_rejected_no_target_total counter\n\
             dataforts_blob_overflow_rejected_no_target_total{{adapter=\"{}\"}} {}\n",
            label, o.rejected_no_target_total
        ));
        // Per-reason rejection family. Operators sum over the
        // label to compare against `pushes_admitted_total +
        // push_errors_total`; the breakdown lets them target a
        // specific reject mode (e.g. ScopeMismatch suggests a
        // capability misconfiguration on the peer).
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_rejected_total Receive-side admission rejections by reason.\n\
             # TYPE dataforts_blob_overflow_rejected_total counter\n\
             dataforts_blob_overflow_rejected_total{{adapter=\"{}\",reason=\"no_storage_cap\"}} {}\n\
             dataforts_blob_overflow_rejected_total{{adapter=\"{}\",reason=\"not_participating\"}} {}\n\
             dataforts_blob_overflow_rejected_total{{adapter=\"{}\",reason=\"sender_not_overflowing\"}} {}\n\
             dataforts_blob_overflow_rejected_total{{adapter=\"{}\",reason=\"unhealthy\"}} {}\n\
             dataforts_blob_overflow_rejected_total{{adapter=\"{}\",reason=\"scope_mismatch\"}} {}\n\
             dataforts_blob_overflow_rejected_total{{adapter=\"{}\",reason=\"insufficient_disk\"}} {}\n",
            label, o.rejected_no_storage_cap_total,
            label, o.rejected_not_participating_total,
            label, o.rejected_sender_not_overflowing_total,
            label, o.rejected_unhealthy_total,
            label, o.rejected_scope_mismatch_total,
            label, o.rejected_insufficient_disk_total,
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_high_water_triggered_total Hysteresis transitions from inactive to active (false -> true).\n\
             # TYPE dataforts_blob_overflow_high_water_triggered_total counter\n\
             dataforts_blob_overflow_high_water_triggered_total{{adapter=\"{}\"}} {}\n",
            label, o.high_water_triggered_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_low_water_cleared_total Hysteresis transitions from active to inactive (true -> false).\n\
             # TYPE dataforts_blob_overflow_low_water_cleared_total counter\n\
             dataforts_blob_overflow_low_water_cleared_total{{adapter=\"{}\"}} {}\n",
            label, o.low_water_cleared_total
        ));
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_active 1 iff the overflow tick is actively shedding (hysteresis high).\n\
             # TYPE dataforts_blob_overflow_active gauge\n\
             dataforts_blob_overflow_active{{adapter=\"{}\"}} {}\n",
            label, if o.active { 1 } else { 0 }
        ));
        // disk_ratio: ratio in [0, 10] with 3 fractional
        // digits. The `:.3` format is bounded — Prometheus
        // accepts free-form floats but tools that bucket by
        // string compare benefit from a stable rendering.
        out.push_str(&format!(
            "# HELP dataforts_blob_overflow_disk_ratio Local disk usage ratio observed at the most recent tick.\n\
             # TYPE dataforts_blob_overflow_disk_ratio gauge\n\
             dataforts_blob_overflow_disk_ratio{{adapter=\"{}\"}} {:.3}\n",
            label, o.disk_ratio
        ));
        out
    }
}

/// Escape a string for use as a Prometheus label value, per the
/// text-exposition spec: backslash, double-quote, newline, and
/// carriage return each get a backslash prefix. Other characters
/// are passed through unchanged. Used by the metrics emitter to
/// defang operator-supplied `adapter_id` values that could
/// otherwise inject new metric lines into the scrape body.
///
/// `\r` is escaped in addition to the spec-required set because
/// a raw CR before LF is a legitimate line terminator on
/// Windows-aware downstream parsers; suppressing it alone would
/// allow CRLF-shaped injection through an operator-supplied
/// adapter_id even with `\n` neutered.
fn escape_prometheus_label(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// Health-gate verdict from [`evaluate_health_gate`]. Per the
/// plan, the node advertises the `dataforts:blob-storage-unhealthy`
/// reserved tag when local disk crosses 95 %, and clears it when
/// disk drops below 85 % (hysteresis).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthGateAction {
    /// Emit the `dataforts:blob-storage-unhealthy` tag — disk is
    /// at or above the emit threshold (95 % by default).
    Emit,
    /// Clear the tag — disk has dropped at or below the clear
    /// threshold (85 % by default).
    Clear,
    /// No change — disk is between the two thresholds (or
    /// emit / clear semantics already match the prior state).
    Unchanged,
}

/// Default emit threshold — node disk usage at or above this
/// fraction advertises `dataforts:blob-storage-unhealthy`.
pub const HEALTH_GATE_EMIT_THRESHOLD: f64 = 0.95;

/// Default clear threshold — node disk usage at or below this
/// fraction clears the unhealthy tag (hysteresis).
pub const HEALTH_GATE_CLEAR_THRESHOLD: f64 = 0.85;

/// Compute the [`HealthGateAction`] for the current disk usage +
/// the previously-advertised state. Pure logic — the caller is
/// responsible for actually emitting / clearing the tag via the
/// mesh's capability-announcement path.
///
/// Hysteresis pins:
/// - usage >= 95 % → Emit (regardless of prior state). Operator
///   sees the unhealthy advertisement on the next heartbeat.
/// - usage <= 85 % → Clear (regardless of prior state).
/// - 85 % < usage < 95 % → Unchanged (avoid flapping when
///   disk usage oscillates in the band).
///
/// `used_bytes == 0` and `capacity_bytes == 0` both treated as
/// "no opinion" (Unchanged) — uninitialized adapter doesn't
/// fire the gate.
pub fn evaluate_health_gate(
    used_bytes: u64,
    capacity_bytes: u64,
    currently_unhealthy: bool,
) -> HealthGateAction {
    if capacity_bytes == 0 {
        return HealthGateAction::Unchanged;
    }
    let usage = used_bytes as f64 / capacity_bytes as f64;
    if usage >= HEALTH_GATE_EMIT_THRESHOLD {
        if currently_unhealthy {
            HealthGateAction::Unchanged
        } else {
            HealthGateAction::Emit
        }
    } else if usage <= HEALTH_GATE_CLEAR_THRESHOLD {
        if currently_unhealthy {
            HealthGateAction::Clear
        } else {
            HealthGateAction::Unchanged
        }
    } else {
        // Inside the hysteresis band — preserve the current state.
        HealthGateAction::Unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_store_bumps_both_counters() {
        let m = BlobMetrics::new();
        m.record_store(1024);
        m.record_store(2048);
        let s = m.snapshot();
        assert_eq!(s.blobs_stored_total, 2);
        assert_eq!(s.bytes_stored_total, 1024 + 2048);
    }

    #[test]
    fn record_fetch_bumps_only_fetched() {
        let m = BlobMetrics::new();
        m.record_fetch();
        m.record_fetch();
        m.record_fetch();
        let s = m.snapshot();
        assert_eq!(s.blobs_fetched_total, 3);
        assert_eq!(s.blobs_stored_total, 0);
        assert_eq!(s.bytes_stored_total, 0);
    }

    #[test]
    fn record_gc_swept_accumulates() {
        let m = BlobMetrics::new();
        m.record_gc_swept(5);
        m.record_gc_swept(0); // empty sweep still records
        m.record_gc_swept(3);
        let s = m.snapshot();
        assert_eq!(s.gc_swept_total, 8);
    }

    #[test]
    fn disk_gauges_set_overwrites() {
        let m = BlobMetrics::new();
        m.set_disk_capacity_bytes(1 << 30); // 1 GiB
        m.set_disk_used_bytes(100);
        m.set_disk_used_bytes(200); // overwrite, not add
        let s = m.snapshot();
        assert_eq!(s.disk_capacity_bytes, 1 << 30);
        assert_eq!(s.disk_used_bytes, 200);
    }

    #[test]
    fn prometheus_text_includes_every_field() {
        let m = BlobMetrics::new();
        m.record_store(1024);
        m.record_fetch();
        m.record_gc_swept(2);
        m.set_disk_capacity_bytes(1 << 30);
        m.set_disk_used_bytes(1 << 28);
        let text = m.snapshot().to_prometheus_text("my-adapter", 7);
        assert!(text.contains("dataforts_blobs_stored_total{adapter=\"my-adapter\"} 1"));
        assert!(text.contains("dataforts_blobs_fetched_total{adapter=\"my-adapter\"} 1"));
        assert!(text.contains("dataforts_blob_bytes_stored_total{adapter=\"my-adapter\"} 1024"));
        assert!(text.contains("dataforts_blob_gc_swept_total{adapter=\"my-adapter\"} 2"));
        assert!(text.contains("dataforts_blob_gc_pending{adapter=\"my-adapter\"} 7"));
        assert!(text.contains("dataforts_blob_disk_capacity_bytes{adapter=\"my-adapter\"}"));
        assert!(text.contains("dataforts_blob_disk_used_bytes{adapter=\"my-adapter\"}"));
    }

    // --- Health-gate hysteresis ---

    #[test]
    fn health_gate_unhealthy_capacity_zero_is_unchanged() {
        assert_eq!(
            evaluate_health_gate(100, 0, false),
            HealthGateAction::Unchanged
        );
        assert_eq!(
            evaluate_health_gate(100, 0, true),
            HealthGateAction::Unchanged
        );
    }

    #[test]
    fn health_gate_emit_when_over_95_percent_and_currently_healthy() {
        // 96 / 100 = 96 % >= 95 % emit threshold; not currently
        // unhealthy → Emit.
        assert_eq!(evaluate_health_gate(96, 100, false), HealthGateAction::Emit);
    }

    #[test]
    fn health_gate_unchanged_when_already_unhealthy_and_over_95() {
        // Already unhealthy + still over 95 % → no re-emit needed.
        assert_eq!(
            evaluate_health_gate(96, 100, true),
            HealthGateAction::Unchanged
        );
    }

    #[test]
    fn health_gate_clear_when_under_85_percent_and_currently_unhealthy() {
        // 50 / 100 = 50 % <= 85 % clear threshold; unhealthy →
        // Clear.
        assert_eq!(evaluate_health_gate(50, 100, true), HealthGateAction::Clear);
    }

    #[test]
    fn health_gate_unchanged_inside_hysteresis_band() {
        // 90 / 100 = 90 %, between 85 % clear and 95 % emit.
        // Both prior states → Unchanged.
        assert_eq!(
            evaluate_health_gate(90, 100, false),
            HealthGateAction::Unchanged
        );
        assert_eq!(
            evaluate_health_gate(90, 100, true),
            HealthGateAction::Unchanged
        );
    }

    #[test]
    fn health_gate_emit_threshold_inclusive() {
        // Exactly 95 % → emit (`>=`).
        assert_eq!(evaluate_health_gate(95, 100, false), HealthGateAction::Emit);
    }

    #[test]
    fn health_gate_clear_threshold_inclusive() {
        // Exactly 85 % → clear (`<=`).
        assert_eq!(evaluate_health_gate(85, 100, true), HealthGateAction::Clear);
    }

    // ========================================================================
    // Prometheus label escaping (regression for the adapter_id
    // injection surface)
    // ========================================================================

    #[test]
    fn prometheus_label_escapes_backslash_quote_newline_carriage_return() {
        // The three characters the Prometheus text-exposition spec
        // requires escaping in label values, plus `\r` (a raw CR
        // before LF is a legitimate line terminator on Windows-
        // aware parsers, so we escape it too to close the CRLF-
        // injection surface).
        assert_eq!(escape_prometheus_label(r"a\b"), r"a\\b");
        assert_eq!(escape_prometheus_label(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_prometheus_label("a\nb"), r"a\nb");
        assert_eq!(escape_prometheus_label("a\rb"), r"a\rb");
        // Compound case: every special character in one value.
        assert_eq!(
            escape_prometheus_label("a\\b\"c\nd\re"),
            "a\\\\b\\\"c\\nd\\re"
        );
        // Plain ASCII passes through unchanged.
        assert_eq!(escape_prometheus_label("mesh-prod"), "mesh-prod");
    }

    #[test]
    fn to_prometheus_text_escapes_adapter_id_against_injection() {
        // An operator passing `--adapter-id 'evil"\n# bogus_metric{}1\n#'`
        // (or any binding caller) must not be able to inject new
        // metric lines into the scrape body. The label value
        // should escape the closing-quote + newline so the body
        // stays well-formed.
        let snap = BlobMetricsSnapshot::default();
        let body = snap.to_prometheus_text("evil\"\n# bogus_metric{} 1\n#", 0);
        // The label value should be quoted-and-escaped inline,
        // not closing the label early.
        assert!(
            body.contains(r#"adapter="evil\"\n# bogus_metric{} 1\n#""#),
            "adapter_id must appear escaped inside the label value; got:\n{}",
            body
        );
        // And no raw injected line should appear in the body.
        assert!(
            !body.contains("\nbogus_metric{}"),
            "raw injected metric line must not survive escaping; got:\n{}",
            body
        );
    }

    // ========================================================================
    // v0.3 overflow counter family (P4)
    // ========================================================================

    #[test]
    fn overflow_metrics_default_snapshot_is_all_zero() {
        let metrics = BlobMetrics::new();
        let o = metrics.snapshot().overflow;
        assert_eq!(o.pushes_admitted_total, 0);
        assert_eq!(o.push_errors_total, 0);
        assert_eq!(o.pushed_bytes_total, 0);
        assert_eq!(o.rejected_no_target_total, 0);
        assert_eq!(o.rejected_no_storage_cap_total, 0);
        assert_eq!(o.rejected_not_participating_total, 0);
        assert_eq!(o.rejected_sender_not_overflowing_total, 0);
        assert_eq!(o.rejected_unhealthy_total, 0);
        assert_eq!(o.rejected_scope_mismatch_total, 0);
        assert_eq!(o.rejected_insufficient_disk_total, 0);
        assert_eq!(o.high_water_triggered_total, 0);
        assert_eq!(o.low_water_cleared_total, 0);
        assert!(!o.active);
        assert_eq!(o.disk_ratio, 0.0);
    }

    #[test]
    fn record_overflow_tick_bumps_counters_and_sets_gauges() {
        // A tick that admitted 3, errored 1, rejected 2 for
        // no_target, and transitioned from inactive →
        // active. Verify every per-field counter advanced
        // by the expected delta and the gauges took the
        // post-tick values.
        let metrics = BlobMetrics::new();
        let report = super::super::overflow::BlobOverflowTickReport {
            admitted: 3,
            rejected_no_target: 2,
            push_errors: 1,
            was_active_at_start: false,
            is_active_at_end: true,
            disk_ratio_at_start: 0.90,
            disk_ratio_at_end: 0.88,
            pushed_bytes: 12_345,
        };
        metrics.record_overflow_tick(&report);
        let o = metrics.snapshot().overflow;
        assert_eq!(o.pushes_admitted_total, 3);
        assert_eq!(o.push_errors_total, 1);
        assert_eq!(o.pushed_bytes_total, 12_345);
        assert_eq!(o.rejected_no_target_total, 2);
        assert_eq!(
            o.high_water_triggered_total, 1,
            "false → true transition must bump the trigger counter exactly once"
        );
        assert_eq!(
            o.low_water_cleared_total, 0,
            "no true → false transition this tick"
        );
        assert!(o.active);
        assert!((o.disk_ratio - 0.88).abs() < 1e-3);
    }

    #[test]
    fn record_overflow_tick_no_transition_does_not_bump_hysteresis_counters() {
        // Two consecutive active-during ticks: only the
        // first bumps `high_water_triggered`; the second
        // (was active → still active) bumps neither.
        let metrics = BlobMetrics::new();
        let tick1 = super::super::overflow::BlobOverflowTickReport {
            was_active_at_start: false,
            is_active_at_end: true,
            disk_ratio_at_end: 0.90,
            ..Default::default()
        };
        let tick2 = super::super::overflow::BlobOverflowTickReport {
            was_active_at_start: true,
            is_active_at_end: true,
            disk_ratio_at_end: 0.88,
            ..Default::default()
        };
        metrics.record_overflow_tick(&tick1);
        metrics.record_overflow_tick(&tick2);
        let o = metrics.snapshot().overflow;
        assert_eq!(o.high_water_triggered_total, 1);
        assert_eq!(o.low_water_cleared_total, 0);
    }

    #[test]
    fn record_overflow_tick_clear_transition_bumps_low_water_cleared() {
        let metrics = BlobMetrics::new();
        let tick_clear = super::super::overflow::BlobOverflowTickReport {
            was_active_at_start: true,
            is_active_at_end: false,
            disk_ratio_at_end: 0.65,
            ..Default::default()
        };
        metrics.record_overflow_tick(&tick_clear);
        let o = metrics.snapshot().overflow;
        assert_eq!(o.high_water_triggered_total, 0);
        assert_eq!(o.low_water_cleared_total, 1);
        assert!(!o.active);
    }

    #[test]
    fn record_overflow_reject_bumps_each_variant_distinctly() {
        // Every `OverflowReject` variant maps to its own
        // counter. Pin the routing so a rename / reshuffle
        // doesn't silently collapse two variants into one.
        let metrics = BlobMetrics::new();
        use super::super::admission::OverflowReject as R;
        metrics.record_overflow_reject(R::NoStorageCap);
        metrics.record_overflow_reject(R::NoStorageCap);
        metrics.record_overflow_reject(R::NotParticipating);
        metrics.record_overflow_reject(R::SenderNotOverflowing);
        metrics.record_overflow_reject(R::Unhealthy);
        metrics.record_overflow_reject(R::ScopeMismatch);
        metrics.record_overflow_reject(R::InsufficientDisk);
        let o = metrics.snapshot().overflow;
        assert_eq!(o.rejected_no_storage_cap_total, 2);
        assert_eq!(o.rejected_not_participating_total, 1);
        assert_eq!(o.rejected_sender_not_overflowing_total, 1);
        assert_eq!(o.rejected_unhealthy_total, 1);
        assert_eq!(o.rejected_scope_mismatch_total, 1);
        assert_eq!(o.rejected_insufficient_disk_total, 1);
    }

    #[test]
    fn to_prometheus_text_emits_overflow_counter_family() {
        // The Prometheus text body must include every
        // overflow counter / gauge under the canonical name.
        // Smoke-test the strings — full parsing is the
        // scraper's job.
        let metrics = BlobMetrics::new();
        let report = super::super::overflow::BlobOverflowTickReport {
            admitted: 7,
            push_errors: 2,
            pushed_bytes: 99_999,
            was_active_at_start: false,
            is_active_at_end: true,
            disk_ratio_at_end: 0.87,
            ..Default::default()
        };
        metrics.record_overflow_tick(&report);
        let body = metrics.snapshot().to_prometheus_text("op-test", 0);

        // Counters present with their values.
        assert!(
            body.contains("dataforts_blob_overflow_pushes_admitted_total{adapter=\"op-test\"} 7")
        );
        assert!(body.contains("dataforts_blob_overflow_push_errors_total{adapter=\"op-test\"} 2"));
        assert!(
            body.contains("dataforts_blob_overflow_pushed_bytes_total{adapter=\"op-test\"} 99999")
        );
        assert!(body
            .contains("dataforts_blob_overflow_high_water_triggered_total{adapter=\"op-test\"} 1"));
        // Per-reason rejected family — all six labels present
        // even at zero (operators dashboarding the label
        // family don't want missing labels).
        assert!(body.contains("reason=\"no_storage_cap\""));
        assert!(body.contains("reason=\"not_participating\""));
        assert!(body.contains("reason=\"sender_not_overflowing\""));
        assert!(body.contains("reason=\"unhealthy\""));
        assert!(body.contains("reason=\"scope_mismatch\""));
        assert!(body.contains("reason=\"insufficient_disk\""));
        // Gauges.
        assert!(body.contains("dataforts_blob_overflow_active{adapter=\"op-test\"} 1"));
        assert!(body.contains("dataforts_blob_overflow_disk_ratio{adapter=\"op-test\"} 0.870"));
        // HELP + TYPE comment lines for the counter family
        // (sanity-check the exposition shape).
        assert!(body.contains("# TYPE dataforts_blob_overflow_pushes_admitted_total counter"));
        assert!(body.contains("# TYPE dataforts_blob_overflow_active gauge"));
    }

    #[test]
    fn to_prometheus_text_overflow_adapter_id_is_escaped() {
        // The label-injection regression from PR-04 also
        // applies to the overflow counter family — every line
        // routes the operator-supplied adapter_id through
        // the escape helper. Pin it.
        let metrics = BlobMetrics::new();
        let body = metrics.snapshot().to_prometheus_text("evil\"\nbogus", 0);
        assert!(body.contains(r#"dataforts_blob_overflow_active{adapter="evil\"\nbogus"}"#));
        // Raw injected line must not survive.
        assert!(!body.contains("\nbogus\nbogus_metric"));
    }
}
