## Build it — Go

```bash
go get github.com/ai-2070/net/go
```

The Go binding is cgo over the C ABI. That is the fact behind most of what is
peculiar below: calls cross an FFI boundary, so operations you would expect to be
infallible reads return an `error` too.

### A bus node

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
}
```

`IngestRaw` is the fast path — it hands the JSON straight across without a
marshalling round trip. `Ingest` takes a Go value and marshals it for you.

`Stats()` returns `(*Stats, error)`, not a bare struct. It crosses the cgo
boundary and can fail once the handle is shut down, which is why a counter read
has an error return you would not expect from a counter read.

### A mesh node

```go
node, err := net.NewMeshNode(net.MeshConfig{
    BindAddr: "127.0.0.1:9000",
    PskHex:   "4242424242424242424242424242424242424242424242424242424242424242",
})
if err != nil {
    log.Fatal(err)
}
defer node.Shutdown()
```

Go takes the PSK as `PskHex` — a **64-character hex string**, matching Python and
differing from the raw byte arrays Rust and TypeScript take. The field name says
so, which is more than the other bindings do for theirs.

### The handshake

```go
const hostAddr = "127.0.0.1:9001"

host, err := net.NewMeshNode(net.MeshConfig{BindAddr: hostAddr, PskHex: psk})
if err != nil {
    log.Fatal(err)
}
agent, err := net.NewMeshNode(net.MeshConfig{BindAddr: "127.0.0.1:9000", PskHex: psk})
if err != nil {
    log.Fatal(err)
}

hostPub, err := host.PublicKey()
if err != nil {
    log.Fatal(err)
}
if _, err := host.Accept(agent.NodeID()); err != nil {
    log.Fatal(err)
}
if err := agent.Connect(hostAddr, hostPub, host.NodeID()); err != nil {
    log.Fatal(err)
}
if err := host.Start(); err != nil {
    log.Fatal(err)
}
if err := agent.Start(); err != nil {
    log.Fatal(err)
}
```

`NodeID()` returns a bare `uint64` — it is the one accessor on this path that does
not cross the boundary. `PublicKey()` returns `(string, error)` and `Accept()`
returns `(string, error)` where the string is the peer's wire address.

### Verify it worked

```go
stats, err := bus.Stats()
if err != nil {
    log.Fatal(err)
}
if stats.EventsIngested != 1 {
    log.Fatal("the bus did not accept the event")
}
fmt.Printf("accepted: ingested=%d\n", stats.EventsIngested)
```

Expect one line, `accepted: ingested=1`. The counter is producer-side: this node
accepted the event, which is not a claim that anything consumed it.

Next: [Announce a capability](/docs/sdk/go/announce).
