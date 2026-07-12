# Go — Discover Capabilities

## Filter nodes by capability

```go
nodes, err := node.FindNodes(net.CapabilityFilter{
    RequireTags: []string{"gpu"},
})
if err != nil {
    log.Fatal(err)
}
// nodes is []uint64 — the node ids that match, right now.
```

`FindNodes` returns `([]uint64, error)` — the matching node ids. Announcements
propagate multi-hop (bounded by a hop count), so a match can be a node several hops
away. Discovery is **advisory** — it tells you who *can*, with no exclusivity.

## List tools

Over a `MeshRpc`, list the tools folded from the mesh:

```go
rpc, err := net.NewMeshRpc(node)
if err != nil {
    log.Fatal(err)
}
tools, err := rpc.ListTools()
if err != nil {
    log.Fatal(err)
}
for _, t := range tools {
    fmt.Printf("%s v%s tags=%v\n", t.ToolID, t.Version, t.Tags)
}
```

Folding is asynchronous — but you don't poll for it. `WatchTools` hands you a
baseline plus a channel of pushed changes, event-driven off the capability fold's
change signal (a `ToolListChange` arrives the moment a tool is added, removed, or
its publisher count changes; an idle mesh costs zero periodic work):

```go
ctx, cancel := context.WithCancel(context.Background())
defer cancel()

changes, errs, baseline, err := net.WatchTools(ctx, rpc, net.WatchOptions{})
if err != nil {
    log.Fatal(err)
}
for _, t := range baseline {
    fmt.Printf("%s v%s tags=%v\n", t.ToolID, t.Version, t.Tags)
}
for {
    select {
    case c := <-changes:
        fmt.Println(c) // pushed on fold mutation — no ticker, no re-diff
    case err := <-errs:
        log.Println("watch:", err)
    case <-ctx.Done():
        return
    }
}
```

`WatchOptions.Interval` is a client-side staleness ceiling (a safety-net re-diff at
least that often), **not** a poll rate — leave it zero for pure event-driven
behavior.
