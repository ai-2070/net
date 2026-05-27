# nRPC AI Tool-Calling Surface (`@tool`-decorated services + mesh-native discovery)

Branch: `nrpc-ai-tools` (suggested).
Predecessors:
- [`NRPC_STREAMING_PARITY_AND_GO_BINDING.md`](./NRPC_STREAMING_PARITY_AND_GO_BINDING.md) — typed nRPC surface (`TypedMeshRpc.call<Req,Resp>`, `serve<Req,Resp>`).
- [`NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md`](./NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md) — `Mesh::reserve_cancel_token` / `Mesh::cancel(token)` substrate primitive.
- [`PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md`](./PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md) — `AsyncMeshRpc` + `AsyncTypedMeshRpc`.
- Aggregator + fold layer for cross-subnet discovery (v0.22).
- The capability fold (`CapabilityFold` + `ToolCapability`) and `capability_aggregation::TagMatcher`, which this plan reuses for discovery instead of inventing a parallel index.

## Scope

Make every typed nRPC service usable as an **AI tool** (LLM function-calling target) without bolting on a second protocol. The agent author writes `@tool def web_search(req: WebSearchReq) -> WebSearchResp`, the mesh handles registration, schema derivation, gossip, discovery, dispatch, streaming, and cancellation. Cross-language transparently: a Python agent calls a Go-hosted tool through the existing nRPC wire format.

Out of scope:
- Inventing a non-nRPC tool wire protocol. Every tool call rides `TypedMeshRpc::call<Req, Resp>` or `TypedMeshRpc::call_service_streaming<Req, Resp>`.
- LLM provider SDKs as core deps. OpenAI / Anthropic / Gemini / MCP **format translators** live in a single companion package (`@net-mesh/tools` / `net-mesh-tools` on pip) with submodule entrypoints (`@net-mesh/tools/formats/openai`, etc.). Provider-SDK invocation glue is left to thin user code or framework-specific adapters built atop these translators.
- Tool-authoring DSLs separate from the existing typed-handler shape. `@tool` is a thin decorator on top of `serve` + a metadata sidecar.
- LLM inference itself. Net is the bus; agents bring their own client.

## Why now

1. **The typed nRPC surface is the natural fit.** Tool calling is "send a JSON object to a named handler, await a JSON response, optionally stream chunks." nRPC already does this for every binding. The only gaps are (a) metadata so a model can decide when/how to call, (b) a service-discovery streaming primitive matching the unary `call_service`, and (c) a structured event envelope for streaming output. Adding those as a thin layer over `TypedMeshRpc` reuses every existing surface (typed wrappers, observer, cancellation, capability auth, subnet visibility).

2. **The mesh's discovery model already matches the agent's mental model.** An agent wants to ask "what tools can I call?" and get a list. The capability fold already aggregates `ToolCapability` instances across every node — `list_tools()` walks the fold rather than fanning out RPC. Subnet visibility, capability auth, region filtering all reused via the existing `TagMatcher`.

3. **Streaming + cancellation are already substrate primitives.** Tools that emit progress ride server-streaming nRPC. The substrate's `cancel_token` (v3) propagates "model decided to stop" all the way to the tool. No new plumbing.

4. **Cross-language tool calls are the killer feature.** A Python LangGraph agent calling a Go-hosted database tool calling a Node-hosted browser tool — all transparent over the existing wire. Every other tool-calling system (MCP servers, function-calling SDKs, Modal's `cls`) needs a separate transport. Net already has the transport; we just need the convention.

5. **The DX gap is huge today.** Without this layer, an agent author has to: define the typed model, register an nRPC service, hand-write the JSON schema, attach the capability tag, hand-roll the discovery loop, hand-build the provider's tools array. Six steps where one decorator should do.

## Locked decisions

Eight decisions every slice codes against:

1. **Tool name == nRPC service name == capability-tag suffix.** No separate registry. A tool registered as `web_search` IS the nRPC service at channel `nrpc:web_search.requests` IS the announcement carrying `ai-tool:web_search` (plus the existing `nrpc:web_search` cap tag). One identifier, one source of truth, no mapping table.

2. **Schema format = JSON Schema draft 2020-12.** OpenAI, Anthropic, Gemini, and MCP all consume it (with provider-specific subset constraints). The format translators do per-provider lowering; the wire shape is one schema.

3. **Discovery is capability-fold-native, not RPC-fanout.** The capability fold already aggregates `ToolCapability` instances across every node. `list_tools()` walks the fold (one in-memory pass, no network) and returns `ToolDescriptor`s carrying id + version + node_count + small metadata. Heavy fields (full schemas) live on the descriptor when they fit; oversized schemas fall back to an on-demand `tool.metadata.fetch` RPC. The substrate publishes `ToolCapability` via the same fold path that already carries every other capability — agents inherit subnet visibility + auth for free.

4. **Streaming tools share one event envelope: `ToolEvent`.** Every streaming tool emits a typed envelope on each chunk:
   - `start { tool_id, call_id, metadata? }` — fires once on open.
   - `progress { pct?, message? }` — coarse progress for spinners.
   - `delta { data }` — partial output (model tokens, file bytes, log lines).
   - `result { data }` — terminal full result; client sees one of these on success.
   - `error { code, message, details? }` — terminal failure with structured detail.
   Unary tools synthesize a single `result` envelope under the hood. The convention lets every adapter (OpenAI / Anthropic / Gemini / MCP / LangChain / custom) lower envelopes into the framework's native streaming protocol without negotiation. No envelope, no adapter interop.

5. **`@tool` decorator opt-in, not implicit.** Plain `rpc.serve("x", handler)` continues to register a service WITHOUT the `ai-tool:*` tag — invisible to `list_tools()`. The decorator (or its equivalent in each binding) is what makes a service a tool. Operators retain control over which nRPC services agents see.

6. **`serve_tool` is atomic w.r.t. observable mesh state.** Either all of (handler registration, capability-fold publish, `nrpc:<tool_id>` tag, `ai-tool:<tool_id>` tag) succeed, or none do. Failure at any step rolls back the others. Bindings expose this as a single `serve_tool` / `tool(...).register()` call; Drop on the returned handle reverses all four.

7. **Schema derivation is per-binding-idiomatic.** Python uses `pydantic.BaseModel.model_json_schema()`. TypeScript uses `zod` + `zod-to-json-schema`. Rust uses `schemars`. Go uses struct-tag-based hand-written schemas in v1 (no good `derive`-style crate that targets JSON Schema 2020-12 well); a `go-jsonschema-derive` follow-up is possible.

8. **Format translators ship in one core tools package per language; LLM-provider SDKs live entirely in user code.** `@net-mesh/tools` (npm) and `net-mesh-tools` (pip) carry `formats/openai`, `formats/anthropic`, `formats/gemini`, `formats/mcp` submodules. Each translator is a small pure function from `ToolDescriptor` → provider tool-array entry. The reverse direction (`tool_use_block` → typed nRPC call) lives next to it. Users wire the result into their OpenAI / Anthropic / LangChain client; no transitive dep on any provider SDK.

Tagged `[S | A | B | C | D | M | T | X]`:

- **S** — substrate (`call_service_streaming` primitive, `tool.metadata.fetch` RPC, `ToolCapability` fold integration, optional `tool` cargo feature).
- **A** — Rust SDK (`net-sdk` — `TypedMeshRpc::serve_tool` / `list_tools` / `watch_tools` + `ToolEvent`).
- **B** — Node TypeScript (`@net-mesh/sdk` — `tool()` helper + `MeshNode.listTools` / `.watchTools`).
- **C** — Python (`net.tools` module — `@tool` decorator + `await mesh.list_tools()` / `mesh.watch_tools()`).
- **D** — Go (`net` package — `Tool[Req, Resp]` registration helper + `mesh.ListTools(ctx)` / `mesh.WatchTools(ctx)`).
- **M** — format translators (`@net-mesh/tools` + `net-mesh-tools` — OpenAI / Anthropic / Gemini / MCP submodules in one package per language).
- **T** — tests (cross-language discovery + invocation round-trips + streaming envelope contract).
- **X** — docs + examples + a demo agent.

---

## Status

| ID    | Pri | Area              | Title                                                                                          | Status |
|-------|-----|-------------------|------------------------------------------------------------------------------------------------|--------|
| S-1   | H   | substrate         | `call_service_streaming` — mirror of `call_service` returning `RpcStream` (capability-routed + auth-gated)  | ⏳     |
| S-2   | H   | substrate         | `ToolCapability` fold integration: `serve_tool` publishes via the existing capability fold       | ⏳     |
| S-3   | M   | substrate         | `tool.metadata.fetch(name)` RPC — on-demand pull for schemas too large for the fold              | ⏳     |
| S-4   | H   | substrate         | `[features] tool = []` on `net-mesh` + optional `tool.rs` module + `ToolEvent` wire type        | ⏳     |
| A-1   | H   | Rust SDK          | `net_sdk::tool::{ToolDescriptor, ToolEvent}` + `schemars` schema derivation helper              | ⏳     |
| A-2   | H   | Rust SDK          | `TypedMeshRpc::serve_tool<Req, Resp>` (unary) + atomic 4-step register/Drop                     | ⏳     |
| A-3   | H   | Rust SDK          | `TypedMeshRpc::serve_tool_streaming<Req, Resp>` — handler returns `Stream<ToolEvent>`           | ⏳     |
| A-4   | H   | Rust SDK          | `MeshNode::list_tools(matcher)` returning `Vec<ToolDescriptor>` via capability-fold walk        | ⏳     |
| A-5   | H   | Rust SDK          | `MeshNode::watch_tools(matcher) -> Stream<ToolListChange>` for dynamic discovery                | ⏳     |
| A-6   | H   | Rust SDK          | `TypedMeshRpc::call_tool<Req, Resp>` (unary) + `::call_tool_streaming<Req, Resp>` over `S-1`    | ⏳     |
| A-7   | M   | Rust SDK          | `#[tool]` proc macro (follow-up — runtime APIs land first)                                      | ⏳     |
| B-1   | H   | Node TS           | `tool({ name, description, schema, handle })` + Zod schema lowering                              | ⏳     |
| B-2   | H   | Node TS           | `tool({ ..., stream: async function* handle() { yield … } })` — streaming via async-iter        | ⏳     |
| B-3   | H   | Node TS           | `MeshNode.listTools({ matcher? })` + `MeshNode.watchTools({ matcher? })`                        | ⏳     |
| B-4   | H   | Node TS           | `TypedMeshRpc.callTool` + `.callToolStreaming` (capability-routed; client of `S-1`)             | ⏳     |
| C-1   | H   | Python            | `from net.tools import tool` decorator (Pydantic-typed + plain-typing fallback)                  | ⏳     |
| C-2   | H   | Python            | `@tool.stream` / `async def gen(...) -> AsyncGenerator[ToolEvent, None]` streaming variant       | ⏳     |
| C-3   | H   | Python            | `await mesh.list_tools(matcher=...)` + `async for change in mesh.watch_tools(matcher=...)`       | ⏳     |
| C-4   | H   | Python            | `AsyncTypedMeshRpc.call_tool` + `.call_tool_streaming` (capability-routed)                       | ⏳     |
| D-1   | M   | Go                | `net.RegisterTool[Req, Resp](rpc, meta, handler)` + streaming variant                            | ⏳     |
| D-2   | M   | Go                | `mesh.ListTools(ctx, matcher)` + `mesh.WatchTools(ctx, matcher) <-chan ToolListChange`           | ⏳     |
| M-1   | H   | format pkg (Py)   | `net_mesh.tools.formats.openai` — `to_openai_tool(desc)` + `lower_tool_call(call) -> CallSpec`  | ⏳     |
| M-2   | H   | format pkg (Py)   | `net_mesh.tools.formats.anthropic` — same shape; streaming via `tool_use_block_delta`           | ⏳     |
| M-3   | M   | format pkg (Py)   | `net_mesh.tools.formats.{gemini,mcp}` — same pattern                                            | ⏳     |
| M-4   | H   | format pkg (TS)   | `@net-mesh/tools/formats/{openai,anthropic,gemini,mcp}` — mirror per submodule                  | ⏳     |
| T-1   | H   | cross-lang tests  | Python agent (M-1) calls Go-hosted tool (D-1) — schema fidelity + golden vector + result match  | ⏳     |
| T-2   | H   | streaming test    | `ToolEvent` envelope round-trip: server emits `start/progress/delta/result`; client decodes     | ⏳     |
| T-3   | M   | discovery test    | TagMatcher filter: `list_tools(matcher=Prefix("region.eu"))` excludes US-region hosts            | ⏳     |
| T-4   | M   | watch test        | Dynamic-discovery: `watch_tools` emits `Added` / `Removed` when a host registers + drops a tool | ⏳     |
| T-5   | L   | cancellation test | `client.cancel()` mid-tool propagates substrate CANCEL; tool observes `Cancelled` status        | ⏳     |
| X-1   | H   | docs              | `docs/AGENT_TOOLS.md` — quickstart + decorator + discovery + format translators + envelope spec | ⏳     |
| X-2   | H   | demo              | `examples/agents/python-langchain-tools.py` — LangChain agent with one local + one Go-hosted tool | ⏳     |
| X-3   | M   | demo              | `examples/agents/node-openai-tools.ts` — minimal OpenAI function-calling loop via `@net-mesh/tools` | ⏳     |

No wire ABI bump for unary tool calls. Streaming tools use `S-1`'s new `call_service_streaming` substrate primitive; the wire shape of an individual stream is unchanged from `call_streaming` today. `ToolEvent` envelopes are JSON-encoded chunks on existing streams.

---

## Phasing

**Recommended order: substrate → Rust SDK → bindings (parallel) → format pkgs (parallel) → tests/docs.**

1. **Wave 1 — Substrate (S-1, S-2, S-3, S-4 in parallel).** The two foundational pieces are `S-1` (`call_service_streaming` — every streaming tool client depends on it) and `S-2` (publish `ToolCapability` via the capability fold so `list_tools` is a fold walk, not RPC fanout). `S-3` (`tool.metadata.fetch` for oversized schemas) and `S-4` (feature gate + `ToolEvent` wire type) can ship alongside.

2. **Wave 2 — Rust SDK (A-1 → A-2/A-3 → A-4/A-5 → A-6).** Sequence: build the types (`A-1`), then `serve_tool` + streaming variant (`A-2`/`A-3`), then discovery (`A-4`/`A-5`), then the client-side `call_tool` helpers (`A-6`) that ride `S-1`. `A-7` (proc macro) is a follow-up — runtime APIs are usable as-is.

3. **Wave 3 — Bindings (B-*, C-*, D-* in parallel).** Each language stack lands together so launch-day agent authors see a coherent surface. D lands last only because the Go schema story is hand-written in v1 — that's an effort comment, not a dependency.

4. **Wave 4 — Format packages (M-1..M-4 in parallel).** Python's `net-mesh-tools` and Node's `@net-mesh/tools` ship together with the four format submodules (OpenAI / Anthropic / Gemini / MCP). Each translator is a pure function; no SDK transitive deps.

5. **Wave 5 — Tests + docs + demo (T-* + X-*).** Cross-language round-trips pin the schema + invocation + envelope contracts; the demo agents make the DX visible.

Wave 1 unblocks every other wave. Waves 2/3/4 can overlap once `S-1` + `S-2` + `S-4` are locked. The Go slice (D) is independent of the Python/Node ones.

---

## Wave 1 — Substrate

### S-1 — `call_service_streaming` mirror of `call_service`

**Rationale.** Today the substrate has `call_service` (capability-routed unary) and `call_streaming` (explicit-target server-streaming). It does NOT have a capability-routed streaming path. Without it, every adapter that wants a streaming tool has to manually combine `find_service_nodes` → health-filter → `TagMatcher` filter → capability-auth gate → `select_target` → `call_streaming`, and that path skips the cap-auth gate `call_service` enforces. Every other slice depends on this primitive existing.

**Design.**

```rust
impl Mesh {
    pub async fn call_service_streaming(
        self: &Arc<Self>,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcStream, RpcError>;
}
```

Same body as `call_service` (target enumeration → health filter → routing-policy sort → capability-auth filter → `select_target`) but the terminal step is `self.call_streaming(target, ...)` instead of `self.call(target, ...)`. Honors the same `CallOptions::cancel_token` (v3) and `CallOptions::deadline` the unary path honors.

**Bindings work.** Each binding's typed wrapper grows `callServiceStreaming<Req, Resp>` / `call_service_streaming` / `CallServiceStreaming[Req, Resp]` mirroring the existing `callStreaming` shape. Wire format is unchanged — this is purely a new caller-side entry point that composes existing wire-level primitives.

**Cross-language test.** Add a golden vector to `tests/cross_lang_nrpc/golden_vectors.json` covering capability-routed streaming. Confirms TS / Python / Go bindings observe identical target selection, chunk ordering, and EOF semantics.

**Files touched.**
- `crates/net/src/adapter/net/mesh_rpc.rs` — `call_service_streaming` parallel to existing `call_service`.
- Each binding's `mesh_rpc.{rs,ts,py,go}` — typed wrapper entry point.

### S-2 — `ToolCapability` fold integration

**Rationale.** The existing capability fold already aggregates `ToolCapability` instances across every node in scope. Today `serve_tool`-like flows are missing the atomic publish — the SDK has to call `serve_rpc`, then `announce_capabilities`, then add the `nrpc:<tool_id>` tag. This slice wires `serve_tool` (A-2) to do all of that in one atomic step via the existing fold publish path; nothing here is new wire-level machinery, it's plumbing that makes `serve_tool` a single call.

**Design.**

- `MeshNode::tool_publish(meta: &ToolDescriptor)` — internal helper that the SDK's `serve_tool` calls after `serve_rpc` succeeds. Constructs a `ToolCapability` from the descriptor + emits an announcement via the existing `announce_capabilities` path with the `nrpc:<tool_id>` + `ai-tool:<tool_id>` tags added. The descriptor's small fields (name, version, description, node_count derived live, `stateless`, `estimated_time_ms`) embed in the capability payload; the full schema (potentially > 8 KB) lives in a local-only registry that S-3's `tool.metadata.fetch` reads.
- `MeshNode::tool_unpublish(name: &str)` — paired Drop helper.

**Files touched.**
- `crates/net/src/adapter/net/cortex/tool.rs` (new) — internal helpers + the `ToolDescriptor` wire type.
- `crates/net/src/adapter/net/behavior/capability.rs` — wire `ToolCapability` payload fields (additive on the existing struct).

### S-3 — `tool.metadata.fetch(name)` on-demand schema pull

**Rationale.** Some tool schemas are too large to gossip via the capability fold (the fold's per-entry payload budget keeps gossip light). For those, an on-demand pull via nRPC is the natural fallback. The descriptor surfaces a `has_inline_schema: bool`; agents that need a non-inline schema call `tool.metadata.fetch`.

**Design.** A new typed nRPC service auto-registered on any node that has called `serve_tool`. Request is `{ name: String }`; response is `{ parameters: serde_json::Value, returns: Option<serde_json::Value> }`. The local registry lookup is one HashMap read; the round-trip is one nRPC.

**Files touched.**
- `crates/net/src/adapter/net/cortex/tool.rs` — service + handler.
- `crates/net/sdk/src/tool.rs` — `MeshNode::fetch_tool_schema(host, name)` helper.

### S-4 — `[features] tool = []` + `ToolEvent` wire type

**Rationale.** Tooling is opt-in: consumers who don't need it skip the binary cost. Same mechanism as the `regex` gate in v0.24.1. Also: lock the `ToolEvent` wire shape here so every streaming binding agrees byte-for-byte.

**Design.**

```rust
// crates/net/src/adapter/net/cortex/tool.rs — gated by `feature = "tool"`

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolEvent {
    /// Fires once on stream open. Carries the substrate's call_id so
    /// clients can correlate progress events to outstanding calls.
    Start {
        tool_id: String,
        call_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    /// Coarse progress for spinner UIs. Numeric pct and/or message.
    Progress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pct: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Partial output — model tokens, file bytes, log lines. The
    /// adapter decides how to lower these into the provider's
    /// streaming protocol (Anthropic `tool_use_block_delta`, etc.).
    Delta { data: serde_json::Value },
    /// Terminal full result. Client sees exactly one `Result` OR one
    /// `Error` per stream.
    Result { data: serde_json::Value },
    /// Terminal failure with structured detail. Adapter lowers this
    /// to the provider's tool-error block.
    Error {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
}
```

JSON-encoded per chunk. Postcard would be smaller but JSON keeps the format readable in dumps + lets clients use whatever JSON parser they already have (no codec mismatch with the bindings' typed wrappers, which already use JSON for the request body).

Unary tools synthesize a single `Result` envelope server-side; clients of `call_tool` (A-6) unwrap it transparently so unary callers never see envelopes.

**Files touched.**
- `crates/net/src/adapter/net/cortex/tool.rs` — `ToolEvent` enum.
- `crates/net/Cargo.toml` — `tool = []` feature.
- `crates/net/sdk/Cargo.toml` — `tool = ["net-mesh/tool", "dep:schemars"]`.

---

## Wave 2 — Rust SDK

### A-1 — `ToolDescriptor` + `ToolEvent` + `schemars` derivation

```rust
// crates/net/sdk/src/tool.rs

pub use net::adapter::net::cortex::tool::{ToolEvent};

/// Discovery shape — what `list_tools` returns. Lives in the
/// capability fold; agents see one of these per (host, name).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDescriptor {
    /// nRPC service name. Same string `call_tool` takes.
    pub name: String,
    /// Tool version (semver-ish). `list_tools` dedupes by (name, version)
    /// and merges node_counts.
    pub version: String,
    /// Human-readable description; the model reads this to decide when to call.
    pub description: String,
    /// JSON Schema for the request body. `None` when the schema is too
    /// large for the fold; fetch via `tool.metadata.fetch`.
    pub parameters: Option<serde_json::Value>,
    /// JSON Schema for the response body. `None` for non-strict tools.
    pub returns: Option<serde_json::Value>,
    /// `true` if the handler is `serve_tool_streaming`. Adapters lower
    /// this into their provider's streaming protocol.
    pub streaming: bool,
    /// Tool is a pure function (same input → same output, no session
    /// state). Adapters use this to decide caching + parallel-invocation
    /// safety.
    pub stateless: bool,
    /// Soft latency hint for the model scheduler / UI spinner.
    pub estimated_time_ms: Option<u32>,
    /// How many nodes currently serve this (name, version). Filled by
    /// `list_tools`; producers leave it at 0.
    pub node_count: u32,
    /// Free-form tags the host attached at register time.
    pub tags: Vec<String>,
}

pub fn metadata_for<Req: schemars::JsonSchema, Resp: schemars::JsonSchema>(
    name: impl Into<String>,
    description: impl Into<String>,
) -> ToolDescriptor { /* schemars schema gen + sane defaults */ }
```

### A-2 — `TypedMeshRpc::serve_tool<Req, Resp>` (unary, atomic)

```rust
impl TypedMeshRpc {
    /// Atomically register a unary tool. All four state changes succeed
    /// or none do; Drop on the returned handle reverses every step.
    pub fn serve_tool<Req, Resp, H>(
        &self,
        meta: ToolDescriptor,
        handler: H,
    ) -> Result<ServeHandle, ServeError>;
}
```

Steps inside (atomic w.r.t. observable mesh state — rollback on any failure):
1. `node.serve_rpc(&meta.name, …)` — register the handler.
2. `node.tool_publish(&meta)` — adds the capability tag + emits the announcement.
3. Insert into the local `tool_registry` (for `S-3`'s on-demand fetch).
4. Wire the Drop hook to reverse 1 → 3.

### A-3 — `TypedMeshRpc::serve_tool_streaming<Req, Resp>`

Same as A-2 but `handler` returns `impl Stream<Item = ToolEvent>`. The SDK serializes each item as one JSON-encoded chunk on the underlying `serve_streaming` path. Terminal `Result` or `Error` closes the stream; if the handler ends without a terminal event, the SDK synthesizes `ToolEvent::Error { code: "missing_terminal", ... }`.

### A-4 — `MeshNode::list_tools(matcher)` via capability-fold walk

```rust
impl MeshNode {
    /// Walk the capability fold for `ai-tool:*` capabilities matching
    /// `matcher`. One in-memory pass; no network. Dedupes by
    /// (name, version) and fills `node_count`.
    pub fn list_tools(&self, matcher: Option<TagMatcher>) -> Vec<ToolDescriptor>;
}
```

`matcher` is the existing `capability_aggregation::TagMatcher` — same Exact / Prefix / Axis / AxisKey / Regex / VersionRange variants the aggregator already uses. Examples:
- `Some(TagMatcher::Prefix { value: "region.eu".into() })` → tools on EU hosts only.
- `Some(TagMatcher::AxisKey { axis: TaxonomyAxis::Hardware, key: "gpu".into() })` → tools on GPU hosts.
- `None` → every tool in scope.

Subnet visibility comes from the existing fold view — local subnet + parent-visible + global, exactly as `Fold::aggregate` already exposes. No new scope enum.

### A-5 — `MeshNode::watch_tools(matcher) -> Stream<ToolListChange>`

```rust
pub enum ToolListChange {
    Added(ToolDescriptor),
    Removed { name: String, version: String, host: NodeId },
    NodeCountChanged { name: String, version: String, new_count: u32 },
}
```

Subscribes to the fold's change-notify channel and emits diffs as they land. Agent loops use this to refresh their tools array when a new host joins or leaves — without polling. Production agent UIs depend on this.

### A-6 — `TypedMeshRpc::call_tool` + `::call_tool_streaming`

```rust
impl TypedMeshRpc {
    /// Capability-routed unary call. Looks up `name` via `find_service_nodes`,
    /// applies the routing policy, calls `call_service`.
    pub async fn call_tool<Req: Serialize, Resp: DeserializeOwned>(
        &self, name: &str, req: Req,
    ) -> Result<Resp, RpcError>;

    /// Capability-routed streaming call. Same routing as `call_tool` but
    /// terminates in `S-1`'s `call_service_streaming`. Returns a stream
    /// of `ToolEvent`s; the caller drives the agent's UI from the events.
    pub async fn call_tool_streaming<Req: Serialize>(
        &self, name: &str, req: Req,
    ) -> Result<impl Stream<Item = Result<ToolEvent, RpcError>>, RpcError>;
}
```

For unary tools whose server side returned a synthesized `Result` envelope, `call_tool` unwraps the inner `data` so callers see a typed `Resp`, never a `ToolEvent`. Streaming callers see envelopes directly.

### A-7 — `#[tool]` proc macro (follow-up)

```rust
#[tool(description = "Search the web.", stateless = true)]
async fn web_search(req: WebSearchReq) -> WebSearchResp { ... }
```

Expands to a `register_<fn>(rpc)` function that builds the descriptor and calls `serve_tool`. Optional polish — the runtime APIs from A-1..A-6 work without it.

---

## Wave 3 — Bindings

### B-1..B-4 — Node TypeScript

```typescript
// @net-mesh/sdk
import { tool } from '@net-mesh/sdk'
import { z } from 'zod'

const webSearch = tool({
  name: 'web_search',
  description: 'Search the web.',
  stateless: true,
  schema: z.object({
    query: z.string(),
    maxResults: z.number().int().positive().default(10),
  }),
  returns: z.object({ results: z.array(z.string()) }),
  async handle({ query, maxResults }, ctx) { /* ... */ },
})

// Streaming variant — async generator yielding ToolEvents:
const longTask = tool({
  name: 'long_task',
  description: '...',
  schema: z.object({ /* ... */ }),
  async *stream(input, ctx) {
    yield { type: 'progress', pct: 10 }
    for await (const chunk of doWork(input)) {
      yield { type: 'delta', data: chunk }
    }
    yield { type: 'result', data: { ok: true } }
  },
})

const handle = await rpc.serveTool(webSearch)

// Discovery:
const tools = await mesh.listTools({ matcher: { kind: 'prefix', value: 'region.eu' } })
const changes = mesh.watchTools()
for await (const change of changes) { /* ... */ }

// Capability-routed invocation:
const result = await rpc.callTool('web_search', { query: 'mesh', maxResults: 5 })
const stream = rpc.callToolStreaming('long_task', { /* ... */ })
for await (const event of stream) { /* ... */ }
```

### C-1..C-4 — Python

```python
from net.tools import tool

class WebSearchRequest(BaseModel):
    query: str
    max_results: int = 10

class WebSearchResponse(BaseModel):
    results: list[str]

@tool(description="Search the web.", stateless=True)
async def web_search(req: WebSearchRequest) -> WebSearchResponse:
    return WebSearchResponse(results=["..."])

# Streaming variant:
@tool.stream(description="Long-running task.")
async def long_task(req: LongTaskRequest):
    yield ToolEvent.progress(pct=10)
    async for chunk in do_work(req):
        yield ToolEvent.delta(chunk)
    yield ToolEvent.result(LongTaskResponse(ok=True))

handle = await web_search.register(rpc)

# Discovery:
tools = await mesh.list_tools(matcher=TagMatcher.prefix("region.eu"))
async for change in mesh.watch_tools():
    ...

# Capability-routed invocation:
result = await rpc.call_tool("web_search", {"query": "mesh", "max_results": 5})
async for event in rpc.call_tool_streaming("long_task", {...}):
    ...
```

Plain-`typing` fallback applies the same way it did in the previous draft — `@tool` introspects signatures with `inspect.signature` + `typing.get_type_hints` and synthesizes a Pydantic model on the fly so users don't need to learn Pydantic first.

### D-1 / D-2 — Go

```go
// Hand-written schema in v1; derive helper is a future slice.
handle, err := net.RegisterTool[WebSearchReq, WebSearchResp](
    rpc,
    net.ToolDescriptor{
        Name: "web_search",
        Version: "1.0.0",
        Description: "Search the web.",
        Stateless: true,
        Parameters: net.SchemaObject{ /* hand-written */ },
    },
    func(ctx context.Context, req WebSearchReq) (WebSearchResp, error) { /* ... */ },
)

// Discovery + watch:
tools, err := mesh.ListTools(ctx, &net.ListToolsOpts{ Matcher: net.PrefixMatcher("region.eu") })
changes := mesh.WatchTools(ctx, &net.ListToolsOpts{ /* ... */ })
for change := range changes {
    // ...
}
```

---

## Wave 4 — Format packages (one per language, format submodules within)

### M-1..M-3 — Python `net-mesh-tools`

```python
from net_mesh.tools.formats.openai import to_openai_tool, lower_tool_call
from net_mesh.tools.formats.anthropic import to_anthropic_tool, lower_tool_use

# Convert descriptors to provider tool shapes:
openai_tools = [to_openai_tool(d) for d in await mesh.list_tools()]
anthropic_tools = [to_anthropic_tool(d) for d in await mesh.list_tools()]

# Lower a provider's tool_call/tool_use into an nRPC invocation:
call_spec = lower_tool_call(openai_tool_call)  # → {"name": "web_search", "request": {...}}
result = await rpc.call_tool(call_spec["name"], call_spec["request"])
```

The package has zero hard dep on `openai` / `anthropic` SDKs — translators are pure functions over JSON-schema dicts and tool-call/tool-use dicts. Users pip-install `openai` separately if they want to talk to OpenAI.

`gemini` and `mcp` translators ship in the same package; same pattern.

### M-4 — Node `@net-mesh/tools`

```typescript
import { toOpenAITool, lowerToolCall } from '@net-mesh/tools/formats/openai'
import { toAnthropicTool, lowerToolUse } from '@net-mesh/tools/formats/anthropic'

const openaiTools = (await mesh.listTools()).map(toOpenAITool)
// pass to openai.chat.completions.create({ tools: openaiTools, ... })

// For streaming tools, the anthropic translator additionally exposes:
//   lowerToolUse(toolUse, mesh): AsyncIterable<AnthropicContentBlockDelta>
// which calls callToolStreaming, lowers each ToolEvent.delta into an
// Anthropic content-block-delta, terminates on result/error.
```

---

## Wave 5 — Tests + docs + demo

### T-1 — Cross-lang tool round-trip

`tests/cross_lang_tools/` — Python agent (M-1) calls a Go-hosted tool (D-1). Asserts:
1. Discovery surfaces the tool with the expected `ToolDescriptor` shape (name, version, schema, node_count = 1).
2. Lowered to OpenAI's tools shape, the descriptor matches a golden vector byte-for-byte.
3. The invocation succeeds; result equals the expected typed shape.

### T-2 — `ToolEvent` envelope contract

A Python `@tool.stream` handler emits `start → progress(10%) → delta → delta → delta → result`; a Node client consumes via `callToolStreaming` and asserts each event arrives with the expected shape + order. Cross-language pin: the JSON wire encoding must be byte-equal between bindings.

Negative cases:
- Server ends without a terminal event → client observes a synthesized `Error { code: "missing_terminal" }`.
- Server emits `Error` mid-stream → client's stream closes after that frame; subsequent reads return EOF.
- Two terminal events (a bug) → client takes the first, logs a warning on the second.

### T-3 — TagMatcher-filtered discovery

Three-node mesh: node A tags `region.us`, B tags `region.eu`, C tags `region.eu`. Each registers a tool. `list_tools(matcher=Prefix("region.eu"))` from a fourth observer node returns exactly the B + C tools.

### T-4 — Watch-tools dynamic discovery

Boot a mesh; subscribe `watch_tools`; register a tool on node X; assert the subscriber sees `Added(...)` within one fold-broadcast cycle. Drop the handle; assert `Removed(...)`. Spawn a second host serving the same (name, version); assert `NodeCountChanged { new_count: 2 }`.

### T-5 — Cancellation mid-tool

Mid-tool `client.cancel()` propagates substrate CANCEL; the tool's stream sees `ctx.cancellation` fire within 50 ms (per the v3 contract). Adapter side receives a synthesized `Error { code: "cancelled" }` envelope.

### X-1 — `docs/AGENT_TOOLS.md`

Sections: quickstart (`@tool` decorator usage), discovery (`list_tools` + `watch_tools` + TagMatcher cookbook), streaming envelopes (`ToolEvent` spec + adapter lowering examples), format packages (one section per provider), capability scoping (subnet visibility + auth), `serve_tool` atomicity contract.

### X-2 — `examples/agents/python-langchain-tools.py`

End-to-end LangChain agent. One Python `@tool` (local), one Go-hosted tool (remote). LangChain's `DynamicStructuredTool` shape gets emitted by `net_mesh.tools.formats.langchain` (companion submodule). Reviewers `python example.py` and watch a multi-step agent loop dispatch tool calls across the language boundary.

### X-3 — `examples/agents/node-openai-tools.ts`

Minimal OpenAI function-calling loop. Discovers tools, lowers via `formats/openai`, hands to `openai.chat.completions.create`, dispatches each tool_call via `callTool`, feeds the result back into the next message turn. Demonstrates the "mesh tools = OpenAI tools" identity claim.

---

## Open questions

1. **Per-call header propagation for agent observability.** Should the agent's request-id / trace-id flow into the nRPC headers? Yes; the format translators add a `headers?` parameter to `lowerToolCall` so callers can attach trace context. Pin in M-1 design.

2. **Tool result schemas under provider strict modes.** Anthropic strict mode + OpenAI structured output both require the response schema be known. The translator opts in when `ToolDescriptor.returns` is `Some`; flips off otherwise. Stays consistent across providers.

3. **Streaming through OpenAI vs Anthropic.** Anthropic has `tool_use_block_delta` for partial JSON. OpenAI does not (as of writing) — its streaming protocol streams only the assistant's outer chat completion. Adapter ships streaming on Anthropic (`formats/anthropic` lowers `Delta` → `tool_use_block_delta`); OpenAI translator surfaces tools as unary only and emits a `delta`-accumulated `result` once the underlying stream terminates.

4. **Identity of the agent itself as a mesh participant.** V1 assumes inside — the agent process IS a mesh node holding `NetMesh` + `AsyncTypedMeshRpc`. Outside-the-mesh agents (browser, OpenAI Realtime client, third-party tool consumer) need a `tool-gateway-daemon` that bridges via a single nRPC call per invocation. Deferred to follow-up.

5. **Tool versioning.** `ToolDescriptor.version` lives on the wire from day one; `list_tools` dedupes by (name, version). What's NOT yet decided: how should an agent pick between v1.2 and v1.3 of the same tool? Latest-wins by semver? Caller-specified? Default to latest-wins; expose a `version_constraint: Option<VersionReq>` on `call_tool` for explicit pinning. Pin in A-6 design.

6. **MCP compatibility — two layers.** Format-translator layer (`formats/mcp`) handles the simple case: convert `ToolDescriptor` to MCP tool shape, lower MCP tool-call to nRPC invocation. Adequate when the agent is mesh-resident and just wants to look like an MCP server to its prompt template. A separate `mcp-bridge` daemon is needed only if hosting an MCP server over a non-mesh transport (stdio, HTTP) to bridge external MCP clients into the mesh. Bridge daemon = deferred; format translator = M-3.

---

## Acceptance criteria

- `cargo test --features tool -- tool::` passes; `cargo doc --features tool` clean.
- `pip install net-mesh && python -c "from net.tools import tool"` succeeds on the v0.x+1 wheel.
- `npm install @net-mesh/sdk && grep -q 'export.*tool' node_modules/@net-mesh/sdk/dist/index.js` succeeds.
- `pip install net-mesh-tools` exposes `net_mesh.tools.formats.{openai,anthropic,gemini,mcp}`; `npm install @net-mesh/tools` exposes the same four submodules.
- T-1 cross-lang round-trip passes; T-2 envelope contract passes byte-for-byte across all three bindings.
- T-4 watch-tools test passes — `Added` / `Removed` / `NodeCountChanged` fire within one fold-broadcast cycle.
- X-2 demo (`python-langchain-tools.py`) runs end-to-end against a local two-node mesh, makes a real LangChain agent call, and successfully dispatches a tool call into a Go-hosted service.
- Wire format adds zero bytes to any non-tool-using nRPC call; the `tool.metadata.fetch` service is registered only on nodes that called `serve_tool` at least once.

---

## Deferred follow-ups (post-this-plan)

1. **MCP bridge daemon.** Bidirectional translation hosted over stdio / HTTP transports, for external MCP clients that aren't mesh participants. Separate concern from the M-3 format translator (which is sufficient for mesh-resident agents that want to *speak* MCP).
2. **Outside-the-mesh gateway.** A single TLS endpoint that fronts the mesh for browser-side agents, OpenAI Realtime, and third-party tool consumers that can't run a mesh node themselves.
3. **Schema codegen for Go.** `go-tool-derive` reads struct tags and emits JSON Schema 2020-12. Lifts the hand-written-schema constraint from D-1.
4. **Per-tool rate-limits + auth scopes.** Agents in `subnet:dev` may call tools at 100 QPS; agents in `subnet:prod` at 10 QPS. Rides existing channel rate-limit + capability auth + per-tool config.
5. **Streaming tool output for OpenAI.** When OpenAI ships a native streaming tool-result protocol, update `formats/openai` accordingly. Today: unary via accumulated `delta`s.
6. **Tool composition macros.** Decorators that wrap LangChain/LangGraph nodes directly so a graph node IS a mesh tool. Likely a separate companion package.
7. **Auto-doc generation.** A `net tool docs --markdown` CLI emits a per-mesh tool catalog page from the capability fold. Operator polish.
8. **`#[tool]` proc macro in Rust.** A-7 stays a follow-up — runtime APIs are usable as-is.
