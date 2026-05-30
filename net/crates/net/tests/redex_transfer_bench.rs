//! Redex replication transfer benchmark — measures the federation
//! phase-2 scale path (directory transfer over RedEX replication, the
//! per-node-replica-set path that scales past the advertisement
//! ceiling).
//!
//! These are `#[ignore]`d so they never run in the normal suite. Run on
//! demand:
//!
//! ```text
//! cargo test --no-default-features --features dataforts,cortex \
//!     --test redex_transfer_bench -- --ignored --nocapture
//! ```
//!
//! What's measured: store time (A walks the tree, stores each file as a
//! content-addressed blob, claims leadership per chunk), cross-peer
//! fetch time (B reconstructs by pulling every leaf over replication),
//! and the derived aggregate throughput. NOT yet measured: peak RSS
//! (needs an external profiler / platform memory API — flagged as a
//! follow-up). The numbers are localhost loopback over a single shared
//! UDP socket, so they reflect the substrate's per-operation overhead,
//! not a real NIC's ceiling.
//!
//! # Results (2026-05-30, localhost, 100 ms replication heartbeat)
//!
//! The per-chunk-channel replication transfer works only in a narrow
//! window — small files, low count — and is slow there. It does NOT
//! scale in EITHER dimension:
//!
//! | shape                | store        | fetch                         |
//! |----------------------|--------------|-------------------------------|
//! | 100 × 4 KiB          | 7-12 MiB/s   | 0.31 MiB/s, 79 files/s — OK   |
//! | 500 × 4 KiB          | 12 MiB/s     | timeout — a chunk never synced|
//! | 1000 × 2 KiB         | 9 MiB/s      | timeout                       |
//! | 8 × 1 MiB            | 150+ MiB/s   | timeout — a 1 MiB chunk never synced |
//! | 2-4 × 4-8 MiB        | 160+ MiB/s   | timeout                       |
//!
//! Two distinct failure modes, both on the FETCH (replication-pull)
//! side; the local STORE is fast and scales fine:
//!
//! 1. **Many small chunks** (≥~500 channels): every chunk is its own
//!    replicated channel with its own coordinator heartbeating every
//!    100 ms. Hundreds of coordinators on one shared UDP socket is a
//!    heartbeat storm; sync traffic for some chunk gets dropped and the
//!    per-chunk recovery doesn't catch up within the 30 s deadline.
//! 2. **Large chunks** (≥~1 MiB): a single chunk is one RedEX event,
//!    and a multi-hundred-KiB event exceeds the sync chunk budget /
//!    wire datagram, so it isn't delivered at all (fails even at 2-8
//!    files).
//!
//! Conclusion: the literal "one replicated channel per chunk" model
//! doesn't carry either `node_modules` (many small) or Cargo `target/`
//! (few large). The phase-2 demo needs a different aggregation — a
//! single transfer-session channel (or a streamed tree blob) rather
//! than a channel per file. Tracked as the next architecture decision.

#![cfg(all(feature = "dataforts", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::dataforts::{fetch_dir, store_dir};
use net::adapter::net::dataforts::blob::MeshBlobAdapter;
use net::adapter::net::redex::{PlacementStrategy, Redex, ReplicationConfig};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
// Bigger socket buffers than the functional tests — a bulk transfer
// bursts many datagrams and the default 256 KiB recv buffer drops them.
const SOCKET_BUF: usize = 8 * 1024 * 1024;

fn test_config(heartbeat_ms: u64) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(30))
        .with_handshake(3, Duration::from_secs(2))
        .with_min_announce_interval(Duration::from_millis(0))
        .with_capability_gc_interval(Duration::from_millis(500));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: SOCKET_BUF,
        recv_buffer_size: SOCKET_BUF,
    };
    let _ = heartbeat_ms; // mesh heartbeat; replication heartbeat is on the cfg below
    cfg
}

async fn build_node(heartbeat_ms: u64) -> Arc<MeshNode> {
    let cfg = test_config(heartbeat_ms);
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

struct Stats {
    label: String,
    n_files: usize,
    total_bytes: u64,
    store: Duration,
    fetch: Duration,
    status: String,
}

impl Stats {
    fn report(&self) {
        let mib = self.total_bytes as f64 / (1024.0 * 1024.0);
        let store_s = self.store.as_secs_f64();
        let fetch_s = self.fetch.as_secs_f64();
        let store_thru = mib / store_s.max(1e-9);
        let fetch_thru = mib / fetch_s.max(1e-9);
        let files_per_s = self.n_files as f64 / fetch_s.max(1e-9);
        println!(
            "[bench:{}] {:>5} files / {:>8.2} MiB | store {:>7.0} ms ({:>6.2} MiB/s) | \
             fetch {:>7.0} ms ({:>6.2} MiB/s, {:>8.0} files/s) | {}",
            self.label,
            self.n_files,
            mib,
            self.store.as_millis(),
            store_thru,
            self.fetch.as_millis(),
            fetch_thru,
            files_per_s,
            self.status,
        );
    }
}

/// Generate a tree of `n_files` files of `file_size` bytes each, spread
/// across a few subdirs, transfer it A→B over replication, and time
/// each phase. Verifies a sample so we know the transfer really
/// happened without paying full-tree verification on huge trees.
async fn bench_transfer(
    label: &str,
    n_files: usize,
    file_size: usize,
    replication_heartbeat_ms: u64,
) -> Stats {
    let node_a = build_node(replication_heartbeat_ms).await;
    let node_b = build_node(replication_heartbeat_ms).await;
    handshake(&node_a, &node_b).await;
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();

    let redex_a = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    let redex_b = Arc::new(Redex::new());
    redex_b.enable_replication(node_b.clone());

    let cfg = ReplicationConfig::new()
        .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
        .with_heartbeat_ms(replication_heartbeat_ms);
    cfg.validate().expect("valid cfg");

    let adapter_a = Arc::new(MeshBlobAdapter::new("a", redex_a).with_replication(cfg.clone()));
    let adapter_b = Arc::new(MeshBlobAdapter::new("b", redex_b).with_replication(cfg.clone()));

    let tmp = std::env::temp_dir().join(format!("net-bench-{}-{}-{}", label, std::process::id(), a_id));
    let src = tmp.join("src");
    let dst = tmp.join("dst");
    let _ = std::fs::remove_dir_all(&tmp);
    let mut total_bytes = 0u64;
    for i in 0..n_files {
        let sub = src.join(format!("d{}", i % 16));
        std::fs::create_dir_all(&sub).unwrap();
        // Distinct content per file (no dedup masking the work).
        let seed = (i as u64).wrapping_mul(2654435761);
        let content: Vec<u8> = (0..file_size)
            .map(|j| (seed.wrapping_add(j as u64) % 251) as u8)
            .collect();
        total_bytes += content.len() as u64;
        std::fs::write(sub.join(format!("f{i}.bin")), &content).unwrap();
    }

    let t0 = Instant::now();
    let root_ref = store_dir(adapter_a.as_ref(), &src).await.expect("store_dir");
    let store = t0.elapsed();

    let t1 = Instant::now();
    let fetch_result = fetch_dir(adapter_b.as_ref(), &root_ref, &dst).await;
    let fetch = t1.elapsed();

    // Resilient: a failure at scale (a chunk that didn't replicate in
    // time) is itself a result, not a crash. Record it and move on so
    // the whole curve prints.
    let status = match fetch_result {
        Ok(()) => {
            // Sample verification (first, middle, last) — full byte
            // equivalence is exercised by the functional tests.
            let mut ok = true;
            for i in [0usize, n_files / 2, n_files.saturating_sub(1)] {
                if i >= n_files {
                    continue;
                }
                let rel = format!("d{}/f{i}.bin", i % 16);
                let want = std::fs::read(src.join(&rel)).unwrap();
                match std::fs::read(dst.join(&rel)) {
                    Ok(got) if got == want => {}
                    _ => ok = false,
                }
            }
            if ok {
                "ok".to_string()
            } else {
                "OK-but-sample-mismatch".to_string()
            }
        }
        Err(e) => format!("INCOMPLETE: {e}"),
    };

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = b_id;
    Stats {
        label: label.to_string(),
        n_files,
        total_bytes,
        store,
        fetch,
        status,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark — run with --ignored --nocapture"]
async fn bench_small_files() {
    println!();
    for (n, sz) in [(100usize, 4096usize), (500, 4096), (1000, 2048)] {
        bench_transfer("small", n, sz, 100).await.report();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark — run with --ignored --nocapture"]
async fn bench_large_files() {
    println!();
    for (n, sz) in [(8usize, 1 << 20), (4, 4 << 20), (2, 8 << 20)] {
        bench_transfer("large", n, sz, 100).await.report();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark — run with --ignored --nocapture"]
async fn bench_throughput_invariance() {
    // Same ~4 MiB total byte volume across very different file counts.
    // The architectural claim: aggregate throughput should hold roughly
    // constant as file count climbs at equal byte volume.
    println!();
    for (n, sz) in [
        (4usize, 1usize << 20),
        (40, 100_000),
        (400, 10_000),
        (4000, 1_000),
    ] {
        bench_transfer("invariance", n, sz, 100).await.report();
    }
}
