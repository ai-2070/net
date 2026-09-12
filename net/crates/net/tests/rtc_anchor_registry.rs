//! Stage 4b: the anchor registry a `net-mesh anchor ls` / Deck
//! ANCHORS column reads.
//!
//! `rtc_addr` and `rtc_bootstrap` ride the capability announcement
//! (plan §11) but were not projected into the fold, so nothing could
//! *read* them back on a receiving node — 4a added `noise_pubkey`'s
//! projection and stopped there. These witnesses hold the two facts
//! an operator surface needs: an anchor that announces itself is
//! listed with the addresses it announced, and a peer that is not an
//! anchor is not listed at all.
//!
//! Run: `cargo test --features "webrtc fixtures cortex nat-traversal" --test rtc_anchor_registry`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::RtcConfig;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x3Du8; 32];

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(5));
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

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    predicate()
}

/// An anchor that announces `rtc-anchor` is listed by a peer that
/// heard it, **with the two addresses that make the role usable** —
/// and a peer that is not an anchor is not listed at all.
///
/// Inverse: drop the `rtc_addr` / `rtc_bootstrap` projection from
/// the capability fold bridge — the row appears with both addresses
/// `None`, and the operator sees an anchor with no way to reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_announced_anchor_is_listed_with_its_addresses_and_a_plain_peer_is_not() {
    let public: std::net::SocketAddr = "203.0.113.7:7101".parse().expect("addr");
    let anchor_rtc = RtcConfig {
        public_addr: Some(public),
        ..RtcConfig::new()
            .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
            .with_bootstrap_url("https://anchor.example.com")
    };
    let anchor = node(Some(anchor_rtc)).await;
    let observer = node(None).await;
    let plain = node(None).await;

    let anchor_id = anchor.node_id();
    let plain_id = plain.node_id();

    for (a, b) in [(&observer, &anchor), (&observer, &plain)] {
        let a_id = a.node_id();
        let b_clone = Arc::clone(b);
        let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
        a.connect(b.local_addr(), b.public_key(), b.node_id())
            .await
            .expect("udp handshake");
        accept.await.expect("accept task").expect("accept");
    }
    anchor.start_arc();
    observer.start_arc();
    plain.start_arc();
    // An announcement is emitted when a node announces; `start` alone
    // schedules the re-announce loop, whose first tick is far outside
    // a test's patience.
    for n in [&anchor, &observer, &plain] {
        n.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
            .await
            .expect("announce");
    }

    assert!(
        wait_for(
            || observer
                .rtc_anchors()
                .iter()
                .any(|row| row.node_id == anchor_id),
            Duration::from_secs(10)
        )
        .await,
        "the observer must learn the anchor from its signed announcement \
         (noise key seen: {:?}, rows: {:?})",
        observer.peer_announced_noise_pubkey(anchor_id).is_some(),
        observer.rtc_anchors().len()
    );

    let rows = observer.rtc_anchors();
    let row = rows
        .iter()
        .find(|row| row.node_id == anchor_id)
        .expect("the anchor row");
    assert_eq!(
        row.rtc_addr,
        Some(public),
        "the announced RTC socket is what a browser aims ICE at; a row without it \
         lists an anchor nobody can reach"
    );
    assert_eq!(
        row.rtc_bootstrap.as_deref(),
        Some("https://anchor.example.com"),
        "…and the bootstrap URL is the operator's configured one, not a synthesised \
         address"
    );
    assert_eq!(
        row.noise_pubkey,
        Some(*anchor.public_key()),
        "the key a browser pins travels on the same announcement"
    );

    assert!(
        !rows.iter().any(|row| row.node_id == plain_id),
        "a peer that never claimed the anchor role must not be listed as one"
    );
}

/// A node with no RTC configured announces no anchor role, so its
/// own listing is empty — the control that keeps the witness above
/// from passing on a registry that lists everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mesh_with_no_anchors_lists_none() {
    let a = node(None).await;
    let b = node(None).await;
    let a_id = a.node_id();
    let b_clone = Arc::clone(&b);
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b.local_addr(), b.public_key(), b.node_id())
        .await
        .expect("udp handshake");
    accept.await.expect("accept task").expect("accept");
    a.start_arc();
    b.start_arc();
    for n in [&a, &b] {
        n.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
            .await
            .expect("announce");
    }

    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(a.rtc_anchors().is_empty());
    assert!(b.rtc_anchors().is_empty());
}
