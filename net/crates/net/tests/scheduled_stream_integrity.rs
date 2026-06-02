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
//! assert each fetched blob matches its source byte-for-byte and that the
//! batched drain was actually exercised (flushes > 0).
//!
//! Two phases:
//!   1. normal socket buffers — `sendmmsg` mostly sends the whole group.
//!   2. **tiny send buffer** — on Linux this forces `sendmmsg` partial sends /
//!      `EWOULDBLOCK`, so the drain's async `send_to` tail-fallback executes;
//!      integrity must still hold byte-for-byte. (On macOS phase 2 runs the
//!      portable per-packet path, which also backpressures — it verifies the
//!      test still delivers, but the sendmmsg tail is a Linux-CI assertion.)
//!
//! Kept light (small blobs, modest buffers) so it runs in CI and on the macOS
//! dev host — unlike `transfer_concurrency` (4 MiB blobs / 8 MiB buffers,
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

fn config(send_buf: usize, recv_buf: usize) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(30))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: send_buf,
        recv_buffer_size: recv_buf,
    };
    cfg
}

async fn build_node(send_buf: usize, recv_buf: usize) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(send_buf, recv_buf))
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

/// Store `k` blobs of `blob` bytes on a holder, fetch them all concurrently
/// from a fetcher (→ `k` concurrent scheduled streams through the holder's
/// batched send loop), assert each arrives byte-for-byte, and return
/// `(flushes, packets)` the batch path shipped during this run (delta).
async fn run_and_verify(label: &str, k: usize, blob: usize, holder_send_buf: usize) {
    let (f0, p0) = send_batch_stats();

    // Tiny send buffer goes on the holder (the batching sender); the fetcher
    // keeps a roomy recv buffer so the bottleneck is the holder's send path.
    let holder = build_node(holder_send_buf, 512 * 1024).await;
    let fetcher = build_node(512 * 1024, 512 * 1024).await;
    handshake(&fetcher, &holder).await;
    let holder_id = holder.node_id();

    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    holder.serve_blob_transfer(adapter.clone());
    let fetcher_adapter = Arc::new(MeshBlobAdapter::new("fetcher", Arc::new(Redex::new())));
    fetcher.serve_blob_transfer(fetcher_adapter);

    let mut originals: Vec<([u8; 32], Vec<u8>)> = Vec::with_capacity(k);
    for i in 0..k {
        let bytes = payload(blob, i as u8 + 1);
        let (h, r) = blob_ref(&bytes);
        adapter.store(&r, &bytes).await.expect("store");
        originals.push((h, bytes));
    }

    let mut tasks = Vec::with_capacity(k);
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
        let got = got.unwrap_or_else(|e| panic!("[{label}] fetch {:02x?} failed: {e:?}", &h[..4]));
        assert_eq!(
            got.len(),
            original.len(),
            "[{label}] blob {:02x?}: length",
            &h[..4]
        );
        assert!(
            got[..] == original[..],
            "[{label}] blob {:02x?}: CONTENT mismatch through the batched drain",
            &h[..4]
        );
        verified += 1;
    }
    assert_eq!(
        verified, k,
        "[{label}] all {k} blobs must verify byte-for-byte"
    );

    let (f1, p1) = send_batch_stats();
    let (flushes, packets) = (f1 - f0, p1 - p0);
    eprintln!(
        "[{label}] {verified}/{k} blobs byte-for-byte; batched drain shipped \
         {packets} packets in {flushes} flushes (holder send_buf={holder_send_buf})"
    );
    assert!(
        flushes > 0,
        "[{label}] expected the holder send loop to use the batched drain \
         (flushes > 0); got 0 — concurrency did not back the scheduler up, so this \
         run did not exercise the batch path",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batched_drain_delivers_concurrent_blobs_byte_for_byte() {
    // Arm so the batched drain counts flushes (gated on the arm flag); must
    // precede start(), which each handshake performs.
    arm_send_drain_histo();

    // Phase 1 — roomy send buffer: sendmmsg mostly ships the whole group.
    run_and_verify("normal", 16, 128 * 1024, 512 * 1024).await;

    // Phase 2 — tiny holder send buffer: on Linux this forces sendmmsg partial
    // sends / EWOULDBLOCK, so the drain's async send_to tail-fallback runs.
    // Integrity must still hold. Smaller blobs keep it quick under the squeeze.
    run_and_verify("tiny-send-buf", 12, 64 * 1024, 16 * 1024).await;
}
