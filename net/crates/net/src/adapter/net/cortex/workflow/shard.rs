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
            let mut z = parent
                .wrapping_add((k as u64).wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        })
        .collect()
}

/// A fan-out: K independent shard tasks plus a reduce task gated on all
/// of them reaching `Done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardGroup {
    shards: Vec<TaskId>,
    reduce: TaskId,
}

impl ShardGroup {
    /// Build a group from explicit shard ids and a reduce id (the
    /// caller owns the task-id space).
    pub fn new(shards: Vec<TaskId>, reduce: TaskId) -> Self {
        Self { shards, reduce }
    }

    /// Build a group whose shard ids are [`derive_shard_ids`] of
    /// `parent`, with an explicit reduce id.
    pub fn derived(parent: TaskId, shard_count: usize, reduce: TaskId) -> Self {
        Self {
            shards: derive_shard_ids(parent, shard_count),
            reduce,
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

    /// Shards not yet `Done` (progress / which to retry).
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
}

/// Fan out: submit every shard task. Returns the last append seq (0 if
/// the group has no shards).
pub fn fan_out(wf: &WorkflowAdapter, group: &ShardGroup) -> Result<u64, CortexAdapterError> {
    let mut last = 0;
    for &shard in group.shards() {
        last = wf.submit(shard)?;
    }
    Ok(last)
}

/// Result of a [`try_join`] attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    /// Every shard is `Done` and the reduce was just submitted at this
    /// seq — `wait_for_seq(seq)` to observe it fold in.
    Submitted(u64),
    /// The reduce was already submitted by a prior `try_join`
    /// (idempotent — not re-submitted).
    AlreadySubmitted,
    /// Shards aren't all `Done` yet; the reduce stays gated.
    Pending,
}

/// Join: if every shard is `Done`, submit the reduce (once) and return
/// [`Join::Submitted`]; if it was already submitted return
/// [`Join::AlreadySubmitted`]; otherwise [`Join::Pending`]. Idempotent.
pub fn try_join(wf: &WorkflowAdapter, group: &ShardGroup) -> Result<Join, CortexAdapterError> {
    let (already, ready) = {
        let state = wf.state();
        let guard = state.read();
        (guard.contains(group.reduce()), group.join_ready(&guard))
    };
    if already {
        Ok(Join::AlreadySubmitted)
    } else if ready {
        Ok(Join::Submitted(wf.submit(group.reduce())?))
    } else {
        Ok(Join::Pending)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use super::super::types::TaskState;
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
        assert!(wf.state().read().contains(99), "reduce submitted on all-done");
        // Idempotent: a second try_join doesn't re-submit / reset it.
        assert_eq!(try_join(&wf, &group).unwrap(), Join::AlreadySubmitted);
        // The retried shard recorded its attempt.
        assert_eq!(wf.get(11).unwrap().attempts, 1);
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
