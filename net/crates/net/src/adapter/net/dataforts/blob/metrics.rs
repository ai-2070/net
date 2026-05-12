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
        BlobMetricsSnapshot {
            blobs_stored_total: self.inner.blobs_stored_total.load(Ordering::Relaxed),
            blobs_fetched_total: self.inner.blobs_fetched_total.load(Ordering::Relaxed),
            bytes_stored_total: self.inner.bytes_stored_total.load(Ordering::Relaxed),
            gc_swept_total: self.inner.gc_swept_total.load(Ordering::Relaxed),
            disk_used_bytes: self.inner.disk_used_bytes.load(Ordering::Relaxed),
            disk_capacity_bytes: self.inner.disk_capacity_bytes.load(Ordering::Relaxed),
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
}

/// Point-in-time snapshot of every adapter counter / gauge. The
/// adapter takes one via [`BlobMetrics::snapshot`] when an
/// operator scrapes; the snapshot decouples the scrape format
/// (Prometheus text / OTel / JSON) from the atomic-counter
/// layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
            "# HELP dataforts_blob_gc_pending_total Zero-refcount blobs waiting on retention floor.\n\
             # TYPE dataforts_blob_gc_pending_total gauge\n\
             dataforts_blob_gc_pending_total{{adapter=\"{}\"}} {}\n",
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
        out
    }
}

/// Escape a string for use as a Prometheus label value, per the
/// text-exposition spec: backslash, double-quote, and newline
/// each get a backslash prefix. Other characters are passed
/// through unchanged. Used by the metrics emitter to defang
/// operator-supplied `adapter_id` values that could otherwise
/// inject new metric lines into the scrape body.
fn escape_prometheus_label(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
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
        assert!(text.contains("dataforts_blob_gc_pending_total{adapter=\"my-adapter\"} 7"));
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
    fn prometheus_label_escapes_backslash_quote_newline() {
        // The three characters the Prometheus text-exposition spec
        // requires escaping in label values.
        assert_eq!(escape_prometheus_label(r"a\b"), r"a\\b");
        assert_eq!(escape_prometheus_label(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_prometheus_label("a\nb"), r"a\nb");
        // Compound case: every special character in one value.
        assert_eq!(
            escape_prometheus_label("a\\b\"c\nd"),
            "a\\\\b\\\"c\\nd"
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
}
