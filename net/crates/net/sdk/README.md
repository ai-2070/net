# Net Rust SDK

Ergonomic Rust SDK for the Net mesh — a latency-first encrypted mesh where
services and agents announce capabilities, discover each other, and invoke work
over typed RPC.

The core `net-mesh` crate is the engine. This is what Rust developers import.

**Docs: <https://ai2070.net/docs/sdk/rust/quickstart>** ·
[API reference](https://docs.rs/net-mesh-sdk) ·
[Concepts](https://ai2070.net/docs/concepts/architecture)

## Install

```bash
cargo add net-mesh-sdk
```

The crate publishes as `net-mesh-sdk` and imports as `net_sdk`:

```rust
use net_sdk::Net;
```

Defaults are deliberately broad — mesh transport, NAT traversal, typed nRPC,
capability discovery, daemon supervision, blob storage and the federated query
plane are all on, so nothing has to be discovered before the protocol's
headline features work. For a leaner build:

```bash
cargo add net-mesh-sdk --no-default-features --features local   # mesh + storage only
cargo add net-mesh-sdk --features redis,jetstream               # external transports
```

Features: `net`, `nat-traversal`, `port-mapping`, `cortex`, `compute`,
`groups`, `meshos`, `deck`, `dataforts`, `meshdb`, `aggregator`, `tool`,
`macros`, `pin-watch`, `testing`, `fixtures`, `redis`, `jetstream`, plus the
meta-features `local`, `agent` and `full`. `redis`, `jetstream` and
`port-mapping` stay opt-in — external I/O and a heavy UPnP dependency tree.

## Quickstart

```rust
use net_sdk::{Backpressure, Net};

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    let node = Net::builder()
        .shards(4)
        .backpressure(Backpressure::DropOldest)
        .memory()
        .build()
        .await?;

    let r = node.emit(&serde_json::json!({ "sensor": "lidar", "range_m": 12.5 }))?;
    println!("emitted -> shard {} at ts {}", r.shard_id, r.timestamp);

    let stats = node.stats();
    println!("{} ingested, {} dropped", stats.events_ingested, stats.events_dropped);

    node.shutdown().await?;
    Ok(())
}
```

`emit` confirms the event was **accepted into the local ring buffer** — not
that a subscriber processed it. Under backpressure it can drop; check
`stats().events_dropped`. See
[Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

## Two node types

`Net` is the **bus**. For the agentic surface — announcing, discovering and
invoking capabilities — build a **`Mesh`**:

```rust
use net_sdk::mesh::MeshBuilder;

const PSK: [u8; 32] = [0x42u8; 32];   // both peers share the same key
let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;
```

From there the loop is
[Announce](https://ai2070.net/docs/sdk/rust/announce) →
[Discover](https://ai2070.net/docs/sdk/rust/discover) →
[Invoke](https://ai2070.net/docs/sdk/rust/invoke) →
[Watch](https://ai2070.net/docs/sdk/rust/watch).

## What's here

| Surface | Guide |
|---|---|
| Event bus — shards, typed streams, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce, discover by tag and characteristics | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response and streaming | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX — durable append-only logs | [Durable logs](https://ai2070.net/docs/guides/durable-logs) |
| CortEX / NetDB — folded state and queries over it | [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries across nodes | [NetDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute — daemons, placement, live migration | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement) |
| Groups — replica / fork / standby | [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| MeshOS + Deck — supervision and the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Scheduler — gang-claim, task lifecycle | [Gang scheduler](https://ai2070.net/docs/guides/gang-scheduler), [Task lifecycle](https://ai2070.net/docs/guides/task-lifecycle) |
| Security — ed25519 identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — every `SdkError` variant and subsystem enum | [Errors](https://ai2070.net/docs/sdk/rust/errors) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## Examples

```bash
cargo run --example hello
```

| Example | Shows |
|---|---|
| `hello.rs` | The emit/stats loop above |
| `channels.rs` | Named pub/sub across two nodes |
| `nrpc_echo.rs` | Typed request/response over the mesh |
| `tool_calling.rs` | Two nodes: announce, discover, invoke |
| `stream.rs` | Multi-peer streaming with backpressure |
| `backpressure.rs` | What drops look like, and how to see them |

## License

MIT OR Apache-2.0
