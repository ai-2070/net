//! Item 3 — server-streaming throughput. The caller issues one
//! `call_streaming_typed` request with `count = N`; the server
//! emits `N` `EchoResp` items down the stream; the caller awaits
//! all of them.
//!
//! Why server-streaming only: nRPC exposes server streams natively
//! via `serve_rpc_streaming_typed`. Client-streaming (caller
//! emits many, server folds) and duplex (both directions
//! interleave) are channel-layer features in this SDK, not RPC
//! features — mixing them in a bench called "nRPC streaming"
//! would mislead. They get their own bench file when the
//! channel-layer bench suite lands.
//!
//! Axes:
//! - Payload per message: 64 B / 1 KiB
//! - Messages per call:   16 / 256
//! - Codec:               json (the only typed-streaming codec)
//!
//! `Throughput::Elements(count)` makes Criterion report
//! msgs/second per call. The bench loop's iter count multiplied
//! by `count` is the sample size.
//!
//! Run with:
//!   cargo bench --bench nrpc_streaming --features net,cortex -p ai2070-net-sdk

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures::StreamExt;
use net_sdk::mesh_rpc::CallOptionsTyped;

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{payload, runtime, EchoResp, Pair, StreamReq, SVC_JSON_STREAM};

const PAYLOADS: &[(&str, usize)] = &[("64B", 64), ("1KiB", 1024)];
const COUNTS: &[u32] = &[16, 256];

fn bench_streaming(c: &mut Criterion) {
    let rt = runtime();
    let pair = rt.block_on(Pair::new());

    let mut group = c.benchmark_group("nrpc_stream_server");
    group.sample_size(20);

    for &(label, size) in PAYLOADS {
        for &count in COUNTS {
            // Per-call throughput in *messages* — the headline
            // metric for server-streaming.
            group.throughput(Throughput::Elements(count as u64));
            let req = StreamReq {
                body: payload(size),
                count,
            };
            let id = BenchmarkId::new(format!("n{count}"), label);
            group.bench_with_input(id, &req, |b, req| {
                b.to_async(&rt).iter(|| async {
                    let mut stream = pair
                        .caller
                        .call_streaming_typed::<StreamReq, EchoResp>(
                            pair.server_node_id,
                            SVC_JSON_STREAM,
                            req,
                            CallOptionsTyped::default(),
                        )
                        .await
                        .expect("open stream");
                    let mut received = 0u32;
                    while let Some(item) = stream.next().await {
                        std::hint::black_box(item.expect("stream item"));
                        received += 1;
                    }
                    assert_eq!(received, count);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_streaming);
criterion_main!(benches);
