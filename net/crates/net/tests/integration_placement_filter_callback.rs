//! End-to-end: register a custom `PlacementFilter` via the
//! process-global registry, build a `StandardPlacement` over a
//! populated `Fold<CapabilityFold>`, and verify the scheduler's
//! per-candidate scoring honors the filter's verdict.
//!
//! Why an integration test (substrate already has unit tests in
//! `behavior/placement.rs`):
//!
//! - The unit suite registers a `FixedScoreFilter` against a
//!   single-entry fold with empty caps. Useful for the wire
//!   contract; doesn't prove that a filter inspecting real
//!   per-candidate tags via the same `Fold<CapabilityFold>` the
//!   scheduler uses produces the right per-candidate verdict.
//! - The full path "callback registered → scheduler scores →
//!   filter consults the live fold → verdict reaches the
//!   composed score" is what binding consumers exercise via
//!   `placement_filter_from_fn` (TS / Python / Go) and Rust SDK
//!   consumers via `global_placement_filter_registry()`. This
//!   test pins it.

#![cfg(feature = "net")]

use std::sync::Arc;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use net::adapter::net::behavior::placement::{
    Artifact, NodeId, PlacementFilter, StandardPlacement,
};
use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;
use net::adapter::net::identity::EntityKeypair;

// ============================================================================
// Test fixtures: a populated Fold<CapabilityFold>.
// ============================================================================

/// Build a `CapabilitySet` carrying a single legacy tag for the
/// candidate-discrimination test. The placement-filter callback
/// inspects these tags via the live fold.
fn caps_with_tag(tag: &str) -> CapabilitySet {
    CapabilitySet::new().add_tag(tag)
}

/// Insert a peer into the fold via the legacy-announcement bridge.
/// The placement filter and scheduler routes both read from the
/// fold; the test filter synthesizes the CapabilitySet via the
/// bridge so it sees the same data the scheduler sees.
fn install_peer(fold: &Fold<CapabilityFold>, node_id: NodeId, caps: CapabilitySet) {
    let kp = EntityKeypair::generate();
    let ann = CapabilityAnnouncement::new(node_id, kp.entity_id().clone(), 1, caps);
    capability_bridge::apply_legacy_announcement(fold, ann);
}

/// 4-peer fixture: GPU, TPU, CPU, untagged. The custom filter we
/// install only admits GPU peers.
fn populated_index() -> Fold<CapabilityFold> {
    let fold = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
    install_peer(&fold, 0xAAA1, caps_with_tag("hardware-class-gpu"));
    install_peer(&fold, 0xAAA2, caps_with_tag("hardware-class-tpu"));
    install_peer(&fold, 0xAAA3, caps_with_tag("hardware-class-cpu"));
    install_peer(&fold, 0xAAA4, CapabilitySet::new());
    fold
}

// ============================================================================
// Test filter: only the GPU-tagged peer is admitted.
// ============================================================================

/// Filter that inspects the candidate's tags via the live
/// `Fold<CapabilityFold>` (a clone of the same fold the scheduler
/// uses) and returns `Some(1.0)` for GPU-tagged candidates,
/// `None` for everything else.
struct GpuOnlyFilter {
    fold: Arc<Fold<CapabilityFold>>,
}

impl PlacementFilter for GpuOnlyFilter {
    fn placement_score(&self, target: &NodeId, _artifact: &Artifact<'_>) -> Option<f32> {
        // Synthesize the candidate's CapabilitySet from the fold's
        // tag set; bridges the test's tag-only predicate to the new
        // fold-backed storage.
        let caps = capability_bridge::synthesize_capability_set(&self.fold, *target);
        if caps.has_tag("hardware-class-gpu") {
            Some(1.0)
        } else {
            None
        }
    }
}

/// RAII so a panicking test never leaks the registration into the
/// process-global singleton (which other tests share).
struct FilterGuard {
    id: String,
}

impl Drop for FilterGuard {
    fn drop(&mut self) {
        global_placement_filter_registry().unregister(&self.id);
    }
}

fn register_filter(id: &str, filter: Arc<dyn PlacementFilter>) -> FilterGuard {
    let reg = global_placement_filter_registry();
    // Cleanup from a possibly-failed prior run.
    let _ = reg.unregister(id);
    assert!(
        reg.register(
            id.to_string(),
            filter,
            "integration_placement_filter_callback"
        ),
        "register {id}",
    );
    FilterGuard { id: id.to_string() }
}

fn empty_caps() -> CapabilitySet {
    CapabilitySet::new()
}

fn daemon_artifact<'a>(
    daemon_id: &'a [u8; 32],
    req: &'a CapabilitySet,
    opt: &'a CapabilitySet,
) -> Artifact<'a> {
    Artifact::Daemon {
        daemon_id: *daemon_id,
        required: req,
        optional: opt,
    }
}

// ============================================================================
// Tests.
// ============================================================================

/// Live-scheduler path: the registered filter selectively admits
/// the GPU-tagged peer (`AAA1`) and vetoes the other three. Pins
/// that the scheduler invokes the filter per candidate AND that
/// the filter's lookup-by-id against the live `CapabilityIndex`
/// resolves to the correct per-candidate caps.
#[test]
fn registered_filter_selectively_admits_via_live_index() {
    let fold = Arc::new(populated_index());
    let id = "integ-pf-gpu-only";
    let _guard = register_filter(
        id,
        Arc::new(GpuOnlyFilter {
            fold: fold.clone(),
        }),
    );

    let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);
    let req = empty_caps();
    let opt = empty_caps();
    let daemon_id = [0u8; 32];
    let artifact = daemon_artifact(&daemon_id, &req, &opt);

    // GPU peer admitted.
    let gpu = placement.placement_score(&0xAAA1, &artifact);
    assert!(
        gpu.is_some_and(|s| s > 0.0),
        "GPU-tagged peer must score > 0; got {gpu:?}",
    );

    // TPU / CPU / untagged all hard-vetoed.
    for nid in [0xAAA2u64, 0xAAA3, 0xAAA4] {
        assert_eq!(
            placement.placement_score(&nid, &artifact),
            None,
            "non-GPU peer {nid:#x} must hard-veto via the registered filter",
        );
    }
}

/// Unregister mid-flight: subsequent scores hard-veto on the
/// missing-id path. Pins the misconfig contract — a stale
/// `custom_filter_id` referencing an unregistered filter must
/// fail closed (drop the candidate), not silently fall through to
/// 1.0. Mirrors the substrate's
/// `standard_placement_unregistered_custom_filter_id_vetoes`
/// unit test but at the integration boundary (real index, real
/// registry, real scheduler scoring).
#[test]
fn unregistering_filter_collapses_to_hard_veto() {
    let fold = Arc::new(populated_index());
    let id = "integ-pf-unregister-mid-flight";
    let guard = register_filter(
        id,
        Arc::new(GpuOnlyFilter {
            fold: fold.clone(),
        }),
    );

    let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);
    let req = empty_caps();
    let opt = empty_caps();
    let daemon_id = [0u8; 32];
    let artifact = daemon_artifact(&daemon_id, &req, &opt);

    // Sanity: filter is registered → GPU peer admitted.
    assert!(placement.placement_score(&0xAAA1, &artifact).is_some());

    // Drop the registration (Drop impl runs).
    drop(guard);

    // Same scoring call now hard-vetoes — the StandardPlacement
    // code path treats an unresolvable id as a misconfig and
    // returns None for every candidate. (The substrate also logs;
    // we don't assert on log output here, just on the contract.)
    assert_eq!(
        placement.placement_score(&0xAAA1, &artifact),
        None,
        "unregistered filter id must hard-veto",
    );
}

/// Filter inspects the daemon's `Artifact` (req / opt caps) — not
/// just the candidate's. Pins that the scheduler passes the
/// daemon's metadata through to the filter so application
/// predicates can match on intent / required-caps / etc., not
/// just the candidate's announced state.
#[test]
fn filter_receives_artifact_with_daemon_capabilities() {
    let _fold = Arc::new(populated_index());
    let id = "integ-pf-artifact-passthrough";

    /// Filter that copies the daemon's required-tags into a
    /// shared `Mutex<Vec<String>>` so the test can assert on
    /// what reached the callback.
    struct TapFilter {
        seen: Arc<parking_lot::Mutex<Vec<String>>>,
    }
    impl PlacementFilter for TapFilter {
        fn placement_score(&self, _: &NodeId, artifact: &Artifact<'_>) -> Option<f32> {
            if let Artifact::Daemon { required, .. } = artifact {
                let tags: Vec<String> = required.tags.iter().map(|t| t.to_string()).collect();
                self.seen.lock().extend(tags);
            }
            Some(1.0)
        }
    }

    let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let _guard = register_filter(id, Arc::new(TapFilter { seen: seen.clone() }));

    // Score against a peer whose announced caps SATISFY the
    // required tags — `StandardPlacement` short-circuits on the
    // required-caps axis before reaching the custom filter, so the
    // candidate must carry the marker tags for the tap to observe
    // anything. (That short-circuit is itself worth pinning, but
    // it's already covered by the substrate's unit suite.)
    let custom_fold = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
    install_peer(
        &custom_fold,
        0xBBB1,
        CapabilitySet::new()
            .add_tag("required-marker-alpha")
            .add_tag("required-marker-beta"),
    );
    let custom_fold = Arc::new(custom_fold);
    let placement = StandardPlacement::new(&custom_fold).with_custom_filter_id(id);
    let req = CapabilitySet::new()
        .add_tag("required-marker-alpha")
        .add_tag("required-marker-beta");
    let opt = empty_caps();
    let daemon_id = [0u8; 32];
    let artifact = daemon_artifact(&daemon_id, &req, &opt);

    let _ = placement.placement_score(&0xBBB1, &artifact);

    let observed = seen.lock().clone();
    assert!(
        observed.iter().any(|t| t == "required-marker-alpha"),
        "filter must receive daemon's required tags via Artifact::Daemon — got {observed:?}",
    );
    assert!(
        observed.iter().any(|t| t == "required-marker-beta"),
        "second required tag should also be visible — got {observed:?}",
    );
}
