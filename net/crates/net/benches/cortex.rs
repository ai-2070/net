//! Benchmarks for the CortEX adapter + NetDB layer.
//!
//! Run with: cargo bench --features cortex --bench cortex
//!
//! Measures four hot paths:
//! - **ingest** — how fast `create` / `store` enqueue events for fold
//! - **fold barrier** — how fast `wait_for_seq` returns once the fold
//!   task catches up (read-after-write latency)
//! - **query** — `find_many` / `count_where` over a populated state
//! - **snapshot** — bincode encode/decode of per-adapter state plus
//!   the NetDb whole-db bundle

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use net::adapter::net::cortex::memories::{MemoriesAdapter, MemoriesFilter};
use net::adapter::net::cortex::tasks::{TaskStatus, TasksAdapter, TasksFilter};
use net::adapter::net::netdb::NetDb;
use net::adapter::net::netdb::NetDbSnapshot;
use net::adapter::net::redex::Redex;
use tokio::runtime::Runtime;

const ORIGIN: u64 = 0xABCD_EF01;

/// A shared tokio runtime for all bench scenarios. `CortexAdapter::open`
/// and every ingest path touch `tokio::spawn` / `broadcast::Sender::send`
/// — both require a live reactor. We enter this runtime at the top of
/// each `bench_function` so the adapter code finds its context.
fn rt() -> Arc<Runtime> {
    Arc::new(Runtime::new().expect("tokio runtime"))
}

// ============================================================================
// Ingest throughput — how fast events enqueue onto the append path.
// wait_for_seq is deliberately NOT called here; this measures the hot
// producer path, not the fold round-trip.
// ============================================================================

fn bench_ingest(c: &mut Criterion) {
    let mut group = c.benchmark_group("cortex_ingest");
    group.throughput(Throughput::Elements(1));
    let runtime = rt();

    group.bench_function("tasks_create", |b| {
        let _enter = runtime.enter();
        let redex = Redex::new();
        let tasks = runtime
            .block_on(TasksAdapter::open(&redex, ORIGIN))
            .unwrap();
        let mut id: u64 = 0;
        b.iter(|| {
            id = id.wrapping_add(1);
            tasks.create(id, "t", 0).unwrap()
        });
    });

    group.bench_function("memories_store", |b| {
        let _enter = runtime.enter();
        let redex = Redex::new();
        let memories = runtime
            .block_on(MemoriesAdapter::open(&redex, ORIGIN))
            .unwrap();
        // Pre-build the tags vec once and `clone()` outside the timed
        // block so the per-iteration cost we measure is the store path,
        // not the Vec<String> allocation. The `store` signature takes
        // an owned `Vec<String>`; cloning per iteration is unavoidable
        // but the clone itself shouldn't be inside `b.iter`.
        let tags_template = vec!["bench".to_string()];
        let mut id: u64 = 0;
        b.iter_batched(
            || tags_template.clone(),
            |tags| {
                id = id.wrapping_add(1);
                memories.store(id, "content", tags, "source", 0).unwrap()
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// Fold barrier — the read-after-write primitive.
// Enqueues one event, awaits wait_for_seq, repeats. Measures the full
// round-trip: ingest → fold task picks it up → materialized state
// advances → wait_for_seq returns.
// ============================================================================

fn bench_fold_barrier(c: &mut Criterion) {
    let mut group = c.benchmark_group("cortex_fold_barrier");
    group.throughput(Throughput::Elements(1));

    let runtime = rt();

    group.bench_function("tasks_create_and_wait", |b| {
        let _enter = runtime.enter();
        let redex = Redex::new();
        let tasks = Arc::new(
            runtime
                .block_on(TasksAdapter::open(&redex, ORIGIN))
                .unwrap(),
        );
        let mut id: u64 = 0;
        b.iter(|| {
            id = id.wrapping_add(1);
            let seq = tasks.create(id, "t", 0).unwrap();
            runtime.block_on(tasks.wait_for_seq(seq));
        });
    });

    group.bench_function("memories_store_and_wait", |b| {
        let _enter = runtime.enter();
        let redex = Redex::new();
        let memories = Arc::new(
            runtime
                .block_on(MemoriesAdapter::open(&redex, ORIGIN))
                .unwrap(),
        );
        let tags = vec!["bench".to_string()];
        let mut id: u64 = 0;
        b.iter(|| {
            id = id.wrapping_add(1);
            let seq = memories
                .store(id, "content", tags.clone(), "source", 0)
                .unwrap();
            runtime.block_on(memories.wait_for_seq(seq));
        });
    });

    group.finish();
}

// ============================================================================
// Query throughput — Prisma-ish `find_many` / `count_where` / `exists_where`
// over a populated state. We pre-populate and then benchmark lookups.
// ============================================================================

fn populated_tasks(runtime: &Runtime, n: usize) -> TasksAdapter {
    let _enter = runtime.enter();
    let redex = Redex::new();
    let tasks = runtime
        .block_on(TasksAdapter::open(&redex, ORIGIN))
        .unwrap();
    let mut last_seq = 0;
    for i in 0..n {
        let id = (i + 1) as u64;
        last_seq = tasks.create(id, format!("task-{}", i), i as u64).unwrap();
        if i % 3 == 0 {
            last_seq = tasks.complete(id, i as u64).unwrap();
        }
    }
    runtime.block_on(tasks.wait_for_seq(last_seq));
    tasks
}

fn populated_memories(runtime: &Runtime, n: usize) -> MemoriesAdapter {
    let _enter = runtime.enter();
    let redex = Redex::new();
    let memories = runtime
        .block_on(MemoriesAdapter::open(&redex, ORIGIN))
        .unwrap();
    let tags_a = vec!["alpha".to_string()];
    let tags_b = vec!["beta".to_string()];
    let mut last_seq = 0;
    for i in 0..n {
        let id = (i + 1) as u64;
        let tags = if i % 2 == 0 {
            tags_a.clone()
        } else {
            tags_b.clone()
        };
        last_seq = memories
            .store(id, format!("content-{}", i), tags, "src", i as u64)
            .unwrap();
    }
    runtime.block_on(memories.wait_for_seq(last_seq));
    memories
}

fn bench_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("cortex_query");
    let runtime = rt();

    for &n in &[100usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n as u64));

        let tasks = populated_tasks(&runtime, n);
        let state = tasks.state();

        let completed_filter = TasksFilter {
            status: Some(TaskStatus::Completed),
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("tasks_find_many", n), &n, |b, _| {
            let guard = state.read();
            b.iter(|| guard.find_many(&completed_filter));
        });

        group.bench_with_input(BenchmarkId::new("tasks_count_where", n), &n, |b, _| {
            let guard = state.read();
            b.iter(|| guard.count_where(&completed_filter));
        });

        group.bench_with_input(BenchmarkId::new("tasks_find_unique", n), &n, |b, _| {
            let guard = state.read();
            b.iter(|| guard.find_unique(1));
        });

        let memories = populated_memories(&runtime, n);
        let mem_state = memories.state();
        let alpha_filter = MemoriesFilter {
            tag: Some("alpha".to_string()),
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("memories_find_many_tag", n), &n, |b, _| {
            let guard = mem_state.read();
            b.iter(|| guard.find_many(&alpha_filter));
        });

        group.bench_with_input(BenchmarkId::new("memories_count_where", n), &n, |b, _| {
            let guard = mem_state.read();
            b.iter(|| guard.count_where(&alpha_filter));
        });
    }

    group.finish();
}

// ============================================================================
// Snapshot encode/decode. Measures the bincode serialize / deserialize
// round-trip on per-adapter state and on the whole-db NetDb bundle.
// ============================================================================

fn bench_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("cortex_snapshot");
    let runtime = rt();

    for &n in &[100usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n as u64));

        let tasks = populated_tasks(&runtime, n);
        let memories = populated_memories(&runtime, n);

        group.bench_with_input(BenchmarkId::new("tasks_encode", n), &n, |b, _| {
            b.iter(|| tasks.snapshot().unwrap());
        });

        let (tasks_bytes, _) = tasks.snapshot().unwrap();
        let tasks_bytes_len = tasks_bytes.len();
        group.bench_with_input(BenchmarkId::new("memories_encode", n), &n, |b, _| {
            b.iter(|| memories.snapshot().unwrap());
        });

        let (mem_bytes, mem_last_seq) = memories.snapshot().unwrap();
        let (task_bytes, task_last_seq) = tasks.snapshot().unwrap();

        // NetDb whole-bundle encode — mirrors NetDb::snapshot. Build
        // the bundle ONCE outside the iter; encode() takes &self so
        // per-iteration cloning of the inner Vec<u8>s would skew the
        // measurement toward memcpy instead of bincode serialization.
        let bundle_snapshot = NetDbSnapshot {
            tasks: Some((task_bytes.clone(), task_last_seq)),
            memories: Some((mem_bytes.clone(), mem_last_seq)),
        };
        group.bench_with_input(
            BenchmarkId::new(
                format!(
                    "netdb_bundle_encode_bytes_{}",
                    tasks_bytes_len + mem_bytes.len()
                ),
                n,
            ),
            &n,
            |b, _| {
                b.iter(|| bundle_snapshot.encode().unwrap());
            },
        );

        let bundle = NetDbSnapshot {
            tasks: Some((task_bytes.clone(), task_last_seq)),
            memories: Some((mem_bytes.clone(), mem_last_seq)),
        }
        .encode()
        .unwrap();

        group.bench_with_input(BenchmarkId::new("netdb_bundle_decode", n), &n, |b, _| {
            b.iter(|| NetDbSnapshot::decode(&bundle).unwrap());
        });
    }

    group.finish();
}

// ============================================================================
// NetDb build — constructing a NetDb handle with both models.
// ============================================================================

// Disabled: `Redex::new()` + `with_tasks()` + `with_memories()`
// allocates 2 × 64 MiB `HeapSegment`s (see
// `src/adapter/net/redex/file.rs:82`). Criterion's warmup runs
// this closure thousands of times in ~3 s, and macOS's allocator
// aborts with a bare-allocation-failure signal partway through.
// The bench also measures construction cost, which is dominated
// by the preallocated segments and isn't representative of
// steady-state NetDb open.
//
// TODO: re-enable with a bounded-capacity `RedexFileConfig` or
// rewrite via `Criterion::iter_batched` so the 128 MiB/iter
// allocation amortizes across a measured batch instead of
// repeating inside the hot loop.
#[allow(dead_code)]
fn bench_netdb_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("netdb_build");
    group.throughput(Throughput::Elements(1));
    let runtime = rt();

    group.bench_function("open_both", |b| {
        let _enter = runtime.enter();
        b.iter(|| {
            let redex = Redex::new();
            runtime
                .block_on(
                    NetDb::builder(redex)
                        .origin(ORIGIN)
                        .with_tasks()
                        .with_memories()
                        .build(),
                )
                .unwrap()
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_ingest,
    bench_fold_barrier,
    bench_query,
    bench_snapshot,
    // bench_netdb_build — disabled, see the function's docstring
    // for the allocator-abort context.
);
criterion_main!(benches);
