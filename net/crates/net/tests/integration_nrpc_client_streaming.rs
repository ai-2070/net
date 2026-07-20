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
use net::adapter::net::behavior::CapabilitySet;
use net::adapter::net::cortex::{
    RequestStream, RpcClientStreamingHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    RpcStreamingContext,
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

    // Exchange SIGNED capability announcements so each side TOFU-pins the
    // other's entity id (`peer_entity_id`). A Noise session authenticates the
    // transport, but the origin_hash <-> node_id binding is only learned from a
    // signed announcement — and the Gate-3 upload-grant classifier
    // (`classify_request_grant_route`) reads exactly that pin: an unpinned
    // caller classifies as `RelayedOrUntrusted`, so a flow-controlled REQUEST is
    // dropped before the fold and the caller just hangs until its deadline.
    // Production peers always announce; this harness did not, which made every
    // `request_window_initial` call unservable.
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("a announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("b announce");
    assert!(
        wait_until(
            || a.peer_entity_id(b_id).is_some() && b.peer_entity_id(a_id).is_some(),
            Duration::from_secs(5),
        )
        .await,
        "both peers must TOFU-pin each other before a flow-controlled call",
    );
}

/// Poll `cond` until true or `timeout` elapses.
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
            body: bytes::Bytes::copy_from_slice(&count.to_le_bytes()),
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
            body: bytes::Bytes::new(),
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

/// Hangs on `requests.next()` forever, ignoring cancellation.
/// Used to verify the server-side deadline guard force-drops the
/// handler when the caller declares `deadline` but no REQUEST_END
/// (or CANCEL) ever arrives.
struct HangForeverHandler;

#[async_trait::async_trait]
impl RpcClientStreamingHandler for HangForeverHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        // Pure sequential `next().await` — does NOT select on
        // ctx.cancellation. This emulates a misbehaving handler
        // that doesn't honor cooperative cancellation.
        while requests.next().await.is_some() {}
        // Add a deliberate forever-hang past the EOF in case the
        // caller did send something — exercises the deadline path
        // for handlers that ignore the stream's EOF signal too.
        std::future::pending::<()>().await;
        unreachable!()
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
            body: bytes::Bytes::new(),
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
        RpcError::ServerError {
            status, message, ..
        } => {
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
/// quickly, grants flow back, and THE SAME blocked send future
/// completes — the REQUEST_GRANT round trip, not just the block.
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

    // Third send must block on credit: the server is still pre-sleeping
    // (500 ms), so it has consumed nothing and emitted no auto-grant.
    //
    // PIN the future and re-poll it rather than dropping it. Dropping after
    // the first timeout proves only that credit is ENFORCED — a server that
    // never emitted a single grant would pass that assertion just as happily
    // (verified: with `add_request_grant_credits` neutered, the drop-based
    // form of this test still passed). Holding the same future and driving it
    // to completion is what actually pins the REQUEST_GRANT round trip.
    {
        let third = call.send(Bytes::from_static(b"c"));
        tokio::pin!(third);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut third)
                .await
                .is_err(),
            "third send must block on credit while server is still pre-sleeping"
        );
        tokio::time::timeout(Duration::from_secs(5), &mut third)
            .await
            .expect("a REQUEST_GRANT must unblock the blocked send once the server drains")
            .expect("send c");
    }

    let _reply = tokio::time::timeout(Duration::from_secs(5), call.finish())
        .await
        .expect("finish must complete")
        .expect("finish ok");

    // All three user sends reached the handler: "a" as the initial REQUEST
    // body, "b" from the second credit, and "c" once a grant replenished it.
    // finish()'s terminator (empty body + FLAG_END) yields no stream item.
    assert_eq!(
        consumed.load(Ordering::SeqCst),
        3,
        "handler must observe all three uploaded chunks",
    );
}

/// Regression (cubic-dev-ai bot P2): `CallOptions::request_window_initial
/// = Some(0)` must be rejected up front. The initial REQUEST is
/// lazy (deferred until the first `send`), so a Some(0) caller
/// would deadlock — `send().await` blocks waiting for a credit
/// but the server never even sees the call, so it can never
/// publish a REQUEST_GRANT. `None` means "no flow control" /
/// unbounded credit and stays accepted; `Some(n>=1)` is the
/// normal flow-control case.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_client_stream_rejects_zero_request_window() {
    let caller = build_node().await;
    let target = 0xC0DE_u64;
    let opts = CallOptions {
        request_window_initial: Some(0),
        ..CallOptions::default()
    };
    let err = match caller.call_client_stream(target, "anything", opts).await {
        Ok(_) => panic!("Some(0) must be rejected before any wire traffic"),
        Err(e) => e,
    };
    match err {
        RpcError::Codec { direction, message } => {
            assert_eq!(direction, CodecDirection::Encode);
            assert!(
                message.contains("request_window_initial"),
                "diagnostic must name the offending option: {message}",
            );
        }
        other => panic!("expected RpcError::Codec(Encode), got {other:?}"),
    }

    // Sanity: Some(1) still works (or at least gets past the
    // validation — it'll later fail with NoRoute because no peer
    // is wired up; we only care that we passed the deadlock guard).
    let opts = CallOptions {
        request_window_initial: Some(1),
        ..CallOptions::default()
    };
    let err = match caller.call_client_stream(target, "anything", opts).await {
        Ok(_) => panic!("no peer is connected; must fail with NoRoute, not Codec"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, RpcError::Codec { .. }),
        "Some(1) must clear the deadlock guard; got {err:?}",
    );
}

/// Server-side deadline guard: a handler that ignores cancellation
/// and hangs on the request stream must still be forced to terminate
/// once `deadline_ns` elapses. Without the guard, the per-call
/// sender in `RpcStreamingRequestFold::senders` would be orphaned
/// indefinitely.
#[tokio::test]
async fn client_streaming_server_deadline_force_drops_hanging_handler() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_client_stream("hang", Arc::new(HangForeverHandler))
        .expect("serve_rpc_client_stream");

    let opts = CallOptions {
        deadline: Some(std::time::Instant::now() + Duration::from_millis(300)),
        ..CallOptions::default()
    };
    let call = caller
        .call_client_stream(server.node_id(), "hang", opts)
        .await
        .expect("call_client_stream");
    // Hold the call open without sending — the handler is stuck on
    // `requests.next().await` waiting for chunks that never arrive.
    // The deadline guard must drop the handler future once
    // deadline_ns elapses; the terminal RESPONSE carries Internal.
    let started = std::time::Instant::now();
    let result = call.finish().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "deadline guard must terminate the call quickly; took {elapsed:?}",
    );
    let err = result.expect_err("hanging handler under deadline must error");
    match err {
        RpcError::Timeout { .. } => { /* caller-side deadline beat the server's terminal */ }
        RpcError::ServerError { status, .. } => {
            assert_eq!(
                status, 0x0006,
                "expected Internal (0x0006), got {status:#06x}"
            );
        }
        other => panic!("expected Timeout or ServerError(Internal), got {other:?}"),
    }
}
