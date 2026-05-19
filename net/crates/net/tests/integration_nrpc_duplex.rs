//! End-to-end nRPC duplex integration tests against real
//! `MeshNode`s. Composes Phase B's client-streaming (request
//! direction) with the existing server-streaming response path
//! (response direction) — both run interleaved over one
//! `call_duplex` / `serve_rpc_duplex` pair.
//!
//! Coverage:
//! 1. Bidirectional echo — caller streams N requests, server
//!    emits one response per request + a final summary; caller
//!    observes interleaved responses while still sending.
//! 2. `finish_sending` keeps the response stream open — caller
//!    closes upload; server keeps emitting; caller drains until
//!    terminal End.
//! 3. Server terminates first; caller's subsequent send surfaces
//!    a clean error.
//! 4. `into_split` lets sink + stream live in independent tokio
//!    tasks; CANCEL only fires when BOTH halves drop.
//! 5. CANCEL from either side closes both halves cleanly.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net::adapter::net::cortex::{
    RequestStream, RpcDuplexHandler, RpcHandlerError, RpcResponseSink, RpcStreamingContext,
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

/// Echo handler: for every inbound request body, emits one
/// response body of the form `"echo:<body>"`. On EOS, emits a
/// summary chunk `"total:<count>"` and returns Ok.
struct EchoDuplexHandler;

#[async_trait::async_trait]
impl RpcDuplexHandler for EchoDuplexHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        let mut count: u64 = 0;
        while let Some(req) = requests.next().await {
            let mut body = b"echo:".to_vec();
            body.extend_from_slice(&req);
            responses.send(body);
            count += 1;
        }
        responses.send(format!("total:{count}").into_bytes());
        Ok(())
    }
}

/// Emits N responses immediately, returns Ok. Used to verify
/// that `finish_sending` doesn't cut off response delivery.
struct EmitNThenWaitForEosHandler {
    n: usize,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for EmitNThenWaitForEosHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        for i in 0..self.n {
            responses.send(format!("pre-{i}").into_bytes());
        }
        // Wait for caller's EOS, then return.
        while requests.next().await.is_some() {}
        Ok(())
    }
}

/// Emits one response, then returns immediately — terminal
/// RESPONSE flies before the caller has finished its sends.
struct ServerTerminatesFirstHandler;

#[async_trait::async_trait]
impl RpcDuplexHandler for ServerTerminatesFirstHandler {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        _requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        responses.send(Bytes::from_static(b"only"));
        // Return immediately — fold emits terminal End; further
        // caller sends arrive at a closed-down call.
        Ok(())
    }
}

/// Loops forever observing cancellation. Used to verify the
/// CANCEL-from-caller path.
struct ForeverHandler {
    observed_cancel: Arc<AtomicBool>,
    consumed: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for ForeverHandler {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        loop {
            tokio::select! {
                _ = ctx.cancellation.cancelled() => {
                    self.observed_cancel.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                maybe = requests.next() => {
                    match maybe {
                        Some(body) => {
                            self.consumed.fetch_add(1, Ordering::SeqCst);
                            responses.send(body);
                        }
                        None => {
                            // EOS without cancel — still wait for
                            // cancellation so the test can drive
                            // the assertion deterministically.
                            ctx.cancellation.cancelled().await;
                            self.observed_cancel.store(true, Ordering::SeqCst);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Tests.
// ============================================================================

/// 1/5 — Bidirectional echo. Caller streams 5 requests, server
/// emits one response per request + a final "total:5" summary
/// before EOS. Caller collects 6 chunks then sees clean EOF.
#[tokio::test]
async fn duplex_interleaves_send_and_recv() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_duplex("echo", Arc::new(EchoDuplexHandler))
        .expect("serve_rpc_duplex");

    let mut call = caller
        .call_duplex(server.node_id(), "echo", CallOptions::default())
        .await
        .expect("call_duplex");

    // Send 5 items, interleaved with response polling. The
    // server emits one Resp per Req, so by the time we finish
    // sending 5 we should have collected ~5 responses (plus the
    // final "total:5" summary after our EOS).
    for i in 0..5u8 {
        call.send(Bytes::copy_from_slice(&[b'a' + i]))
            .await
            .expect("send");
    }
    call.finish_sending().await.expect("finish_sending");

    let mut collected: Vec<String> = Vec::new();
    while let Some(item) = call.next().await {
        let chunk = item.expect("chunk must be Ok");
        collected.push(String::from_utf8(chunk.to_vec()).unwrap());
    }
    assert_eq!(collected.len(), 6, "5 echoes + 1 summary");
    for (i, label) in (0..5u8).zip(["a", "b", "c", "d", "e"]) {
        assert_eq!(collected[i as usize], format!("echo:{label}"));
    }
    assert_eq!(collected[5], "total:5");
}

/// 2/5 — `finish_sending` closes the upload but keeps the
/// response stream alive. Server emits N=3 responses BEFORE the
/// caller finishes sending; caller calls finish_sending without
/// any sends, then drains all 3 responses + sees clean EOF.
#[tokio::test]
async fn duplex_finish_sending_keeps_response_stream_open() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_duplex("emit_n", Arc::new(EmitNThenWaitForEosHandler { n: 3 }))
        .expect("serve_rpc_duplex");

    let mut call = caller
        .call_duplex(server.node_id(), "emit_n", CallOptions::default())
        .await
        .expect("call_duplex");
    // finish_sending immediately — degenerate "no upload" path.
    call.finish_sending().await.expect("finish_sending");

    let mut collected: Vec<String> = Vec::new();
    while let Some(item) = call.next().await {
        let chunk = item.expect("Ok chunk");
        collected.push(String::from_utf8(chunk.to_vec()).unwrap());
    }
    assert_eq!(collected, vec!["pre-0", "pre-1", "pre-2"]);
}

/// 3/5 — Server emits terminal RESPONSE before the caller is
/// done. Subsequent sends fail with a transport error (the
/// server's reply path is torn down once the terminal frame is
/// emitted; further REQUEST_CHUNK frames arrive at a closed
/// entry on the server side, are silently dropped, but the
/// caller's send / publish_to_peer either succeeds locally or
/// surfaces the transport-level failure). Either way the
/// response stream's terminal End is observed.
#[tokio::test]
async fn duplex_server_terminates_first() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_duplex("term_first", Arc::new(ServerTerminatesFirstHandler))
        .expect("serve_rpc_duplex");

    let mut call = caller
        .call_duplex(server.node_id(), "term_first", CallOptions::default())
        .await
        .expect("call_duplex");

    // Send one item so the initial REQUEST flies. The server
    // emits one response + terminates. Drain the response stream
    // until None.
    call.send(Bytes::from_static(b"hello"))
        .await
        .expect("send hello");
    let mut collected: Vec<Bytes> = Vec::new();
    while let Some(item) = call.next().await {
        match item {
            Ok(body) => collected.push(body),
            Err(_) => break,
        }
    }
    // Server emitted exactly "only" + terminal End.
    assert_eq!(collected.len(), 1);
    assert_eq!(&collected[0][..], b"only");
}

/// 4/5 — `into_split` lets the sink + stream live in separate
/// tokio tasks. The sink task encodes 5 requests then
/// finish_sending; the stream task collects 6 responses (5
/// echoes + 1 summary) then EOF. CANCEL only fires when BOTH
/// halves drop — here neither half drops early (both run to
/// clean completion), so the server-side cancellation token
/// stays unfired.
#[tokio::test]
async fn duplex_into_split_lets_halves_run_independently() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let _serve = server
        .serve_rpc_duplex("echo_split", Arc::new(EchoDuplexHandler))
        .expect("serve_rpc_duplex");

    let call = caller
        .call_duplex(server.node_id(), "echo_split", CallOptions::default())
        .await
        .expect("call_duplex");
    let (mut sink, mut stream) = call.into_split();

    let sender = tokio::spawn(async move {
        for i in 0..5u8 {
            sink.send(Bytes::copy_from_slice(&[b'A' + i]))
                .await
                .expect("send");
        }
        sink.finish_sending().await.expect("finish_sending");
    });

    let receiver = tokio::spawn(async move {
        let mut count = 0;
        while let Some(item) = stream.next().await {
            std::hint::black_box(item.expect("Ok"));
            count += 1;
        }
        count
    });

    sender.await.expect("sender task");
    let count = receiver.await.expect("receiver task");
    assert_eq!(count, 6, "5 echoes + 1 summary");
}

/// 5/5 — CANCEL from the caller closes both halves. Caller
/// streams 2 chunks then drops the handle without
/// finish_sending. Server's ForeverHandler observes
/// `ctx.cancellation` firing and returns; terminal RESPONSE is
/// `Cancelled` (CANCEL-wins ordering in the fold).
#[tokio::test]
async fn duplex_cancel_from_caller_closes_both_halves() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let observed = Arc::new(AtomicBool::new(false));
    let consumed = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_duplex(
            "forever",
            Arc::new(ForeverHandler {
                observed_cancel: observed.clone(),
                consumed: consumed.clone(),
            }),
        )
        .expect("serve_rpc_duplex");

    let mut call = caller
        .call_duplex(server.node_id(), "forever", CallOptions::default())
        .await
        .expect("call_duplex");
    call.send(Bytes::from_static(b"first"))
        .await
        .expect("send 1");
    call.send(Bytes::from_static(b"second"))
        .await
        .expect("send 2");
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

/// `into_split` CANCEL semantics: dropping ONE half (sink OR
/// stream) must NOT fire CANCEL on the wire — the surviving half
/// keeps the call alive. The server only observes CANCEL after
/// BOTH halves have dropped without a clean close.
#[tokio::test]
async fn duplex_into_split_one_half_drop_does_not_cancel() {
    let server = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server).await;

    let observed = Arc::new(AtomicBool::new(false));
    let consumed = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_duplex(
            "no_cancel_on_one_drop",
            Arc::new(ForeverHandler {
                observed_cancel: observed.clone(),
                consumed: consumed.clone(),
            }),
        )
        .expect("serve_rpc_duplex");

    let call = caller
        .call_duplex(
            server.node_id(),
            "no_cancel_on_one_drop",
            CallOptions::default(),
        )
        .await
        .expect("call_duplex");
    let (mut sink, stream) = call.into_split();
    // Publish the initial REQUEST so the server registers the
    // call — without this the server doesn't know about the call
    // and "no CANCEL observed" would be a vacuous pass.
    sink.send(Bytes::from_static(b"keepalive"))
        .await
        .expect("send");

    // Drop the sink half ONLY. The stream stays alive — Arc
    // refcount is still > 0 inside DuplexInner.
    drop(sink);

    // Wait long enough that, if CANCEL were going to fire, it
    // would have arrived by now (the server publishes
    // cancellation on the receive path very quickly).
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !observed.load(Ordering::SeqCst),
        "server must NOT observe CANCEL while the stream half is alive",
    );

    // Now drop the stream too — both halves gone → CANCEL fires.
    drop(stream);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !observed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        observed.load(Ordering::SeqCst),
        "server must observe CANCEL once BOTH halves drop",
    );
}

/// Regression (cubic-dev-ai bot P2): mirror of the client-stream
/// guard for duplex. `Some(0)` would deadlock the upload sink the
/// same way (initial REQUEST is lazy → server never sees the call
/// → no GRANT ever lands); reject before any wire traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_duplex_rejects_zero_request_window() {
    let caller = build_node().await;
    let target = 0xC0DE_u64;
    let opts = CallOptions {
        request_window_initial: Some(0),
        ..CallOptions::default()
    };
    let err = match caller.call_duplex(target, "anything", opts).await {
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
}
