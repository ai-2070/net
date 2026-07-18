//! OA2-E0/E1 (Kyra re-review) — the shared callee capability gate on
//! ALL four serve shapes, and denial routing that a forged origin
//! cannot weaponize.
//!
//! NC1: client-streaming and duplex used to admit inbound REQUESTs to
//! the fold WITHOUT running `may_execute` — transport-authenticated
//! but not capability-authorized. Both now run the same shared
//! preflight as unary/response-streaming, so an unauthorized caller is
//! denied before the handler and no fold/handler state is created.
//!
//! NC2: the terminal `CapabilityDenied` is unicast ONLY to the
//! AEAD-authenticated session peer (`from_node`), never fanned out to
//! the wire-claimed origin's subscriber roster — so a malicious peer
//! that forged a victim's origin cannot reflect a denial into the
//! victim's reply channel.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::cortex::{
    RequestStream, RpcClientStreamingHandler, RpcDuplexHandler, RpcHandlerError,
    RpcInboundDispatcher, RpcInboundEvent, RpcResponsePayload, RpcResponseSink, RpcStatus,
    RpcStreamingContext,
};
use net::adapter::net::mesh_rpc::CallOptions;
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};
use parking_lot::Mutex as PlMutex;

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

/// Fold a restrictive self-announcement into `server` (deny everyone
/// but a bogus node) and a permissive one into `caller` (so its own
/// index doesn't pre-empt the call) for `nrpc:<service>`.
fn arrange_server_denies(server: &Arc<MeshNode>, caller: &Arc<MeshNode>, service: &str) {
    let tag = format!("nrpc:{service}");
    let permissive = CapabilityAnnouncement::new(
        server.node_id(),
        server.entity_id().clone(),
        50,
        CapabilitySet::new().add_tag(&tag),
    );
    caller.test_inject_capability_announcement(permissive);
    let mut restrictive = CapabilityAnnouncement::new(
        server.node_id(),
        server.entity_id().clone(),
        100,
        CapabilitySet::new().add_tag(&tag),
    );
    restrictive.allowed_nodes = vec![0xDEAD_BEEF_BAAD_F00D];
    server.test_inject_capability_announcement(restrictive);
}

fn assert_gated(server: &Arc<MeshNode>, service: &str) {
    let snap = server.rpc_metrics_snapshot();
    let svc = snap
        .services
        .iter()
        .find(|s| s.service == service)
        .unwrap_or_else(|| panic!("{service} tracked in registry"));
    assert!(
        svc.capability_denied_total >= 1,
        "{service}: bridge must bump capability_denied_total; got {svc:?}",
    );
    assert_eq!(
        svc.handler_invocations_total, 0,
        "{service}: handler must not run on a denied call",
    );
}

struct CountingClientStream(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl RpcClientStreamingHandler for CountingClientStream {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        while requests.next().await.is_some() {}
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

struct CountingDuplex(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl RpcDuplexHandler for CountingDuplex {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        while let Some(r) = requests.next().await {
            responses.send(r.to_vec());
        }
        Ok(())
    }
}

/// NC1 — an unauthorized caller to a CLIENT-STREAMING service is
/// gated at the bridge: the handler never runs and the denial metric
/// bumps.
#[tokio::test]
async fn client_streaming_denies_unauthorized_caller() {
    let server = build_node().await;
    let caller = build_node().await;
    connect_accept(&caller, &server).await;
    caller.start();
    server.start();

    let hits = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_client_stream("cs", Arc::new(CountingClientStream(hits.clone())))
        .expect("serve");
    arrange_server_denies(&server, &caller, "cs");

    if let Ok(mut call) = caller
        .call_client_stream(server.node_id(), "cs", CallOptions::default())
        .await
    {
        let _ = call.send(Bytes::from_static(b"x")).await;
        let _ = tokio::time::timeout(Duration::from_secs(3), call.finish()).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(hits.load(Ordering::Relaxed), 0, "handler must not run");
    assert_gated(&server, "cs");
}

/// NC1 — same for a DUPLEX service.
#[tokio::test]
async fn duplex_denies_unauthorized_caller() {
    let server = build_node().await;
    let caller = build_node().await;
    connect_accept(&caller, &server).await;
    caller.start();
    server.start();

    let hits = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc_duplex("dx", Arc::new(CountingDuplex(hits.clone())))
        .expect("serve");
    arrange_server_denies(&server, &caller, "dx");

    if let Ok(mut call) = caller
        .call_duplex(server.node_id(), "dx", CallOptions::default())
        .await
    {
        let _ = call.send(Bytes::from_static(b"x")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(hits.load(Ordering::Relaxed), 0, "handler must not run");
    assert_gated(&server, "dx");
}

struct RejectAllHandler;
#[async_trait::async_trait]
impl net::adapter::net::cortex::RpcHandler for RejectAllHandler {
    async fn call(
        &self,
        ctx: net::adapter::net::cortex::RpcContext,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// NC2 — the terminal denial is UNICAST to the authenticated session
/// peer, never fanned out to the claimed origin's reply-channel
/// roster. A bystander node subscribed to `svc.replies.<caller_origin>`
/// receives NOTHING when the caller (on a different session) is
/// denied — so a forged denial cannot be reflected onto another node.
#[tokio::test]
async fn denial_is_not_fanned_out_to_the_reply_roster() {
    let server = build_node().await;
    let caller = build_node().await;
    let bystander = build_node().await;
    connect_accept(&caller, &server).await;
    connect_accept(&bystander, &server).await;
    caller.start();
    bystander.start();
    server.start();

    let _serve = server
        .serve_rpc("svc", Arc::new(RejectAllHandler))
        .expect("serve");
    arrange_server_denies(&server, &caller, "svc");

    // The bystander subscribes to the CALLER's reply channel and
    // records anything delivered on it.
    let caller_origin = caller.origin_hash();
    let reply_channel = ChannelName::new(&format!("svc.replies.{caller_origin:016x}")).unwrap();
    let seen: Arc<PlMutex<Vec<RpcInboundEvent>>> = Arc::new(PlMutex::new(Vec::new()));
    let seen_disp = seen.clone();
    let disp: RpcInboundDispatcher = Arc::new(move |ev| seen_disp.lock().push(ev));
    assert!(bystander
        .register_rpc_inbound(reply_channel.hash(), disp)
        .is_some());
    bystander
        .subscribe_channel(server.node_id(), reply_channel.clone())
        .await
        .expect("bystander subscribes to caller's reply channel");
    // Keep the publisher handle so the subscription is live.
    let _pub = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());

    // The caller (denied) issues a directed call — it is gated, and
    // the denial must reach ONLY the caller's own node.
    let err = caller
        .call(
            server.node_id(),
            "svc",
            Bytes::from_static(b"x"),
            CallOptions {
                deadline: Some(std::time::Instant::now() + Duration::from_secs(3)),
                ..Default::default()
            },
        )
        .await
        .expect_err("gate denies");
    assert!(
        matches!(
            err,
            net::adapter::net::mesh_rpc::RpcError::CapabilityDenied { .. }
        ),
        "the caller itself must receive the denial, got {err:?}",
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        seen.lock().is_empty(),
        "the bystander must NOT receive the denial — it was not fanned out to the roster",
    );
}
