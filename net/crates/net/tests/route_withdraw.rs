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

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

/// Detector-tight config for the node expected to notice the death:
/// 1 s of silence + 3 misses at the 200 ms tick ≈ 2 s to Failed.
fn detector_config() -> MeshNodeConfig {
    base_config().with_session_timeout(Duration::from_secs(1))
}

/// `common::connect_pair` (connect + accept) plus the `start()` calls
/// these tests need. The started node MUST be the initiator — a running
/// dispatch loop owns the socket, so a post-start `accept` never sees
/// the handshake packets.
async fn handshake(initiator: &Arc<MeshNode>, acceptor: &Arc<MeshNode>) {
    connect_pair(initiator, acceptor).await;
    initiator.start();
    acceptor.start();
}

/// Cond-first arg-order adapter over `common::poll_until`, so the poll
/// loop lives in one place (a CI-cadence tweak reaches every test).
async fn wait_until<F: FnMut() -> bool>(cond: F, timeout: Duration) -> bool {
    poll_until(timeout, cond).await
}

/// Build the C ↔ B ↔ A chain and wait until C has the pingwave-
/// learned `(A via B)` route. Returns (a, b, c).
async fn chain(c_cfg: MeshNodeConfig) -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node_with(base_config()).await;
    let b = build_node_with(detector_config()).await;
    let c = build_node_with(c_cfg).await;

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
    let a = build_node_with(base_config()).await;
    let b = build_node_with(detector_config()).await;
    let c = build_node_with(base_config()).await;

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

/// A `node_id_to_graph_id` clone for tests: the proximity graph keys
/// nodes by a 32-byte id that is the u64 node_id (LE) zero-padded into
/// its first 8 bytes (see `mesh.rs::node_id_to_graph_id`).
fn graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// RT-5 addendum review P1: a REAL node death must not be masked by a
/// stale soft-state graph path. In the review's triangle A/B/C, B holds
/// both a direct edge B→A and an indirect edge B→C→A. When A dies the
/// indirect edge has not yet aged out (its sweep is 3× session_timeout
/// out), so the PRE-FIX failure path treated it as evidence A was still
/// reachable and SUPPRESSED B's withdrawal — pinning downstream nodes to
/// a dead route and risking a B↔C micro-loop. B must withdraw on the
/// authoritative direct-peer failure regardless of the graph snapshot.
///
/// Topology: A↔B (so B is A's direct peer and its detector notices the
/// death), B↔C (a real peer C, giving B a natural B→C edge), observer
/// D↔B (learns `(A via B)`). D keeps a 10 s session_timeout so its own
/// 30 s age-out sweep cannot explain an in-window drop.
///
/// The stale C→A edge is INJECTED rather than grown from live traffic:
/// on localhost A's 1-hop direct pingwave always beats C's 2-hop forward
/// of the same seq to B, so dedup drops the forward and B never records
/// C→A naturally. The injected edge models the real transient the review
/// targets — B learned A via C before the direct A↔B link came up (or
/// during a flap) and that soft state has not yet swept. C is a genuine
/// peer, so it is an indirect path the pre-fix code would suppress on.
#[tokio::test]
async fn node_death_withdraws_even_with_a_stale_graph_alternate() {
    let a = build_node_with(base_config()).await;
    let b = build_node_with(detector_config()).await;
    let c = build_node_with(base_config()).await;
    let d = build_node_with(base_config()).await;

    // Every handshake runs BEFORE any `start()`: an unstarted node can
    // `accept`, a started one (its dispatch loop owns the socket) can
    // only initiate.
    connect_pair(&a, &b).await;
    connect_pair(&b, &c).await;
    connect_pair(&b, &d).await;

    a.start();
    b.start();
    c.start();
    d.start();

    let a_id = a.node_id();
    let c_id = c.node_id();
    let a_graph_id = graph_id(a_id);

    // Precondition part 1: D learned `(A via B)`, and B has grown its
    // natural direct B→A and B→C edges from the pingwave floods.
    assert!(
        wait_until(
            || {
                d.router().routing_table().lookup(a_id) == Some(b.local_addr())
                    && b.proximity_graph().path_to(&a_graph_id).is_some()
                    && b.proximity_graph()
                        .edge_latency(b.proximity_graph().my_id(), graph_id(c_id))
                        .is_some()
            },
            Duration::from_secs(6),
        )
        .await,
        "precondition: D never learned (A via B), or B never built its \
         B→A / B→C edges from the pingwave flood",
    );

    // Inject the stale indirect edge C→A into B's graph, then confirm B
    // now holds an INDIRECT alternate to A (B→C→A) — exactly the soft
    // state the pre-fix code suppressed the withdrawal on.
    b.proximity_graph()
        .test_insert_edge(graph_id(c_id), a_graph_id, 500);
    assert!(
        b.proximity_graph()
            .path_to_excluding_direct(&a_graph_id)
            .is_some(),
        "precondition: B must hold an indirect B→C→A alternate to A",
    );

    // Kill A. B (tight detector) marks it Failed in ~2 s — with the
    // B→C→A edge still present, i.e. the pre-fix suppression window open.
    a.shutdown().await.expect("shutdown A");

    // The fix: B floods the withdrawal despite holding the graph
    // alternate, so D drops `(A via B)`. Pre-fix, B stayed quiet and D
    // kept the dead route until its 30 s sweep.
    assert!(
        wait_until(
            || d.router().routing_table().lookup(a_id).is_none(),
            Duration::from_secs(8),
        )
        .await,
        "D kept its route to dead A — B suppressed its withdrawal on the \
         stale B→C→A graph path (D's own age-out sweep is 30 s out, so it \
         cannot mask this)",
    );

    drop((b, c, d));
}
