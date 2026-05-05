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
/// an `Arc` per active service. Covers BOTH the caller-side path
/// (call_*, errors_*, in_flight, latency_*) and the server-side
/// path (handler_*, streaming_chunks_*) so a node that both calls
/// and serves a service has complete observability for that
/// service in one record.
pub struct ServiceMetricsAtomic {
    // ------------- Caller side -------------
    /// Total calls that resolved (success + any error).
    pub calls_total: AtomicU64,
    /// Calls that returned `RpcError::NoRoute`.
    pub errors_no_route: AtomicU64,
    /// Calls that returned `RpcError::Timeout`.
    pub errors_timeout: AtomicU64,
    /// Calls that returned `RpcError::ServerError`.
    pub errors_server: AtomicU64,
    /// Calls that returned `RpcError::Transport`.
    pub errors_transport: AtomicU64,
    /// Currently-in-flight calls. Balanced by an RAII guard that
    /// `+1`s on call entry and `-1`s on Drop. Can briefly observe
    /// negative values under racy reads but converges.
    pub in_flight: AtomicI64,
    /// Sum of resolved-call latencies in nanoseconds.
    pub latency_sum_ns: AtomicU64,
    /// Number of latency observations recorded.
    pub latency_count: AtomicU64,
    /// Cumulative bucket counts: `latency_buckets[i]` = number of
    /// observations with latency `<= DEFAULT_LATENCY_BUCKETS_SECS[i]`.
    /// Last entry (`[N_BUCKETS-1]`) is the `+Inf` bucket — equal to
    /// `latency_count` by Prometheus convention.
    pub latency_buckets: [AtomicU64; N_BUCKETS],

    // ------------- Server side -------------
    /// Total handler invocations (every spawned task, regardless
    /// of outcome). Incremented at the start of the spawned
    /// handler task in `RpcServerFold` / `RpcServerStreamingFold`.
    pub handler_invocations_total: AtomicU64,
    /// Handler panics caught by the fold's `catch_unwind`. Useful
    /// alerting signal — should be ~0 in healthy steady state.
    pub handler_panics_total: AtomicU64,
    /// Currently-in-flight handler tasks. Balanced by `+1` at
    /// task spawn and `-1` after the handler returns / panics.
    pub handler_in_flight: AtomicI64,
    /// Sum of handler durations in nanoseconds — the per-task
    /// wall-clock time from spawn to handler return (excludes
    /// network round-trip).
    pub handler_duration_sum_ns: AtomicU64,
    /// Number of handler observations (success + error + panic)
    /// included in `handler_duration_*`.
    pub handler_duration_count: AtomicU64,
    /// Cumulative bucket counts for `handler_duration_seconds`.
    /// Same shape / semantics as the caller-side `latency_buckets`.
    pub handler_duration_buckets: [AtomicU64; N_BUCKETS],
    /// Streaming-only: total chunks emitted by all streaming
    /// handlers for this service. Bumped per `sink.send(...)` in
    /// the streaming fold's pump task.
    pub streaming_chunks_emitted_total: AtomicU64,
    /// Streaming-only: total chunks dropped because the per-call
    /// pump mpsc was full at `sink.send(...)` time. Indicates the
    /// handler is producing chunks faster than the publish path
    /// can drain — usually because the caller didn't enable flow
    /// control via `CallOptions::stream_window_initial`. A non-
    /// zero value means data loss.
    pub streaming_chunks_dropped_total: AtomicU64,
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
            handler_invocations_total: AtomicU64::new(0),
            handler_panics_total: AtomicU64::new(0),
            handler_in_flight: AtomicI64::new(0),
            handler_duration_sum_ns: AtomicU64::new(0),
            handler_duration_count: AtomicU64::new(0),
            handler_duration_buckets: Default::default(),
            streaming_chunks_emitted_total: AtomicU64::new(0),
            streaming_chunks_dropped_total: AtomicU64::new(0),
        }
    }

    /// Record one caller-side latency observation.
    pub(super) fn record_latency(&self, elapsed: Duration) {
        record_into_histogram(
            elapsed,
            &self.latency_sum_ns,
            &self.latency_count,
            &self.latency_buckets,
        );
    }

    /// Record one server-side handler-duration observation.
    /// Called by the spawned handler task after the handler
    /// returns (or panics).
    pub fn record_handler_duration(&self, elapsed: Duration) {
        record_into_histogram(
            elapsed,
            &self.handler_duration_sum_ns,
            &self.handler_duration_count,
            &self.handler_duration_buckets,
        );
    }
}

/// Internal: bump `sum_ns`, `count`, and every cumulative bucket
/// the observation satisfies, plus the `+Inf` terminal bucket.
fn record_into_histogram(
    elapsed: Duration,
    sum_ns: &AtomicU64,
    count: &AtomicU64,
    buckets: &[AtomicU64; N_BUCKETS],
) {
    let ns = elapsed.as_nanos() as u64;
    sum_ns.fetch_add(ns, Ordering::Relaxed);
    count.fetch_add(1, Ordering::Relaxed);
    let secs = ns as f64 / 1.0e9_f64;
    for (i, le) in DEFAULT_LATENCY_BUCKETS_SECS.iter().enumerate() {
        if secs <= *le {
            buckets[i].fetch_add(1, Ordering::Relaxed);
        }
    }
    buckets[N_BUCKETS - 1].fetch_add(1, Ordering::Relaxed);
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

/// Hard cap on the number of distinct services tracked per
/// registry. New services past this cap share a single fall-back
/// "overflow" counter set so a malicious peer (or a bug emitting
/// random service names) can't grow the DashMap unboundedly.
/// 4096 is comfortable for any realistic deployment — typical
/// node serves O(10) services; large clusters might track O(100)
/// across all callers/servers.
pub const MAX_TRACKED_SERVICES: usize = 4096;

/// Sentinel service name used when [`MAX_TRACKED_SERVICES`] is
/// exceeded. Counters under this name aggregate every overflow
/// service, so operators can still see "we lost detail past the
/// cap" without leaking memory.
pub const OVERFLOW_SERVICE_LABEL: &str = "__overflow__";

/// Per-Mesh registry of `service` → counters. Built once at
/// `MeshNode::new`; all caller-side hooks consult it via
/// `MeshNode::rpc_metrics_arc` (a `pub(super)` accessor used by
/// the `mesh_rpc::Mesh::call` glue).
///
/// **Bounded.** New services past [`MAX_TRACKED_SERVICES`] are
/// folded into a single `__overflow__` counter set, so a peer
/// that emits a fresh service name per request can't grow the
/// DashMap without bound. The first-N services keep their own
/// per-service counters.
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
    ///
    /// **Bounded:** if the registry is at [`MAX_TRACKED_SERVICES`]
    /// and `service` isn't already known, returns the shared
    /// `__overflow__` counter set instead of inserting a new
    /// entry. This prevents unbounded growth from a peer emitting
    /// distinct service names per request.
    ///
    /// `pub(crate)` so the cortex-side server folds (in
    /// `adapter/net/cortex/rpc.rs`) can grab a per-service
    /// counter handle at construction time and bump it from the
    /// spawned handler task.
    pub(crate) fn for_service(&self, service: &str) -> Arc<ServiceMetricsAtomic> {
        if let Some(m) = self.services.get(service) {
            return m.clone();
        }
        // Cap check BEFORE the entry-API insert: if we're at the
        // limit, fold this call into the overflow bucket.
        // (The overflow bucket itself counts as one slot and is
        // created lazily on first overflow — net: at most cap+1
        // entries.)
        if self.services.len() >= MAX_TRACKED_SERVICES
            && !self.services.contains_key(service)
        {
            return self
                .services
                .entry(OVERFLOW_SERVICE_LABEL.to_string())
                .or_insert_with(|| Arc::new(ServiceMetricsAtomic::new()))
                .clone();
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
            let mut handler_buckets = Vec::with_capacity(N_BUCKETS);
            for b in &m.handler_duration_buckets {
                handler_buckets.push(b.load(Ordering::Relaxed));
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
                handler_invocations_total: m.handler_invocations_total.load(Ordering::Relaxed),
                handler_panics_total: m.handler_panics_total.load(Ordering::Relaxed),
                handler_in_flight: m.handler_in_flight.load(Ordering::Relaxed),
                handler_duration_sum_ns: m.handler_duration_sum_ns.load(Ordering::Relaxed),
                handler_duration_count: m.handler_duration_count.load(Ordering::Relaxed),
                handler_duration_buckets: handler_buckets,
                streaming_chunks_emitted_total: m
                    .streaming_chunks_emitted_total
                    .load(Ordering::Relaxed),
                streaming_chunks_dropped_total: m
                    .streaming_chunks_dropped_total
                    .load(Ordering::Relaxed),
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

    // ------------- Caller side -------------
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

    // ------------- Server side -------------
    /// Total handler invocations on this node for `service`.
    /// Bumped at the start of each spawned handler task.
    pub handler_invocations_total: u64,
    /// Handler panics caught by the fold's `catch_unwind`.
    /// Bumped from the spawned task's panic-catch arm. Should be
    /// near-zero in healthy steady state.
    pub handler_panics_total: u64,
    /// Currently-running handler tasks for this service on this
    /// node. Useful for server-side concurrency budgeting.
    pub handler_in_flight: i64,
    /// Sum of handler durations in nanoseconds — wall-clock
    /// from spawn to handler return / panic. Excludes network
    /// round-trip; pair with caller-side `latency_*` for
    /// network overhead.
    pub handler_duration_sum_ns: u64,
    /// Number of handler observations included in
    /// `handler_duration_sum_ns` and `handler_duration_buckets`.
    pub handler_duration_count: u64,
    /// Cumulative bucket counts for handler duration; index `i`
    /// = observations `<= DEFAULT_LATENCY_BUCKETS_SECS[i]`. Last
    /// entry is the `+Inf` bucket and equals
    /// `handler_duration_count`.
    pub handler_duration_buckets: Vec<u64>,
    /// Total streaming chunks emitted by all handler invocations
    /// of this service via `RpcResponseSink::send`. Zero for
    /// services that only register unary handlers.
    pub streaming_chunks_emitted_total: u64,
    /// Total streaming chunks DROPPED because the per-call pump
    /// mpsc was full at `sink.send(...)` time. Non-zero implies
    /// data loss — the handler is producing chunks faster than
    /// the publish path can drain. Operators should either lower
    /// the producer rate or have the caller enable per-call flow
    /// control via `CallOptions::stream_window_initial`.
    pub streaming_chunks_dropped_total: u64,
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
        out.push_str(
            "# HELP nrpc_calls_total Total nRPC calls that resolved (success or error).\n",
        );
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

        // in_flight (gauge). Clamp at 0 — the underlying counter
        // can momentarily read negative under racing
        // increment/decrement (documented in
        // ServiceMetricsAtomic), and Prometheus rejects negative
        // gauge values for samples typed as `gauge`.
        out.push_str("# HELP nrpc_in_flight_calls Currently-in-flight nRPC calls.\n");
        out.push_str("# TYPE nrpc_in_flight_calls gauge\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_in_flight_calls{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.in_flight.max(0),
            );
        }

        // latency histogram
        out.push_str("# HELP nrpc_call_latency_seconds Wall-clock nRPC call latency in seconds.\n");
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

        // ------------- Server side -------------

        // handler_invocations_total
        out.push_str(
            "# HELP nrpc_handler_invocations_total Total nRPC handler invocations on this node.\n",
        );
        out.push_str("# TYPE nrpc_handler_invocations_total counter\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_handler_invocations_total{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.handler_invocations_total
            );
        }

        // handler_panics_total
        out.push_str(
            "# HELP nrpc_handler_panics_total Handler panics caught by the fold's catch_unwind.\n",
        );
        out.push_str("# TYPE nrpc_handler_panics_total counter\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_handler_panics_total{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.handler_panics_total
            );
        }

        // handler_in_flight (gauge). Same clamp-at-0 rationale
        // as nrpc_in_flight_calls.
        out.push_str(
            "# HELP nrpc_handler_in_flight Currently-running handler tasks for this service.\n",
        );
        out.push_str("# TYPE nrpc_handler_in_flight gauge\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_handler_in_flight{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.handler_in_flight.max(0),
            );
        }

        // handler_duration_seconds histogram
        out.push_str(
            "# HELP nrpc_handler_duration_seconds Server-side handler wall-clock duration (excludes network).\n",
        );
        out.push_str("# TYPE nrpc_handler_duration_seconds histogram\n");
        for s in &self.services {
            let svc = escape_label(&s.service);
            for (i, le) in DEFAULT_LATENCY_BUCKETS_SECS.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "nrpc_handler_duration_seconds_bucket{{service=\"{svc}\",le=\"{le}\"}} {}",
                    s.handler_duration_buckets.get(i).copied().unwrap_or(0)
                );
            }
            let _ = writeln!(
                out,
                "nrpc_handler_duration_seconds_bucket{{service=\"{svc}\",le=\"+Inf\"}} {}",
                s.handler_duration_buckets.last().copied().unwrap_or(0)
            );
            let _ = writeln!(
                out,
                "nrpc_handler_duration_seconds_sum{{service=\"{svc}\"}} {}",
                s.handler_duration_sum_ns as f64 / 1.0e9_f64
            );
            let _ = writeln!(
                out,
                "nrpc_handler_duration_seconds_count{{service=\"{svc}\"}} {}",
                s.handler_duration_count
            );
        }

        // streaming_chunks_emitted_total
        out.push_str(
            "# HELP nrpc_streaming_chunks_emitted_total Total chunks emitted by streaming handlers via sink.send().\n",
        );
        out.push_str("# TYPE nrpc_streaming_chunks_emitted_total counter\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_streaming_chunks_emitted_total{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.streaming_chunks_emitted_total
            );
        }

        // streaming_chunks_dropped_total
        out.push_str(
            "# HELP nrpc_streaming_chunks_dropped_total Streaming chunks dropped because the per-call pump mpsc was full (handler outpaced the publish path).\n",
        );
        out.push_str("# TYPE nrpc_streaming_chunks_dropped_total counter\n");
        for s in &self.services {
            let _ = writeln!(
                out,
                "nrpc_streaming_chunks_dropped_total{{service=\"{}\"}} {}",
                escape_label(&s.service),
                s.streaming_chunks_dropped_total
            );
        }

        out
    }
}

/// Escape a label value per Prometheus exposition format:
/// backslash, double-quote, newline, and carriage-return get
/// backslash-escaped. The spec requires `\n` and `\\` and `\"`;
/// we additionally escape `\r` to avoid CRLF/parser-version
/// inconsistencies — some scrapers tolerate raw CR, others reject
/// it as a malformed line terminator.
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
                    self.metrics.errors_no_route.fetch_add(1, Ordering::Relaxed);
                }
                CallOutcome::Timeout => {
                    self.metrics.errors_timeout.fetch_add(1, Ordering::Relaxed);
                }
                CallOutcome::ServerError => {
                    self.metrics.errors_server.fetch_add(1, Ordering::Relaxed);
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
        m.handler_invocations_total.fetch_add(2, Ordering::Relaxed);
        m.record_handler_duration(Duration::from_millis(3));
        let text = r.snapshot().prometheus_text();
        // Caller side
        assert!(text.contains("nrpc_calls_total"));
        assert!(text.contains("nrpc_errors_total"));
        assert!(text.contains("nrpc_in_flight_calls"));
        assert!(text.contains("nrpc_call_latency_seconds_bucket"));
        assert!(text.contains("nrpc_call_latency_seconds_sum"));
        assert!(text.contains("nrpc_call_latency_seconds_count"));
        // Server side
        assert!(text.contains("nrpc_handler_invocations_total"));
        assert!(text.contains("nrpc_handler_panics_total"));
        assert!(text.contains("nrpc_handler_in_flight"));
        assert!(text.contains("nrpc_handler_duration_seconds_bucket"));
        assert!(text.contains("nrpc_streaming_chunks_emitted_total"));
        assert!(text.contains("le=\"+Inf\""));
    }

    #[test]
    fn record_handler_duration_lands_in_buckets() {
        let r = RpcMetricsRegistry::new();
        let m = r.for_service("svc");
        m.record_handler_duration(Duration::from_millis(7));
        m.record_handler_duration(Duration::from_secs(3));
        let snap = r.snapshot();
        let s = &snap.services[0];
        assert_eq!(s.handler_duration_count, 2);
        assert_eq!(
            *s.handler_duration_buckets.last().unwrap(),
            2,
            "+Inf bucket equals count",
        );
        // 7ms ≤ 10ms bucket (index 1), 3s ≤ 5s bucket (index 9).
        assert_eq!(s.handler_duration_buckets[1], 1, "7ms ≤ 10ms");
        assert_eq!(s.handler_duration_buckets[9], 2, "7ms + 3s both ≤ 5s");
    }

    #[test]
    fn label_escaping() {
        assert_eq!(escape_label("simple"), "simple");
        assert_eq!(escape_label(r#"has"quote"#), r#"has\"quote"#);
        assert_eq!(escape_label("has\\bs"), "has\\\\bs");
        // CR + LF both get escaped — some Prometheus parsers
        // tolerate raw CR but stricter scrapers reject it as a
        // malformed line terminator.
        assert_eq!(escape_label("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_label("dos\r\nstyle"), "dos\\r\\nstyle");
    }

    /// Regression: the registry caps service-name growth at
    /// `MAX_TRACKED_SERVICES`. Past the cap, additional services
    /// share the `__overflow__` counter set so a peer that emits
    /// a fresh service name per request can't grow the DashMap
    /// without bound.
    #[test]
    fn registry_caps_service_count_at_max_tracked_services() {
        let reg = RpcMetricsRegistry::new();
        // Fill up to the cap.
        for i in 0..MAX_TRACKED_SERVICES {
            let _ = reg.for_service(&format!("svc-{i}"));
        }
        assert_eq!(reg.services.len(), MAX_TRACKED_SERVICES);
        // Adding more services routes them to the overflow bucket.
        // The overflow entry itself counts as one new slot, but
        // every subsequent overflow request reuses it.
        let m1 = reg.for_service("overflow-1");
        let m2 = reg.for_service("overflow-2");
        let m3 = reg.for_service("overflow-3");
        assert!(
            Arc::ptr_eq(&m1, &m2) && Arc::ptr_eq(&m2, &m3),
            "overflow services must share the __overflow__ counter set",
        );
        // Cap+1 (the original cap entries plus the overflow slot)
        // is the maximum the registry ever reaches.
        assert_eq!(
            reg.services.len(),
            MAX_TRACKED_SERVICES + 1,
            "registry size must never exceed MAX_TRACKED_SERVICES + 1",
        );
        // An already-known service still returns its own counter set.
        let known = reg.for_service("svc-0");
        assert!(
            !Arc::ptr_eq(&known, &m1),
            "known services keep their dedicated counters",
        );
    }

    /// Regression: the in_flight gauge can momentarily read
    /// negative under racing increment/decrement (a Drop runs
    /// before its matching new(), or a snapshot interleaves with
    /// a Drop). Prometheus rejects negative values for samples
    /// typed as `gauge`, so the formatter must clamp at 0.
    #[test]
    fn prometheus_text_clamps_negative_gauge() {
        let r = RpcMetricsRegistry::new();
        let m = r.for_service("clamp");
        // Force the gauge negative to simulate the racing-Drop case.
        m.in_flight.store(-3, Ordering::Relaxed);
        m.handler_in_flight.store(-7, Ordering::Relaxed);
        let snap = r.snapshot();
        let txt = snap.prometheus_text();
        assert!(
            txt.contains("nrpc_in_flight_calls{service=\"clamp\"} 0"),
            "must clamp negative caller-side gauge to 0; got:\n{txt}",
        );
        assert!(
            txt.contains("nrpc_handler_in_flight{service=\"clamp\"} 0"),
            "must clamp negative server-side gauge to 0; got:\n{txt}",
        );
        // Sanity: a positive value is emitted as-is.
        m.in_flight.store(5, Ordering::Relaxed);
        let snap = r.snapshot();
        let txt = snap.prometheus_text();
        assert!(txt.contains("nrpc_in_flight_calls{service=\"clamp\"} 5"));
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
        assert_eq!(
            m.calls_total.load(Ordering::Relaxed),
            0,
            "no outcome recorded"
        );
        assert_eq!(m.latency_count.load(Ordering::Relaxed), 0);
    }
}
