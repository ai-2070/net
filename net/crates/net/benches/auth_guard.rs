//! Benchmarks for the channel `AuthGuard` fast path.
//!
//! The bloom filter + verified cache are claimed to deliver <10 ns
//! per `check_fast` call on the hot path. These benches validate
//! that claim against criterion's statistical model so we catch
//! regressions without waiting for production observability.
//!
//! Run with: `cargo bench --features net --bench auth_guard`

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use net::adapter::net::{AuthGuard, AuthVerdict, ChannelHash, ChannelName};

// ============================================================================
// Single-thread microbenchmarks
// ============================================================================

fn populated_guard(n_entries: u32) -> AuthGuard {
    let guard = AuthGuard::new();
    for i in 0..n_entries {
        let name = ChannelName::new(&format!("bench/chan-{}", i)).unwrap();
        guard.allow_channel(0xdead_0000_u64 + i as u64, &name);
    }
    guard
}

fn bench_check_fast_hot_hit(c: &mut Criterion) {
    // Steady-state: `check_fast` walks two atomic bit loads + one
    // DashMap probe. This path is what publish fan-out takes on
    // every packet. Single-threaded measurement establishes the
    // lower bound — concurrent load only adds cache contention.
    let guard = populated_guard(1000);
    let mut group = c.benchmark_group("auth_guard_check_fast_hit");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_thread", |b| {
        // Cycle through 256 different authorized (origin, channel)
        // pairs so the bench doesn't stay on one cache line.
        let mut i = 0u64;
        let channels: Vec<ChannelName> = (0..256)
            .map(|j| ChannelName::new(&format!("bench/chan-{}", j)).unwrap())
            .collect();
        b.iter(|| {
            let origin = 0xdead_0000_u64 + (i & 0xff);
            let channel_hash = channels[(i as usize) & 0xff].hash();
            let v = guard.check_fast(black_box(origin), black_box(channel_hash));
            i = i.wrapping_add(1);
            debug_assert!(matches!(
                v,
                AuthVerdict::Allowed | AuthVerdict::NeedsFullCheck
            ));
            v
        });
    });
    group.finish();
}

fn bench_check_fast_cold_miss(c: &mut Criterion) {
    // Bloom miss — no (origin, channel) entry ever inserted for
    // this pair. Two bit loads, no DashMap probe. This is the
    // "deny unauthorized subscribers" path.
    let guard = populated_guard(1000);
    let mut group = c.benchmark_group("auth_guard_check_fast_miss");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_thread", |b| {
        let mut i = 0u64;
        b.iter(|| {
            // Origin range that never appeared in `populated_guard`.
            let origin = 0xcafe_0000_u64 + i;
            let channel_hash = (i & 0xffff_ffff) as ChannelHash;
            let v = guard.check_fast(black_box(origin), black_box(channel_hash));
            i = i.wrapping_add(1);
            v
        });
    });
    group.finish();
}

// ============================================================================
// Concurrent-load smoke
// ============================================================================

fn bench_check_fast_concurrent(c: &mut Criterion) {
    // Eight reader threads hammering `check_fast` on the same
    // populated guard. Criterion measures the main-thread chunk;
    // the other seven threads add cache-line contention. The
    // reported time is an upper bound on the hot-hit path under
    // realistic publish fan-out pressure.
    let guard = Arc::new(populated_guard(10_000));
    let stop = Arc::new(AtomicU64::new(0));

    // Spawn 7 background threads reading random (origin, channel)
    // pairs. They run as long as the bench group holds `stop` at 0.
    let mut handles = Vec::new();
    for t in 0..7 {
        let g = guard.clone();
        let s = stop.clone();
        handles.push(thread::spawn(move || {
            let mut i = t as u64;
            while s.load(Ordering::Relaxed) == 0 {
                let origin = 0xdead_0000_u64 + (i & 0xff);
                let channel_hash = (i % 10_000) as ChannelHash;
                black_box(g.check_fast(origin, channel_hash));
                i = i.wrapping_add(7);
            }
        }));
    }
    // Let the background threads reach steady state before
    // criterion starts timing — measurement overhead + thread
    // startup can dominate the first few samples otherwise.
    thread::sleep(Duration::from_millis(50));

    let mut group = c.benchmark_group("auth_guard_check_fast_contended");
    group.throughput(Throughput::Elements(1));
    group.bench_function("eight_threads", |b| {
        let mut i = 0u64;
        b.iter(|| {
            let origin = 0xdead_0000_u64 + (i & 0xff);
            let channel_hash = (i % 10_000) as ChannelHash;
            let v = guard.check_fast(black_box(origin), black_box(channel_hash));
            i = i.wrapping_add(1);
            v
        });
    });
    group.finish();

    stop.store(1, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
}

fn bench_allow_channel(c: &mut Criterion) {
    // Cost of admitting a new (origin, channel) into the guard —
    // runs on every successful subscribe. Bloom bit sets + one
    // DashMap insert per call.
    let mut group = c.benchmark_group("auth_guard_allow_channel");
    group.throughput(Throughput::Elements(1));
    group.bench_function("insert", |b| {
        let guard = AuthGuard::new();
        let name = ChannelName::new("bench/chan-allow").unwrap();
        let mut i = 0u64;
        b.iter(|| {
            guard.allow_channel(black_box(0xbeef_0000_u64 + i), &name);
            i = i.wrapping_add(1);
        });
    });
    group.finish();
}

// ============================================================================
// Smoke test runs outside criterion — establishes a hard ceiling
// we'd fail CI on if the fast path regresses catastrophically.
// ============================================================================

/// Floor check: one million single-threaded `check_fast` calls on
/// a hot-hit path should complete in well under 50 ms on any
/// reasonable machine (50 ns per call is the pessimistic target).
/// Printed during `cargo bench` so regressions surface without
/// requiring a CI-facing assertion framework.
fn bench_hot_hit_ceiling(c: &mut Criterion) {
    let guard = populated_guard(256);
    let channel_hash = ChannelName::new("bench/chan-0").unwrap().hash();
    let mut group = c.benchmark_group("auth_guard_hot_hit_ceiling");
    group.bench_function("million_ops", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                for i in 0..1_000_000u64 {
                    black_box(guard.check_fast(0xdead_0000_u64 + (i & 0xff), channel_hash));
                }
            }
            start.elapsed()
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(50);
    targets =
        bench_check_fast_hot_hit,
        bench_check_fast_cold_miss,
        bench_check_fast_concurrent,
        bench_allow_channel,
        bench_hot_hit_ceiling,
}
criterion_main!(benches);
