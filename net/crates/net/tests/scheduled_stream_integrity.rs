//! Reliable end-to-end integrity through the scheduled-stream batched drain.
//!
//! NRPC_SEND_LOOP_BATCHING_PLAN Phase 1: the router send loop ships
//! scheduled-stream data via a group-by-destination batched drain (one
//! `sendmmsg` per peer on Linux). `send_drain_depth` proves the *syscall
//! collapse* but NOT that the batched bytes arrive intact — it is
//! `FireAndForget` and the drain falls back to `send_to` on a `send_batch`
//! error, so a silently mis-delivering sendmmsg would not fail it.
//!
//! This test closes that gap. Blob transfer's holder-side data rides
//! `scheduled = true` streams (`dataforts/blob/transfer.rs`), so fetching K
//! blobs concurrently from one holder makes its single send loop back up into
//! the batch path. The transfer is RELIABLE, so every byte must arrive; we
//! assert each fetched blob matches its source byte-for-byte, and that the
//! batched drain was actually exercised (flushes > 0).
//!
//! Kept light (small blobs, modest socket buffers) so it runs in CI and on the
//! macOS dev host — unlike `transfer_concurrency` (4 MiB blobs / 8 MiB buffers,
//! which can't even bind on macOS).
//!
//! Run: cargo test --features dataforts --test scheduled_stream_integrity -- --nocapture

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{
    arm_send_drain_histo, send_batch_stats, EntityKeypair, MeshNode, MeshNodeConfig,
    SocketBufferConfig,
};

const PSK: [u8; 32] = [0x42u8; 32];
const SOCK_BUF: usize = 512 * 1024;

fn config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(30))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: SOCK_BUF,
        recv_buffer_size: SOCK_BUF,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config())
            .await
            .expect("new"),
    )
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

/// Distinct, content-addressable bytes (seed in the byte pattern so blobs
/// differ → distinct hash/channel/stream and a real content check).
fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i.wrapping_mul(31) + seed as usize) % 251) as u8)
        .collect()
}

fn blob_ref(bytes: &[u8]) -> ([u8; 32], BlobRef) {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    (
        hash,
        BlobRef::small("mesh://integrity", hash, bytes.len() as u64),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batched_drain_delivers_concurrent_blobs_byte_for_byte() {
    // Arm so the batched drain counts flushes (gated on the arm flag); must
    // precede start(), which the handshake performs.
    arm_send_drain_histo();

    let holder = build_node().await; // serves blobs; its send loop batches.
    let fetcher = build_node().await; // fetches all blobs concurrently.
    handshake(&fetcher, &holder).await;
    let holder_id = holder.node_id();

    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    holder.serve_blob_transfer(adapter.clone());
    let fetcher_adapter = Arc::new(MeshBlobAdapter::new("fetcher", Arc::new(Redex::new())));
    fetcher.serve_blob_transfer(fetcher_adapter);

    // K distinct blobs: enough packets/blob × concurrency to back the holder's
    // single send loop into the batch path.
    const K: usize = 16;
    const BLOB: usize = 128 * 1024;
    let mut originals: Vec<([u8; 32], Vec<u8>)> = Vec::with_capacity(K);
    for i in 0..K {
        let bytes = payload(BLOB, i as u8 + 1);
        let (h, r) = blob_ref(&bytes);
        adapter.store(&r, &bytes).await.expect("store");
        originals.push((h, bytes));
    }

    // Fetch all K concurrently → K concurrent scheduled streams holder→fetcher.
    let mut tasks = Vec::with_capacity(K);
    for (h, bytes) in originals {
        let f = fetcher.clone();
        tasks.push(tokio::spawn(async move {
            let got = f.transfer_fetch_chunk(holder_id, h).await;
            (h, bytes, got)
        }));
    }

    let mut verified = 0usize;
    for t in tasks {
        let (h, original, got) = t.await.expect("join");
        let got = got.unwrap_or_else(|e| panic!("fetch {:02x?} failed: {e:?}", &h[..4]));
        assert_eq!(got.len(), original.len(), "blob {:02x?}: length", &h[..4]);
        assert!(
            got[..] == original[..],
            "blob {:02x?}: CONTENT mismatch through the batched drain",
            &h[..4]
        );
        verified += 1;
    }
    assert_eq!(verified, K, "all {K} blobs must verify byte-for-byte");

    let (flushes, packets) = send_batch_stats();
    eprintln!(
        "integrity: {verified}/{K} blobs byte-for-byte; batched drain shipped \
         {packets} packets in {flushes} flushes"
    );
    assert!(
        flushes > 0,
        "expected the holder send loop to use the batched drain (flushes > 0); got 0 \
         — concurrency did not back the scheduler up, so this run did not actually \
         exercise the batch path",
    );
}
