# Quickstart

This page gets you from zero to a working event bus in about five minutes. We'll start a bus on a single process, publish a few events, consume them back, and then point at what changes when you want the same code to run across a real mesh.

The examples are in Rust because the core crate is Rust, but the same surface exists for [Node, Python, and Go](./install). If you're working in one of those bindings, swap the import line and the syntax — the call shapes match.

## Install

```sh
cargo add net-mesh tokio --features tokio/macros,tokio/rt-multi-thread
```

The crate name on crates.io is `net-mesh`; you import it as `net`. Default features compile the full stack — mesh transport, NAT traversal, CortEX, MeshDB, MeshOS, Dataforts. You can pare that down later if you want a smaller build.

## Publish and consume

```rust
use net::{Event, EventBus, EventBusConfig, ConsumeRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bus = EventBus::new(EventBusConfig::default()).await?;

    bus.ingest(Event::from_str(r#"{"token": "hello", "index": 0}"#)?)?;
    bus.ingest(Event::from_str(r#"{"token": "world", "index": 1}"#)?)?;

    let response = bus.poll(ConsumeRequest::new(100)).await?;
    for event in response.events {
        println!("{}", event.raw);
    }

    bus.shutdown().await?;
    Ok(())
}
```

Run it and you'll see both events printed back in order. That's the entire event-bus loop: construct, ingest, poll, shutdown.

A few things worth knowing about what just happened:

- `EventBusConfig::default()` gives you a single-node bus backed by a no-op adapter — events live in memory, no replication, no persistence. It's the right shape for tests and local development and the wrong shape for production.
- `bus.ingest()` is non-blocking. It hashes the event onto a shard and returns; a background worker drains the shard into the adapter. Ingestion is built to sustain tens of millions of events per second on commodity hardware.
- `bus.poll()` is the cursor-based consumer. Pass a `from(...)` cursor on the request to resume from where you left off; pass a `filter(...)` to subscribe only to events matching a predicate.
- `bus.shutdown()` drains in-flight ingests, flushes everything to the adapter, and stops the workers cleanly. Calling it is the contract — dropping the bus without shutting down will lose anything still in the ring buffer.

## Add a filter

Most consumers don't want every event on the bus. Filters are JSON predicates evaluated against the event payload:

```rust
use net::Filter;

let request = ConsumeRequest::new(100)
    .filter(Filter::new().eq("token", "hello"));

let response = bus.poll(request).await?;
```

The filter DSL covers existence, equality, numeric comparisons, string matching, and semver — the full grammar lives in the [filter reference](../reference/filter-dsl).

## Switch to the mesh

Everything above runs in one process. To turn the same code into a real distributed bus, you swap the adapter:

```rust
use net::{EventBusConfig, AdapterConfig};

let config = EventBusConfig::builder()
    .adapter(AdapterConfig::net()
        .listen("0.0.0.0:7777")
        .peer("10.0.0.2:7777"))
    .build()?;

let bus = EventBus::new(config).await?;
```

Once configured, ingestion and consumption work identically — `ingest()` publishes onto the mesh, `poll()` receives from it. The bus on node A and the bus on node B share state through the channels they both subscribe to. Identity, encryption, NAT traversal, and routing are all handled for you.

That's the part that takes longer than five minutes to fully explore — channel naming, visibility scopes, durable persistence, capability-based authorization — but the call shape never changes. Once you have the loop above working locally, the rest is configuration.

## Next: the agentic path

The event bus above is the substrate. Net's flagship use is agents discovering and
invoking work across the mesh — a different loop on the same foundation:

- [Discover and Invoke](/docs/guides/discover-and-invoke) — query the mesh by
  capability (`net cap query --tag …`) and make a typed call.
- [Wrap an MCP Server](/docs/guides/wrap-mcp-server) — turn an existing MCP tool
  into a discoverable capability with one command.
- [Expose Net as MCP](/docs/guides/expose-net-as-mcp) — let any MCP host use the
  mesh.
- [The Agentic Mesh](/docs/worldview/agentic-mesh) — the worldview behind all of it.
