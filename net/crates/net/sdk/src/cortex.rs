//! CortEX + RedEX + NetDb surface.
//!
//! Re-exports of the core event-sourced storage layer so SDK users
//! can open typed domain adapters (tasks, memories) without depending
//! on the `net` crate directly.
//!
//! ## Entry points
//!
//! - [`Redex`] — storage manager. Create in-memory with [`Redex::new`]
//!   or disk-backed with [`Redex::with_persistent_dir`].
//! - [`NetDb`] — unified handle bundling the enabled model adapters
//!   behind a single query facade. Build via [`NetDb::builder`].
//! - [`TasksAdapter`] / [`MemoriesAdapter`] — typed adapters if you
//!   only need one model and don't want the `NetDb` wrapper.
//! - [`RedexFile`] — raw event-log primitive for domain-agnostic use.
//!
//! ## Example
//!
//! ```no_run
//! use net_sdk::cortex::{NetDb, Redex};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let redex = Redex::new();
//! let db = NetDb::builder(redex)
//!     .origin(0xABCD_EF01)
//!     .with_tasks()
//!     .with_memories()
//!     .build()
//!     .await?;
//!
//! // Drive the tasks adapter:
//! let seq = db.tasks().create(1, "write docs", 0)?;
//! db.tasks().wait_for_seq(seq).await;
//!
//! // Snapshot + watch for reactive UI:
//! let watcher = db.tasks().watch();
//! let (snapshot, _stream) = db.tasks().snapshot_and_watch(watcher);
//! assert_eq!(snapshot.len(), 1);
//! # Ok::<_, Box<dyn std::error::Error>>(())
//! # })
//! # }
//! ```
//!
//! ## Persistence
//!
//! Disk-backed files need `Redex::with_persistent_dir`. Pair with
//! `NetDbBuilder::persistent(true)` to route every enabled model's
//! RedEX file through the disk segment:
//!
//! ```no_run
//! # use net_sdk::cortex::{NetDb, Redex};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let redex = Redex::new().with_persistent_dir("/var/lib/net/redex");
//! let db = NetDb::builder(redex)
//!     .origin(0xABCD_EF01)
//!     .persistent(true)
//!     .with_tasks()
//!     .build()
//!     .await?;
//! # drop(db);
//! # Ok::<_, Box<dyn std::error::Error>>(())
//! # })
//! # }
//! ```

// ---- Storage primitive (RedEX) ---------------------------------------------

pub use ::net::adapter::net::redex::{
    FsyncPolicy, OrderedAppender, Redex, RedexError, RedexEvent, RedexFile, RedexFileConfig,
    TypedRedexFile,
};

// ---- CortEX domain adapters ------------------------------------------------

pub use ::net::adapter::net::cortex::{
    compute_checksum, CortexAdapterError, EventEnvelope, EventMeta, IntoRedexPayload,
    EVENT_META_SIZE,
};

pub use ::net::adapter::net::cortex::tasks::{
    Task, TaskId, TaskStatus, TasksAdapter, TasksFilter, TasksQuery, TasksState, TasksWatcher,
    DISPATCH_TASK_COMPLETED, DISPATCH_TASK_CREATED, DISPATCH_TASK_DELETED, DISPATCH_TASK_RENAMED,
    TASKS_CHANNEL,
};

/// Re-export of the tasks-module `OrderBy` enum. Aliased so the
/// memories variant can coexist in this flat namespace.
pub use ::net::adapter::net::cortex::tasks::OrderBy as TasksOrderBy;

pub use ::net::adapter::net::cortex::memories::{
    MemoriesAdapter, MemoriesFilter, MemoriesQuery, MemoriesState, MemoriesWatcher, Memory,
    MemoryId, MEMORIES_CHANNEL,
};

pub use ::net::adapter::net::cortex::memories::OrderBy as MemoriesOrderBy;

// ---- NetDb facade ----------------------------------------------------------

pub use ::net::adapter::net::netdb::{NetDb, NetDbBuilder, NetDbError, NetDbSnapshot};

// ---- Task lifecycle (workflow) ---------------------------------------------

/// Task-lifecycle orchestration ([`WorkflowAdapter`](workflow::WorkflowAdapter)).
///
/// Namespaced under `workflow` rather than flattened here because its
/// `TaskId` / `TaskStatus` would otherwise collide with the cortex
/// *tasks* model re-exported above.
pub mod workflow;
