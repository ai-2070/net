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

`FindNodes` returns `([]uint64, error)` — the matching node ids. Announcements reach
every **directly-connected** peer (the announcing node also self-indexes) — multi-hop
propagation is deferred, so a match is a direct neighbour, not a node several hops
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

Folding is asynchronous — poll `ListTools` until the tool you expect appears rather
than assuming it's there on the first call. For a long-running agent, prefer the
event-driven `WatchTools` subscription, which delivers changes as they fold in
instead of re-scanning on a timer.
