//! Failure chain seam.
//!
//! [`super::executor::ActionExecutor`] calls
//! [`FailureChainAppender::append`] every time it pushes a
//! [`super::snapshot::FailureRecord`] onto the in-memory
//! recent-failures ring. Production deployments wire a real
//! `TypedRedexFile<FailureRecord>` for cluster-lifetime
//! failure replay; tests + bootstrap use the
//! [`NoOpFailureChainAppender`] default. Mirrors the
//! [`super::audit_chain`] and [`super::log_chain`] patterns
//! exactly — see those modules for the design rationale.
//!
//! # Scope
//!
//! This module ships the seam (trait + no-op + buffering
//! impls). The real `TypedRedexFile<FailureRecord>` impl
//! lives in the substrate slice that wires MeshOS to RedEX
//! for cluster-lifetime replay. Until that lands, the
//! in-memory ring on the executor (read via
//! `MeshOsSnapshot.recent_failures`) is the only readable
//! surface.

use std::sync::Arc;

use super::snapshot::FailureRecord;

/// Append-failure surface — operator-readable reason. The
/// executor logs the error and continues; a failure-chain
/// hiccup must never wedge the executor's dispatch loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureAppendError {
    /// Reason the append failed.
    pub reason: String,
}

impl std::fmt::Display for FailureAppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failure-chain append failed: {}", self.reason)
    }
}

impl std::error::Error for FailureAppendError {}

/// Trait the executor calls per recorded `FailureRecord`.
/// Production impls write to a RedEX chain so consumers can
/// replay the full failure history. Test + bootstrap impls
/// can be no-op or buffering.
pub trait FailureChainAppender: Send + Sync + 'static {
    /// Append a record. Errors are non-fatal — the executor
    /// continues regardless.
    fn append(&self, record: &FailureRecord) -> Result<(), FailureAppendError>;
}

/// No-op appender. The default. Returns `Ok(())` for every
/// record. Used when no RedEX chain is wired.
#[derive(Debug, Default)]
pub struct NoOpFailureChainAppender;

impl FailureChainAppender for NoOpFailureChainAppender {
    fn append(&self, _record: &FailureRecord) -> Result<(), FailureAppendError> {
        Ok(())
    }
}

/// Default cap on [`BufferingFailureChainAppender`]. Sized to
/// match the executor's in-memory ring scale.
pub const DEFAULT_FAILURE_BUFFERING_APPENDER_CAPACITY: usize = 4096;

/// Buffering appender — collects records in an internal
/// `VecDeque` so tests can inspect the captured stream.
/// Bounded by [`Self::with_capacity`] (default
/// [`DEFAULT_FAILURE_BUFFERING_APPENDER_CAPACITY`]); past the
/// cap oldest records drop FIFO and `dropped_count`
/// increments.
#[derive(Debug)]
pub struct BufferingFailureChainAppender {
    records: parking_lot::Mutex<std::collections::VecDeque<FailureRecord>>,
    capacity: usize,
    dropped: std::sync::atomic::AtomicU64,
}

impl Default for BufferingFailureChainAppender {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_FAILURE_BUFFERING_APPENDER_CAPACITY)
    }
}

impl BufferingFailureChainAppender {
    /// Build with the given capacity. `capacity = 0` is
    /// clamped to `1`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            capacity: capacity.max(1),
            dropped: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Snapshot the captured records.
    pub fn captured(&self) -> Vec<FailureRecord> {
        self.records.lock().iter().cloned().collect()
    }

    /// Number of records dropped because the buffer was at
    /// capacity.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl FailureChainAppender for BufferingFailureChainAppender {
    fn append(&self, record: &FailureRecord) -> Result<(), FailureAppendError> {
        let mut buf = self.records.lock();
        if buf.len() >= self.capacity {
            buf.pop_front();
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        buf.push_back(record.clone());
        Ok(())
    }
}

/// Convenient `Arc`-wrapped default; the executor holds an
/// `Arc<dyn FailureChainAppender>` internally.
pub(crate) fn no_op_arc() -> Arc<dyn FailureChainAppender> {
    Arc::new(NoOpFailureChainAppender)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(seq: u64) -> FailureRecord {
        FailureRecord {
            seq,
            source: format!("source:{seq}"),
            reason: format!("reason {seq}"),
            recorded_at_ms: 1_000 + seq,
        }
    }

    #[test]
    fn no_op_returns_ok_for_every_record() {
        let app = NoOpFailureChainAppender;
        app.append(&fixture(1)).expect("no_op should be infallible");
        app.append(&fixture(2)).expect("no_op should be infallible");
    }

    #[test]
    fn buffering_captures_records_in_order() {
        let app = BufferingFailureChainAppender::default();
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
        let app = BufferingFailureChainAppender::with_capacity(2);
        for i in 1..=5 {
            app.append(&fixture(i)).unwrap();
        }
        let captured = app.captured();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].seq, 4);
        assert_eq!(captured[1].seq, 5);
        assert_eq!(app.dropped_count(), 3);
    }
}
