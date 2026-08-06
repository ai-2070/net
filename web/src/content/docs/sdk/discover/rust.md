## Discover it — Rust

### List tools

```rust
// `agent` is a Mesh node handshaked with a host that announced tools.
let tools = agent.list_tools(None);
for t in &tools {
    println!("{} v{}  tags={:?}", t.tool_id, t.version, t.tags);
}
```

`list_tools` takes an optional `&TagMatcher` — pass `None` for everything.

### Watch for changes

```rust
use futures::StreamExt;

for t in agent.list_tools(None) { /* baseline */ }

let mut watch = agent.watch_tools(None, None);   // matcher, staleness ceiling
while let Some(change) = watch.next().await {
    println!("{change:?}");   // added / removed / publisher-count change
}
```

The second argument is the optional `Duration` staleness ceiling. `None` is pure
event-driven. Dropping the stream stops the substrate task.

### Filter nodes by capability

```rust
use net_sdk::capabilities::CapabilityFilter;

let filter = CapabilityFilter {
    require_gpu: true,
    min_vram_gb: Some(24),
    ..Default::default()
};
let nodes: Vec<u64> = mesh.find_nodes(&filter);
```

`find_nodes` is **not async** — it reads the local index and returns node ids
directly. `find_nodes_scoped` narrows to a tenant, region or subnet pool.

### Pick one node

```rust
use net_sdk::capabilities::CapabilityRequirement;

let req = CapabilityRequirement::from_filter(filter).prefer_vram(1.0);
let target: Option<u64> = mesh.find_best_node(&req);
```

`find_best_node` applies the requirement's weights and returns one winner instead
of the whole matching set. Each weight scores one axis of what a candidate
announced about itself — system memory, GPU VRAM, model inference speed in
tokens/sec, and the share of its models already loaded — and each is clamped to
`[0.0, 1.0]`. Ties, including the case where every weight is zero, resolve to
the lowest matching node id.

`None` means nothing matched; node id `0` is a real id, so match on the `Option`
rather than comparing against zero. `find_best_node_scoped` applies a scope
first, so a peer outside the scope cannot win on capacity.

Same local-index read as `find_nodes`: no network, no `await`, and only peers
whose announcements have already arrived.

### Lower a descriptor for an LLM

```rust
use net_sdk::tool::formats::openai;

let lowered: Vec<_> = tools.iter().map(openai::to_openai_tool).collect();
```

`to_openai_tool` produces an entry that drops straight into an OpenAI-compatible
`tools` array. `anthropic`, `mcp` and `gemini` modules sit beside it with the same
shape, and each has a `lower_*` counterpart for parsing the model's reply back
into a call spec.

### Verify it worked

```rust
let tools = agent.list_tools(None);
assert!(
    tools.iter().any(|t| t.tool_id == "web_search"),
    "web_search did not fold — is the pair handshaked and started?",
);
```

The full announce → list → lower → invoke loop is `sdk/examples/tool_calling.rs`,
which runs in CI.

Next: [Invoke a capability](/docs/sdk/rust/invoke).
