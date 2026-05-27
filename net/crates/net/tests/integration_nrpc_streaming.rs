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
use net::adapter::net::behavior::CapabilitySet;
use net::adapter::net::cortex::{
    RpcContext, RpcHandlerError, RpcResponseSink, RpcStreamingHandler,
};
use net::adapter::net::mesh_rpc::{CallOptions, CodecDirection, RpcError};
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
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
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
    async fn call(&self, ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
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
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
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

/// Flow control: server emits 20 chunks back-to-back; caller
/// sets `stream_window_initial = 3` and DOES NOT consume. The
/// server-side metric `streaming_chunks_emitted_total` should
/// stall at 3 (the initial window). Pin: server pump genuinely
/// blocks on credit, not "everything goes through, caller just
/// drains slowly."
#[tokio::test]
async fn rpc_streaming_window_throttles_pump_until_grants() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    /// Emits N chunks back-to-back via sink.send (handler-side
    /// is non-blocking; the pump is what's flow-controlled).
    struct EmitNHandler {
        n: usize,
    }
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcStreamingHandler for EmitNHandler {
        async fn call(
            &self,
            _ctx: RpcContext,
            sink: net::adapter::net::cortex::RpcResponseSink,
        ) -> Result<(), RpcHandlerError> {
            for i in 0..self.n {
                sink.send(format!("chunk-{i}").into_bytes());
            }
            Ok(())
        }
    }

    let _serve = server
        .serve_rpc_streaming("throttle", Arc::new(EmitNHandler { n: 20 }))
        .expect("serve_rpc_streaming");

    let stream = caller
        .call_streaming(
            server.node_id(),
            "throttle",
            Bytes::from_static(b""),
            CallOptions {
                stream_window_initial: Some(3),
                ..Default::default()
            },
        )
        .await
        .expect("call_streaming");
    assert!(stream.flow_controlled(), "stream must be flow-controlled");

    // Two-phase poll-then-verify-stable: (1) wait until the
    // pump has emitted the initial window (proves the server
    // got the request and started streaming), (2) sleep a small
    // margin and re-read — flow control should hold the count
    // STILL at 3 (proves the pump genuinely blocks on credit,
    // not "everything goes through, caller just hasn't drained").
    // The previous version used `sleep(300ms); assert == 3`,
    // which flaked when the server was slow to emit even the
    // initial 3 (read too early → assertion saw < 3 chunks).
    let count = |service: &str| -> u64 {
        server
            .rpc_metrics_snapshot()
            .services
            .iter()
            .find(|s| s.service == service)
            .map(|s| s.streaming_chunks_emitted_total)
            .unwrap_or(0)
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while count("throttle") < 3 {
        if std::time::Instant::now() >= deadline {
            panic!(
                "pump didn't reach initial window of 3 within 2s; got {}",
                count("throttle"),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Now verify the count stays at 3 over a stability window —
    // the server's pump should be blocked on the empty
    // semaphore, not racing toward 20.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        count("throttle"),
        3,
        "pump must stop at initial_window=3 without grants",
    );

    // Hold the stream so it doesn't drop and tear down the
    // server-side handler. Drop releases the test's reference.
    drop(stream);
}

/// Flow control under auto-grant: caller sets a small window, the
/// server emits N >> window chunks, and the caller drains
/// normally. RpcStream::poll_next auto-grants 1 credit per
/// delivered chunk → server pump never starves → all N chunks
/// arrive in order.
#[tokio::test]
async fn rpc_streaming_auto_grant_drains_full_stream_under_small_window() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    struct EmitN {
        n: usize,
    }
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcStreamingHandler for EmitN {
        async fn call(
            &self,
            _ctx: RpcContext,
            sink: net::adapter::net::cortex::RpcResponseSink,
        ) -> Result<(), RpcHandlerError> {
            for i in 0..self.n {
                sink.send(format!("c-{i}").into_bytes());
            }
            Ok(())
        }
    }

    let _serve = server
        .serve_rpc_streaming("autograntn", Arc::new(EmitN { n: 25 }))
        .expect("serve_rpc_streaming");

    use futures::StreamExt;
    let mut stream = caller
        .call_streaming(
            server.node_id(),
            "autograntn",
            Bytes::from_static(b""),
            CallOptions {
                stream_window_initial: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("call_streaming");

    let mut got = Vec::new();
    while let Some(item) = stream.next().await {
        got.push(String::from_utf8(item.expect("ok").to_vec()).unwrap());
    }
    let want: Vec<String> = (0..25).map(|i| format!("c-{i}")).collect();
    assert_eq!(
        got, want,
        "auto-grant must let the full stream flow through"
    );
}

/// Explicit `RpcStream::grant(n)` adds credit on demand. Pin:
/// after open with window=2, manually granting 5 should let the
/// pump emit (2 + 5) = 7 chunks total even with no auto-grant
/// (which only fires when chunks are consumed via poll_next —
/// here we never poll until the end).
#[tokio::test]
async fn rpc_streaming_explicit_grant_unblocks_pump() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    struct EmitN {
        n: usize,
    }
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcStreamingHandler for EmitN {
        async fn call(
            &self,
            _ctx: RpcContext,
            sink: net::adapter::net::cortex::RpcResponseSink,
        ) -> Result<(), RpcHandlerError> {
            for i in 0..self.n {
                sink.send(format!("g-{i}").into_bytes());
            }
            Ok(())
        }
    }

    let _serve = server
        .serve_rpc_streaming("explicitgrant", Arc::new(EmitN { n: 20 }))
        .expect("serve_rpc_streaming");

    let stream = caller
        .call_streaming(
            server.node_id(),
            "explicitgrant",
            Bytes::from_static(b""),
            CallOptions {
                stream_window_initial: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("call_streaming");

    // Two-phase: (1) poll until initial window is consumed,
    // (2) verify stable. Same shape as the throttle test above —
    // removes the dependency on a hard-coded sleep being long
    // enough for the server to get going AND short enough that
    // the test doesn't drag.
    let count = |service: &str| -> u64 {
        server
            .rpc_metrics_snapshot()
            .services
            .iter()
            .find(|s| s.service == service)
            .map(|s| s.streaming_chunks_emitted_total)
            .unwrap_or(0)
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while count("explicitgrant") < 2 {
        if std::time::Instant::now() >= deadline {
            panic!(
                "pump didn't reach initial window of 2 within 2s; got {}",
                count("explicitgrant"),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        count("explicitgrant"),
        2,
        "pump must stall at the initial window with no consumption + no grants",
    );

    // Explicit grant of 5 → server should now emit 5 more
    // (total 7). Poll until reached, then verify stable.
    stream.grant(5);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while count("explicitgrant") < 7 {
        if std::time::Instant::now() >= deadline {
            panic!(
                "after grant(5), pump didn't reach 7 within 2s; got {}",
                count("explicitgrant"),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        count("explicitgrant"),
        7,
        "after grant(5), pump must emit exactly 5 more (total 7) and stop again",
    );

    drop(stream);
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

/// Regression: `CallOptions::stream_window_initial = Some(0)` must
/// be rejected up front. Server's response pump awaits one credit
/// per chunk; the caller's auto-grant only fires on consumed chunks,
/// so the first chunk can never be delivered. Symmetric with the
/// request-side `request_window_initial = Some(0)` guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_streaming_rejects_zero_stream_window() {
    let caller = build_node().await;
    let target = 0xC0DE_u64;
    let opts = CallOptions {
        stream_window_initial: Some(0),
        ..CallOptions::default()
    };
    let err = match caller
        .call_streaming(target, "anything", Bytes::new(), opts)
        .await
    {
        Ok(_) => panic!("Some(0) must be rejected before any wire traffic"),
        Err(e) => e,
    };
    match err {
        RpcError::Codec { direction, message } => {
            assert_eq!(direction, CodecDirection::Encode);
            assert!(
                message.contains("stream_window_initial"),
                "diagnostic must name the offending option: {message}",
            );
        }
        other => panic!("expected RpcError::Codec(Encode), got {other:?}"),
    }

    // Sanity: Some(1) passes the guard (and will fail later for an
    // unrelated reason since no peer is wired).
    let opts = CallOptions {
        stream_window_initial: Some(1),
        ..CallOptions::default()
    };
    let err = match caller
        .call_streaming(target, "anything", Bytes::new(), opts)
        .await
    {
        Ok(_) => panic!("no peer connected; must fail with a non-Codec error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, RpcError::Codec { .. }),
        "Some(1) must clear the deadlock guard; got {err:?}",
    );
}

// ============================================================================
// `call_service_streaming` — capability-routed mirror of `call_service`
// that terminates in `call_streaming` rather than the unary `call`.
//
// Two-server topology: both servers register `serve_rpc_streaming` against
// the same service name + announce capabilities, the caller calls
// `call_service_streaming` and asserts the stream collects cleanly from
// whichever server the routing policy picked. Confirms the cap-routed
// surface composes the existing health-filter + cap-auth gate from
// `call_service` with the existing chunk-streaming behavior from
// `call_streaming`.
// ============================================================================

/// Best-effort polling helper — capability propagation is async, so we
/// wait until the caller's index sees both servers before issuing calls.
async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

#[tokio::test]
async fn call_service_streaming_routes_via_capability_index() {
    let server_a = build_node().await;
    let server_b = build_node().await;
    let caller = build_node().await;

    handshake_pair(&caller, &server_a).await;
    handshake_pair(&caller, &server_b).await;

    // Both servers register the same streaming service.
    let _serve_a = server_a
        .serve_rpc_streaming("counter", Arc::new(CounterStreamHandler { count: 5 }))
        .expect("serve_rpc_streaming A");
    let _serve_b = server_b
        .serve_rpc_streaming("counter", Arc::new(CounterStreamHandler { count: 5 }))
        .expect("serve_rpc_streaming B");

    // Both announce capabilities so the caller's index sees both.
    server_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce A");
    server_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce B");

    // Wait for capability propagation before issuing the call.
    assert!(
        wait_until(
            || {
                let nodes = caller.find_service_nodes("counter");
                nodes.contains(&server_a.node_id()) && nodes.contains(&server_b.node_id())
            },
            Duration::from_secs(5),
        )
        .await,
        "capability index must discover both servers; sees {:?}",
        caller.find_service_nodes("counter"),
    );

    // Capability-routed streaming call — caller does not pick the target.
    let mut stream = caller
        .call_service_streaming("counter", Bytes::from_static(b"go"), CallOptions::default())
        .await
        .expect("call_service_streaming must succeed");

    let mut collected: Vec<String> = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk = item.expect("chunk must be Ok");
        collected.push(String::from_utf8(chunk.to_vec()).unwrap());
    }
    let expected: Vec<String> = (0..5).map(|i| format!("chunk-{i}")).collect();
    assert_eq!(
        collected, expected,
        "call_service_streaming must collect all chunks from whichever \
         server the routing policy picked",
    );
}

#[tokio::test]
async fn call_service_streaming_no_servers_returns_no_route() {
    let caller = build_node().await;
    caller.start();

    let err = match caller
        .call_service_streaming(
            "nonexistent",
            Bytes::from_static(b"x"),
            CallOptions::default(),
        )
        .await
    {
        Ok(_) => panic!("call_service_streaming for unknown service must fail"),
        Err(e) => e,
    };

    match err {
        RpcError::NoRoute { reason, .. } => {
            assert!(
                reason.contains("nonexistent"),
                "NoRoute diagnostic must name the missing service: {reason}",
            );
        }
        other => panic!("expected RpcError::NoRoute, got {other:?}"),
    }
}
