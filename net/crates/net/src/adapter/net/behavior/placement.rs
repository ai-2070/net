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

use crate::adapter::net::behavior::capability::{
    CapabilityFilter, CapabilitySet, CapabilityIndex,
};
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
/// The shim consults a borrowed `&CapabilityIndex` to look up the
/// candidate's announced caps; this matches the existing
/// `Scheduler` plumbing where the index is already in scope.
pub struct LegacyPlacement<'a> {
    filter: CapabilityFilter,
    index: &'a CapabilityIndex,
}

impl<'a> LegacyPlacement<'a> {
    /// Construct from an explicit legacy filter + the live index.
    /// The index is borrowed and SHOULD outlive the filter; in
    /// practice both are owned by the `Scheduler` struct.
    pub fn new(filter: CapabilityFilter, index: &'a CapabilityIndex) -> Self {
        Self { filter, index }
    }

    /// Construct an empty filter — every candidate that exists in
    /// the index is eligible. Equivalent to today's
    /// `CapabilityFilter::default()` behavior in
    /// `find_migration_targets`.
    pub fn permissive(index: &'a CapabilityIndex) -> Self {
        Self {
            filter: CapabilityFilter::default(),
            index,
        }
    }
}

impl<'a> PlacementFilter for LegacyPlacement<'a> {
    fn placement_score(&self, target: &NodeId, _artifact: &Artifact<'_>) -> Option<f32> {
        // The shim is filter-driven, not artifact-driven — the
        // legacy code paths never inspected the artifact's required
        // / optional caps; they ran the global filter. Phase G
        // wires the artifact-aware variant via `StandardPlacement`.
        let candidates = self.index.query(&self.filter);
        if candidates.contains(target) {
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
    /// `hardware.cpu_cores` / `hardware.memory_mb` /
    /// `hardware.gpu.vram_mb` — daemon artifacts.
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
#[derive(Debug, Clone)]
pub enum IntentMatchPolicy {
    /// Disabled — intent axis always returns `1.0`. Default.
    Disabled,
    /// Node fulfills any intent it has capability for. Slice 5
    /// wires the actual lookup against `IntentRegistry` (slice 3).
    AnyOfLocalCapabilities,
    /// Node must satisfy the registry's required capabilities for
    /// the artifact's declared `metadata.intent` value.
    Strict,
}

impl Default for IntentMatchPolicy {
    fn default() -> Self {
        IntentMatchPolicy::Disabled
    }
}

/// How the colocation axis weights `metadata.colocate-with` matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColocationPolicy {
    /// Ignore colocation hints entirely (axis returns `1.0`). Default.
    Ignore,
    /// Boost score when target hosts the colocation chain.
    SoftPreference,
    /// Refuse placement (return `None`) unless target hosts the
    /// colocation chain. Triggered by `colocate-with-strict`.
    StrictRequired,
}

impl Default for ColocationPolicy {
    fn default() -> Self {
        ColocationPolicy::Ignore
    }
}

/// Reference `PlacementFilter` impl. Five-axis multi-criteria
/// scoring (scope / proximity / intent / colocation / resource) +
/// anti-affinity penalty, all composing multiplicatively per the
/// plan §7 LOCKED contract. Slice 2 ships the composition machinery
/// + hard-constraint check; slice 5 fills in the per-axis scorers.
///
/// Borrows a `&CapabilityIndex` for target-cap lookup (matches the
/// `LegacyPlacement` borrow shape). Hold a fresh `StandardPlacement`
/// per scheduler call site or share an `Arc<...>` if sharing
/// configuration.
pub struct StandardPlacement<'a> {
    index: &'a CapabilityIndex,
    /// Set of scope labels the target must match (one-of). `None`
    /// disables the scope axis.
    pub scope_filter: Option<Vec<ScopeLabel>>,
    /// Hard ceiling on the proximity-graph RTT to the candidate.
    /// `None` disables the proximity axis.
    pub proximity_max_rtt: Option<Duration>,
    /// Intent-match strategy. `Disabled` skips the axis.
    pub intent_match: IntentMatchPolicy,
    /// Colocation strategy.
    pub colocation_policy: ColocationPolicy,
    /// Which resource pool the resource axis scores. `Compute` is
    /// the daemon-friendly default.
    pub resource_axis: ResourceAxis,
    /// Metadata-key names for the intent + colocation axes.
    pub metadata_keys: PlacementMetadataKeys,
    /// Anti-affinity penalty configuration.
    pub anti_affinity: AntiAffinityConfig,
}

impl<'a> StandardPlacement<'a> {
    /// Build with default config — every axis disabled. Equivalent
    /// to `LegacyPlacement::permissive` in behavior (returns
    /// `Some(1.0)` for any candidate that satisfies the artifact's
    /// hard `required` constraint).
    pub fn new(index: &'a CapabilityIndex) -> Self {
        Self {
            index,
            scope_filter: None,
            proximity_max_rtt: None,
            intent_match: IntentMatchPolicy::default(),
            colocation_policy: ColocationPolicy::default(),
            resource_axis: ResourceAxis::Compute,
            metadata_keys: PlacementMetadataKeys::default(),
            anti_affinity: AntiAffinityConfig::default(),
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

    /// Replace the intent-match policy.
    pub fn with_intent_match(mut self, policy: IntentMatchPolicy) -> Self {
        self.intent_match = policy;
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
        let clamped = if s.is_nan() {
            0.0
        } else {
            s.clamp(0.0, 1.0)
        };
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
        // Look up the candidate's announced caps. An unindexed target
        // is a hard veto — we cannot reason about it.
        let target_caps = self.index.get(*target)?;

        // Hard-constraint check: artifact's `required` caps must be
        // a subset of the target's tag set. `Chain` and `Replica`
        // variants don't carry required caps directly; they pass
        // through this check (slice 5 may extend with per-variant
        // checks).
        if let Artifact::Daemon { required, .. } = artifact {
            if !required.tags.iter().all(|t| target_caps.tags.contains(t)) {
                return None;
            }
        }

        // Per-axis scoring stubs (slice 5 fills these in).
        let scope = self.score_scope_axis(&target_caps);
        let proximity = self.score_proximity_axis(target);
        let intent = self.score_intent_axis(&target_caps, artifact);
        let colocation = self.score_colocation_axis(&target_caps, artifact);
        let resource = self.score_resource_axis(&target_caps, artifact);
        let anti_affinity = self.score_anti_affinity_axis(target);

        Some(compose_axis_scores([
            scope,
            proximity,
            intent,
            colocation,
            resource,
            anti_affinity,
        ]))
    }
}

// Per-axis scoring stubs. Slice 5 replaces each body with the
// real evaluator. Each returns a value in `[0.0, 1.0]`; `1.0`
// means "axis is satisfied / doesn't apply"; `0.0` means
// "hard veto on this axis."
impl<'a> StandardPlacement<'a> {
    fn score_scope_axis(&self, _target_caps: &CapabilitySet) -> f32 {
        // Stub: scope axis always satisfied. Slice 5 evaluates
        // `scope_filter` membership against target's reserved-tag set.
        1.0
    }

    fn score_proximity_axis(&self, _target: &NodeId) -> f32 {
        // Stub: proximity axis always satisfied. Slice 4 wires
        // `ProximityGraph::nearest_rtt`; slice 5 thresholds against
        // `proximity_max_rtt`.
        1.0
    }

    fn score_intent_axis(&self, _target_caps: &CapabilitySet, _artifact: &Artifact<'_>) -> f32 {
        // Stub: intent axis always satisfied. Slice 3 + 5 wire
        // `IntentRegistry` lookup + `IntentMatchPolicy` evaluation.
        1.0
    }

    fn score_colocation_axis(&self, _target_caps: &CapabilitySet, _artifact: &Artifact<'_>) -> f32 {
        // Stub: colocation axis always satisfied. Slice 5 evaluates
        // `metadata.colocate-with` against target's local chain
        // holdings (`causal:*` tags).
        1.0
    }

    fn score_resource_axis(&self, _target_caps: &CapabilitySet, _artifact: &Artifact<'_>) -> f32 {
        // Stub: resource axis always satisfied. Slice 5 reads
        // `hardware.*` (Compute) / `dataforts.free_storage_gb`
        // (Storage) tags from target caps and computes a fit score.
        1.0
    }

    fn score_anti_affinity_axis(&self, _target: &NodeId) -> f32 {
        // Stub: anti-affinity axis always satisfied. Slice 5 reads
        // local-view leadership-concentration stats and applies
        // `leadership_concentration_penalty` past the threshold.
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        CapabilityAnnouncement, CapabilitySet,
    };
    use crate::adapter::net::identity::EntityId;
    use std::sync::Arc;

    fn index_with(nodes: &[(NodeId, CapabilitySet)]) -> Arc<CapabilityIndex> {
        let index = CapabilityIndex::new();
        let eid = EntityId::from_bytes([0u8; 32]);
        for (node_id, caps) in nodes {
            let ad = CapabilityAnnouncement::new(*node_id, eid.clone(), 1, caps.clone());
            index.index(ad);
        }
        Arc::new(index)
    }

    fn empty_caps() -> CapabilitySet {
        CapabilitySet::default()
    }

    fn daemon_artifact<'a>(required: &'a CapabilitySet, optional: &'a CapabilitySet) -> Artifact<'a> {
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
        let index = index_with(&[
            (0x1111, empty_caps()),
            (0x2222, empty_caps()),
        ]);
        let filter = LegacyPlacement::permissive(&index);
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
        let index = index_with(&[(0x1111, empty_caps())]);
        let filter = LegacyPlacement::permissive(&index);
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
        let index = index_with(&[
            (0x1111, caps_with_tag),
            (0x2222, empty_caps()),
        ]);

        let required = CapabilityFilter::default().require_tag("hardware.gpu".to_string());

        let filter = LegacyPlacement::new(required, &index);
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
        let index = index_with(&[(0x1111, empty_caps())]);
        let filter = LegacyPlacement::permissive(&index);
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
        let index = index_with(&[(0x1111, empty_caps()), (0x2222, empty_caps())]);
        let placement = StandardPlacement::new(&index);
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
        let index = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&index);
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
        let index = index_with(&[
            (0x1111, caps_with_gpu),
            (0x2222, empty_caps()),
        ]);
        let placement = StandardPlacement::new(&index);

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
        let index = index_with(&[]);
        let placement = StandardPlacement::new(&index)
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
        assert_eq!(
            placement.proximity_max_rtt,
            Some(Duration::from_millis(50))
        );
        assert!(matches!(
            placement.intent_match,
            IntentMatchPolicy::Strict
        ));
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
        let index = index_with(&[]);
        let placement = StandardPlacement::new(&index);
        assert!(placement.scope_filter.is_none());
        assert!(placement.proximity_max_rtt.is_none());
        assert!(matches!(placement.intent_match, IntentMatchPolicy::Disabled));
        assert_eq!(placement.colocation_policy, ColocationPolicy::Ignore);
    }

    /// `Chain` and `Replica` artifacts pass through hard-constraint
    /// checks today (slice 5 may add per-variant checks). Pin the
    /// current behavior so slice 5's additions are an explicit
    /// extension, not silent.
    #[test]
    fn standard_chain_and_replica_artifacts_pass_through_today() {
        let index = index_with(&[(0x1111, empty_caps())]);
        let placement = StandardPlacement::new(&index);
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
}
