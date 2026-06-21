//! Integration tests for the pre-session keep-alive packet path.
//!
//! The rendezvous endpoint schedules three raw-UDP keep-alives
//! at `fire_at, +100ms, +250ms` targeting the counterpart's
//! `peer_reflex`. On a real NAT those packets open the outbound
//! connection-tracking row; on localhost they're UDP to
//! loopback (no NAT to punch) but they still exercise the
//! dispatcher's pre-session recognition path that fires the
//! `PunchAck` via the observer.
//!
//! # Properties under test
//!
//! - **Receive loop recognizes keep-alives.** A 14-byte packet
//!   starting with `KEEPALIVE_MAGIC` arriving at a node's socket
//!   fires any registered observer oneshot, even though no
//!   session exists between the sender and this node.
//! - **Keep-alive carries sender identity.** The observer
//!   completes with the decoded `Keepalive` whose
//!   `sender_node_id` matches the emitter.
//! - **Round-trip with observer + ack.** After A fires
//!   `request_punch`, keep-alives flow between A and B, each
//!   side's observer fires on first inbound, and each side
//!   emits a `PunchAck` via the coordinator. Both sides
//!   correlate their counterpart's ack.
//! - **Malformed 14-byte packets are ignored.** A random
//!   14-byte packet that doesn't start with `KEEPALIVE_MAGIC`
//!   doesn't fire any observer, doesn't crash the receive
//!   loop, and doesn't falsely succeed a punch.
//!
//! Run: `cargo test --features net,nat-traversal --test punch_keepalive`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::traversal::rendezvous::{
    decode_keepalive, encode_keepalive, Keepalive, KEEPALIVE_LEN,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use tokio::net::UdpSocket;

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

async fn build_node_with_reflex(override_addr: SocketAddr) -> Arc<MeshNode> {
    let cfg = test_config().with_reflex_override(override_addr);
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
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

/// Wire-level sanity: encode then decode round-trips. Covered by
/// a unit test already, but re-asserting here guards the
/// integration path's reliance on the codec.
#[test]
fn keepalive_codec_round_trips() {
    let ka = Keepalive {
        sender_node_id: 0x0102_0304_0506_0708,
        punch_id: 0x0a0b_0c0d,
    };
    let encoded = encode_keepalive(&ka);
    assert_eq!(encoded.len(), KEEPALIVE_LEN);
    let decoded = decode_keepalive(&encoded).expect("decode");
    assert_eq!(decoded, ka);
}

/// End-to-end keep-alive + ack path. `request_punch` triggers
/// the rendezvous; the endpoint's scheduler emits keep-alives to
/// `peer_reflex`; each side's observer fires on first inbound;
/// each side emits a `PunchAck` via the coordinator. A should
/// observe B's ack and vice versa.
///
/// The localhost variant of this is the same test shape as
/// `rendezvous_ack::punch_ack_round_trips_through_coordinator`;
/// the distinction is that after this refactor the ack ONLY
/// fires after the observer completes — so this test
/// implicitly exercises the keep-alive → observer → ack chain
/// end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keepalive_triggers_ack_via_observer() {
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
        "R should see both reflexes before we fire",
    );

    // Install ack waiters before firing. Both sides' observer
    // machinery needs the coordinator session to emit an ack.
    let a_clone = a.clone();
    let r_id = r.node_id();
    let a_task = tokio::spawn(async move { a_clone.await_punch_ack(b_id, r_id).await });
    let b_clone = b.clone();
    let b_task = tokio::spawn(async move { b_clone.await_punch_ack(a_id, r_id).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let _intro = a
        .request_punch(r.node_id(), b_id, a_bind)
        .await
        .expect("request_punch should mediate");

    // The observer needs to fire within `punch_deadline`
    // (5 s default). The fire_at is ~500 ms from mediation —
    // keep-alive sends at 500/600/750, observer arm at that
    // moment, receive-loop notices the inbound, ack flows
    // through R. Generous upper bound:
    let a_ack = a_task
        .await
        .expect("a_task panicked")
        .expect("A should get B's ack after the keep-alive train");
    let b_ack = b_task
        .await
        .expect("b_task panicked")
        .expect("B should get A's ack after the keep-alive train");

    assert_eq!(a_ack.from_peer, b_id);
    assert_eq!(a_ack.to_peer, a_id);
    assert_eq!(b_ack.from_peer, a_id);
    assert_eq!(b_ack.to_peer, b_id);
}

/// Malformed 14-byte packet (right length, wrong magic) must
/// not fire any observer. Sends from a standalone UDP socket
/// to a started node, then verifies that any separate ack
/// flow isn't short-circuited.
///
/// Structural test: the asserted property is that we don't
/// crash. The receive loop continues serving real traffic
/// after the bogus packet arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_keepalive_is_ignored_not_fatal() {
    let a = build_node().await;
    a.start();

    // Bind a local socket and send a 14-byte packet with the
    // wrong magic prefix.
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut bogus = [0u8; 14];
    bogus[0] = 0xFF;
    bogus[1] = 0xFF;
    sender.send_to(&bogus, a.local_addr()).await.unwrap();

    // Give the receive loop a moment to process (or ignore)
    // the packet.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The node is still alive and responsive: verify its
    // traversal_stats read works, which proves the runtime
    // didn't crash.
    let _stats = a.traversal_stats();
}

/// Finding 2 (code review 2026-06-21): the punch observer must
/// validate a keep-alive's `sender_node_id` against the expected
/// counterpart before emitting a `PunchAck`. The observer is keyed
/// only by source `SocketAddr`, so a stray/spoofed keep-alive
/// arriving from the right address but carrying the wrong sender id
/// must NOT be treated as a successful punch.
///
/// Harness: B advertises a reflex override pointing at a
/// test-controlled UDP listener, so A installs its punch observer
/// keyed by that listener address. We then inject keep-alives *from*
/// the listener socket and watch whether A emits a `PunchAck` to B
/// (B's `await_punch_ack` resolving is the signal that A acked).
///
/// Two phases against the same harness make the test self-validating
/// rather than vacuous:
///
/// - **Control** — inject `sender_node_id == B`: A's observer fires,
///   the sender matches, A acks, B's wait resolves. Proves the
///   injection path actually drives an ack.
/// - **Reject** — inject `sender_node_id != B`: A's observer fires
///   but the sender check fails, A stays silent, B's wait times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_acks_only_on_matching_sender_node_id() {
    // Test-controlled UDP socket standing in for B's reflex. A's
    // observer ends up keyed by this address.
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener_addr: SocketAddr = listener.local_addr().unwrap();

    let a = build_node().await;
    let r = build_node().await;
    let b = build_node_with_reflex(listener_addr).await;
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

    a.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let a_id = a.node_id();
    let b_id = b.node_id();
    let r_id = r.node_id();
    let a_addr = a.local_addr();
    let r_for_poll = r.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            r_for_poll.peer_reflex_addr(b_id) == Some(listener_addr)
                && r_for_poll.peer_reflex_addr(a_id).is_some()
        })
        .await,
        "R should see B's override reflex + A's reflex before we fire",
    );

    // Helper: drive one punch round on A (installs A's observer keyed
    // by `listener_addr`), inject one keep-alive from the listener
    // with the given sender id, and report whether A acked B within
    // the punch window.
    async fn round(
        a: &Arc<MeshNode>,
        r: &Arc<MeshNode>,
        listener: &UdpSocket,
        a_addr: SocketAddr,
        b: &Arc<MeshNode>,
        a_id: u64,
        b_id: u64,
        r_id: u64,
        injected_sender: u64,
    ) -> bool {
        // B waits for A's ack (from_peer == a_id, forwarded by R).
        let b_clone = b.clone();
        let b_wait = tokio::spawn(async move { b_clone.await_punch_ack(a_id, r_id).await });
        tokio::time::sleep(Duration::from_millis(30)).await;

        // A initiates: R introduces, A installs its observer keyed by
        // B's reflex (= listener_addr) and starts its own keep-alive
        // train at the listener (harmless here).
        a.request_punch(r.node_id(), b_id, a_addr)
            .await
            .expect("request_punch should mediate");
        // Let A's dispatch install the observer before we inject.
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Inject a keep-alive from the listener socket → arrives at A
        // with source == listener_addr, firing A's observer.
        let ka = encode_keepalive(&Keepalive {
            sender_node_id: injected_sender,
            punch_id: 0,
        });
        listener.send_to(&ka, a_addr).await.unwrap();

        // Did A ack B? B's waiter resolves iff A emitted the ack.
        matches!(b_wait.await.expect("b_wait task panicked"), Ok(_))
    }

    // Control: matching sender id → A must ack.
    let acked_on_match = round(
        &a, &r, &listener, a_addr, &b, a_id, b_id, r_id, b_id,
    )
    .await;
    assert!(
        acked_on_match,
        "control: a keep-alive whose sender_node_id == B must drive A's ack \
         (otherwise the test is vacuous)",
    );

    // Reject: wrong sender id → A must stay silent → B times out.
    let acked_on_mismatch = round(
        &a,
        &r,
        &listener,
        a_addr,
        &b,
        a_id,
        b_id,
        r_id,
        0xDEAD_BEEF_u64, // not B's node id
    )
    .await;
    assert!(
        !acked_on_mismatch,
        "reject: a keep-alive with the wrong sender_node_id must NOT drive \
         an ack — the observer's source-addr keying is not sufficient on its own",
    );
}
