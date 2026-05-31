//! Diagnostic — concurrent large-file transfer at increasing fan-out.
//!
//! The flow-control window is per-stream (5 MiB for transfers), so N
//! concurrent large transfers put up to `N × window` bytes in flight
//! against a single receive loop + kernel recv buffer. This sweep finds
//! whether (and at what fan-out) concurrent 4 MiB transfers degrade or
//! fail, and prints send-side scheduler drop/queue counters to localise
//! the mechanism (scheduler queue-full vs kernel recv-buffer overflow vs
//! retransmit non-recovery).
//!
//! Run: `cargo test --features dataforts --test transfer_concurrency -- --ignored --nocapture --test-threads=1`

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::dataforts::blob::{BlobAdapter, BlobRef, MeshBlobAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];

fn config(socket_buf: usize) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(30))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: socket_buf,
        recv_buffer_size: socket_buf,
    };
    cfg
}

async fn build_node(socket_buf: usize) -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, config(socket_buf)).await.expect("new"))
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
    (0..len).map(|i| ((i + seed as usize) % 251) as u8).collect()
}

fn small_ref(bytes: &[u8]) -> ([u8; 32], BlobRef) {
    let hash: [u8; 32] = blake3::hash(bytes).into();
    (hash, BlobRef::small("mesh://conc", hash, bytes.len() as u64))
}

/// Fetch `k` distinct 4 MiB blobs from one holder concurrently. Returns
/// (successes, elapsed, sched_queued, sched_dropped, first_error).
async fn run_level(
    k: usize,
    file_bytes: usize,
    socket_buf: usize,
) -> (usize, Duration, u64, u64, Option<String>) {
    let node_a = build_node(socket_buf).await;
    let node_b = build_node(socket_buf).await;
    handshake(&node_b, &node_a).await;
    let a_id = node_a.node_id();

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", Arc::new(Redex::new())));
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", Arc::new(Redex::new())));
    node_a.serve_blob_transfer(adapter_a.clone());
    node_b.serve_blob_transfer(adapter_b);

    // k distinct blobs (distinct content → distinct hash/channel/stream).
    let mut hashes = Vec::with_capacity(k);
    for i in 0..k {
        let bytes = payload(file_bytes, i as u8 + 1);
        let (h, r) = small_ref(&bytes);
        adapter_a.store(&r, &bytes).await.expect("store");
        hashes.push(h);
    }

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(k);
    for h in hashes {
        let nb = node_b.clone();
        tasks.push(tokio::spawn(async move { nb.transfer_fetch_chunk(a_id, h).await }));
    }
    let mut ok = 0usize;
    let mut first_err: Option<String> = None;
    for t in tasks {
        match t.await {
            Ok(Ok(bytes)) if bytes.len() == file_bytes => ok += 1,
            Ok(Ok(bytes)) => {
                first_err.get_or_insert(format!("short: {} bytes", bytes.len()));
            }
            Ok(Err(e)) => {
                first_err.get_or_insert(format!("{e:?}"));
            }
            Err(join) => {
                first_err.get_or_insert(format!("join: {join}"));
            }
        }
    }
    let elapsed = start.elapsed();
    let sched = node_a.router().scheduler();
    (ok, elapsed, sched.total_queued(), sched.total_dropped(), first_err)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "diagnostic — run with --ignored --nocapture --test-threads=1"]
async fn sweep_concurrent_large_transfers() {
    const FILE: usize = 4 * 1024 * 1024; // 4 MiB each
    const SOCKET_BUF: usize = 8 * 1024 * 1024;
    println!(
        "── concurrent 4 MiB transfers, {} MiB socket buffer ──",
        SOCKET_BUF / (1024 * 1024)
    );
    for k in [2usize, 4, 6, 8] {
        let (ok, elapsed, queued, dropped, err) = run_level(k, FILE, SOCKET_BUF).await;
        let mib = (k * FILE) as f64 / (1024.0 * 1024.0);
        let rate = if elapsed.as_secs_f64() > 0.0 {
            mib / elapsed.as_secs_f64()
        } else {
            0.0
        };
        let verdict = if ok == k { "ok " } else { "FAIL" };
        println!(
            "  k={k}: {verdict} {ok}/{k} ok, {mib:.0} MiB in {elapsed:?} = {rate:.1} MiB/s | A sched: queued={queued} dropped={dropped}{}",
            err.map(|e| format!(" | first err: {e}")).unwrap_or_default()
        );
    }
}
