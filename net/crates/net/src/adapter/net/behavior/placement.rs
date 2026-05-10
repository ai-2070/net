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
}
