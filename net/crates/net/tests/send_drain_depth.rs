//! Phase 0 measurement for NRPC_SEND_LOOP_BATCHING_PLAN.
//!
//! Question: under a *saturated scheduled bulk stream* — the only path that
//! actually routes through the router's `FairScheduler` send loop — does the
//! loop drain many packets back-to-back (so a `sendmmsg` batch would pay), or
//! does flow-control drip-feed it ~1 packet at a time?
//!
//! Method: arm the send-loop drain-run histogram, then blast bursts of events
//! on a `scheduled` stream with backpressure disabled (`window_bytes = 0`) so
//! the producer can pile packets into the scheduler faster than the loop's
//! per-packet `send_to().await` drains them. The histogram buckets each
//! "consecutive Some dequeues before the loop blocks" run by floor(log2(len)).
//!
//! This is a measurement, not a pass/fail invariant — it prints the
//! distribution. The single assertion only guards that the scheduled path was
//! actually exercised (so a silent routing regression can't make it vacuous).
//!
//! Run: `cargo test --features net --test send_drain_depth -- --nocapture`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::{
    arm_send_drain_histo, send_batch_stats, send_drain_histo_snapshot, send_drain_max,
    EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig, StreamConfig,
};

const TEST_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_scheduled_stream_drain_depth() {
    // Must arm BEFORE start() spawns the send loop (it latches the flag once).
    arm_send_drain_histo();

    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;
    let b_id = node_b.node_id();

    let scheduler = node_a.router().scheduler();
    let queued_before = scheduler.total_queued();

    // Scheduled bulk stream, backpressure disabled so the burst is bounded
    // only by the scheduler's per-stream queue cap (1024), not tx credit.
    let stream = node_a
        .open_stream(
            b_id,
            0x4000_0000_0000_00FF,
            StreamConfig::new()
                .with_scheduled(true)
                .with_window_bytes(0)
                .with_fairness_weight(1),
        )
        .expect("open scheduled stream");

    // ~1 KiB events; at ~1 event/packet this enqueues ~`BURST` packets per
    // call, synchronously (the scheduled deliver path does not await the
    // socket), so they pile into the scheduler ahead of the drain loop.
    const BURST: usize = 800; // < max_queue_depth (1024)
    const ROUNDS: usize = 40;
    let event = Bytes::from(vec![0xABu8; 1024]);
    let events: Vec<Bytes> = std::iter::repeat_n(event, BURST).collect();

    let mut sent_ok = 0usize;
    let mut backpressure = 0usize;
    for _ in 0..ROUNDS {
        match node_a.send_on_stream(&stream, &events).await {
            Ok(()) => sent_ok += 1,
            Err(_) => backpressure += 1,
        }
        // Let the loop fully drain this burst (and hit `None`, recording the
        // run) before the next one, so runs are cleanly separated.
        tokio::time::sleep(Duration::from_millis(8)).await;
    }

    // Final drain.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let queued_after = scheduler.total_queued();
    let histo_single = send_drain_histo_snapshot();
    print_histo(
        "single saturated scheduled stream (1 producer)",
        &histo_single,
        &[0u64; 9],
        &format!(
            "bursts sent_ok={sent_ok} backpressure={backpressure} \
             enqueued {queued_before} -> {queued_after}"
        ),
    );

    // --- Phase 0b: N concurrent scheduled streams, N producers, 1 drain loop.
    // This is the FairScheduler's actual purpose. With backpressure disabled
    // and no inter-round sleep, the aggregate producer rate races the single
    // consumer — the one scenario where the queue could back up.
    const STREAMS: usize = 16;
    const ROUNDS_MULTI: usize = 8;
    let node = node_a.clone();
    let mut handles = Vec::new();
    for s in 0..STREAMS {
        let node = node.clone();
        let events = events.clone();
        handles.push(tokio::spawn(async move {
            let stream = node
                .open_stream(
                    b_id,
                    0x4000_0000_0001_0000 | s as u64,
                    StreamConfig::new()
                        .with_scheduled(true)
                        .with_window_bytes(0)
                        .with_fairness_weight(1),
                )
                .expect("open scheduled stream");
            for _ in 0..ROUNDS_MULTI {
                // No sleep: let producers pile into the scheduler.
                let _ = node.send_on_stream(&stream, &events).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let histo_multi_total = send_drain_histo_snapshot();
    let (flushes, batched_packets) = send_batch_stats();
    let avg_batch = if flushes > 0 {
        batched_packets as f64 / flushes as f64
    } else {
        0.0
    };
    print_histo(
        &format!("{STREAMS} concurrent scheduled streams ({STREAMS} producers)"),
        &histo_multi_total,
        &histo_single, // subtract the single-stream phase
        &format!(
            "enqueued (cumulative) {queued_after} -> {} | longest single drain run = {} packets",
            scheduler.total_queued(),
            send_drain_max(),
        ),
    );
    eprintln!(
        "  batched-drain path: {flushes} flushes (= sendmmsg syscalls on Linux) for \
         {batched_packets} packets  =>  avg {avg_batch:.1} packets/syscall \
         ({:.0}x fewer send syscalls)\n",
        avg_batch.max(1.0),
    );

    let total_runs: u64 = histo_single.iter().sum();
    assert!(
        queued_after > queued_before,
        "scheduled stream must route through the scheduler (enqueue counter \
         {queued_before} -> {queued_after}) — else this measurement is vacuous",
    );
    assert!(
        total_runs > 0,
        "send loop recorded no drain runs while armed — instrument not wired?",
    );
}

/// Print a bucketed drain-run histogram (`now` minus `base`), with a lower
/// bound on packets accounted (sum of count × 2^bucket).
fn print_histo(title: &str, now: &[u64; 9], base: &[u64; 9], header: &str) {
    let labels = [
        "1", "2-3", "4-7", "8-15", "16-31", "32-63", "64-127", "128-255", "256+",
    ];
    let mut total_runs = 0u64;
    let mut weighted = 0u64;
    eprintln!("\n=== drain run-length: {title} ===");
    eprintln!("  {header}");
    for (i, label) in labels.iter().enumerate() {
        let count = now[i].saturating_sub(base[i]);
        total_runs += count;
        weighted += count * (1u64 << i);
        let bar = "#".repeat(count.min(60) as usize);
        eprintln!("  run-len {label:>8} : {count:>8}  {bar}");
    }
    eprintln!("  total runs: {total_runs}   lower-bound packets (sum count*2^bucket): {weighted}");
    eprintln!("==============================================\n");
}
