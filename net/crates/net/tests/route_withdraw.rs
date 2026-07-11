//! RT-5 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): poison-reverse route
//! withdrawals (`SUBPROTOCOL_ROUTE_WITHDRAW`).
//!
//! Topology: C ↔ B ↔ A. C learns `(A via B)` from the RT-4
//! session-open pingwave flood. A is then killed. B's failure
//! detector (tight timeouts) marks A Failed and floods a
//! withdrawal; C must drop its route within that flood.
//!
//! The timing trick that makes causality provable: C's
//! `session_timeout` is 10 s, so its `sweep_stale` age-out
//! (3 × session_timeout = 30 s) cannot fire inside the test window.
//! Any route C loses in-window was withdrawn, not swept. B keeps a
//! 1 s session timeout so it detects A's death in ~2 s.
//!
//! Run: `cargo test --features net --test route_withdraw`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn base_config() -> MeshNodeConfig {
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

/// Detector-tight config for the node expected to notice the death:
/// 1 s of silence + 3 misses at the 200 ms tick ≈ 2 s to Failed.
fn detector_config() -> MeshNodeConfig {
    base_config().with_session_timeout(Duration::from_secs(1))
}

async fn build(cfg: MeshNodeConfig) -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Initiator must be the already-started node (see event_pingwave.rs).
async fn handshake(initiator: &Arc<MeshNode>, acceptor: &Arc<MeshNode>) {
    let i_id = initiator.node_id();
    let a_id = acceptor.node_id();
    let a_pub = *acceptor.public_key();
    let a_addr = acceptor.local_addr();

    let acc = acceptor.clone();
    let accept = tokio::spawn(async move { acc.accept(i_id).await });
    initiator
        .connect(a_addr, &a_pub, a_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    initiator.start();
    acceptor.start();
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

/// Build the C ↔ B ↔ A chain and wait until C has the pingwave-
/// learned `(A via B)` route. Returns (a, b, c).
async fn chain(c_cfg: MeshNodeConfig) -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = build(base_config()).await;
    let b = build(detector_config()).await;
    let c = build(c_cfg).await;

    handshake(&b, &c).await;
    handshake(&b, &a).await;

    let a_id = a.node_id();
    assert!(
        wait_until(
            || c.router().routing_table().lookup(a_id) == Some(b.local_addr()),
            Duration::from_secs(3),
        )
        .await,
        "precondition: C never learned (A via B) from the session-open flood",
    );
    (a, b, c)
}

/// Core RT-5 claim: after B detects A's death, C's `(A via B)` route
/// is withdrawn within one flood — with C's own age-out sweep parked
/// 30 s out, the withdrawal is the only thing that can remove it.
#[tokio::test]
async fn failed_peer_routes_are_withdrawn_mesh_wide() {
    let (a, b, c) = chain(base_config()).await;
    let a_id = a.node_id();

    a.shutdown().await.expect("shutdown A");

    // B: silence > 1 s, Failed after 3 missed ticks (~2 s), then the
    // on_failure flood. 8 s is generous CI slack; the sweep
    // non-explanation holds until 30 s.
    assert!(
        wait_until(
            || c.router().routing_table().lookup(a_id).is_none(),
            Duration::from_secs(8),
        )
        .await,
        "C kept its route to dead A — the withdrawal flood never \
         arrived (C's own age-out sweep is 30 s out, so it cannot \
         mask this)",
    );

    drop(b);
}

/// Mixed-version / kill-switch degradation: a node with
/// `enable_route_withdraw = false` ignores inbound withdrawals the
/// same way a pre-RT-5 node drops the unknown subprotocol — the
/// route survives until the age-out sweep.
#[tokio::test]
async fn disabled_receiver_keeps_route_until_age_out() {
    let (a, b, c) = chain(base_config().with_route_withdraw(false)).await;
    let a_id = a.node_id();

    a.shutdown().await.expect("shutdown A");

    // Give B ample time to detect + flood (same budget as the
    // positive test), then require the route to still be there:
    // the withdrawal was ignored and the 30 s sweep hasn't fired.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        c.router().routing_table().lookup(a_id).is_some(),
        "C dropped its route although route-withdraw handling is \
         disabled and its age-out sweep is 30 s out — something else \
         removed it",
    );

    drop(b);
}

/// Cubic review P1: a withdrawal must not "promote" a relayed
/// (`connect_via`) session as if it were a direct one. A's peer
/// entry for C stores the RELAY's address (B) — re-adding it would
/// re-install exactly the `(C via B)` route the withdrawal just
/// dropped, making withdrawals a no-op for relayed destinations.
#[tokio::test]
async fn withdrawal_is_not_undone_by_relayed_session_promotion() {
    // A ↔ B direct, B ↔ C direct, then A's session to C is
    // established THROUGH B (`connect_via`) — A has no direct UDP
    // path to C in the routing sense (its peer entry for C carries
    // B's address).
    let a = build(base_config()).await;
    let b = build(detector_config()).await;
    let c = build(base_config()).await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let c_id = c.node_id();
    let b_pub = *b.public_key();
    let c_pub = *c.public_key();
    let b_addr = b.local_addr();
    let c_addr = c.local_addr();

    // Pre-start direct handshakes (accept + connect joined, same
    // shape as the three_node connect_via tests).
    let (r1, r2) = tokio::join!(b.accept(a_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.connect(b_addr, &b_pub, b_id).await
    });
    r1.expect("B accept A");
    r2.expect("A connect B");
    let (r1, r2) = tokio::join!(c.accept(b_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        b.connect(c_addr, &c_pub, c_id).await
    });
    r1.expect("C accept B");
    r2.expect("B connect C");

    // Receive loops must run before connect_via.
    a.start();
    b.start();
    c.start();

    a.connect_via(b_addr, &c_pub, c_id)
        .await
        .expect("A connect_via B to C");
    assert_eq!(
        a.router().routing_table().lookup(c_id),
        Some(b_addr),
        "precondition: A routes to C via the relay B",
    );

    // Kill C. B (tight detector) marks it Failed and floods the
    // withdrawal; A must drop (C via B) and — the regression — must
    // NOT re-promote its relayed peer entry for C, whose address IS
    // the withdrawing relay.
    c.shutdown().await.expect("shutdown C");

    assert!(
        wait_until(
            || a.router().routing_table().lookup(c_id).is_none(),
            Duration::from_secs(8),
        )
        .await,
        "A still routes to dead C via B — the relayed peer entry was \
         promoted straight back into the routing table, undoing the \
         withdrawal",
    );
}
