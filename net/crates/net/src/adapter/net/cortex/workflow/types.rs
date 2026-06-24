//! Domain types and event payloads for the task-lifecycle model.
//!
//! A task is a single-writer chain; its lifecycle is the deterministic
//! fold of transition events into a [`TaskState`]. Per the plan, the
//! state carries only `{ step, status, attempts }` — no wall-clock —
//! so the same chain always folds to the same state and replay is
//! exact.

use serde::{Deserialize, Serialize};

/// Identifier for a task. Opaque `u64`; the same id is the
/// [`ResourceId`](crate::adapter::net::behavior::fold::ResourceId) the
/// task lease ([`super::lease`]) reserves.
pub type TaskId = u64;

/// Lifecycle status of a task. The single-writer worker advances it;
/// schedulers read it. `Done` / `Failed` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Created, not yet running.
    Submitted,
    /// A step is executing.
    Running,
    /// Parked on a trigger / claim and will re-request (the
    /// Thunderdome-reject and trigger-wait state).
    Waiting,
    /// Parked on an unmet dependency.
    Blocked,
    /// Every step complete — terminal.
    Done,
    /// Terminal failure.
    Failed,
}

impl TaskStatus {
    /// Is this a terminal status (`Done` / `Failed`)?
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Failed)
    }
}

/// The materialized lifecycle state of one task — the explicit cursor
/// schedulers read.
///
/// - `step` — the worker-advanced cursor (which step of the job is
///   current).
/// - `status` — the current [`TaskStatus`].
/// - `attempts` — retries of the *current* step.
///
/// Deliberately carries no timestamp: transitions are driven by
/// explicit events, so the fold never reads `now()` and replay
/// reproduces the state byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    /// Current step cursor.
    pub step: u32,
    /// Current status.
    pub status: TaskStatus,
    /// Retries of the current step.
    pub attempts: u32,
}

impl TaskState {
    /// The state a freshly-submitted task starts in: step 0,
    /// `Submitted`, zero attempts.
    pub fn submitted() -> Self {
        Self {
            step: 0,
            status: TaskStatus::Submitted,
            attempts: 0,
        }
    }
}

// ---- Event payload structs (serialized after the EventMeta header) ----

/// Payload for `DISPATCH_TASK_SUBMITTED`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SubmittedPayload {
    pub id: TaskId,
}

/// Payload for `DISPATCH_TASK_TRANSITIONED` — set the task's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TransitionedPayload {
    pub id: TaskId,
    pub status: TaskStatus,
}

/// Payload for `DISPATCH_TASK_ADVANCED` — advance the step cursor and
/// reset the per-step attempt counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AdvancedPayload {
    pub id: TaskId,
}

/// Payload for `DISPATCH_TASK_RETRIED` — bump attempts and re-run the
/// current step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RetriedPayload {
    pub id: TaskId,
}

/// Payload for `DISPATCH_TASK_DELETED` — reclaim the task subtree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DeletedPayload {
    pub id: TaskId,
}

/// Payload for `DISPATCH_TASK_CANCEL_REQUESTED` — record a cancel
/// signal for the worker to observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CancelRequestedPayload {
    pub id: TaskId,
}
