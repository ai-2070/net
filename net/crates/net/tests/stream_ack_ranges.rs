//! STREAM_ACK_BATCHING Phase 2 (R-6) — end-to-end SACK-range ACKs.
//!
//! The killer demo: under injected loss with both peers advertising
//! `net.reliable.stream_ack_ranges@1`, the data receiver emits
//! `StreamAckRanges` (observable via `control_plane_stats`) and the
//! transfer completes byte-for-byte. Without the capability
//! announcement the gate stays shut: zero range packets on the wire,
//! and recovery still converges through the legacy cumulative-ACK +
//! NACK + RTO path.

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];

fn config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(30))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 8 * 1024 * 1024,
        recv_buffer_size: 8 * 1024 * 1024,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, config()).await.expect("new"))
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i + seed as usize) % 251) as u8)
        .collect()
}

fn small_ref(bytes: &[u8]) -> ([u8; 32], BlobRef) {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    (
        hash,
        BlobRef::small("mesh://sack", hash, bytes.len() as u64),
    )
}

/// Lossy transfer with both peers advertising the capability: SACK
/// ranges flow (receiver-side counters move) and the chunk arrives
/// byte-for-byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sack_ranges_engage_under_loss_and_transfer_completes() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    // Advertise capabilities on both sides. The transport tag
    // (`net.reliable.stream_ack_ranges@1`) auto-merges into the
    // announcement (R-5); the empty user set is enough. The
    // announcement broadcasts to connected peers immediately — the
    // sleep lets it land in the peer folds BEFORE any gapped stream
    // consults the (TTL-cached) gate, so the first lookup doesn't
    // cache a stale `false`.
    node_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce a");
    node_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce b");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Drop every 4th scheduled (data) packet on the holder's send
    // loop — same shape as the STREAM_RETRANSMIT D-5 tests. Control
    // packets and resends go direct, so recovery converges.
    node_a.router().set_test_drop_every_n(4);

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    // ~256 KiB ⇒ ~33 data packets; every 4th dropped ⇒ recurring
    // gaps for the receiver's range index to advertise.
    let bytes = payload(256 * 1024, 1);
    let (h, r) = small_ref(&bytes);
    adapter_a.store(&r, &bytes).await.expect("store");

    let got = node_b
        .transfer_fetch_chunk(a_id, h)
        .await
        .expect("transfer must recover dropped packets");
    assert_eq!(got.as_ref(), bytes.as_slice(), "chunk byte-for-byte");

    // The data receiver (node_b) saw gaps and its peer advertises the
    // capability ⇒ it must have emitted SACK ranges.
    let stats = node_b.control_plane_stats();
    let packets = stats
        .ack_range_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed);
    let events = stats
        .ack_range_events_sent
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        packets > 0,
        "gapped reliable stream to a capable peer must emit StreamAckRanges"
    );
    assert!(events >= packets, "every range packet carries ≥1 event");
}

/// Without any capability announcement the gate stays shut: zero
/// `StreamAckRanges` on the wire, and the transfer still completes
/// through the legacy cumulative-ACK + NACK + RTO path (old-peer
/// interop equivalence).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sack_ranges_stay_off_without_peer_capability() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();
    // No announce_capabilities on either side — node_b's gate finds
    // no tag for node_a and keeps the legacy path.
    node_a.router().set_test_drop_every_n(4);

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    let bytes = payload(256 * 1024, 2);
    let (h, r) = small_ref(&bytes);
    adapter_a.store(&r, &bytes).await.expect("store");

    let got = node_b
        .transfer_fetch_chunk(a_id, h)
        .await
        .expect("legacy NACK/RTO recovery must still converge");
    assert_eq!(got.as_ref(), bytes.as_slice(), "chunk byte-for-byte");

    let stats = node_b.control_plane_stats();
    assert_eq!(
        stats
            .ack_range_packets_sent
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no capability advertised ⇒ no StreamAckRanges on the wire"
    );
}

/// Config kill-switch: with `enable_stream_ack_ranges = false` on the
/// announcing node, the tag is NOT advertised — peers keep the legacy
/// path toward it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_kill_switch_suppresses_advertisement() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg_off = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_handshake(3, Duration::from_secs(2))
        .with_stream_ack_ranges(false);
    let node_a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg_off)
            .await
            .expect("new"),
    );
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;

    node_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce a");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // node_b's fold must NOT carry the tag for node_a.
    use net::adapter::net::behavior::fold::capability::capability_tags_for;
    let tags = capability_tags_for(node_b.capability_fold(), node_a.node_id());
    assert!(
        !tags
            .iter()
            .any(|t| t == net::adapter::net::ACK_RANGES_CAPABILITY_TAG),
        "kill switch must suppress the capability tag, got {tags:?}"
    );
}
