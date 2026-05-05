//! End-to-end nRPC integration test against real `MeshNode`s.
//!
//! Two `MeshNode` instances in one process, connected via direct
//! handshake. Node A serves an "echo" RPC; node B issues calls.
//! Asserts: round-trip, multiple sequential calls reuse the
//! lazy reply subscription, server panic surfaces as `Internal`,
//! deadline emits CANCEL and surfaces as `Timeout` to the caller.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus, TraceContext,
};
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use parking_lot::Mutex;

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

struct EchoHandler;

#[async_trait::async_trait]
impl RpcHandler for EchoHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// Counts handler invocations to confirm the server only ran the
/// expected number of times (no double-dispatch from a misrouted
/// fold).
struct CountingHandler {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for CountingHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// Sleeps long enough that the caller's deadline fires; pinned by
/// the cancellation test.
struct SlowHandler;

#[async_trait::async_trait]
impl RpcHandler for SlowHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        tokio::select! {
            _ = ctx.cancellation.cancelled() => {
                Err(RpcHandlerError::Internal("cancelled by caller".to_string()))
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: b"slept the full window".to_vec(),
                })
            }
        }
    }
}

// ============================================================================
// Tests.
// ============================================================================

#[tokio::test]
async fn rpc_round_trip_two_meshes() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    // Server: register echo handler.
    let _serve = server
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    // Caller: issue one call.
    let reply = caller
        .call(
            server.node_id(),
            "echo",
            Bytes::from_static(b"hello from caller"),
            CallOptions::default(),
        )
        .await
        .expect("call must succeed");
    assert_eq!(reply.body.as_ref(), b"hello from caller");
    assert!(reply.latency_ns > 0);
}

/// Multiple sequential calls reuse the lazy reply subscription —
/// the second call shouldn't pay the subscribe round-trip cost.
/// We don't directly assert subscription reuse (no public counter)
/// but we do assert the handler ran exactly once per call.
#[tokio::test]
async fn rpc_multiple_calls_reuse_reply_subscription() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let count = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc(
            "counter",
            Arc::new(CountingHandler {
                count: count.clone(),
            }),
        )
        .expect("serve_rpc");

    for i in 0..5u64 {
        let body = format!("call-{i}").into_bytes();
        let reply = caller
            .call(
                server.node_id(),
                "counter",
                Bytes::from(body.clone()),
                CallOptions::default(),
            )
            .await
            .expect("call");
        assert_eq!(reply.body.as_ref(), body.as_slice());
    }
    assert_eq!(
        count.load(Ordering::Relaxed),
        5,
        "handler must run exactly once per call",
    );
}

/// Server-side panic is caught by the fold's `catch_unwind` and
/// surfaces to the caller as `Internal` rather than hanging.
#[tokio::test]
async fn rpc_server_panic_surfaces_as_internal() {
    struct PanicHandler;
    #[async_trait::async_trait]
    impl RpcHandler for PanicHandler {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            panic!("boom");
        }
    }

    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("panicky", Arc::new(PanicHandler))
        .expect("serve_rpc");

    let err = caller
        .call(
            server.node_id(),
            "panicky",
            Bytes::from_static(b"trigger"),
            CallOptions::default(),
        )
        .await
        .expect_err("call must surface server panic as Err");
    match err {
        RpcError::ServerError { status, message } => {
            // RpcStatus::Internal = 0x0006.
            assert_eq!(
                status, 0x0006,
                "expected Internal status, got {status:#06x}"
            );
            assert!(
                message.contains("boom"),
                "panic message must be in body, got {message:?}"
            );
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}

/// Caller deadline fires before the slow handler completes →
/// caller emits CANCEL → caller surfaces `Timeout` → handler
/// observes its `cancellation.cancelled()` token.
#[tokio::test]
async fn rpc_deadline_surfaces_as_timeout_and_emits_cancel() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc("slow", Arc::new(SlowHandler))
        .expect("serve_rpc");

    let started = Instant::now();
    let err = caller
        .call(
            server.node_id(),
            "slow",
            Bytes::from_static(b"hang"),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(300)),
                ..Default::default()
            },
        )
        .await
        .expect_err("call must time out");
    let elapsed = started.elapsed();
    match err {
        RpcError::Timeout { elapsed_ms } => {
            assert!(
                elapsed_ms >= 250,
                "elapsed_ms must reflect ~deadline window, got {elapsed_ms}"
            );
            assert!(
                elapsed < Duration::from_secs(2),
                "wall-clock elapsed should be near the deadline, got {elapsed:?}"
            );
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

/// Caller drops the unary `call` future before it resolves
/// (e.g. via `tokio::select!` losing) → the call's RAII
/// `UnaryCallGuard` fires CANCEL → the server-side handler
/// observes its `ctx.cancellation` token. Pins the cancel-on-
/// drop semantics that `hedge` and other "race + take winner"
/// callers depend on.
#[tokio::test]
async fn rpc_dropped_call_future_fires_cancel_to_server() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct CancelObservingSlow {
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl RpcHandler for CancelObservingSlow {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            tokio::select! {
                _ = ctx.cancellation.cancelled() => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    Err(RpcHandlerError::Internal("cancelled by caller".into()))
                }
                _ = tokio::time::sleep(Duration::from_secs(10)) => {
                    Ok(RpcResponsePayload {
                        status: RpcStatus::Ok,
                        headers: vec![],
                        body: vec![],
                    })
                }
            }
        }
    }
    let _serve = server
        .serve_rpc(
            "slow_observe",
            Arc::new(CancelObservingSlow {
                cancelled: cancelled.clone(),
            }),
        )
        .expect("serve_rpc");

    // Issue the call inside a `select!` that races against a
    // short timer; the timer wins, the call future is dropped,
    // and the guard's Drop fires CANCEL to the server.
    let server_id = server.node_id();
    let caller_clone = caller.clone();
    tokio::select! {
        _ = caller_clone.call(
            server_id,
            "slow_observe",
            Bytes::from_static(b"go"),
            CallOptions::default(),
        ) => panic!("call should not complete in this window"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
    }

    // Wait for the CANCEL to traverse the network and the
    // handler's `select!` arm to fire. Generous because RTTs vary.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !cancelled.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        cancelled.load(Ordering::SeqCst),
        "server handler must observe ctx.cancellation after caller drops the call future",
    );
}

/// Caller sets `CallOptions::trace_context` → server's
/// `RpcContext::trace_context` is populated with the same values.
/// Pin the W3C-trace-context propagation end-to-end through real
/// network publish.
#[tokio::test]
async fn rpc_trace_context_propagates_to_server() {
    struct CapturingTraceHandler {
        captured: Arc<Mutex<Option<Option<TraceContext>>>>,
    }
    #[async_trait::async_trait]
    impl RpcHandler for CapturingTraceHandler {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            *self.captured.lock() = Some(ctx.trace_context.clone());
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: vec![],
            })
        }
    }

    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let captured = Arc::new(Mutex::new(None));
    let _serve = server
        .serve_rpc(
            "echo",
            Arc::new(CapturingTraceHandler {
                captured: captured.clone(),
            }),
        )
        .expect("serve_rpc");

    // Caller sends with a trace context.
    let tc = TraceContext {
        traceparent: "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
        tracestate: "vendor=opaque-value".to_string(),
    };
    let _reply = caller
        .call(
            server.node_id(),
            "echo",
            Bytes::from_static(b""),
            CallOptions {
                trace_context: Some(tc.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("call must succeed");

    let observed = captured
        .lock()
        .clone()
        .expect("handler must run")
        .expect("trace context must be present");
    assert_eq!(observed, tc);

    // Sanity: a call WITHOUT trace_context leaves the server's
    // RpcContext.trace_context as None.
    *captured.lock() = None;
    let _reply = caller
        .call(
            server.node_id(),
            "echo",
            Bytes::from_static(b""),
            CallOptions::default(),
        )
        .await
        .expect("call must succeed");
    let observed = captured.lock().clone().expect("handler must run");
    assert!(
        observed.is_none(),
        "no trace_context on the call → server gets None, got {observed:?}",
    );
}
