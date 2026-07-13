# Rust — Discover Capabilities

Query the mesh by *what you need*, not by who has it. Two surfaces: list tools, or
filter nodes by capability.

## List tools

After a peer announces tools, they fold into your local index. Walk them:

```rust
// `agent` is a Mesh node handshaked with the host that announced tools.
let tools = agent.list_tools(None);            // Vec of tool descriptors
for t in &tools {
    println!("{} v{}  tags={:?}", t.tool_id, t.version, t.tags);
}
```

Folding is asynchronous — an announcement takes a moment to propagate. Poll until
the tool you expect appears rather than assuming it's there on the first line:

```rust
use std::time::{Duration, Instant};
let deadline = Instant::now() + Duration::from_secs(3);
while Instant::now() < deadline && agent.list_tools(None).len() < 1 {
    tokio::time::sleep(Duration::from_millis(20)).await;
}
```

For a long-running agent, prefer the push-based subscription `agent.watch_tools(..)`
over a poll loop — it yields tool changes as they fold in, instead of re-scanning on
a timer. The poll above is fine for a one-shot wait at startup.

Tool descriptors lower to provider tool-call formats — e.g.
`net_sdk::tool::formats::openai::to_openai_tool(&t)` produces an entry you can drop
straight into an OpenAI-compatible `tools` array. The full loop (announce → list →
lower → invoke) is `sdk/examples/tool_calling.rs`.

## Filter nodes by capability

For hardware/model/tag placement, filter the capability index. `find_nodes` is
**synchronous** and returns matching node ids:

```rust
use net_sdk::capabilities::CapabilityFilter;

let filter = CapabilityFilter {
    require_gpu: true,
    min_vram_gb: Some(24),
    ..Default::default()
};
let nodes: Vec<u64> = mesh.find_nodes(&filter);   // not async — returns node ids
```

`find_best_node` returns a single highest-scoring node for a weighted requirement,
and `find_nodes_scoped` narrows to a tenant/region/subnet pool. Announcements reach
every **directly-connected** peer (and self-index locally) — multi-hop propagation
is deferred, so a match is a direct neighbour, not a node several hops away.
Richer predicates (numeric, semver, AND/OR/NOT) and the CLI equivalent
(`net cap query --tag …`) are in [Capabilities](/docs/concepts/capabilities).

Discovery is **advisory** — it tells you who *can*, with no exclusivity. To
atomically claim a contended exclusive resource, that's the scheduler, not
`find_nodes`.
