//! Admin audit chain seam.
//!
//! [`super::event_loop::MeshOsLoop`] calls
//! [`AdminAuditChainAppender::append`]
//! every time it pushes an [`AdminAuditRecord`] onto the
//! in-memory ring. The trait lets production deployments wire
//! a real RedEX chain for unbounded history while tests and
//! bootstrap paths use the [`NoOpAdminAuditChainAppender`]
//! default. Mirrors [`super::chain::ActionChainAppender`]'s
//! pattern — see that module for the design rationale.
//!
//! # Scope
//!
//! This module ships the seam (trait + no-op + buffering
//! impls). The real `TypedRedexFile<AdminAuditRecord>` impl
//! lives in the substrate slice that wires MeshOS to RedEX
//! for cluster-lifetime replay. Until that lands, the
//! in-memory ring on `MeshOsState.admin_audit` is the only
//! readable surface — the chain seam is a no-op in the
//! default constructors.

use std::sync::Arc;

use super::ice::AdminAuditRecord;

/// Append-failure surface — operator-readable reason. The
/// loop logs the error and continues; an audit-chain hiccup
/// must never wedge the reconcile pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminAuditAppendError {
    /// Reason the append failed.
    pub reason: String,
}

impl std::fmt::Display for AdminAuditAppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "admin-audit-chain append failed: {}", self.reason)
    }
}

impl std::error::Error for AdminAuditAppendError {}

/// Trait the loop calls per recorded `AdminAuditRecord`.
/// Production impls write to a RedEX chain so consumers can
/// replay the full history. Test + bootstrap impls can be
/// no-op or buffering.
pub trait AdminAuditChainAppender: Send + Sync + 'static {
    /// Append a record. Errors are non-fatal — the loop
    /// continues regardless.
    fn append(&self, record: &AdminAuditRecord) -> Result<(), AdminAuditAppendError>;
}

/// No-op appender. The default. Returns `Ok(())` for every
/// record. Used when no RedEX chain is wired (tests,
/// in-process dev runtimes, the bootstrap window before the
/// substrate's chain layer is up).
#[derive(Debug, Default)]
pub struct NoOpAdminAuditChainAppender;

impl AdminAuditChainAppender for NoOpAdminAuditChainAppender {
    fn append(&self, _record: &AdminAuditRecord) -> Result<(), AdminAuditAppendError> {
        Ok(())
    }
}

/// Default cap on [`BufferingAdminAuditChainAppender`] —
/// bounds the buffer so a runaway test can't OOM the process.
/// Past the cap, oldest records drop FIFO.
pub const DEFAULT_AUDIT_BUFFERING_APPENDER_CAPACITY: usize = 4096;

/// Buffering appender — collects records in an internal bounded
/// ring so tests can `assert_eq!` against the captured stream.
/// Default capacity is [`DEFAULT_AUDIT_BUFFERING_APPENDER_CAPACITY`];
/// past the cap, oldest records drop FIFO and `dropped_count`
/// increments.
#[derive(Debug)]
pub struct BufferingAdminAuditChainAppender {
    records: super::super::bounded_ring::BoundedRing<AdminAuditRecord>,
}

impl Default for BufferingAdminAuditChainAppender {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_AUDIT_BUFFERING_APPENDER_CAPACITY)
    }
}

impl BufferingAdminAuditChainAppender {
    /// Build with the given capacity. `capacity = 0` is clamped
    /// to `1` — a zero-cap buffer can't hold the record
    /// currently being appended.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: super::super::bounded_ring::BoundedRing::new(capacity.max(1)),
        }
    }

    /// Snapshot the captured records (oldest → newest).
    pub fn captured(&self) -> Vec<AdminAuditRecord> {
        self.records.snapshot()
    }

    /// Number of records dropped because the buffer was at
    /// capacity. Strictly non-decreasing.
    pub fn dropped_count(&self) -> u64 {
        self.records.dropped()
    }
}

impl AdminAuditChainAppender for BufferingAdminAuditChainAppender {
    fn append(&self, record: &AdminAuditRecord) -> Result<(), AdminAuditAppendError> {
        self.records.push(record.clone());
        Ok(())
    }
}

/// Convenient `Arc`-wrapped default; the loop holds an
/// [`Arc<dyn AdminAuditChainAppender>`] internally.
pub(crate) fn no_op_arc() -> Arc<dyn AdminAuditChainAppender> {
    Arc::new(NoOpAdminAuditChainAppender)
}

#[cfg(test)]
mod tests {
    use super::super::event::AdminEvent;
    use super::super::ice::VerificationOutcome;
    use super::*;
    use std::time::Duration;

    fn fixture(seq: u64) -> AdminAuditRecord {
        AdminAuditRecord {
            seq,
            committed_at_ms: 1_000 + seq,
            event: AdminEvent::FreezeCluster {
                ttl: Duration::from_secs(30),
            },
            operator_ids: vec![7],
            outcome: VerificationOutcome::Accepted,
            chain_pending: false,
        }
    }

    #[test]
    fn no_op_returns_ok_for_every_record() {
        let app = NoOpAdminAuditChainAppender;
        app.append(&fixture(1)).expect("no_op should be infallible");
        app.append(&fixture(2)).expect("no_op should be infallible");
    }

    #[test]
    fn buffering_captures_records_in_order() {
        let app = BufferingAdminAuditChainAppender::default();
        for i in 1..=3 {
            app.append(&fixture(i)).unwrap();
        }
        let captured = app.captured();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0].seq, 1);
        assert_eq!(captured[2].seq, 3);
    }

    #[test]
    fn buffering_drops_oldest_when_over_capacity() {
        let app = BufferingAdminAuditChainAppender::with_capacity(2);
        for i in 1..=5 {
            app.append(&fixture(i)).unwrap();
        }
        let captured = app.captured();
        // Capacity 2, so only the last two records remain.
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].seq, 4);
        assert_eq!(captured[1].seq, 5);
        // Three records dropped (1, 2, 3).
        assert_eq!(app.dropped_count(), 3);
    }

    #[test]
    fn buffering_with_capacity_zero_clamps_to_one() {
        let app = BufferingAdminAuditChainAppender::with_capacity(0);
        app.append(&fixture(1)).unwrap();
        app.append(&fixture(2)).unwrap();
        let captured = app.captured();
        // Only the most recent record fits.
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].seq, 2);
        assert_eq!(app.dropped_count(), 1);
    }
}
