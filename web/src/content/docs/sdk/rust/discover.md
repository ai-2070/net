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

Folding is asynchronous — an announcement takes a moment to propagate — but you
don't poll for it. `watch_tools` returns a `futures::Stream` of `ToolListChange`
events, pushed the moment the capability fold mutates (an idle mesh costs zero
periodic work). Take a `list_tools` baseline first; the stream emits only changes
after subscription:

```rust
use futures::StreamExt;

for t in agent.list_tools(None) { /* baseline */ }

let mut watch = agent.watch_tools(None, None); // event-driven; no timer
while let Some(change) = watch.next().await {
    println!("{change:?}"); // added / removed / publisher-count change
}
```

The second argument is an optional staleness ceiling (a safety-net re-diff at
least every `Duration`), **not** a poll rate — pass `None` for pure event-driven
behavior. Dropping the stream (or calling `cancel`) stops the substrate task.

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
and `find_nodes_scoped` narrows to a tenant/region/subnet pool. Announcements
propagate multi-hop (bounded by a hop count), so a match can be several hops away.
Richer predicates (numeric, semver, AND/OR/NOT) and the CLI equivalent
(`net cap query --tag …`) are in [Capabilities](/docs/concepts/capabilities).

Discovery is **advisory** — it tells you who *can*, with no exclusivity. To
atomically claim a contended exclusive resource, that's the scheduler, not
`find_nodes`.
