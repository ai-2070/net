//! `WorkflowState` — the materialized view behind the
//! `CortexAdapter<WorkflowState>`'s `RwLock`: one [`TaskState`] per
//! live task id.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::types::{TaskId, TaskState, TaskStatus};

/// Counts of tasks per status — the observability / metrics summary
/// (plan piece 7). The event log is the chain itself; this is the
/// materialized roll-up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    /// Tasks in `Submitted`.
    pub submitted: usize,
    /// Tasks in `Running`.
    pub running: usize,
    /// Tasks in `Waiting`.
    pub waiting: usize,
    /// Tasks in `Blocked`.
    pub blocked: usize,
    /// Tasks in `Done`.
    pub done: usize,
    /// Tasks in `Failed`.
    pub failed: usize,
}

impl StatusCounts {
    /// Total live tasks across every status.
    pub fn total(&self) -> usize {
        self.submitted + self.running + self.waiting + self.blocked + self.done + self.failed
    }
}

/// Materialized view over the task-lifecycle log.
///
/// `Serialize` / `Deserialize` are derived so the state can be
/// snapshotted via [`CortexAdapter::snapshot`](super::super::CortexAdapter::snapshot)
/// (the plan's per-chain checkpoint that bounds failover replay) and
/// restored via `open_from_snapshot`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub(super) tasks: HashMap<TaskId, TaskState>,
    /// Task ids with a pending cancel request — the worker-observed
    /// signal (plan piece 6). Set by `request_cancel`, cleared on
    /// delete or a fresh submit.
    pub(super) cancelled: HashSet<TaskId>,
}

impl WorkflowState {
    /// Create an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a task's state by id (`TaskState` is `Copy`).
    pub fn get(&self, id: TaskId) -> Option<TaskState> {
        self.tasks.get(&id).copied()
    }

    /// Number of live tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// True if no tasks are live.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// True if a task with `id` is live.
    pub fn contains(&self, id: TaskId) -> bool {
        self.tasks.contains_key(&id)
    }

    /// Iterate over every live `(id, state)`.
    pub fn all(&self) -> impl Iterator<Item = (TaskId, TaskState)> + '_ {
        self.tasks.iter().map(|(id, st)| (*id, *st))
    }

    /// Iterate over the ids of tasks currently in `status` — the
    /// scheduler's "what's runnable / waiting / blocked" read.
    pub fn in_status(&self, status: TaskStatus) -> impl Iterator<Item = TaskId> + '_ {
        self.tasks
            .iter()
            .filter(move |(_, st)| st.status == status)
            .map(|(id, _)| *id)
    }

    /// Has cancellation been requested for `id`? The single-writer
    /// worker polls this and drives the task to a terminal status.
    pub fn is_cancel_requested(&self, id: TaskId) -> bool {
        self.cancelled.contains(&id)
    }

    /// Number of tasks with a pending cancel request.
    pub fn cancel_requested_count(&self) -> usize {
        self.cancelled.len()
    }

    /// Roll-up of task counts per status (observability summary).
    pub fn status_counts(&self) -> StatusCounts {
        let mut c = StatusCounts::default();
        for st in self.tasks.values() {
            match st.status {
                TaskStatus::Submitted => c.submitted += 1,
                TaskStatus::Running => c.running += 1,
                TaskStatus::Waiting => c.waiting += 1,
                TaskStatus::Blocked => c.blocked += 1,
                TaskStatus::Done => c.done += 1,
                TaskStatus::Failed => c.failed += 1,
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state() {
        let s = WorkflowState::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.get(1).is_none());
        assert!(!s.contains(1));
    }

    #[test]
    fn in_status_filters() {
        let mut s = WorkflowState::new();
        s.tasks.insert(1, TaskState::submitted());
        s.tasks.insert(
            2,
            TaskState {
                step: 3,
                status: TaskStatus::Running,
                attempts: 0,
            },
        );
        s.tasks.insert(
            3,
            TaskState {
                step: 9,
                status: TaskStatus::Done,
                attempts: 1,
            },
        );

        assert_eq!(s.len(), 3);
        let running: Vec<TaskId> = s.in_status(TaskStatus::Running).collect();
        assert_eq!(running, vec![2]);
        assert_eq!(s.in_status(TaskStatus::Submitted).count(), 1);
        assert_eq!(s.in_status(TaskStatus::Done).count(), 1);
        assert_eq!(s.get(2).unwrap().step, 3);
    }
}
