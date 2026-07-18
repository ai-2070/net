//! OA2-E1 (Kyra E1 audit) — a denied/untrusted frame must not disturb
//! a legitimate call's response routing (delivery-integrity witness).
//!
//! The origin→node response-route cache is keyed on the wire
//! `origin_hash`. The unit witness
//! `mesh_rpc::origin_cache_tests::response_route_trust_requires_authenticated_direct_origin`
//! pins the load-bearing logic: a claim is cached ONLY for an initial
//! REQUEST whose wire origin matches the AEAD-authenticated
//! `from_node` peer's OWN origin — so a malicious node forging a
//! victim's origin, a control frame, or an unpinned/loopback peer
//! never establishes a destination.
//!
//! This END-TO-END witness is the delivery-integrity companion: while
//! the victim's real call is paused inside the handler, a DIFFERENT
//! peer injects a concurrent cross-service frame on its own session;
//! the handler resumes and the victim's `call()` still returns the
//! correct response. (A raw `publish` stamps the publisher's own
//! authenticated origin on the wire header, so it cannot forge the
//! victim's — the forged-origin poison is exercised deterministically
//! by the unit witness above.)

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RpcContext, RpcHandler, RpcHandlerError, RpcRequestPayload,
    RpcResponsePayload, RpcStatus, DISPATCH_RPC_REQUEST, EVENT_META_SIZE, RPC_ROUTE_V1_SIZE,
};
use net::adapter::net::mesh_rpc::CallOptions;
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};
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

/// Enters, signals `entered`, waits for `release`, then returns
/// `body` — so a test can inject frames while the call is in flight.
struct PausingHandler {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    body: Bytes,
}

#[async_trait::async_trait]
impl RpcHandler for PausingHandler {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: self.body.clone(),
        })
    }
}

/// A raw initial-REQUEST frame with an attacker-chosen `origin`.
fn forged_request(route: u64, service: &str, origin: u64, body: &[u8]) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin, 999, 0);
    let req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    };
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, route);
    req.encode_into(&mut buf);
    Bytes::from(buf)
}

/// A concurrent cross-service frame from a DIFFERENT peer, injected
/// while the victim's call is paused in the handler, must NOT disturb
/// the victim's response delivery.
#[tokio::test]
async fn concurrent_injection_does_not_disturb_a_victims_response() {
    let server = build_node().await;
    let victim = build_node().await;
    let attacker = build_node().await;
    // Accept both peers on the shared server before any node's receive
    // loop starts, then start all three exactly once.
    connect_accept(&victim, &server).await;
    connect_accept(&attacker, &server).await;
    victim.start();
    attacker.start();
    server.start();

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _serve = server
        .serve_rpc(
            "svc",
            Arc::new(PausingHandler {
                entered: entered.clone(),
                release: release.clone(),
                body: Bytes::from_static(b"for-the-victim"),
            }),
        )
        .expect("serve");

    // The attacker also needs the server subscribed to svc.requests
    // from ITS session so a raw publish is delivered.
    let svc_requests = ChannelName::new("svc.requests").unwrap();
    server
        .subscribe_channel(attacker.node_id(), svc_requests.clone())
        .await
        .expect("sub attacker→server");

    // The victim issues a real call; it will block in the paused
    // handler after its REQUEST caches victim_origin → victim_node.
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

    // Wait until the handler is running (REQUEST cached, paused).
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("handler entered");

    // The attacker injects a CROSS-SERVICE frame on its own session
    // while the victim's handler is paused. The cache only ever binds
    // the attacker's own authenticated origin (never the victim's), so
    // the victim's response route is untouched.
    let victim_origin = victim.origin_hash();
    let route = svc_requests.hash();
    let publisher = ChannelPublisher::new(svc_requests.clone(), PublishConfig::default());
    attacker
        .publish(
            &publisher,
            forged_request(route, "other-service", victim_origin, b"attack"),
        )
        .await
        .expect("attacker publish");
    // Give the forged frame time to be processed (and dropped).
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Release the handler; the response must reach the VICTIM.
    release.notify_one();

    let reply = tokio::time::timeout(Duration::from_secs(5), victim_call)
        .await
        .expect("victim call did not hang")
        .expect("victim call task")
        .expect("victim call succeeds despite the forged injection");
    assert_eq!(
        reply.body.as_ref(),
        b"for-the-victim",
        "the victim's response must not be redirected by a forged frame",
    );
}
