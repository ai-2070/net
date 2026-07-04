# Rust — Announce a Capability

A capability is a typed unit of work a node can do. You announce it; every peer
folds the announcement into its local index; anyone can then discover and invoke
it. The ergonomic path for a callable tool is the `#[tool]` macro.

## A tool in one attribute

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

The `#[tool]` attribute derives the tool's JSON Schema (from `JsonSchema`),
generates an atomic register function (`web_search_register`), and captures the
metadata (description, tags, estimated time). Register it on a node and announce:

```rust
let host = MeshBuilder::new("127.0.0.1:0", &PSK)?.build().await?;
let _handle = web_search_register(&host)?;        // registered; unregisters on drop
host.announce_capabilities(Default::default()).await?;
```

After `announce_capabilities`, peers that fold the announcement can discover
`web_search` by tag or list it as a tool. Re-announce to update; the mesh diffs
your last set so steady-state changes cost ~tens of bytes, not a full rebroadcast.

## Capabilities beyond tools

For hardware, models, or free-form tags (not a callable tool), build a
`CapabilitySet` directly:

```rust
use net_sdk::capabilities::CapabilitySet;

let caps = CapabilitySet::new()
    .add_tag("region:eu-west")
    .add_tag("gpu");
host.announce_capabilities(caps).await?;
```

`announce_capabilities_with(caps, ttl, sign)` overrides the default 5-minute TTL.
The full tag/axis model (hardware, software, model, tool, resource-limit
projections) is in [Capabilities](/docs/concepts/capabilities) and
[Capability Schema](/docs/reference/capability-schema).

## Policy: announced ≠ invocable

Announcing a capability makes it **discoverable**, not open. Visibility and
invocation are separate: a credentialed capability can be visible while invocable
only by its owner. That boundary is enforced on invoke, not on announce — see
[Invoke](/docs/sdk/rust/invoke) and, for wrapped MCP tools,
[Wrap an MCP Server](/docs/guides/wrap-mcp-server).

## Next

[Discover](/docs/sdk/rust/discover) — find capabilities from another node.
