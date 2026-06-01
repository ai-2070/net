//! Phase 1 of NRPC_QPS_CONCURRENCY_SCALING_PLAN.md — does spreading
//! load across N *independent* service channels lift the unary QPS
//! ceiling?
//!
//! `nrpc_qps` saturates a single channel: at c16/32B it tops out
//! ~93 K req/s (~4x over c1, not 16x). The suspected wall is a chain
//! of single-consumer server stages — one recv loop + inline decrypt,
//! then one bridge task + one `fold` mutex *per channel*. This bench
//! holds concurrency fixed and varies how many channels that load is
//! spread over:
//!
//! - Throughput climbs with shards → the per-channel bridge/mutex
//!   (stage 4) is the binding constraint → fix = audit T2.1.
//! - Throughput flat across shards → the bottleneck is upstream and
//!   shared by all channels: the single recv loop + inline decrypt
//!   (stages 1-2) → fix = move decrypt off the recv loop.
//!
//! `s1` (one shard) reproduces the single-channel `nrpc_qps` setup and
//! is the in-bench baseline — its bars should track the matching
//! `nrpc_qps cN/…` bars.
//!
//! Axes:
//! - Shards:      1 / 4 / 16 independent channels
//! - Concurrency: 16 / 128 in-flight callers, round-robined over shards
//! - Payload:     32 B / 1 KiB
//!
//! Both `Pair` nodes share one runtime; size it with
//! `NRPC_BENCH_WORKER_THREADS` (Phase 0a) to read the shard curve at
//! different core counts.
//!
//! Run with:
//!   cargo bench --bench nrpc_qps_shard --features net,cortex -p net-mesh-sdk

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures::stream::{FuturesUnordered, StreamExt};

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{call_json_shard_retrying, payload, runtime, EchoReq, Pair};

const SHARDS: &[usize] = &[1, 4, 16];
const CONCURRENCY: &[usize] = &[16, 128];
const PAYLOADS: &[(&str, usize)] = &[("32B", 32), ("1KiB", 1024)];

fn bench_qps_shard(c: &mut Criterion) {
    let rt = runtime();

    let mut group = c.benchmark_group("nrpc_qps_shard");
    // Same modest sample size as nrpc_qps — throughput is outlier-prone
    // and the matrix (3 shards × 2 concurrency × 2 payloads) is already
    // 12 bars per run.
    group.sample_size(20);

    for &shards in SHARDS {
        // One pair per shard count: registers `shards` JSON echo
        // channels, each with its own bridge task + fold mutex. Dropped
        // at the end of the iteration so the next shard count starts
        // from fresh nodes.
        let pair = rt.block_on(Pair::new_sharded(shards));

        for &(label, size) in PAYLOADS {
            let req = EchoReq {
                body: payload(size),
            };

            for &concurrency in CONCURRENCY {
                group.throughput(Throughput::Elements(concurrency as u64));
                let id = BenchmarkId::new(format!("s{shards}/c{concurrency}"), label);
                group.bench_with_input(id, &req, |b, req| {
                    b.to_async(&rt).iter(|| async {
                        let mut futs = FuturesUnordered::new();
                        for i in 0..concurrency {
                            // i % shards inside the helper → even
                            // round-robin across channels.
                            futs.push(call_json_shard_retrying(&pair, req, i));
                        }
                        while let Some(resp) = futs.next().await {
                            std::hint::black_box(resp);
                        }
                    });
                });
            }
        }
    }

    group.finish();
}

criterion_group!(benches, bench_qps_shard);
criterion_main!(benches);
