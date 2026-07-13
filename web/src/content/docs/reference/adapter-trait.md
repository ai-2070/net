# Adapter Trait

Adapters are the persistence and transport plug-in for the event bus. The bus's ingestion pipeline batches events per shard and hands the batches to an adapter; the adapter is responsible for getting them to durable storage (or to the mesh, or to another broker). The trait is small on purpose — most of the bus's complexity lives in the bus itself, leaving adapters as a focused integration point.

This page is for two audiences: people wiring up one of the shipped adapters, and people writing their own.

## The trait

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    async fn init(&mut self) -> Result<(), AdapterError>;
    async fn on_batch(&self, batch: Arc<Batch>) -> Result<(), AdapterError>;
    async fn poll_shard(&self, shard_id: u16, from_id: Option<&str>, limit: usize)
        -> Result<ShardPollResult, AdapterError>;
    async fn flush(&self) -> Result<(), AdapterError>;
    async fn shutdown(&self) -> Result<(), AdapterError>;
    fn name(&self) -> &'static str;
    async fn is_healthy(&self) -> bool { true }   // defaulted
}
```

Six required methods (plus a defaulted `is_healthy`):

- **`init`** runs once before any batches flow — open connections, create streams, run migrations.
- **`on_batch`** receives an `Arc<Batch>` of events from one shard's batch worker. The adapter persists (or forwards) every event in the batch atomically — partial batches are not allowed.
- **`poll_shard`** is the consumer side. Given a shard id, an optional `from_id` cursor, and a limit, return the next batch of events from that shard's stream.
- **`flush`** is a barrier — wait until everything previously accepted via `on_batch` is durably stored.
- **`shutdown`** signals the adapter to drain and stop. After `shutdown()` returns, `on_batch` calls must reject with `AdapterError::Shutdown`.
- **`name`** returns a stable `&'static str` for logs and metrics.
- **`is_healthy`** (defaulted to `true`) lets the bus gate on adapter health.

## Contract

Adapter implementations have to satisfy five properties:

- **Append in received order.** Within a shard, the order of events in the batch matches the order they were ingested. The adapter must preserve that.
- **Never block ingestion indefinitely.** The ingestion path runs at multi-million events per second; an adapter that backs up for too long should return `AdapterError::Backpressure` so the bus can apply its policy.
- **Fail fast on internal errors.** Non-retryable failures should return `AdapterError::Fatal`. The bus's classifier reads `is_retryable()` and `is_fatal()` to decide what to do next.
- **Be idempotent under retry.** A batch that's already been written and is retried (because the adapter returned `Transient`) must not duplicate. The shipped adapters use per-batch producer nonces; custom adapters should follow the same pattern.
- **Preserve per-shard FIFO.** Cross-shard order isn't guaranteed (and isn't expected); per-shard order is the load-bearing invariant.

The adapter should also avoid per-event allocations. Per-batch or static allocations are fine; allocating per event will show up as a throughput cliff at high ingest rates.

## Shipped adapters

### `NoopAdapter`

```rust
pub struct NoopAdapter;
```

Discards events. The default, used when no other adapter is configured. Useful for tests, benchmarks, and the "let me try the API" case.

### `NetAdapter` (feature = `"net"`)

```rust
pub struct NetAdapter { /* ... */ }

pub struct NetAdapterConfig {
    pub listen: Option<SocketAddr>,
    pub peers: Vec<SocketAddr>,
    pub keypair: Option<EntityKeypair>,
    pub psk: Option<[u8; 32]>,
    // ...
}
```

The mesh transport. Events flow over the Net protocol with end-to-end encryption, identity binding, and capability-aware routing. This is the production adapter for distributed deployments.

### `RedisAdapter` (feature = `"redis"`)

```rust
pub struct RedisAdapter { /* ... */ }

pub struct RedisAdapterConfig {
    pub url: String,
    pub stream_name: String,
    pub max_len: Option<usize>,
    pub consumer_group: Option<String>,
}
```

Bridges to Redis Streams. Useful as a transitional path for shops migrating from an existing Redis-based event pipeline.

### `JetStreamAdapter` (feature = `"jetstream"`)

```rust
pub struct JetStreamAdapter { /* ... */ }

pub struct JetStreamAdapterConfig {
    pub url: String,
    pub stream_name: String,
    pub subjects: Vec<String>,
    pub deliver_policy: DeliverPolicy,
}
```

Bridges to NATS JetStream. Same transitional role as the Redis adapter, for shops using NATS.

## Writing a custom adapter

The minimum-viable shape:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use net::adapter::Adapter;
use net::event::Batch;
use net::error::AdapterError;

pub struct MyAdapter {
    // ... your backend state ...
}

#[async_trait]
impl Adapter for MyAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        self.connect().await
            .map_err(|e| AdapterError::Fatal(e.to_string()))
    }

    async fn on_batch(&self, batch: Arc<Batch>) -> Result<(), AdapterError> {
        for event in &batch.events {
            self.write(event).await
                .map_err(|e| AdapterError::Transient(e.to_string()))?;
        }
        Ok(())
    }

    async fn poll_shard(&self, shard_id: u16, from_id: Option<&str>, limit: usize)
        -> Result<ShardPollResult, AdapterError>
    {
        let events = self.read_from(shard_id, from_id, limit).await
            .map_err(|e| AdapterError::Transient(e.to_string()))?;
        let has_more = !events.is_empty();
        Ok(ShardPollResult {
            events,
            next_id: self.next_cursor(shard_id).await,   // Option<String>
            has_more,
        })
    }

    async fn flush(&self) -> Result<(), AdapterError> {
        self.fsync().await
            .map_err(|e| AdapterError::Transient(e.to_string()))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.close().await
            .map_err(|e| AdapterError::Fatal(e.to_string()))
    }

    fn name(&self) -> &'static str {
        "my-adapter"
    }
}
```

The methods compose: initialise (`init`), ingest a batch (`on_batch`), consume a shard (`poll_shard`), wait for durability (`flush`), tear down (`shutdown`), and identify for logs/metrics (`name`). The contract makes the rest of the integration mostly mechanical.

## Idempotency and deduplication

The bus stamps each batch with a producer nonce — a per-bus identifier the adapter can use to detect duplicates on retry. The shipped adapters dedupe using these nonces plus the per-batch sequence ID. Custom adapters that target a backend without native dedup should do the same:

```rust
// On batch arrival:
if self.has_seen(batch.producer_nonce, batch.first_seq) {
    return Ok(());  // Already written; idempotent retry
}
self.write_all(&batch.events).await?;
self.record_seen(batch.producer_nonce, batch.first_seq);
```

For backends with native dedup (Redis Streams' MSGID, JetStream's `Nats-Msg-Id`), the adapter just passes the nonce through. For backends without, a small persistent map suffices.

## Polling shape

`ShardPollResult` is the consumer-side type:

```rust
pub struct ShardPollResult {
    pub events: Vec<StoredEvent>,
    pub next_id: Option<String>,   // cursor for the next poll; None if no events
    pub has_more: bool,
}
```

`events` is the page of events at this shard. `cursor` is the next cursor to pass on a follow-up call (opaque to the bus; the adapter chooses the format). `has_more` is a hint — if false, the bus's poll merger may decide to wait for the next batch rather than polling again immediately.

The bus tolerates a stale cursor by skipping ahead to the earliest available event and noting the gap in the response's metadata. Adapters with retention (RedEX with `retention_max_*`, Redis Streams with `MAXLEN`) should signal a stale cursor with an empty events list and an updated cursor — *not* an error.
