//! Federation phase 1 — blob transfer over router streams.
//!
//! Two paired `MeshNode`s. A stores a chunk and serves blob transfer;
//! B fetches it via `transfer_fetch_chunk`. The bytes move over a
//! reliable, scheduled router stream (the FairScheduler transport) —
//! NOT RedEX replication, NOT nRPC. Control rides a
//! `SUBPROTOCOL_BLOB_TRANSFER` packet; data rides the reliable stream
//! and is diverted to the engine by the transfer stream-id convention.
//!
//! Run: `cargo test --features dataforts --test blob_transfer`

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const SOCKET_BUF: usize = 8 * 1024 * 1024;

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(15))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: SOCKET_BUF,
        recv_buffer_size: SOCKET_BUF,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
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

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn small_ref(bytes: &[u8]) -> ([u8; 32], BlobRef) {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    (hash, BlobRef::small("mesh://transfer", hash, bytes.len() as u64))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_fetch_chunk_round_trip() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    // B (requester) connects to A (holder).
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    // A holds the chunk; B's adapter starts empty.
    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));

    // Both install the engine (A to serve, B to receive + reassemble).
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b.clone());

    // ~50 KiB chunk → ~7 data packets (one per ~8000-byte frame).
    let bytes = payload(50_000);
    let (hash, blob_ref) = small_ref(&bytes);
    adapter_a.store(&blob_ref, &bytes).await.expect("A store");

    // B pulls the chunk over the stream transport.
    let got = node_b
        .transfer_fetch_chunk(a_id, hash)
        .await
        .expect("B transfer_fetch_chunk");
    assert_eq!(
        got.as_ref(),
        bytes.as_slice(),
        "transferred chunk must match byte-for-byte",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_fetch_chunk_large_exercises_window_refill() {
    // 2 MiB ≫ the 64 KiB tx-credit window, so the send loop fills the
    // window, blocks on Backpressure, and resumes on receiver
    // StreamWindow grants — exercising flow control + ~260 packets
    // through the scheduler. This is the regime the per-chunk
    // replication path couldn't deliver.
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    let bytes = payload(2 * 1024 * 1024);
    let (hash, blob_ref) = small_ref(&bytes);
    adapter_a.store(&blob_ref, &bytes).await.expect("A store");

    let got = node_b
        .transfer_fetch_chunk(a_id, hash)
        .await
        .expect("B transfer_fetch_chunk (2 MiB)");
    assert_eq!(got.len(), bytes.len(), "length match");
    assert_eq!(got.as_ref(), bytes.as_slice(), "2 MiB chunk byte-for-byte");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_fetch_chunk_discovered_finds_the_holder() {
    // B is connected to two peers: A holds the chunk, C does not. B
    // fetches by hash alone (no named holder) and discovery probes its
    // peers until one serves it — A's bytes come back, C's prompt
    // NotFound is just skipped.
    let node_a = build_node().await;
    let node_b = build_node().await;
    let node_c = build_node().await;
    // B (requester) connects to both A (holder) and C (non-holder).
    handshake(&node_b, &node_a).await;
    handshake(&node_b, &node_c).await;
    let a_id = node_a.node_id();

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    let redex_c = Arc::new(Redex::new());
    let adapter_c = Arc::new(MeshBlobAdapter::new("c", redex_c));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);
    node_c.serve_blob_transfer(adapter_c);

    // Only A holds the chunk.
    let bytes = payload(40_000);
    let (hash, blob_ref) = small_ref(&bytes);
    adapter_a.store(&blob_ref, &bytes).await.expect("A store");
    let _ = a_id;

    let got = node_b
        .transfer_fetch_chunk_discovered(hash)
        .await
        .expect("B discovers + fetches from whichever peer holds it");
    assert_eq!(
        got.as_ref(),
        bytes.as_slice(),
        "discovered chunk must match byte-for-byte",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_fetch_chunk_discovered_none_hold_it() {
    // No connected peer holds the chunk → discovery exhausts its
    // candidates and returns NotFound (the caller fails over / errors).
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    node_a.serve_blob_transfer(adapter_a);
    node_b.serve_blob_transfer(adapter_b);

    let bytes = payload(2048);
    let (hash, _) = small_ref(&bytes);
    use net::adapter::net::dataforts::blob::BlobError;
    let err = node_b
        .transfer_fetch_chunk_discovered(hash)
        .await
        .expect_err("no peer holds it");
    assert!(matches!(err, BlobError::NotFound(_)), "got {err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_fetch_chunk_not_found() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let redex_a = Arc::new(Redex::new());
    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a));
    let redex_b = Arc::new(Redex::new());
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b));
    node_a.serve_blob_transfer(adapter_a);
    node_b.serve_blob_transfer(adapter_b);

    // A doesn't hold this chunk → holder replies NotFound.
    let bytes = payload(1024);
    let (hash, _) = small_ref(&bytes);
    use net::adapter::net::dataforts::blob::BlobError;
    let err = node_b
        .transfer_fetch_chunk(a_id, hash)
        .await
        .expect_err("unheld chunk must error");
    assert!(matches!(err, BlobError::NotFound(_)), "got {err:?}");
}
