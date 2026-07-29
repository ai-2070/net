# Rust binding

Read `../apis.md` first for the four surfaces and the cross-SDK rules. This page
is only what is Rust-specific.

## Package and import

```toml
[dependencies]
net-mesh-sdk = "0.34"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt", "macros"] }
futures = "0.3"
```

**Publishes as `net-mesh-sdk`, imports as `net_sdk`.** There is no crate called
`net-sdk`; a `net-sdk = "..."` dependency line does not resolve.

## Features — the default is not empty

This is the single most misread thing about the Rust SDK.

**`default` is a ten-feature bundle**, not `[]`:

```
net · nat-traversal · cortex · compute · groups · meshos · dataforts · meshdb · aggregator · tool
```

So mesh transport, NAT traversal, capability discovery, daemon supervision, the
dataforts blob surface and the MeshDB query plane are **already on** in a plain
`net-mesh-sdk = "0.34"`. You do not opt into `net` to get mesh transport. The
set is chosen so a consumer of a prebuilt binding artifact — where opting in
post-install is not a thing — gets every primitive the protocol advertises.

To get a lean build, opt *out* and re-add:

```toml
net-mesh-sdk = { version = "0.34", default-features = false, features = ["local"] }
```

| Meta-feature | Expands to |
|---|---|
| `local` | `net`, `cortex`, `compute`, `groups`, `aggregator` — single-node or LAN-only |
| `agent` | the same ten features as `default` |
| `full` | `local`, `nat-traversal`, `redis`, `jetstream`, `meshos`, `deck`, `tool`, `macros` |

**`full` is not everything.** It deliberately omits `dataforts` and `meshdb`,
because both pull substrate features that enlarge the dependency footprint. It
means "the common bundle for an operator's full-stack daemon". Add those two
flags explicitly if you want them — and note that `default` already has them, so
switching from `default` to `full` *removes* surface.

External transports are opt-in and off by default: `redis`, `jetstream`.
`port-mapping` builds on `nat-traversal`; `macros` builds on `tool`; `deck`
builds on `meshos`.

## Construction and lifecycle

<!-- skill-check: compile -->
```rust
use net_sdk::{Backpressure, Net};
use serde::{Deserialize, Serialize};
use futures::StreamExt;

#[derive(Serialize, Deserialize)]
struct TempReading { sensor_id: String, celsius: f64 }

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    let node = Net::builder()
        .shards(4)
        .backpressure(Backpressure::DropOldest)
        .memory()                          // or .redis(..) / .jetstream(..) / .mesh(..)
        .build()
        .await?;

    node.emit(&TempReading { sensor_id: "A1".into(), celsius: 22.5 })?;

    let mut stream = node.subscribe_typed::<TempReading>(Default::default());
    while let Some(r) = stream.next().await {
        let r = r?;
        println!("{}: {}°C", r.sensor_id, r.celsius);
    }

    node.shutdown().await?;
    Ok(())
}
```

`Net::builder()` … `.build().await` is async. The builder also selects the
transport: `.memory()`, `.redis(RedisAdapterConfig)`,
`.jetstream(JetStreamAdapterConfig)`, `.mesh(NetAdapterConfig)`. **Adapter
methods take typed configs, not URL strings.**

Convenience presets: `.high_throughput()`, `.low_latency()`, `.batch(BatchConfig)`,
`.scaling(ScalingPolicy)`, `.adapter_timeout(Duration)`.

## Names and shapes

- `emit(&T)` → `Receipt { shard_id, timestamp }`. `emit_batch(&[T])` → `usize`.
- Fast paths (`net/crates/net/sdk/src/net.rs`): `emit_raw(impl Into<Bytes>)` for
  already-serialized bytes (zero-copy), `emit_str(&str)` for JSON-as-string,
  `emit_raw_batch(Vec<Bytes>)` for batched bytes. Same `Receipt`, or `usize` for
  the batch form.
- `subscribe()` and `subscribe_typed::<T>()` return async streams — poll with
  `.next().await`.
- One-shot pull:
  `node.poll(PollRequest { limit, cursor, filter, ordering, shards }).await?` →
  `PollResponse { events, next_id, has_more }`. Use when you want explicit cursor
  management instead of a stream.
- Observability and lifecycle: `node.flush().await?` drains pending batches into
  the adapter, `node.health().await -> bool`, `node.shards() -> u16`,
  `node.bus() -> &EventBus` as the escape hatch to the core API.
- `Backpressure::{DropOldest (default), DropNewest, FailProducer, Sample(u32)}`
  is set at build time. `Sample(N)` keeps 1 in N events when overloaded.

## The buffer-capacity rule no compiler enforces

`ring_buffer_capacity` must be a **power of two and at least 1024**. It is
validated in the shared core config at construction, so every binding raises the
same way — and no compile or type check catches it. The default is 1,048,576
(1M events per shard), which is also why a "demonstrate backpressure" snippet
that emits a few thousand events into a default node drops nothing at all.

Spelt `.buffer_capacity(1024)` in this binding.

## Errors

Ingestion returns `Result`, and the failure cause is structured:
`SdkError::Backpressure`, `Sampled`, `Unrouted`, `Ingestion(_)`. Older releases
routed everything through `Ingestion(String)`; do not write code that matches on
the string. `SdkError` is `#[non_exhaustive]` — always keep a `_` arm. The full
taxonomy is in `error-codes.md`.

## Shutdown

`node.shutdown().await?` is reference-based and **tolerates outstanding
`subscribe` stream clones** — both `subscribe()` and `subscribe_typed()` clone
the inner `Arc<EventBus>`. If references are genuinely still held, shutdown
returns `SdkError::Adapter("cannot shutdown: outstanding references exist")`
rather than hanging. This is the one binding that reports that condition; see
`runtime.md` § "Rust: subscribe streams and shutdown".

Call `flush()` before `shutdown()` if you cannot tolerate losing in-flight
batches.

## Gaps

- **No `node.channel()` API.** Rust has only the raw firehose. To split topics,
  use distinct types or enum variants in the payload and match on the consumer,
  or run separate `Net` instances per logical channel. This is the most common
  thing ported wrongly from the TypeScript or Python docs.
- Everything else: `bindings/coverage.md`.

## Where to look when this page is not enough

- **Authoritative source:** `net/crates/net/sdk/src/` — `net.rs` for the emit
  and poll surface, `error.rs` for the error enum.
- **Checked examples:** `../examples/hello.rs` (construct · publish · subscribe ·
  shutdown) and `../examples/observe.rs` (drops under backpressure, and the one
  mode that reports them). Both built against the workspace SDK in CI — a
  compile floor, not a behaviour proof.
- **Canonical typed-emit example:** `net/crates/net/sdk/examples/channels.rs`.

## Never infer from another binding

- Rust has **no named channels**. `node.channel(...)` in a TypeScript or Python
  snippet has no Rust equivalent.
- Discovery returns **one** node here (`find_best_node`); Node and Python return
  a list from `findNodes` / `find_nodes` and make you choose.
- nRPC is a method on the mesh here (`call_typed`), not a handle you construct.
  Everywhere else it is a `TypedMeshRpc` object.
- The predicate DSL is a **macro** in Rust (`pred!`), not the builder-and-
  function shape (`p`, `evaluate_predicate`, `predicate_debug_report`) that
  Python and TypeScript expose. Transliterating a Python predicate example into
  Rust produces functions that do not exist.
