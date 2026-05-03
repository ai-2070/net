//! Redis Streams adapter for durable event storage.
//!
//! This adapter uses Redis Streams (XADD/XRANGE) for persistent storage.
//!
//! # Design
//!
//! - Each shard maps to one Redis Stream: `{prefix}:shard:{shard_id}`
//! - Writes use pipelined XADD for high throughput
//! - Reads use XRANGE with exclusive cursors for efficient pagination
//! - Reusable serialization buffers to avoid per-event allocation
//!
//! # Throughput Expectations
//!
//! Redis throughput is LOWER than ingestion throughput:
//! - Ingestion: 10M-100M events/sec (in-memory)
//! - Redis: 100K-500K events/sec (network-bound)
//!
//! The batch aggregation layer smooths bursts before they reach Redis.
//!
//! # Consumer-side dedup contract
//!
//! Redis Streams does NOT have server-side dedup. The `MULTI/EXEC`
//! `tokio::time::timeout` cancellation hazard at `on_batch` (the
//! local future is dropped on timeout but the bytes are already on
//! the wire — Redis can still execute the EXEC server-side after
//! the future is dropped, then the caller's retry runs another
//! EXEC and produces duplicate stream entries with distinct
//! server-generated `*` ids) means duplicates are an inherent
//! producer-side risk.
//!
//! To make duplicates filterable downstream, every XADD entry
//! carries a `dedup_id` field of the form
//! `"{producer_nonce}:{shard_id}:{sequence_start}:{i}"` — the same
//! string JetStream uses for `Nats-Msg-Id`. The id is:
//!
//! - **Stable across retries**: deterministic from `(shard,
//!   sequence_start, i)` plus the bus's persistent
//!   `producer_nonce`. A duplicate XADD from a retry carries the
//!   same `dedup_id`.
//! - **Stable across process restart**: when the bus is
//!   configured with `EventBusConfig::producer_nonce_path`, the
//!   nonce survives restart so post-crash retries still produce
//!   the same `dedup_id`.
//! - **Unique per logical event**: distinct events from the same
//!   producer never share an id.
//!
//! Consumers MUST treat `dedup_id` as the application-level
//! idempotency key:
//!
//! - Read a stream entry, extract `dedup_id` from its field map.
//! - If the id is in the seen-set, skip the entry.
//! - Otherwise, process the event and add the id to the set.
//!
//! The seen-set is an LRU sized to the worst-case
//! out-of-window dedup horizon the caller cares about. The
//! reference helper [`net_sdk::RedisStreamDedup`] (Rust SDK)
//! ships with a 4 096-entry default tuned for low-throughput
//! / short-window deployments; production callers must size
//! explicitly via `RedisStreamDedup::with_capacity`. As a
//! rough guideline, capacity must cover at least
//! `peak_events_per_sec × out_of_order_tolerance_seconds`:
//! 10 K events/sec with ~1 minute of tolerance needs ~600 K,
//! and the default 4 096 covers ~0.4 s at that throughput —
//! two orders of magnitude below the "minutes" horizon the
//! older documentation implied. Cross-language wrappers
//! (NAPI / PyO3) ship in the bindings.

use async_trait::async_trait;
use bytes::Bytes;
use redis::aio::ConnectionManager;
use redis::{Client, RedisError, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::adapter::{Adapter, ShardPollResult};
use crate::config::RedisAdapterConfig;
use crate::error::AdapterError;
use crate::event::{Batch, InternalEvent, StoredEvent};

/// Redis Streams adapter.
pub struct RedisAdapter {
    /// Redis client.
    client: Client,
    /// Connection manager (pooled connections).
    conn: Option<ConnectionManager>,
    /// Configuration.
    config: RedisAdapterConfig,
    /// Whether the adapter has been initialized.
    initialized: AtomicBool,
    /// Interned stream keys keyed by shard id. Avoids rebuilding
    /// `"{prefix}:shard:{n}"` on every `on_batch` / `poll_shard`.
    /// `HashMap` rather than `Vec` so sparse / hashed shard ids do not
    /// allocate placeholder entries up to `max_shard_id`.
    stream_keys: parking_lot::RwLock<HashMap<u16, Arc<str>>>,
}

impl RedisAdapter {
    /// Create a new Redis adapter.
    pub fn new(config: RedisAdapterConfig) -> Result<Self, AdapterError> {
        let client = Client::open(config.url.as_str())
            .map_err(|e| AdapterError::Connection(e.to_string()))?;

        Ok(Self {
            client,
            conn: None,
            config,
            initialized: AtomicBool::new(false),
            stream_keys: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    /// Get the stream key for a shard, populating the cache on first
    /// access for that shard id.
    #[inline]
    fn stream_key(&self, shard_id: u16) -> Arc<str> {
        // Fast path: cache hit under read lock.
        if let Some(k) = self.stream_keys.read().get(&shard_id) {
            return k.clone();
        }
        // Slow path: insert just this shard's key. No placeholder fill
        // for sparse / hashed shard ids.
        let mut cache = self.stream_keys.write();
        cache
            .entry(shard_id)
            .or_insert_with(|| {
                Arc::from(format!("{}:shard:{}", self.config.prefix, shard_id).as_str())
            })
            .clone()
    }

    /// Serialize an event for storage.
    ///
    /// Format: JSON with `raw` and `ts` fields.
    /// Since `event.raw` is already pre-serialized JSON bytes, we embed it directly
    /// using `RawValue` semantics to avoid double-serialization.
    fn serialize_event(event: &InternalEvent) -> Result<Vec<u8>, AdapterError> {
        // Build JSON manually to avoid re-parsing/re-serializing the raw bytes
        // Format: {"r":<raw_json>,"t":<ts>,"s":<shard_id>}
        let mut buf = Vec::with_capacity(event.raw.len() + 32);
        buf.extend_from_slice(b"{\"r\":");
        buf.extend_from_slice(&event.raw); // Already valid JSON
        buf.extend_from_slice(b",\"t\":");
        buf.extend_from_slice(event.insertion_ts.to_string().as_bytes());
        buf.extend_from_slice(b",\"s\":");
        buf.extend_from_slice(event.shard_id.to_string().as_bytes());
        buf.push(b'}');
        Ok(buf)
    }

    /// Deserialize a stored event.
    ///
    /// Borrows `id` so the caller can defer the owned-String
    /// allocation until success. Uses `RawValue` to slice the `r`
    /// field directly out of the stored bytes — no full JSON tree
    /// allocation, no re-serialize.
    fn deserialize_event(id: &str, data: &[u8]) -> Result<StoredEvent, AdapterError> {
        #[derive(serde::Deserialize)]
        struct StoredFormat<'a> {
            #[serde(borrow)]
            r: &'a serde_json::value::RawValue,
            #[serde(default)]
            t: u64,
            #[serde(default)]
            s: u16,
        }

        let parsed: StoredFormat = serde_json::from_slice(data)?;

        let raw_bytes = Bytes::copy_from_slice(parsed.r.get().as_bytes());

        Ok(StoredEvent::new(
            id.to_string(),
            raw_bytes,
            parsed.t,
            parsed.s,
        ))
    }

    /// Get a connection (with error handling).
    ///
    /// Pre-fix this returned the cached `ConnectionManager` whenever
    /// `self.conn` was `Some(_)`, even after `shutdown()` had run.
    /// `shutdown` only flips `initialized = false`; the
    /// `ConnectionManager` field itself stays set (we can't clear
    /// it from `&self` without interior mutability), so a
    /// post-shutdown `on_batch` / `poll_shard` would happily write
    /// to Redis after the operator believes the adapter is gone.
    /// Consult `initialized` first to refuse cleanly.
    async fn get_conn(&self) -> Result<ConnectionManager, AdapterError> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(AdapterError::Fatal("adapter not initialized".into()));
        }
        self.conn
            .clone()
            .ok_or_else(|| AdapterError::Connection("adapter not initialized".into()))
    }

    /// Parse an XRANGE response into a `ShardPollResult`.
    ///
    /// `next_id` is computed from the last *raw* entry id observed, not the
    /// last successfully-deserialized event. If every entry fails to
    /// deserialize, the cursor must still advance past them or the consumer
    /// re-fetches the same corrupt entries indefinitely.
    fn parse_xrange_response(results: Value, limit: usize, stream_key: &str) -> ShardPollResult {
        let entries = match results {
            Value::Array(entries) => entries,
            _ => return ShardPollResult::empty(),
        };

        let mut events = Vec::with_capacity(limit);
        let mut last_seen_id: Option<String> = None;

        for entry in entries.iter().take(limit) {
            let Value::Array(parts) = entry else { continue };
            if parts.len() < 2 {
                continue;
            }

            // First element is the ID. Borrow it as `&str` until we know
            // we'll keep the event — defers the owned `String` allocation
            // to the success path inside `deserialize_event`.
            let id: std::borrow::Cow<str> = match &parts[0] {
                Value::BulkString(bytes) => String::from_utf8_lossy(bytes),
                Value::SimpleString(s) => std::borrow::Cow::Borrowed(s.as_str()),
                _ => continue,
            };

            last_seen_id = Some(id.to_string());

            let Value::Array(fields) = &parts[1] else {
                continue;
            };
            // Find the "d" field. Compare against the byte literal directly
            // — no allocation for what is otherwise a constant-name probe
            // on every entry.
            let mut i = 0;
            while i + 1 < fields.len() {
                let is_data_field = match &fields[i] {
                    Value::BulkString(bytes) => bytes.as_slice() == b"d",
                    Value::SimpleString(s) => s == "d",
                    _ => false,
                };

                if is_data_field {
                    if let Value::BulkString(data) = &fields[i + 1] {
                        match Self::deserialize_event(&id, data) {
                            Ok(event) => events.push(event),
                            Err(e) => {
                                tracing::warn!(
                                    stream = %stream_key,
                                    id = %id,
                                    error = %e,
                                    "Failed to deserialize event, skipping"
                                );
                            }
                        }
                    }
                    break;
                }
                i += 2;
            }
        }

        let has_more = entries.len() > limit;
        // Always prefer the last *seen* id — `last_seen_id` is set on
        // every iterated entry regardless of deserialize outcome, so it
        // is a strict superset of `events.last().id`. Using
        // `events.last()` here would leave the cursor stuck behind any
        // *trailing* corrupt entries.
        let next_id = last_seen_id.or_else(|| events.last().map(|e| e.id.clone()));

        ShardPollResult {
            events,
            next_id,
            has_more,
        }
    }
}

impl std::fmt::Debug for RedisAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisAdapter")
            .field("url", &self.config.url)
            .field("prefix", &self.config.prefix)
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .finish()
    }
}

#[async_trait]
impl Adapter for RedisAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        // Idempotency. Same hazard pattern as
        // `JetStreamAdapter::init` — a second `init` would have
        // dropped the prior connection-manager (and any in-flight
        // publishes piggybacking on it) when assigning
        // `self.conn = Some(conn)`. Now we no-op when already
        // initialized and log at warn so a misbehaving caller is
        // observable.
        if self.initialized.load(Ordering::Acquire) {
            tracing::warn!(
                adapter = "redis",
                "Redis adapter::init called twice; ignoring"
            );
            return Ok(());
        }

        let conn = self
            .client
            .get_connection_manager()
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;

        // Test the connection
        let mut test_conn = conn.clone();
        redis::cmd("PING")
            .query_async::<String>(&mut test_conn)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;

        self.conn = Some(conn);
        self.initialized.store(true, Ordering::Release);

        tracing::info!(
            adapter = "redis",
            url = %self.config.url,
            prefix = %self.config.prefix,
            "Redis adapter initialized"
        );

        Ok(())
    }

    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
        if batch.is_empty() {
            return Ok(());
        }

        let mut conn = self.get_conn().await?;
        let stream_key = self.stream_key(batch.shard_id);

        // Build pipeline with serialized events
        // Serialize all events first (no await while holding data)
        let serialized: Vec<Vec<u8>> = batch
            .events
            .iter()
            .map(Self::serialize_event)
            .collect::<Result<Vec<_>, _>>()?;

        // Build atomic pipeline (MULTI/EXEC) so retries are safe —
        // either all XADDs succeed or none do.
        let mut pipe = redis::pipe();
        pipe.atomic();

        // Pre-render the dedup_id prefix for the batch — same shape
        // as JetStream's `Nats-Msg-Id`:
        // `{producer_nonce:hex}:{shard_id}:{sequence_start}`.
        // We render the prefix once and append `:{i}` per event,
        // matching the JetStream adapter's allocation strategy.
        let mut dedup_id_buf = String::new();
        use std::fmt::Write as _;
        let _ = write!(
            dedup_id_buf,
            "{:x}:{}:{}",
            batch.process_nonce, batch.shard_id, batch.sequence_start
        );
        let prefix_len = dedup_id_buf.len();

        for (i, data) in serialized.iter().enumerate() {
            // Build XADD command
            let mut cmd = redis::cmd("XADD");
            cmd.arg(&*stream_key);

            // Add MAXLEN if configured
            if let Some(max_len) = self.config.max_stream_len {
                cmd.arg("MAXLEN").arg("~").arg(max_len);
            }

            cmd.arg("*"); // Auto-generate ID
            cmd.arg("d").arg(data.as_slice()); // "d" = data field

            // Render the per-event dedup_id and add it as the
            // second field. See the module docs for the
            // consumer-side dedup contract: downstream consumers
            // dedup on this field to filter duplicates introduced
            // by the timeout-cancellation race below.
            dedup_id_buf.truncate(prefix_len);
            let _ = write!(dedup_id_buf, ":{i}");
            cmd.arg("dedup_id").arg(dedup_id_buf.as_str());

            pipe.add_command(cmd);
        }

        // Execute pipeline with command timeout.
        //
        // `tokio::time::timeout` cancels the future locally but does
        // NOT roll back bytes already on the wire. Redis can still
        // execute the EXEC after the future is dropped, so a
        // timeout-then-retry can produce duplicate XADDs (each with a
        // fresh `*` auto-id). The delivery semantics are therefore
        // at-least-once *with duplicates on retry* — not
        // exactly-once at the producer.
        //
        // **Mitigation:** every XADD above carries a `dedup_id`
        // field that's stable across retries (and across process
        // restart when `producer_nonce_path` is configured).
        // Consumers filter duplicates by keying on that field — see
        // the module docs for the contract and the
        // [`net_sdk::RedisStreamDedup`] helper for a reference
        // implementation. The Redis stream itself may still carry
        // duplicate entries on the wire; the dedup happens at
        // consume time.
        let fut = pipe.query_async::<()>(&mut conn);
        tokio::time::timeout(self.config.command_timeout, fut)
            .await
            .map_err(|_| AdapterError::Transient("Redis command timeout".into()))?
            .map_err(|e: RedisError| {
                if is_transient_error(&e) {
                    AdapterError::Transient(e.to_string())
                } else {
                    AdapterError::Fatal(e.to_string())
                }
            })?;

        tracing::trace!(
            shard_id = batch.shard_id,
            event_count = batch.events.len(),
            "Batch written to Redis"
        );

        Ok(())
    }

    async fn flush(&self) -> Result<(), AdapterError> {
        // Redis writes are synchronous in the pipeline, nothing to flush
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.initialized.store(false, Ordering::Release);
        // ConnectionManager handles cleanup automatically
        tracing::info!(adapter = "redis", "Redis adapter shut down");
        Ok(())
    }

    async fn poll_shard(
        &self,
        shard_id: u16,
        from_id: Option<&str>,
        limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        let mut conn = self.get_conn().await?;
        let stream_key = self.stream_key(shard_id);

        // Use exclusive range start to avoid re-reading the last event
        // Redis XRANGE is inclusive, so we use "(" prefix for exclusive
        let start = from_id
            .map(|id| format!("({}", id)) // Exclusive: "(1702123456789-0"
            .unwrap_or_else(|| "-".to_string()); // "-" = from beginning

        // Fetch one extra to detect has_more.
        //
        // Pre-fix `limit + 1` panicked in debug or wrapped
        // to 0 in release on `limit == usize::MAX`, silently
        // returning an empty result with no error. The FFI
        // poll-request JSON path does `usize::try_from` but doesn't
        // bound the value, so this is reachable from an
        // attacker-controlled cursor. `saturating_add(1)` clamps
        // at `usize::MAX`; Redis accepts the COUNT arg as i64 and
        // will simply cap to its own maximum, returning whatever
        // is in the stream.
        let fetch_limit = limit.saturating_add(1);

        // XRANGE key start + COUNT limit
        // Returns array of [id, [field, value, field, value, ...]]
        //
        // Wrap the XRANGE in `command_timeout` so a slow or wedged
        // Redis node doesn't block this poll indefinitely. Pre-fix
        // `poll_shard` relied entirely on the `ConnectionManager`'s
        // implicit timeout, while `on_batch` (line 408) and
        // `is_healthy` (line 516) wrap their pipelines in
        // `tokio::time::timeout(self.config.command_timeout, ...)`.
        // The inconsistent timeout policy meant that a partially-
        // healthy Redis (returns connections but stalls on commands)
        // would let `on_batch` and `is_healthy` surface a Transient
        // error within `command_timeout`, while `poll_shard` hung
        // until Redis itself replied or the connection broke. Apply
        // the same wrapper here so the timeout contract is uniform
        // across the adapter.
        let mut cmd = redis::cmd("XRANGE");
        cmd.arg(&*stream_key)
            .arg(&start)
            .arg("+") // To end
            .arg("COUNT")
            .arg(fetch_limit);
        let fut = cmd.query_async::<Value>(&mut conn);
        let results = tokio::time::timeout(self.config.command_timeout, fut)
            .await
            .map_err(|_| AdapterError::Transient("Redis XRANGE timeout".into()))?
            .map_err(|e| AdapterError::Transient(e.to_string()))?;

        Ok(Self::parse_xrange_response(results, limit, &stream_key))
    }

    fn name(&self) -> &'static str {
        "redis"
    }

    async fn is_healthy(&self) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            return false;
        }

        // Use a dedicated single-shot multiplexed connection for
        // the health check rather than the shared
        // `ConnectionManager` used by `on_batch` / `poll_shard`.
        // `tokio::time::timeout` cancels the PING future locally
        // but does NOT roll back bytes already on the wire (same
        // hazard documented for `on_batch` above). On the SHARED
        // connection a leftover PING reply could in principle
        // confuse the multiplexed correlation when followed by
        // a real command — using a fresh connection that's
        // dropped after the PING means any leftover bytes go to
        // a connection that's already being torn down.
        //
        // Also bounds the check by `command_timeout` so an
        // unhealthy backend always returns `false` within a
        // predictable window (orchestrator liveness probes
        // require a deterministic cap).
        let conn_fut = self.client.get_multiplexed_async_connection();
        let mut conn = match tokio::time::timeout(self.config.command_timeout, conn_fut).await {
            Ok(Ok(c)) => c,
            _ => return false,
        };
        let cmd = redis::cmd("PING");
        let fut = cmd.query_async::<String>(&mut conn);
        matches!(
            tokio::time::timeout(self.config.command_timeout, fut).await,
            Ok(Ok(_))
        )
        // `conn` is dropped here, so any leftover PING reply
        // ends up on a torn-down connection rather than the
        // shared multiplex.
    }
}

/// Check if a Redis error is transient (retryable).
fn is_transient_error(e: &RedisError) -> bool {
    use redis::{ErrorKind, ServerErrorKind};
    match e.kind() {
        // I/O errors are always transient
        ErrorKind::Io => true,
        // Cluster connection issues are transient
        ErrorKind::ClusterConnectionNotFound => true,
        // Typed server errors with documented retryable semantics.
        // Pre-fix the cluster-topology errors (Moved/Ask/ReadOnly/
        // ClusterDown) were classified fatal, taking the adapter
        // offline until process restart on any cluster slot move
        // or replica-failover event.
        ErrorKind::Server(ServerErrorKind::BusyLoading)
        | ErrorKind::Server(ServerErrorKind::Moved)
        | ErrorKind::Server(ServerErrorKind::Ask)
        | ErrorKind::Server(ServerErrorKind::TryAgain)
        | ErrorKind::Server(ServerErrorKind::ClusterDown)
        | ErrorKind::Server(ServerErrorKind::MasterDown)
        | ErrorKind::Server(ServerErrorKind::ReadOnly) => true,
        // Catch-all for `Server(ResponseError)` and unknown
        // extension errors that surface only via the message
        // body. Includes `NOREPLICAS` (a wait-aof timeout) which
        // doesn't have a typed kind in this redis crate version.
        ErrorKind::Server(_) | ErrorKind::Extension => {
            let msg = e.to_string().to_uppercase();
            msg.contains("LOADING")
                || msg.contains("BUSY")
                || msg.contains("TRYAGAIN")
                || msg.contains("MASTERDOWN")
                || msg.contains("MOVED")
                || msg.contains("ASK")
                || msg.contains("READONLY")
                || msg.contains("CLUSTERDOWN")
                || msg.contains("NOREPLICAS")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_serialize_event() {
        let event =
            InternalEvent::from_value(json!({"token": "hello", "index": 42}), 1702123456789, 3);

        let buffer = RedisAdapter::serialize_event(&event).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&buffer).unwrap();
        assert_eq!(parsed["t"], 1702123456789u64);
        assert_eq!(parsed["s"], 3);
        assert_eq!(parsed["r"]["token"], "hello");
        assert_eq!(parsed["r"]["index"], 42);
    }

    #[test]
    fn test_deserialize_event() {
        let data = br#"{"r":{"token":"world"},"t":9999,"s":7}"#;
        let event = RedisAdapter::deserialize_event("1702123456789-0", data).unwrap();

        assert_eq!(event.id, "1702123456789-0");
        assert_eq!(event.insertion_ts, 9999);
        assert_eq!(event.shard_id, 7);
        let raw: serde_json::Value = serde_json::from_slice(&event.raw).unwrap();
        assert_eq!(raw["token"], "world");
    }

    #[test]
    fn test_stream_key() {
        let config = RedisAdapterConfig::new("redis://localhost:6379").with_prefix("myapp");
        let adapter = RedisAdapter::new(config).unwrap();

        assert_eq!(&*adapter.stream_key(0), "myapp:shard:0");
        assert_eq!(&*adapter.stream_key(15), "myapp:shard:15");
        // Repeat access should hit the interned cache rather than
        // re-running `format!`.
        assert_eq!(&*adapter.stream_key(0), "myapp:shard:0");
    }

    /// Regression: BUG_REPORT.md #13 — sparse shard ids must not
    /// allocate placeholder entries up to `max_shard_id`. Cold-accessing
    /// shard 65535 with the previous `Vec`-keyed-by-index cache
    /// allocated 65536 entries; a `HashMap` cache stores only what is
    /// touched.
    #[test]
    fn test_stream_key_sparse_shard_ids() {
        let config = RedisAdapterConfig::new("redis://localhost:6379").with_prefix("myapp");
        let adapter = RedisAdapter::new(config).unwrap();

        assert_eq!(&*adapter.stream_key(65535), "myapp:shard:65535");
        assert_eq!(&*adapter.stream_key(7), "myapp:shard:7");

        // Only the two shards we actually touched should be in the cache.
        assert_eq!(adapter.stream_keys.read().len(), 2);
    }

    /// Build a synthetic XRANGE entry: `[id, ["d", payload_bytes]]`.
    fn xrange_entry(id: &str, payload: &[u8]) -> Value {
        Value::Array(vec![
            Value::BulkString(id.as_bytes().to_vec()),
            Value::Array(vec![
                Value::BulkString(b"d".to_vec()),
                Value::BulkString(payload.to_vec()),
            ]),
        ])
    }

    /// Regression: BUG_REPORT.md #4 — when every XRANGE entry fails to
    /// deserialize, `parse_xrange_response` previously returned
    /// `next_id == None`, which wedged the consumer on the same start
    /// position forever. The fix advances `next_id` from the last raw
    /// entry id observed, not from the last successfully-deserialized
    /// event.
    #[test]
    fn test_poll_shard_advances_cursor_on_all_corrupt_entries() {
        // Every payload is malformed JSON — every deserialize will fail.
        let response = Value::Array(vec![
            xrange_entry("1-0", b"not json"),
            xrange_entry("2-0", b"{also not"),
            xrange_entry("3-0", b"][broken"),
        ]);

        let result = RedisAdapter::parse_xrange_response(response, 10, "myapp:shard:0");

        // No events deserialized successfully.
        assert!(
            result.events.is_empty(),
            "all corrupt entries should be skipped"
        );
        // But the cursor MUST advance, otherwise the consumer will
        // re-fetch the same corrupt range forever.
        assert_eq!(
            result.next_id.as_deref(),
            Some("3-0"),
            "next_id must advance to the last raw entry id, not None"
        );
    }

    /// Regression: mixed-success path. Some entries deserialize, some
    /// don't — `next_id` should still come from the last *seen* id even
    /// if the final entry was corrupt.
    #[test]
    fn test_poll_shard_advances_past_trailing_corrupt_entries() {
        let good = br#"{"r":{"k":"v"},"t":1,"s":0}"#;
        let response = Value::Array(vec![
            xrange_entry("1-0", good),
            xrange_entry("2-0", b"corrupt"),
            xrange_entry("3-0", b"also corrupt"),
        ]);

        let result = RedisAdapter::parse_xrange_response(response, 10, "myapp:shard:0");

        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].id, "1-0");
        // Cursor must advance past the trailing corrupt entries, not
        // just to the last successful event ("1-0").
        assert_eq!(result.next_id.as_deref(), Some("3-0"));
    }

    /// Sanity: empty XRANGE result returns an empty poll result with no
    /// cursor — the caller should not advance.
    #[test]
    fn test_poll_shard_empty_response_has_no_cursor() {
        let result = RedisAdapter::parse_xrange_response(Value::Array(vec![]), 10, "myapp:shard:0");
        assert!(result.events.is_empty());
        assert!(result.next_id.is_none());
        assert!(!result.has_more);
    }

    /// Pin: Redis Cluster topology errors (`MOVED`, `ASK`,
    /// `READONLY`, `CLUSTERDOWN`, etc.) must be classified as
    /// transient. Pre-fix only `LOADING | BUSY | TRYAGAIN |
    /// MASTERDOWN` substrings matched — every cluster failover
    /// took the adapter offline until process restart.
    #[test]
    fn is_transient_error_recognizes_cluster_recoverables() {
        use redis::{ErrorKind, ServerErrorKind};

        // Typed server errors — the production path. Cluster-
        // topology errors map to specific `ServerErrorKind`
        // variants and must classify as transient.
        let typed_transient: &[(ErrorKind, &str)] = &[
            (ErrorKind::Server(ServerErrorKind::Moved), "MOVED redirect"),
            (ErrorKind::Server(ServerErrorKind::Ask), "ASK redirect"),
            (
                ErrorKind::Server(ServerErrorKind::ClusterDown),
                "cluster down",
            ),
            (
                ErrorKind::Server(ServerErrorKind::MasterDown),
                "master down",
            ),
            (
                ErrorKind::Server(ServerErrorKind::ReadOnly),
                "read-only replica",
            ),
            (ErrorKind::Server(ServerErrorKind::BusyLoading), "loading"),
            (ErrorKind::Server(ServerErrorKind::TryAgain), "try again"),
            (ErrorKind::Io, "I/O error"),
            (
                ErrorKind::ClusterConnectionNotFound,
                "no cluster connection",
            ),
        ];
        for (kind, label) in typed_transient {
            let err = RedisError::from((*kind, "test"));
            assert!(
                is_transient_error(&err),
                "{} ({:?}) must classify as transient",
                label,
                kind,
            );
        }

        // Untyped extension errors — message-substring branch.
        // `NOREPLICAS` doesn't have a typed kind in this redis
        // crate version, so it surfaces as Extension.
        let extension_transient: &[&str] = &["NOREPLICAS Not enough good replicas to write"];
        for msg in extension_transient {
            let err = RedisError::from((ErrorKind::Extension, "test", msg.to_string()));
            assert!(
                is_transient_error(&err),
                "extension `{}` must classify as transient",
                msg,
            );
        }

        // Genuinely fatal — must remain non-transient.
        let fatal: &[ErrorKind] = &[
            ErrorKind::AuthenticationFailed,
            ErrorKind::UnexpectedReturnType,
            ErrorKind::InvalidClientConfig,
            ErrorKind::Client,
            ErrorKind::Server(ServerErrorKind::ExecAbort),
            ErrorKind::Server(ServerErrorKind::NoScript),
            ErrorKind::Server(ServerErrorKind::CrossSlot),
            ErrorKind::Server(ServerErrorKind::NoPerm),
        ];
        for kind in fatal {
            let err = RedisError::from((*kind, "test"));
            assert!(
                !is_transient_error(&err),
                "{:?} must classify as fatal (non-transient)",
                kind,
            );
        }
    }
}
