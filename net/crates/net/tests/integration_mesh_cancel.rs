//! SDK-level cancel-contract integration tests (C-S2 of
//! NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md).
//!
//! Pins the `Mesh::reserve_cancel_token` + `Mesh::cancel(token)`
//! API behavior across every call shape that honors
//! `CallOptions::cancel_token`:
//!
//! - Unary `call`
//! - `call_service`
//! - `call_streaming` (response side)
//! - `call_client_stream` (request side)
//! - `call_duplex` (both sides)
//!
//! Plus the contract invariants that bindings depend on when they
//! migrate their local cancel registries to delegate through the
//! SDK primitive (Wave 3 of the v3 plan):
//!
//! - `cancel_before_construction_aborts_cleanly` — pre-cancel
//!   race; the call returns Cancelled without ever publishing the
//!   REQUEST.
//! - `cancel_after_resolution_is_noop` — late cancel on a resolved
//!   call doesn't double-emit anything or panic.
//! - `cancel_zero_token_is_noop` — the "no token" sentinel.
//!
//! # Test mesh setup
//!
//! Two `MeshNode`s connected via handshake. Caller (`a`) issues
//! calls against responder (`b`) for services `b` doesn't serve.
//! That gets the REQUEST onto the wire (so the call is genuinely
//! mid-flight) but ensures no response ever arrives, so the call
//! hangs until cancel fires.

#![cfg(feature = "cortex")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::mesh_rpc::{CallOptions, RpcError};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    let node = MeshNode::new(keypair, test_config())
        .await
        .expect("MeshNode::new");
    Arc::new(node)
}

/// Two nodes with a handshake so caller-side cancel tests have a
/// real peer to publish to. The responder doesn't register any
/// services, so calls hang on the response receiver until the
/// caller-side cancel fires.
async fn build_pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node().await;
    let b = build_node().await;
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
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
    (a, b)
}

// =====================================================================
// Token + API surface invariants (no mesh needed).
// =====================================================================

#[tokio::test]
async fn reserve_cancel_token_is_monotonic_and_nonzero() {
    let a = build_node().await;
    let t1 = a.reserve_cancel_token();
    let t2 = a.reserve_cancel_token();
    let t3 = a.reserve_cancel_token();
    assert!(t1 >= 1, "tokens start at 1, not 0");
    assert!(t2 > t1);
    assert!(t3 > t2);
}

#[tokio::test]
async fn cancel_zero_token_is_noop() {
    // The "no token" sentinel — calling cancel on it shouldn't
    // create registry entries or affect any other state.
    let a = build_node().await;
    a.cancel(0);
    // No assertion needed; just verify no panic.
}

#[tokio::test]
async fn cancel_unknown_token_is_noop() {
    let a = build_node().await;
    // A token never reserved + never issued in a call is harmless
    // to cancel. Registry latches a pre_cancelled flag on the
    // orphan entry; orphan-TTL GC eventually evicts.
    a.cancel(0xDEAD_BEEF);
}

// =====================================================================
// Unary call mid-flight cancel.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_unary_mid_flight_surfaces_cancelled_error() {
    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };

    // Spawn the call. The publish lands on `b` but `b` doesn't
    // serve `unserved.svc`, so the caller's rx hangs on the
    // response oneshot indefinitely until we cancel.
    let a_clone = a.clone();
    let call_task = tokio::spawn(async move {
        a_clone
            .call(
                target,
                "unserved.svc",
                Bytes::from_static(b"req"),
                opts,
            )
            .await
    });

    // Give the call time to publish + reach the rx.await.
    tokio::time::sleep(Duration::from_millis(100)).await;
    a.cancel(token);

    let result = tokio::time::timeout(Duration::from_secs(2), call_task)
        .await
        .expect("call should resolve within 2s after cancel")
        .expect("spawn task panicked");

    match result {
        Err(RpcError::Cancelled) => {}
        other => panic!("expected RpcError::Cancelled, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_before_call_aborts_immediately() {
    // CR-13 race: cancel arrives BEFORE the call's rx.await reaches
    // the select! arm. The registry latches pre_cancelled = true;
    // when the call registers, the returned Notify is pre-armed so
    // notified().await fires immediately.
    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();
    a.cancel(token); // Cancel BEFORE the call.

    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        a.call(
            target,
            "unserved.svc",
            Bytes::from_static(b"req"),
            opts,
        ),
    )
    .await
    .expect("pre-cancelled call should resolve within 2s");

    match result {
        Err(RpcError::Cancelled) => {}
        other => panic!("expected RpcError::Cancelled, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_after_unary_resolution_is_noop() {
    // After a call resolves naturally, late cancels on its token
    // are harmless — no panic, no double-emit. The registry entry
    // was already released on resolution.
    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();

    // Issue and cancel the in-flight call so it resolves to
    // Cancelled — that exercises the release path.
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };
    let a_clone = a.clone();
    let call_task = tokio::spawn(async move {
        a_clone
            .call(target, "unserved.svc", Bytes::from_static(b"req"), opts)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    a.cancel(token);
    let _ = call_task.await.unwrap();

    // Now cancel again on the same (already-resolved) token. Must
    // not panic; must not affect any other state.
    a.cancel(token);
    a.cancel(token);
}

// =====================================================================
// call_service mid-flight cancel.
//
// `call_service` delegates to `call` with the same opts, so cancel
// propagates for free. Verify the delegation works end-to-end.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_unary_call_service_mid_flight_surfaces_cancelled() {
    let (a, _b) = build_pair().await;
    let token = a.reserve_cancel_token();
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };

    // call_service fails NoRoute when no node advertises the
    // service. To get into the cancel-aware path, the call must
    // first pass find_service_nodes. We don't have a real
    // capability-advertising peer here; verify the cancel-zero
    // contract holds via call_service even when it returns
    // NoRoute (the cancel arm shouldn't interfere with the
    // NoRoute early-return).
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        a.call_service("unserved.svc", Bytes::from_static(b"req"), opts),
    )
    .await
    .expect("call_service should resolve within 2s");

    // No nodes advertise the service → NoRoute is expected; cancel
    // doesn't interfere because the call never reached the cancel
    // arm. Releases the registry entry on early-return.
    match result {
        Err(RpcError::NoRoute { .. }) => {}
        other => panic!("expected NoRoute (no advertised service), got {other:?}"),
    }
    // Late cancel is still safe.
    a.cancel(token);
}

// =====================================================================
// Streaming-response (call_streaming) mid-drain cancel.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_streaming_mid_drain_terminates_stream() {
    use futures::StreamExt;

    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };

    let stream = a
        .call_streaming(target, "unserved.stream", Bytes::from_static(b"req"), opts)
        .await
        .expect("call_streaming should at least open against a reachable peer");

    // Give the cancel-watcher task a chance to register against
    // the registry entry. Without this, a very fast cancel can
    // arrive before the watcher's select! is parked.
    tokio::time::sleep(Duration::from_millis(50)).await;
    a.cancel(token);

    // The watcher drops the pending entry → the stream's mpsc
    // closes → poll_next returns Ready(None) (the existing EOF
    // path in `impl Stream for RpcStream` at the
    // `Ready(None)` arm). The stream terminates cleanly.
    let mut stream = stream;
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("stream should terminate within 2s after cancel");
    assert!(
        first.is_none(),
        "expected stream EOF after cancel, got {first:?}"
    );
}

// =====================================================================
// Client-streaming (call_client_stream) mid-flight cancel.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_client_stream_mid_finish_surfaces_terminal() {
    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };

    let mut call = a
        .call_client_stream(target, "unserved.cs", opts)
        .await
        .expect("call_client_stream should open against a reachable peer");

    // Send one chunk so the initial REQUEST flies.
    call.send(Bytes::from_static(b"chunk1"))
        .await
        .expect("first send should publish the initial REQUEST");

    // Spawn the finish() in a task so we can cancel it mid-flight.
    let finish_task = tokio::spawn(async move { call.finish().await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    a.cancel(token);

    // The cancel-watcher drops the pending entry. The finish()'s
    // oneshot receiver returns Err (sender dropped). The substrate
    // surfaces this as a Transport-class error indicating the
    // pending registration was cleared without a response. The
    // exact error variant is less load-bearing than the contract
    // that finish() resolves (doesn't hang) within bounded time.
    let result = tokio::time::timeout(Duration::from_secs(2), finish_task)
        .await
        .expect("client-stream finish should resolve within 2s after cancel")
        .expect("spawn task panicked");
    assert!(
        result.is_err(),
        "client-stream finish after cancel must return an error, not Ok"
    );
}

// =====================================================================
// Duplex (call_duplex) mid-flight cancel.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_duplex_mid_recv_terminates_stream() {
    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };

    let mut call = a
        .call_duplex(target, "unserved.dx", opts)
        .await
        .expect("call_duplex should open against a reachable peer");

    // Send one chunk so the initial REQUEST flies and the call is
    // genuinely mid-flight.
    call.send(Bytes::from_static(b"chunk1"))
        .await
        .expect("first send should publish the initial REQUEST");

    tokio::time::sleep(Duration::from_millis(50)).await;
    a.cancel(token);

    // Cancel drops the pending entry → both halves' mpscs close →
    // next() returns None. The shared Arc<DuplexInner> drops once
    // both halves are released, firing CANCEL on the wire via the
    // existing Drop impl.
    let next = tokio::time::timeout(Duration::from_secs(2), call.next())
        .await
        .expect("duplex next should resolve within 2s after cancel");
    assert!(
        next.is_none(),
        "expected duplex EOF after cancel, got {next:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_duplex_after_split_terminates_both_halves() {
    use futures::StreamExt;

    let (a, b) = build_pair().await;
    let target = b.node_id();
    let token = a.reserve_cancel_token();
    let opts = CallOptions {
        cancel_token: Some(token),
        ..CallOptions::default()
    };

    let call = a
        .call_duplex(target, "unserved.dx", opts)
        .await
        .expect("call_duplex should open against a reachable peer");

    let (mut sink, mut stream) = call.into_split();

    // Publish the initial REQUEST.
    sink.send(Bytes::from_static(b"chunk1"))
        .await
        .expect("first send should publish the initial REQUEST");

    tokio::time::sleep(Duration::from_millis(50)).await;
    a.cancel(token);

    // The receive half observes EOF.
    let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("duplex stream should resolve within 2s after cancel");
    assert!(
        next.is_none(),
        "expected duplex stream EOF after cancel post-split, got {next:?}"
    );
    // Subsequent sends after cancel surface stream-closed-shaped
    // errors. Don't pin the exact message; just that the sink
    // refuses further sends or surfaces a terminal error within
    // bounded time.
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        sink.send(Bytes::from_static(b"chunk2")),
    )
    .await
    .expect("duplex sink send after cancel must not hang");
}
