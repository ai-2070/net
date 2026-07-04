# Go — Quickstart

```bash
go get github.com/ai-2070/net/go
```

## A node that ingests and polls

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

    // Ingest — raw JSON is the fast path.
    if err := bus.IngestRaw(`{"sensor": "lidar", "range_m": 12.5}`); err != nil {
        log.Fatal(err)
    }
    // Or ingest a Go value (serialized to JSON).
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

    stats := bus.Stats()
    fmt.Printf("%d ingested, %d dropped\n", stats.EventsIngested, stats.EventsDropped)
}
```

`Ingest` returns once the event is accepted into the local ring buffer —
acceptance, not delivery (see
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)). Under
backpressure events can drop; check `Stats().EventsDropped`.

`Poll(limit, cursor)` returns a `*PollResponse` with `Events []json.RawMessage` and
a `NextID` cursor — pass `NextID` back to `Poll` to page forward. This is the Go
consumption model; there is no async subscribe iterator (see
[Watch](/docs/sdk/go/watch)).

## The mesh node

```go
node, err := net.NewMeshNode(net.MeshConfig{
    BindAddr: "127.0.0.1:0",
    PSK:      "42424242...",  // 32-byte pre-shared key, hex-encoded
})
```

From here: [Announce](/docs/sdk/go/announce) → [Discover](/docs/sdk/go/discover) →
[Invoke](/docs/sdk/go/invoke).
