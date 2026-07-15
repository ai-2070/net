//! Shared in-process harness for the `net-payments` benchmark suite.
//! Every bench under `benches/*.rs` pulls this in via
//! `#[path = "bench_common/mod.rs"] mod bench_common;`.
//!
//! Why a directory (not `bench_common.rs`): so Cargo's bench
//! auto-discovery never treats it as a target. `autobenches = false`
//! in `payments/Cargo.toml` reinforces this — every bench is registered
//! explicitly with `[[bench]]`. Same regime as `sdk/benches/nrpc_common`.
//!
//! Scope: the *non-mesh* money path — an in-process `PaymentEngine` over
//! the (zero-delay) mock facilitator, quote/proof minting, fixed-cardinality
//! fixture seeding, durable-state metadata, and a p50/p95/p99 + throughput
//! reporter. The mesh two-node `Pair` (feature `mesh`) is added by the mesh
//! benches when they land (P4/P6).
//!
//! Durable state placement follows decision D1 in
//! `docs/plans/PAYMENTS_BENCHMARKS_PLAN.md`: the ordinary operational
//! filesystem is the PRIMARY result (the durable JSON+fsync transaction IS
//! the current payment semantics); tmpfs is a diagnostic floor that the
//! operator must opt into AND label — we never infer "memory-backed" from a
//! path, and never assume `std::env::temp_dir()` is tmpfs.

#![allow(dead_code)] // each bench uses only a subset of these helpers

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hdrhistogram::Histogram;
use net::adapter::net::identity::EntityKeypair;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use tokio::runtime::{Builder as RtBuilder, Runtime};

// ============================================================================
// Constants — the fixture identity/timing every bench shares.
// ============================================================================

/// A fixed, far-from-zero base timestamp (ns). Matches the payment tests
/// so quote expiry math is identical to what the suite already asserts.
pub const NOW: u64 = 1_000_000_000_000_000;
/// Default quote TTL: 60 s in ns.
pub const TTL_NS: u64 = 60_000_000_000;
/// The capability every minted quote is issued against.
pub const CAPABILITY: &str = "fixture-provider/fixture-tool";
/// The tool id `redeem_for_invocation` binds to (the tail of `CAPABILITY`).
pub const TOOL_ID: &str = "fixture-tool";
/// Default per-call amount, in atomic units of the mock asset.
pub const AMOUNT: &str = "2500";

// ============================================================================
// Durable state placement (decision D1).
// ============================================================================

/// Where a fixture's JSON state lives, plus whether the operator asserted it
/// is memory-backed. `memory_backed` is set ONLY by an explicit
/// `NET_PAY_BENCH_STATE_TMPFS=1`; it is never inferred from the path.
pub struct StatePlacement {
    pub dir: tempfile::TempDir,
    pub memory_backed: bool,
}

/// Resolve the state directory (D1):
/// - `NET_PAY_BENCH_STATE_DIR` set → create the state under that base. This
///   is the diagnostic-floor knob (typically a tmpfs mount); the operator
///   must ALSO set `NET_PAY_BENCH_STATE_TMPFS=1` to label it memory-backed,
///   otherwise we report it as an ordinary path.
/// - unset → the OS temp dir, reported as the primary operational filesystem
///   (NOT assumed tmpfs).
pub fn state_placement() -> StatePlacement {
    match std::env::var_os("NET_PAY_BENCH_STATE_DIR") {
        Some(base) => {
            let memory_backed = std::env::var("NET_PAY_BENCH_STATE_TMPFS")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let dir = tempfile::Builder::new()
                .prefix("net-pay-bench-")
                .tempdir_in(&base)
                .expect("tempdir under NET_PAY_BENCH_STATE_DIR");
            StatePlacement { dir, memory_backed }
        }
        None => StatePlacement {
            dir: tempfile::tempdir().expect("tempdir"),
            memory_backed: false,
        },
    }
}

/// Bytes of the JSON state file (0 if it does not exist yet — e.g. before
/// the first write). The store's total serialized size is the dominant
/// admission cost (F2), so benches report this before and after.
pub fn state_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// ============================================================================
// Engine fixture — a mock-money-path `PaymentEngine` and its identities.
// ============================================================================

/// An engine wired for the mock money path (`MockFacilitator` + `AdmitAll`),
/// its provider/caller identities, a registry clone, and the durable state
/// placement (held so state survives for the fixture's life, cleaned on drop).
pub struct EngineFixture {
    pub engine: Arc<PaymentEngine>,
    pub provider: Arc<EntityKeypair>,
    pub caller: EntityKeypair,
    pub registry: AssetRegistry,
    pub state_file: PathBuf,
    pub placement: StatePlacement,
}

impl EngineFixture {
    /// Absolute path of the engine's JSON state file.
    pub fn state_path(&self) -> &Path {
        &self.state_file
    }

    /// Current serialized size of the state file, in bytes.
    pub fn state_bytes(&self) -> u64 {
        state_bytes(&self.state_file)
    }

    /// A one-line placement label for a metadata row: absolute path plus
    /// whether the operator asserted it is memory-backed.
    pub fn placement_label(&self) -> String {
        format!(
            "{} ({})",
            self.state_file.display(),
            if self.placement.memory_backed {
                "memory-backed (asserted)"
            } else {
                "operational-fs"
            }
        )
    }
}

/// Build a fresh engine over its own state file. Cheap enough to call once
/// per bench — never inside a timed loop (it does filesystem + keygen work).
pub fn build_engine() -> EngineFixture {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let registry = default_mock_registry(provider.entity_id().clone());
    let placement = state_placement();
    let state_file = placement.dir.path().join("engine.json");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            state_file.clone(),
        )
        .expect("build PaymentEngine"),
    );
    EngineFixture {
        engine,
        provider,
        caller,
        registry,
        state_file,
        placement,
    }
}

// ============================================================================
// Quote / proof minting + fixed-cardinality fixture seeding.
// ============================================================================

/// Author the mock `PaymentRequirements` for `amount`.
pub fn mock_requirements(amount: &str) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: amount.into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author requirements")
}

/// Issue a signed quote against `CAPABILITY` at `issued_ns`. Distinct
/// `issued_ns` yields distinct `quote_id`s, so callers that need N
/// independent quotes just vary this.
pub fn issue(fx: &EngineFixture, amount: &str, issued_ns: u64) -> PaymentQuote {
    fx.engine
        .issue_quote(
            fx.caller.entity_id().clone(),
            CAPABILITY,
            mock_requirements(amount),
            issued_ns,
            TTL_NS,
        )
        .expect("issue_quote")
}

/// Author the payment proof for `quote`. The authorization nonce is the
/// quote id, so each distinct quote gets a distinct replay key (no
/// cross-quote replay), while re-authoring the SAME quote reproduces the
/// SAME proof — exactly the duplicate-storm input.
pub fn payload_for(quote: &PaymentQuote) -> X402Carry<PaymentPayload> {
    X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": quote.quote_id }),
        extensions: None,
    })
    .expect("author payload")
}

/// Issue a quote and settle it through `accept_payment` (mock verify +
/// settle), asserting it Served. Returns the settled quote — its
/// `quote_id` is ready for `redeem_for_invocation`. Distinct `i` ⇒ distinct
/// quote, so N mints give N independently-redeemable, already-paid quotes.
pub async fn mint_settled(fx: &EngineFixture, i: u64) -> PaymentQuote {
    let quote = issue(fx, AMOUNT, NOW + i);
    let payload = payload_for(&quote);
    let decision = fx
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + i + 1)
        .await
        .expect("accept_payment");
    assert!(
        matches!(decision, PaymentDecision::Served { .. }),
        "mint_settled expected a Served decision"
    );
    quote
}

/// Seed the store to a fixed cardinality of `n` already-settled quotes and
/// return them. This is a FIXTURE builder: run it before timing so the
/// measured op sees a store of known size (decision: store cardinality is a
/// controlled axis, never a side effect of sample count). Callers read
/// `fx.state_bytes()` before/after to prove the count held.
pub async fn mint_n_settled(fx: &EngineFixture, n: u64) -> Vec<PaymentQuote> {
    let mut quotes = Vec::with_capacity(n as usize);
    for i in 0..n {
        quotes.push(mint_settled(fx, i).await);
    }
    quotes
}

// ============================================================================
// Runtime + histogram reporting (for the custom-harness benches).
// ============================================================================

/// Worker-thread count for the bench runtime, overridable via
/// `NET_PAY_BENCH_WORKER_THREADS` (default 4, matching the nRPC suite).
pub fn worker_threads() -> usize {
    std::env::var("NET_PAY_BENCH_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4)
}

/// Multi-threaded tokio runtime shared by every bench.
pub fn runtime() -> Runtime {
    RtBuilder::new_multi_thread()
        .worker_threads(worker_threads())
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// A fresh latency histogram: 1 ns .. 60 s, 3 significant figures — covers
/// every plausible admission latency this suite records.
pub fn new_hist() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("hdrhistogram alloc")
}

/// Print the shared table header for a custom-harness bench.
pub fn print_header(title: &str) {
    println!("{title}");
    println!(
        "  {:>18}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}",
        "case", "p50_us", "p95_us", "p99_us", "max_us", "throughput/s"
    );
}

/// Print one p50/p95/p99/max + throughput row (latencies ns → µs). The
/// `throughput_per_s` label (attempts / admissions / unique payments) is
/// the caller's responsibility — a single number would lie for a storm.
pub fn print_row(label: &str, hist: &Histogram<u64>, throughput_per_s: f64) {
    let to_us = |v: u64| v as f64 / 1_000.0;
    println!(
        "  {:>18}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}  {:>12.0}",
        label,
        to_us(hist.value_at_quantile(0.50)),
        to_us(hist.value_at_quantile(0.95)),
        to_us(hist.value_at_quantile(0.99)),
        to_us(hist.max()),
        throughput_per_s,
    );
}
