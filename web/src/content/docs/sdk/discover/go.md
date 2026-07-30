## Discover it — Go

### Filter nodes by capability

```go
nodes, err := node.FindNodes(net.CapabilityFilter{
    RequireTags: []string{"gpu"},
    MinVRAMGB:   24,
})
if err != nil {
    log.Fatal(err)
}
// nodes is []uint64 — the node ids matching right now.
```

`FindNodes` returns `([]uint64, error)`. The error is not about the query — it is
the cgo boundary, which every Go call on this page crosses.

### List tools

```go
raw, err := net.NewMeshRpc(node)
if err != nil {
    log.Fatal(err)
}
rpc := net.NewTypedMeshRpc(raw)

tools, err := raw.ListTools()          // on the raw handle
if err != nil {
    log.Fatal(err)
}
for _, t := range tools {
    fmt.Printf("%s v%s tags=%v\n", t.ToolID, t.Version, t.Tags)
}
```

Two handles, two surfaces: `ListTools()` is a method on `*MeshRpc`, while
`WatchTools` and every generic tool function take the `*TypedMeshRpc` built over
it. `net.ListTools(rpc)` is the package-level form that takes the typed one, so
either works — pick one and stay with it.

### Watch for changes

```go
ctx, cancel := context.WithCancel(context.Background())
defer cancel()

changes, errs, baseline, err := net.WatchTools(ctx, rpc, net.WatchOptions{})
if err != nil {
    log.Fatal(err)
}
for _, t := range baseline {
    fmt.Printf("baseline %s v%s\n", t.ToolID, t.Version)
}
for {
    select {
    case c := <-changes:
        fmt.Println(c)         // pushed on fold mutation — no ticker
    case err := <-errs:
        log.Println("watch:", err)
    case <-ctx.Done():
        return
    }
}
```

Go is the only binding that hands you the baseline **from the watch call itself**,
taken before the watch opens — so there is no window between snapshot and
subscription to reason about. Elsewhere you take the baseline yourself and rely on
the eager subscribe.

`WatchOptions.Interval` is a `time.Duration` staleness ceiling. Leave it zero for
pure event-driven behaviour.

### Verify it worked

```go
tools, err := raw.ListTools()
if err != nil {
    log.Fatal(err)
}
found := false
for _, t := range tools {
    if t.ToolID == "web_search" {
        found = true
    }
}
if !found {
    log.Fatal("web_search did not fold — is the pair handshaked and started?")
}
```

Expect the descriptor to arrive **without its schemas** if the tool was announced
from Go — see the seam described in [Announce](/docs/sdk/go/announce). A tool
announced from Rust, TypeScript or Python and discovered from Go carries them.

Next: [Invoke a capability](/docs/sdk/go/invoke).
