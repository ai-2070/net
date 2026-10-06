//! Capability-fold scale report: counts, bytes, hit rates, tails.
//!
//! Slice 0 of CAPABILITY_FOLD_SCALE_PLAN.md, the half of the baseline
//! that Criterion cannot produce. Four sections, each printed as a
//! table:
//!
//! - `sweep`: how many entries the expiry sweep's candidate walks yield
//!   (from `FoldMetrics::sweep_yielded`) and how many walks it makes,
//!   against the entry table's capacity, for a steady sweep, a 10% mass
//!   expiry and a whole-fleet expiry.
//! - `footprint`: retained and cumulative heap bytes per entry, from a
//!   counting global allocator, split into payload heap, primary table,
//!   reverse index and secondary index. Three configurations: the
//!   fixture's tag repetition, a mixed fixture where ~20% of each
//!   node's tags are unique to it, and a churn run.
//! - `cache`: `CapabilitySetCache` hits, coherence (stale) misses and
//!   capacity misses, for a hot set below and above the default
//!   capacity, with and without fleet-wide announcement load.
//! - `mixed`: one concurrent workload at 1M residency. Refreshes at the
//!   operating point's rate, selective and broad queries, a mass expiry
//!   partway through, and the 500 ms sweeper, with latency histograms.
//!
//! Run: `cargo bench --features "net fixtures" --bench fold_scale_report`
//! (all sections), or name sections after `--`, e.g.
//! `-- footprint cache`. `sweep` and `mixed` build 1M-entry folds and
//! need several GB of RAM.
//!
//! This is a custom-main target (`harness = false`) so it can install a
//! counting global allocator without slowing the Criterion timings in
//! `fold_scale`, and without installing one into the library's unit-test
//! binary, where every test would pay for it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use net::adapter::net::behavior::fold::capability_bridge::{
    self, CapabilitySetCache, CapabilitySetCacheStats,
};
use net::adapter::net::behavior::fold::{
    CapabilityFold, CapabilityMembership, Fold, FoldEntry, FoldKind, NodeId,
};
use net::adapter::net::behavior::CapabilityFilter;

#[path = "fold_scale_fixture/mod.rs"]
mod fold_scale_fixture;
use fold_scale_fixture::*;

// ---------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------

struct Counting;

/// Bytes currently allocated (allocations minus deallocations).
static LIVE: AtomicI64 = AtomicI64::new(0);
/// Bytes ever allocated.
static CUMULATIVE: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` with the caller's own
// arguments, so `System`'s guarantees carry over unchanged; the counters
// are only bookkeeping beside the forwarded call.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        CUMULATIVE.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarded verbatim; `ptr` came from this allocator,
        // which is `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        CUMULATIVE.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: forwarded verbatim; `ptr` came from this allocator,
        // which is `System`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn live_bytes() -> i64 {
    LIVE.load(Ordering::Relaxed)
}

fn cumulative_bytes() -> u64 {
    CUMULATIVE.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type Key = <CapabilityFold as FoldKind>::Key;

/// Heap bytes of a std `HashMap`/`HashSet` table holding `T` slots, from
/// its reported capacity. std's table is hashbrown's: buckets are a power
/// of two, usable capacity is 7/8 of buckets from 8 buckets up (buckets - 1
/// below), and the allocation is one slot array plus one control byte per
/// bucket plus a 16-byte trailing group. An estimate from capacity, not a
/// measurement; the counting allocator gives the measured totals.
fn table_bytes<T>(capacity: usize) -> u64 {
    if capacity == 0 {
        return 0;
    }
    let buckets = if capacity < 8 {
        (capacity + 1).next_power_of_two()
    } else {
        (capacity * 8 / 7).next_power_of_two()
    };
    let slots = (buckets * size_of::<T>()).next_multiple_of(16);
    (slots + buckets + 16) as u64
}

fn per_entry(bytes: i64, n: u64) -> f64 {
    bytes as f64 / n as f64
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Run `op` at `rate` per second for `duration`, in catch-up bursts on a
/// 1 ms tick (Windows sleep granularity rules out sleeping per op at
/// thousands per second). Returns the number of ops run.
fn paced(rate: f64, duration: Duration, stop: &AtomicBool, mut op: impl FnMut()) -> u64 {
    let start = Instant::now();
    let mut done = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let elapsed = start.elapsed();
        if elapsed >= duration {
            break;
        }
        let owed = (elapsed.as_secs_f64() * rate) as u64;
        while done < owed {
            op();
            done += 1;
        }
        thread::sleep(Duration::from_millis(1));
    }
    done
}

fn quantiles_us(h: &Histogram<u64>) -> String {
    format!(
        "{:.1} | {:.1} | {:.1} | {:.1}",
        h.value_at_quantile(0.50) as f64 / 1e3,
        h.value_at_quantile(0.99) as f64 / 1e3,
        h.value_at_quantile(0.999) as f64 / 1e3,
        h.max() as f64 / 1e3,
    )
}

fn histogram() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("histogram bounds")
}

// ---------------------------------------------------------------------------
// sweep
// ---------------------------------------------------------------------------

fn section_sweep(templates: &Templates) {
    println!("\n## sweep: candidate-walk visits\n");
    println!(
        "| entries | case | expired | walks | yielded | yielded / entry | table capacity | sweep (ms) | evict same set (ms) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");

    for n in [100_000u64, 1_000_000] {
        let fold = live_fold(templates, n);
        let capacity = fold.with_state(|s| s.entries.capacity());
        let mut generation = 1u64;

        let mut row = |case: &str, expired: &[NodeId]| {
            generation += 1;
            for &node in expired {
                fold.apply(templates.envelope(node, generation, 0, 0))
                    .expect("re-arm apply");
            }
            let walks0 = fold.metrics().sweep_walks();
            let yielded0 = fold.metrics().sweep_yielded();
            let start = Instant::now();
            let reaped = fold.sweep_expired_now();
            let elapsed = start.elapsed();
            assert_eq!(reaped, expired.len(), "{case} at {n}");
            let walks = fold.metrics().sweep_walks() - walks0;
            let yielded = fold.metrics().sweep_yielded() - yielded0;
            // The same removals with no candidate walk: `evict_node`
            // drops each entry through the same `by_node` + index
            // removal the sweep's write pass does. The gap between the
            // two columns is what the walks cost.
            let mut restore = |ttl: u32| {
                generation += 1;
                for &node in expired {
                    fold.apply(templates.envelope(node, generation, ttl, 0))
                        .expect("restore apply");
                }
            };
            restore(LIVE_TTL_SECS);
            let start = Instant::now();
            for &node in expired {
                fold.evict_node(node, "removal-cost probe");
            }
            let evict_elapsed = start.elapsed();
            println!(
                "| {n} | {case} | {} | {walks} | {yielded} | {:.2} | {capacity} | {:.2} | {:.2} |",
                expired.len(),
                yielded as f64 / n as f64,
                ms(elapsed),
                ms(evict_elapsed),
            );
            // Restore residency so the next case starts from `n` live.
            restore(LIVE_TTL_SECS);
        };

        row("steady", &[]);
        row("mass 10%", &every_nth(n, 10));
        row("whole fleet", &every_nth(n, 1));
    }
}

// ---------------------------------------------------------------------------
// footprint
// ---------------------------------------------------------------------------

struct Footprint {
    entries: u64,
    retained: i64,
    cumulative: u64,
    payload_heap: i64,
    primary: u64,
    reverse: u64,
}

/// Measure retained bytes for a fold built from `envelopes`.
fn measure_fold(
    n: u64,
    make: &dyn Fn(
        u64,
    )
        -> net::adapter::net::behavior::fold::SignedAnnouncement<CapabilityMembership>,
) -> (Fold<CapabilityFold>, Footprint) {
    // Payload heap: build the payloads alone and subtract their inline
    // size (which lives inside the primary table once applied).
    let before = live_bytes();
    let payloads: Vec<CapabilityMembership> = (0..n).map(|i| make(i).payload).collect();
    let payload_heap =
        live_bytes() - before - (payloads.capacity() * size_of::<CapabilityMembership>()) as i64;
    drop(payloads);

    let before = live_bytes();
    let cumulative_before = cumulative_bytes();
    let fold = empty_fold();
    for i in 0..n {
        fold.apply(make(i)).expect("footprint apply");
    }
    let retained = live_bytes() - before;
    let cumulative = cumulative_bytes() - cumulative_before;
    let (primary, reverse) = fold_tables(&fold);
    let fp = Footprint {
        entries: n,
        retained,
        cumulative,
        payload_heap,
        primary,
        reverse,
    };
    (fold, fp)
}

/// Estimated table bytes of the primary map and the reverse index
/// (its outer table plus every per-node set).
fn fold_tables(fold: &Fold<CapabilityFold>) -> (u64, u64) {
    fold.with_state(|s| {
        let primary = table_bytes::<(Key, FoldEntry<CapabilityFold>)>(s.entries.capacity());
        let reverse_outer = table_bytes::<(NodeId, HashSet<Key>)>(s.by_node.capacity());
        let reverse_inner: u64 = s
            .by_node
            .values()
            .map(|set| table_bytes::<Key>(set.capacity()))
            .sum();
        (primary, reverse_outer + reverse_inner)
    })
}

fn print_footprint(config: &str, fp: &Footprint) {
    let n = fp.entries;
    let index = fp.retained - fp.payload_heap - fp.primary as i64 - fp.reverse as i64;
    println!(
        "| {config} | {n} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} |",
        per_entry(fp.retained, n),
        per_entry(fp.payload_heap, n),
        per_entry(fp.primary as i64, n),
        per_entry(fp.reverse as i64, n),
        per_entry(index, n),
        fp.cumulative as f64 / n as f64,
    );
}

fn section_footprint(templates: &Templates) {
    const N: u64 = 100_000;
    // ~20% of the fixture's tags, added per node and unique to it.
    const UNIQUE: usize = 7;

    let tags: f64 = (0..TEMPLATE_PERIOD)
        .map(|i| templates.envelope(i, 1, 1, 0).payload.tags.len() as f64)
        .sum::<f64>()
        / TEMPLATE_PERIOD as f64;
    println!("\n## footprint: bytes per entry\n");
    println!(
        "Fixture carries {tags:.1} tags per entry. Primary and reverse are estimated \
         from table capacity; retained and cumulative are measured; index is the \
         remainder (retained − payload heap − primary − reverse). Sizes: \
         CapabilityMembership {} B, FoldEntry {} B, key {} B.\n",
        size_of::<CapabilityMembership>(),
        size_of::<FoldEntry<CapabilityFold>>(),
        size_of::<Key>(),
    );
    println!(
        "| configuration | entries | retained | payload heap | primary | reverse | index | cumulative alloc |"
    );
    println!("|---|---|---|---|---|---|---|---|");

    let (fold, fp) = measure_fold(N, &|i| templates.envelope(node_id(i), 1, LIVE_TTL_SECS, 0));
    print_footprint("fixture repetition", &fp);

    // Churn: three rounds, each replacing 20% of publishers with new
    // node ids. Retained bytes after churn show what departed publishers
    // leave behind (today: table capacity; with an interner: its
    // dictionary).
    let before = live_bytes() - fp.retained;
    let cumulative_before = cumulative_bytes();
    let mut next_id = N;
    let mut resident: Vec<NodeId> = (0..N).map(node_id).collect();
    for round in 0..3u64 {
        for slot in (round as usize..resident.len()).step_by(5) {
            fold.evict_node(resident[slot], "churn");
            let fresh = node_id(next_id);
            next_id += 1;
            fold.apply(templates.envelope(fresh, 1, LIVE_TTL_SECS, 0))
                .expect("churn apply");
            resident[slot] = fresh;
        }
    }
    let (primary, reverse) = fold_tables(&fold);
    let churned = Footprint {
        entries: N,
        retained: live_bytes() - before,
        cumulative: fp.cumulative + (cumulative_bytes() - cumulative_before),
        payload_heap: fp.payload_heap,
        primary,
        reverse,
    };
    print_footprint("after 3× 20% churn", &churned);
    drop(fold);

    let (fold, fp) = measure_fold(N, &|i| {
        templates.envelope_with_unique_tags(node_id(i), 1, UNIQUE)
    });
    print_footprint(&format!("mixed: +{UNIQUE} unique tags"), &fp);
    drop(fold);
}

// ---------------------------------------------------------------------------
// cache
// ---------------------------------------------------------------------------

fn section_cache(templates: &Templates) {
    const PUBLISHERS: u64 = 10_000;
    const LOOKUP_RATE: f64 = 100_000.0;
    // The operating point's fleet-wide apply rate: 1M publishers at a
    // 150 s re-announce interval. Coherence damage depends on the
    // fleet-wide rate (the cache keys on one fold-wide generation), so
    // the rate is applied over the smaller fixture fleet.
    const OPERATING_APPLY_RATE: f64 = 1_000_000.0 / 150.0;
    const RUN: Duration = Duration::from_secs(5);

    println!("\n## cache: CapabilitySetCache outcomes\n");
    println!(
        "{PUBLISHERS} resident publishers, default cache capacity (256), uniform lookups \
         over the hot set at {LOOKUP_RATE:.0}/s for {}s. Capacity misses = absent \
         misses − distinct nodes looked up.\n",
        RUN.as_secs()
    );
    println!(
        "| hot set | fleet apply rate | lookups | hits | coherence misses | capacity misses | first-lookup misses | hit rate |"
    );
    println!("|---|---|---|---|---|---|---|---|");

    let fold = Arc::new(live_fold(templates, PUBLISHERS));
    let generations = Arc::new(Mutex::new(vec![1u64; PUBLISHERS as usize]));

    for hot in [200u64, 1000] {
        for apply_rate in [0.0, OPERATING_APPLY_RATE] {
            let cache = CapabilitySetCache::new();
            let stop = Arc::new(AtomicBool::new(false));

            let mutator = {
                let fold = Arc::clone(&fold);
                let generations = Arc::clone(&generations);
                let stop = Arc::clone(&stop);
                let templates_anns = Templates::new();
                thread::spawn(move || {
                    if apply_rate == 0.0 {
                        return 0;
                    }
                    let mut rng = Rng::new(7);
                    paced(apply_rate, RUN + Duration::from_secs(1), &stop, || {
                        let i = rng.below(PUBLISHERS);
                        let generation = {
                            let mut g = generations.lock();
                            g[i as usize] += 1;
                            g[i as usize]
                        };
                        fold.apply(templates_anns.envelope(
                            node_id(i),
                            generation,
                            LIVE_TTL_SECS,
                            0,
                        ))
                        .expect("cache-load apply");
                    })
                })
            };

            let mut rng = Rng::new(11);
            let mut seen = HashSet::new();
            let lookups = paced(LOOKUP_RATE, RUN, &AtomicBool::new(false), || {
                let node = node_id(rng.below(hot));
                seen.insert(node);
                std::hint::black_box(cache.get_or_synthesize(&fold, node));
            });
            stop.store(true, Ordering::Relaxed);
            mutator.join().expect("mutator thread");

            let CapabilitySetCacheStats {
                hits,
                stale_misses,
                absent_misses,
            } = cache.stats();
            let distinct = seen.len() as u64;
            println!(
                "| {hot} | {apply_rate:.0}/s | {lookups} | {hits} | {stale_misses} | {} | {distinct} | {:.1}% |",
                absent_misses.saturating_sub(distinct),
                100.0 * hits as f64 / lookups.max(1) as f64,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// mixed
// ---------------------------------------------------------------------------

fn section_mixed(templates: &Templates) {
    const N: u64 = 1_000_000;
    const APPLY_RATE: f64 = 1_000_000.0 / 150.0;
    const SELECTIVE_RATE: f64 = 1_000.0;
    const BROAD_RATE: f64 = 2.0;
    const RUN: Duration = Duration::from_secs(20);
    // Indices [0, SELECTIVE) carry the variant tag (the selective
    // query's 100 matches); [SELECTIVE, SELECTIVE + EXPIRING) expire
    // together EXPIRE_AFTER into the run; the rest are refreshed.
    const SELECTIVE: u64 = 100;
    const EXPIRING: u64 = 10_000;
    const EXPIRE_AFTER_SECS: u32 = 10;

    println!("\n## mixed: concurrent workload at 1M residency\n");

    let fold = empty_fold();
    for i in 0..N {
        let (ttl, variant) = if i < SELECTIVE {
            (LIVE_TTL_SECS, 1)
        } else if i < SELECTIVE + EXPIRING {
            (EXPIRE_AFTER_SECS, 0)
        } else {
            (LIVE_TTL_SECS, 0)
        };
        fold.apply(templates.envelope(node_id(i), 1, ttl, variant))
            .expect("mixed fixture apply");
    }
    let fold = Arc::new(fold);
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    let applier = {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let templates = Templates::new();
            let mut generations: HashMap<NodeId, u64> = HashMap::new();
            let mut rng = Rng::new(3);
            let mut h = histogram();
            let ops = paced(APPLY_RATE, RUN, &stop, || {
                let i = SELECTIVE + EXPIRING + rng.below(N - SELECTIVE - EXPIRING);
                let node = node_id(i);
                let g = generations.entry(node).or_insert(1);
                *g += 1;
                let ann = templates.envelope(node, *g, LIVE_TTL_SECS, 0);
                let t = Instant::now();
                fold.apply(ann).expect("mixed apply");
                h.saturating_record(t.elapsed().as_nanos() as u64);
            });
            (ops, h)
        })
    };

    let query_thread = |rate: f64, filter: CapabilityFilter, expect: Option<usize>| {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut h = histogram();
            let ops = paced(rate, RUN, &stop, || {
                let t = Instant::now();
                let found = capability_bridge::find_nodes_matching(&fold, &filter);
                h.saturating_record(t.elapsed().as_nanos() as u64);
                if let Some(expected) = expect {
                    assert_eq!(found.len(), expected, "selective query result size");
                }
            });
            (ops, h)
        })
    };
    let selective = query_thread(
        SELECTIVE_RATE,
        CapabilityFilter::new().require_tag("bench-variant-b"),
        Some(SELECTIVE as usize),
    );
    let broad = query_thread(
        BROAD_RATE,
        CapabilityFilter::new().require_tag("inference"),
        None,
    );

    let sweeper = {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut h = histogram();
            let mut reaped_total = 0usize;
            let mut worst = (Duration::ZERO, 0usize);
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                let reaped = fold.sweep_expired_now();
                let d = t.elapsed();
                h.saturating_record(d.as_nanos() as u64);
                reaped_total += reaped;
                if d > worst.0 {
                    worst = (d, reaped);
                }
                thread::sleep(Duration::from_millis(500));
            }
            (h, reaped_total, worst)
        })
    };

    let (applies, apply_h) = applier.join().expect("applier");
    let (sel_ops, sel_h) = selective.join().expect("selective");
    let (broad_ops, broad_h) = broad.join().expect("broad");
    stop.store(true, Ordering::Relaxed);
    let (sweep_h, reaped, worst) = sweeper.join().expect("sweeper");
    let elapsed = start.elapsed();

    println!(
        "{N} resident, {:.1}s run. Pacing is catch-up bursts on a 1 ms tick, and \
         latency is measured from each op's start, so queueing behind a burst is \
         not counted. Sweep time is the full `sweep_expired_now` call (read walks \
         plus write-locked eviction chunks), the closest available proxy for the \
         sweeper's lock holds.\n",
        elapsed.as_secs_f64()
    );
    println!("| stream | target rate | achieved | p50 µs | p99 µs | p99.9 µs | max µs |");
    println!("|---|---|---|---|---|---|---|");
    let secs = RUN.as_secs_f64();
    println!(
        "| refresh apply | {APPLY_RATE:.0}/s | {:.0}/s | {} |",
        applies as f64 / secs,
        quantiles_us(&apply_h)
    );
    println!(
        "| selective query (100 matches) | {SELECTIVE_RATE:.0}/s | {:.0}/s | {} |",
        sel_ops as f64 / secs,
        quantiles_us(&sel_h)
    );
    println!(
        "| broad query (~{} matches) | {BROAD_RATE:.0}/s | {:.1}/s | {} |",
        N / 2,
        broad_ops as f64 / secs,
        quantiles_us(&broad_h)
    );
    println!(
        "| sweep tick | 2/s | {:.1}/s | {} |",
        sweep_h.len() as f64 / elapsed.as_secs_f64(),
        quantiles_us(&sweep_h)
    );
    println!(
        "\nSweeps reaped {reaped} entries (expected {EXPIRING}); slowest tick {:.1} ms \
         reaped {}.",
        ms(worst.0),
        worst.1
    );
}

// ---------------------------------------------------------------------------

fn main() {
    let requested: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let wants = |name: &str| requested.is_empty() || requested.iter().any(|r| r == name);

    println!("# Capability fold scale report");
    let templates = Templates::new();

    if wants("footprint") {
        section_footprint(&templates);
    }
    if wants("cache") {
        section_cache(&templates);
    }
    if wants("sweep") {
        section_sweep(&templates);
    }
    if wants("mixed") {
        section_mixed(&templates);
    }
}
