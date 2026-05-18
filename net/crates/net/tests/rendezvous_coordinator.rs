//! Integration tests for stage 3b of `docs/NAT_TRAVERSAL_PLAN.md`:
//! the rendezvous coordinator's fan-out behavior.
//!
//! Three nodes: A (punch initiator), R (coordinator), B (target).
//! A↔R and R↔B have direct sessions; A and B don't. A sends
//! `PunchRequest { target: B, self_reflex }` to R; R should:
//!
//! 1. Look up B's reflex in its capability index (populated by B's
//!    own `announce_capabilities` after stage 2's classification
//!    sweep).
//! 2. Fan out `PunchIntroduce` to both A and B with the
//!    respective counterpart's reflex and a shared `fire_at`.
//!
//! # Properties under test
//!
//! - **Fan-out success.** Both A and B receive `PunchIntroduce`.
//!   A's introduce carries `peer = B, peer_reflex = B's reflex`;
//!   B's introduce carries `peer = A, peer_reflex = A's reflex`.
//! - **Shared `fire_at`.** Both introductions carry the same
//!   `fire_at_ms` within a millisecond — required for a
//!   synchronized punch.
//! - **Missing reflex → drop.** If R has no cached reflex for B
//!   (B never announced, or B's announcement is absent from R's
//!   index), R drops the `PunchRequest`; A times out with
//!   `PunchFailed` inside `punch_deadline`.
//!
//! Stage 3c will build on top of this to schedule the keep-alive
//! train and finalize the session on the punched path. Stage 3b
//! only verifies the coordinator fan-out.
//!
//! Run: `cargo test --features net,nat-traversal --test rendezvous_coordinator`

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
        .with_handshake(3, Duration::from_secs(2));
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

/// Connect `a` to `b`. Neither side is `start()`ed afterward —
/// the caller batches handshakes then starts everyone at once.
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

/// Build a four-node topology: R at the center mediates A↔B,
/// plus an auxiliary X connected to A and B so both leaves have
/// ≥2 peers. The aux node is required because
/// [`MeshNode::reclassify_nat`] needs at least two probe targets
/// to produce a classification — without X, A and B would never
/// populate their `reflex_addr`, and R wouldn't have a cached
/// reflex to mediate with.
///
/// Returns `(A, R, B, X)`. X is only used as a classification
/// target; the punch itself still routes A → R → B.
async fn rendezvous_topology() -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node().await;
    let r = build_node().await;
    let b = build_node().await;
    let x = build_node().await;
    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    // X provides the second probe target for A and B's
    // classification sweeps. R also connects to X so X's own
    // announcements (and reclassification) are stable, though the
    // test doesn't depend on X's NAT state.
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    connect_pair(&r, &x).await;
    a.start();
    r.start();
    b.start();
    x.start();
    (a, r, b, x)
}

/// Wait up to `limit` for `check` to return true. Polls every
/// 50 ms. Used for settling cross-node state like a peer's
/// capability announcement reaching the coordinator's index.
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

/// Happy path: A asks R to punch to B. R has B's reflex cached
/// (B announced after classification). Both A and B receive
/// `PunchIntroduce` carrying the counterpart's reflex + a shared
/// `fire_at`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coordinator_fans_out_to_both_endpoints() {
    let (a, r, b, _x) = rendezvous_topology().await;

    // Both A and B classify + announce so R's index has their
    // reflex addresses. On localhost, reclassify → Open with
    // reflex = local_addr.
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    // R's capability index needs to absorb both announcements
    // before it can coordinate. Poll until B's reflex is
    // visible (proxy for "announcement has been indexed").
    let a_id = a.node_id();
    let b_id = b.node_id();
    let a_bind = a.local_addr();
    let b_bind = b.local_addr();
    // Poll until R's index has B's reflex populated — the
    // coordinator reads exactly that field when mediating.
    let r_for_poll = r.clone();
    let b_id_copy = b_id;
    let b_bind_copy = b_bind;
    let reflex_ready = wait_for(Duration::from_secs(3), || {
        let got = r_for_poll.peer_reflex_addr(b_id_copy);
        got == Some(b_bind_copy)
    })
    .await;
    assert!(
        reflex_ready,
        "R should see B's reflex in its capability index; got {:?}",
        r.peer_reflex_addr(b_id),
    );
    // Also R should have A's reflex (A announced too).
    assert_eq!(
        r.peer_reflex_addr(a_id),
        Some(a_bind),
        "R should see A's reflex too",
    );

    // B installs its waiter BEFORE A's PunchRequest lands, so the
    // dispatch branch finds a pending oneshot. A request_punch
    // installs the waiter atomically as part of the call.
    let b_clone = b.clone();
    let r_id = r.node_id();
    let b_wait = tokio::spawn(async move { b_clone.await_punch_introduce(a_id, r_id).await });

    // Give B a moment to register its waiter before A fires.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A sends the PunchRequest via R. Returns A's side of the
    // introduce.
    let a_intro = a
        .request_punch(r.node_id(), b_id, a_bind)
        .await
        .expect("A should receive PunchIntroduce");
    let b_intro = b_wait
        .await
        .expect("B wait task panicked")
        .expect("B should receive PunchIntroduce");

    // A's introduce carries B's identity + reflex.
    assert_eq!(a_intro.peer, b_id, "A's introduce.peer should be B");
    assert_eq!(
        a_intro.peer_reflex, b_bind,
        "A's introduce.peer_reflex should be B's reflex",
    );

    // B's introduce carries A's identity + reflex.
    assert_eq!(b_intro.peer, a_id, "B's introduce.peer should be A");
    assert_eq!(
        b_intro.peer_reflex, a_bind,
        "B's introduce.peer_reflex should be A's reflex",
    );

    // Shared fire_at: same millisecond tick (R computes once,
    // sends to both). Allow zero drift — R's single dispatch
    // call synthesizes a single `fire_at_ms`.
    assert_eq!(
        a_intro.fire_at_ms, b_intro.fire_at_ms,
        "A and B should see the same fire_at_ms (single coordinator compute)",
    );
}

/// Negative path: B never announces its reflex, so R can't
/// introduce. A's `request_punch` should time out with
/// `PunchFailed` inside `punch_deadline`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_punch_times_out_when_target_has_no_cached_reflex() {
    let (a, r, b, _x) = rendezvous_topology().await;

    // Only A announces — B stays unclassified so R has no reflex
    // for B. Don't call `b.reclassify_nat()` either: we want R's
    // index of B to be missing `reflex_addr`.
    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    let start = tokio::time::Instant::now();
    let result = a
        .request_punch(r.node_id(), b.node_id(), a.local_addr())
        .await;
    let elapsed = start.elapsed();

    match result {
        Err(TraversalError::PunchFailed) => {}
        other => panic!("expected PunchFailed, got {other:?}"),
    }
    // Default `punch_deadline` is 5 s. Must be within that
    // window — but not too fast, since the coordinator has no
    // explicit rejection message (stage 3b: silent drop on
    // missing reflex, A times out).
    assert!(
        elapsed >= Duration::from_secs(4),
        "should wait ~punch_deadline (5s) before failing; elapsed {elapsed:?}",
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "should not wait much past punch_deadline; elapsed {elapsed:?}",
    );
}

/// A calls `request_punch` with a `relay` node_id it has no
/// session with. Must fail fast with `PeerNotReachable`, never
/// hit the 5 s deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_punch_unknown_relay_fails_fast() {
    let a = build_node().await;
    a.start();

    let start = tokio::time::Instant::now();
    let err = a
        .request_punch(0xDEAD_BEEF, 0xCAFE, a.local_addr())
        .await
        .expect_err("unknown relay should fail");
    let elapsed = start.elapsed();

    match err {
        TraversalError::PeerNotReachable => {}
        other => panic!("expected PeerNotReachable, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_millis(500),
        "fast-fail took {elapsed:?}; want < 500 ms",
    );
}

/// Regression test for TEST_COVERAGE_PLAN §P1-4 case (a): B
/// announced at some point, R indexed B's reflex, but B's TTL
/// expired and R's capability-GC has since evicted B. When A
/// fires a PunchRequest, R must drop it silently — the
/// coordinator-side lookup at `capability_index.reflex_addr(b_id)`
/// returns None once GC has swept, same as if B had never
/// announced at all. A observes a `PunchFailed` timeout.
///
/// Case (b) — B never announced — is covered by the existing
/// `request_punch_times_out_when_target_has_no_cached_reflex`
/// above. Case (c) — GC racing the handler itself — isn't
/// exercised here: the handler + GC operate over DashMap, so
/// each entry-level read is atomic; a mid-handler eviction can
/// only cause the same observable outcome as a pre-handler
/// eviction (this test), not a torn dispatch state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_punch_times_out_when_targets_reflex_was_evicted_by_ttl_gc() {
    // Build R with a short GC interval so it sweeps frequently.
    // Default 60 s would keep B's entry indexed past any
    // practical test deadline.
    let r = {
        let cfg = test_config().with_capability_gc_interval(Duration::from_millis(100));
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("R build"),
        )
    };
    let a = build_node().await;
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

    // A announces normally (5-min default TTL, plenty of runway).
    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    // B classifies + announces with a TINY TTL (1 second). After
    // R indexes, its GC will evict within the sweep cadence.
    b.reclassify_nat().await;
    b.announce_capabilities_with(CapabilitySet::new(), Duration::from_secs(1), true)
        .await
        .expect("B short-TTL announce");

    // Wait for R to first see B's reflex — otherwise the test
    // reduces to "never announced" which is the existing test.
    let r_for_poll = r.clone();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    let indexed = wait_for(Duration::from_secs(3), || {
        r_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
    })
    .await;
    assert!(
        indexed,
        "R must index B's announcement before its TTL expires"
    );

    // Now wait for the TTL (1 s) + a GC cycle (100 ms) + margin
    // so R has definitely evicted B.
    tokio::time::sleep(Duration::from_millis(1_400)).await;
    assert!(
        r.peer_reflex_addr(b_id).is_none(),
        "R's capability-GC should have evicted B by now; got {:?}",
        r.peer_reflex_addr(b_id),
    );

    // A fires a PunchRequest against B. R's coordinator looks up
    // B's reflex, finds nothing, drops silently. A times out.
    let start = tokio::time::Instant::now();
    let result = a
        .request_punch(r.node_id(), b.node_id(), a.local_addr())
        .await;
    let elapsed = start.elapsed();

    match result {
        Err(TraversalError::PunchFailed) => {}
        other => panic!("expected PunchFailed after TTL eviction, got {other:?}"),
    }
    assert!(
        elapsed >= Duration::from_secs(4),
        "should wait ~punch_deadline (5s) before failing; elapsed {elapsed:?}",
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "should not wait much past punch_deadline; elapsed {elapsed:?}",
    );
}
