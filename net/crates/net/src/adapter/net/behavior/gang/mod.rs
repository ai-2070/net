//! Gang-claim scheduler ("Thunderdome") — contended resource-island
//! arbitration over the substrate's [`ReservationFold`].
//!
//! Where the placement scheduler keeps daemon *placements* optimal
//! over time, this module answers the orthogonal question: *which of
//! N contending gang jobs atomically wins a contended island of
//! exclusive units, right now, without double-booking it across a
//! partition.* (A GPU NVLink domain is the motivating instance; the
//! mechanism is resource-agnostic.) There
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
// Ties the island's replica placement to RedEX's `ReplicationConfig`
// / `PlacementStrategy` (plan §5), so it rides the `redex` feature —
// a plain `--features net` build has no replication layer to
// configure. (Pre-SI-2a this was ungated and broke the net-only
// build; every other gang module is fold-only and stays ungated.)
#[cfg(feature = "redex")]
pub mod placement;
pub mod quorum;
pub mod schedule;

#[cfg(test)]
mod proptest;

pub use active::{commit_active, ActiveCommitOutcome, ReplicaCohort};
pub use claim::{
    activate_announcement, activate_island, release_announcement, release_island,
    reserve_announcement, single_island_claim, ClaimError, ClaimOutcome, Claimant,
};
pub use contention::claim_first_available;
pub use filter::{
    candidate_hosts, numeric_filter, select_islands, select_with_affinity, NumericFilter,
    SelectionPolicy,
};
pub use multi::{acquire_gang, try_acquire_gang, AcquireAttempt, GangClaim, GangOutcome};
#[cfg(feature = "redex")]
pub use placement::{colocated_island_config, pinned_island_replicas, COLOCATE_WITH_STRICT_KEY};
pub use quorum::{Epoch, FenceLedger, QuorumWitness, ReplicaSet};
pub use schedule::{
    schedule_gang, schedule_single, GangRequest, GangScheduler, ScheduleError, Scheduled,
};

use std::collections::HashSet;

use crate::adapter::net::behavior::fold::{
    CapabilityFold, CapabilityQuery, Fold, IslandId, IslandQuery, IslandRecord, IslandTopologyFold,
    NodeId,
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
    /// Soft capability affinity (step 3): islands with this capability
    /// already resident rank ahead of the rest, within the selection
    /// policy. `None` = no affinity. Distinct from
    /// [`NumericFilter::require_all`] / [`NumericFilter::require_any`],
    /// which are hard filters.
    pub prefer_capability: Option<String>,
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
    down_nodes: &HashSet<NodeId>,
) -> Vec<IslandId> {
    // [1] coarse capability match → candidate hosts.
    let matches = capability_fold.query(criteria.capability.clone());
    let mut hosts = candidate_hosts(&matches);
    // Liveness gate (MeshOS ↔ Scheduler Projection 4): drop hosts MeshOS
    // currently observes as Unreachable *before* the island query, so
    // neither a dead host's capability match nor its islands can ever be
    // offered. Pruning the candidate-host set here — rather than mutating
    // either fold — leaves both folds' CRDT-grade AP state byte-identical,
    // and skips the candidate-then-filter work of fetching dead-node
    // islands only to discard them. `down_nodes` empty ⇒ no-op.
    if !down_nodes.is_empty() {
        hosts.retain(|host| !down_nodes.contains(host));
    }
    if hosts.is_empty() {
        return Vec::new();
    }
    // [2] live island records on those hosts, numeric-filtered. The
    // HostedByAny query filters by host inside the fold's single scan,
    // so only candidate-host islands are cloned (not the whole
    // topology, then discarded) — this runs on every claim retry.
    let candidates: Vec<IslandRecord> = topology_fold
        .query(IslandQuery::HostedByAny(hosts))
        .into_iter()
        .map(|(_, record)| record)
        .filter(|record| criteria.numeric.accepts(record))
        .collect();
    // [3] selection ordering (with soft capability affinity) → claim
    // order.
    select_with_affinity(
        candidates,
        criteria.selection,
        criteria.prefer_capability.clone(),
    )
}

/// SI-6 (sensing plan §6/§4.9): [`match_islands`] with the sensed
/// per-interest candidate delta joined at the SAME seam as the
/// liveness gate. Hosts in `sensed_non_viable` (explicitly NotReady
/// for THIS interest) are pruned from THIS match exactly like down
/// hosts — the fold state stays byte-identical and the entry-level
/// suspension flag is never touched (§4.9 reserves it for
/// *unconditional* loss: one conditional observation must never
/// suspend the capability entry or affect any OTHER match). The
/// final claim order is then re-ranked so islands hosted by
/// `sensed_viable_order` providers come first, in that order (the
/// aggregate's own consumer-local economics — which is what makes
/// the first successful claim target the SELECTED provider); the
/// re-rank is STABLE, so islands within one band — and every island
/// of an unsensed/potential host — keep the selection policy's
/// order. Both inputs empty ⇒ byte-identical to [`match_islands`]:
/// absence of evidence never prunes and never reorders.
pub fn match_islands_sensed(
    capability_fold: &Fold<CapabilityFold>,
    topology_fold: &Fold<IslandTopologyFold>,
    criteria: &MatchCriteria,
    down_nodes: &HashSet<NodeId>,
    sensed_non_viable: &HashSet<NodeId>,
    sensed_viable_order: &[NodeId],
) -> Vec<IslandId> {
    let pruned: HashSet<NodeId> = if sensed_non_viable.is_empty() {
        down_nodes.clone()
    } else {
        down_nodes.union(sensed_non_viable).copied().collect()
    };
    let mut ordered = match_islands(capability_fold, topology_fold, criteria, &pruned);
    if sensed_viable_order.is_empty() || ordered.len() < 2 {
        return ordered;
    }
    // SI-6 review (non-blocking note, taken) + SI-6.1 closure
    // refinement: derive the island → band map from ONE literal
    // topology snapshot — a single `All` scan, not one `Get` per
    // island (separate fold reads could interleave with concurrent
    // updates and hand the sort a mixed-time view). No fold queries
    // inside the O(n log n) sort.
    let snapshot: std::collections::HashMap<IslandId, NodeId> = topology_fold
        .query(IslandQuery::All)
        .into_iter()
        .map(|(island, record)| (island, record.host))
        .collect();
    let bands: std::collections::HashMap<IslandId, usize> = ordered
        .iter()
        .map(|island| {
            let band = snapshot
                .get(island)
                .and_then(|host| {
                    sensed_viable_order
                        .iter()
                        .position(|provider| provider == host)
                })
                // Unsensed / potential hosts form the trailing
                // band, in the selection policy's own order.
                .unwrap_or(usize::MAX);
            (*island, band)
        })
        .collect();
    // Stable: within a band, the [3]-step selection order survives.
    ordered.sort_by_key(|island| bands.get(island).copied().unwrap_or(usize::MAX));
    ordered
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        CapabilityFilter, CapabilityMembership, EnvelopeMeta, Fold, FoldKind, IslandRecord,
        IslandTopologyFold, NodeState, ReservationFold, ReservationQuery, ReservationState,
        SignedAnnouncement, UnitSet,
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
        announce_capability_in(fold, kp, node, tags, None);
    }

    /// Like [`announce_capability`] but with a host `region` (the
    /// network-locality axis subnet / zone filtering rides).
    fn announce_capability_in(
        fold: &Fold<CapabilityFold>,
        kp: &EntityKeypair,
        node: u64,
        tags: Vec<String>,
        region: Option<String>,
    ) {
        let membership = CapabilityMembership {
            class_hash: 0x67_70_75, // "gpu" — any stable class id
            tags,
            hardware: None,
            state: NodeState::Idle,
            region,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
            owner: None,
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
        units: usize,
        load: f32,
    ) {
        let record = IslandRecord {
            id,
            units: UnitSet::new((0..units as u32).collect()),
            host: node,
            capabilities: vec!["model:a1".into()],
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
                min_units: 8,
                max_load: Some(0.5),
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_capability: None,
        };

        let order = match_islands(&caps, &topo, &criteria, &HashSet::new());
        // C's island (0xC0) excluded by capability; A's 0xA0 excluded
        // by load>0.5. Remaining: A's 0xA5 (0.2) then B's 0xB0 (0.4),
        // least-loaded first.
        assert_eq!(order, vec![0xA5, 0xB0]);
    }

    #[test]
    fn sensed_match_prunes_non_viable_and_ranks_viable_first() {
        // SI-6: three hosts carry the tag; the sensed delta says C is
        // best-ranked viable, A is viable second, B is explicitly
        // NotReady for THIS interest. B's islands are pruned exactly
        // like a down host's; C's islands lead the claim order even
        // where the selection policy (least-loaded) would have put A
        // first — the first claim targets the SELECTED provider.
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
        for (kp, node) in [(&kp_a, na), (&kp_b, nb), (&kp_c, nc)] {
            announce_capability(&caps, kp, node, vec!["gpu:h100".into()]);
        }
        announce_island(&topo, &kp_a, na, 0xA0, 8, 0.1);
        announce_island(&topo, &kp_b, nb, 0xB0, 8, 0.2);
        announce_island(&topo, &kp_c, nc, 0xC0, 8, 0.3);

        let criteria = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter {
                min_units: 8,
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_capability: None,
        };

        // Baseline (no sensing): least-loaded order.
        assert_eq!(
            match_islands(&caps, &topo, &criteria, &HashSet::new()),
            vec![0xA0, 0xB0, 0xC0],
        );

        let non_viable: HashSet<NodeId> = [nb].into_iter().collect();
        let order = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &non_viable,
            &[nc, na],
        );
        assert_eq!(
            order,
            vec![0xC0, 0xA0],
            "NotReady host pruned; sensed rank leads the claim order",
        );

        // The §4.9 tripwire: the sensed prune is PER-MATCH state —
        // no fold was mutated, so the plain match (any other
        // interest, any other consumer) still sees every host.
        assert_eq!(
            match_islands(&caps, &topo, &criteria, &HashSet::new()),
            vec![0xA0, 0xB0, 0xC0],
            "one interest's NotReady never suspends the entry",
        );
    }

    #[test]
    fn sensed_match_with_empty_delta_is_identical_and_potential_is_never_pruned() {
        // Absence of evidence never prunes and never reorders: an
        // empty sensed delta must reproduce match_islands exactly,
        // and hosts OUTSIDE the viable order (potential/unsensed)
        // keep the selection policy's order behind the viable band.
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let (na, nb) = (kp_a.entity_id().node_id(), kp_b.entity_id().node_id());
        announce_capability(&caps, &kp_a, na, vec!["gpu:h100".into()]);
        announce_capability(&caps, &kp_b, nb, vec!["gpu:h100".into()]);
        announce_island(&topo, &kp_a, na, 0xA0, 8, 0.1);
        announce_island(&topo, &kp_b, nb, 0xB0, 8, 0.2);

        let criteria = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter {
                min_units: 8,
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_capability: None,
        };

        let plain = match_islands(&caps, &topo, &criteria, &HashSet::new());
        assert_eq!(
            match_islands_sensed(
                &caps,
                &topo,
                &criteria,
                &HashSet::new(),
                &HashSet::new(),
                &[],
            ),
            plain,
            "empty sensed delta ⇒ byte-identical to match_islands",
        );
        // B sensed viable, A unsensed (potential): B's band leads,
        // A is retained behind it — never pruned.
        assert_eq!(
            match_islands_sensed(
                &caps,
                &topo,
                &criteria,
                &HashSet::new(),
                &HashSet::new(),
                &[nb],
            ),
            vec![0xB0, 0xA0],
            "potential hosts trail the viable band but are never pruned",
        );
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
            prefer_capability: None,
        };
        assert!(match_islands(&caps, &topo, &criteria, &HashSet::new()).is_empty());
    }

    /// MeshOS ↔ Scheduler Projection 4: a host MeshOS observes as down is
    /// pruned from the candidate set before the island query, so its
    /// islands are never offered — without mutating either fold.
    #[test]
    fn dead_host_islands_are_pruned_from_matching() {
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let na = kp_a.entity_id().node_id();
        let nb = kp_b.entity_id().node_id();
        announce_capability(&caps, &kp_a, na, vec!["gpu:h100".into()]);
        announce_capability(&caps, &kp_b, nb, vec!["gpu:h100".into()]);
        announce_island(&topo, &kp_a, na, 0xA0, 8, 0.1);
        announce_island(&topo, &kp_b, nb, 0xB0, 8, 0.2);

        let criteria = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter::default(),
            selection: SelectionPolicy::LeastLoaded,
            prefer_capability: None,
        };

        // No nodes down → both islands match (least-loaded first).
        assert_eq!(
            match_islands(&caps, &topo, &criteria, &HashSet::new()),
            vec![0xA0, 0xB0],
        );

        // Host A down → only B's island survives the host prune.
        let a_down: HashSet<NodeId> = [na].into_iter().collect();
        assert_eq!(match_islands(&caps, &topo, &criteria, &a_down), vec![0xB0]);

        // Both down → nothing offered.
        let both_down: HashSet<NodeId> = [na, nb].into_iter().collect();
        assert!(match_islands(&caps, &topo, &criteria, &both_down).is_empty());
    }

    /// Subnet / region / zone is a **host** property (network locality),
    /// so it filters at the capability stage (step 1) — never the island
    /// stage. Two hosts carry the same capability + an equivalent island
    /// and differ only in region; a region-scoped match returns only the
    /// in-region host's island, because the out-of-region host is dropped
    /// before its islands are ever inspected.
    #[test]
    fn region_filters_at_the_host_stage_not_the_island() {
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let kp_east = EntityKeypair::generate();
        let kp_west = EntityKeypair::generate();
        let ne = kp_east.entity_id().node_id();
        let nw = kp_west.entity_id().node_id();

        announce_capability_in(
            &caps,
            &kp_east,
            ne,
            vec!["gpu:h100".into()],
            Some("us-east".into()),
        );
        announce_capability_in(
            &caps,
            &kp_west,
            nw,
            vec!["gpu:h100".into()],
            Some("us-west".into()),
        );
        announce_island(&topo, &kp_east, ne, 0xE0, 8, 0.1);
        announce_island(&topo, &kp_west, nw, 0xF0, 8, 0.1);

        // Region-scoped: only the us-east host's island survives. The
        // west host never reaches the numeric/topology stage.
        let east_only = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                region: Some("us-east".into()),
                ..Default::default()
            }),
            numeric: NumericFilter::default(),
            selection: SelectionPolicy::LeastLoaded,
            prefer_capability: None,
        };
        assert_eq!(
            match_islands(&caps, &topo, &east_only, &HashSet::new()),
            vec![0xE0]
        );

        // No region constraint → both hosts' islands match.
        let any_region = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            ..east_only.clone()
        };
        let mut both = match_islands(&caps, &topo, &any_region, &HashSet::new());
        both.sort_unstable();
        assert_eq!(both, vec![0xE0, 0xF0]);

        // A region nobody is in → empty.
        let nowhere = MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                region: Some("ap-south".into()),
                ..Default::default()
            }),
            ..east_only.clone()
        };
        assert!(match_islands(&caps, &topo, &nowhere, &HashSet::new()).is_empty());
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
                min_units: 8,
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_capability: None,
        };

        let order = match_islands(&caps, &topo, &criteria, &HashSet::new());
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
