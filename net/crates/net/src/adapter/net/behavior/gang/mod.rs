//! Gang-claim scheduler ("Thunderdome") — contended GPU-island
//! arbitration over the substrate's [`ReservationFold`].
//!
//! Where the placement scheduler keeps daemon *placements* optimal
//! over time, this module answers the orthogonal question: *which of
//! N contending gang jobs atomically wins a contended GPU island,
//! right now, without double-booking it across a partition.* There
//! is no central coordinator — matching is a local read, the claim
//! is a CAS against a single-writer chain, and arbitration falls out
//! of the chain's total order.
//!
//! The pipeline (plan §2):
//!
//! ```text
//! affinity hint
//!   └─[1] CapabilityQuery::Composite  → candidate hosts   (capability fold, read)
//!        └─[2] numeric filter          → tightened islands (IslandTopology, read)
//!             └─[3] select             → ordered island list (pure fn)
//!                  └─[4] ReservationFold CAS                (the only commit)
//! ```
//!
//! Steps 1–3 ([`match_islands`]) are read-only and cheap — "match
//! narrows, CAS commits" (locked decision 4). Step 4 is the single
//! reservation CAS in [`claim`]; for a single-island gang that is the
//! whole claim, atomic and deadlock-free because the island *is* the
//! [`ResourceId`](crate::adapter::net::behavior::fold::ResourceId)
//! (locked decision 1).
//!
//! Phasing (see `docs/plans/MESH_SCHEDULER_GANG_CLAIM_PLAN.md`):
//! Phase A ships the topology fold + this read pipeline + the single-
//! island CAS; multi-island ordered-acquire (Phase C) and the
//! quorum-witnessed `→ Active` with a fencing epoch (Phase D) build
//! on top.
//!
//! [`ReservationFold`]: crate::adapter::net::behavior::fold::ReservationFold

pub mod active;
pub mod claim;
pub mod contention;
pub mod filter;
pub mod multi;
pub mod placement;
pub mod quorum;

pub use active::{commit_active, ActiveCommitOutcome, ReplicaCohort};
pub use claim::{
    activate_announcement, activate_island, release_announcement, release_island,
    reserve_announcement, single_island_claim, ClaimError, ClaimOutcome,
};
pub use contention::claim_first_available;
pub use filter::{
    candidate_hosts, numeric_filter, select_islands, select_with_affinity, NumericFilter,
    SelectionPolicy,
};
pub use multi::{acquire_gang, try_acquire_gang, AcquireAttempt, GangClaim, GangOutcome};
pub use placement::{colocated_island_config, pinned_island_replicas, COLOCATE_WITH_STRICT_KEY};
pub use quorum::{Epoch, FenceLedger, QuorumWitness, ReplicaSet};

use crate::adapter::net::behavior::fold::{
    CapabilityFold, CapabilityQuery, Fold, IslandId, IslandQuery, IslandRecord, IslandTopologyFold,
    ModelId,
};

/// Inputs to the read-only match→select pipeline ([`match_islands`],
/// plan §2 steps 1–3).
#[derive(Debug, Clone)]
pub struct MatchCriteria {
    /// Coarse capability prefilter (tags / state / region) — step 1.
    /// Typically a [`CapabilityQuery::Composite`].
    pub capability: CapabilityQuery,
    /// Live numeric constraints over the topology — step 2.
    pub numeric: NumericFilter,
    /// Claim-order policy — step 3.
    pub selection: SelectionPolicy,
    /// Soft warm-model affinity (step 3): islands with this model
    /// already resident rank ahead of cold ones, within the selection
    /// policy. `None` = no affinity. Distinct from
    /// [`NumericFilter::require_warm_model`], which is a hard filter.
    pub prefer_warm_model: Option<ModelId>,
}

/// Run the read-only match→select pipeline: coarse capability match
/// → candidate hosts → their live island records → numeric filter →
/// selection ordering. Returns the islands to attempt claiming, in
/// order (best first). Pure read over both folds; safe to run
/// optimistically and re-run on a claim reject (plan §2).
///
/// An empty result means nothing matched — no host carried the
/// required capability tags, or none of their islands passed the
/// numeric filter. The caller queues / backs off (Phase E).
pub fn match_islands(
    capability_fold: &Fold<CapabilityFold>,
    topology_fold: &Fold<IslandTopologyFold>,
    criteria: &MatchCriteria,
) -> Vec<IslandId> {
    // [1] coarse capability match → candidate hosts.
    let matches = capability_fold.query(criteria.capability.clone());
    let hosts = candidate_hosts(&matches);
    if hosts.is_empty() {
        return Vec::new();
    }
    // [2] live island records on those hosts, numeric-filtered.
    let candidates: Vec<IslandRecord> = topology_fold
        .query(IslandQuery::All)
        .into_iter()
        .map(|(_, record)| record)
        .filter(|record| hosts.contains(&record.host) && criteria.numeric.accepts(record))
        .collect();
    // [3] selection ordering (with soft warm-model affinity) → claim
    // order.
    select_with_affinity(candidates, criteria.selection, criteria.prefer_warm_model)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        CapabilityFilter, CapabilityMembership, EnvelopeMeta, Fold, FoldKind, GpuSet, IslandRecord,
        IslandTopologyFold, NodeState, ReservationFold, ReservationQuery, ReservationState,
        SignedAnnouncement,
    };
    use crate::adapter::net::current_timestamp_micros;
    use crate::adapter::net::identity::EntityKeypair;

    /// Announce `node` as carrying `tags` in the capability fold,
    /// `Idle` and accepting work.
    fn announce_capability(
        fold: &Fold<CapabilityFold>,
        kp: &EntityKeypair,
        node: u64,
        tags: Vec<String>,
    ) {
        let membership = CapabilityMembership {
            class_hash: 0x6770_75, // "gpu" — any stable class id
            tags,
            hardware: None,
            state: NodeState::Idle,
            region: None,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
        };
        let ann = SignedAnnouncement::sign(
            kp,
            CapabilityFold::KIND_ID,
            membership.class_hash,
            node,
            1,
            EnvelopeMeta::default(),
            membership,
        )
        .expect("sign cap");
        fold.apply(ann).expect("apply cap");
    }

    /// Announce island `id` hosted by `node` with `load`.
    fn announce_island(
        fold: &Fold<IslandTopologyFold>,
        kp: &EntityKeypair,
        node: u64,
        id: IslandId,
        gpus: usize,
        load: f32,
    ) {
        let record = IslandRecord {
            id,
            gpus: GpuSet::new((0..gpus as u32).collect()),
            host: node,
            warm_models: vec![0xA1],
            load,
            p50_latency_us: 1_500,
        };
        let ann = SignedAnnouncement::sign(
            kp,
            IslandTopologyFold::KIND_ID,
            0,
            node,
            1,
            EnvelopeMeta::default(),
            record,
        )
        .expect("sign island");
        fold.apply(ann).expect("apply island");
    }

    fn new_fold<K: crate::adapter::net::behavior::fold::FoldKind>() -> Fold<K> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    #[test]
    fn match_islands_narrows_by_capability_then_numeric_then_orders() {
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let kp_c = EntityKeypair::generate();
        let (na, nb, nc) = (
            kp_a.entity_id().node_id(),
            kp_b.entity_id().node_id(),
            kp_c.entity_id().node_id(),
        );

        // A and B carry the gpu:h100 tag; C does not.
        announce_capability(&caps, &kp_a, na, vec!["gpu:h100".into()]);
        announce_capability(&caps, &kp_b, nb, vec!["gpu:h100".into()]);
        announce_capability(&caps, &kp_c, nc, vec!["gpu:a10".into()]);

        // A hosts two islands (loads 0.6, 0.2); B one (load 0.4);
        // C one (load 0.0) — but C is filtered out at step 1.
        announce_island(&topo, &kp_a, na, 0xA0, 8, 0.6);
        announce_island(&topo, &kp_a, na, 0xA5, 8, 0.2);
        announce_island(&topo, &kp_b, nb, 0xB0, 8, 0.4);
        announce_island(&topo, &kp_c, nc, 0xC0, 8, 0.0);

        let criteria = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter {
                min_gpus: 8,
                max_load: Some(0.5),
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_warm_model: None,
        };

        let order = match_islands(&caps, &topo, &criteria);
        // C's island (0xC0) excluded by capability; A's 0xA0 excluded
        // by load>0.5. Remaining: A's 0xA5 (0.2) then B's 0xB0 (0.4),
        // least-loaded first.
        assert_eq!(order, vec![0xA5, 0xB0]);
    }

    #[test]
    fn match_islands_empty_when_no_capability_match() {
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let kp = EntityKeypair::generate();
        let n = kp.entity_id().node_id();
        announce_capability(&caps, &kp, n, vec!["gpu:a10".into()]);
        announce_island(&topo, &kp, n, 0xA0, 8, 0.1);

        let criteria = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter::default(),
            selection: SelectionPolicy::LeastLoaded,
            prefer_warm_model: None,
        };
        assert!(match_islands(&caps, &topo, &criteria).is_empty());
    }

    /// End-to-end Phase A "done when": match → claim the top island
    /// via the existing CAS → run (Active) → release.
    #[test]
    fn pipeline_then_claim_run_release() {
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let reservations: Fold<ReservationFold> = new_fold();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();

        announce_capability(&caps, &kp, node, vec!["gpu:h100".into()]);
        announce_island(&topo, &kp, node, 0xA0, 8, 0.3);

        let criteria = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter {
                min_gpus: 8,
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_warm_model: None,
        };

        let order = match_islands(&caps, &topo, &criteria);
        let island = *order.first().expect("a candidate island");
        assert_eq!(island, 0xA0);

        let deadline = current_timestamp_micros() + 60_000_000;
        assert_eq!(
            single_island_claim(&reservations, &kp, node, 1, island, deadline).unwrap(),
            ClaimOutcome::Won,
        );
        assert_eq!(
            activate_island(&reservations, &kp, node, 2, island, 0x42).unwrap(),
            ClaimOutcome::Won,
        );
        assert!(matches!(
            reservations.query(ReservationQuery::State(island))[0].1,
            ReservationState::Active { job_id: 0x42, .. }
        ));
        assert_eq!(
            release_island(&reservations, &kp, node, 3, island).unwrap(),
            ClaimOutcome::Won,
        );
        assert_eq!(
            reservations.query(ReservationQuery::State(island))[0].1,
            ReservationState::Free,
        );
    }
}
