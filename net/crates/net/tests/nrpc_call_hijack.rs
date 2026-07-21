//! AV-1 item 1 (Kyra amended verdict) — end-to-end: server-fold call
//! state is bound to the AEAD-authenticated session peer `from_node`,
//! so a peer that copies a victim's origin + call_id onto a control
//! frame cannot mutate the victim's in-flight call.
//!
//! The deterministic per-control-class proofs live at the fold seam
//! (`cortex::rpc::tests::*_foreign_session_cannot_*`, driving the
//! production `apply_inbound`). This is the real-wire companion for the
//! headline class (CANCEL): a real, separately-authenticated attacker
//! node publishes a forged CANCEL carrying the victim's origin + call
//! id on its OWN session; the victim's paused call still returns its
//! own Ok response. Before AV-1 the fold keyed active calls on
//! `(origin, call_id)` alone, so the forged CANCEL would have torn the
//! victim's call down.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload,
    RpcStatus, DISPATCH_RPC_CANCEL,
};
use net::adapter::net::mesh_rpc::CallOptions;
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};
use parking_lot::Mutex as PlMutex;
use tokio::sync::Notify;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Connect `a`→`b` and accept, WITHOUT starting either node (so the
/// shared `b` can accept several peers before its receive loop runs).
async fn connect_accept(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
}

/// Captures the server-assigned `call_id`, signals `entered`, then
/// blocks until `release` — so the test can inject a forged CANCEL
/// while the call is in flight.
struct CapturingPausingHandler {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    call_id: Arc<PlMutex<Option<u64>>>,
    body: Bytes,
}

#[async_trait::async_trait]
impl RpcHandler for CapturingPausingHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        *self.call_id.lock() = Some(ctx.call_id);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: self.body.clone(),
        })
    }
}

/// A raw CANCEL frame with an attacker-chosen `origin` + `call_id`.
/// Control frames are `EventMeta ‖ RpcRouteV1` with no body.
fn forged_cancel(route: u64, origin: u64, call_id: u64) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, origin, call_id, 0);
    let mut buf = meta.to_bytes().to_vec();
    encode_rpc_route(&mut buf, route);
    Bytes::from(buf)
}

/// A forged CANCEL carrying the victim's ORIGIN is stopped by Gate-3's
/// packet-vs-payload origin bind, before it ever reaches the fold.
///
/// §T2: this test used to be named for the fold's `(from_node, origin,
/// call_id)` keying, which it never exercised — `bridge_origin_check` drops
/// the frame on `inbound.origin_hash != meta.origin_hash` first, so reverting
/// the fold key to `(origin, seq)` left it green. It now asserts the
/// origin-mismatch counter actually fired, which pins WHICH defence is doing
/// the work. The fold-keying property has its own test below.
#[tokio::test]
async fn a_forged_cancel_claiming_a_victims_origin_is_dropped_at_the_origin_bind() {
    let server = build_node().await;
    let victim = build_node().await;
    let attacker = build_node().await;
    // Accept both peers on the shared server before any receive loop
    // starts, then start all three exactly once.
    connect_accept(&victim, &server).await;
    connect_accept(&attacker, &server).await;
    victim.start();
    attacker.start();
    server.start();

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let call_id_cell: Arc<PlMutex<Option<u64>>> = Arc::new(PlMutex::new(None));
    let _serve = server
        .serve_rpc(
            "svc",
            Arc::new(CapturingPausingHandler {
                entered: entered.clone(),
                release: release.clone(),
                call_id: call_id_cell.clone(),
                body: Bytes::from_static(b"for-the-victim"),
            }),
        )
        .expect("serve");

    // For a RAW publish to reach the server it must be subscribed to
    // svc.requests from the ATTACKER's session (a directed high-level
    // call() handles its own delivery; a raw ChannelPublisher does not).
    let svc_requests = ChannelName::new("svc.requests").unwrap();
    server
        .subscribe_channel(attacker.node_id(), svc_requests.clone())
        .await
        .expect("sub attacker→server");

    // The victim issues a real call; it blocks in the paused handler
    // after the REQUEST creates in_flight[(victim_node, origin, cid)].
    let victim_call = {
        let victim = victim.clone();
        let server_id = server.node_id();
        tokio::spawn(async move {
            victim
                .call(
                    server_id,
                    "svc",
                    Bytes::from_static(b"req"),
                    CallOptions {
                        deadline: Some(Instant::now() + Duration::from_secs(8)),
                        ..Default::default()
                    },
                )
                .await
        })
    };

    // Wait until the handler is running; read the victim's real call_id.
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler entered");
    let victim_call_id = (*call_id_cell.lock()).expect("victim call_id captured by handler");
    let victim_origin = victim.origin_hash();

    // The attacker forges a CANCEL carrying the victim's origin +
    // call_id and publishes it on its OWN session. Its authenticated
    // `from_node` is the attacker, so the fold keys the lookup by
    // (attacker_node, victim_origin, call_id) and misses the victim's
    // (victim_node, victim_origin, call_id) entry.
    let route = svc_requests.hash();
    let publisher = ChannelPublisher::new(svc_requests.clone(), PublishConfig::default());
    attacker
        .publish(
            &publisher,
            forged_cancel(route, victim_origin, victim_call_id),
        )
        .await
        .expect("attacker publish");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Release the handler; the victim's call must still return its OWN
    // Ok response — never Cancelled.
    release.notify_one();

    let reply = tokio::time::timeout(Duration::from_secs(5), victim_call)
        .await
        .expect("victim call did not hang")
        .expect("victim call task")
        .expect("victim call succeeds despite the forged CANCEL — a hijack maps to Err");
    // A cancelled call surfaces as `Err`, caught by the `.expect` above;
    // a successful call returns the handler's own body.
    assert_eq!(
        reply.body.as_ref(),
        b"for-the-victim",
        "the victim must receive its own response body, not a hijacked cancellation",
    );

    // Pin the defence: the frame was refused at the ORIGIN BIND, never handed
    // to the fold. Without this the test cannot distinguish "Gate-3 dropped
    // it" from "the fold keying rejected it" — and only the former is true.
    let snap = server.rpc_metrics_snapshot();
    let svc = snap
        .services
        .iter()
        .find(|s| s.service == "svc")
        .expect("svc tracked");
    assert!(
        svc.packet_origin_mismatch_dropped_total >= 1,
        "the forged CANCEL must be dropped by the packet/payload origin bind; got {svc:?}",
    );
}

/// The fold's per-call state is keyed by the AUTHENTICATED session, so a
/// CANCEL that passes the origin bind still cannot reach another session's
/// call.
///
/// §T2 companion. The attacker sends a CANCEL under its OWN origin — packet
/// and payload origin agree, so Gate-3 admits it and it reaches
/// `apply_inbound` — carrying the VICTIM's `call_id`. The only thing that can
/// stop it now is that `InFlightCalls` keys on `(from_node, origin,
/// call_id)`: the attacker's tuple simply does not name the victim's entry.
///
/// Red-witness: reverting `cortex/rpc.rs`'s key to `(origin, seq_or_ts)`
/// still misses here (the origins differ), but reverting it to `seq_or_ts`
/// alone — the shape a call_id-only cache would have — cancels the victim.
/// The origin-mismatch counter is asserted UNCHANGED so this test provably
/// exercises the fold path and not the bind.
#[tokio::test]
async fn a_cancel_under_the_attackers_own_origin_cannot_reach_a_victims_call() {
    let server = build_node().await;
    let victim = build_node().await;
    let attacker = build_node().await;
    connect_accept(&victim, &server).await;
    connect_accept(&attacker, &server).await;
    victim.start();
    attacker.start();
    server.start();

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let call_id_cell: Arc<PlMutex<Option<u64>>> = Arc::new(PlMutex::new(None));
    let _serve = server
        .serve_rpc(
            "svc",
            Arc::new(CapturingPausingHandler {
                entered: entered.clone(),
                release: release.clone(),
                call_id: call_id_cell.clone(),
                body: Bytes::from_static(b"for-the-victim"),
            }),
        )
        .expect("serve");

    let svc_requests = ChannelName::new("svc.requests").unwrap();
    server
        .subscribe_channel(attacker.node_id(), svc_requests.clone())
        .await
        .expect("sub attacker→server");

    let victim_call = {
        let victim = victim.clone();
        let server_id = server.node_id();
        tokio::spawn(async move {
            victim
                .call(
                    server_id,
                    "svc",
                    Bytes::from_static(b"req"),
                    CallOptions {
                        deadline: Some(Instant::now() + Duration::from_secs(8)),
                        ..Default::default()
                    },
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler entered");
    let victim_call_id = (*call_id_cell.lock()).expect("victim call_id captured");

    let before = server
        .rpc_metrics_snapshot()
        .services
        .iter()
        .find(|s| s.service == "svc")
        .map(|s| s.packet_origin_mismatch_dropped_total)
        .unwrap_or(0);

    // The attacker's OWN origin — so packet and payload origin agree and the
    // frame passes the bind — but the victim's call_id.
    let route = svc_requests.hash();
    let publisher = ChannelPublisher::new(svc_requests.clone(), PublishConfig::default());
    attacker
        .publish(
            &publisher,
            forged_cancel(route, attacker.origin_hash(), victim_call_id),
        )
        .await
        .expect("attacker publish");
    tokio::time::sleep(Duration::from_millis(200)).await;

    release.notify_one();
    let reply = tokio::time::timeout(Duration::from_secs(5), victim_call)
        .await
        .expect("victim call did not hang")
        .expect("victim call task")
        .expect("victim call succeeds — the fold key is session-scoped");
    assert_eq!(
        reply.body.as_ref(),
        b"for-the-victim",
        "the victim received its own response",
    );

    let after = server
        .rpc_metrics_snapshot()
        .services
        .iter()
        .find(|s| s.service == "svc")
        .map(|s| s.packet_origin_mismatch_dropped_total)
        .unwrap_or(0);
    assert_eq!(
        after, before,
        "this frame must PASS the origin bind — otherwise the test witnesses          Gate-3 again rather than the fold's session-scoped keying",
    );
}
