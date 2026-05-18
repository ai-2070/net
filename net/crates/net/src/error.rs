//! Error types for the Net event bus.

use thiserror::Error;

/// Errors that can occur during event ingestion.
#[derive(Debug, Error)]
pub enum IngestionError {
    /// Ring buffer is full and backpressure policy rejected the event.
    #[error("backpressure: ring buffer full")]
    Backpressure,

    /// Event was dropped due to sampling/decimation policy.
    #[error("event dropped due to sampling")]
    Sampled,

    /// Hashed shard id is not in the routing table (e.g. a concurrent
    /// scale-down removed it, or the shard is still provisioning).
    /// Previously collapsed into `Backpressure`, which made callers
    /// apply the wrong remediation (back-off-and-retry on a routing
    /// miss is futile until the topology stabilizes). Distinct from
    /// `Backpressure` so callers can distinguish "buffer full" from
    /// "no destination".
    #[error("event has no routable shard")]
    Unrouted,

    /// The event bus has been shut down.
    #[error("event bus is shutting down")]
    ShuttingDown,

    /// Serialization failed. Wraps the underlying `serde_json::Error` so
    /// callers can read the category, line, and column via `source()`.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Errors that can occur in adapter operations.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// Transient error - operation can be retried.
    #[error("transient error: {0}")]
    Transient(String),

    /// Fatal error - adapter is in an unrecoverable state.
    #[error("fatal error: {0}")]
    Fatal(String),

    /// Backend cannot accept more data - apply backpressure.
    #[error("backend backpressure")]
    Backpressure,

    /// Connection error.
    #[error("connection error: {0}")]
    Connection(String),

    /// The adapter has been shut down. Distinct from `Connection`
    /// so callers (and the bus's retry classifier) can tell a "we
    /// asked this adapter to stop" reject from a transport failure;
    /// pre-fix every post-shutdown `on_batch` returned `Connection`,
    /// which classified as non-retryable and silently dropped the
    /// batch instead of either re-routing or surfacing the shutdown
    /// as a distinct state to the caller.
    #[error("adapter is shut down")]
    Shutdown,

    /// Serialization/deserialization error. Wraps the underlying
    /// `serde_json::Error` so callers can read the category, line, and
    /// column via `source()`.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl AdapterError {
    /// Returns true if this error is retryable.
    #[inline]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::Backpressure)
    }

    /// Returns true if this error is fatal.
    #[inline]
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Fatal(_))
    }

    /// Returns true if this error means the adapter has been shut
    /// down. Callers can react to this without scraping the
    /// `Connection` message string.
    #[inline]
    pub fn is_shutdown(&self) -> bool {
        matches!(self, Self::Shutdown)
    }
}

/// Errors that can occur during event consumption/polling.
#[derive(Debug, Error)]
pub enum ConsumerError {
    /// Adapter error during polling.
    #[error("adapter error: {0}")]
    Adapter(#[from] AdapterError),

    /// Invalid cursor format.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// Invalid filter specification.
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
}

/// Result type alias for ingestion operations.
pub type IngestionResult<T> = Result<T, IngestionError>;

/// Result type alias for adapter operations.
pub type AdapterResult<T> = Result<T, AdapterError>;

/// Result type alias for consumer operations.
pub type ConsumerResult<T> = Result<T, ConsumerError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_serde_error() -> serde_json::Error {
        serde_json::from_str::<serde_json::Value>("not json").unwrap_err()
    }

    #[test]
    fn test_adapter_error_is_retryable() {
        assert!(AdapterError::Transient("temp".into()).is_retryable());
        assert!(AdapterError::Backpressure.is_retryable());
        assert!(!AdapterError::Fatal("dead".into()).is_retryable());
        assert!(!AdapterError::Connection("refused".into()).is_retryable());
        assert!(!AdapterError::Shutdown.is_retryable());
        assert!(!AdapterError::Serialization(make_serde_error()).is_retryable());
    }

    /// `Shutdown` is its own filterable category — distinct from
    /// generic `Connection` errors so observability tools can tell
    /// "sending to a stopped adapter" from "transport failure".
    #[test]
    fn test_adapter_error_is_shutdown_only_for_shutdown() {
        assert!(AdapterError::Shutdown.is_shutdown());
        assert!(!AdapterError::Connection("refused".into()).is_shutdown());
        assert!(!AdapterError::Fatal("dead".into()).is_shutdown());
        assert!(!AdapterError::Transient("temp".into()).is_shutdown());
        assert!(!AdapterError::Backpressure.is_shutdown());
    }

    /// Regression: BUG_REPORT.md #18 — `Serialization` previously stored
    /// the rendered error string and broke the `source()` chain. Wrapping
    /// `serde_json::Error` directly preserves the category/line/column.
    #[test]
    fn test_serialization_error_preserves_source() {
        let err = AdapterError::Serialization(make_serde_error());
        // The Display impl still renders the inner error.
        assert!(err.to_string().contains("serialization error"));
        // And the source chain points at the original serde_json::Error.
        let source = std::error::Error::source(&err)
            .expect("Serialization variant should expose its source");
        assert!(source.is::<serde_json::Error>());
    }

    #[test]
    fn test_adapter_error_is_fatal() {
        assert!(AdapterError::Fatal("dead".into()).is_fatal());
        assert!(!AdapterError::Transient("temp".into()).is_fatal());
        assert!(!AdapterError::Backpressure.is_fatal());
        assert!(!AdapterError::Connection("refused".into()).is_fatal());
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            IngestionError::Backpressure.to_string(),
            "backpressure: ring buffer full"
        );
        assert_eq!(
            IngestionError::Sampled.to_string(),
            "event dropped due to sampling"
        );
        assert_eq!(
            IngestionError::Unrouted.to_string(),
            "event has no routable shard"
        );
        assert_eq!(
            IngestionError::ShuttingDown.to_string(),
            "event bus is shutting down"
        );
        assert_eq!(
            AdapterError::Transient("timeout".into()).to_string(),
            "transient error: timeout"
        );
        assert_eq!(
            AdapterError::Fatal("crash".into()).to_string(),
            "fatal error: crash"
        );
        assert_eq!(
            AdapterError::Backpressure.to_string(),
            "backend backpressure"
        );
    }

    #[test]
    fn test_connection_error_not_retryable() {
        // Connection errors cover both transient failures ("send failed") and
        // permanent ones ("adapter not initialized"). Since we can't distinguish
        // them at the type level, Connection is conservatively non-retryable.
        // `bus::dispatch_batch` honors `is_retryable()` and skips the retry
        // loop when this returns false, so a Connection error drops the
        // batch immediately rather than burning the retry budget.
        assert!(!AdapterError::Connection("refused".into()).is_retryable());
        assert!(!AdapterError::Connection("adapter not initialized".into()).is_retryable());
    }

    #[test]
    fn test_consumer_error_from_adapter() {
        let adapter_err = AdapterError::Connection("refused".into());
        let consumer_err: ConsumerError = adapter_err.into();
        assert!(matches!(consumer_err, ConsumerError::Adapter(_)));
    }
}
