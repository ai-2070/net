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

/// A node with a short session timeout so that a peer whose traffic is
/// blocked trips the failure detector (3 × timeout ≈ 1.5 s) within a
/// couple of seconds — used to drive the `on_failure` cleanup path
/// deterministically.
async fn build_fast_fail_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_millis(500))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
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
    let session = a.peer_session_for_test(b_id).expect("A has a session to B");
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
    // Attempt/success stay in lockstep: the earlier deferral counted
    // as neither, the quiescent retry as exactly one of each.
    assert_eq!(
        a.traversal_stats().upgrades_attempted,
        1,
        "one upgrade attempt (the deferral was not an attempt)"
    );
    assert_eq!(
        a.traversal_stats().upgrades_succeeded,
        1,
        "upgrade succeeded"
    );
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

/// Review finding #2 (growth + reconnect pin): a failed peer's
/// direct-path upgrade throttle entry is dropped by the failure
/// detector's `on_failure` callback. Without this the cache grows
/// without bound under peer churn, and a peer that reconnects under the
/// same node_id stays pinned to the relay by a stale terminal `done`
/// from its previous session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_peer_drops_its_upgrade_cache_entry() {
    let a = build_fast_fail_node().await;
    let b = build_fast_fail_node().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();
    let b_id = b.node_id();

    // Seed A's cache with an entry for B. The pair is direct (localhost),
    // so the attempt records a terminal entry — the point is only that
    // *something* is cached for B so we can watch it get dropped.
    a.attempt_direct_upgrade_for_test(b_id).await;
    assert!(
        a.upgrade_cache_contains_for_test(b_id),
        "attempt should have recorded a throttle entry for B",
    );

    // Cut B's traffic so A's failure detector marks it Failed; the
    // on_failure callback must then drop B's throttle entry.
    a.block_peer(b.local_addr());
    let ac = a.clone();
    assert!(
        wait_for(Duration::from_secs(8), || {
            !ac.upgrade_cache_contains_for_test(b_id)
        })
        .await,
        "the upgrade throttle entry for B must be dropped when the \
         failure detector evicts B",
    );
}

/// Review finding #2 (SinglePunch pin): a relay-routed `SinglePunch`
/// pair is *deferred*, not marked terminal `done`. A later NAT
/// reclassification can flip the pair to `Direct`, so a permanent
/// `done` would pin the session to the relay for the process's life.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_punch_pair_is_deferred_not_terminal() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    establish_relay_session(&a, &r, &b).await;
    // Let the relay session go quiescent so the attempt clears the C3
    // busy gate and actually reaches the pair-action classification.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Force A symmetric; B classifies Open on localhost, so
    // pair_action(Symmetric, Open) = SinglePunch.
    a.force_nat_class_for_test(NatClass::Symmetric);
    let b_id = b.node_id();

    a.attempt_direct_upgrade_for_test(b_id).await;

    assert_eq!(
        a.upgrade_entry_is_done_for_test(b_id),
        Some(false),
        "a SinglePunch pair must be deferred (not terminal) so a later \
         reclassification can still upgrade it",
    );
}

/// A `SkipPunch` pair must be *deferred*, not marked terminal — the
/// classification it rests on can be provisional.
///
/// The matrix reaches `SkipPunch` from `Unknown`, not just from
/// symmetric × symmetric: `(Symmetric, Unknown)` lands here, and
/// `Unknown` is exactly what `peer_nat_class` returns until the peer's
/// announcement is indexed. The scan loop starts ticking at 1 s, well
/// inside that window, so a peer evaluated during startup used to be
/// marked terminal `done` and pinned to the relay for the life of its
/// peer entry — on a classification that was about to change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_punch_from_an_unclassified_peer_is_not_terminal() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = upgrade_topology().await;
    // Deliberately no `classify_and_exchange_reflexes`: B never
    // announces a `nat:*` tag, so A sees it as `Unknown` — the startup
    // state this test is about.
    a.reclassify_nat().await;
    establish_relay_session(&a, &r, &b).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.force_nat_class_for_test(NatClass::Symmetric);
    let b_id = b.node_id();
    assert_eq!(
        a.peer_nat_class(b_id),
        NatClass::Unknown,
        "precondition: B's class has not been announced yet",
    );

    a.attempt_direct_upgrade_for_test(b_id).await;

    assert_eq!(
        a.upgrade_entry_is_done_for_test(b_id),
        Some(false),
        "a SkipPunch pair must not be terminal — this one is only \
         SkipPunch because B's class hasn't arrived",
    );
    let next = a
        .upgrade_next_eligible_in_for_test(b_id)
        .expect("B has a throttle entry");
    assert!(
        next <= Duration::from_secs(300),
        "the SkipPunch re-check must be bounded, got {next:?}",
    );
}

/// The genuinely-unpunchable pair (symmetric × symmetric) is deferred on
/// the same bounded re-check rather than cached off for good. Re-deriving
/// `SkipPunch` every 5 min costs nothing on the wire — `pair_action`
/// reads a local atomic plus the capability fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symmetric_pair_defers_on_the_long_recheck() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = upgrade_topology().await;
    // Both ends symmetric → SkipPunch on its own merits, not because of
    // a missing announcement. The class is forced *before* the first
    // announce, per `force_nat_class_for_test`'s contract: a re-announce
    // after an earlier `nat:open` leaves the prior entry in the reader's
    // fold, and `peer_nat_class` returns the first `nat:*` it finds.
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.force_nat_class_for_test(NatClass::Symmetric);
    b.force_nat_class_for_test(NatClass::Symmetric);
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let b_id = b.node_id();
    let ac = a.clone();
    assert!(
        wait_for(Duration::from_secs(8), || {
            ac.peer_nat_class(b_id) == NatClass::Symmetric
        })
        .await,
        "A should fold B's symmetric classification",
    );

    establish_relay_session(&a, &r, &b).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.attempt_direct_upgrade_for_test(b_id).await;

    assert_eq!(
        a.upgrade_entry_is_done_for_test(b_id),
        Some(false),
        "even a true symmetric × symmetric pair defers rather than \
         caching a terminal outcome",
    );
    assert_eq!(
        a.traversal_stats().upgrades_attempted,
        0,
        "a SkipPunch defer must not touch the wire",
    );
}

/// A local NAT reclassification re-arms the upgrade throttle: every
/// pair action is a function of the local class, so a change invalidates
/// every cached outcome. Without this the peer waits out a window sized
/// for the old classification — up to the 5 min `SkipPunch` re-check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_reclassification_rearms_the_upgrade_throttle() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = upgrade_topology().await;
    // No reflex exchange → the Direct arm has no target, so the attempt
    // fails immediately and banks a backoff.
    a.reclassify_nat().await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.attempt_direct_upgrade_for_test(b_id).await;
    assert_eq!(
        a.upgrade_failure_count_for_test(b_id),
        Some(1),
        "precondition: the failed attempt banked a backoff",
    );
    assert!(
        a.upgrade_next_eligible_in_for_test(b_id) > Some(Duration::from_secs(1)),
        "precondition: the throttle window is open for seconds",
    );

    // Localhost classifies Open; move to a genuinely different class.
    a.force_nat_class_for_test(NatClass::Cone);

    assert_eq!(
        a.upgrade_failure_count_for_test(b_id),
        Some(0),
        "a reclassification must clear the failure count",
    );
    assert_eq!(
        a.upgrade_next_eligible_in_for_test(b_id),
        Some(Duration::ZERO),
        "a reclassification must reopen the throttle window immediately",
    );
    assert!(
        a.upgrade_try_acquire_for_test(b_id).is_some(),
        "the peer should be immediately attemptable again",
    );
}

/// The reset fires only on an *actual* class change. The periodic
/// classify loop commits on every tick (60 s default); resetting
/// unconditionally would wipe the failure counter that often and cap a
/// pathological pair's retry interval at 60 s forever, defeating the
/// exponential backoff entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reclassification_to_the_same_class_leaves_the_backoff_intact() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = upgrade_topology().await;
    a.reclassify_nat().await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.attempt_direct_upgrade_for_test(b_id).await;
    a.attempt_direct_upgrade_for_test(b_id).await;
    assert_eq!(
        a.upgrade_failure_count_for_test(b_id),
        Some(2),
        "precondition: two failures banked",
    );

    // Re-publish the class the node already holds — a no-change commit,
    // which is what every steady-state classify tick is.
    assert_eq!(a.nat_class(), NatClass::Open, "localhost classifies Open");
    a.force_nat_class_for_test(NatClass::Open);

    assert_eq!(
        a.upgrade_failure_count_for_test(b_id),
        Some(2),
        "an unchanged classification must not reset the backoff",
    );
    assert!(
        a.upgrade_next_eligible_in_for_test(b_id) > Some(Duration::from_secs(1)),
        "an unchanged classification must not reopen the throttle window",
    );
}

/// The reset must not clear `in_flight`. An attempt that is running
/// right now still owns its slot; releasing it here would let the scan
/// loop spawn the duplicate that `UpgradeAttemptGuard` exists to
/// prevent — a reclassification is exactly the kind of event that can
/// land mid-attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reclassification_does_not_release_an_in_flight_attempt() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = upgrade_topology().await;
    a.reclassify_nat().await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();

    let guard = a
        .upgrade_try_acquire_for_test(b_id)
        .expect("first acquire should win the slot");

    a.force_nat_class_for_test(NatClass::Cone);

    assert!(
        a.upgrade_try_acquire_for_test(b_id).is_none(),
        "a reclassification must not release a running attempt's slot",
    );
    drop(guard);
    assert!(
        a.upgrade_try_acquire_for_test(b_id).is_some(),
        "the slot is still released normally when the attempt finishes",
    );
}

/// Review finding #3: the background upgrade loop holds only a Weak
/// self-ref, so an `Arc<MeshNode>` dropped WITHOUT an explicit
/// `shutdown()` still runs `Drop` (which sets `shutdown` and tears the
/// loop down). A strong self-ref captured in the loop task would keep
/// the node — and its socket and every background task — alive forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_loop_does_not_leak_the_node() {
    let node = build_upgrading_node().await;
    node.start_arc(); // spawns the direct-upgrade loop
                      // Let the loop reach its steady-state `select!` park.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let weak = Arc::downgrade(&node);
    drop(node); // no shutdown() — rely on Drop to tear things down

    // Drop must win: with a Weak self-ref the strong count hits zero and
    // the node is freed. A strong self-ref in the loop would pin it and
    // this poll would time out.
    let freed = wait_for(Duration::from_secs(5), || weak.upgrade().is_none()).await;
    assert!(
        freed,
        "node should be freed after its last Arc is dropped; the upgrade \
         loop is holding a strong self-ref",
    );
}

/// The default-on blocker: exactly one upgrade attempt per peer can be
/// in flight, enforced by an RAII slot rather than a timed lease.
///
/// The shipped 10 s `ATTEMPT_LEASE` was shorter than the attempt's own
/// worst case — `handshake_retries × handshake_timeout` plus the
/// 100 ms + 200 ms inter-retry sleeps, 15.3 s at the library defaults —
/// so the 1 s scan loop could spawn a second attempt for a peer whose
/// first was still running. The duplicate fast-fails on the occupied
/// `pending_handshakes` slot and records a *failure*, double-counting
/// `failures` and escalating the backoff on a peer that is merely slow.
///
/// The slot is claimed under the cache entry's write guard, so this
/// holds regardless of how long an attempt runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_one_upgrade_attempt_per_peer_can_be_in_flight() {
    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();

    let guard = a
        .upgrade_try_acquire_for_test(b_id)
        .expect("first acquire should win the slot");

    // Every subsequent claim loses while the first attempt is alive —
    // this is what the expired lease used to allow through. The loop
    // does exactly this and `continue`s on None.
    for i in 0..5 {
        assert!(
            a.upgrade_try_acquire_for_test(b_id).is_none(),
            "acquire #{i} must lose the slot while an attempt is in flight",
        );
    }

    // A losing claim must not touch the throttle: no phantom failure, no
    // pushed-out window. Otherwise a peer with a slow attempt would be
    // punished by its own duplicate.
    assert_eq!(
        a.upgrade_failure_count_for_test(b_id),
        Some(0),
        "a lost acquire must not count as a failure",
    );

    // Releasing the guard reopens the slot — including on a panicking
    // attempt task, which is why it's RAII and not a manual clear.
    drop(guard);
    assert!(
        a.upgrade_try_acquire_for_test(b_id).is_some(),
        "the slot must be released when the attempt's guard drops",
    );
}

/// A busy-defer must not advance the failure backoff: the session is
/// healthy, just carrying traffic. If deferrals counted as failures, a
/// continuously-busy session would be pushed to the 64 s ceiling and
/// effectively stop being considered for upgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_defer_does_not_advance_the_failure_backoff() {
    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();

    let session = a.peer_session_for_test(b_id).expect("A has a session to B");
    session.get_or_create_stream(0xBEEF);

    for _ in 0..4 {
        a.attempt_direct_upgrade_for_test(b_id).await;
    }

    assert_eq!(
        a.traversal_stats().upgrades_deferred_busy,
        4,
        "each attempt against a busy session should defer",
    );
    assert_eq!(
        a.upgrade_failure_count_for_test(b_id),
        Some(0),
        "busy deferrals must not increment the failure counter",
    );
    // The defer window is the short 1 s re-check, not a backoff step.
    let next = a
        .upgrade_next_eligible_in_for_test(b_id)
        .expect("B has a throttle entry");
    assert!(
        next <= Duration::from_secs(1),
        "a busy defer should schedule the short re-check, got {next:?}",
    );
}

/// The observable retry schedule after real failed attempts: nominal
/// 4 s then 8 s, each shaved by deterministic jitter into the lower
/// quarter of its window ([0.75 × nominal, nominal]).
///
/// Jitter subtracts rather than adds so the documented 64 s ceiling
/// stays true as a wall-clock statement. Without it, nodes started
/// together scan on the same 1 s cadence and retry failed upgrades in
/// synchronized 4/8/16/32/64 s waves — the schedule itself is verified
/// exhaustively by the `upgrade_backoff` / `upgrade_jitter` unit tests;
/// this pins that the live path actually applies it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_attempts_back_off_with_jitter() {
    let (a, r, b, _x) = upgrade_topology().await;
    // No reflex exchange → the Direct arm has no target address, so each
    // attempt fails immediately and without wire activity. Real elapsed
    // time inside an attempt is therefore ~0 and the remaining window
    // can be read straight back.
    a.reclassify_nat().await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();
    // Quiescent, so attempts reach the classification instead of the C3
    // busy gate.
    tokio::time::sleep(Duration::from_millis(300)).await;

    for (expected_failures, nominal) in [(1u32, 4u64), (2, 8)] {
        a.attempt_direct_upgrade_for_test(b_id).await;
        assert_eq!(
            a.upgrade_failure_count_for_test(b_id),
            Some(expected_failures),
            "each failed attempt should advance the failure counter once",
        );

        let nominal = Duration::from_secs(nominal);
        let next = a
            .upgrade_next_eligible_in_for_test(b_id)
            .expect("B has a throttle entry");
        assert!(
            next <= nominal,
            "failure #{expected_failures}: jitter must only shave — {next:?} \
             exceeds the {nominal:?} nominal window",
        );
        // 100 ms of slack for the attempt's own (network-free) runtime.
        assert!(
            next >= nominal * 3 / 4 - Duration::from_millis(100),
            "failure #{expected_failures}: {next:?} is below the 0.75 × \
             {nominal:?} jitter floor",
        );
        // No need to clear the window between iterations:
        // `attempt_direct_upgrade_for_test` drives the attempt directly
        // and never consults `next_eligible` — only the scan loop's
        // acquire does.
    }
}

/// A successful upgrade is terminal: the entry is marked `done` and the
/// peer stops being a scan candidate. Nothing left to upgrade — the
/// session is already on the direct path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_upgrade_stops_further_attempts() {
    let (a, r, b, _x) = upgrade_topology().await;
    classify_and_exchange_reflexes(&a, &b).await;
    establish_relay_session(&a, &r, &b).await;
    let b_id = b.node_id();
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.attempt_direct_upgrade_for_test(b_id).await;
    assert_eq!(
        a.peer_addr(b_id),
        Some(b.local_addr()),
        "precondition: the session upgraded to the direct path",
    );
    assert_eq!(
        a.upgrade_entry_is_done_for_test(b_id),
        Some(true),
        "a successful upgrade must be terminal",
    );
    assert!(
        a.upgrade_try_acquire_for_test(b_id).is_none(),
        "a terminal entry must refuse further attempts",
    );
    assert!(
        !a.upgrade_is_loop_candidate_for_test(b_id),
        "an upgraded peer must drop out of the scan loop's candidate set",
    );
}
