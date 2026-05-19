//! Phase F — client-streaming throughput. Caller pushes N
//! typed requests to a server that collects them all and returns
//! one terminal Resp; bench measures total msgs/sec across the
//! upload.
//!
//! Axes:
//! - Payload per item: 64 B / 1 KiB
//! - Items per call:   16 / 256
//! - Codec:            json (the only typed codec on this surface)
//!
//! `Throughput::Elements(N)` makes Criterion report msgs/sec.
//! Each iter constructs a NEW call (new call_id, new initial
//! REQUEST) so we measure the full upload cost, not just the
//! per-chunk steady-state cost. This is the relevant number for
//! short-lived upload sessions; long-running sessions amortize
//! the call-setup overhead and are better captured by a separate
//! bench (deferred).
//!
//! Run with:
//!   cargo bench --bench nrpc_client_streaming --features net,cortex \
//!     -p ai2070-net-sdk

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use net_sdk::mesh_rpc::CallOptionsTyped;

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{payload, runtime, EchoReq, EchoResp, Pair, SVC_JSON_CLIENT_STREAM};

const PAYLOADS: &[(&str, usize)] = &[("64B", 64), ("1KiB", 1024)];
const COUNTS: &[u32] = &[16, 256];

fn bench_client_streaming(c: &mut Criterion) {
    let rt = runtime();
    let pair = rt.block_on(Pair::new());

    let mut group = c.benchmark_group("nrpc_client_stream");
    group.sample_size(20);

    for &(label, size) in PAYLOADS {
        for &count in COUNTS {
            // Per-call throughput in *messages* — headline
            // metric for client-streaming throughput.
            group.throughput(Throughput::Elements(count as u64));
            let req = EchoReq {
                body: payload(size),
            };
            let id = BenchmarkId::new(format!("n{count}"), label);
            group.bench_with_input(id, &req, |b, req| {
                b.to_async(&rt).iter(|| async {
                    let mut call = pair
                        .caller
                        .call_client_stream_typed::<EchoReq, EchoResp>(
                            pair.server_node_id,
                            SVC_JSON_CLIENT_STREAM,
                            CallOptionsTyped::default(),
                        )
                        .await
                        .expect("open client stream");
                    for _ in 0..count {
                        call.send(req).await.expect("typed send");
                    }
                    let resp = call.finish().await.expect("typed finish");
                    std::hint::black_box(resp);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_client_streaming);
criterion_main!(benches);
