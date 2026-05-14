//! Per-node log ring for operator-visible diagnostics.
//!
//! `MeshOsLoop` maintains a bounded ring of [`LogRecord`]s
//! produced via `MeshOsEvent::LogLine`. Daemons / source
//! converters publish into the ring; the Deck SDK's
//! `subscribe_logs(filter)` returns a stream over it.
//!
//! # Scope
//!
//! This slice ships an **in-memory ring per node**. Bounded
//! by [`DEFAULT_MAX_LOG_RING_RECORDS`]; older entries drop
//! FIFO when the cap is exceeded. The canonical per-daemon
//! RedEX-tail integration the plan calls out ("per-daemon log
//! chains via RedEX `tail()`") replaces the ring's backing
//! store in a future substrate slice without changing the SDK
//! API.
//!
//! # Wire shape
//!
//! Both [`LogRecord`] and [`LogLevel`] are
//! `Serialize + Deserialize` so the snapshot's log ring
//! round-trips through postcard and serde_json identically to
//! the admin audit ring.

use serde::{Deserialize, Serialize};

use super::event::NodeId;

/// Default cap on the per-node log ring. Records older than
/// this drop FIFO so the substrate's log buffer stays
/// fixed-overhead under churn. Sized larger than the admin
/// audit ring because log volume is naturally noisier than
/// admin volume.
pub const DEFAULT_MAX_LOG_RING_RECORDS: usize = 2048;

/// Log severity. Matches `tracing` semantics so converters
/// that bridge `tracing::Event` into the ring have a clean
/// mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LogLevel {
    /// Finest-grained signal — only useful for deep tracing.
    Trace,
    /// Verbose runtime signal.
    Debug,
    /// Normal operational signal.
    Info,
    /// Operator-relevant but non-fatal anomaly.
    Warn,
    /// Operator must act.
    Error,
}

/// One entry on the per-node log ring.
///
/// `seq` is a monotonic per-runtime counter the loop stamps
/// onto every record — the Deck SDK's log-tail stream uses
/// this for dedup across snapshot polls (same pattern as
/// [`super::ice::AdminAuditRecord::seq`]).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    /// Monotonic per-runtime sequence number. Strictly
    /// increasing across the runtime's lifetime.
    pub seq: u64,
    /// Wall-clock milliseconds since `UNIX_EPOCH` when the
    /// record was published to the loop.
    pub ts_ms: u64,
    /// Severity.
    pub level: LogLevel,
    /// Daemon id the log line belongs to. `None` for log
    /// lines that aren't daemon-scoped (substrate-level
    /// messages, source-converter diagnostics).
    pub daemon_id: Option<u64>,
    /// Node id the log line originated on. The loop stamps
    /// `Some(this_node)` for locally-generated lines; remote
    /// lines (eventually replicated via the per-daemon log
    /// chain) preserve the originating node's id.
    pub node_id: Option<NodeId>,
    /// The log message body.
    pub message: String,
}

/// Input form a publisher constructs and sends through the
/// loop's handle. The loop stamps `seq` + `ts_ms` + this
/// node's id before pushing onto the ring, so publishers
/// don't need to know either.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    /// Severity.
    pub level: LogLevel,
    /// Daemon id the line belongs to, if any.
    pub daemon_id: Option<u64>,
    /// Message body.
    pub message: String,
}

impl LogLine {
    /// Convenience: build an info-level log line for `daemon`
    /// with the given message.
    pub fn info(daemon_id: Option<u64>, message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Info,
            daemon_id,
            message: message.into(),
        }
    }

    /// Convenience: build a warn-level log line for `daemon`.
    pub fn warn(daemon_id: Option<u64>, message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Warn,
            daemon_id,
            message: message.into(),
        }
    }

    /// Convenience: build an error-level log line for `daemon`.
    pub fn error(daemon_id: Option<u64>, message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Error,
            daemon_id,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_record_postcard_round_trips() {
        let record = LogRecord {
            seq: 42,
            ts_ms: 1_700_000_000_000,
            level: LogLevel::Warn,
            daemon_id: Some(7),
            node_id: Some(100),
            message: "drain timeout".into(),
        };
        let bytes = postcard::to_allocvec(&record).expect("encode");
        let decoded: LogRecord = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn log_line_helpers_set_level_and_daemon() {
        let info = LogLine::info(Some(1), "hello");
        assert_eq!(info.level, LogLevel::Info);
        assert_eq!(info.daemon_id, Some(1));
        assert_eq!(info.message, "hello");

        let warn = LogLine::warn(None, "rtt high");
        assert_eq!(warn.level, LogLevel::Warn);
        assert!(warn.daemon_id.is_none());

        let err = LogLine::error(Some(2), "crashed");
        assert_eq!(err.level, LogLevel::Error);
    }

    #[test]
    fn log_levels_order_lowest_to_highest_for_filter_thresholds() {
        // The Deck SDK's `LogFilter { min_level }` uses this
        // ordering as `record.level >= min_level`.
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }
}
