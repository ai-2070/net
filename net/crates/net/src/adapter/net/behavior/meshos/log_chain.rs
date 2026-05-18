//! Per-node log chain seam.
//!
//! [`super::event_loop::MeshOsLoop`] calls
//! [`LogChainAppender::append`] every time it pushes a
//! [`super::logs::LogRecord`] onto the in-memory ring.
//! Production deployments wire a real
//! `TypedRedexFile<LogRecord>` for cluster-lifetime log
//! replay; tests + bootstrap use the [`NoOpLogChainAppender`]
//! default. Mirrors the [`super::audit_chain`] pattern exactly
//! — see that module for the design rationale.
//!
//! # Scope
//!
//! This module ships the seam (trait + no-op + buffering
//! impls). The real `TypedRedexFile<LogRecord>` impl lives in
//! the substrate slice that wires MeshOS to RedEX for the
//! per-daemon log chain the plan describes
//! (`DECK_SDK_PLAN.md` Phase 1, "log stream"). Until that
//! lands, the in-memory ring on `MeshOsState.log_ring` is the
//! only readable surface.

use std::sync::Arc;

use super::logs::LogRecord;

/// Append-failure surface — operator-readable reason. The
/// loop logs the error and continues; a log-chain hiccup
/// must never wedge the reconcile pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogAppendError {
    /// Reason the append failed.
    pub reason: String,
}

impl std::fmt::Display for LogAppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "log-chain append failed: {}", self.reason)
    }
}

impl std::error::Error for LogAppendError {}

/// Trait the loop calls per recorded `LogRecord`. Production
/// impls write to a RedEX chain so consumers can replay the
/// full log history. Test + bootstrap impls can be no-op or
/// buffering.
pub trait LogChainAppender: Send + Sync + 'static {
    /// Append a record. Errors are non-fatal — the loop
    /// continues regardless.
    fn append(&self, record: &LogRecord) -> Result<(), LogAppendError>;
}

/// No-op appender. The default. Returns `Ok(())` for every
/// record. Used when no RedEX chain is wired.
#[derive(Debug, Default)]
pub struct NoOpLogChainAppender;

impl LogChainAppender for NoOpLogChainAppender {
    fn append(&self, _record: &LogRecord) -> Result<(), LogAppendError> {
        Ok(())
    }
}

/// Default cap on [`BufferingLogChainAppender`]. Sized larger
/// than the admin-audit buffer because log volume is
/// naturally noisier than admin volume.
pub const DEFAULT_LOG_BUFFERING_APPENDER_CAPACITY: usize = 16_384;

/// Buffering appender — collects records in an internal
/// `VecDeque` so tests can inspect the captured stream.
/// Bounded by [`Self::with_capacity`] (default
/// [`DEFAULT_LOG_BUFFERING_APPENDER_CAPACITY`]); past the cap
/// oldest records drop FIFO and `dropped_count` increments.
#[derive(Debug)]
pub struct BufferingLogChainAppender {
    records: parking_lot::Mutex<std::collections::VecDeque<LogRecord>>,
    capacity: usize,
    dropped: std::sync::atomic::AtomicU64,
}

impl Default for BufferingLogChainAppender {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_LOG_BUFFERING_APPENDER_CAPACITY)
    }
}

impl BufferingLogChainAppender {
    /// Build with the given capacity. `capacity = 0` is
    /// clamped to `1`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            capacity: capacity.max(1),
            dropped: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Snapshot the captured records. Cheap — one mutex lock
    /// + a vec clone.
    pub fn captured(&self) -> Vec<LogRecord> {
        self.records.lock().iter().cloned().collect()
    }

    /// Number of records dropped because the buffer was at
    /// capacity. Strictly increasing.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl LogChainAppender for BufferingLogChainAppender {
    fn append(&self, record: &LogRecord) -> Result<(), LogAppendError> {
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

/// Convenient `Arc`-wrapped default; the loop holds an
/// `Arc<dyn LogChainAppender>` internally.
pub(crate) fn no_op_arc() -> Arc<dyn LogChainAppender> {
    Arc::new(NoOpLogChainAppender)
}

#[cfg(test)]
mod tests {
    use super::super::logs::LogLevel;
    use super::*;

    fn fixture(seq: u64) -> LogRecord {
        LogRecord {
            seq,
            ts_ms: 1_700_000_000_000 + seq,
            level: LogLevel::Info,
            daemon_id: Some(7),
            node_id: Some(100),
            message: format!("message {seq}"),
            chain_pending: false,
        }
    }

    #[test]
    fn no_op_returns_ok_for_every_record() {
        let app = NoOpLogChainAppender;
        app.append(&fixture(1)).expect("no_op should be infallible");
        app.append(&fixture(2)).expect("no_op should be infallible");
    }

    #[test]
    fn buffering_captures_records_in_order() {
        let app = BufferingLogChainAppender::default();
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
        let app = BufferingLogChainAppender::with_capacity(2);
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
