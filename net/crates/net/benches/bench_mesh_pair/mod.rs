//! CPB shared harness — two/three transport-connected `MeshNode`s on
//! localhost, capability-manifest fixtures, an exact-state await
//! helper, and an hdrhistogram reporting spine. `#[path]`-included by
//! every `capability_*` bench (Cargo gives each bench binary its own
//! copy, like the SDK crate's `nrpc_common`).
//!
//! Everything here rides the crate's PUBLIC API — the same
//! `MeshNode::new` + `accept`/`connect` dance `tests/common::connect_pair`
//! uses — so the measurement's honesty is a property of shipped
//! surfaces, not a test-only backdoor.
//!
//! # Measurement discipline (per the v0.2 plan review)
//!
//! - **Endpoint = watch wake + exact-state read** (C1). `Fold::apply`
//!   calls `signal_changed()` while the fold write locks are still
//!   held, so a `changed()` wake can arrive *before* the woken task can
//!   read the fold. The timer therefore stops only after
//!   [`await_capability_state`]'s predicate — an exact fold/query check
//!   — returns true. The watch is the wake mechanism; the read is the
//!   endpoint. Still poll-free.
//! - **Start = API invocation** (C2). Public API cannot timestamp the
//!   internal commit, so the timer starts immediately before the
//!   publication (`announce_capabilities`) or registry mutation call.
//! - **`capability_announce_version()` is a version delta, NOT an
//!   emission count** (D2). It over-bumps on `serve_rpc` nodes.
//! - **`start_arc()`, not `start()`** (D1) — installs the weak
//!   self-reference the change-driven announcer + deferred flush need.
//!
//! See `docs/plans/CAPABILITY_PROPAGATION_BENCHMARK_PLAN.md`.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

use net::adapter::net::behavior::capability::{
    CapabilityFilter, CapabilitySet, GpuInfo, GpuVendor, HardwareCapabilities, Modality,
    ModelCapability,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

/// Shared PSK across every CPB node (matches the chaos harness).
pub const PSK: [u8; 32] = [0x42u8; 32];

/// Multi-threaded runtime for the transport path.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build()
        .expect("runtime")
}

/// Worker count reported in the sample protocol (C10).
pub const WORKER_THREADS: usize = 4;

// ============================================================================
// Node configuration — the mechanism modes and the two policy modes.
// ============================================================================

/// The knobs Kyra requires exposed on every run. `None` leaves the
/// crate's config default in place (used by `default_policy` so a
/// deliberate 10 s rate-limit floor is measured as-shipped, not
/// overridden). `wire_floor` isolates mechanism; `debounce_only` and
/// `default_policy` are the two distinct RT-3 policy modes (C5).
#[derive(Clone, Copy)]
pub struct BenchConfig {
    pub sensing_coalescing: bool,
    pub announce_debounce: Option<Duration>,
    pub min_announce_interval: Option<Duration>,
}

impl BenchConfig {
    /// Explicit announcement, no rate limit, no debounce — the
    /// mechanism floor (transport + decode + fold apply).
    pub fn wire_floor() -> Self {
        Self {
            sensing_coalescing: false,
            announce_debounce: Some(Duration::ZERO),
            min_announce_interval: Some(Duration::ZERO),
        }
    }

    /// Wire floor with the scheduler-input plane armed
    /// (`enable_sensing_coalescing`) so `subscribe_sensing_scheduler_inputs`
    /// is bumped on an inbound capability-fold change (CPB-2).
    pub fn wire_floor_scheduler() -> Self {
        Self {
            sensing_coalescing: true,
            ..Self::wire_floor()
        }
    }

    /// RT-3 debounce isolated: 100 ms debounce, NO rate-limit floor. The
    /// burst-settling number (C5). Explicitly NOT "production defaults".
    pub fn debounce_only() -> Self {
        Self {
            sensing_coalescing: false,
            announce_debounce: Some(Duration::from_millis(100)),
            min_announce_interval: Some(Duration::ZERO),
        }
    }

    /// The shipped defaults untouched (debounce + 10 s rate limit). A
    /// small labeled scenario — thousands of ~10 s samples are
    /// impractical (C5).
    pub fn default_policy() -> Self {
        Self {
            sensing_coalescing: false,
            announce_debounce: None,
            min_announce_interval: None,
        }
    }

    fn mesh_config(&self) -> MeshNodeConfig {
        let addr: SocketAddr = "127.0.0.1:0".parse().expect("bind addr");
        let mut cfg =
            MeshNodeConfig::new(addr, PSK).with_sensing_coalescing(self.sensing_coalescing);
        if let Some(d) = self.announce_debounce {
            cfg = cfg.with_announce_debounce(d);
        }
        if let Some(i) = self.min_announce_interval {
            cfg = cfg.with_min_announce_interval(i);
        }
        cfg
    }
}

// ============================================================================
// Node + topology builders (public API only; started via start_arc).
// ============================================================================

/// Build one node (not yet started) against a `BenchConfig`.
pub async fn node(cfg: &BenchConfig) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg.mesh_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Connect A→B via the handshake + accept pattern (replica of
/// `tests/common::connect_pair`; public-API only). Neither node started.
pub async fn connect(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task panicked").expect("accept");
}

/// A↔B direct, both started via `start_arc()` (D1), warmed so B's fold
/// already knows A — subsequent announces measure steady state, not
/// first-contact handshake.
pub async fn direct_pair(cfg: &BenchConfig) -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(cfg).await;
    let b = node(cfg).await;
    connect(&a, &b).await;
    a.start_arc();
    b.start_arc();
    warm(&a, &b).await;
    (a, b)
}

/// A↔R↔B chain (no direct A–B edge), all started via `start_arc()`,
/// warmed so B has learned A THROUGH the relay R. warm() panics on
/// non-convergence, so a routed measurement can never silently degrade
/// into "no propagation".
pub async fn routed_chain(cfg: &BenchConfig) -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(cfg).await;
    let r = node(cfg).await;
    let b = node(cfg).await;
    connect(&a, &r).await;
    connect(&r, &b).await;
    a.start_arc();
    r.start_arc();
    b.start_arc();
    warm(&a, &b).await;
    (a, r, b)
}

/// Warm a publisher→observer relationship: `a` announces a sentinel
/// manifest; wait (bounded) until `b`'s fold exposes `a`'s node id.
/// Panics on non-convergence so a broken topology fails loud.
pub async fn warm(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    a.announce_capabilities(CapabilitySet::new().add_tag("cpb:warmup"))
        .await
        .expect("warm announce");
    let a_id = a.node_id();
    let ok = wait_until(Duration::from_secs(5), || {
        b.find_nodes_by_filter(&permissive()).contains(&a_id)
    })
    .await;
    assert!(ok, "warm-up: B never learned A's capabilities within 5s");
}

/// Poll `cond` until true or `limit` elapses (returns whether it held).
/// One-time topology warm-up only — never inside a timed region.
pub async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    cond()
}

// ============================================================================
// Exact-state endpoint (C1) — the primitive every timed sample uses.
// ============================================================================

/// Await B's capability state satisfying `predicate`, driven by the
/// missed-wakeup-safe fold watch. Checks the predicate FIRST (the
/// change may already be visible), then parks on `changed()`. The
/// caller stops the timer only after this returns — never at the bare
/// `changed()` wake, because `signal_changed()` runs under the fold
/// write locks (C1). Handles unrelated/intermediate updates without
/// polling: a wake that doesn't satisfy the predicate just re-parks.
pub async fn await_capability_state(
    rx: &mut tokio::sync::watch::Receiver<u64>,
    mut predicate: impl FnMut() -> bool,
) {
    loop {
        if predicate() {
            return;
        }
        rx.changed().await.expect("fold sender alive");
    }
}

// ============================================================================
// Capability-manifest fixtures.
// ============================================================================

/// A minimal single-tag manifest — the small-payload axis.
pub fn manifest_small(tag: &str) -> CapabilitySet {
    CapabilitySet::new().add_tag(tag.to_string())
}

/// A plausible GPU inference worker (loaded 70B model, an H100,
/// service/readiness tags) — the realistic-payload axis. `tag` threads
/// a per-iteration discriminator so successive announces are genuine
/// updates.
pub fn manifest_realistic_gpu(tag: &str) -> CapabilitySet {
    let mut hw = HardwareCapabilities::new();
    hw.cpu_cores = 64;
    hw.cpu_threads = 128;
    hw.memory_gb = 512;
    hw.gpu = Some(GpuInfo::new(GpuVendor::Nvidia, "H100", 80));
    hw.storage_gb = 4000;
    hw.network_gbps = 100;

    CapabilitySet::new()
        .add_tag(tag.to_string())
        .add_tag("gpu")
        .add_tag("service:inference")
        .with_hardware(hw)
        .add_model(ModelCapability {
            model_id: "llama-3.1-70b".to_string(),
            family: "llama".to_string(),
            parameters_b_x10: 700,
            context_length: 131_072,
            quantization: Some("fp16".to_string()),
            modalities: vec![Modality::Text, Modality::Code],
            tokens_per_sec: 1200,
            loaded: true,
        })
}

/// Encoded size of the tested manifest STATE — the "manifest bytes"
/// reporting column. NOT "bytes sent" (D2); the wire may frame,
/// encrypt, fan-out, relay, or retransmit.
pub fn manifest_bytes(caps: &CapabilitySet) -> usize {
    postcard::to_allocvec(caps).map(|v| v.len()).unwrap_or(0)
}

/// A filter matching publishers carrying `tag`.
pub fn require_tag(tag: &str) -> CapabilityFilter {
    CapabilityFilter::new().require_tag(tag.to_string())
}

/// A permissive filter — matches every publisher in the fold.
pub fn permissive() -> CapabilityFilter {
    CapabilityFilter::new()
}

// ============================================================================
// Reporting spine — hdrhistogram + Kyra's metadata and sample protocol.
// ============================================================================

/// Below this sample count p99.9 is not reported (shown as `-`); a
/// handful of hundreds cannot resolve a 99.9th percentile (C10). Policy
/// runs stay under it and report p99 as the tail.
pub const P999_MIN_SAMPLES: u64 = 10_000;

/// Per-row context (C2/C10/D2): the exact start event and endpoint,
/// topology, and the full sampling protocol, so a published number
/// states precisely what it proves.
pub struct RowMeta<'a> {
    pub label: &'a str,
    /// Exact start event, e.g. "publish_call" or "serve_tool".
    pub start_event: &'a str,
    /// Exact endpoint, e.g. "exact-state read (find_nodes_by_filter)".
    pub endpoint: &'a str,
    pub topology: &'a str,
    pub hop_count: u32,
    pub manifest_bytes: usize,
    /// Origin `capability_announce_version()` delta — a VERSION DELTA,
    /// not an emission count (D2).
    pub version_delta: u64,
    pub candidate_pop: usize,
    // Sample protocol (C10):
    pub warmup: u64,
    pub workers: usize,
    pub topology_reused: bool,
    pub timeouts: u64,
    pub outliers: u64,
}

/// hdrhistogram wrapper: nanosecond samples, percentiles in µs.
pub struct LatencyReport {
    hist: Histogram<u64>,
}

impl Default for LatencyReport {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyReport {
    pub fn new() -> Self {
        // 1 ns … 60 s, 3 significant figures — the SDK latency-bench recipe.
        Self {
            hist: Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                .expect("hdrhistogram alloc"),
        }
    }

    pub fn record(&mut self, ns: u64) {
        self.hist.record(ns.max(1)).expect("record");
    }

    pub fn samples(&self) -> u64 {
        self.hist.len()
    }

    /// Print a self-describing result block: what was measured, the
    /// sampling protocol, then the distribution. p99.9 is suppressed
    /// below [`P999_MIN_SAMPLES`].
    pub fn print_row(&self, meta: RowMeta<'_>) {
        let us = |v: u64| v as f64 / 1_000.0;
        let n = self.samples();
        let p999 = if n >= P999_MIN_SAMPLES {
            format!("{:.2}", us(self.hist.value_at_quantile(0.999)))
        } else {
            "-".to_string()
        };
        println!(
            "── {} · {} · hop {} ──",
            meta.label, meta.topology, meta.hop_count
        );
        println!("   start={}  endpoint={}", meta.start_event, meta.endpoint);
        println!(
            "   manifest={} B  version_delta={} (delta, not emissions)  candidate_pop={}",
            meta.manifest_bytes, meta.version_delta, meta.candidate_pop
        );
        println!(
            "   samples={} warmup={} workers={} topo_reused={} timeouts={} outliers={}",
            n,
            meta.warmup,
            meta.workers,
            if meta.topology_reused { "yes" } else { "no" },
            meta.timeouts,
            meta.outliers,
        );
        println!(
            "   p50={:.2}us p95={:.2}us p99={:.2}us p99.9={}us max={:.2}us mean={:.2}us",
            us(self.hist.value_at_quantile(0.50)),
            us(self.hist.value_at_quantile(0.95)),
            us(self.hist.value_at_quantile(0.99)),
            p999,
            us(self.hist.max()),
            self.hist.mean() / 1_000.0,
        );
        println!();
    }
}
