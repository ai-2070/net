# net-mesh-sdk-macros

Procedural macros for [`net-mesh-sdk`](../sdk). Currently ships the
`#[tool]` attribute macro — the Rust equivalent of the `@tool`
decorators in the Node / Python bindings.

Use through the SDK's `macros` feature:

```toml
net-mesh-sdk = { version = "0.24", features = ["tool", "macros"] }
```

Then:

```rust
use net_sdk::tool::{self};
use net_sdk::macros::tool;

#[tool(
    description = "Search the web for relevant pages.",
    tag = "web",
    tag = "research",
    stateless = true,
    estimated_time_ms = 500,
)]
async fn web_search(req: WebSearchReq) -> Result<WebSearchResp, String> {
    Ok(WebSearchResp { results: vec![] })
}

// Register on a Mesh — the macro generates `web_search_register(&mesh)`.
let handle = web_search_register(&mesh)?;
```

See the crate-level rustdoc in `src/lib.rs` for the full attribute
surface (`name`, `description`, `version`, `tag` (repeatable),
`stateless`, `estimated_time_ms`).

Plan: A-7 of
[`docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`](../docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md).
