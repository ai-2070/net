//! Item 4 — tail latency under load. This bench does not use
//! Criterion: it's `harness = false` so we can run a custom
//! hdrhistogram-backed loop that captures every sample, not just
//! Criterion's bootstrap-resampled summary. p99.9 needs ~10⁵
//! samples to be statistically meaningful; Criterion's default
//! sample budget is way smaller than that.
//!
//! Shape:
//! 1. Stand up one peer pair (see `nrpc_common::Pair`).
//! 2. Warm up: 1000 sequential calls, discard timings.
//! 3. For each concurrency C ∈ {16, 64, 256}:
//!    - Keep C unary 32 B calls in flight at all times.
//!    - Issue `TOTAL_SAMPLES` calls, record per-call latency in
//!      an `hdrhistogram::Histogram<u64>` (1 ns precision).
//!    - Report p50 / p95 / p99 / p99.9 / max + mean + std.
//!
//! Concurrency is enforced with a `tokio::sync::Semaphore` rather
//! than a sized `FuturesUnordered` because we want a stable
//! steady-state where new calls launch the moment any one
//! finishes — that's the regime where tail shape actually
//! matters.
//!
//! Run with:
//!   cargo bench --bench nrpc_tail --features net,cortex -p ai2070-net-sdk

use std::sync::Arc;
use std::time::Instant;

use hdrhistogram::Histogram;
use tokio::sync::Semaphore;

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{call_json_direct_retrying, payload, runtime, EchoReq, Pair};

const WARMUP_CALLS: usize = 1_000;
const TOTAL_SAMPLES: usize = 100_000;
const CONCURRENCY: &[usize] = &[16, 64, 256];

fn main() {
    let rt = runtime();
    let pair = Arc::new(rt.block_on(Pair::new()));
    let req = Arc::new(EchoReq { body: payload(32) });

    // Warm up the connection + dispatcher / GC pools / etc. so
    // the first measured sample isn't disproportionately slow.
    rt.block_on(async {
        for _ in 0..WARMUP_CALLS {
            std::hint::black_box(call_json_direct_retrying(&pair, &req).await);
        }
    });

    println!(
        "nrpc_tail — payload=32B, samples_per_concurrency={TOTAL_SAMPLES}, codec=json, routing=direct"
    );
    println!(
        "  {:>5}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        "C", "p50_us", "p95_us", "p99_us", "p99.9_us", "max_us", "mean_us"
    );

    for &concurrency in CONCURRENCY {
        let hist = rt.block_on(run_one(concurrency, Arc::clone(&pair), Arc::clone(&req)));
        let to_us = |v: u64| v as f64 / 1_000.0;
        println!(
            "  {:>5}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}",
            concurrency,
            to_us(hist.value_at_quantile(0.50)),
            to_us(hist.value_at_quantile(0.95)),
            to_us(hist.value_at_quantile(0.99)),
            to_us(hist.value_at_quantile(0.999)),
            to_us(hist.max()),
            hist.mean() / 1_000.0,
        );
    }
}

async fn run_one(concurrency: usize, pair: Arc<Pair>, req: Arc<EchoReq>) -> Histogram<u64> {
    // 1 ns .. 60 s, 3 significant figures — covers every plausible
    // RPC latency this bench will see.
    let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
        .expect("hdrhistogram alloc");
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(TOTAL_SAMPLES);

    for _ in 0..TOTAL_SAMPLES {
        let permit = Arc::clone(&sem).acquire_owned().await.expect("semaphore");
        let pair = Arc::clone(&pair);
        let req = Arc::clone(&req);
        handles.push(tokio::spawn(async move {
            let start = Instant::now();
            let resp = call_json_direct_retrying(&pair, &req).await;
            let elapsed = start.elapsed().as_nanos() as u64;
            drop(permit);
            std::hint::black_box(resp);
            elapsed
        }));
    }

    for h in handles {
        let ns = h.await.expect("join");
        hist.record(ns).expect("record");
    }
    hist
}
