# EventBus API

This page is the reference for the core event-bus surface — the types you'll touch when constructing, ingesting, polling, and shutting down a bus. The shapes here are the Rust API; the bindings mirror them with language-native conventions.

## `EventBus`

The bus is the single handle for ingestion and consumption.

```rust
pub struct EventBus { /* ... */ }

impl EventBus {
    pub async fn new(config: EventBusConfig) -> Result<Self, AdapterError>;

    // On success both return (shard_id, insertion_ts).
    pub fn ingest(&self, event: Event) -> IngestionResult<(u16, u64)>;
    pub fn ingest_raw(&self, event: RawEvent) -> IngestionResult<(u16, u64)>;

    pub async fn poll(&self, request: ConsumeRequest) -> ConsumerResult<ConsumeResponse>;

    pub async fn flush(&self) -> Result<(), AdapterError>;
    pub async fn shutdown(self) -> Result<(), AdapterError>;
    pub async fn shutdown_via_ref(&self) -> Result<(), AdapterError>;

    pub fn stats(&self) -> &EventBusStats;

    pub async fn manual_scale_up(&self, count: u16) -> Result<Vec<u16>, AdapterError>;
    pub async fn manual_scale_down(&self, count: u16) -> Result<Vec<u16>, AdapterError>;
}
```

Notes:

- `ingest` is non-blocking. It hashes the event to a shard, pushes onto the shard's ring buffer, and returns `(shard_id, insertion_ts)`. Failure modes are documented under `IngestionError` below. (There is no `add_shards`/`remove_shards`/`suggest_scaling`; the manual scaling methods are `manual_scale_up`/`manual_scale_down`, and a `ScalingDecision` is produced by the shard mapper's `evaluate_scaling`, not the bus.)
- `poll` merges results across shards in causal order. Pass a `from(cursor)` on the request to resume from a previous response's cursor.
- `flush` waits for every queued event to reach the adapter. Useful as a barrier in tests; not typically called in production code.
- `shutdown` consumes the bus; `shutdown_via_ref` is the non-consuming variant for callers that hold the bus behind a shared reference. Both drain in-flight ingests, flush, and tear down workers.

## `EventBusConfig`

```rust
pub struct EventBusConfig {
    pub num_shards: u16,
    pub ring_buffer_capacity: usize,
    pub batch: BatchConfig,
    pub backpressure_mode: BackpressureMode,
    pub adapter: AdapterConfig,
    pub scaling: Option<ScalingPolicy>,
    pub producer_nonce_path: Option<PathBuf>,
}

impl EventBusConfig {
    pub fn builder() -> EventBusConfigBuilder;
    pub fn default() -> Self;  // single-node, NoopAdapter
}
```

The builder pattern is the conventional construction path:

```rust
EventBusConfig::builder()
    .num_shards(16)
    .ring_buffer_capacity(4096)
    .batch(BatchConfig::default().max_events(1024).max_delay_ms(5))
    .backpressure_mode(BackpressureMode::DropOldest)
    .adapter(AdapterConfig::net().listen("0.0.0.0:7777").peer("10.0.0.2:7777"))
    .scaling(ScalingPolicy::default())
    .build()?
```

### `BackpressureMode`

| Variant           | Behavior when a shard's ring buffer is full |
|-------------------|---------------------------------------------|
| `DropNewest`      | Drop the incoming event (the one being inserted); the ring is unchanged. |
| `DropOldest`      | Evict the oldest event in the ring; accept the new one. |
| `FailProducer`    | Return an error to the producer (`IngestionError::Backpressure`). |
| `Sample { rate }` | Keep 1 event in every `rate`; drop the rest. |

There is no `Block` variant — `FailProducer` (not `DropNewest`) is the one that surfaces an error to the producer.

### `AdapterConfig`

```rust
pub enum AdapterConfig {
    Noop,
    Net(NetAdapterConfig),
    Redis(RedisAdapterConfig),       // feature = "redis"
    JetStream(JetStreamAdapterConfig), // feature = "jetstream"
}
```

`Noop` is the default. `Net` enables the mesh transport. `Redis` and `JetStream` are external-broker bridges, available behind feature flags.

## `Event` and `RawEvent`

```rust
pub struct Event(pub serde_json::Value);

impl Event {
    pub fn new(value: JsonValue) -> Self;
    pub fn from_str(s: &str) -> Result<Self, serde_json::Error>;
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error>;
    pub fn into_inner(self) -> JsonValue;
    pub fn as_value(&self) -> &JsonValue;
    pub fn into_raw(self) -> RawEvent;
}

pub struct RawEvent { /* opaque */ }

impl RawEvent {
    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self;
    pub fn from_bytes_validated(bytes: impl Into<Bytes>) -> Result<Self, serde_json::Error>;
    pub fn from_bytes_with_hash(bytes: impl Into<Bytes>, hash: u64) -> Self;
    pub fn from_value(value: JsonValue) -> Self;
}
```

`Event` is the convenient form (`serde_json::Value` wrapper). `RawEvent` is the high-throughput form (pre-serialized bytes with cached xxhash for shard selection). `RawEvent` skips JSON parsing on the hot path; use it for ingesting from network buffers or files.

## `ConsumeRequest` and `ConsumeResponse`

```rust
pub struct ConsumeRequest {
    pub limit: usize,
    pub from_id: Option<String>,
    pub filter: Option<Filter>,
    pub ordering: Ordering,
    pub shards: Option<Vec<u16>>,
}

impl ConsumeRequest {
    pub fn new(limit: usize) -> Self;
    pub fn from(self, cursor: impl Into<String>) -> Self;
    pub fn filter(self, filter: Filter) -> Self;
    pub fn ordering(self, ordering: Ordering) -> Self;
    pub fn shards(self, shards: Vec<u16>) -> Self;
}

pub struct ConsumeResponse {
    pub events: Vec<StoredEvent>,
    pub next_id: Option<String>,          // cursor for the next poll; None if no events returned
    pub has_more: bool,
    pub truncated_at_per_shard_cap: bool, // per-shard fetch was clamped by the internal cap
    pub stalled_shards: Vec<u16>,         // shards that reported more but contributed nothing
    pub failed_shards: Vec<u16>,          // shards whose adapter call errored this poll
}

pub enum Ordering {
    None,        // default — arbitrary cross-shard order (fastest; per-shard order preserved)
    InsertionTs, // sort by insertion timestamp (cross-shard merge)
}
```

`next_id` is the opaque cursor; persist it and pass it back via `ConsumeRequest::from(next_id)` on the next call for at-least-once resumption.

## `EventBusStats`

```rust
pub struct EventBusStats {
    pub events_ingested: AtomicU64,
    pub events_dropped: AtomicU64,
    pub batches_dispatched: AtomicU64,
    pub events_dispatched: AtomicU64,
    pub shutdown_was_lossy: AtomicBool,
    // ...
}
```

Counters are atomic; reads are lock-free.

The invariant after `shutdown()` completes: `events_dispatched + events_dropped == events_ingested`. Any drift indicates a bug; the stats are useful for catching one.

## Errors

```rust
pub enum IngestionError {
    Backpressure,                // Ring buffer full + policy rejected
    Sampled,                     // Dropped by sampling policy
    Unrouted,                    // No routable shard (mid-scaling)
    ShuttingDown,                // Bus is shutting down
    Serialization(serde_json::Error),
}

pub enum ConsumerError {
    Adapter(AdapterError),
    InvalidCursor(String),
    InvalidFilter(String),
}

pub enum AdapterError {
    Transient(String),           // Retry
    Fatal(String),               // Don't retry
    Backpressure,                // Backend full
    Connection(String),
    Shutdown,
    Serialization(serde_json::Error),
}
```

`AdapterError::is_retryable()` returns true for `Transient` and `Backpressure`. The bus's dispatch loop honors this — non-retryable adapter errors drop the batch immediately rather than burning the retry budget.

`AdapterError::Shutdown` is a distinct category from `Connection`, so callers can distinguish "we asked this adapter to stop" from "transport failure" without scraping the error message.
