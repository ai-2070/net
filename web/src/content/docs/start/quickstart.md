# Quickstart

This page gets you from zero to a working event bus in about five minutes. We'll start a bus on a single process, publish a few events, see what the bus did with them, and then point at what changes when you want the same code to run across a real mesh.

Every snippet below is compiled in CI as [`examples/docs_quickstart.rs`](https://github.com/ai-2070/net/blob/master/net/crates/net/examples/docs_quickstart.rs) — if the API moves, the build breaks before the page goes stale.

The examples are in Rust because the core crate is Rust, but the same surface exists for [Node, Python, and Go](/docs/start/install). If you're working in one of those bindings, swap the import line and the syntax — the call shapes match.

## First, which crate?

Net ships two Rust entry points, and picking the wrong one is the most common way to lose an afternoon. They are layers, not alternatives:

| | `net-mesh` (this page) | `net-mesh-sdk` ([SDK quickstart](/docs/sdk/rust/quickstart)) |
|---|---|---|
| Imports as | `net` | `net_sdk` |
| You get | `EventBus` — shards, ring buffer, adapters, filters | `Net` (the bus, ergonomically) and `Mesh` (capabilities, nRPC, tools) |
| Reach for it when | you're embedding the bus, writing an adapter, or tuning the ingest path | you're building an agent, a service, or anything that discovers and calls capabilities |

**If you're new, you almost certainly want the SDK.** Capability discovery, typed RPC, daemons, and tool calling all live there, and its `Mesh` node finds peers by capability rather than by address. Read this page to understand the substrate underneath — it's short, and the mental model pays for itself the first time you tune a shard count or write an adapter.

The two share one runtime: `net_sdk::Net` *is* an `EventBus` with a friendlier surface, and both crates pin to the same version.

## Install

```sh
cargo add net-mesh tokio --features tokio/macros,tokio/rt-multi-thread
```

The crate name on crates.io is `net-mesh`; you import it as `net`. Default features compile the full stack — mesh transport, NAT traversal, CortEX, MeshDB, MeshOS, Dataforts. You can pare that down later if you want a smaller build.

## Publish

```rust
use std::sync::atomic::Ordering;
use net::{Event, EventBus, EventBusConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bus = EventBus::new(EventBusConfig::default()).await?;

    bus.ingest(Event::from_str(r#"{"token": "hello", "index": 0}"#)?)?;
    bus.ingest(Event::from_str(r#"{"token": "world", "index": 1}"#)?)?;

    // Ingest is non-blocking; flush waits for the batch workers to drain.
    bus.flush().await?;

    let stats = bus.stats();
    println!(
        "ingested={} dispatched={} dropped={}",
        stats.events_ingested.load(Ordering::Relaxed),
        stats.events_dispatched.load(Ordering::Relaxed),
        stats.events_dropped.load(Ordering::Relaxed),
    );

    bus.shutdown().await?;
    Ok(())
}
```

Run it and you'll see `ingested=2 dispatched=2 dropped=0`. That's the whole ingest path: construct, ingest, flush, shutdown.

A few things worth knowing about what just happened:

- `EventBusConfig::default()` gives you a single-node bus backed by the **no-op adapter**, which accepts batches and discards them. It's the right shape for benchmarking the ingest path and the wrong shape for anything else.
- `bus.ingest()` is non-blocking. It hashes the event onto a shard and returns; a background worker drains the shard into the adapter. Ingestion is built to sustain tens of millions of events per second on commodity hardware.
- Because ingest is asynchronous, a receipt means *accepted into the local ring buffer* — not *delivered*, and not *processed*. Under backpressure it can drop, which is why `events_dropped` is worth printing. [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed) is the long version of that distinction.
- `bus.shutdown()` drains in-flight ingests, flushes everything to the adapter, and stops the workers cleanly. Calling it is the contract — dropping the bus without shutting down will lose anything still in the ring buffer.

## Reading events back

`bus.poll()` is the cursor-based consumer, and it reads **through the adapter** rather than out of the local ring buffer:

```rust
use net::ConsumeRequest;

let response = bus.poll(ConsumeRequest::new(100)).await?;
for event in response.events {
    // `raw` is the payload as `Bytes` — the bus never assumes it's text.
    println!("{}", String::from_utf8_lossy(&event.raw));
}
```

Run that against the configuration above and it returns **zero events** — the no-op adapter had nothing to hand back. That is not a bug you can configure away in one process: the core crate's readable adapters are Redis, JetStream, and the mesh itself, so reading your own events back means choosing one of them. The [mesh section below](#switch-to-the-mesh) is the usual answer; `--features redis` or `--features jetstream` are the others.

Pass `from(...)` on the request to resume from a cursor, and `filter(...)` to narrow what comes back.

## Add a filter

Most consumers don't want every event on the bus. Filters are JSON predicates evaluated against the event payload:

```rust
use net::Filter;
use serde_json::json;

let request = ConsumeRequest::new(100)
    .filter(Filter::eq("token", json!("hello")));

let response = bus.poll(request).await?;
```

Filter values are `serde_json::Value`, so `json!` is the shortest way to write
one. For several conditions at once, `FilterBuilder` accumulates them and
`build_and()` / `build_or()` combine them.

The filter DSL covers existence, equality, numeric comparisons, string matching, and semver — the full grammar lives in the [filter reference](/docs/reference/filter-dsl).

## Switch to the mesh

Everything above runs in one process. To turn the same code into a real distributed bus, you swap the adapter:

```rust
use net::adapter::net::{NetAdapterConfig, StaticKeypair};
use net::{AdapterConfig, EventBus, EventBusConfig};

// Both sides share a pre-shared key; the responder owns a static keypair
// and the initiator must already know its public half.
let psk = [0x42u8; 32];
let responder = StaticKeypair::generate();

let adapter = NetAdapterConfig::initiator(
    "0.0.0.0:7777".parse()?,   // bind
    "10.0.0.2:7777".parse()?,  // peer
    psk,
    responder.public,
);

let config = EventBusConfig::builder()
    .adapter(AdapterConfig::Net(Box::new(adapter)))
    .build()?;

let bus = EventBus::new(config).await?;
```

Once configured, ingestion and consumption work identically — `ingest()` publishes onto the mesh, `poll()` receives from it.

Two things this snippet makes explicit that are easy to skip past. A Net-adapter link is a **Noise session between two named peers**, so it is not symmetric: one side is the `initiator` and must already hold the other's static public key, the other is the `responder` and holds the keypair. And the key material is yours to distribute — the transport encrypts and authenticates every frame for you, but nothing here invents a PSK or discovers a peer on your behalf.

If what you actually want is a node that finds its peers by capability rather than by address, that's the `Mesh` surface in the SDK, not the raw adapter — see the [Rust SDK quickstart](/docs/sdk/rust/quickstart).

That's the part that takes longer than five minutes to fully explore — channel naming, visibility scopes, durable persistence, capability-based authorization — but the call shape never changes. Once you have the loop above working locally, the rest is configuration.

## The agentic path

The event bus above is the substrate. Net's flagship use is agents discovering and
invoking work across the mesh — a different loop on the same foundation, and the
one the SDK is built for:

- [Rust SDK quickstart](/docs/sdk/rust/quickstart) — the same first program on the
  `net_sdk` surface, then a two-node `Mesh` that discovers capabilities. **Start
  here if you skipped the crate table above.**
- [Discover and Invoke](/docs/guides/discover-and-invoke) — query the mesh by
  capability (`net-mesh cap query --tag …`) and make a typed call.
- [Wrap an MCP Server](/docs/guides/wrap-mcp-server) — turn an existing MCP tool
  into a discoverable capability with one command.
- [Expose Net as MCP](/docs/guides/expose-net-as-mcp) — let any MCP host use the
  mesh.
- [The Agentic Mesh](/docs/worldview/agentic-mesh) — the worldview behind all of it.
