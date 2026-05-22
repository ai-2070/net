//! No-op adapter for testing and benchmarking.
//!
//! This adapter discards all events. Useful for:
//! - Benchmarking ingestion throughput without backend overhead
//! - Testing the event bus without a real backend
//! - Development and prototyping

use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::adapter::{Adapter, ShardPollResult};
use crate::error::AdapterError;
use crate::event::Batch;

/// No-op adapter that discards all events.
///
/// This adapter is useful for:
/// - Measuring pure ingestion throughput
/// - Testing without a backend
/// - Development/prototyping
#[derive(Debug, Default)]
pub struct NoopAdapter {
    /// Count of batches received (for testing).
    batches_received: AtomicU64,
    /// Count of events received (for testing).
    events_received: AtomicU64,
    /// Whether the adapter has been initialized.
    initialized: std::sync::atomic::AtomicBool,
}

impl NoopAdapter {
    /// Create a new no-op adapter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the number of batches received.
    pub fn batches_received(&self) -> u64 {
        self.batches_received.load(Ordering::Relaxed)
    }

    /// Get the number of events received.
    pub fn events_received(&self) -> u64 {
        self.events_received.load(Ordering::Relaxed)
    }

    /// Reset counters.
    pub fn reset(&self) {
        self.batches_received.store(0, Ordering::Relaxed);
        self.events_received.store(0, Ordering::Relaxed);
    }
}

#[async_trait]
impl Adapter for NoopAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        self.initialized
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    async fn on_batch(&self, batch: std::sync::Arc<Batch>) -> Result<(), AdapterError> {
        // Just count, don't store
        self.batches_received.fetch_add(1, Ordering::Relaxed);
        self.events_received
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    async fn flush(&self) -> Result<(), AdapterError> {
        // Nothing to flush
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        // Nothing to clean up
        Ok(())
    }

    async fn poll_shard(
        &self,
        _shard_id: u16,
        _from_id: Option<&str>,
        _limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        // No events stored
        Ok(ShardPollResult::empty())
    }

    fn name(&self) -> &'static str {
        "noop"
    }

    async fn is_healthy(&self) -> bool {
        self.initialized.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::InternalEvent;
    use serde_json::json;

    #[tokio::test]
    async fn test_noop_counts() {
        let mut adapter = NoopAdapter::new();
        adapter.init().await.unwrap();

        assert_eq!(adapter.batches_received(), 0);
        assert_eq!(adapter.events_received(), 0);

        let events = vec![
            InternalEvent::from_value(json!({"a": 1}), 1, 0),
            InternalEvent::from_value(json!({"a": 2}), 2, 0),
            InternalEvent::from_value(json!({"a": 3}), 3, 0),
        ];
        let batch = Batch::new(0, events, 0);

        adapter.on_batch(std::sync::Arc::new(batch)).await.unwrap();

        assert_eq!(adapter.batches_received(), 1);
        assert_eq!(adapter.events_received(), 3);

        adapter.reset();
        assert_eq!(adapter.batches_received(), 0);
        assert_eq!(adapter.events_received(), 0);
    }

    #[tokio::test]
    async fn test_noop_poll_empty() {
        let mut adapter = NoopAdapter::new();
        adapter.init().await.unwrap();

        let result = adapter.poll_shard(0, None, 100).await.unwrap();
        assert!(result.events.is_empty());
        assert!(!result.has_more);
        assert!(result.next_id.is_none());
    }
}
