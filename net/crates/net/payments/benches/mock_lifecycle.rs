//! P6 — full mock lifecycle (in-process composition). One lifecycle, two
//! timer endpoints from the SAME start:
//!
//!   Boundary A — quote request → stable billing identity
//!     issue quote → mock proof → accept_payment (verify + settle) → the
//!     stable billing-event id `accept_payment` returns.
//!   Boundary B — quote request → paid handler response
//!     …accepted+billed → redeem_for_invocation → handler executes → response.
//!
//! **Facilitator: in-process zero-delay mock. External rail latency excluded;
//! chain inclusion/finality excluded.** A controlled composition measurement,
//! not a payment-network benchmark. "Billing identity" here is precisely *the
//! stable billing-event id `accept_payment` returns* — no external
//! publication or sink durability is claimed (the harness observes none).
//!
//! Composes existing pieces; no new matrix. c1 headline + c16 composition
//! diagnostic (P3/P4 already establish the global-lock collapse, so c128 is
//! not repeated). Fixed-cardinality operational-filesystem fixture, restored
//! outside timing before every sample. Measures the current implementation —
//! no storage/locking/state/billing/facilitator changes.
//!
//! Run: cargo bench -p net-payments --bench mock_lifecycle

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use net::adapter::net::identity::{EntityId, EntityKeypair};
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::{VerificationTier, VerifierRef};
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine, RedeemDecision};
use net_payments::facilitator::mock::MockFacilitator;
use net_payments::facilitator::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{
    mock_requirements, new_hist, payload_for, record_count, restore_state, runtime, snapshot_state,
    state_bytes, state_placement, MemoryBacking, StatePlacement, AMOUNT, CAPABILITY, NOW, TOOL_ID,
    TTL_NS,
};

const OBS: VerificationTier = VerificationTier::Observed;
const CONCURRENCY: &[usize] = &[1, 16]; // c1 headline, c16 diagnostic; no c128 (see P3/P4)
const BASELINE_RECORDS: u64 = 100;

fn samples() -> usize {
    std::env::var("NET_PAY_BENCH_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(128)
}

/// Counts verify/settle so the lifecycle PROVES "verify once, settle once"
/// (per lifecycle; aggregate == batch size under concurrency).
struct CountingFacilitator {
    inner: MockFacilitator,
    verifies: Arc<AtomicUsize>,
    settles: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Facilitator for CountingFacilitator {
    fn reference(&self) -> VerifierRef {
        self.inner.reference()
    }
    async fn verify(
        &self,
        p: &X402Carry<PaymentPayload>,
        r: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        self.inner.verify(p, r).await
    }
    async fn settle(
        &self,
        p: &X402Carry<PaymentPayload>,
        r: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        self.settles.fetch_add(1, Ordering::SeqCst);
        self.inner.settle(p, r).await
    }
}

struct Fixture {
    engine: Arc<PaymentEngine>,
    caller: EntityKeypair,
    verifies: Arc<AtomicUsize>,
    settles: Arc<AtomicUsize>,
    state_file: std::path::PathBuf,
    placement: StatePlacement,
}

fn build_fixture() -> Fixture {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let registry = default_mock_registry(provider.entity_id().clone());
    let placement = state_placement();
    let state_file = placement.dir.path().join("engine.json");
    let verifies = Arc::new(AtomicUsize::new(0));
    let settles = Arc::new(AtomicUsize::new(0));
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(CountingFacilitator {
                inner: MockFacilitator::new(),
                verifies: Arc::clone(&verifies),
                settles: Arc::clone(&settles),
            }),
            Arc::new(AdmitAll),
            registry,
            state_file.clone(),
        )
        .expect("engine"),
    );
    Fixture {
        engine,
        caller,
        verifies,
        settles,
        state_file,
        placement,
    }
}

/// Issue + settle one quote (baseline seeding / setup — not timed).
async fn settle_one(fx: &Fixture, idx: u64) {
    let quote = fx
        .engine
        .issue_quote(
            fx.caller.entity_id().clone(),
            CAPABILITY,
            mock_requirements(AMOUNT),
            NOW + idx,
            TTL_NS,
        )
        .expect("issue");
    let proof = payload_for(&quote);
    let d = fx
        .engine
        .accept_payment(&quote, &proof, OBS, NOW + idx + 1)
        .await
        .expect("accept");
    assert!(matches!(d, PaymentDecision::Served { .. }));
}

/// One full lifecycle for a fresh quote `idx`. Returns
/// (quote→billing ns, quote→handler ns, billing_id). The handler is trivial —
/// the composition cost is the payment path, not the handler body.
async fn lifecycle(
    engine: Arc<PaymentEngine>,
    caller_id: EntityId,
    idx: u64,
    handler_runs: Arc<AtomicUsize>,
) -> (u64, u64, String) {
    let start = Instant::now();
    let quote = engine
        .issue_quote(
            caller_id,
            CAPABILITY,
            mock_requirements(AMOUNT),
            NOW + idx,
            TTL_NS,
        )
        .expect("issue_quote");
    let proof = payload_for(&quote);
    let decision = engine
        .accept_payment(&quote, &proof, OBS, NOW + idx + 1)
        .await
        .expect("accept_payment");
    let a = start.elapsed().as_nanos() as u64; // endpoint A: quote → billing
    let billing_id = match &decision {
        PaymentDecision::Served { billing, .. } => billing.billing_event_id.clone(),
        other => panic!("lifecycle accept did not Serve: {other:?}"),
    };
    let redeem = engine
        .redeem_for_invocation(TOOL_ID, &quote.quote_id, None)
        .await
        .expect("redeem");
    assert!(
        matches!(redeem, RedeemDecision::Admitted),
        "lifecycle redeem must admit"
    );
    handler_runs.fetch_add(1, Ordering::SeqCst); // "handler executes" (trivial body)
    let b = start.elapsed().as_nanos() as u64; // endpoint B: quote → handler response
    (a, b, billing_id)
}

/// Correctness witness (OUTSIDE timing): a full lifecycle, then replay — the
/// handler must not run again and the retry returns the same billing identity.
async fn replay_witness(fx: &Fixture, idx: u64) {
    restore_state(&fx.state_file, &snapshot_of(fx));
    let engine = &fx.engine;
    let caller = fx.caller.entity_id().clone();
    let quote = engine
        .issue_quote(
            caller,
            CAPABILITY,
            mock_requirements(AMOUNT),
            NOW + idx,
            TTL_NS,
        )
        .expect("issue");
    let proof = payload_for(&quote);
    let id1 = match engine
        .accept_payment(&quote, &proof, OBS, NOW + idx + 1)
        .await
        .expect("accept")
    {
        PaymentDecision::Served { billing, .. } => billing.billing_event_id.clone(),
        other => panic!("witness accept not Served: {other:?}"),
    };
    assert!(matches!(
        engine
            .redeem_for_invocation(TOOL_ID, &quote.quote_id, None)
            .await
            .expect("redeem"),
        RedeemDecision::Admitted
    ));
    // Replay: redeem again → AlreadyRedeemed; the handler must NOT run again.
    let mut handler_ran_again = false;
    if let RedeemDecision::Admitted = engine
        .redeem_for_invocation(TOOL_ID, &quote.quote_id, None)
        .await
        .expect("redeem2")
    {
        handler_ran_again = true;
    }
    assert!(
        !handler_ran_again,
        "replay must not re-admit / re-run the handler"
    );
    // Retry accept → Served via AlreadyServed, SAME billing identity.
    let id2 = match engine
        .accept_payment(&quote, &proof, OBS, NOW + idx + 2)
        .await
        .expect("reaccept")
    {
        PaymentDecision::Served { billing, .. } => billing.billing_event_id.clone(),
        other => panic!("witness reaccept not Served: {other:?}"),
    };
    assert_eq!(id1, id2, "retry returns the same billing identity");
}

fn snapshot_of(fx: &Fixture) -> Vec<u8> {
    snapshot_state(&fx.state_file)
}

fn main() {
    let rt = runtime();
    let n = samples();

    println!("mock_lifecycle — samples={n}, facilitator: in-process zero-delay mock");
    println!("external rail latency: excluded; chain inclusion/finality: excluded");
    println!("A = quote request -> stable billing identity (accept_payment's billing-event id)");
    println!("B = quote request -> paid handler response (accept -> redeem -> handler)");

    for &conc in CONCURRENCY {
        let fx = build_fixture();
        rt.block_on(async {
            for i in 0..BASELINE_RECORDS {
                settle_one(&fx, i).await;
            }
        });
        let baseline = snapshot_of(&fx);
        let bytes_before = state_bytes(&fx.state_file);
        let cards_before = record_count(&fx.state_file);

        let mut hist_a = new_hist();
        let mut hist_b = new_hist();
        let mut idx = 1_000u64;
        let rounds = n.div_ceil(conc);
        let (mut billings, mut handlers) = (0usize, 0usize);

        let start = Instant::now();
        rt.block_on(async {
            for _ in 0..rounds {
                restore_state(&fx.state_file, &baseline); // outside timer
                fx.verifies.store(0, Ordering::SeqCst);
                fx.settles.store(0, Ordering::SeqCst);
                let handler_runs = Arc::new(AtomicUsize::new(0));

                let mut handles = Vec::with_capacity(conc);
                for _ in 0..conc {
                    idx += 1;
                    handles.push(tokio::spawn(lifecycle(
                        Arc::clone(&fx.engine),
                        fx.caller.entity_id().clone(),
                        idx,
                        Arc::clone(&handler_runs),
                    )));
                }
                let mut billing_ids: HashSet<String> = HashSet::new();
                for h in handles {
                    let (a, b, id) = h.await.expect("join");
                    hist_a.record(a).unwrap();
                    hist_b.record(b).unwrap();
                    billing_ids.insert(id);
                }
                assert_eq!(
                    fx.verifies.load(Ordering::SeqCst),
                    conc,
                    "verify once per lifecycle"
                );
                assert_eq!(
                    fx.settles.load(Ordering::SeqCst),
                    conc,
                    "settle once per lifecycle"
                );
                assert_eq!(
                    billing_ids.len(),
                    conc,
                    "one stable billing identity per lifecycle"
                );
                assert_eq!(
                    handler_runs.load(Ordering::SeqCst),
                    conc,
                    "handler once per lifecycle"
                );
                billings += billing_ids.len();
                handlers += handler_runs.load(Ordering::SeqCst);
            }
        });
        let wall = start.elapsed().as_secs_f64();
        let total = rounds * conc;

        rt.block_on(replay_witness(&fx, 900_000));

        let placement = format!(
            "{} ({})",
            fx.state_file.display(),
            match fx.placement.memory_backed {
                MemoryBacking::Asserted => "memory-backed: asserted",
                MemoryBacking::NotAsserted => "memory-backed: not asserted",
            }
        );
        let cards_after = record_count(&fx.state_file);
        let bytes_after = state_bytes(&fx.state_file);
        println!("\n== concurrency {conc} ==");
        report(
            "A quote->billing",
            &hist_a,
            total,
            conc,
            billings as f64 / wall,
        );
        report(
            "B quote->handler",
            &hist_b,
            total,
            conc,
            handlers as f64 / wall,
        );
        println!(
            "      lifecycle_attempts/s={:.1} conc={conc} samples={n} records={cards_before}->{cards_after} \
             bytes={bytes_before}->{bytes_after} binding=off facilitator=in-process-zero-delay-mock timeouts=0",
            total as f64 / wall,
        );
        println!("      {placement}");
    }
}

fn report(
    label: &str,
    hist: &hdrhistogram::Histogram<u64>,
    total: usize,
    _conc: usize,
    completed_per_s: f64,
) {
    let us = |q: f64| hist.value_at_quantile(q) as f64 / 1000.0;
    let p99 = if total >= 500 {
        format!("{:.1}", us(0.99))
    } else {
        "n/a".into()
    };
    println!(
        "  {label:<18} p50={:>9.1}us p95={:>9.1}us p99={p99:>9}us max={:>9.1}us  completed/s={completed_per_s:.1}",
        us(0.50),
        us(0.95),
        hist.max() as f64 / 1000.0,
    );
}
