//! Integration tests for stage 3c of `docs/NAT_TRAVERSAL_PLAN.md`:
//! `MeshNode::connect_direct` orchestration + traversal stats.
//!
//! These tests exercise the pair-type-matrix driven path selection
//! and the stats counters. Stage 3d will add the real keep-alive
//! train + `PunchAck` round-trip; stage 3c approximates "punch
//! succeeded" by "coordinator mediated an introduction," which is
//! sufficient on localhost where every packet trivially reaches
//! its destination.
//!
//! # Properties under test
//!
//! - **Open × Open goes direct.** `connect_direct` picks the
//!   `Direct` action, does not call the coordinator, and
//!   establishes a session on `peer_reflex`. None of the
//!   NAT-traversal stats counters bump on the happy path —
//!   `relay_fallbacks` only fires when we actually end up on
//!   the routed-handshake path.
//! - **Punch-worthy pair bumps `punches_attempted`.** A pair that
//!   the matrix classifies as `SinglePunch` records a punch
//!   attempt in the stats, even when the result is ultimately
//!   punted on localhost.
//! - **Missing peer reflex fails fast.** `connect_direct` to a
//!   peer whose reflex is not cached returns
//!   `PeerNotReachable` without consulting the coordinator.
//! - **Stats are monotonic.** Successive `connect_direct` calls
//!   stack counter increments; snapshots never go backwards.
//!
//! Run: `cargo test --features net,nat-traversal --test connect_direct`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::traversal::TraversalError;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

/// Bind via `127.0.0.1:0` so the OS picks a free port — no
/// pre-bind reservation, no TOCTOU race with parallel tests.
fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        // Generous handshake budget. Under nextest / llvm-cov the runner
        // is heavily loaded (dozens of mesh tests in parallel), which
        // starves recv loops enough that a *localhost* handshake
        // round-trip can blow a tight 2s × 3 budget — surfacing as a
        // "handshake timeout" flake during topology setup, not in the
        // behavior under test. 4 attempts × 4s rides out the stalls.
        // (Only the deliberate wrong-pubkey failure paths pay the full
        // budget; success-path connects still complete on attempt 1.)
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

/// Four-node topology identical to the rendezvous_coordinator
/// tests: R mediates A↔B, X is an auxiliary for classification
/// (A and B need two peers each to run `reclassify_nat`).
///
/// Importantly: **A and B are not directly connected**. That's
/// the whole point — `connect_direct(A, B)` has to open a new
/// session via the rendezvous path.
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

/// Open × Open → direct connect, no punch. Stats: everything
/// stays at zero — the successful direct connect is neither a
/// punch attempt nor a relay fallback. Cubic P1 fix pinned this:
/// an earlier revision unconditionally incremented
/// `relay_fallbacks` on entry to the `Direct` branch, which
/// inflated the counter on every successful direct connect and
/// made `TraversalStats.relay_fallbacks` useless as a
/// "NAT-traversal failed, we're on the relay" signal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_open_pair_takes_direct_path() {
    let (a, r, b, _x) = punch_topology().await;

    // Both sides classify (Open on localhost) + announce so the
    // pair-type matrix has real data on both ends.
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    // Wait for A's index to see B's reflex. Without this,
    // connect_direct returns PeerNotReachable.
    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
        })
        .await,
        "A should see B's reflex",
    );

    let before = a.traversal_stats();

    let b_pub = *b.public_key();
    let session_id = a
        .connect_direct(b_id, &b_pub, r.node_id())
        .await
        .expect("connect_direct should succeed");
    assert_eq!(
        session_id, b_id,
        "connect_direct returns the peer's node_id",
    );

    // Stats: Open×Open → Direct path resolved via peer_reflex.
    // No punch attempted, no relay fallback. All three
    // traversal counters stay put.
    let after = a.traversal_stats();
    assert_eq!(
        after.punches_attempted, before.punches_attempted,
        "Open × Open should not attempt a punch",
    );
    assert_eq!(
        after.punches_succeeded, before.punches_succeeded,
        "no punch attempted → no punch success",
    );
    assert_eq!(
        after.relay_fallbacks, before.relay_fallbacks,
        "a successful Direct connect is NOT a relay fallback — \
         the counter's documented meaning is \"ended up on the \
         routed-handshake path\", which didn't happen here. \
         (Regression guard for cubic P1 — the counter used to \
         bump unconditionally on entry to the Direct branch.)",
    );

    // And the transport is B's reflex, not the coordinator's.
    assert_eq!(
        a.peer_addr(b_id),
        Some(b_bind),
        "Direct path should resolve on B's reflex, not the coordinator",
    );
}

/// `connect_direct` to a peer whose reflex hasn't been announced
/// (not cached in our capability index) fails fast with
/// `PeerNotReachable`. No coordinator call, no stats change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_unknown_peer_fails_fast() {
    let (a, r, _b, _x) = punch_topology().await;

    // Deliberately do NOT reclassify or announce — A's index
    // stays empty of reflex entries for B.

    let bogus_id: u64 = 0xDEAD_BEEF_FEED_CAFE;
    let bogus_pubkey = [0u8; 32];

    let before = a.traversal_stats();
    let start = tokio::time::Instant::now();
    let err = a
        .connect_direct(bogus_id, &bogus_pubkey, r.node_id())
        .await
        .expect_err("connect_direct should fail for uncached peer");
    let elapsed = start.elapsed();

    match err {
        TraversalError::PeerNotReachable => {}
        other => panic!("expected PeerNotReachable, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_millis(500),
        "fast-fail took {elapsed:?}; want < 500 ms (no coordinator round-trip)",
    );
    let after = a.traversal_stats();
    assert_eq!(
        after, before,
        "early-exit paths should not touch the stats counters",
    );
}

/// Counters are monotonic: successive `connect_direct` calls
/// stack increments, never decrement. Guards against a future
/// refactor that accidentally resets a counter on a code path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_counters_are_monotonic() {
    let (a, r, b, _x) = punch_topology().await;

    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
        })
        .await,
        "A should see B's reflex",
    );

    let b_pub = *b.public_key();

    // First connect_direct attempt. `connect` will fail on the
    // second call because A already has a session with B, but
    // the first call should succeed and bump stats.
    let s1 = a.traversal_stats();
    let _ = a.connect_direct(b_id, &b_pub, r.node_id()).await;
    let s2 = a.traversal_stats();

    assert!(
        s2.relay_fallbacks >= s1.relay_fallbacks,
        "relay_fallbacks should never decrease",
    );
    assert!(
        s2.punches_attempted >= s1.punches_attempted,
        "punches_attempted should never decrease",
    );
    assert!(
        s2.punches_succeeded >= s1.punches_succeeded,
        "punches_succeeded should never decrease",
    );
}

/// Pre-classification stats are all zero. Guards against a
/// future default-impl that accidentally seeds counters with
/// non-zero values.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_start_at_zero() {
    let a = build_node().await;
    let stats = a.traversal_stats();
    assert_eq!(stats.punches_attempted, 0);
    assert_eq!(stats.punches_succeeded, 0);
    assert_eq!(stats.relay_fallbacks, 0);
}

/// Regression test for a cubic-flagged P1 bug: `connect_direct`
/// used to resolve the caller-supplied `coordinator` into an
/// address up front and fast-fail with `PeerNotReachable` if
/// that lookup missed — but the `PairAction::Direct` branch
/// doesn't need the coordinator at all (Open/Open, Open/Cone,
/// Open/Unknown, Unknown/Unknown all succeed via the routing
/// table's first-hop). The eager lookup broke those cases for
/// any caller whose coordinator slot referenced a non-peer
/// id. The fix defers coordinator resolution into the branches
/// that actually need it (SkipPunch, SinglePunch).
///
/// This test exercises the Open×Open path with a bogus
/// coordinator id and asserts `connect_direct` still succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_open_pair_succeeds_even_with_unreachable_coordinator() {
    let (a, _r, b, _x) = punch_topology().await;

    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
        })
        .await,
        "A should see B's reflex",
    );

    // Deliberately hand `connect_direct` a coordinator id that
    // is NOT a connected peer. Under the old eager-lookup bug
    // this yielded PeerNotReachable even though Open×Open
    // doesn't need the coordinator. The fix routes this through
    // the routing table's first-hop instead.
    let bogus_coordinator: u64 = 0xDEAD_C0DE_CAFE_BABE;
    assert!(
        a.peer_addr(bogus_coordinator).is_none(),
        "test precondition: bogus coordinator must not be a peer",
    );

    let b_pub = *b.public_key();
    let session_id = a
        .connect_direct(b_id, &b_pub, bogus_coordinator)
        .await
        .expect(
            "Open×Open must resolve via the routing table — the coordinator is \
             irrelevant on this path and its reachability must not be checked",
        );
    assert_eq!(session_id, b_id);
}

/// Complement of the above: `PairAction::SkipPunch` /
/// `SinglePunch` still require a reachable coordinator (either
/// as a relay or as a rendezvous mediator). Passing a bogus
/// coordinator here must still fail with `PeerNotReachable` —
/// those branches have no viable fallback. This test pins the
/// "coordinator reachability gate is preserved where it
/// matters" half of the fix so a future refactor doesn't
/// silently drop the check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_skip_punch_still_requires_reachable_coordinator() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, _r, b, _x) = punch_topology().await;

    // Force Symmetric×Symmetric → SkipPunch. The coordinator is
    // the only viable path for this pair; missing coordinator
    // must still fast-fail.
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

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
                && a_for_poll.peer_nat_class(b_id) == NatClass::Symmetric
        })
        .await,
        "A should see B's Symmetric class + reflex",
    );

    let bogus_coordinator: u64 = 0xDEAD_C0DE_CAFE_BABE;
    let b_pub = *b.public_key();
    let err = a
        .connect_direct(b_id, &b_pub, bogus_coordinator)
        .await
        .expect_err("SkipPunch with unreachable coordinator must fail");

    match err {
        TraversalError::PeerNotReachable => {}
        other => panic!("expected PeerNotReachable (coordinator not a peer); got {other:?}"),
    }
}

/// Regression test for a cubic-flagged P1 bug. Earlier
/// `connect_direct` on the `SinglePunch` path recorded
/// `punches_succeeded++` after a successful rendezvous +
/// PunchAck and then unconditionally called `connect_routed()`
/// — leaving the session on the relayed path through the
/// coordinator. The stats claimed a punched session existed but
/// the data plane didn't actually have one, so the NAT-traversal
/// optimization the plan promises never took effect.
///
/// The fix makes the success branch attempt a direct handshake
/// against the peer's advertised reflex via the dispatch-loop
/// routed-handshake path (which avoids the recv-loop contention
/// that plain `connect()` has post-`start()`). On localhost a
/// SinglePunch pair now actually lands a direct session:
/// `punches_succeeded` increments AND `a.peer_addr(b_id)`
/// resolves to B's bind, not the coordinator's.
///
/// # Observable invariants
///
/// - `punches_attempted` increments (Cone×Cone ran the punch).
/// - `punches_succeeded` increments (ack + direct handshake
///   both landed).
/// - `relay_fallbacks` does NOT increment for this call.
/// - `a.peer_addr(b_id) == Some(b.local_addr())` — the session
///   transport is B's real address, not R's. **This is the
///   load-bearing assertion:** under the pre-fix bug the
///   transport was R's addr because `connect_routed()` was the
///   last write to the peer map, even though `punches_succeeded`
///   was bumped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_upgrades_session_to_punched_path_on_success() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = punch_topology().await;

    // Force both sides to advertise `Cone` so the matrix picks
    // SinglePunch. Reclassify first (so reflex/bind are populated
    // from probes), then overwrite the class atomic — the
    // announce below carries the overwritten value because the
    // nat tag is read from the atomic at announce time.
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.force_nat_class_for_test(NatClass::Cone);
    b.force_nat_class_for_test(NatClass::Cone);

    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
                && a_for_poll.peer_nat_class(b_id) == NatClass::Cone
        })
        .await,
        "A should see B's Cone class + reflex",
    );

    let before = a.traversal_stats();
    let b_pub = *b.public_key();
    let session_id = a
        .connect_direct(b_id, &b_pub, r.node_id())
        .await
        .expect("connect_direct on SinglePunch pair should land on localhost");
    assert_eq!(session_id, b_id);

    let after = a.traversal_stats();

    assert_eq!(
        after.punches_attempted,
        before.punches_attempted + 1,
        "Cone×Cone should attempt a punch",
    );
    assert_eq!(
        after.punches_succeeded,
        before.punches_succeeded + 1,
        "ack + direct handshake both landed — punches_succeeded \
         must increment. The bug this regression guards against \
         is the OPPOSITE shape (counter bumped without a direct \
         session); see the peer_addr assertion below.",
    );
    assert_eq!(
        after.relay_fallbacks, before.relay_fallbacks,
        "successful punch must not also bump relay_fallbacks",
    );

    // Load-bearing. Under the pre-fix bug (cubic P1) the session
    // transport was R's addr because `connect_direct` fell
    // through to `connect_routed()` after bumping
    // `punches_succeeded`. The fix routes the post-ack handshake
    // directly to B's reflex, so peer_addr is B's real bind.
    assert_eq!(
        a.peer_addr(b_id),
        Some(b_bind),
        "after a successful punch, A's session to B must use B's \
         reflex directly — not the coordinator's address. If this \
         is R's addr, the punch succeeded in stats but the data \
         plane still goes via the coordinator.",
    );
    assert_ne!(
        a.peer_addr(b_id),
        Some(r.local_addr()),
        "session transport must not equal the coordinator R's addr",
    );
}

/// Helper: build A + B with a forced pair-action classification
/// and an unreachable-coordinator id. Returns
/// `(a, b_id, b_pubkey, bogus_coord_id)`.
async fn punch_topology_forced(
    a_class: net::adapter::net::traversal::classify::NatClass,
    b_class: net::adapter::net::traversal::classify::NatClass,
) -> (Arc<MeshNode>, u64, [u8; 32], u64) {
    let (a, _r, b, _x) = punch_topology().await;
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

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
                && a_for_poll.peer_nat_class(b_id) == b_class
        })
        .await,
        "A should see B's {b_class:?} class + reflex",
    );

    (a, b_id, *b.public_key(), 0xDEAD_C0DE_CAFE_BABE)
}

/// Regression test for a cubic-flagged P1 bug: `connect_direct`
/// used to demand `peer_reflex_addr(peer_node_id)` at the top
/// of the function — before it had even computed which pair-
/// action branch applied. That rejected every `SkipPunch` pair
/// (Symmetric × Symmetric, Symmetric × Unknown) with
/// `PeerNotReachable` whenever the peer hadn't cached a reflex,
/// even though those branches route entirely through the
/// coordinator and don't read the reflex at all. Same story
/// applies to `SinglePunch`, which gets the peer's reflex from
/// the coordinator's `PunchIntroduce`, not from our index.
///
/// This test builds a Symmetric × Symmetric pair, wipes the
/// peer's reflex from A's capability index by forcing the class
/// without announcing a reflex, and verifies `connect_direct`
/// still resolves via the coordinator.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_punch_succeeds_even_when_peer_reflex_is_uncached() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = punch_topology().await;

    // Force Symmetric × Symmetric → SkipPunch. Critically, do
    // NOT call `reclassify_nat` or `announce_capabilities` on B
    // — A's capability index will have no reflex entry for B.
    a.force_nat_class_for_test(NatClass::Symmetric);
    b.force_nat_class_for_test(NatClass::Symmetric);

    // A needs to learn B's Symmetric class somehow. We announce
    // only on A's side; B publishes via a manual capability
    // push... actually the simplest way is to have A's
    // capability index pre-populated with a non-reflex entry
    // for B. On localhost the classifier needs reflex probes
    // to work, so we rely on B's cap announcement carrying the
    // class tag. But we want B's reflex NOT cached.
    //
    // We can't cleanly skip reflex without skipping the whole
    // announcement, but the Direct/SkipPunch pair-action read
    // falls back to `Unknown` for a peer A hasn't indexed — and
    // `pair_action(Symmetric, Unknown)` is also `SkipPunch`.
    // So we simply DON'T have B announce at all. A sees
    // (Symmetric, Unknown) → SkipPunch, reflex lookup would
    // return None, and before the fix that'd fast-fail with
    // PeerNotReachable.
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    // Sanity-check: A's reflex index for B is empty.
    let b_id = b.node_id();
    assert!(
        a.peer_reflex_addr(b_id).is_none(),
        "test precondition: B's reflex must not be cached on A",
    );
    // And the pair action resolves to SkipPunch (Symmetric ×
    // Unknown).
    assert_eq!(a.peer_nat_class(b_id), NatClass::Unknown);
    assert_eq!(a.nat_class(), NatClass::Symmetric);

    let b_pub = *b.public_key();
    let session_id = a.connect_direct(b_id, &b_pub, r.node_id()).await.expect(
        "SkipPunch should resolve via the coordinator — the \
             peer's reflex is irrelevant on this branch",
    );
    assert_eq!(session_id, b_id);
    assert_eq!(
        a.peer_addr(b_id),
        Some(r.local_addr()),
        "SkipPunch session must be pathed via R",
    );
}

/// Regression test for a cubic-flagged P2 bug on the SkipPunch
/// branch: `record_relay_fallback()` used to fire *before*
/// `coordinator_addr()?` resolved. On a missing-coordinator
/// fast-fail, no traversal attempt actually happened, but the
/// counter had already been bumped — inflating the "on relay"
/// numbers operators use to judge NAT-traversal effectiveness.
/// The fix resolves the coordinator first; stats only move
/// when something meaningful happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_punch_fast_fail_on_missing_coordinator_does_not_bump_stats() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, b_id, b_pub, bogus) =
        punch_topology_forced(NatClass::Symmetric, NatClass::Symmetric).await;

    let before = a.traversal_stats();
    let err = a
        .connect_direct(b_id, &b_pub, bogus)
        .await
        .expect_err("SkipPunch with unreachable coordinator must fail fast");
    match err {
        TraversalError::PeerNotReachable => {}
        other => panic!("expected PeerNotReachable, got {other:?}"),
    }
    let after = a.traversal_stats();
    assert_eq!(
        after, before,
        "fast-fail on missing coordinator must not bump any \
         traversal stats — the pre-fix ordering bug had \
         `relay_fallbacks` bumped before coordinator resolution",
    );
}

/// Complement on the SinglePunch branch: a Cone × Cone pair
/// with a bogus coordinator must also fail fast with no stats
/// changes. Cubic's original complaint was specifically about
/// this branch (they referenced `punches_attempted` before
/// `coordinator_addr()` resolution); verified fixed here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_punch_fast_fail_on_missing_coordinator_does_not_bump_stats() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, b_id, b_pub, bogus) = punch_topology_forced(NatClass::Cone, NatClass::Cone).await;

    let before = a.traversal_stats();
    let err = a
        .connect_direct(b_id, &b_pub, bogus)
        .await
        .expect_err("SinglePunch with unreachable coordinator must fail fast");
    match err {
        TraversalError::PeerNotReachable => {}
        other => panic!("expected PeerNotReachable, got {other:?}"),
    }
    let after = a.traversal_stats();
    assert_eq!(
        after, before,
        "fast-fail on missing coordinator must not bump \
         `punches_attempted` nor `relay_fallbacks`",
    );
}

/// Regression test for a cubic-flagged P2 bug: the stats
/// counters used to bump *before* the operation they were
/// counting actually succeeded, so a fully-failed
/// `connect_direct` could still move `relay_fallbacks` (and,
/// on SinglePunch, `punches_attempted`). That broke the
/// documented meanings of both counters — operators reading
/// them to gauge NAT-traversal effectiveness would get numbers
/// inflated by calls where no actual session was established
/// and no punch traffic went out.
///
/// The fix: `record_punch_attempt` fires only when
/// `request_punch` returns Ok (the wire `PunchRequest` was
/// sent AND the coordinator mediated an `Introduce`).
/// `record_relay_fallback` fires only when
/// `connect_via_coordinator` / `connect_routed` actually
/// succeed. Fully-failed calls leave the counters at zero.
///
/// # Scenario
///
/// Drive the SkipPunch branch with a live coordinator but
/// wipe that peer from the routing table so the relayed
/// handshake fails. Expect:
/// - `connect_direct` returns Err.
/// - `relay_fallbacks` stays unchanged (no handshake landed).
/// - All other counters stay unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_does_not_bump_relay_fallbacks_on_failed_fallback() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, _x) = punch_topology().await;

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

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
                && a_for_poll.peer_nat_class(b_id) == NatClass::Symmetric
        })
        .await,
        "A should see B's Symmetric class + reflex",
    );

    // Use an unreachable-peer pubkey so the relayed Noise
    // handshake fails. The coordinator r IS reachable and
    // IS a live peer — so `coordinator_addr()?` resolves and
    // the SkipPunch branch proceeds to `connect_via_coordinator`.
    // That call hands msg1 to r, but msg2 never comes back with
    // a useful handshake because the target b never receives
    // a valid msg1 (wrong static pubkey).
    let wrong_pubkey = [0u8; 32];

    let before = a.traversal_stats();
    let result = a.connect_direct(b_id, &wrong_pubkey, r.node_id()).await;
    let after = a.traversal_stats();

    assert!(
        result.is_err(),
        "connect_direct with wrong pubkey must fail — got {result:?}",
    );
    assert_eq!(
        after, before,
        "a fully-failed connect_direct must not bump any counters — \
         the pre-fix bug bumped `relay_fallbacks` before the \
         handshake ran, so a failed fallback still inflated the \
         'we're on the relay' signal",
    );
}

/// Regression test for a cubic-flagged P1 bug: the
/// `connect_via_coordinator` helper short-circuited on any
/// existing peer-map entry, so a second `connect_direct` call
/// that *should* have re-established the session via a
/// *different* coordinator silently returned the stale entry
/// still pointed at the first coordinator. Callers saw
/// "success" but the data plane still rode the previous path.
///
/// # Scenario
///
/// 1. Force Symmetric×Symmetric → pair_action is `SkipPunch`;
///    the only viable path is the routed one via the supplied
///    coordinator.
/// 2. First `connect_direct(..., coordinator = R)` establishes
///    a session; peer_addr ends up at R.
/// 3. Second `connect_direct(..., coordinator = X)` targets a
///    different coordinator. Under the pre-fix bug this would
///    short-circuit on `peers.contains_key` and return the R-
///    pathed session unchanged. Under the fix,
///    `session_matches(X.addr)` is false so the handshake runs
///    and the entry gets overwritten with an X-pathed session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_retargets_coordinator_does_not_short_circuit_on_stale_session() {
    use net::adapter::net::traversal::classify::NatClass;

    let (a, r, b, x) = punch_topology().await;

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

    let a_for_poll = a.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    assert!(
        wait_for(Duration::from_secs(3), || {
            a_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
                && a_for_poll.peer_nat_class(b_id) == NatClass::Symmetric
        })
        .await,
        "A should see B's Symmetric class + reflex",
    );

    let b_pub = *b.public_key();

    // First call: SkipPunch via R. Session should land on R.
    a.connect_direct(b_id, &b_pub, r.node_id())
        .await
        .expect("first connect_direct via R should succeed");
    assert_eq!(
        a.peer_addr(b_id),
        Some(r.local_addr()),
        "after first call, session should be pathed via R",
    );

    // Second call: different coordinator (X). Under the old
    // `contains_key` short-circuit this returned Ok without
    // touching the peers map; the caller thinks they're on X
    // but the data plane is still on R.
    a.connect_direct(b_id, &b_pub, x.node_id())
        .await
        .expect("second connect_direct via X should succeed");
    assert_eq!(
        a.peer_addr(b_id),
        Some(x.local_addr()),
        "after re-targeting to coordinator X, session transport \
         must move to X. If still pointed at R, the fix to \
         `connect_via_coordinator` regressed — it silently \
         short-circuited on the stale entry, masking the \
         caller's explicit request to re-home via X.",
    );
    assert_ne!(
        a.peer_addr(b_id),
        Some(r.local_addr()),
        "session transport must not still equal the old coordinator R",
    );
}
