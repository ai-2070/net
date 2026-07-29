# Net Go Bindings

**A latency-first encrypted mesh where services and agents announce what they
can do, discover each other at runtime, and invoke work over typed RPC.**

[![Go Reference](https://pkg.go.dev/badge/github.com/ai-2070/net/go.svg)](https://pkg.go.dev/github.com/ai-2070/net/go)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)

There is no broker. Every node is a peer on a flat, encrypted topology. A node
publishes the capabilities it has — a GPU, a model, a tool, a licensed seat —
and other nodes find it by *what it can do*, not by hostname. Credentials never
leave the node that holds them: the machine with the secret runs the work.

```go
// Find something that can do the job, then do it — no registry, no config.
nodes, _ := node.FindNodes(net.CapabilityFilter{RequireTags: []string{"gpu"}})
resp, _ := net.CallTool[Req, Resp](ctx, rpc, "summarize", Req{Text: text})
```

## Why this instead of a queue

Kafka, NATS and Redis Streams move bytes between a fixed producer and a fixed
consumer through a broker you operate. That's a different problem. Net is for
when **the set of participants isn't known in advance** and **work has state
worth observing**:

| You need | Net gives you |
|---|---|
| Tools that appear and vanish at runtime | Capability announce + discovery — no polling, no service registry |
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
go get github.com/ai-2070/net/go
```

The package imports as `"github.com/ai-2070/net/go"` and its identifier is
`net`, so usage reads `net.New(...)`, `net.NewMeshNode(...)` — no rename
needed.

### Prerequisites

The bindings link against the `libnet` cdylib through cgo, so you need a
[Rust toolchain](https://rustup.rs) and **Go 1.21+**, and the shared library
has to exist before the Go package will link. From the Cargo workspace root
(`net/crates/net/`):

```bash
cargo build --release
cargo build --release --features "netdb redex-disk"   # for the cortex.go surface
```

| Platform | Output |
|---|---|
| Linux | `target/release/libnet.so` |
| macOS | `target/release/libnet.dylib` |
| Windows | `target/release/net.dll` |

Several surfaces live in their own cdylibs — `libnet_rpc`, `libnet_meshdb`,
`libnet_meshos`, `libnet_deck`, `libnet_org` — built with `-p net-rpc-ffi` and
friends. [Headers and Linking](https://ai2070.net/docs/sdk/c/headers-and-linking)
maps every surface to its library.

## The loop: announce → discover → invoke

**Announce** what this node can do:

```go
node, err := net.NewMeshNode(net.MeshConfig{
    BindAddr: "127.0.0.1:0",
    PskHex:   psk,   // 32-byte pre-shared key, hex-encoded
})
if err != nil {
    log.Fatal(err)
}
defer node.Shutdown()

err = node.AnnounceCapabilities(net.CapabilitySet{
    Tags: []string{"gpu", "inference", "region:eu-west"},
})
```

**Discover** — by capability, or by listing the tools peers are serving:

```go
nodes, err := node.FindNodes(net.CapabilityFilter{RequireTags: []string{"gpu"}})
// nodes is []uint64 — the node ids that match, right now.

rpc, err := net.NewMeshRpc(node)
tools, err := rpc.ListTools()
```

**Invoke** — generic over request and response types:

```go
type WebSearchReq struct {
    Query string `json:"query"`
}
type WebSearchResp struct {
    Results []string `json:"results"`
}

resp, err := net.CallTool[WebSearchReq, WebSearchResp](
    context.Background(),
    rpc,
    "web_search",
    WebSearchReq{Query: "how does the capability fold work"},
)
```

For services rather than tools, nRPC gives you the same shape with deadlines,
streaming and cancellation — [Typed RPC](https://ai2070.net/docs/guides/nrpc).

## The bus

```go
bus, err := net.New(nil)   // nil = default (memory) config
if err != nil {
    log.Fatal(err)
}
defer bus.Shutdown()

if err := bus.IngestRaw(`{"sensor": "lidar", "range_m": 12.5}`); err != nil {
    log.Fatal(err)
}
if err := bus.Ingest(map[string]any{"sensor": "radar", "range_m": 45.0}); err != nil {
    log.Fatal(err)
}

// Poll — cursor-paginated. "" starts from the earliest buffered event.
resp, err := bus.Poll(100, "")
for _, ev := range resp.Events {
    fmt.Println("event", string(ev))
}

stats, err := bus.Stats()
fmt.Printf("%d ingested, %d dropped\n", stats.EventsIngested, stats.EventsDropped)
```

`Ingest` returns once the event is **accepted into the local ring buffer** —
not that anyone processed it. Under backpressure it drops, and
`EventsDropped` is how you find out:
[Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

Two Go-specific shapes worth knowing. `Stats()` returns `(*Stats, error)`
because it crosses the cgo boundary and can fail once the handle is shut down.
And consumption is **poll-based** — pass `resp.NextID` back as the cursor to
page forward. There is no async subscribe iterator; see
[Watch](https://ai2070.net/docs/sdk/go/watch).

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

> *"Wire up a Net publisher and subscriber over the mesh in Go."*

`net-event-bus` covers pub/sub, nRPC, the MCP bridge, organization capability
auth, the gang-claim scheduler, and RedEX / CortEX / Dataforts.
`net-payments` covers x402 pricing, quotes, settlement and spend policy. Full
install options in [Claude Skills](https://ai2070.net/docs/start/claude-skills).

## Thread safety and lifetimes

All methods on `Net` are safe to call from multiple goroutines. Handles that
wrap native resources — `Redex`, `TasksAdapter`, `MeshDbReader`,
`MeshDbRunner`, `MeshDbIter`, `RedisStreamDedup` — need an explicit `Close()`
or `Free()`. Some set a finalizer as a backstop; don't rely on it.

Errors are 49 sentinels you compare with `errors.Is`, plus typed structs
carrying a `Kind` for `errors.As`:
[Errors](https://ai2070.net/docs/sdk/go/errors).

## What's in the box

| Surface | Guide |
|---|---|
| Event bus — shards, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response, streaming, cancellation | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX / CortEX / NetDB — logs, folds, queries | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries | [MeshDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute + Groups — daemons, migration, replica/fork/standby | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Organizations — capabilities only your org can discover | [Private capabilities](https://ai2070.net/docs/guides/private-capabilities) |
| Security — identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## Performance

Match `NumShards` to your core count, and raise `RingBufferCapacity` if you see
drops under burst.
[Running in production](https://ai2070.net/docs/guides/production-deployment)
covers the rest.

## Links

[Docs](https://ai2070.net/docs) ·
[Quickstart](https://ai2070.net/docs/sdk/go/quickstart) ·
[Go reference](https://pkg.go.dev/github.com/ai-2070/net/go) ·
[GitHub](https://github.com/ai-2070/net)

## License

MIT OR Apache-2.0
