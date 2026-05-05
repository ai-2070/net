//! End-to-end SDK test for the typed nRPC **streaming** surface.
//!
//! Two `Mesh` instances built via `MeshBuilder` (the public SDK
//! entry point), connected via direct handshake. Server registers a
//! typed streaming handler via `serve_rpc_streaming_typed`; caller
//! invokes via `call_streaming_typed` and consumes the typed
//! `RpcStreamTyped<Resp>`. Pinned: clean stream end with N typed
//! chunks, handler `Err(message)` mapping, and chunk-level decode-
//! error termination.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::time::Duration;

use futures::StreamExt;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec, RpcError};
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
struct CountRequest {
    n: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct Tick {
    i: u32,
    label: String,
}

/// Typed handler emits N typed chunks via the typed sink, then
/// closes cleanly. Caller collects via `RpcStreamTyped<Tick>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_streaming_collects_all_chunks() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_streaming_typed(
            "ticker",
            Codec::Json,
            |req: CountRequest, sink| async move {
                for i in 0..req.n {
                    sink.send(&Tick {
                        i,
                        label: format!("t-{i}"),
                    })?;
                }
                Ok(())
            },
        )
        .expect("serve_rpc_streaming_typed");

    let mut stream = caller
        .call_streaming_typed::<CountRequest, Tick>(
            server.inner().node_id(),
            "ticker",
            &CountRequest { n: 4 },
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_streaming_typed");

    let mut got: Vec<Tick> = Vec::new();
    while let Some(item) = stream.next().await {
        got.push(item.expect("chunk must decode + be Ok"));
    }
    let want: Vec<Tick> = (0..4)
        .map(|i| Tick {
            i,
            label: format!("t-{i}"),
        })
        .collect();
    assert_eq!(got, want, "must yield all typed chunks in order");
}

/// Typed handler returns `Err(message)` after emitting some chunks
/// → caller observes those chunks then a terminal
/// `RpcError::ServerError` carrying the diagnostic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_streaming_handler_error_after_partial_stream() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_streaming_typed(
            "ticker_err",
            Codec::Json,
            |_req: CountRequest, sink| async move {
                sink.send(&Tick {
                    i: 0,
                    label: "first".into(),
                })?;
                sink.send(&Tick {
                    i: 1,
                    label: "second".into(),
                })?;
                // Give the pump a beat to drain before returning
                // Err so the two chunks reach the caller before
                // the terminal error frame.
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err("simulated typed failure".to_string())
            },
        )
        .expect("serve_rpc_streaming_typed");

    let mut stream = caller
        .call_streaming_typed::<CountRequest, Tick>(
            server.inner().node_id(),
            "ticker_err",
            &CountRequest { n: 0 },
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_streaming_typed");

    let mut got: Vec<Tick> = Vec::new();
    let mut terminal: Option<RpcError> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(t) => got.push(t),
            Err(e) => {
                terminal = Some(e);
                break;
            }
        }
    }
    assert_eq!(got.len(), 2, "must yield both pre-error chunks");
    assert_eq!(got[0].label, "first");
    assert_eq!(got[1].label, "second");
    match terminal.expect("must terminate with Err") {
        RpcError::ServerError { message, .. } => {
            assert!(
                message.contains("simulated typed failure"),
                "diagnostic must propagate, got {message:?}",
            );
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}

/// Server emits a chunk that DOES NOT decode as the caller's `Resp`
/// type → caller's typed stream terminates with a single decode-
/// error `Err`. Pin: client-side decode failure is surfaced to the
/// caller (not silently dropped).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_streaming_chunk_decode_failure_terminates_stream() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    // Server uses a different `Resp` shape than the caller will
    // expect. Both encode JSON, but the JSON shapes are
    // incompatible — caller's `Tick` decode will fail.
    #[derive(Serialize)]
    struct Wrong {
        not_a_tick: bool,
    }

    let _serve = server
        .serve_rpc_streaming_typed(
            "wrong_shape",
            Codec::Json,
            |_req: CountRequest, sink| async move {
                sink.send(&Wrong { not_a_tick: true })?;
                Ok(())
            },
        )
        .expect("serve_rpc_streaming_typed");

    let mut stream = caller
        .call_streaming_typed::<CountRequest, Tick>(
            server.inner().node_id(),
            "wrong_shape",
            &CountRequest { n: 0 },
            CallOptionsTyped::default(),
        )
        .await
        .expect("call_streaming_typed");

    let first = stream
        .next()
        .await
        .expect("must yield exactly one item")
        .expect_err("must surface as decode error");
    match first {
        RpcError::ServerError { message, .. } => {
            assert!(
                message.contains("client decode"),
                "decode-failure diagnostic must be marked, got {message:?}",
            );
        }
        other => panic!("expected ServerError(Internal), got {other:?}"),
    }
    // Subsequent polls return None — the typed stream marks itself
    // done after a decode failure.
    assert!(stream.next().await.is_none(), "stream must close after decode error");
}
