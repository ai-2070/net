//! Task Lifecycle — the workflow layer that runs *on top of* a held
//! resource (companion to the Thunderdome gang claim in
//! `behavior::gang`).
//!
//! Where Thunderdome decides *who atomically gets an exclusive
//! capability under contention*, this layer plans what happens *after*
//! it is held: task state, the step cursor, retries — and, in later
//! phases, dependencies / fan-out / DAGs — all as **emergence from
//! RedEX primitives**, with no workflow engine, no DAG DSL, no
//! controller loops.
//!
//! A task is a single-writer RedEX chain; its [`TaskState`] is the
//! deterministic fold ([`WorkflowFold`]) of transition events. The
//! single writer is the task-lease holder; reopening the chain replays
//! the history exactly (failover-resume). See
//! `docs/plans/TASK_LIFECYCLE_PLAN.md`.
//!
//! Phase A ships the state machine + replay here ([`WorkflowAdapter`])
//! and the task lease in [`lease`]; triggers, shards, capability-
//! bearing steps (which route through Thunderdome) build on top.

mod adapter;
mod dispatch;
mod fold;
pub mod lease;
mod state;
mod types;

pub use adapter::WorkflowAdapter;
pub use dispatch::{
    DISPATCH_TASK_ADVANCED, DISPATCH_TASK_DELETED, DISPATCH_TASK_RETRIED, DISPATCH_TASK_SUBMITTED,
    DISPATCH_TASK_TRANSITIONED, WORKFLOW_CHANNEL,
};
pub use fold::WorkflowFold;
pub use lease::{TaskLease, TaskLeaseOutcome};
pub use state::WorkflowState;
pub use types::{TaskId, TaskState, TaskStatus};
