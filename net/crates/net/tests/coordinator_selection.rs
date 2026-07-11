//! Integration tests for `NAT_TRAVERSAL_V2_PLAN.md` Stage 3a —
//! rendezvous-coordinator auto-selection and the `connect_direct_auto`
//! entry point.
//!
//! `MeshNode::select_punch_coordinator(target)` picks a coordinator
//! without the caller naming one, in four tiers (decision 5):
//!
//! 1. the relay currently forwarding to `target` (routing next-hop),
//! 2. a `relay-capable`-tagged mutual direct peer,
//! 3. any mutual direct peer,
//! 4. none — the caller stays on the routed path.
//!
//! `connect_direct_auto` wraps that: `Direct` pairs need no
//! coordinator; punch-needing pairs with no candidate surface
//! `RendezvousNoRelay` (tier-4 skip) rather than failing connectivity.
//!
//! Run: `cargo test --features net,nat-traversal --test coordinator_selection`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::traversal::classify::NatClass;
use net::adapter::net::traversal::TraversalError;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(4, Duration::from_secs(4));
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

async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

async fn wait_for<F: Fn() -> bool>(limit: Duration, check: F) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

/// A phantom target the local node has no route to — forces the
/// coordinator search past tier 1 (routing next-hop) into the
/// direct-peer tiers.
const PHANTOM_TARGET: u64 = 0xDEAD_BEEF_0000_9999;

/// Tier 2 beats tier 3: a `relay-capable` peer is chosen over a plain
/// peer *even when the plain peer has the lower node id* (tier 3's
/// deterministic pick). Roles are assigned by node id so the result
/// can't be explained by the lowest-id fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_prefers_relay_capable_over_lower_id_peer() {
    let a = build_node().await;
    let p1 = build_node().await;
    let p2 = build_node().await;

    // The relay-capable peer must be the HIGHER-id one, so tier 3
    // (lowest id) would pick the *other* peer — proving a
    // relay-capable result comes from tier 2, not the fallback.
    let (relay_peer, plain_peer) = if p1.node_id() > p2.node_id() {
        (p1, p2)
    } else {
        (p2, p1)
    };
    assert!(
        relay_peer.node_id() > plain_peer.node_id(),
        "precondition: relay-capable peer has the higher node id",
    );

    connect_pair(&a, &relay_peer).await;
    connect_pair(&a, &plain_peer).await;
    a.start();
    relay_peer.start();
    plain_peer.start();

    relay_peer
        .announce_capabilities(CapabilitySet::new().with_relay_capable())
        .await
        .expect("relay peer announce");
    plain_peer
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("plain peer announce");

    // Poll until A has folded the relay-capable tag — before that,
    // tier 3 returns the lowest-id peer (the plain one).
    let relay_id = relay_peer.node_id();
    let a_poll = a.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_poll.select_punch_coordinator(PHANTOM_TARGET) == Some(relay_id)
        })
        .await,
        "A should pick the relay-capable peer as coordinator; got {:?}",
        a.select_punch_coordinator(PHANTOM_TARGET),
    );
    assert_ne!(
        a.select_punch_coordinator(PHANTOM_TARGET),
        Some(plain_peer.node_id()),
        "must not fall back to the lower-id plain peer once relay-capable is known",
    );
}

/// Tier 3: with no `relay-capable` peer, any mutual direct peer is a
/// valid coordinator. The pick is the lowest node id (deterministic).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_falls_back_to_any_mutual_peer() {
    let a = build_node().await;
    let q1 = build_node().await;
    let q2 = build_node().await;
    connect_pair(&a, &q1).await;
    connect_pair(&a, &q2).await;
    a.start();
    q1.start();
    q2.start();

    // No announcements needed — tier 3 scans the live peer table
    // directly. Lowest id wins.
    let expected = q1.node_id().min(q2.node_id());
    assert_eq!(
        a.select_punch_coordinator(PHANTOM_TARGET),
        Some(expected),
        "tier 3 should return the lowest-id mutual peer",
    );
}

/// Tier 4: a node with no mutual peers has no coordinator. Selection
/// returns `None`, and `connect_direct_auto` for a punch-needing pair
/// surfaces `RendezvousNoRelay` — connectivity is never at risk, the
/// punch just isn't attempted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_coordinator_yields_none_and_rendezvous_no_relay() {
    let a = build_node().await;
    a.start();

    assert_eq!(
        a.select_punch_coordinator(PHANTOM_TARGET),
        None,
        "a peerless node has no coordinator candidate",
    );

    // Force a pair type that *requires* a coordinator: local Cone ×
    // remote Unknown (a never-announced phantom) → SinglePunch.
    a.force_nat_class_for_test(NatClass::Cone);
    let dummy_pubkey = [7u8; 32];
    let result = a.connect_direct_auto(PHANTOM_TARGET, &dummy_pubkey).await;
    match result {
        Err(TraversalError::RendezvousNoRelay) => {}
        other => panic!("expected RendezvousNoRelay with no coordinator, got {other:?}"),
    }
}

/// `connect_direct_auto` for a `Direct` pair (Open × Open) needs no
/// coordinator at all — it resolves on the peer's reflex, same as the
/// coordinator-supplied `connect_direct`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_auto_open_pair_needs_no_coordinator() {
    let a = build_node().await;
    let b = build_node().await;
    let x = build_node().await;
    // Two peers each so both classify (reclassify needs ≥2 probe
    // targets). A and B are NOT directly connected — the direct
    // handshake in connect_direct_auto establishes that session.
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    let y = build_node().await;
    connect_pair(&a, &y).await;
    connect_pair(&b, &y).await;
    a.start();
    b.start();
    x.start();
    y.start();

    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let b_id = b.node_id();
    let b_bind = b.local_addr();
    let a_poll = a.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_poll.peer_reflex_addr(b_id) == Some(b_bind)
        })
        .await,
        "A should see B's reflex before connecting",
    );

    let b_pub = *b.public_key();
    let sid = a
        .connect_direct_auto(b_id, &b_pub)
        .await
        .expect("connect_direct_auto should succeed for an Open pair");
    assert_eq!(sid, b_id, "returns the peer's node_id");
    assert_eq!(
        a.peer_addr(b_id),
        Some(b_bind),
        "Direct auto path resolves on B's reflex",
    );
}

/// Relay-routed peer-table entries are NEVER coordinator candidates
/// (cubic P2). A session reached via a relay has the relay's address
/// in its `PeerInfo` — a `PunchRequest` sent there can't even reach
/// the peer (the relay's own session can't decrypt it), and the
/// peer's anti-reflection guard would reject the request anyway
/// (it reaches us via a relay address, not our reflex).
///
/// Deterministic in both directions: P is `relay-capable` and would
/// win tier 2 outright pre-fix (regardless of node-id ordering);
/// post-fix P is excluded as routed and the only direct peer R wins
/// tier 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routed_peer_is_never_a_coordinator_candidate() {
    let a = build_node().await;
    let r = build_node().await;
    let p = build_node().await;
    // A—R and P—R direct; A and P never connect directly.
    connect_pair(&a, &r).await;
    connect_pair(&p, &r).await;
    a.start();
    r.start();
    p.start();

    // P advertises relay-capable; the announcement crosses R into
    // A's index. P has a single peer so classification no-ops and
    // its announcement carries no reflex — poll the fold's entry
    // presence instead (the tags ride the same announcement).
    p.announce_capabilities(CapabilitySet::new().with_relay_capable())
        .await
        .expect("P announce");
    let p_id = p.node_id();
    let a_poll = a.clone();
    assert!(
        wait_for(Duration::from_secs(5), || {
            a_poll.test_capability_fold_has(p_id)
        })
        .await,
        "P's announcement should reach A's index via R",
    );

    // Give A a relay-routed session to P through R.
    let r_bind = r.local_addr();
    let p_pub = *p.public_key();
    a.connect_via(r_bind, &p_pub, p_id)
        .await
        .expect("relay-routed connect_via");
    assert_eq!(
        a.peer_addr(p_id),
        Some(r_bind),
        "precondition: A's session to P rides the relay",
    );

    // Selection for an unrelated target must skip routed P — even
    // though P is relay-capable (tier 2) — and land on direct R.
    let selected = a.select_punch_coordinator(PHANTOM_TARGET);
    assert_ne!(
        selected,
        Some(p_id),
        "a relay-routed peer must never be selected as coordinator",
    );
    assert_eq!(
        selected,
        Some(r.node_id()),
        "the only direct peer (R) should win tier 3",
    );
}
