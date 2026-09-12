//! H2: the RTC install path is a **commit fence**, not a sequence of
//! prechecks.
//!
//! Kyra established three schedules from source, and showed that the
//! landed dead-handle and busy-incumbent fixtures pass with their
//! guards removed — they exercise independent refusal paths
//! (a handshake that cannot complete at all), never the install race.
//!
//! Every witness here uses a **pausable completed exchange**: the
//! DataChannel is open, Noise finishes, and the installer parks at
//! `MeshNode::rtc_install_pause` with its keys in hand. The
//! interference then happens in that gap, which is the window the
//! prechecks cannot see.
//!
//! Run: `cargo test --features "webrtc fixtures" --test rtc_install_race`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use std::net::SocketAddr;

use net::adapter::net::rtc::{open_rtc_channel, RtcConfig};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, Reliability, SocketBufferConfig,
    StreamConfig,
};

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32]);
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.rtc = rtc;
    cfg
}

async fn node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(rtc))
            .await
            .expect("MeshNode::new"),
    )
}

fn rtc_config() -> RtcConfig {
    RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

/// Two started nodes with RTC drivers.
async fn pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    a.start();
    b.start();
    (a, b)
}

/// A and B plus a UDP relay R, so a **competing incarnation** can be
/// installed for B while an RTC install is parked. `connect`/
/// `accept` cannot serve: the UDP responder reads the socket
/// directly and is a pre-`start()` API, and a second DataChannel
/// between the same socket pair collides in ICE. A routed handshake
/// through R is a genuine authenticated session, installable
/// repeatedly on started nodes.
async fn trio() -> (Arc<MeshNode>, Arc<MeshNode>, SocketAddr) {
    let a = node(Some(rtc_config())).await;
    let r = node(None).await;
    let b = node(Some(rtc_config())).await;
    connect_udp(&a, &r).await;
    connect_udp(&r, &b).await;
    a.start();
    r.start();
    b.start();
    let r_addr = r.local_addr();
    // The relay must outlive the witness; leak it into the returned
    // tuple's lifetime by keeping it alive in a task-local Arc.
    std::mem::forget(r);
    (a, b, r_addr)
}

async fn connect_udp(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = Arc::clone(b);
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
}

/// A second, genuine session between the same two started nodes:
/// another DataChannel carrying its own Noise exchange through the
/// ordinary installer. This is the "competing incarnation" the
/// install race is about — a real authenticated session, not a
/// fabricated peer record. (`connect`/`accept` cannot serve here:
/// the UDP responder reads the socket directly and is a
/// pre-`start()` API.)
async fn install_competing_session(a: &Arc<MeshNode>, b: &Arc<MeshNode>, relay: SocketAddr) -> u64 {
    let b_pub = *b.public_key();
    a.connect_via(relay, &b_pub, b.node_id())
        .await
        .expect("routed handshake through the relay");
    a.peer_session_id(b.node_id())
        .expect("the competing session")
}

/// Start the RTC Noise exchange with A's install parked at the seam.
///
/// Returns A's endpoint handle and the two handshake tasks. The
/// responder is left to complete normally: the contract under test is
/// the initiator's commit unless a witness says otherwise.
async fn park_initiator_install(
    a: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
) -> (
    net::adapter::net::rtc::RtcPeerId,
    tokio::task::JoinHandle<Result<u64, net::error::AdapterError>>,
    tokio::task::JoinHandle<Result<u64, net::error::AdapterError>>,
) {
    let (id_a, id_b) = open_rtc_channel(a, b).await.expect("datachannel");
    a.rtc_install_pause().arm_once();

    let responder = {
        let b = Arc::clone(b);
        let a_id = a.node_id();
        tokio::spawn(async move { b.accept_rtc(id_b, a_id).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let initiator = {
        let a = Arc::clone(a);
        let b_pub = *b.public_key();
        let b_id = b.node_id();
        tokio::spawn(async move { a.connect_rtc(id_a, &b_pub, b_id).await })
    };

    // The exchange has COMPLETED and the installer is holding its
    // keys at the seam. Everything a witness does after this point
    // happens strictly between the prechecks and the commit.
    tokio::time::timeout(
        Duration::from_secs(10),
        a.rtc_install_pause().wait_until_reached(),
    )
    .await
    .expect("the initiator must reach the install seam");
    (id_a, initiator, responder)
}

/// H2 schedule 1: the endpoint is closed — and its close notification
/// **consumed** — while a completed exchange waits to install. The
/// pre-repair check read `is_open` and released; nothing revalidated
/// at commit, so the dead endpoint was published and no eviction was
/// left to remove it.
///
/// Inverse: drop the `intent.still_live()` re-check from
/// `install_peer_locked` (or go back to `require_live_rtc_endpoint`
/// before the commit) — the dead endpoint installs and this fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_close_consumed_before_the_commit_refuses_the_dead_endpoint() {
    let (a, b) = pair().await;
    let b_id = b.node_id();
    let (id_a, initiator, responder) = park_initiator_install(&a, &b).await;

    // Close the exact endpoint the parked install is holding, and
    // let the mesh consume the close notification — with no reverse
    // index yet, the eviction finds nothing to remove, which is the
    // whole point of the schedule.
    let driver = a.rtc_driver().expect("driver").clone();
    driver.close(id_a).await.expect("close");
    assert!(
        wait_for(|| !driver.transport().is_open(id_a), Duration::from_secs(5)).await,
        "the endpoint must actually be closed before the install resumes"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    a.rtc_install_pause().release();
    let result = initiator.await.expect("initiator task");

    assert!(
        result.is_err(),
        "an install whose endpoint closed during the handshake must be refused"
    );
    assert_eq!(
        a.peer_endpoint(b_id),
        None,
        "nothing may be published for a dead endpoint — there is no close \
         notification left to evict it"
    );
    let _ = responder.await;
}

/// The same fence on the **responder** branch: `accept_rtc` commits
/// through the same installer and must revalidate the same way.
///
/// Inverse: the same one — the responder installs a dead endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_close_before_the_responder_commits_refuses_the_dead_endpoint() {
    let (a, b) = pair().await;
    let a_id = a.node_id();
    let (id_a, id_b) = open_rtc_channel(&a, &b).await.expect("datachannel");
    b.rtc_install_pause().arm_once();

    let responder = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.accept_rtc(id_b, a_id).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let initiator = {
        let a = Arc::clone(&a);
        let b_pub = *b.public_key();
        let b_id = b.node_id();
        tokio::spawn(async move { a.connect_rtc(id_a, &b_pub, b_id).await })
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        b.rtc_install_pause().wait_until_reached(),
    )
    .await
    .expect("the responder must reach the install seam");

    let driver_b = b.rtc_driver().expect("driver").clone();
    driver_b.close(id_b).await.expect("close");
    assert!(
        wait_for(
            || !driver_b.transport().is_open(id_b),
            Duration::from_secs(5)
        )
        .await,
        "the responder's endpoint must be closed before its install resumes"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    b.rtc_install_pause().release();
    let result = responder.await.expect("responder task");
    assert!(
        result.is_err(),
        "the responder must refuse to install on a closed endpoint too"
    );
    assert_eq!(
        b.peer_endpoint(a_id),
        None,
        "the responder must publish nothing for the dead endpoint"
    );
    let _ = initiator.await;
}

/// H2 schedule 2: an **absent** snapshot is an expectation, not
/// permission. The RTC caller sampled "no incumbent" and passed the
/// shared installer a `None`, which that helper reads as
/// *unconditional replacement* — so a session installed while Noise
/// was in flight was overwritten by the older attempt.
///
/// Inverse: map the absent snapshot back to `PriorSession::Any`
/// (i.e. `None`) — the RTC install overwrites the UDP session
/// installed during the handshake and this fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_absent_snapshot_cannot_overwrite_a_session_installed_during_the_handshake() {
    let (a, b, relay) = trio().await;
    let b_id = b.node_id();
    let (_id_a, initiator, responder) = park_initiator_install(&a, &b).await;

    // A competing, genuine incarnation installs while the RTC
    // attempt is parked.
    let competing = install_competing_session(&a, &b, relay).await;

    a.rtc_install_pause().release();
    let result = initiator.await.expect("initiator task");

    assert!(
        result.is_err(),
        "an install that expected NO incumbent must not overwrite one that arrived"
    );
    assert_eq!(
        a.peer_session_id(b_id),
        Some(competing),
        "the session installed during the handshake must survive"
    );
    assert!(
        matches!(a.peer_endpoint(b_id), Some(PeerAddr::Udp(_))),
        "the surviving routed session keeps its own endpoint, not the loser's"
    );
    let _ = responder.await;
}

/// H2 schedule 2, the expected-present half: the incumbent the caller
/// sampled is replaced by a newer incarnation during the handshake,
/// so the compare-and-swap must lose.
///
/// Inverse: remove the `Exactly` arm's session-id comparison — the
/// stale attempt clobbers the newer session.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_expected_present_snapshot_loses_to_a_newer_incarnation() {
    let (a, b, relay) = trio().await;
    let b_id = b.node_id();
    let first = install_competing_session(&a, &b, relay).await;

    let (_id_a, initiator, responder) = park_initiator_install(&a, &b).await;

    // A fresh handshake replaces the incumbent the parked install
    // sampled.
    let second = install_competing_session(&a, &b, relay).await;
    assert_ne!(first, second, "the replacement must be a new incarnation");

    a.rtc_install_pause().release();
    let result = initiator.await.expect("initiator task");

    assert!(
        result.is_err(),
        "an install holding a stale expected session id must lose the swap"
    );
    assert_eq!(
        a.peer_session_id(b_id),
        Some(second),
        "the newer incarnation must survive the loser's commit"
    );
    let _ = responder.await;
}

/// H2 schedule 3: quiescence is a **commit-time** property. A stream
/// opened on the incumbent during the handshake does not change its
/// session id, so the id-only compare-and-swap happily replaced a
/// session that had become busy — losing exactly the in-flight state
/// the gate exists to protect.
///
/// Inverse: delete the `require_quiescent` re-check in
/// `install_peer_locked` — the busy incumbent is replaced and its
/// stream disappears.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_incumbent_that_becomes_busy_during_the_handshake_is_preserved() {
    let (a, b, relay) = trio().await;
    let b_id = b.node_id();
    let incumbent = install_competing_session(&a, &b, relay).await;

    let (_id_a, initiator, responder) = park_initiator_install(&a, &b).await;

    // Quiet at the snapshot, busy at the commit.
    let _stream = a
        .open_stream(
            b_id,
            0x0D77,
            StreamConfig {
                reliability: Reliability::Reliable,
                ..StreamConfig::new()
            },
        )
        .expect("open a stream on the incumbent");
    let stream_id = 0x0D77u64;

    a.rtc_install_pause().release();
    let result = initiator.await.expect("initiator task");

    assert!(
        result.is_err(),
        "an incumbent that became busy during the handshake must not be replaced"
    );
    assert_eq!(
        a.peer_session_id(b_id),
        Some(incumbent),
        "the busy session must survive"
    );
    assert!(
        a.peer_session_for_test(b_id)
            .is_some_and(|s| s.stream_ids().contains(&stream_id)),
        "and so must the stream that made it busy — that state is the reason \
         the gate exists"
    );
    let _ = responder.await;
}

/// The control that gives the four refusals meaning: a genuinely
/// quiet incumbent, the same parked completed exchange, no
/// interference — the upgrade lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_quiet_incumbent_is_replaced_by_the_parked_exchange() {
    let (a, b, relay) = trio().await;
    let b_id = b.node_id();
    let incumbent = install_competing_session(&a, &b, relay).await;

    let (id_a, initiator, responder) = park_initiator_install(&a, &b).await;
    a.rtc_install_pause().release();

    let result = initiator.await.expect("initiator task");
    assert!(
        result.is_ok(),
        "the fence must not refuse an install nothing interfered with: {result:?}"
    );
    assert_eq!(
        a.peer_endpoint(b_id),
        Some(PeerAddr::Rtc(id_a)),
        "the quiet incumbent is replaced by the RTC endpoint"
    );
    assert_ne!(
        a.peer_session_id(b_id),
        Some(incumbent),
        "replacement means a new incarnation, not the old session at a new address"
    );
    let _ = responder.await;
}
