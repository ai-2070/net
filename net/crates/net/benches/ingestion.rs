//! Benchmarks for event ingestion throughput.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use serde_json::json;

use net::bus::EventBus;
use net::config::{BackpressureMode, EventBusConfig};
use net::event::{InternalEvent, RawEvent};
use net::shard::ShardManager;
use net::timestamp::TimestampGenerator;

/// Benchmark shard ingest/drain through the public API.
///
/// Replaces a previous bench against the raw `RingBuffer` type. That
/// type is now `pub(crate)`, so the next-cleanest proxy is
/// `ShardManager`, which is what real ingestion paths use.
/// The numbers therefore include the per-shard atomic counter
/// updates and the `Mutex<Shard>` acquire/release — i.e. the actual
/// hot-path overhead, not just the lock-free ring atomics.
fn bench_shard(c: &mut Criterion) {
    let mut group = c.benchmark_group("shard");

    // Single shard so the hash routing is deterministic and the
    // bench measures the push/pop hot path rather than hashing.
    for capacity in [1024, 8192, 65536, 1_048_576].iter() {
        group.throughput(Throughput::Elements(1));

        // Pre-built `RawEvent` so each iteration measures only ingest,
        // not JSON construction.
        let raw_template = RawEvent::from_str(r#"{"i":0}"#);

        group.bench_with_input(
            BenchmarkId::new("ingest_raw", capacity),
            capacity,
            |b, &cap| {
                let manager = ShardManager::new(1, cap, BackpressureMode::DropOldest);
                b.iter(|| {
                    let _ = manager.ingest_raw(raw_template.clone());
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("ingest_raw_pop", capacity),
            capacity,
            |b, &cap| {
                let manager = ShardManager::new(1, cap, BackpressureMode::DropNewest);
                b.iter(|| {
                    let _ = manager.ingest_raw(raw_template.clone());
                    // Pop one to make room for the next push.
                    let _ = manager.with_shard(0, |s| s.try_pop());
                });
            },
        );
    }

    group.finish();
}

/// Benchmark timestamp generation.
fn bench_timestamp(c: &mut Criterion) {
    let mut group = c.benchmark_group("timestamp");
    group.throughput(Throughput::Elements(1));

    let ts_gen = TimestampGenerator::new();

    group.bench_function("next", |b| {
        b.iter(|| ts_gen.next());
    });

    group.bench_function("now_raw", |b| {
        b.iter(|| ts_gen.now_raw());
    });

    group.finish();
}

/// Benchmark event creation and serialization.
fn bench_event_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("event");
    group.throughput(Throughput::Elements(1));

    let ts_gen = TimestampGenerator::new();

    // End-to-end "build a Value, then make an event from it". This is a
    // strict SUPERSET of `json_creation` below: it builds the same
    // serde_json Value *and then* serializes it to the canonical `Bytes`
    // form. The delta over json_creation is one serde_json::to_vec into a
    // 128-byte-preallocated Vec, moved zero-copy into Bytes — necessary
    // work to produce the stored representation, not waste. So
    // internal_event_new > json_creation is expected, not "backwards".
    group.bench_function("internal_event_new", |b| {
        b.iter(|| {
            InternalEvent::from_value(json!({"token": "hello", "index": 42}), ts_gen.next(), 0)
        });
    });

    // The allocation-free ingestion floor: callers that already hold
    // pre-serialized bytes go through Shard::try_push_raw -> the
    // InternalEvent::new path, which only stamps metadata (Bytes clone is
    // a refcount bump, no serialize, no alloc). This is the lower bound
    // and the escape hatch from from_value's serialization cost.
    let pre_serialized =
        bytes::Bytes::from(serde_json::to_vec(&json!({"token": "hello", "index": 42})).unwrap());
    group.bench_function("internal_event_from_bytes", |b| {
        b.iter(|| InternalEvent::new(pre_serialized.clone(), ts_gen.next(), 0));
    });

    group.bench_function("json_creation", |b| {
        b.iter(|| json!({"token": "hello", "index": 42}));
    });

    group.finish();
}

/// Benchmark batch operations.
///
/// Steady-state pop-after-refill: every iteration pops `size`
/// elements then immediately re-pushes the same count to keep the
/// buffer at its target depth. The number we report is therefore
/// *not* a pure pop cost — it includes the refill and the
/// partial-pop branch. That's intentional for tracking real
/// workloads (consumers that drain into the same ring), but call
/// it out so future readers don't compare it against an
/// isolated-pop benchmark.
fn bench_batch_pop(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch");

    let raw_template = RawEvent::from_value(json!({"i": 0}));

    for batch_size in [100, 1000, 10000].iter() {
        let manager = ShardManager::new(1, 1 << 20, BackpressureMode::DropNewest);

        // Pre-fill the shard once so the first iteration starts in
        // steady state.
        for _ in 0..(*batch_size * 10) {
            let _ = manager.ingest_raw(raw_template.clone());
        }

        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("pop_batch_steady_state", batch_size),
            batch_size,
            |b, &size| {
                b.iter(|| {
                    let batch = manager
                        .with_shard(0, |s| s.pop_batch(size))
                        .unwrap_or_default();
                    // Refill what we popped to maintain depth.
                    let popped = batch.len();
                    for _ in 0..popped {
                        let _ = manager.ingest_raw(raw_template.clone());
                    }
                    batch
                });
            },
        );
    }

    group.finish();
}

/// PERF_AUDIT bench-coverage-gap #1 — drives
/// [`EventBus::ingest_raw`] with 1/2/4/8 producer threads. The
/// bus-level hot path the existing single-threaded `bench_shard`
/// can't see lives here: the bus-side `events_ingested` counter
/// (striped per PERF_AUDIT §1.1+§1.2) plus the dynamic-scaling
/// metrics collector subsampling (§1.3) all run on each
/// `ingest_raw`. Multi-producer contention is the regime where
/// the striped-counter fix shows up — under a single producer
/// the cache line never bounces, so the pre-fix vs post-fix
/// numbers look identical.
///
/// Bench shape:
///   - Pre-built [`RawEvent`] template cloned per push (a
///     refcount bump; no JSON re-serialization).
///   - `BackpressureMode::DropOldest` so a saturated ring never
///     stalls a producer.
///   - 16-shard bus so producers route to distinct shards
///     statistically — the bench measures contention on the
///     bus-level counter, NOT per-shard mutex contention (the
///     dynamic-scaling regime is a separate axis the audit
///     covers under §1.3 / §1.4).
fn bench_event_bus_ingest_raw_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_bus_ingest_raw_concurrent");

    const EVENTS_PER_BATCH: u64 = 8_192;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime for bench setup");

    for &producers in &[1usize, 2, 4, 8] {
        group.throughput(Throughput::Elements(EVENTS_PER_BATCH));
        group.bench_with_input(
            BenchmarkId::new("producers", producers),
            &producers,
            |b, &producers| {
                // Spin a fresh bus per iteration so a saturated
                // ring buffer from a prior iteration doesn't bias
                // the next sample. 16 shards × 64 K capacity =
                // plenty of headroom for `EVENTS_PER_BATCH`
                // events even under DropNewest, and the bench
                // routes through `ingest_raw`'s atomic-only path
                // (no JSON parse, no per-shard mutex serialization
                // unless producers happen to collide on a shard).
                let config = EventBusConfig::builder()
                    .num_shards(16)
                    .ring_buffer_capacity(65_536)
                    .backpressure_mode(BackpressureMode::DropOldest)
                    .build()
                    .unwrap();
                let bus = std::sync::Arc::new(
                    runtime
                        .block_on(EventBus::new(config))
                        .expect("EventBus::new for bench"),
                );
                let raw_template = RawEvent::from_str(r#"{"i":0}"#);

                b.iter(|| {
                    let per_thread = EVENTS_PER_BATCH as usize / producers;
                    std::thread::scope(|scope| {
                        for _ in 0..producers {
                            let bus = std::sync::Arc::clone(&bus);
                            let template = raw_template.clone();
                            scope.spawn(move || {
                                for _ in 0..per_thread {
                                    let _ = bus.ingest_raw(template.clone());
                                }
                            });
                        }
                    });
                });

                // EventBus::shutdown consumes `self`, so we can't
                // call it through the Arc handle. Drop the Arc and
                // rely on the bus's `Drop` to tear down background
                // workers. The runtime stays alive across
                // iterations to amortize its setup cost.
                drop(bus);
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_shard,
    bench_timestamp,
    bench_event_creation,
    bench_batch_pop,
    bench_event_bus_ingest_raw_concurrent,
);

criterion_main!(benches);
