//! Per-service caller-side nRPC metrics.
//!
//! Tracks the four numbers operators actually need: how many calls
//! went out, how many failed (by failure kind), how long they took
//! (bucketed histogram + sum/count for averages), and how many are
//! currently in flight. Exposed as a [`RpcMetricsSnapshot`] cheap
//! enough to collect on every Prometheus scrape, plus a built-in
//! [`RpcMetricsSnapshot::prometheus_text`] formatter for users who
//! want to plug straight into a `text/plain; version=0.0.4`
//! HTTP endpoint.
//!
//! **Caller-side only** for v1. Server-side handler invocation /
//! panic / streaming-chunk counters are a planned follow-up; the
//! caller-side surface covers the bulk of the user-facing
//! observability story (latency p99, error rate by kind, in-flight
//! gauge for concurrency budgeting).

use std::fmt::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

/// Prometheus-default histogram buckets (seconds). Mirrors the
/// `prometheus_client::metrics::histogram::DEFAULT_BUCKETS`
/// canonical layout so users can wire this snapshot into a
/// Prometheus exporter without re-bucketing.
pub const DEFAULT_LATENCY_BUCKETS_SECS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Number of cumulative buckets including the implicit `+Inf`
/// terminal bucket Prometheus expects.
const N_BUCKETS: usize = 12; // = DEFAULT_LATENCY_BUCKETS_SECS.len() + 1

/// Atomic per-service counters. Held by [`RpcMetricsRegistry`] in
/// an `Arc` per active service.
pub(super) struct ServiceMetricsAtomic {
    pub calls_total: AtomicU64,
    pub errors_no_route: AtomicU64,
    pub errors_timeout: AtomicU64,
    pub errors_server: AtomicU64,
    pub errors_transport: AtomicU64,
    /// Currently-in-flight calls. Balanced by an RAII guard that
    /// `+1`s on call entry and `-1`s on Drop. Can briefly observe
    /// negative values under racy reads but converges.
    pub in_flight: AtomicI64,
    pub latency_sum_ns: AtomicU64,
    pub latency_count: AtomicU64,
    /// Cumulative bucket counts: `latency_buckets[i]` = number of
    /// observations with latency `<= DEFAULT_LATENCY_BUCKETS_SECS[i]`.
    /// Last entry (`[N_BUCKETS-1]`) is the `+Inf` bucket — equal to
    /// `latency_count` by Prometheus convention.
    pub latency_buckets: [AtomicU64; N_BUCKETS],
}

impl ServiceMetricsAtomic {
    fn new() -> Self {
        Self {
            calls_total: AtomicU64::new(0),
            errors_no_route: AtomicU64::new(0),
            errors_timeout: AtomicU64::new(0),
            errors_server: AtomicU64::new(0),
            errors_transport: AtomicU64::new(0),
            in_flight: AtomicI64::new(0),
            latency_sum_ns: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            latency_buckets: Default::default(),
        }
    }

    /// Record one observation. Bumps `latency_count`,
    /// `latency_sum_ns`, and every cumulative bucket whose upper
    /// bound the observation satisfies.
    pub(super) fn record_latency(&self, elapsed: Duration) {
        let ns = elapsed.as_nanos() as u64;
        self.latency_sum_ns.fetch_add(ns, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        let secs = ns as f64 / 1.0e9_f64;
        for (i, le) in DEFAULT_LATENCY_BUCKETS_SECS.iter().enumerate() {
            if secs <= *le {
                self.latency_buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        // +Inf bucket counts every observation (Prometheus
        // requires the terminal bucket to equal `_count`).
        self.latency_buckets[N_BUCKETS - 1].fetch_add(1, Ordering::Relaxed);
    }
}

/// What outcome to record on call exit. Drives which error
/// counter (if any) gets bumped; success bumps no error counter.
#[derive(Debug, Clone, Copy)]
pub(super) enum CallOutcome {
    Ok,
    NoRoute,
    Timeout,
    ServerError,
    Transport,
}

/// Per-Mesh registry of `service` → counters. Built once at
/// `MeshNode::new`; all caller-side hooks consult it via
/// [`MeshNode::rpc_metrics_arc`].
pub struct RpcMetricsRegistry {
    services: DashMap<String, Arc<ServiceMetricsAtomic>>,
}

impl Default for RpcMetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcMetricsRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            services: DashMap::new(),
        }
    }

    /// Get-or-create the per-service counter set. Cheap on the
    /// hot path (single DashMap get); falls back to an entry-API
    /// insert on first access for a service.
    pub(super) fn for_service(&self, service: &str) -> Arc<ServiceMetricsAtomic> {
        if let Some(m) = self.services.get(service) {
            return m.clone();
        }
        self.services
            .entry(service.to_string())
            .or_insert_with(|| Arc::new(ServiceMetricsAtomic::new()))
            .clone()
    }

    /// Read-only snapshot — copies the atomic counters into a
    /// plain data type. Suitable for a per-scrape Prometheus pull;
    /// allocation cost is one `Vec` per active service.
    pub fn snapshot(&self) -> RpcMetricsSnapshot {
        let mut services = Vec::with_capacity(self.services.len());
        for entry in self.services.iter() {
            let m = entry.value();
            let mut buckets = Vec::with_capacity(N_BUCKETS);
            for b in &m.latency_buckets {
                buckets.push(b.load(Ordering::Relaxed));
            }
            services.push(ServiceMetrics {
                service: entry.key().clone(),
                calls_total: m.calls_total.load(Ordering::Relaxed),
                errors_no_route: m.errors_no_route.load(Ordering::Relaxed),
                errors_timeout: m.errors_timeout.load(Ordering::Relaxed),
                errors_server: m.errors_server.load(Ordering::Relaxed),
                errors_transport: m.errors_transport.load(Ordering::Relaxed),
                in_flight: m.in_flight.load(Ordering::Relaxed),
                latency_sum_ns: m.latency_sum_ns.load(Ordering::Relaxed),
                latency_count: m.latency_count.load(Ordering::Relaxed),
                latency_buckets: buckets,
            });
        }
        services.sort_by(|a, b| a.service.cmp(&b.service));
        RpcMetricsSnapshot { services }
    }
}

/// Plain-data snapshot of the registry at a point in time.
/// Returned by [`RpcMetricsRegistry::snapshot`]; format with
/// [`Self::prometheus_text`] or read fields directly.
#[derive(Debug, Clone)]
pub struct RpcMetricsSnapshot {
    /// One entry per service that has been called at least once
    /// since the registry was created. Sorted by service name for
    /// stable scrape output.
    pub services: Vec<ServiceMetrics>,
}

/// Per-service counters at a point in time.
#[derive(Debug, Clone)]
pub struct ServiceMetrics {
    /// Service name (e.g. `"echo"`, `"my.svc.lookup"`).
    pub service: String,
    /// Total calls that *resolved* (success + any error). Calls
    /// that were dropped before resolving are NOT counted.
    pub calls_total: u64,
    /// Calls that returned `RpcError::NoRoute`.
    pub errors_no_route: u64,
    /// Calls that returned `RpcError::Timeout`.
    pub errors_timeout: u64,
    /// Calls that returned `RpcError::ServerError`.
    pub errors_server: u64,
    /// Calls that returned `RpcError::Transport`.
    pub errors_transport: u64,
    /// Currently-in-flight calls (started but not yet resolved
    /// AND not yet dropped). Includes hedge losers up until their
    /// future is dropped.
    pub in_flight: i64,
    /// Sum of resolved-call latencies in nanoseconds. Pair with
    /// `latency_count` for the average; or use bucket counts for
    /// quantile estimation.
    pub latency_sum_ns: u64,
    /// Number of observations included in `latency_sum_ns` and
    /// `latency_buckets`. Equal to (success + error) — i.e.
    /// every call that resolved.
    pub latency_count: u64,
    /// Cumulative bucket counts: index `i` = count of
    /// observations `<= DEFAULT_LATENCY_BUCKETS_SECS[i]`. The
    /// last entry is the `+Inf` bucket and equals
    /// `latency_count`.
    pub latency_buckets: Vec<u64>,
}

impl RpcMetricsSnapshot {
    /// Format as Prometheus text exposition format
    /// (`text/plain; version=0.0.4`). Drop straight into an HTTP
    /// `/metrics` handler:
    ///
    /// ```ignore
    /// // axum / hyper / etc:
    /// async fn metrics(mesh: Arc<MeshNode>) -> String {
    ///     mesh.rpc_metrics_snapshot().prometheus_text()
    /// }
    /// ```
    ///
    /// Emits five metrics per service: `nrpc_calls_total`,
    /// `nrpc_errors_total{kind=...}`, `nrpc_in_flight_calls`,
    /// `nrpc_call_latency_seconds_{bucket,sum,count}`. Service
    /// names are escaped per Prometheus convention.
    pub fn prometheus_text(&self) -> String {
        let mut out = String::with_capacity(2048);

        // calls_total
        out.push_str("# HELP nrpc_calls_total Total nRPC calls that resolved (success or error).\n");
        out.push_str("# TYPE nrpc_calls_total counter\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_calls_total{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.calls_total
            );
        }

        // errors_total{kind}
        out.push_str("# HELP nrpc_errors_total nRPC call failures, partitioned by error kind.\n");
        out.push_str("# TYPE nrpc_errors_total counter\n");
        for s in &self.services {
            let svc = escape_label(&s.service);
            let _ = writeln!(
                out,
                "nrpc_errors_total{{service=\"{svc}\",kind=\"no_route\"}} {}",
                s.errors_no_route
            );
            let _ = writeln!(
                out,
                "nrpc_errors_total{{service=\"{svc}\",kind=\"timeout\"}} {}",
                s.errors_timeout
            );
            let _ = writeln!(
                out,
                "nrpc_errors_total{{service=\"{svc}\",kind=\"server\"}} {}",
                s.errors_server
            );
            let _ = writeln!(
                out,
                "nrpc_errors_total{{service=\"{svc}\",kind=\"transport\"}} {}",
                s.errors_transport
            );
        }

        // in_flight (gauge)
        out.push_str("# HELP nrpc_in_flight_calls Currently-in-flight nRPC calls.\n");
        out.push_str("# TYPE nrpc_in_flight_calls gauge\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_in_flight_calls{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.in_flight
            );
        }

        // latency histogram
        out.push_str(
            "# HELP nrpc_call_latency_seconds Wall-clock nRPC call latency in seconds.\n",
        );
        out.push_str("# TYPE nrpc_call_latency_seconds histogram\n");
        for s in &self.services {
            let svc = escape_label(&s.service);
            for (i, le) in DEFAULT_LATENCY_BUCKETS_SECS.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "nrpc_call_latency_seconds_bucket{{service=\"{svc}\",le=\"{le}\"}} {}",
                    s.latency_buckets.get(i).copied().unwrap_or(0)
                );
            }
            let _ = writeln!(
                out,
                "nrpc_call_latency_seconds_bucket{{service=\"{svc}\",le=\"+Inf\"}} {}",
                s.latency_buckets.last().copied().unwrap_or(0)
            );
            let _ = writeln!(
                out,
                "nrpc_call_latency_seconds_sum{{service=\"{svc}\"}} {}",
                s.latency_sum_ns as f64 / 1.0e9_f64
            );
            let _ = writeln!(
                out,
                "nrpc_call_latency_seconds_count{{service=\"{svc}\"}} {}",
                s.latency_count
            );
        }

        out
    }
}

/// Escape a label value per Prometheus exposition format:
/// backslash, double-quote, and newline get backslash-escaped.
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

// ============================================================================
// Caller-side metrics guard.
// ============================================================================

/// RAII guard for one in-flight call. Bumps `in_flight` on
/// construction; balances it on Drop. The Mesh::call code calls
/// `record_outcome` exactly once before the guard goes out of
/// scope, which logs the latency + outcome counter.
///
/// Built at function entry (BEFORE any potential early-return
/// path) so even publish-failure paths get counted as a `NoRoute`
/// error. The hedge loser path (where the call future is dropped
/// without ever calling `record_outcome`) leaves `in_flight`
/// correctly decremented but NOT recording a latency or outcome
/// — dropped calls didn't resolve, so we don't synthesize a
/// resolution.
pub(super) struct CallMetricsGuard {
    metrics: Arc<ServiceMetricsAtomic>,
    started: std::time::Instant,
    /// Set to `Some(outcome)` when the call resolves; Drop
    /// records the counter + latency for that outcome. `None`
    /// means the future was dropped mid-flight — `in_flight`
    /// still decrements but no outcome is recorded.
    outcome: Option<CallOutcome>,
}

impl CallMetricsGuard {
    pub(super) fn new(metrics: Arc<ServiceMetricsAtomic>) -> Self {
        metrics.in_flight.fetch_add(1, Ordering::Relaxed);
        Self {
            metrics,
            started: std::time::Instant::now(),
            outcome: None,
        }
    }

    /// Mark this call resolved with the given outcome — records
    /// happen on Drop.
    pub(super) fn record(&mut self, outcome: CallOutcome) {
        self.outcome = Some(outcome);
    }
}

impl Drop for CallMetricsGuard {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
        if let Some(outcome) = self.outcome {
            self.metrics.calls_total.fetch_add(1, Ordering::Relaxed);
            self.metrics.record_latency(self.started.elapsed());
            match outcome {
                CallOutcome::Ok => {}
                CallOutcome::NoRoute => {
                    self.metrics
                        .errors_no_route
                        .fetch_add(1, Ordering::Relaxed);
                }
                CallOutcome::Timeout => {
                    self.metrics
                        .errors_timeout
                        .fetch_add(1, Ordering::Relaxed);
                }
                CallOutcome::ServerError => {
                    self.metrics
                        .errors_server
                        .fetch_add(1, Ordering::Relaxed);
                }
                CallOutcome::Transport => {
                    self.metrics
                        .errors_transport
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_snapshot_round_trip() {
        let r = RpcMetricsRegistry::new();
        let m = r.for_service("echo");
        m.calls_total.fetch_add(3, Ordering::Relaxed);
        m.errors_timeout.fetch_add(1, Ordering::Relaxed);
        m.record_latency(Duration::from_millis(7));
        m.record_latency(Duration::from_millis(150));
        m.record_latency(Duration::from_secs(3));

        let snap = r.snapshot();
        assert_eq!(snap.services.len(), 1);
        let s = &snap.services[0];
        assert_eq!(s.service, "echo");
        assert_eq!(s.calls_total, 3);
        assert_eq!(s.errors_timeout, 1);
        assert_eq!(s.latency_count, 3);

        // Cumulative buckets: 7ms hits ≤0.01s and up; 150ms hits ≤0.25s
        // and up; 3s hits ≤5s and up. So bucket[0] (≤5ms) = 0,
        // bucket[1] (≤10ms) = 1 (the 7ms), bucket[5] (≤0.25s) = 2
        // (7ms + 150ms), bucket[N-1] (+Inf) = 3.
        assert_eq!(s.latency_buckets[0], 0, "no obs ≤ 5ms");
        assert_eq!(s.latency_buckets[1], 1, "7ms ≤ 10ms");
        assert_eq!(s.latency_buckets[5], 2, "7ms + 150ms ≤ 0.25s");
        assert_eq!(s.latency_buckets[N_BUCKETS - 1], 3, "+Inf == count");
    }

    #[test]
    fn prometheus_text_emits_canonical_metric_names() {
        let r = RpcMetricsRegistry::new();
        let m = r.for_service("echo");
        m.calls_total.fetch_add(1, Ordering::Relaxed);
        m.record_latency(Duration::from_millis(5));
        let text = r.snapshot().prometheus_text();
        assert!(text.contains("nrpc_calls_total"));
        assert!(text.contains("nrpc_errors_total"));
        assert!(text.contains("nrpc_in_flight_calls"));
        assert!(text.contains("nrpc_call_latency_seconds_bucket"));
        assert!(text.contains("nrpc_call_latency_seconds_sum"));
        assert!(text.contains("nrpc_call_latency_seconds_count"));
        assert!(text.contains("le=\"+Inf\""));
    }

    #[test]
    fn label_escaping() {
        assert_eq!(escape_label("simple"), "simple");
        assert_eq!(escape_label(r#"has"quote"#), r#"has\"quote"#);
        assert_eq!(escape_label("has\\bs"), "has\\\\bs");
    }

    #[test]
    fn guard_records_in_flight_and_outcome() {
        let r = RpcMetricsRegistry::new();
        let m = r.for_service("svc");
        {
            let mut g = CallMetricsGuard::new(m.clone());
            assert_eq!(m.in_flight.load(Ordering::Relaxed), 1);
            g.record(CallOutcome::Ok);
        }
        assert_eq!(m.in_flight.load(Ordering::Relaxed), 0);
        assert_eq!(m.calls_total.load(Ordering::Relaxed), 1);
        assert_eq!(m.latency_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn guard_dropped_without_outcome_balances_in_flight_only() {
        let r = RpcMetricsRegistry::new();
        let m = r.for_service("dropped");
        {
            let _g = CallMetricsGuard::new(m.clone());
            assert_eq!(m.in_flight.load(Ordering::Relaxed), 1);
            // Drop without record() — simulates hedge loser.
        }
        assert_eq!(m.in_flight.load(Ordering::Relaxed), 0, "in_flight balanced");
        assert_eq!(m.calls_total.load(Ordering::Relaxed), 0, "no outcome recorded");
        assert_eq!(m.latency_count.load(Ordering::Relaxed), 0);
    }
}
