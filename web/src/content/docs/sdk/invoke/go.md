## Invoke it — Go

### Call a tool

```go
import "context"

type WebSearchReq struct {
    Query string `json:"query"`
}
type WebSearchResp struct {
    Results []string `json:"results"`
}

raw, err := net.NewMeshRpc(node)
if err != nil {
    log.Fatal(err)
}
rpc := net.NewTypedMeshRpc(raw)

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

`CallTool[Req, Resp](ctx, rpc, toolID, request)` takes the **`*TypedMeshRpc`**.
`NewMeshRpc` gives you a `*MeshRpc`; the extra `NewTypedMeshRpc` wrap is not
optional and the compiler will say so.

`CallToolStreaming[Req]` returns a `*ToolEventStream` for tools that emit multiple
chunks — drain it with `Recv()` until `ok` is false.

### Deadlines are the context

Go is the only binding where the deadline is not an option field. It is the
`context.Context` you already have:

```go
ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
defer cancel()

resp, err := net.CallTool[WebSearchReq, WebSearchResp](ctx, rpc, "web_search", req)
```

Cancelling the context mid-stream closes the underlying stream and emits CANCEL on
the wire, so `ctx` is the cancellation path as well as the deadline.

### Serve and call over nRPC directly

```go
handle, err := net.TypedServe[SummarizeReq, SummarizeResp](
    rpc, "summarize",
    func(req SummarizeReq) (SummarizeResp, error) {
        return SummarizeResp{Summary: req.Text[:min(40, len(req.Text))]}, nil
    },
)
if err != nil {
    log.Fatal(err)
}
defer handle.Close()

resp, err := net.TypedCall[SummarizeReq, SummarizeResp](
    ctx, rpc, providerNodeID, "summarize", SummarizeReq{Text: "…"},
)
```

`TypedCall` pins a node id; `TypedCallService` resolves by service name through the
capability index, which is what makes failover to a standby possible. `CallTool`
is a thin wrapper over `TypedCallService`.

### A typed application error

```go
body := []byte(`{"error":"invalid_request"}`)
return SummarizeResp{}, net.AppError(net.NrpcTypedBadRequest, body)
```

`AppError(code, body)` is how a handler returns a typed application status rather
than an opaque failure — the caller sees the code and the body. **Any other error
a handler returns surfaces as `Internal`**, so a handler that returns a bare
`fmt.Errorf` has thrown away the distinction the caller needs to decide whether to
retry.

`TypedServe` already does this for you on a request that will not unmarshal: the
caller gets `Application(NrpcTypedBadRequest)` with
`{"error":"invalid_request","detail":…}`, which is the same contract every binding
on this spine implements.

### Verify it worked

```go
resp, err := net.CallTool[WebSearchReq, WebSearchResp](
    ctx, rpc, "web_search", WebSearchReq{Query: "ping"},
)
if err != nil {
    log.Fatal(err)
}
if len(resp.Results) == 0 {
    log.Fatal("the provider answered, but with nothing")
}
fmt.Printf("invoked: %d result(s)\n", len(resp.Results))
```

The Go binding has no live-mesh `RegisterTool` + `CallTool` test yet — the
descriptor, merge and codec helpers are unit-tested and the round trip is not. Run
this against a Rust or TypeScript provider if you want the strongest signal.

Next: [Watch the event stream](/docs/sdk/go/watch).
