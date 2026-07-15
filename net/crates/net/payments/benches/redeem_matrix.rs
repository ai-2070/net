//! Redemption-gate matrix — the before/after baseline for the read-only-
//! denial write-amplification finding.
//!
//! The `admission` diagnostics exposed it: `redeem_unknown_quote` cost ~5 ms
//! (host-dependent) against ~44 µs for a pure-crypto rejection, because
//! `redeem_for_invocation` ran `mutate_json`, which serialized + fsync'd +
//! renamed the whole state file even for read-only `Denied{..}` outcomes.
//! Fixed by `mutate_json_if_changed` (denials no longer write); see
//! `docs/performance/payments-redeem-write-amplification.md`.
//!
//! Custom hdrhistogram harness (successful admission is stateful and
//! single-use; store cardinality is a controlled axis), reporting through
//! the shared `BenchMetadata::report` so every row carries p50/p95/p99, the
//! three throughputs, and full environment metadata.
//!
//! Rows (per cardinality × concurrency):
//! - `unknown` — earliest-exit denial (id not in the store)
//! - `wrong_tool` — settled quote redeemed for the wrong tool
//! - `invalid_binding` — settled quote with a bad ed25519 binding sig
//! - `already_redeemed` — a quote consumed once, redeemed again
//! - `valid_admitted` — a fresh settled quote (single-use; the honest durable
//!   write that must stay ~unchanged)
//!
//! Store cardinality 1 / 100 / 1 000; concurrency 1 / 16 / 128. Denial rows
//! are repeatable (no state consumed) so they take `NET_PAY_BENCH_SAMPLES`
//! (default 200) each; `valid_admitted` is single-use, so its sample count
//! equals the number of fresh seeded quotes, run at a representative
//! concurrency of 16.
//!
//! Run: cargo bench -p net-payments --bench redeem_matrix

use std::sync::Arc;
use std::time::Instant;

use hdrhistogram::Histogram;
use net_payments::engine::{PaymentEngine, RedeemDecision};
use tokio::sync::Semaphore;

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{
    build_engine, mint_n_settled, new_hist, runtime, BenchMetadata, Throughput, TOOL_ID,
};

const CARDINALITIES: &[u64] = &[1, 100, 1000];
const CONCURRENCY: &[usize] = &[1, 16, 128];
const VALID_CONCURRENCY: usize = 16;

fn denial_samples() -> usize {
    std::env::var("NET_PAY_BENCH_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(200)
}

/// Redeem `targets.len()` times at concurrency `conc`, recording per-call
/// latency. Returns the histogram, attempts/s, and the number of `Admitted`
/// outcomes (0 for a denial row; == n for `valid_admitted`).
async fn run_cell(
    engine: Arc<PaymentEngine>,
    tool: String,
    targets: Arc<Vec<String>>,
    binding: Option<Vec<u8>>,
    conc: usize,
) -> (Histogram<u64>, f64, usize) {
    let n = targets.len();
    let sem = Arc::new(Semaphore::new(conc));
    let mut handles = Vec::with_capacity(n);
    let start = Instant::now();
    for i in 0..n {
        let permit = Arc::clone(&sem).acquire_owned().await.expect("semaphore");
        let engine = Arc::clone(&engine);
        let tool = tool.clone();
        let targets = Arc::clone(&targets);
        let binding = binding.clone();
        handles.push(tokio::spawn(async move {
            let qid = &targets[i];
            let t = Instant::now();
            let decision = engine
                .redeem_for_invocation(&tool, qid, binding.as_deref())
                .await
                .expect("redeem_for_invocation");
            let elapsed = t.elapsed().as_nanos() as u64;
            drop(permit);
            (elapsed, matches!(decision, RedeemDecision::Admitted))
        }));
    }
    let mut hist = new_hist();
    let mut admits = 0usize;
    for h in handles {
        let (ns, admitted) = h.await.expect("join");
        hist.record(ns).expect("record");
        if admitted {
            admits += 1;
        }
    }
    let wall = start.elapsed().as_secs_f64();
    (hist, n as f64 / wall, admits)
}

fn main() {
    let rt = runtime();
    let denial_n = denial_samples();

    println!("redeem_matrix — denial_samples={denial_n}, valid_concurrency={VALID_CONCURRENCY}, tool={TOOL_ID}");

    for &card in CARDINALITIES {
        // Fresh engine seeded to a FIXED cardinality of `card` settled quotes
        // (timed, outside the measured region → fixture_prep).
        let seed_start = Instant::now();
        let (fx, quotes) = rt.block_on(async {
            let fx = build_engine();
            let quotes = mint_n_settled(&fx, card).await;
            (fx, quotes)
        });
        let fixture_prep = seed_start.elapsed();
        let engine = Arc::clone(&fx.engine);
        let base = BenchMetadata::base(&fx, fixture_prep);

        println!("\n== cardinality {card} ==");

        // Consume quotes[0] once, OUTSIDE timing, as the already-redeemed
        // victim. Denials never consume state, so quotes[1..] stay fresh.
        let victim = quotes[0].quote_id.clone();
        let consumed = rt.block_on(engine.redeem_for_invocation(TOOL_ID, &victim, None));
        assert!(
            matches!(consumed, Ok(RedeemDecision::Admitted)),
            "victim quote must admit once before the already-redeemed row"
        );

        for &conc in CONCURRENCY {
            // unknown — id absent from the store (earliest-exit denial).
            let targets = Arc::new(vec!["no-such-quote".to_string(); denial_n]);
            let (h, tput, _) = rt.block_on(run_cell(
                engine.clone(),
                TOOL_ID.into(),
                targets,
                None,
                conc,
            ));
            base.for_row(format!("unknown c{conc}"), denial_n, conc, false, &fx)
                .report(&h, &Throughput::denial(tput));

            // wrong_tool — settled quote redeemed for a different tool.
            let targets = Arc::new(
                (0..denial_n)
                    .map(|i| quotes[i % quotes.len()].quote_id.clone())
                    .collect::<Vec<_>>(),
            );
            let (h, tput, _) = rt.block_on(run_cell(
                engine.clone(),
                "other-tool".into(),
                targets,
                None,
                conc,
            ));
            base.for_row(format!("wrong_tool c{conc}"), denial_n, conc, false, &fx)
                .report(&h, &Throughput::denial(tput));

            // invalid_binding — settled quote, 64-byte sig that won't verify.
            let targets = Arc::new(
                (0..denial_n)
                    .map(|i| quotes[i % quotes.len()].quote_id.clone())
                    .collect::<Vec<_>>(),
            );
            let (h, tput, _) = rt.block_on(run_cell(
                engine.clone(),
                TOOL_ID.into(),
                targets,
                Some(vec![0u8; 64]),
                conc,
            ));
            base.for_row(
                format!("invalid_binding c{conc}"),
                denial_n,
                conc,
                true,
                &fx,
            )
            .report(&h, &Throughput::denial(tput));

            // already_redeemed — the consumed victim, redeemed again.
            let targets = Arc::new(vec![victim.clone(); denial_n]);
            let (h, tput, _) = rt.block_on(run_cell(
                engine.clone(),
                TOOL_ID.into(),
                targets,
                None,
                conc,
            ));
            base.for_row(
                format!("already_redeemed c{conc}"),
                denial_n,
                conc,
                false,
                &fx,
            )
            .report(&h, &Throughput::denial(tput));
        }

        // valid_admitted — the fresh quotes[1..], each redeemed exactly once
        // (single-use). The honest durable write the fix must leave alone.
        // Records stay at `card` (redeem flips a flag, adds no record).
        let fresh: Vec<String> = quotes[1..].iter().map(|q| q.quote_id.clone()).collect();
        if fresh.is_empty() {
            println!("  valid_admitted: skipped (no fresh quotes at cardinality {card})");
        } else {
            let n = fresh.len();
            let (h, tput, admits) = rt.block_on(run_cell(
                engine.clone(),
                TOOL_ID.into(),
                Arc::new(fresh),
                None,
                VALID_CONCURRENCY,
            ));
            assert_eq!(
                admits, n,
                "valid_admitted must admit every fresh quote once"
            );
            base.for_row(
                format!("valid_admitted c{VALID_CONCURRENCY} (n={n})"),
                n,
                VALID_CONCURRENCY,
                false,
                &fx,
            )
            .report(&h, &Throughput::uniform(tput));
        }
    }
}
