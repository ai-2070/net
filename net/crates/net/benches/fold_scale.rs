//! Capability-fold scale benches: apply, translate, sweep.
//!
//! Slice 0 of CAPABILITY_FOLD_SCALE_PLAN.md. These are the "before"
//! numbers every later slice of that plan reports against.
//!
//! Each bench prepares its state outside the timed region and asserts
//! the outcome of every measured operation, so a bench cannot quietly
//! report the speed of a rejected apply or of a sweep with nothing left
//! to evict. Benches that reset a shared fold use
//! `BatchSize::PerIteration`: with larger batches Criterion runs every
//! setup in the batch before any routine, so all but the first routine
//! would see the state the earlier routines left behind.
//!
//! The counting workloads (footprint, cache hit/miss split, the mixed
//! 1M workload, sweep visit counts) live in `fold_scale_report`; they
//! need a counting global allocator or wall-clock pacing that would
//! distort these timings.
//!
//! Run: `cargo bench --features "net fixtures" --bench fold_scale`.
//! The 1M rows build a ~1M-entry fold per group and need several GB of
//! RAM; filter them out with e.g. `-- '/(10000|100000)$'`.

use std::hint::black_box;
use std::time::Duration;

use criterion::{
    criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use net::adapter::net::behavior::fold::{capability_bridge, ApplyOutcome, NodeId};

#[path = "fold_scale_fixture/mod.rs"]
mod fold_scale_fixture;
use fold_scale_fixture::*;

/// Resident fold sizes for the apply benches. 1M is the plan's
/// operating point.
const APPLY_SIZES: [u64; 3] = [10_000, 100_000, 1_000_000];

/// Resident fold sizes for the sweep benches.
const SWEEP_SIZES: [u64; 2] = [100_000, 1_000_000];

/// Apply targets rotate through this many residents, sampled uniformly
/// at random (fixed seed) across the fold. A fixed stride would alias with
/// the fixture's 4800-entry template period: at 1M a stride of 976 shares a
/// factor of 16 with it, and every selected target lands on an odd template
/// index without the `inference` tag. Random sampling keeps the targets'
/// payload mix the fleet's at every size; `Targets::new` prints the share
/// carrying `inference` so the mix is visible next to the numbers.
const TARGETS: u64 = 1024;

/// Per-target apply state: the next generation to use and the payload
/// variant currently resident.
struct Targets {
    nodes: Vec<NodeId>,
    generation: Vec<u64>,
    variant: Vec<u8>,
    next: usize,
}

impl Targets {
    fn new(templates: &Templates, n: u64) -> Self {
        let mut rng = Rng::new(0x5EED);
        let mut picked = std::collections::HashSet::new();
        while (picked.len() as u64) < TARGETS.min(n) {
            picked.insert(rng.below(n));
        }
        let mut indices: Vec<u64> = picked.into_iter().collect();
        indices.sort_unstable();
        let nodes: Vec<NodeId> = indices.into_iter().map(node_id).collect();
        let len = nodes.len();
        let with_inference = nodes
            .iter()
            .filter(|&&node| {
                templates
                    .envelope(node, 1, LIVE_TTL_SECS, 0)
                    .payload
                    .tags
                    .iter()
                    .any(|t| t == "inference")
            })
            .count();
        eprintln!(
            "apply targets at {n}: {len} sampled, {:.1}% carry `inference` (fleet: 50%)",
            100.0 * with_inference as f64 / len as f64
        );
        Self {
            nodes,
            // Every resident starts at generation 1 (see `live_fold`).
            generation: vec![1; len],
            variant: vec![0; len],
            next: 0,
        }
    }

    /// The next target, with its generation advanced past the
    /// resident one.
    fn advance(&mut self) -> (usize, NodeId, u64) {
        let i = self.next;
        self.next = (self.next + 1) % self.nodes.len();
        self.generation[i] += 1;
        (i, self.nodes[i], self.generation[i])
    }
}

fn bench_capability_fold_apply(c: &mut Criterion) {
    let templates = Templates::new();
    let mut group = c.benchmark_group("capability_fold_apply");
    group.throughput(Throughput::Elements(1));

    for &n in &APPLY_SIZES {
        // One fold per size, shared by the three benches. Each leaves
        // the fold at size `n` with every resident live.
        let fold = live_fold(&templates, n);
        let mut targets = Targets::new(&templates, n);

        // Insert into a fold of `n - 1`: setup evicts the target, the
        // timed apply re-inserts it, so residency is `n` again after
        // every iteration.
        group.bench_with_input(BenchmarkId::new("insert", n), &n, |b, _| {
            b.iter_batched(
                || {
                    let (i, node, generation) = targets.advance();
                    fold.evict_node(node, "bench reset");
                    templates.envelope(node, generation, LIVE_TTL_SECS, targets.variant[i])
                },
                |ann| {
                    let outcome = fold.apply(ann).expect("apply");
                    assert_eq!(outcome, ApplyOutcome::Inserted);
                },
                BatchSize::PerIteration,
            );
        });

        // Steady-state refresh: same payload, newer generation.
        // `index_payload_equivalent` skips the index churn here.
        group.bench_with_input(BenchmarkId::new("refresh_equivalent", n), &n, |b, _| {
            b.iter_batched(
                || {
                    let (i, node, generation) = targets.advance();
                    templates.envelope(node, generation, LIVE_TTL_SECS, targets.variant[i])
                },
                |ann| {
                    let outcome = fold.apply(ann).expect("apply");
                    assert_eq!(outcome, ApplyOutcome::Replaced);
                },
                BatchSize::PerIteration,
            );
        });

        // Changed payload: alternates the target between two variants
        // that differ in an indexed tag, so every apply re-indexes.
        group.bench_with_input(BenchmarkId::new("replace_changed", n), &n, |b, _| {
            b.iter_batched(
                || {
                    let (i, node, generation) = targets.advance();
                    targets.variant[i] ^= 1;
                    templates.envelope(node, generation, LIVE_TTL_SECS, targets.variant[i])
                },
                |ann| {
                    let outcome = fold.apply(ann).expect("apply");
                    assert_eq!(outcome, ApplyOutcome::Replaced);
                },
                BatchSize::PerIteration,
            );
        });

        assert_eq!(
            fold.metrics().entries(),
            n,
            "apply benches changed residency"
        );
    }

    group.finish();
}

/// The legacy-announcement translation that runs upstream of the fold
/// lock on every inbound announcement. Apply-only timing excludes it.
fn bench_capability_translate(c: &mut Criterion) {
    let anns: Vec<_> = (0..64).map(legacy_announcement).collect();
    let mut group = c.benchmark_group("capability_translate");
    group.throughput(Throughput::Elements(1));

    let mut k = 0usize;
    group.bench_function("announcement", |b| {
        b.iter(|| {
            k = (k + 1) % anns.len();
            capability_bridge::translate_announcement(black_box(&anns[k]), None)
        });
    });

    group.finish();
}

fn bench_capability_fold_sweep(c: &mut Criterion) {
    let templates = Templates::new();
    let mut group = c.benchmark_group("capability_fold_sweep");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for &n in &SWEEP_SIZES {
        let fold = live_fold(&templates, n);

        // Nothing expired: the walk that runs twice a second and finds
        // nothing.
        group.bench_with_input(BenchmarkId::new("steady", n), &n, |b, _| {
            b.iter(|| {
                let reaped = fold.sweep_expired_now();
                assert_eq!(reaped, 0);
            });
        });

        // 10% expired. Setup re-arms the expired population before every
        // iteration: TTL 0 makes each re-applied entry due at once, and
        // the previous iteration's sweep already removed it, so the
        // apply is an Insert (a Replace on the first iteration, where
        // the entry is still live).
        let expired = every_nth(n, 10);
        let mut generation = 1u64;
        group.bench_with_input(BenchmarkId::new("mass_expiry", n), &n, |b, _| {
            b.iter_batched(
                || {
                    generation += 1;
                    for &node in &expired {
                        fold.apply(templates.envelope(node, generation, 0, 0))
                            .expect("re-arm apply");
                    }
                },
                |()| {
                    let reaped = fold.sweep_expired_now();
                    assert_eq!(reaped, expired.len());
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_capability_translate,
    bench_capability_fold_apply,
    bench_capability_fold_sweep
);
criterion_main!(benches);
