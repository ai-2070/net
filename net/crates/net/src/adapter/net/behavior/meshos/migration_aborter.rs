//! Migration-abort dispatcher seam.
//!
//! Closes the gap left by `AdminEvent::KillMigration`: the
//! chain commit lands on every node, but until this seam is
//! installed nothing actually stops the migration. The MeshOS
//! event loop calls [`MigrationAborter::abort`] after folding a
//! verified [`super::event::AdminEvent::KillMigration`]; the
//! production adapter ([`OrchestratorMigrationAborter`]) wraps a
//! [`crate::adapter::net::compute::orchestrator::MigrationOrchestrator`]
//! and routes the call to
//! [`crate::adapter::net::compute::orchestrator::MigrationOrchestrator::abort_migration`].
//!
//! Mirrors the [`super::audit_chain`] /
//! [`super::log_chain`] / [`super::failure_chain`] pattern
//! exactly — trait + NoOp + Buffering + production adapter.
//!
//! # Per-node scope
//!
//! Only the node that owns the in-flight migration can actually
//! abort it. Other nodes that observe the same `KillMigration`
//! event call into their local aborter regardless; the
//! orchestrator's `abort_migration` is a fast no-op when the
//! `daemon_origin` isn't tracked locally. So every node wiring
//! the production adapter is the correct default; nodes without
//! it leave the migration unaffected, which is the
//! pre-integration behavior.

use std::sync::Arc;

use super::event::MigrationId;

/// Abort-failure surface — operator-readable reason. The event
/// loop logs the error and continues; a dispatcher hiccup must
/// never wedge the loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationAbortError {
    /// Reason the abort failed.
    pub reason: String,
}

impl std::fmt::Display for MigrationAbortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "migration-abort dispatcher failed: {}", self.reason)
    }
}

impl std::error::Error for MigrationAbortError {}

/// Trait the event loop calls after folding a verified
/// [`super::event::AdminEvent::KillMigration`]. Production
/// impls wrap a `MigrationOrchestrator` and route the call to
/// its abort path; tests + bootstrap use the
/// [`NoOpMigrationAborter`] default.
pub trait MigrationAborter: Send + Sync + 'static {
    /// Abort the migration with the given id. Returning `Err`
    /// is non-fatal — the loop logs and continues.
    fn abort(&self, migration: MigrationId) -> Result<(), MigrationAbortError>;
}

/// No-op aborter. The default. Returns `Ok(())` for every
/// call. Used when no `MigrationOrchestrator` is wired (tests,
/// bootstrap, in-process probes).
#[derive(Debug, Default)]
pub struct NoOpMigrationAborter;

impl MigrationAborter for NoOpMigrationAborter {
    fn abort(&self, _migration: MigrationId) -> Result<(), MigrationAbortError> {
        Ok(())
    }
}

/// Default cap on [`BufferingMigrationAborter`]. Sized to keep
/// tests bounded without truncating realistic burst sequences.
pub const DEFAULT_MIGRATION_ABORT_BUFFERING_CAPACITY: usize = 256;

/// Buffering aborter — captures abort calls in an internal
/// `VecDeque` so tests can inspect what the loop dispatched.
/// Bounded by [`Self::with_capacity`] (default
/// [`DEFAULT_MIGRATION_ABORT_BUFFERING_CAPACITY`]); past the
/// cap oldest entries drop FIFO and `dropped_count` increments.
#[derive(Debug)]
pub struct BufferingMigrationAborter {
    calls: parking_lot::Mutex<std::collections::VecDeque<MigrationId>>,
    capacity: usize,
    dropped: std::sync::atomic::AtomicU64,
}

impl Default for BufferingMigrationAborter {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MIGRATION_ABORT_BUFFERING_CAPACITY)
    }
}

impl BufferingMigrationAborter {
    /// Build with the given capacity. `capacity = 0` is clamped
    /// to `1`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            calls: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            capacity: capacity.max(1),
            dropped: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Snapshot the captured abort calls.
    pub fn captured(&self) -> Vec<MigrationId> {
        self.calls.lock().iter().copied().collect()
    }

    /// Number of calls dropped because the buffer was at
    /// capacity.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl MigrationAborter for BufferingMigrationAborter {
    fn abort(&self, migration: MigrationId) -> Result<(), MigrationAbortError> {
        let mut buf = self.calls.lock();
        if buf.len() >= self.capacity {
            buf.pop_front();
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        buf.push_back(migration);
        Ok(())
    }
}

/// Production aborter — wraps a
/// [`crate::adapter::net::compute::orchestrator::MigrationOrchestrator`]
/// and routes [`MigrationAborter::abort`] to its
/// `abort_migration` API. The `daemon_origin` argument the
/// orchestrator expects is the same `u64`
/// [`MigrationId`] the chain carries — they are the same
/// substrate-wide identifier.
pub struct OrchestratorMigrationAborter {
    orchestrator: Arc<crate::adapter::net::compute::orchestrator::MigrationOrchestrator>,
    /// Human-readable reason recorded with every abort. Lets
    /// the orchestrator's failure record disambiguate
    /// ICE-driven aborts from organic failures.
    reason: String,
}

impl OrchestratorMigrationAborter {
    /// Wrap an orchestrator with the default
    /// `"ICE KillMigration"` reason.
    pub fn new(
        orchestrator: Arc<crate::adapter::net::compute::orchestrator::MigrationOrchestrator>,
    ) -> Self {
        Self::with_reason(orchestrator, "ICE KillMigration".to_string())
    }

    /// Wrap an orchestrator with a custom reason. The reason is
    /// embedded in the orchestrator's failure record so audit
    /// review can distinguish ICE-driven from organic aborts.
    pub fn with_reason(
        orchestrator: Arc<crate::adapter::net::compute::orchestrator::MigrationOrchestrator>,
        reason: String,
    ) -> Self {
        Self {
            orchestrator,
            reason,
        }
    }
}

impl MigrationAborter for OrchestratorMigrationAborter {
    fn abort(&self, migration: MigrationId) -> Result<(), MigrationAbortError> {
        match self
            .orchestrator
            .abort_migration(migration, self.reason.clone())
        {
            Ok(_) => Ok(()),
            // The chain commit reaches every node; nodes that
            // don't host this migration return DaemonNotFound,
            // which is the expected per-node case. Treat as a
            // benign no-op rather than surface it to the loop.
            Err(crate::adapter::net::MigrationError::DaemonNotFound(_)) => Ok(()),
            Err(e) => Err(MigrationAbortError {
                reason: e.to_string(),
            }),
        }
    }
}

/// Convenient `Arc`-wrapped default; the loop holds an
/// `Arc<dyn MigrationAborter>` internally.
pub(crate) fn no_op_arc() -> Arc<dyn MigrationAborter> {
    Arc::new(NoOpMigrationAborter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_returns_ok_for_every_id() {
        let a = NoOpMigrationAborter;
        a.abort(1).expect("no_op infallible");
        a.abort(u64::MAX).expect("no_op infallible");
    }

    #[test]
    fn buffering_captures_ids_in_order() {
        let a = BufferingMigrationAborter::default();
        for i in 1..=3 {
            a.abort(i).unwrap();
        }
        let captured = a.captured();
        assert_eq!(captured, vec![1, 2, 3]);
        assert_eq!(a.dropped_count(), 0);
    }

    #[test]
    fn buffering_drops_oldest_when_over_capacity() {
        let a = BufferingMigrationAborter::with_capacity(2);
        for i in 1..=5 {
            a.abort(i).unwrap();
        }
        assert_eq!(a.captured(), vec![4, 5]);
        assert_eq!(a.dropped_count(), 3);
    }
}
