//! Placement filter — substrate primitive for "given an artifact and a
//! candidate node, how good a fit is the candidate?"
//!
//! Phase F slice 1 of [`CAPABILITY_SYSTEM_PLAN.md`](../../../../docs/plans/CAPABILITY_SYSTEM_PLAN.md).
//! Lays the trait + `Artifact` enum + a `LegacyPlacement` shim that
//! preserves today's `find_migration_targets` behavior. Phase F
//! slice 2+ will land `StandardPlacement` (the multi-axis reference
//! impl), `IntentRegistry`, and the tie-breaking comparator the
//! scheduler will consume.
//!
//! ## Trait contract (LOCKED in plan §7)
//!
//! - `placement_score` returns `Option<f32>`:
//!   - `None` — node is ineligible (hard constraint failed). Equivalent
//!     to `Some(0.0)` for ranking, but lets the scheduler short-
//!     circuit candidate enumeration.
//!   - `Some(score)` with `score ∈ [0.0, 1.0]` — higher is a better fit.
//! - Composes multiplicatively across axes (`StandardPlacement` does
//!   this internally; custom impls SHOULD preserve the
//!   `0.0 anywhere → 0.0 final` invariant).
//! - Tie-breaking lives in the scheduler, not here — the same score
//!   from two candidates is resolved via the locked
//!   RTT → free-resource → lexicographic-NodeId chain.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use crate::adapter::net::behavior::predicate::Predicate;
use crate::adapter::net::behavior::required_capability::RequiredCapability;
use crate::adapter::net::behavior::tag::{Tag, TagKey, TaxonomyAxis};
use crate::adapter::net::channel::ChannelName;

/// Identifier of a candidate node — the substrate's `u64` `node_id`.
/// Aliased here so trait signatures stay readable; future variants
/// (signing-key bound vs. ephemeral) compose without churning the
/// trait.
pub type NodeId = u64;

/// What is being placed. Captures everything a `PlacementFilter`
/// implementation needs to score a candidate, by reference (no
/// allocation per scoring call).
///
/// Variants match the plan §7-locked surface:
///
/// - [`Artifact::Chain`] — placing a causal chain (storage workload).
/// - [`Artifact::Replica`] — placing a replica of a channel
///   (replication workload).
/// - [`Artifact::Daemon`] — placing (or migrating) a daemon
///   (compute workload).
///
/// Borrowed `&'a` data so callers can construct an `Artifact` from
/// references they already hold without a clone — the placement
/// hot path is per-candidate scoring across many candidates.
#[derive(Debug)]
pub enum Artifact<'a> {
    /// Causal chain — placement decisions for `dataforts` workloads.
    Chain {
        /// Origin hash uniquely identifying the chain. Used for
        /// stable ordering when ties occur.
        origin_hash: [u8; 32],
        /// Capability set published by the chain (storage / region /
        /// retention metadata).
        capabilities: &'a CapabilitySet,
    },
    /// Channel replica — placement decisions for replicated state.
    Replica {
        /// Channel name being replicated.
        channel: &'a ChannelName,
        /// Capability profile required of the candidate replica
        /// host (advertised storage capacity, region, etc.).
        capabilities: &'a CapabilitySet,
    },
    /// Daemon — placement decisions for compute workloads.
    /// Carries the daemon's required + optional capability sets.
    Daemon {
        /// Daemon identity (origin hash); used for stable ordering.
        daemon_id: [u8; 32],
        /// Hard requirements — the candidate node MUST satisfy these
        /// or the filter SHOULD return `None`.
        required: &'a CapabilitySet,
        /// Soft preferences — the candidate's score reflects how
        /// many of these are satisfied; missing optional caps don't
        /// veto placement.
        optional: &'a CapabilitySet,
    },
    /// Blob — placement decisions for Dataforts mesh-native blob
    /// storage (v0.2 PR-2b). Carries the blob's content hash + size
    /// so the placement gate can check the candidate's
    /// `dataforts.blob.disk_free_gb` against the blob's storage cost.
    /// Stable ordering during ties uses `blob_hash`.
    ///
    /// Hard constraints applied in
    /// [`StandardPlacement::placement_score`]:
    ///
    /// - Target carries `dataforts.blob.storage = true`.
    /// - Target NOT carrying the reserved
    ///   `dataforts:blob-storage-unhealthy` tag.
    /// - Target's `dataforts.blob.disk_free_gb` ≥ `size_bytes / 1 GiB`
    ///   (rounded up).
    Blob {
        /// Content-address of the blob — manifest hash for a chunked
        /// blob; Small-blob hash for a single content-addressed
        /// payload. Used for stable ordering when ties occur.
        blob_hash: [u8; 32],
        /// Total payload size in bytes. Drives the
        /// `dataforts.blob.disk_free_gb` hard-constraint check.
        size_bytes: u64,
    },
}

/// Substrate-level placement primitive. Trait surface locked by
/// [`CAPABILITY_SYSTEM_PLAN.md`](../../../../docs/plans/CAPABILITY_SYSTEM_PLAN.md) §7.
///
/// Application code that wants opinionated placement implements
/// this trait directly; callers pass `&dyn PlacementFilter` to the
/// scheduler. The reference impl (`StandardPlacement`, slice 2)
/// covers the common case; `LegacyPlacement` here preserves the
/// pre-Phase-F "any node satisfying the legacy `CapabilityFilter`
/// is fine" behavior so existing scheduler call sites keep working
/// during the migration window.
pub trait PlacementFilter: Send + Sync {
    /// Score `target` for hosting `artifact`.
    ///
    /// Return value:
    ///
    /// - `None` — `target` is ineligible. The scheduler treats this
    ///   as a hard veto and excludes the candidate from ranking.
    ///   Use this when a hard constraint (required capability,
    ///   region restriction) fails — equivalent to `Some(0.0)` for
    ///   ranking but lets the caller short-circuit.
    /// - `Some(score)` — score in `[0.0, 1.0]`. Higher is a better
    ///   fit. The scheduler picks the highest-scoring candidate;
    ///   ties resolve via the locked
    ///   RTT → free-resource → lexicographic-NodeId chain (lives
    ///   in the scheduler, not here).
    ///
    /// Implementations MUST be `Send + Sync` (the trait bound
    /// enforces it).
    fn placement_score(&self, target: &NodeId, artifact: &Artifact<'_>) -> Option<f32>;
}

/// Backward-compatible shim that mirrors today's
/// `Scheduler::find_migration_targets` behavior: a candidate is
/// either eligible (matches the legacy `CapabilityFilter`) or it
/// isn't. Eligible candidates score `1.0`; ineligible return `None`.
///
/// Used during the Phase G rollout window so existing scheduler
/// call sites can swap from "query the index, take the first match"
/// to "score via `PlacementFilter`, take the highest" without
/// changing observable behavior. New impls should target
/// `StandardPlacement` (slice 2).
///
/// The shim consults a borrowed `&Fold<CapabilityFold>` to look up the
/// candidate's announced caps; this matches the existing
/// `Scheduler` plumbing where the index is already in scope.
pub struct LegacyPlacement<'a> {
    filter: CapabilityFilter,
    fold: &'a Fold<CapabilityFold>,
}

impl<'a> LegacyPlacement<'a> {
    /// Construct from an explicit legacy filter + the live fold.
    pub fn new(filter: CapabilityFilter, fold: &'a Fold<CapabilityFold>) -> Self {
        Self { filter, fold }
    }

    /// Construct an empty filter — every candidate that exists in
    /// the fold is eligible. Equivalent to today's
    /// `CapabilityFilter::default()` behavior in
    /// `find_migration_targets`.
    pub fn permissive(fold: &'a Fold<CapabilityFold>) -> Self {
        Self {
            filter: CapabilityFilter::default(),
            fold,
        }
    }
}

impl<'a> PlacementFilter for LegacyPlacement<'a> {
    fn placement_score(&self, target: &NodeId, _artifact: &Artifact<'_>) -> Option<f32> {
        // Per-target O(num classes target owns) check — calling
        // `find_nodes_matching` here would run the full composite
        // query and then linearly scan the result, making
        // `placement_score` quadratic when the scheduler invokes
        // it once per candidate target.
        if capability_bridge::target_matches_filter(self.fold, *target, &self.filter) {
            Some(1.0)
        } else {
            None
        }
    }
}

// =============================================================================
// StandardPlacement (slice 2) — multi-axis reference impl skeleton.
//
// Phase F slice 2 ships the COMPOSITION MACHINERY locked by plan §7
// ("Score composition is multiplicative across all axes including
// anti-affinity") plus the public CONFIG SHAPE the rest of Phase F
// will fill in. All five scoring axes return a placeholder `1.0`
// for now; slice 5 wires the per-axis evaluators.
//
// Hard-constraint check (artifact's `required` caps must be a subset
// of the target's tag set) is in place today — that gives Phase G a
// usable filter even before the per-axis scorers ship: candidates
// missing a required cap return `None` (hard veto), candidates that
// satisfy required caps return `Some(1.0)` (matches `LegacyPlacement`
// behavior).
// =============================================================================

use std::time::Duration;

/// Reserved-tag scope label (`scope:tenant:foo`, `scope:region:us-east`,
/// etc.). Slice 5 wires the actual scope-axis scoring against this;
/// slice 2 carries it for shape stability.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeLabel(String);

impl ScopeLabel {
    /// Wrap an arbitrary scope-tag body. Caller responsibility to
    /// match the substrate's reserved-prefix conventions
    /// (`scope:tenant:<id>`, `scope:region:<name>`, `scope:subnet-local`).
    pub fn new(label: impl Into<String>) -> Self {
        Self(label.into())
    }

    /// Borrow the raw label string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which resource pool the resource-availability axis (slice 5)
/// scores against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceAxis {
    /// `dataforts.free_storage_gb` — chain / replica artifacts.
    Storage,
    /// `hardware.cpu_cores` / `hardware.memory_gb` /
    /// `hardware.gpu.vram_gb` — daemon artifacts.
    Compute,
    /// Weighted average of `Storage` + `Compute` — replicated daemons.
    Both,
}

/// Metadata-key names used by the intent + colocation axes. Allows
/// applications to override the default keys when migrating from a
/// legacy convention.
#[derive(Debug, Clone)]
pub struct PlacementMetadataKeys {
    /// Default `"intent"`.
    pub intent: String,
    /// Default `"colocate-with"`.
    pub colocate_with: String,
    /// Default `"colocate-with-strict"`.
    pub colocate_with_strict: String,
}

impl Default for PlacementMetadataKeys {
    fn default() -> Self {
        Self {
            intent: "intent".to_string(),
            colocate_with: "colocate-with".to_string(),
            colocate_with_strict: "colocate-with-strict".to_string(),
        }
    }
}

/// Anti-affinity axis configuration. Penalizes nodes that already
/// lead more than `leadership_concentration_threshold` fraction of
/// channels in local view, multiplying their final score by
/// `leadership_concentration_penalty`.
#[derive(Debug, Clone, Copy)]
pub struct AntiAffinityConfig {
    /// `[0.0, 1.0]`. Default `0.30` (penalize past 30% leadership).
    pub leadership_concentration_threshold: f32,
    /// `[0.0, 1.0]`. Default `0.4` (multiply by 0.4 when over).
    pub leadership_concentration_penalty: f32,
}

impl Default for AntiAffinityConfig {
    fn default() -> Self {
        Self {
            leadership_concentration_threshold: 0.30,
            leadership_concentration_penalty: 0.4,
        }
    }
}

/// How the intent-match axis decides eligibility.
#[derive(Debug, Clone, Default)]
pub enum IntentMatchPolicy {
    /// Disabled — intent axis always returns `1.0`. Default.
    #[default]
    Disabled,
    /// Node fulfills any intent it has capability for. Slice 5
    /// wires the actual lookup against `IntentRegistry` (slice 3).
    AnyOfLocalCapabilities,
    /// Node must satisfy the registry's required capabilities for
    /// the artifact's declared `metadata.intent` value.
    Strict,
}

/// How the colocation axis weights `metadata.colocate-with` matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColocationPolicy {
    /// Ignore colocation hints entirely (axis returns `1.0`). Default.
    #[default]
    Ignore,
    /// Boost score when target hosts the colocation chain.
    SoftPreference,
    /// Refuse placement (return `None`) unless target hosts the
    /// colocation chain. Triggered by `colocate-with-strict`.
    StrictRequired,
}

/// Reference `PlacementFilter` impl. Five-axis multi-criteria
/// scoring (scope / proximity / intent / colocation / resource) +
/// anti-affinity penalty, all composing multiplicatively per the
/// plan §7 LOCKED contract. Slice 2 shipped the composition
/// machinery + hard-constraint check; slice 5 fills in the
/// per-axis scorers incrementally — intent axis lands first.
///
/// Borrows a `&Fold<CapabilityFold>` for target-cap lookup
/// (matches the `LegacyPlacement` borrow shape). Hold a fresh
/// `StandardPlacement` per scheduler call site or share an
/// `Arc<...>` if sharing configuration.
pub struct StandardPlacement<'a> {
    fold: &'a Fold<CapabilityFold>,
    /// Set of scope labels the target must match (one-of). `None`
    /// disables the scope axis.
    pub scope_filter: Option<Vec<ScopeLabel>>,
    /// Hard ceiling on the proximity-graph RTT to the candidate.
    /// `None` disables the proximity axis.
    pub proximity_max_rtt: Option<Duration>,
    /// Closure-based RTT lookup. Returns microsecond-resolution
    /// RTT for a candidate, or `None` if not measured. Decouples
    /// the placement module from the proximity graph's internal
    /// node-id shape (see [`RttLookup`]); scheduler integrations
    /// supply a bridge closure when wiring through. `None` (the
    /// default) skips the proximity axis regardless of
    /// `proximity_max_rtt`.
    pub rtt_lookup: Option<&'a dyn RttLookup>,
    /// Intent-match strategy. `Disabled` skips the axis.
    pub intent_match: IntentMatchPolicy,
    /// Intent → required-caps registry consumed when
    /// `intent_match` is `Strict` or `AnyOfLocalCapabilities`.
    /// Empty default — `Disabled` policy ignores it; non-empty
    /// registries drive intent-axis scoring.
    pub intent_registry: IntentRegistry,
    /// Colocation strategy.
    pub colocation_policy: ColocationPolicy,
    /// Which resource pool the resource axis scores. `Compute` is
    /// the daemon-friendly default.
    pub resource_axis: ResourceAxis,
    /// Metadata-key names for the intent + colocation axes.
    pub metadata_keys: PlacementMetadataKeys,
    /// Anti-affinity penalty configuration.
    pub anti_affinity: AntiAffinityConfig,
    /// Closure-based leadership-stats lookup. `None` (the
    /// default) skips the anti-affinity axis. Scheduler
    /// integrations supply a closure that consults the local
    /// view of `causal:*` chain holdings per peer.
    pub leadership_stats: Option<&'a dyn LeadershipStatsLookup>,
    /// SDK Phase 7 slice 5 — custom placement-filter id.
    ///
    /// Set to the `id` returned by `placementFilterFromFn` /
    /// `placement_filter_from_fn` / `PlacementFilterFromFn` (TS /
    /// Python / Go SDKs), AFTER calling
    /// `mesh.registerPlacementFilter(id, fn)` to wire the closure
    /// across the FFI. During scoring, the substrate looks up `id`
    /// in [`global_placement_filter_registry`](super::placement_registry::global_placement_filter_registry)
    /// and treats the registered filter as an additional axis:
    ///
    /// - registered, returns `Some(score)` — multiplied into the
    ///   composition (LOCKED §7 multiplicative invariant).
    /// - registered, returns `None` — hard veto, candidate
    ///   excluded (no score is composed; `placement_score` returns
    ///   `None`).
    /// - id NOT registered — hard veto + log via `eprintln!`.
    ///   Operators who want a permissive default for missing
    ///   registrations should leave `custom_filter_id` as `None`
    ///   instead of referencing an unset id.
    ///
    /// `None` (the default) disables the custom-filter axis.
    pub custom_filter_id: Option<String>,
}

impl<'a> StandardPlacement<'a> {
    /// Build with default config — every axis disabled. Equivalent
    /// to `LegacyPlacement::permissive` in behavior (returns
    /// `Some(1.0)` for any candidate that satisfies the artifact's
    /// hard `required` constraint).
    pub fn new(fold: &'a Fold<CapabilityFold>) -> Self {
        Self {
            fold,
            scope_filter: None,
            proximity_max_rtt: None,
            rtt_lookup: None,
            intent_match: IntentMatchPolicy::default(),
            intent_registry: IntentRegistry::default(),
            colocation_policy: ColocationPolicy::default(),
            resource_axis: ResourceAxis::Compute,
            metadata_keys: PlacementMetadataKeys::default(),
            anti_affinity: AntiAffinityConfig::default(),
            leadership_stats: None,
            custom_filter_id: None,
        }
    }

    /// Replace the scope filter.
    pub fn with_scope_filter(mut self, labels: Vec<ScopeLabel>) -> Self {
        self.scope_filter = Some(labels);
        self
    }

    /// Replace the proximity max-RTT bound.
    pub fn with_proximity_max_rtt(mut self, max: Duration) -> Self {
        self.proximity_max_rtt = Some(max);
        self
    }

    /// Replace the RTT lookup closure. Pair with
    /// `with_proximity_max_rtt` for the proximity axis to score
    /// (both must be `Some` — either alone leaves the axis at
    /// 1.0 / disabled).
    pub fn with_rtt_lookup(mut self, lookup: &'a dyn RttLookup) -> Self {
        self.rtt_lookup = Some(lookup);
        self
    }

    /// Replace the leadership-stats lookup closure. Pair with
    /// the default `anti_affinity` config (or override via direct
    /// field write) for the anti-affinity axis to apply.
    pub fn with_leadership_stats(mut self, lookup: &'a dyn LeadershipStatsLookup) -> Self {
        self.leadership_stats = Some(lookup);
        self
    }

    /// Replace the intent-match policy.
    pub fn with_intent_match(mut self, policy: IntentMatchPolicy) -> Self {
        self.intent_match = policy;
        self
    }

    /// Replace the intent registry. Pair with `with_intent_match`
    /// (a non-`Disabled` policy + non-empty registry are required
    /// for the intent axis to score below 1.0).
    pub fn with_intent_registry(mut self, registry: IntentRegistry) -> Self {
        self.intent_registry = registry;
        self
    }

    /// Replace the colocation policy.
    pub fn with_colocation_policy(mut self, policy: ColocationPolicy) -> Self {
        self.colocation_policy = policy;
        self
    }

    /// Replace the resource axis.
    pub fn with_resource_axis(mut self, axis: ResourceAxis) -> Self {
        self.resource_axis = axis;
        self
    }

    /// SDK Phase 7 slice 5 — set the custom placement-filter id.
    ///
    /// `id` MUST match the id returned by an SDK
    /// `placementFilterFromFn` call AND have been previously
    /// registered via `mesh.registerPlacementFilter(id, fn)` over
    /// the FFI. See the [`Self::custom_filter_id`] field docs for
    /// veto semantics on missing registrations.
    pub fn with_custom_filter_id(mut self, id: impl Into<String>) -> Self {
        self.custom_filter_id = Some(id.into());
        self
    }
}

/// Multiplicative-composition fold. Pinned by plan §7 LOCKED:
/// "All axes — including the anti-affinity term — combine
/// multiplicatively in [0.0, 1.0]. A 0.0 on any axis zeroes the
/// final score."
///
/// Empty input returns `1.0` (identity for multiplication). NaN /
/// out-of-range inputs are clamped to `[0.0, 1.0]` so a
/// well-intentioned but buggy axis can't blow up the composition.
///
/// Public so applications building custom `PlacementFilter` impls
/// reuse the same composition rule the substrate's `StandardPlacement`
/// applies.
pub fn compose_axis_scores(scores: impl IntoIterator<Item = f32>) -> f32 {
    let mut product: f32 = 1.0;
    for s in scores {
        let clamped = if s.is_nan() { 0.0 } else { s.clamp(0.0, 1.0) };
        product *= clamped;
        // Short-circuit on a 0.0 — preserves the "0.0 anywhere → 0.0
        // final" invariant without iterating the remaining axes.
        if product == 0.0 {
            return 0.0;
        }
    }
    product
}

impl<'a> PlacementFilter for StandardPlacement<'a> {
    fn placement_score(&self, target: &NodeId, artifact: &Artifact<'_>) -> Option<f32> {
        // N-4: resolve the custom-filter axis BEFORE entering
        // `with_caps`. `with_caps` holds a per-shard read lock; its
        // doc explicitly warns the closure must not acquire any
        // other index locks (`capability.rs:3119-3122`).
        // `score_custom_filter_axis` invokes externally registered
        // `&dyn PlacementFilter` impls — including FFI filters from
        // JS / Python / Go (Phase 7 slices 2-4) and Rust-side
        // `LegacyPlacement` shims that call
        // `self.index.query(&self.filter)` directly
        // (`placement.rs:159-172`). Either path under the shard lock
        // deadlocks against a concurrent `index.index(...)` insert.
        //
        // The custom filter only needs `target` + `artifact` — no
        // dependency on `target_caps` — so lift it out. A `None`
        // return (the filter vetoed) short-circuits before the
        // `with_caps` clone path, saving the lookup work.
        let custom = self.score_custom_filter_axis(target, artifact)?;

        // Look up the candidate's announced caps. Phase 3b
        // routes through the fold's tag set via
        // capability_bridge::synthesize_capability_set; the
        // synthesized CapabilitySet carries tags only (the fold
        // doesn't model the legacy metadata map). Hard veto if
        // the publisher isn't known to the fold at all
        // (matches the legacy with_caps `Option::None` shape:
        // "unindexed candidate"); empty-tag publishers ARE
        // indexed and proceed to the scoring axes.
        let known = self
            .fold
            .with_state(|state| state.by_node.contains_key(target));
        if !known {
            return None;
        }
        let target_caps = capability_bridge::synthesize_capability_set(self.fold, *target);
        (|target_caps: &CapabilitySet| -> Option<f32> {
            // Hard-constraint check: artifact's `required` caps
            // must be a subset of the target's tag set. `Chain`
            // and `Replica` variants don't carry required caps
            // directly; they pass through this check (slice 5
            // may extend with per-variant checks).
            if let Artifact::Daemon { required, .. } = artifact {
                if !required.tags.iter().all(|t| target_caps.tags.contains(t)) {
                    return None;
                }
            }

            // `Artifact::Blob` hard constraints (v0.2 PR-2b):
            //
            // 1. Target advertises `dataforts.blob.storage`.
            // 2. Target does NOT carry the reserved
            //    `dataforts:blob-storage-unhealthy` tag.
            // 3. Target's `dataforts.blob.disk_free_gb` is at
            //    least `ceil(size_bytes / 1 GiB)`.
            //
            // Compute / replica / chain artifacts pass through
            // these checks (the `if let` short-circuits when
            // the variant doesn't match). The remaining
            // multi-axis composition (scope / proximity /
            // intent / colocation / resource / anti-affinity)
            // still applies — blobs participate in those axes
            // the same way chains do.
            if let Artifact::Blob { size_bytes, .. } = artifact {
                use super::dataforts_capabilities::{is_blob_storage_unhealthy, BlobCapability};
                let blob_caps = BlobCapability::from_capability_set(target_caps);
                if !blob_caps.storage {
                    return None;
                }
                if is_blob_storage_unhealthy(target_caps) {
                    return None;
                }
                let required_gb = size_bytes.div_ceil(1 << 30);
                // `disk_free_gb` is the target's last-heartbeat-
                // observed free space. It is *eventually
                // consistent* — two independent schedulers may
                // see the same value and both decide to place
                // onto the same candidate. The hard-constraint
                // here keeps placements correct in the single-
                // scheduler case; deployments running multiple
                // schedulers against the same candidate set must
                // route the placement decision through a single
                // coordinator when `required_gb > disk_free_gb /
                // 2`, or accept that races can co-place blobs
                // whose combined size exceeds the candidate's
                // disk. Scope / proximity / intent axes above are
                // pure tag-set logic and deterministic across
                // schedulers — this is the only axis with this
                // caveat.
                if blob_caps.disk_free_gb < required_gb {
                    return None;
                }
            }

            // Per-axis scoring (slice 5 of Phase F filled these
            // in). Each takes the borrowed `&CapabilitySet`.
            let scope = self.score_scope_axis(target_caps);
            let proximity = self.score_proximity_axis(target);
            let intent = self.score_intent_axis(target_caps, artifact);
            let colocation = self.score_colocation_axis(target_caps, artifact);
            let resource = self.score_resource_axis(target_caps, artifact);
            let anti_affinity = self.score_anti_affinity_axis(target);

            Some(compose_axis_scores([
                scope,
                proximity,
                intent,
                colocation,
                resource,
                anti_affinity,
                custom,
            ]))
        })(&target_caps)
    }
}

// Per-axis scoring stubs. Slice 5 replaces each body with the
// real evaluator. Each returns a value in `[0.0, 1.0]`; `1.0`
// means "axis is satisfied / doesn't apply"; `0.0` means
// "hard veto on this axis."
impl<'a> StandardPlacement<'a> {
    /// Phase F slice 5 scope axis. Returns:
    ///
    /// - `1.0` when `scope_filter` is `None` or empty (axis
    ///   disabled / no constraint).
    /// - `1.0` when ANY label in `scope_filter` matches a
    ///   `scope:*` reserved tag on the target — set-membership
    ///   "any-of" semantics: the daemon wants to land on a node
    ///   tagged with at least one of the listed scopes.
    /// - `0.0` when `scope_filter` is non-empty and no label
    ///   matches any of the target's `scope:*` tags.
    ///
    /// Each [`ScopeLabel`] accepts either the full form
    /// (`"scope:tenant:foo"`) or the body alone (`"tenant:foo"`)
    /// — both compare equal against the target's `Tag::Reserved
    /// { prefix: "scope:", body: "tenant:foo" }`.
    fn score_scope_axis(&self, target_caps: &CapabilitySet) -> f32 {
        let Some(filter) = self.scope_filter.as_ref() else {
            return 1.0;
        };
        if filter.is_empty() {
            return 1.0;
        }
        // Collect target's `scope:*` reserved-tag bodies for fast
        // any-of matching. A non-scope reserved tag (`causal:`,
        // `fork-of:`, `heat:`) is ignored — only `scope:` matters
        // here.
        let target_scope_bodies: Vec<&str> = target_caps
            .tags
            .iter()
            .filter_map(|t| match t {
                Tag::Reserved { prefix, body } if prefix.as_str() == "scope:" => {
                    Some(body.as_str())
                }
                _ => None,
            })
            .collect();

        if target_scope_bodies.is_empty() {
            // Target has no scope tags. The daemon's filter is non-
            // empty so this is a miss → 0.0. Operators who want
            // unscoped peers to satisfy the axis disable the axis
            // by leaving `scope_filter` as `None`.
            return 0.0;
        }

        let any_match = filter.iter().any(|label| {
            let raw = label.as_str();
            // Strip the optional `scope:` prefix from the label so
            // both `"scope:tenant:foo"` and `"tenant:foo"` compare
            // equal against the target's body field.
            let body = raw.strip_prefix("scope:").unwrap_or(raw);
            target_scope_bodies.contains(&body)
        });

        if any_match {
            1.0
        } else {
            0.0
        }
    }

    /// Phase F slice 5 proximity axis. Hard-bound RTT check:
    ///
    /// - `proximity_max_rtt: None` → 1.0 (axis disabled).
    /// - `rtt_lookup: None` → 1.0 (no measurement source; can't
    ///   enforce, default permissive — operators who care wire
    ///   the lookup explicitly).
    /// - RTT lookup returns `None` for the target → 1.0 (no data
    ///   for this candidate; default permissive — scoring an
    ///   unreached peer at 0.0 would flip the placement-decision
    ///   semantics for fresh-mesh / disconnected-peer scenarios).
    /// - RTT lookup returns `Some(rtt) <= max_rtt` → 1.0.
    /// - RTT lookup returns `Some(rtt) > max_rtt` → 0.0 (hard
    ///   veto).
    ///
    /// Asymmetry vs. tie-breaker step 1 is intentional: the
    /// tie-breaker needs strict ordering ("present beats missing"),
    /// the scoring axis just enforces the bound when measurement
    /// is available.
    fn score_proximity_axis(&self, target: &NodeId) -> f32 {
        let Some(max_rtt) = self.proximity_max_rtt else {
            return 1.0;
        };
        let Some(lookup) = self.rtt_lookup else {
            return 1.0;
        };
        let Some(rtt_us) = lookup(*target) else {
            return 1.0;
        };
        // u64 microseconds → Duration. Saturate at u64::MAX to
        // avoid overflow on a misbehaving lookup; in practice
        // lookups return wall-clock-bounded values.
        let target_rtt = Duration::from_micros(rtt_us);
        if target_rtt <= max_rtt {
            1.0
        } else {
            0.0
        }
    }

    /// Phase F slice 5 intent axis. Reads the artifact's
    /// `metadata.<intent_key>` value (default `"intent"`) and
    /// applies the configured policy:
    ///
    /// - `Disabled` → `1.0` (axis skipped).
    /// - `Strict` → look up `metadata.intent` value in
    ///   `intent_registry`. If the artifact didn't declare an
    ///   intent, score `1.0` (no constraint). If declared but
    ///   unknown to the registry, score `1.0` (forward-compat —
    ///   future intents from a newer caller don't veto on an
    ///   older substrate). If declared + known, evaluate the
    ///   intent's required-caps list against the target's
    ///   `(tags, metadata)` — `1.0` iff every requirement passes,
    ///   `0.0` if any fails.
    /// - `AnyOfLocalCapabilities` → walk every registered intent;
    ///   score `1.0` iff the target satisfies *any* intent's
    ///   required-caps list, `0.0` if none. Useful for "is this
    ///   node generally capable" gating.
    fn score_intent_axis(&self, target_caps: &CapabilitySet, artifact: &Artifact<'_>) -> f32 {
        match self.intent_match {
            IntentMatchPolicy::Disabled => 1.0,
            IntentMatchPolicy::Strict => {
                // Pull the intent value from the artifact's
                // metadata. For Daemon artifacts that's
                // `required.metadata`; for Chain / Replica
                // artifacts it's the `capabilities.metadata` map.
                let Some(intent) = artifact_intent(artifact, &self.metadata_keys.intent) else {
                    return 1.0; // No intent declared — no constraint.
                };

                // Look up the intent's required-cap list. Unknown
                // intents pass through (forward-compat).
                let Some(reqs) = self.intent_registry.lookup(intent) else {
                    return 1.0;
                };

                evaluate_required_caps(target_caps, reqs)
            }
            IntentMatchPolicy::AnyOfLocalCapabilities => {
                // CR-22: empty registry → axis-disabled (1.0), not
                // a hard veto. Pre-CR-22 this returned 0.0, which
                // multiplicatively wedged the entire cluster's
                // placement decisions for any operator who
                // selected the policy without populating
                // `intent_registry`. Operators expect "no
                // constraint expressible = pass-through" (the
                // shape every other axis here uses for the
                // empty-config case).
                if self.intent_registry.is_empty() {
                    return 1.0;
                }
                let any_satisfied = self
                    .intent_registry
                    .iter()
                    .any(|(_, reqs)| evaluate_required_caps(target_caps, reqs) >= 1.0);
                if any_satisfied {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// Phase F slice 5 colocation axis. Reads the artifact's
    /// `metadata.<colocate_with>` and `metadata.<colocate_with_strict>`
    /// values (default keys `"colocate-with"` and
    /// `"colocate-with-strict"`); each value is a chain origin hash.
    /// Matches against the target's `causal:<hash>` reserved tags.
    ///
    /// Strict semantics (`colocate-with-strict` always; OR
    /// `colocate-with` when policy is `StrictRequired`):
    /// - Target hosts the chain → 1.0.
    /// - Target doesn't host the chain → 0.0 (hard veto).
    ///
    /// Soft semantics (`colocate-with` under `SoftPreference`):
    /// - Target hosts the chain → 1.0.
    /// - Target doesn't host the chain → 0.7 (penalty boost — the
    ///   non-colocated candidate scores below a colocated one but
    ///   isn't vetoed; the multiplicative composition factors the
    ///   penalty into the final score).
    ///
    /// Disabled (`Ignore` policy or no metadata key set) → 1.0.
    ///
    /// Match logic: a `causal:<chain_hash>` tag matches the
    /// chain. Prefix-form variants (`causal:<hash>:<tip>` and
    /// `causal:<hash>[<range>]`) also match the same chain hash —
    /// per `CAPABILITY_SYSTEM_PLAN.md` §2, a tip / range
    /// announcement is implicitly a holder of the underlying
    /// chain.
    fn score_colocation_axis(&self, target_caps: &CapabilitySet, artifact: &Artifact<'_>) -> f32 {
        if self.colocation_policy == ColocationPolicy::Ignore {
            return 1.0;
        }

        // Pull both metadata values. Strict-key always vetoes;
        // soft-key's behavior depends on policy.
        let strict_chain = artifact_metadata(artifact, &self.metadata_keys.colocate_with_strict);
        let soft_chain = artifact_metadata(artifact, &self.metadata_keys.colocate_with);

        if strict_chain.is_none() && soft_chain.is_none() {
            return 1.0; // No declaration — nothing to enforce.
        }

        // Strict-key: hard veto regardless of policy. The
        // declarer explicitly opted into strict semantics by
        // using the strict key.
        if let Some(chain) = strict_chain {
            if !target_holds_chain(target_caps, chain) {
                return 0.0;
            }
        }

        // Soft-key: behavior depends on policy.
        if let Some(chain) = soft_chain {
            let does_host = target_holds_chain(target_caps, chain);
            match self.colocation_policy {
                ColocationPolicy::Ignore => unreachable!("handled above"),
                ColocationPolicy::SoftPreference => {
                    // Boost: 1.0 with chain, 0.7 without.
                    return if does_host { 1.0 } else { 0.7 };
                }
                ColocationPolicy::StrictRequired => {
                    // Policy upgrades soft-key declarations to
                    // strict semantics.
                    if !does_host {
                        return 0.0;
                    }
                }
            }
        }

        1.0
    }

    /// Phase F slice 5 resource axis. Graded fit scoring (unlike
    /// the binary axes shipped earlier) — a node with more
    /// resources scores higher than one with less. The score is
    /// soft-saturating per `value / (value + reference)`: bounded
    /// in `[0.0, 1.0]`, half at the reference value, asymptoting
    /// to 1.0 as resource grows.
    ///
    /// Branches on `resource_axis`:
    ///
    /// - `Compute` — averages per-component scores for
    ///   `hardware.cpu_cores` (reference 8 cores),
    ///   `hardware.memory_gb` (reference 16 GB),
    ///   `hardware.gpu.vram_gb` (reference 16 GB VRAM). Components
    ///   that the target doesn't advertise are skipped, and the
    ///   average is over the components that DO have data.
    /// - `Storage` — single-component score from
    ///   `dataforts.capacity_gb` (reference 1 TB).
    /// - `Both` — equal-weighted mean of `Compute` + `Storage`
    ///   scores (each computed independently).
    ///
    /// **Permissive when no data.** If the target advertises none
    /// of the relevant numeric tags, the axis returns `1.0` —
    /// "can't measure, default permissive" semantics matching the
    /// proximity axis. Operators who want strict resource enforcement
    /// declare specific minimums via the daemon's required caps
    /// (which the hard-constraint check at the top of
    /// `placement_score` enforces).
    fn score_resource_axis(&self, target_caps: &CapabilitySet, _artifact: &Artifact<'_>) -> f32 {
        match self.resource_axis {
            ResourceAxis::Compute => score_compute_axis(target_caps),
            ResourceAxis::Storage => score_storage_axis(target_caps),
            ResourceAxis::Both => {
                // N-11: track per-axis "had-data" so a no-data
                // candidate doesn't average two `1.0` placeholders
                // into a `1.0` final that ties a maxed-out
                // candidate. Pre-fix the average `(1.0 + 1.0) /
                // 2.0` let the lex-NodeId tie-breaker bias placement
                // toward (often misconfigured) lower-id peers under
                // the `Both` axis. Post-fix:
                //   - neither had data → 1.0 (still permissive)
                //   - exactly one had data → that axis's score
                //     alone (don't dilute against a permissive
                //     placeholder)
                //   - both had data → average
                let c = score_compute_axis_with_data(target_caps);
                let s = score_storage_axis_with_data(target_caps);
                match (c, s) {
                    (None, None) => 1.0,
                    (Some(score), None) | (None, Some(score)) => score,
                    (Some(cs), Some(ss)) => (cs + ss) / 2.0,
                }
            }
        }
    }

    /// Phase F slice 5 anti-affinity axis. Penalizes nodes that
    /// already lead a high fraction of locally-observed channels;
    /// load-bearing for `REDEX_DISTRIBUTED_PLAN.md`'s
    /// leader-election where uneven leadership concentration is
    /// the failure mode worth preventing.
    ///
    /// Score:
    /// - `leadership_stats: None` → 1.0 (axis disabled).
    /// - Lookup returns `None` for the target → 1.0 (no data;
    ///   permissive).
    /// - Lookup returns `Some(c)` with `c <= threshold` → 1.0
    ///   (under threshold; no penalty).
    /// - Lookup returns `Some(c)` with `c > threshold` →
    ///   `leadership_concentration_penalty` (default 0.4) — the
    ///   over-concentrated candidate is penalized but not vetoed.
    ///
    /// Penalty values are clamped to `[0.0, 1.0]` defensively;
    /// callers that misconfigure the penalty (e.g. NaN or > 1.0)
    /// can't blow up the multiplicative composition.
    fn score_anti_affinity_axis(&self, target: &NodeId) -> f32 {
        let Some(lookup) = self.leadership_stats else {
            return 1.0;
        };
        let Some(concentration) = lookup(*target) else {
            return 1.0;
        };
        // CR-10: symmetric NaN guard. The file already clamps a
        // NaN *penalty* below; without this guard a NaN
        // *concentration* would slip through `<=` (NaN compares
        // false against everything), fall into the else branch,
        // and apply the penalty even though the threshold check
        // is meaningless. Treat NaN concentration as "no data" —
        // the same shape as `lookup` returning `None`.
        if !concentration.is_finite() {
            return 1.0;
        }
        if concentration <= self.anti_affinity.leadership_concentration_threshold {
            1.0
        } else {
            // Defensive clamp — a misconfigured penalty (NaN or
            // out-of-range) shouldn't escape into the composition.
            let p = self.anti_affinity.leadership_concentration_penalty;
            if p.is_nan() {
                0.0
            } else {
                p.clamp(0.0, 1.0)
            }
        }
    }

    /// SDK Phase 7 slice 5 — custom-filter axis. Resolves
    /// [`Self::custom_filter_id`] against
    /// [`global_placement_filter_registry`](super::placement_registry::global_placement_filter_registry),
    /// invokes the registered filter, and translates its
    /// `Option<f32>` into the same shape as the in-tree axes:
    ///
    /// - `custom_filter_id: None` → `Some(1.0)` (axis disabled,
    ///   identity for the multiplicative composition).
    /// - id registered, filter returns `Some(score)` → `Some(score)`.
    /// - id registered, filter returns `None` → `None` (hard veto
    ///   propagates up).
    /// - id NOT registered → `None` + log via `eprintln!`. Operators
    ///   shouldn't reference an unset id; veto-with-log surfaces the
    ///   misconfiguration loudly rather than silently routing to
    ///   the wrong node.
    ///
    /// Returns `Option<f32>` rather than the in-tree axes' bare `f32`
    /// because a custom filter MAY hard-veto, and we want the veto to
    /// short-circuit the rest of the composition (no point computing
    /// scope / proximity / etc. when the candidate is going to be
    /// dropped). The caller in [`Self::placement_score`] does the
    /// short-circuit explicitly.
    fn score_custom_filter_axis(&self, target: &NodeId, artifact: &Artifact<'_>) -> Option<f32> {
        let Some(id) = self.custom_filter_id.as_deref() else {
            return Some(1.0);
        };
        let registry = super::placement_registry::global_placement_filter_registry();
        let Some(filter) = registry.get(id) else {
            eprintln!(
                "StandardPlacement: custom_filter_id {id:?} not registered in \
                 global_placement_filter_registry; vetoing target {target:#x}. \
                 Did the SDK call mesh.registerPlacementFilter before placement?",
            );
            return None;
        };
        // Delegate. The registered filter's `None` cascades up
        // through this `Option<f32>` and onward to
        // `placement_score`'s early return.
        filter.placement_score(target, artifact)
    }
}

/// Pull a metadata value off the artifact's variant-specific
/// `CapabilitySet` references. Used by axes that read declarative
/// hints (`metadata.intent`, `metadata.colocate-with`,
/// `metadata.colocate-with-strict`).
///
/// - `Daemon { required, optional, .. }` → checks `required.metadata`
///   first, falling back to `optional.metadata`. Required is the
///   primary declaration site; optional fallback handles the rare
///   case where the value is treated as a soft hint.
/// - `Chain { capabilities, .. }` → checks `capabilities.metadata`.
/// - `Replica { capabilities, .. }` → same as `Chain`.
fn artifact_metadata<'a>(artifact: &'a Artifact<'_>, key: &str) -> Option<&'a str> {
    let try_caps = |caps: &'a CapabilitySet| caps.metadata.get(key).map(|s| s.as_str());
    match artifact {
        Artifact::Daemon {
            required, optional, ..
        } => try_caps(required).or_else(|| try_caps(optional)),
        Artifact::Chain { capabilities, .. } => try_caps(capabilities),
        Artifact::Replica { capabilities, .. } => try_caps(capabilities),
        // `Artifact::Blob` carries no `CapabilitySet` — declarative
        // hints (intent / colocate-with) live on the chain that
        // *references* the blob, not on the blob itself. Return
        // `None` so the metadata-driven axes (intent / colocation)
        // are no-ops for blob placement; the substrate's existing
        // scope / proximity / disk-free axes do the real work.
        Artifact::Blob { .. } => None,
    }
}

/// Backward-compat alias for the intent axis. Pre-slice-5 helper
/// name; slice 5 generalized it to `artifact_metadata` so the
/// colocation axis can reuse the same lookup. Kept to minimize
/// the slice 5 intent-axis diff churn.
fn artifact_intent<'a>(artifact: &'a Artifact<'_>, intent_key: &str) -> Option<&'a str> {
    artifact_metadata(artifact, intent_key)
}

/// Find an `<axis>.<key>=<value>` tag on the target and parse its
/// value as `f64`. Returns `None` if the tag is absent, the tag
/// has no value (presence-form), or the value doesn't parse.
///
/// Used by the resource axis (slice 5) to read numeric capacity
/// declarations off the target's `CapabilitySet`.
///
/// N-12: range-check the parsed f64 against a sane sentinel
/// (`[0.0, MAX_RESOURCE_VALUE]`) before passing it back. A
/// malformed peer announcement like `hardware.cpu_cores=1e308`
/// parses as a finite f64 but, when downcast to f32 in the resource
/// axis, saturates to `f32::INFINITY` — the CR-9 NaN/inf guard
/// then clamps it to 0.0 and the candidate is silently downscored
/// to "looks like a bad fit" when the tag was simply absurd. By
/// returning `None` for out-of-range values we treat the tag as
/// "no data" and fall through to the permissive identity path,
/// which is what an operator would expect for a malformed input.
fn target_axis_value_numeric(caps: &CapabilitySet, axis: TaxonomyAxis, key: &str) -> Option<f64> {
    caps.tags.iter().find_map(|t| match t {
        Tag::AxisValue {
            axis: a,
            key: k,
            value,
            ..
        } if *a == axis && k == key => {
            let v = value.parse::<f64>().ok()?;
            if !(0.0..=MAX_RESOURCE_VALUE).contains(&v) || !v.is_finite() {
                return None;
            }
            Some(v)
        }
        _ => None,
    })
}

/// N-12: largest sane resource-axis numeric value (1e15). Higher
/// values are below the f64 → f32 overflow threshold but well above
/// any plausible real-world capacity: 1 PB of memory, 1 ZB of
/// storage. A peer announcing past this bound is malformed; we
/// drop the value rather than letting it poison the score.
const MAX_RESOURCE_VALUE: f64 = 1e15;

/// Soft-saturating score: `value / (value + reference)`.
/// Bounded in `[0.0, 1.0]`; half at `value == reference`;
/// asymptotes to 1.0 as value grows. Returns 0.0 for non-positive
/// or non-finite inputs (defensive against malformed numeric tags).
///
/// CR-9: `f64::from_str` parses `"NaN"` / `"+inf"` / `"-inf"`
/// successfully, and `Tag::AxisValue` stores the raw string. A
/// single `hardware.cpu_cores=NaN` would otherwise pass the
/// `value <= 0.0` check (NaN is not <= anything), producing
/// `value / (value + ref) = NaN` that propagates through
/// `score_compute_axis`'s sum and gets clamped to 0.0 by
/// `compose_axis_scores` — silently vetoing the candidate. Add
/// the `is_finite()` guard so the NaN never enters the chain.
fn saturating_score(value: f32, reference: f32) -> f32 {
    if value <= 0.0 || reference <= 0.0 || !value.is_finite() || !reference.is_finite() {
        return 0.0;
    }
    value / (value + reference)
}

/// Compute the Compute resource score: averages saturating
/// per-component scores for the three core compute resources.
/// Returns 1.0 if the target advertises none of them
/// ("permissive when no data").
fn score_compute_axis(caps: &CapabilitySet) -> f32 {
    let mut sum = 0.0_f32;
    let mut count = 0u32;
    if let Some(c) = target_axis_value_numeric(caps, TaxonomyAxis::Hardware, "cpu_cores") {
        // Reference: 8 cores — half at 8, → 1 as cores grow.
        sum += saturating_score(c as f32, 8.0);
        count += 1;
    }
    if let Some(m) = target_axis_value_numeric(caps, TaxonomyAxis::Hardware, "memory_gb") {
        // Reference: 16 GB.
        sum += saturating_score(m as f32, 16.0);
        count += 1;
    }
    if let Some(v) = target_axis_value_numeric(caps, TaxonomyAxis::Hardware, "gpu.vram_gb") {
        // Reference: 16 GB VRAM.
        sum += saturating_score(v as f32, 16.0);
        count += 1;
    }
    if count == 0 {
        return 1.0;
    }
    sum / count as f32
}

/// Compute the Storage resource score: saturating score on
/// `dataforts.capacity_gb`. Returns 1.0 if the tag is absent.
fn score_storage_axis(caps: &CapabilitySet) -> f32 {
    if let Some(s) = target_axis_value_numeric(caps, TaxonomyAxis::Dataforts, "capacity_gb") {
        // Reference: 1 TB.
        saturating_score(s as f32, 1000.0)
    } else {
        1.0
    }
}

/// N-11: variant of [`score_compute_axis`] that reports whether the
/// target carried ANY of the compute-axis tags. `None` ↔ "no data,
/// don't dilute the resource composition"; `Some(score)` ↔ at least
/// one tag was found and contributed. Used by `score_resource_axis`'s
/// `Both` branch so a no-data candidate doesn't tie a maxed-out one
/// by averaging two permissive `1.0` placeholders.
fn score_compute_axis_with_data(caps: &CapabilitySet) -> Option<f32> {
    let mut sum = 0.0_f32;
    let mut count = 0u32;
    if let Some(c) = target_axis_value_numeric(caps, TaxonomyAxis::Hardware, "cpu_cores") {
        sum += saturating_score(c as f32, 8.0);
        count += 1;
    }
    if let Some(m) = target_axis_value_numeric(caps, TaxonomyAxis::Hardware, "memory_gb") {
        sum += saturating_score(m as f32, 16.0);
        count += 1;
    }
    if let Some(v) = target_axis_value_numeric(caps, TaxonomyAxis::Hardware, "gpu.vram_gb") {
        sum += saturating_score(v as f32, 16.0);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some(sum / count as f32)
}

/// N-11: storage-axis sibling of [`score_compute_axis_with_data`].
fn score_storage_axis_with_data(caps: &CapabilitySet) -> Option<f32> {
    let s = target_axis_value_numeric(caps, TaxonomyAxis::Dataforts, "capacity_gb")?;
    Some(saturating_score(s as f32, 1000.0))
}

/// Does the target host the named chain? Walks the target's
/// `causal:*` reserved tags and matches the chain-hash prefix
/// against `chain_hash`.
///
/// Match semantics (per `CAPABILITY_SYSTEM_PLAN.md` §2):
/// - `causal:<hash>` (presence-form) → exact match on the hash.
/// - `causal:<hash>:<tip_seq>` (tip-form) → matches; the peer
///   announcing a tip implicitly holds the chain prefix.
/// - `causal:<hash>[<range>]` (range-form) → matches; same
///   reasoning.
fn target_holds_chain(target_caps: &CapabilitySet, chain_hash: &str) -> bool {
    target_caps.tags.iter().any(|t| match t {
        Tag::Reserved { prefix, body } if prefix.as_str() == "causal:" => {
            // Body is `<hash>` / `<hash>:<tip>` / `<hash>[<range>]`.
            // The hash extends until the first `:` or `[`; before
            // that boundary it's the chain id.
            let hash_end = body.find([':', '[']).unwrap_or(body.len());
            &body[..hash_end] == chain_hash
        }
        _ => false,
    })
}

/// Evaluate a list of `RequiredCapability` checks against a target's
/// `(tags, metadata)`. Returns `1.0` iff every requirement passes,
/// `0.0` if any fails. Used by the intent axis (and reused by
/// future axes that need the same all-pass-or-fail semantics).
fn evaluate_required_caps(target_caps: &CapabilitySet, reqs: &[RequiredCapability]) -> f32 {
    if reqs.is_empty() {
        return 1.0;
    }
    // Materialize tags into a Vec so EvalContext can borrow a slice.
    let tags: Vec<Tag> = target_caps.tags.iter().cloned().collect();
    let ctx =
        crate::adapter::net::behavior::predicate::EvalContext::new(&tags, &target_caps.metadata);
    if reqs.iter().all(|r| r.evaluate(&ctx)) {
        1.0
    } else {
        0.0
    }
}

// =============================================================================
// IntentRegistry (slice 3) — `intent` metadata key → required-caps lookup.
//
// Phase F slice 3 of `CAPABILITY_SYSTEM_PLAN.md`. Drives the intent
// axis (axis #3) of `StandardPlacement` once slice 5 wires it up:
// look up `metadata.intent` value → list of `RequiredCapability` →
// evaluate each against the candidate's `(tags, metadata)` →
// boolean satisfies-all.
//
// `BTreeMap` for deterministic iteration order. O(log n) lookup
// keeps the placement hot path bounded; benchmark target is ≤ 10 µs
// scoring overhead at 1000 registered intents (pinned in F.5 perf
// tests).
// =============================================================================

/// Intent → required-capabilities lookup. Built via
/// [`IntentRegistry::defaults`] for the substrate-shipped baseline
/// or [`IntentRegistry::new`] for an empty registry; extended via
/// [`IntentRegistry::register`].
///
/// Lookups via [`IntentRegistry::lookup`] return a borrowed slice;
/// callers iterate and evaluate each `RequiredCapability` against
/// the candidate's `EvalContext`.
#[derive(Debug, Clone, Default)]
pub struct IntentRegistry {
    map: BTreeMap<String, Vec<RequiredCapability>>,
}

impl IntentRegistry {
    /// Empty registry. Use [`Self::register`] to add intents.
    pub fn new() -> Self {
        Self::default()
    }

    /// Substrate-shipped baseline mappings. Adapted to the current
    /// `CAPABILITIES_SCHEMA.md`-validated key set; future schema
    /// extensions land additional intents here.
    ///
    /// Defaults:
    ///
    /// - `ml-training` — `hardware.gpu` present + `hardware.gpu.vram_gb >= 24`.
    /// - `inference` — `hardware.gpu` present + any `software.model.*` tag
    ///   (axis-key match — version / quantization independent).
    /// - `cpu-bound` — `hardware.cpu_cores >= 4`.
    /// - `sensor-telemetry` — any `devices.*` tag (axis-any match).
    pub fn defaults() -> Self {
        let mut map: BTreeMap<String, Vec<RequiredCapability>> = BTreeMap::new();

        // ml-training: GPU present + at least 24 GB VRAM.
        map.insert(
            "ml-training".to_string(),
            vec![
                RequiredCapability::Tag(Tag::AxisPresent {
                    axis: TaxonomyAxis::Hardware,
                    key: "gpu".to_string(),
                }),
                RequiredCapability::Predicate(Predicate::numeric_at_least(
                    TagKey::new(TaxonomyAxis::Hardware, "gpu.vram_gb"),
                    24.0,
                )),
            ],
        );

        // inference: GPU present + any software.model.* tag
        // (caller doesn't care about specific model — version /
        // quant orthogonal to the placement decision).
        map.insert(
            "inference".to_string(),
            vec![
                RequiredCapability::Tag(Tag::AxisPresent {
                    axis: TaxonomyAxis::Hardware,
                    key: "gpu".to_string(),
                }),
                RequiredCapability::AxisKey(TagKey::new(
                    TaxonomyAxis::Software,
                    "model".to_string(),
                )),
            ],
        );

        // cpu-bound: at least 4 CPU cores.
        map.insert(
            "cpu-bound".to_string(),
            vec![RequiredCapability::Predicate(Predicate::numeric_at_least(
                TagKey::new(TaxonomyAxis::Hardware, "cpu_cores"),
                4.0,
            ))],
        );

        // sensor-telemetry: any devices.* tag (devices axis empty in
        // schema today; once the schema enumerates concrete devices
        // keys, tighten to specific tags).
        map.insert(
            "sensor-telemetry".to_string(),
            vec![RequiredCapability::AxisAny(TaxonomyAxis::Devices)],
        );

        Self { map }
    }

    /// Register (or replace) an intent's required-capability list.
    /// Returns the previous mapping, if any — useful for migrations
    /// where applications layer their own intents on top of the
    /// substrate defaults.
    pub fn register(
        &mut self,
        intent: impl Into<String>,
        requirements: Vec<RequiredCapability>,
    ) -> Option<Vec<RequiredCapability>> {
        self.map.insert(intent.into(), requirements)
    }

    /// Borrow the requirements list for `intent`. Returns `None`
    /// for intents the registry doesn't recognize (caller chooses
    /// `Disabled` / `AnyOfLocalCapabilities` / `Strict` reaction
    /// per [`IntentMatchPolicy`]).
    pub fn lookup(&self, intent: &str) -> Option<&[RequiredCapability]> {
        self.map.get(intent).map(|v| v.as_slice())
    }

    /// Iterate `(intent, requirements)` pairs in lex order. Used by
    /// the `IntentMatchPolicy::AnyOfLocalCapabilities` evaluator
    /// (slice 5) to pick whichever intent the candidate satisfies.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[RequiredCapability])> {
        self.map.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// Number of registered intents.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True iff zero intents are registered.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// =============================================================================
// Tie-breaker (slice 4) — `CAPABILITY_SYSTEM_PLAN.md` §7 LOCKED
// three-step ordering: RTT → free-resource → lexicographic NodeId.
//
// `tie_break_compare(a, b, ctx)` is the resolver. Used by the
// scheduler (Phase G slice 1) when `placement_score` returns equal
// `f32` values across multiple candidates; ALSO usable as a primary
// comparator after dropping the score axis (e.g. when
// `LegacyPlacement` returns `Some(1.0)` for everyone).
//
// Pinned property: deterministic across runs. Replaces the legacy
// `partial_cmp(...).unwrap_or(Ordering::Equal)` non-determinism in
// `compute/scheduler.rs` once Phase G slice 1 swaps call sites.
// =============================================================================

/// Closure-based RTT lookup. Returns microsecond-resolution RTT for
/// a given candidate, or `None` if not measured. Decouples the
/// placement module from the proximity graph's internal node-id
/// representation (`[u8; 32]`) — scheduler call sites that have
/// the bridge available wire a `|u64| -> Option<u64>` closure that
/// translates and consults the graph.
///
/// Trait alias rather than free Fn so the context struct can hold
/// a `&dyn` to it without dragging Fn lifetimes through.
pub trait RttLookup: Fn(NodeId) -> Option<u64> + Sync {}
impl<F: Fn(NodeId) -> Option<u64> + Sync> RttLookup for F {}

/// Closure-based leadership-concentration stats lookup. Returns
/// the candidate's leadership-concentration ratio in `[0.0, 1.0]`
/// — fraction of locally-observed channels for which the
/// candidate is currently the leader. The substrate exposes the
/// raw ratio elsewhere; the placement module stays decoupled via
/// this closure.
///
/// `None` return = "no data for this candidate" (treated as
/// permissive by the anti-affinity axis); `Some(0.0)` = "leads
/// nothing"; `Some(1.0)` = "leads every observed channel."
///
/// Trait alias mirroring `RttLookup` so both lookups have parallel
/// builder + storage shape.
pub trait LeadershipStatsLookup: Fn(NodeId) -> Option<f32> + Sync {}
impl<F: Fn(NodeId) -> Option<f32> + Sync> LeadershipStatsLookup for F {}

/// Inputs the tie-breaker reads.
///
/// `rtt_lookup` is optional: scheduler call sites without proximity
/// data (e.g. early bootstrap, tests) pass `None` and step 1 falls
/// through. `index` is required for step 2 — free-resource lookup
/// (slice 5 fills in; slice 4 stubs to 0 so step 2 always falls
/// through to step 3).
pub struct TieBreakContext<'a> {
    /// Optional callback that resolves a `NodeId` to its
    /// microsecond-RTT estimate. Scheduler integrations wire this
    /// with their proximity-graph bridge; `None` skips step 1.
    pub rtt_lookup: Option<&'a dyn RttLookup>,
    /// Which resource pool the free-resource step scores. Mirrors
    /// `StandardPlacement::resource_axis`.
    pub resource_axis: ResourceAxis,
}

/// Three-step tie-break comparator. LOCKED ordering per
/// `CAPABILITY_SYSTEM_PLAN.md` §7 "Tie-breaking":
///
/// 1. **Lower RTT wins.** Looked up via `ProximityGraph` if
///    available. A candidate with no RTT entry sorts AFTER one
///    that has data (data > no-data).
/// 2. **Higher free resource wins.** Slice 4 stubs both candidates
///    to `0` so step 2 falls through; slice 5 fills in based on
///    `ctx.resource_axis`.
/// 3. **Lexicographic `NodeId`.** Final deterministic fallback;
///    eliminates the non-determinism the legacy
///    `partial_cmp(...).unwrap_or(Ordering::Equal)` exhibited.
///
/// Returns `Ordering::Less` when `a` is *better* than `b` — sort
/// the candidates with `Vec::sort_by` and the best lands first.
/// Comparing `a` against itself returns `Ordering::Equal`.
pub fn tie_break_compare(a: NodeId, b: NodeId, ctx: &TieBreakContext<'_>) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }

    // Step 1: RTT — lower wins. Lookup is delegated to the
    // caller-provided closure so the placement module stays
    // decoupled from the proximity graph's internal node-id shape;
    // missing entries sort after present entries (so an unreached
    // candidate doesn't accidentally win on a 0 fallback).
    if let Some(lookup) = ctx.rtt_lookup {
        let a_rtt = lookup(a);
        let b_rtt = lookup(b);
        match (a_rtt, b_rtt) {
            (Some(ar), Some(br)) if ar != br => return ar.cmp(&br),
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            _ => {} // both Some-equal or both None → fall through.
        }
    }

    // Step 2: free resource — higher wins. Slice 4 stub returns 0
    // for both; slice 5 wires per-axis lookups. Note: descending
    // order, so `b_free.cmp(&a_free)`.
    let a_free = free_resource_for(a, ctx);
    let b_free = free_resource_for(b, ctx);
    if a_free != b_free {
        return b_free.cmp(&a_free);
    }

    // Step 3: lex NodeId fallback. Always produces a strict ordering
    // for distinct NodeIds.
    a.cmp(&b)
}

/// Free-resource lookup. Slice 4 stub returns 0 for every candidate
/// — step 2 of `tie_break_compare` falls through to step 3 in this
/// state. Slice 5 reads the actual free-resource tags off the
/// candidate's `CapabilitySet`:
///
/// - `ResourceAxis::Compute` → free RAM (or VRAM if the artifact
///   required GPU; threaded via the artifact, not via this
///   helper).
/// - `ResourceAxis::Storage` → `dataforts.free_storage_gb`.
/// - `ResourceAxis::Both` → weighted average (axis-side decision).
fn free_resource_for(_node: NodeId, _ctx: &TieBreakContext<'_>) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
    use crate::adapter::net::identity::EntityId;
    use std::sync::Arc;

    /// Build an `Arc<Fold<CapabilityFold>>` populated with the
    /// supplied nodes via the legacy-announcement bridge. Mirrors
    /// the production cap-ann path; tests that need a re-entrant
    /// `CapabilityIndex` for legacy-lock regression coverage build
    /// one alongside.
    fn index_with(nodes: &[(NodeId, CapabilitySet)]) -> Arc<Fold<CapabilityFold>> {
        let fold: Arc<Fold<CapabilityFold>> =
            Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
        let eid = EntityId::from_bytes([0u8; 32]);
        for (node_id, caps) in nodes {
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(*node_id, eid.clone(), 1, caps.clone()),
            );
        }
        fold
    }

    fn empty_caps() -> CapabilitySet {
        CapabilitySet::default()
    }

    fn daemon_artifact<'a>(
        required: &'a CapabilitySet,
        optional: &'a CapabilitySet,
    ) -> Artifact<'a> {
        Artifact::Daemon {
            daemon_id: [0u8; 32],
            required,
            optional,
        }
    }

    /// `LegacyPlacement::permissive` scores every node in the index
    /// at 1.0 — matches the pre-Phase-F "any node matching
    /// CapabilityFilter::default() is eligible" contract.
    #[test]
    fn legacy_permissive_scores_all_indexed_nodes() {
        let fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let filter = LegacyPlacement::permissive(&fold);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        assert_eq!(filter.placement_score(&0x1111, &artifact), Some(1.0));
        assert_eq!(filter.placement_score(&0x2222, &artifact), Some(1.0));
    }

    /// Unknown candidate (not in the capability index) → `None`.
    /// Pinned: the trait contract says `None` is a hard veto, and
    /// the scheduler must short-circuit candidates that fail
    /// here.
    #[test]
    fn legacy_returns_none_for_unindexed_candidate() {
        let fold = index_with(&[(0x1111, empty_caps())]);
        let filter = LegacyPlacement::permissive(&fold);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        assert_eq!(filter.placement_score(&0xDEAD, &artifact), None);
    }

    /// A non-default `CapabilityFilter` filters out candidates that
    /// don't match — matches the existing
    /// `Scheduler::find_migration_targets` semantic where the
    /// daemon's filter is consulted before placement.
    #[test]
    fn legacy_filter_vetoes_non_matching_candidates() {
        let caps_with_tag = empty_caps().add_tag("hardware.gpu");
        let fold = index_with(&[(0x1111, caps_with_tag), (0x2222, empty_caps())]);

        let required = CapabilityFilter::default().require_tag("hardware.gpu".to_string());

        let filter = LegacyPlacement::new(required, &fold);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        assert_eq!(filter.placement_score(&0x1111, &artifact), Some(1.0));
        assert_eq!(
            filter.placement_score(&0x2222, &artifact),
            None,
            "candidate without the required tag must veto via None"
        );
    }

    /// Trait-object usage compiles cleanly. Pinned because the
    /// scheduler will hold a `&dyn PlacementFilter` and a regression
    /// here (e.g. a non-object-safe method added later) would break
    /// the substrate.
    #[test]
    fn placement_filter_is_dyn_compatible() {
        let fold = index_with(&[(0x1111, empty_caps())]);
        let filter = LegacyPlacement::permissive(&fold);
        let dyn_filter: &dyn PlacementFilter = &filter;
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        let _ = dyn_filter.placement_score(&0x1111, &artifact);
    }

    /// Send + Sync are part of the trait bound — pinned at
    /// compile-time so a non-Send field added to the trait by a
    /// future patch fails build, not just at the call site.
    #[test]
    fn placement_filter_requires_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<&dyn PlacementFilter>();
    }

    // ====================================================================
    // Phase F slice 2: composition + StandardPlacement
    // ====================================================================

    /// Pin §7 LOCKED: empty input returns 1.0 (identity for
    /// multiplication). Default `StandardPlacement` config produces
    /// the same observable behavior as `LegacyPlacement::permissive`.
    #[test]
    fn compose_empty_returns_one() {
        assert_eq!(compose_axis_scores(std::iter::empty::<f32>()), 1.0);
    }

    /// Pin §7 LOCKED: "0.0 anywhere → 0.0 final." A single 0.0 in
    /// the input zeros out the final score regardless of other
    /// axes' values.
    #[test]
    fn compose_zero_anywhere_zeroes_final_score() {
        assert_eq!(compose_axis_scores([1.0, 1.0, 0.0, 1.0]), 0.0);
        assert_eq!(compose_axis_scores([0.0, 0.5, 0.7]), 0.0);
        assert_eq!(compose_axis_scores([0.5, 0.7, 0.0]), 0.0);
    }

    /// Multiplicative composition: [0.5, 0.5, 0.5] → 0.125.
    #[test]
    fn compose_multiplies_per_axis_scores() {
        let got = compose_axis_scores([0.5, 0.5, 0.5]);
        assert!((got - 0.125).abs() < 1e-6, "got {got}, want 0.125");
    }

    /// Out-of-range / NaN inputs are clamped, not panic-causing.
    /// Pin: a buggy axis returning -0.5 or NaN doesn't blow up the
    /// composition; downstream callers see a sane in-range result.
    #[test]
    fn compose_clamps_out_of_range_and_nan_inputs() {
        // -0.5 clamps to 0.0 → 0.0 final.
        assert_eq!(compose_axis_scores([1.0, -0.5, 1.0]), 0.0);
        // 1.5 clamps to 1.0 → identity.
        assert_eq!(compose_axis_scores([1.0, 1.5, 1.0]), 1.0);
        // NaN clamps to 0.0 → 0.0 final.
        assert_eq!(compose_axis_scores([1.0, f32::NAN, 1.0]), 0.0);
    }

    /// `StandardPlacement::new` with default config returns 1.0 for
    /// any indexed candidate satisfying the artifact's required
    /// caps. Mirrors `LegacyPlacement::permissive` — the migration
    /// from `Legacy*` to `Standard*` is observation-preserving when
    /// no axes are configured.
    #[test]
    fn standard_default_config_matches_legacy_permissive() {
        let fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let placement = StandardPlacement::new(&fold);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        assert_eq!(placement.placement_score(&0x1111, &artifact), Some(1.0));
        assert_eq!(placement.placement_score(&0x2222, &artifact), Some(1.0));
    }

    /// Unindexed candidate → `None`. Same hard-veto contract as
    /// `LegacyPlacement`.
    #[test]
    fn standard_returns_none_for_unindexed_target() {
        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        assert_eq!(placement.placement_score(&0xDEAD, &artifact), None);
    }

    /// `Daemon` artifact carrying a non-empty `required` set: the
    /// scorer vetoes (returns `None`) any candidate whose tag set
    /// doesn't include all required tags. Pin the hard-constraint
    /// check that lets Phase G's scheduler short-circuit
    /// candidate enumeration.
    #[test]
    fn standard_vetoes_candidates_missing_required_caps() {
        let caps_with_gpu = empty_caps().add_tag("hardware.gpu");
        let fold = index_with(&[(0x1111, caps_with_gpu), (0x2222, empty_caps())]);
        let placement = StandardPlacement::new(&fold);

        let required = empty_caps().add_tag("hardware.gpu");
        let opt = empty_caps();
        let artifact = daemon_artifact(&required, &opt);

        assert_eq!(placement.placement_score(&0x1111, &artifact), Some(1.0));
        assert_eq!(
            placement.placement_score(&0x2222, &artifact),
            None,
            "candidate without required hardware.gpu must veto"
        );
    }

    /// Builder API round-trips: `with_*` setters populate the
    /// public fields. Pin so a refactor adding a field to
    /// `StandardPlacement` requires updating every setter.
    #[test]
    fn standard_builder_setters_populate_fields() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold)
            .with_scope_filter(vec![ScopeLabel::new("scope:tenant:foo")])
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_colocation_policy(ColocationPolicy::SoftPreference)
            .with_resource_axis(ResourceAxis::Storage);

        assert_eq!(placement.scope_filter.as_ref().unwrap().len(), 1);
        assert_eq!(
            placement.scope_filter.as_ref().unwrap()[0].as_str(),
            "scope:tenant:foo"
        );
        assert_eq!(placement.proximity_max_rtt, Some(Duration::from_millis(50)));
        assert!(matches!(placement.intent_match, IntentMatchPolicy::Strict));
        assert_eq!(
            placement.colocation_policy,
            ColocationPolicy::SoftPreference
        );
        assert_eq!(placement.resource_axis, ResourceAxis::Storage);
    }

    /// Sensible defaults. Pin so adding a new config field with a
    /// sane default in the future doesn't accidentally land with
    /// an axis-disabling 0.0.
    #[test]
    fn standard_default_axis_configs_disable_each_axis() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold);
        assert!(placement.scope_filter.is_none());
        assert!(placement.proximity_max_rtt.is_none());
        assert!(matches!(
            placement.intent_match,
            IntentMatchPolicy::Disabled
        ));
        assert_eq!(placement.colocation_policy, ColocationPolicy::Ignore);
    }

    // ====================================================================
    // Phase F slice 3: IntentRegistry
    // ====================================================================

    /// Substrate-shipped defaults must include the four documented
    /// intents — pin so adding / removing one fails CI rather than
    /// silently changing the baseline.
    #[test]
    fn intent_registry_defaults_include_all_baseline_intents() {
        let r = IntentRegistry::defaults();
        assert!(r.lookup("ml-training").is_some());
        assert!(r.lookup("inference").is_some());
        assert!(r.lookup("cpu-bound").is_some());
        assert!(r.lookup("sensor-telemetry").is_some());
        assert_eq!(r.len(), 4);
    }

    /// `defaults()` for `ml-training` requires both GPU presence
    /// and the 24 GB VRAM threshold — pinning the *minimum* shape
    /// downstream callers can rely on.
    #[test]
    fn intent_registry_defaults_ml_training_requires_gpu_plus_vram_threshold() {
        let r = IntentRegistry::defaults();
        let reqs = r.lookup("ml-training").expect("ml-training present");
        assert_eq!(reqs.len(), 2, "ml-training requires gpu + vram threshold");

        let has_gpu_tag = reqs.iter().any(|rc| {
            matches!(
                rc,
                RequiredCapability::Tag(Tag::AxisPresent { axis, key })
                    if *axis == TaxonomyAxis::Hardware && key == "gpu"
            )
        });
        assert!(has_gpu_tag, "ml-training must include hardware.gpu tag");

        let has_vram_predicate = reqs
            .iter()
            .any(|rc| matches!(rc, RequiredCapability::Predicate(_)));
        assert!(
            has_vram_predicate,
            "ml-training must include vram numeric predicate"
        );
    }

    /// `register` adds a new intent; subsequent `lookup` returns it.
    #[test]
    fn intent_registry_register_adds_lookup() {
        let mut r = IntentRegistry::new();
        assert!(r.is_empty());
        let prev = r.register(
            "custom-intent",
            vec![RequiredCapability::AxisAny(TaxonomyAxis::Hardware)],
        );
        assert!(prev.is_none());
        assert_eq!(r.len(), 1);
        assert_eq!(r.lookup("custom-intent").expect("just registered").len(), 1);
    }

    /// `register` of an existing intent returns the old mapping —
    /// useful for application-side overrides of substrate defaults.
    #[test]
    fn intent_registry_register_replaces_returns_previous() {
        let mut r = IntentRegistry::defaults();
        let prev = r.register(
            "ml-training",
            vec![RequiredCapability::AxisAny(TaxonomyAxis::Software)],
        );
        assert!(prev.is_some(), "previous mapping returned");
        assert_eq!(prev.unwrap().len(), 2, "old ml-training had 2 requirements");
        // After replace, the new mapping is in place.
        assert_eq!(r.lookup("ml-training").expect("re-registered").len(), 1);
    }

    /// Unknown intent → `None`.
    #[test]
    fn intent_registry_lookup_unknown_returns_none() {
        let r = IntentRegistry::defaults();
        assert!(r.lookup("nonexistent-intent").is_none());
    }

    /// `iter` returns intents in lex order (BTreeMap iteration
    /// semantics) — pinned so dependent code (`AnyOfLocalCapabilities`
    /// evaluator in slice 5) gets a deterministic match order.
    #[test]
    fn intent_registry_iter_is_lex_ordered() {
        let mut r = IntentRegistry::new();
        r.register("zebra", vec![]);
        r.register("alpha", vec![]);
        r.register("middle", vec![]);
        let names: Vec<&str> = r.iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    /// `defaults` evaluation contract: `ml-training`'s requirements
    /// are satisfied by a candidate carrying gpu + 32 GB VRAM but
    /// not by one with just gpu (no VRAM threshold tag).
    #[test]
    fn intent_registry_defaults_evaluation_round_trip() {
        use crate::adapter::net::behavior::predicate::EvalContext;
        let r = IntentRegistry::defaults();
        let reqs = r.lookup("ml-training").unwrap();

        // Candidate A: GPU + 32 GB VRAM — satisfies all.
        let satisfying_caps = empty_caps()
            .add_tag("hardware.gpu")
            .add_tag("hardware.gpu.vram_gb=32");
        let tags_a: Vec<Tag> = satisfying_caps.tags.iter().cloned().collect();
        let meta_a = satisfying_caps.metadata.clone();
        let ctx_a = EvalContext::new(&tags_a, &meta_a);
        assert!(
            reqs.iter().all(|rc| rc.evaluate(&ctx_a)),
            "candidate with gpu + 32GB vram satisfies ml-training"
        );

        // Candidate B: just GPU, no VRAM tag — VRAM predicate fails
        // because the tag is absent.
        let partial_caps = empty_caps().add_tag("hardware.gpu");
        let tags_b: Vec<Tag> = partial_caps.tags.iter().cloned().collect();
        let meta_b = partial_caps.metadata.clone();
        let ctx_b = EvalContext::new(&tags_b, &meta_b);
        assert!(
            !reqs.iter().all(|rc| rc.evaluate(&ctx_b)),
            "candidate without vram threshold doesn't satisfy ml-training"
        );
    }

    // ====================================================================
    // Phase F slice 4: tie-breaker
    // ====================================================================

    /// Helper: synthetic RTT lookup with a static map. Decouples
    /// tie-breaker tests from the proximity graph; substrate
    /// scheduler integrations supply a real closure that consults
    /// `ProximityGraph` (post-Phase-G when the u64↔[u8;32] bridge
    /// is wired).
    fn rtt_map(entries: &'static [(NodeId, u64)]) -> impl Fn(NodeId) -> Option<u64> + Sync {
        |id: NodeId| entries.iter().find(|(n, _)| *n == id).map(|(_, rtt)| *rtt)
    }

    /// Step 1 — RTT: lower wins. With both candidates having RTT
    /// data, the comparator picks the lower-latency one as `Less`
    /// (i.e. better, sorts first).
    #[test]
    fn tie_break_step_1_rtt_lower_wins() {
        let _fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let lookup = rtt_map(&[(0x1111, 5_000), (0x2222, 50_000)]);
        let ctx = TieBreakContext {
            rtt_lookup: Some(&lookup),
            resource_axis: ResourceAxis::Compute,
        };
        assert_eq!(tie_break_compare(0x1111, 0x2222, &ctx), Ordering::Less);
        assert_eq!(tie_break_compare(0x2222, 0x1111, &ctx), Ordering::Greater);
    }

    /// Step 1 short-circuit: a candidate with RTT data sorts before
    /// one without. Pin so an unreached peer doesn't win on a 0
    /// fallback.
    #[test]
    fn tie_break_step_1_present_rtt_beats_missing() {
        let _fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let lookup = rtt_map(&[(0x1111, 10_000)]);
        let ctx = TieBreakContext {
            rtt_lookup: Some(&lookup),
            resource_axis: ResourceAxis::Compute,
        };
        assert_eq!(
            tie_break_compare(0x1111, 0x2222, &ctx),
            Ordering::Less,
            "candidate with RTT data must sort before one without",
        );
        assert_eq!(tie_break_compare(0x2222, 0x1111, &ctx), Ordering::Greater,);
    }

    /// `rtt_lookup = None` skips step 1 — falls through to step 2
    /// (slice 4 stub) → step 3 (lex NodeId fallback).
    #[test]
    fn tie_break_falls_through_to_lex_node_id_when_no_rtt_lookup() {
        let _fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let ctx = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };
        // 0x1111 < 0x2222 lexicographically → 0x1111 wins.
        assert_eq!(tie_break_compare(0x1111, 0x2222, &ctx), Ordering::Less);
        assert_eq!(tie_break_compare(0x2222, 0x1111, &ctx), Ordering::Greater);
    }

    /// Identity: comparing a candidate to itself returns Equal.
    #[test]
    fn tie_break_self_compare_is_equal() {
        let _fold = index_with(&[(0x1111, empty_caps())]);
        let ctx = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };
        assert_eq!(tie_break_compare(0x1111, 0x1111, &ctx), Ordering::Equal);
    }

    /// Determinism: comparing the same pair twice produces the same
    /// answer. Pin so a future free-resource impl that uses
    /// non-deterministic data (e.g. random sample) must snapshot.
    #[test]
    fn tie_break_is_deterministic_across_repeated_calls() {
        let _fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let lookup = rtt_map(&[(0x1111, 5_000), (0x2222, 50_000)]);
        let ctx = TieBreakContext {
            rtt_lookup: Some(&lookup),
            resource_axis: ResourceAxis::Compute,
        };
        let first = tie_break_compare(0x1111, 0x2222, &ctx);
        for _ in 0..16 {
            assert_eq!(tie_break_compare(0x1111, 0x2222, &ctx), first);
        }
    }

    /// Step 2 stub returns 0 for both candidates → step 3 (lex
    /// NodeId) resolves equal-RTT pairs. Pin via a lookup that
    /// reports identical RTT for two distinct candidates.
    #[test]
    fn tie_break_equal_rtt_falls_through_to_lex_node_id() {
        let _fold = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let lookup = rtt_map(&[(0x1111, 10_000), (0x2222, 10_000)]);
        let ctx = TieBreakContext {
            rtt_lookup: Some(&lookup),
            resource_axis: ResourceAxis::Compute,
        };
        assert_eq!(tie_break_compare(0x1111, 0x2222, &ctx), Ordering::Less);
    }

    /// `ProximityGraph::nearest_rtt` returns the lowest-RTT node
    /// matching the predicate. Pin the proximity-API contract
    /// slice 5's scope-attraction scoring depends on. Uses the
    /// proximity graph directly (the API lives there, separate
    /// from the placement tie-breaker).
    #[test]
    fn nearest_rtt_returns_lowest_matching_peer() {
        use crate::adapter::net::behavior::proximity::{
            EnhancedPingwave, ProximityConfig, ProximityGraph,
        };
        use std::net::SocketAddr;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn now_us() -> u64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64
        }
        fn nid(n: u8) -> [u8; 32] {
            let mut id = [0u8; 32];
            id[0] = n;
            id
        }

        let my = nid(0xFF);
        let graph = ProximityGraph::new(my, ProximityConfig::default());
        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let now = now_us();

        let mut pw_a = EnhancedPingwave::new(nid(1), 1, 3);
        pw_a.origin_timestamp_us = now.saturating_sub(5_000);
        graph.on_pingwave(pw_a, addr);

        let mut pw_b = EnhancedPingwave::new(nid(2), 1, 3);
        pw_b.origin_timestamp_us = now.saturating_sub(50_000);
        graph.on_pingwave(pw_b, addr);

        // Match all peers — should return the lower latency
        // (~5 ms — but skewed by wall-clock; allow a generous
        // window below the slower peer's 50 ms).
        let nearest = graph.nearest_rtt(|_| true).expect("at least one peer");
        assert!(
            nearest.as_micros() < 30_000,
            "expected the lower-latency peer (~5 ms), got {nearest:?}",
        );

        // Predicate-restricted lookup. Only 0x02 candidate.
        let only_b = graph.nearest_rtt(|n| n.node_id == nid(2));
        assert!(only_b.is_some());

        // No matching predicate → None.
        let none = graph.nearest_rtt(|_| false);
        assert!(none.is_none());
    }

    // ====================================================================
    // Phase F slice 5 — intent axis
    // ====================================================================

    /// Helper: build a Daemon artifact with the artifact's
    /// `required.metadata` carrying an intent declaration.
    fn daemon_with_intent<'a>(
        required: &'a CapabilitySet,
        optional: &'a CapabilitySet,
    ) -> Artifact<'a> {
        Artifact::Daemon {
            daemon_id: [0u8; 32],
            required,
            optional,
        }
    }

    /// `IntentMatchPolicy::Disabled` returns 1.0 regardless — pin
    /// the back-compat behavior with stub-axis behavior. Slice 5
    /// must not change observable scoring for daemons that don't
    /// opt in.
    #[test]
    fn intent_axis_disabled_always_returns_one() {
        let mut required = empty_caps();
        required = with_metadata_pair(required, "intent", "ml-training");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::Disabled)
            .with_intent_registry(IntentRegistry::defaults());
        let target_caps = empty_caps();
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 1.0);
    }

    /// `Strict` policy + no intent in artifact metadata → 1.0
    /// (no constraint to satisfy).
    #[test]
    fn intent_axis_strict_no_intent_metadata_returns_one() {
        let required = empty_caps();
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_intent_registry(IntentRegistry::defaults());
        let target_caps = empty_caps();
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 1.0, "no intent declared → no constraint");
    }

    /// `Strict` + intent declared but unknown to registry → 1.0
    /// (forward-compat — newer intents from a future caller don't
    /// veto on an older substrate).
    #[test]
    fn intent_axis_strict_unknown_intent_returns_one_forward_compat() {
        let mut required = empty_caps();
        required = with_metadata_pair(required, "intent", "future-intent-not-in-registry");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_intent_registry(IntentRegistry::defaults());
        let target_caps = empty_caps();
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 1.0, "unknown intent passes through");
    }

    /// `Strict` + known intent + target satisfies all required
    /// caps → 1.0.
    #[test]
    fn intent_axis_strict_satisfied_returns_one() {
        let mut required = empty_caps();
        required = with_metadata_pair(required, "intent", "ml-training");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_intent_registry(IntentRegistry::defaults());

        // Target with GPU + 32 GB VRAM satisfies ml-training.
        let target_caps = empty_caps()
            .add_tag("hardware.gpu")
            .add_tag("hardware.gpu.vram_gb=32");
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 1.0);
    }

    /// `Strict` + known intent + target missing a required cap → 0.0.
    /// Pin the hard-veto behavior the multiplicative composition
    /// relies on.
    #[test]
    fn intent_axis_strict_unsatisfied_returns_zero() {
        let mut required = empty_caps();
        required = with_metadata_pair(required, "intent", "ml-training");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_intent_registry(IntentRegistry::defaults());

        // Target with GPU but only 8 GB VRAM — fails the
        // `gpu.vram_gb >= 24` requirement.
        let target_caps = empty_caps()
            .add_tag("hardware.gpu")
            .add_tag("hardware.gpu.vram_gb=8");
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 0.0);
    }

    /// CR-22: `AnyOfLocalCapabilities` + empty registry → 1.0
    /// (axis-disabled identity), NOT 0.0. Pre-CR-22 this returned
    /// 0.0, multiplicatively wedging cluster-wide placement for
    /// any operator who selected the policy without populating
    /// `intent_registry`. Other axes use 1.0 for the empty-config
    /// case (proximity, anti-affinity, custom-filter); this one
    /// is now consistent.
    #[test]
    fn intent_axis_any_of_with_empty_registry_is_pass_through() {
        let required = empty_caps();
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::AnyOfLocalCapabilities);
        // Default empty registry.
        let target_caps = empty_caps()
            .add_tag("hardware.gpu")
            .add_tag("hardware.gpu.vram_gb=32");
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(
            score, 1.0,
            "empty registry must pass through (axis-disabled identity), \
             not hard-veto every candidate cluster-wide"
        );
    }

    /// `AnyOfLocalCapabilities` + target satisfies one intent → 1.0.
    #[test]
    fn intent_axis_any_of_satisfies_via_one_intent() {
        let required = empty_caps();
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::AnyOfLocalCapabilities)
            .with_intent_registry(IntentRegistry::defaults());

        // GPU + 32 GB VRAM satisfies ml-training (and inference's
        // GPU requirement, though inference also needs a software.model.* tag).
        let target_caps = empty_caps()
            .add_tag("hardware.gpu")
            .add_tag("hardware.gpu.vram_gb=32");
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 1.0, "ml-training reqs satisfy → axis passes");
    }

    /// `AnyOfLocalCapabilities` + target satisfies no intent → 0.0.
    /// Pin the policy's "useful for *something*" semantic.
    #[test]
    fn intent_axis_any_of_target_useful_for_nothing_returns_zero() {
        let required = empty_caps();
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::AnyOfLocalCapabilities)
            .with_intent_registry(IntentRegistry::defaults());

        // 2-core CPU node — fails ml-training (no GPU), inference (no
        // GPU + no model tag), cpu-bound (cpu_cores < 4), and
        // sensor-telemetry (no devices tag).
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=2");
        let score = placement.score_intent_axis(&target_caps, &artifact);
        assert_eq!(score, 0.0);
    }

    /// End-to-end: `placement_score` composes the intent axis
    /// multiplicatively. A `Strict` veto (intent unsatisfied)
    /// zeros the final score even though all other axes are
    /// stubbed at 1.0. Pin the §7-LOCKED "0.0 anywhere → 0.0
    /// final" invariant flowing through the real intent axis.
    #[test]
    fn intent_axis_zero_zeros_final_score_via_composition() {
        let mut required = empty_caps();
        required = with_metadata_pair(required, "intent", "ml-training");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        // Target lacks the GPU + VRAM requirements.
        let target_caps = empty_caps();
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let placement = StandardPlacement::new(&fold)
            .with_intent_match(IntentMatchPolicy::Strict)
            .with_intent_registry(IntentRegistry::defaults());

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes (required tag set is empty)");
        assert_eq!(score, 0.0, "intent axis vetoes — final score 0.0");
    }

    /// Helper: append a (key, value) metadata pair to a CapabilitySet.
    fn with_metadata_pair(mut caps: CapabilitySet, key: &str, value: &str) -> CapabilitySet {
        caps.metadata.insert(key.to_string(), value.to_string());
        caps
    }

    // ====================================================================
    // Phase F slice 5 — scope axis
    // ====================================================================

    /// `scope_filter: None` → 1.0 regardless of target tags.
    /// Default config with no scope filter set.
    #[test]
    fn scope_axis_none_filter_returns_one() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold);
        let target_caps = empty_caps().with_tenant_scope("foo");
        assert_eq!(placement.score_scope_axis(&target_caps), 1.0);
    }

    /// `scope_filter: Some(empty)` → 1.0 (no-constraint case).
    #[test]
    fn scope_axis_empty_filter_returns_one() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_scope_filter(vec![]);
        let target_caps = empty_caps().with_tenant_scope("foo");
        assert_eq!(placement.score_scope_axis(&target_caps), 1.0);
    }

    /// `scope_filter` non-empty + target has matching scope tag
    /// → 1.0 (any-of match).
    #[test]
    fn scope_axis_matches_full_form() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold)
            .with_scope_filter(vec![ScopeLabel::new("scope:tenant:foo")]);
        let target_caps = empty_caps().with_tenant_scope("foo");
        assert_eq!(placement.score_scope_axis(&target_caps), 1.0);
    }

    /// Body-form labels work too: `"tenant:foo"` matches a
    /// `scope:tenant:foo` target tag.
    #[test]
    fn scope_axis_matches_body_form() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_scope_filter(vec![ScopeLabel::new("tenant:foo")]);
        let target_caps = empty_caps().with_tenant_scope("foo");
        assert_eq!(
            placement.score_scope_axis(&target_caps),
            1.0,
            "body-form label must match the full scope tag"
        );
    }

    /// Non-empty filter + target has no scope tags → 0.0
    /// (operator wanted scoped placement; target is unscoped).
    #[test]
    fn scope_axis_unscoped_target_returns_zero() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_scope_filter(vec![ScopeLabel::new("tenant:foo")]);
        let target_caps = empty_caps().add_tag("hardware.gpu");
        assert_eq!(placement.score_scope_axis(&target_caps), 0.0);
    }

    /// Filter with multiple labels, target matches any-of → 1.0.
    #[test]
    fn scope_axis_matches_any_of_multiple_labels() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_scope_filter(vec![
            ScopeLabel::new("tenant:bar"),
            ScopeLabel::new("region:us-east"),
            ScopeLabel::new("tenant:foo"),
        ]);
        // Target has the third label.
        let target_caps = empty_caps().with_tenant_scope("foo");
        assert_eq!(placement.score_scope_axis(&target_caps), 1.0);
    }

    /// Non-empty filter + non-empty target scope tags + no match
    /// → 0.0.
    #[test]
    fn scope_axis_no_match_returns_zero() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_scope_filter(vec![ScopeLabel::new("tenant:foo")]);
        // Target tagged with a different tenant.
        let target_caps = empty_caps().with_tenant_scope("bar");
        assert_eq!(placement.score_scope_axis(&target_caps), 0.0);
    }

    // ====================================================================
    // Phase F slice 5 — proximity axis
    // ====================================================================

    /// `proximity_max_rtt: None` → 1.0 (axis disabled). Default.
    #[test]
    fn proximity_axis_no_threshold_returns_one() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold);
        assert_eq!(placement.score_proximity_axis(&0x1111), 1.0);
    }

    /// Threshold set + `rtt_lookup: None` → 1.0 (no measurement
    /// source; can't enforce). Pin the "default permissive when
    /// no data" contract.
    #[test]
    fn proximity_axis_threshold_without_lookup_returns_one() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_proximity_max_rtt(Duration::from_millis(50));
        assert_eq!(placement.score_proximity_axis(&0x1111), 1.0);
    }

    /// Threshold + lookup, target absent from lookup → 1.0.
    /// Asymmetric to tie-breaker step 1 which sorts present-RTT
    /// before missing — scoring axis defaults permissive on
    /// missing data.
    #[test]
    fn proximity_axis_unmeasured_target_returns_one() {
        let fold = index_with(&[]);
        let lookup = |_id: NodeId| -> Option<u64> { None };
        let placement = StandardPlacement::new(&fold)
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_rtt_lookup(&lookup);
        assert_eq!(placement.score_proximity_axis(&0x1111), 1.0);
    }

    /// RTT under the threshold → 1.0.
    #[test]
    fn proximity_axis_rtt_under_threshold_returns_one() {
        let fold = index_with(&[]);
        let lookup = |_id: NodeId| -> Option<u64> { Some(10_000) }; // 10 ms
        let placement = StandardPlacement::new(&fold)
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_rtt_lookup(&lookup);
        assert_eq!(placement.score_proximity_axis(&0x1111), 1.0);
    }

    /// RTT exactly at the threshold → 1.0 (inclusive bound).
    #[test]
    fn proximity_axis_rtt_at_threshold_returns_one_inclusive() {
        let fold = index_with(&[]);
        let lookup = |_id: NodeId| -> Option<u64> { Some(50_000) }; // exactly 50 ms
        let placement = StandardPlacement::new(&fold)
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_rtt_lookup(&lookup);
        assert_eq!(
            placement.score_proximity_axis(&0x1111),
            1.0,
            "threshold is inclusive (≤)",
        );
    }

    /// RTT over the threshold → 0.0 (hard veto).
    #[test]
    fn proximity_axis_rtt_over_threshold_returns_zero() {
        let fold = index_with(&[]);
        let lookup = |_id: NodeId| -> Option<u64> { Some(100_000) }; // 100 ms
        let placement = StandardPlacement::new(&fold)
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_rtt_lookup(&lookup);
        assert_eq!(placement.score_proximity_axis(&0x1111), 0.0);
    }

    /// Per-candidate RTT discrimination: the lookup distinguishes
    /// candidates by their NodeId. Pin: an over-threshold candidate
    /// vetoes while an under-threshold one passes.
    #[test]
    fn proximity_axis_per_candidate_via_lookup() {
        let fold = index_with(&[]);
        let lookup = |id: NodeId| -> Option<u64> {
            match id {
                0x1111 => Some(10_000), // 10 ms — under
                0x2222 => Some(80_000), // 80 ms — over
                _ => None,
            }
        };
        let placement = StandardPlacement::new(&fold)
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_rtt_lookup(&lookup);
        assert_eq!(placement.score_proximity_axis(&0x1111), 1.0);
        assert_eq!(placement.score_proximity_axis(&0x2222), 0.0);
    }

    /// End-to-end: a proximity over-threshold zeros the final
    /// score via multiplicative composition.
    #[test]
    fn proximity_axis_zero_zeros_final_score() {
        let target_caps = empty_caps();
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let lookup = |_id: NodeId| -> Option<u64> { Some(200_000) }; // 200 ms
        let placement = StandardPlacement::new(&fold)
            .with_proximity_max_rtt(Duration::from_millis(50))
            .with_rtt_lookup(&lookup);

        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes");
        assert_eq!(score, 0.0, "proximity axis vetoes — final score 0.0");
    }

    // ====================================================================
    // Phase F slice 5 — colocation axis
    // ====================================================================

    /// `ColocationPolicy::Ignore` → 1.0 regardless. Default.
    #[test]
    fn colocation_axis_ignore_returns_one() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::Ignore);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        let target_caps = empty_caps();
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            1.0
        );
    }

    /// SoftPreference + no colocate metadata declared → 1.0
    /// (no constraint).
    #[test]
    fn colocation_axis_soft_no_metadata_returns_one() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::SoftPreference);
        let required = empty_caps();
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        let target_caps = empty_caps();
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            1.0
        );
    }

    /// SoftPreference + soft-key declared + target hosts → 1.0.
    #[test]
    fn colocation_axis_soft_target_hosts_returns_one() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::SoftPreference);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        let target_caps = empty_caps().require_chain("abc123");
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            1.0
        );
    }

    /// SoftPreference + soft-key declared + target doesn't host
    /// → 0.7 (soft penalty boost).
    #[test]
    fn colocation_axis_soft_target_misses_returns_penalty() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::SoftPreference);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        let target_caps = empty_caps();
        let score = placement.score_colocation_axis(&target_caps, &artifact);
        assert!(
            (score - 0.7).abs() < 1e-6,
            "soft penalty expected ~0.7, got {score}"
        );
    }

    /// Strict-key always vetoes regardless of policy. Pin: under
    /// SoftPreference, declaring `colocate-with-strict` upgrades
    /// to hard veto.
    #[test]
    fn colocation_axis_strict_key_vetoes_under_soft_policy() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::SoftPreference);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with-strict", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        // Target doesn't host the chain.
        let target_caps = empty_caps();
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            0.0,
            "strict-key vetoes regardless of policy",
        );
    }

    /// StrictRequired + soft-key declared + target doesn't host
    /// → 0.0. The policy upgrades the soft-key declaration to
    /// strict semantics.
    #[test]
    fn colocation_axis_strict_policy_upgrades_soft_key_to_veto() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::StrictRequired);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        let target_caps = empty_caps();
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            0.0
        );
    }

    /// StrictRequired + soft-key declared + target hosts → 1.0.
    #[test]
    fn colocation_axis_strict_policy_target_hosts_returns_one() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::StrictRequired);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        let target_caps = empty_caps().require_chain("abc123");
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            1.0
        );
    }

    /// Tip-form `causal:<hash>:<tip_seq>` satisfies the colocation
    /// axis — peer announcing a tip implicitly holds the chain.
    #[test]
    fn colocation_axis_tip_form_satisfies_match() {
        let fold = index_with(&[]);
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::SoftPreference);
        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);
        // Target announces a tip-form chain.
        let target_caps = empty_caps().require_chain_tip("abc123", 42);
        assert_eq!(
            placement.score_colocation_axis(&target_caps, &artifact),
            1.0
        );
    }

    /// End-to-end: a strict colocation veto zeros the final score
    /// via multiplicative composition.
    #[test]
    fn colocation_axis_zero_zeros_final_score() {
        let target_caps = empty_caps();
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::StrictRequired);

        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes");
        assert_eq!(score, 0.0);
    }

    // ====================================================================
    // Phase F slice 5 — resource axis
    // ====================================================================

    /// Target with no relevant tags → 1.0 (axis can't measure;
    /// permissive default). Pin: pre-slice-5 default config /
    /// empty-target combination scores 1.0 — backward-compat with
    /// the stub.
    #[test]
    fn resource_axis_compute_no_data_returns_one() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);
        let target_caps = empty_caps();
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        assert_eq!(placement.score_resource_axis(&target_caps, &artifact), 1.0);
    }

    /// `Compute` axis with cpu_cores at the reference (8) →
    /// score 0.5 (saturating function half at the reference).
    #[test]
    fn resource_axis_compute_cpu_at_reference_scores_half() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=8");
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        let score = placement.score_resource_axis(&target_caps, &artifact);
        assert!(
            (score - 0.5).abs() < 1e-6,
            "8 cores at 8-reference scores 0.5; got {score}"
        );
    }

    /// More resources score higher than fewer. Pin the monotonic
    /// property the axis exists to provide.
    #[test]
    fn resource_axis_compute_monotonic_in_capacity() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        let small = empty_caps().add_tag("hardware.cpu_cores=4");
        let large = empty_caps().add_tag("hardware.cpu_cores=64");

        let small_score = placement.score_resource_axis(&small, &artifact);
        let large_score = placement.score_resource_axis(&large, &artifact);
        assert!(
            large_score > small_score,
            "64-core node ({large_score}) must score higher than 4-core ({small_score})"
        );
    }

    /// Axis averages over the components that have data, ignores
    /// missing components. Pin: a node with cpu + memory + vram
    /// declared scores against all three; a node with only cpu
    /// scores against just cpu.
    #[test]
    fn resource_axis_compute_averages_present_components() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        // All three: cpu_cores=8 (→ 0.5), memory_gb=16 (→ 0.5),
        // vram_gb=16 (→ 0.5). Average: 0.5.
        let three_components = empty_caps()
            .add_tag("hardware.cpu_cores=8")
            .add_tag("hardware.memory_gb=16")
            .add_tag("hardware.gpu.vram_gb=16");
        let s_three = placement.score_resource_axis(&three_components, &artifact);
        assert!(
            (s_three - 0.5).abs() < 1e-6,
            "3-comp avg got {s_three}, want 0.5"
        );

        // CPU only at the reference: avg = 0.5.
        let cpu_only = empty_caps().add_tag("hardware.cpu_cores=8");
        let s_one = placement.score_resource_axis(&cpu_only, &artifact);
        assert!((s_one - 0.5).abs() < 1e-6, "1-comp got {s_one}, want 0.5");
    }

    /// `Storage` axis: `dataforts.capacity_gb` at the reference
    /// (1 TB) → 0.5.
    #[test]
    fn resource_axis_storage_at_reference_scores_half() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Storage);
        let target_caps = empty_caps().add_tag("dataforts.capacity_gb=1000");
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        let score = placement.score_resource_axis(&target_caps, &artifact);
        assert!(
            (score - 0.5).abs() < 1e-6,
            "1 TB at 1 TB reference scores 0.5; got {score}"
        );
    }

    /// `Storage` axis: target without `capacity_gb` tag → 1.0
    /// (permissive when no data).
    #[test]
    fn resource_axis_storage_no_data_returns_one() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Storage);
        let target_caps = empty_caps();
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        assert_eq!(placement.score_resource_axis(&target_caps, &artifact), 1.0);
    }

    /// N-11: `Both` axis no longer dilutes against a no-data axis.
    /// Compute at half (8 cores) + storage no-data → returns the
    /// compute score alone (0.5), NOT the pre-fix average
    /// `(0.5 + 1.0) / 2 = 0.75` which incorrectly inflated the
    /// candidate's resource fit. The pre-fix shape let a no-data
    /// candidate tie a maxed-out one, biasing placement toward
    /// often-misconfigured lower-NodeId peers via the lex tie-break.
    #[test]
    fn resource_axis_both_uses_only_axes_with_data() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Both);

        // Compute has data (cpu_cores=8 → 0.5), storage does not.
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=8");
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        let score = placement.score_resource_axis(&target_caps, &artifact);
        assert!(
            (score - 0.5).abs() < 1e-6,
            "Both with only compute data should return compute score; got {score}"
        );

        // Storage has data (capacity_gb=500 → 0.333), compute
        // does not. Should return the storage score alone.
        let target_caps = empty_caps().add_tag("dataforts.capacity_gb=500");
        let score = placement.score_resource_axis(&target_caps, &artifact);
        let expected = saturating_score(500.0, 1000.0);
        assert!(
            (score - expected).abs() < 1e-6,
            "Both with only storage data should return storage score; got {score}"
        );

        // Both axes have data — average is computed.
        let target_caps = empty_caps()
            .add_tag("hardware.cpu_cores=8")
            .add_tag("dataforts.capacity_gb=500");
        let score = placement.score_resource_axis(&target_caps, &artifact);
        let expected = (0.5 + saturating_score(500.0, 1000.0)) / 2.0;
        assert!(
            (score - expected).abs() < 1e-6,
            "Both with both axes' data averages; got {score}, expected {expected}"
        );

        // Neither axis has data — still permissive 1.0 identity.
        let target_caps = empty_caps();
        assert_eq!(placement.score_resource_axis(&target_caps, &artifact), 1.0);
    }

    /// N-12 regression: a `hardware.cpu_cores=1e308` announcement
    /// parses as a finite f64 (≈1.8e308 < f64::MAX) but, when cast
    /// to f32 in the downstream score path, saturates to
    /// `f32::INFINITY`. The CR-9 guard then clamps the score to
    /// 0.0 — silently down-scoring the candidate to "bad fit"
    /// when the tag was absurd. Post-fix: `target_axis_value_numeric`
    /// range-checks against `MAX_RESOURCE_VALUE` and returns
    /// `None` (treated as "no data" by the score helper).
    #[test]
    fn resource_axis_overflow_value_treated_as_no_data() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);
        // Absurd `hardware.cpu_cores=1e308`: a finite f64, but
        // saturates to f32::INFINITY when downcast. Treated as
        // no-data → axis returns permissive 1.0.
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=1e308");
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        let score = placement.score_resource_axis(&target_caps, &artifact);
        assert_eq!(
            score, 1.0,
            "overflow value should be treated as no-data, not as 0.0; got {score}",
        );

        // Sanity: a sane value in the same axis still scores correctly.
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=8");
        let score = placement.score_resource_axis(&target_caps, &artifact);
        assert!(
            (score - 0.5).abs() < 1e-6,
            "sane value still scores normally; got {score}",
        );
    }

    /// Defensive: a malformed numeric tag (non-parseable value)
    /// is silently treated as "no data" — doesn't blow up the
    /// score with NaN.
    #[test]
    fn resource_axis_compute_malformed_value_treated_as_no_data() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=lots");
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        let score = placement.score_resource_axis(&target_caps, &artifact);
        // No parseable component → 1.0.
        assert_eq!(score, 1.0);
    }

    // ====================================================================
    // Phase F slice 5 — anti-affinity axis
    // ====================================================================

    /// `leadership_stats: None` → 1.0 (axis disabled). Default.
    #[test]
    fn anti_affinity_no_stats_returns_one() {
        let fold = index_with(&[]);
        let placement = StandardPlacement::new(&fold);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);
    }

    /// Stats configured + target has no data → 1.0 (permissive on
    /// missing data, parallel to proximity).
    #[test]
    fn anti_affinity_target_without_data_returns_one() {
        let fold = index_with(&[]);
        let stats = |_id: NodeId| -> Option<f32> { None };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);
    }

    /// Concentration under the threshold → 1.0 (no penalty).
    /// Default threshold is 0.30.
    #[test]
    fn anti_affinity_under_threshold_returns_one() {
        let fold = index_with(&[]);
        let stats = |_id: NodeId| -> Option<f32> { Some(0.20) };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);
    }

    /// Concentration exactly at the threshold → 1.0 (inclusive).
    #[test]
    fn anti_affinity_at_threshold_returns_one_inclusive() {
        let fold = index_with(&[]);
        let stats = |_id: NodeId| -> Option<f32> { Some(0.30) };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);
    }

    /// Concentration over the threshold → penalty value.
    /// Default penalty is 0.4.
    #[test]
    fn anti_affinity_over_threshold_returns_penalty() {
        let fold = index_with(&[]);
        let stats = |_id: NodeId| -> Option<f32> { Some(0.50) };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        let score = placement.score_anti_affinity_axis(&0x1111);
        assert!(
            (score - 0.4).abs() < 1e-6,
            "default penalty 0.4; got {score}"
        );
    }

    /// Per-candidate discrimination: stats lookup distinguishes
    /// candidates by NodeId. Under-threshold one passes; over-
    /// threshold one is penalized.
    #[test]
    fn anti_affinity_per_candidate_via_stats() {
        let fold = index_with(&[]);
        let stats = |id: NodeId| -> Option<f32> {
            match id {
                0x1111 => Some(0.10), // light leader
                0x2222 => Some(0.50), // heavy leader
                _ => None,
            }
        };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);
        assert!((placement.score_anti_affinity_axis(&0x2222) - 0.4).abs() < 1e-6);
    }

    /// CR-10: NaN concentration is treated as "no data" (returns
    /// 1.0). Pre-CR-10, `concentration <= threshold` was `false`
    /// for NaN, so the else branch fired and applied the penalty
    /// even though the threshold check is meaningless. This was
    /// asymmetric with the existing penalty-side NaN guard.
    #[test]
    fn anti_affinity_treats_nan_concentration_as_no_data() {
        let fold = index_with(&[]);
        let stats = |_id: NodeId| -> Option<f32> { Some(f32::NAN) };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);

        // Infinity has the same shape — non-finite means
        // unusable, fall back to permissive 1.0.
        let stats_inf = |_id: NodeId| -> Option<f32> { Some(f32::INFINITY) };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats_inf);
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);
    }

    /// CR-9: `saturating_score` rejects non-finite inputs (NaN,
    /// ±inf). `Tag::AxisValue` stores raw strings and
    /// `f64::from_str("NaN")` parses successfully, so a single
    /// `hardware.cpu_cores=NaN` would otherwise produce
    /// `value/(value+ref) = NaN` that cascades into the resource
    /// axis sum and silently vetoes the candidate via
    /// `compose_axis_scores`'s NaN clamp.
    #[test]
    fn saturating_score_rejects_non_finite_inputs() {
        // NaN value → 0.0 (no negative score; no NaN propagation).
        assert_eq!(saturating_score(f32::NAN, 8.0), 0.0);
        // NaN reference → 0.0.
        assert_eq!(saturating_score(8.0, f32::NAN), 0.0);
        // +inf value → 0.0.
        assert_eq!(saturating_score(f32::INFINITY, 8.0), 0.0);
        // -inf value → 0.0 (also caught by `<= 0.0`).
        assert_eq!(saturating_score(f32::NEG_INFINITY, 8.0), 0.0);
        // Sanity: regular path still works.
        let s = saturating_score(8.0, 8.0);
        assert!((s - 0.5).abs() < 1e-6);
    }

    /// Defensive clamp: a misconfigured penalty (NaN, negative,
    /// > 1.0) doesn't blow up the multiplicative composition.
    #[test]
    fn anti_affinity_defensive_clamps_misconfigured_penalty() {
        let fold = index_with(&[]);
        let stats = |_id: NodeId| -> Option<f32> { Some(0.50) };

        // NaN → 0.0.
        let mut placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);
        placement.anti_affinity.leadership_concentration_penalty = f32::NAN;
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 0.0);

        // > 1.0 → clamped to 1.0.
        placement.anti_affinity.leadership_concentration_penalty = 2.0;
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 1.0);

        // < 0.0 → clamped to 0.0.
        placement.anti_affinity.leadership_concentration_penalty = -0.5;
        assert_eq!(placement.score_anti_affinity_axis(&0x1111), 0.0);
    }

    /// End-to-end: an over-threshold candidate's penalty
    /// multiplies through the composition. Other axes 1.0 →
    /// final score = 0.4.
    #[test]
    fn anti_affinity_penalty_multiplies_through_composition() {
        let target_caps = empty_caps();
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let stats = |_id: NodeId| -> Option<f32> { Some(0.50) };
        let placement = StandardPlacement::new(&fold).with_leadership_stats(&stats);

        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes");
        assert!(
            (score - 0.4).abs() < 1e-6,
            "anti-affinity penalty (0.4) is the only non-1.0 axis; got {score}"
        );
    }

    /// End-to-end: low resource score multiplies through
    /// the composition. Target with cpu=2 (~0.2) → final score
    /// near 0.2 (other axes all 1.0).
    #[test]
    fn resource_axis_low_score_multiplies_through_composition() {
        let target_caps = empty_caps().add_tag("hardware.cpu_cores=2");
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let placement = StandardPlacement::new(&fold).with_resource_axis(ResourceAxis::Compute);

        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes");
        // 2 / (2 + 8) = 0.2 — exactly the resource axis score, no
        // other axes contribute (all 1.0).
        assert!(
            (score - 0.2).abs() < 1e-6,
            "2-core compute score 0.2 multiplies to final; got {score}"
        );
    }

    /// End-to-end: a soft-preference penalty multiplies through
    /// the composition. Target doesn't host the chain → final
    /// score 0.7 (other axes all stub / pass at 1.0).
    #[test]
    fn colocation_axis_soft_penalty_multiplies_through_composition() {
        let target_caps = empty_caps();
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let placement =
            StandardPlacement::new(&fold).with_colocation_policy(ColocationPolicy::SoftPreference);

        let mut required = empty_caps();
        required = with_metadata_pair(required, "colocate-with", "abc123");
        let optional = empty_caps();
        let artifact = daemon_with_intent(&required, &optional);

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes");
        assert!(
            (score - 0.7).abs() < 1e-6,
            "soft penalty multiplies through (other axes 1.0); got {score}"
        );
    }

    /// End-to-end: a proximity over-threshold zeros the final
    /// score via multiplicative composition.
    #[test]
    fn scope_axis_zero_zeros_final_score() {
        let target_caps = empty_caps().add_tag("hardware.gpu");
        let fold = {
            let f = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            let ad = CapabilityAnnouncement::new(0x1111, eid.clone(), 1, target_caps.clone());
            capability_bridge::apply_legacy_announcement(&f, ad);
            f
        };
        let placement =
            StandardPlacement::new(&fold).with_scope_filter(vec![ScopeLabel::new("tenant:foo")]);

        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);

        let score = placement
            .placement_score(&0x1111, &artifact)
            .expect("hard-constraint check passes (empty required tags)");
        assert_eq!(score, 0.0, "scope axis vetoes — final score 0.0");
    }

    /// `Chain` and `Replica` artifacts pass through hard-constraint
    /// checks today (slice 5 may add per-variant checks). Pin the
    /// current behavior so slice 5's additions are an explicit
    /// extension, not silent.
    #[test]
    fn standard_chain_and_replica_artifacts_pass_through_today() {
        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold);
        let chain_caps = empty_caps();
        let chain = Artifact::Chain {
            origin_hash: [1u8; 32],
            capabilities: &chain_caps,
        };
        assert_eq!(placement.placement_score(&0x1111, &chain), Some(1.0));

        let channel = crate::adapter::net::channel::ChannelName::new("rep").unwrap();
        let replica_caps = empty_caps();
        let replica = Artifact::Replica {
            channel: &channel,
            capabilities: &replica_caps,
        };
        assert_eq!(placement.placement_score(&0x1111, &replica), Some(1.0));
    }

    // =====================================================================
    // v0.2 PR-2b — `Artifact::Blob` placement gating.
    //
    // Pins four guarantees:
    //   1. Target without `dataforts.blob.storage` → hard veto.
    //   2. Target with `dataforts:blob-storage-unhealthy` reserved
    //      tag → hard veto (operator-emitted under disk pressure).
    //   3. Target with insufficient `dataforts.blob.disk_free_gb`
    //      vs the blob's size → hard veto.
    //   4. Target satisfying all three → score follows the existing
    //      multi-axis composition (chain/replica baseline 1.0).
    // =====================================================================

    fn caps_with_blob_storage(disk_free_gb: u64) -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag(format!("dataforts.blob.disk_total_gb={}", disk_free_gb))
            .add_tag(format!("dataforts.blob.disk_free_gb={}", disk_free_gb))
    }

    #[test]
    fn standard_blob_placement_rejects_node_without_blob_storage() {
        // Target node has no `dataforts.blob.storage` tag — placement
        // hard-vetoes.
        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold);
        let blob = Artifact::Blob {
            blob_hash: [0xAA; 32],
            size_bytes: 1024,
        };
        assert_eq!(placement.placement_score(&0x1111, &blob), None);
    }

    #[test]
    fn standard_blob_placement_admits_storage_participating_node() {
        // Target has `dataforts.blob.storage` + 100 GiB free; 1 KiB
        // blob fits easily.
        let fold = index_with(&[(0x1111, caps_with_blob_storage(100))]);
        let placement = StandardPlacement::new(&fold);
        let blob = Artifact::Blob {
            blob_hash: [0xBB; 32],
            size_bytes: 1024,
        };
        let score = placement.placement_score(&0x1111, &blob);
        assert!(
            score.is_some(),
            "expected placement to admit, got {:?}",
            score
        );
        // Default multi-axis composition without other gates is 1.0
        // — baseline parity with chain/replica passing through.
        assert_eq!(score, Some(1.0));
    }

    #[test]
    fn standard_blob_placement_rejects_insufficient_disk_free() {
        // Target has `dataforts.blob.storage` but only 2 GiB free;
        // 10 GiB blob can't fit. Hard veto.
        let fold = index_with(&[(0x1111, caps_with_blob_storage(2))]);
        let placement = StandardPlacement::new(&fold);
        let blob = Artifact::Blob {
            blob_hash: [0xCC; 32],
            size_bytes: 10 * (1 << 30), // 10 GiB
        };
        assert_eq!(placement.placement_score(&0x1111, &blob), None);
    }

    #[test]
    fn standard_blob_placement_rejects_unhealthy_node() {
        // Target advertises `dataforts.blob.storage` + ample disk, but
        // also carries the unhealthy health-gate tag → hard veto.
        // `add_tag` rejects reserved-prefix strings — insert the
        // `Tag::Reserved` directly to bypass the application-facing
        // parser.
        let mut unhealthy_caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=100");
        unhealthy_caps
            .tags
            .insert(crate::adapter::net::behavior::Tag::Reserved {
                prefix: "dataforts:".to_owned(),
                body: "blob-storage-unhealthy".to_owned(),
            });
        let fold = index_with(&[(0x1111, unhealthy_caps)]);
        let placement = StandardPlacement::new(&fold);
        let blob = Artifact::Blob {
            blob_hash: [0xDD; 32],
            size_bytes: 1024,
        };
        assert_eq!(placement.placement_score(&0x1111, &blob), None);
    }

    #[test]
    fn standard_blob_placement_disk_free_gb_rounds_up() {
        // 1.5 GiB blob requires `ceil(1.5 GiB / 1 GiB) = 2` free
        // GiB. Pin the rounding-up direction — under-counting would
        // admit a placement that overflows the disk.
        let one_and_a_half_gib: u64 = (1 << 30) + (1 << 29);

        // 1 GiB free → too small, veto.
        let fold = index_with(&[(0x2222, caps_with_blob_storage(1))]);
        let placement = StandardPlacement::new(&fold);
        let blob = Artifact::Blob {
            blob_hash: [0xEE; 32],
            size_bytes: one_and_a_half_gib,
        };
        assert_eq!(placement.placement_score(&0x2222, &blob), None);

        // 2 GiB free → fits, admit.
        let fold2 = index_with(&[(0x3333, caps_with_blob_storage(2))]);
        let placement2 = StandardPlacement::new(&fold2);
        assert!(placement2.placement_score(&0x3333, &blob).is_some());
    }

    // =====================================================================
    // SDK Phase 7 slice 5 — `StandardPlacement.custom_filter_id` consumption
    //
    // Pins five guarantees:
    //   1. `custom_filter_id: None` (default) → axis disabled
    //      (always 1.0 contribution).
    //   2. Registered filter returning `Some(1.0)` → composes
    //      multiplicatively (no observable change vs. no filter).
    //   3. Registered filter returning `Some(score)` → score is
    //      composed in (multiplied with other axes).
    //   4. Registered filter returning `None` → hard veto
    //      propagates up; the candidate is dropped.
    //   5. id NOT registered → hard veto + log; pin the misconfig
    //      contract.
    // =====================================================================

    use crate::adapter::net::behavior::placement_registry::global_placement_filter_registry;

    /// Test filter that returns a fixed `Option<f32>` regardless of
    /// candidate. Combined with a unique id per test case so the
    /// global singleton doesn't leak state between tests.
    struct FixedScoreFilter(Option<f32>);

    impl PlacementFilter for FixedScoreFilter {
        fn placement_score(&self, _: &NodeId, _: &Artifact<'_>) -> Option<f32> {
            self.0
        }
    }

    /// RAII-style registration cleanup so a panicking test doesn't
    /// leak the registration into other tests' singleton state.
    struct FilterGuard {
        id: String,
    }

    impl Drop for FilterGuard {
        fn drop(&mut self) {
            global_placement_filter_registry().unregister(&self.id);
        }
    }

    /// Helper: register, run a closure with the registered id, then
    /// always unregister via the RAII guard's `Drop`.
    fn with_registered_filter<F: FnOnce(&str)>(
        id: &str,
        filter: Arc<dyn PlacementFilter>,
        body: F,
    ) {
        let reg = global_placement_filter_registry();
        let _ = reg.unregister(id); // cleanup from a possibly-failed prior run
        assert!(
            reg.register(id.to_string(), filter, "test"),
            "register {id}"
        );
        let _guard = FilterGuard { id: id.to_string() };
        body(id);
    }

    /// `custom_filter_id: None` (default) leaves the axis at 1.0.
    /// Composes identically to a default `StandardPlacement` with
    /// no filter configured — pin the back-compat default.
    #[test]
    fn standard_placement_no_custom_filter_acts_as_identity_axis() {
        let fold = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&fold);
        assert!(placement.custom_filter_id.is_none());

        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        assert_eq!(placement.placement_score(&0x1111, &artifact), Some(1.0));
    }

    /// Registered filter returning `Some(1.0)` is identity for the
    /// composition; other axes' scores pass through unchanged. With
    /// every other axis disabled (default config), the final score
    /// stays 1.0.
    #[test]
    fn standard_placement_custom_filter_one_is_identity() {
        let fold = index_with(&[(0x2222, empty_caps())]);
        let id = "pf-test-slice5-identity";
        with_registered_filter(id, Arc::new(FixedScoreFilter(Some(1.0))), |id| {
            let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);
            let req = empty_caps();
            let opt = empty_caps();
            let artifact = daemon_artifact(&req, &opt);
            assert_eq!(
                placement.placement_score(&0x2222, &artifact),
                Some(1.0),
                "Some(1.0) custom score should compose identically",
            );
        });
    }

    /// Registered filter returning a fractional score is composed
    /// multiplicatively. With other axes disabled, the final score
    /// equals the custom score.
    #[test]
    fn standard_placement_custom_filter_score_composes_multiplicatively() {
        let fold = index_with(&[(0x3333, empty_caps())]);
        let id = "pf-test-slice5-multiply";
        with_registered_filter(id, Arc::new(FixedScoreFilter(Some(0.5))), |id| {
            let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);
            let req = empty_caps();
            let opt = empty_caps();
            let artifact = daemon_artifact(&req, &opt);
            let score = placement.placement_score(&0x3333, &artifact);
            assert_eq!(score, Some(0.5));
        });
    }

    /// Registered filter returning `None` propagates as a hard veto.
    /// Pin LOCKED §7: a None on the custom-filter axis drops the
    /// candidate entirely — no other axis can rescue it.
    #[test]
    fn standard_placement_custom_filter_none_is_hard_veto() {
        let fold = index_with(&[(0x4444, empty_caps())]);
        let id = "pf-test-slice5-veto";
        with_registered_filter(id, Arc::new(FixedScoreFilter(None)), |id| {
            let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);
            let req = empty_caps();
            let opt = empty_caps();
            let artifact = daemon_artifact(&req, &opt);
            assert_eq!(
                placement.placement_score(&0x4444, &artifact),
                None,
                "None from custom filter must veto the candidate",
            );
        });
    }

    /// `custom_filter_id` references an unregistered id → hard veto.
    /// Pin the misconfiguration contract: operators see a logged
    /// veto rather than silent permissive routing.
    #[test]
    fn standard_placement_unregistered_custom_filter_id_vetoes() {
        let fold = index_with(&[(0x5555, empty_caps())]);
        // Use a uniquely-prefixed id we never register so concurrent
        // test runs don't collide on the global singleton.
        let placement = StandardPlacement::new(&fold)
            .with_custom_filter_id("pf-test-slice5-NEVER-REGISTERED-xyz");
        let req = empty_caps();
        let opt = empty_caps();
        let artifact = daemon_artifact(&req, &opt);
        assert_eq!(
            placement.placement_score(&0x5555, &artifact),
            None,
            "unregistered custom_filter_id must veto",
        );
    }

    /// Custom filter composes alongside in-tree axes. Configure the
    /// scope axis to score 0.0 (filter mismatch) → final score is
    /// 0.0 even if custom filter says 1.0. Pin the multiplicative
    /// invariant: a 0.0 from any axis (including the new one) wins.
    #[test]
    fn standard_placement_custom_filter_composes_with_in_tree_axes() {
        let fold = index_with(&[(0x6666, empty_caps())]);
        let id = "pf-test-slice5-compose";
        with_registered_filter(id, Arc::new(FixedScoreFilter(Some(1.0))), |id| {
            // Scope filter requires a specific tag; target has
            // no scope tags, so scope axis returns 0.0.
            let placement = StandardPlacement::new(&fold)
                .with_custom_filter_id(id)
                .with_scope_filter(vec![ScopeLabel::new("scope:tenant:foo")]);
            let req = empty_caps();
            let opt = empty_caps();
            let artifact = daemon_artifact(&req, &opt);
            assert_eq!(
                placement.placement_score(&0x6666, &artifact),
                Some(0.0),
                "scope-axis 0.0 zeroes the composition even when custom = 1.0",
            );
        });
    }

    /// N-4 regression: a custom `PlacementFilter` that re-enters the
    /// fold (via `find_nodes_matching` / state queries on the same
    /// target) must NOT deadlock during
    /// `StandardPlacement::placement_score`. Pre-fix the custom-filter
    /// axis was invoked inside the outer state-lock closure — re-entrant
    /// fold access under a concurrent writer deadlocked.
    ///
    /// The filter below queries the fold from inside its
    /// `placement_score` callback. If the fix regresses, this test
    /// hangs (the harness will time it out).
    #[test]
    fn standard_placement_custom_filter_can_query_index_without_deadlock() {
        struct ReentrantFilter {
            fold: Arc<Fold<CapabilityFold>>,
        }
        impl PlacementFilter for ReentrantFilter {
            fn placement_score(&self, target: &NodeId, _artifact: &Artifact<'_>) -> Option<f32> {
                // Touch the fold from inside the callback. Pre-fix
                // this was running under the outer state-lock and
                // would deadlock against a concurrent writer; post-fix
                // the custom filter runs BEFORE the inner closure so
                // re-entry is safe.
                let legacy = crate::adapter::net::behavior::capability::CapabilityFilter::default();
                let _ = capability_bridge::find_nodes_matching(&self.fold, &legacy);
                let _ = capability_bridge::synthesize_capability_set(&self.fold, *target);
                Some(0.75)
            }
        }

        let fold = index_with(&[(0x7777, empty_caps())]);
        let id = "pf-test-N4-reentrant";
        let filter = Arc::new(ReentrantFilter { fold: fold.clone() });
        with_registered_filter(id, filter, |id| {
            let placement = StandardPlacement::new(&fold).with_custom_filter_id(id);
            let req = empty_caps();
            let opt = empty_caps();
            let artifact = daemon_artifact(&req, &opt);
            // Score composes through: ReentrantFilter returns 0.75,
            // every in-tree axis is at its default 1.0, so the
            // product is 0.75.
            assert_eq!(
                placement.placement_score(&0x7777, &artifact),
                Some(0.75),
                "reentrant custom filter must complete without deadlock",
            );
        });
    }
}
