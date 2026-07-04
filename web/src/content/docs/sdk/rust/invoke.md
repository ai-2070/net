# Rust — Invoke a Capability

Discovery tells you who *can*; invoking does the work and returns a typed result.
The ergonomic path is `call_tool`; the general path is nRPC (`call_typed`).

## Call a tool

```rust
// `agent` discovered a tool named "web_search" from a peer.
#[derive(serde::Serialize)]
struct WebSearchReq { query: String }
#[derive(serde::Deserialize, Debug)]
struct WebSearchResp { results: Vec<String> }

let resp: WebSearchResp = agent
    .call_tool("web_search", &WebSearchReq { query: "how does the capability fold work".into() })
    .await?;
println!("{:?}", resp);
```

`call_tool` finds a provider for the named tool, makes a typed request, and
deserializes the typed response — the round-trip a `tools/call` becomes on the
mesh. Request and response types are your own structs; the wire is JSON over the
encrypted transport.

## Serve and call over nRPC directly

`call_tool` is sugar over nRPC. When you want request/response without the tool
abstraction — your own service name, deadlines, streaming — use it directly:

```rust
use net_sdk::mesh_rpc::CallOptions;
use std::time::Duration;

// Provider: register a typed handler (returns a ServeHandle; unregisters on drop).
let _h = provider.serve_rpc_typed("summarize", |req: SummarizeReq| async move {
    Ok::<_, String>(SummarizeResp { summary: summarize(&req.text) })
})?;

// Caller: typed call with a deadline.
let resp: SummarizeResp = caller.call_typed(
    provider_node_id,
    "summarize",
    &SummarizeReq { text: "…".into() },
    CallOptions::default().with_deadline(Duration::from_millis(500)),
).await?;
```

Use `call_service_typed("summarize", …)` to let the mesh pick any provider
advertising the service (the basis for failover — see
[Errors](/docs/sdk/rust/errors)). Deadlines, cancellation, and streaming are
covered in [Typed RPC with nRPC](/docs/guides/nrpc).

## Policy: invocation is authorized, discovery is not

Seeing a capability does not grant the right to invoke it. A provider enforces
scope at call time — an owner-only capability rejects a caller outside its scope,
verified against the authenticated caller origin, regardless of who can *see* it.
For wrapped MCP tools this is the owner-scope / consent model in
[Wrap an MCP Server](/docs/guides/wrap-mcp-server) and
[Expose Net as MCP](/docs/guides/expose-net-as-mcp).
