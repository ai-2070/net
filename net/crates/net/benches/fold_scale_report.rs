//! Capability-fold scale report: counts, bytes, hit rates, tails.
//!
//! Slice 0 of CAPABILITY_FOLD_SCALE_PLAN.md, the half of the baseline
//! that Criterion cannot produce. Five sections, each printed as a
//! table:
//!
//! - `sweep`: how many entries the expiry sweep's candidate walks yield
//!   (from `FoldMetrics::sweep_yielded`) and how many walks it makes,
//!   against the entry table's capacity, for a steady sweep, a 10% mass
//!   expiry and a whole-fleet expiry. A diagnostic column times removing
//!   the same entries through `evict_node`.
//! - `footprint`: retained and cumulative heap bytes per entry, from a
//!   counting global allocator, split into payload heap, primary table,
//!   reverse index and secondary index. The fixture's tag repetition at
//!   100k and 1M; a churn run and a mixed fixture where ~20% of each
//!   node's tags are unique to it, at 100k.
//! - `cache`: `CapabilitySetCache` hits, coherence (stale) misses and
//!   capacity misses, for a hot set below and above the default
//!   capacity, with and without fleet-wide announcement load.
//! - `mixed`: one concurrent workload at 1M residency. Refreshes at the
//!   operating point's rate, selective and broad queries, a batch of
//!   entries armed at the workload's start to expire ten seconds later,
//!   and a 500 ms sweeper, with per-call service-latency histograms.
//! - `alloc`: allocation calls inside the fold for warm refreshes (gated
//!   at zero), changed-index replaces, evictions, cold inserts and
//!   translations.
//!
//! Every section asserts its outcomes: an apply that should replace must
//! replace, a sweep must reap exactly the armed batch, query results must
//! match the population, so a broken workload fails instead of printing
//! a plausible table.
//!
//! Run: `cargo bench --features "net fixtures" --bench fold_scale_report`
//! (all sections), or name sections after `--`, e.g.
//! `-- footprint cache`. An unknown section name is an error. `sweep`,
//! `mixed` and the 1M footprint row build 1M-entry folds and need several
//! GB of RAM.
//!
//! This is a custom-main target (`harness = false`) so it can install a
//! counting global allocator without slowing the Criterion timings in
//! `fold_scale`, and without installing one into the library's unit-test
//! binary, where every test would pay for it. Counting is switched on only
//! while the `footprint` section runs; every timed section runs with it off.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use net::adapter::net::behavior::fold::capability_bridge::{
    self, CapabilitySetCache, CapabilitySetCacheStats,
};
use net::adapter::net::behavior::fold::{
    ApplyOutcome, CapabilityFold, CapabilityMembership, Fold, FoldEntry, FoldKind, NodeId,
    NodeRecord, SignedAnnouncement,
};
use net::adapter::net::behavior::CapabilityFilter;
use parking_lot::Mutex;

#[path = "fold_scale_fixture/mod.rs"]
mod fold_scale_fixture;
use fold_scale_fixture::*;

const SECTIONS: [&str; 5] = ["footprint", "cache", "sweep", "mixed", "alloc"];

// ---------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------

struct Counting;

/// Whether allocations are being counted. Off except in `footprint`, so
/// the timed sections pay one relaxed load per allocation, not two
/// read-modify-write atomics.
static COUNTING: AtomicBool = AtomicBool::new(false);
/// Bytes currently allocated (allocations minus deallocations) while
/// counting was on.
static LIVE: AtomicI64 = AtomicI64::new(0);
/// Bytes allocated while counting was on.
static CUMULATIVE: AtomicU64 = AtomicU64::new(0);
/// Allocation calls (alloc + realloc) while counting was on.
static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` with the caller's own
// arguments, so `System`'s guarantees carry over unchanged; the counters
// are only bookkeeping beside the forwarded call.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
            CUMULATIVE.fetch_add(layout.size() as u64, Ordering::Relaxed);
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: forwarded verbatim; `ptr` came from this allocator,
        // which is `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
            CUMULATIVE.fetch_add(new_size as u64, Ordering::Relaxed);
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
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
type Envelope = SignedAnnouncement<CapabilityMembership>;

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

/// Below this many samples a p99 or p99.9 is one or two observations, not
/// a tail; those columns print `–` and only p50 and max are reported.
const MIN_TAIL_SAMPLES: u64 = 1000;

/// `samples | p50 | p99 | p99.9 | max`, in microseconds.
fn latency_row(h: &Histogram<u64>) -> String {
    let us = |q: f64| format!("{:.1}", h.value_at_quantile(q) as f64 / 1e3);
    let (p99, p999) = if h.len() >= MIN_TAIL_SAMPLES {
        (us(0.99), us(0.999))
    } else {
        ("–".to_string(), "–".to_string())
    };
    format!(
        "{} | {} | {p99} | {p999} | {:.1}",
        h.len(),
        us(0.50),
        h.max() as f64 / 1e3
    )
}

fn histogram() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("histogram bounds")
}

fn apply_expect(fold: &Fold<CapabilityFold>, ann: Envelope, expected: ApplyOutcome, what: &str) {
    let outcome = fold.apply(ann).expect(what);
    assert_eq!(outcome, expected, "{what}");
}

// ---------------------------------------------------------------------------
// sweep
// ---------------------------------------------------------------------------

fn section_sweep(templates: &Templates) {
    println!("\n## sweep: candidate-walk visits\n");
    println!(
        "Counting allocator off. Walks and yielded are counted by `FoldMetrics`. \
         \"Evict same set\" is a diagnostic: it removes the same entries through \
         `evict_node`, one per-node lock acquisition at a time, in node order, after \
         the sweep's removal and re-insertion. Its gap to the sweep column is not an \
         exact measurement of walk cost.\n"
    );
    println!(
        "| entries | case | expired | walks | yielded | yielded / entry | table capacity | sweep (ms) | evict same set (ms) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");

    for n in [100_000u64, 1_000_000] {
        let fold = live_fold(templates, n);
        let capacity = fold.with_state(|s| s.entries.capacity());
        let mut generation = 1u64;

        let mut row = |case: &str, expired: &[NodeId]| {
            let mut rearm = |ttl: u32| {
                generation += 1;
                for &node in expired {
                    apply_expect(
                        &fold,
                        templates.envelope(node, generation, ttl, 0),
                        ApplyOutcome::Replaced,
                        "re-arm apply",
                    );
                }
            };
            rearm(0);
            let walks0 = fold.metrics().sweep_walks();
            let yielded0 = fold.metrics().sweep_yielded();
            let before = fold.metrics().entries();
            let start = Instant::now();
            let reaped = fold.sweep_expired_now();
            let elapsed = start.elapsed();
            assert_eq!(reaped, expired.len(), "{case} at {n}: reaped");
            assert_eq!(
                fold.metrics().entries(),
                before - expired.len() as u64,
                "{case} at {n}: residency after sweep"
            );
            let walks = fold.metrics().sweep_walks() - walks0;
            let yielded = fold.metrics().sweep_yielded() - yielded0;

            // Put the batch back (as Inserts: the sweep removed it), then
            // remove it again through `evict_node`.
            generation += 1;
            for &node in expired {
                apply_expect(
                    &fold,
                    templates.envelope(node, generation, LIVE_TTL_SECS, 0),
                    ApplyOutcome::Inserted,
                    "restore apply",
                );
            }
            let start = Instant::now();
            for &node in expired {
                fold.evict_node(node, "removal-cost probe");
            }
            let evict_elapsed = start.elapsed();
            assert_eq!(fold.metrics().entries(), before - expired.len() as u64);

            println!(
                "| {n} | {case} | {} | {walks} | {yielded} | {:.2} | {capacity} | {:.2} | {:.2} |",
                expired.len(),
                yielded as f64 / n as f64,
                ms(elapsed),
                ms(evict_elapsed),
            );

            // Restore residency so the next case starts from `n` live.
            generation += 1;
            for &node in expired {
                apply_expect(
                    &fold,
                    templates.envelope(node, generation, LIVE_TTL_SECS, 0),
                    ApplyOutcome::Inserted,
                    "restore apply",
                );
            }
            assert_eq!(fold.metrics().entries(), n);
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

/// Measure retained bytes for a fold built from `make(0..n)`.
fn measure_fold(n: u64, make: &dyn Fn(u64) -> Envelope) -> (Fold<CapabilityFold>, Footprint) {
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
        apply_expect(&fold, make(i), ApplyOutcome::Inserted, "footprint apply");
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
        let reverse_outer = table_bytes::<(NodeId, NodeRecord<Key>)>(s.by_node.capacity());
        // A record's key list is inline for one key and spills to the
        // heap past that; the spilled capacity is not visible, so the
        // estimate uses the length. Every record also owns its shared
        // revision cell: an `Arc<AtomicU64>`, two refcounts plus the
        // value.
        let reverse_inner: u64 = s
            .by_node
            .values()
            .map(|record| record.keys().len())
            .filter(|&len| len > 1)
            .map(|len| (len * size_of::<Key>()) as u64)
            .sum();
        let rev_cells = (s.by_node.len() * 3 * size_of::<u64>()) as u64;
        (primary, reverse_outer + reverse_inner + rev_cells)
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

    COUNTING.store(true, Ordering::Relaxed);

    let tags: f64 = (0..TEMPLATE_PERIOD)
        .map(|i| templates.envelope(i, 1, 1, 0).payload.tags.len() as f64)
        .sum::<f64>()
        / TEMPLATE_PERIOD as f64;
    println!("\n## footprint: bytes per entry\n");
    println!(
        "Fixture carries {tags:.1} tags per entry. Retained, payload heap and \
         cumulative are measured by the counting allocator. Primary and reverse are \
         estimated from table capacity. Index is the remainder (retained − payload \
         heap − primary − reverse), so it absorbs any estimation error. Sizes: \
         CapabilityMembership {} B, FoldEntry {} B, key {} B.\n",
        size_of::<CapabilityMembership>(),
        size_of::<FoldEntry<CapabilityFold>>(),
        size_of::<Key>(),
    );
    println!(
        "| configuration | entries | retained | payload heap | primary (est.) | reverse (est.) | index (remainder) | cumulative alloc |"
    );
    println!("|---|---|---|---|---|---|---|---|");

    for n in [N, 1_000_000] {
        let (fold, fp) = measure_fold(n, &|i| templates.envelope(node_id(i), 1, LIVE_TTL_SECS, 0));
        print_footprint("fixture repetition", &fp);
        if n != N {
            drop(fold);
            continue;
        }

        // Churn: three rounds, each replacing 20% of publishers with new
        // node ids. Retained bytes after churn show what departed
        // publishers leave behind (today: table capacity; with an
        // interner: its dictionary).
        let before = live_bytes() - fp.retained;
        let cumulative_before = cumulative_bytes();
        let mut next_id = N;
        let mut resident: Vec<NodeId> = (0..N).map(node_id).collect();
        for round in 0..3u64 {
            for slot in (round as usize..resident.len()).step_by(5) {
                fold.evict_node(resident[slot], "churn");
                let fresh = node_id(next_id);
                next_id += 1;
                apply_expect(
                    &fold,
                    templates.envelope(fresh, 1, LIVE_TTL_SECS, 0),
                    ApplyOutcome::Inserted,
                    "churn apply",
                );
                resident[slot] = fresh;
            }
        }
        assert_eq!(fold.metrics().entries(), N, "churn keeps residency");
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

        // Translated per entry, NOT cloned from a template: production
        // moves `translate_announcement`'s output straight into the fold,
        // so any spare capacity in its strings stays resident. The
        // template rows above clone payloads, and a clone allocates
        // exactly the length, which hides that slack.
        let (fold, fp) = measure_fold(N, &|i| {
            capability_bridge::translate_announcement(&legacy_announcement(i), None)
        });
        print_footprint("translated per entry (no clone)", &fp);
        drop(fold);
    }

    COUNTING.store(false, Ordering::Relaxed);
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
    // the rate is applied over the smaller fixture fleet. Each publisher
    // therefore refreshes ~100x more often than a real 1M-publisher
    // fleet's would, which matters once validity is per publisher.
    const OPERATING_APPLY_RATE: f64 = 1_000_000.0 / 150.0;
    const RUN: Duration = Duration::from_secs(5);

    println!("\n## cache: CapabilitySetCache outcomes\n");
    println!(
        "Counting allocator off. {PUBLISHERS} resident publishers, default cache \
         capacity (256), uniform lookups over the hot set at {LOOKUP_RATE:.0}/s for \
         {}s. Every load apply is asserted to be an accepted Replace. Capacity misses \
         = absent misses − distinct nodes looked up, which holds because a single \
         thread looks up and nothing clears the cache.\n",
        RUN.as_secs()
    );
    println!(
        "| hot set | announcing publishers | target apply rate | accepted applies/s | lookups | hits | coherence misses | capacity misses | first-lookup misses | hit rate |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");

    let fold = Arc::new(live_fold(templates, PUBLISHERS));
    let generations = Arc::new(Mutex::new(vec![1u64; PUBLISHERS as usize]));

    // Who announces under load: every publisher (the hot set's own
    // refreshes legitimately invalidate it), or only publishers OUTSIDE
    // the hot set. The second isolates coherence misses caused by
    // other publishers, which per-publisher validity must reduce to 0.
    #[derive(Clone, Copy, PartialEq)]
    enum Announcing {
        All,
        OutsideHotSet,
    }
    let configs = [
        (0.0, Announcing::All),
        (OPERATING_APPLY_RATE, Announcing::All),
        (OPERATING_APPLY_RATE, Announcing::OutsideHotSet),
    ];

    for hot in [200u64, 1000] {
        for (apply_rate, announcing) in configs {
            let cache = CapabilitySetCache::new();
            let stop = Arc::new(AtomicBool::new(false));

            let mutator = {
                let fold = Arc::clone(&fold);
                let generations = Arc::clone(&generations);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    if apply_rate == 0.0 {
                        return (0u64, Duration::ZERO);
                    }
                    let templates = Templates::new();
                    let mut rng = Rng::new(7);
                    let start = Instant::now();
                    let accepted = paced(apply_rate, RUN + Duration::from_secs(1), &stop, || {
                        let i = match announcing {
                            Announcing::All => rng.below(PUBLISHERS),
                            Announcing::OutsideHotSet => hot + rng.below(PUBLISHERS - hot),
                        };
                        let generation = {
                            let mut g = generations.lock();
                            g[i as usize] += 1;
                            g[i as usize]
                        };
                        apply_expect(
                            &fold,
                            templates.envelope(node_id(i), generation, LIVE_TTL_SECS, 0),
                            ApplyOutcome::Replaced,
                            "cache-load apply",
                        );
                    });
                    (accepted, start.elapsed())
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
            let (accepted, mutator_elapsed) = mutator.join().expect("mutator thread");
            let achieved = if mutator_elapsed.is_zero() {
                0.0
            } else {
                accepted as f64 / mutator_elapsed.as_secs_f64()
            };

            let CapabilitySetCacheStats {
                hits,
                stale_misses,
                absent_misses,
            } = cache.stats();
            assert_eq!(
                hits + stale_misses + absent_misses,
                lookups,
                "every lookup has exactly one outcome"
            );
            if announcing == Announcing::OutsideHotSet {
                assert_eq!(
                    stale_misses, 0,
                    "announcements outside the hot set invalidated it"
                );
            }
            let distinct = seen.len() as u64;
            let who = match (apply_rate == 0.0, announcing) {
                (true, _) => "none",
                (false, Announcing::All) => "all",
                (false, Announcing::OutsideHotSet) => "outside hot set",
            };
            println!(
                "| {hot} | {who} | {apply_rate:.0}/s | {achieved:.0} | {lookups} | {hits} | {stale_misses} | {} | {distinct} | {:.1}% |",
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
    // query's 100 matches); [SELECTIVE, SELECTIVE + EXPIRING) are armed
    // at the workload's start to expire EXPIRE_AFTER later; the rest are
    // refreshed.
    const SELECTIVE: u64 = 100;
    const EXPIRING: u64 = 10_000;
    const EXPIRE_AFTER: Duration = Duration::from_secs(10);
    const SWEEP_EVERY: Duration = Duration::from_millis(500);

    println!("\n## mixed: concurrent workload at 1M residency\n");

    // 1. The complete live population, every entry on the long TTL.
    let fold = empty_fold();
    for i in 0..N {
        let variant = u8::from(i < SELECTIVE);
        apply_expect(
            &fold,
            templates.envelope(node_id(i), 1, LIVE_TTL_SECS, variant),
            ApplyOutcome::Inserted,
            "mixed fixture apply",
        );
    }
    let expiring: Vec<NodeId> = (SELECTIVE..SELECTIVE + EXPIRING).map(node_id).collect();

    // The broad query's expected populations: every "inference" carrier
    // before the batch expires, and the same minus the batch after.
    // Between the two, a chunked sweep exposes intermediate states, so a
    // result must contain every post-expiry carrier and nothing outside
    // the pre-expiry set.
    let broad_filter = CapabilityFilter::new().require_tag("inference");
    let pre: HashSet<NodeId> = capability_bridge::find_nodes_matching(&fold, &broad_filter)
        .into_iter()
        .collect();
    let expiring_set: HashSet<NodeId> = expiring.iter().copied().collect();
    let post: HashSet<NodeId> = pre.difference(&expiring_set).copied().collect();
    assert!(
        pre.len() > post.len(),
        "the batch includes broad-query carriers"
    );

    let fold = Arc::new(fold);
    let stop = Arc::new(AtomicBool::new(false));
    // Workers + the arming main thread start together.
    let barrier = Arc::new(Barrier::new(5));
    let epoch: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let applier = {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            let templates = Templates::new();
            let mut generations: HashMap<NodeId, u64> = HashMap::new();
            let mut rng = Rng::new(3);
            let mut h = histogram();
            barrier.wait();
            let accepted = paced(APPLY_RATE, RUN, &stop, || {
                let i = SELECTIVE + EXPIRING + rng.below(N - SELECTIVE - EXPIRING);
                let node = node_id(i);
                let g = generations.entry(node).or_insert(1);
                *g += 1;
                let ann = templates.envelope(node, *g, LIVE_TTL_SECS, 0);
                let t = Instant::now();
                let outcome = fold.apply(ann).expect("mixed apply");
                h.saturating_record(t.elapsed().as_nanos() as u64);
                assert_eq!(outcome, ApplyOutcome::Replaced, "mixed refresh");
            });
            (accepted, h)
        })
    };

    let selective = {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            let filter = CapabilityFilter::new().require_tag("bench-variant-b");
            let mut h = histogram();
            barrier.wait();
            let ops = paced(SELECTIVE_RATE, RUN, &stop, || {
                let t = Instant::now();
                let found = capability_bridge::find_nodes_matching(&fold, &filter);
                h.saturating_record(t.elapsed().as_nanos() as u64);
                assert_eq!(found.len(), SELECTIVE as usize, "selective result size");
            });
            (ops, h)
        })
    };

    let broad = {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        let filter = broad_filter.clone();
        thread::spawn(move || {
            let mut h = histogram();
            let mut sizes = (usize::MAX, 0usize);
            barrier.wait();
            let ops = paced(BROAD_RATE, RUN, &stop, || {
                let t = Instant::now();
                let found = capability_bridge::find_nodes_matching(&fold, &filter);
                h.saturating_record(t.elapsed().as_nanos() as u64);
                // Membership check, outside the timed call.
                let mut post_found = 0usize;
                for id in &found {
                    assert!(pre.contains(id), "broad result {id} outside the population");
                    post_found += usize::from(post.contains(id));
                }
                assert_eq!(post_found, post.len(), "broad result missing live carriers");
                sizes = (sizes.0.min(found.len()), sizes.1.max(found.len()));
            });
            (ops, h, sizes, pre.len(), post.len())
        })
    };

    let sweeper = {
        let fold = Arc::clone(&fold);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        let epoch = Arc::clone(&epoch);
        thread::spawn(move || {
            let mut h = histogram();
            // (tick start since epoch, tick duration, reaped) for every
            // tick that reaped anything.
            let mut reaps: Vec<(Duration, Duration, usize)> = Vec::new();
            barrier.wait();
            let start = loop {
                if let Some(e) = *epoch.lock() {
                    break e;
                }
                thread::yield_now();
            };
            while !stop.load(Ordering::Relaxed) {
                let at = start.elapsed();
                let t = Instant::now();
                let reaped = fold.sweep_expired_now();
                let d = t.elapsed();
                h.saturating_record(d.as_nanos() as u64);
                if reaped > 0 {
                    reaps.push((at, d, reaped));
                }
                thread::sleep(SWEEP_EVERY);
            }
            (h, reaps)
        })
    };

    // 2. Common start, then arm the batch relative to it. Each entry's
    //    deadline is its own apply time + EXPIRE_AFTER, so the batch's
    //    deadlines fall in [EXPIRE_AFTER, EXPIRE_AFTER + arm duration).
    barrier.wait();
    let start = Instant::now();
    *epoch.lock() = Some(start);
    for &node in &expiring {
        apply_expect(
            &fold,
            templates.envelope(node, 2, EXPIRE_AFTER.as_secs() as u32, 0),
            ApplyOutcome::Replaced,
            "arm expiring batch",
        );
    }
    let armed_by = start.elapsed();

    let (applies, apply_h) = applier.join().expect("applier");
    let (sel_ops, sel_h) = selective.join().expect("selective");
    let (broad_ops, broad_h, broad_sizes, pre_len, post_len) = broad.join().expect("broad");
    stop.store(true, Ordering::Relaxed);
    let (sweep_h, reaps) = sweeper.join().expect("sweeper");
    let elapsed = start.elapsed();

    // 3. The expiry event happened in the run, at the boundary, and
    //    removed exactly the batch.
    let reaped: usize = reaps.iter().map(|r| r.2).sum();
    assert_eq!(
        reaped, EXPIRING as usize,
        "sweeps reap exactly the armed batch"
    );
    let first = reaps.first().expect("an expiry event in the run");
    let last = reaps.last().expect("an expiry event in the run");
    // Every batch deadline is >= start + EXPIRE_AFTER, so no tick that
    // ended before the boundary can have reaped (tick end, not start:
    // the sweep reads its own `now` after the tick's start stamp).
    assert!(
        first.0 + first.1 >= EXPIRE_AFTER,
        "a sweep reaped in a tick ending at {:?}, before the {EXPIRE_AFTER:?} boundary",
        first.0 + first.1
    );
    assert!(
        last.0 + last.1 < RUN,
        "the batch's removal finished inside the run"
    );
    assert_eq!(fold.metrics().entries(), N - EXPIRING, "final residency");
    fold.with_state(|s| {
        for node in &expiring {
            assert!(
                !s.by_node.contains_key(node),
                "batch node {node} still resident"
            );
        }
    });
    assert_eq!(fold.with_state(|s| s.entries.len()) as u64, N - EXPIRING);

    let secs = RUN.as_secs_f64();
    println!(
        "{N} resident, {:.1}s run from a common start. Counting allocator off. The \
         {EXPIRING}-entry batch was armed between 0 and {:.0} ms with a {}s TTL. \
         Latency is per-call service time from each op's start; pacing is catch-up \
         bursts on a 1 ms tick, so time queued behind a burst is not counted \
         (no coordinated-omission correction). Streams with fewer than \
         {MIN_TAIL_SAMPLES} samples report only p50 and max. Sweep tick is the whole \
         `sweep_expired_now` call (read walks plus every write-locked eviction chunk), \
         not one lock hold.\n",
        elapsed.as_secs_f64(),
        ms(armed_by),
        EXPIRE_AFTER.as_secs(),
    );
    println!("| stream | target rate | achieved | samples | p50 µs | p99 µs | p99.9 µs | max µs |");
    println!("|---|---|---|---|---|---|---|---|");
    println!(
        "| refresh apply (accepted Replace) | {APPLY_RATE:.0}/s | {:.0}/s | {} |",
        applies as f64 / secs,
        latency_row(&apply_h)
    );
    println!(
        "| selective query (100 matches) | {SELECTIVE_RATE:.0}/s | {:.0}/s | {} |",
        sel_ops as f64 / secs,
        latency_row(&sel_h)
    );
    println!(
        "| broad query ({}–{} matches) | {BROAD_RATE:.0}/s | {:.1}/s | {} |",
        broad_sizes.0,
        broad_sizes.1,
        broad_ops as f64 / secs,
        latency_row(&broad_h)
    );
    println!(
        "| sweep tick | 2/s | {:.1}/s | {} |",
        sweep_h.len() as f64 / elapsed.as_secs_f64(),
        latency_row(&sweep_h)
    );
    println!(
        "\nBroad query population: {pre_len} carriers before the batch expired, \
         {post_len} after; every result was checked against both."
    );
    println!("\nExpiry events (ticks that reaped anything):\n");
    println!("| tick at (s) | tick duration (ms) | reaped |");
    println!("|---|---|---|");
    for (at, d, n) in &reaps {
        println!("| {:.2} | {:.1} | {n} |", at.as_secs_f64(), ms(*d));
    }
}

// ---------------------------------------------------------------------------
// alloc
// ---------------------------------------------------------------------------

/// Allocation calls made by `op`, counted only while it runs.
fn count_allocs<R>(op: impl FnOnce() -> R) -> (R, u64) {
    let before = ALLOC_CALLS.load(Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let out = op();
    COUNTING.store(false, Ordering::Relaxed);
    (out, ALLOC_CALLS.load(Ordering::Relaxed) - before)
}

fn section_alloc(templates: &Templates) {
    const N: u64 = 10_000;
    const OPS: u64 = 1_000;

    println!("\n## alloc: allocation calls inside the fold\n");
    println!(
        "Allocation calls (alloc + realloc) counted only around the measured \
         calls; every envelope is built before counting starts. The warm \
         index-equivalent refresh is the Slice 3 gate and is asserted to be 0. \
         The other rows are reported, not gated: a changed index, a cold insert \
         and a translate all allocate by definition.\n"
    );
    println!("| operation | calls | allocations | per call |");
    println!("|---|---|---|---|");

    let fold = live_fold(templates, N);
    let targets: Vec<NodeId> = (0..OPS).map(|i| node_id(i * (N / OPS))).collect();
    let row = |what: &str, allocs: u64| {
        println!(
            "| {what} | {OPS} | {allocs} | {:.2} |",
            allocs as f64 / OPS as f64
        );
    };

    // Warm, index-equivalent refresh: resident entries, same payload,
    // newer generation, no table growth. Warm up first so any one-time
    // growth (the cache-free index buckets already exist) is excluded.
    let warmup: Vec<_> = targets
        .iter()
        .map(|&n| templates.envelope(n, 2, LIVE_TTL_SECS, 0))
        .collect();
    for ann in warmup {
        apply_expect(&fold, ann, ApplyOutcome::Replaced, "warm-up refresh");
    }
    let refresh: Vec<_> = targets
        .iter()
        .map(|&n| templates.envelope(n, 3, LIVE_TTL_SECS, 0))
        .collect();
    let ((), allocs) = count_allocs(|| {
        for ann in refresh {
            apply_expect(&fold, ann, ApplyOutcome::Replaced, "warm refresh");
        }
    });
    row("refresh, index-equivalent (warm)", allocs);
    assert_eq!(allocs, 0, "a warm index-equivalent refresh allocated");

    // Index-changing replace: variant 1 adds one tag, so every apply
    // re-indexes. Its bucket exists after the first, but `on_insert`
    // still writes ~31 set memberships, which may grow a set.
    let changed: Vec<_> = targets
        .iter()
        .map(|&n| templates.envelope(n, 4, LIVE_TTL_SECS, 1))
        .collect();
    let ((), allocs) = count_allocs(|| {
        for ann in changed {
            apply_expect(&fold, ann, ApplyOutcome::Replaced, "changed replace");
        }
    });
    row("replace, index changed", allocs);

    // Removal through `evict_node`: frees the entry, allocates nothing
    // it does not have to.
    let ((), allocs) = count_allocs(|| {
        for &n in &targets {
            fold.evict_node(n, "alloc probe");
        }
    });
    row("evict (removal path)", allocs);

    // Cold insert of the evicted entries back.
    let inserts: Vec<_> = targets
        .iter()
        .map(|&n| templates.envelope(n, 5, LIVE_TTL_SECS, 0))
        .collect();
    let ((), allocs) = count_allocs(|| {
        for ann in inserts {
            apply_expect(&fold, ann, ApplyOutcome::Inserted, "cold insert");
        }
    });
    row("insert (cold)", allocs);

    // Translate: builds the owned payload, so it allocates by design.
    let anns: Vec<_> = (0..OPS).map(legacy_announcement).collect();
    let (translated, allocs) = count_allocs(|| {
        anns.iter()
            .map(|a| capability_bridge::translate_announcement(a, None))
            .collect::<Vec<_>>()
    });
    drop(translated);
    row("translate_announcement", allocs);
}

// ---------------------------------------------------------------------------

fn main() {
    let requested: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let unknown: Vec<&String> = requested
        .iter()
        .filter(|r| !SECTIONS.contains(&r.as_str()))
        .collect();
    if !unknown.is_empty() {
        eprintln!("unknown section(s) {unknown:?}; expected any of {SECTIONS:?}");
        std::process::exit(2);
    }
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
    if wants("alloc") {
        section_alloc(&templates);
    }
}
