//! Benchmarks for `StandardPlacement::placement_score`.
//!
//! Run with: `cargo bench --features net --bench placement`
//!
//! Pins the perf budgets called out in `CAPABILITY_SYSTEM_SDK_PLAN.md`
//! § Performance:
//!
//! > Configuration-driven `StandardPlacement::placement_score`
//! > ≤ 5 μs across 100 candidate nodes (matches the substrate plan's
//! > budget). Callback-driven `PlacementFilter` ≤ 50 μs per call
//! > across the FFI boundary; pin in tests so a regression is loud.
//!
//! The FFI-crossing budget is downstream's concern (each binding's
//! TSFN / PyAny / cgo path has different test infrastructure).
//! What this bench pins is the substrate-side dispatch overhead:
//!
//!   - `baseline_no_custom_filter` — pure in-tree axes, no
//!     registry lookup. Budget reference for "how fast can the
//!     six in-tree axes score 100 candidates."
//!   - `with_custom_filter_rust_callback` — same scenario, but
//!     a `custom_filter_id` is configured pointing at a no-op
//!     Rust filter registered in `global_placement_filter_registry`.
//!     Pins the registry-lookup + dispatch overhead per candidate.
//!     The delta vs. baseline is the cost of the Phase 7 hook in
//!     the absence of a real FFI boundary.
//!   - `with_custom_filter_predicate` — wraps a 2-clause predicate
//!     as a `PlacementFilter`. Pins the realistic path where the
//!     custom filter does meaningful work (predicate evaluation
//!     against the candidate's caps).

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use net::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use net::adapter::net::behavior::{
    global_placement_filter_registry, Artifact, CapabilityAnnouncement, CapabilitySet, EvalContext,
    PlacementFilter, PlacementNodeId, Predicate, StandardPlacement, Tag, TagKey, TaxonomyAxis,
};
use net::adapter::net::identity::EntityId;

// ============================================================================
// Setup helpers
// ============================================================================

/// Build a `Fold<CapabilityFold>` populated with `count` candidate
/// nodes. Each node has a small, realistic cap set: a region scope
/// tag, a hardware GPU tag, and a 2-key metadata blob. Mirrors the
/// kind of fold a placement decision would actually run against.
fn make_fold(count: usize) -> Arc<Fold<CapabilityFold>> {
    let fold = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
    let eid = EntityId::from_bytes([0u8; 32]);
    for i in 0..count {
        let caps = CapabilitySet::default()
            .add_tag("hardware.gpu")
            .add_tag(format!("hardware.gpu.vram_gb={}", 16 + i))
            .with_metadata("intent", "ml-training")
            .with_metadata("region", if i % 2 == 0 { "us-east" } else { "us-west" });
        let ad = CapabilityAnnouncement::new(0x1000 + i as u64, eid.clone(), 1, caps);
        capability_bridge::apply_legacy_announcement(&fold, ad);
    }
    Arc::new(fold)
}

/// `Arc<dyn PlacementFilter>` factory that scores every candidate
/// at 1.0 unconditionally. Used to measure registry-lookup +
/// dispatch overhead in isolation from the actual scoring logic.
struct AlwaysOneFilter;
impl PlacementFilter for AlwaysOneFilter {
    fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
        Some(1.0)
    }
}

/// Predicate-backed filter — same construction as the cross-binding
/// fixture's `PredicatePlacementFilter`. Two-clause predicate
/// (`exists hardware.gpu AND equals region us-east`); pins the
/// realistic per-candidate cost.
///
/// Synthesizes a `CapabilitySet` from the fold's tag set per
/// scoring call (the bridge's standard pattern). Allocates the
/// synthesized set + a `Vec<Tag>` projection; bounded by the
/// candidate's tag cardinality.
struct PredicateFilter {
    pred: Predicate,
    fold: Arc<Fold<CapabilityFold>>,
}
impl PlacementFilter for PredicateFilter {
    fn placement_score(&self, target: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
        let caps = capability_bridge::synthesize_capability_set(&self.fold, *target);
        let tags: Vec<Tag> = caps.tags.iter().cloned().collect();
        let ctx = EvalContext::new(&tags, &caps.metadata);
        if self.pred.evaluate_unplanned(&ctx) {
            Some(1.0)
        } else {
            None
        }
    }
}

// ============================================================================
// Benches
// ============================================================================

fn bench_placement_score(c: &mut Criterion) {
    let mut group = c.benchmark_group("standard_placement_score");
    group.throughput(Throughput::Elements(100));

    let req_caps = CapabilitySet::default();
    let opt_caps = CapabilitySet::default();
    let artifact = Artifact::Daemon {
        daemon_id: [0u8; 32],
        required: &req_caps,
        optional: &opt_caps,
    };

    // ── Baseline: 100 candidates, no custom filter ──
    //
    // Plan budget: ≤ 5 μs across 100 candidates with config-driven
    // (in-tree-axes-only) placement.
    {
        let fold = make_fold(100);
        let placement = StandardPlacement::new(&fold);

        group.bench_function(BenchmarkId::new("baseline_no_custom_filter", 100), |b| {
            b.iter(|| {
                for i in 0..100u64 {
                    let node = 0x1000 + i;
                    let _ = black_box(placement.placement_score(&node, &artifact));
                }
            });
        });
    }

    // ── With custom filter (Rust no-op callback) ──
    //
    // Pins the registry-lookup + dispatch overhead. Same 100
    // candidates, but each scoring call detours through
    // `global_placement_filter_registry().get(id)` →
    // `Arc<dyn PlacementFilter>::placement_score` → `Some(1.0)`.
    // The delta vs baseline is the substrate-side custom-filter
    // tax in isolation from any FFI cost.
    {
        let fold = make_fold(100);
        let id = "bench-pf-noop";
        let registry = global_placement_filter_registry();
        // Defensive cleanup if a prior run aborted.
        let _ = registry.unregister(id);
        let arc: Arc<dyn PlacementFilter> = Arc::new(AlwaysOneFilter);
        registry.register(id.to_string(), arc, "bench");

        let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);

        group.bench_function(
            BenchmarkId::new("with_custom_filter_rust_callback", 100),
            |b| {
                b.iter(|| {
                    for i in 0..100u64 {
                        let node = 0x1000 + i;
                        let _ = black_box(placement.placement_score(&node, &artifact));
                    }
                });
            },
        );

        registry.unregister(id);
    }

    // ── With custom filter (2-clause predicate) ──
    //
    // Realistic path: the filter does meaningful work. Same 100
    // candidates; the predicate evaluates `exists hardware.gpu
    // AND metadata.region == "us-east"`. Half the candidates pass
    // (us-east), half fail (us-west).
    {
        let fold = make_fold(100);
        let pred = {
            let gpu_key = TagKey::new(TaxonomyAxis::Hardware, "gpu");
            let exists_gpu = Predicate::exists(gpu_key);
            let region_us_east = Predicate::metadata_equals("region", "us-east");
            Predicate::and(vec![exists_gpu, region_us_east])
        };
        let id = "bench-pf-predicate";
        let registry = global_placement_filter_registry();
        let _ = registry.unregister(id);
        let arc: Arc<dyn PlacementFilter> = Arc::new(PredicateFilter {
            pred,
            fold: fold.clone(),
        });
        registry.register(id.to_string(), arc, "bench");

        let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);

        group.bench_function(BenchmarkId::new("with_custom_filter_predicate", 100), |b| {
            b.iter(|| {
                for i in 0..100u64 {
                    let node = 0x1000 + i;
                    let _ = black_box(placement.placement_score(&node, &artifact));
                }
            });
        });

        registry.unregister(id);
    }

    group.finish();
}

criterion_group!(benches, bench_placement_score);
criterion_main!(benches);
