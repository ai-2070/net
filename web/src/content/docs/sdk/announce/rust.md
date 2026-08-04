## Announce it — Rust

The ergonomic path for a callable tool is the `#[tool]` attribute.

```rust
use net_sdk::macros::tool;
use net_sdk::mesh::{Mesh, MeshBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchReq {
    /// Free-text query string.
    query: String,
}
#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchResp {
    results: Vec<String>,
}

#[tool(
    description = "Search the web for relevant pages.",
    tag = "web",
    tag = "research",
    estimated_time_ms = 500
)]
async fn web_search(req: WebSearchReq) -> Result<WebSearchResp, String> {
    Ok(WebSearchResp { results: vec![format!("first hit for '{}'", req.query)] })
}
```

`#[tool]` derives the JSON Schema from `JsonSchema`, captures the metadata, and
generates a register function named after the function: `web_search_register`.

### Serve and announce

```rust
let host = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;

let _handle = web_search_register(&host)?;   // registered; unregisters on drop
host.announce_capabilities(Default::default()).await?;
```

**This is the binding where the two steps fuse.** `web_search_register` inserts the
descriptor into the node's tool registry, and `announce_capabilities` merges the
registry into whatever set you pass — which is why `Default::default()` is enough
here and is not enough anywhere else on this spine.

The returned handle unregisters on drop. Binding it to `_handle` rather than `_`
matters: `let _ = web_search_register(&host)?;` drops it immediately and
deregisters the tool you just served.

### Tags without a tool

```rust
use net_sdk::capabilities::CapabilitySet;

let caps = CapabilitySet::new()
    .add_tag("region:eu-west")
    .add_tag("gpu");
host.announce_capabilities(caps).await?;
```

`announce_capabilities_with(caps, ttl, sign)` overrides the five-minute default and
controls signing.

### Verify it worked

Verify from another peer after it folds the announcement:

```rust
// `agent` is handshaked with `host`, per the Quickstart.
let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
while std::time::Instant::now() < deadline && agent.list_tools(None).is_empty() {
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}
assert!(!agent.list_tools(None).is_empty(), "the announcement did not fold");
```

Folding is asynchronous, so the loop is the point — an immediate `list_tools` after
`announce_capabilities` will usually be empty, and that is not a failure.

The full announce → list → lower → invoke loop is `sdk/examples/tool_calling.rs`,
which runs in CI.

Next: [Discover a capability](/docs/sdk/rust/discover).
