//! End-to-end nRPC streaming integration tests against real
//! `MeshNode`s. Mirrors `integration_nrpc_mesh.rs` but pins the
//! streaming-response path: server emits N chunks via
//! `RpcResponseSink`, caller collects via `RpcStream`, terminal
//! frames close the stream cleanly OR with an error, and a
//! mid-stream drop emits CANCEL + cooperatively stops the handler.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net::adapter::net::cortex::{
    RpcContext, RpcHandlerError, RpcResponseSink, RpcStreamingHandler,
};
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

// ============================================================================
// Test handlers.
// ============================================================================

/// Emits `count` chunks (`chunk-0`, `chunk-1`, ...) then closes
/// the stream cleanly.
struct CounterStreamHandler {
    count: usize,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for CounterStreamHandler {
    async fn call(
        &self,
        _ctx: RpcContext,
        sink: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        for i in 0..self.count {
            sink.send(format!("chunk-{i}").into_bytes());
        }
        Ok(())
    }
}

/// Emits chunks until cancelled. Sets `observed_cancel` once the
/// caller's CANCEL fires.
struct ForeverStreamHandler {
    observed_cancel: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for ForeverStreamHandler {
    async fn call(
        &self,
        ctx: RpcContext,
        sink: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        let mut i: u64 = 0;
        loop {
            tokio::select! {
                _ = ctx.cancellation.cancelled() => {
                    self.observed_cancel.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    sink.send(format!("tick-{i}").into_bytes());
                    i += 1;
                }
            }
        }
    }
}

/// Emits two chunks then returns an Err so the caller observes a
/// terminal `ServerError` after a partial stream.
struct ErrAfterTwoHandler;

#[async_trait::async_trait]
impl RpcStreamingHandler for ErrAfterTwoHandler {
    async fn call(
        &self,
        _ctx: RpcContext,
        sink: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        sink.send(Bytes::from_static(b"first"));
        sink.send(Bytes::from_static(b"second"));
        // Give the pump a beat to drain before returning Err.
        tokio::time::sleep(Duration::from_millis(20)).await;
        Err(RpcHandlerError::Internal("simulated failure".into()))
    }
}

// ============================================================================
// Tests.
// ============================================================================

/// Server emits N chunks → caller collects all N + sees clean EOF.
#[tokio::test]
async fn rpc_streaming_collects_all_chunks() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_streaming("counter", Arc::new(CounterStreamHandler { count: 5 }))
        .expect("serve_rpc_streaming");

    let mut stream = caller
        .call_streaming(
            server.node_id(),
            "counter",
            Bytes::from_static(b"go"),
            CallOptions::default(),
        )
        .await
        .expect("call_streaming must succeed");

    let mut collected: Vec<String> = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk = item.expect("chunk must be Ok");
        collected.push(String::from_utf8(chunk.to_vec()).unwrap());
    }
    let expected: Vec<String> = (0..5).map(|i| format!("chunk-{i}")).collect();
    assert_eq!(collected, expected, "must yield all chunks in order");
}

/// Caller drops the stream mid-flight → CANCEL is emitted →
/// handler's `ctx.cancellation` fires.
#[tokio::test]
async fn rpc_streaming_drop_cancels_handler() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let observed = Arc::new(AtomicBool::new(false));
    let _serve = server
        .serve_rpc_streaming(
            "forever",
            Arc::new(ForeverStreamHandler {
                observed_cancel: observed.clone(),
            }),
        )
        .expect("serve_rpc_streaming");

    let mut stream = caller
        .call_streaming(
            server.node_id(),
            "forever",
            Bytes::from_static(b"go"),
            CallOptions::default(),
        )
        .await
        .expect("call_streaming");

    // Pull a couple of chunks then drop the stream.
    let _ = stream.next().await.expect("first chunk").expect("Ok");
    let _ = stream.next().await.expect("second chunk").expect("Ok");
    drop(stream);

    // Allow the CANCEL to propagate and the handler's select! arm
    // to fire. Generous because handshake-level RTTs vary.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !observed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        observed.load(Ordering::SeqCst),
        "handler must observe ctx.cancellation after caller drops the stream",
    );
}

/// Server emits 2 chunks then returns Err → caller sees those 2
/// chunks then a terminal `RpcError::ServerError`.
#[tokio::test]
async fn rpc_streaming_terminal_error_after_partial_stream() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_streaming("err_after_two", Arc::new(ErrAfterTwoHandler))
        .expect("serve_rpc_streaming");

    let mut stream = caller
        .call_streaming(
            server.node_id(),
            "err_after_two",
            Bytes::from_static(b"go"),
            CallOptions::default(),
        )
        .await
        .expect("call_streaming");

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut terminal_err: Option<RpcError> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(b) => chunks.push(b.to_vec()),
            Err(e) => {
                terminal_err = Some(e);
                break;
            }
        }
    }
    assert_eq!(
        chunks,
        vec![b"first".to_vec(), b"second".to_vec()],
        "must yield both pre-error chunks",
    );
    match terminal_err.expect("must terminate with Err") {
        RpcError::ServerError { status, message } => {
            // RpcStatus::Internal = 0x0006.
            assert_eq!(status, 0x0006, "expected Internal, got {status:#06x}");
            assert!(
                message.contains("simulated failure"),
                "diagnostic must propagate, got {message:?}",
            );
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}
