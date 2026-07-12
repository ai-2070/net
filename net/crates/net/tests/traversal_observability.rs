//! Integration tests for `NAT_TRAVERSAL_V2_PLAN.md` Stage 5 —
//! observability: punch failure-reason counters and the reflex-diff
//! re-classification trigger (decisions 7 + 8).
//!
//! # Properties under test
//!
//! - **Each failure cause increments exactly its reason counter.**
//!   A typed coordinator rejection bumps `punch_rejections` (and
//!   nothing else); a silent-drop deadline elapse bumps
//!   `punch_timeouts`; a punch-needing pair with no coordinator
//!   candidate bumps `rendezvous_no_relay`.
//! - **`punches_failed` is derived** — mediated attempts minus
//!   successes; pre-mediation failures (rejections, introduce-wait
//!   timeouts) don't contribute.
//! - **Reflex drift triggers exactly one re-classify at re-announce.**
//!   Steady state (no drift) adds zero sweeps to the re-announce
//!   cadence; a drifted reflex is reconciled by one sweep so tag +
//!   reflex ship together; an active override pins state and skips
//!   the trigger entirely.
//!
//! Run: `cargo test --features net,nat-traversal --test traversal_observability`

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

/// `A ↔ R ↔ B` plus auxiliary X so A and B can classify (the sweep
/// needs ≥2 probe targets). Same shape as the rendezvous suites.
async fn rendezvous_topology() -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
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

// =========================================================================
// Failure-reason counters (decision 7)
// =========================================================================

/// A typed coordinator rejection increments `punch_rejections` and
/// nothing else: no timeout, no attempt, no derived failure. Forced
/// via the `unknown-target-reflex` reject — the target never
/// announced, so R has no cached reflex.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejection_increments_exactly_punch_rejections() {
    let (a, r, b, _x) = rendezvous_topology().await;

    // A announces (R needs A's reflex for the anti-reflection
    // check); B deliberately does NOT — that's the forced reject.
    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    let a_bind = a.local_addr();
    let r_for_poll = r.clone();
    let a_id = a.node_id();
    assert!(
        wait_for(Duration::from_secs(3), || {
            r_for_poll.peer_reflex_addr(a_id) == Some(a_bind)
        })
        .await,
        "R should see A's reflex before the request",
    );

    let result = a.request_punch(r.node_id(), b.node_id(), a_bind).await;
    match result {
        Err(TraversalError::RendezvousRejected(reason)) => {
            assert_eq!(reason, "unknown-target-reflex");
        }
        other => panic!("expected typed rejection, got {other:?}"),
    }

    let stats = a.traversal_stats();
    assert_eq!(stats.punch_rejections, 1, "exactly one rejection");
    assert_eq!(stats.punch_timeouts, 0, "a rejection is not a timeout");
    assert_eq!(stats.rendezvous_no_relay, 0, "a coordinator existed");
    assert_eq!(
        stats.punches_attempted, 0,
        "rejected before mediation — no attempt counted",
    );
    assert_eq!(
        stats.punches_failed, 0,
        "punches_failed derives from mediated attempts only",
    );
}

/// A silent drop on the coordinator increments `punch_timeouts` and
/// nothing else. Forced via the partition filter: R passes every
/// typed-reject branch (both endpoints announced) but drops the
/// fan-out because B's addr is partition-filtered — the one
/// remaining silent path, so A waits out the full `punch_deadline`.
///
/// Timing budget: ~punch_deadline (5 s default).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_drop_increments_exactly_punch_timeouts() {
    let (a, r, b, _x) = rendezvous_topology().await;

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
    let r_for_poll = r.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            r_for_poll.peer_reflex_addr(a_id) == Some(a_bind)
                && r_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
        })
        .await,
        "R should see both reflexes before the request",
    );

    // Partition B from R's perspective — handle_punch_request's
    // partition check silently drops the introduce fan-out.
    r.block_peer(b_bind);

    let start = tokio::time::Instant::now();
    let result = a.request_punch(r.node_id(), b_id, a_bind).await;
    let elapsed = start.elapsed();
    match result {
        Err(TraversalError::PunchFailed) => {}
        other => panic!("expected PunchFailed timeout, got {other:?}"),
    }
    assert!(
        elapsed >= Duration::from_secs(4),
        "silent drop must consume ~punch_deadline; elapsed {elapsed:?}",
    );

    let stats = a.traversal_stats();
    assert_eq!(stats.punch_timeouts, 1, "exactly one timeout");
    assert_eq!(stats.punch_rejections, 0, "no typed reject was sent");
    assert_eq!(stats.rendezvous_no_relay, 0, "a coordinator existed");
    assert_eq!(
        stats.punches_attempted, 0,
        "no introduce arrived — no attempt counted",
    );
}

/// A punch-needing pair with no coordinator candidate increments
/// `rendezvous_no_relay` and nothing else. Forced via a peerless
/// node whose class is pinned to `Cone` (Cone × Unknown →
/// `SinglePunch`, which needs a coordinator).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_relay_increments_exactly_rendezvous_no_relay() {
    let a = build_node().await;
    a.start();
    a.force_nat_class_for_test(NatClass::Cone);

    let phantom = 0xDEAD_BEEF_0000_9999u64;
    let dummy_pubkey = [7u8; 32];
    match a.connect_direct_auto(phantom, &dummy_pubkey).await {
        Err(TraversalError::RendezvousNoRelay) => {}
        other => panic!("expected RendezvousNoRelay, got {other:?}"),
    }

    let stats = a.traversal_stats();
    assert_eq!(stats.rendezvous_no_relay, 1, "exactly one no-relay skip");
    assert_eq!(stats.punch_timeouts, 0, "nothing waited on a deadline");
    assert_eq!(stats.punch_rejections, 0, "no coordinator to reject");
    assert_eq!(stats.punches_attempted, 0, "no punch was mediated");
    assert_eq!(stats.punches_failed, 0, "derived field stays zero");
}

// =========================================================================
// Reflex-diff re-classification trigger (decision 8, trigger 2)
// =========================================================================

/// The trigger's contract, driven directly: steady state → no sweep;
/// a drifted reflex → exactly one sweep that re-observes the truth;
/// immediately after → no further sweep (cadence guard).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reflex_drift_triggers_exactly_one_reclassify() {
    let (a, _r, _b, _x) = rendezvous_topology().await;

    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    let bind = a.local_addr();
    assert_eq!(
        a.reflex_addr(),
        Some(bind),
        "precondition: loopback classify observes the bind addr",
    );

    // Steady state: published == observed → no sweep.
    assert!(
        !a.reclassify_if_reflex_drifted().await,
        "no drift → the trigger must not add a sweep to the cadence",
    );

    // Drift the observed reflex (simulates a gateway reboot seen by
    // probe activity between announces).
    let fake: SocketAddr = "203.0.113.9:4444".parse().unwrap();
    a.set_reflex_for_test(fake);
    assert!(
        a.reclassify_if_reflex_drifted().await,
        "drifted reflex must trigger a sweep",
    );
    // The sweep re-observed reality: reflex back to the bind addr.
    assert_eq!(
        a.reflex_addr(),
        Some(bind),
        "the triggered sweep should re-observe the real reflex",
    );

    // Exactly once: the drift is reconciled, so no second sweep.
    assert!(
        !a.reclassify_if_reflex_drifted().await,
        "reconciled state must not trigger again",
    );
}

/// An active reflex override pins `(class, reflex)`; the trigger
/// must skip rather than fight the pin (the sweep would be
/// short-circuited anyway — skipping documents the intent).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reflex_override_skips_drift_trigger() {
    let (a, _r, _b, _x) = rendezvous_topology().await;
    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    // Override to an address that differs from the published reflex —
    // a drift the trigger would normally chase.
    let external: SocketAddr = "198.51.100.7:9999".parse().unwrap();
    a.set_reflex_override(external);
    assert!(
        !a.reclassify_if_reflex_drifted().await,
        "override pins (class, reflex) — the trigger must skip",
    );
    assert_eq!(
        a.reflex_addr(),
        Some(external),
        "the override must survive the (skipped) trigger",
    );
}

/// End-to-end wiring: the re-announce loop runs the trigger before
/// each periodic announce, so a drifted reflex never ships — the
/// published announcement carries the re-observed reflex and the
/// matching fresh `nat:*` tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reannounce_loop_corrects_drifted_reflex_before_publish() {
    // Short re-announce cadence so the test observes a tick quickly.
    let mut cfg = test_config();
    cfg.capability_reannounce_interval = Duration::from_millis(300);
    cfg.min_announce_interval = Duration::from_millis(0);
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    let x = build_node().await;
    let y = build_node().await;
    connect_pair(&a, &x).await;
    connect_pair(&a, &y).await;
    // start_arc: the re-announce loop needs the self-weak.
    a.start_arc();
    x.start();
    y.start();

    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    let bind = a.local_addr();

    // Drift, then wait for a re-announce tick to reconcile + publish.
    let fake: SocketAddr = "203.0.113.9:4444".parse().unwrap();
    a.set_reflex_for_test(fake);
    let a_poll = a.clone();
    let corrected = wait_for(Duration::from_secs(5), || {
        a_poll
            .local_announcement_for_test()
            .is_some_and(|ann| ann.reflex_addr == Some(bind))
    })
    .await;
    assert!(
        corrected,
        "the re-announce tick should reconcile the drift and publish \
         the re-observed reflex; got {:?}",
        a.local_announcement_for_test()
            .and_then(|ann| ann.reflex_addr),
    );
    // Tag + reflex shipped together: the same announcement carries
    // the fresh (loopback = open) class tag.
    let ann = a.local_announcement_for_test().expect("announcement");
    assert!(
        ann.capabilities.has_tag("nat:open"),
        "the corrected announcement must carry the matching nat:* tag",
    );
}
