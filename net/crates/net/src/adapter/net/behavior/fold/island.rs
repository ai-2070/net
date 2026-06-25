//! `IslandTopologyFold` — folded GPU-island topology surface.
//!
//! An *island* is one NVLink domain — the unit a gang job actually
//! claims (plan locked decision 1). Each island carries one entry
//! whose payload is the host's self-announced [`IslandRecord`]:
//! its GPU set, host node, warm models, and the **live numeric
//! axes** (`load`, `p50_latency_us`).
//!
//! Those live axes are deliberately kept *here* and **not** in the
//! capability index: they churn every heartbeat, and baking them
//! into signed/replicated capability tags would cause tag-churn and
//! stale reads (plan locked decision 4 — "match narrows, CAS
//! commits"). The capability fold answers the coarse *which nodes
//! have H100s with NVLink* question; this fold answers the live
//! *and which island is least-loaded / has the model warm* question
//! the scheduler's numeric filter needs.
//!
//! ## Ownership
//!
//! `island_id = hash(host, nvlink_domain)`, so an island belongs to
//! exactly one host. [`IslandTopologyFold::merge`] enforces that a
//! node may only announce islands it hosts (`record.host` must equal
//! the publishing `node_id`) and that, once installed, only that
//! same publisher updates the entry (generation-monotonic
//! anti-reorder). A foreign node can neither publish a bogus record
//! for someone else's island nor take a key over. The residual — a
//! node squatting an island id it does not really host *before* the
//! true host announces — only poisons advisory match data; the
//! exclusive grant is the separate [`super::ReservationFold`] CAS
//! keyed by the same id, which is the real arbiter (plan §2: "match
//! narrows, CAS commits").
//!
//! `DEFAULT_TTL = 30s` matches the reservation fold: the live axes
//! are only useful fresh, and a host that stops heartbeating should
//! drop out of the topology within the window. See
//! `docs/plans/MESH_SCHEDULER_GANG_CLAIM_PLAN.md` §1.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::state::{FoldEntry, FoldState, MergeAction, NoIndex, NodeId};
use super::{FoldKind, SignedAnnouncement};

/// Island identifier — `hash(host, nvlink_domain)`. The same `u64`
/// space as [`super::ResourceId`], so a single-island gang claim is
/// one existing [`super::ReservationFold`] CAS with zero new code
/// (plan locked decision 1).
pub type IslandId = u64;

/// Model identifier — opaque `u64` (e.g. a content hash of the
/// model name / weights). A *warm* model is one already resident in
/// an island's GPU memory, so a job targeting it skips cold-load —
/// the warm-model-affinity axis the selection policy reads.
pub type ModelId = u64;

/// A GPU index within a host's NVLink domain.
pub type GpuId = u32;

/// The set of GPUs composing one NVLink island. Stored sorted +
/// deduped so [`GpuSet::intersects`] is a linear merge — the check
/// the "no two `Active` claims share a GPU" property leans on once
/// gangs span islands (Phase C/D).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuSet(Vec<GpuId>);

impl GpuSet {
    /// Build a `GpuSet` from arbitrary GPU ids, normalizing to
    /// sorted + deduped order.
    pub fn new(mut gpus: Vec<GpuId>) -> Self {
        gpus.sort_unstable();
        gpus.dedup();
        Self(gpus)
    }

    /// Number of distinct GPUs in the island — the axis a
    /// `min_gpus` numeric filter compares against.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Is the island empty (no GPUs)? Such a record is malformed;
    /// the numeric filter rejects it under any positive `min_gpus`.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrow the sorted GPU ids.
    pub fn gpus(&self) -> &[GpuId] {
        &self.0
    }

    /// Do these two islands share any GPU? Both vecs are sorted, so
    /// this is an O(n+m) merge, not a nested scan.
    pub fn intersects(&self, other: &GpuSet) -> bool {
        let (mut i, mut j) = (0, 0);
        while i < self.0.len() && j < other.0.len() {
            match self.0[i].cmp(&other.0[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }
}

/// One island's folded topology record — the host's self-report.
///
/// `load` and `p50_latency_us` are the **live** axes that update on
/// every heartbeat re-announcement; the rest are quasi-static (they
/// change only on a hardware/host event).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IslandRecord {
    /// `hash(host, nvlink_domain)` — also this entry's fold key and
    /// the [`super::ResourceId`] a claim targets.
    pub id: IslandId,
    /// GPUs in the NVLink domain.
    pub gpus: GpuSet,
    /// Host node that owns + announces this island. Must equal the
    /// announcement's publisher (`node_id`); enforced in `merge`.
    pub host: NodeId,
    /// Models currently resident in the island's GPU memory.
    pub warm_models: Vec<ModelId>,
    /// Live utilization in `0.0..=1.0`. Fold updates on heartbeat.
    pub load: f32,
    /// Live p50 request latency in microseconds.
    pub p50_latency_us: u32,
}

/// Query shapes the [`IslandTopologyFold`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IslandQuery {
    /// One island by id. At most one row.
    Get(IslandId),
    /// Every known island — the input to the scheduler's numeric
    /// filter (plan §2 step 2).
    All,
    /// Every island hosted by a given node. Used to map a
    /// capability match (which is keyed by host node) onto the
    /// islands that node offers.
    HostedBy(NodeId),
    /// Every island hosted by any node in the set — the batched form
    /// of [`HostedBy`](Self::HostedBy) for the scheduler's
    /// candidate-host match. One scan clones only islands on candidate
    /// hosts, instead of cloning the whole topology and discarding the
    /// majority (the `All`-then-filter path it replaces).
    HostedByAny(std::collections::HashSet<NodeId>),
}

/// One row in an [`IslandQuery`] result: the island id plus its
/// folded record.
pub type IslandRow = (IslandId, IslandRecord);

/// Marker type for the [`FoldKind`] impl. Carries no state — that
/// lives in the [`super::Fold`] instance parameterized by this type.
#[derive(Debug)]
pub struct IslandTopologyFold;

impl FoldKind for IslandTopologyFold {
    /// Built-in fold id `4`, the next free slot after capability
    /// (1), routing (2), reservation (3) in the reserved
    /// `0x0000..=0x00FF` built-in range.
    const KIND_ID: u16 = 4;
    const CHANNEL_PREFIX: &'static str = "fold:island:";
    /// 30-second runtime TTL — the live axes are only useful fresh,
    /// and a host that stops heartbeating drops out of the topology
    /// within the window. Matches [`super::ReservationFold`].
    const DEFAULT_TTL: Duration = Duration::from_secs(30);

    type Key = IslandId;
    type Payload = IslandRecord;
    type Query = IslandQuery;
    type Result = Vec<IslandRow>;
    type Index = NoIndex;

    fn key_for(_publisher: NodeId, payload: &Self::Payload) -> IslandId {
        payload.id
    }

    fn build_index() -> NoIndex {
        NoIndex
    }

    /// Ownership-gated last-write-wins.
    ///
    /// 1. A node may only announce an island it hosts — `record.host`
    ///    must equal the publishing `node_id`. This blocks a foreign
    ///    node from publishing a doctored record (e.g. understating
    ///    `load` to attract claims) for an island it does not own.
    /// 2. Once an entry exists, only its original publisher updates
    ///    it, and only at a strictly-higher generation (anti-reorder).
    ///    A different publisher is rejected even with `host` forged to
    ///    itself — first-writer-wins pins the key to the real host.
    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction {
        // Ownership gate: announcer must be the island's host.
        if incoming.payload.host != incoming.node_id {
            return MergeAction::Reject;
        }
        // A non-finite live `load` (NaN/Inf) would make the selection
        // comparator (filter::policy_cmp, partial_cmp→Equal) non-total
        // and corrupt claim ordering. The axes are advisory — the
        // reservation CAS is the real arbiter — but keep them sane so a
        // buggy/hostile host can't silently scramble placement.
        if !incoming.payload.load.is_finite() {
            return MergeAction::Reject;
        }
        match existing {
            None => MergeAction::Insert,
            Some(entry) => {
                // Cross-publisher updates can't take the key over —
                // the host gate above already blocks foreign hosts;
                // this also stops a node that forged `host == self`
                // for a key the true host installed first.
                if entry.node_id != incoming.node_id {
                    return MergeAction::Reject;
                }
                // Same publisher: strictly-newer generation wins, so
                // the freshest live-axis read replaces the older one.
                if incoming.generation > entry.generation {
                    MergeAction::Replace
                } else {
                    MergeAction::Reject
                }
            }
        }
    }

    fn query(state: &FoldState<Self>, _index: &NoIndex, query: IslandQuery) -> Vec<IslandRow> {
        match query {
            IslandQuery::Get(id) => state
                .entries
                .get(&id)
                .map(|e| vec![(id, e.payload.clone())])
                .unwrap_or_default(),
            IslandQuery::All => state
                .entries
                .iter()
                .map(|(k, e)| (*k, e.payload.clone()))
                .collect(),
            IslandQuery::HostedBy(host) => state
                .entries
                .iter()
                .filter(|(_, e)| e.payload.host == host)
                .map(|(k, e)| (*k, e.payload.clone()))
                .collect(),
            IslandQuery::HostedByAny(hosts) => state
                .entries
                .iter()
                .filter(|(_, e)| hosts.contains(&e.payload.host))
                .map(|(k, e)| (*k, e.payload.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        ApplyOutcome, EnvelopeMeta, Fold, FoldRegistry, SignedAnnouncement,
    };
    use crate::adapter::net::identity::EntityKeypair;

    /// Build an island announcement signed by `keypair`, claiming
    /// `node_id` as the publisher. The record's `host` defaults to
    /// `node_id` (the legitimate self-announce shape); pass a
    /// distinct `host` to exercise the ownership gate.
    fn sign_island(
        keypair: &EntityKeypair,
        node_id: NodeId,
        generation: u64,
        record: IslandRecord,
    ) -> SignedAnnouncement<IslandRecord> {
        SignedAnnouncement::sign(
            keypair,
            IslandTopologyFold::KIND_ID,
            0, // class (pool) — reserved
            node_id,
            generation,
            EnvelopeMeta::default(),
            record,
        )
        .expect("sign succeeds")
    }

    fn record(id: IslandId, host: NodeId, load: f32) -> IslandRecord {
        IslandRecord {
            id,
            gpus: GpuSet::new(vec![0, 1, 2, 3]),
            host,
            warm_models: vec![0xA1],
            load,
            p50_latency_us: 1_500,
        }
    }

    fn new_fold() -> Fold<IslandTopologyFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    #[test]
    fn first_announcement_installs_the_island() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let outcome = fold
            .apply(sign_island(&kp, 0xAA, 1, record(0x10, 0xAA, 0.25)))
            .expect("apply");
        assert_eq!(outcome, ApplyOutcome::Inserted);
        let q = fold.query(IslandQuery::Get(0x10));
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].1.host, 0xAA);
        assert_eq!(q[0].1.load, 0.25);
    }

    #[test]
    fn host_re_announce_replaces_live_axes() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_island(&kp, 0xAA, 1, record(0x10, 0xAA, 0.10)))
            .expect("first");
        let outcome = fold
            .apply(sign_island(&kp, 0xAA, 2, record(0x10, 0xAA, 0.90)))
            .expect("heartbeat");
        assert_eq!(outcome, ApplyOutcome::Replaced);
        assert_eq!(fold.query(IslandQuery::Get(0x10))[0].1.load, 0.90);
    }

    #[test]
    fn stale_generation_from_host_is_rejected() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_island(&kp, 0xAA, 5, record(0x10, 0xAA, 0.5)))
            .expect("gen=5");
        // Replayed gen=5 and lower gen=4 both lose to the installed
        // entry — anti-reorder.
        assert_eq!(
            fold.apply(sign_island(&kp, 0xAA, 5, record(0x10, 0xAA, 0.1)))
                .unwrap(),
            ApplyOutcome::Rejected,
        );
        assert_eq!(
            fold.apply(sign_island(&kp, 0xAA, 4, record(0x10, 0xAA, 0.1)))
                .unwrap(),
            ApplyOutcome::Rejected,
        );
        assert_eq!(fold.query(IslandQuery::Get(0x10))[0].1.load, 0.5);
    }

    #[test]
    fn announcement_for_a_non_self_host_is_rejected() {
        // A node announcing an island whose `host` is NOT itself is
        // rejected outright — ownership gate. (Publisher 0xAA claims
        // the record is hosted by 0xBB.)
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let outcome = fold
            .apply(sign_island(&kp, 0xAA, 1, record(0x10, 0xBB, 0.0)))
            .expect("apply");
        assert_eq!(outcome, ApplyOutcome::Rejected);
        assert!(fold.query(IslandQuery::Get(0x10)).is_empty());
    }

    #[test]
    fn foreign_publisher_cannot_take_over_an_island_key() {
        // 0xAA installs island 0x10. 0xBB then tries to claim the
        // same id with host forged to itself — must be rejected so a
        // squatter can't repoint an existing island's topology.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        fold.apply(sign_island(&kp_a, 0xAA, 1, record(0x10, 0xAA, 0.2)))
            .expect("A installs");
        let outcome = fold
            .apply(sign_island(&kp_b, 0xBB, 99, record(0x10, 0xBB, 0.0)))
            .expect("B attempts takeover");
        assert_eq!(outcome, ApplyOutcome::Rejected);
        assert_eq!(fold.query(IslandQuery::Get(0x10))[0].1.host, 0xAA);
    }

    #[test]
    fn query_all_and_hosted_by() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        fold.apply(sign_island(&kp_a, 0xAA, 1, record(0x10, 0xAA, 0.2)))
            .unwrap();
        fold.apply(sign_island(&kp_a, 0xAA, 1, record(0x11, 0xAA, 0.3)))
            .unwrap();
        fold.apply(sign_island(&kp_b, 0xBB, 1, record(0x20, 0xBB, 0.4)))
            .unwrap();

        let mut all: Vec<IslandId> = fold
            .query(IslandQuery::All)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        all.sort();
        assert_eq!(all, vec![0x10, 0x11, 0x20]);

        let mut by_a: Vec<IslandId> = fold
            .query(IslandQuery::HostedBy(0xAA))
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        by_a.sort();
        assert_eq!(by_a, vec![0x10, 0x11]);

        // HostedByAny is the batched form: islands on any host in the
        // set, in one scan (review #12).
        let hosts: std::collections::HashSet<NodeId> = [0xAA, 0xBB].into_iter().collect();
        let mut any: Vec<IslandId> = fold
            .query(IslandQuery::HostedByAny(hosts))
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        any.sort();
        assert_eq!(any, vec![0x10, 0x11, 0x20]);
        // A host not in the set contributes nothing.
        let only_b: std::collections::HashSet<NodeId> = [0xBB].into_iter().collect();
        assert_eq!(
            fold.query(IslandQuery::HostedByAny(only_b))
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec![0x20],
        );
    }

    /// A non-finite live `load` is rejected so it can't make the
    /// selection comparator non-total (review, advisory).
    #[test]
    fn non_finite_load_is_rejected() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let outcome = fold
                .apply(sign_island(&kp, 0xAA, 1, record(0x10, 0xAA, bad)))
                .expect("apply");
            assert_eq!(outcome, ApplyOutcome::Rejected, "load {bad} rejected");
        }
        assert!(fold.query(IslandQuery::Get(0x10)).is_empty());
    }

    #[test]
    fn runtime_ttl_sweeps_stale_islands() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let ann = SignedAnnouncement::sign(
            &kp,
            IslandTopologyFold::KIND_ID,
            0,
            0xAA,
            1,
            EnvelopeMeta {
                ttl_secs: Some(0),
                ..Default::default()
            },
            record(0x10, 0xAA, 0.5),
        )
        .unwrap();
        fold.apply(ann).unwrap();
        assert_eq!(fold.metrics().entries(), 1);

        std::thread::sleep(Duration::from_millis(10));
        let n = fold.sweep_expired_now();
        assert_eq!(n, 1);
        assert!(fold.query(IslandQuery::Get(0x10)).is_empty());
    }

    #[test]
    fn gpu_set_normalizes_and_intersects() {
        let a = GpuSet::new(vec![3, 1, 1, 2]);
        assert_eq!(a.gpus(), &[1, 2, 3]);
        assert_eq!(a.len(), 3);
        assert!(a.intersects(&GpuSet::new(vec![5, 3])));
        assert!(!a.intersects(&GpuSet::new(vec![4, 5, 6])));
        assert!(!a.intersects(&GpuSet::default()));
    }

    #[test]
    fn island_fold_plugs_into_registry_and_dispatches_signed_envelopes() {
        let registry = FoldRegistry::new();
        let fold: Arc<Fold<IslandTopologyFold>> = Arc::new(new_fold());
        registry.register(fold.clone());

        let kp = EntityKeypair::generate();
        // Dispatch verifies publisher-binding, and merge requires
        // host == publisher, so the honest envelope names the
        // signer's own node_id as both.
        let nid = kp.entity_id().node_id();
        let ann = sign_island(&kp, nid, 1, record(0x10, nid, 0.5));
        let bytes = ann.encode().expect("encode");
        let outcome = registry.dispatch(&bytes, kp.entity_id()).expect("dispatch");
        assert_eq!(outcome, ApplyOutcome::Inserted);
    }
}
