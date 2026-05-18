//! End-to-end SDK tests for the typed nRPC **bidirectional** surface
//! (Phase E). Two `Mesh` instances via `MeshBuilder`, connected via
//! direct handshake. Server registers typed handlers via
//! `serve_rpc_client_stream_typed` / `serve_rpc_duplex_typed`; caller
//! invokes via `call_client_stream_typed` / `call_duplex_typed`.
//!
//! Coverage:
//! 1. Client-streaming typed round-trip (N=10).
//! 2. Client-streaming typed handler `Err(String)` → ServerError.
//! 3. Client-streaming typed caller-side decode failure surfaces
//!    `RpcError::Codec` (server emits an incompatible Resp shape
//!    via the raw API).
//! 4. Duplex typed interleaved send-and-recv.
//! 5. Duplex typed `into_split` lets halves run in separate tasks.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{
    CallOptions, CallOptionsTyped, Codec, RequestStream, RpcClientStreamingHandler, RpcError,
    RpcHandlerError, RpcResponsePayload, RpcStatus, RpcStreamingContext,
};
use serde::{Deserialize, Serialize};

async fn two_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: std::net::SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct Item {
    n: u32,
    label: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct Summary {
    count: u32,
}

/// 1/5 — Typed client-streaming round-trip with N=10 items.
/// Handler drains the typed RequestStream into a count, returns
/// a typed Summary. Caller's finish() decodes the typed terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_stream_typed_round_trips_n_items() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_client_stream_typed(
            "aggregate",
            Codec::Json,
            |mut requests| async move {
                let mut count = 0u32;
                let mut last_label = String::new();
                while let Some(item) = requests.next().await {
                    let item: Item = item.map_err(|e| format!("decode: {e}"))?;
                    count += 1;
                    last_label = item.label;
                }
                Ok::<_, String>(Summary { count })
                    .map(|s| {
                        std::hint::black_box(last_label);
                        s
                    })
            },
        )
        .expect("serve_rpc_client_stream_typed");

    let mut call = caller
        .call_client_stream_typed::<Item, Summary>(
            server.inner().node_id(),
            "aggregate",
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_client_stream_typed");
    for i in 0..10u32 {
        call.send(&Item {
            n: i,
            label: format!("item-{i}"),
        })
        .await
        .expect("typed send");
    }
    let summary = call.finish().await.expect("typed finish");
    assert_eq!(summary, Summary { count: 10 });
}

/// 2/5 — Typed handler returns `Err(String)`. The caller's
/// `finish` surfaces it as `RpcError::ServerError` with the
/// `NRPC_TYPED_HANDLER_ERROR` application code and the user's
/// message in the body.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_stream_typed_handler_err_round_trips() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_client_stream_typed(
            "rejecter",
            Codec::Json,
            |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<Item>| async move {
                while let Some(_) = requests.next().await {}
                Err::<Summary, _>("application-level reject".to_string())
            },
        )
        .expect("serve_rpc_client_stream_typed");

    let mut call = caller
        .call_client_stream_typed::<Item, Summary>(
            server.inner().node_id(),
            "rejecter",
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_client_stream_typed");
    call.send(&Item {
        n: 1,
        label: "x".into(),
    })
    .await
    .expect("send");
    let err = call.finish().await.expect_err("must error");
    match err {
        RpcError::ServerError { status, message } => {
            assert_eq!(status, net_sdk::mesh_rpc::NRPC_TYPED_HANDLER_ERROR);
            assert!(message.contains("application-level reject"));
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}

/// 3/5 — Server uses the RAW client-stream API to emit a
/// terminal RESPONSE whose body is NOT valid `Summary` JSON; the
/// typed caller's `finish()` decode fails and surfaces a
/// `RpcError::Codec`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_stream_typed_caller_decode_failure_surfaces_codec_error() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    // Raw handler: emits a terminal RESPONSE whose body is the
    // bare string "not-json-shape" — not a Summary.
    struct WrongShapeHandler;
    #[async_trait::async_trait]
    impl RpcClientStreamingHandler for WrongShapeHandler {
        async fn call(
            &self,
            _ctx: RpcStreamingContext,
            mut requests: RequestStream,
        ) -> Result<RpcResponsePayload, RpcHandlerError> {
            while requests.next().await.is_some() {}
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: b"not-json-shape".to_vec(),
            })
        }
    }
    let _serve = server
        .serve_rpc_client_stream("wrong_shape", Arc::new(WrongShapeHandler))
        .expect("serve_rpc_client_stream");

    let mut call = caller
        .call_client_stream_typed::<Item, Summary>(
            server.inner().node_id(),
            "wrong_shape",
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_client_stream_typed");
    call.send(&Item {
        n: 0,
        label: "x".into(),
    })
    .await
    .expect("send");
    let err = call.finish().await.expect_err("must error");
    match err {
        RpcError::Codec { direction, message } => {
            assert!(
                matches!(direction, net_sdk::mesh_rpc::CodecDirection::Decode),
                "expected Decode direction, got {direction:?}"
            );
            assert!(message.contains("client stream typed decode"));
        }
        other => panic!("expected Codec(Decode), got {other:?}"),
    }
}

/// 4/5 — Typed duplex echo. Handler emits one typed Resp per
/// inbound typed Req plus a final Summary; caller streams 5
/// requests and collects 6 responses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplex_typed_interleaves_send_and_recv() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_duplex_typed(
            "echo_typed",
            Codec::Json,
            |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<Item>, sink| async move {
                let mut count = 0u32;
                while let Some(item) = requests.next().await {
                    let item: Item = item.map_err(|e| format!("decode: {e}"))?;
                    // Echo with a "echo-" prefix on the label.
                    sink.send(&Item {
                        n: item.n,
                        label: format!("echo-{}", item.label),
                    })?;
                    count += 1;
                }
                // Final summary.
                sink.send(&Item {
                    n: count,
                    label: "summary".into(),
                })?;
                Ok::<_, String>(())
            },
        )
        .expect("serve_rpc_duplex_typed");

    let mut call = caller
        .call_duplex_typed::<Item, Item>(
            server.inner().node_id(),
            "echo_typed",
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_duplex_typed");
    for i in 0..5u32 {
        call.send(&Item {
            n: i,
            label: format!("{i}"),
        })
        .await
        .expect("typed send");
    }
    call.finish_sending().await.expect("finish_sending");

    let mut collected: Vec<Item> = Vec::new();
    while let Some(item) = call.next().await {
        collected.push(item.expect("typed item must decode"));
    }
    assert_eq!(collected.len(), 6);
    for i in 0..5u32 {
        assert_eq!(collected[i as usize].label, format!("echo-{i}"));
    }
    assert_eq!(collected[5].label, "summary");
    assert_eq!(collected[5].n, 5);
}

/// 5/5 — Typed duplex into_split. Sink half in one task, stream
/// half in another. Pins that the Arc<DuplexInner> CANCEL-on-
/// both-drop semantics carry through the typed wrappers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplex_typed_into_split_lets_halves_run_independently() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_duplex_typed(
            "echo_split_typed",
            Codec::Json,
            |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<Item>, sink| async move {
                while let Some(item) = requests.next().await {
                    let item: Item = item.map_err(|e| format!("decode: {e}"))?;
                    sink.send(&Item {
                        n: item.n + 100,
                        label: item.label,
                    })?;
                }
                Ok::<_, String>(())
            },
        )
        .expect("serve_rpc_duplex_typed");

    let call = caller
        .call_duplex_typed::<Item, Item>(
            server.inner().node_id(),
            "echo_split_typed",
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_duplex_typed");
    let (mut sink, mut stream) = call.into_split();

    let sender = tokio::spawn(async move {
        for i in 0..5u32 {
            sink.send(&Item {
                n: i,
                label: format!("s{i}"),
            })
            .await
            .expect("send");
        }
        sink.finish_sending().await.expect("finish_sending");
    });

    let receiver = tokio::spawn(async move {
        let mut collected: Vec<Item> = Vec::new();
        while let Some(item) = stream.next().await {
            collected.push(item.expect("decode"));
        }
        collected
    });

    sender.await.expect("sender task");
    let received = receiver.await.expect("receiver task");
    assert_eq!(received.len(), 5);
    for i in 0..5u32 {
        assert_eq!(received[i as usize].n, i + 100);
        assert_eq!(received[i as usize].label, format!("s{i}"));
    }
}

// Suppress unused-import warnings for the tests that only use a
// subset of the imports.
#[allow(dead_code)]
fn _unused() {
    let _: AtomicUsize;
    let _: Ordering;
    let _: Bytes;
    let _: CallOptions;
}
