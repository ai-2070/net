## Install it — Rust

```sh
cargo add net-mesh          # the core crate
cargo add net-mesh-sdk      # the ergonomic SDK, if you want typed channels
```

`net-mesh` re-exports as `net`, so user code keeps `use net::…` short. The SDK
imports as `net_sdk`.

Pinning explicitly, and note the version — **0.36 is the published release**:

```toml
[dependencies]
net-mesh = "0.36"
net-mesh-sdk = "0.36"
```

### Feature flags

Rust is the one surface where you choose what gets compiled. The default set
compiles the full stack:

| Feature | What it adds | On by default |
| --- | --- | --- |
| `net` | Mesh transport — Noise handshakes, ChaCha20-Poly1305, ed25519 identities | yes |
| `nat-traversal` | Reflex probes, classification, rendezvous punch | yes |
| `cortex` | Folded-state driver (pulls in `redex`) | yes |
| `meshdb` | Federated query layer | yes |
| `meshos` | Cluster behaviour engine, daemon supervision | yes |
| `dataforts` | Content-addressed blobs, greedy-LRU cache, gravity placement | yes |
| `redex` | RedEX append-only logs — implied by `cortex` | via `cortex` |
| `redex-disk` | Disk-backed RedEX segments rather than memory-only | no |
| `netdb` | NetDB query surface over folded state | no |
| `tool` | `ToolDescriptor` + tool-metadata RPC (requires `cortex`) | no |
| `regex` | Regex predicates in the filter DSL | no |
| `batched-ingress` | Batched receive path on the Net adapter | no |
| `port-mapping` | UPnP-IGD / NAT-PMP opportunistic port mapping | no |
| `redis` | Redis Streams adapter | no |
| `jetstream` | NATS JetStream adapter | no |
| `cli` | The `net-blob` operator CLI | no |
| `ffi` | C ABI surface — enabled by the `cdylib` / `staticlib` builds | no |

Features compose: `cortex` implies `redex`, and `meshdb`, `netdb` and `tool` each
imply `cortex`. Turning on `netdb` therefore also pulls in the fold driver and the
log underneath it.

**One flag fails quietly.** Without `regex`, a peer can still send you a regex
predicate and your node matches it **closed** — the pattern matches nothing rather
than erroring. If you rely on regex predicates, enable it explicitly.

A minimal build — in-memory bus only, no mesh, no persistence:

```toml
[dependencies]
net-mesh = { version = "0.36", default-features = false }
```

### What is peculiar about Rust here

The SDK is async and expects a Tokio runtime; `#[tokio::main]` is enough for a
first program. Builder methods are async and fallible, so `.build().await?` rather
than a constructor.

### Verify it worked

```rust
use net_sdk::Net;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Hello {
    msg: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> net_sdk::error::Result<()> {
    let node = Net::builder().shards(1).memory().build().await?;
    node.emit(&Hello { msg: "hello, mesh".into() })?;

    // Counts at the PRODUCER boundary: the bus accepted the event. It does not
    // say anything received or stored it — see the note above about memory.
    let stats = node.stats();
    assert_eq!(stats.events_ingested, 1, "the bus did not accept the event");
    println!("accepted: ingested={}", stats.events_ingested);

    node.shutdown().await?;
    Ok(())
}
```

Expect one line, `accepted: ingested=1`, and a clean exit. This program is the
same one CI runs on every commit — see [Claude Skills](/docs/start/claude-skills)
for the executed examples.

Next: [the Rust SDK spine](/docs/sdk/rust/quickstart).
