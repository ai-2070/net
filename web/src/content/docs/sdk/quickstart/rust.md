## Build it — Rust

```bash
cargo add net-mesh-sdk tokio serde serde_json
```

The crate installs as `net-mesh-sdk` and imports as `net_sdk`.

### A bus node

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

    node.shutdown().await?;   // drains the ring buffer
    Ok(())
}
```

`emit` hands back a receipt (`shard_id`, `timestamp`). Under backpressure it can
drop rather than block, so the receipt is the acceptance signal and
`stats().events_dropped` is the one to watch.

### A mesh node

```rust
use net_sdk::mesh::MeshBuilder;

const PSK: [u8; 32] = [0x42u8; 32];   // raw bytes in Rust, not hex

let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;
```

Rust takes the PSK as `&[u8; 32]` — a fixed-size array, checked at compile time.
This is the only binding where a wrong-length key cannot reach a runtime error.

### The handshake, in full

Nothing folds between two nodes until they are connected and started. `.inner()`
reaches the transport handle that carries these:

```rust
let host = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;
let agent = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;

let host_addr = host.inner().local_addr();
let host_pub = *host.inner().public_key();
let host_id = host.inner().node_id();
let agent_id = agent.inner().node_id();

let (a, c) = tokio::join!(
    host.inner().accept(agent_id),
    agent.inner().connect(host_addr, &host_pub, host_id),
);
a?;
c?;

host.inner().start();
agent.inner().start();
```

Both sides must `start()`. A connected pair that never started looks identical to
an unconnected pair from the announce side: no error, no peers.

### Verify it worked

```rust
let stats = node.stats();
assert_eq!(stats.events_ingested, 1, "the bus did not accept the event");
println!("accepted: ingested={}", stats.events_ingested);
```

Expect one line, `accepted: ingested=1`. The counter is read at the producer
boundary — it says the local node took the event, not that anything consumed it.

The runnable version of the bus loop is `cargo run --example hello` in the Net
repo; the two-node mesh version with the handshake above is
`sdk/examples/tool_calling.rs`.

Next: [Announce a capability](/docs/sdk/rust/announce).
