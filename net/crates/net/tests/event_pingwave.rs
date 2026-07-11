//! RT-4 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): event-triggered
//! pingwaves.
//!
//! Pre-RT-4, pingwaves were emitted only from the heartbeat tick, so
//! a third party learned about a new session after up to
//! `heartbeat_interval` PER HOP. These tests park the heartbeat far
//! past the assertion window: any route that shows up inside it can
//! only have arrived via an event-triggered flood.
//!
//! Run: `cargo test --features net --test event_pingwave`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn slow_heartbeat_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        // The periodic pingwave tick sits 30 s out — far past every
        // assertion deadline below.
        .with_heartbeat_interval(Duration::from_secs(30))
        .with_session_timeout(Duration::from_secs(120))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    build_node_with(|cfg| cfg).await
}

async fn build_node_with<F>(tweak: F) -> Arc<MeshNode>
where
    F: FnOnce(MeshNodeConfig) -> MeshNodeConfig,
{
    let cfg = tweak(slow_heartbeat_config());
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Handshake two nodes (A initiator, B responder) and `start()`
/// both. `start` is idempotent, so re-handshaking an already-started
/// node is fine.
async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// The RT-4 core claim: when A joins B, a node C two hops away
/// installs a route to A within one flood — with the heartbeat tick
/// (the only pre-RT-4 pingwave source) parked 30 s in the future.
#[tokio::test]
async fn new_session_installs_multihop_route_at_flood_speed() {
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;

    // B ↔ C first; both running.
    handshake(&b, &c).await;

    // A joins B. The started node must be the initiator (a running
    // dispatch loop owns the socket, so post-start `accept` doesn't
    // see handshake packets) — B connects, A accepts. A's
    // accept-side event pingwave (origin A) goes to B, which
    // forwards it to C on the receive path.
    handshake(&b, &a).await;

    let a_id = a.node_id();
    let found = wait_until(
        || c.router().routing_table().lookup(a_id).is_some(),
        Duration::from_secs(2),
    )
    .await;
    if !found {
        // Failure triage: whose pingwaves were sent/received/
        // forwarded tells apart "never emitted" / "dropped at the
        // gate" / "not forwarded".
        for (name, n) in [("A", &a), ("B", &b), ("C", &c)] {
            let s = n.proximity_graph().stats();
            eprintln!(
                "{name}: id={:#x} addr={} sent={} recv={} fwd={} nodes={} edges={}",
                n.node_id(),
                n.local_addr(),
                s.pingwaves_sent,
                s.pingwaves_received,
                s.pingwaves_forwarded,
                s.node_count,
                s.edge_count
            );
        }
    }
    assert!(
        found,
        "C never installed a route to A — the session-open event \
         pingwave did not flood (the heartbeat tick is 30 s away, so \
         nothing else could have carried it)",
    );

    // The installed next-hop must be B — C has no direct session
    // with A, so the route can only be the pingwave-learned
    // (A via B) entry.
    let next_hop = c.router().routing_table().lookup(a_id);
    let b_addr_for_c = b.local_addr();
    assert_eq!(
        next_hop,
        Some(b_addr_for_c),
        "C's route to A must go via B (the forwarding peer)",
    );
}

/// `event_pingwave_min_gap = Duration::MAX` disables event
/// pingwaves: with the tick also parked, C must NOT learn about A.
/// Guards against the gate check being bypassed or inverted.
#[tokio::test]
async fn max_gap_disables_event_pingwaves() {
    let a = build_node_with(|cfg| cfg.with_event_pingwave_min_gap(Duration::MAX)).await;
    let b = build_node_with(|cfg| cfg.with_event_pingwave_min_gap(Duration::MAX)).await;
    let c = build_node_with(|cfg| cfg.with_event_pingwave_min_gap(Duration::MAX)).await;

    handshake(&b, &c).await;
    handshake(&b, &a).await;

    let a_id = a.node_id();
    // Negative window: generous enough that a mistakenly-emitted
    // flood would land well inside it.
    assert!(
        !wait_until(
            || c.router().routing_table().lookup(a_id).is_some(),
            Duration::from_millis(750),
        )
        .await,
        "C learned a route to A although event pingwaves are disabled \
         and the heartbeat tick hasn't fired — something emitted an \
         unexpected flood",
    );
}
