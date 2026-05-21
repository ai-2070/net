# Using the Event Bus

The event bus is the surface you'll spend most of your time at. This guide goes past the quickstart into the patterns you'll actually need in production: cursored consumption, filtered subscriptions, multi-shard polling, backpressure, and the lifecycle invariants that keep the bus from losing data.

The API is small. You'll mostly be composing four operations — construct, ingest, poll, shutdown — into the shape your workload needs.

## Constructing a bus

A bus is built from an `EventBusConfig`. The default config gives you a single-node, in-memory bus with sensible shard counts and batch settings. You'll replace pieces of it as your deployment grows.

```rust
use net::{EventBus, EventBusConfig, AdapterConfig, BatchConfig, BackpressureMode};

let config = EventBusConfig::builder()
    .shards(16)
    .batch(BatchConfig::default()
        .max_events(1024)
        .max_delay_ms(5))
    .backpressure(BackpressureMode::DropOldest)
    .adapter(AdapterConfig::net()
        .listen("0.0.0.0:7777")
        .peer("10.0.0.2:7777"))
    .build()?;

let bus = EventBus::new(config).await?;
```

A few of the knobs are worth understanding:

**Shards.** Ingestion is parallelized across `shards` independent ring buffers. Each ingest call hashes the event onto a shard and pushes lock-free; one batch worker per shard drains into the adapter. More shards mean less contention under heavy load but more memory and more workers. The default is a reasonable compromise; bump it if you're seeing contention metrics climb under sustained ingest.

**Batch.** A batch worker pulls events off its shard and dispatches them to the adapter in batches. `max_events` caps the batch size; `max_delay_ms` caps how long the worker waits to fill one before flushing. The trade-off is straightforward — bigger batches amortize the adapter call, smaller batches reduce tail latency.

**Backpressure.** When a shard's ring buffer fills, the backpressure mode decides what happens to new ingests. `Block` waits for room (turns ingest into a back-pressured call). `DropOldest` evicts the oldest event in the buffer and accepts the new one. `DropNewest` rejects the new ingest with an error. Pick based on whether your data is more valuable at the head or the tail of the stream.

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

let request = ConsumeRequest::new(100)
    .filter(Filter::new().eq("token", "hello"));

let response = bus.poll(request).await?;
for event in response.events {
    process(&event);
}

// Resume from where we left off
let next = ConsumeRequest::new(100).from(response.cursor);
let next_response = bus.poll(next).await?;
```

The cursor is opaque — it encodes per-shard sequence positions in a base64 string, and it's the only thing you need to persist for at-least-once resumption. Hand it back unchanged on the next call and the bus picks up from exactly where the previous response ended.

If you don't pass a cursor, the bus returns from the current tail. If you pass a stale cursor (one whose events have been compacted away by the adapter), the bus skips ahead to the earliest available position and reports the gap in the response metadata.

## Filtering

Filters are JSON-path equality predicates evaluated against the event payload after retrieval from the adapter, composable through the boolean operators `$and`, `$or`, and `$not`:

```rust
use net::{Filter, FilterBuilder};
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

A bus can add and remove shards at runtime. The scaling monitor watches per-shard utilization and proposes scaling decisions; you can either auto-apply them or feed them through your own policy:

```rust
let decision = bus.suggest_scaling().await;
match decision {
    ScalingDecision::AddShards(n) => bus.add_shards(n).await?,
    ScalingDecision::RemoveShards(ids) => bus.remove_shards(ids).await?,
    ScalingDecision::NoChange => {}
}
```

Adding shards is cheap — new ring buffers, new workers, and the next ingest with a hash that lands on them will be served immediately. Removing shards is more involved: the bus waits for the shard's drain worker to clear the buffer and dispatch the final batch before the shard is unregistered. The merger's view is updated atomically so a poll can't be routed to a half-removed shard.

## Common shapes

Three patterns cover most of what you'll write:

**Producer-only**, where the bus is purely an ingestion endpoint. Construct with a durable adapter (`NetAdapter` against a mesh that has RedEX enabled, or `RedisAdapter`); ingest from your application; let consumers elsewhere on the mesh poll independently.

**Consumer-only**, where the bus exists only to drive a downstream component. Construct with the same adapter, never call `ingest`, and run a polling loop against `poll()` until shutdown.

**Both**, where a single bus instance both ingests and consumes — typical for daemons that read events, transform them, and emit derived events. Most application code lives here.

The single primitive shapes all three. The bus doesn't care which role you're playing.
