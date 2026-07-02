//! Criterion benches for the reliability hot paths
//! (STREAM_ACK_BATCHING R-6).
//!
//! `on_receive` runs once per inbound data packet — the in-order,
//! no-gap shape is THE hot path and must stay cheap after the R-2
//! range-index rewrite. The gap/merge/SACK shapes are loss-path only
//! but bound the per-packet cost during a loss episode.
//!
//! Noise floor: at ns scale, ±20–30% criterion deltas are jitter —
//! compare distributions, not single runs.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use net::adapter::net::{PacketFlags, ReliabilityMode, ReliableStream, RetransmitDescriptor};

fn descriptor(seq: u64) -> Arc<RetransmitDescriptor> {
    Arc::new(RetransmitDescriptor {
        seq,
        stream_id: 0,
        events: vec![Bytes::from_static(b"payload")],
        flags: PacketFlags::RELIABLE,
    })
}

/// The per-packet hot path: in-order accept with an empty range
/// index (fast branch: one compare + increment).
fn bench_on_receive_in_order(c: &mut Criterion) {
    let mut s = ReliableStream::new();
    let mut seq = 0u64;
    c.bench_function("reliability/on_receive_in_order", |b| {
        b.iter(|| {
            let ok = s.on_receive(seq);
            seq += 1;
            std::hint::black_box(ok)
        })
    });
}

/// In-order accept while a loss episode is open (one distant range
/// in the index): the head-advance branch peeks the front range.
fn bench_on_receive_in_order_with_open_gap(c: &mut Criterion) {
    c.bench_function("reliability/on_receive_in_order_with_open_gap", |b| {
        b.iter_batched_ref(
            || {
                let mut s = ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3);
                assert!(s.on_receive(16_000), "far gap within horizon");
                (s, 0u64)
            },
            |(s, seq)| {
                let ok = s.on_receive(*seq);
                *seq += 1;
                std::hint::black_box(ok)
            },
            BatchSize::SmallInput,
        )
    });
}

/// Out-of-order insert + merge: 32 disjoint holes then 31 bridging
/// fills — the worst-case index churn one loss burst can cause
/// (63 inserts per iteration).
fn bench_on_receive_out_of_order_merge(c: &mut Criterion) {
    c.bench_function("reliability/on_receive_ooo_insert_merge_x63", |b| {
        b.iter_batched_ref(
            || ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3),
            |s| {
                for i in 0..32u64 {
                    s.on_receive(2 + 2 * i);
                }
                for i in 0..31u64 {
                    s.on_receive(3 + 2 * i);
                }
                std::hint::black_box(s.reorder_ranges())
            },
            BatchSize::SmallInput,
        )
    });
}

/// Cumulative-ack prefix pop over a 1000-packet window (PERF_AUDIT
/// §3.4 pop-front fast path).
fn bench_on_ack_cumulative(c: &mut Criterion) {
    c.bench_function("reliability/on_ack_cumulative_1000", |b| {
        b.iter_batched_ref(
            || {
                let mut s = ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3);
                for seq in 0..1000u64 {
                    s.on_send(descriptor(seq));
                }
                s
            },
            |s| s.on_ack(1000),
            BatchSize::SmallInput,
        )
    });
}

/// SACK-range prune over a 1000-packet window with a head loss
/// (R-3): one retained walk removing 999 sacked packets.
fn bench_on_ack_ranges(c: &mut Criterion) {
    c.bench_function("reliability/on_ack_ranges_1000_head_loss", |b| {
        b.iter_batched_ref(
            || {
                let mut s = ReliableStream::with_settings(Duration::from_millis(50), 16_384, 3);
                for seq in 0..1000u64 {
                    s.on_send(descriptor(seq));
                }
                s
            },
            |s| s.on_ack_ranges(0, &[(1, 1000)]),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_on_receive_in_order,
    bench_on_receive_in_order_with_open_gap,
    bench_on_receive_out_of_order_merge,
    bench_on_ack_cumulative,
    bench_on_ack_ranges,
);
criterion_main!(benches);
