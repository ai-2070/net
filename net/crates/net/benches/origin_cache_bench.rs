//! Throwaway diagnostic: before/after for the per-`serve_rpc` caller-keyed
//! cache swap (audit §8b cache + cubic P2 bounding).
//!
//! BEFORE: `dashmap::DashMap<u64, ChannelName>` — unbounded, concurrent.
//! AFTER:  `parking_lot::Mutex<lru::LruCache<u64, ChannelName>>` — bounded.
//!
//! The value is `Arc<str>` (exactly what `ChannelName` is), so the clone cost
//! — an Arc bump — is identical to production; this isolates the *container*
//! op cost the swap actually changed. Two hot ops:
//!   * `hit`    — the per-response common path (resolve a warm caller).
//!   * `insert` — first time a given origin is seen.
//!
//! Run: `cargo bench --bench origin_cache_bench --features net`

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use dashmap::DashMap;
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::Arc;

const CAP: usize = 4096;
/// Warm working set primed before the hit benchmark.
const WARM: u64 = 256;

fn sample_value() -> Arc<str> {
    Arc::from("example-service.replies.0123456789abcdef")
}

fn bench_origin_cache(c: &mut Criterion) {
    let v = sample_value();

    // ── cache HIT: the per-response common path ──────────────────────────
    {
        let mut g = c.benchmark_group("origin_cache_hit");

        let dm: DashMap<u64, Arc<str>> = DashMap::new();
        for k in 0..WARM {
            dm.insert(k, v.clone());
        }
        g.bench_function("dashmap", |b| {
            b.iter(|| black_box(dm.get(&black_box(7u64)).map(|x| x.clone())))
        });

        let lru: Mutex<LruCache<u64, Arc<str>>> =
            Mutex::new(LruCache::new(NonZeroUsize::new(CAP).unwrap()));
        {
            let mut l = lru.lock();
            for k in 0..WARM {
                l.put(k, v.clone());
            }
        }
        g.bench_function("mutex_lru", |b| {
            b.iter(|| black_box(lru.lock().get(&black_box(7u64)).cloned()))
        });
        g.finish();
    }

    // ── cache INSERT: first time an origin is seen (cold) ────────────────
    {
        let mut g = c.benchmark_group("origin_cache_insert_256");
        g.bench_function("dashmap", |b| {
            b.iter_batched(
                DashMap::<u64, Arc<str>>::new,
                |dm| {
                    for k in 0..WARM {
                        dm.insert(black_box(k), v.clone());
                    }
                    black_box(dm)
                },
                BatchSize::SmallInput,
            )
        });
        g.bench_function("mutex_lru", |b| {
            b.iter_batched(
                || Mutex::new(LruCache::<u64, Arc<str>>::new(NonZeroUsize::new(CAP).unwrap())),
                |lru| {
                    {
                        let mut l = lru.lock();
                        for k in 0..WARM {
                            l.put(black_box(k), v.clone());
                        }
                    }
                    black_box(lru)
                },
                BatchSize::SmallInput,
            )
        });
        g.finish();
    }
}

criterion_group!(benches, bench_origin_cache);
criterion_main!(benches);
