//! Integration tests for `NAT_TRAVERSAL_V2_PLAN.md` Stage 3 — the
//! background direct-path upgrade and its migration contract.
//!
//! A relay-routed session (`A → R → B`) is opportunistically
//! re-handshaked over a direct path and migrated, cutting the relay
//! hop out of the data plane. The swap obeys the migration contract:
//!
//! - **C1** — only the lower-node-id end initiates (no crossing
//!   re-handshake race).
//! - **C2** — the install is compare-and-swap'd against a racing
//!   rotation (covered by the `install_peer_cas` unit tests).
//! - **C3** — a session with open streams / unacked in-flight data
//!   defers the swap rather than dropping that state.
//!
//! Set up on localhost: `connect_via(relay_addr, …)` gives A a
//! relay-routed session to B (and B a relay-routed session to A via the
//! responder), then the lower-id node's upgrade loop re-handshakes
//! directly to the peer's reflex.
//!
//! Run: `cargo test --features net,nat-traversal --test direct_upgrade`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn base_config() -> MeshNodeConfig {
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
        MeshNode::new(EntityKeypair::generate(), base_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// A node with the background direct-path upgrade enabled.
async fn build_upgrading_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            base_config().with_auto_direct_upgrade(true),
        )
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

/// Build the `A ↔ R ↔ B` topology with an auxiliary X so A and B each
/// have ≥2 peers to classify. Returns `(a, r, b, x)`.
///
/// The upgrade is driven deterministically via
/// `attempt_direct_upgrade_for_test` rather than the background scan
/// loop, so these tests don't race the loop's 1 s cadence under heavy
/// parallel test load. The loop's own wiring (spawned by `start_arc`
/// when `auto_direct_upgrade` is set, with the C1 lower-id filter) is
/// verified by `loop_wiring_is_gated_by_config_and_c1`.
async fn upgrade_topology() -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node().await;
    let r = build_node().await;
    let b = build_node().await;
    let x = build_node().await;
    connect_pair(&a, &r).await;
    connect_pair(&r, &b).await;
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    connect_pair(&r, &x).await;
    a.start();
    b.start();
    r.start();
    x.start();
    (a, r, b, x)
}

/// Drive both A and B to classify (Open on localhost) and announce, then
/// wait until each has folded the other's reflex — the precondition for
/// a `Direct`-pair upgrade.
async fn classify_and_exchange_reflexes(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    let a_id = a.node_id();
    let b_id = b.node_id();
    let a_bind = a.local_addr();
    let b_bind = b.local_addr();
    let (ac, bc) = (a.clone(), b.clone());
    assert!(
        wait_for(Duration::from_secs(8), || {
            ac.peer_reflex_addr(b_id) == Some(b_bind) && bc.peer_reflex_addr(a_id) == Some(a_bind)
        })
        .await,
        "A and B should exchange reflexes before the upgrade",
    );
}

/// Establish A's relay-routed session to B through R, and B's
/// (responder-side) relay-routed session to A. Returns the relay addr.
async fn establish_relay_session(
    a: &Arc<MeshNode>,
    r: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
) -> SocketAddr {
    let r_bind = r.local_addr();
    let b_pub = *b.public_key();
    let b_id = b.node_id();
    a.connect_via(r_bind, &b_pub, b_id)
        .await
        .expect("relay-routed connect_via should establish a session");
    assert_eq!(
        a.peer_addr(b_id),
        Some(r_bind),
        "precondition: A's session to B rides the relay",
    );
    r_bind
}

/// Happy path: an idle relay-routed session is upgraded to a direct
/// session. The session's transport flips from the relay's address to
/// the peer's reflex on both ends, and the upgrade stats bump.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_relay_session_upgrades_to_direct() {
    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    let r_bind = establish_relay_session(&a, &r, &b).await;

    let b_id = b.node_id();
    let a_id = a.node_id();
    let b_bind = b.local_addr();
    let a_bind = a.local_addr();

    // A (initiator) upgrades its relay session to B onto the direct
    // path (B's own reflex). Driven synchronously for determinism.
    a.attempt_direct_upgrade_for_test(b_id).await;

    assert_eq!(
        a.peer_addr(b_id),
        Some(b_bind),
        "A's session should be upgraded to B's direct reflex, not the relay",
    );
    assert_ne!(
        a.peer_addr(b_id),
        Some(r_bind),
        "the upgraded session must no longer ride the relay",
    );
    // The responder side rotated onto the direct path too: B's session
    // to A now points at A's reflex (settled on B's dispatch).
    let b_poll = b.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            b_poll.peer_addr(a_id) == Some(a_bind)
        })
        .await,
        "B's session to A should also migrate to the direct path; got {:?}",
        b.peer_addr(a_id),
    );

    let stats = a.traversal_stats();
    assert_eq!(stats.upgrades_attempted, 1, "one upgrade attempt");
    assert_eq!(stats.upgrades_succeeded, 1, "one successful upgrade");
}

/// C3 busy gate: a relay-routed session carrying an open application
/// stream is NOT swapped — the upgrade defers (recording
/// `upgrades_deferred_busy`) so the in-flight stream isn't dropped. Once
/// the stream is gone the session upgrades.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_relay_session_defers_then_upgrades() {
    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    let r_bind = establish_relay_session(&a, &r, &b).await;

    let b_id = b.node_id();
    let b_bind = b.local_addr();

    // Open an application stream on A's session to B → busy → defer.
    let session = a
        .peer_session_for_test(b_id)
        .expect("A has a session to B");
    session.get_or_create_stream(0xABCD);
    assert!(session.has_open_streams(), "precondition: session is busy");

    a.attempt_direct_upgrade_for_test(b_id).await;
    assert_eq!(
        a.traversal_stats().upgrades_deferred_busy,
        1,
        "a busy session must defer the upgrade",
    );
    assert_eq!(
        a.traversal_stats().upgrades_attempted,
        0,
        "a deferred upgrade must not count as an attempt (no wire activity)",
    );
    assert_eq!(
        a.peer_addr(b_id),
        Some(r_bind),
        "a deferred upgrade must leave the busy session on the relay",
    );

    // Drop the stream → quiescent → the upgrade proceeds.
    session.close_stream(0xABCD);
    assert!(!session.has_open_streams(), "stream removed");
    a.attempt_direct_upgrade_for_test(b_id).await;
    assert_eq!(
        a.peer_addr(b_id),
        Some(b_bind),
        "once quiescent the session should upgrade to the direct path",
    );
    assert_eq!(a.traversal_stats().upgrades_succeeded, 1, "upgrade succeeded");
}

/// Failure atomicity (F6/C5): when the upgrade can't proceed (here: the
/// peer's reflex isn't cached, so the `Direct` arm has no target), the
/// working relay session is left byte-for-byte intact and `addr_to_node`
/// gained no direct entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_upgrade_leaves_relay_session_intact() {
    let (a, r, b, _x) = upgrade_topology().await;
    // Deliberately do NOT exchange reflexes — A has no cached reflex for
    // B, so a Direct-pair upgrade has no target address.
    a.reclassify_nat().await;
    let r_bind = establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();

    let session_before = a
        .peer_session_for_test(b_id)
        .expect("A has a relay session to B")
        .session_id();

    a.attempt_direct_upgrade_for_test(b_id).await;

    assert_eq!(
        a.peer_addr(b_id),
        Some(r_bind),
        "a failed upgrade must leave the session on the relay",
    );
    assert_eq!(
        a.peer_session_for_test(b_id).map(|s| s.session_id()),
        Some(session_before),
        "the relay session must be byte-for-byte intact (same session_id)",
    );
    assert_eq!(
        a.traversal_stats().upgrades_succeeded,
        0,
        "no successful upgrade recorded",
    );
}

/// The background scan loop's candidate filter (C1 + relay-routed +
/// throttle), asserted deterministically rather than by racing the
/// loop's cadence. Over a relay session between A and B: the lower-id
/// node treats the higher-id peer as a candidate; the higher-id node
/// does NOT (C1 — only the lower-id end initiates).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_candidate_filter_enforces_c1_and_relay() {
    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    establish_relay_session(&a, &r, &b).await;

    let (lo, hi) = if a.node_id() < b.node_id() {
        (a.clone(), b.clone())
    } else {
        (b.clone(), a.clone())
    };

    // Lower-id node → the higher-id peer is a relay-routed candidate.
    assert!(
        lo.upgrade_is_loop_candidate_for_test(hi.node_id()),
        "the lower-id node should consider its relay session to the higher peer",
    );
    // Higher-id node → the lower-id peer is NOT a candidate (C1).
    assert!(
        !hi.upgrade_is_loop_candidate_for_test(lo.node_id()),
        "the higher-id node must not initiate (C1)",
    );
    // The relay R is a direct peer of the lower node, not relay-routed,
    // so it's never an upgrade candidate.
    assert!(
        !lo.upgrade_is_loop_candidate_for_test(r.node_id()),
        "a directly-connected peer is not an upgrade candidate",
    );
}
