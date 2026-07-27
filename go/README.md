# Net Go Bindings

Go bindings for the Net mesh — a latency-first encrypted mesh where services
and agents announce capabilities, discover each other, and invoke work over
typed RPC.

**Docs: <https://ai2070.net/docs/sdk/go/quickstart>** ·
[Concepts](https://ai2070.net/docs/concepts/architecture)

## Prerequisites

1. **Rust toolchain** — <https://rustup.rs>
2. **Go 1.21+** — <https://go.dev>

The bindings link against the `libnet` cdylib through cgo, so the shared
library has to be built before the Go package will link.

## Building the shared library

Run from the Cargo workspace root (`net/crates/net/`):

```bash
cargo build --release

# CortEX + RedEX support (required for the cortex.go surface):
cargo build --release --features "netdb redex-disk"
```

Output, relative to that root:

| Platform | Path |
|---|---|
| Linux | `target/release/libnet.so` |
| macOS | `target/release/libnet.dylib` |
| Windows | `target/release/net.dll` |

Several surfaces live in their own cdylibs — `libnet_rpc`, `libnet_meshdb`,
`libnet_meshos`, `libnet_deck`, `libnet_org` — built with `-p net-rpc-ffi` and
friends. See [Headers and Linking](https://ai2070.net/docs/sdk/c/headers-and-linking),
which maps every surface to its library.

## Install

```bash
go get github.com/ai-2070/net/go
```

The package imports as `"github.com/ai-2070/net/go"` and its identifier is
`net`, so usage reads `net.New(...)`, `net.NewMeshNode(...)` — no rename
needed.

## Quickstart

```go
package main

import (
    "fmt"
    "log"

    net "github.com/ai-2070/net/go"
)

func main() {
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
    if err != nil {
        log.Fatal(err)
    }
    for _, ev := range resp.Events {
        fmt.Println("event", string(ev))
    }

    stats, err := bus.Stats()
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("%d ingested, %d dropped\n", stats.EventsIngested, stats.EventsDropped)
}
```

`Ingest` returns once the event is accepted into the local ring buffer —
acceptance, not delivery. Under backpressure events can drop; check
`EventsDropped`. See
[Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

Two Go-specific shapes worth knowing. `Stats()` returns `(*Stats, error)`
because it crosses the cgo boundary and can fail once the handle is shut down.
And consumption is **poll-based** — `Poll(limit, cursor)` returns a
`*PollResponse` whose `NextID` you pass back to page forward. There is no async
subscribe iterator; see [Watch](https://ai2070.net/docs/sdk/go/watch).

## The mesh node

```go
node, err := net.NewMeshNode(net.MeshConfig{
    BindAddr: "127.0.0.1:0",
    PskHex:   "4242...",   // 32-byte pre-shared key, hex-encoded
})
```

From there the loop is
[Announce](https://ai2070.net/docs/sdk/go/announce) →
[Discover](https://ai2070.net/docs/sdk/go/discover) →
[Invoke](https://ai2070.net/docs/sdk/go/invoke) →
[Watch](https://ai2070.net/docs/sdk/go/watch).

## Thread safety and lifetimes

All methods on `Net` are safe to call from multiple goroutines. Handles that
wrap native resources — `Redex`, `TasksAdapter`, `MeshDbReader`,
`MeshDbRunner`, `MeshDbIter`, `RedisStreamDedup` — need an explicit `Close()`
or `Free()`. Some set a finalizer as a backstop, but don't rely on it.

## What's here

| Surface | Guide |
|---|---|
| Event bus — shards, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response and streaming | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX / CortEX / NetDB — logs, folds, queries | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries | [NetDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute + Groups — daemons, migration, replica/fork/standby | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Security — identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — all 49 sentinels and the typed structs | [Errors](https://ai2070.net/docs/sdk/go/errors) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## Performance

Match `NumShards` to your core count, and raise `RingBufferCapacity` if you see
drops under burst. [Running in production](https://ai2070.net/docs/guides/production-deployment)
covers the rest.

## License

MIT OR Apache-2.0
