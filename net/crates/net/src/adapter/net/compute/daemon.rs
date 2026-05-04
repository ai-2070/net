//! MeshDaemon trait and supporting types.
//!
//! A daemon is a stateful or stateless event processor that runs on the mesh.
//! It consumes causal events and produces output events. The runtime handles
//! chain building, horizon tracking, and snapshot packaging.

use bytes::Bytes;

use crate::adapter::net::behavior::capability::CapabilityFilter;
use crate::adapter::net::state::causal::CausalEvent;

/// A daemon that runs on the mesh.
///
/// Daemons consume inbound causal events via `process()` and return zero or
/// more output payloads. The runtime wraps outputs in `CausalLink`s
/// automatically — the daemon only produces raw payloads.
///
/// # Performance
///
/// `process()` must complete in microseconds. Heavy work should be deferred
/// to a background task and emitted as a later event.
///
/// # WASM compatibility
///
/// All methods are synchronous — no async. Input/output are `Bytes` — maps
/// cleanly to WASM linear memory. No generics or associated types.
pub trait MeshDaemon: Send + Sync {
    /// Human-readable name (for logging, placement ads).
    fn name(&self) -> &str;

    /// Capability requirements for placement.
    ///
    /// The scheduler uses this to find nodes whose `CapabilitySet` matches.
    /// Return `CapabilityFilter::default()` to run anywhere.
    fn requirements(&self) -> CapabilityFilter;

    /// Process one inbound causal event, returning zero or more output payloads.
    ///
    /// The output `Bytes` values become payloads in the daemon's own causal
    /// chain (the runtime wraps them in CausalLinks automatically).
    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError>;

    /// Serialize current state for migration/checkpoint.
    ///
    /// Returns `None` for stateless daemons. Stateful daemons must return
    /// opaque bytes that `restore()` can accept.
    fn snapshot(&self) -> Option<Bytes> {
        None
    }

    /// Whether this daemon carries persistent state that
    /// migration / restart paths must preserve.
    ///
    /// The default `restore` previously accepted any bytes silently
    /// for daemons that didn't override it, including ones that
    /// *should* have been stateful but forgot to provide a `restore`
    /// impl. The new default restores correctly: it matches
    /// `is_stateful()`'s answer. Stateless daemons leave
    /// `is_stateful` at `false` (matches `snapshot() = None`);
    /// stateful daemons override `is_stateful` to `true` AND
    /// `snapshot` / `restore`.
    ///
    /// The migration path can use this to refuse to migrate a
    /// stateful daemon's snapshot bytes into a stateless target,
    /// surfacing the misconfiguration rather than silently
    /// dropping state.
    fn is_stateful(&self) -> bool {
        false
    }

    /// Restore from a previous snapshot.
    ///
    /// Called before any `process()` calls after migration.
    ///
    /// The default implementation now refuses non-empty state on
    /// stateless daemons (`is_stateful() == false`) — silently
    /// discarding a stateful source's snapshot into a stateless
    /// target loses every byte of state with no signal. Stateful
    /// daemons must override both `is_stateful` and `restore`. An
    /// empty `state` is still accepted (it's what
    /// `snapshot() -> None` produces under the migration adapter),
    /// so genuine stateless-to-stateless migrations
    /// continue to work.
    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
        if !self.is_stateful() && !state.is_empty() {
            return Err(DaemonError::RestoreFailed(format!(
                "stateless daemon (is_stateful=false) cannot restore \
                 {}-byte snapshot — override is_stateful() + restore() \
                 if this daemon is actually stateful",
                state.len()
            )));
        }
        Ok(())
    }
}

/// Errors from daemon operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    /// Daemon processing logic failed.
    ProcessFailed(String),
    /// Snapshot serialization failed.
    SnapshotFailed(String),
    /// Restore from snapshot failed.
    RestoreFailed(String),
    /// Daemon not found in registry.
    NotFound(u32),
    /// The daemon at this origin_hash was concurrently swapped
    /// (`replace`d) or `unregister`ed while this caller was
    /// preparing to mutate it. The caller's mutation did not
    /// land — the registry detected the orphaned `Arc` after
    /// acquiring the inner lock and bailed before invoking the
    /// host. Retry the operation against the current
    /// registered host (if any). Audit #13/#68.
    Stale(u32),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessFailed(msg) => write!(f, "daemon process failed: {}", msg),
            Self::SnapshotFailed(msg) => write!(f, "snapshot failed: {}", msg),
            Self::RestoreFailed(msg) => write!(f, "restore failed: {}", msg),
            Self::NotFound(id) => write!(f, "daemon not found: {:#x}", id),
            Self::Stale(id) => write!(
                f,
                "daemon {:#x} was swapped or unregistered concurrently; mutation did not land",
                id
            ),
        }
    }
}

impl std::error::Error for DaemonError {}

/// Configuration for a daemon host.
#[derive(Debug, Clone)]
pub struct DaemonHostConfig {
    /// How often to auto-snapshot (in events processed). 0 = manual only.
    pub auto_snapshot_interval: u64,
    /// Maximum events to buffer before forcing a snapshot.
    pub max_log_entries: u32,
}

impl Default for DaemonHostConfig {
    fn default() -> Self {
        Self {
            auto_snapshot_interval: 0,
            max_log_entries: 10_000,
        }
    }
}

/// Runtime statistics for a daemon.
#[derive(Debug, Clone, Default)]
pub struct DaemonStats {
    /// Total events processed.
    pub events_processed: u64,
    /// Total output events emitted.
    pub events_emitted: u64,
    /// Total processing errors.
    pub errors: u64,
    /// Number of snapshots taken.
    pub snapshots_taken: u64,
}
