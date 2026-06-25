//! Shards — fan-out / fan-in (plan piece 4 / Phase C).
//!
//! A task fans out into K **independent** shard tasks (the *map*):
//! each shard is an ordinary task with its own [`TaskState`](super::TaskState) cursor and
//! its own [`TaskLease`](super::TaskLease) — leases are keyed by
//! task-id, and shard ids are distinct task ids, so shard ownership is
//! independent for free. A *reduce* step is gated on the **join**:
//! `all shards/* == Done`.
//!
//! Like the rest of the layer this is emergence, not machinery — a
//! shard group is just a set of task ids plus the [`ShardGroup::join_ready`]
//! predicate. Fan-out submits the shard tasks; the reduce is submitted
//! once the predicate holds. Per-shard retry is the ordinary
//! [`retry`](super::WorkflowAdapter::retry) on a shard, independent of
//! the others.

use super::super::error::CortexAdapterError;
use super::adapter::WorkflowAdapter;
use super::state::WorkflowState;
use super::types::{TaskId, TaskStatus};

/// Derive `count` deterministic shard ids from a parent id (a
/// splitmix64 mix), so reopening / replaying reconstructs the same
/// group without storing the ids.
pub fn derive_shard_ids(parent: TaskId, count: usize) -> Vec<TaskId> {
    (0..count)
        .map(|k| {
            let mut z = parent.wrapping_add(
                (k as u64)
                    .wrapping_add(1)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15),
            );
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        })
        .collect()
}

/// A fan-out: K independent shard tasks plus a reduce task gated on the
/// **join** of all of them (per the group's [`JoinPolicy`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardGroup {
    shards: Vec<TaskId>,
    reduce: TaskId,
    /// The job task that fanned out, if known — the propagation target
    /// for [`propagate_failure`] / [`block_on_failure`]. `None` for a
    /// group built from explicit ids with no owning job.
    parent: Option<TaskId>,
}

impl ShardGroup {
    /// Build a group from explicit shard ids and a reduce id (the
    /// caller owns the task-id space). No parent — failure propagation
    /// targets only the shards themselves.
    pub fn new(shards: Vec<TaskId>, reduce: TaskId) -> Self {
        Self {
            shards,
            reduce,
            parent: None,
        }
    }

    /// Build a group whose shard ids are [`derive_shard_ids`] of
    /// `parent`, with an explicit reduce id. `parent` is retained as
    /// the failure-propagation target.
    pub fn derived(parent: TaskId, shard_count: usize, reduce: TaskId) -> Self {
        Self {
            shards: derive_shard_ids(parent, shard_count),
            reduce,
            parent: Some(parent),
        }
    }

    /// The shard task ids (the map fan-out).
    pub fn shards(&self) -> &[TaskId] {
        &self.shards
    }

    /// The reduce task id (the fan-in).
    pub fn reduce(&self) -> TaskId {
        self.reduce
    }

    /// The owning job task (the propagation target), if this group was
    /// [`derived`](Self::derived) from one.
    pub fn parent(&self) -> Option<TaskId> {
        self.parent
    }

    /// Number of shards.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// The **join** predicate: is every shard `Done`? An empty group is
    /// trivially ready. Pure + deterministic.
    pub fn join_ready(&self, state: &WorkflowState) -> bool {
        self.shards.iter().all(|s| {
            state
                .get(*s)
                .map(|t| t.status == TaskStatus::Done)
                .unwrap_or(false)
        })
    }

    /// Shards not yet `Done` (progress / which to retry). Includes
    /// `Failed` shards — they are "not done" until retried.
    pub fn pending(&self, state: &WorkflowState) -> Vec<TaskId> {
        self.shards
            .iter()
            .copied()
            .filter(|s| {
                state
                    .get(*s)
                    .map(|t| t.status != TaskStatus::Done)
                    .unwrap_or(true)
            })
            .collect()
    }

    /// Shards currently in `Failed` — symmetric with [`pending`](Self::pending).
    /// A non-empty result is what `AllOrNothing` propagates on.
    pub fn failed(&self, state: &WorkflowState) -> Vec<TaskId> {
        self.shards
            .iter()
            .copied()
            .filter(|s| {
                state
                    .get(*s)
                    .map(|t| t.status == TaskStatus::Failed)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Count of shards currently `Done`.
    pub fn done_count(&self, state: &WorkflowState) -> usize {
        self.shards
            .iter()
            .filter(|s| {
                state
                    .get(**s)
                    .map(|t| t.status == TaskStatus::Done)
                    .unwrap_or(false)
            })
            .count()
    }

    /// Evaluate the join under `policy` — the failure-aware fan-in
    /// status. Pure + deterministic. A failed shard never silently
    /// hangs the reduce (the corrections #2 fix): it surfaces as
    /// [`JoinStatus::Failed`] under `AllOrNothing`, or is tolerated
    /// under `BestEffort` / `Threshold`.
    pub fn join_status(&self, state: &WorkflowState, policy: JoinPolicy) -> JoinStatus {
        let failed = self.failed(state);
        let done = self.done_count(state);
        let total = self.shards.len();
        match policy {
            JoinPolicy::AllOrNothing => {
                if !failed.is_empty() {
                    JoinStatus::Failed(failed)
                } else if done == total {
                    JoinStatus::Ready
                } else {
                    JoinStatus::Pending
                }
            }
            JoinPolicy::BestEffort => {
                // Ready once every shard is terminal (Done or Failed);
                // the reducer decides what to do with partial results.
                let terminal = self
                    .shards
                    .iter()
                    .filter(|s| {
                        state
                            .get(**s)
                            .map(|t| t.status.is_terminal())
                            .unwrap_or(false)
                    })
                    .count();
                if terminal == total {
                    JoinStatus::Ready
                } else {
                    JoinStatus::Pending
                }
            }
            JoinPolicy::Threshold(n) => {
                // `n > total` is unsatisfiable by construction — a
                // misconfigured join, not a shard failure. It is the
                // only way the `Failed` branch below can fire with an
                // empty `failed` set (failed.is_empty() ⟹ total < n),
                // which would otherwise fail the parent citing no shard.
                // Catch it in debug as the config bug it is.
                debug_assert!(
                    n <= total,
                    "Threshold({n}) exceeds shard count {total}: unsatisfiable by construction"
                );
                if done >= n {
                    JoinStatus::Ready
                } else if total.saturating_sub(failed.len()) < n {
                    // Even if every still-running shard finishes, `n`
                    // is unreachable — the join can never satisfy.
                    JoinStatus::Failed(failed)
                } else {
                    JoinStatus::Pending
                }
            }
        }
    }
}

/// How a fan-in treats shards reaching terminal states. The default
/// (`AllOrNothing`) is strict map-reduce; the others are escape hatches
/// the structure supports without API breakage (corrections #2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JoinPolicy {
    /// Reduce fires only when **all** shards are `Done`; any `Failed`
    /// shard fails the join. The default.
    #[default]
    AllOrNothing,
    /// Reduce fires once **every** shard is terminal (`Done` or
    /// `Failed`); the reducer inspects which succeeded. For
    /// embarrassingly-parallel work where partial results are usable.
    BestEffort,
    /// Reduce fires once at least `n` shards are `Done` (quorum
    /// map-reduce); becomes unsatisfiable — and so `Failed` — once too
    /// many shards have failed for `n` to be reachable.
    Threshold(usize),
}

/// The join's verdict under a [`JoinPolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinStatus {
    /// The join is satisfied — submit the reduce.
    Ready,
    /// The join can never be satisfied: these shards `Failed`. The
    /// caller propagates ([`propagate_failure`] / [`block_on_failure`]).
    Failed(Vec<TaskId>),
    /// Not yet — shards still running, none disqualifying.
    Pending,
}

/// Fan out: submit every shard task. When the group was
/// [`derived`](ShardGroup::derived) from a parent, also record the
/// parent→shard lineage so a later [`delete`](super::WorkflowAdapter::delete)
/// of the parent cascades to the shards (corrections #4). Returns the
/// last append seq (0 if the group has no shards).
pub fn fan_out(wf: &WorkflowAdapter, group: &ShardGroup) -> Result<u64, CortexAdapterError> {
    let mut last = 0;
    for &shard in group.shards() {
        last = wf.submit(shard)?;
        if let Some(parent) = group.parent() {
            last = wf.link(parent, shard)?;
        }
    }
    Ok(last)
}

/// Result of a [`try_join`] attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Join {
    /// The join was satisfied and the reduce was just submitted at this
    /// seq — `wait_for_seq(seq)` to observe it fold in.
    Submitted(u64),
    /// The reduce was already submitted by a prior `try_join`
    /// (idempotent — not re-submitted).
    AlreadySubmitted,
    /// The join isn't satisfied yet; the reduce stays gated.
    Pending,
    /// The join can **never** be satisfied under the policy: these
    /// shards `Failed`. The reduce is not submitted — the caller
    /// propagates the failure ([`propagate_failure`] /
    /// [`block_on_failure`]) instead of waiting forever (corrections #2).
    Failed(Vec<TaskId>),
}

/// Join under the default [`JoinPolicy::AllOrNothing`]: submit the
/// reduce once every shard is `Done`; surface [`Join::Failed`] if any
/// shard failed; otherwise [`Join::Pending`]. Idempotent.
pub fn try_join(wf: &WorkflowAdapter, group: &ShardGroup) -> Result<Join, CortexAdapterError> {
    try_join_with(wf, group, JoinPolicy::AllOrNothing)
}

/// Like [`try_join`] but with an explicit [`JoinPolicy`]. Idempotent:
/// once the reduce is present, returns [`Join::AlreadySubmitted`]
/// regardless of policy.
pub fn try_join_with(
    wf: &WorkflowAdapter,
    group: &ShardGroup,
    policy: JoinPolicy,
) -> Result<Join, CortexAdapterError> {
    let (already, status) = {
        let state = wf.state();
        let guard = state.read();
        (
            guard.contains(group.reduce()),
            group.join_status(&guard, policy),
        )
    };
    if already {
        return Ok(Join::AlreadySubmitted);
    }
    match status {
        JoinStatus::Ready => {
            let mut seq = wf.submit(group.reduce())?;
            // Link the reduce under the parent too, so deleting the job
            // reclaims its fan-in along with the shards.
            if let Some(parent) = group.parent() {
                seq = wf.link(parent, group.reduce())?;
            }
            Ok(Join::Submitted(seq))
        }
        JoinStatus::Failed(f) => Ok(Join::Failed(f)),
        JoinStatus::Pending => Ok(Join::Pending),
    }
}

/// Propagate a shard failure as **terminal** (the `AllOrNothing`
/// default disposition): request-cancel every still-pending shard so
/// they stop holding work / claims, then mark the group's `parent`
/// `Failed`. Returns the last append seq, or `Ok(0)` if there's nothing
/// to do (no parent, no pending shards). Idempotent-ish: re-running
/// re-issues cancels, which are themselves idempotent signals.
pub fn propagate_failure(
    wf: &WorkflowAdapter,
    group: &ShardGroup,
) -> Result<u64, CortexAdapterError> {
    let mut last = 0;
    // Stop the siblings — an orphaned shard that keeps running keeps
    // holding whatever resource/claim it acquired (the stranded-GPU
    // failure mode of the audit's cross-cutting rule).
    let pending = group.pending(&wf.state().read());
    for shard in pending {
        last = wf.request_cancel(shard)?;
    }
    if let Some(parent) = group.parent() {
        last = wf.fail(parent)?;
    }
    Ok(last)
}

/// Propagate a shard failure as **recoverable**: mark the group's
/// `parent` `Blocked` — it cannot proceed on its own, but an operator
/// (or a retry policy) can re-run the failed shards, and when they
/// reach `Done` the join is re-evaluated. Unlike [`propagate_failure`]
/// this does **not** cancel the other shards. This is the real call
/// site that gives `Blocked` distinct semantics from `Waiting`
/// (corrections #1: `Blocked` = parked on external state, not a
/// self-retrying claim reject). Returns the append seq, or `Ok(0)` if
/// the group has no parent.
pub fn block_on_failure(
    wf: &WorkflowAdapter,
    group: &ShardGroup,
) -> Result<u64, CortexAdapterError> {
    match group.parent() {
        Some(parent) => wf.block(parent),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::types::TaskState;
    use super::*;
    use crate::adapter::net::redex::Redex;

    fn done_state(ids: &[TaskId]) -> WorkflowState {
        let mut s = WorkflowState::new();
        for id in ids {
            s.tasks.insert(
                *id,
                TaskState {
                    step: 0,
                    status: TaskStatus::Done,
                    attempts: 0,
                },
            );
        }
        s
    }

    fn state_with(pairs: &[(TaskId, TaskStatus)]) -> WorkflowState {
        let mut s = WorkflowState::new();
        for (id, status) in pairs {
            s.tasks.insert(
                *id,
                TaskState {
                    step: 0,
                    status: *status,
                    attempts: 0,
                },
            );
        }
        s
    }

    #[test]
    fn derived_shard_ids_are_deterministic_and_distinct() {
        let a = derive_shard_ids(0xABCD, 4);
        let b = derive_shard_ids(0xABCD, 4);
        assert_eq!(a, b, "same parent → same shard ids (replay-stable)");
        assert_eq!(a.len(), 4);
        // Distinct from each other + the parent.
        let mut sorted = a.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "shard ids are distinct");
        assert!(!a.contains(&0xABCD));
    }

    #[test]
    fn join_ready_only_when_all_shards_done() {
        let group = ShardGroup::new(vec![1, 2, 3], 9);
        // None done.
        assert!(!group.join_ready(&WorkflowState::new()));
        assert_eq!(group.pending(&WorkflowState::new()), vec![1, 2, 3]);
        // Two of three done.
        let partial = done_state(&[1, 2]);
        assert!(!group.join_ready(&partial));
        assert_eq!(group.pending(&partial), vec![3]);
        // All done.
        let all = done_state(&[1, 2, 3]);
        assert!(group.join_ready(&all));
        assert!(group.pending(&all).is_empty());
    }

    #[test]
    fn empty_group_joins_immediately() {
        let group = ShardGroup::new(vec![], 9);
        assert!(group.join_ready(&WorkflowState::new()));
    }

    /// A `Threshold(n)` with `n` above the shard count can never satisfy
    /// — it is a misconfiguration, not a shard failure. Without the
    /// guard it returned `Failed(vec![])` (an empty failed-set that
    /// would fail the parent citing no shard); the debug_assert catches
    /// it as the config bug it is (review #6).
    #[test]
    #[should_panic(expected = "exceeds shard count")]
    fn threshold_above_shard_count_is_caught() {
        let group = ShardGroup::new(vec![1, 2, 3], 9);
        let running = state_with(&[
            (1, TaskStatus::Running),
            (2, TaskStatus::Running),
            (3, TaskStatus::Running),
        ]);
        let _ = group.join_status(&running, JoinPolicy::Threshold(5));
    }

    /// Phase C "Done when": a map-reduce runs with per-shard retry and a
    /// correct join — the reduce is submitted only once every shard is
    /// Done.
    #[tokio::test]
    async fn map_reduce_join_fires_reduce_only_after_all_shards_done() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00C1).await.unwrap();
        let group = ShardGroup::new(vec![10, 11, 12], 99);

        let seq = fan_out(&wf, &group).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert_eq!(group.pending(&wf.state().read()).len(), 3);

        // Shard 10: clean run. Shard 11: a per-shard retry then Done.
        wf.start(10).unwrap();
        wf.complete(10).unwrap();
        wf.start(11).unwrap();
        wf.retry(11).unwrap(); // independent per-shard retry
        let seq = wf.complete(11).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // Two of three done → join NOT ready, reduce not submitted.
        assert_eq!(try_join(&wf, &group).unwrap(), Join::Pending);
        assert!(
            !wf.state().read().contains(99),
            "reduce not submitted until every shard is Done",
        );

        // Finish the third shard.
        wf.start(12).unwrap();
        let seq = wf.complete(12).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // Now the join fires: reduce submitted (await it to observe).
        let reduce_seq = match try_join(&wf, &group).unwrap() {
            Join::Submitted(seq) => seq,
            other => panic!("expected Submitted, got {other:?}"),
        };
        wf.wait_for_seq(reduce_seq).await.unwrap();
        assert!(
            wf.state().read().contains(99),
            "reduce submitted on all-done"
        );
        // Idempotent: a second try_join doesn't re-submit / reset it.
        assert_eq!(try_join(&wf, &group).unwrap(), Join::AlreadySubmitted);
        // The retried shard recorded its attempt.
        assert_eq!(wf.get(11).unwrap().attempts, 1);
    }

    // --- corrections #2 / #1: failure propagation + Blocked ---

    #[test]
    fn failed_shard_surfaces_as_join_failed_not_pending() {
        let group = ShardGroup::new(vec![1, 2, 3], 9);
        // Shard 2 failed, 1 done, 3 still running.
        let st = state_with(&[
            (1, TaskStatus::Done),
            (2, TaskStatus::Failed),
            (3, TaskStatus::Running),
        ]);
        // The old join_ready hangs (never all-Done); join_status names
        // the failure instead — the hang fix.
        assert!(!group.join_ready(&st));
        assert_eq!(group.failed(&st), vec![2]);
        assert_eq!(
            group.join_status(&st, JoinPolicy::AllOrNothing),
            JoinStatus::Failed(vec![2]),
        );
    }

    #[test]
    fn best_effort_joins_when_every_shard_is_terminal() {
        let group = ShardGroup::new(vec![1, 2, 3], 9);
        let mixed = state_with(&[
            (1, TaskStatus::Done),
            (2, TaskStatus::Failed),
            (3, TaskStatus::Running),
        ]);
        // One still running → not ready.
        assert_eq!(
            group.join_status(&mixed, JoinPolicy::BestEffort),
            JoinStatus::Pending
        );
        // All terminal (Done + Failed) → ready; reducer sees partials.
        let terminal = state_with(&[
            (1, TaskStatus::Done),
            (2, TaskStatus::Failed),
            (3, TaskStatus::Done),
        ]);
        assert_eq!(
            group.join_status(&terminal, JoinPolicy::BestEffort),
            JoinStatus::Ready
        );
    }

    #[test]
    fn threshold_joins_at_n_done_and_fails_once_unreachable() {
        let group = ShardGroup::new(vec![1, 2, 3], 9);
        // Need 2 of 3 Done. One Done, two running → pending.
        let one = state_with(&[
            (1, TaskStatus::Done),
            (2, TaskStatus::Running),
            (3, TaskStatus::Running),
        ]);
        assert_eq!(
            group.join_status(&one, JoinPolicy::Threshold(2)),
            JoinStatus::Pending
        );
        // Two Done → ready.
        let two = state_with(&[
            (1, TaskStatus::Done),
            (2, TaskStatus::Done),
            (3, TaskStatus::Running),
        ]);
        assert_eq!(
            group.join_status(&two, JoinPolicy::Threshold(2)),
            JoinStatus::Ready
        );
        // One Done, two Failed → only 1 can ever be Done < 2 → Failed.
        let lost = state_with(&[
            (1, TaskStatus::Done),
            (2, TaskStatus::Failed),
            (3, TaskStatus::Failed),
        ]);
        assert_eq!(
            group.join_status(&lost, JoinPolicy::Threshold(2)),
            JoinStatus::Failed(vec![2, 3]),
        );
    }

    /// A failed shard makes `try_join` return `Failed` (not hang on
    /// `Pending`), and `propagate_failure` cancels the still-pending
    /// siblings and fails the parent job.
    #[tokio::test]
    async fn propagate_failure_cancels_pending_and_fails_parent() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00C2).await.unwrap();
        // Group derived from parent 7 (so it has a propagation target).
        let group = ShardGroup::derived(7, 3, 99);
        let shards = group.shards().to_vec();
        let seq = wf.submit(7).unwrap(); // the job task
        wf.wait_for_seq(seq).await.unwrap();
        let seq = fan_out(&wf, &group).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // Shard 0 done, shard 1 FAILED, shard 2 still running.
        wf.complete(shards[0]).unwrap();
        let seq = wf.fail(shards[1]).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        // try_join surfaces the failure rather than hanging.
        assert_eq!(
            try_join(&wf, &group).unwrap(),
            Join::Failed(vec![shards[1]]),
        );
        assert!(
            !wf.state().read().contains(99),
            "reduce never submitted on failure"
        );

        // Propagate: pending sibling (shard 2) cancelled, parent Failed.
        let seq = propagate_failure(&wf, &group).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(
            wf.is_cancel_requested(shards[2]),
            "running sibling cancelled"
        );
        assert!(
            !wf.is_cancel_requested(shards[0]),
            "the Done shard isn't cancelled"
        );
        assert_eq!(
            wf.get(7).unwrap().status,
            TaskStatus::Failed,
            "parent failed"
        );
    }

    /// Recoverable disposition: `block_on_failure` parks the parent
    /// `Blocked` (corrections #1's real call site for `block()`) and
    /// leaves the siblings alone, so a retry of the failed shard can
    /// later clear the join.
    #[tokio::test]
    async fn block_on_failure_marks_parent_blocked_and_spares_siblings() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00C3).await.unwrap();
        let group = ShardGroup::derived(7, 3, 99);
        let shards = group.shards().to_vec();
        let seq = wf.submit(7).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        fan_out(&wf, &group).unwrap();
        wf.start(shards[2]).unwrap();
        let seq = wf.fail(shards[1]).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let seq = block_on_failure(&wf, &group).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert_eq!(
            wf.get(7).unwrap().status,
            TaskStatus::Blocked,
            "parent parked Blocked (external state), distinct from Waiting",
        );
        // Siblings untouched — the failed shard can be retried to clear.
        assert!(!wf.is_cancel_requested(shards[2]));
    }

    /// `fan_out` of a derived group records lineage, so deleting the
    /// parent job reclaims all its shards (corrections #4 — no orphaned
    /// shards left running).
    #[tokio::test]
    async fn delete_parent_reclaims_all_shards() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00C4).await.unwrap();
        let group = ShardGroup::derived(7, 3, 99);
        let shards = group.shards().to_vec();
        let seq = wf.submit(7).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        let seq = fan_out(&wf, &group).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        for s in &shards {
            assert!(wf.get(*s).is_some());
        }

        // Delete the job → its shards cascade away.
        let seq = wf.delete(7).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(7).is_none());
        for s in &shards {
            assert!(wf.get(*s).is_none(), "shard {s} reclaimed with the parent");
        }
    }

    /// Shards have **independent** leases: distinct shard ids are
    /// distinct reservation resources, so two owners hold two shards at
    /// once, and neither can take the other's.
    #[test]
    fn shards_have_independent_leases() {
        use super::super::TaskLease;
        use super::super::TaskLeaseOutcome;
        use crate::adapter::net::behavior::fold::{Fold, ReservationFold};
        use crate::adapter::net::current_timestamp_micros;
        use crate::adapter::net::identity::EntityKeypair;

        let fold = Fold::<ReservationFold>::with_sweep_interval(Duration::ZERO);
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());
        let mut la = TaskLease::new(&fold, &a, na);
        let mut lb = TaskLease::new(&fold, &b, nb);
        let until = current_timestamp_micros() + 60_000_000;

        // A owns shard 10, B owns shard 11 — independent, both Acquired.
        assert_eq!(la.acquire(10, until).unwrap(), TaskLeaseOutcome::Acquired);
        assert_eq!(lb.acquire(11, until).unwrap(), TaskLeaseOutcome::Acquired);
        assert_eq!(la.current_holder(10), Some(na));
        assert_eq!(lb.current_holder(11), Some(nb));
        // A can't take B's shard.
        assert_eq!(la.acquire(11, until).unwrap(), TaskLeaseOutcome::Contended);
    }
}
