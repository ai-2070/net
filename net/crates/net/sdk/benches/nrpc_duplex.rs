//! Phase F — duplex throughput. Caller streams N typed
//! requests, server emits one Resp per Req; bench measures the
//! ROUND-TRIP msgs/sec across the bidirectional flow.
//!
//! Axes:
//! - Payload per item: 64 B / 1 KiB
//! - Items per call:   16 / 256
//! - Codec:            json
//!
//! `Throughput::Elements(N)` reports msgs/sec where one "msg"
//! is a complete Req->Resp round-trip on the same wire-level
//! call_id. Each iter constructs a new call (same rationale as
//! `nrpc_client_streaming.rs`).
//!
//! Run with:
//!   cargo bench --bench nrpc_duplex --features net,cortex -p net-mesh-sdk

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures::StreamExt;
use net_sdk::mesh_rpc::{CallOptionsTyped, RpcError};

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{payload, runtime, EchoReq, EchoResp, Pair, SVC_JSON_DUPLEX};

const PAYLOADS: &[(&str, usize)] = &[("64B", 64), ("1KiB", 1024)];
const COUNTS: &[u32] = &[16, 256];

fn bench_duplex(c: &mut Criterion) {
    let rt = runtime();
    let pair = rt.block_on(Pair::new());

    let mut group = c.benchmark_group("nrpc_duplex");
    group.sample_size(20);

    for &(label, size) in PAYLOADS {
        for &count in COUNTS {
            // Per-call round-trip throughput. Each "element" is
            // one Req->Resp pair.
            group.throughput(Throughput::Elements(count as u64));
            let req = EchoReq {
                body: payload(size),
            };
            let id = BenchmarkId::new(format!("n{count}"), label);
            group.bench_with_input(id, &req, |b, req| {
                b.to_async(&rt).iter(|| async {
                    let call = pair
                        .caller
                        .call_duplex_typed::<EchoReq, EchoResp>(
                            pair.server_node_id,
                            SVC_JSON_DUPLEX,
                            CallOptionsTyped::default(),
                        )
                        .await
                        .expect("open duplex");
                    // Use into_split so send + recv overlap across
                    // tokio tasks — the realistic shape for any
                    // duplex application doing real work.
                    let (mut sink, mut stream) = call.into_split();
                    let req_owned = req.clone();
                    let sender = tokio::spawn(async move {
                        for _ in 0..count {
                            // Streaming `count` frames back-to-back on
                            // one call fills the per-stream publish
                            // window faster than the receiver drains
                            // it. mesh_rpc surfaces that as a retriable
                            // `RpcError::Transport(_)` (see
                            // `call_json_direct_retrying`); yield and
                            // resend the same frame rather than
                            // panicking. The frame isn't published on a
                            // backpressure error, so the resend is not
                            // a duplicate. The concurrent receiver task
                            // drains the window, so the retry makes
                            // progress — this is the realistic duplex
                            // flow-control shape, not masking a fault.
                            loop {
                                match sink.send(&req_owned).await {
                                    Ok(()) => break,
                                    Err(RpcError::Transport(_)) => {
                                        tokio::task::yield_now().await;
                                    }
                                    Err(e) => panic!("send: {e}"),
                                }
                            }
                        }
                        sink.finish_sending().await.expect("finish_sending");
                    });
                    let mut received = 0u32;
                    while let Some(resp) = stream.next().await {
                        std::hint::black_box(resp.expect("decode"));
                        received += 1;
                    }
                    sender.await.expect("sender task");
                    // assert_eq, not debug_assert_eq: benches run
                    // in release mode where debug_assert is
                    // stripped. A silent send/recv mismatch would
                    // poison the throughput numbers (cubic P2).
                    assert_eq!(received, count);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_duplex);
criterion_main!(benches);
