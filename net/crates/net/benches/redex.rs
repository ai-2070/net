//! Microbenchmarks for the RedEX storage primitive.
//!
//! Run with: cargo bench --features redex --bench redex
//!            cargo bench --features "redex redex-disk" --bench redex
//!
//! These answer "is the local log the bottleneck?" independently of
//! CortEX / NetDB. The end-to-end story (ingest → fold → query) lives
//! in `benches/cortex.rs`.
//!
//! Measures:
//! - **Append** throughput at inline (≤8 B) and heap (32 / 256 / 1024 B) sizes,
//!   with and without disk durability (the latter gated on `redex-disk`)
//! - **Batch append** throughput, on heap and on disk
//! - **Disk policies** — single and batch appends across every
//!   `FsyncPolicy` variant (`Never`, `EveryN`, `Interval`,
//!   `IntervalOrBytes`). Confirms the appender doesn't pay
//!   fsync cost when the worker absorbs it (Phases 3 + 4).
//! - **Tail latency** — append → subscriber observes the new seq
//!
//! ## Why the append loops recreate the file
//!
//! Every `append` grows an append-only segment capped at
//! `MAX_SEGMENT_BYTES` (3 GB) with no in-loop retention/rollover. A
//! naive `b.iter(|| f.append(..))` over a single reused file fills
//! that 3 GB on a fast machine mid-measurement, after which every
//! further `append` returns `PayloadTooLarge` and the `.unwrap()`
//! panics. The append benches therefore drive `b.iter_custom` through
//! [`timed_appends`], which recreates the backing file every
//! [`BENCH_FILE_CAP_BYTES`] of payload — only the appends are timed,
//! file (re)creation and teardown are excluded. This also bounds peak
//! RAM and on-disk footprint to a single file's worth.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures::StreamExt;
use net::adapter::net::channel::ChannelName;
#[cfg(feature = "redex-disk")]
use net::adapter::net::redex::FsyncPolicy;
use net::adapter::net::redex::{Redex, RedexFileConfig};
use tokio::runtime::Runtime;

fn rt() -> Arc<Runtime> {
    Arc::new(Runtime::new().expect("tokio runtime"))
}

fn cn(s: &str) -> ChannelName {
    ChannelName::new(s).unwrap()
}

/// Per-file payload budget before [`timed_appends`] recreates the
/// backing file. Comfortably under `MAX_SEGMENT_BYTES` (3 GB) so a
/// long criterion run never trips `PayloadTooLarge`, yet large enough
/// that file (re)creation amortises across many appends and the
/// segment reaches steady state. Doubles as the peak RAM / on-disk
/// bound per bench.
const BENCH_FILE_CAP_BYTES: usize = 256 * 1024 * 1024;

/// How many ops a single file should absorb given `bytes_per_op`
/// (payload size for single appends; `batch_len * payload` for batch
/// appends). Floored at 1 so a pathologically large payload still
/// makes progress.
fn ops_per_file(bytes_per_op: usize) -> u64 {
    (BENCH_FILE_CAP_BYTES / bytes_per_op.max(1)).max(1) as u64
}

/// Time exactly `iters` append-style ops, recreating the backing file
/// every `per_file` ops so the append-only segment never fills to
/// `MAX_SEGMENT_BYTES`. Only the `op` calls are timed; `make` and
/// dropping its result (which, for disk benches, deletes the file's
/// temp dir via `DirGuard`) are excluded.
///
/// `make` returns whatever state must stay alive for the run — a
/// `(RedexFile, Redex, …)` tuple — and `op` is handed `&that`. The
/// tuple drops front-to-back at the end of each file's run, so the
/// `RedexFile` closes before any `DirGuard` removes its directory.
fn timed_appends<T>(
    iters: u64,
    per_file: u64,
    mut make: impl FnMut() -> T,
    op: impl Fn(&T),
) -> Duration {
    let mut total = Duration::ZERO;
    let mut remaining = iters;
    while remaining > 0 {
        let file = make();
        let n = per_file.min(remaining);
        let start = Instant::now();
        for _ in 0..n {
            op(&file);
        }
        total += start.elapsed();
        remaining -= n;
        drop(file);
    }
    total
}

/// Deletes a per-file temp dir on drop so a long disk bench keeps
/// only one file's data on disk at a time. Held last in the
/// `make`-returned tuple so the `RedexFile` (held first) closes
/// before the directory is removed.
#[cfg(feature = "redex-disk")]
struct DirGuard(std::path::PathBuf);

#[cfg(feature = "redex-disk")]
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ============================================================================
// Append — inline vs heap.
//
// Inline is the zero-alloc fast path for ≤8 B payloads (sensor ticks,
// counter bumps). Heap pays one memcpy into the append-only segment
// and proportional bytes through the checksum, so we sweep payload
// size to measure where that cost starts to bite.
// ============================================================================

fn bench_append_inline(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_inline");
    group.throughput(Throughput::Elements(1));

    // Inline payloads (≤8 B) live in the entry record, not the heap
    // segment, so `append_inline` never grows `live_bytes` and a
    // single reused file is safe here.
    group.bench_function("heap_file", |b| {
        let r = Redex::new();
        let f = r
            .open_file(&cn("bench/inline/heap"), RedexFileConfig::default())
            .unwrap();
        let payload: [u8; 8] = [0xAB; 8];
        b.iter(|| f.append_inline(&payload).unwrap());
    });

    group.finish();
}

fn bench_append_heap(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_heap");

    for &size in &[32usize, 256, 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        let payload = vec![0xCDu8; size];

        group.bench_with_input(BenchmarkId::new("heap_file", size), &size, |b, &s| {
            b.iter_custom(|iters| {
                timed_appends(
                    iters,
                    ops_per_file(s),
                    || {
                        let r = Redex::new();
                        let f = r
                            .open_file(
                                &cn(&format!("bench/heap/mem/{}", s)),
                                RedexFileConfig::default(),
                            )
                            .unwrap();
                        (f, r)
                    },
                    |(f, _r)| {
                        f.append(&payload).unwrap();
                    },
                )
            });
        });
    }

    group.finish();
}

// ============================================================================
// Append: no-watcher fast path vs. with-tail path.
//
// `append` skips the `Bytes::copy_from_slice` event materialization
// when nobody is tailing — most production traffic. The "with_tail"
// variant pre-subscribes (and drains) a tail stream so the copy +
// `notify_watchers` path is exercised. The delta between the two is
// the cost paid solely for live delivery.
// ============================================================================

fn bench_append_watcher_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_watcher_paths");
    let runtime = rt();
    let payload = vec![0xCDu8; 256];
    group.throughput(Throughput::Bytes(payload.len() as u64));

    group.bench_function("no_watchers", |b| {
        b.iter_custom(|iters| {
            timed_appends(
                iters,
                ops_per_file(payload.len()),
                || {
                    let r = Redex::new();
                    let f = r
                        .open_file(&cn("bench/watcher/none"), RedexFileConfig::default())
                        .unwrap();
                    (f, r)
                },
                |(f, _r)| {
                    f.append(&payload).unwrap();
                },
            )
        });
    });

    group.bench_function("with_tail", |b| {
        b.iter_custom(|iters| {
            // Heap files are non-persistent, so no fsync workers spawn
            // at open and we don't need an ambient runtime context —
            // `runtime.block_on` drives the drain explicitly. Recreate
            // the file + its tail stream every `per_file` ops so the
            // segment never fills.
            let per_file = ops_per_file(payload.len());
            let mut total = Duration::ZERO;
            let mut remaining = iters;
            while remaining > 0 {
                let r = Redex::new();
                let f = r
                    .open_file(&cn("bench/watcher/with"), RedexFileConfig::default())
                    .unwrap();
                let mut stream = Box::pin(f.tail(0));
                let n = per_file.min(remaining);
                let start = Instant::now();
                for _ in 0..n {
                    f.append(&payload).unwrap();
                    // Drain so the bounded buffer never saturates and
                    // the benchmark doesn't measure disconnect handling.
                    runtime.block_on(async {
                        let _ = stream.next().await.unwrap();
                    });
                }
                total += start.elapsed();
                remaining -= n;
                drop(stream);
                drop(f);
                drop(r);
            }
            total
        });
    });

    group.finish();
}

// ============================================================================
// Batch append.
//
// Amortizes the seq-allocation and lock overhead across N payloads in
// one call. We benchmark a batch of 64 small heap payloads.
// ============================================================================

fn bench_append_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_batch");
    const BATCH: usize = 64;
    group.throughput(Throughput::Elements(BATCH as u64));

    group.bench_function(format!("batch_{}_x_64B", BATCH), |b| {
        let payloads: Vec<Bytes> = (0..BATCH).map(|_| Bytes::from(vec![0xEE; 64])).collect();
        b.iter_custom(|iters| {
            timed_appends(
                iters,
                ops_per_file(BATCH * 64),
                || {
                    let r = Redex::new();
                    let f = r
                        .open_file(&cn("bench/batch/heap"), RedexFileConfig::default())
                        .unwrap();
                    (f, r)
                },
                |(f, _r)| {
                    f.append_batch(&payloads).unwrap();
                },
            )
        });
    });

    group.finish();
}

// ============================================================================
// Disk durability (feature `redex-disk`).
//
// Measures the cost of the disk segment write on the append path.
// Compare these numbers against `redex_append_heap::heap_file` at the
// same payload size to see the overhead of durability.
// ============================================================================

#[cfg(feature = "redex-disk")]
fn bench_append_disk(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_disk");

    for &size in &[32usize, 256, 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        let payload = vec![0xABu8; size];

        group.bench_with_input(BenchmarkId::new("disk_file", size), &size, |b, &s| {
            b.iter_custom(|iters| {
                timed_appends(
                    iters,
                    ops_per_file(s),
                    || {
                        // Fresh isolated dir per file so `DirGuard` can
                        // reclaim it; a fresh channel name avoids
                        // recovering stale on-disk state.
                        let dir = tempdir_prefix("redex_bench_disk");
                        let r = Redex::new().with_persistent_dir(&dir);
                        let cfg = RedexFileConfig::default().with_persistent(true);
                        let name = cn(&format!("bench/disk/{}/{}", s, rand_suffix()));
                        let f = r.open_file(&name, cfg).unwrap();
                        (f, r, DirGuard(dir))
                    },
                    |(f, _r, _g)| {
                        f.append(&payload).unwrap();
                    },
                )
            });
        });
    }

    group.finish();
}

// Disk batch append. Targets the buffered-write path in
// `DiskSegment::append_entries_inner`: a batch of N entries should
// emit at most 3 syscalls (one each to dat / idx / ts), not 3·N.
#[cfg(feature = "redex-disk")]
fn bench_append_batch_disk(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_batch_disk");
    const BATCH: usize = 64;
    group.throughput(Throughput::Elements(BATCH as u64));

    for &size in &[64usize, 1024] {
        group.bench_with_input(
            BenchmarkId::new(format!("batch_{}_x", BATCH), size),
            &size,
            |b, &s| {
                let payloads: Vec<Bytes> = (0..BATCH).map(|_| Bytes::from(vec![0xEE; s])).collect();
                b.iter_custom(|iters| {
                    timed_appends(
                        iters,
                        ops_per_file(BATCH * s),
                        || {
                            let dir = tempdir_prefix("redex_bench_batch_disk");
                            let r = Redex::new().with_persistent_dir(&dir);
                            let cfg = RedexFileConfig::default().with_persistent(true);
                            let name = cn(&format!("bench/disk_batch/{}/{}", s, rand_suffix()));
                            let f = r.open_file(&name, cfg).unwrap();
                            (f, r, DirGuard(dir))
                        },
                        |(f, _r, _g)| {
                            f.append_batch(&payloads).unwrap();
                        },
                    )
                });
            },
        );
    }

    group.finish();
}

// Single-append cost across every `FsyncPolicy` variant. The
// non-`Never` policies should track close to `Never` because the
// fsync runs on a background worker (Phases 3 and 4); the
// appender pays only for the page-cache write. A regression that
// re-introduced synchronous fsync on the appender would surface
// here as a 10x–100x latency jump on `every_n_1` / `every_n_64`
// vs `never`.
#[cfg(feature = "redex-disk")]
fn bench_append_disk_policies(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_disk_policies");
    let runtime = rt();
    let payload = vec![0xABu8; 256];
    group.throughput(Throughput::Bytes(payload.len() as u64));

    let policies: &[(&str, FsyncPolicy)] = &[
        ("never", FsyncPolicy::Never),
        ("every_n_1", FsyncPolicy::EveryN(1)),
        ("every_n_64", FsyncPolicy::EveryN(64)),
        (
            "interval_50ms",
            FsyncPolicy::Interval(std::time::Duration::from_millis(50)),
        ),
        (
            "interval_or_bytes",
            FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_millis(50),
                max_bytes: 1024 * 1024,
            },
        ),
    ];

    for (name, policy) in policies {
        let policy = *policy;
        group.bench_with_input(
            BenchmarkId::new("disk_file_256B", name),
            &policy,
            |b, &p| {
                b.iter_custom(|iters| {
                    // Fsync workers spawn at file-open time, so each
                    // `make` opens under the runtime context. The appends
                    // themselves are synchronous (they don't await).
                    let _enter = runtime.enter();
                    timed_appends(
                        iters,
                        ops_per_file(payload.len()),
                        || {
                            let dir = tempdir_prefix("redex_bench_policies");
                            let r = Redex::new().with_persistent_dir(&dir);
                            let cfg = RedexFileConfig::default()
                                .with_persistent(true)
                                .with_fsync_policy(p);
                            let chan = cn(&format!("bench/policies/{}", rand_suffix()));
                            let f = r.open_file(&chan, cfg).unwrap();
                            (f, r, DirGuard(dir))
                        },
                        |(f, _r, _g)| {
                            f.append(&payload).unwrap();
                        },
                    )
                });
            },
        );
    }

    group.finish();
}

// Batch-append cost across every `FsyncPolicy` variant. Combines
// Phase 1 (syscall coalescing) with Phase 3 / 4 (worker offload).
// `BATCH=64` × 64 B at the policy that fires fastest (`every_n_1`)
// is the most adversarial case — each batch crosses the cadence
// AND notifies the worker. Should still track `never` closely.
#[cfg(feature = "redex-disk")]
fn bench_append_batch_disk_policies(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_append_batch_disk_policies");
    let runtime = rt();
    const BATCH: usize = 64;
    group.throughput(Throughput::Elements(BATCH as u64));

    let policies: &[(&str, FsyncPolicy)] = &[
        ("never", FsyncPolicy::Never),
        ("every_n_1", FsyncPolicy::EveryN(1)),
        (
            "interval_or_bytes_small",
            // Small max_bytes so the byte arm fires every batch.
            FsyncPolicy::IntervalOrBytes {
                period: std::time::Duration::from_secs(60),
                max_bytes: 1024,
            },
        ),
    ];

    for (name, policy) in policies {
        let policy = *policy;
        group.bench_with_input(
            BenchmarkId::new("batch_64_x_64B", name),
            &policy,
            |b, &p| {
                let payloads: Vec<Bytes> =
                    (0..BATCH).map(|_| Bytes::from(vec![0xEE; 64])).collect();
                b.iter_custom(|iters| {
                    let _enter = runtime.enter();
                    timed_appends(
                        iters,
                        ops_per_file(BATCH * 64),
                        || {
                            let dir = tempdir_prefix("redex_bench_batch_policies");
                            let r = Redex::new().with_persistent_dir(&dir);
                            let cfg = RedexFileConfig::default()
                                .with_persistent(true)
                                .with_fsync_policy(p);
                            let chan = cn(&format!("bench/batch_policies/{}", rand_suffix()));
                            let f = r.open_file(&chan, cfg).unwrap();
                            (f, r, DirGuard(dir))
                        },
                        |(f, _r, _g)| {
                            f.append_batch(&payloads).unwrap();
                        },
                    )
                });
            },
        );
    }

    group.finish();
}

#[cfg(feature = "redex-disk")]
fn tempdir_prefix(prefix: &str) -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("{}_{}", prefix, rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(feature = "redex-disk")]
fn rand_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    // Monotonic counter + nanos: `timed_appends` recreates files in a
    // tight loop, so two `rand_suffix()` calls can land in the same
    // nanosecond on a fast clock; the counter keeps channel/dir names
    // unique regardless.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}_{}", nanos, n)
}

// ============================================================================
// Tail latency — append → subscriber observes the new event.
//
// Pre-subscribes a tail stream, measures the time from `append()`
// producing a new seq to `stream.next().await` returning it. This is
// the read-after-write path without the CortEX fold on top.
// ============================================================================

fn bench_tail_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("redex_tail");
    group.throughput(Throughput::Elements(1));
    let runtime = rt();

    group.bench_function("append_to_next", |b| {
        let payload = vec![0u8; 32];
        b.iter_custom(|iters| {
            let per_file = ops_per_file(payload.len());
            let mut total = Duration::ZERO;
            let mut remaining = iters;
            while remaining > 0 {
                let r = Redex::new();
                let f = r
                    .open_file(&cn("bench/tail/latency"), RedexFileConfig::default())
                    .unwrap();
                let mut stream = Box::pin(f.tail(0));
                let n = per_file.min(remaining);
                let start = Instant::now();
                for _ in 0..n {
                    f.append(&payload).unwrap();
                    runtime.block_on(async {
                        let _ = stream.next().await.unwrap();
                    });
                }
                total += start.elapsed();
                remaining -= n;
                drop(stream);
                drop(f);
                drop(r);
            }
            total
        });
    });

    group.finish();
}

#[cfg(feature = "redex-disk")]
criterion_group!(
    benches,
    bench_append_inline,
    bench_append_heap,
    bench_append_watcher_paths,
    bench_append_batch,
    bench_append_disk,
    bench_append_batch_disk,
    bench_append_disk_policies,
    bench_append_batch_disk_policies,
    bench_tail_latency,
);

#[cfg(not(feature = "redex-disk"))]
criterion_group!(
    benches,
    bench_append_inline,
    bench_append_heap,
    bench_append_watcher_paths,
    bench_append_batch,
    bench_tail_latency,
);

criterion_main!(benches);
