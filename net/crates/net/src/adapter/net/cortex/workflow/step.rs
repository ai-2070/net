//! Capability-bearing steps (plan piece 5 / Phase D) — the one
//! cross-plan seam to Thunderdome.
//!
//! A step that requires an *exclusive* capability must obtain it
//! through the Thunderdome match→claim pipeline and **must not run**
//! until an `Active` claim handle is held. The lifecycle layer states
//! the requirement and reacts to the claim result; it never appends to
//! a `ReservationFold` and never reads the capability/topology folds
//! for placement (locked decision 4: `requires_capability` is a
//! *filter*, not a claim — a hint is never a hold).
//!
//! That contract is made **structural** here, not conventional:
//! [`drive_capability_step`] takes only a [`WorkflowAdapter`] and a
//! [`ClaimPipeline`] — it has no fold to touch, so a step *cannot*
//! bypass Thunderdome by construction. The production pipeline
//! ([`GangClaimPipeline`]) is the only thing wired to the reservation
//! fold, and it is the Thunderdome flow itself (match → reserve →
//! quorum-`Active`).

use crate::adapter::net::behavior::fold::{
    CapabilityFold, Fold, IslandId, IslandTopologyFold, JobId, NodeId, ReservationFold,
};
use crate::adapter::net::behavior::gang::{
    commit_active, match_islands, single_island_claim, ActiveCommitOutcome, ClaimError,
    ClaimOutcome, Claimant, Epoch, MatchCriteria, ReplicaCohort, ReplicaSet,
};
use crate::adapter::net::current_timestamp_micros;
use crate::adapter::net::identity::EntityKeypair;

use super::adapter::WorkflowAdapter;
use super::types::TaskId;

/// A held `Active` claim handle — proof a step may start its
/// irreversible work on an exclusively-held capability (one island).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveClaim {
    /// The island (exclusive resource) held in `Active`.
    pub island: IslandId,
}

/// The exclusive-capability requirement of a step. Per locked decision
/// 4 this is a *match* the pipeline consumes — never a hold. The
/// lifecycle states it; it never evaluates placement itself.
pub struct CapabilityRequirement {
    /// The Thunderdome match (capability query + numeric filter +
    /// selection policy).
    pub criteria: MatchCriteria,
    /// How long the resulting `Reserved` lasts before foreign takeover.
    pub reserve_ttl_us: u64,
}

/// Outcome of handing a [`CapabilityRequirement`] to the claim
/// pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimResult {
    /// An exclusive capability is held — the `Active` claim handle. The
    /// step may run.
    Active(ActiveClaim),
    /// No capacity / contention / lost reservation. The step stays
    /// `Waiting` and re-requests later.
    Rejected,
}

/// The one cross-plan seam. The lifecycle hands a
/// [`CapabilityRequirement`] to this and reacts to the
/// [`ClaimResult`]. Implementors encapsulate the *entire* contact with
/// resource arbitration; the lifecycle depends only on this trait.
pub trait ClaimPipeline {
    /// Error type for a claim attempt (sign/apply-level failures,
    /// distinct from a clean [`ClaimResult::Rejected`]).
    type Error;

    /// Hand `req` to Thunderdome's match→claim pipeline and report
    /// whether an `Active` handle is now held.
    fn claim(&mut self, req: &CapabilityRequirement) -> Result<ClaimResult, Self::Error>;
}

/// What [`drive_capability_step`] did with the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepGate {
    /// The capability is held; the task is now `Running` and may
    /// execute its step. Carries the `Active` handle.
    Running(ActiveClaim),
    /// The claim was rejected; the task is parked `Waiting` and will
    /// re-request on a later drive.
    Waiting,
}

/// Error from driving a capability-bearing step.
#[derive(Debug)]
pub enum StepError<E> {
    /// The claim pipeline errored.
    Pipeline(E),
    /// Writing the resulting task transition to the workflow chain
    /// failed.
    Workflow(super::super::error::CortexAdapterError),
}

/// Drive a capability-bearing step: hand its requirement to `pipeline`
/// and transition `task` accordingly — `Active` → `Running` (the step
/// may execute), `Rejected` → `Waiting` (re-request later).
///
/// The task **never** touches a reservation fold here: this function
/// has none to touch. The only path to an exclusive resource is
/// through `pipeline`, so "a step can't bypass Thunderdome" is a
/// property of the signature, not a discipline.
pub fn drive_capability_step<P: ClaimPipeline>(
    wf: &WorkflowAdapter,
    pipeline: &mut P,
    task: TaskId,
    req: &CapabilityRequirement,
) -> Result<StepGate, StepError<P::Error>> {
    match pipeline.claim(req).map_err(StepError::Pipeline)? {
        ClaimResult::Active(claim) => {
            wf.start(task).map_err(StepError::Workflow)?;
            Ok(StepGate::Running(claim))
        }
        ClaimResult::Rejected => {
            wf.wait(task).map_err(StepError::Workflow)?;
            Ok(StepGate::Waiting)
        }
    }
}

/// The folds + identity a [`GangClaimPipeline`] reads/writes — the
/// "where I match and reserve, and who I am" context. Bundled so the
/// pipeline constructor doesn't thread five `&_`/`u64` args (two of
/// which, `node_id` and `job`, are bare `u64`s).
pub struct GangClaimContext<'a> {
    /// Capability fold (step 1 of the match).
    pub capability: &'a Fold<CapabilityFold>,
    /// Island-topology fold (step 2 numeric filter).
    pub topology: &'a Fold<IslandTopologyFold>,
    /// Reservation fold the reserve + Active commit land on.
    pub reservations: &'a Fold<ReservationFold>,
    /// Identity signing the reservation announcements.
    pub keypair: &'a EntityKeypair,
    /// This node's id (the claim holder).
    pub node_id: NodeId,
}

/// Production [`ClaimPipeline`] backed by the Thunderdome gang
/// scheduler — the only component here wired to the reservation fold.
///
/// `claim` is the Thunderdome flow itself: match (read capability +
/// topology) → reserve the first available island (AP) → quorum-commit
/// `Active` (the one CP edge). It uses only the public gang surface
/// (`match_islands` / `single_island_claim` / `commit_active`), so it
/// can't reach into the scheduler's internals.
pub struct GangClaimPipeline<'a> {
    ctx: GangClaimContext<'a>,
    generation: u64,
    cohort: ReplicaCohort,
    replica_set: ReplicaSet,
    reachable: Vec<NodeId>,
    job: JobId,
}

impl<'a> GangClaimPipeline<'a> {
    /// Build a pipeline for `job`, claiming over `ctx`, committing
    /// `Active` against the island's `replica_set` (with `reachable`
    /// the subset currently reachable — all of `set` when healthy; a
    /// strict subset models a partition).
    pub fn new(
        ctx: GangClaimContext<'a>,
        replica_set: ReplicaSet,
        reachable: Vec<NodeId>,
        job: JobId,
    ) -> Self {
        let cohort = ReplicaCohort::new(replica_set.members());
        Self {
            ctx,
            generation: 1,
            cohort,
            replica_set,
            reachable,
            job,
        }
    }

    fn next_gen(&mut self) -> u64 {
        let g = self.generation;
        self.generation += 1;
        g
    }
}

impl ClaimPipeline for GangClaimPipeline<'_> {
    type Error = ClaimError;

    fn claim(&mut self, req: &CapabilityRequirement) -> Result<ClaimResult, ClaimError> {
        // [1] Match — read-only over capability + topology. The
        //     lifecycle stated the requirement; Thunderdome evaluates
        //     placement.
        let islands = match_islands(self.ctx.capability, self.ctx.topology, &req.criteria);
        if islands.is_empty() {
            return Ok(ClaimResult::Rejected);
        }

        // [2] Reserve the first available island (AP, optimistic).
        let until = current_timestamp_micros().saturating_add(req.reserve_ttl_us);
        let mut reserved = None;
        for island in islands {
            let gen = self.next_gen();
            if single_island_claim(
                self.ctx.reservations,
                self.ctx.keypair,
                self.ctx.node_id,
                gen,
                island,
                until,
            )? == ClaimOutcome::Won
            {
                reserved = Some(island);
                break;
            }
        }
        let Some(island) = reserved else {
            return Ok(ClaimResult::Rejected);
        };

        // [3] Quorum-commit Active (the one CP edge). The epoch rides
        //     the generation: take the next counter value, which is
        //     strictly above the reserve's generation.
        let epoch: Epoch = self.next_gen();
        let claimant = Claimant::new(self.ctx.reservations, self.ctx.keypair, self.ctx.node_id);
        match commit_active(
            &claimant,
            &mut self.cohort,
            &self.replica_set,
            &self.reachable,
            island,
            self.job,
            epoch,
        )? {
            ActiveCommitOutcome::Committed => Ok(ClaimResult::Active(ActiveClaim { island })),
            // No quorum (minority partition) or a takeover stole the
            // reserve: no Active, so the step is rejected and re-
            // requests. The orphaned reserve TTL-expires.
            ActiveCommitOutcome::NoQuorum { .. } | ActiveCommitOutcome::LostReservation => {
                Ok(ClaimResult::Rejected)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        CapabilityFilter, CapabilityMembership, CapabilityQuery, EnvelopeMeta, FoldKind, GpuSet,
        IslandRecord, NodeState, ReservationQuery, ReservationState, SignedAnnouncement,
    };
    use crate::adapter::net::behavior::gang::{NumericFilter, SelectionPolicy};
    use crate::adapter::net::redex::Redex;
    use crate::adapter::net::cortex::workflow::TaskStatus;

    /// A test double: returns a forced result and records that it was
    /// consulted. Lets the seam tests prove the driver routes purely
    /// through the pipeline.
    struct ForcedPipeline {
        result: ClaimResult,
        calls: u32,
    }
    impl ClaimPipeline for ForcedPipeline {
        type Error = std::convert::Infallible;
        fn claim(&mut self, _req: &CapabilityRequirement) -> Result<ClaimResult, Self::Error> {
            self.calls += 1;
            Ok(self.result)
        }
    }

    fn requirement() -> CapabilityRequirement {
        CapabilityRequirement {
            criteria: MatchCriteria {
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
            },
            reserve_ttl_us: 60_000_000,
        }
    }

    async fn submitted_task(wf: &WorkflowAdapter, id: TaskId) {
        let seq = wf.submit(id).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
    }

    #[tokio::test]
    async fn forced_reject_leaves_the_step_waiting_never_running() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00D1).await.unwrap();
        submitted_task(&wf, 1).await;

        let mut pipeline = ForcedPipeline {
            result: ClaimResult::Rejected,
            calls: 0,
        };
        let gate = drive_capability_step(&wf, &mut pipeline, 1, &requirement()).unwrap();
        let seq = wf.wait(1).unwrap(); // flush + read
        wf.wait_for_seq(seq).await.unwrap();

        assert_eq!(gate, StepGate::Waiting);
        assert_eq!(pipeline.calls, 1, "the requirement is handed to the pipeline");
        // The step is parked Waiting and never reached Running.
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Waiting);
    }

    #[tokio::test]
    async fn forced_active_runs_the_step() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00D2).await.unwrap();
        submitted_task(&wf, 1).await;

        let mut pipeline = ForcedPipeline {
            result: ClaimResult::Active(ActiveClaim { island: 0xA0 }),
            calls: 0,
        };
        let gate = drive_capability_step(&wf, &mut pipeline, 1, &requirement()).unwrap();
        let seq = wf.start(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        assert_eq!(gate, StepGate::Running(ActiveClaim { island: 0xA0 }));
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Running);
    }

    // --- production pipeline over real Thunderdome folds ---

    fn new_fold<K: FoldKind>() -> Fold<K> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn announce_capability(fold: &Fold<CapabilityFold>, kp: &EntityKeypair, node: u64) {
        let m = CapabilityMembership {
            class_hash: 0x67_70_75,
            tags: vec!["gpu:h100".into()],
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
        fold.apply(
            SignedAnnouncement::sign(
                kp,
                CapabilityFold::KIND_ID,
                m.class_hash,
                node,
                1,
                EnvelopeMeta::default(),
                m,
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn announce_island(fold: &Fold<IslandTopologyFold>, kp: &EntityKeypair, node: u64, id: u64) {
        let record = IslandRecord {
            id,
            gpus: GpuSet::new((0..8).collect()),
            host: node,
            warm_models: vec![0xA1],
            load: 0.2,
            p50_latency_us: 1_000,
        };
        fold.apply(
            SignedAnnouncement::sign(
                kp,
                IslandTopologyFold::KIND_ID,
                0,
                node,
                1,
                EnvelopeMeta::default(),
                record,
            )
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn gang_pipeline_claims_active_and_runs_when_capacity_exists() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let gpu = EntityKeypair::generate();
        let gn = gpu.entity_id().node_id();
        announce_capability(&caps, &gpu, gn);
        announce_island(&topo, &gpu, gn, 0xA0);

        let leader = EntityKeypair::generate();
        let ln = leader.entity_id().node_id();
        let mut pipeline = GangClaimPipeline::new(
            GangClaimContext {
                capability: &caps,
                topology: &topo,
                reservations: &res,
                keypair: &leader,
                node_id: ln,
            },
            ReplicaSet::new([1, 2, 3]),
            vec![1, 2, 3], // healthy: full majority reachable
            42,
        );

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00D3).await.unwrap();
        submitted_task(&wf, 1).await;

        let gate = drive_capability_step(&wf, &mut pipeline, 1, &requirement()).unwrap();
        let seq = wf.start(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        assert_eq!(gate, StepGate::Running(ActiveClaim { island: 0xA0 }));
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Running);
        // The island is held in Active by the leader — through
        // Thunderdome, the only path that touched the reservation fold.
        assert!(matches!(
            res.query(ReservationQuery::State(0xA0))[0].1,
            ReservationState::Active { holder, .. } if holder == ln
        ));
    }

    #[tokio::test]
    async fn gang_pipeline_rejects_and_waits_with_no_capacity_leaving_nothing_reserved() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        // Capability announced but NO island → match is empty.
        let gpu = EntityKeypair::generate();
        let gn = gpu.entity_id().node_id();
        announce_capability(&caps, &gpu, gn);

        let leader = EntityKeypair::generate();
        let ln = leader.entity_id().node_id();
        let mut pipeline = GangClaimPipeline::new(
            GangClaimContext {
                capability: &caps,
                topology: &topo,
                reservations: &res,
                keypair: &leader,
                node_id: ln,
            },
            ReplicaSet::new([1, 2, 3]),
            vec![1, 2, 3],
            42,
        );

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00D4).await.unwrap();
        submitted_task(&wf, 1).await;

        let gate = drive_capability_step(&wf, &mut pipeline, 1, &requirement()).unwrap();
        let seq = wf.wait(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        assert_eq!(gate, StepGate::Waiting);
        assert_eq!(wf.get(1).unwrap().status, TaskStatus::Waiting);
        // A rejected step leaves NOTHING reserved — no leaked hold.
        assert!(res.query(ReservationQuery::State(0xA0)).is_empty());
    }

    /// Minority partition: the leader reaches only 1 of 3 replicas, so
    /// the `Active` commit is quorum-starved → `Rejected`/`Waiting`,
    /// and the step never starts compute (the Thunderdome guarantee,
    /// surfaced at the lifecycle seam).
    #[tokio::test]
    async fn gang_pipeline_minority_partition_cannot_run_the_step() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let gpu = EntityKeypair::generate();
        let gn = gpu.entity_id().node_id();
        announce_capability(&caps, &gpu, gn);
        announce_island(&topo, &gpu, gn, 0xA0);

        let leader = EntityKeypair::generate();
        let ln = leader.entity_id().node_id();
        let mut pipeline = GangClaimPipeline::new(
            GangClaimContext {
                capability: &caps,
                topology: &topo,
                reservations: &res,
                keypair: &leader,
                node_id: ln,
            },
            ReplicaSet::new([1, 2, 3, 4, 5]),
            vec![1, 2], // minority side of a 3|2 split
            42,
        );

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00D5).await.unwrap();
        submitted_task(&wf, 1).await;

        let gate = drive_capability_step(&wf, &mut pipeline, 1, &requirement()).unwrap();
        assert_eq!(gate, StepGate::Waiting, "minority side can't reach Active → step waits");
        // Reserved (the AP optimistic hold) but never Active — no
        // compute starts.
        assert!(matches!(
            res.query(ReservationQuery::State(0xA0))[0].1,
            ReservationState::Reserved { .. }
        ));
    }
}
