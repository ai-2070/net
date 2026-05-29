//! Item 2 — unary throughput (QPS) at varying concurrency and
//! payload size. The bench loop fans out N concurrent
//! `call_typed` futures via `FuturesUnordered`, awaits all of
//! them, and lets Criterion's `Throughput::Elements(N)` report
//! requests/second.
//!
//! Axes:
//! - Concurrency: 1 / 16 / 128 in-flight callers
//! - Payload: 32 B / 1 KiB / 16 KiB
//! - Codec: json + postcard. JSON is the ergonomic default;
//!   postcard is the compact-binary path and the closest in-tree
//!   analog to a protobuf-style wire format, so running the two
//!   side by side gives a codec-matched read on transport
//!   throughput (and a fairer point of comparison against a
//!   protobuf-based RPC stack). `raw` (no codec) stays in
//!   `nrpc_unary.rs`: adding it here would push the matrix to 27
//!   bars; json+postcard keeps it at 18 (2 × 3 × 3).
//!
//! Why direct routing (`call_typed`): the discovery axis lives in
//! `nrpc_unary.rs`; throughput here is meant to measure the
//! transport hot path under saturation, not the capability index.
//!
//! Run with:
//!   cargo bench --bench nrpc_qps --features net,cortex -p net-mesh-sdk

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures::stream::{FuturesUnordered, StreamExt};

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{
    call_json_direct_retrying, call_postcard_direct_retrying, payload, runtime, EchoReq, Pair,
};

const CONCURRENCY: &[usize] = &[1, 16, 128];
const PAYLOADS: &[(&str, usize)] = &[("32B", 32), ("1KiB", 1024), ("16KiB", 16 * 1024)];

fn bench_qps(c: &mut Criterion) {
    let rt = runtime();
    let pair = rt.block_on(Pair::new());

    let mut group = c.benchmark_group("nrpc_qps");
    // Throughput is sensitive to outliers from GC pauses / OS
    // scheduling; keep sample_size modest so individual runs don't
    // sprawl into minutes per concurrency level.
    group.sample_size(20);

    for &(label, size) in PAYLOADS {
        let req = EchoReq {
            body: payload(size),
        };

        for &concurrency in CONCURRENCY {
            group.throughput(Throughput::Elements(concurrency as u64));

            // JSON codec.
            let id = BenchmarkId::new(format!("json/c{concurrency}"), label);
            group.bench_with_input(id, &req, |b, req| {
                b.to_async(&rt).iter(|| async {
                    let mut futs = FuturesUnordered::new();
                    for _ in 0..concurrency {
                        futs.push(call_json_direct_retrying(&pair, req));
                    }
                    while let Some(resp) = futs.next().await {
                        std::hint::black_box(resp);
                    }
                });
            });

            // Postcard codec — compact binary, the codec-matched
            // counterpart to the JSON bars above.
            let id = BenchmarkId::new(format!("postcard/c{concurrency}"), label);
            group.bench_with_input(id, &req, |b, req| {
                b.to_async(&rt).iter(|| async {
                    let mut futs = FuturesUnordered::new();
                    for _ in 0..concurrency {
                        futs.push(call_postcard_direct_retrying(&pair, req));
                    }
                    while let Some(resp) = futs.next().await {
                        std::hint::black_box(resp);
                    }
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_qps);
criterion_main!(benches);
