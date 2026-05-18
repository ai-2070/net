//! Integration tests for stage 3d of `docs/NAT_TRAVERSAL_PLAN.md`:
//! the full `PunchAck` round-trip and the coordinator's ack-
//! forwarding role.
//!
//! Three-message dance, completed end-to-end:
//!
//! ```text
//! A ─ PunchRequest ─> R
//! R ─ PunchIntroduce ─> A
//! R ─ PunchIntroduce ─> B
//! A ─ PunchAck {from: A, to: B} ─> R
//! B ─ PunchAck {from: B, to: A} ─> R
//! R ─ PunchAck {from: A, to: B} ─> B  (forwarded)
//! R ─ PunchAck {from: B, to: A} ─> A  (forwarded)
//! ```
//!
//! # Properties under test
//!
//! - **Round-trip correlation.** After A fires `request_punch`, A
//!   observes a `PunchAck` with `from_peer = B, to_peer = A`. The
//!   ack reaches A via R's forwarding logic — A has no direct
//!   session to B.
//! - **Coordinator forwards by `to_peer`.** R inspects the
//!   arriving ack's `to_peer` field; when it's not R, R sends
//!   the ack to that peer's session. Forwarding preserves the
//!   `from_peer` identity intact.
//! - **Late acks are dropped, not crashed.** A `PunchAck` with
//!   no installed waiter drops silently. No panic, no state
//!   corruption.
//!
//! Stage 3d doesn't yet include the real keep-alive train — on
//! localhost the PunchAck is auto-emitted immediately on
//! `PunchIntroduce` receipt, which is semantically equivalent
//! to the "first-inbound" trigger when there's no NAT to punch
//! through.
//!
//! Run: `cargo test --features net,nat-traversal --test rendezvous_ack`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
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

/// Four-node topology (A, R, B, X) matching the other stage-3
/// tests. X is a classification helper — both A and B need
/// ≥2 peers for `reclassify_nat` to produce a reflex.
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

/// End-to-end: A fires `request_punch` via R. A should observe
/// B's `PunchAck` arriving via R's forwarding. The ack's
/// `from_peer` identifies B, `to_peer` identifies A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn punch_ack_round_trips_through_coordinator() {
    let (a, r, b, _x) = rendezvous_topology().await;

    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    // R needs B's reflex cached before it can mediate.
    let a_bind = a.local_addr();
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    let r_for_poll = r.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            r_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
        })
        .await,
        "R should see B's reflex",
    );

    // Install A's PunchAck waiter BEFORE firing the request so
    // the round-trip can't beat the await onto the map.
    let a_clone = a.clone();
    let r_id = r.node_id();
    let ack_task = tokio::spawn(async move { a_clone.await_punch_ack(b_id, r_id).await });
    // Let the waiter register before triggering the flow.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let _intro = a
        .request_punch(r.node_id(), b_id, a_bind)
        .await
        .expect("request_punch should succeed");

    let ack = ack_task
        .await
        .expect("ack task panicked")
        .expect("PunchAck should arrive via R's forwarding");

    assert_eq!(ack.from_peer, b_id, "ack.from_peer should name B");
    assert_eq!(ack.to_peer, a_id, "ack.to_peer should name A");
}

/// Both endpoints receive their counterpart's ack. Guards
/// against a forwarding bug where R only relays in one
/// direction (e.g. forgets to forward A's ack to B, and vice
/// versa).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_endpoints_see_counterpart_ack() {
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
            r_for_poll.peer_reflex_addr(b_id) == Some(b_bind)
                && r_for_poll.peer_reflex_addr(a_id) == Some(a_bind)
        })
        .await,
        "R should see both reflexes",
    );

    // Both sides install waiters. A waits on B's ack; B waits
    // on A's ack. Both are forwarded by R.
    let a_clone = a.clone();
    let r_id = r.node_id();
    let a_task = tokio::spawn(async move { a_clone.await_punch_ack(b_id, r_id).await });
    let b_clone = b.clone();
    let b_task = tokio::spawn(async move { b_clone.await_punch_ack(a_id, r_id).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let _intro = a
        .request_punch(r.node_id(), b_id, a_bind)
        .await
        .expect("request_punch should succeed");

    let a_ack = a_task
        .await
        .expect("a_task panicked")
        .expect("A should see B's ack");
    let b_ack = b_task
        .await
        .expect("b_task panicked")
        .expect("B should see A's ack");

    assert_eq!(a_ack.from_peer, b_id);
    assert_eq!(a_ack.to_peer, a_id);
    assert_eq!(b_ack.from_peer, a_id);
    assert_eq!(b_ack.to_peer, b_id);
}

/// Peer-auth regression: a forged `PunchAck` from a session
/// peer that is NOT the recorded coordinator must NOT complete
/// the local node's pending ack waiter. Pre-binding, any session
/// peer could ship `PunchAck { from_peer: <victim>, to_peer:
/// <local> }` and the local `connect_direct` future would
/// resolve with the attacker's payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn punch_ack_forged_by_non_coordinator_session_peer_is_dropped() {
    use net::adapter::net::traversal::rendezvous::{PunchAck, RendezvousMsg};
    use net::adapter::net::traversal::SUBPROTOCOL_RENDEZVOUS;

    let (a, r, b, x) = rendezvous_topology().await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let r_id = r.node_id();

    // A installs an ack waiter expecting forwarding via R. The
    // gate at the dispatch arm requires `from_node == r_id`
    // before completing the oneshot.
    let a_clone = a.clone();
    let ack_task = tokio::spawn(async move { a_clone.await_punch_ack(b_id, r_id).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // X forges a PunchAck claiming to come from B. X has a
    // legitimate session with A (rendezvous_topology connects
    // every pair) so the encrypted send path lands.
    let forged = RendezvousMsg::PunchAck(PunchAck {
        from_peer: b_id,
        to_peer: a_id,
        punch_id: 0,
    })
    .encode();
    x.send_subprotocol(a.local_addr(), SUBPROTOCOL_RENDEZVOUS, &forged)
        .await
        .expect("forged subprotocol send should reach a");

    // The forged ack must NOT resolve A's waiter. await with a
    // short fudge — the deadline-based PunchFailed is the
    // expected outcome; a successful Ok(PunchAck) would mean the
    // gate failed.
    let outcome = tokio::time::timeout(Duration::from_secs(7), ack_task).await;
    let ack_result = outcome
        .expect("ack task should finish within the punch_deadline window")
        .expect("ack task panicked");
    assert!(
        ack_result.is_err(),
        "forged ack from non-coordinator session peer must not complete the waiter"
    );
}

/// Punch target isn't in R's index → A gets a timeout, and A's
/// own ack waiter times out too (no counterpart to ack).
/// Timing budget: ~punch_deadline (5s default).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ack_wait_times_out_when_punch_uncoordinated() {
    let (a, r, b, _x) = rendezvous_topology().await;

    // A classifies + announces; B deliberately does NOT, so R's
    // index has no reflex for B.
    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    let b_id = b.node_id();

    let a_clone = a.clone();
    let r_id = r.node_id();
    let ack_task = tokio::spawn(async move { a_clone.await_punch_ack(b_id, r_id).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    // request_punch will itself time out because R has no
    // reflex for B. Running the two concurrently makes the
    // test bounded by the longer of the two deadlines.
    let a_bind = a.local_addr();
    let (req_result, ack_result) =
        tokio::join!(a.request_punch(r.node_id(), b_id, a_bind), async {
            ack_task.await.expect("ack task panicked")
        },);

    assert!(req_result.is_err(), "request_punch should fail");
    assert!(ack_result.is_err(), "ack waiter should time out");
}
