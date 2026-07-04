# Rust — Quickstart

Install the SDK and run a node that emits and counts events — the smallest loop
that proves the bus works.

```bash
cargo add net-mesh-sdk tokio serde serde_json
```

## A node that emits events

```rust
use net_sdk::{Backpressure, Net};

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    // Build an in-process node. Memory transport = no network, fastest, for tests.
    let node = Net::builder()
        .shards(4)
        .backpressure(Backpressure::DropOldest)
        .memory()
        .build()
        .await?;

    // Emit structured events (serialized via serde).
    let r = node.emit(&serde_json::json!({ "sensor": "lidar", "range_m": 12.5 }))?;
    println!("emitted -> shard {} at ts {}", r.shard_id, r.timestamp);

    let stats = node.stats();
    println!("{} ingested, {} dropped", stats.events_ingested, stats.events_dropped);

    node.shutdown().await?;   // drain the ring buffer cleanly
    Ok(())
}
```

`emit` returns a receipt (`shard_id`, `timestamp`) — confirmation the event was
**accepted into the local ring buffer**, not that a subscriber processed it (see
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)). Under
backpressure `emit` can drop; always check `stats().events_dropped`.

Run it the same way as the in-tree example:

```bash
cargo run --example hello     # from the net repo — the canonical version of this loop
```

## Two node types

`Net` (above) is the **bus**. For the agentic surface — announcing, discovering,
and invoking capabilities — you build a **`Mesh`** node instead:

```rust
use net_sdk::mesh::MeshBuilder;

const PSK: [u8; 32] = [0x42u8; 32];   // pre-shared key; both peers use the same one
let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;
```

A `Mesh` node speaks encrypted UDP to peers and carries capabilities, tools, and
nRPC. Two `Mesh` nodes handshake and then discover each other's capabilities — the
runnable two-node version is `sdk/examples/tool_calling.rs`.
