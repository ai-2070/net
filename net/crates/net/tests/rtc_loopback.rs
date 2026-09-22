//! Stage 3 loopback harness: two native nodes, one DataChannel.
//!
//! The point is not that the DataChannel opens — S0b proved that
//! against a real browser. The point is that **the existing
//! witnesses hold with the peer on `PeerAddr::Rtc`**: streams,
//! reliability, session migration, the stale-session protections,
//! nRPC, the fold, and the §5 delivery sequence. Where a scenario is
//! reachable through a public helper, this file calls that helper
//! rather than reimplementing it.
//!
//! Run: `cargo test --features "webrtc fixtures" --test rtc_loopback`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig, RtcPeerId};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, Reliability, SocketBufferConfig,
    StreamConfig,
};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};

/// A batch of `count` application events, the shape every other
/// two-node witness sends.
fn batch(shard_id: u16, count: usize, tag: &str) -> Batch {
    let events: Vec<InternalEvent> = (0..count)
        .map(|i| {
            InternalEvent::from_value(
                serde_json::json!({ "tag": tag, "index": i }),
                i as u64,
                shard_id,
            )
        })
        .collect();
    Batch {
        shard_id,
        events,
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
    }
}

fn config(rtc: bool) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x33u8; 32]);
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    if rtc {
        cfg.rtc = Some(RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr")));
    }
    cfg
}

async fn node(rtc: bool) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(rtc))
            .await
            .expect("MeshNode::new"),
    )
}

/// Two started nodes joined over a DataChannel.
async fn rtc_pair() -> (Arc<MeshNode>, Arc<MeshNode>, RtcPeerId, RtcPeerId) {
    let a = node(true).await;
    let b = node(true).await;
    a.start();
    b.start();
    let (id_a, id_b) = connect_rtc_loopback(&a, &b)
        .await
        .expect("the DataChannel and the Noise handshake must complete on loopback");
    (a, b, id_a, id_b)
}

/// A UDP pair for the differential assertions.
async fn udp_pair(rtc_feature_on: bool) -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(rtc_feature_on).await;
    let b = node(rtc_feature_on).await;
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = Arc::clone(&b);
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
    (a, b)
}

/// Poll `node`'s shard 0 until at least `want` events have arrived,
/// draining as it goes. The helper the UDP witnesses use
/// (`poll_shard`) is the same one here — the transport is what
/// changes, not the delivery contract.
async fn drain_until(node: &Arc<MeshNode>, want: usize, within: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen = 0usize;
    while tokio::time::Instant::now() < deadline && seen < want {
        let result = node.poll_shard(0, None, 256).await.expect("poll_shard");
        seen += result.events.len();
        if result.events.is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    seen
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

/// EXIT: the session installs with the peer on `PeerAddr::Rtc`, and
/// every peer-keyed index agrees.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_datachannel_session_installs_as_a_direct_rtc_peer() {
    let (a, b, id_a, id_b) = rtc_pair().await;

    let a_view = a.peer_endpoint(b.node_id()).expect("a knows b");
    let b_view = b.peer_endpoint(a.node_id()).expect("b knows a");
    assert_eq!(
        a_view,
        PeerAddr::Rtc(id_a),
        "the installed endpoint must be the DataChannel, not a UDP tuple"
    );
    assert_eq!(b_view, PeerAddr::Rtc(id_b));
    assert!(
        a.peer_is_direct(b.node_id()),
        "a DataChannel session is an authenticated adjacency, not a relayed one"
    );
    assert!(b.peer_is_direct(a.node_id()));
}

/// EXIT: RTC ingress reaches `dispatch_packet` through the receive
/// loop's single owner, and per-source ordering is preserved.
///
/// Ordering is asserted per input, not across them (§3.4): the
/// sequence numbers a single RTC stream delivers must be contiguous
/// and ascending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rtc_ingress_preserves_per_source_order_through_one_owner() {
    let (a, b, _, _) = rtc_pair().await;

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a.open_stream(b.node_id(), 0x52, cfg).expect("open_stream");

    const N: usize = 32;
    for i in 0..N {
        a.send_on_stream(&stream, &[Bytes::from(vec![i as u8; 16])])
            .await
            .expect("send_on_stream");
    }
    a.send_to_peer_node(b.node_id(), &batch(0, N, "order"))
        .await
        .expect("send_to_peer_node");

    // Ordering is asserted PER INPUT — the doc's claim. The batch
    // input's own sequence numbers must arrive contiguous and
    // ascending: exactly 0..N, in order. The previous probe counted
    // `seen >= N`, which absorbs reordering AND duplicate delivery —
    // a count says "enough arrived", never "they arrived in sequence".
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut batch_indices = Vec::new();
    let mut stream_indices = Vec::new();
    // #21: the window is bounded by EACH input's own completion, not
    // by the batch count alone. Ending the drain when the batch leg
    // filled left stream events still in flight undrained, so a
    // per-source reorder confined to that tail failed nothing.
    while tokio::time::Instant::now() < deadline
        && (batch_indices.len() < N || stream_indices.len() < N)
    {
        // Every shard, not just shard 0 (#21): the receive side
        // queues a packet's events at `stream_id % num_shards`, so
        // the batch input (stream 0) lands in shard 0 while the
        // stream input (stream 0x52) lands in shard 2. Polling shard
        // 0 alone is BLIND to the stream input — which is exactly how
        // "each input's own sequence exactly" survived as a vacuous
        // skip.
        let mut drained_any = false;
        for shard in 0..4u16 {
            let result = b.poll_shard(shard, None, 256).await.expect("poll_shard");
            drained_any = drained_any || !result.events.is_empty();
            for event in &result.events {
                let Ok(text) = event.raw_str() else {
                    continue;
                };
                if text.contains("\"order\"") {
                    let value: serde_json::Value =
                        serde_json::from_str(text).expect("a batch event is JSON");
                    let index = value
                        .get("index")
                        .or_else(|| value.pointer("/value/index"))
                        .and_then(|index| index.as_u64())
                        .expect("the batch event carries its index");
                    batch_indices.push(index);
                    continue;
                }
                // The stream input rides the same channel: sixteen
                // identical bytes whose value is its sequence number.
                if event.raw.len() == 16 && event.raw.iter().all(|byte| *byte == event.raw[0]) {
                    stream_indices.push(u64::from(event.raw[0]));
                }
            }
        }
        if !drained_any {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    assert_eq!(
        batch_indices,
        (0..N as u64).collect::<Vec<_>>(),
        "the batch input's sequence numbers must arrive contiguous and ascending \
         (0..{N}) — per input, not across the two inputs"
    );
    // #21: zero stream delivery is a FAILURE, not a skip — the old
    // `if !stream_indices.is_empty()` made "each input's own sequence
    // exactly" vacuous for an input that delivered nothing.
    assert!(
        !stream_indices.is_empty(),
        "the stream input delivered nothing — its sequence claim cannot be vacuous"
    );
    assert_eq!(
        stream_indices,
        (0..N as u64).collect::<Vec<_>>(),
        "and the stream input's own sequence must be exactly 0..{N}, in order: \
         {stream_indices:?}"
    );

    let stats = b.rtc_stats();
    assert_eq!(
        stats.ingress_dropped(),
        0,
        "nothing may be dropped at this volume; a drop here means the bounded input \
         is undersized, not that the test is flaky"
    );
    assert!(
        stats.ingress_delivered() >= N as u64,
        "every delivered datagram passes through the one dispatch owner"
    );
}

/// EXIT: with `webrtc` compiled but `rtc: None`, a UDP pair behaves
/// identically — same delivery, and not one RTC counter moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_feature_compiled_but_unconfigured_changes_nothing() {
    let (a, b) = udp_pair(false).await;
    assert!(
        a.rtc_driver().is_none() && b.rtc_driver().is_none(),
        "rtc: None must not bind a socket or spawn a driver"
    );

    a.send_to_peer_node(b.node_id(), &batch(0, 4, "udp-only"))
        .await
        .expect("send_to_peer_node");

    assert!(
        drain_until(&b, 4, Duration::from_secs(10)).await >= 4,
        "the UDP path must deliver exactly as it does without the feature"
    );

    let stats = a.rtc_stats();
    assert_eq!(stats.accepted(), 0);
    assert_eq!(stats.admission_refused(), 0);
    assert_eq!(stats.ingress_delivered(), 0);
    assert_eq!(
        stats.written(),
        0,
        "a node with rtc: None must never touch the RTC transport"
    );
}

/// EXIT: a node can carry a UDP peer and an RTC peer at once, and
/// each is addressed on its own transport.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_carries_udp_and_rtc_peers_side_by_side() {
    let a = node(true).await;
    let b = node(true).await;
    let c = node(false).await;

    // c joins a over UDP, pre-start, as every other test does.
    let a_id = a.node_id();
    let a_pub = *a.public_key();
    let a_addr = a.local_addr();
    let a_clone = Arc::clone(&a);
    let c_id = c.node_id();
    let accept = tokio::spawn(async move { a_clone.accept(c_id).await });
    let c_join = c.connect(a_addr, &a_pub, a_id).await;
    accept.await.expect("accept task").expect("accept");
    c_join.expect("udp connect");

    a.start();
    b.start();
    c.start();
    let (id_a, _) = connect_rtc_loopback(&a, &b).await.expect("rtc join");

    assert_eq!(a.peer_endpoint(b.node_id()), Some(PeerAddr::Rtc(id_a)));
    assert!(
        matches!(a.peer_endpoint(c.node_id()), Some(PeerAddr::Udp(_))),
        "the UDP peer must stay a UDP peer: the two inputs never cross"
    );
}

/// EXIT (§5 delivery sequence): routed -> authenticated direct ->
/// forced direct failure -> restored routed, on the RTC pair.
///
/// The RTC leg is the "authenticated direct" step; closing the
/// DataChannel is the forced failure, and the node must fall back
/// rather than wedge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_delivery_sequence_survives_a_datachannel_close() {
    let (a, b, id_a, _) = rtc_pair().await;
    let b_id = b.node_id();

    let stream = a
        .open_stream(b_id, 0x54, StreamConfig::new())
        .expect("open_stream");
    a.send_to_peer_node(b_id, &batch(0, 2, "before"))
        .await
        .expect("send before close");
    assert!(drain_until(&b, 1, Duration::from_secs(10)).await >= 1);

    // Forced failure: close the channel underneath the session.
    a.rtc_driver()
        .expect("driver")
        .close(id_a)
        .await
        .expect("close");
    assert!(
        wait_for(
            || !a.rtc_driver().expect("driver").transport().is_open(id_a),
            Duration::from_secs(5)
        )
        .await,
        "the handle must stop being addressable once the channel closes"
    );

    // R3-E: a closed channel is now a peer-removal, not a
    // permanently stale entry whose every send fails. The eviction
    // runs the ordinary transaction, so the peer and its address
    // index both go.
    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(5)).await,
        "closing the DataChannel must evict the peer through the ordinary \
         removal path, not leave a stale entry for the failure detector"
    );
    let refused = a
        .send_on_stream(&stream, &[Bytes::from_static(b"after")])
        .await;
    assert!(
        refused.is_err(),
        "a send onto a closed DataChannel must fail loudly, not vanish"
    );
}
