# AI Tool Calling Integration Plan

## What ships

---

## Item 1: `call_service_streaming` in core

**Where:** `net/crates/net/src/adapter/net/mesh_rpc.rs`, mirrors `call_service` at lines 2742–2835.

**Why first:** Every other item depends on this. The integration module's `tool.submit()` returns a stream. Without `call_service_streaming` the adapter has to assemble `find_service_nodes` → `select_target` → `call_streaming` manually, duplicating logic and losing the capability-auth gate that `call_service` enforces between candidate filtering and target selection.

**What:**

```rust
pub async fn call_service_streaming(
    self: &Arc<Self>,
    service: &str,
    payload: Bytes,
    opts: CallOptions,
) -> Result<RpcStream, RpcError>
```

Same body as `call_service` (lines 2748–2833): `find_service_nodes` → health filter → sort → capability-auth filter → `select_target`. Terminal call switches from `self.call(target, ...)` to `self.call_streaming(target, ...)`.

**Bindings work:**

- `net/crates/net/bindings/node/src/mesh_rpc.rs`: add `call_service_streaming` napi method mirroring `call_streaming`'s napi shape.
- `net/crates/net/bindings/node/mesh_rpc.ts`: add `RawMeshRpc.callServiceStreaming` interface entry and `TypedMeshRpc.callServiceStreaming<Req, Resp>` returning `TypedRpcStream<Resp>`. Mirrors the `callStreaming` wrapper at lines 615–629.
- `net/crates/net/bindings/python/`: parallel addition.
- `net/crates/net/bindings/go/net/mesh_rpc_typed.go`: parallel addition.

**Cross-language test:** Add a vector to `tests/cross_lang_nrpc/golden_vectors.json` covering capability-routed streaming. Confirms TS/Python/Go bindings all observe identical behavior — same target selection, same chunk ordering, same EOF semantics.

**Size:** ~150 LoC Rust, ~80 LoC per binding, ~50 LoC tests. One PR. **~1 day.**

---

## Item 2: Tool-to-service binding convention

**Where:** New module `net/crates/net/src/adapter/net/tools.rs` (Rust) plus mirror in each SDK.

**Why:** `ToolCapability` (capability.rs:471) and `nrpc:<service>` tags (mesh_rpc.rs:2730) are independent today. A node advertising tool `python_repl` doesn't automatically serve `nrpc:python_repl`. The integration module needs `tools.serve(tool_id, schema, handler)` to atomically (a) register the RPC handler and (b) announce the `ToolCapability` and (c) ensure `nrpc:<tool_id>` is on the announcement.

**Convention:** `tool_id` IS the RPC service name. One identifier, one source of truth, no mapping table.

**Rust API:**

```rust
impl Mesh {
    pub fn serve_tool<H: RpcHandler>(
        self: &Arc<Self>,
        tool_id: &str,
        capability: ToolCapability,  // input_schema, output_schema, version, etc.
        handler: H,
    ) -> Result<ServeHandle, ServeError>;
}
```

Internally: validates `capability.tool_id == tool_id`, calls `serve_rpc(tool_id, handler)`, calls `announce_capabilities` with the ToolCapability added to the set AND `nrpc:<tool_id>` in the tag list. Atomic w.r.t. observable mesh state — either both register or neither does.

**SDK surfaces:**

- TS: `tools.serve(toolId, schema, handler)` where schema is a Zod schema or raw JSON Schema string. If Zod, derive JSON Schema via `zod-to-json-schema` for the announcement.
- Python: `tools.serve(tool_id, schema, handler)` where schema is a Pydantic model class or raw JSON Schema dict. If Pydantic, derive JSON Schema via `model.model_json_schema()`.
- Go: `tools.Serve(toolID, schema, handler)` where schema is a struct with JSON Schema tags.

The schema-derivation makes operators write one typed definition rather than maintaining types and JSON Schema separately.

**Size:** ~100 LoC Rust, ~150 LoC TS (including zod-to-json-schema bridge), ~120 LoC Python, ~120 LoC Go. **~1.5 days.**

---

## Item 3: Tool listing / manifest API

**Where:** New module function alongside Item 2.

**Why:** Frameworks need to ask "what tools are available right now?" to surface them to the LLM. `find_by_tool(tool_id)` (swarm.rs:702) finds nodes for a known tool. Nothing currently returns the aggregated set of distinct tools available across the whole substrate.

**Rust API:**

```rust
impl Mesh {
    pub fn list_available_tools(&self) -> Vec<ToolDescriptor>;
}

pub struct ToolDescriptor {
    pub tool_id: String,
    pub name: String,
    pub version: String,
    pub input_schema: Option<String>,
    pub output_schema: Option<String>,
    pub estimated_time_ms: u32,
    pub stateless: bool,
    pub node_count: usize,  // how many nodes currently serve it
}
```

Implementation: walks the capability fold, collects all `ToolCapability` instances across all known nodes, deduplicates by `(tool_id, version)`, returns aggregated descriptors with `node_count`.

**Optional filter:** `list_available_tools_filtered(matcher: TagMatcher)` reuses `capability_aggregation.rs` to filter nodes first (e.g. "only tools on EU nodes"), then aggregates tools.

**SDK surface:**

```typescript
const tools = await net.tools.list();
// [{ toolId, name, inputSchema, nodeCount, ... }]

const euTools = await net.tools.list({
  matcher: { kind: 'prefix', value: 'region.eu' }
});
```

**Size:** ~80 LoC Rust, ~60 LoC TS, ~60 LoC Python, ~60 LoC Go. **~0.5 day.**

---

## Item 4: Streaming envelope convention

**Where:** Documented wire convention + serialization helpers in `@net-mesh/tools`.

**Why:** `call_streaming` returns `Bytes`. Frameworks want structured events: `start`, `progress`, `delta`, `result`, `error`. Without a convention, every adapter reinvents this and they don't interoperate.

**Wire format:** Each chunk in a streaming response is a JSON envelope:

```typescript
type ToolEvent =
  | { type: 'start'; toolId: string; callId: string; metadata?: object }
  | { type: 'progress'; pct?: number; message?: string }
  | { type: 'delta'; data: unknown }       // streaming partial output (tokens, chunks)
  | { type: 'result'; data: unknown }      // terminal full result
  | { type: 'error'; code: string; message: string; details?: object };
```

**Helpers in the integration module:**

```typescript
// Server side
async function* myHandler(input: Input): AsyncGenerator<ToolEvent> {
  yield { type: 'progress', pct: 10, message: 'parsing' };
  for await (const token of generate(input)) {
    yield { type: 'delta', data: token };
  }
  yield { type: 'result', data: finalOutput };
}

tools.serve('my_tool', schema, myHandler);
// The wrapper handles envelope serialization, terminal-event detection,
// EOF semantics, error propagation.

// Client side
const stream = tools.submit('my_tool', args);
for await (const event of stream) {
  // event is typed ToolEvent
}
```

For non-streaming tools, the handler can be a plain async function returning the result; the wrapper synthesizes a single `result` event.

**Size:** ~150 LoC TS, ~120 LoC Python, ~120 LoC Go. **~1 day.**

---

## Item 5: Framework format translators

**Where:** `@net-mesh/tools/formats/{openai,anthropic,mcp,gemini}.ts`.

**Why:** `ToolCapability` carries JSON Schema. OpenAI function calling wants `{ name, description, parameters: <JSON Schema> }`. Anthropic tool use wants `{ name, description, input_schema: <JSON Schema> }`. MCP wants its own shape. Gemini has yet another. Frameworks call `tools.list()` then need to translate to the format their LLM API expects.

**API:**

```typescript
import { toOpenAIFormat, toAnthropicFormat, toMCPFormat } from '@net-mesh/tools/formats';

const tools = await net.tools.list();
const openaiTools = tools.map(toOpenAIFormat);
// pass to openai.chat.completions.create({ tools: openaiTools, ... })
```

Each translator is a 20–40 line pure function. JSON Schema is already the common substrate so translation is mostly key renaming and shape rearrangement.

**Targets at launch:** OpenAI, Anthropic, MCP, Gemini. Add more on demand. Possibly if Hermes/OpenClaw don't natively consume one of the above.

**Size:** ~50 LoC per format × 4 formats = ~200 LoC TS, mirror in Python (~200 LoC), Go probably skip initially (Go AI ecosystem is less format-fragmented). **~1 day.**

---

## Item 6: Hermes adapter + working demo

**Where:** New repo `net-mesh/hermes-net` or under `net/crates/net/integrations/hermes/`.

**Why:** Wednesday huddle needs a runnable thing, not slides. Adapter is the demonstration vehicle.

**API surface:**

```typescript
// hermes-net adapter
import { createNetToolProvider } from '@net-mesh/hermes-adapter';
import { Hermes } from 'hermes';

const hermes = new Hermes({
  toolProviders: [
    createNetToolProvider({
      endpoint: process.env.NET_NODE_ENDPOINT,
      // optional capability filter: only EU tools, only specific providers, etc.
      filter: { matcher: { kind: 'prefix', value: 'region.eu' } },
    }),
  ],
});

// Hermes now sees every tool advertised on Net.
// Tool calls flow through Net's substrate to whichever node provides the tool.
```

Internally, `createNetToolProvider`:
1. Connects to the local Net node via the SDK.
2. On Hermes's "list tools" callback: calls `tools.list()`, translates each via `toAnthropicFormat` or whatever Hermes expects, returns the list.
3. On Hermes's "call tool" callback: calls `tools.submit(toolId, args)`, consumes the event stream, translates `delta` events to Hermes's streaming chunks, surfaces `result` as the final response, surfaces `error` as the error.
4. Watches the local capability fold for tool list changes; re-emits "tools changed" to Hermes when the set changes.
