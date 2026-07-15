//! P3 — duplicate storms. Two distinct money-path invariants under
//! concurrency, measured AND asserted (custom hdrhistogram harness). This is
//! also the at-most-once / concurrency baseline any later lock-regime change
//! must preserve.
//!
//! **Duplicate acceptance storm:** N concurrent `accept_payment` on the SAME
//! quote + payload. Invariant: the facilitator verifies once and settles
//! once; exactly one fresh billing event is created; every attempt that
//! returns Served carries the SAME billing id; no duplicate records.
//! Timing-tolerant: contenders that arrive while the claim is in-flight
//! return `InProgress` — they are retried after the storm and must then
//! return the same billing event (per review).
//!
//! **Duplicate redemption storm:** N concurrent `redeem_for_invocation` for
//! the SAME settled quote. Invariant: exactly one returns `Admitted`; all
//! others `AlreadyRedeemed`; the handler (a counter) runs exactly once.
//! (`redeem` returns no billing, so "same billing" belongs to the acceptance
//! storm, not here.)
//!
//! Throughput is three numbers on purpose: a storm produces high attempts/s
//! but ONE unique payment (acceptance) / ONE admission (redemption) per
//! storm — a single "throughput" would lie.
//!
//! Run: cargo bench -p net-payments --bench duplicate_storm

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use net::adapter::net::identity::EntityKeypair;
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
    issue, mint_settled, new_hist, payload_for, runtime, state_placement, BenchMetadata,
    EngineFixture, Throughput, AMOUNT, NOW, TOOL_ID,
};

const OBS: VerificationTier = VerificationTier::Observed;
const CONCURRENCY: &[usize] = &[16, 128];

fn storms() -> usize {
    std::env::var("NET_PAY_BENCH_STORMS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(8)
}

/// A facilitator that counts verify/settle calls, so the acceptance storm can
/// PROVE "verify once, settle once" rather than infer it. Delegates to the
/// mock in every other respect.
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
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        self.inner.verify(payload, requirements).await
    }
    async fn settle(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        self.settles.fetch_add(1, Ordering::SeqCst);
        self.inner.settle(payload, requirements).await
    }
}

struct CountingFixture {
    fx: EngineFixture,
    verifies: Arc<AtomicUsize>,
    settles: Arc<AtomicUsize>,
}

/// An engine over a counting facilitator, reusing bench_common's identities,
/// registry, and D1-compliant state placement.
fn build_counting_fixture() -> CountingFixture {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let registry = default_mock_registry(provider.entity_id().clone());
    let placement = state_placement();
    let state_file = placement.dir.path().join("engine.json");
    let verifies = Arc::new(AtomicUsize::new(0));
    let settles = Arc::new(AtomicUsize::new(0));
    let facilitator = Arc::new(CountingFacilitator {
        inner: MockFacilitator::new(),
        verifies: Arc::clone(&verifies),
        settles: Arc::clone(&settles),
    });
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            facilitator,
            Arc::new(AdmitAll),
            registry.clone(),
            state_file.clone(),
        )
        .expect("build PaymentEngine"),
    );
    CountingFixture {
        fx: EngineFixture {
            engine,
            provider,
            caller,
            registry,
            state_file,
            placement,
        },
        verifies,
        settles,
    }
}

fn main() {
    let rt = runtime();
    let m = storms();

    println!("duplicate_storm — storms_per_cell={m}, facilitator=mock(zero-delay,counting), tool={TOOL_ID}");

    // ===================== Duplicate acceptance storm =====================
    println!("\n## Duplicate acceptance storm — N concurrent accept_payment(same quote+payload)");
    for &conc in CONCURRENCY {
        let cf = build_counting_fixture();
        let prep_start = Instant::now();
        let base = BenchMetadata::base(&cf.fx, prep_start.elapsed());

        let mut hist = new_hist();
        let mut total_attempts = 0usize;
        let mut total_served = 0usize;
        let mut unique_payments = 0usize; // distinct billing ids across storms

        let cell_start = Instant::now();
        rt.block_on(async {
            for storm in 0..m {
                let issued = NOW + 1_000_000 * (storm as u64 + 1);
                let quote = Arc::new(issue(&cf.fx, AMOUNT, issued));
                let proof = Arc::new(payload_for(&quote));
                cf.verifies.store(0, Ordering::SeqCst);
                cf.settles.store(0, Ordering::SeqCst);

                // Fire N concurrent accepts on the one quote.
                let mut handles = Vec::with_capacity(conc);
                for _ in 0..conc {
                    let engine = Arc::clone(&cf.fx.engine);
                    let quote = Arc::clone(&quote);
                    let proof = Arc::clone(&proof);
                    handles.push(tokio::spawn(async move {
                        let t = Instant::now();
                        let d = engine
                            .accept_payment(&quote, &proof, OBS, issued + 1)
                            .await
                            .expect("accept_payment");
                        (t.elapsed().as_nanos() as u64, d)
                    }));
                }

                let mut billing_ids: HashSet<String> = HashSet::new();
                let mut in_progress = 0usize;
                for h in handles {
                    let (ns, d) = h.await.expect("join");
                    hist.record(ns).expect("record");
                    total_attempts += 1;
                    match d {
                        PaymentDecision::Served { billing, .. } => {
                            total_served += 1;
                            billing_ids.insert(billing.billing_event_id.clone());
                        }
                        PaymentDecision::InProgress => in_progress += 1,
                        other => panic!("unexpected storm outcome: {other:?}"),
                    }
                }

                // Timing-tolerance: retry the in-flight losers now that the
                // storm settled; each must return the SAME billing event.
                for _ in 0..in_progress {
                    let d = cf
                        .fx
                        .engine
                        .accept_payment(&quote, &proof, OBS, issued + 1)
                        .await
                        .expect("accept retry");
                    match d {
                        PaymentDecision::Served { billing, .. } => {
                            billing_ids.insert(billing.billing_event_id.clone());
                        }
                        other => panic!("InProgress retry did not settle: {other:?}"),
                    }
                }

                // Invariants: settle once, one billing, all retries identical.
                assert_eq!(cf.verifies.load(Ordering::SeqCst), 1, "verify must run once");
                assert_eq!(cf.settles.load(Ordering::SeqCst), 1, "settle must run once");
                assert_eq!(
                    billing_ids.len(),
                    1,
                    "one billing event; every Served retry returns it"
                );
                unique_payments += billing_ids.len();
            }
        });

        let wall = cell_start.elapsed().as_secs_f64();
        let tput = Throughput {
            attempts_per_s: total_attempts as f64 / wall,
            admissions_per_s: total_served as f64 / wall,
            unique_payments_per_s: unique_payments as f64 / wall,
        };
        base.for_row(format!("accept_storm c{conc}"), total_attempts, conc, false, &cf.fx)
            .report(&hist, &tput);
        println!(
            "      invariant: {m} storms · verify/settle once each · unique_payments={unique_payments} (== storms)"
        );
    }

    // ===================== Duplicate redemption storm =====================
    println!("\n## Duplicate redemption storm — N concurrent redeem(same settled quote)");
    for &conc in CONCURRENCY {
        let cf = build_counting_fixture();
        let prep_start = Instant::now();
        let base = BenchMetadata::base(&cf.fx, prep_start.elapsed());

        let mut hist = new_hist();
        let mut total_attempts = 0usize;
        let mut total_admitted = 0usize;

        let cell_start = Instant::now();
        rt.block_on(async {
            for storm in 0..m {
                // A fresh settled, redeemable quote for this storm.
                let quote = mint_settled(&cf.fx, storm as u64).await;
                let qid = Arc::new(quote.quote_id.clone());
                let handler = Arc::new(AtomicUsize::new(0));

                let mut handles = Vec::with_capacity(conc);
                for _ in 0..conc {
                    let engine = Arc::clone(&cf.fx.engine);
                    let qid = Arc::clone(&qid);
                    let handler = Arc::clone(&handler);
                    handles.push(tokio::spawn(async move {
                        let t = Instant::now();
                        let d = engine
                            .redeem_for_invocation(TOOL_ID, &qid, None)
                            .await
                            .expect("redeem_for_invocation");
                        let admitted = matches!(d, RedeemDecision::Admitted);
                        if admitted {
                            // The bench "runs the handler" only for Admitted.
                            handler.fetch_add(1, Ordering::SeqCst);
                        }
                        (t.elapsed().as_nanos() as u64, admitted)
                    }));
                }

                let mut admitted = 0usize;
                for h in handles {
                    let (ns, adm) = h.await.expect("join");
                    hist.record(ns).expect("record");
                    total_attempts += 1;
                    if adm {
                        admitted += 1;
                    }
                }

                assert_eq!(admitted, 1, "exactly one redemption admits");
                assert_eq!(handler.load(Ordering::SeqCst), 1, "the handler runs exactly once");
                total_admitted += admitted;
            }
        });

        let wall = cell_start.elapsed().as_secs_f64();
        let tput = Throughput {
            attempts_per_s: total_attempts as f64 / wall,
            admissions_per_s: total_admitted as f64 / wall,
            unique_payments_per_s: total_admitted as f64 / wall, // one invocation admitted per storm
        };
        base.for_row(format!("redeem_storm c{conc}"), total_attempts, conc, false, &cf.fx)
            .report(&hist, &tput);
        println!("      invariant: {m} storms · exactly one Admitted + one handler run each");
    }
}
