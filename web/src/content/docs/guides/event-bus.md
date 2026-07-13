# Using the Event Bus

The event bus is the surface you'll spend most of your time at. This guide goes past the quickstart into the patterns you'll actually need in production: cursored consumption, filtered subscriptions, multi-shard polling, backpressure, and the lifecycle invariants that keep the bus from losing data.

The API is small. You'll mostly be composing four operations — construct, ingest, poll, shutdown — into the shape your workload needs.

## Constructing a bus

A bus is built from an `EventBusConfig`. The default config gives you a single-node, in-memory bus with sensible shard counts and batch settings. You'll replace pieces of it as your deployment grows.

```rust
use std::time::Duration;
use net::{EventBus, EventBusConfig, AdapterConfig, BatchConfig, BackpressureMode};
use net::adapter::net::NetAdapterConfig;

// The Net mesh adapter speaks the Net L0 transport. The initiator
// side needs the local bind address, the peer address, a shared
// pre-shared key, and the peer's static public key.
let net_adapter = NetAdapterConfig::initiator(
    "0.0.0.0:7777".parse()?,   // bind_addr
    "10.0.0.2:7777".parse()?,  // peer_addr
    psk,                        // [u8; 32] pre-shared key
    peer_static_pubkey,        // [u8; 32] responder static pubkey
);

let config = EventBusConfig::builder()
    .num_shards(16)
    .batch(BatchConfig {
        max_size: 1_024,
        max_delay: Duration::from_millis(5),
        ..BatchConfig::default()
    })
    .backpressure_mode(BackpressureMode::DropOldest)
    .adapter(AdapterConfig::Net(Box::new(net_adapter)))
    .build()?;

let bus = EventBus::new(config).await?;
```

A few of the knobs are worth understanding:

**Shards.** Ingestion is parallelized across `num_shards` independent ring buffers. Each ingest call hashes the event onto a shard and pushes lock-free; one batch worker per shard drains into the adapter. More shards mean less contention under heavy load but more memory and more workers. The default tracks your physical core count, which is a reasonable compromise; bump it if you're seeing contention metrics climb under sustained ingest.

**Batch.** A batch worker pulls events off its shard and dispatches them to the adapter in batches. `max_size` is the ceiling on a batch and `min_size` the floor; `max_delay` caps how long the worker waits to fill one before flushing. With `adaptive` on (the default), the worker sizes each batch between those bounds based on recent ingestion velocity. The trade-off is straightforward — bigger batches amortize the adapter call, smaller batches reduce tail latency. `BatchConfig::high_throughput()` and `BatchConfig::low_latency()` ship as presets for the two ends of that trade-off.

**Backpressure.** When a shard's ring buffer fills, the backpressure mode decides what happens to new ingests. `DropOldest` (the default) evicts the oldest event in the buffer and accepts the new one. `DropNewest` drops the incoming event and silently moves on. `FailProducer` returns an error to the producer so the caller can react to the drop. `Sample { rate }` keeps one event in every `rate` and discards the rest — useful for lossy high-volume telemetry. Pick based on whether your data is more valuable at the head or the tail of the stream, and whether producers should learn about drops.

**Adapter.** Where events actually go. `NoopAdapter` is the default (in-memory, no persistence). `NetAdapter` is the mesh transport. `RedisAdapter` and `JetStreamAdapter` are available behind feature flags for shops that want to bridge to an existing broker during a transition.

## Ingestion

`bus.ingest()` is non-blocking. It hashes the event onto a shard, pushes onto the ring buffer, and returns. The hot path doesn't allocate, doesn't take a lock, and doesn't make a system call.

```rust
let event = Event::from_str(r#"{"token": "hello", "index": 0}"#)?;
bus.ingest(event)?;
```

For the highest-throughput case — pre-serialized bytes from a network buffer or a file — use `RawEvent` to skip the JSON parse and hash computation:

```rust
use net::RawEvent;

let raw = RawEvent::from_bytes(network_buffer);
bus.ingest_raw(raw)?;
```

Either form is safe to call from many threads concurrently. The shard hashing keeps producers from contending on the same buffer.

## Consumption

`bus.poll()` is the cursor-based consumer. You pass a `ConsumeRequest` describing what you want (limit, optional cursor, optional filter, optional shard set), and the bus merges results across shards in causal order.

```rust
use net::{ConsumeRequest, Filter};
use serde_json::json;

let request = ConsumeRequest::new(100)
    .filter(Filter::eq("token", json!("hello")));

let response = bus.poll(request).await?;
for event in &response.events {
    process(event);
}

// Resume from where we left off
if let Some(cursor) = response.next_id {
    let next = ConsumeRequest::new(100).from(cursor);
    let next_response = bus.poll(next).await?;
    /* ... */
}
```

The `next_id` is opaque — it encodes each shard's stream position as a base64-encoded JSON map, and it's the only thing you need to persist for at-least-once resumption. Hand it back unchanged through `.from(...)` on the next request and the bus picks up from exactly where the previous response ended. It's `None` only when the poll made no progress at all, so a resumption loop simply carries the last non-`None` cursor forward.

If you don't pass a cursor, the bus starts from the beginning of each shard's stream. If the events your cursor points past have already been trimmed by the adapter (Redis `max_stream_len`, JetStream `max_messages` / `max_age`), the next poll resumes from the earliest event still retained — there's no separate gap signal, but the `has_more`, `failed_shards`, and `stalled_shards` fields on the response surface the adapter-health conditions you'll want to alert on.

## Filtering

Filters are JSON-path equality predicates evaluated against the event payload after retrieval from the adapter, composable through the boolean operators `$and`, `$or`, and `$not`:

```rust
use net::Filter;
use serde_json::json;

let filter = Filter::and(vec![
    Filter::eq("level", json!("error")),
    Filter::eq("service", json!("api")),
]);

let request = ConsumeRequest::new(100).filter(filter);
```

The path syntax is dot-separated (`"error.stack.0"`) and supports numeric segments for array indexing. The value side of an equality is any JSON value — strings, numbers, booleans, nested objects — and is compared by structural equality.

Filters serialize as JSON, so the same filter can travel as a subscription parameter or as an nRPC `net-where` header without leaving your Rust code. For the exact grammar, see [filter-dsl reference](../reference/filter-dsl).

## Targeting specific shards

By default, `poll()` merges results across every shard. For specialized workloads — sharded daemons, partitioned consumers — you can restrict the poll to a specific subset:

```rust
let request = ConsumeRequest::new(100)
    .shards(vec![0, 2, 4, 6]);

let response = bus.poll(request).await?;
```

This is mostly useful when you've partitioned consumers by shard (the hash that puts an event on shard `N` will keep putting events from the same key on shard `N`), so a partition-N consumer can poll only its own slice without paying for the cross-shard merge.

## Lifecycle

The bus has an explicit shutdown protocol that has to be called for clean termination:

```rust
bus.shutdown().await?;
```

What this does, in order: stops accepting new ingests, waits for in-flight ingests to finish, signals each shard's drain worker to flush, waits for batch workers to dispatch the final batches to the adapter, and tears down the workers.

If you drop the bus without calling `shutdown()` — or if a panic short-circuits your shutdown path — events still in the ring buffers are lost. The bus's `Drop` impl prints a warning when this happens, and the `EventBusStats` flags the shutdown as lossy so monitoring can pick it up. Treat the warning as a bug to fix, not a routine event.

## Watching the bus

`bus.stats()` returns a snapshot of the bus's counters: events ingested, events dropped (to backpressure), batches dispatched, events dispatched, current shard counts, current backpressure pressure. Hook this into your metrics pipeline and you'll see hot spots before they become outages:

```rust
let stats = bus.stats();
println!("ingested: {}, dropped: {}, dispatched: {}",
    stats.events_ingested.load(Ordering::Relaxed),
    stats.events_dropped.load(Ordering::Relaxed),
    stats.events_dispatched.load(Ordering::Relaxed));
```

The counters are atomic and lock-free; reading them is free.

## Scaling shards on the fly

A bus can add and remove shards at runtime whenever it was built with a `ScalingPolicy` (the default). Under the hood the mapper's `evaluate_scaling()` watches per-shard utilization and returns a `ScalingDecision` — `None`, `ScaleUp(n)`, or `ScaleDown(n)`. You can either let the built-in monitor act on those decisions for you, or drive scaling yourself:

```rust
use std::sync::Arc;

let bus = Arc::new(EventBus::new(config).await?);

// Option 1: let the built-in monitor watch utilization and
// apply ScaleUp / ScaleDown within the policy's bounds.
bus.start_scaling_monitor();

// Option 2: drive it yourself. Both take a count and return the
// affected shard ids.
let added = bus.manual_scale_up(2).await?;     // Vec<u16> of new shard ids
let removed = bus.manual_scale_down(1).await?; // Vec<u16> of drained ids
```

Adding shards is cheap — new ring buffers, new workers, and the next ingest with a hash that lands on them will be served immediately. Removing shards is more involved: the bus marks each shard draining, waits for its drain worker to clear the buffer and dispatch the final batch, and only then unregisters it (`manual_scale_down` returns the ids that fully drained). The merger's view is updated atomically so a poll can't be routed to a half-removed shard.

## Common shapes

Three patterns cover most of what you'll write:

**Producer-only**, where the bus is purely an ingestion endpoint. Construct with a durable adapter (`NetAdapter` against a mesh that has RedEX enabled, or `RedisAdapter`); ingest from your application; let consumers elsewhere on the mesh poll independently.

**Consumer-only**, where the bus exists only to drive a downstream component. Construct with the same adapter, never call `ingest`, and run a polling loop against `poll()` until shutdown.

**Both**, where a single bus instance both ingests and consumes — typical for daemons that read events, transform them, and emit derived events. Most application code lives here.

The single primitive shapes all three. The bus doesn't care which role you're playing.
