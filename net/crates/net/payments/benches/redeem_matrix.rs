//! Redemption-gate matrix — the before/after baseline for the read-only-
//! denial write-amplification finding.
//!
//! The `admission` smoke bar exposed it: `redeem_unknown_quote` costs ~5 ms
//! against ~44 µs for a pure-crypto rejection, because `redeem_for_invocation`
//! runs `mutate_json` (`policy/store.rs`), which **always** serializes +
//! fsyncs + renames the whole state file — even when the closure only reads
//! (every `Denied{..}` branch; `engine/mod.rs:1500-1567`). Only the
//! `Admitted` branch (`mod.rs:1568`) actually mutates. So a caller who can
//! guess/spray quote ids forces a global-lock + whole-file-fsync per attempt:
//! write amplification and a denial-of-service surface.
//!
//! This bench captures the *before* state (run it, preserve the numbers),
//! then the same rows are rerun after the `mutate_json_if_changed` fix. It
//! is a custom hdrhistogram harness because successful admission is stateful
//! and single-use, and store cardinality is a controlled axis, not a side
//! effect of sample count (decision D3 + the fixture protocol in the plan).
//!
//! Rows (per cardinality × concurrency):
//!   - `unknown`          — earliest-exit denial (id not in the store)
//!   - `wrong_tool`       — settled quote redeemed for the wrong tool
//!   - `invalid_binding`  — settled quote with a bad ed25519 binding sig
//!   - `already_redeemed` — a quote consumed once, redeemed again
//!   - `valid_admitted`   — a fresh settled quote (single-use; the honest
//!                          durable write that must stay ~unchanged)
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

use bench_common::{build_engine, mint_n_settled, new_hist, print_header, print_row, runtime, TOOL_ID};

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
/// latency. Returns the histogram, throughput (attempts/s), and the number
/// of `Admitted` outcomes (0 for a denial row; == n for `valid_admitted`).
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
    println!("throughput is attempts/s (denials admit 0; valid_admitted admits every fresh quote once)");

    for &card in CARDINALITIES {
        // Fresh engine seeded to a FIXED cardinality of `card` settled
        // quotes (the store history that every row below measures against).
        let (fx, quotes) = rt.block_on(async {
            let fx = build_engine();
            let quotes = mint_n_settled(&fx, card).await;
            (fx, quotes)
        });
        let engine = Arc::clone(&fx.engine);
        let bytes_seeded = fx.state_bytes();

        println!(
            "\n== cardinality {card} · state_bytes(seeded)={bytes_seeded} · placement={} ==",
            fx.placement_label()
        );
        print_header("");

        // Consume quotes[0] once, OUTSIDE timing, as the already-redeemed
        // victim. Denials never consume state, so quotes[1..] stay fresh for
        // valid_admitted.
        let victim = quotes[0].quote_id.clone();
        let consumed = rt.block_on(engine.redeem_for_invocation(TOOL_ID, &victim, None));
        assert!(
            matches!(consumed, Ok(RedeemDecision::Admitted)),
            "victim quote must admit once before the already-redeemed row"
        );

        for &conc in CONCURRENCY {
            // unknown — id absent from the store (earliest-exit denial).
            let targets = Arc::new(vec!["no-such-quote".to_string(); denial_n]);
            let (h, tput, _) =
                rt.block_on(run_cell(engine.clone(), TOOL_ID.into(), targets, None, conc));
            print_row(&format!("unknown c{conc}"), &h, tput);

            // wrong_tool — settled quote redeemed for a different tool.
            let targets = Arc::new(
                (0..denial_n)
                    .map(|i| quotes[i % quotes.len()].quote_id.clone())
                    .collect::<Vec<_>>(),
            );
            let (h, tput, _) =
                rt.block_on(run_cell(engine.clone(), "other-tool".into(), targets, None, conc));
            print_row(&format!("wrong_tool c{conc}"), &h, tput);

            // invalid_binding — settled quote, malformed-but-64-byte sig.
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
            print_row(&format!("invalid_binding c{conc}"), &h, tput);

            // already_redeemed — the consumed victim, redeemed again.
            let targets = Arc::new(vec![victim.clone(); denial_n]);
            let (h, tput, _) =
                rt.block_on(run_cell(engine.clone(), TOOL_ID.into(), targets, None, conc));
            print_row(&format!("already_redeemed c{conc}"), &h, tput);
        }

        // valid_admitted — the fresh quotes[1..], each redeemed exactly once
        // (single-use), at a representative concurrency. This is the honest
        // durable write that the fix must leave ~unchanged.
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
            assert_eq!(admits, n, "valid_admitted must admit every fresh quote once");
            print_row(&format!("valid_admitted c{VALID_CONCURRENCY} (n={n})"), &h, tput);
        }

        println!("  state_bytes(final)={}", fx.state_bytes());
    }
}
