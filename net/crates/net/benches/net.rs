//! Benchmarks for Net adapter throughput.
//!
//! Run with: cargo bench --features net --bench net
//!
//! These benchmarks measure:
//! - Packet header serialization/deserialization
//! - Encryption/decryption throughput
//! - Event frame serialization
//! - End-to-end send/receive throughput
//! - Shared vs Thread-local pool comparison
//! - Multi-threaded concurrency (Phase 2)
//! - Router and fair scheduler (Phase 3A)

// The diff-application benches intentionally exercise the
// deprecated `DiffEngine::apply` (version-naive) entry point. The
// deprecation warning is the right signal for production callers
// but only noise for the benchmark, which is measuring the
// primitive's cost.
#![allow(deprecated)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use net::adapter::net::identity::EntityId;
use net::adapter::net::{
    // Phase 4A-4G: Behavior Plane - Capabilities, Diffs, Metadata, APIs, Rules, Context & LoadBalancing
    behavior::{
        fold::{capability_bridge, CapabilityFold, Fold},
        Action, AlertSeverity, ApiAnnouncement, ApiEndpoint, ApiMethod, ApiParameter, ApiQuery,
        ApiRegistry, ApiSchema, ApiVersion, Baggage, CapabilityAnnouncement, CapabilityFilter,
        CapabilityRequirement, CapabilitySet, CompareOp, Condition, ConditionExpr, Context,
        ContextStore, Endpoint, GpuInfo, GpuVendor, HardwareCapabilities, HealthStatus,
        LbRequestContext, LoadBalancer, LoadBalancerConfig, LoadMetrics, LocationInfo, LogLevel,
        MetadataQuery, MetadataStore, Modality, ModelCapability, NatType, NetworkTier,
        NodeMetadata, NodeStatus, Priority, PropagationContext, Region, ResourceLimits, Rule,
        RuleContext, RuleEngine, RuleSet, SamplingStrategy, SchemaType, SoftwareCapabilities, Span,
        SpanKind, Strategy, ToolCapability, TopologyHints, TraceId,
    },
    AdaptiveBatcher,
    Capabilities,
    CapabilityAd,
    CircuitBreaker,
    EventFrame,
    FailureDetector,
    FailureDetectorConfig,
    FairScheduler,
    LocalGraph,
    LossSimulator,
    MultiHopPacketBuilder,
    NetHeader,
    PacketFlags,
    PacketPool,
    Pingwave,
    RecoveryManager,
    RoutingHeader,
    RoutingTable,
    StaticKeypair,
    ThreadLocalPool,
    NONCE_SIZE,
    ROUTING_HEADER_SIZE,
};

/// Benchmark packet header serialization/deserialization.
fn bench_header(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_header");
    group.throughput(Throughput::Elements(1));

    let nonce = [0x42u8; NONCE_SIZE];
    let header = NetHeader::new(0x1234, 0x5678, 42, nonce, 1000, 10, PacketFlags::RELIABLE);

    group.bench_function("serialize", |b| {
        b.iter(|| header.to_bytes());
    });

    let header_bytes = header.to_bytes();
    group.bench_function("deserialize", |b| {
        b.iter(|| NetHeader::from_bytes(&header_bytes));
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let bytes = header.to_bytes();
            NetHeader::from_bytes(&bytes)
        });
    });

    group.finish();
}

/// Benchmark event frame serialization.
fn bench_event_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_event_frame");

    // Test with different event sizes
    for event_size in [64, 256, 1024, 4096].iter() {
        let event_data = Bytes::from(vec![0x42u8; *event_size]);

        group.throughput(Throughput::Bytes(*event_size as u64));
        group.bench_with_input(
            BenchmarkId::new("write_single", event_size),
            &event_data,
            |b, data| {
                let events = vec![data.clone()];
                b.iter(|| {
                    let mut buf = BytesMut::with_capacity(event_size + 4);
                    EventFrame::write_events(&events, &mut buf);
                    buf
                });
            },
        );

        // Same write, but reusing one buffer instead of allocating a
        // fresh `BytesMut` per iteration.
        //
        // `write_single` is non-monotonic in payload size (256B is
        // slower than 1024B) on macOS. That is NOT a copy/branch in
        // `write_events` — it lowers to a single `memcpy` intrinsic
        // regardless of size. It is the system allocator: with no
        // `#[global_allocator]`, libmalloc's "nano" zone services
        // allocations ≤256B on a fast path; a 256B payload plus the
        // 4-byte length prefix (260B) spills into the slower magazine
        // zone, while 64B stays in nano. The pooled production paths
        // (`PacketPool` / `ThreadLocalPool`) reuse buffers and show no
        // such inversion — `net_encryption/encrypt` is monotonic. This
        // variant reuses the buffer to demonstrate that the allocator,
        // not the write, owns the 128–512B cost.
        group.bench_with_input(
            BenchmarkId::new("write_single_reused", event_size),
            &event_data,
            |b, data| {
                let events = vec![data.clone()];
                let mut buf = BytesMut::with_capacity(event_size + 4);
                b.iter(|| {
                    buf.clear();
                    EventFrame::write_events(&events, &mut buf);
                });
            },
        );
    }

    // Test with different batch sizes
    for batch_size in [1, 10, 50, 100].iter() {
        let event_data = Bytes::from(vec![0x42u8; 64]);
        let events: Vec<Bytes> = (0..*batch_size).map(|_| event_data.clone()).collect();
        let total_bytes = 64 * *batch_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("write_batch", batch_size),
            &events,
            |b, events| {
                b.iter(|| {
                    let mut buf = BytesMut::with_capacity(total_bytes + 4 * events.len());
                    EventFrame::write_events(events, &mut buf);
                    buf
                });
            },
        );
    }

    // Benchmark read
    let events: Vec<Bytes> = (0..10).map(|_| Bytes::from(vec![0x42u8; 64])).collect();
    let mut buf = BytesMut::new();
    EventFrame::write_events(&events, &mut buf);
    let serialized = buf.freeze();

    group.throughput(Throughput::Elements(10));
    group.bench_function("read_batch_10", |b| {
        b.iter(|| EventFrame::read_events(serialized.clone(), 10));
    });

    group.finish();
}

/// Benchmark packet pool allocation.
fn bench_packet_pool(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_packet_pool");
    group.throughput(Throughput::Elements(1));

    let key = [0u8; 32];

    for pool_size in [16, 64, 256].iter() {
        let pool = PacketPool::new(*pool_size, &key, 0x1234);

        group.bench_with_input(
            BenchmarkId::new("get_return", pool_size),
            &pool,
            |b, pool| {
                b.iter(|| {
                    let builder = pool.get();
                    drop(builder);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark packet building (header + events + encryption).
fn bench_packet_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_packet_build");

    let key = [0u8; 32];
    let pool = PacketPool::new(64, &key, 0x1234);

    for event_count in [1, 10, 50].iter() {
        let event_data = Bytes::from(vec![0x42u8; 64]);
        let events: Vec<Bytes> = (0..*event_count).map(|_| event_data.clone()).collect();
        let total_bytes = 64 * *event_count;

        group.throughput(Throughput::Bytes(total_bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("build_packet", event_count),
            &events,
            |b, events| {
                b.iter(|| {
                    let mut builder = pool.get();
                    builder.build(0x5678, 42, events, PacketFlags::NONE)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark encryption/decryption (via PacketBuilder — ChaCha20-Poly1305
/// IETF, 12-byte template+counter nonces; see crypto::PacketCipher).
fn bench_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_encryption");

    let key = [0x42u8; 32];
    let pool = PacketPool::new(64, &key, 0x1234);

    // Different payload sizes
    for payload_size in [64, 256, 1024, 4096].iter() {
        let event_data = Bytes::from(vec![0x42u8; *payload_size]);
        let events = vec![event_data];

        group.throughput(Throughput::Bytes(*payload_size as u64));
        group.bench_with_input(
            BenchmarkId::new("encrypt", payload_size),
            &events,
            |b, events| {
                b.iter(|| {
                    let mut builder = pool.get();
                    builder.build(0x5678, 42, events, PacketFlags::NONE)
                });
            },
        );
    }

    // Decomposition: the raw AEAD call alone — crate-level
    // `ChaCha20Poly1305::encrypt_in_place_detached` with a
    // template nonce and a 56-byte AAD (the size
    // `NetHeader::aad()` produces), isolating cipher cost from
    // the builder's framing / header / buffer work in `encrypt/N`
    // above. The gap between `encrypt/N` and `raw_aead/N` is the
    // builder's overhead; the raw fixed cost at small sizes is
    // the crate's per-message setup (one ChaCha block to derive
    // the one-time Poly1305 key, plus poly1305 0.8's per-message
    // AVX2 key-power precomputation — amortized only on multi-KB
    // payloads).
    {
        use chacha20poly1305::{
            aead::{AeadInPlace, KeyInit},
            ChaCha20Poly1305,
        };
        let cipher = ChaCha20Poly1305::new((&key).into());
        let aad = [0x42u8; 56];
        // Prefix-filled template, exactly `PacketCipher`'s shape:
        // the 4-byte session prefix is derived ONCE at construction
        // (any non-zero stand-in works — AEAD timing is independent
        // of nonce VALUES); per call the template is copied and only
        // the counter bytes 4..12 are overwritten, mirroring
        // `nonce_from_counter` instruction-for-instruction.
        let nonce_template: [u8; 12] = [0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0, 0, 0, 0, 0];
        for payload_size in [64usize, 256, 1024, 4096].iter() {
            let mut buf = vec![0x42u8; *payload_size];
            let mut counter = 0u64;
            // Cheap clone (key schedule state) so each size's move
            // closure owns its instance.
            let cipher = cipher.clone();
            group.throughput(Throughput::Bytes(*payload_size as u64));
            group.bench_with_input(
                BenchmarkId::new("raw_aead", payload_size),
                payload_size,
                move |b, _| {
                    b.iter(|| {
                        // Counter bump keeps (key, nonce) pairs
                        // unique across iterations.
                        counter = counter.wrapping_add(1);
                        let mut nonce = nonce_template;
                        nonce[4..12].copy_from_slice(&counter.to_le_bytes());
                        let tag = cipher
                            .encrypt_in_place_detached((&nonce).into(), &aad, &mut buf)
                            .expect("AEAD encrypt cannot fail on valid inputs");
                        std::hint::black_box(tag)
                    });
                },
            );
        }
    }

    // ring's RFC 8439 implementation — the backend `PacketCipher`
    // uses post-spike. `raw_aead` above stays as the RustCrypto
    // reference so the cipher-vs-cipher fixed/marginal profile is
    // visible side by side in every run.
    {
        use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
        let aad = [0x42u8; 56];
        // Same prefix-filled template shape as `raw_aead` above —
        // see the comment there.
        let nonce_template: [u8; 12] = [0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0, 0, 0, 0, 0];
        for payload_size in [64usize, 256, 1024, 4096].iter() {
            let unbound = UnboundKey::new(&CHACHA20_POLY1305, &key).expect("32-byte key");
            let cipher = LessSafeKey::new(unbound);
            let mut buf = vec![0x42u8; *payload_size];
            let mut counter = 0u64;
            group.throughput(Throughput::Bytes(*payload_size as u64));
            group.bench_with_input(
                BenchmarkId::new("raw_ring", payload_size),
                payload_size,
                move |b, _| {
                    b.iter(|| {
                        counter = counter.wrapping_add(1);
                        let mut nonce = nonce_template;
                        nonce[4..12].copy_from_slice(&counter.to_le_bytes());
                        let tag = cipher
                            .seal_in_place_separate_tag(
                                Nonce::assume_unique_for_key(nonce),
                                Aad::from(&aad),
                                &mut buf,
                            )
                            .expect("seal cannot fail on valid inputs");
                        std::hint::black_box(tag)
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark keypair generation.
fn bench_keypair(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_keypair");
    group.throughput(Throughput::Elements(1));

    group.bench_function("generate", |b| {
        b.iter(StaticKeypair::generate);
    });

    group.finish();
}

/// Benchmark AAD (Additional Authenticated Data) generation.
fn bench_aad(c: &mut Criterion) {
    let mut group = c.benchmark_group("net_aad");
    group.throughput(Throughput::Elements(1));

    let nonce = [0x42u8; NONCE_SIZE];
    let header = NetHeader::new(0x1234, 0x5678, 42, nonce, 1000, 10, PacketFlags::RELIABLE);

    group.bench_function("generate", |b| {
        b.iter(|| header.aad());
    });

    group.finish();
}

/// Benchmark: Legacy PacketPool vs ThreadLocalPool
fn bench_pool_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_comparison");
    group.throughput(Throughput::Elements(1));

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;

    // Legacy PacketPool
    let shared_pool = PacketPool::new(64, &key, 0x1234);
    group.bench_function("shared_pool_get_return", |b| {
        b.iter(|| {
            let builder = shared_pool.get();
            drop(builder);
        });
    });

    // ThreadLocalPool (should be faster due to thread-local caching)
    let fast_pool = ThreadLocalPool::new(64, &key, session_id);
    group.bench_function("thread_local_pool_get_return", |b| {
        b.iter(|| {
            let builder = fast_pool.get();
            drop(builder);
        });
    });

    // Measure with contention simulation (multiple acquire/release)
    group.bench_function("shared_pool_10x", |b| {
        b.iter(|| {
            for _ in 0..10 {
                let builder = shared_pool.get();
                drop(builder);
            }
        });
    });

    group.bench_function("thread_local_pool_10x", |b| {
        b.iter(|| {
            for _ in 0..10 {
                let builder = fast_pool.get();
                drop(builder);
            }
        });
    });

    group.finish();
}

/// Benchmark: Legacy encryption vs Fast encryption (counter-based nonces)
fn bench_cipher_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("cipher_comparison");

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;

    // Test different payload sizes
    for payload_size in [64, 256, 1024, 4096].iter() {
        let event_data = Bytes::from(vec![0x42u8; *payload_size]);
        let events = vec![event_data];

        group.throughput(Throughput::Bytes(*payload_size as u64));

        // Legacy: XChaCha20-Poly1305 with random nonces
        let shared_pool = PacketPool::new(64, &key, 0x1234);
        group.bench_with_input(
            BenchmarkId::new("shared_pool", payload_size),
            &events,
            |b, events| {
                b.iter(|| {
                    let mut builder = shared_pool.get();
                    builder.build(0x5678, 42, events, PacketFlags::NONE)
                });
            },
        );

        // Fast: ChaCha20-Poly1305 with counter-based nonces
        let fast_pool = ThreadLocalPool::new(64, &key, session_id);
        group.bench_with_input(
            BenchmarkId::new("fast_chacha20", payload_size),
            &events,
            |b, events| {
                b.iter(|| {
                    let mut builder = fast_pool.get();
                    builder.build(0x5678, 42, events, PacketFlags::NONE)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Adaptive batcher overhead
fn bench_adaptive_batcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("adaptive_batcher");
    group.throughput(Throughput::Elements(1));

    let batcher = AdaptiveBatcher::new();

    // Measure optimal_size() call overhead
    group.bench_function("optimal_size", |b| {
        b.iter(|| batcher.optimal_size());
    });

    // Measure record() call overhead
    group.bench_function("record", |b| {
        b.iter(|| batcher.record(1000, 50, 20));
    });

    // Measure full cycle (get size, simulate work, record)
    group.bench_function("full_cycle", |b| {
        b.iter(|| {
            let _size = batcher.optimal_size();
            // Simulate some work
            std::hint::black_box(42);
            batcher.record(1000, 50, 20);
        });
    });

    group.finish();
}

/// Benchmark: End-to-end packet building comparison
fn bench_e2e_packet_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_packet_build");

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;

    // Simulate realistic batch: 50 events of 64 bytes each
    let event_data = Bytes::from(vec![0x42u8; 64]);
    let events: Vec<Bytes> = (0..50).map(|_| event_data.clone()).collect();
    let total_bytes = 64 * 50;

    group.throughput(Throughput::Bytes(total_bytes as u64));

    // Legacy path
    let shared_pool = PacketPool::new(64, &key, 0x1234);
    group.bench_function("shared_pool_50_events", |b| {
        b.iter(|| {
            let mut builder = shared_pool.get();
            builder.build(0x5678, 42, &events, PacketFlags::NONE)
        });
    });

    // Fast path (thread-local + counter nonces)
    let fast_pool = ThreadLocalPool::new(64, &key, session_id);
    group.bench_function("fast_50_events", |b| {
        b.iter(|| {
            let mut builder = fast_pool.get();
            builder.build(0x5678, 42, &events, PacketFlags::NONE)
        });
    });

    group.finish();
}

// =============================================================================
// Phase 2: Concurrency Reality Testing
// =============================================================================

/// Benchmark: Multi-threaded packet building with thread counts 8, 16, 24, 32
fn bench_multithread_packet_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("multithread_packet_build");

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;
    let iterations_per_thread = 1000;

    for thread_count in [8, 16, 24, 32].iter() {
        let total_packets = iterations_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_packets as u64));

        // Legacy PacketPool (shared, has contention)
        let shared_pool = Arc::new(PacketPool::new(256, &key, 0x1234));
        group.bench_with_input(
            BenchmarkId::new("shared_pool", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let pool = Arc::clone(&shared_pool);
                            thread::spawn(move || {
                                let event_data = Bytes::from(vec![0x42u8; 64]);
                                let events = vec![event_data];
                                for seq in 0..iterations_per_thread {
                                    let mut builder = pool.get();
                                    let _packet = builder.build(
                                        0x5678,
                                        seq as u64,
                                        &events,
                                        PacketFlags::NONE,
                                    );
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // ThreadLocalPool (thread-local caching, less contention)
        let fast_pool = Arc::new(ThreadLocalPool::new(256, &key, session_id));
        group.bench_with_input(
            BenchmarkId::new("thread_local_pool", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let pool = Arc::clone(&fast_pool);
                            thread::spawn(move || {
                                let event_data = Bytes::from(vec![0x42u8; 64]);
                                let events = vec![event_data];
                                for seq in 0..iterations_per_thread {
                                    let mut builder = pool.get();
                                    let _packet = builder.build(
                                        0x5678,
                                        seq as u64,
                                        &events,
                                        PacketFlags::NONE,
                                    );
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Mixed frame sizes under concurrency
/// Simulates realistic traffic with small (64B), medium (256B), and large (1KB) frames
fn bench_multithread_mixed_frames(c: &mut Criterion) {
    let mut group = c.benchmark_group("multithread_mixed_frames");

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;
    let iterations_per_thread = 500;

    // Pre-create frame data
    let small_frame = Bytes::from(vec![0x42u8; 64]);
    let medium_frame = Bytes::from(vec![0x42u8; 256]);
    let large_frame = Bytes::from(vec![0x42u8; 1024]);

    for thread_count in [8, 16, 24, 32].iter() {
        let total_packets = iterations_per_thread * *thread_count * 3; // 3 frame sizes
        group.throughput(Throughput::Elements(total_packets as u64));

        // Legacy pool with mixed frames
        let shared_pool = Arc::new(PacketPool::new(256, &key, 0x1234));
        let small = small_frame.clone();
        let medium = medium_frame.clone();
        let large = large_frame.clone();

        group.bench_with_input(
            BenchmarkId::new("shared_mixed", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let pool = Arc::clone(&shared_pool);
                            let s = small.clone();
                            let m = medium.clone();
                            let l = large.clone();
                            thread::spawn(move || {
                                for i in 0..iterations_per_thread {
                                    // Rotate through frame sizes
                                    let events = match i % 3 {
                                        0 => vec![s.clone()],
                                        1 => vec![m.clone()],
                                        _ => vec![l.clone()],
                                    };
                                    let mut builder = pool.get();
                                    let _packet = builder.build(
                                        thread_id as u64,
                                        i as u64,
                                        &events,
                                        PacketFlags::NONE,
                                    );
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Fast pool with mixed frames
        let fast_pool = Arc::new(ThreadLocalPool::new(256, &key, session_id));

        group.bench_with_input(
            BenchmarkId::new("fast_mixed", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let pool = Arc::clone(&fast_pool);
                            let s = small.clone();
                            let m = medium.clone();
                            let l = large.clone();
                            thread::spawn(move || {
                                for i in 0..iterations_per_thread {
                                    let events = match i % 3 {
                                        0 => vec![s.clone()],
                                        1 => vec![m.clone()],
                                        _ => vec![l.clone()],
                                    };
                                    let mut builder = pool.get();
                                    let _packet = builder.build(
                                        thread_id as u64,
                                        i as u64,
                                        &events,
                                        PacketFlags::NONE,
                                    );
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Pool contention stress test
/// Measures how pools behave under heavy acquire/release cycles
fn bench_pool_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_contention");

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;

    // Stress test: rapid acquire/release without building packets
    for thread_count in [8, 16, 24, 32].iter() {
        let cycles_per_thread = 10_000;
        let total_cycles = cycles_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_cycles as u64));

        // Legacy pool contention
        let shared_pool = Arc::new(PacketPool::new(64, &key, 0x1234));
        group.bench_with_input(
            BenchmarkId::new("shared_acquire_release", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let pool = Arc::clone(&shared_pool);
                            thread::spawn(move || {
                                for _ in 0..cycles_per_thread {
                                    let builder = pool.get();
                                    std::hint::black_box(&builder);
                                    drop(builder);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Fast pool contention (should show thread-local benefit)
        let fast_pool = Arc::new(ThreadLocalPool::new(64, &key, session_id));
        group.bench_with_input(
            BenchmarkId::new("fast_acquire_release", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let pool = Arc::clone(&fast_pool);
                            thread::spawn(move || {
                                for _ in 0..cycles_per_thread {
                                    let builder = pool.get();
                                    std::hint::black_box(&builder);
                                    drop(builder);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Sustained throughput under thread count scaling
/// Measures packets/second as we scale threads
fn bench_throughput_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput_scaling");
    group.sample_size(20); // Reduce samples for long-running tests

    let key = [0x42u8; 32];
    let session_id = 0x1234567890ABCDEF_u64;

    // Fixed work per benchmark iteration
    let packets_per_thread = 2000;
    let event_data = Bytes::from(vec![0x42u8; 128]); // Medium-sized events

    for thread_count in [1, 2, 4, 8, 16, 24, 32].iter() {
        let total_packets = packets_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_packets as u64));

        let fast_pool = Arc::new(ThreadLocalPool::new(256, &key, session_id));
        let events_template = vec![event_data.clone(); 10]; // Batch of 10

        group.bench_with_input(
            BenchmarkId::new("fast_pool_scaling", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let pool = Arc::clone(&fast_pool);
                            let events = events_template.clone();
                            thread::spawn(move || {
                                for seq in 0..packets_per_thread {
                                    let mut builder = pool.get();
                                    let _packet = builder.build(
                                        0x5678,
                                        seq as u64,
                                        &events,
                                        PacketFlags::NONE,
                                    );
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
// Phase 3A: Router and Fair Scheduler Benchmarks
// =============================================================================

/// Benchmark: RoutingHeader serialization/deserialization
fn bench_routing_header(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing_header");
    group.throughput(Throughput::Elements(1));

    let header = RoutingHeader::new(0x123456789ABCDEF0, 0xDEADBEEF, 8);

    group.bench_function("serialize", |b| {
        b.iter(|| header.to_bytes());
    });

    let header_bytes = header.to_bytes();
    group.bench_function("deserialize", |b| {
        b.iter(|| RoutingHeader::from_bytes(&header_bytes));
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let bytes = header.to_bytes();
            RoutingHeader::from_bytes(&bytes)
        });
    });

    // Benchmark forwarding (TTL decrement + hop count increment)
    group.bench_function("forward", |b| {
        b.iter(|| {
            let mut h = RoutingHeader::new(0x1234, 0x5678, 8);
            h.forward();
            h
        });
    });

    group.finish();
}

/// Benchmark: RoutingTable operations
fn bench_routing_table(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing_table");
    group.throughput(Throughput::Elements(1));

    let table = RoutingTable::new(0x1234);

    // Pre-populate with routes
    for i in 0..1000 {
        let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        table.add_route(i as u64, addr);
    }

    group.bench_function("lookup_hit", |b| {
        b.iter(|| table.lookup(500));
    });

    group.bench_function("lookup_miss", |b| {
        b.iter(|| table.lookup(99999));
    });

    group.bench_function("is_local", |b| {
        b.iter(|| table.is_local(0x1234));
    });

    // Steady-state overwrite of an existing key.
    //
    // The previous form incremented `i` and inserted a brand-new key on
    // every iteration. Over a Criterion window that is millions of fresh
    // inserts into the shared DashMap, so the table grew unbounded and
    // periodically resized/rehashed — which is what produced the ~416ns
    // mean and the "huge variance" (resize amortization), not the cost
    // of add_route itself. RoutingTable is the same type benched in
    // mesh.rs as `mesh_routing/add_route` (a fixed-key overwrite, ~45ns);
    // there is no second routing-table design. Overwrite a fixed,
    // pre-populated key here so the two benches measure the same
    // operation and the result reflects in-place update, not map growth.
    let addr: std::net::SocketAddr = "127.0.0.1:8000".parse().unwrap();
    group.bench_function("add_route", |b| {
        b.iter(|| table.add_route(500, addr));
    });

    // Stream stats recording
    group.bench_function("record_in", |b| {
        b.iter(|| table.record_in(42, 1024));
    });

    group.bench_function("record_out", |b| {
        b.iter(|| table.record_out(42, 1024));
    });

    group.bench_function("aggregate_stats", |b| {
        // Add some stream activity first
        for i in 0..100 {
            table.record_in(i, 100);
            table.record_out(i, 100);
        }
        b.iter(|| table.aggregate_stats());
    });

    group.finish();
}

/// Benchmark: FairScheduler operations
fn bench_fair_scheduler(c: &mut Criterion) {
    let mut group = c.benchmark_group("fair_scheduler");
    group.throughput(Throughput::Elements(1));

    // Note: FairScheduler's enqueue/dequeue use private QueuedPacket type
    // We benchmark the creation and basic operations

    let scheduler = FairScheduler::new(16, 1024);

    group.bench_function("creation", |b| {
        b.iter(|| FairScheduler::new(16, 1024));
    });

    group.bench_function("stream_count_empty", |b| {
        b.iter(|| scheduler.stream_count());
    });

    group.bench_function("total_queued", |b| {
        b.iter(|| scheduler.total_queued());
    });

    group.bench_function("cleanup_empty", |b| {
        b.iter(|| scheduler.cleanup_empty());
    });

    group.finish();
}

/// Benchmark: Routing table with concurrent access
fn bench_routing_table_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing_table_concurrent");

    let iterations_per_thread = 1000;

    for thread_count in [4, 8, 16].iter() {
        let total_ops = iterations_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_ops as u64));

        // Concurrent lookups
        let table = Arc::new(RoutingTable::new(0x1234));
        for i in 0..1000 {
            let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
            table.add_route(i as u64, addr);
        }

        group.bench_with_input(
            BenchmarkId::new("concurrent_lookup", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let t = Arc::clone(&table);
                            thread::spawn(move || {
                                for i in 0..iterations_per_thread {
                                    let _ = t.lookup((i % 1000) as u64);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Concurrent record_in/record_out (stream stats)
        group.bench_with_input(
            BenchmarkId::new("concurrent_stats", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let t = Arc::clone(&table);
                            thread::spawn(move || {
                                for i in 0..iterations_per_thread {
                                    let stream_id = (thread_id * 100 + i % 100) as u64;
                                    t.record_in(stream_id, 128);
                                    t.record_out(stream_id, 128);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Full routing decision path (header parse -> lookup -> forward)
fn bench_routing_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing_decision");
    group.throughput(Throughput::Elements(1));

    let table = RoutingTable::new(0x1234);

    // Add some routes
    for i in 0..100 {
        let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        table.add_route(0x5000 + i as u64, addr);
    }

    // Create a sample packet with routing header
    let header = RoutingHeader::new(0x5042, 0xABCD, 8);
    let header_bytes = header.to_bytes();

    group.bench_function("parse_lookup_forward", |b| {
        b.iter(|| {
            // Parse header
            let mut h = RoutingHeader::from_bytes(&header_bytes).unwrap();

            // Check if local
            if table.is_local(h.dest_id) {
                return Some(h);
            }

            // Lookup next hop
            let _next_hop = table.lookup(h.dest_id)?;

            // Forward (decrement TTL)
            h.forward();

            Some(h)
        });
    });

    // Benchmark with stream stats recording
    group.bench_function("full_with_stats", |b| {
        b.iter(|| {
            let mut h = RoutingHeader::from_bytes(&header_bytes).unwrap();

            // Record incoming
            table.record_in(h.src_id as u64, ROUTING_HEADER_SIZE as u64 + 1024);

            if table.is_local(h.dest_id) {
                return Some(h);
            }

            let _next_hop = table.lookup(h.dest_id)?;
            h.forward();

            // Record outgoing
            table.record_out(h.dest_id, ROUTING_HEADER_SIZE as u64 + 1024);

            Some(h)
        });
    });

    group.finish();
}

/// Benchmark: Stream multiplexing overhead (many streams)
fn bench_stream_multiplexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream_multiplexing");

    for stream_count in [10, 100, 1000, 10000].iter() {
        let table = RoutingTable::new(0x1234);

        // Add routes for all streams
        for i in 0..*stream_count {
            let addr: std::net::SocketAddr =
                format!("127.0.0.1:{}", 9000 + (i % 1000)).parse().unwrap();
            table.add_route(i as u64, addr);
        }

        group.throughput(Throughput::Elements(*stream_count as u64));
        group.bench_with_input(
            BenchmarkId::new("lookup_all", stream_count),
            stream_count,
            |b, &count| {
                b.iter(|| {
                    for i in 0..count {
                        let _ = table.lookup(i as u64);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("stats_all", stream_count),
            stream_count,
            |b, &count| {
                b.iter(|| {
                    for i in 0..count {
                        table.record_in(i as u64, 64);
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_header,
    bench_event_frame,
    bench_packet_pool,
    bench_packet_build,
    bench_encryption,
    bench_keypair,
    bench_aad,
    bench_pool_comparison,
    bench_cipher_comparison,
    bench_adaptive_batcher,
    bench_e2e_packet_build,
);

// Phase 2: Concurrency benchmarks (run separately due to longer duration)
criterion_group!(
    concurrency_benches,
    bench_multithread_packet_build,
    bench_multithread_mixed_frames,
    bench_pool_contention,
    bench_throughput_scaling,
);

// Phase 3A: Router benchmarks
criterion_group!(
    router_benches,
    bench_routing_header,
    bench_routing_table,
    bench_fair_scheduler,
    bench_routing_table_concurrent,
    bench_routing_decision,
    bench_stream_multiplexing,
);

// =============================================================================
// Phase 3B: Multi-Hop Proxy Benchmarks
// =============================================================================

/// Benchmark: MultiHopPacketBuilder
fn bench_multihop_packet_builder(c: &mut Criterion) {
    let mut group = c.benchmark_group("multihop_packet_builder");

    let builder = MultiHopPacketBuilder::new(0xABCD);

    // Different payload sizes
    for payload_size in [64, 256, 1024, 4096].iter() {
        let payload = vec![0x42u8; *payload_size];

        group.throughput(Throughput::Bytes(*payload_size as u64));
        group.bench_with_input(
            BenchmarkId::new("build", payload_size),
            &payload,
            |b, payload| {
                b.iter(|| builder.build(0x1234, 8, payload));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("build_priority", payload_size),
            &payload,
            |b, payload| {
                b.iter(|| builder.build_priority(0x1234, 8, payload));
            },
        );
    }

    group.finish();
}

/// Benchmark: Simulated multi-hop forwarding chain
fn bench_multihop_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("multihop_chain");

    let builder = MultiHopPacketBuilder::new(0xABCD);

    // Simulate forwarding through N hops (header parse + update)
    for hop_count in [1, 2, 3, 4, 5].iter() {
        let payload = vec![0x42u8; 256];
        let initial_ttl = *hop_count as u8 + 1;
        let packet = builder.build(0x1234, initial_ttl, &payload);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("forward_chain", hop_count),
            hop_count,
            |b, &hops| {
                b.iter(|| {
                    let mut data = packet.clone();
                    for _ in 0..hops {
                        // Parse header
                        let mut header =
                            RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE]).unwrap();
                        // Forward (decrement TTL)
                        header.forward();
                        // Rebuild packet with new header
                        let mut new_data = bytes::BytesMut::with_capacity(data.len());
                        header.write_to(&mut new_data);
                        new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);
                        data = new_data.freeze();
                    }
                    data
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Per-hop latency overhead
fn bench_hop_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("hop_latency");
    group.throughput(Throughput::Elements(1));

    let builder = MultiHopPacketBuilder::new(0xABCD);

    // Single hop processing (the core forwarding operation)
    let packet = builder.build(0x5678, 8, &[0x42u8; 256]);

    group.bench_function("single_hop_process", |b| {
        b.iter(|| {
            // Parse header
            let mut header = RoutingHeader::from_bytes(&packet[..ROUTING_HEADER_SIZE]).unwrap();
            // Forward
            header.forward();
            // Serialize updated header
            header.to_bytes()
        });
    });

    // Full hop with packet reconstruction
    group.bench_function("single_hop_full", |b| {
        b.iter(|| {
            let mut header = RoutingHeader::from_bytes(&packet[..ROUTING_HEADER_SIZE]).unwrap();
            header.forward();
            let mut new_data = bytes::BytesMut::with_capacity(packet.len());
            header.write_to(&mut new_data);
            new_data.extend_from_slice(&packet[ROUTING_HEADER_SIZE..]);
            new_data.freeze()
        });
    });

    group.finish();
}

/// Benchmark: Hop latency scaling (compare 1 hop vs N hops)
fn bench_hop_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("hop_scaling");

    let builder = MultiHopPacketBuilder::new(0xABCD);

    // Test different payload sizes across hop counts
    for payload_size in [64, 256, 1024].iter() {
        let payload = vec![0x42u8; *payload_size];

        for hop_count in [1, 2, 3, 4, 5].iter() {
            let packet = builder.build(0x1234, *hop_count as u8 + 1, &payload);
            let label = format!("{}B_{}hops", payload_size, hop_count);

            group.throughput(Throughput::Bytes(*payload_size as u64));
            group.bench_function(&label, |b| {
                b.iter(|| {
                    let mut data = packet.clone();
                    for _ in 0..*hop_count {
                        let mut header =
                            RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE]).unwrap();
                        header.forward();
                        let mut new_data = bytes::BytesMut::with_capacity(data.len());
                        header.write_to(&mut new_data);
                        new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);
                        data = new_data.freeze();
                    }
                    data
                });
            });
        }
    }

    group.finish();
}

/// Benchmark: Multi-hop forwarding with routing table lookups
fn bench_multihop_with_routing(c: &mut Criterion) {
    let mut group = c.benchmark_group("multihop_with_routing");
    group.throughput(Throughput::Elements(1));

    let builder = MultiHopPacketBuilder::new(0xABCD);

    // Create routing tables for each "hop"
    let tables: Vec<RoutingTable> = (0..5)
        .map(|i| {
            let table = RoutingTable::new(0x1000 + i);
            // Add route to next hop
            let next_addr: std::net::SocketAddr =
                format!("127.0.0.1:{}", 9000 + i + 1).parse().unwrap();
            table.add_route(0x9999, next_addr); // Final destination
            table
        })
        .collect();

    let packet = builder.build(0x9999, 6, &[0x42u8; 256]);

    for hop_count in [1, 2, 3, 4, 5].iter() {
        group.bench_with_input(
            BenchmarkId::new("route_and_forward", hop_count),
            hop_count,
            |b, &hops| {
                b.iter(|| {
                    let mut data = packet.clone();
                    for i in 0..hops {
                        let mut header =
                            RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE]).unwrap();

                        // Lookup (as proxy would do)
                        let _next_hop = tables[i as usize].lookup(header.dest_id);

                        // Record stats
                        tables[i as usize].record_in(header.src_id as u64, data.len() as u64);

                        // Forward
                        header.forward();

                        // Rebuild
                        let mut new_data = bytes::BytesMut::with_capacity(data.len());
                        header.write_to(&mut new_data);
                        new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);

                        tables[i as usize].record_out(header.dest_id, new_data.len() as u64);

                        data = new_data.freeze();
                    }
                    data
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Concurrent multi-hop forwarding
fn bench_multihop_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("multihop_concurrent");
    group.sample_size(20);

    let packets_per_thread = 1000;
    let payload = vec![0x42u8; 256];

    for thread_count in [4, 8, 16].iter() {
        let total_packets = packets_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_packets as u64));

        // Shared routing table
        let table = Arc::new(RoutingTable::new(0x1234));
        for i in 0..100 {
            let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
            table.add_route(0x5000 + i as u64, addr);
        }

        group.bench_with_input(
            BenchmarkId::new("concurrent_forward", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let t = Arc::clone(&table);
                            let p = payload.clone();
                            thread::spawn(move || {
                                let builder = MultiHopPacketBuilder::new(thread_id as u32);
                                for i in 0..packets_per_thread {
                                    let dest_id = 0x5000 + (i % 100) as u64;
                                    let packet = builder.build(dest_id, 4, &p);

                                    // Simulate 3-hop forwarding
                                    let mut data = packet;
                                    for _ in 0..3 {
                                        let mut header =
                                            RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE])
                                                .unwrap();
                                        let _ = t.lookup(header.dest_id);
                                        t.record_in(header.src_id as u64, data.len() as u64);
                                        header.forward();
                                        let mut new_data =
                                            bytes::BytesMut::with_capacity(data.len());
                                        header.write_to(&mut new_data);
                                        new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);
                                        data = new_data.freeze();
                                    }
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

// Phase 3B: Multi-hop benchmarks
criterion_group!(
    multihop_benches,
    bench_multihop_packet_builder,
    bench_multihop_chain,
    bench_hop_latency,
    bench_hop_scaling,
    bench_multihop_with_routing,
    bench_multihop_concurrent,
);

// =============================================================================
// Phase 3C: Swarm / Pingwave / Graph Benchmarks
// =============================================================================

/// Benchmark: Pingwave serialization/deserialization
fn bench_pingwave(c: &mut Criterion) {
    let mut group = c.benchmark_group("pingwave");
    group.throughput(Throughput::Elements(1));

    let pw = Pingwave::new(0x123456789ABCDEF0, 42, 3);

    group.bench_function("serialize", |b| {
        b.iter(|| pw.to_bytes());
    });

    let bytes = pw.to_bytes();
    group.bench_function("deserialize", |b| {
        b.iter(|| Pingwave::from_bytes(&bytes));
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let bytes = pw.to_bytes();
            Pingwave::from_bytes(&bytes)
        });
    });

    group.bench_function("forward", |b| {
        b.iter(|| {
            let mut p = Pingwave::new(0x1234, 1, 3);
            p.forward();
            p
        });
    });

    group.finish();
}

/// Benchmark: Capabilities serialization
fn bench_capabilities(c: &mut Criterion) {
    let mut group = c.benchmark_group("capabilities");
    group.throughput(Throughput::Elements(1));

    // Simple capabilities
    let simple = Capabilities::new().with_gpu(true).with_memory(16);

    group.bench_function("serialize_simple", |b| {
        b.iter(|| simple.to_bytes());
    });

    let simple_bytes = simple.to_bytes();
    group.bench_function("deserialize_simple", |b| {
        b.iter(|| Capabilities::from_bytes(&simple_bytes));
    });

    // Complex capabilities with tools and tags
    let complex = Capabilities::new()
        .with_gpu(true)
        .with_memory(32)
        .with_model_slots(8)
        .with_tool("python")
        .with_tool("rust")
        .with_tool("javascript")
        .with_tag("inference")
        .with_tag("training")
        .with_tag("gpu-cluster");

    group.bench_function("serialize_complex", |b| {
        b.iter(|| complex.to_bytes());
    });

    let complex_bytes = complex.to_bytes();
    group.bench_function("deserialize_complex", |b| {
        b.iter(|| Capabilities::from_bytes(&complex_bytes));
    });

    group.finish();
}

/// Benchmark: LocalGraph operations
fn bench_local_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_graph");
    group.throughput(Throughput::Elements(1));

    let graph = LocalGraph::new(0x1111, 3);

    // Pre-populate with nodes
    for i in 0..1000 {
        let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        let pw = Pingwave::new(0x2000 + i as u64, i as u64, 3);
        graph.on_pingwave(pw, addr);
    }

    group.bench_function("create_pingwave", |b| {
        b.iter(|| graph.create_pingwave());
    });

    // Process new pingwave
    group.bench_function("on_pingwave_new", |b| {
        let mut seq = 10000u64;
        b.iter(|| {
            let pw = Pingwave::new(0x9999, seq, 3);
            seq += 1;
            let addr: std::net::SocketAddr = "127.0.0.1:8000".parse().unwrap();
            graph.on_pingwave(pw, addr)
        });
    });

    // Process duplicate pingwave (should be fast - just cache lookup)
    let dup_pw = Pingwave::new(0x2500, 500, 3);
    group.bench_function("on_pingwave_duplicate", |b| {
        let addr: std::net::SocketAddr = "127.0.0.1:8000".parse().unwrap();
        b.iter(|| graph.on_pingwave(dup_pw, addr));
    });

    group.bench_function("get_node", |b| {
        b.iter(|| graph.get_node(0x2500));
    });

    group.bench_function("node_count", |b| {
        b.iter(|| graph.node_count());
    });

    group.bench_function("stats", |b| {
        b.iter(|| graph.stats());
    });

    group.finish();
}

/// Benchmark: LocalGraph with many nodes (scalability test)
fn bench_graph_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_scaling");

    for node_count in [100, 500, 1000, 5000].iter() {
        let graph = LocalGraph::new(0x1111, 3);

        // Pre-populate
        for i in 0..*node_count {
            let addr: std::net::SocketAddr =
                format!("127.0.0.1:{}", 9000 + (i % 1000)).parse().unwrap();
            let pw = Pingwave::new(0x2000 + i as u64, 1, 3);
            graph.on_pingwave(pw, addr);
        }

        group.throughput(Throughput::Elements(*node_count as u64));

        group.bench_with_input(
            BenchmarkId::new("all_nodes", node_count),
            node_count,
            |b, _| {
                b.iter(|| graph.all_nodes());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("nodes_within_hops", node_count),
            node_count,
            |b, _| {
                b.iter(|| graph.nodes_within_hops(2));
            },
        );
    }

    group.finish();
}

/// Benchmark: Capability-based search
fn bench_capability_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_search");

    let graph = LocalGraph::new(0x1111, 3);

    // Add 1000 nodes with varying capabilities
    for i in 0..1000 {
        let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        let pw = Pingwave::new(0x2000 + i as u64, 1, 3);
        graph.on_pingwave(pw, addr);

        // 20% have GPU
        // 30% have python
        // 50% have rust
        let mut caps = Capabilities::new()
            .with_memory((i % 64 + 1) as u32 * 1024)
            .with_model_slots((i % 8) as u8);

        if i % 5 == 0 {
            caps = caps.with_gpu(true);
        }
        if i % 3 == 0 {
            caps = caps.with_tool("python");
        }
        if i % 2 == 0 {
            caps = caps.with_tool("rust");
        }

        let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        graph.on_capability(CapabilityAd::new(0x2000 + i as u64, 1, caps), addr);
    }

    group.throughput(Throughput::Elements(1));

    group.bench_function("find_with_gpu", |b| {
        b.iter(|| graph.find_with_gpu());
    });

    group.bench_function("find_by_tool_python", |b| {
        b.iter(|| graph.find_by_tool("python"));
    });

    group.bench_function("find_by_tool_rust", |b| {
        b.iter(|| graph.find_by_tool("rust"));
    });

    group.finish();
}

/// Benchmark: Concurrent graph operations
fn bench_graph_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_concurrent");
    group.sample_size(20);

    let pingwaves_per_thread = 500;

    for thread_count in [4, 8, 16].iter() {
        let total_ops = pingwaves_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_ops as u64));

        let graph = Arc::new(LocalGraph::new(0x1111, 3));

        group.bench_with_input(
            BenchmarkId::new("concurrent_pingwave", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let g = Arc::clone(&graph);
                            thread::spawn(move || {
                                for i in 0..pingwaves_per_thread {
                                    let node_id = (thread_id as u64 * 10000) + i as u64;
                                    let pw = Pingwave::new(node_id, i as u64, 3);
                                    let addr: std::net::SocketAddr =
                                        format!("127.0.0.1:{}", 9000 + (i % 1000)).parse().unwrap();
                                    g.on_pingwave(pw, addr);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Path finding in graph
fn bench_path_finding(c: &mut Criterion) {
    let mut group = c.benchmark_group("path_finding");
    group.throughput(Throughput::Elements(1));

    let graph = LocalGraph::new(0x1111, 5);

    // Create a chain: 1111 -> 2222 -> 3333 -> 4444 -> 5555
    graph.add_edge(0x1111, 0x2222, 100);
    graph.add_edge(0x2222, 0x3333, 100);
    graph.add_edge(0x3333, 0x4444, 100);
    graph.add_edge(0x4444, 0x5555, 100);

    group.bench_function("path_1_hop", |b| {
        b.iter(|| graph.path_to(0x2222));
    });

    group.bench_function("path_2_hops", |b| {
        b.iter(|| graph.path_to(0x3333));
    });

    group.bench_function("path_4_hops", |b| {
        b.iter(|| graph.path_to(0x5555));
    });

    group.bench_function("path_not_found", |b| {
        b.iter(|| graph.path_to(0x9999));
    });

    // Create a more complex graph for scaling test
    let complex_graph = LocalGraph::new(0x0001, 10);
    // Create a grid-like structure
    for i in 0..100 {
        for j in 0..10 {
            let from = (i * 10 + j) as u64;
            if j < 9 {
                complex_graph.add_edge(from, from + 1, 100);
            }
            if i < 99 {
                complex_graph.add_edge(from, from + 10, 100);
            }
        }
    }

    group.bench_function("path_complex_graph", |b| {
        b.iter(|| complex_graph.path_to(999)); // Far corner
    });

    group.finish();
}

// Phase 3C: Swarm benchmarks
criterion_group!(
    swarm_benches,
    bench_pingwave,
    bench_capabilities,
    bench_local_graph,
    bench_graph_scaling,
    bench_capability_search,
    bench_graph_concurrent,
    bench_path_finding,
);

// =============================================================================
// Phase 3D: Failure Detection & Recovery Benchmarks
// =============================================================================

/// Benchmark: FailureDetector operations
fn bench_failure_detector(c: &mut Criterion) {
    let mut group = c.benchmark_group("failure_detector");
    group.throughput(Throughput::Elements(1));

    let detector = FailureDetector::new();

    // Pre-populate with nodes
    for i in 0..1000 {
        let addr: std::net::SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        detector.heartbeat(i as u64, addr);
    }

    group.bench_function("heartbeat_existing", |b| {
        let addr: std::net::SocketAddr = "127.0.0.1:9500".parse().unwrap();
        b.iter(|| detector.heartbeat(500, addr));
    });

    // Dedicated detector: heartbeat_new inserts a fresh id every iteration,
    // which over a Criterion measurement window balloons the map to millions
    // of entries. Sharing the main `detector` would pollute the steady-state
    // 1000-node fixture that check_all/stats measure against (the source of
    // the old multi-hundred-ms check_all artifact). See
    // docs/misc/PERF_AUDIT_2026_06_08_BENCHMARK_WINS.md §7.
    let growth_detector = FailureDetector::new();
    group.bench_function("heartbeat_new", |b| {
        let mut id = 10000u64;
        let addr: std::net::SocketAddr = "127.0.0.1:8000".parse().unwrap();
        b.iter(|| {
            growth_detector.heartbeat(id, addr);
            id += 1;
        });
    });

    group.bench_function("status_check", |b| {
        b.iter(|| detector.status(500));
    });

    group.bench_function("check_all", |b| {
        b.iter(|| detector.check_all());
    });

    group.bench_function("stats", |b| {
        b.iter(|| detector.stats());
    });

    group.finish();
}

/// Benchmark: LossSimulator
fn bench_loss_simulator(c: &mut Criterion) {
    let mut group = c.benchmark_group("loss_simulator");
    group.throughput(Throughput::Elements(1));

    // Different loss rates
    for loss_rate in [0.01, 0.05, 0.10, 0.20].iter() {
        let sim = LossSimulator::new(*loss_rate);
        let label = format!("should_drop_{}pct", (*loss_rate * 100.0) as u32);

        group.bench_function(&label, |b| {
            b.iter(|| sim.should_drop());
        });
    }

    // With burst losses
    let burst_sim = LossSimulator::new(0.05).with_bursts(0.02, 10);
    group.bench_function("should_drop_burst", |b| {
        b.iter(|| burst_sim.should_drop());
    });

    group.finish();
}

/// Benchmark: CircuitBreaker
fn bench_circuit_breaker(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_breaker");
    group.throughput(Throughput::Elements(1));

    let cb = CircuitBreaker::new(5, 3, std::time::Duration::from_secs(30));

    group.bench_function("allow_closed", |b| {
        b.iter(|| cb.allow());
    });

    group.bench_function("record_success", |b| {
        b.iter(|| cb.record_success());
    });

    group.bench_function("record_failure", |b| {
        // Reset before each iteration to stay closed
        cb.reset();
        b.iter(|| cb.record_failure());
    });

    group.bench_function("state", |b| {
        b.iter(|| cb.state());
    });

    group.finish();
}

/// Benchmark: RecoveryManager
fn bench_recovery_manager(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_manager");
    group.throughput(Throughput::Elements(1));

    let mgr = RecoveryManager::new();

    // Pre-populate some failed nodes
    for i in 0..100 {
        mgr.on_failure(i as u64, vec![1000 + i as u64, 2000 + i as u64]);
    }

    group.bench_function("on_failure_with_alternates", |b| {
        let mut id = 10000u64;
        b.iter(|| {
            mgr.on_failure(id, vec![id + 1, id + 2]);
            id += 1;
        });
    });

    group.bench_function("on_failure_no_alternates", |b| {
        let mut id = 20000u64;
        b.iter(|| {
            mgr.on_failure(id, vec![]);
            id += 1;
        });
    });

    group.bench_function("get_action", |b| {
        b.iter(|| mgr.get_action(50, 5));
    });

    group.bench_function("is_failed", |b| {
        b.iter(|| mgr.is_failed(50));
    });

    group.bench_function("on_recovery", |b| {
        let mut id = 0u64;
        b.iter(|| {
            // Re-fail the node first
            mgr.on_failure(id % 100, vec![]);
            mgr.on_recovery(id % 100);
            id += 1;
        });
    });

    group.bench_function("stats", |b| {
        b.iter(|| mgr.stats());
    });

    group.finish();
}

/// Benchmark: Failure detection scaling
fn bench_failure_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("failure_scaling");

    for node_count in [100, 500, 1000, 5000].iter() {
        let detector = FailureDetector::with_config(FailureDetectorConfig {
            timeout: std::time::Duration::from_secs(5),
            miss_threshold: 3,
            suspicion_threshold: 2,
            cleanup_interval: std::time::Duration::from_secs(60),
        });

        // Pre-populate
        for i in 0..*node_count {
            let addr: std::net::SocketAddr =
                format!("127.0.0.1:{}", 9000 + (i % 1000)).parse().unwrap();
            detector.heartbeat(i as u64, addr);
        }

        group.throughput(Throughput::Elements(*node_count as u64));

        group.bench_with_input(
            BenchmarkId::new("check_all", node_count),
            node_count,
            |b, _| {
                b.iter(|| detector.check_all());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("healthy_nodes", node_count),
            node_count,
            |b, _| {
                b.iter(|| detector.healthy_nodes());
            },
        );
    }

    group.finish();
}

/// Benchmark: Concurrent failure detection
fn bench_failure_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("failure_concurrent");
    group.sample_size(20);

    let heartbeats_per_thread = 500;

    for thread_count in [4, 8, 16].iter() {
        let total_ops = heartbeats_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_ops as u64));

        let detector = Arc::new(FailureDetector::new());

        group.bench_with_input(
            BenchmarkId::new("concurrent_heartbeat", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let d = Arc::clone(&detector);
                            thread::spawn(move || {
                                for i in 0..heartbeats_per_thread {
                                    let node_id = (thread_id as u64 * 10000) + i as u64;
                                    let addr: std::net::SocketAddr =
                                        format!("127.0.0.1:{}", 9000 + (i % 1000)).parse().unwrap();
                                    d.heartbeat(node_id, addr);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: End-to-end failure/recovery cycle
fn bench_failure_recovery_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("failure_recovery_cycle");
    group.throughput(Throughput::Elements(1));

    let detector = FailureDetector::with_config(FailureDetectorConfig {
        timeout: std::time::Duration::from_millis(1), // Very short for benchmarking
        miss_threshold: 1,
        suspicion_threshold: 1,
        cleanup_interval: std::time::Duration::from_secs(60),
    });
    let mgr = RecoveryManager::new();

    let addr: std::net::SocketAddr = "127.0.0.1:9000".parse().unwrap();

    group.bench_function("full_cycle", |b| {
        let mut id = 0u64;
        b.iter(|| {
            // Heartbeat
            detector.heartbeat(id, addr);

            // Simulate failure check (would normally timeout)
            let _ = detector.status(id);

            // Handle potential failure
            if detector.status(id) == net::adapter::net::NodeStatus::Failed {
                let action = mgr.on_failure(id, vec![id + 1000]);
                std::hint::black_box(action);
            }

            // Recovery
            detector.heartbeat(id, addr);
            mgr.on_recovery(id);

            id += 1;
        });
    });

    group.finish();
}

// Phase 3D: Failure benchmarks
criterion_group!(
    failure_benches,
    bench_failure_detector,
    bench_loss_simulator,
    bench_circuit_breaker,
    bench_recovery_manager,
    bench_failure_scaling,
    bench_failure_concurrent,
    bench_failure_recovery_cycle,
);

// =============================================================================
// Phase 4A: Capability Announcements (CAP-ANN) Benchmarks
// =============================================================================

/// Helper to create a sample capability set for benchmarking
fn sample_capability_set(node_index: u64) -> CapabilitySet {
    let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24)
        .with_compute_units(128)
        .with_tensor_cores(512)
        .with_fp16_tflops(82.5);

    let hardware = HardwareCapabilities::new()
        .with_cpu(16, 32)
        .with_memory(64 + (node_index as u32 % 64))
        .with_gpu(gpu)
        .with_storage(2000)
        .with_network(10);

    let software = SoftwareCapabilities::new()
        .with_os("linux", "6.1")
        .add_runtime("python", "3.11")
        .add_framework("pytorch", "2.1")
        .with_cuda("12.1");

    let model = ModelCapability::new(format!("llama-3.1-{}b", 7 + (node_index % 4) * 20), "llama")
        .with_parameters(7.0 + (node_index % 4) as f32 * 20.0)
        .with_context_length(128000)
        .with_quantization("fp16")
        .add_modality(Modality::Text)
        .add_modality(Modality::Code)
        .with_tokens_per_sec(50 + (node_index % 100) as u32)
        .with_loaded(node_index.is_multiple_of(3));

    let tool = ToolCapability::new("python_repl", "Python REPL")
        .with_version("1.0.0")
        .with_estimated_time(100);

    let mut caps = CapabilitySet::new()
        .with_hardware(hardware)
        .with_software(software)
        .add_model(model)
        .add_tool(tool)
        .with_limits(ResourceLimits::new().with_max_concurrent(10));

    // Add varying tags based on node index
    if node_index.is_multiple_of(2) {
        caps = caps.add_tag("inference");
    }
    if node_index.is_multiple_of(3) {
        caps = caps.add_tag("training");
    }
    if node_index.is_multiple_of(5) {
        caps = caps.add_tag("gpu-cluster");
    }

    caps
}

/// Benchmark: CapabilitySet creation and serialization
fn bench_capability_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_set");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| sample_capability_set(42));
    });

    let caps = sample_capability_set(42);

    group.bench_function("serialize", |b| {
        b.iter(|| caps.to_bytes());
    });

    let bytes = caps.to_bytes();
    group.bench_function("deserialize", |b| {
        b.iter(|| CapabilitySet::from_bytes(&bytes));
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let bytes = caps.to_bytes();
            CapabilitySet::from_bytes(&bytes)
        });
    });

    // Compact (postcard) codec — same semantics as serialize /
    // deserialize / roundtrip above but through `to_bytes_compact`.
    // Lets the audit doc compare JSON vs compact side-by-side.
    group.bench_function("serialize_compact", |b| {
        b.iter(|| caps.to_bytes_compact());
    });

    let compact_bytes = caps.to_bytes_compact();
    group.bench_function("deserialize_compact", |b| {
        b.iter(|| CapabilitySet::from_bytes(&compact_bytes));
    });

    group.bench_function("roundtrip_compact", |b| {
        b.iter(|| {
            let bytes = caps.to_bytes_compact();
            CapabilitySet::from_bytes(&bytes)
        });
    });

    // Test has_* methods
    group.bench_function("has_tag", |b| {
        b.iter(|| caps.has_tag("inference"));
    });

    group.bench_function("has_model", |b| {
        b.iter(|| caps.has_model("llama-3.1-7b"));
    });

    group.bench_function("has_tool", |b| {
        b.iter(|| caps.has_tool("python_repl"));
    });

    group.bench_function("has_gpu", |b| {
        b.iter(|| caps.has_gpu());
    });

    group.finish();
}

/// Benchmark: CapabilityAnnouncement creation and serialization
fn bench_capability_announcement(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_announcement");
    group.throughput(Throughput::Elements(1));

    let caps = sample_capability_set(42);

    group.bench_function("create", |b| {
        b.iter(|| {
            CapabilityAnnouncement::new(0x1234, EntityId::from_bytes([0u8; 32]), 1, caps.clone())
        });
    });

    let ann = CapabilityAnnouncement::new(0x1234, EntityId::from_bytes([0u8; 32]), 1, caps.clone());

    group.bench_function("serialize", |b| {
        b.iter(|| ann.to_bytes());
    });

    let bytes = ann.to_bytes();
    group.bench_function("deserialize", |b| {
        b.iter(|| CapabilityAnnouncement::from_bytes(&bytes));
    });

    group.bench_function("is_expired", |b| {
        b.iter(|| ann.is_expired());
    });

    group.finish();
}

/// Benchmark: CapabilityFilter matching
fn bench_capability_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_filter");
    group.throughput(Throughput::Elements(1));

    let caps = sample_capability_set(42);

    // Simple filters
    let tag_filter = CapabilityFilter::new().require_tag("inference");
    group.bench_function("match_single_tag", |b| {
        b.iter(|| tag_filter.matches(&caps));
    });

    let gpu_filter = CapabilityFilter::new().require_gpu();
    group.bench_function("match_require_gpu", |b| {
        b.iter(|| gpu_filter.matches(&caps));
    });

    let vendor_filter = CapabilityFilter::new().with_gpu_vendor(GpuVendor::Nvidia);
    group.bench_function("match_gpu_vendor", |b| {
        b.iter(|| vendor_filter.matches(&caps));
    });

    let memory_filter = CapabilityFilter::new().with_min_memory(32);
    group.bench_function("match_min_memory", |b| {
        b.iter(|| memory_filter.matches(&caps));
    });

    // Complex filter
    let complex_filter = CapabilityFilter::new()
        .require_tag("inference")
        .require_gpu()
        .with_gpu_vendor(GpuVendor::Nvidia)
        .with_min_memory(32)
        .with_min_vram(16)
        .require_modality(Modality::Text);

    group.bench_function("match_complex", |b| {
        b.iter(|| complex_filter.matches(&caps));
    });

    // Filter that doesn't match
    let no_match_filter = CapabilityFilter::new().require_tag("nonexistent");
    group.bench_function("match_no_match", |b| {
        b.iter(|| no_match_filter.matches(&caps));
    });

    group.finish();
}

/// Benchmark: CapabilityFold indexing throughput
fn bench_capability_fold_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_fold_insert");

    for node_count in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*node_count as u64));

        group.bench_with_input(
            BenchmarkId::new("index_nodes", node_count),
            node_count,
            |b, &count| {
                b.iter(|| {
                    let fold = Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO);
                    for i in 0..count {
                        let caps = sample_capability_set(i as u64);
                        let ann = CapabilityAnnouncement::new(
                            i as u64,
                            EntityId::from_bytes([0u8; 32]),
                            1,
                            caps,
                        );
                        capability_bridge::apply_legacy_announcement(&fold, ann)
                            .expect("apply legacy announcement in fixture");
                    }
                    fold
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: CapabilityFold query performance
fn bench_capability_fold_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_fold_query");
    group.throughput(Throughput::Elements(1));

    // Pre-populate fold with 10k nodes
    let fold = Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO);
    for i in 0..10000 {
        let caps = sample_capability_set(i);
        let ann = CapabilityAnnouncement::new(i, EntityId::from_bytes([0u8; 32]), 1, caps);
        capability_bridge::apply_legacy_announcement(&fold, ann)
            .expect("apply legacy announcement in fixture");
    }

    // Single tag query (uses inverted index)
    let tag_filter = CapabilityFilter::new().require_tag("inference");
    group.bench_function("query_single_tag", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &tag_filter));
    });

    // GPU query (uses inverted index)
    let gpu_filter = CapabilityFilter::new().require_gpu();
    group.bench_function("query_require_gpu", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &gpu_filter));
    });

    // GPU vendor query (uses inverted index)
    let vendor_filter = CapabilityFilter::new().with_gpu_vendor(GpuVendor::Nvidia);
    group.bench_function("query_gpu_vendor", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &vendor_filter));
    });

    // Memory filter (requires full scan of candidates)
    let memory_filter = CapabilityFilter::new().with_min_memory(80);
    group.bench_function("query_min_memory", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &memory_filter));
    });

    // Complex query (multiple inverted indexes + full check)
    let complex_filter = CapabilityFilter::new()
        .require_tag("inference")
        .require_gpu()
        .with_min_memory(64);
    group.bench_function("query_complex", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &complex_filter));
    });

    // Model query
    let model_filter = CapabilityFilter::new().require_model("llama-3.1-7b");
    group.bench_function("query_model", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &model_filter));
    });

    // Tool query
    let tool_filter = CapabilityFilter::new().require_tool("python_repl");
    group.bench_function("query_tool", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &tool_filter));
    });

    // No results query
    let empty_filter = CapabilityFilter::new().require_tag("nonexistent");
    group.bench_function("query_no_results", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &empty_filter));
    });

    group.finish();
}

/// Benchmark: CapabilityFold candidate-set lookup
///
/// The legacy `CapabilityIndex::find_best` scoring path has no
/// fold-side equivalent yet; this bench treats the requirement
/// as a candidate-set query via `find_nodes_matching(req.filter)`
/// and exercises throughput only — scoring is NOT measured here.
fn bench_capability_fold_find_best(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_fold_find_best");
    group.throughput(Throughput::Elements(1));

    // Pre-populate fold
    let fold = Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO);
    for i in 0..10000 {
        let caps = sample_capability_set(i);
        let ann = CapabilityAnnouncement::new(i, EntityId::from_bytes([0u8; 32]), 1, caps);
        capability_bridge::apply_legacy_announcement(&fold, ann)
            .expect("apply legacy announcement in fixture");
    }

    // Simple requirement
    let simple_req = CapabilityRequirement::from_filter(CapabilityFilter::new().require_gpu());
    group.bench_function("find_best_simple", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &simple_req.filter));
    });

    // Requirement with preferences (preferences are not exercised
    // on the fold path; the bridge does candidate-set queries only).
    let pref_req = CapabilityRequirement::from_filter(
        CapabilityFilter::new()
            .require_gpu()
            .require_tag("inference"),
    )
    .prefer_memory(0.5)
    .prefer_vram(0.5)
    .prefer_speed(0.3);

    group.bench_function("find_best_with_prefs", |b| {
        b.iter(|| capability_bridge::find_nodes_matching(&fold, &pref_req.filter));
    });

    group.finish();
}

/// Benchmark: CapabilityFold scaling
fn bench_capability_fold_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_fold_scaling");

    for node_count in [1000, 5000, 10000, 50000].iter() {
        let fold = Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO);
        for i in 0..*node_count {
            let mut caps = sample_capability_set(i as u64);
            // Fixed-cardinality discovery target: exactly 100 nodes
            // carry this tag at EVERY fleet size. `query_tag_rare`
            // below isolates indexed-lookup cost from result
            // materialization — `query_tag`'s "inference" rides on
            // half the fleet (`sample_capability_set` tags every
            // even index), so its per-query growth with node_count
            // is the result set, not the index.
            if i < 100 {
                caps = caps.add_tag("rare-fixed");
            }
            let ann =
                CapabilityAnnouncement::new(i as u64, EntityId::from_bytes([0u8; 32]), 1, caps);
            capability_bridge::apply_legacy_announcement(&fold, ann)
                .expect("apply legacy announcement in fixture");
        }

        // Throughput is per MATCH, not per query: a query that
        // returns half the fleet is information-theoretically
        // O(matches), so per-query wall time grows linearly with
        // node_count by fixture construction. Per-match throughput
        // is the metric that's comparable across sizes —
        // 2026-06-11 service-discovery feedback read the per-query
        // growth as "linear scan" when the linear factor was the
        // result set, not the lookup.
        let tag_filter = CapabilityFilter::new().require_tag("inference");
        let tag_matches = capability_bridge::find_nodes_matching(&fold, &tag_filter).len() as u64;
        group.throughput(Throughput::Elements(tag_matches.max(1)));
        group.bench_with_input(
            BenchmarkId::new("query_tag", node_count),
            node_count,
            |b, _| {
                b.iter(|| capability_bridge::find_nodes_matching(&fold, &tag_filter));
            },
        );

        let complex_filter = CapabilityFilter::new()
            .require_tag("inference")
            .require_gpu()
            .with_min_memory(70);
        let complex_matches =
            capability_bridge::find_nodes_matching(&fold, &complex_filter).len() as u64;
        group.throughput(Throughput::Elements(complex_matches.max(1)));
        group.bench_with_input(
            BenchmarkId::new("query_complex", node_count),
            node_count,
            |b, _| {
                b.iter(|| capability_bridge::find_nodes_matching(&fold, &complex_filter));
            },
        );

        // The fleet-scale discovery shape: constant match
        // cardinality (100) while node_count scales. With the
        // inverted `by_tag` index + the single-constraint borrowed
        // fast path this should hold roughly flat as node_count
        // grows; growth here would mean the index itself (not
        // result materialization) is degrading.
        let rare_filter = CapabilityFilter::new().require_tag("rare-fixed");
        let rare_matches = capability_bridge::find_nodes_matching(&fold, &rare_filter).len() as u64;
        assert_eq!(
            rare_matches, 100,
            "rare-tag fixture must stay fixed-cardinality across fleet sizes"
        );
        group.throughput(Throughput::Elements(rare_matches));
        group.bench_with_input(
            BenchmarkId::new("query_tag_rare", node_count),
            node_count,
            |b, _| {
                b.iter(|| capability_bridge::find_nodes_matching(&fold, &rare_filter));
            },
        );
    }

    group.finish();
}

/// Benchmark: CapabilityFold concurrent access
fn bench_capability_fold_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_fold_concurrent");
    group.sample_size(20);

    let ops_per_thread = 500;

    for thread_count in [4, 8, 16].iter() {
        let total_ops = ops_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_ops as u64));

        // Concurrent indexing
        let fold = Arc::new(Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO));

        group.bench_with_input(
            BenchmarkId::new("concurrent_index", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let f = Arc::clone(&fold);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    let node_id = (thread_id as u64 * 100000) + i as u64;
                                    let caps = sample_capability_set(node_id);
                                    let ann = CapabilityAnnouncement::new(
                                        node_id,
                                        EntityId::from_bytes([0u8; 32]),
                                        1,
                                        caps,
                                    );
                                    capability_bridge::apply_legacy_announcement(&f, ann)
                                        .expect("apply legacy announcement in fixture");
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Concurrent querying (pre-populate first)
        let query_fold = Arc::new(Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO));
        for i in 0..10000 {
            let caps = sample_capability_set(i);
            let ann = CapabilityAnnouncement::new(i, EntityId::from_bytes([0u8; 32]), 1, caps);
            capability_bridge::apply_legacy_announcement(&query_fold, ann)
                .expect("apply legacy announcement in fixture");
        }

        group.bench_with_input(
            BenchmarkId::new("concurrent_query", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let f = Arc::clone(&query_fold);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    let filter = match (thread_id + i) % 4 {
                                        0 => CapabilityFilter::new().require_tag("inference"),
                                        1 => CapabilityFilter::new().require_gpu(),
                                        2 => CapabilityFilter::new().require_tool("python_repl"),
                                        _ => CapabilityFilter::new().with_min_memory(70),
                                    };
                                    let _ = capability_bridge::find_nodes_matching(&f, &filter);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Mixed read/write
        group.bench_with_input(
            BenchmarkId::new("concurrent_mixed", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let f = Arc::clone(&query_fold);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    if i % 10 == 0 {
                                        // 10% writes
                                        let node_id = (thread_id as u64 * 100000) + i as u64;
                                        let caps = sample_capability_set(node_id);
                                        let ann = CapabilityAnnouncement::new(
                                            node_id,
                                            EntityId::from_bytes([0u8; 32]),
                                            1,
                                            caps,
                                        );
                                        capability_bridge::apply_legacy_announcement(&f, ann)
                                            .expect("apply legacy announcement in fixture");
                                    } else {
                                        // 90% reads
                                        let filter =
                                            CapabilityFilter::new().require_tag("inference");
                                        let _ = capability_bridge::find_nodes_matching(&f, &filter);
                                    }
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Version handling and updates
fn bench_capability_fold_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("capability_fold_updates");
    group.throughput(Throughput::Elements(1));

    let fold = Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO);

    // Pre-populate
    for i in 0..1000 {
        let caps = sample_capability_set(i);
        let ann = CapabilityAnnouncement::new(i, EntityId::from_bytes([0u8; 32]), 1, caps);
        capability_bridge::apply_legacy_announcement(&fold, ann)
            .expect("apply legacy announcement in fixture");
    }

    // Update existing node (higher version)
    group.bench_function("update_higher_version", |b| {
        let mut version = 2u64;
        b.iter(|| {
            let caps = sample_capability_set(500);
            let ann =
                CapabilityAnnouncement::new(500, EntityId::from_bytes([0u8; 32]), version, caps);
            capability_bridge::apply_legacy_announcement(&fold, ann)
                .expect("apply legacy announcement in fixture");
            version += 1;
        });
    });

    // Update with same version (should be ignored)
    group.bench_function("update_same_version", |b| {
        b.iter(|| {
            let caps = sample_capability_set(500);
            let ann = CapabilityAnnouncement::new(500, EntityId::from_bytes([0u8; 32]), 1, caps);
            capability_bridge::apply_legacy_announcement(&fold, ann)
                .expect("apply legacy announcement in fixture");
        });
    });

    // Remove and re-add
    group.bench_function("remove_and_readd", |b| {
        let mut id = 10000u64;
        b.iter(|| {
            // Add
            let caps = sample_capability_set(id);
            let ann = CapabilityAnnouncement::new(id, EntityId::from_bytes([0u8; 32]), 1, caps);
            capability_bridge::apply_legacy_announcement(&fold, ann)
                .expect("apply legacy announcement in fixture");
            // Remove
            fold.evict_node(id, "bench");
            id += 1;
        });
    });

    group.finish();
}

// Phase 4A: Capability benchmarks
criterion_group!(
    capability_benches,
    bench_capability_set,
    bench_capability_announcement,
    bench_capability_filter,
    bench_capability_fold_insert,
    bench_capability_fold_query,
    bench_capability_fold_find_best,
    bench_capability_fold_scaling,
    bench_capability_fold_concurrent,
    bench_capability_fold_updates,
);

// =============================================================================
// Phase 4C: Node Metadata Surface (NODE-META) Benchmarks
// =============================================================================

/// Helper to create sample node metadata
fn sample_node_metadata(index: u64) -> NodeMetadata {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&index.to_le_bytes());

    let location = LocationInfo::new(match index % 6 {
        0 => Region::NorthAmerica("us-east".into()),
        1 => Region::NorthAmerica("us-west".into()),
        2 => Region::Europe("eu-west".into()),
        3 => Region::Europe("eu-central".into()),
        4 => Region::AsiaPacific("ap-northeast".into()),
        _ => Region::AsiaPacific("ap-southeast".into()),
    })
    .with_coordinates(
        40.0 + (index % 50) as f64 * 0.5,
        -74.0 + (index % 100) as f64 * 0.3,
    )
    .with_asn(10000 + (index % 1000) as u32)
    .with_provider(match index % 4 {
        0 => "aws",
        1 => "gcp",
        2 => "azure",
        _ => "cloudflare",
    });

    let topology = TopologyHints::new(match index % 5 {
        0 => NetworkTier::Core,
        1 => NetworkTier::Premium,
        2 => NetworkTier::Datacenter,
        3 => NetworkTier::Business,
        _ => NetworkTier::Consumer,
    })
    .with_bandwidth(100 + (index % 900) as u32, 100 + (index % 900) as u32)
    .with_nat(match index % 5 {
        0 => NatType::None,
        1 => NatType::FullCone,
        2 => NatType::RestrictedCone,
        3 => NatType::PortRestrictedCone,
        _ => NatType::Symmetric,
    });

    let status = match index % 10 {
        0..=6 => NodeStatus::Online,
        7 => NodeStatus::Degraded,
        8 => NodeStatus::Draining,
        _ => NodeStatus::Maintenance,
    };

    let mut node = NodeMetadata::new(id)
        .with_name(format!("node-{}", index))
        .with_owner(format!("org-{}", index % 100))
        .with_location(location)
        .with_topology(topology)
        .with_status(status);

    // Add varying tags
    if index.is_multiple_of(2) {
        node = node.with_tag("gpu");
    }
    if index.is_multiple_of(3) {
        node = node.with_tag("inference");
    }
    if index.is_multiple_of(5) {
        node = node.with_role("relay");
    }
    if index.is_multiple_of(7) {
        node = node.with_role("coordinator");
    }

    node
}

/// Benchmark: LocationInfo operations
fn bench_location_info(c: &mut Criterion) {
    let mut group = c.benchmark_group("location_info");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| {
            LocationInfo::new(Region::NorthAmerica("us-east".into()))
                .with_coordinates(40.7128, -74.0060)
                .with_asn(12345)
                .with_provider("aws")
        });
    });

    let ny = LocationInfo::new(Region::NorthAmerica("us-east".into()))
        .with_coordinates(40.7128, -74.0060);
    let la = LocationInfo::new(Region::NorthAmerica("us-west".into()))
        .with_coordinates(34.0522, -118.2437);
    let london = LocationInfo::new(Region::Europe("uk".into())).with_coordinates(51.5074, -0.1278);

    group.bench_function("distance_to", |b| {
        b.iter(|| ny.distance_to(&la));
    });

    group.bench_function("same_continent", |b| {
        b.iter(|| ny.same_continent(&la));
    });

    group.bench_function("same_continent_cross", |b| {
        b.iter(|| ny.same_continent(&london));
    });

    group.bench_function("same_region", |b| {
        b.iter(|| ny.same_region(&la));
    });

    group.finish();
}

/// Benchmark: TopologyHints operations
fn bench_topology_hints(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology_hints");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| {
            TopologyHints::new(NetworkTier::Datacenter)
                .with_bandwidth(1000, 1000)
                .with_nat(NatType::None)
                .with_relay(100)
        });
    });

    let hints = TopologyHints::new(NetworkTier::Premium)
        .with_bandwidth(1000, 1000)
        .with_nat(NatType::FullCone)
        .with_relay(50);

    group.bench_function("connectivity_score", |b| {
        b.iter(|| hints.connectivity_score());
    });

    group.bench_function("average_latency_empty", |b| {
        b.iter(|| hints.average_latency());
    });

    // With latencies
    let mut hints_with_latency = hints.clone();
    for i in 0..100 {
        let mut peer_id = [0u8; 32];
        peer_id[0] = i;
        hints_with_latency.update_latency(peer_id, 10 + i as u32);
    }

    group.bench_function("average_latency_100", |b| {
        b.iter(|| hints_with_latency.average_latency());
    });

    group.finish();
}

/// Benchmark: NatType operations
fn bench_nat_type(c: &mut Criterion) {
    let mut group = c.benchmark_group("nat_type");
    group.throughput(Throughput::Elements(1));

    group.bench_function("difficulty", |b| {
        b.iter(|| NatType::Symmetric.difficulty());
    });

    group.bench_function("can_connect_direct", |b| {
        b.iter(|| NatType::FullCone.can_connect_direct(&NatType::RestrictedCone));
    });

    group.bench_function("can_connect_symmetric", |b| {
        b.iter(|| NatType::Symmetric.can_connect_direct(&NatType::Symmetric));
    });

    group.finish();
}

/// Benchmark: NodeMetadata creation and operations
fn bench_node_metadata(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_metadata");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create_simple", |b| {
        b.iter(|| {
            let mut id = [0u8; 32];
            id[0] = 42;
            NodeMetadata::new(id)
        });
    });

    group.bench_function("create_full", |b| {
        b.iter(|| sample_node_metadata(42));
    });

    let node = sample_node_metadata(42);

    group.bench_function("routing_score", |b| {
        b.iter(|| node.routing_score());
    });

    group.bench_function("age", |b| {
        b.iter(|| node.age());
    });

    group.bench_function("is_stale", |b| {
        b.iter(|| node.is_stale(std::time::Duration::from_secs(60)));
    });

    // Serialization
    group.bench_function("serialize", |b| {
        b.iter(|| serde_json::to_vec(&node));
    });

    let bytes = serde_json::to_vec(&node).unwrap();
    group.bench_function("deserialize", |b| {
        b.iter(|| serde_json::from_slice::<NodeMetadata>(&bytes));
    });

    group.finish();
}

/// Benchmark: MetadataQuery matching
fn bench_metadata_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_query");
    group.throughput(Throughput::Elements(1));

    let node = sample_node_metadata(42);

    // Simple queries
    let status_query = MetadataQuery::new().with_status(NodeStatus::Online);
    group.bench_function("match_status", |b| {
        b.iter(|| status_query.matches(&node));
    });

    let tier_query = MetadataQuery::new().with_min_tier(NetworkTier::Datacenter);
    group.bench_function("match_min_tier", |b| {
        b.iter(|| tier_query.matches(&node));
    });

    let continent_query = MetadataQuery::new().with_continent("north_america");
    group.bench_function("match_continent", |b| {
        b.iter(|| continent_query.matches(&node));
    });

    // Complex query
    let complex_query = MetadataQuery::new()
        .with_status(NodeStatus::Online)
        .with_min_tier(NetworkTier::Business)
        .with_continent("north_america")
        .accepting_work();

    group.bench_function("match_complex", |b| {
        b.iter(|| complex_query.matches(&node));
    });

    // No match
    let no_match_query = MetadataQuery::new().with_status(NodeStatus::Offline);
    group.bench_function("match_no_match", |b| {
        b.iter(|| no_match_query.matches(&node));
    });

    group.finish();
}

/// Benchmark: MetadataStore basic operations
fn bench_metadata_store_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_store_basic");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(MetadataStore::new);
    });

    let store = MetadataStore::new();

    // Pre-populate
    for i in 0..1000 {
        let node = sample_node_metadata(i);
        store.upsert(node).unwrap();
    }

    group.bench_function("upsert_new", |b| {
        let mut id = 10000u64;
        b.iter(|| {
            let node = sample_node_metadata(id);
            store.upsert(node).unwrap();
            id += 1;
        });
    });

    group.bench_function("upsert_existing", |b| {
        b.iter(|| {
            let node = sample_node_metadata(500);
            store.upsert(node).unwrap();
        });
    });

    let mut lookup_id = [0u8; 32];
    lookup_id[0..8].copy_from_slice(&500u64.to_le_bytes());

    group.bench_function("get", |b| {
        b.iter(|| store.get(&lookup_id));
    });

    group.bench_function("get_miss", |b| {
        let mut miss_id = [0u8; 32];
        miss_id[0] = 255;
        b.iter(|| store.get(&miss_id));
    });

    group.bench_function("len", |b| {
        b.iter(|| store.len());
    });

    group.bench_function("stats", |b| {
        b.iter(|| store.stats());
    });

    group.finish();
}

/// Benchmark: MetadataStore queries
fn bench_metadata_store_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_store_query");
    group.throughput(Throughput::Elements(1));

    let store = MetadataStore::new();

    // Pre-populate with 10k nodes
    for i in 0..10000 {
        let node = sample_node_metadata(i);
        store.upsert(node).unwrap();
    }

    // Query by status (uses index)
    let status_query = MetadataQuery::new().with_status(NodeStatus::Online);
    group.bench_function("query_by_status", |b| {
        b.iter(|| store.query(&status_query));
    });

    // Query by continent (uses index)
    let continent_query = MetadataQuery::new().with_continent("north_america");
    group.bench_function("query_by_continent", |b| {
        b.iter(|| store.query(&continent_query));
    });

    // Query by tier (uses index)
    let tier_query = MetadataQuery::new().with_min_tier(NetworkTier::Datacenter);
    group.bench_function("query_by_tier", |b| {
        b.iter(|| store.query(&tier_query));
    });

    // Query accepting work
    let work_query = MetadataQuery::new().accepting_work();
    group.bench_function("query_accepting_work", |b| {
        b.iter(|| store.query(&work_query));
    });

    // Query with limit
    let limited_query = MetadataQuery::new().with_limit(10);
    group.bench_function("query_with_limit", |b| {
        b.iter(|| store.query(&limited_query));
    });

    // Complex query
    let complex_query = MetadataQuery::new()
        .with_status(NodeStatus::Online)
        .with_continent("north_america")
        .with_min_tier(NetworkTier::Business)
        .with_limit(50);

    group.bench_function("query_complex", |b| {
        b.iter(|| store.query(&complex_query));
    });

    group.finish();
}

/// Benchmark: MetadataStore spatial queries
fn bench_metadata_store_spatial(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_store_spatial");
    group.throughput(Throughput::Elements(1));

    let store = MetadataStore::new();

    // Pre-populate
    for i in 0..10000 {
        let node = sample_node_metadata(i);
        store.upsert(node).unwrap();
    }

    let ny = LocationInfo::new(Region::NorthAmerica("us-east".into()))
        .with_coordinates(40.7128, -74.0060);

    group.bench_function("find_nearby_100km", |b| {
        b.iter(|| store.find_nearby(&ny, 100.0, 10));
    });

    group.bench_function("find_nearby_1000km", |b| {
        b.iter(|| store.find_nearby(&ny, 1000.0, 10));
    });

    group.bench_function("find_nearby_5000km", |b| {
        b.iter(|| store.find_nearby(&ny, 5000.0, 50));
    });

    group.bench_function("find_best_for_routing", |b| {
        b.iter(|| store.find_best_for_routing(10));
    });

    group.bench_function("find_relays", |b| {
        b.iter(|| store.find_relays());
    });

    group.finish();
}

/// Benchmark: MetadataStore scaling
fn bench_metadata_store_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_store_scaling");

    for node_count in [1000, 5000, 10000, 50000].iter() {
        let store = MetadataStore::new();

        for i in 0..*node_count {
            let node = sample_node_metadata(i as u64);
            store.upsert(node).unwrap();
        }

        let status_query = MetadataQuery::new().with_status(NodeStatus::Online);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("query_status", node_count),
            node_count,
            |b, _| {
                b.iter(|| store.query(&status_query));
            },
        );

        let complex_query = MetadataQuery::new()
            .with_status(NodeStatus::Online)
            .with_min_tier(NetworkTier::Datacenter)
            .with_limit(100);

        group.bench_with_input(
            BenchmarkId::new("query_complex", node_count),
            node_count,
            |b, _| {
                b.iter(|| store.query(&complex_query));
            },
        );

        let ny = LocationInfo::new(Region::NorthAmerica("us-east".into()))
            .with_coordinates(40.7128, -74.0060);

        group.bench_with_input(
            BenchmarkId::new("find_nearby", node_count),
            node_count,
            |b, _| {
                b.iter(|| store.find_nearby(&ny, 2000.0, 20));
            },
        );
    }

    group.finish();
}

/// Benchmark: MetadataStore concurrent access
fn bench_metadata_store_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_store_concurrent");
    group.sample_size(20);

    let ops_per_thread = 500;

    for thread_count in [4, 8, 16].iter() {
        let total_ops = ops_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_ops as u64));

        // Concurrent inserts
        let store = Arc::new(MetadataStore::new());

        group.bench_with_input(
            BenchmarkId::new("concurrent_upsert", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let s = Arc::clone(&store);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    let node_idx = (thread_id as u64 * 100000) + i as u64;
                                    let node = sample_node_metadata(node_idx);
                                    s.upsert(node).unwrap();
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Pre-populate for query tests
        let query_store = Arc::new(MetadataStore::new());
        for i in 0..10000 {
            let node = sample_node_metadata(i);
            query_store.upsert(node).unwrap();
        }

        // Concurrent queries
        group.bench_with_input(
            BenchmarkId::new("concurrent_query", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let s = Arc::clone(&query_store);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    let query = match (thread_id + i) % 4 {
                                        0 => MetadataQuery::new().with_status(NodeStatus::Online),
                                        1 => MetadataQuery::new()
                                            .with_min_tier(NetworkTier::Datacenter),
                                        2 => MetadataQuery::new().with_continent("north_america"),
                                        _ => MetadataQuery::new().accepting_work(),
                                    };
                                    let _ = s.query(&query);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Mixed read/write
        group.bench_with_input(
            BenchmarkId::new("concurrent_mixed", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let s = Arc::clone(&query_store);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    if i % 10 == 0 {
                                        // 10% writes
                                        let node_idx = (thread_id as u64 * 100000) + i as u64;
                                        let node = sample_node_metadata(node_idx);
                                        let _ = s.upsert(node);
                                    } else {
                                        // 90% reads
                                        let query =
                                            MetadataQuery::new().with_status(NodeStatus::Online);
                                        let _ = s.query(&query);
                                    }
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Version conflict handling
fn bench_metadata_store_versioning(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_store_versioning");
    group.throughput(Throughput::Elements(1));

    let store = MetadataStore::new();

    // Add initial node
    let node = sample_node_metadata(1);
    store.upsert(node).unwrap();

    group.bench_function("update_versioned_success", |b| {
        let mut version = 1u64;
        b.iter(|| {
            let mut node = sample_node_metadata(1);
            node.version = version + 1;
            let result = store.update_versioned(node, version);
            version += 1;
            result
        });
    });

    group.bench_function("update_versioned_conflict", |b| {
        b.iter(|| {
            let node = sample_node_metadata(1);
            store.update_versioned(node, 999)
        });
    });

    group.finish();
}

// Phase 4C: Metadata benchmarks
criterion_group!(
    metadata_benches,
    bench_location_info,
    bench_topology_hints,
    bench_nat_type,
    bench_node_metadata,
    bench_metadata_query,
    bench_metadata_store_basic,
    bench_metadata_store_query,
    bench_metadata_store_spatial,
    bench_metadata_store_scaling,
    bench_metadata_store_concurrent,
    bench_metadata_store_versioning,
);

// =============================================================================
// Phase 4D: Node APIs & Schemas (API-SCHEMA) Benchmarks
// =============================================================================

/// Helper to create a sample API schema
fn sample_api_schema(index: u64) -> ApiSchema {
    let infer_endpoint = ApiEndpoint::new("/models/{model_id}/infer", ApiMethod::Post)
        .with_description("Run inference on a model")
        .with_path_param(ApiParameter::required("model_id", SchemaType::string()))
        .with_request_body(
            SchemaType::object()
                .with_property("prompt", SchemaType::string())
                .with_property(
                    "max_tokens",
                    SchemaType::integer().with_minimum(1).with_maximum(4096),
                )
                .with_property("temperature", SchemaType::number())
                .with_required("prompt"),
        )
        .with_response(
            SchemaType::object()
                .with_property("text", SchemaType::string())
                .with_property("tokens_used", SchemaType::integer()),
        )
        .with_tag("inference")
        .with_rate_limit(100)
        .with_timeout(30000);

    let list_endpoint = ApiEndpoint::new("/models", ApiMethod::Get)
        .with_description("List available models")
        .with_query_param(ApiParameter::optional("limit", SchemaType::integer()))
        .with_query_param(ApiParameter::optional("offset", SchemaType::integer()))
        .with_response(SchemaType::array(
            SchemaType::object()
                .with_property("id", SchemaType::string())
                .with_property("name", SchemaType::string()),
        ))
        .with_tag("models");

    let stream_endpoint = ApiEndpoint::new("/models/{model_id}/stream", ApiMethod::Stream)
        .with_description("Stream inference results")
        .with_path_param(ApiParameter::required("model_id", SchemaType::string()))
        .with_tag("inference")
        .with_tag("streaming");

    ApiSchema::new(
        format!("inference-api-{}", index % 10),
        ApiVersion::new(1, (index % 5) as u32, (index % 10) as u32),
    )
    .with_description("Model inference API")
    .with_base_path("/api/v1")
    .with_tag("ai")
    .with_tag(if index.is_multiple_of(2) {
        "gpu"
    } else {
        "cpu"
    })
    .add_endpoint(infer_endpoint)
    .add_endpoint(list_endpoint)
    .add_endpoint(stream_endpoint)
}

/// Benchmark: SchemaType validation
fn bench_schema_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("schema_validation");
    group.throughput(Throughput::Elements(1));

    // Simple string validation
    let string_schema = SchemaType::string().with_max_length(100);
    let string_value = serde_json::json!("hello world");

    group.bench_function("validate_string", |b| {
        b.iter(|| string_schema.validate(&string_value));
    });

    // Integer with range
    let int_schema = SchemaType::integer().with_minimum(0).with_maximum(1000);
    let int_value = serde_json::json!(500);

    group.bench_function("validate_integer", |b| {
        b.iter(|| int_schema.validate(&int_value));
    });

    // Object validation
    let obj_schema = SchemaType::object()
        .with_property("name", SchemaType::string())
        .with_property("age", SchemaType::integer())
        .with_property("email", SchemaType::string())
        .with_required("name")
        .with_required("email");

    let obj_value = serde_json::json!({
        "name": "Alice",
        "age": 30,
        "email": "alice@example.com"
    });

    group.bench_function("validate_object", |b| {
        b.iter(|| obj_schema.validate(&obj_value));
    });

    // Array validation
    let arr_schema = SchemaType::array(SchemaType::integer());
    let arr_value = serde_json::json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

    group.bench_function("validate_array_10", |b| {
        b.iter(|| arr_schema.validate(&arr_value));
    });

    // Complex nested object
    let complex_schema = SchemaType::object()
        .with_property(
            "user",
            SchemaType::object()
                .with_property("name", SchemaType::string())
                .with_property("roles", SchemaType::array(SchemaType::string())),
        )
        .with_property(
            "data",
            SchemaType::array(
                SchemaType::object()
                    .with_property("id", SchemaType::integer())
                    .with_property("value", SchemaType::number()),
            ),
        );

    let complex_value = serde_json::json!({
        "user": {"name": "Bob", "roles": ["admin", "user"]},
        "data": [
            {"id": 1, "value": 1.5},
            {"id": 2, "value": 2.5},
            {"id": 3, "value": 3.5}
        ]
    });

    group.bench_function("validate_complex", |b| {
        b.iter(|| complex_schema.validate(&complex_value));
    });

    group.finish();
}

/// Benchmark: ApiEndpoint path matching
fn bench_endpoint_matching(c: &mut Criterion) {
    let mut group = c.benchmark_group("endpoint_matching");
    group.throughput(Throughput::Elements(1));

    let endpoint = ApiEndpoint::new("/models/{model_id}/infer", ApiMethod::Post);

    group.bench_function("match_success", |b| {
        b.iter(|| endpoint.matches_path("/models/llama-7b/infer"));
    });

    group.bench_function("match_failure", |b| {
        b.iter(|| endpoint.matches_path("/models/llama-7b/train"));
    });

    // Endpoint with multiple params
    let multi_param = ApiEndpoint::new(
        "/orgs/{org_id}/projects/{project_id}/models/{model_id}",
        ApiMethod::Get,
    );

    group.bench_function("match_multi_param", |b| {
        b.iter(|| multi_param.matches_path("/orgs/acme/projects/ml/models/bert"));
    });

    group.finish();
}

/// Benchmark: ApiVersion operations
fn bench_api_version(c: &mut Criterion) {
    let mut group = c.benchmark_group("api_version");
    group.throughput(Throughput::Elements(1));

    let v1 = ApiVersion::new(1, 2, 3);
    let v2 = ApiVersion::new(1, 1, 0);

    group.bench_function("is_compatible_with", |b| {
        b.iter(|| v1.is_compatible_with(&v2));
    });

    group.bench_function("parse", |b| {
        b.iter(|| ApiVersion::parse("1.2.3"));
    });

    group.bench_function("to_string", |b| {
        b.iter(|| v1.to_string());
    });

    group.finish();
}

/// Benchmark: ApiSchema creation and serialization
fn bench_api_schema(c: &mut Criterion) {
    let mut group = c.benchmark_group("api_schema");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| sample_api_schema(42));
    });

    let schema = sample_api_schema(42);

    group.bench_function("serialize", |b| {
        b.iter(|| schema.to_bytes());
    });

    let bytes = schema.to_bytes();
    group.bench_function("deserialize", |b| {
        b.iter(|| ApiSchema::from_bytes(&bytes));
    });

    group.bench_function("find_endpoint", |b| {
        b.iter(|| schema.find_endpoint("/models/{model_id}/infer", ApiMethod::Post));
    });

    group.bench_function("endpoints_by_tag", |b| {
        b.iter(|| schema.endpoints_by_tag("inference"));
    });

    group.finish();
}

/// Benchmark: Request validation
fn bench_request_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("request_validation");
    group.throughput(Throughput::Elements(1));

    let endpoint = ApiEndpoint::new("/models/{model_id}/infer", ApiMethod::Post)
        .with_path_param(ApiParameter::required("model_id", SchemaType::string()))
        .with_query_param(ApiParameter::optional("stream", SchemaType::boolean()))
        .with_request_body(
            SchemaType::object()
                .with_property("prompt", SchemaType::string())
                .with_property("max_tokens", SchemaType::integer())
                .with_required("prompt"),
        );

    let mut path_params = std::collections::HashMap::new();
    path_params.insert("model_id".to_string(), serde_json::json!("llama-7b"));

    let query_params = std::collections::HashMap::new();

    let body = serde_json::json!({
        "prompt": "Hello, world!",
        "max_tokens": 100
    });

    group.bench_function("validate_full_request", |b| {
        b.iter(|| endpoint.validate_request(&path_params, &query_params, Some(&body)));
    });

    group.bench_function("validate_path_only", |b| {
        let empty_endpoint = ApiEndpoint::new("/models/{model_id}", ApiMethod::Get)
            .with_path_param(ApiParameter::required("model_id", SchemaType::string()));
        let empty_query = std::collections::HashMap::new();
        b.iter(|| empty_endpoint.validate_request(&path_params, &empty_query, None));
    });

    group.finish();
}

/// Benchmark: ApiRegistry basic operations
fn bench_api_registry_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("api_registry_basic");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(ApiRegistry::new);
    });

    let registry = ApiRegistry::new();

    // Pre-populate
    for i in 0..1000 {
        let mut id = [0u8; 32];
        id[0..8].copy_from_slice(&(i as u64).to_le_bytes());
        let schema = sample_api_schema(i as u64);
        let ann = ApiAnnouncement::new(id, vec![schema]);
        registry.register(ann).unwrap();
    }

    group.bench_function("register_new", |b| {
        let mut idx = 10000u64;
        b.iter(|| {
            let mut id = [0u8; 32];
            id[0..8].copy_from_slice(&idx.to_le_bytes());
            let schema = sample_api_schema(idx);
            let ann = ApiAnnouncement::new(id, vec![schema]);
            registry.register(ann).unwrap();
            idx += 1;
        });
    });

    let mut lookup_id = [0u8; 32];
    lookup_id[0..8].copy_from_slice(&500u64.to_le_bytes());

    group.bench_function("get", |b| {
        b.iter(|| registry.get(&lookup_id));
    });

    group.bench_function("len", |b| {
        b.iter(|| registry.len());
    });

    group.bench_function("stats", |b| {
        b.iter(|| registry.stats());
    });

    group.finish();
}

/// Benchmark: ApiRegistry queries
fn bench_api_registry_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("api_registry_query");
    group.throughput(Throughput::Elements(1));

    let registry = ApiRegistry::new();

    // Pre-populate with 10k nodes
    for i in 0..10000 {
        let mut id = [0u8; 32];
        id[0..8].copy_from_slice(&(i as u64).to_le_bytes());
        let schema = sample_api_schema(i as u64);
        let ann = ApiAnnouncement::new(id, vec![schema]);
        registry.register(ann).unwrap();
    }

    // Query by API name
    let name_query = ApiQuery::new().with_api("inference-api-5");
    group.bench_function("query_by_name", |b| {
        b.iter(|| registry.query(&name_query));
    });

    // Query by tag
    let tag_query = ApiQuery::new().with_tag("gpu");
    group.bench_function("query_by_tag", |b| {
        b.iter(|| registry.query(&tag_query));
    });

    // Query with version
    let version_query = ApiQuery::new()
        .with_api("inference-api-0")
        .with_min_version(ApiVersion::new(1, 2, 0));
    group.bench_function("query_with_version", |b| {
        b.iter(|| registry.query(&version_query));
    });

    // Find by endpoint
    group.bench_function("find_by_endpoint", |b| {
        b.iter(|| registry.find_by_endpoint("/models/{model_id}/infer", ApiMethod::Post));
    });

    // Find compatible
    group.bench_function("find_compatible", |b| {
        b.iter(|| registry.find_compatible("inference-api-0", &ApiVersion::new(1, 1, 0)));
    });

    group.finish();
}

/// Benchmark: ApiRegistry scaling
fn bench_api_registry_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("api_registry_scaling");

    for node_count in [1000, 5000, 10000].iter() {
        let registry = ApiRegistry::new();

        for i in 0..*node_count {
            let mut id = [0u8; 32];
            id[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            let schema = sample_api_schema(i as u64);
            let ann = ApiAnnouncement::new(id, vec![schema]);
            registry.register(ann).unwrap();
        }

        let name_query = ApiQuery::new().with_api("inference-api-5");

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("query_by_name", node_count),
            node_count,
            |b, _| {
                b.iter(|| registry.query(&name_query));
            },
        );

        let tag_query = ApiQuery::new().with_tag("gpu");
        group.bench_with_input(
            BenchmarkId::new("query_by_tag", node_count),
            node_count,
            |b, _| {
                b.iter(|| registry.query(&tag_query));
            },
        );
    }

    group.finish();
}

/// Benchmark: ApiRegistry concurrent access
fn bench_api_registry_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("api_registry_concurrent");
    group.sample_size(20);

    let ops_per_thread = 500;

    for thread_count in [4, 8, 16].iter() {
        let total_ops = ops_per_thread * *thread_count;
        group.throughput(Throughput::Elements(total_ops as u64));

        // Pre-populate for query tests
        let registry = Arc::new(ApiRegistry::new());
        for i in 0..10000 {
            let mut id = [0u8; 32];
            id[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            let schema = sample_api_schema(i as u64);
            let ann = ApiAnnouncement::new(id, vec![schema]);
            registry.register(ann).unwrap();
        }

        // Concurrent queries
        group.bench_with_input(
            BenchmarkId::new("concurrent_query", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let r = Arc::clone(&registry);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    let query = match (thread_id + i) % 3 {
                                        0 => ApiQuery::new()
                                            .with_api(format!("inference-api-{}", i % 10)),
                                        1 => ApiQuery::new().with_tag("gpu"),
                                        _ => ApiQuery::new().with_tag("ai"),
                                    };
                                    let _ = r.query(&query);
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        // Mixed read/write
        group.bench_with_input(
            BenchmarkId::new("concurrent_mixed", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|thread_id| {
                            let r = Arc::clone(&registry);
                            thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    if i % 10 == 0 {
                                        // 10% writes
                                        let mut id = [0u8; 32];
                                        let node_idx = (thread_id as u64 * 100000) + i as u64;
                                        id[0..8].copy_from_slice(&node_idx.to_le_bytes());
                                        let schema = sample_api_schema(node_idx);
                                        let ann = ApiAnnouncement::new(id, vec![schema]);
                                        let _ = r.register(ann);
                                    } else {
                                        // 90% reads
                                        let query = ApiQuery::new().with_tag("gpu");
                                        let _ = r.query(&query);
                                    }
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

// Phase 4D: API benchmarks
criterion_group!(
    api_benches,
    bench_schema_validation,
    bench_endpoint_matching,
    bench_api_version,
    bench_api_schema,
    bench_request_validation,
    bench_api_registry_basic,
    bench_api_registry_query,
    bench_api_registry_scaling,
    bench_api_registry_concurrent,
);

// =============================================================================
// Phase 4E: Device Autonomy Rules (DEVICE-RULES) Benchmarks
// =============================================================================

/// Helper to create a sample rule
fn sample_rule(index: u64) -> Rule {
    let condition = match index % 5 {
        0 => ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(80))),
        1 => ConditionExpr::single(Condition::lt("memory_free", serde_json::json!(1024))),
        2 => ConditionExpr::and(vec![
            ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(70))),
            ConditionExpr::single(Condition::gt("memory_usage", serde_json::json!(80))),
        ]),
        3 => ConditionExpr::or(vec![
            ConditionExpr::single(Condition::eq("status", serde_json::json!("error"))),
            ConditionExpr::single(Condition::gt("error_count", serde_json::json!(10))),
        ]),
        _ => ConditionExpr::single(Condition::is_in(
            "region",
            vec![
                serde_json::json!("us-east"),
                serde_json::json!("us-west"),
                serde_json::json!("eu-west"),
            ],
        )),
    };

    let action = match index % 4 {
        0 => Action::log(LogLevel::Warn, format!("Rule {} triggered", index)),
        1 => Action::alert(AlertSeverity::High, "Alert", "High resource usage"),
        2 => Action::emit("rule_triggered", serde_json::json!({"rule_id": index})),
        _ => Action::chain(vec![
            Action::log(LogLevel::Info, "Chain action 1"),
            Action::emit("event", serde_json::json!({})),
        ]),
    };

    let priority = match index % 5 {
        0 => Priority::Highest,
        1 => Priority::High,
        2 => Priority::Normal,
        3 => Priority::Low,
        _ => Priority::Lowest,
    };

    Rule::new(format!("rule-{}", index), format!("Rule {}", index))
        .with_priority(priority)
        .with_condition(condition)
        .with_action(action)
        .with_tag(if index.is_multiple_of(2) {
            "monitoring"
        } else {
            "alerting"
        })
        .with_tag(if index.is_multiple_of(3) {
            "critical"
        } else {
            "normal"
        })
}

/// Helper to create a sample context
fn sample_context(cpu: i64, memory: i64, status: &str) -> RuleContext {
    let mut ctx = RuleContext::new();
    ctx.set("cpu_usage", serde_json::json!(cpu));
    ctx.set("memory_usage", serde_json::json!(memory));
    ctx.set("memory_free", serde_json::json!(16384 - memory * 164));
    ctx.set("status", serde_json::json!(status));
    ctx.set("error_count", serde_json::json!(cpu / 10));
    ctx.set("region", serde_json::json!("us-east"));
    ctx.set(
        "metrics",
        serde_json::json!({
            "cpu": {"usage": cpu, "cores": 8},
            "memory": {"used": memory * 100, "total": 16384},
            "disk": {"used": 50000, "total": 100000}
        }),
    );
    ctx
}

/// Benchmark: CompareOp evaluation
fn bench_compare_op(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_op");
    group.throughput(Throughput::Elements(1));

    let val_a = serde_json::json!(85);
    let val_b = serde_json::json!(80);

    group.bench_function("eq", |b| {
        b.iter(|| CompareOp::Eq.evaluate(&val_a, &val_b));
    });

    group.bench_function("gt", |b| {
        b.iter(|| CompareOp::Gt.evaluate(&val_a, &val_b));
    });

    let str_a = serde_json::json!("hello world");
    let str_b = serde_json::json!("world");

    group.bench_function("contains_string", |b| {
        b.iter(|| CompareOp::Contains.evaluate(&str_a, &str_b));
    });

    let val = serde_json::json!("us-east");
    let arr = serde_json::json!(["us-east", "us-west", "eu-west"]);

    group.bench_function("in_array", |b| {
        b.iter(|| CompareOp::In.evaluate(&val, &arr));
    });

    group.finish();
}

/// Benchmark: Condition evaluation
fn bench_condition(c: &mut Criterion) {
    let mut group = c.benchmark_group("condition");
    group.throughput(Throughput::Elements(1));

    let ctx = sample_context(85, 70, "running");

    // Simple condition
    let simple = Condition::gt("cpu_usage", serde_json::json!(80));
    group.bench_function("simple", |b| {
        b.iter(|| simple.evaluate(&ctx));
    });

    // Nested field access
    let nested = Condition::gt("metrics.cpu.usage", serde_json::json!(80));
    group.bench_function("nested_field", |b| {
        b.iter(|| nested.evaluate(&ctx));
    });

    // String comparison
    let string_cond = Condition::eq("status", serde_json::json!("running"));
    group.bench_function("string_eq", |b| {
        b.iter(|| string_cond.evaluate(&ctx));
    });

    group.finish();
}

/// Benchmark: ConditionExpr evaluation
fn bench_condition_expr(c: &mut Criterion) {
    let mut group = c.benchmark_group("condition_expr");
    group.throughput(Throughput::Elements(1));

    let ctx = sample_context(85, 70, "running");

    // Single condition
    let single = ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(80)));
    group.bench_function("single", |b| {
        b.iter(|| single.evaluate(&ctx));
    });

    // AND with 2 conditions
    let and_2 = ConditionExpr::and(vec![
        ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(80))),
        ConditionExpr::single(Condition::gt("memory_usage", serde_json::json!(60))),
    ]);
    group.bench_function("and_2", |b| {
        b.iter(|| and_2.evaluate(&ctx));
    });

    // AND with 5 conditions
    let and_5 = ConditionExpr::and(vec![
        ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(50))),
        ConditionExpr::single(Condition::gt("memory_usage", serde_json::json!(50))),
        ConditionExpr::single(Condition::eq("status", serde_json::json!("running"))),
        ConditionExpr::single(Condition::eq("region", serde_json::json!("us-east"))),
        ConditionExpr::single(Condition::lt("error_count", serde_json::json!(100))),
    ]);
    group.bench_function("and_5", |b| {
        b.iter(|| and_5.evaluate(&ctx));
    });

    // OR with 3 conditions
    let or_3 = ConditionExpr::or(vec![
        ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(90))),
        ConditionExpr::single(Condition::gt("memory_usage", serde_json::json!(90))),
        ConditionExpr::single(Condition::eq("status", serde_json::json!("error"))),
    ]);
    group.bench_function("or_3", |b| {
        b.iter(|| or_3.evaluate(&ctx));
    });

    // Nested: (A AND B) OR (C AND D)
    let nested = ConditionExpr::or(vec![
        ConditionExpr::and(vec![
            ConditionExpr::single(Condition::gt("cpu_usage", serde_json::json!(90))),
            ConditionExpr::single(Condition::gt("memory_usage", serde_json::json!(90))),
        ]),
        ConditionExpr::and(vec![
            ConditionExpr::single(Condition::eq("status", serde_json::json!("error"))),
            ConditionExpr::single(Condition::gt("error_count", serde_json::json!(5))),
        ]),
    ]);
    group.bench_function("nested", |b| {
        b.iter(|| nested.evaluate(&ctx));
    });

    group.finish();
}

/// Benchmark: Rule creation and serialization
fn bench_rule(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| sample_rule(42));
    });

    let rule = sample_rule(42);

    let ctx = sample_context(85, 70, "running");
    group.bench_function("matches", |b| {
        b.iter(|| rule.matches(&ctx));
    });

    group.finish();
}

/// Benchmark: RuleContext operations
fn bench_rule_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_context");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| sample_context(85, 70, "running"));
    });

    let ctx = sample_context(85, 70, "running");

    group.bench_function("get_simple", |b| {
        b.iter(|| ctx.get_field("cpu_usage"));
    });

    group.bench_function("get_nested", |b| {
        b.iter(|| ctx.get_field("metrics.cpu.usage"));
    });

    group.bench_function("get_deep_nested", |b| {
        b.iter(|| ctx.get_field("metrics.memory.used"));
    });

    group.finish();
}

/// Benchmark: RuleEngine basic operations
fn bench_rule_engine_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_engine_basic");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(RuleEngine::new);
    });

    let mut engine = RuleEngine::new();
    for i in 0..100 {
        engine.add_rule(sample_rule(i)).unwrap();
    }

    group.bench_function("add_rule", |b| {
        let mut idx = 1000u64;
        let mut e = RuleEngine::new();
        b.iter(|| {
            e.add_rule(sample_rule(idx)).unwrap();
            idx += 1;
        });
    });

    group.bench_function("get_rule", |b| {
        b.iter(|| engine.get_rule("rule-50"));
    });

    group.bench_function("rules_by_tag", |b| {
        b.iter(|| engine.rules_by_tag("monitoring"));
    });

    group.bench_function("stats", |b| {
        b.iter(|| engine.stats());
    });

    group.finish();
}

/// Benchmark: RuleEngine evaluation
fn bench_rule_engine_evaluate(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_engine_evaluate");
    group.throughput(Throughput::Elements(1));

    // Engine with 10 rules
    let mut engine_10 = RuleEngine::new();
    for i in 0..10 {
        engine_10.add_rule(sample_rule(i)).unwrap();
    }

    let ctx = sample_context(85, 70, "running");

    group.bench_function("evaluate_10_rules", |b| {
        b.iter(|| engine_10.evaluate(&ctx));
    });

    group.bench_function("evaluate_first_10_rules", |b| {
        b.iter(|| engine_10.evaluate_first(&ctx));
    });

    // Engine with 100 rules
    let mut engine_100 = RuleEngine::new();
    for i in 0..100 {
        engine_100.add_rule(sample_rule(i)).unwrap();
    }

    group.bench_function("evaluate_100_rules", |b| {
        b.iter(|| engine_100.evaluate(&ctx));
    });

    group.bench_function("evaluate_first_100_rules", |b| {
        b.iter(|| engine_100.evaluate_first(&ctx));
    });

    group.bench_function("evaluate_matching_100_rules", |b| {
        b.iter(|| engine_100.evaluate_matching(&ctx));
    });

    // Engine with 1000 rules
    let mut engine_1000 = RuleEngine::new();
    for i in 0..1000 {
        engine_1000.add_rule(sample_rule(i)).unwrap();
    }

    group.bench_function("evaluate_1000_rules", |b| {
        b.iter(|| engine_1000.evaluate(&ctx));
    });

    group.bench_function("evaluate_first_1000_rules", |b| {
        b.iter(|| engine_1000.evaluate_first(&ctx));
    });

    group.finish();
}

/// Benchmark: RuleEngine scaling
fn bench_rule_engine_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_engine_scaling");

    let ctx = sample_context(85, 70, "running");

    for rule_count in [10, 50, 100, 500, 1000].iter() {
        let mut engine = RuleEngine::new();
        for i in 0..*rule_count {
            engine.add_rule(sample_rule(i as u64)).unwrap();
        }

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("evaluate", rule_count),
            rule_count,
            |b, _| {
                b.iter(|| engine.evaluate(&ctx));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("evaluate_first", rule_count),
            rule_count,
            |b, _| {
                b.iter(|| engine.evaluate_first(&ctx));
            },
        );
    }

    group.finish();
}

/// Benchmark: RuleSet operations
fn bench_rule_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_set");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| {
            let mut set = RuleSet::new("test", "Test Rules");
            for i in 0..10 {
                set = set.add_rule(sample_rule(i));
            }
            set
        });
    });

    let mut rule_set = RuleSet::new("test", "Test Rules");
    for i in 0..10 {
        rule_set = rule_set.add_rule(sample_rule(i));
    }

    group.bench_function("load_into_engine", |b| {
        b.iter(|| {
            let mut engine = RuleEngine::new();
            rule_set.load_into(&mut engine).unwrap();
            engine
        });
    });

    group.finish();
}

// ============================================================================
// Phase 4F: Context Fabric Benchmarks
// ============================================================================

fn sample_node_id() -> [u8; 32] {
    [42u8; 32]
}

/// Benchmark: TraceId generation and parsing
fn bench_trace_id(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_id");
    group.throughput(Throughput::Elements(1));

    group.bench_function("generate", |b| {
        b.iter(TraceId::generate);
    });

    let trace_id = TraceId::generate();
    let hex = trace_id.to_hex();

    group.bench_function("to_hex", |b| {
        b.iter(|| trace_id.to_hex());
    });

    group.bench_function("from_hex", |b| {
        b.iter(|| TraceId::from_hex(&hex));
    });

    group.finish();
}

/// Benchmark: Context creation and operations
fn bench_context_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_operations");
    group.throughput(Throughput::Elements(1));

    let node_id = sample_node_id();

    group.bench_function("create", |b| {
        b.iter(|| Context::new(node_id));
    });

    let ctx = Context::new(node_id);

    group.bench_function("child", |b| {
        b.iter(|| ctx.child("child_operation"));
    });

    group.bench_function("for_remote", |b| {
        b.iter(|| ctx.for_remote());
    });

    group.bench_function("to_traceparent", |b| {
        b.iter(|| ctx.to_traceparent());
    });

    let traceparent = ctx.to_traceparent();
    group.bench_function("from_traceparent", |b| {
        b.iter(|| Context::from_traceparent(&traceparent, node_id));
    });

    group.finish();
}

/// Benchmark: Baggage operations
fn bench_baggage(c: &mut Criterion) {
    let mut group = c.benchmark_group("baggage");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(Baggage::new);
    });

    let mut baggage = Baggage::new();
    for i in 0..10 {
        baggage.set(format!("key_{}", i), format!("value_{}", i));
    }

    group.bench_function("get", |b| {
        b.iter(|| baggage.get("key_5"));
    });

    group.bench_function("set", |b| {
        let mut bg = Baggage::new();
        b.iter(|| {
            bg.set("test_key", "test_value");
        });
    });

    let other = baggage.clone();
    group.bench_function("merge", |b| {
        b.iter(|| {
            let mut bg = Baggage::new();
            bg.merge(&other);
            bg
        });
    });

    group.finish();
}

/// Benchmark: Span operations
fn bench_span(c: &mut Criterion) {
    let mut group = c.benchmark_group("span");
    group.throughput(Throughput::Elements(1));

    let trace_id = TraceId::generate();
    let node_id = sample_node_id();

    group.bench_function("create", |b| {
        b.iter(|| Span::new(trace_id, "test_operation", node_id));
    });

    let mut span = Span::new(trace_id, "test_operation", node_id);

    group.bench_function("set_attribute", |b| {
        b.iter(|| {
            span.set_attribute("key", "value");
        });
    });

    group.bench_function("add_event", |b| {
        b.iter(|| {
            span.add_event("event_occurred");
        });
    });

    group.bench_function("with_kind", |b| {
        b.iter(|| Span::new(trace_id, "op", node_id).with_kind(SpanKind::Server));
    });

    group.finish();
}

/// Benchmark: ContextStore operations
fn bench_context_store(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_store");
    group.throughput(Throughput::Elements(1));

    let node_id = sample_node_id();
    let store = ContextStore::new(10000, 1000, std::time::Duration::from_secs(60))
        .with_sampling(SamplingStrategy::AlwaysOn);

    group.bench_function("create_context", |b| {
        b.iter(|| {
            // Reclaim each context's slot immediately. With `AlwaysOn`
            // sampling every create commits a `max_traces` slot, so a
            // long criterion run would otherwise fill the store to
            // capacity and every subsequent call (here and at the
            // `unwrap` below) would return `CapacityExceeded`.
            let ctx = store.create_context(node_id).expect("capacity available");
            store.complete_trace(&ctx.trace_id);
        });
    });

    // Pre-create a context for the read/append benches below. The
    // create loop above reclaims its slots, so the store has room.
    let ctx = store.create_context(node_id).unwrap();

    group.bench_function("get_context", |b| {
        b.iter(|| store.get_context(&ctx.trace_id));
    });

    group.bench_function("add_span", |b| {
        let trace_id = ctx.trace_id;
        b.iter(|| {
            let mut span = Span::new(trace_id, "operation", node_id);
            span.end();
            let _ = store.add_span(span);
        });
    });

    group.finish();
}

/// Benchmark: PropagationContext serialization
fn bench_propagation_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("propagation_context");
    group.throughput(Throughput::Elements(1));

    let node_id = sample_node_id();
    let mut ctx = Context::new(node_id);
    ctx.baggage.set("user_id", "12345");
    ctx.baggage.set("tenant", "acme_corp");
    ctx.trace_state.insert("vendor".into(), "data".into());

    group.bench_function("from_context", |b| {
        b.iter(|| PropagationContext::from_context(&ctx));
    });

    let prop = PropagationContext::from_context(&ctx);

    group.bench_function("to_context", |b| {
        b.iter(|| prop.to_context(node_id));
    });

    group.finish();
}

/// Benchmark: Context store with concurrent access simulation
fn bench_context_store_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_store_concurrent");

    let store = Arc::new(
        ContextStore::new(100000, 100, std::time::Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOn),
    );
    let node_id = sample_node_id();

    // Pre-populate with contexts
    let mut trace_ids = Vec::new();
    for _ in 0..1000 {
        if let Ok(ctx) = store.create_context(node_id) {
            trace_ids.push(ctx.trace_id);
        }
    }

    group.throughput(Throughput::Elements(1));

    group.bench_function("concurrent_get", |b| {
        let store = Arc::clone(&store);
        let trace_ids = trace_ids.clone();
        b.iter(|| {
            let idx = rand::random_range(0..trace_ids.len());
            store.get_context(&trace_ids[idx])
        });
    });

    group.finish();
}

// Phase 4F: Context benchmarks
criterion_group!(
    context_benches,
    bench_trace_id,
    bench_context_operations,
    bench_baggage,
    bench_span,
    bench_context_store,
    bench_propagation_context,
    bench_context_store_concurrent,
);

// ============================================================================
// Phase 4G: Load Balancing Benchmarks
// ============================================================================

fn make_lb_node_id(n: u8) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0] = n;
    id
}

/// Benchmark: Endpoint creation
fn bench_endpoint(c: &mut Criterion) {
    let mut group = c.benchmark_group("endpoint");
    group.throughput(Throughput::Elements(1));

    group.bench_function("create", |b| {
        b.iter(|| Endpoint::new(make_lb_node_id(1)));
    });

    group.bench_function("create_with_config", |b| {
        b.iter(|| {
            Endpoint::new(make_lb_node_id(1))
                .with_weight(200)
                .with_zone("us-east-1")
                .with_tag("gpu")
                .with_priority(1)
        });
    });

    let endpoint = Endpoint::new(make_lb_node_id(1)).with_weight(150);
    group.bench_function("effective_weight", |b| {
        b.iter(|| endpoint.effective_weight());
    });

    group.finish();
}

/// Benchmark: Load metrics
fn bench_load_metrics(c: &mut Criterion) {
    let mut group = c.benchmark_group("load_metrics");
    group.throughput(Throughput::Elements(1));

    let metrics = LoadMetrics {
        cpu_usage: 0.65,
        memory_usage: 0.45,
        active_connections: 500,
        requests_per_second: 2500.0,
        avg_response_time_ms: 25.0,
        error_rate: 0.01,
        queue_depth: 50,
        bandwidth_usage: 0.3,
        updated_at: 0,
    };

    group.bench_function("load_score", |b| {
        b.iter(|| metrics.load_score());
    });

    group.bench_function("is_overloaded", |b| {
        b.iter(|| metrics.is_overloaded());
    });

    group.finish();
}

/// Benchmark: Load balancer strategies
fn bench_lb_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb_strategies");
    group.throughput(Throughput::Elements(1));

    // Setup: Create load balancer with 10 endpoints
    fn setup_lb(strategy: Strategy) -> LoadBalancer {
        let lb = LoadBalancer::with_strategy(strategy);
        for i in 0..10 {
            lb.add_endpoint(
                Endpoint::new(make_lb_node_id(i))
                    .with_weight(100 + (i as u32 * 10))
                    .with_zone(if i < 5 { "us-east" } else { "us-west" }),
            );
        }
        lb
    }

    let ctx = LbRequestContext::new();

    // Round Robin
    let lb_rr = setup_lb(Strategy::RoundRobin);
    group.bench_function("round_robin", |b| {
        b.iter(|| {
            let sel = lb_rr.select(&ctx).unwrap();
            lb_rr.record_completion(&sel.node_id, true);
        });
    });

    // Weighted Round Robin
    let lb_wrr = setup_lb(Strategy::WeightedRoundRobin);
    group.bench_function("weighted_round_robin", |b| {
        b.iter(|| {
            let sel = lb_wrr.select(&ctx).unwrap();
            lb_wrr.record_completion(&sel.node_id, true);
        });
    });

    // Least Connections
    let lb_lc = setup_lb(Strategy::LeastConnections);
    group.bench_function("least_connections", |b| {
        b.iter(|| {
            let sel = lb_lc.select(&ctx).unwrap();
            lb_lc.record_completion(&sel.node_id, true);
        });
    });

    // Random
    let lb_rand = setup_lb(Strategy::Random);
    group.bench_function("random", |b| {
        b.iter(|| {
            let sel = lb_rand.select(&ctx).unwrap();
            lb_rand.record_completion(&sel.node_id, true);
        });
    });

    // Power of Two
    let lb_p2 = setup_lb(Strategy::PowerOfTwo);
    group.bench_function("power_of_two", |b| {
        b.iter(|| {
            let sel = lb_p2.select(&ctx).unwrap();
            lb_p2.record_completion(&sel.node_id, true);
        });
    });

    // Consistent Hash
    let lb_ch = setup_lb(Strategy::ConsistentHash);
    let ctx_session = LbRequestContext::new().with_session("user-12345");
    group.bench_function("consistent_hash", |b| {
        b.iter(|| {
            let sel = lb_ch.select(&ctx_session).unwrap();
            lb_ch.record_completion(&sel.node_id, true);
        });
    });

    // Least Load
    let lb_ll = setup_lb(Strategy::LeastLoad);
    group.bench_function("least_load", |b| {
        b.iter(|| {
            let sel = lb_ll.select(&ctx).unwrap();
            lb_ll.record_completion(&sel.node_id, true);
        });
    });

    group.finish();
}

/// Benchmark: Load balancer scaling
fn bench_lb_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb_scaling");

    for endpoint_count in [10, 50, 100, 500].iter() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);
        for i in 0..*endpoint_count {
            lb.add_endpoint(Endpoint::new(make_lb_node_id(i as u8)));
        }

        let ctx = LbRequestContext::new();

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("select", endpoint_count),
            endpoint_count,
            |b, _| {
                b.iter(|| {
                    let sel = lb.select(&ctx).unwrap();
                    lb.record_completion(&sel.node_id, true);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Zone-aware routing
fn bench_lb_zone_aware(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb_zone_aware");
    group.throughput(Throughput::Elements(1));

    let config = LoadBalancerConfig {
        strategy: Strategy::RoundRobin,
        zone_aware: true,
        ..Default::default()
    };
    let lb = LoadBalancer::new(config);

    for i in 0..10 {
        lb.add_endpoint(Endpoint::new(make_lb_node_id(i)).with_zone(if i < 5 {
            "us-east"
        } else {
            "us-west"
        }));
    }

    let ctx_east = LbRequestContext::new().with_zone("us-east");
    let _ctx_west = LbRequestContext::new().with_zone("us-west");
    let ctx_none = LbRequestContext::new();

    group.bench_function("zone_match", |b| {
        b.iter(|| {
            let sel = lb.select(&ctx_east).unwrap();
            lb.record_completion(&sel.node_id, true);
        });
    });

    group.bench_function("zone_fallback", |b| {
        b.iter(|| {
            let sel = lb.select(&ctx_none).unwrap();
            lb.record_completion(&sel.node_id, true);
        });
    });

    group.finish();
}

/// Benchmark: Health updates
fn bench_lb_health_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb_health_updates");
    group.throughput(Throughput::Elements(1));

    let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);
    for i in 0..10 {
        lb.add_endpoint(Endpoint::new(make_lb_node_id(i)));
    }

    group.bench_function("update_health", |b| {
        let mut i = 0u8;
        b.iter(|| {
            lb.update_health(&make_lb_node_id(i % 10), HealthStatus::Healthy);
            i = i.wrapping_add(1);
        });
    });

    let metrics = LoadMetrics {
        cpu_usage: 0.5,
        memory_usage: 0.4,
        active_connections: 100,
        ..Default::default()
    };

    group.bench_function("update_metrics", |b| {
        let mut i = 0u8;
        b.iter(|| {
            lb.update_metrics(&make_lb_node_id(i % 10), metrics.clone());
            i = i.wrapping_add(1);
        });
    });

    group.finish();
}

// Phase 4G: Load balancing benchmarks
criterion_group!(
    loadbalance_benches,
    bench_endpoint,
    bench_load_metrics,
    bench_lb_strategies,
    bench_lb_scaling,
    bench_lb_zone_aware,
    bench_lb_health_updates,
);

// Phase 4E: Rules benchmarks
criterion_group!(
    rules_benches,
    bench_compare_op,
    bench_condition,
    bench_condition_expr,
    bench_rule,
    bench_rule_context,
    bench_rule_engine_basic,
    bench_rule_engine_evaluate,
    bench_rule_engine_scaling,
    bench_rule_set,
);

criterion_main!(
    benches,
    concurrency_benches,
    router_benches,
    multihop_benches,
    swarm_benches,
    failure_benches,
    capability_benches,
    metadata_benches,
    api_benches,
    rules_benches,
    context_benches,
    loadbalance_benches
);
