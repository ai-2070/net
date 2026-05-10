//! Daemon placement scheduler.
//!
//! Connects `CapabilityFilter` requirements to `CapabilityIndex` queries
//! to decide where to run a daemon. Prefers local placement, falls back
//! to the least-loaded candidate.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::adapter::net::behavior::capability::{CapabilityFilter, CapabilityIndex, CapabilitySet};
use crate::adapter::net::behavior::placement::{
    tie_break_compare, Artifact, LegacyPlacement, PlacementFilter, ResourceAxis, TieBreakContext,
};
use crate::adapter::net::subprotocol::SubprotocolRegistry;

/// Why a particular node was chosen for placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementReason {
    /// Only node matching the filter.
    OnlyCandidate,
    /// Preferred because it's the local node.
    LocalPreferred,
    /// First candidate from the index (tie-breaking).
    FirstMatch,
    /// Explicitly pinned to a specific node.
    Pinned,
    /// Highest-scoring candidate from a `&dyn PlacementFilter`,
    /// ties broken by the §7-LOCKED RTT → free-resource → lex-NodeId
    /// chain. Stamped by `Scheduler::select_*` callers (Phase G).
    BestScore,
}

/// Result of a placement decision.
#[derive(Debug, Clone)]
pub struct PlacementDecision {
    /// Selected node ID.
    pub node_id: u64,
    /// Why this node was chosen.
    pub reason: PlacementReason,
}

/// Errors from scheduling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    /// No nodes match the capability filter.
    NoCandidate,
    /// Capability index unavailable.
    IndexUnavailable,
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCandidate => write!(f, "no nodes match capability requirements"),
            Self::IndexUnavailable => write!(f, "capability index unavailable"),
        }
    }
}

impl std::error::Error for SchedulerError {}

/// Daemon placement scheduler.
///
/// Queries the `CapabilityIndex` to find nodes matching a daemon's
/// requirements. Prefers local placement when possible.
pub struct Scheduler {
    /// Reference to the shared capability index.
    capability_index: Arc<CapabilityIndex>,
    /// This node's ID (for local preference).
    local_node_id: u64,
    /// This node's capabilities (for fast local check).
    local_caps: CapabilitySet,
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(
        capability_index: Arc<CapabilityIndex>,
        local_node_id: u64,
        local_caps: CapabilitySet,
    ) -> Self {
        Self {
            capability_index,
            local_node_id,
            local_caps,
        }
    }

    /// Check if a daemon can run locally.
    #[inline]
    pub fn can_run_locally(&self, filter: &CapabilityFilter) -> bool {
        filter.matches(&self.local_caps)
    }

    /// Place a daemon given its capability requirements.
    ///
    /// Strategy:
    /// 1. If local node matches, prefer local (zero network hop).
    /// 2. Otherwise, query the capability index for candidates.
    /// 3. Return the first match (future: least-loaded via LoadBalancer).
    pub fn place(&self, filter: &CapabilityFilter) -> Result<PlacementDecision, SchedulerError> {
        // Fast path: try local
        if self.can_run_locally(filter) {
            return Ok(PlacementDecision {
                node_id: self.local_node_id,
                reason: PlacementReason::LocalPreferred,
            });
        }

        // Query the index for matching nodes
        let candidates = self.capability_index.query(filter);

        if candidates.is_empty() {
            return Err(SchedulerError::NoCandidate);
        }

        if candidates.len() == 1 {
            return Ok(PlacementDecision {
                node_id: candidates[0],
                reason: PlacementReason::OnlyCandidate,
            });
        }

        // Multiple candidates — pick first (future: load-aware)
        Ok(PlacementDecision {
            node_id: candidates[0],
            reason: PlacementReason::FirstMatch,
        })
    }

    /// Query candidate node IDs matching a capability filter.
    pub fn query_candidates(&self, filter: &CapabilityFilter) -> Vec<u64> {
        self.capability_index.query(filter)
    }

    /// Place a daemon on a specific node (pinning).
    pub fn pin(&self, node_id: u64) -> PlacementDecision {
        PlacementDecision {
            node_id,
            reason: PlacementReason::Pinned,
        }
    }

    /// Find nodes that support a given subprotocol.
    ///
    /// Queries the capability index for nodes advertising the tag
    /// `subprotocol:0x{id:04x}`. Returns node IDs of all matches.
    pub fn find_subprotocol_nodes(&self, subprotocol_id: u16) -> Vec<u64> {
        let filter = SubprotocolRegistry::capability_filter_for(subprotocol_id);
        self.capability_index.query(&filter)
    }

    /// Find nodes capable of receiving a daemon migration.
    ///
    /// Queries for nodes advertising the migration subprotocol tag
    /// (`subprotocol:0x0500`), combined with the daemon's own capability
    /// requirements. Excludes the source node.
    pub fn find_migration_targets(
        &self,
        daemon_filter: &CapabilityFilter,
        source_node: u64,
    ) -> Vec<u64> {
        // Build a combined filter: must support migration AND daemon requirements
        let mut combined = daemon_filter.clone();
        combined =
            combined.require_tag(format!("subprotocol:{:#06x}", super::SUBPROTOCOL_MIGRATION,));

        self.capability_index
            .query(&combined)
            .into_iter()
            .filter(|&node_id| node_id != source_node)
            .collect()
    }

    /// Place a daemon for migration — find the best target node.
    ///
    /// Combines the daemon's capability requirements with the migration
    /// subprotocol tag to find eligible target nodes. Excludes the source.
    /// Prefers local node if eligible, otherwise first match.
    pub fn place_migration(
        &self,
        daemon_filter: &CapabilityFilter,
        source_node: u64,
    ) -> Result<PlacementDecision, SchedulerError> {
        let candidates = self.find_migration_targets(daemon_filter, source_node);

        if candidates.is_empty() {
            return Err(SchedulerError::NoCandidate);
        }

        // Prefer local if it's a candidate (and not the source)
        if self.local_node_id != source_node && candidates.contains(&self.local_node_id) {
            return Ok(PlacementDecision {
                node_id: self.local_node_id,
                reason: PlacementReason::LocalPreferred,
            });
        }

        if candidates.len() == 1 {
            return Ok(PlacementDecision {
                node_id: candidates[0],
                reason: PlacementReason::OnlyCandidate,
            });
        }

        Ok(PlacementDecision {
            node_id: candidates[0],
            reason: PlacementReason::FirstMatch,
        })
    }

    /// Phase G slice 8 of `CAPABILITY_SYSTEM_PLAN.md` — v2 of
    /// [`Self::place_migration`] using the `&dyn PlacementFilter`
    /// machinery. Uses [`LegacyPlacement::permissive`] against the
    /// scheduler's own `CapabilityIndex` so observable eligibility
    /// matches v1 (any migration-capable, daemon-filter-compatible
    /// node is eligible), but ranking flows through
    /// [`Self::select_migration_target`] and the LOCKED §7
    /// tie-breaker.
    ///
    /// **Default migration path.** `Orchestrator::start_migration_auto`
    /// (in `orchestrator.rs`) calls this method, so any auto-target
    /// migration in the substrate runs through v2 plumbing. The
    /// legacy [`Self::place_migration`] is kept around for callers
    /// who explicitly want v1's `LocalPreferred` / `FirstMatch`
    /// behavior (e.g. tests pinning the legacy contract); production
    /// code should use this v2 entry point.
    ///
    /// The returned `PlacementDecision` carries
    /// [`PlacementReason::BestScore`] so observers can distinguish
    /// the v2 path in telemetry.
    ///
    /// `tie_break.rtt_lookup` is `None` here — the substrate-level
    /// scheduler has no RTT data plumbed in by default. Operators
    /// who want RTT-aware tie-breaking should call
    /// [`Self::select_migration_target`] directly with a populated
    /// `TieBreakContext`; this convenience wrapper falls through to
    /// step 3 (lex NodeId) without RTT data.
    pub fn place_migration_v2(
        &self,
        daemon_filter: &CapabilityFilter,
        source_node: u64,
    ) -> Result<PlacementDecision, SchedulerError> {
        let placement = LegacyPlacement::permissive(&self.capability_index);
        let tie_break = TieBreakContext {
            rtt_lookup: None,
            index: &self.capability_index,
            resource_axis: ResourceAxis::Compute,
        };
        // Empty required / optional caps: the migration artifact
        // carries its hard-constraint shape via `daemon_filter`
        // (which narrows the candidate pool inside
        // `find_migration_targets`). Splitting hard constraints
        // between the filter and the artifact would double-count
        // them; the artifact stays empty so the
        // `LegacyPlacement::permissive` shim's "any eligible
        // candidate scores 1.0" semantics hold.
        let empty = CapabilitySet::default();
        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &empty,
            optional: &empty,
        };
        let node_id = self
            .select_migration_target(
                &artifact,
                daemon_filter,
                source_node,
                &placement,
                &tie_break,
            )
            .ok_or(SchedulerError::NoCandidate)?;
        Ok(PlacementDecision {
            node_id,
            reason: PlacementReason::BestScore,
        })
    }

    /// Phase G slice 1 of `CAPABILITY_SYSTEM_PLAN.md`. Select the
    /// best migration target for `artifact` from the candidates
    /// returned by [`Self::find_migration_targets`], scored via
    /// `&dyn PlacementFilter` with ties broken by the §7-LOCKED
    /// three-step ordering (RTT → free-resource → lex NodeId).
    ///
    /// `daemon_filter` narrows the candidate pool BEFORE scoring —
    /// it's the legacy `CapabilityFilter` plumbed through
    /// `find_migration_targets` so existing callsites keep
    /// working. `placement` runs against each surviving candidate;
    /// any candidate scoring `None` is excluded (hard veto).
    ///
    /// `tie_break` carries the inputs the comparator reads (RTT
    /// closure + capability index + resource axis). Pass a
    /// `TieBreakContext { rtt_lookup: None, ... }` to fall through
    /// to step 3 (lex NodeId) when no proximity data is available.
    ///
    /// Returns the highest-scoring candidate's node id, or `None`
    /// when every candidate is vetoed (or the candidate list was
    /// empty to begin with).
    ///
    /// Additive — does NOT change `place_migration`'s observable
    /// behavior. Operators opt in by calling this method directly;
    /// a future slice gates the default migration path behind the
    /// `mikoshi-placement-v2` feature flag.
    pub fn select_migration_target(
        &self,
        artifact: &Artifact<'_>,
        daemon_filter: &CapabilityFilter,
        source_node: u64,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Option<u64> {
        let candidates = self.find_migration_targets(daemon_filter, source_node);
        Self::pick_best_candidate(candidates, artifact, placement, tie_break)
    }

    /// Phase G slice 3 of `CAPABILITY_SYSTEM_PLAN.md`. Select the
    /// best node for placing a new group member (replica / fork /
    /// standby), scored via `&dyn PlacementFilter` with the same
    /// §7-LOCKED tie-breaker as
    /// [`Self::select_migration_target`].
    ///
    /// `requirements` filters the candidate pool via the legacy
    /// `CapabilityFilter` path (matches today's
    /// `Scheduler::query_candidates` behavior); `exclude` is a
    /// set of NodeIds to skip — used by replica groups for
    /// best-effort spread (don't pick a node already running a
    /// member). `placement` scores the survivors; ties broken by
    /// `tie_break`.
    ///
    /// Returns `None` when every candidate is excluded or vetoed.
    pub fn select_member_node(
        &self,
        artifact: &Artifact<'_>,
        requirements: &CapabilityFilter,
        exclude: &std::collections::HashSet<u64>,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Option<u64> {
        let candidates = self
            .query_candidates(requirements)
            .into_iter()
            .filter(|n| !exclude.contains(n));
        Self::pick_best_candidate(candidates, artifact, placement, tie_break)
    }

    /// Phase G slice 3. Select the standby member that should be
    /// promoted to active. Same shape as
    /// [`Self::select_member_node`] without the exclusion-set
    /// concept (promotion is over the existing standby pool, not
    /// a fresh placement decision).
    ///
    /// `candidates` is the standby roster — typically the live
    /// `StandbyGroup`'s `members` projected to `Vec<u64>`.
    pub fn select_promotion_target(
        &self,
        candidates: Vec<u64>,
        artifact: &Artifact<'_>,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Option<u64> {
        Self::pick_best_candidate(candidates, artifact, placement, tie_break)
    }

    /// Shared scoring + tie-breaking core. Used by
    /// [`Self::select_migration_target`],
    /// [`Self::select_member_node`], and
    /// [`Self::select_promotion_target`].
    ///
    /// Filter-vetoed (`None`-scoring) candidates are dropped;
    /// survivors sort highest-score-first with the §7-LOCKED
    /// tie-breaker resolving equal scores. Returns the best
    /// candidate's NodeId, or `None` when every candidate vetoes /
    /// the input is empty.
    fn pick_best_candidate<I: IntoIterator<Item = u64>>(
        candidates: I,
        artifact: &Artifact<'_>,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Option<u64> {
        // CR-6: drop NaN scores at the boundary. `StandardPlacement`
        // clamps NaN in `compose_axis_scores`, but FFI-registered
        // filters resolved via `placement_registry` are free to
        // return raw `Some(f32::NAN)` (JS `NaN` round-trips
        // trivially through napi; Python f64 NaN through pyo3).
        // A NaN candidate compares `Equal` to everything, so
        // `Vec::sort_by` over a non-total ordering produces
        // *undefined* placement order — different runs would pick
        // different winners. Treat NaN as a hard veto (drop the
        // candidate) so the contract is enforceable at the
        // boundary, matching the StandardPlacement clamp behavior.
        let mut scored: Vec<(u64, f32)> = candidates
            .into_iter()
            .filter_map(|n| placement.placement_score(&n, artifact).map(|s| (n, s)))
            .filter(|(_, s)| s.is_finite())
            .collect();

        if scored.is_empty() {
            return None;
        }

        // Highest score first; ties broken via the locked
        // three-step ordering. NaN is no longer reachable here
        // (filtered above), so `partial_cmp.unwrap_or(Equal)` is
        // a safety belt only — a strict total order is now in force.
        scored.sort_by(|(a, sa), (b, sb)| {
            sb.partial_cmp(sa)
                .unwrap_or(Ordering::Equal)
                .then_with(|| tie_break_compare(*a, *b, tie_break))
        });

        scored.first().map(|(n, _)| *n)
    }
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("local_node_id", &format!("{:#x}", self.local_node_id))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        CapabilityAnnouncement, GpuInfo, GpuVendor, HardwareCapabilities,
    };

    fn make_index_with_nodes(nodes: Vec<(u64, CapabilitySet)>) -> Arc<CapabilityIndex> {
        let index = CapabilityIndex::new();
        let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
        for (node_id, caps) in nodes {
            let ad = CapabilityAnnouncement::new(node_id, eid.clone(), 1, caps);
            index.index(ad);
        }
        Arc::new(index)
    }

    fn caps_with_gpu() -> CapabilitySet {
        let gpu = GpuInfo {
            vendor: GpuVendor::Nvidia,
            model: "test".into(),
            vram_mb: 8192,
            compute_units: 0,
            tensor_cores: 0,
            fp16_tflops_x10: 0,
        };
        CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_gpu(gpu))
    }

    fn caps_no_gpu() -> CapabilitySet {
        CapabilitySet::new()
    }

    #[test]
    fn test_local_preferred() {
        let local_caps = caps_no_gpu();
        let index = make_index_with_nodes(vec![]);
        let scheduler = Scheduler::new(index, 0x1111, local_caps);

        // Empty filter = runs anywhere, including local
        let decision = scheduler.place(&CapabilityFilter::default()).unwrap();
        assert_eq!(decision.node_id, 0x1111);
        assert_eq!(decision.reason, PlacementReason::LocalPreferred);
    }

    #[test]
    fn test_remote_when_local_insufficient() {
        let local_caps = caps_no_gpu(); // no GPU
        let remote_caps = caps_with_gpu();
        let index = make_index_with_nodes(vec![(0x2222, remote_caps)]);
        let scheduler = Scheduler::new(index, 0x1111, local_caps);

        let filter = CapabilityFilter::new().require_gpu();
        let decision = scheduler.place(&filter).unwrap();
        assert_eq!(decision.node_id, 0x2222);
        assert_eq!(decision.reason, PlacementReason::OnlyCandidate);
    }

    #[test]
    fn test_no_candidate() {
        let local_caps = caps_no_gpu();
        let index = make_index_with_nodes(vec![]);
        let scheduler = Scheduler::new(index, 0x1111, local_caps);

        let filter = CapabilityFilter::new().require_gpu();
        assert_eq!(
            scheduler.place(&filter).unwrap_err(),
            SchedulerError::NoCandidate
        );
    }

    #[test]
    fn test_pin() {
        let index = make_index_with_nodes(vec![]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let decision = scheduler.pin(0x9999);
        assert_eq!(decision.node_id, 0x9999);
        assert_eq!(decision.reason, PlacementReason::Pinned);
    }

    #[test]
    fn test_can_run_locally() {
        let local_caps = caps_with_gpu();
        let index = make_index_with_nodes(vec![]);
        let scheduler = Scheduler::new(index, 0x1111, local_caps);

        assert!(scheduler.can_run_locally(&CapabilityFilter::new().require_gpu()));
        assert!(scheduler.can_run_locally(&CapabilityFilter::default()));
    }

    fn caps_with_migration_tag() -> CapabilitySet {
        CapabilitySet::new().add_tag("subprotocol:0x0500")
    }

    #[test]
    fn test_find_migration_targets() {
        let index = make_index_with_nodes(vec![
            (0x2222, caps_with_migration_tag()),
            (0x3333, caps_with_migration_tag()),
            (0x4444, caps_no_gpu()), // no migration tag
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let targets = scheduler.find_migration_targets(&CapabilityFilter::default(), 0x1111);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&0x2222));
        assert!(targets.contains(&0x3333));
    }

    #[test]
    fn test_find_migration_targets_excludes_source() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag()), // source
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let targets = scheduler.find_migration_targets(&CapabilityFilter::default(), 0x1111);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], 0x2222);
    }

    #[test]
    fn test_place_migration() {
        let index = make_index_with_nodes(vec![(0x2222, caps_with_migration_tag())]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let decision = scheduler
            .place_migration(&CapabilityFilter::default(), 0x1111)
            .unwrap();
        assert_eq!(decision.node_id, 0x2222);
        assert_eq!(decision.reason, PlacementReason::OnlyCandidate);
    }

    #[test]
    fn test_place_migration_no_targets() {
        let index = make_index_with_nodes(vec![
            (0x2222, caps_no_gpu()), // no migration tag
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let err = scheduler
            .place_migration(&CapabilityFilter::default(), 0x1111)
            .unwrap_err();
        assert_eq!(err, SchedulerError::NoCandidate);
    }

    #[test]
    fn test_place_migration_prefers_local() {
        let local_caps = caps_with_migration_tag();
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag()),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index, 0x1111, local_caps);

        // Source is 0x3333, so local (0x1111) is a valid target
        let decision = scheduler
            .place_migration(&CapabilityFilter::default(), 0x3333)
            .unwrap();
        assert_eq!(decision.node_id, 0x1111);
        assert_eq!(decision.reason, PlacementReason::LocalPreferred);
    }

    #[test]
    fn test_find_subprotocol_nodes() {
        let index = make_index_with_nodes(vec![
            (0x2222, CapabilitySet::new().add_tag("subprotocol:0x0400")),
            (0x3333, CapabilitySet::new().add_tag("subprotocol:0x0500")),
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let causal_nodes = scheduler.find_subprotocol_nodes(0x0400);
        assert_eq!(causal_nodes.len(), 1);
        assert_eq!(causal_nodes[0], 0x2222);

        let migration_nodes = scheduler.find_subprotocol_nodes(0x0500);
        assert_eq!(migration_nodes.len(), 1);
        assert_eq!(migration_nodes[0], 0x3333);
    }

    // ==================================================================
    // Phase G slice 1: select_migration_target via &dyn PlacementFilter
    // ==================================================================

    use crate::adapter::net::behavior::placement::{
        Artifact, LegacyPlacement, NodeId as PlacementNodeId, PlacementFilter, ResourceAxis,
        TieBreakContext,
    };

    /// Empty capability set — placeholder for Daemon artifact's
    /// `required` / `optional` slots.
    fn empty_caps_pf() -> CapabilitySet {
        CapabilitySet::default()
    }

    /// Synthetic placement filter that returns a fixed score for
    /// every candidate, optionally vetoing specific node ids via
    /// `None`.
    struct FixedScore {
        score: f32,
        veto: Vec<u64>,
    }

    impl PlacementFilter for FixedScore {
        fn placement_score(
            &self,
            target: &PlacementNodeId,
            _artifact: &Artifact<'_>,
        ) -> Option<f32> {
            if self.veto.contains(target) {
                None
            } else {
                Some(self.score)
            }
        }
    }

    fn daemon_artifact_pf<'a>(
        required: &'a CapabilitySet,
        optional: &'a CapabilitySet,
    ) -> Artifact<'a> {
        Artifact::Daemon {
            daemon_id: [0u8; 32],
            required,
            optional,
        }
    }

    /// Single eligible candidate → `select_migration_target`
    /// returns it.
    #[test]
    fn select_migration_target_returns_only_candidate() {
        let index = make_index_with_nodes(vec![(0x2222, caps_with_migration_tag())]);
        let scheduler = Scheduler::new(index.clone(), 0x1111, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = LegacyPlacement::permissive(&index);
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_migration_target(
            &artifact,
            &CapabilityFilter::default(),
            0x1111,
            &placement,
            &tb,
        );
        assert_eq!(target, Some(0x2222));
    }

    /// Multiple candidates with different scores → highest wins.
    #[test]
    fn select_migration_target_picks_highest_scoring() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag()),
            (0x2222, caps_with_migration_tag()),
            (0x3333, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);

        // Synthetic filter: 0x1111 → 0.3, 0x2222 → 0.9, 0x3333 → 0.5.
        struct ScoredFilter;
        impl PlacementFilter for ScoredFilter {
            fn placement_score(&self, t: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                Some(match *t {
                    0x1111 => 0.3,
                    0x2222 => 0.9,
                    0x3333 => 0.5,
                    _ => 0.0,
                })
            }
        }

        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_migration_target(
            &artifact,
            &CapabilityFilter::default(),
            0xFFFF,
            &ScoredFilter,
            &tb,
        );
        assert_eq!(target, Some(0x2222), "highest scorer wins");
    }

    /// Tied scores → tie-breaker resolves via lex NodeId fallback.
    #[test]
    fn select_migration_target_ties_resolved_by_lex_node_id() {
        let index = make_index_with_nodes(vec![
            (0x3333, caps_with_migration_tag()),
            (0x1111, caps_with_migration_tag()),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 0.5,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_migration_target(
            &artifact,
            &CapabilityFilter::default(),
            0xFFFF,
            &placement,
            &tb,
        );
        // All three score 0.5; tie-breaker step 3 (lex NodeId)
        // picks the lowest.
        assert_eq!(target, Some(0x1111));
    }

    /// Filter vetoes everyone → `None`.
    #[test]
    fn select_migration_target_returns_none_when_all_vetoed() {
        let index = make_index_with_nodes(vec![
            (0x2222, caps_with_migration_tag()),
            (0x3333, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 1.0,
            veto: vec![0x2222, 0x3333],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_migration_target(
            &artifact,
            &CapabilityFilter::default(),
            0xFFFF,
            &placement,
            &tb,
        );
        assert_eq!(target, None);
    }

    /// Empty candidate list (no migration-tagged nodes) → `None`.
    #[test]
    fn select_migration_target_returns_none_for_empty_candidates() {
        let index = make_index_with_nodes(vec![]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 1.0,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_migration_target(
            &artifact,
            &CapabilityFilter::default(),
            0xFFFF,
            &placement,
            &tb,
        );
        assert_eq!(target, None);
    }

    /// Source node is excluded from candidates (already pinned by
    /// `find_migration_targets`; pin again at the
    /// `select_migration_target` level).
    #[test]
    fn select_migration_target_excludes_source_node() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag()),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 1.0,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        // Source = 0x1111; target list excludes it. Only 0x2222 remains.
        let target = scheduler.select_migration_target(
            &artifact,
            &CapabilityFilter::default(),
            0x1111,
            &placement,
            &tb,
        );
        assert_eq!(target, Some(0x2222));
    }

    /// `daemon_filter` narrows candidates BEFORE scoring. A node
    /// without the required tag is excluded by
    /// `find_migration_targets`, so the placement filter never sees it.
    #[test]
    fn select_migration_target_honors_daemon_filter() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag().add_tag("hardware.gpu")),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 1.0,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        // Filter: must have hardware.gpu — narrows to 0x1111 only.
        let filter = CapabilityFilter::default().require_tag("hardware.gpu".to_string());
        let target = scheduler.select_migration_target(&artifact, &filter, 0xFFFF, &placement, &tb);
        assert_eq!(target, Some(0x1111));
    }

    // ==================================================================
    // Phase G slice 3: select_member_node + select_promotion_target
    // ==================================================================

    use std::collections::HashSet;

    /// `select_member_node` with empty exclusion set — picks the
    /// best-scoring candidate from the entire filter pool.
    /// Tie-breaker resolves equal scores via lex NodeId.
    #[test]
    fn select_member_node_picks_best_with_empty_exclusion() {
        let index = make_index_with_nodes(vec![
            (0x3333, caps_no_gpu()),
            (0x1111, caps_no_gpu()),
            (0x2222, caps_no_gpu()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 0.5,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };
        let exclude = HashSet::new();

        let target = scheduler.select_member_node(
            &artifact,
            &CapabilityFilter::default(),
            &exclude,
            &placement,
            &tb,
        );
        // All three score 0.5; lex tie-breaker picks the lowest.
        assert_eq!(target, Some(0x1111));
    }

    /// `select_member_node` honors the exclusion set — already-
    /// placed members aren't re-picked. Pin the spread-aware
    /// behavior replica groups depend on.
    #[test]
    fn select_member_node_excludes_already_placed_members() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_no_gpu()),
            (0x2222, caps_no_gpu()),
            (0x3333, caps_no_gpu()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 0.5,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        // 0x1111 is already a member; spread should pick the next-
        // lowest by lex order.
        let mut exclude = HashSet::new();
        exclude.insert(0x1111u64);

        let target = scheduler.select_member_node(
            &artifact,
            &CapabilityFilter::default(),
            &exclude,
            &placement,
            &tb,
        );
        assert_eq!(target, Some(0x2222));
    }

    /// `select_member_node`: exclusion set covers every candidate
    /// → `None`.
    #[test]
    fn select_member_node_returns_none_when_all_excluded() {
        let index = make_index_with_nodes(vec![(0x1111, caps_no_gpu()), (0x2222, caps_no_gpu())]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 0.5,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let mut exclude = HashSet::new();
        exclude.insert(0x1111u64);
        exclude.insert(0x2222u64);

        let target = scheduler.select_member_node(
            &artifact,
            &CapabilityFilter::default(),
            &exclude,
            &placement,
            &tb,
        );
        assert_eq!(target, None);
    }

    /// `select_member_node` honors the placement filter veto —
    /// excluded set + filter-vetoed candidates both drop out.
    #[test]
    fn select_member_node_combines_exclusion_with_filter_veto() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_no_gpu()),
            (0x2222, caps_no_gpu()),
            (0x3333, caps_no_gpu()),
        ]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        // Filter vetoes 0x2222.
        let placement = FixedScore {
            score: 1.0,
            veto: vec![0x2222],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };
        // Exclude 0x1111; with 0x2222 vetoed, only 0x3333 remains.
        let mut exclude = HashSet::new();
        exclude.insert(0x1111u64);

        let target = scheduler.select_member_node(
            &artifact,
            &CapabilityFilter::default(),
            &exclude,
            &placement,
            &tb,
        );
        assert_eq!(target, Some(0x3333));
    }

    /// `select_promotion_target` over a pre-computed roster
    /// (standby members) — picks the best by score + tie-break.
    #[test]
    fn select_promotion_target_picks_highest_scoring_standby() {
        let index = make_index_with_nodes(vec![(0x1111, caps_no_gpu()), (0x2222, caps_no_gpu())]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);

        // Synthetic filter: 0x1111 → 0.3, 0x2222 → 0.9.
        struct ScoredFilter;
        impl PlacementFilter for ScoredFilter {
            fn placement_score(&self, t: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                Some(match *t {
                    0x1111 => 0.3,
                    0x2222 => 0.9,
                    _ => 0.0,
                })
            }
        }
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let standbys = vec![0x1111u64, 0x2222u64];
        let target = scheduler.select_promotion_target(standbys, &artifact, &ScoredFilter, &tb);
        assert_eq!(target, Some(0x2222));
    }

    /// `select_promotion_target` over an empty roster → `None`.
    #[test]
    fn select_promotion_target_empty_roster_returns_none() {
        let index = make_index_with_nodes(vec![]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 1.0,
            veto: vec![],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_promotion_target(vec![], &artifact, &placement, &tb);
        assert_eq!(target, None);
    }

    /// `select_promotion_target` propagates filter vetoes — every
    /// candidate vetoed → `None`.
    #[test]
    fn select_promotion_target_returns_none_when_all_vetoed() {
        let index = make_index_with_nodes(vec![(0x1111, caps_no_gpu()), (0x2222, caps_no_gpu())]);
        let scheduler = Scheduler::new(index.clone(), 0xFFFF, caps_no_gpu());
        let req = empty_caps_pf();
        let opt = empty_caps_pf();
        let artifact = daemon_artifact_pf(&req, &opt);
        let placement = FixedScore {
            score: 1.0,
            veto: vec![0x1111, 0x2222],
        };
        let tb = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        let target = scheduler.select_promotion_target(
            vec![0x1111u64, 0x2222u64],
            &artifact,
            &placement,
            &tb,
        );
        assert_eq!(target, None);
    }

    // ==================================================================
    // Phase G slice 8: place_migration_v2 — default migration path
    //
    // Pin three guarantees:
    //   1. v2 always stamps `PlacementReason::BestScore` (telemetry
    //      contract — operators can grep for the v2 path).
    //   2. v2 finds the same eligibility set as v1 (uses
    //      `LegacyPlacement::permissive` against the same index +
    //      same `daemon_filter` narrowing via
    //      `find_migration_targets`).
    //   3. v2 excludes the source node and uses §7-LOCKED tie-breaker
    //      (lex NodeId fallback when no RTT data) — yields a
    //      deterministic pick across multiple eligible candidates.
    //   4. v1 (`place_migration`) still works — kept around for
    //      callers who explicitly want the legacy `LocalPreferred`
    //      / `FirstMatch` reasons.
    // ==================================================================

    /// `place_migration_v2` stamps `BestScore` on every successful
    /// pick. Pins the telemetry contract that distinguishes v2 from
    /// v1 (`LocalPreferred` / `OnlyCandidate` / `FirstMatch`).
    #[test]
    fn place_migration_v2_stamps_best_score_reason() {
        let index = make_index_with_nodes(vec![
            (0x2222, caps_with_migration_tag()),
            (0x3333, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let decision = scheduler
            .place_migration_v2(&CapabilityFilter::default(), 0x1111)
            .expect("two eligible targets, source excluded");

        assert_eq!(decision.reason, PlacementReason::BestScore);
        // 0x2222 wins lex tie-break over 0x3333 (no RTT data, all
        // candidates score 1.0 via LegacyPlacement::permissive).
        assert_eq!(decision.node_id, 0x2222);
    }

    /// `place_migration_v2` excludes the source node — v2 honors
    /// the same source-exclusion contract as v1.
    #[test]
    fn place_migration_v2_excludes_source_node() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag()),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index, 0xFFFF, caps_no_gpu());

        // Source = 0x1111; v2 must pick 0x2222.
        let decision = scheduler
            .place_migration_v2(&CapabilityFilter::default(), 0x1111)
            .unwrap();
        assert_eq!(decision.node_id, 0x2222);
        assert_eq!(decision.reason, PlacementReason::BestScore);
    }

    /// `place_migration_v2` returns `NoCandidate` when nothing
    /// matches — same failure mode as v1.
    #[test]
    fn place_migration_v2_returns_no_candidate_when_nothing_matches() {
        let index = make_index_with_nodes(vec![
            (0x2222, caps_no_gpu()), // no migration tag
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_no_gpu());

        let err = scheduler
            .place_migration_v2(&CapabilityFilter::default(), 0x1111)
            .unwrap_err();
        assert_eq!(err, SchedulerError::NoCandidate);
    }

    /// `place_migration_v2` honors the daemon_filter — a node
    /// without the required tag is excluded by
    /// `find_migration_targets` before scoring (same path as v1).
    #[test]
    fn place_migration_v2_honors_daemon_filter() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag().add_tag("hardware.gpu")),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index, 0xFFFF, caps_no_gpu());

        // Filter requires hardware.gpu — narrows candidate pool to
        // 0x1111. Source 0x9999 doesn't intersect either candidate.
        let filter = CapabilityFilter::default().require_tag("hardware.gpu".to_string());
        let decision = scheduler
            .place_migration_v2(&filter, 0x9999)
            .expect("0x1111 is the only filter-passing candidate");
        assert_eq!(decision.node_id, 0x1111);
        assert_eq!(decision.reason, PlacementReason::BestScore);
    }

    /// CR-6: NaN scores from custom `PlacementFilter` impls (e.g.
    /// FFI-registered Python/JS predicates) must not poison the
    /// placement sort. `Vec::sort_by` over a non-total ordering is
    /// undefined; `pick_best_candidate` must drop NaN candidates at
    /// the boundary so the surviving sort is a strict total order.
    #[test]
    fn pick_best_candidate_drops_nan_scores() {
        use crate::adapter::net::behavior::placement::{
            Artifact, NodeId, PlacementFilter, ResourceAxis, TieBreakContext,
        };

        struct NaNFilter;
        impl PlacementFilter for NaNFilter {
            fn placement_score(&self, target: &NodeId, _: &Artifact<'_>) -> Option<f32> {
                // Node 0x2222 — NaN score (must be dropped).
                // All others — finite score 1.0 (eligible).
                if *target == 0x2222 {
                    Some(f32::NAN)
                } else {
                    Some(1.0)
                }
            }
        }

        let required = CapabilitySet::new();
        let optional = CapabilitySet::new();
        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &required,
            optional: &optional,
        };
        let index = CapabilityIndex::new();
        let tie_break = TieBreakContext {
            rtt_lookup: None,
            index: &index,
            resource_axis: ResourceAxis::Compute,
        };

        // 64 runs to make any ordering randomness extremely
        // unlikely to mask the bug if the filter regresses.
        for run in 0..64 {
            let pick = Scheduler::pick_best_candidate(
                vec![0x1111u64, 0x2222u64, 0x3333u64],
                &artifact,
                &NaNFilter,
                &tie_break,
            );
            // 0x2222 (NaN) must NEVER win; 0x1111 wins by lex
            // tie-break against 0x3333 (both score 1.0).
            assert_eq!(pick, Some(0x1111), "run {run}: NaN candidate must be dropped");
        }
    }

    /// `place_migration` (v1) is still available and unchanged.
    /// Pins the back-compat guarantee that callers who explicitly
    /// want v1's `LocalPreferred` reason can keep using it.
    #[test]
    fn place_migration_v1_still_returns_legacy_reasons() {
        let index = make_index_with_nodes(vec![
            (0x1111, caps_with_migration_tag()),
            (0x2222, caps_with_migration_tag()),
        ]);
        let scheduler = Scheduler::new(index, 0x1111, caps_with_migration_tag());

        // Source 0x3333; local 0x1111 is eligible → LocalPreferred.
        let decision = scheduler
            .place_migration(&CapabilityFilter::default(), 0x3333)
            .unwrap();
        assert_eq!(decision.node_id, 0x1111);
        assert_eq!(
            decision.reason,
            PlacementReason::LocalPreferred,
            "v1 path should still stamp LocalPreferred (legacy contract)"
        );
    }
}
