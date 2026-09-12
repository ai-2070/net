//! H4: an RTC **relay** is not an RTC **target**.
//!
//! `peer_endpoint_is_rtc` read `PeerInfo::addr()` — where datagrams
//! go, which for a routed peer is the relay. In
//! X —UDP— R —RTC— Y, Y's routed session to X has an RTC next hop,
//! so Y classified **X** as `PairAction::Ice` and marked X's native
//! direct upgrade done. ICE had negotiated Y↔R, not Y↔X.
//!
//! The classifier now reads target-owned direct attachment
//! (`Direct { owned: PeerAddr::Rtc(_) }`) or the target's announced
//! `transport:rtc` tag, and never a routed session's relay
//! endpoint. No traversal redesign: the matrix and its inputs are
//! otherwise unchanged.
//!
//! Run: `cargo test --features "webrtc fixtures nat-traversal" --test rtc_classifier`
#![cfg(all(feature = "webrtc", feature = "fixtures", feature = "nat-traversal"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig};
use net::adapter::net::traversal::classify::PairAction;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, SocketBufferConfig};

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

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    predicate()
}

/// The mixed-next-hop topology: X reaches R over UDP, R reaches Y
/// over a DataChannel, and X runs a routed handshake to Y through R.
/// Y's session to X therefore has an **RTC relay** and a **UDP
/// target**.
///
/// Y must classify X by the classic matrix and keep X upgradeable;
/// a genuinely direct RTC peer (R, which owns a DataChannel to Y) is
/// the control that still classifies `Ice`.
///
/// Inverse: classify on `PeerInfo::addr()` again (the send endpoint)
/// — X is classified `Ice` off R's RTC next hop and this fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_rtc_relay_does_not_make_a_udp_target_an_ice_pair() {
    // X has no RTC at all; R and Y do.
    let x = node(None).await;
    let r = node(Some(rtc_config())).await;
    let y = node(Some(rtc_config())).await;

    connect_udp(&x, &r).await;
    x.start();
    r.start();
    y.start();

    // R ↔ Y over a DataChannel: this is the RTC leg, and the only
    // one.
    let (_id_r, id_y) = connect_rtc_loopback(&r, &y).await.expect("rtc pair");
    assert_eq!(
        y.peer_endpoint(r.node_id()),
        Some(PeerAddr::Rtc(id_y)),
        "Y reaches R over the DataChannel"
    );

    // X → R → Y: a routed handshake, so Y installs a routed session
    // for X whose relay endpoint is R's RTC handle.
    let y_pub = *y.public_key();
    let y_id = y.node_id();
    let x_id = x.node_id();
    x.connect_via(r.local_addr(), &y_pub, y_id)
        .await
        .expect("routed handshake through the relay");
    assert!(
        wait_for(|| y.peer_endpoint(x_id).is_some(), Duration::from_secs(10)).await,
        "Y must have installed its side of the routed session"
    );

    // The premise: Y's *send endpoint* for X is an RTC handle…
    assert!(
        matches!(y.peer_endpoint(x_id), Some(PeerAddr::Rtc(_))),
        "the schedule requires X's next hop from Y to be the RTC relay; got {:?}",
        y.peer_endpoint(x_id)
    );
    // …and X is nevertheless not an ICE pair for Y.
    assert_ne!(
        y.pair_action_for_test(x_id),
        PairAction::Ice,
        "an RTC relay is not an RTC target: ICE negotiated Y↔R, not Y↔X, so \
         classifying X as Ice marks a native upgrade done that never happened"
    );

    // The control: R genuinely owns a DataChannel to Y.
    assert_eq!(
        y.pair_action_for_test(r.node_id()),
        PairAction::Ice,
        "a target that owns a direct RTC attachment is still an ICE pair"
    );
}
