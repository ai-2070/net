# AI Tool Calling on net

`net-mesh` exposes a typed AI-tool calling surface: agents discover
tools advertised by mesh nodes, invoke them by name with JSON request
payloads, and receive either a typed JSON response (unary) or a stream
of `ToolEvent` envelopes (streaming). Tools are first-class
`nrpc:<tool_id>` services with an `ai-tool:<tool_id>` capability tag
on top.

This doc covers the **Rust SDK** surface that landed in Wave 2 of
the AI tool-calling work. Bindings (Node / Python / Go) and format
translators (OpenAI / Anthropic / Gemini / MCP) are tracked in
`docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`.

## Quickstart

```rust
use net_sdk::mesh::MeshBuilder;
use net_sdk::tool::metadata_for;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mesh = MeshBuilder::new("127.0.0.1:0", &[0u8; 32])?.build().await?;

    // Build a descriptor: name + Rust types → JSON Schemas via
    // `schemars`, plus the fluent setters for the fields not
    // derivable from the type signature.
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
        .description("Search the web for relevant pages.")
        .stateless(true)
        .estimated_time_ms(500)
        .tag("web")
        .tag("research")
        .build();

    // Atomically register: handler + tool registry + capability tag.
    // Drop the handle to deregister everything.
    let _handle = mesh.serve_tool::<WebSearchReq, WebSearchResp, _, _>(
        descriptor,
        |req| async move {
            Ok(WebSearchResp {
                results: vec![format!("hit for {}", req.query)],
            })
        },
    )?;

    // Announce so peers' capability folds pick up the tool.
    mesh.announce_capabilities(Default::default()).await?;

    // … keep the mesh alive …
    Ok(())
}
```

On the caller side:

```rust
use net_sdk::tool::{TagMatcher, ToolDescriptor};

// One in-memory walk of the local capability fold; no network.
let tools: Vec<ToolDescriptor> = mesh.list_tools(None);

// Capability-routed call. JSON codec under the hood.
let resp: WebSearchResp = mesh
    .call_tool("web_search", &WebSearchReq { query: "mesh".into() })
    .await?;
```

## Concepts

### One identifier — three roles

A tool's `tool_id` is simultaneously:

1. The nRPC service name (`nrpc:<tool_id>.requests` channel),
2. The AI-tool capability tag (`ai-tool:<tool_id>`),
3. The unary-call service the underlying `serve_rpc_typed` is keyed on.

No mapping table. A tool registered as `web_search` IS the nRPC service
at `nrpc:web_search` IS the announcement carrying `ai-tool:web_search`.
This is locked decision #1 of the plan.

### JSON codec everywhere

Every tool request and response is JSON-encoded. Every `ToolEvent`
envelope is one JSON chunk on the streaming path. This is locked
decision #3: every LLM provider (OpenAI, Anthropic, Gemini, MCP)
consumes JSON for tool input/output, so the substrate enforces one
codec for the whole tool surface.

### Schema format = JSON Schema draft 2020-12

The `metadata_for::<Req, Resp>(name)` helper derives input/output
schemas via the `schemars` crate. Both schemas land on the
`ToolDescriptor` as JSON-encoded strings. Format translators
(`formats/openai`, `formats/anthropic`, …) consume the schemas
verbatim or lower them to per-provider shapes.

### `ToolEvent` streaming envelope

Streaming tools emit one `ToolEvent` per chunk:

| Variant       | Direction | Purpose                                         |
|---------------|-----------|-------------------------------------------------|
| `Start`       | first     | `tool_id`, optional `call_id`, optional metadata |
| `Progress`    | mid       | Optional `pct` + `message` for spinners         |
| `Delta`       | mid       | Partial output (token, file chunk, log line)    |
| `Result`      | terminal  | Final result payload                            |
| `Error`       | terminal  | Structured failure: `code`, `message`, `details`|

Every stream ends with **exactly one** terminal event (`Result` or
`Error`). If a handler ends its stream without one, the SDK's
streaming wrapper synthesizes `ToolEvent::Error { code: "missing_terminal", ... }`
so callers can rely on the contract.

Unary tools (`serve_tool` / `call_tool`) bypass the envelope: the wire
shape is just the typed `Resp` bytes. Adapter packages synthesize a
single `Result` envelope locally when lowering a unary call into a
provider's streaming protocol.

## Server side

### Unary: `Mesh::serve_tool`

```rust
let handle = mesh.serve_tool::<Req, Resp, _, _>(descriptor, handler)?;
```

Atomically performs four steps:

1. Insert the descriptor into the local `tool_registry` (so the next
   `announce_capabilities` auto-emits the `ai-tool:<id>` tag + the
   typed `ToolCapability` + description/streaming/tags metadata keys).
2. Register the handler via `serve_rpc_typed` at `tool_id` with JSON codec.
3. On the FIRST `serve_tool` call to this `Mesh`, lazy-install the
   `tool.metadata.fetch` nRPC service handler so agents can pull full
   descriptors for tools whose schemas were too large for the fold.
4. Return a `ToolServeHandle`. Drop reverses steps 1 + 2.

Duplicate registrations of the same `tool_id` return
`ServeError::AlreadyServing(tool_id)`. The prior handler's registry
entry is preserved.

### Streaming: `Mesh::serve_tool_streaming`

```rust
let handle = mesh.serve_tool_streaming::<Req, _, _, _>(descriptor, |req| async move {
    futures::stream::iter(vec![
        ToolEvent::Start { tool_id: "web_search".into(), call_id: None, metadata: None },
        ToolEvent::Progress { pct: Some(50.0), message: Some("indexing".into()) },
        ToolEvent::Delta { data: serde_json::json!({ "token": "result " }) },
        ToolEvent::Result { data: serde_json::json!({ "results": ["hit"] }) },
    ])
})?;
```

Contract:

- The handler returns `impl Future<Output = impl Stream<Item = ToolEvent>>`.
- The SDK serializes each item as one JSON chunk on the underlying
  `serve_rpc_streaming_typed` path with `Resp = ToolEvent`.
- Stop on the first terminal event — the SDK does not drain past it.
- Missing-terminal synthesis ensures every stream closes cleanly.
- `descriptor.streaming` is forced to `true` on register so announce
  metadata reflects reality.

## Discovery

### `Mesh::list_tools`

```rust
// All tools the local fold has seen.
let tools = mesh.list_tools(None);

// Region-scoped: only EU hosts.
let matcher = TagMatcher::Prefix { value: "region.eu".into() };
let eu_tools = mesh.list_tools(Some(&matcher));
```

One in-memory walk of the capability fold. For each
`(class, NodeId) → CapabilityMembership` entry:

1. Pre-filter by the optional `TagMatcher` — entry must have ANY tag
   matching.
2. Decode `software.tool.<i>.*` tags into `Vec<ToolCapability>`.
3. Hydrate input/output schemas from
   `tool::<id>::input_schema` / `output_schema` metadata keys.
4. Build `ToolDescriptor::from_capability(cap, metadata)`.
5. Dedupe by `(tool_id, version)`; accumulate `node_count`.

Returns a `Vec<ToolDescriptor>` sorted by `(tool_id, version)` for
stable snapshots.

### `Mesh::watch_tools`

```rust
let mut changes = mesh.watch_tools(None, Some(Duration::from_millis(250)));
while let Some(change) = changes.next().await {
    match change {
        ToolListChange::Added(desc) => println!("+ {}", desc.tool_id),
        ToolListChange::Removed(desc) => println!("- {}", desc.tool_id),
        ToolListChange::NodeCountChanged { descriptor, prev_node_count } => {
            println!("~ {}: {} -> {}", descriptor.tool_id, prev_node_count, descriptor.node_count);
        }
    }
}
```

Backed by a polling task: every `interval` (default `1s`), the task
re-runs `list_tools(&matcher)` and diffs against the prior snapshot.
The first event fires AFTER the initial baseline — call `list_tools`
first if you need the starting shape.

The initial baseline is taken **synchronously** before the task
spawns so a subscribe-then-publish call sequence never loses the
`Added` event to a race.

Dropping the `ToolListWatch` ends the polling task on its next tick.

### `tool.metadata.fetch` — oversized schemas

The capability fold has a per-entry payload budget. Tools with large
JSON schemas (multi-KB Pydantic-derived shapes, deep nested Zod
output) may have their `input_schema` / `output_schema` dropped on
the fold path; the resulting descriptor lands with `input_schema: None`.

Agents that need the full schema for strict-mode adapters
(OpenAI's `strict: true`, Anthropic's tool blocks, MCP's
`inputSchema`) call the auto-installed `tool.metadata.fetch` nRPC
service against the publishing host:

```rust
let resp: ToolMetadataResponse = mesh.call_typed(
    host_node_id,
    TOOL_METADATA_FETCH_SERVICE,
    &ToolMetadataRequest { name: "web_search".into() },
    CallOptionsTyped { codec: Codec::Json, raw: Default::default() },
).await?;
match resp {
    ToolMetadataResponse::Found { descriptor } => { /* use full schema */ }
    ToolMetadataResponse::NotFound { name } => { /* host doesn't serve this tool */ }
}
```

The host's `tool.metadata.fetch` handler is lazy-installed on the
first `serve_tool` call and stays alive for the `Mesh` lifetime;
empty registries just return `NotFound` for every request.

## Client side

### Unary: `Mesh::call_tool`

```rust
let resp: WebSearchResp = mesh
    .call_tool("web_search", &WebSearchReq { query: "mesh".into() })
    .await?;
```

Capability-routed: consults the local fold for nodes advertising
`nrpc:web_search`, picks one per the default routing policy, calls.
Returns `RpcError::NoRoute` if no host currently serves the tool.
Bubbles handler errors as `RpcError::ServerError` with status
`NRPC_TYPED_HANDLER_ERROR` carrying the handler's error message.

### Streaming: `Mesh::call_tool_streaming`

```rust
let stream = mesh
    .call_tool_streaming("web_search_stream", &WebSearchReq { query: "mesh".into() })
    .await?;
let events: Vec<ToolEvent> = stream.map(|item| item.unwrap()).collect().await;
```

Returns `RpcStreamTyped<ToolEvent>`. Implements `futures::Stream`.
Caller surfaces wire events verbatim; adapter packages
(`formats/anthropic`, `formats/openai`, etc.) own the contract
enforcement (e.g. lowering `Delta` envelopes to provider streaming
protocols, accumulating partial JSON on Anthropic
`tool_use_block_delta`, etc.).

Dropping the stream emits CANCEL to the server (substrate cancel-token
contract).

## Capability scoping

Tool discovery rides the existing capability fold, so the same
filters and scope mechanisms apply:

- **Region scoping**: `TagMatcher::Prefix { value: "region.eu" }` —
  see Discovery above.
- **Subnet visibility**: callers in a subnet only discover tools
  advertised within their subnet (same as any other capability).
- **Capability auth**: a tool's host can populate `allowed_nodes` /
  `allowed_subnets` / `allowed_groups` on its `CapabilitySet` to gate
  invocation. Off-mesh callers fail the auth check before the handler
  runs.
- **Predicate-pushdown**: `CallOptions::with_where(&pred)` rides the
  `net-where` request header to push per-call predicate filtering.

## Atomicity

`Mesh::serve_tool` is atomic with respect to observable mesh state:
either all of (handler registration, capability-fold tag publish,
`nrpc:<id>` + `ai-tool:<id>` tags, descriptor in `tool_registry`)
succeed, or none do.

- Step 1 failure (duplicate `tool_id`): nothing changed.
- Step 2 failure (`serve_rpc_typed` returned `Err`): step 1's
  registry insert is paired-removed before the error returns.
- Drop reverses steps 1 + 2 in order. The lazy
  `tool.metadata.fetch` handler from step 3 stays installed for the
  `Mesh` lifetime (idempotent; harmless when the registry is empty).

## Wire compatibility

No wire-ABI bump for unary tool calls — they ride the existing
`call_service` + `serve_rpc_typed` path. Streaming tools use the new
substrate primitive `MeshNode::call_service_streaming` (S-1); the
wire shape of an individual stream is unchanged from `call_streaming`.
`ToolEvent` envelopes are JSON-encoded chunks on existing streams.

Receiving binaries WITHOUT the `tool` Cargo feature still apply
inbound tool announcements correctly: the `software.tool.<i>.*` tags
and the `tool::<id>::*` metadata keys ride through the standard
`CapabilitySet` shape. They just don't have a `list_tools` /
`call_tool` surface to consume them with.

## Cargo features

- `net-mesh/tool` enables substrate-side `ToolDescriptor`,
  `ToolMetadataRegistry`, `MeshNode::list_tools` / `watch_tools`, the
  announce-time merge from the registry into `CapabilitySet`, and the
  `ToolEvent` wire type.
- `net-mesh-sdk/tool` enables the SDK-facing wrappers:
  `Mesh::serve_tool` / `serve_tool_streaming` / `list_tools` /
  `watch_tools` / `call_tool` / `call_tool_streaming` plus the
  `ToolMetadataBuilder` + `metadata_for` schema helpers (pulls in
  `schemars`).

Both gated off by default. Bindings that consume `net-mesh-sdk`
through prebuilt artifacts (Node / Python / Go) enable the feature in
their build pipeline.

## Plan reference

See `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md` for the full
slice-by-slice plan, locked design decisions, and Wave 3 / 4 follow-
ups (bindings + format translators).
