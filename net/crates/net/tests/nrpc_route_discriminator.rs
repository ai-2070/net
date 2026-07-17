//! OA2-E0.2 witnesses — the RpcRouteV1 canonical discriminator on
//! nRPC frames drives mesh ingress to select EXACTLY ONE registered
//! canonical dispatcher, instead of fanning a bucket-colliding frame
//! to every candidate.
//!
//! Two canonical `ChannelHash` values that collide in the wire
//! `u16` bucket register two dispatchers. A real inbound nRPC frame
//! (`EventMeta ‖ RpcRouteV1 ‖ payload`), published through the real
//! network path, must reach only the dispatcher named by its route:
//!
//! - REQUEST / CANCEL / CHUNK / GRANT each single-select (2/3/4);
//! - a route absent from the bucket drops (6);
//! - a malformed/short discriminator drops (7);
//! - the collision test IS the red-witness (9): under the removed
//!   fan-out both dispatchers would fire; here exactly one does.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::channel::wire_channel_hash;
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RpcInboundDispatcher, RpcInboundEvent, DISPATCH_RPC_CANCEL,
    DISPATCH_RPC_REQUEST, DISPATCH_RPC_REQUEST_CHUNK, DISPATCH_RPC_STREAM_GRANT, EVENT_META_SIZE,
    RPC_ROUTE_V1_SIZE,
};
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};

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
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

/// Two channel names that share a wire `u16` bucket but differ in
/// canonical hash.
fn colliding_channel_names() -> (ChannelName, ChannelName) {
    let mut seen = std::collections::HashMap::<u16, String>::new();
    for i in 0..500_000u64 {
        let name = format!("rpc/route/coll/{i}");
        let wire = wire_channel_hash(&name);
        if let Some(prev) = seen.get(&wire) {
            return (
                ChannelName::new(prev).unwrap(),
                ChannelName::new(&name).unwrap(),
            );
        }
        seen.insert(wire, name);
    }
    panic!("no wire-bucket collision found");
}

/// Build a raw nRPC frame `EventMeta(dispatch) ‖ RpcRouteV1(route) ‖
/// tail`, matching what a real sender emits.
fn rpc_frame(dispatch: u8, route: u64, tail: &[u8]) -> Bytes {
    let meta = EventMeta::new(dispatch, 0, 0xC0FFEE, 1, 0);
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + tail.len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, route);
    buf.extend_from_slice(tail);
    Bytes::from(buf)
}

/// A dispatcher that records every canonical hash it was invoked
/// with (so a test can assert which registration fired).
fn recording_dispatcher(sink: Arc<parking_lot::Mutex<Vec<u64>>>) -> RpcInboundDispatcher {
    Arc::new(move |ev: RpcInboundEvent| sink.lock().push(ev.channel_hash))
}

/// Harness: node B registers two RPC dispatchers for two
/// bucket-colliding canonical channels and subscribes to both, so A
/// can deliver a frame to that wire bucket.
struct Collision {
    a: Arc<MeshNode>,
    /// Held to keep node B (which owns the two registered
    /// dispatchers) alive for the test's duration; not read directly.
    #[allow(dead_code)]
    b: Arc<MeshNode>,
    ch1: ChannelName,
    ch2: ChannelName,
    fired1: Arc<parking_lot::Mutex<Vec<u64>>>,
    fired2: Arc<parking_lot::Mutex<Vec<u64>>>,
}

impl Collision {
    async fn new() -> Self {
        let a = build_node().await;
        let b = build_node().await;
        handshake(&a, &b).await;
        let (ch1, ch2) = colliding_channel_names();
        assert_eq!(ch1.wire_hash(), ch2.wire_hash());
        assert_ne!(ch1.hash(), ch2.hash());
        b.subscribe_channel(a.node_id(), ch1.clone())
            .await
            .expect("sub ch1");
        b.subscribe_channel(a.node_id(), ch2.clone())
            .await
            .expect("sub ch2");
        let fired1 = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let fired2 = Arc::new(parking_lot::Mutex::new(Vec::new()));
        assert!(b
            .register_rpc_inbound(ch1.hash(), recording_dispatcher(fired1.clone()))
            .is_some());
        assert!(b
            .register_rpc_inbound(ch2.hash(), recording_dispatcher(fired2.clone()))
            .is_some());
        Self {
            a,
            b,
            ch1,
            ch2,
            fired1,
            fired2,
        }
    }

    /// Publish `frame` from A onto ch1's wire bucket (B is
    /// subscribed), returning after B has had a chance to dispatch.
    async fn deliver(&self, frame: Bytes) {
        let publisher = ChannelPublisher::new(self.ch1.clone(), PublishConfig::default());
        self.a.publish(&publisher, frame).await.expect("publish");
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

/// Witnesses 1/2/9: a REQUEST frame whose route names ch1 reaches
/// ONLY ch1's dispatcher, never the bucket-colliding ch2 — the
/// exact property the removed fan-out violated.
#[tokio::test]
async fn request_route_selects_exactly_one_colliding_dispatcher() {
    let c = Collision::new().await;
    c.deliver(rpc_frame(DISPATCH_RPC_REQUEST, c.ch1.hash(), b"body"))
        .await;
    assert!(
        wait_until(
            || c.fired1.lock().contains(&c.ch1.hash()),
            Duration::from_secs(2)
        )
        .await,
        "the routed dispatcher (ch1) must fire",
    );
    assert!(
        c.fired2.lock().is_empty(),
        "the bucket-colliding sibling (ch2) must NOT fire — no fan-out",
    );

    // The mirror: a route naming ch2 fires only ch2.
    c.fired1.lock().clear();
    c.deliver(rpc_frame(DISPATCH_RPC_REQUEST, c.ch2.hash(), b"body2"))
        .await;
    assert!(
        wait_until(
            || c.fired2.lock().contains(&c.ch2.hash()),
            Duration::from_secs(2)
        )
        .await,
        "the routed dispatcher (ch2) must fire",
    );
    assert!(
        c.fired1.lock().is_empty(),
        "ch1 must not fire for a ch2-routed frame"
    );
}

/// Witnesses 3/4: control frames (CANCEL, CHUNK, STREAM_GRANT)
/// likewise mutate ONLY the dispatcher named by the canonical
/// route — a colliding sibling never sees them.
#[tokio::test]
async fn control_frames_route_to_exactly_one_dispatcher() {
    for dispatch in [
        DISPATCH_RPC_CANCEL,
        DISPATCH_RPC_REQUEST_CHUNK,
        DISPATCH_RPC_STREAM_GRANT,
    ] {
        let c = Collision::new().await;
        c.deliver(rpc_frame(dispatch, c.ch1.hash(), &[0u8; 4]))
            .await;
        assert!(
            wait_until(|| !c.fired1.lock().is_empty(), Duration::from_secs(2)).await,
            "dispatch {dispatch:#x}: routed dispatcher must fire",
        );
        assert!(
            c.fired2.lock().is_empty(),
            "dispatch {dispatch:#x}: colliding sibling must not fire",
        );
    }
}

/// Witness 6: a route naming a canonical hash ABSENT from the
/// packet's wire bucket is dropped — neither dispatcher fires, no
/// competing response.
#[tokio::test]
async fn route_absent_from_bucket_is_dropped() {
    let c = Collision::new().await;
    let foreign = c.ch1.hash() ^ 0xDEAD_BEEF_0000_0000;
    c.deliver(rpc_frame(DISPATCH_RPC_REQUEST, foreign, b"body"))
        .await;
    // Give it real time to (not) fire.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        c.fired1.lock().is_empty(),
        "ch1 must not fire for a foreign route"
    );
    assert!(
        c.fired2.lock().is_empty(),
        "ch2 must not fire for a foreign route"
    );
}

/// Witness 7: an nRPC-typed frame too short to carry the route
/// discriminator is dropped (never delivered under a truncated
/// read).
#[tokio::test]
async fn malformed_route_is_dropped() {
    let c = Collision::new().await;
    // An RPC REQUEST meta with NO route section (frame ends right
    // after the 24-byte meta).
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 0xC0FFEE, 1, 0);
    let frame = Bytes::from(meta.to_bytes().to_vec());
    assert_eq!(frame.len(), EVENT_META_SIZE);
    c.deliver(frame).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        c.fired1.lock().is_empty() && c.fired2.lock().is_empty(),
        "a route-less RPC frame must be dropped, not delivered",
    );
}
