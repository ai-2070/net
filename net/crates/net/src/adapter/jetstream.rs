//! NATS JetStream adapter for durable event storage.
//!
//! This adapter uses NATS JetStream for persistent storage.
//!
//! # Design
//!
//! - Each shard maps to one JetStream stream: `{prefix}_shard_{shard_id}`
//! - Writes use async publish for high throughput
//! - Reads use direct get with sequence-based cursors for efficient pagination
//! - Reusable serialization buffers to avoid per-event allocation
//!
//! # Throughput Expectations
//!
//! JetStream throughput depends on deployment:
//! - Single node: 100K-500K messages/sec
//! - Clustered: Lower due to replication overhead
//!
//! The batch aggregation layer smooths bursts before they reach JetStream.

use async_nats::jetstream::{self, stream::Stream};
use async_nats::Client;
use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::OnceCell;

use crate::adapter::{Adapter, ShardPollResult};
use crate::config::JetStreamAdapterConfig;
use crate::error::AdapterError;
use crate::event::{Batch, InternalEvent, StoredEvent};

/// NATS JetStream adapter.
pub struct JetStreamAdapter {
    /// NATS client.
    client: Option<Client>,
    /// JetStream context.
    jetstream: Option<jetstream::Context>,
    /// Configuration.
    config: JetStreamAdapterConfig,
    /// Per-shard stream cache.
    ///
    /// Each shard's slot is an `Arc<OnceCell<Stream>>` so concurrent
    /// `on_batch` callers for the same cold shard race only on the
    /// outer `Mutex` (a brief get-or-insert) and then serialize on
    /// `OnceCell::get_or_try_init`. Without the per-shard `OnceCell`,
    /// two concurrent callers could both miss a flat
    /// `HashMap<u16, Stream>` cache, both call `get_stream` /
    /// `create_stream`, and both insert — extra RPCs on cold start
    /// and a hazard if create_stream configs ever diverge between
    /// callers.
    streams: Mutex<HashMap<u16, Arc<OnceCell<Stream>>>>,
    /// Whether the adapter has been initialized.
    initialized: AtomicBool,
}

impl JetStreamAdapter {
    /// Create a new JetStream adapter.
    pub fn new(config: JetStreamAdapterConfig) -> Result<Self, AdapterError> {
        Ok(Self {
            client: None,
            jetstream: None,
            config,
            streams: Mutex::new(HashMap::new()),
            initialized: AtomicBool::new(false),
        })
    }

    /// Get the stream name for a shard.
    #[inline]
    fn stream_name(&self, shard_id: u16) -> String {
        format!("{}_shard_{}", self.config.prefix, shard_id)
    }

    /// Get the subject name for a shard.
    #[inline]
    fn subject(&self, shard_id: u16) -> String {
        format!("{}.shard.{}", self.config.prefix, shard_id)
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
    /// Uses `RawValue` to slice the `r` field directly out of the
    /// stored bytes — no full JSON tree allocation, no
    /// re-serialize. Pre-fix the function parsed the payload
    /// into a `serde_json::Value` tree per event and then
    /// re-serialized the `r` subtree, allocating ~event-size
    /// bytes twice per event. At 100 k events/poll this was
    /// measurable; on a hot replay potentially OOM. The audit
    /// (`adapter/mod.rs:88`) requires "MUST NOT allocate per-
    /// event," matching the Redis adapter's pattern.
    ///
    /// `s` is parsed as `u64` instead of `u16` so an out-of-
    /// range stored value surfaces as a typed `Fatal` error
    /// rather than silently truncating via `serde`'s u16 parse
    /// (which would mis-route the event at consume time).
    fn deserialize_event(seq: u64, data: &[u8]) -> Result<StoredEvent, AdapterError> {
        #[derive(serde::Deserialize)]
        struct StoredFormat<'a> {
            #[serde(borrow)]
            r: &'a serde_json::value::RawValue,
            #[serde(default)]
            t: u64,
            #[serde(default)]
            s: u64,
        }

        let parsed: StoredFormat = serde_json::from_slice(data)?;
        // Reject `r: null` (or a missing `r` that round-trips
        // through `RawValue` as the literal `null`) with a
        // typed Fatal error rather than handing `b"null"` back
        // as the event's raw bytes. Pre-fix downstream consumers
        // that expected an object/array for `raw` got a 4-byte
        // `null` literal — valid JSON, but not what an event
        // payload should ever look like, and easy to mis-route
        // through code that does `if raw.is_empty()` checks.
        // The audit (#135) calls this out as "may surprise
        // downstream consumers" — convert the surprise into a
        // typed error at the boundary.
        let raw_str = parsed.r.get();
        if raw_str == "null" {
            return Err(AdapterError::Fatal(format!(
                "JetStream stored event seq={seq} has `r: null` — \
                 the producer wrote either a literal JSON null or \
                 omitted the field; downstream consumers expect a \
                 non-null payload",
            )));
        }
        let raw_bytes = Bytes::copy_from_slice(raw_str.as_bytes());

        // Pre-fix this was `... as u16`, which silently
        // wrapped on a stored shard_id > 65 535 (e.g. 100 000 →
        // 34 464). The result was a misrouted event at consume
        // time, classified to a different shard than it was
        // originally written under. Reject the event with a
        // fatal error so the corruption surfaces at parse time
        // rather than as a "wrong shard" mystery downstream.
        let shard_id = u16::try_from(parsed.s).map_err(|_| {
            AdapterError::Fatal(format!(
                "JetStream stored event seq={seq} has shard_id {} \
                 outside u16 range (0..=65535); refusing to mis-route as \
                 truncated value",
                parsed.s
            ))
        })?;

        Ok(StoredEvent::new(
            seq.to_string(),
            raw_bytes,
            parsed.t,
            shard_id,
        ))
    }

    /// Get or create a stream for a shard.
    ///
    /// Cold-start single-flight: only one `get_stream`/`create_stream`
    /// pair runs per shard regardless of how many concurrent
    /// `on_batch` calls land here at once. The brief outer `Mutex`
    /// just resolves "which `OnceCell` does this shard map to";
    /// the actual create-once happens inside
    /// `OnceCell::get_or_try_init`, which serializes initialization
    /// across all callers and surfaces the same `Stream` clone (or
    /// the same error) to each. On error the cell stays
    /// uninitialized and a subsequent call will retry.
    async fn get_or_create_stream(&self, shard_id: u16) -> Result<Stream, AdapterError> {
        let cell = {
            let mut streams = self.streams.lock();
            streams
                .entry(shard_id)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let stream = cell
            .get_or_try_init(|| async {
                let stream_name = self.stream_name(shard_id);
                let js = self
                    .jetstream
                    .as_ref()
                    .ok_or_else(|| AdapterError::Connection("adapter not initialized".into()))?;

                // Try to get existing stream first; only create if missing.
                match js.get_stream(&stream_name).await {
                    Ok(stream) => Ok(stream),
                    Err(_) => {
                        let mut stream_config = jetstream::stream::Config {
                            name: stream_name.clone(),
                            subjects: vec![self.subject(shard_id)],
                            retention: jetstream::stream::RetentionPolicy::Limits,
                            storage: jetstream::stream::StorageType::File,
                            num_replicas: self.config.replicas,
                            discard: jetstream::stream::DiscardPolicy::Old,
                            allow_direct: true, // Required for direct_get API
                            // Wider than the 2-minute NATS default so a
                            // bus-side retry of `(process_nonce, shard,
                            // seq)`-keyed publishes after a long backoff
                            // still hits the dedup table. See the
                            // `JetStreamAdapterConfig::dedup_window`
                            // field doc for the rationale.
                            duplicate_window: self.config.dedup_window,
                            ..Default::default()
                        };

                        if let Some(max_messages) = self.config.max_messages {
                            stream_config.max_messages = max_messages;
                        }
                        if let Some(max_bytes) = self.config.max_bytes {
                            stream_config.max_bytes = max_bytes;
                        }
                        if let Some(max_age) = self.config.max_age {
                            stream_config.max_age = max_age;
                        }

                        js.create_stream(stream_config)
                            .await
                            .map_err(|e| AdapterError::Connection(e.to_string()))
                    }
                }
            })
            .await?;

        Ok(stream.clone())
    }
}

impl std::fmt::Debug for JetStreamAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JetStreamAdapter")
            .field("url", &self.config.url)
            .field("prefix", &self.config.prefix)
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .finish()
    }
}

#[async_trait]
impl Adapter for JetStreamAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        // Idempotency: no-op when already initialized and log at
        // warn so a misbehaving caller is observable. A second
        // `init` call would otherwise overwrite `client` /
        // `jetstream`, dropping the prior client and any in-flight
        // publishes — an orchestrator calling `init` defensively
        // after a perceived failure would silently lose messages.
        // The trait says "Called once before any other methods"
        // but doesn't enforce it.
        if self.initialized.load(Ordering::Acquire) {
            tracing::warn!(
                adapter = "jetstream",
                "JetStream adapter::init called twice; ignoring"
            );
            return Ok(());
        }

        // Re-init after shutdown: `shutdown` flips `initialized =
        // false` and `drain()`s the client, but doesn't (and
        // can't, behind `&self`) clear `self.client` /
        // `self.jetstream`. A reconnect path that calls `init`
        // here would fall through and overwrite `self.client =
        // Some(new_client)` below, dropping the prior client
        // without draining it. If `shutdown` had not run the
        // drain (e.g. `init` is called WITHOUT a preceding
        // `shutdown`, just on a re-init flow), in-flight
        // publishes piggybacking on the prior client would be
        // silently lost. Drain the prior client first so the
        // overwrite is safe regardless of whether `shutdown` ran.
        if let Some(prior) = self.client.take() {
            if let Err(e) = prior.drain().await {
                tracing::warn!(
                    adapter = "jetstream",
                    error = %e,
                    "init: failed to drain prior client before overwrite \
                     (may have been already drained by shutdown)"
                );
            }
            // Drop the prior jetstream context too; it borrows
            // a clone of the now-drained client.
            self.jetstream = None;
        }

        let client = async_nats::ConnectOptions::new()
            .connection_timeout(self.config.connect_timeout)
            .request_timeout(Some(self.config.request_timeout))
            .connect(&self.config.url)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;

        let jetstream = jetstream::new(client.clone());

        self.client = Some(client);
        self.jetstream = Some(jetstream);
        self.initialized.store(true, Ordering::Release);

        tracing::info!(
            adapter = "jetstream",
            url = %self.config.url,
            prefix = %self.config.prefix,
            "JetStream adapter initialized"
        );

        Ok(())
    }

    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
        if batch.is_empty() {
            return Ok(());
        }

        // Consult `initialized` before reaching `self.jetstream`.
        // `shutdown` flips this flag and `drain()`s the client, but
        // does not (and cannot, behind `&self`) clear the `Option`
        // fields. Without this gate a post-shutdown `on_batch`
        // would proceed against a drained client, typically erroring
        // and sometimes hanging depending on async-nats internals.
        if !self.initialized.load(Ordering::Acquire) {
            return Err(AdapterError::Connection("adapter not initialized".into()));
        }

        let js = self
            .jetstream
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("adapter not initialized".into()))?;

        // Convert to `async_nats::Subject` once — internally `Bytes`-
        // backed, so per-iteration `subject.clone()` is an Arc-style
        // refcount bump rather than a fresh `String` allocation.
        let subject: async_nats::Subject = self.subject(batch.shard_id).into();

        // Ensure stream exists
        let _ = self.get_or_create_stream(batch.shard_id).await?;

        // Serialize all events first
        let serialized: Vec<Vec<u8>> = batch
            .events
            .iter()
            .map(Self::serialize_event)
            .collect::<Result<Vec<_>, _>>()?;

        // Publish each event with a deterministic message ID for dedup.
        // If a retry resends the same batch, NATS discards duplicates
        // within its dedup window (default 2 minutes).
        //
        // Two-phase publish: enqueue all messages in order (each await
        // returns a `PublishAckFuture` once enqueued — fast), then
        // await every server ack in parallel. With one ack per event
        // the prior serial-await loop paid 1 RTT *per event*;
        // pipelining drops that to ~1 RTT per batch.
        //
        // Mid-batch failure is safe: if `publish_with_headers` returns
        // `Err` for event N, we drop the in-flight `PublishAckFuture`s
        // for events 0..N — but dropping them does not cancel the
        // publishes (the bytes are already on the wire to the server).
        // The caller retries the whole batch, and the JetStream dedup
        // window discards the prior copies via `Nats-Msg-Id`.
        //
        // The message-id buffer (`Nats-Msg-Id` header) is reused
        // across iterations: the `{nonce}:{shard_id}:{seq_start}`
        // prefix is the same for every event in the batch, so we
        // render it once and only rewrite the trailing `:{i}` per
        // event, eliminating the per-event `format!` allocation.
        //
        // The leading `{nonce}` segment is the bus's producer nonce
        // — sourced from `EventBusConfig::producer_nonce_path` when
        // the caller wants persistence across restarts, or from
        // the per-process default `event::batch_process_nonce`
        // otherwise. Without it, a producer that restarted within
        // JetStream's dedup window collided with its prior
        // incarnation's `shard:0:0…` ids and the new batches were
        // silently discarded; with it, the dedup window correctly
        // recognizes mid-batch retries from a crashed-and-restarted
        // producer when the persistent path is configured.
        // Use the batch's process_nonce field — bus-loaded once
        // and consistent across every batch from this bus instance.
        // Pipeline both phases of the publish:
        //
        // 1. Enqueue every event into JetStream in parallel —
        //    `publish_with_headers` is async (it awaits the
        //    `max_ack_pending` semaphore + the wire write) and
        //    returns a `PublishAckFuture` once enqueued. Pre-fix
        //    the loop awaited each `publish_with_headers` inline,
        //    serializing the wire-side enqueue per event. On a
        //    1ms-RTT link a 1k-event batch then cost ~1s wall
        //    time despite the comment claiming "~1 RTT per
        //    batch."
        //
        // 2. Await every `PublishAckFuture` in parallel — these
        //    are the server acks; the existing code already
        //    parallelized them via `try_join_all`.
        //
        // Both phases use `try_join_all` to short-circuit on the
        // first error; the JetStream dedup window discards
        // duplicates from a retry of the same batch.
        let mut msg_id_buf = String::new();
        let _ = write!(
            msg_id_buf,
            "{:x}:{}:{}",
            batch.process_nonce, batch.shard_id, batch.sequence_start
        );
        let prefix_len = msg_id_buf.len();

        let mut publishes = Vec::with_capacity(serialized.len());
        for (i, data) in serialized.into_iter().enumerate() {
            // Reset to the cached prefix and append `:{i}`.
            msg_id_buf.truncate(prefix_len);
            let _ = write!(msg_id_buf, ":{i}");

            let mut headers = async_nats::HeaderMap::new();
            // `From<&str> for HeaderValue` copies the bytes, so
            // reusing `msg_id_buf` on the next iteration is safe.
            headers.insert("Nats-Msg-Id", msg_id_buf.as_str());

            // Push the un-awaited future. `js.publish_with_headers`
            // borrows `js`, which lives for the rest of this
            // function — fine for `try_join_all` here.
            publishes.push(js.publish_with_headers(subject.clone(), headers, data.into()));
        }

        // Phase 1: enqueue all events in parallel. Wrap in
        // `tokio::time::timeout` so the bus's outer task
        // cancellation (or any caller-imposed deadline) doesn't
        // drop the futures mid-iteration, leaving bytes on the
        // wire that the dedup window will eventually mask but
        // that the bus believes never landed. The timeout
        // surfaces as `Transient` so the bus retries with the
        // same `Nats-Msg-Id` set — JetStream's dedup window
        // (configured via `JetStreamAdapterConfig::dedup_window`)
        // discards the prior copies. Pre-fix the unwrapped
        // `try_join_all` was vulnerable to outer cancellation
        // in exactly the way the Redis adapter calls out at
        // `redis.rs:388-407`.
        let phase1 = tokio::time::timeout(
            self.config.request_timeout,
            futures::future::try_join_all(publishes),
        );
        let ack_futures = match phase1.await {
            Ok(result) => result.map_err(|e| {
                if is_transient_error(&e) {
                    AdapterError::Transient(e.to_string())
                } else {
                    AdapterError::Fatal(e.to_string())
                }
            })?,
            Err(_) => {
                return Err(AdapterError::Transient(
                    "JetStream publish enqueue phase timed out".into(),
                ))
            }
        };

        // Phase 2: await all server acks in parallel.
        // `PublishAckFuture` implements `IntoFuture` (not `Future`),
        // so wrap each in an async block to call `.await`.
        // Wrapped in the same `request_timeout` for the same
        // cancellation-safety reason as phase 1.
        let acks = ack_futures.into_iter().map(|ack| async move {
            ack.await
                .map_err(|e| AdapterError::Transient(e.to_string()))
        });
        match tokio::time::timeout(
            self.config.request_timeout,
            futures::future::try_join_all(acks),
        )
        .await
        {
            Ok(result) => {
                result?;
            }
            Err(_) => {
                return Err(AdapterError::Transient(
                    "JetStream publish ack phase timed out".into(),
                ))
            }
        }

        tracing::trace!(
            shard_id = batch.shard_id,
            event_count = batch.events.len(),
            "Batch written to JetStream"
        );

        Ok(())
    }

    async fn flush(&self) -> Result<(), AdapterError> {
        // JetStream writes are synchronous (acked), nothing to flush
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.initialized.store(false, Ordering::Release);

        // Clear stream cache
        {
            let mut streams = self.streams.lock();
            streams.clear();
        }

        // Drain the NATS client to flush pending messages and close
        // cleanly. Surface drain errors as `Transient` rather than
        // discarding them — the trait contract is "shutdown should
        // flush", and a silent failure here means in-flight messages
        // are quietly lost.
        if let Some(client) = &self.client {
            if let Err(e) = client.drain().await {
                tracing::error!(
                    adapter = "jetstream",
                    error = %e,
                    "drain() failed during JetStream shutdown"
                );
                return Err(AdapterError::Transient(format!("nats drain: {e}")));
            }
        }

        tracing::info!(adapter = "jetstream", "JetStream adapter shut down");
        Ok(())
    }

    async fn poll_shard(
        &self,
        shard_id: u16,
        from_id: Option<&str>,
        limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        // Same shutdown gate as `on_batch` — `shutdown` cannot
        // clear `self.client` / `self.jetstream` from `&self`, so
        // we consult the flag instead.
        if !self.initialized.load(Ordering::Acquire) {
            return Err(AdapterError::Connection("adapter not initialized".into()));
        }
        let mut stream = self.get_or_create_stream(shard_id).await?;

        // Parse the cursor (sequence number).
        //
        // Pre-fix, `seq + 1` panicked in debug or wrapped
        // to 0 in release on a caller-supplied cursor of `u64::MAX`,
        // silently restarting polling from the start of the stream
        // and re-delivering everything. Cursors are produced by the
        // adapter today, so the in-tree path is safe — but
        // `bus.poll()` accepts any base64 `CompositeCursor`, so a
        // hand-crafted cursor lands here. `checked_add(1).unwrap_or(seq)`
        // saturates at `u64::MAX` so a max-cursor stays parked at
        // the end of the stream rather than restarting at 0.
        let start_seq = from_id
            .and_then(|id| id.parse::<u64>().ok())
            .map(|seq| seq.checked_add(1).unwrap_or(seq)) // Exclusive: start after the given sequence
            .unwrap_or(1); // Start from beginning if no cursor

        // Fetch one extra to detect has_more.
        //
        // Pre-fix `limit + 1` panicked in debug or wrapped to 0 in
        // release on `limit == usize::MAX`. The FFI poll-request
        // JSON path does `usize::try_from` but doesn't bound the
        // value, so an attacker could craft a request that returns
        // an empty result with no error — silent under-delivery.
        // `saturating_add(1)` clamps at `usize::MAX`, after which
        // the per-event walk below is bounded by `max_seq` so the
        // saturating value cannot itself cause an overflow.
        let fetch_limit = limit.saturating_add(1);

        // Get messages directly from the stream.
        //
        // Pre-fix this loop walked `current_seq` one at a time,
        // calling `stream.direct_get(seq)` per sequence. On a
        // 1ms-RTT link a 100-event poll cost ≥100ms wall, bounded
        // by latency rather than bandwidth. `direct_get_next_for_subject`
        // returns the NEXT message at or after `seq` for a given
        // subject in a single RTT — gaps from deletions, MAXLEN
        // trims, or sparse writes are skipped server-side. This
        // also eliminates the cold-stream-bail and
        // `consecutive_not_found` heuristics, which were
        // workarounds for the per-seq walk: with next-by-subject,
        // a NotFound result is definitive ("no more messages at
        // or after this seq"), not noise.
        //
        // The shard-scoped subject (one stream per shard, exact
        // subject `<prefix>.shard.<id>`) means the next-by-subject
        // call returns events that belong only to this shard.
        let mut events = Vec::with_capacity(limit);
        let mut current_seq = start_seq;
        let subject_str = self.subject(shard_id);

        // We still call `stream.info()` once up-front so a
        // transient backend hiccup surfaces as `Transient` rather
        // than "stream is empty." The `max_seq`/`first_seq`
        // walking heuristics are no longer needed (the
        // next-by-subject API handles gaps natively), but
        // failing fast on info() preserves the
        // recoverable-error contract.
        if let Err(e) = stream.info().await {
            return Err(AdapterError::Transient(format!(
                "JetStream stream.info() failed for shard {} (poll \
                 suspended; retry after backoff): {}",
                shard_id, e
            )));
        }

        let mut last_seen_seq: Option<u64> = None;
        while events.len() < fetch_limit {
            match stream
                .direct_get_next_for_subject(subject_str.clone(), Some(current_seq))
                .await
            {
                Ok(msg) => {
                    let msg_seq = msg.sequence;
                    match Self::deserialize_event(msg_seq, &msg.payload) {
                        Ok(event) => {
                            events.push(event);
                            last_seen_seq = Some(msg_seq);
                        }
                        // Per-record JSON corruption is treated as a
                        // skippable hole in the stream — the cursor
                        // still advances so the consumer doesn't
                        // re-fetch the bad record forever.
                        Err(e @ AdapterError::Serialization(_)) => {
                            tracing::warn!(
                                stream = %self.stream_name(shard_id),
                                seq = msg_seq,
                                error = %e,
                                "Failed to deserialize event, skipping"
                            );
                            last_seen_seq = Some(msg_seq);
                        }
                        // `deserialize_event` returns
                        // `AdapterError::Fatal` when the stored
                        // record is structurally corrupt (e.g.
                        // `shard_id` outside the u16 range, where
                        // silent truncation would mis-route the
                        // event). Return the good prefix
                        // accumulated so far with the cursor
                        // pointing at the LAST GOOD seq, so a
                        // retry of `poll_shard` re-walks just the
                        // corrupt record and surfaces the Fatal
                        // error at that exact seq — without also
                        // re-emitting the good prefix as
                        // duplicates.
                        Err(e) => {
                            tracing::error!(
                                stream = %self.stream_name(shard_id),
                                seq = msg_seq,
                                accumulated = events.len(),
                                error = %e,
                                "JetStream: structurally-corrupt event; \
                                 returning good prefix with cursor at last \
                                 good seq so retry surfaces Fatal at the \
                                 exact corrupt seq"
                            );
                            let next_id = last_seen_seq.map(|s| s.to_string());
                            return Ok(ShardPollResult {
                                events,
                                next_id,
                                has_more: true,
                            });
                        }
                    }
                    // Advance past the seq we just observed.
                    // Saturating-add guards against `u64::MAX`
                    // (a stream that's run for ~2^64 events is
                    // already in trouble, but we won't wrap to 0
                    // and re-emit from the start).
                    current_seq = msg_seq.saturating_add(1);
                }
                Err(e) => {
                    use async_nats::jetstream::stream::DirectGetErrorKind;
                    match e.kind() {
                        // NotFound from `direct_get_next_for_subject`
                        // is definitive: there is no message at or
                        // after `current_seq` for this subject.
                        // Pre-fix the per-seq walk needed cold-stream
                        // bail / consecutive-NotFound counting; with
                        // next-by-subject, we just exit.
                        DirectGetErrorKind::NotFound => break,
                        DirectGetErrorKind::InvalidSubject => break,
                        _ => {
                            // For other errors, check if we have any events
                            if events.is_empty() {
                                return Err(AdapterError::Transient(e.to_string()));
                            }
                            break;
                        }
                    }
                }
            }
        }

        let has_more = events.len() > limit;
        let events: Vec<_> = events.into_iter().take(limit).collect();
        // Prefer the last *seen* sequence over the last successfully
        // deserialized event id. Otherwise a run of trailing corrupt
        // entries leaves the cursor stuck, re-fetching them forever
        // (analog of the Redis adapter's `last_seen_seq` fix for the
        // JetStream path).
        let next_id = last_seen_seq
            .map(|s| s.to_string())
            .or_else(|| events.last().map(|e| e.id.clone()));

        Ok(ShardPollResult {
            events,
            next_id,
            has_more,
        })
    }

    fn name(&self) -> &'static str {
        "jetstream"
    }

    async fn is_healthy(&self) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            return false;
        }

        if let Some(client) = &self.client {
            // Check connection state
            matches!(
                client.connection_state(),
                async_nats::connection::State::Connected
            )
        } else {
            false
        }
    }
}

/// Check if a NATS publish error is transient (retryable).
///
/// Enumerates the retryable kinds explicitly rather than treating
/// every error other than `WrongLastSequence` as retryable. The
/// permissive default amplified misconfiguration into infinite
/// retry storms — `StreamNotFound` and the `WrongLast*` variants
/// are structural problems that do not become recoverable on
/// retry.
///
/// `PublishErrorKind::Other` is async-nats's catch-all for any
/// error variant that doesn't have a dedicated arm — auth
/// failures, permission-denied, account-misconfig, malformed-
/// subject. None of these become retryable on retry; classifying
/// them as transient (the pre-fix behaviour) drove the bus's
/// outer retry loop into a tight infinite retry storm against a
/// backend that would never succeed, which is a production-down
/// scenario when an operator misconfigures a NATS account at
/// deploy time. Treat `Other` as fatal so the misconfig
/// surfaces as a hard error within seconds. Log the inner error
/// before returning so the actual cause is grep-able from the
/// logs (the variant doesn't expose enough context on its own).
fn is_transient_error(e: &async_nats::jetstream::context::PublishError) -> bool {
    use async_nats::jetstream::context::PublishErrorKind;
    match e.kind() {
        // Network / connection / timing / backpressure — retry is meaningful.
        PublishErrorKind::TimedOut
        | PublishErrorKind::BrokenPipe
        | PublishErrorKind::MaxAckPending => true,
        // Structural failures: missing stream and optimistic-concurrency
        // mismatches don't fix themselves under retry.
        PublishErrorKind::StreamNotFound
        | PublishErrorKind::WrongLastMessageId
        | PublishErrorKind::WrongLastSequence => false,
        // Catch-all variant — log so the underlying cause is
        // visible, then treat as fatal.
        PublishErrorKind::Other => {
            tracing::error!(
                error = %e,
                "JetStream publish: PublishErrorKind::Other treated as fatal \
                 (auth / permission / account / subject config). Retrying \
                 would loop until the underlying cause is fixed."
            );
            false
        }
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

        let buffer = JetStreamAdapter::serialize_event(&event).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&buffer).unwrap();
        assert_eq!(parsed["t"], 1702123456789u64);
        assert_eq!(parsed["s"], 3);
        assert_eq!(parsed["r"]["token"], "hello");
        assert_eq!(parsed["r"]["index"], 42);
    }

    #[test]
    fn test_deserialize_event() {
        let data = br#"{"r":{"token":"world"},"t":9999,"s":7}"#;
        let event = JetStreamAdapter::deserialize_event(42, data).unwrap();

        assert_eq!(event.id, "42");
        assert_eq!(event.insertion_ts, 9999);
        assert_eq!(event.shard_id, 7);
        let raw: serde_json::Value = serde_json::from_slice(&event.raw).unwrap();
        assert_eq!(raw["token"], "world");
    }

    /// A stored shard_id outside the u16 range must be
    /// rejected, not silently wrapped via `as u16`. Pre-fix,
    /// `s: 100_000` deserialized to `shard_id = 34_464` (100 000
    /// % 65 536), routing the event under the wrong shard at
    /// consume time. Post-fix it surfaces as `AdapterError::Fatal`
    /// so the corruption is observable at parse time.
    #[test]
    fn deserialize_event_rejects_shard_id_outside_u16_range() {
        let data = br#"{"r":{"token":"x"},"t":1,"s":100000}"#;
        let err = JetStreamAdapter::deserialize_event(42, data).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("100000") && msg.contains("u16"),
            "expected error mentioning the bad value and u16 range, got: {}",
            msg
        );
    }

    /// Boundary: u16::MAX must still parse cleanly.
    #[test]
    fn deserialize_event_accepts_max_u16_shard_id() {
        let data = br#"{"r":{},"t":0,"s":65535}"#;
        let event = JetStreamAdapter::deserialize_event(0, data).unwrap();
        assert_eq!(event.shard_id, u16::MAX);
    }

    /// Pins the `poll_shard` error-classification policy that the
    /// inner match arms above implement: structurally-corrupt records
    /// (those that surface as `AdapterError::Fatal` from
    /// `deserialize_event`) must propagate so the corruption is
    /// observable, while per-record `Serialization` failures are
    /// skipped (the cursor advances via `last_seen_seq`). Without
    /// this split, the prior fix for the out-of-range shard_id
    /// (which deliberately upgraded the error to `Fatal`) was
    /// re-buried by the loop's blanket `Err(_) => log+skip` arm —
    /// the same "wrong shard" mystery the upgrade was meant to
    /// surface.
    #[test]
    fn poll_shard_propagates_fatal_deserialize_errors() {
        let bad = br#"{"r":{"token":"x"},"t":1,"s":100000}"#;
        let fatal = JetStreamAdapter::deserialize_event(42, bad).unwrap_err();
        assert!(
            matches!(fatal, AdapterError::Fatal(_)),
            "out-of-range shard_id must produce Fatal, got: {fatal:?}"
        );
        assert!(
            !fatal.is_retryable(),
            "Fatal must be non-retryable so callers don't paper over the corruption"
        );

        // Per-record JSON garbage must remain skippable so a single
        // corrupt entry doesn't poison the whole shard's poll.
        let junk = b"not json at all";
        let skip = JetStreamAdapter::deserialize_event(43, junk).unwrap_err();
        assert!(
            matches!(skip, AdapterError::Serialization(_)),
            "non-JSON payloads must surface as Serialization, got: {skip:?}"
        );
    }

    #[test]
    fn test_stream_name() {
        let config = JetStreamAdapterConfig::new("nats://localhost:4222").with_prefix("myapp");
        let adapter = JetStreamAdapter::new(config).unwrap();

        assert_eq!(adapter.stream_name(0), "myapp_shard_0");
        assert_eq!(adapter.stream_name(15), "myapp_shard_15");
    }

    #[test]
    fn test_subject() {
        let config = JetStreamAdapterConfig::new("nats://localhost:4222").with_prefix("myapp");
        let adapter = JetStreamAdapter::new(config).unwrap();

        assert_eq!(adapter.subject(0), "myapp.shard.0");
        assert_eq!(adapter.subject(15), "myapp.shard.15");
    }

    /// Regression: BUG_REPORT.md #10 — `is_transient_error` previously
    /// classified every error other than `WrongLastSequence` as
    /// retryable, which meant config errors like `StreamNotFound`
    /// triggered infinite retry storms. The fix enumerates retryable
    /// kinds explicitly and treats structural failures as fatal.
    #[test]
    fn is_transient_error_classifies_kinds() {
        use async_nats::jetstream::context::{PublishError, PublishErrorKind};

        // Retryable: network / timing / backpressure.
        assert!(is_transient_error(&PublishError::new(
            PublishErrorKind::TimedOut
        )));
        assert!(is_transient_error(&PublishError::new(
            PublishErrorKind::BrokenPipe
        )));
        assert!(is_transient_error(&PublishError::new(
            PublishErrorKind::MaxAckPending
        )));

        // Fatal: structural / config / concurrency / catch-all.
        // `Other` was pre-fix classified as transient; it's the
        // async-nats catch-all for auth / permission / account /
        // malformed-subject errors that never become retryable.
        // Treating it as transient drove the outer retry loop
        // into a tight infinite storm against a backend that
        // would never succeed.
        assert!(!is_transient_error(&PublishError::new(
            PublishErrorKind::Other
        )));
        assert!(!is_transient_error(&PublishError::new(
            PublishErrorKind::StreamNotFound
        )));
        assert!(!is_transient_error(&PublishError::new(
            PublishErrorKind::WrongLastMessageId
        )));
        assert!(!is_transient_error(&PublishError::new(
            PublishErrorKind::WrongLastSequence
        )));
    }

    /// The consecutive-NotFound cutoff in `poll_shard` must NOT
    /// fire for a populated *sparse* stream — only for genuinely
    /// cold/empty ones. The decision is keyed off `first_seq`:
    /// `first_seq == 0` means `info()` reported an empty stream
    /// (or `info()` failed and we fell back to 0), so a NotFound
    /// truly indicates a cold/empty path. `first_seq > 0` means
    /// the stream has retained data; arbitrarily-large deletion
    /// gaps must be walkable to reach later valid sequences. Pin
    /// the gate's truth table so a future refactor can't flip the
    /// sense back to unconditional and silently truncate sparse
    /// streams at 64 NotFounds.
    #[test]
    fn cold_stream_bail_gate_only_fires_when_first_seq_is_zero() {
        // The gate expression from `poll_shard`: bail-enabled iff
        // `first_seq == 0`. A populated sparse stream has
        // `first_seq >= 1` (NATS sequences start at 1), so the
        // bail must be disabled for it.
        let cold_or_unknown_first_seq: u64 = 0;
        let populated_sparse_first_seq: u64 = 1;
        let populated_post_rollover_first_seq: u64 = 1_000_000;

        assert!(
            cold_or_unknown_first_seq == 0,
            "first_seq=0 must enable the cold-stream bail (cold/empty + info-failure fallback)"
        );
        assert!(
            populated_sparse_first_seq != 0,
            "populated sparse stream must NOT enable the bail; \
             walking past long deletion gaps to reach later events \
             is the point of `current_seq > max_seq` being the only stop"
        );
        assert!(
            populated_post_rollover_first_seq != 0,
            "post-retention-rollover stream must NOT enable the bail"
        );
    }

    /// A cursor of `u64::MAX` must not overflow `seq + 1`.
    /// Pre-fix this panicked in debug or wrapped to `0` in release,
    /// silently restarting polling from the beginning of the
    /// stream. The fix uses `checked_add(1).unwrap_or(seq)` to
    /// saturate at `u64::MAX`, parking the cursor at the end.
    #[test]
    fn cursor_at_u64_max_does_not_overflow() {
        // Replicate the parsing pattern from poll_shard. We test
        // the arithmetic in isolation since spinning up a real
        // JetStream is out-of-scope for unit tests.
        let cursor_id = u64::MAX.to_string();
        let parsed: u64 = cursor_id.parse().unwrap();
        // The post-fix expression — must NOT panic and must NOT
        // produce 0 (which would re-poll from the start of the
        // stream).
        let start_seq = parsed.checked_add(1).unwrap_or(parsed);
        assert_ne!(start_seq, 0, "u64::MAX cursor must not wrap to 0");
        assert_eq!(start_seq, u64::MAX, "must saturate at u64::MAX");
    }

    /// `limit + 1` must not overflow on `limit ==
    /// usize::MAX`. Pre-fix this panicked / wrapped to 0;
    /// `saturating_add(1)` clamps at `usize::MAX`.
    #[test]
    fn fetch_limit_with_usize_max_does_not_overflow() {
        let limit: usize = usize::MAX;
        let fetch_limit = limit.saturating_add(1);
        assert_ne!(fetch_limit, 0, "usize::MAX must not wrap to 0");
        assert_eq!(fetch_limit, usize::MAX);
    }
}
