//! NetDB — unified query façade over CortEX state.
//!
//! Sits above the CortEX adapter layer. Bundles one or more models
//! (tasks, memories) behind a single [`NetDb`] handle and exposes a
//! Prisma-ish query surface (`db.tasks().find_many(&filter)`,
//! `db.memories().count_where(&filter)`, …).
//!
//! NetDB does **not** expose raw RedEX events or streams. For raw
//! event access, drop down to [`crate::adapter::net::redex::RedexFile`]
//! directly.
//!
//! See `docs/internal/plans/NETDB_PLAN.md` for the full design.

mod db;
mod error;

pub use db::{NetDb, NetDbBuilder, NetDbSnapshot};
pub use error::NetDbError;

// Re-export the filter types. They're owned by the model modules
// (tasks / memories) so the fluent query builders there can see
// their internal `apply` method, but callers of NetDB import them
// from one place.
pub use super::cortex::memories::MemoriesFilter;
pub use super::cortex::tasks::TasksFilter;
