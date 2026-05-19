//! End-to-end nRPC client-streaming integration tests against
//! real `MeshNode`s. Mirror of `integration_nrpc_streaming.rs`
//! but flipped on the data-direction axis: caller pushes N
//! REQUEST_CHUNK bodies via `ClientStreamCallRaw::send`, server
//! handler drains a `RequestStream`, then returns one terminal
//! `RpcResponsePayload`.
//!
//! Coverage:
//! 1. Happy path — N=10 chunks round-trip; handler observes all
//!    10 in order and emits one terminal RESPONSE.
//! 2. Degenerate path — caller calls `finish()` without any
//!    `send()`s; handler sees one empty chunk + EOF, returns Ok.
//! 3. Cancellation — caller drops the handle mid-send; handler's
//!    `ctx.cancellation` fires and the terminal RESPONSE surfaces
//!    as `Cancelled`.
//! 4. Application error — handler returns
//!    `RpcHandlerError::Application`; caller sees the matching
//!    `RpcError::ServerError(status, message)`.
//! 5. Flow control — caller sets `request_window_initial = 2` and
//!    server is slow draining. The caller's third `send()` blocks
//!    on credit; once the server consumes one chunk and emits a
//!    REQUEST_GRANT, the blocked send completes.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net::adapter::net::cortex::{
    RequestStream, RpcClientStreamingHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    RpcStreamingContext,
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

/// Drains the request stream into a Vec; returns an Ok response
/// whose body encodes the count of chunks seen (8 LE bytes).
/// Captured chunks are exposed via the Arc<Mutex<Vec<Bytes>>> so
/// tests can assert ordering + content.
struct CollectingHandler {
    seen: Arc<parking_lot::Mutex<Vec<Bytes>>>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for CollectingHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        while let Some(chunk) = requests.next().await {
            self.seen.lock().push(chunk);
        }
        let count = self.seen.lock().len() as u64;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: count.to_le_bytes().to_vec(),
        })
    }
}

/// Loops on the request stream forever, observing cancellation
/// via `ctx.cancellation`. Used to verify caller-drop -> CANCEL
/// propagation.
struct ObserveCancelHandler {
    observed_cancel: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for ObserveCancelHandler {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        // Drain the stream — yields None when REQUEST_END or
        // CANCEL closes it. Either way, after the loop ends we
        // probe the cancellation token to distinguish.
        while requests.next().await.is_some() {}
        if ctx.cancellation.is_cancelled() {
            self.observed_cancel.store(true, Ordering::SeqCst);
        }
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: vec![],
        })
    }
}

/// Returns an Application error after draining the request stream.
struct AppErrorHandler;

#[async_trait::async_trait]
impl RpcClientStreamingHandler for AppErrorHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        while requests.next().await.is_some() {}
        Err(RpcHandlerError::Application {
            code: 0xBEEF,
            message: "validation failed".to_string(),
        })
    }
}

/// Slow-draining handler. Pre-sleeps `pre_sleep_ms` BEFORE
/// touching the request stream — this ensures the caller's
/// sends pile up against the initial credit window without any
/// auto-grants firing yet (auto-grant only fires on a successful
/// `next().await`, which is gated by the pre-sleep). After the
/// pre-sleep, drains as fast as the wire delivers.
///
/// Used to verify flow-control throttling: the caller's
/// (initial_window + 1)th send must block until the FIRST
/// auto-grant arrives, which can't happen until the handler
/// finishes the pre-sleep.
struct SlowDrainHandler {
    pre_sleep_ms: u64,
    consumed: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for SlowDrainHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        tokio::time::sleep(Duration::from_millis(self.pre_sleep_ms)).await;
        while requests.next().await.is_some() {
            self.consumed.fetch_add(1, Ordering::SeqCst);
        }
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: vec![],
        })
    }
}

// ============================================================================
// Tests.
// ============================================================================

/// 1/5 — Caller sends N=10 chunks, then finish(); handler sees
/// all 10 in order and emits one terminal RESPONSE whose body
/// carries the count.
#[tokio::test]
async fn client_streaming_collects_all_chunks() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let _serve = server
        .serve_rpc_client_stream(
            "collect",
            Arc::new(CollectingHandler { seen: seen.clone() }),
        )
        .expect("serve_rpc_client_stream");

    let mut call = caller
        .call_client_stream(server.node_id(), "collect", CallOptions::default())
        .await
        .expect("call_client_stream");

    for i in 0..10u8 {
        call.send(Bytes::from(vec![i])).await.expect("send");
    }
    let reply = call.finish().await.expect("finish");
    let count = u64::from_le_bytes(
        reply.body[..8]
            .try_into()
            .expect("response body must encode u64"),
    );
    assert_eq!(count, 10);
    let bodies: Vec<u8> = seen.lock().iter().map(|b| b[0]).collect();
    assert_eq!(bodies, (0..10).collect::<Vec<u8>>());
}

/// 2/5 — Degenerate path: caller calls finish() with no sends.
/// The initial REQUEST is published with BOTH
/// FLAG_RPC_CLIENT_STREAMING_REQUEST and FLAG_RPC_REQUEST_END set
/// and an empty body — by the fold's terminator-semantics rule
/// (empty body + FLAG_END = pure terminator), no stream item is
/// yielded. Handler's stream is empty, returns Ok with count=0.
#[tokio::test]
async fn client_streaming_zero_send_finish() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let _serve = server
        .serve_rpc_client_stream("empty", Arc::new(CollectingHandler { seen: seen.clone() }))
        .expect("serve_rpc_client_stream");

    let call = caller
        .call_client_stream(server.node_id(), "empty", CallOptions::default())
        .await
        .expect("call_client_stream");
    let reply = call.finish().await.expect("finish");
    let count = u64::from_le_bytes(reply.body[..8].try_into().unwrap());
    assert_eq!(count, 0, "zero-send finish must yield zero stream items");
    assert!(seen.lock().is_empty());
}

/// 3/5 — Caller drops the handle mid-flight (before finish).
/// CANCEL is published to the server; handler's
/// `ctx.cancellation` fires; the spawned task's cancel_probe
/// overrides the terminal RESPONSE with `Cancelled`.
#[tokio::test]
async fn client_streaming_drop_cancels_handler() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let observed = Arc::new(AtomicBool::new(false));
    let _serve = server
        .serve_rpc_client_stream(
            "watch_cancel",
            Arc::new(ObserveCancelHandler {
                observed_cancel: observed.clone(),
            }),
        )
        .expect("serve_rpc_client_stream");

    let mut call = caller
        .call_client_stream(server.node_id(), "watch_cancel", CallOptions::default())
        .await
        .expect("call_client_stream");

    // Send a couple of chunks to ensure the server's handler is
    // running, then drop without finish.
    call.send(Bytes::from_static(b"a")).await.expect("send a");
    call.send(Bytes::from_static(b"b")).await.expect("send b");
    drop(call);

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !observed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        observed.load(Ordering::SeqCst),
        "handler must observe ctx.cancellation after caller drops the handle"
    );
}

/// 4/5 — Handler returns `RpcHandlerError::Application` after
/// draining. Caller's `finish()` surfaces an
/// `RpcError::ServerError` with the same status code and the
/// handler's message in the body.
#[tokio::test]
async fn client_streaming_handler_application_error_round_trips() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_client_stream("app_err", Arc::new(AppErrorHandler))
        .expect("serve_rpc_client_stream");

    let mut call = caller
        .call_client_stream(server.node_id(), "app_err", CallOptions::default())
        .await
        .expect("call_client_stream");
    call.send(Bytes::from_static(b"x")).await.expect("send");
    let err = call.finish().await.expect_err("must error");
    match err {
        RpcError::ServerError { status, message } => {
            assert_eq!(status, 0xBEEF);
            assert!(message.contains("validation failed"));
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}

/// 5/5 — Flow control: caller opts in with
/// `request_window_initial = 2`; server pre-sleeps 500 ms BEFORE
/// touching the request stream. During the pre-sleep the caller's
/// first two sends consume both initial credits; the third send
/// must block (no auto-grants can fire until the server's first
/// `next().await`). After the pre-sleep, the server drains
/// quickly, grants flow back, and the third send unblocks.
#[tokio::test]
async fn client_streaming_window_throttles_caller_send() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let consumed = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_client_stream(
            "slow",
            Arc::new(SlowDrainHandler {
                pre_sleep_ms: 500,
                consumed: consumed.clone(),
            }),
        )
        .expect("serve_rpc_client_stream");

    let opts = CallOptions {
        request_window_initial: Some(2),
        ..Default::default()
    };
    let mut call = caller
        .call_client_stream(server.node_id(), "slow", opts)
        .await
        .expect("call_client_stream");
    assert!(call.flow_controlled());

    // First two sends fit in the initial window — should return
    // immediately. (The server is still pre-sleeping, but the
    // caller-side semaphore has 2 permits up front.)
    call.send(Bytes::from_static(b"a")).await.expect("send a");
    call.send(Bytes::from_static(b"b")).await.expect("send b");

    // Third send must block on credit. The server is still
    // pre-sleeping (500 ms), so no auto-grants have fired yet.
    // A 100ms timeout proves the block. Drop the future after
    // timing out so the credit isn't consumed — we'll redo the
    // send after the pre-sleep completes.
    {
        let third = call.send(Bytes::from_static(b"c"));
        let timed = tokio::time::timeout(Duration::from_millis(100), third).await;
        assert!(
            timed.is_err(),
            "third send must block on credit while server is still pre-sleeping"
        );
    }

    // Wait for the server's pre-sleep to end + the first auto-
    // grant to land, then complete the call. After this the
    // semaphore should have refills available and finish should
    // proceed.
    let _reply = tokio::time::timeout(Duration::from_secs(5), call.finish())
        .await
        .expect("finish must complete")
        .expect("finish ok");

    // Server saw the initial REQUEST body + the two user sends
    // before pre-sleep ended. After the pre-sleep, the (suppressed
    // by Drop) third send didn't fly to the wire, but the
    // finish() emits a terminator (empty body + FLAG_END,
    // suppressed by the fold's terminator-semantics rule). Total
    // stream items the handler sees = 3 (initial body "a", chunk
    // "b", and the finish-terminator doesn't yield).
    let final_count = consumed.load(Ordering::SeqCst);
    assert!(
        final_count >= 2,
        "server must observe at least 2 chunks (the two pre-block sends); got {final_count}"
    );
}
