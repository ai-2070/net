//! Dispatch byte constants for task-lifecycle events.
//!
//! These live in the `0x00..0x7F` CortEX-internal range (per the
//! adapter plan) on the dedicated [`WORKFLOW_CHANNEL`], so they share
//! no dispatch space with the `cortex/tasks` demo model.

/// A task was submitted (enters the chain as `Submitted`, step 0).
pub const DISPATCH_TASK_SUBMITTED: u8 = 0x01;
/// A task's status changed (`Submitted`/`Running`/`Waiting`/`Blocked`/
/// `Done`/`Failed`).
pub const DISPATCH_TASK_TRANSITIONED: u8 = 0x02;
/// A task advanced its step cursor (a step completed) — resets the
/// per-step attempt counter.
pub const DISPATCH_TASK_ADVANCED: u8 = 0x03;
/// A task retried the current step (bumps `attempts`, status →
/// `Running`).
pub const DISPATCH_TASK_RETRIED: u8 = 0x04;
/// A task was deleted (reclaims its subtree).
pub const DISPATCH_TASK_DELETED: u8 = 0x05;
/// Cancellation was requested for a task — a worker-observed signal
/// (the `cancel.json` of the plan). The single-writer worker sees it
/// and drives the task to a terminal status; this event only records
/// the request.
pub const DISPATCH_TASK_CANCEL_REQUESTED: u8 = 0x06;

/// Canonical channel name for the task-lifecycle model.
pub const WORKFLOW_CHANNEL: &str = "cortex/workflow";

// Static assertions that the allocated dispatches fall in CortEX's
// reserved range (0x00..0x7F).
const _: () = {
    assert!(DISPATCH_TASK_SUBMITTED < 0x80);
    assert!(DISPATCH_TASK_TRANSITIONED < 0x80);
    assert!(DISPATCH_TASK_ADVANCED < 0x80);
    assert!(DISPATCH_TASK_RETRIED < 0x80);
    assert!(DISPATCH_TASK_DELETED < 0x80);
    assert!(DISPATCH_TASK_CANCEL_REQUESTED < 0x80);
};
