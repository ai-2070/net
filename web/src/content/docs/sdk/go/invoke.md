# Go — Invoke a Capability

Call a discovered tool with the generic `net.CallTool`, which is typed on the
request and response:

```go
import "context"

type WebSearchReq struct {
    Query string `json:"query"`
}
type WebSearchResp struct {
    Results []string `json:"results"`
}

rpc, err := net.NewMeshRpc(node)
if err != nil {
    log.Fatal(err)
}

resp, err := net.CallTool[WebSearchReq, WebSearchResp](
    context.Background(),
    rpc,
    "web_search",
    WebSearchReq{Query: "how does the capability fold work"},
)
if err != nil {
    log.Fatal(err)
}
fmt.Println(resp.Results)
```

`CallTool[Req, Resp]` finds a provider for the named tool, sends a typed request,
and decodes the typed response. `CallToolStreaming[Req]` returns a stream for
tools that emit multiple chunks. For raw request/response without the tool
abstraction, `rpc.Call(...)` is the underlying nRPC surface — see
[Typed RPC with nRPC](/docs/guides/nrpc).

## Policy: invocation is authorized, discovery is not

Seeing a capability does not grant the right to invoke it. A provider enforces
scope at call time — an owner-only capability rejects a caller outside its scope,
regardless of who can see it. For wrapped MCP tools this is the owner-scope /
consent model in [Wrap an MCP Server](/docs/guides/wrap-mcp-server).
