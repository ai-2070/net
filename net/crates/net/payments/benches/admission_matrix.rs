//! P2 — the published admission matrix (custom hdrhistogram harness).
//!
//! This is the headline the plan promised: what latency Net adds when a
//! proof is already available, measured end to end at concurrency 1 with a
//! zero-delay mock facilitator (external-rail latency excluded by design).
//! Everything here reports through `BenchMetadata::report` — per-op
//! p50/p95/p99, three throughputs, full environment metadata.
//!
//! Three sections:
//!   1. **Boundary 2 — exact-proof provider admission** (the headline):
//!      `accept_payment` + `redeem_for_invocation`, the acceptance transition
//!      `0→1 / 99→100 / 999→1 000`. `accept_payment` INSERTS a record, so
//!      cardinality cannot be held — each sample restores the prepared
//!      baseline OUTSIDE the timer and authors a fresh quote/proof, then
//!      times accept+redeem.
//!   2. **Boundary 1 — ready-settled redemption gate**: `redeem_for_invocation`
//!      alone on an already-settled quote, cardinality held `N→N`, restored
//!      to an un-redeemed baseline before every sample.
//!   3. **Rejection matrix**: each `accept_payment` failure class, labeled by
//!      whether it rejects before state access (pre-state) or touches durable
//!      state. `in_progress` is a race (a concurrent active claim) and is
//!      measured in the P3 acceptance storm, not here.
//!
//! Raw output is preserved in `docs/performance/payments-admission-matrix.md`
//! as the baseline for later store-architecture work.
//!
//! Run: cargo bench -p net-payments --bench admission_matrix

use std::sync::Arc;
use std::time::Instant;

use hdrhistogram::Histogram;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{PaymentDecision, PaymentEngine, RedeemDecision, RejectReason};
use net_payments::facilitator::mock::MockMode;
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{
    build_engine, build_engine_with_mode, issue, mint_n_settled, mock_requirements, new_hist,
    payload_for, payload_with_nonce, restore_state, runtime, snapshot_state, BenchMetadata,
    Throughput, AMOUNT, NOW, TOOL_ID, TTL_NS,
};

const OBS: VerificationTier = VerificationTier::Observed;

fn samples() -> usize {
    std::env::var("NET_PAY_BENCH_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(60)
}

/// Author a payload whose `accepted` is `reqs` (used to force a
/// payload↔requirements mismatch, and for the shared-nonce replay input).
fn payload_accepting(reqs: &X402Carry<PaymentRequirements>, nonce: &str) -> X402Carry<PaymentPayload> {
    X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: reqs.view().clone(),
        payload: serde_json::json!({ "mock_authorization": nonce }),
        extensions: None,
    })
    .expect("author payload")
}

fn main() {
    let rt = runtime();
    let n = samples();

    println!("admission_matrix — samples={n}, facilitator=mock(zero-delay), tier=Observed, conc=1");
    println!("boundary 2 (headline) = accept_payment + redeem_for_invocation");

    // ======================================================================
    // 1. Boundary 2 — exact-proof provider admission (accept + redeem).
    //    Transition 0→1 / 99→100 / 999→1 000; restore-per-sample.
    // ======================================================================
    println!("\n## Boundary 2 — exact-proof admission (accept + redeem), restore-per-sample");
    for &seed in &[0u64, 99, 999] {
        let prep_start = Instant::now();
        let fx = build_engine();
        rt.block_on(mint_n_settled(&fx, seed));
        let prep = prep_start.elapsed();
        let baseline = snapshot_state(fx.state_path());
        let base = BenchMetadata::base(&fx, prep);

        let (hist, admits, wall) = rt.block_on(async {
            let mut hist = new_hist();
            let mut admits = 0usize;
            let start = Instant::now();
            for i in 0..n {
                // Prepared baseline + fresh quote/proof — OUTSIDE the timer.
                restore_state(fx.state_path(), &baseline);
                let issued = NOW + 1_000_000 + i as u64;
                let quote = issue(&fx, AMOUNT, issued);
                let proof = payload_for(&quote);

                let t = Instant::now();
                let decision = fx
                    .engine
                    .accept_payment(&quote, &proof, OBS, issued + 1)
                    .await
                    .expect("accept_payment");
                let redeem = fx
                    .engine
                    .redeem_for_invocation(TOOL_ID, &quote.quote_id, None)
                    .await
                    .expect("redeem_for_invocation");
                let elapsed = t.elapsed().as_nanos() as u64;
                hist.record(elapsed).expect("record");

                assert!(matches!(decision, PaymentDecision::Served { .. }), "accept must Serve");
                assert!(matches!(redeem, RedeemDecision::Admitted), "redeem must Admit");
                admits += 1;
            }
            (hist, admits, start.elapsed().as_secs_f64())
        });

        // Each timed op transitions the store seed → seed+1.
        base.for_row(format!("accept+redeem {seed}->{}", seed + 1), n, 1, false, &fx)
            .report(&hist, &Throughput::uniform(admits as f64 / wall));
    }

    // ======================================================================
    // 2. Boundary 1 — ready-settled redemption gate. Cardinality held N→N;
    //    restore an un-redeemed baseline before every sample.
    // ======================================================================
    println!("\n## Boundary 1 — ready-settled redemption gate (redeem only), restore-per-sample");
    for &card in &[1u64, 100, 1000] {
        let prep_start = Instant::now();
        let fx = build_engine();
        let quotes = rt.block_on(mint_n_settled(&fx, card));
        let prep = prep_start.elapsed();
        let target = quotes[0].quote_id.clone();
        let baseline = snapshot_state(fx.state_path()); // all un-redeemed
        let base = BenchMetadata::base(&fx, prep);

        let (hist, admits, wall) = rt.block_on(async {
            let mut hist = new_hist();
            let mut admits = 0usize;
            let start = Instant::now();
            for _ in 0..n {
                restore_state(fx.state_path(), &baseline);
                let t = Instant::now();
                let redeem = fx
                    .engine
                    .redeem_for_invocation(TOOL_ID, &target, None)
                    .await
                    .expect("redeem_for_invocation");
                hist.record(t.elapsed().as_nanos() as u64).expect("record");
                assert!(matches!(redeem, RedeemDecision::Admitted), "redeem must Admit");
                admits += 1;
            }
            (hist, admits, start.elapsed().as_secs_f64())
        });

        base.for_row(format!("redeem_only card={card}"), n, 1, false, &fx)
            .report(&hist, &Throughput::uniform(admits as f64 / wall));
    }

    // ======================================================================
    // 3. Rejection matrix — accept_payment failure classes (card ~100).
    // ======================================================================
    println!("\n## Rejection matrix — accept_payment outcomes (card~100, conc=1)");

    // ---- pre-state rows (reject before any state access; repeatable) ----
    // One shared, read-only engine seeded to 100.
    let prep_start = Instant::now();
    let fx_pre = build_engine();
    rt.block_on(mint_n_settled(&fx_pre, 100));
    let prep_pre = prep_start.elapsed();
    let base_pre = BenchMetadata::base(&fx_pre, prep_pre);

    // payload_mismatch — payload.accepted != quote.requirements.
    {
        let quote = issue(&fx_pre, AMOUNT, NOW + 5_000_000);
        let mismatch = payload_accepting(&mock_requirements("9999"), "mismatch");
        let (hist, tput, reason) = rt.block_on(repeat_accept(
            &fx_pre.engine,
            &quote,
            &mismatch,
            NOW + 5_000_001,
            n,
        ));
        assert!(matches!(reason, Some(RejectReason::PayloadMismatch)), "got {reason:?}");
        base_pre
            .for_row("reject: payload_mismatch [pre-state]", n, 1, false, &fx_pre)
            .report(&hist, &Throughput::denial(tput));
    }

    // expired — now >= expires + tolerance.
    {
        let quote = issue(&fx_pre, AMOUNT, NOW + 5_100_000);
        let proof = payload_for(&quote);
        let past = NOW + 5_100_000 + TTL_NS + 86_400_000_000_000;
        let (hist, tput, reason) =
            rt.block_on(repeat_accept(&fx_pre.engine, &quote, &proof, past, n));
        assert!(matches!(reason, Some(RejectReason::QuoteExpired)), "got {reason:?}");
        base_pre
            .for_row("reject: expired [pre-state]", n, 1, false, &fx_pre)
            .report(&hist, &Throughput::denial(tput));
    }

    // bad_quote — a quote from a FOREIGN provider: valid self-signature,
    // wrong provider → BadQuote, before any state/facilitator access.
    {
        let foreign = build_engine();
        let fq = issue(&foreign, AMOUNT, NOW + 5_200_000);
        let fproof = payload_for(&fq);
        let (hist, tput, reason) =
            rt.block_on(repeat_accept(&fx_pre.engine, &fq, &fproof, NOW + 5_200_001, n));
        assert!(matches!(reason, Some(RejectReason::BadQuote(_))), "got {reason:?}");
        base_pre
            .for_row("reject: bad_quote [pre-state]", n, 1, false, &fx_pre)
            .report(&hist, &Throughput::denial(tput));
    }

    // ---- state-touching rows (each its own engine; restore-per-sample) ----

    // verify_rejected — facilitator verify returns invalid (WrongAmount
    // mode). accept claims state, verify fails, the claim is released. A
    // WrongAmount engine can't settle, so seed the 100-record bulk with a
    // Success engine and restore that snapshot into the WrongAmount store
    // (the engine never re-validates existing records; they are size bulk).
    {
        let prep_start = Instant::now();
        let seed_fx = build_engine();
        rt.block_on(mint_n_settled(&seed_fx, 100));
        let seed_snapshot = snapshot_state(seed_fx.state_path());
        let fx = build_engine_with_mode(MockMode::WrongAmount);
        restore_state(fx.state_path(), &seed_snapshot);
        let prep = prep_start.elapsed();
        let quote = issue(&fx, AMOUNT, NOW + 5_300_000);
        let proof = payload_for(&quote);
        let baseline = snapshot_state(fx.state_path());
        let base = BenchMetadata::base(&fx, prep);
        let (hist, tput, reason) = rt.block_on(restore_accept(
            &fx.engine,
            fx.state_path(),
            &baseline,
            &quote,
            &proof,
            NOW + 5_300_001,
            n,
        ));
        assert!(matches!(reason, Some(RejectReason::VerifyRejected(_))), "got {reason:?}");
        base.for_row("reject: verify_rejected [claims+releases]", n, 1, false, &fx)
            .report(&hist, &Throughput::denial(tput));
    }

    // already_served — re-submit a settled quote+payload → Served via the
    // AlreadyServed short-circuit (reads durable state).
    {
        let prep_start = Instant::now();
        let fx = build_engine();
        rt.block_on(mint_n_settled(&fx, 100));
        let quote = issue(&fx, AMOUNT, NOW + 5_400_000);
        let proof = payload_for(&quote);
        let first = rt.block_on(fx.engine.accept_payment(&quote, &proof, OBS, NOW + 5_400_001));
        assert!(matches!(first, Ok(PaymentDecision::Served { .. })));
        let prep = prep_start.elapsed();
        let baseline = snapshot_state(fx.state_path());
        let base = BenchMetadata::base(&fx, prep);
        let (hist, admits, wall, served) = rt.block_on(async {
            let mut hist = new_hist();
            let mut admits = 0usize;
            let start = Instant::now();
            let mut served = false;
            for _ in 0..n {
                restore_state(fx.state_path(), &baseline);
                let t = Instant::now();
                let d = fx
                    .engine
                    .accept_payment(&quote, &proof, OBS, NOW + 5_400_002)
                    .await
                    .expect("accept");
                hist.record(t.elapsed().as_nanos() as u64).expect("record");
                served = matches!(d, PaymentDecision::Served { .. });
                if served {
                    admits += 1;
                }
            }
            (hist, admits, start.elapsed().as_secs_f64(), served)
        });
        assert!(served, "already_served must return Served");
        // AlreadyServed is a Served outcome, not a new unique payment: it
        // admits (returns the prior billing) but settles nothing new.
        let mut t = Throughput::uniform(admits as f64 / wall);
        t.unique_payments_per_s = 0.0;
        base.for_row("accept: already_served [reads state]", n, 1, false, &fx)
            .report(&hist, &t);
    }

    // replay — the same payload bytes under a DIFFERENT quote (shared nonce)
    // → Rejected{Replay} (consults replay state).
    {
        let prep_start = Instant::now();
        let fx = build_engine();
        rt.block_on(mint_n_settled(&fx, 100));
        let qa = issue(&fx, AMOUNT, NOW + 6_000_000);
        let qb = issue(&fx, AMOUNT, NOW + 6_000_001);
        let shared = payload_with_nonce(&qa, "replay-nonce"); // accepted == requirements (shared content)
        let first = rt.block_on(fx.engine.accept_payment(&qa, &shared, OBS, NOW + 6_000_002));
        assert!(matches!(first, Ok(PaymentDecision::Served { .. })));
        let prep = prep_start.elapsed();
        let baseline = snapshot_state(fx.state_path());
        let base = BenchMetadata::base(&fx, prep);
        let (hist, tput, reason) = rt.block_on(restore_accept(
            &fx.engine,
            fx.state_path(),
            &baseline,
            &qb,
            &shared,
            NOW + 6_000_003,
            n,
        ));
        assert!(matches!(reason, Some(RejectReason::Replay)), "got {reason:?}");
        base.for_row("reject: replay [replay state]", n, 1, false, &fx)
            .report(&hist, &Throughput::denial(tput));
    }

    // quote_already_paid — the same quote with a DIFFERENT payload after it
    // settled → Rejected{QuoteAlreadyPaid} (consults quote state).
    {
        let prep_start = Instant::now();
        let fx = build_engine();
        rt.block_on(mint_n_settled(&fx, 100));
        let quote = issue(&fx, AMOUNT, NOW + 6_100_000);
        let payload1 = payload_with_nonce(&quote, "paid-1");
        let payload2 = payload_with_nonce(&quote, "paid-2");
        let first = rt.block_on(fx.engine.accept_payment(&quote, &payload1, OBS, NOW + 6_100_001));
        assert!(matches!(first, Ok(PaymentDecision::Served { .. })));
        let prep = prep_start.elapsed();
        let baseline = snapshot_state(fx.state_path());
        let base = BenchMetadata::base(&fx, prep);
        let (hist, tput, reason) = rt.block_on(restore_accept(
            &fx.engine,
            fx.state_path(),
            &baseline,
            &quote,
            &payload2,
            NOW + 6_100_002,
            n,
        ));
        assert!(matches!(reason, Some(RejectReason::QuoteAlreadyPaid)), "got {reason:?}");
        base.for_row("reject: quote_already_paid [quote state]", n, 1, false, &fx)
            .report(&hist, &Throughput::denial(tput));
    }

    println!("\n(in_progress is a race — a concurrent active claim — measured in the P3 acceptance storm)");
}

/// Repeatable accept (no state change between samples): time `samples`
/// `accept_payment` calls, returning the histogram, attempts/s, and the last
/// decision's `RejectReason` (if any) for the caller's assertion.
async fn repeat_accept(
    engine: &Arc<PaymentEngine>,
    quote: &net_payments::core::quote::PaymentQuote,
    proof: &X402Carry<PaymentPayload>,
    now_ns: u64,
    samples: usize,
) -> (Histogram<u64>, f64, Option<RejectReason>) {
    let mut hist = new_hist();
    let mut last = None;
    let start = Instant::now();
    for _ in 0..samples {
        let t = Instant::now();
        let d = engine
            .accept_payment(quote, proof, OBS, now_ns)
            .await
            .expect("accept_payment");
        hist.record(t.elapsed().as_nanos() as u64).expect("record");
        last = match d {
            PaymentDecision::Rejected { reason } => Some(reason),
            _ => None,
        };
    }
    (hist, samples as f64 / start.elapsed().as_secs_f64(), last)
}

/// Accept with a restore-per-sample baseline (for state-touching rejections
/// that must start from the same prepared store each time). Returns the
/// histogram, attempts/s, and the last `RejectReason`.
async fn restore_accept(
    engine: &Arc<PaymentEngine>,
    state_path: &std::path::Path,
    baseline: &[u8],
    quote: &net_payments::core::quote::PaymentQuote,
    proof: &X402Carry<PaymentPayload>,
    now_ns: u64,
    samples: usize,
) -> (Histogram<u64>, f64, Option<RejectReason>) {
    let mut hist = new_hist();
    let mut last = None;
    let start = Instant::now();
    for _ in 0..samples {
        restore_state(state_path, baseline);
        let t = Instant::now();
        let d = engine
            .accept_payment(quote, proof, OBS, now_ns)
            .await
            .expect("accept_payment");
        hist.record(t.elapsed().as_nanos() as u64).expect("record");
        last = match d {
            PaymentDecision::Rejected { reason } => Some(reason),
            _ => None,
        };
    }
    (hist, samples as f64 / start.elapsed().as_secs_f64(), last)
}
