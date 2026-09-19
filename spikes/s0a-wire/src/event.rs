//! `StoredEvent` — copied from `net/crates/net/src/event.rs:454`.
//!
//! Coupling cut #1 (`session.rs:16`): **move the type**. `NetSession`
//! only ever pushes and pops this through a `SegQueue`; it never reads
//! a field. The struct itself is a pure data carrier over `bytes`,
//! with no dependency on the adapter/event machinery around it, so
//! moving it into `net-wire` (and re-exporting from `crate::event`)
//! costs nothing and keeps `NetSession` non-generic. See the report
//! for the alternative that was rejected.
//!
//! `from_value` (the only `serde_json` user on the type) is left
//! behind in core — it is a convenience constructor, not part of the
//! session queue's contract.

use bytes::Bytes;

/// An event as stored/queued by an adapter.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    /// Backend-specific identifier.
    pub id: String,
    /// Raw JSON payload bytes (deferred parsing for performance).
    pub raw: Bytes,
    /// Insertion timestamp from ingestion.
    pub insertion_ts: u64,
    /// Shard this event belongs to.
    pub shard_id: u16,
    /// Application-level idempotency key as written by the producer.
    pub dedup_id: Option<String>,
}

impl StoredEvent {
    /// Create a new stored event from raw bytes.
    #[inline]
    pub fn new(id: String, raw: Bytes, insertion_ts: u64, shard_id: u16) -> Self {
        Self {
            id,
            raw,
            insertion_ts,
            shard_id,
            dedup_id: None,
        }
    }

    /// Attach an application-level dedup identifier.
    #[inline]
    #[must_use]
    pub fn with_dedup_id(mut self, dedup_id: Option<String>) -> Self {
        self.dedup_id = dedup_id;
        self
    }
}
