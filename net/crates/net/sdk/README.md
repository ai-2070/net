# Net Rust SDK

**A latency-first encrypted mesh where services and agents announce what they
can do, discover each other at runtime, and invoke work over typed RPC.**

[![crates.io](https://img.shields.io/crates/v/net-mesh-sdk.svg)](https://crates.io/crates/net-mesh-sdk)
[![docs.rs](https://img.shields.io/docsrs/net-mesh-sdk)](https://docs.rs/net-mesh-sdk)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)

There is no broker. Every node is a peer on a flat, encrypted topology. A node
publishes the capabilities it has — a GPU, a model, a tool, a licensed seat —
and other nodes find it by *what it can do*, not by hostname. Credentials never
leave the node that holds them: the machine with the secret runs the work.

```rust
// Find something that can do the job, then do it — no registry, no config.
let nodes = mesh.find_nodes(&CapabilityFilter {
    require_gpu: true,
    min_vram_gb: Some(24),
    ..Default::default()
});
let resp: SummarizeResp = caller.call_typed(nodes[0], "summarize", &req, opts).await?;
```

## Why this instead of a queue

Kafka, NATS and Redis Streams move bytes between a fixed producer and a fixed
consumer through a broker you operate. That's a different problem. Net is for
when **the set of participants isn't known in advance** and **work has state
worth observing**:

| You need | Net gives you |
|---|---|
| Tools that appear and vanish at runtime | Capability announce + discovery, event-driven — no polling, no service registry |
| Work spread across machines or organizations | One flat encrypted mesh; multi-hop discovery bounded at 16 hops |
| Credentials that must not travel | The node holding the secret executes; callers never see it |
| More than "did it return 200" | Durable logs, folded state, artifacts, streams, and replayable recovery |
| GPUs matched by capability, not hostname | Hardware is a discoverable characteristic, with atomic gang-claim under contention |

**Don't** reach for it when one API call solves the problem, when a single
server and database are enough, or when you have a fixed producer, a fixed
consumer and a broker you're happy operating. The honest version of that list
is in [When to Use Net](https://ai2070.net/docs/worldview/right-and-wrong-use-cases).

## Install

```bash
cargo add net-mesh-sdk
```

Publishes as `net-mesh-sdk`, imports as `net_sdk`.

Defaults are broad on purpose — mesh transport, NAT traversal, typed nRPC,
capability discovery, daemon supervision, blob storage and federated queries
are all on, so nothing has to be discovered before the headline features work.

```bash
cargo add net-mesh-sdk --no-default-features --features local   # mesh + storage only
cargo add net-mesh-sdk --features redis,jetstream               # external transports
```

Features: `net`, `nat-traversal`, `port-mapping`, `cortex`, `compute`,
`groups`, `meshos`, `deck`, `dataforts`, `meshdb`, `aggregator`, `tool`,
`macros`, `pin-watch`, `testing`, `fixtures`, `redis`, `jetstream`, plus
`local`, `agent` and `full`. `redis`, `jetstream` and `port-mapping` stay
opt-in — external I/O and a heavy UPnP dependency tree.

## The loop: announce → discover → invoke

**Announce.** The `#[tool]` macro derives the schema and registers a handler:

```rust
use net_sdk::macros::tool;
use net_sdk::mesh::MeshBuilder;

#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchReq { /// Free-text query string.
                      query: String }
#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchResp { results: Vec<String> }

#[tool(description = "Search the web for relevant pages.", tag = "web", tag = "research")]
async fn web_search(req: WebSearchReq) -> Result<WebSearchResp, String> {
    Ok(WebSearchResp { results: vec![format!("first hit for '{}'", req.query)] })
}

let host = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;
let _handle = web_search_register(&host)?;      // unregisters on drop
host.announce_capabilities(Default::default()).await?;
```

**Discover.** From any peer that has handshaked with the host:

```rust
for t in agent.list_tools(None) {
    println!("{} v{}  tags={:?}", t.tool_id, t.version, t.tags);
}

// Or react to the mesh changing, rather than polling it.
let mut watch = agent.watch_tools(None, None);
while let Some(change) = watch.next().await {
    println!("{change:?}");        // added / removed / publisher-count change
}
```

**Invoke.** Typed in, typed out:

```rust
let resp: WebSearchResp = agent
    .call_tool("web_search", &WebSearchReq { query: "capability folds".into() })
    .await?;
```

For services rather than tools, `serve_rpc_typed` and `call_typed` give you the
same shape with deadlines, streaming and cancellation —
[Typed RPC](https://ai2070.net/docs/guides/nrpc).

## The bus

`Net` is the other node type: a sharded, in-process event bus with explicit
backpressure.

```rust
use net_sdk::{Backpressure, Net};

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
```

`emit` confirms the event was **accepted into the local ring buffer** — not
that anyone processed it. Under backpressure it drops, and
`stats().events_dropped` is how you find out. That distinction is the whole
philosophy: [Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

## Claude Code Skill

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that compiles, runs, and is quietly wrong. Install the skills
first:

```bash
npx skills add ai-2070/net-claude-skill -g
```

Drop `-g` to install into the current project only. To update to the latest
version:

```bash
npx skills update -g
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. They load automatically when a request matches:

> *"Wire up a Net publisher and subscriber over the mesh in Rust."*

`net-event-bus` covers pub/sub, nRPC, the MCP bridge, organization capability
auth, the gang-claim scheduler, and RedEX / CortEX / Dataforts.
`net-payments` covers x402 pricing, quotes, settlement and spend policy. Full
install options in [Claude Skills](https://ai2070.net/docs/start/claude-skills).

## What's in the box

| Surface | Guide |
|---|---|
| Event bus — shards, typed streams, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce, discover by tag and characteristics | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response, streaming, cancellation | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX — durable append-only logs | [Durable logs](https://ai2070.net/docs/guides/durable-logs) |
| CortEX / NetDB — folded state and queries over it | [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries across nodes | [MeshDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute — daemons, placement, live migration | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement) |
| Groups — replica / fork / standby | [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| MeshOS + Deck — supervision and the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Scheduler — atomic gang-claim, task lifecycle | [Gang scheduler](https://ai2070.net/docs/guides/gang-scheduler), [Task lifecycle](https://ai2070.net/docs/guides/task-lifecycle) |
| MCP bridge — wrap an MCP server, or serve the mesh as MCP | [Wrap MCP](https://ai2070.net/docs/guides/wrap-mcp-server), [Expose as MCP](https://ai2070.net/docs/guides/expose-net-as-mcp) |
| Organizations — capabilities only your org can discover | [Private capabilities](https://ai2070.net/docs/guides/private-capabilities) |
| Security — ed25519 identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — every `SdkError` variant and subsystem enum | [Errors](https://ai2070.net/docs/sdk/rust/errors) |

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

## Links

[Docs](https://ai2070.net/docs) ·
[Quickstart](https://ai2070.net/docs/sdk/rust/quickstart) ·
[API reference](https://docs.rs/net-mesh-sdk) ·
[Concepts](https://ai2070.net/docs/concepts/architecture) ·
[GitHub](https://github.com/ai-2070/net)

## License

MIT OR Apache-2.0
