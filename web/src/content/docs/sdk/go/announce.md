# Go — Announce a Capability

Announce what a node can do; peers fold it into their index and can then discover
and invoke it.

```go
node, err := net.NewMeshNode(net.MeshConfig{BindAddr: "127.0.0.1:0", PSK: psk})
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

`CapabilitySet` carries `Tags` (and hardware/model/tool fields); `AnnounceCapabilities`
returns an `error` like every mesh call. Re-announce to update — the mesh diffs your
last set, so steady-state changes are cheap. The default TTL is 5 minutes.

The full capability shape is in [Capabilities](/docs/concepts/capabilities) and
[Capability Schema](/docs/reference/capability-schema).

## Serving a tool

To serve a callable tool, create a `MeshRpc` over the node and register a handler;
peers discover it via `ListTools` ([Discover](/docs/sdk/go/discover)) and call it
with `net.CallTool` ([Invoke](/docs/sdk/go/invoke)):

```go
rpc, err := net.NewMeshRpc(node)
// register a typed tool handler on `rpc` (returns a ToolServeHandle; Close() to withdraw).
```

The exact serve-handler signature is in the Go binding's tool example; the shape
mirrors the other SDKs (name, description, tags, typed handler).

## Policy: announced ≠ invocable

Announcing makes a capability discoverable, not open. The visible/invocable
boundary is enforced on invoke, not announce — see [Invoke](/docs/sdk/go/invoke).
