//! Federation phase 3 — transfer fairness over the FairScheduler.
//!
//! The whole reason blob transfer rides *scheduled* router streams
//! (rather than raw `socket.send_to` at native UDP speed) is that the
//! `FairScheduler` interleaves a node's outbound streams by weighted
//! round-robin — so a big transfer can't monopolise the egress and
//! starve other traffic on the same node. The deterministic guarantee
//! is pinned at the scheduler level
//! (`router::tests::bulk_backlog_does_not_starve_an_equal_weight_interactive_stream`);
//! this file demonstrates it end-to-end over real nodes.
//!
//! - `concurrent_transfers_both_succeed` — two concurrent bulk fetches
//!   from one holder both complete byte-for-byte (the serving node's
//!   send loop interleaves the two scheduled streams; neither is
//!   starved).
//! - `bench_concurrent_transfer_throughput` (`#[ignore]`) — prints
//!   aggregate + per-transfer throughput and the interleaving gap.
//!
//! Run: `cargo test --features dataforts --test transfer_fairness`
//! Bench: `cargo test --features dataforts --test transfer_fairness -- --ignored --nocapture`

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const SOCKET_BUF: usize = 16 * 1024 * 1024;

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(20))
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

/// Distinct content per `seed` so the two blobs hash differently (and
/// hence ride distinct chunk channels + transfer streams).
fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i + seed as usize) % 251) as u8).collect()
}

fn small_ref(bytes: &[u8]) -> ([u8; 32], BlobRef) {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    (hash, BlobRef::small("mesh://fairness", hash, bytes.len() as u64))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_transfers_both_succeed() {
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

    // Two distinct chunks, each multi-frame so they genuinely contend
    // for A's send loop.
    let bytes1 = payload(1_500_000, 1);
    let bytes2 = payload(1_500_000, 2);
    let (h1, r1) = small_ref(&bytes1);
    let (h2, r2) = small_ref(&bytes2);
    adapter_a.store(&r1, &bytes1).await.expect("store 1");
    adapter_a.store(&r2, &bytes2).await.expect("store 2");

    // Fetch both concurrently from the same holder.
    let (g1, g2) = tokio::join!(
        node_b.transfer_fetch_chunk(a_id, h1),
        node_b.transfer_fetch_chunk(a_id, h2),
    );
    let g1 = g1.expect("fetch 1");
    let g2 = g2.expect("fetch 2");
    assert_eq!(g1.as_ref(), bytes1.as_slice(), "chunk 1 byte-for-byte");
    assert_eq!(g2.as_ref(), bytes2.as_slice(), "chunk 2 byte-for-byte");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark — run with --ignored --nocapture"]
async fn bench_concurrent_transfer_throughput() {
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

    let each = 4 * 1024 * 1024usize; // 4 MiB per transfer
    let bytes1 = payload(each, 1);
    let bytes2 = payload(each, 2);
    let (h1, r1) = small_ref(&bytes1);
    let (h2, r2) = small_ref(&bytes2);
    adapter_a.store(&r1, &bytes1).await.expect("store 1");
    adapter_a.store(&r2, &bytes2).await.expect("store 2");

    let start = Instant::now();
    let nb1 = node_b.clone();
    let nb2 = node_b.clone();
    let t1 = tokio::spawn(async move {
        let s = Instant::now();
        let got = nb1.transfer_fetch_chunk(a_id, h1).await.expect("fetch 1");
        (got.len(), s.elapsed())
    });
    let t2 = tokio::spawn(async move {
        let s = Instant::now();
        let got = nb2.transfer_fetch_chunk(a_id, h2).await.expect("fetch 2");
        (got.len(), s.elapsed())
    });
    let (len1, e1) = t1.await.unwrap();
    let (len2, e2) = t2.await.unwrap();
    let total_elapsed = start.elapsed();

    let total_mib = (len1 + len2) as f64 / (1024.0 * 1024.0);
    let throughput = total_mib / total_elapsed.as_secs_f64();
    // Interleaving gap: how far apart the two equal-weight transfers
    // finished, as a fraction of the total. Near 0 ⇒ the scheduler
    // interleaved them and they finished together (fair); near 1 ⇒ one
    // ran (almost) to completion before the other got service (serial
    // / starved).
    let gap = (e1.as_secs_f64() - e2.as_secs_f64()).abs();
    let gap_frac = gap / total_elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    println!("── concurrent transfer fairness ──");
    println!("  two 4 MiB transfers from one holder, equal weight (1:1)");
    println!("  total:      {total_mib:.1} MiB in {total_elapsed:?} = {throughput:.1} MiB/s");
    println!("  transfer 1: {:.1} MiB in {e1:?}", len1 as f64 / (1024.0 * 1024.0));
    println!("  transfer 2: {:.1} MiB in {e2:?}", len2 as f64 / (1024.0 * 1024.0));
    println!("  finish gap: {gap:.4}s ({:.1}% of total) — lower ⇒ fairer interleaving", gap_frac * 100.0);

    assert_eq!(len1, each);
    assert_eq!(len2, each);
}
