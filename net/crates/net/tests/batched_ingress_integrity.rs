//! Reliable end-to-end integrity through the batched-ingress receive path.
//!
//! NRPC_RECV_LOOP_BATCHING_PLAN stage 4. The mesh receive loop can drain the
//! socket with `recvmmsg` (Linux) via `BatchedPacketReceiver`, opted in with
//! `MeshNodeConfig::with_batched_ingress(true)`. This test drives a bulk
//! inbound workload through that path and asserts two things the unit tests
//! can't: (1) every byte arrives intact through the batched receiver, and
//! (2) on Linux the batched path actually carried the traffic
//! (`recv_batch_stats().syscalls > 0`).
//!
//! Blob transfer's holder-side data rides reliable scheduled streams
//! (`dataforts/blob/transfer.rs`); fetching K blobs concurrently from one
//! holder floods the *fetcher's* receive loop, which is the side we enable
//! batched ingress on. Reliability means every byte must arrive — so a
//! mis-delivering recvmmsg (wrong source, dropped/duplicated packet, reordered
//! within a peer) would fail the byte-for-byte check.
//!
//! Off Linux, `batched_ingress` is a no-op (the per-packet path runs), so the
//! batch-path assertion is Linux-only; the integrity check still holds and
//! verifies the test itself delivers.
//!
//! Run: cargo test --features dataforts --test batched_ingress_integrity -- --nocapture

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{
    arm_recv_drain_histo, recv_batch_stats, EntityKeypair, MeshNode, MeshNodeConfig,
    SocketBufferConfig,
};

const PSK: [u8; 32] = [0x42u8; 32];

fn config(recv_buf: usize, batched_ingress: bool) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(30))
        .with_handshake(3, Duration::from_secs(2))
        .with_batched_ingress(batched_ingress);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 512 * 1024,
        recv_buffer_size: recv_buf,
    };
    cfg
}

async fn build_node(recv_buf: usize, batched_ingress: bool) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(recv_buf, batched_ingress))
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
        BlobRef::small("mesh://ingress", hash, bytes.len() as u64),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batched_ingress_delivers_concurrent_blobs_byte_for_byte() {
    // Arm so the batched receive path counts its recvmmsg batches (gated on the
    // arm flag, latched at receive-thread startup); must precede start(), which
    // handshake() performs.
    arm_recv_drain_histo();

    let (s0, p0) = recv_batch_stats();

    // The fetcher is the bulk *receiver* — enable batched ingress there. The
    // holder streams the blob bytes out; the fetcher's receive loop is the one
    // that backs up into multi-packet recvmmsg batches.
    let holder = build_node(512 * 1024, false).await;
    let fetcher = build_node(512 * 1024, true).await;
    handshake(&fetcher, &holder).await;
    let holder_id = holder.node_id();

    let adapter = Arc::new(MeshBlobAdapter::new("holder", Arc::new(Redex::new())));
    holder.serve_blob_transfer(adapter.clone());
    let fetcher_adapter = Arc::new(MeshBlobAdapter::new("fetcher", Arc::new(Redex::new())));
    fetcher.serve_blob_transfer(fetcher_adapter);

    // Store K distinct blobs on the holder.
    const K: usize = 16;
    const BLOB: usize = 128 * 1024;
    let mut originals: Vec<([u8; 32], Vec<u8>)> = Vec::with_capacity(K);
    for i in 0..K {
        let bytes = payload(BLOB, i as u8 + 1);
        let (h, r) = blob_ref(&bytes);
        adapter.store(&r, &bytes).await.expect("store");
        originals.push((h, bytes));
    }

    // Fetch them all concurrently → K concurrent inbound scheduled streams
    // through the fetcher's batched receive loop.
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
            "blob {:02x?}: CONTENT mismatch through the batched ingress path",
            &h[..4]
        );
        verified += 1;
    }
    assert_eq!(verified, K, "all {K} blobs must verify byte-for-byte");

    let (s1, p1) = recv_batch_stats();
    let (syscalls, packets) = (s1 - s0, p1 - p0);
    eprintln!(
        "batched ingress: {verified}/{K} blobs byte-for-byte; recv path drained \
         {packets} packets in {syscalls} recvmmsg batches"
    );

    // On Linux the fetcher's batched-ingress path must have carried the
    // inbound bulk: a non-empty recvmmsg recorded at least one batch. Off
    // Linux `batched_ingress` is a no-op (per-packet path), so the counter
    // stays zero and we only assert delivery above.
    #[cfg(target_os = "linux")]
    assert!(
        syscalls > 0,
        "expected the fetcher's batched-ingress recv path to carry the inbound \
         bulk (recvmmsg syscalls > 0); got 0 — the batched path was not exercised",
    );
}
