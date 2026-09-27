// SPDX-License-Identifier: MIT OR Apache-2.0
//! A peer that sends no heartbeats and no pingwaves — a browser leaf —
//! stays alive and routable for as long as its authenticated traffic
//! arrives (`docs/internal/misc/NET_ISSUE_PEERS_EXPIRE.md`).
//!
//! Before the fix, an anchor declared every such peer failed, and let
//! its direct route to it age out, `3 × session_timeout` after the
//! session came up, however much the peer was talking, and relayed
//! traffic to it was dropped. A native node with a heartbeat interval
//! longer than the test stands in for the leaf: its heartbeat loop
//! sleeps a full interval before its first heartbeat or pingwave, so
//! during the test it sends only what the test sends.
//!
//! Real UDP sessions between real nodes; nothing mocked.
use super::*;

/// The anchor side: a short `session_timeout`, so both expiries (the
/// failure detector's `timeout × miss_threshold` and `max_route_age`,
/// each `3 × session_timeout`) land at 1.5 s.
async fn anchor() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x42; 32])
                .with_heartbeat_interval(Duration::from_millis(100))
                .with_session_timeout(Duration::from_millis(500)),
        )
        .await
        .unwrap(),
    )
}

/// The leaf stand-in: no heartbeat or pingwave inside the test's
/// window.
async fn quiet_peer() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x42; 32])
                .with_heartbeat_interval(Duration::from_secs(60))
                .with_session_timeout(Duration::from_secs(300)),
        )
        .await
        .unwrap(),
    )
}

async fn connect(client: &Arc<MeshNode>, server: &Arc<MeshNode>) {
    let accept = {
        let server = server.clone();
        let client_id = client.node_id();
        tokio::spawn(async move { server.accept(client_id).await.unwrap() })
    };
    client
        .connect(server.local_addr(), server.public_key(), server.node_id())
        .await
        .unwrap();
    accept.await.unwrap();
}

/// Well past both 1.5 s expiries.
const WINDOW: Duration = Duration::from_millis(2_500);

fn reliable() -> StreamConfig {
    let mut config = StreamConfig::new();
    config.reliability = super::super::stream::Reliability::Reliable;
    config
}

#[tokio::test]
async fn a_heartbeatless_peer_that_keeps_talking_stays_healthy_and_routable() {
    let anchor = anchor().await;
    let leaf = quiet_peer().await;
    connect(&leaf, &anchor).await;
    anchor.start();
    leaf.start();

    let stream_id = net_wire::channel::name::stream_id_from_label("leaf-liveness-test");
    let stream = leaf
        .open_stream(anchor.node_id(), stream_id, reliable())
        .unwrap();
    let started = std::time::Instant::now();
    let mut sent = 0u32;
    while started.elapsed() < WINDOW {
        leaf.send_on_stream(&stream, &[Bytes::from(format!("tick-{sent}"))])
            .await
            .unwrap();
        sent += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(
        anchor.failure_detector().status(leaf.node_id()),
        NodeStatus::Healthy,
        "a peer whose authenticated traffic keeps arriving must not be declared failed"
    );
    assert!(
        anchor
            .router()
            .routing_table()
            .lookup(leaf.node_id())
            .is_some(),
        "its direct route must still be effective, or relayed traffic to it is dropped"
    );
}

/// The control: the same pair with no traffic reaches both expiries,
/// so the test above is not passing because nothing expires.
#[tokio::test]
async fn a_heartbeatless_peer_that_goes_quiet_still_expires() {
    let anchor = anchor().await;
    let leaf = quiet_peer().await;
    connect(&leaf, &anchor).await;
    anchor.start();
    leaf.start();

    tokio::time::sleep(WINDOW).await;

    assert_ne!(
        anchor.failure_detector().status(leaf.node_id()),
        NodeStatus::Healthy,
        "a silent peer must still be declared failed"
    );
    assert!(
        anchor
            .router()
            .routing_table()
            .lookup(leaf.node_id())
            .is_none(),
        "a silent peer's route must still age out"
    );
}
