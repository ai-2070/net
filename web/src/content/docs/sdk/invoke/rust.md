## Invoke it — Rust

### Call a tool

```rust
#[derive(serde::Serialize)]
struct WebSearchReq { query: String }
#[derive(serde::Deserialize, Debug)]
struct WebSearchResp { results: Vec<String> }

let resp: WebSearchResp = agent
    .call_tool("web_search", &WebSearchReq {
        query: "how does the capability fold work".into(),
    })
    .await?;
println!("{resp:?}");
```

`call_tool` is a method on the `Mesh` node. It finds a provider for the named
tool, sends the typed request, and deserializes the reply. Request and response
are your own types; the wire is JSON over the encrypted transport.

### Serve and call over nRPC directly

```rust
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec};
use std::time::{Duration, Instant};

// Provider: three arguments — service name, codec, handler.
let _h = provider.serve_rpc_typed::<SummarizeReq, SummarizeResp, _, _>(
    "summarize",
    Codec::default(),
    |req: SummarizeReq| async move {
        Ok(SummarizeResp { summary: summarize(&req.text) })
    },
)?;

// Caller: typed call with a deadline.
let mut opts = CallOptionsTyped::default();
opts.raw.deadline = Some(Instant::now() + Duration::from_millis(500));

let resp: SummarizeResp = caller
    .call_typed(provider_node_id, "summarize", &SummarizeReq { text: "…".into() }, opts)
    .await?;
```

Two shapes worth reading twice. **`serve_rpc_typed` takes a `Codec` between the
service name and the handler** — it is not a two-argument call. And **the deadline
is an `Instant`, not a `Duration`**: it lives on `CallOptionsTyped::raw.deadline`
and is an absolute point in time, so it is `Instant::now() + d` rather than `d`.
There is no `with_deadline` builder on `CallOptions`.

`_h` matters as much here as on the announce page — the handle deregisters on
drop, so binding it to `_` unserves the handler immediately.

### Let the mesh pick the provider

```rust
let resp: SummarizeResp = caller
    .call_service_typed("summarize", &req, opts)
    .await?;
```

`call_service_typed` routes by service name through the capability index, which is
what makes failover to a standby possible. `CallOptions::routing_policy` chooses
between round-robin and lowest-latency selection.

### Verify it worked

```rust
let resp: WebSearchResp = agent.call_tool("web_search", &req).await?;
assert!(!resp.results.is_empty(), "the provider answered, but with nothing");
println!("invoked: {} result(s)", resp.results.len());
```

A successful typed invocation is the strongest claim on this page: it proves the
handshake, the announcement, the fold, the route and the codec all worked.

The end-to-end version is `sdk/examples/tool_calling.rs`, which runs in CI.

Next: [Watch the event stream](/docs/sdk/rust/watch).
