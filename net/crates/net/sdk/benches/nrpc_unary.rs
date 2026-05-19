//! Item 1 — unary RPC round-trip latency. Headline screenshot bench.
//!
//! Axes:
//! - Payload: 0 B (empty) / 32 B / 1 KiB
//! - Codec: json / postcard / raw bytes
//! - Routing: direct (`call_typed` with known node id) /
//!   discovery (`call_service_typed` via capability index)
//!
//! For each (payload, codec, routing) combo Criterion reports
//! mean + std-dev + the percentile tail it samples (p50 / p90 /
//! p99 surface in the HTML report; max bubbles up to the
//! "outliers" section). For deeper tail (p99.9) see
//! `nrpc_tail.rs`, which uses a custom hdrhistogram loop.
//!
//! The discovery path is only benched for the JSON codec — the
//! discovery cost is the capability-index lookup, which is codec-
//! independent. Three codecs × two routing paths × three
//! payloads = 18 bars is too many; we keep the discovery vs
//! direct delta on one axis and the codec delta on the other.
//!
//! Run with:
//!   cargo bench --bench nrpc_unary --features net,cortex -p net-mesh-sdk

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{
    call_json_direct, call_json_discovery, call_postcard_direct, call_raw_direct, payload, runtime,
    EchoReq, Pair,
};

const PAYLOADS: &[(&str, usize)] = &[("empty", 0), ("32B", 32), ("1KiB", 1024)];

fn bench_unary(c: &mut Criterion) {
    let rt = runtime();
    let pair = rt.block_on(Pair::new());

    // ---------- Codec axis (direct routing only) ----------
    let mut group = c.benchmark_group("nrpc_unary_codec");
    group.sample_size(50);
    for &(label, size) in PAYLOADS {
        group.throughput(Throughput::Elements(1));

        // JSON via typed API.
        let req = EchoReq {
            body: payload(size),
        };
        group.bench_with_input(BenchmarkId::new("json", label), &req, |b, req| {
            b.to_async(&rt).iter(|| async {
                let resp = call_json_direct(&pair, req).await;
                std::hint::black_box(resp);
            });
        });

        // Postcard via raw `serve_rpc` + manual encode.
        let req = EchoReq {
            body: payload(size),
        };
        group.bench_with_input(BenchmarkId::new("postcard", label), &req, |b, req| {
            b.to_async(&rt).iter(|| async {
                let resp = call_postcard_direct(&pair, req).await;
                std::hint::black_box(resp);
            });
        });

        // Raw bytes — theoretical floor (no codec on either side).
        let body = Bytes::copy_from_slice(payload(size).as_bytes());
        group.bench_with_input(BenchmarkId::new("raw", label), &body, |b, body| {
            b.to_async(&rt).iter(|| async {
                let resp = call_raw_direct(&pair, body.clone()).await;
                std::hint::black_box(resp);
            });
        });
    }
    group.finish();

    // ---------- Routing axis (JSON only) ----------
    let mut group = c.benchmark_group("nrpc_unary_routing");
    group.sample_size(50);
    for &(label, size) in PAYLOADS {
        group.throughput(Throughput::Elements(1));
        let req = EchoReq {
            body: payload(size),
        };

        group.bench_with_input(BenchmarkId::new("direct", label), &req, |b, req| {
            b.to_async(&rt).iter(|| async {
                let resp = call_json_direct(&pair, req).await;
                std::hint::black_box(resp);
            });
        });

        group.bench_with_input(BenchmarkId::new("discovery", label), &req, |b, req| {
            b.to_async(&rt).iter(|| async {
                let resp = call_json_discovery(&pair, req).await;
                std::hint::black_box(resp);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_unary);
criterion_main!(benches);
