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
//! Phasing (see `docs/internal/plans/MESH_SCHEDULER_GANG_CLAIM_PLAN.md`):
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
    candidate_hosts, candidate_hosts_for, numeric_filter, select_islands, select_with_affinity,
    NumericFilter, SelectionPolicy,
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
    let candidates = match_island_records(capability_fold, topology_fold, criteria, down_nodes);
    // [3] selection ordering (with soft capability affinity) → claim
    // order.
    select_with_affinity(
        candidates,
        criteria.selection,
        criteria.prefer_capability.clone(),
    )
}

/// Steps 1–2 of [`match_islands`], stopping before the selection
/// ordering: the filter-passing island *records* from ONE topology
/// read.
///
/// Split out so [`match_islands_sensed`] can derive its band map
/// from the very records that produced the ordering, instead of
/// taking a second whole-topology snapshot to recover a field it
/// already had in hand (PERF_AUDIT_2026_07_31_GANG_SCHEDULER §3).
fn match_island_records(
    capability_fold: &Fold<CapabilityFold>,
    topology_fold: &Fold<IslandTopologyFold>,
    criteria: &MatchCriteria,
    down_nodes: &HashSet<NodeId>,
) -> Vec<IslandRecord> {
    // [1] coarse capability match → candidate hosts. Resolved
    // straight to node ids: the matched `CapabilityMembership`
    // payloads are never materialized, because this step keeps only
    // the host out of each (PERF_AUDIT_2026_07_31_GANG_SCHEDULER §1).
    // Borrowing the query also drops the per-round `CapabilityFilter`
    // clone — this runs on every retry round, not once per job.
    let mut hosts = candidate_hosts_for(capability_fold, &criteria.capability);
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
    topology_fold
        .query(IslandQuery::HostedByAny(hosts))
        .into_iter()
        .map(|(_, record)| record)
        .filter(|record| criteria.numeric.accepts(record))
        .collect()
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
///
/// # Snapshot contract (locked, PERF_AUDIT_2026_07_31_GANG_SCHEDULER §3)
///
/// Band assignment uses the **same topology snapshot that produced
/// the filtered and selected records**. Topology replacements or
/// evictions after that snapshot affect the next matcher invocation,
/// not the in-progress one.
///
/// This intentionally replaced a mixed-time read: selection used
/// snapshot T1 while banding re-read the fold for T2. Since banding
/// consults only `IslandRecord::host`, an ordinary same-host
/// heartbeat never moved a band either way; the observable
/// difference was an island **evicted or expired** between T1 and
/// T2, which T2 could no longer find and so dropped into the
/// trailing band — and, if the same `IslandId` was reinserted under
/// a different host, T2 banded it by the new host. One snapshot
/// makes both cases rank by what the matcher actually selected.
pub fn match_islands_sensed(
    capability_fold: &Fold<CapabilityFold>,
    topology_fold: &Fold<IslandTopologyFold>,
    criteria: &MatchCriteria,
    down_nodes: &HashSet<NodeId>,
    sensed_non_viable: &HashSet<NodeId>,
    sensed_viable_order: &[NodeId],
) -> Vec<IslandId> {
    // Borrowed when there is no sensed prune — the common path, and
    // the one the contract above calls "byte-identical to
    // match_islands" (§3).
    let pruned: std::borrow::Cow<'_, HashSet<NodeId>> = if sensed_non_viable.is_empty() {
        std::borrow::Cow::Borrowed(down_nodes)
    } else {
        std::borrow::Cow::Owned(down_nodes.union(sensed_non_viable).copied().collect())
    };
    // ONE topology read (T1). `candidates` carries the host field
    // banding needs, so no second scan is taken to recover it.
    let candidates = match_island_records(capability_fold, topology_fold, criteria, &pruned);
    // Bail out BEFORE building the band map, not after: on the
    // no-sensing path that map is never read, and building it there
    // would charge this path an allocation `match_islands` does not
    // pay — the same discarded-work shape §3 was opened against.
    // `select_with_affinity` is a 1:1 projection (`select_islands`
    // sorts and maps; the affinity arm partitions and concatenates —
    // neither drops a record), so `candidates.len()` is the
    // `ordered.len()` this guard used to read.
    if sensed_viable_order.is_empty() || candidates.len() < 2 {
        return select_with_affinity(
            candidates,
            criteria.selection,
            criteria.prefer_capability.clone(),
        );
    }
    let hosts: std::collections::HashMap<IslandId, NodeId> =
        candidates.iter().map(|r| (r.id, r.host)).collect();
    let mut ordered = select_with_affinity(
        candidates,
        criteria.selection,
        criteria.prefer_capability.clone(),
    );
    // Provider → rank, resolved once. The linear `position()` this
    // replaces ran per island, making band derivation
    // O(islands × providers). `or_insert` keeps first-occurrence
    // wins, exactly as `position()` did for a duplicated provider.
    let mut provider_rank: std::collections::HashMap<NodeId, usize> =
        std::collections::HashMap::with_capacity(sensed_viable_order.len());
    for (rank, provider) in sensed_viable_order.iter().enumerate() {
        provider_rank.entry(*provider).or_insert(rank);
    }
    let bands: std::collections::HashMap<IslandId, usize> = ordered
        .iter()
        .map(|island| {
            let band = hosts
                .get(island)
                .and_then(|host| provider_rank.get(host).copied())
                // Unsensed / potential hosts form the trailing
                // band, in the selection policy's own order.
                .unwrap_or(usize::MAX);
            (*island, band)
        })
        .collect();
    // Stable: within a band, the [3]-step selection order survives.
    // No fold queries inside the O(n log n) sort.
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

    /// Announce island `id` hosted by `node` with `load`, at
    /// `generation` — the heartbeat form, so a test can replace a
    /// live entry (`merge` requires a strictly-higher generation
    /// from the same publisher).
    fn announce_island_gen(
        fold: &Fold<IslandTopologyFold>,
        kp: &EntityKeypair,
        node: u64,
        id: IslandId,
        units: usize,
        load: f32,
        generation: u64,
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
            generation,
            EnvelopeMeta::default(),
            record,
        )
        .expect("sign island");
        fold.apply(ann).expect("apply island");
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

    // -----------------------------------------------------------
    // PERF_AUDIT_2026_07_31_GANG_SCHEDULER §3 — locked T1 snapshot.
    // -----------------------------------------------------------

    /// A three-host sensed fixture: A/B/C each carry the tag and
    /// host one island, loads ascending so the plain selection
    /// order is deterministic (A, B, C).
    fn sensed_fixture() -> (
        Fold<CapabilityFold>,
        Fold<IslandTopologyFold>,
        Vec<EntityKeypair>,
        Vec<NodeId>,
        MatchCriteria,
    ) {
        let caps: Fold<CapabilityFold> = new_fold();
        let topo: Fold<IslandTopologyFold> = new_fold();
        let kps: Vec<EntityKeypair> = (0..3).map(|_| EntityKeypair::generate()).collect();
        let nodes: Vec<NodeId> = kps.iter().map(|k| k.entity_id().node_id()).collect();
        for (i, (kp, node)) in kps.iter().zip(nodes.iter()).enumerate() {
            announce_capability(&caps, kp, *node, vec!["gpu:h100".into()]);
            announce_island(
                &topo,
                kp,
                *node,
                0xA0 + i as u64,
                8,
                0.1 + 0.1 * i as f32,
            );
        }
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
        (caps, topo, kps, nodes, criteria)
    }

    /// §3, the direct witness: the sensed matcher must read the
    /// topology fold EXACTLY ONCE. Pre-fix it took two snapshots —
    /// `HostedByAny` for selection, then a whole-topology `All`
    /// scan purely to recover `record.host` for banding — so this
    /// asserted a delta of 2. The fold's own query counter is the
    /// instrument, so the test cannot be satisfied by a faster
    /// second scan; only by not taking one.
    #[test]
    fn sensed_match_takes_exactly_one_topology_snapshot() {
        let (caps, topo, _kps, nodes, criteria) = sensed_fixture();

        let before = topo.metrics().queries();
        let order = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &HashSet::new(),
            // Non-empty and re-ranking, so the banding path runs.
            &[nodes[2], nodes[0]],
        );
        let delta = topo.metrics().queries() - before;

        assert_eq!(order.len(), 3, "the re-ranking path must have run");
        assert_eq!(
            delta, 1,
            "sensed banding must reuse the selection snapshot, not take a second one",
        );
    }

    /// §3 negative control: banding reads only `IslandRecord::host`,
    /// so an ordinary same-host heartbeat — new generation, new
    /// load, same host — must not move any band. This is the case
    /// that does NOT differ between the old two-snapshot read and
    /// the locked one-snapshot contract, and pinning it keeps the
    /// contract's scope honest.
    #[test]
    fn same_host_heartbeat_does_not_change_band_assignment() {
        let (caps, topo, kps, nodes, criteria) = sensed_fixture();
        let viable = [nodes[2], nodes[0]];

        let before = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &HashSet::new(),
            &viable,
        );
        // C's island re-announced by C at a higher generation with a
        // different load — a heartbeat. Host is unchanged.
        announce_island_gen(&topo, &kps[2], nodes[2], 0xA2, 8, 0.99, 2);

        let after = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &HashSet::new(),
            &viable,
        );
        assert_eq!(
            before, after,
            "a same-host heartbeat must not change band assignment",
        );
    }

    /// §3: an island evicted after a match affects the NEXT
    /// invocation, not the completed one. The locked contract says
    /// post-snapshot topology changes land on the next matcher run;
    /// this pins that the completed result is unaffected and the
    /// next one is.
    #[test]
    fn eviction_lands_on_the_next_invocation_not_the_completed_one() {
        let (caps, topo, _kps, nodes, criteria) = sensed_fixture();
        let viable = [nodes[2], nodes[0]];

        let first = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &HashSet::new(),
            &viable,
        );
        // C leads (best-ranked viable), then A, then unsensed B.
        assert_eq!(first, vec![0xA2, 0xA0, 0xA1]);

        // Evict C's island entirely.
        topo.evict_node(nodes[2], "test");

        let second = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &HashSet::new(),
            &viable,
        );
        assert_eq!(
            first,
            vec![0xA2, 0xA0, 0xA1],
            "the completed result must be unchanged by a later eviction",
        );
        assert_eq!(
            second,
            vec![0xA0, 0xA1],
            "the next invocation must reflect the eviction",
        );
    }

    /// §3: eviction followed by reinsertion of the same `IslandId`
    /// under a DIFFERENT host — constructible because `merge`'s
    /// first-writer pin only holds while the entry exists. The
    /// island must band by its new host on the next invocation,
    /// from the one snapshot that invocation takes.
    #[test]
    fn island_reinserted_under_a_new_host_bands_by_that_host() {
        let (caps, topo, kps, nodes, criteria) = sensed_fixture();
        // Rank B first, then A. C is unsensed (trailing band).
        let viable = [nodes[1], nodes[0]];

        assert_eq!(
            match_islands_sensed(
                &caps,
                &topo,
                &criteria,
                &HashSet::new(),
                &HashSet::new(),
                &viable,
            ),
            vec![0xA1, 0xA0, 0xA2],
            "B's island leads, then A's, then unsensed C's",
        );

        // C's island 0xA2 is evicted, then re-announced by B — the
        // same island id under a new host.
        topo.evict_node(nodes[2], "test");
        announce_island_gen(&topo, &kps[1], nodes[1], 0xA2, 8, 0.35, 1);

        let after = match_islands_sensed(
            &caps,
            &topo,
            &criteria,
            &HashSet::new(),
            &HashSet::new(),
            &viable,
        );
        assert_eq!(
            after,
            vec![0xA1, 0xA2, 0xA0],
            "0xA2 must band by its NEW host B (leading band), not its old host C",
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
