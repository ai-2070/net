# nRPC AI Tool-Calling Surface (`@tool`-decorated services + mesh-native discovery)

Branch: `nrpc-ai-tools` (suggested).
Predecessors:
- [`NRPC_STREAMING_PARITY_AND_GO_BINDING.md`](./NRPC_STREAMING_PARITY_AND_GO_BINDING.md) — typed nRPC surface (`TypedMeshRpc.call<Req,Resp>`, `serve<Req,Resp>`).
- [`NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md`](./NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md) — `Mesh::reserve_cancel_token` / `Mesh::cancel(token)` substrate primitive.
- [`PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md`](./PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md) — `AsyncMeshRpc` + `AsyncTypedMeshRpc`.
- Aggregator + fold layer for cross-subnet discovery (v0.22).

## Scope

Make every typed nRPC service usable as an **AI tool** (LLM function-calling target) without bolting on a second protocol. The agent author writes `@tool def web_search(req: WebSearchReq) -> WebSearchResp`, the mesh handles registration, schema derivation, gossip, discovery, dispatch, streaming, and cancellation. Cross-language transparently: a Python agent calls a Go-hosted tool through the existing nRPC wire format.

Out of scope:
- Inventing a non-nRPC tool wire protocol. Every tool call rides `TypedMeshRpc::call<Req, Resp>`.
- LLM provider SDKs as core deps. OpenAI / Anthropic / Gemini adapters live in companion packages (`@net-mesh/openai-tools`, `net-mesh-anthropic-tools` on pip, etc.).
- Tool-authoring DSLs separate from the existing typed-handler shape. `@tool` is a thin decorator on top of `serve` + a metadata sidecar.
- LLM inference itself. Net is the bus; agents bring their own client.

## Why now

1. **The typed nRPC surface is the natural fit.** Tool calling is "send a JSON object to a named handler, await a JSON response." nRPC already does this for every binding. The only gap is metadata: tools need a description + parameter schema the model can read. Adding that as a thin layer over `TypedMeshRpc` reuses every existing surface (typed wrappers, observer, cancellation, streaming).

2. **The mesh's discovery model already matches the agent's mental model.** An agent wants to ask "what tools can I call?" and get a list. The mesh's capability index already answers "what nodes serve service X?" Each tool becomes a capability tag (`ai-tool:<name>`), and `mesh.list_tools()` walks the same index `find_service_nodes` walks today. Subnet visibility, capability auth, region filtering — all reused, not reinvented.

3. **Streaming + cancellation are already substrate primitives.** Tools that emit progress (a long-running compute task, a multi-step plan) ride server-streaming or duplex nRPC. The substrate's `cancel_token` (v3) propagates "model decided to stop" all the way to the tool. No new plumbing.

4. **Cross-language tool calls are the killer feature.** A Python LangGraph agent calling a Go-hosted database tool calling a Node-hosted browser tool — all transparent over the existing wire. Every other tool-calling system (MCP, function-calling SDKs, Modal's `cls`) needs a separate transport. Net already has the transport; we just need the convention.

5. **The DX gap is huge today.** Without this layer, an agent author has to: define the Pydantic model, register an nRPC service, hand-write the JSON schema, hand-roll the discovery loop, hand-build the OpenAI/Anthropic tools array. Six steps where one decorator should do.

## Locked decisions

Six decisions every slice codes against:

1. **Tool name == nRPC service name.** No separate tool-name registry. A tool registered as `web_search` IS the nRPC service at channel `nrpc:web_search.requests`. Capability tag is `ai-tool:web_search`. One namespace.

2. **Schema format = JSON Schema draft 2020-12.** OpenAI, Anthropic, and Gemini all accept it (with provider-specific subset constraints, but the canonical form is portable). The adapters in M-slices do per-provider lowering; the core wire shape is one schema.

3. **Metadata transport = a single companion RPC service `tool.metadata`.** Each node serving tools auto-exposes this. Agents call `mesh.list_tools()` → `find_service_nodes("tool.metadata")` → fan-out RPC call → merge results → return a flat `[ToolMetadata]`. Schemas don't get gossiped on capability channels (they're too big and they change too infrequently for the gossip path to be worth it); they're pulled on demand.

4. **`@tool` decorator opt-in, not implicit.** Plain `rpc.serve("x", handler)` continues to register a service WITHOUT the `ai-tool:*` tag — invisible to `list_tools()`. The decorator (or its equivalent in each binding) is what makes a service a tool. Operators retain control over which nRPC services agents see.

5. **Schema derivation is per-binding-idiomatic.** Python uses `pydantic.BaseModel.model_json_schema()`. TypeScript uses `zod` + `zod-to-json-schema` (the existing ecosystem). Rust uses `schemars`. Go uses hand-written schemas in v1 (no good `derive`-style schema crate that targets JSON Schema 2020-12 well); a `go-jsonschema-derive` follow-up is possible but not required for v1.

6. **Agent adapters live in companion packages, not the core wheel/npm.** `@net-mesh/openai-tools`, `@net-mesh/anthropic-tools`, `net-mesh-openai-tools` (pip), `net-mesh-anthropic-tools` (pip). Core nRPC has zero dependency on any LLM provider's SDK. Adapters take a `[ToolMetadata]` and emit the provider's tool-array shape; they also wrap the invocation half so the agent's tool-result block is correctly populated.

Tagged `[S | A | B | C | D | M | T | X]`:

- **S** — substrate (`tool.metadata` RPC schema, capability-tag convention, optional `tool` cargo feature on `net-mesh`).
- **A** — Rust SDK (`net-sdk` — `#[tool]` proc macro + `TypedMeshRpc::serve_tool` + `MeshNode::list_tools`).
- **B** — Node TypeScript (`@net-mesh/sdk` — `tool()` helper + `MeshNode.listTools`).
- **C** — Python (`net.tools` module — `@tool` decorator + `mesh.list_tools()` async).
- **D** — Go (`net` package — `Tool[Req, Resp]` registration helper + `mesh.ListTools(ctx)`).
- **M** — agent-framework adapters (OpenAI + Anthropic for Python and Node; the two languages most agent code lives in).
- **T** — tests (cross-language discovery + invocation round-trips).
- **X** — docs + examples + a demo agent.

---

## Status

| ID    | Pri | Area              | Title                                                                                          | Status |
|-------|-----|-------------------|------------------------------------------------------------------------------------------------|--------|
| S-1   | H   | substrate         | `tool.metadata` typed RPC service shape + `ai-tool:<name>` capability-tag convention            | ⏳     |
| S-2   | H   | substrate         | `[features] tool = []` on `net-mesh` + optional `tool.rs` module with `ToolMetadata` wire type | ⏳     |
| A-1   | H   | Rust SDK          | `net_sdk::tool::ToolMetadata` + `JsonSchema`-via-`schemars` integration                         | ⏳     |
| A-2   | H   | Rust SDK          | `TypedMeshRpc::serve_tool<Req, Resp>(&self, meta, handler)` + auto-publish to `tool.metadata`  | ⏳     |
| A-3   | H   | Rust SDK          | `MeshNode::list_tools(scope) -> Vec<ToolMetadata>` + capability-scoped discovery                | ⏳     |
| A-4   | M   | Rust SDK          | `#[tool]` proc macro (derives schema from sig + docstring; calls `serve_tool` at runtime)       | ⏳     |
| B-1   | H   | Node TS           | `tool({ name, description, schema, handle })` helper in `@net-mesh/sdk`                         | ⏳     |
| B-2   | H   | Node TS           | `MeshNode.listTools(opts?) -> Promise<ToolMetadata[]>` with optional capability filter          | ⏳     |
| B-3   | M   | Node TS           | `zod`-schema convenience: `tool({ name, schema: zodSchema, ... })` auto-converts via `zod-to-json-schema` | ⏳     |
| C-1   | H   | Python            | `from net.tools import tool` decorator that wraps Pydantic-typed `async def` into `serve_tool` | ⏳     |
| C-2   | H   | Python            | `await mesh.list_tools(scope=...)` returning `list[ToolMetadata]` dataclasses                   | ⏳     |
| C-3   | M   | Python            | Plain-`typing` derivation: `@tool` works on functions WITHOUT a Pydantic model (synthesizes one) | ⏳     |
| D-1   | M   | Go                | `net.Tool[Req, Resp](rpc, meta, handler)` registration helper + hand-written schemas in v1      | ⏳     |
| D-2   | M   | Go                | `mesh.ListTools(ctx) -> ([]ToolMetadata, error)`                                                | ⏳     |
| M-1   | H   | Python adapter    | `net-mesh-openai-tools` — `list.to_openai_tools()` + `invoke_tool_call(client_call)`            | ⏳     |
| M-2   | H   | Python adapter    | `net-mesh-anthropic-tools` — `list.to_anthropic_tools()` + `invoke_tool_use(tool_use)`          | ⏳     |
| M-3   | M   | Node adapter      | `@net-mesh/openai-tools` (npm)                                                                  | ⏳     |
| M-4   | M   | Node adapter      | `@net-mesh/anthropic-tools` (npm)                                                               | ⏳     |
| T-1   | H   | cross-lang tests  | `tests/cross_lang_tools/` — Python agent calls Go-hosted tool, asserts schema fidelity          | ⏳     |
| T-2   | M   | cross-lang tests  | Streaming tool: Anthropic-style progress chunks → `serve_tool_streaming` → client-side decode  | ⏳     |
| T-3   | M   | substrate tests   | Capability-scoped `list_tools`: subnet-local tool invisible to a peer in another subnet         | ⏳     |
| T-4   | L   | cancellation test | `client.cancel()` mid-tool propagates substrate CANCEL; tool observes `Cancelled` status        | ⏳     |
| X-1   | H   | docs              | `docs/AGENT_TOOLS.md` — quickstart + decorator + discovery + provider adapters                  | ⏳     |
| X-2   | M   | demo              | `examples/agents/python-anthropic-tools.py` — end-to-end agent with two cross-language tools    | ⏳     |
| X-3   | L   | demo              | `examples/agents/node-langchain-tools.ts` — LangChain-compatible binding                        | ⏳     |

No wire ABI bump. `tool.metadata` is a new nRPC service registered alongside existing ones; existing peers ignore it.

---

## Phasing

**Recommended order: substrate → Rust SDK → Python + Node bindings in parallel → adapters → Go → polish.**

1. **Wave 1 — Substrate convention (S-1, S-2).** Lock the `tool.metadata` RPC shape, the capability-tag convention, and the `tool` cargo feature. Pure-additive; no existing code changes.

2. **Wave 2 — Rust SDK (A-1 → A-2 → A-3).** Ship the foundation everything else builds on. A-4 (proc macro) can land in parallel or as a follow-up — the runtime APIs are usable without it.

3. **Wave 3 — Bindings (B-* + C-* in parallel; D-* last).** Node + Python land alongside their adapter packages so launch-day agent authors see a coherent surface in both languages. Go follows because the Go binding doesn't have a schema-derivation story yet (D-1 ships with hand-written schemas; an auto-deriving helper is a follow-up).

4. **Wave 4 — Agent adapters (M-1..M-4).** Two adapter packages per language (OpenAI + Anthropic). Each is small (~200 LOC) — wrap the discovery result, expose provider-native tool shapes, expose an `invoke` helper that dispatches a tool call through `TypedMeshRpc::call`.

5. **Wave 5 — Tests + docs + demo (T-* + X-*).** Cross-language round-trips pin the schema + invocation contract; the demo agent makes the DX visible.

Wave 1 unblocks every other wave. Waves 2 and 3 can overlap once S-1 is locked. M-slices wait on their language's B/C slice. The Go slice (D) lands last so its hand-written-schema gap doesn't block the Python + Node agent flow.

---

## Wave 1 — Substrate convention

### S-1 — `tool.metadata` typed RPC service shape + capability-tag convention

**Rationale.** Discovery rides nRPC. We need a single canonical service name + request/response shape so every binding's `list_tools()` interoperates with every other binding's `serve_tool`. No new wire protocol.

**Design.**

```rust
// crates/net/src/adapter/net/cortex/tool.rs (new, behind `feature = "tool"`)

/// Wire shape for one tool. Carried in the `tool.metadata.list` reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolMetadata {
    /// nRPC service name. Same string the agent passes to `TypedMeshRpc::call`.
    pub name: String,
    /// Human-readable description; the model reads this to decide when to call.
    pub description: String,
    /// JSON Schema (draft 2020-12) for the request body.
    pub parameters: serde_json::Value,
    /// JSON Schema for the response body. Optional — many models ignore it,
    /// but provider adapters (Anthropic strict mode, OpenAI structured output)
    /// can opt in.
    pub returns: Option<serde_json::Value>,
    /// Streaming shape. `false` for unary tools; `true` for server-streaming /
    /// duplex tools (chunked output the agent can render as it arrives).
    pub streaming: bool,
    /// Free-form tags the host attached at register time. Adapters surface
    /// these as metadata; some providers (Anthropic) expose them under
    /// `cache_control` or custom fields.
    pub tags: Vec<String>,
}

/// `tool.metadata.list` request — agents send this to enumerate tools on a node.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ListToolsRequest {
    /// Optional name filter (substring match). `None` = all tools.
    pub name_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResponse {
    pub tools: Vec<ToolMetadata>,
}
```

**Capability tag convention.** Each tool publishes `ai-tool:<name>` on its host's capability tags. `mesh.list_tools()` walks `find_nodes_for_tag_prefix("ai-tool:")` to discover *hosts*, then RPCs `tool.metadata.list` against each to fetch the full metadata. Two-step discovery (tag lookup → metadata pull) keeps the gossip path small (just the tag) and pushes the heavy payload (the schema) to on-demand fetch.

**Wire compat.** `ToolMetadata` is postcard-encoded; postcard is append-tolerant so future fields land additively. The `tool.metadata` service itself is just an nRPC service — peers without the `tool` feature compile and run normally; they just don't expose the service.

**Files touched.**
- `crates/net/src/adapter/net/cortex/tool.rs` (new — wire types, behind `feature = "tool"`).
- `crates/net/src/adapter/net/cortex/mod.rs` — `pub mod tool;` (gated).

### S-2 — `[features] tool = []` on `net-mesh` + module gate

**Rationale.** Tooling is opt-in: consumers who don't need it shouldn't pay the (small) binary cost or the `schemars` dep when they pull `net-sdk`. Same mechanism as the `regex` feature gate in v0.24.1.

**Design.** Add `[features] tool = []` to `crates/net/Cargo.toml`. The `cortex::tool` module compiles only with the feature on. The `net-sdk` crate adds an `tool` feature that flips on `net-mesh/tool` + `schemars` + the SDK-side surface from A-1 / A-2 / A-3.

**Default policy.** The Rust SDK leaves `tool` OFF by default. The Python wheel, Node npm package, and Go binding leave it OFF by default. Consumers turn it on with `--features tool` (Rust / Python build) or by importing `@net-mesh/sdk-tools` (Node — separate package importable next to `@net-mesh/sdk`, mirrors the OpenAI/Anthropic adapter package pattern).

**Files touched.**
- `crates/net/Cargo.toml` — `tool = []` under `[features]`.
- `crates/net/sdk/Cargo.toml` — `tool = ["net-mesh/tool", "dep:schemars"]`; `schemars` as an optional dep.

---

## Wave 2 — Rust SDK

### A-1 — `net_sdk::tool::ToolMetadata` + `schemars` integration

**Rationale.** Re-export the wire `ToolMetadata` from the substrate as the SDK-facing type, and provide one helper that builds it from any `schemars::JsonSchema`-implementing Rust type pair.

**Design.**

```rust
// crates/net/sdk/src/tool.rs (new)

pub use net::adapter::net::cortex::tool::{ToolMetadata, ListToolsRequest, ListToolsResponse};

/// Build `ToolMetadata` from a Rust type pair that implements
/// `schemars::JsonSchema`. The name + description come from the
/// caller; schemas are generated.
pub fn metadata_for<Req: schemars::JsonSchema, Resp: schemars::JsonSchema>(
    name: impl Into<String>,
    description: impl Into<String>,
) -> ToolMetadata {
    let mut gen = schemars::SchemaGenerator::default();
    let parameters = serde_json::to_value(gen.subschema_for::<Req>())
        .expect("schemars output is always valid JSON");
    let returns = serde_json::to_value(gen.subschema_for::<Resp>()).ok();
    ToolMetadata {
        name: name.into(),
        description: description.into(),
        parameters,
        returns,
        streaming: false,
        tags: vec![],
    }
}
```

**Files touched.**
- `crates/net/sdk/src/tool.rs` (new, behind `feature = "tool"`).
- `crates/net/sdk/src/lib.rs` — `#[cfg(feature = "tool")] pub mod tool;`.

### A-2 — `TypedMeshRpc::serve_tool` + auto-publish to `tool.metadata`

**Rationale.** Registering a tool should be one call. Today it's two — `rpc.serve(name, handler)` + (manual) capability-tag publish + (manual) `tool.metadata` registration. Collapse into one.

**Design.**

```rust
impl TypedMeshRpc {
    /// Register `handler` as a typed nRPC service AND advertise it as
    /// an AI tool: adds the `ai-tool:<name>` capability tag and
    /// registers metadata with the local `tool.metadata` registry.
    /// Returns a `ServeHandle` whose Drop unregisters both.
    pub fn serve_tool<Req, Resp, H>(
        &self,
        meta: ToolMetadata,
        handler: H,
    ) -> Result<ServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        H: TypedRpcHandler<Req, Resp> + Send + Sync + 'static,
    {
        // Re-uses the substrate's `MeshNode.serve_rpc` underneath.
        let inner = self.serve(&meta.name, handler)?;
        self.node.add_capability_tag(format!("ai-tool:{}", meta.name));
        self.node.tool_registry().insert(meta.clone());
        Ok(inner.with_drop_hook(move || {
            self.node.remove_capability_tag(format!("ai-tool:{}", meta.name));
            self.node.tool_registry().remove(&meta.name);
        }))
    }
}
```

**Auto-registered `tool.metadata` service.** The first `serve_tool` call on a `MeshNode` lazily installs the `tool.metadata.list` server handler. Subsequent registrations just push into the registry. Operators who never call `serve_tool` never expose the service.

**Files touched.**
- `crates/net/sdk/src/mesh_rpc.rs` — `serve_tool` method on `TypedMeshRpc`.
- `crates/net/sdk/src/mesh.rs` — `tool_registry()` accessor on `MeshNode`.
- `crates/net/src/adapter/net/cortex/tool.rs` — `ToolRegistry` (a parking_lot-protected `HashMap<String, ToolMetadata>`).

### A-3 — `MeshNode::list_tools(scope) -> Vec<ToolMetadata>`

**Rationale.** The agent-facing discovery API. Walks the cap-index for `ai-tool:*` tag hosts, RPCs each, merges results.

**Design.**

```rust
impl MeshNode {
    /// Discover every tool advertised in `scope`. Returns one
    /// `ToolMetadata` per (host, name) pair. If the same tool name is
    /// served by multiple hosts (HA replicas), each appears once;
    /// the caller picks one (or invokes via service-discovery and
    /// the substrate's routing policy chooses).
    pub async fn list_tools(&self, scope: ToolScope) -> Result<Vec<ToolMetadata>, ListToolsError> {
        let hosts = self.find_nodes_for_tag_prefix("ai-tool:");
        let filtered = scope.filter(hosts, &self.local_subnet());
        let mut results = Vec::new();
        for host in filtered {
            // Fan out one RPC per host; small N, no parallel ceremony.
            match self.typed_rpc().call::<_, ListToolsResponse>(
                host,
                "tool.metadata.list",
                ListToolsRequest::default(),
            ).await {
                Ok(resp) => results.extend(resp.tools),
                Err(e) => tracing::warn!("list_tools: {host:#x} failed: {e}"),
            }
        }
        Ok(results)
    }
}

#[derive(Debug, Clone)]
pub enum ToolScope {
    /// Tools served by THIS node only.
    Local,
    /// Tools visible within the local subnet (and any subnets
    /// where the host's announce visibility makes it reachable).
    Subnet,
    /// Tools globally visible — restricted to `Visibility::Global`
    /// announcements.
    Global,
}
```

**Capability filter.** `scope.filter(...)` consults the capability index's existing per-subnet view. A `Local` scope returns only `local_subnet == this_subnet` hosts; `Subnet` returns up to and including `ParentVisible`; `Global` returns the full set. Same model the existing aggregator query layer uses — no new auth path.

**Files touched.**
- `crates/net/sdk/src/mesh.rs` — `list_tools` + `ToolScope` enum.
- `crates/net/sdk/src/tool.rs` — `ListToolsError`.

### A-4 — `#[tool]` proc macro

**Rationale.** Ergonomic registration without hand-rolling `ToolMetadata`. Reads:

```rust
#[tool(description = "Search the web.")]
async fn web_search(req: WebSearchReq) -> WebSearchResp { ... }
```

At call time, the macro expands to: derive `JsonSchema` for `WebSearchReq` / `WebSearchResp`, build `ToolMetadata::metadata_for::<WebSearchReq, WebSearchResp>(...)`, and emit a `register_tool(&rpc)` function the binary can call.

**Design.** Companion crate `net-tool-macro` (proc-macro). Expansion:

```rust
fn register_tool(rpc: &TypedMeshRpc) -> Result<ServeHandle, ServeError> {
    let meta = net_sdk::tool::metadata_for::<WebSearchReq, WebSearchResp>(
        "web_search",
        "Search the web.",
    );
    rpc.serve_tool::<WebSearchReq, WebSearchResp, _>(meta, web_search)
}
```

**Tag attribute.** `#[tool(description = "...", tags = ["web", "research"])]` flows the tags into `ToolMetadata`.

**Optional.** A-4 is a follow-up if a real consumer asks. The runtime APIs from A-1..A-3 are usable as-is for slightly more typing.

**Files touched.**
- `crates/net-tool-macro/` (new crate).
- `crates/net/sdk/Cargo.toml` — re-export the macro behind `feature = "tool"`.

---

## Wave 3 — Bindings

### B-1 — Node `tool({ name, description, schema, handle })` helper

**Design.**

```typescript
// @net-mesh/sdk
import { tool, type ToolMetadata } from '@net-mesh/sdk'
import { z } from 'zod'

const webSearchTool = tool({
  name: 'web_search',
  description: 'Search the web for relevant results.',
  schema: z.object({
    query: z.string(),
    maxResults: z.number().int().positive().default(10),
  }),
  // Optional — derived from `returns` schema if present
  returns: z.object({ results: z.array(z.string()) }),
  async handle({ query, maxResults }, ctx) {
    // ctx carries the substrate's cancel token + headers.
    if (ctx.signal.aborted) return { results: [] }
    return { results: ['…'] }
  },
})

// Register against an existing TypedMeshRpc:
const handle = await rpc.serveTool(webSearchTool)
```

**Schema lowering.** `zod-to-json-schema` converts the Zod schema to JSON Schema 2020-12 at registration time. Cost is one-shot; the converted schema is cached on the `ToolMetadata` object.

**Files touched.**
- `bindings/node/tool.ts` (new — exports `tool`, `serveTool`, type definitions).
- `bindings/node/package.json` — `peerDependency` on `zod` (optional); `zod-to-json-schema` as a direct dep behind the tools entrypoint.

### B-2 — Node `MeshNode.listTools(opts?) -> Promise<ToolMetadata[]>`

**Design.** Mirrors A-3. Optional `opts`: `{ scope?: 'local' | 'subnet' | 'global', nameContains?: string }`. Returns a flat `ToolMetadata[]` matching the Rust shape (same JSON wire encoding).

### B-3 — `zod`-schema convenience

Implicit in B-1; pinned here as a separate slice to allow shipping B-1 with hand-written JSON schemas first, and adding the Zod convenience as a polish step.

### C-1 — Python `@tool` decorator + Pydantic schema derivation

**Design.**

```python
# net.tools (new module)
from net.tools import tool

class WebSearchRequest(BaseModel):
    query: str
    max_results: int = 10

class WebSearchResponse(BaseModel):
    results: list[str]

@tool(description="Search the web for relevant results.")
async def web_search(req: WebSearchRequest) -> WebSearchResponse:
    return WebSearchResponse(results=["..."])

# Registration:
mesh = NetMesh(...)
rpc = AsyncTypedMeshRpc(mesh)
handle = await web_search.register(rpc)
```

**Schema derivation.** `WebSearchRequest.model_json_schema()` produces JSON Schema 2020-12 natively in Pydantic 2.x. The decorator builds `ToolMetadata` at decoration time so introspection (`web_search.metadata`) works before registration.

**`@tool` on a plain function.** If the function's type hints aren't Pydantic models, the decorator synthesizes a Pydantic model from the signature on the fly (using `pydantic.create_model`). Plain `str`/`int`/`list[str]` / etc. all work. C-3 expands this.

**Cancellation.** The handler can `await some_io()` normally; substrate cancel arrives as `asyncio.CancelledError` via the v3 dispatcher-loop relay landed in the python-async-sdks branch.

**Files touched.**
- `bindings/python/python/net/tools.py` (new).
- `bindings/python/python/net/__init__.py` — re-export.

### C-2 — `await mesh.list_tools(scope=...)`

**Design.** `AsyncNetMesh.list_tools(scope: ToolScope = ToolScope.Subnet) -> list[ToolMetadata]`. `ToolMetadata` is a frozen dataclass mirroring the wire shape.

### C-3 — Plain-`typing` derivation

Detail: `@tool` introspects the function signature with `inspect.signature` + `typing.get_type_hints`, synthesizes a Pydantic model named `<FnName>Request` with the signature's parameter names + types + defaults. Same for the return type via `Annotated[..., Field(...)]` if needed. Covers 90% of casual tool authoring without making users learn Pydantic first.

### D-1 / D-2 — Go `Tool[Req, Resp]` + `ListTools`

**Design.** No proc-macro story in Go; hand-written schemas in v1:

```go
package main

import (
    "context"
    "github.com/ai-2070/net/bindings/go/net"
)

type WebSearchReq struct{ Query string `json:"query"` }
type WebSearchResp struct{ Results []string `json:"results"` }

func main() {
    rpc := net.NewTypedMeshRpc(rawRpc)
    handle, err := net.RegisterTool[WebSearchReq, WebSearchResp](
        rpc,
        net.ToolMetadata{
            Name: "web_search",
            Description: "Search the web for relevant results.",
            Parameters: net.SchemaObject{ /* hand-written */ },
        },
        func(ctx context.Context, req WebSearchReq) (WebSearchResp, error) {
            return WebSearchResp{Results: []string{"..."}}, nil
        },
    )
    ...
}
```

`net.ListTools(ctx, mesh)` mirrors A-3 with the Go-idiomatic `(values, err)` shape.

**Schema derivation follow-up.** Adding a `derive`-style schema generator from Go struct tags is a future slice gated on a real consumer ask. Today's hand-written schemas are workable since the tool list is small and stable.

---

## Wave 4 — Agent adapters

Two adapter packages per language. Each is small — the canonical shape is one helper to lower `[ToolMetadata]` into the provider's tool-array format + one helper to upgrade a provider's tool_use block into a typed nRPC call.

### M-1 — `net-mesh-openai-tools` (Python)

```python
from net.tools import discover
from net_mesh_openai_tools import OpenAIToolBridge
from openai import OpenAI

tools = await mesh.list_tools(scope="subnet")
bridge = OpenAIToolBridge(tools, mesh)

client = OpenAI()
response = client.chat.completions.create(
    model="gpt-4",
    tools=bridge.openai_tools(),  # JSON-schema array OpenAI wants
    messages=[...]
)

for tool_call in response.choices[0].message.tool_calls:
    result = await bridge.invoke(tool_call)
    # result is the typed dict the tool returned; bridge auto-encodes
    # for the next message turn
```

### M-2 — `net-mesh-anthropic-tools` (Python)

Mirror for Anthropic SDK + tool_use / tool_result blocks. Same shape.

### M-3 / M-4 — npm equivalents

`@net-mesh/openai-tools` and `@net-mesh/anthropic-tools`. Take a `ToolMetadata[]` + a `MeshRpc` instance; expose `.toOpenAITools()` / `.toAnthropicTools()` + `.invoke(toolCall)`.

---

## Wave 5 — Tests + docs + demo

### T-1 — Cross-lang tool round-trip

`tests/cross_lang_tools/` — Python agent (M-1) calls a Go-hosted tool (D-1). Asserts:
1. Discovery returns the tool with the expected schema.
2. The schema, when lowered to OpenAI's tools shape, round-trips bytes-equal to a golden vector.
3. The invocation succeeds; the result is the expected typed shape.

### T-2 — Streaming tool

A Python tool that emits 5 progress chunks via `serve_streaming`; an Anthropic-adapter consumer iterates and accumulates. Pin the wire shape of streaming tool output.

### T-3 — Capability-scoped discovery

Two-subnet test: a tool registered on a node in subnet A with `Visibility::SubnetLocal` is invisible to `list_tools(scope=Global)` from a node in subnet B. Pins the existing visibility model carries through.

### T-4 — Cancellation

Mid-tool `client.cancel()` propagates substrate CANCEL; the tool observes the cancel within 50 ms (per the v3 contract). Same shape as `integration_mesh_cancel.rs::cancel_unary_mid_flight_emits_cancel_on_wire` but with a tool wrapper on the server side.

### X-1 — `docs/AGENT_TOOLS.md`

Quickstart, decorator usage, discovery, provider adapters. Cookbook: "register a tool in 10 lines"; "consume a tool from an LLM agent in 20 lines"; "stream progress from a long-running tool"; "scope tools to a subnet."

### X-2 — `examples/agents/python-anthropic-tools.py`

End-to-end agent loop. Two tools, one local (Python `@tool`) and one remote (Go-hosted). Anthropic Claude as the model. The example runs against a single-process mesh-pair so reviewers can `python example.py` and watch it work.

### X-3 — `examples/agents/node-langchain-tools.ts`

LangChain `DynamicStructuredTool` shape; adapter package emits LangChain-compatible tools. Shows the "mesh tools = LangChain tools" identity claim.

---

## Open questions

1. **Per-call header propagation for agent observability.** Should the agent's request-id / trace-id flow into the nRPC headers? Yes, but the adapter needs to provide a hook (M-1's `bridge.invoke(tool_call, headers={...})`). Pin in M-1 design.

2. **Tool result schemas under provider strict modes.** Anthropic strict mode + OpenAI structured output both require the response schema be known. The adapter should opt in when `ToolMetadata.returns` is `Some`; flip off otherwise.

3. **Streaming tool output through OpenAI.** OpenAI doesn't have a native streaming tool-result protocol. Anthropic does (via `tool_use_block_delta` + partial JSON streaming). Adapter ships partial-result streaming on Anthropic only in v1; OpenAI gets unary-only.

4. **Identity of the agent itself as a mesh participant.** Does the agent run *inside* the mesh (a Python process with `NetMesh` + `AsyncTypedMeshRpc`), or *outside* (an OpenAI Realtime API client that connects via a gateway)? V1 assumes inside — the agent process IS a mesh node. An outside-the-mesh gateway is a follow-up (likely a `tool-gateway-daemon` that bridges a non-mesh agent to mesh-hosted tools over a single nRPC call per invocation).

5. **Tool versioning.** Multiple versions of the same tool name on the mesh — same problem as service versioning generally. The capability tag could carry `ai-tool:web_search@1.2`; `list_tools` deduplicates by name + latest version. Follow-up; v1 ships single-version assumption.

6. **MCP (Model Context Protocol) compat.** Anthropic's MCP is the closest external standard to what this plan ships. A separate `mcp-bridge` daemon could expose mesh tools to MCP clients and vice versa. Out of scope for v1 — call it out in the deferred section so users know it's tracked.

---

## Acceptance criteria

- `cargo test --features tool -- tool::` passes; `cargo doc --features tool` clean.
- `pip install net-mesh && python -c "from net.tools import tool"` succeeds on the v0.x+1 wheel.
- `npm install @net-mesh/sdk && grep -q 'export.*tool' node_modules/@net-mesh/sdk/dist/index.js` succeeds.
- T-1 cross-lang round-trip passes.
- X-2 demo (`python-anthropic-tools.py`) runs end-to-end against a local two-node mesh, makes a real Anthropic API call, and successfully dispatches a tool call into a Go-hosted service.
- Wire format adds zero bytes to any non-tool-using nRPC call; the `tool.metadata` service is registered only on nodes that called `serve_tool` at least once.

---

## Deferred follow-ups (post-this-plan)

1. **MCP bridge daemon.** Bidirectional translation: mesh tools → MCP server; MCP clients → mesh tool calls. Standalone binary, depends on `tool` feature.
2. **Outside-the-mesh gateway.** A single TLS endpoint that fronts the mesh for agents that can't be mesh participants (browser-side agents, OpenAI Realtime, third-party tool consumers).
3. **Schema codegen for Go.** `go-tool-derive` generator that reads struct tags and emits JSON Schema. Lifts the hand-written-schema constraint from D-1.
4. **Tool versioning.** Per the open question; tag-encoded version + `list_tools` dedupe.
5. **Per-tool rate-limits + auth scopes.** Agents in `subnet:dev` may call tools at 100 QPS; agents in `subnet:prod` at 10 QPS. Rides the existing channel rate-limit knob + capability auth, but needs a per-tool config layer.
6. **Streaming tool output for OpenAI.** When OpenAI ships a native streaming tool-result protocol (currently absent), update M-1 / M-3 to use it. Today: unary on OpenAI.
7. **Tool composition macros.** `@tool` decorators that wrap LangChain/LangGraph nodes directly so a graph node IS a mesh tool. Likely a separate companion package.
8. **Auto-doc generation.** The `tool.metadata` registry already has descriptions + schemas; a `net tool docs --markdown` CLI could emit a per-mesh tool catalog page. Operator-facing polish.
