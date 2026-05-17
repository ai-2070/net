//! Error type for NetDB operations.

use thiserror::Error;

use super::super::cortex::CortexAdapterError;

/// Errors produced by [`super::NetDb`] / [`super::NetDbBuilder`]
/// / [`super::NetDbSnapshot`].
#[derive(Debug, Error)]
pub enum NetDbError {
    /// An underlying CortEX adapter operation failed.
    #[error("cortex: {0}")]
    Cortex(#[from] CortexAdapterError),

    /// A model was accessed that wasn't included at build time.
    /// (Only raised by the `try_*` accessors; the panicking
    /// accessors never return this — they panic instead.)
    #[error("model '{0}' was not included in this NetDb")]
    ModelNotIncluded(&'static str),

    /// Snapshot encode / decode failure.
    #[error("snapshot: {0}")]
    Snapshot(String),

    /// `NetDbBuilder::build()` / `build_from_snapshot()` was
    /// called with neither `with_tasks()` nor `with_memories()`
    /// configured. Pre-fix this returned a no-op `NetDb` whose
    /// `tasks()` / `memories()` accessors panicked on first call.
    /// Surface it at build time as a typed error so a
    /// misconfigured profile or test fixture turns into a clean
    /// `?` rather than a process panic.
    #[error("NetDb must include at least one model; call with_tasks() or with_memories()")]
    NoModelsEnabled,
}
