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
//! fixture seeding, snapshot/restore for single-use stateful sampling,
//! durable-state metadata, and the single public-result reporter
//! ([`BenchMetadata::report`]). The mesh two-node `Pair` (feature `mesh`)
//! is added by the mesh benches when they land (P4/P6).
//!
//! **Reporter contract (P1.1):** every *public* payment result goes through
//! the custom histogram harness and [`BenchMetadata::report`], which prints
//! per-op p50/p95/p99, the three throughputs ([`Throughput`]), and a full
//! environment/metadata line. Criterion is for *diagnostic* microbenchmarks
//! only — its bootstrap confidence interval is not a per-op percentile and
//! cannot satisfy the public-output contract.
//!
//! Durable state placement follows decision D1 in
//! `docs/plans/PAYMENTS_BENCHMARKS_PLAN.md`: the ordinary operational
//! filesystem is the PRIMARY result (the durable JSON+fsync transaction IS
//! the current payment semantics); tmpfs is a diagnostic floor that the
//! operator must opt into AND label. We never infer "memory-backed" from a
//! path, and absence of the assertion does not prove the path is disk-backed.

#![allow(dead_code)] // each bench uses only a subset of these helpers

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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
/// The mock facilitator settles with zero injected delay — recorded in
/// metadata so a reader knows external-rail latency is excluded by design.
pub const MOCK_FACILITATOR_DELAY_MS: u64 = 0;

// ============================================================================
// Durable state placement (decision D1) — tri-state memory backing.
// ============================================================================

/// Whether the state directory is memory-backed. This is a *claim*, never an
/// inference: we can only record that the operator asserted it. `NotAsserted`
/// means exactly that — it does NOT prove the path is disk-backed.
#[derive(Clone, Copy)]
pub enum MemoryBacking {
    Asserted,
    NotAsserted,
}

impl MemoryBacking {
    pub fn label(self) -> &'static str {
        match self {
            Self::Asserted => "memory-backed: asserted",
            Self::NotAsserted => "memory-backed: not asserted",
        }
    }
}

/// Where a fixture's JSON state lives, plus the memory-backing claim.
pub struct StatePlacement {
    pub dir: tempfile::TempDir,
    pub memory_backed: MemoryBacking,
}

/// Resolve the state directory (D1):
/// - `NET_PAY_BENCH_STATE_DIR` set → create state under that base (the
///   diagnostic-floor knob, typically a tmpfs mount). `memory_backed` is
///   `Asserted` only if `NET_PAY_BENCH_STATE_TMPFS=1` is ALSO set.
/// - unset → the OS temp dir (the primary operational filesystem),
///   `NotAsserted`.
///
/// Fails loudly if `NET_PAY_BENCH_STATE_TMPFS=1` is supplied without a
/// `NET_PAY_BENCH_STATE_DIR` — we refuse to assert memory-backed for the
/// default OS temp dir (on macOS it is disk).
pub fn state_placement() -> StatePlacement {
    let tmpfs_asserted = std::env::var("NET_PAY_BENCH_STATE_TMPFS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    match std::env::var_os("NET_PAY_BENCH_STATE_DIR") {
        Some(base) => {
            let dir = tempfile::Builder::new()
                .prefix("net-pay-bench-")
                .tempdir_in(&base)
                .expect("tempdir under NET_PAY_BENCH_STATE_DIR");
            let memory_backed = if tmpfs_asserted {
                MemoryBacking::Asserted
            } else {
                MemoryBacking::NotAsserted
            };
            StatePlacement { dir, memory_backed }
        }
        None => {
            assert!(
                !tmpfs_asserted,
                "NET_PAY_BENCH_STATE_TMPFS=1 requires NET_PAY_BENCH_STATE_DIR (the tmpfs \
                 mount); refusing to assert memory-backed for the default OS temp dir"
            );
            StatePlacement {
                dir: tempfile::tempdir().expect("tempdir"),
                memory_backed: MemoryBacking::NotAsserted,
            }
        }
    }
}

/// Bytes of the JSON state file. A missing file → 0 (the first-run case).
/// Any OTHER error (permission, metadata) FAILS the bench rather than
/// masquerading as an empty store.
pub fn state_bytes(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("state_bytes({}): {e}", path.display()),
    }
}

/// Number of quote records in the store (0 if missing). The engine's state
/// type is crate-private, so we parse the JSON generically and count the
/// `quotes` object — the store's record cardinality. Non-NotFound I/O and
/// parse errors fail the bench.
pub fn record_count(path: &Path) -> usize {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(e) => panic!("record_count({}): {e}", path.display()),
    };
    let v: serde_json::Value = serde_json::from_slice(&raw).expect("state file is valid JSON");
    v.get("quotes")
        .and_then(|q| q.as_object())
        .map(|o| o.len())
        .unwrap_or(0)
}

/// Snapshot the raw state-file bytes (empty if missing). Restore with
/// [`restore_state`] to reset a fixture to a known baseline OUTSIDE the
/// timed region — required for single-use stateful samples (a fresh accept,
/// or an unredeemed redemption) so cardinality / redeemed state is identical
/// at the start of every timed sample. See the plan's fixture protocol.
pub fn snapshot_state(path: &Path) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("snapshot_state({}): {e}", path.display()),
    }
}

/// Restore the state file to a snapshot (write temp + rename). Call OUTSIDE
/// the timer. An empty snapshot removes the file (baseline = first-run).
pub fn restore_state(path: &Path, snapshot: &[u8]) {
    if snapshot.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("restore_state remove({}): {e}", path.display()),
        }
        return;
    }
    let tmp = path.with_extension("restore.tmp");
    std::fs::write(&tmp, snapshot).expect("write restore temp");
    std::fs::rename(&tmp, path).expect("rename restore temp");
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

    /// Current number of quote records in the store.
    pub fn record_count(&self) -> usize {
        record_count(&self.state_file)
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
/// return them. FIXTURE builder: run it before timing so the measured op
/// sees a store of known size (store cardinality is a controlled axis, never
/// a side effect of sample count). Callers read `fx.record_count()` /
/// `fx.state_bytes()` before/after to prove the count held.
pub async fn mint_n_settled(fx: &EngineFixture, n: u64) -> Vec<PaymentQuote> {
    let mut quotes = Vec::with_capacity(n as usize);
    for i in 0..n {
        quotes.push(mint_settled(fx, i).await);
    }
    quotes
}

// ============================================================================
// Runtime.
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

// ============================================================================
// The public-result reporter — three throughputs + unified metadata.
// ============================================================================

/// The three throughput numbers the plan's contract requires. A single
/// "throughput" would lie: a duplicate storm produces high `attempts_per_s`
/// but one admission. For ordinary successful rows all three are equal; for
/// storms and denials they diverge.
#[derive(Clone, Copy)]
pub struct Throughput {
    pub attempts_per_s: f64,
    pub admissions_per_s: f64,
    pub unique_payments_per_s: f64,
}

impl Throughput {
    /// All three equal — an ordinary successful row.
    pub fn uniform(v: f64) -> Self {
        Self {
            attempts_per_s: v,
            admissions_per_s: v,
            unique_payments_per_s: v,
        }
    }

    /// A denial row: attempts flow, nothing is admitted or paid.
    pub fn denial(attempts_per_s: f64) -> Self {
        Self {
            attempts_per_s,
            admissions_per_s: 0.0,
            unique_payments_per_s: 0.0,
        }
    }
}

/// The unified metadata every custom-harness (public) result prints, via the
/// one shared [`report`](BenchMetadata::report) method. Later phases must not
/// hand-format a different subset — construct one of these and print it.
#[derive(Clone)]
pub struct BenchMetadata {
    pub label: String,
    pub samples: usize,
    pub warmups: usize,
    pub concurrency: usize,
    pub runtime_workers: usize,
    pub records_before: usize,
    pub records_after: usize,
    pub state_bytes_before: u64,
    pub state_bytes_after: u64,
    pub state_path: String,
    pub memory_backed: MemoryBacking,
    pub binding_enabled: bool,
    pub billing_sink: bool,
    pub facilitator_delay_ms: u64,
    pub fixture_prep: Duration,
}

impl BenchMetadata {
    /// A base record for a fixture, with per-row fields (label, concurrency,
    /// samples, records/bytes after, throughput) filled in by the caller via
    /// [`Self::for_row`]. `records_before` / `state_bytes_before` are read
    /// from the fixture now (the prepared baseline).
    pub fn base(fx: &EngineFixture, fixture_prep: Duration) -> Self {
        Self {
            label: String::new(),
            samples: 0,
            warmups: 0,
            concurrency: 0,
            runtime_workers: worker_threads(),
            records_before: fx.record_count(),
            records_after: fx.record_count(),
            state_bytes_before: fx.state_bytes(),
            state_bytes_after: fx.state_bytes(),
            state_path: fx.state_file.display().to_string(),
            memory_backed: fx.placement.memory_backed,
            binding_enabled: false,
            billing_sink: false,
            facilitator_delay_ms: MOCK_FACILITATOR_DELAY_MS,
            fixture_prep,
        }
    }

    /// Derive a per-row record from this base.
    pub fn for_row(
        &self,
        label: impl Into<String>,
        samples: usize,
        concurrency: usize,
        binding_enabled: bool,
        fx: &EngineFixture,
    ) -> Self {
        let mut m = self.clone();
        m.label = label.into();
        m.samples = samples;
        m.concurrency = concurrency;
        m.binding_enabled = binding_enabled;
        m.records_after = fx.record_count();
        m.state_bytes_after = fx.state_bytes();
        m
    }

    /// Print this result: latency percentiles + the three throughputs + the
    /// full environment/metadata line. THE single public-result format.
    pub fn report(&self, hist: &Histogram<u64>, tput: &Throughput) {
        let to_us = |v: u64| v as f64 / 1_000.0;
        println!(
            "  {label:<26} p50={p50:>10.2}us p95={p95:>10.2}us p99={p99:>10.2}us max={max:>10.2}us",
            label = self.label,
            p50 = to_us(hist.value_at_quantile(0.50)),
            p95 = to_us(hist.value_at_quantile(0.95)),
            p99 = to_us(hist.value_at_quantile(0.99)),
            max = to_us(hist.max()),
        );
        println!(
            "      throughput: attempts/s={:.0} admissions/s={:.0} unique_payments/s={:.0}",
            tput.attempts_per_s, tput.admissions_per_s, tput.unique_payments_per_s,
        );
        println!(
            "      meta: samples={} warmups={} conc={} workers={} records={}->{} bytes={}->{} \
             {mb} binding={} billing_sink={} facilitator_delay_ms={} fixture_prep_ms={:.1}",
            self.samples,
            self.warmups,
            self.concurrency,
            self.runtime_workers,
            self.records_before,
            self.records_after,
            self.state_bytes_before,
            self.state_bytes_after,
            self.binding_enabled,
            self.billing_sink,
            self.facilitator_delay_ms,
            self.fixture_prep.as_secs_f64() * 1000.0,
            mb = self.memory_backed.label(),
        );
        println!("      path={}", self.state_path);
    }
}
