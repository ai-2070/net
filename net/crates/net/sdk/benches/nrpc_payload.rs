//! Item 6 — large-payload efficiency. Unary throughput at the
//! upper end of what a single request body can carry.
//!
//! Hard limit: `MAX_RPC_BODY_LEN = 4 MiB` in
//! `adapter::net::cortex::rpc` (line 227). Bodies that breach it
//! are rejected at the codec, so this bench can't drive 16 MiB
//! unary. Anything that large needs the streaming API (see
//! `nrpc_streaming.rs`) — the spec's 16 MiB axis lives there
//! once the chunked-payload throughput bench lands.
//!
//! Axes:
//! - Payload: 256 KiB / 1 MiB / 3 MiB (safely under 4 MiB even
//!   after JSON envelope expansion)
//! - Codec: json / postcard / raw bytes
//!
//! `Throughput::Bytes(size)` makes Criterion report MB/s — the
//! headline metric for large payloads. Sample count is intentionally
//! small (10) because each iteration moves megabytes; running
//! 100 samples per bar would dominate the bench wall-time.
//!
//! CPU% and alloc counts are NOT collected here. Criterion can't
//! sample them inside its timing loop without skewing results;
//! the dhat / sysinfo wiring lives in a separate v2 profiling
//! binary.
//!
//! Run with:
//!   cargo bench --bench nrpc_payload --features net,cortex -p net-mesh-sdk

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{
    call_json_direct_retrying, call_postcard_direct_retrying, call_raw_direct_retrying, payload,
    runtime, EchoReq, Pair,
};

const SIZES: &[(&str, usize)] = &[
    ("256KiB", 256 * 1024),
    ("1MiB", 1024 * 1024),
    ("3MiB", 3 * 1024 * 1024),
];

fn bench_payload(c: &mut Criterion) {
    let rt = runtime();
    let pair = rt.block_on(Pair::new());

    let mut group = c.benchmark_group("nrpc_payload");
    // Each iter moves megabytes — keep sample_size small so wall
    // time stays sane on a 3-codec × 3-size matrix.
    group.sample_size(10);
    // Half a second per sample is plenty at this scale and dodges
    // Criterion's default 5s warmup which would otherwise dominate.
    group.measurement_time(std::time::Duration::from_secs(5));
    group.warm_up_time(std::time::Duration::from_secs(1));

    for &(label, size) in SIZES {
        group.throughput(Throughput::Bytes(size as u64));

        let req = EchoReq {
            body: payload(size),
        };
        group.bench_with_input(BenchmarkId::new("json", label), &req, |b, req| {
            b.to_async(&rt).iter(|| async {
                let resp = call_json_direct_retrying(&pair, req).await;
                std::hint::black_box(resp);
            });
        });

        let req = EchoReq {
            body: payload(size),
        };
        group.bench_with_input(BenchmarkId::new("postcard", label), &req, |b, req| {
            b.to_async(&rt).iter(|| async {
                let resp = call_postcard_direct_retrying(&pair, req).await;
                std::hint::black_box(resp);
            });
        });

        let body = Bytes::copy_from_slice(payload(size).as_bytes());
        group.bench_with_input(BenchmarkId::new("raw", label), &body, |b, body| {
            b.to_async(&rt).iter(|| async {
                let resp = call_raw_direct_retrying(&pair, body.clone()).await;
                std::hint::black_box(resp);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_payload);
criterion_main!(benches);
