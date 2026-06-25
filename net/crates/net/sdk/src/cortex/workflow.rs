//! Task-lifecycle surface — the cortex workflow layer.
//!
//! A task is a single-writer RedEX chain; its `TaskState` is the
//! deterministic fold of transition events (`WorkflowAdapter`). This is
//! the companion to the gang-claim scheduler ([`crate::gang`]): the gang
//! decides *who* atomically holds an exclusive GPU island under
//! contention; this layer plans what happens *after* it is held — task
//! state, the step cursor, retries — with no workflow engine.
//!
//! ## Namespacing
//!
//! Distinct from the cortex *tasks* model (`Task` / `TaskStatus` in the
//! parent `cortex` module): the workflow `TaskStatus` is a lifecycle
//! state machine (`Submitted` / `Running` / `Waiting` / `Blocked` /
//! `Done` / `Failed`), so its `TaskId` / `TaskStatus` live here under
//! `cortex::workflow::` rather than the flat `cortex::` namespace to
//! avoid colliding with the tasks-model names.
//!
//! ## Example
//!
//! ```no_run
//! use net_sdk::cortex::{workflow::WorkflowAdapter, Redex};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let redex = Redex::new();
//! let wf = WorkflowAdapter::open(&redex, 0xABCD_EF01).await?;
//!
//! wf.submit(1)?; // Submitted
//! wf.start(1)?; // Running
//! let seq = wf.complete(1)?; // Done (terminal)
//! wf.wait_for_seq(seq).await.ok();
//!
//! let state = wf.get(1).expect("task present");
//! assert!(state.status.is_terminal());
//! # Ok::<_, Box<dyn std::error::Error>>(())
//! # })
//! # }
//! ```

pub use ::net::adapter::net::cortex::workflow::{
    StatusCounts, TaskId, TaskState, TaskStatus, WorkflowAdapter,
};
