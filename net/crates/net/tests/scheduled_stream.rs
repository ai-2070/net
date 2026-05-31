//! T-0.5 — opt-in scheduled-stream routing.
//!
//! Pins that `StreamConfig.scheduled` controls whether a stream's
//! originating sends go through the router's `FairScheduler` (so bulk
//! transfers participate in per-stream weighted fairness) or take the
//! direct `socket.send_to` path (every existing caller's behavior).
//!
//! The scheduler's `total_queued()` is a cumulative enqueue counter, so
//! a scheduled send bumps it and a default send leaves it unchanged.
//!
//! Run: `cargo test --features net --test scheduled_stream`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig, StreamConfig,
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
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_stream_routes_through_scheduler_default_does_not() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;
    let b_id = node_b.node_id();

    let scheduler = node_a.router().scheduler();
    let payload = Bytes::from(vec![7u8; 1024]);

    // A default (unscheduled) stream: send goes straight to the socket,
    // the scheduler never sees it.
    let before_default = scheduler.total_queued();
    let direct = node_a
        .open_stream(b_id, 0x4000_0000_0000_0001, StreamConfig::new())
        .expect("open direct stream");
    node_a
        .send_on_stream(&direct, std::slice::from_ref(&payload))
        .await
        .expect("direct send");
    let after_default = scheduler.total_queued();
    assert_eq!(
        after_default, before_default,
        "a default stream's send must NOT touch the scheduler",
    );

    // A scheduled stream: the same send is enqueued on the scheduler,
    // so the cumulative enqueue counter advances.
    let before_sched = scheduler.total_queued();
    let scheduled = node_a
        .open_stream(
            b_id,
            0x4000_0000_0000_0002,
            StreamConfig::new()
                .with_scheduled(true)
                .with_fairness_weight(4),
        )
        .expect("open scheduled stream");
    node_a
        .send_on_stream(&scheduled, std::slice::from_ref(&payload))
        .await
        .expect("scheduled send");
    let after_sched = scheduler.total_queued();
    assert!(
        after_sched > before_sched,
        "a scheduled stream's send must route through the scheduler \
         (total_queued {before_sched} -> {after_sched})",
    );
}
