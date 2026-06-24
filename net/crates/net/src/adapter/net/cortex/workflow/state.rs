//! `WorkflowState` — the materialized view behind the
//! `CortexAdapter<WorkflowState>`'s `RwLock`: one [`TaskState`] per
//! live task id.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::types::{TaskId, TaskState, TaskStatus};

/// Materialized view over the task-lifecycle log.
///
/// `Serialize` / `Deserialize` are derived so the state can be
/// snapshotted via [`CortexAdapter::snapshot`](super::super::CortexAdapter::snapshot)
/// (the plan's per-chain checkpoint that bounds failover replay) and
/// restored via `open_from_snapshot`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub(super) tasks: HashMap<TaskId, TaskState>,
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
