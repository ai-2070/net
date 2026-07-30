## Announce it — Go

### Tags

```go
node, err := net.NewMeshNode(net.MeshConfig{BindAddr: "127.0.0.1:9001", PskHex: psk})
if err != nil {
    log.Fatal(err)
}
defer node.Shutdown()

err = node.AnnounceCapabilities(net.CapabilitySet{
    Tags: []string{"gpu", "inference", "region:eu-west"},
})
if err != nil {
    log.Fatal(err)
}
```

`CapabilitySet` carries `Tags` plus `Hardware`, `Software`, `Models`, `Tools` and
`Limits`. `AnnounceCapabilities` returns an `error` like every call that crosses
the cgo boundary.

### Serve a tool

The tool API takes a `*TypedMeshRpc`, and getting one is two constructors:

```go
raw, err := net.NewMeshRpc(node)          // *MeshRpc
if err != nil {
    log.Fatal(err)
}
rpc := net.NewTypedMeshRpc(raw)           // *TypedMeshRpc — what the tool API wants

type WebSearchReq struct {
    Query string `json:"query"`
}
type WebSearchResp struct {
    Results []string `json:"results"`
}

desc, err := net.DescriptorFor(net.ToolOptions{
    Name:        "web_search",
    Description: "Search the web for relevant pages.",
    Tags:        []string{"web", "research"},
})
if err != nil {
    log.Fatal(err)
}

handle, err := net.RegisterTool[WebSearchReq, WebSearchResp](
    rpc,
    desc,
    func(req WebSearchReq) (WebSearchResp, error) {
        return WebSearchResp{Results: []string{"first hit for '" + req.Query + "'"}}, nil
    },
)
if err != nil {
    log.Fatal(err)
}
defer handle.Close()
```

`NewMeshRpc` returns `*MeshRpc`; every generic tool function
(`RegisterTool`, `CallTool`, `WatchTools`) wants `*TypedMeshRpc`. Passing the raw
one is a compile error, and it is the most common way this page goes wrong.

### Announcing the tool — and where Go currently stops

`AddToolCapabilitiesToAnnounce` builds the merged announcement:

```go
wire := net.AddToolCapabilitiesToAnnounce(
    net.CapabilitySetWire{}, []net.ToolDescriptor{desc},
)
```

**Then there is a seam.** `wire` is a `CapabilitySetWire` — `Tags` plus a flat
`Metadata map[string]string` holding the `tool::<id>::input_schema` keys peers
hydrate schemas from. `AnnounceCapabilities` takes a `CapabilitySet`, which has no
`Metadata` field, and nothing else in the binding consumes a `CapabilitySetWire`.

So what you can do today from Go is announce the discovery tags:

```go
if err := node.AnnounceCapabilities(net.CapabilitySet{Tags: wire.Tags}); err != nil {
    log.Fatal(err)
}
```

That carries `ai-tool:web_search`, so peers can find the tool by tag and route a
call to it. What it does not carry is the schema metadata, so a peer's
`ListTools()` sees the tool without its input and output schemas — enough to
invoke if the caller already knows the shape, not enough to hand an LLM a
tool definition.

This page states that rather than showing a call that does not compile. The Go
binding has no live-mesh `RegisterTool` + `CallTool` test yet; the descriptor and
merge helpers are unit-tested, the round trip is not.

### Verify it worked

From a peer that folded the announcement:

```go
peers, err := agent.FindNodes(net.CapabilityFilter{
    RequireTags: []string{"ai-tool:web_search"},
})
if err != nil {
    log.Fatal(err)
}
if len(peers) == 0 {
    log.Fatal("the announcement did not fold")
}
```

Checking the tag rather than `ListTools()` is deliberate: the tag is what Go
actually announced, so it is what Go can actually verify.

Next: [Discover a capability](/docs/sdk/go/discover).
