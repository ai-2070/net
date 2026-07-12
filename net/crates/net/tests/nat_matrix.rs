//! Stage 4 (loopback half) of `NAT_TRAVERSAL_V2_PLAN.md` — the two
//! pair-type-matrix tests the parent plan called for that don't need
//! real NAT and were still missing from `tests/connect_direct.rs`:
//!
//! - **Symmetric × Cone attempts exactly once** (parent decision 8).
//!   The matrix elects `SinglePunch`; when the punch fails the
//!   initiator records exactly one attempt (`punches_attempted == 1`,
//!   no retry loop) and exactly one relay fallback, and resolves
//!   within the punch deadline + fallback budget.
//! - **Pre-announced reflex** (parent stage-2 exit wording). A fresh
//!   joiner — no session with the target, so `probe_reflex` toward it
//!   is *impossible* (`PeerNotReachable`) — punches using only the
//!   target's announced reflex, which reaches it via the
//!   coordinator's `PunchIntroduce` (read from the capability index,
//!   populated by the announcement).
//!
//! The real-NAT versions of these scenarios (masqueraded namespaces,
//! where "punch fails" comes from actual endpoint-dependent mappings
//! rather than a partition filter) live in the Linux-only natsim
//! suite (`tests/natsim/`).
//!
//! Run: `cargo test --features net,nat-traversal --test nat_matrix`

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

/// `A — R — B` plus X so both endpoints classify. A and B are never
/// directly connected — the punch (or its fallback) is what joins
/// them.
async fn punch_topology() -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node().await;
    let r = build_node().await;
    let b = build_node().await;
    let x = build_node().await;
    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    connect_pair(&r, &x).await;
    a.start();
    r.start();
    b.start();
    x.start();
    (a, r, b, x)
}

/// Classify both ends, pin the given classes, announce, and wait for
/// A's index to hold B's class + reflex.
async fn force_classes_and_announce(
    a: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
    a_class: NatClass,
    b_class: NatClass,
) {
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.force_nat_class_for_test(a_class);
    b.force_nat_class_for_test(b_class);
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    let a_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_poll.peer_reflex_addr(b_id) == Some(b_bind) && a_poll.peer_nat_class(b_id) == b_class
        })
        .await,
        "A should see B's {b_class:?} class + reflex",
    );
}

/// Parent decision 8: a Symmetric × Cone pair elects `SinglePunch`
/// and, on punch failure, attempts **exactly once** — one
/// `punches_attempted`, one `relay_fallbacks`, no retry loop — and
/// resolves within the punch deadline + fallback budget.
///
/// Failure injection: A partition-filters B's address, so A's
/// keep-alive train never reaches B. B's punch observer never fires,
/// B never emits its `PunchAck`, and A's ack-wait times out after
/// `punch_deadline` — the loopback stand-in for a symmetric NAT
/// whose per-destination mapping defeats the advertised reflex. The
/// coordinator paths (introduce, ack forwarding, routed fallback)
/// all ride A—R—B and stay unblocked, so the fallback lands.
///
/// Timing budget: ~punch_deadline (5 s default).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symmetric_cone_punch_failure_attempts_exactly_once() {
    let (a, r, b, _x) = punch_topology().await;
    force_classes_and_announce(&a, &b, NatClass::Symmetric, NatClass::Cone).await;

    let b_id = b.node_id();
    let b_bind = b.local_addr();
    let b_pub = *b.public_key();

    // Kill the direct path A → B. Everything via R stays open.
    a.block_peer(b_bind);

    let before = a.traversal_stats();
    let start = tokio::time::Instant::now();
    let session_id = a
        .connect_direct(b_id, &b_pub, r.node_id())
        .await
        .expect("punch-failed connect_direct must still resolve via the relay");
    let elapsed = start.elapsed();
    assert_eq!(session_id, b_id);

    let after = a.traversal_stats();
    assert_eq!(
        after.punches_attempted - before.punches_attempted,
        1,
        "Symmetric × Cone must attempt the punch EXACTLY once — no retry loop",
    );
    assert_eq!(
        after.punches_succeeded, before.punches_succeeded,
        "the blocked punch must not be counted as a success",
    );
    assert_eq!(
        after.relay_fallbacks - before.relay_fallbacks,
        1,
        "the single failed attempt falls back to the relay exactly once",
    );
    assert_eq!(
        after.punches_failed - before.punches_failed,
        1,
        "derived failure count reflects the one failed attempt",
    );
    assert_eq!(
        after.punch_timeouts - before.punch_timeouts,
        1,
        "the failure cause is the ack-wait deadline (B never observed A's train)",
    );
    assert_eq!(
        after.punch_rejections, before.punch_rejections,
        "no coordinator rejection was involved",
    );

    // The failed punch consumed its ack deadline, then the routed
    // fallback resolved promptly — bounded, not hanging.
    assert!(
        elapsed >= Duration::from_secs(4),
        "the ack-wait should consume ~punch_deadline; elapsed {elapsed:?}",
    );
    assert!(
        elapsed < Duration::from_secs(12),
        "punch-failed fallback must resolve within deadline + fallback \
         budget; elapsed {elapsed:?}",
    );

    // And the session rides the relay, not the (blocked) direct path.
    assert_eq!(
        a.peer_addr(b_id),
        Some(r.local_addr()),
        "the fallback session must ride the relay",
    );
}

/// Parent stage-2 exit criterion: a fresh joiner punches a peer it
/// has never talked to using only that peer's announced reflex.
///
/// "Zero `probe_reflex` emissions to the target" is structural, and
/// this test pins the structure: `probe_reflex` requires an existing
/// session (asserted: it fails `PeerNotReachable` pre-punch), and
/// the `SinglePunch` path sources the target's reflex from the
/// coordinator's `PunchIntroduce` — which the coordinator reads from
/// its capability index, populated by the target's announcement.
/// The announcement is therefore the only possible source of the
/// reflex the punch lands on, which the final assert confirms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_joiner_punches_using_only_the_announcement() {
    let (a, r, b, _x) = punch_topology().await;
    // Cone × Cone → SinglePunch (punch-worthy, succeeds on loopback).
    force_classes_and_announce(&a, &b, NatClass::Cone, NatClass::Cone).await;

    let b_id = b.node_id();
    let b_bind = b.local_addr();
    let b_pub = *b.public_key();

    // Fresh joiner: no session with the target...
    assert_eq!(
        a.peer_addr(b_id),
        None,
        "precondition: A has never talked to B directly",
    );
    // ...so probing the target is impossible — the announcement is
    // the only reflex source available to this punch.
    match a.probe_reflex(b_id).await {
        Err(TraversalError::PeerNotReachable) => {}
        other => panic!(
            "probe_reflex to a never-connected peer must be PeerNotReachable \
             (proving the punch can't have probed); got {other:?}",
        ),
    }

    let before = a.traversal_stats();
    let session_id = a
        .connect_direct(b_id, &b_pub, r.node_id())
        .await
        .expect("announcement-only punch should succeed");
    assert_eq!(session_id, b_id);

    let after = a.traversal_stats();
    assert_eq!(
        after.punches_attempted - before.punches_attempted,
        1,
        "one mediated punch attempt",
    );
    assert_eq!(
        after.punches_succeeded - before.punches_succeeded,
        1,
        "the punch landed",
    );
    assert_eq!(
        after.relay_fallbacks, before.relay_fallbacks,
        "no fallback — the announced reflex was enough",
    );
    // The session sits on exactly the address B announced.
    assert_eq!(
        a.peer_addr(b_id),
        Some(b_bind),
        "the punched session must land on B's ANNOUNCED reflex",
    );
}
