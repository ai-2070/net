//! Recvmmsg syscall-collapse measurement for NRPC_RECV_LOOP_BATCHING_PLAN.
//!
//! The recv analogue of `send_drain_depth`. Question: under heavy concurrent
//! ingress, does the Linux batched receive path (`recvmmsg` via
//! `BatchedPacketReceiver`, enabled with `with_batched_ingress(true)`) coalesce
//! many packets per syscall, or one at a time?
//!
//! Method: arm the recv-batch instrument, then blast a node's UDP socket with
//! fire-and-forget garbage datagrams from several concurrent senders. They are
//! *received* via recvmmsg (and recorded by the recv thread) then silently
//! dropped at dispatch — `ParsedPacket::parse` rejects them (no Net magic), no
//! session exists, nothing is logged. Crucially there is **no reliable
//! transfer**, so the flood can be arbitrarily heavy without the
//! retransmit-exhaustion flakiness a reliable workload (blob transfer) hits
//! under loopback congestion. The instrument buckets each non-empty recvmmsg by
//! floor(log2(packet count)) and tracks `(syscalls, packets)`.
//!
//! This is a measurement that also asserts the collapse actually happened on
//! Linux (`packets > syscalls`). Off Linux `batched_ingress` is a no-op (the
//! per-packet path runs), so there's nothing to measure — the flood still runs
//! (exercising the receive loop) but the batch counters stay zero and the
//! assertions are skipped.
//!
//! Run: `cargo test --features net --test recv_drain_depth -- --nocapture`

// Needs `batched-ingress` (the recvmmsg path + instrument + `with_batched_ingress`
// builder); that feature pulls in `net`.
#![cfg(feature = "batched-ingress")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{
    arm_recv_drain_histo, recv_batch_stats, recv_drain_histo_snapshot, recv_drain_max,
    EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig,
};
use net::adapter::Adapter; // MeshNode::shutdown for graceful teardown
use tokio::net::UdpSocket;

const PSK: [u8; 32] = [0x42u8; 32];

// A large recv buffer on Linux so the flood queues in the kernel and recvmmsg
// can return full batches; modest elsewhere (the path is per-packet off Linux,
// and oversized SO_RCVBUF is needlessly aggressive on the macOS dev host).
#[cfg(target_os = "linux")]
const RECV_BUF: usize = 4 * 1024 * 1024;
#[cfg(not(target_os = "linux"))]
const RECV_BUF: usize = 512 * 1024;

async fn build_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2))
        .with_batched_ingress(true);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 512 * 1024,
        recv_buffer_size: RECV_BUF,
    };
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_recv_batch_collapse() {
    // Must arm BEFORE start() spawns the receive loop (the recv thread latches
    // the flag once at startup).
    arm_recv_drain_histo();

    let node = build_node().await;
    // Lone node, no peers: start() spawns the receive loop, which drains the
    // socket regardless of sessions. No heartbeats are sent (no peers), so the
    // only inbound traffic is our flood.
    node.start();
    let target = node.local_addr();

    let (s0, p0) = recv_batch_stats();

    // Blast fire-and-forget garbage from several concurrent senders so the
    // aggregate arrival rate outpaces the single recv thread and the kernel
    // recv buffer accumulates → recvmmsg returns multi-packet batches. 256-byte
    // payload: not 14 (keep-alive) or 72 (pingwave), and not Net magic, so it
    // is rejected by `ParsedPacket::parse` and silently dropped at dispatch.
    const SENDERS: usize = 8;
    const PER_SENDER: usize = 6000;
    const PKT: usize = 256;
    let mut handles = Vec::with_capacity(SENDERS);
    for _ in 0..SENDERS {
        handles.push(tokio::spawn(async move {
            let sock = UdpSocket::bind(("127.0.0.1", 0))
                .await
                .expect("bind flooder");
            let pkt = vec![0xABu8; PKT];
            for _ in 0..PER_SENDER {
                // Fire-and-forget: ignore EWOULDBLOCK / drops — losing packets
                // here is the point (no reliability layer), and the receiver
                // counts whatever the kernel actually delivers.
                let _ = sock.send_to(&pkt, target).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    // Let the recv thread drain the kernel buffer and record the tail batch.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (s1, p1) = recv_batch_stats();
    let (syscalls, packets) = (s1 - s0, p1 - p0);
    let histo = recv_drain_histo_snapshot();
    let avg = if syscalls > 0 {
        packets as f64 / syscalls as f64
    } else {
        0.0
    };

    print_histo(
        &format!("{SENDERS} concurrent flooders x {PER_SENDER} datagrams"),
        &histo,
        &format!(
            "sent {} datagrams (fire-and-forget) | largest single recvmmsg batch = {} packets",
            SENDERS * PER_SENDER,
            recv_drain_max(),
        ),
    );
    eprintln!(
        "  batched-ingress path: {syscalls} recvmmsg syscalls for {packets} packets  =>  \
         avg {avg:.1} packets/syscall ({:.0}x fewer recv syscalls)\n",
        avg.max(1.0),
    );

    // On Linux the batched path must have carried the flood AND collapsed
    // syscalls (more packets received than recvmmsg calls = at least one
    // multi-packet batch). The heavy lossy flood guarantees queueing, so unlike
    // the reliable integrity test this can assert the collapse without
    // flakiness. Off Linux the per-packet path runs; counters stay zero.
    #[cfg(target_os = "linux")]
    {
        assert!(
            syscalls > 0,
            "batched-ingress recv path recorded no recvmmsg batches while armed \
             — instrument not wired, or the flood never reached the receiver?",
        );
        assert!(
            packets > syscalls,
            "expected recvmmsg to coalesce multiple packets per syscall under a \
             heavy concurrent flood; got {packets} packets in {syscalls} syscalls \
             (avg {avg:.2} <= 1 means no syscall collapse)",
        );
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("  (off Linux: per-packet receive path, no batching to measure)");

    let _ = node.shutdown().await;
}

/// Print a bucketed recvmmsg-batch-size histogram, with a lower bound on
/// packets accounted (sum of count × 2^bucket). Mirrors `send_drain_depth`.
fn print_histo(title: &str, histo: &[u64; 9], header: &str) {
    let labels = [
        "1", "2-3", "4-7", "8-15", "16-31", "32-63", "64-127", "128-255", "256+",
    ];
    let mut total_batches = 0u64;
    let mut weighted = 0u64;
    eprintln!("\n=== recvmmsg batch size: {title} ===");
    eprintln!("  {header}");
    for (i, label) in labels.iter().enumerate() {
        let count = histo[i];
        total_batches += count;
        weighted += count * (1u64 << i);
        let bar = "#".repeat(count.min(60) as usize);
        eprintln!("  batch {label:>8} : {count:>8}  {bar}");
    }
    eprintln!(
        "  total batches: {total_batches}   lower-bound packets (sum count*2^bucket): {weighted}"
    );
    eprintln!("==============================================\n");
}
