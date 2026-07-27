# Net ↔ Hermes Adapter Plan

Bridge Hermes Agent's tool surface to the Net mesh's AI tool-calling layer (`net.tool`), so that:

1. A running Hermes agent can **discover and call** tools served on any reachable Net mesh node — without code changes, just by enabling a plugin.
2. A Hermes process can **expose its locally-registered tools** as Net mesh services, so other agents (Hermes elsewhere, Claude TS clients, raw nRPC callers) can invoke them.

This is the concrete realization of the deferred plan items **X-2** (Hermes Python demo), **X-3** (Claude TS demo — out of scope here, separate plan), and **M-3** (Hermes format adapter) from [`NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`](./NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md).

---

## Existing pieces this plan composes

- **`net.tool`** (Python binding, already shipped this session):
  - `serve_tool`, `serve_tool_streaming{,_async}` — register a handler
  - `call_tool`, `call_tool_streaming{,_async}` — invoke a remote tool
  - `list_tools`, `watch_tools` — discovery (polling-backed)
  - `fetch_tool_metadata{,_async}` — pull oversized schemas
  - `openai.to_openai_tool(desc)` — produces the `{"type":"function","function":{name, description, parameters}}` shape Hermes already speaks
  - `add_tool_capabilities_to_announce` — for the Hermes→Net direction

- **Hermes plugin system** (`hermes_cli/plugins.py`):
  - Each plugin is `hermes/plugins/<category>/<name>/{plugin.yaml, __init__.py}`
  - `__init__.py` exposes `def register(ctx) -> None`
  - `ctx.register_tool(name, toolset, schema, handler, ...)` is the canonical entry
  - Plugin manifest pins `pip_dependencies` so Hermes installs them on enable

- **Hermes tool registry** (`hermes/tools/registry.py`):
  - `registry.register(name, toolset, schema, handler, is_async=False, ...)`
  - Schema shape: `{name, description, parameters}` (the inner OpenAI `function` object — Hermes wraps it at request time)
  - Handler signature: `handler(args: dict, **kwargs) -> result` where `args` is the model-supplied JSON object

These two registries are the two ends of the bridge.

---

## Scope

**In scope.**
- A first-party plugin under `hermes/plugins/net-mesh/` (categorized similarly to `memory/` or `web/`).
- Mesh→Hermes direction: discovered mesh tools surface as Hermes tools.
- Hermes→Mesh direction: a small `hermes net publish` command that exposes selected Hermes tools as mesh services.
- Schema lowering reuses `net.tool.openai.to_openai_tool` to keep wire-shape parity with the T-1 golden fixture.
- A one-page end-to-end demo (`examples/agents/python-hermes-tools.py`) showing two processes — host serves a tool, Hermes agent calls it — wired by name only.

**Out of scope.**
- TypeScript / Claude demo (X-3 — separate plan, MCP surface lives there).
- Toolset reorganization in Hermes (the plugin will use a single `toolset="net-mesh"` namespace, dynamic-only — see "Open questions").
- Auth scopes / per-tool rate limits (deferred plan-wide; mesh-layer concern, plugin inherits whatever the local mesh node carries).
- Tool composition / skill creation from mesh tools (Hermes's own ML-on-tools loop, owned by Hermes core).

---

## Locked decisions

1. **Plugin, not built-in.** The adapter lives under `hermes/plugins/net-mesh/` and follows the existing `register(ctx)` contract. This keeps `net` an optional Hermes dep — users who don't want a mesh runtime never pay the import cost.

2. **Schema lowering uses the existing `net.tool.openai.to_openai_tool`.** Hermes's schema is the inner `function` object; we strip the outer `{"type":"function","function":{...}}` wrap. Reusing the existing translator guarantees byte-equality with the T-1 fixture.

3. **One Hermes toolset for all discovered mesh tools: `"net-mesh"`.** Sub-grouping inside the mesh (by tag, by host, by namespace) is exposed via tool *name prefixes* in the descriptor (e.g. `web.search`, `db.query`) — the descriptor already carries `tool_id` as the source of truth.

4. **Polling-backed discovery on a 1s default.** Mirrors the polling interval the substrate uses everywhere else for `watch_tools` (Node, Python, Go). User can override via plugin config.

5. **Streaming tools collapse to a final result by default.** Hermes's tool-call model is "handler returns one result string". A mesh streaming tool emits a sequence of `ToolEvent`s; the adapter waits for the terminal `result` envelope and returns its `data` field. Incremental display lives in a follow-up (Hermes's spinner-feed could host it, but it's a Hermes-side rendering concern).

6. **Tool-name collision with Hermes built-ins is an error.** The plugin refuses to register a name that already exists in Hermes's registry — matches Hermes's own `override=False` default. Operator can pass `prefix="mesh."` in plugin config to namespace all discovered tools, sidestepping the collision.

7. **AsyncTypedMeshRpc for the runtime path.** Hermes is asyncio-friendly; we use `serve_tool_async` + `call_tool_async` rather than the sync surfaces so the agent loop doesn't block on a long mesh round-trip. The plugin's `register()` is sync (Hermes pluginctx is sync) but the registered tool handlers are `is_async=True`.

---

## Phasing

**Recommended order:** H-1 (substrate) → H-2 (discovery wiring) → H-3 (tool handler) → H-4 (demo) → H-5 (publish direction) → H-6 (tests + docs).

Wave H-1 ships standalone — just the mesh-connection scaffolding. H-2 / H-3 are the M-3 surface. H-4 is X-2. H-5 / H-6 close the loop.

---

## Status

| ID    | Pri | Area              | Title                                                                                          | Status |
|-------|-----|-------------------|------------------------------------------------------------------------------------------------|--------|
| H-1   | H   | plugin scaffold   | `hermes/plugins/net-mesh/{plugin.yaml, __init__.py}` skeleton; lazy mesh handle, config schema | ⏳     |
| H-2   | H   | discovery         | `MeshToolProvider` background task: `watch_tools` → `registry.register` / deregister on changes | ⏳     |
| H-3   | H   | handler           | `_make_mesh_tool_handler(descriptor)` — async handler that calls `call_tool_async` + JSON-decodes args | ⏳     |
| H-4   | H   | demo              | `examples/agents/python-hermes-tools.py` — two-process demo (host serves, Hermes calls)        | ⏳     |
| H-5   | M   | publish direction | `hermes net publish <tool-name>` CLI — wraps a Hermes-registered tool with `serve_tool_async` | ⏳     |
| H-6   | H   | tests + docs      | Plugin unit tests + a short README in the plugin dir + cross-link from `AGENT_TOOLS.md`        | ⏳     |
| H-7   | M   | streaming UX      | Incremental render path: stream `ToolEvent.Delta` chunks into Hermes's tool-feed spinner       | ⏳ (follow-up) |
| H-8   | L   | conflicts         | Prefix-based name namespacing (`prefix="mesh."`) when colliding with built-ins                 | ⏳     |

Legend: ✅ done · 🟡 partial · ⏳ todo.

---

## H-1 — Plugin scaffold + mesh-handle lifecycle

**Rationale.** The plugin needs to own (or borrow) a `NetMesh` + `AsyncTypedMeshRpc` for its entire lifetime. Most Hermes processes are single-tenant; the plugin instantiates its own mesh node by default, but accepts an externally-provided one via config for embedded use.

**Files.**
- `hermes/plugins/net-mesh/plugin.yaml` — `name: net-mesh`, `pip_dependencies: [net-mesh-sdk]` (the Python `net` wheel)
- `hermes/plugins/net-mesh/__init__.py` — top-level `register(ctx) -> None`; reads config from `ctx.config.plugins.entries.net-mesh.*`
- `hermes/plugins/net-mesh/provider.py` — `MeshToolProvider` class (started in H-2)

**Config shape (in `~/.hermes/config.yaml`):**
```yaml
plugins:
  entries:
    net-mesh:
      enabled: true
      # If not set, plugin spawns its own MeshNode bound to a random port.
      mesh_bind: "127.0.0.1:0"
      psk_env: "NET_MESH_PSK"          # env var holding the 32-byte PSK (hex)
      # Tools that should be visible to this agent. Defaults to "all".
      matcher: null                    # or "ai-tool:web.*" / TagMatcher::Prefix(...)
      poll_interval_ms: 1000
      # When a discovered tool name collides with an existing Hermes tool:
      collision_policy: "error"        # "error" | "prefix"
      prefix: "mesh."                  # used when collision_policy == "prefix"
```

**Lifecycle.**
- `register(ctx)` is sync — it creates an `asyncio.new_event_loop()` for the plugin background work, spawns a daemon thread to run it, and parks the loop until `ctx.on_shutdown` fires.
- The mesh node + watcher live on the plugin's loop. Tool handlers cross loop boundaries via `asyncio.run_coroutine_threadsafe` — Hermes's main loop calls `handler(args)`, the handler bounces onto the plugin loop, awaits `call_tool_async`, returns the result.

**Acceptance.** Plugin loads with `enabled: true` + a valid PSK → log shows mesh started + listening on the configured port; no tools registered yet (H-2 ships them).

---

## H-2 — Discovery wiring (`MeshToolProvider`)

**Rationale.** This is the M-3 core. A background asyncio task drains `watch_tools` and reconciles each event with Hermes's `registry`.

**Design.**
```python
# hermes/plugins/net-mesh/provider.py
class MeshToolProvider:
    def __init__(self, rpc: AsyncTypedMeshRpc, ctx: PluginContext, config: dict):
        self._rpc = rpc
        self._ctx = ctx
        self._matcher = config.get("matcher")
        self._collision = config.get("collision_policy", "error")
        self._prefix = config.get("prefix", "mesh.")
        # Tracks names we registered so we can deregister on Removed.
        self._registered: dict[str, str] = {}  # tool_id → hermes_name

    async def run(self) -> None:
        async for change in watch_tools(self._rpc, matcher=self._matcher):
            if isinstance(change, ToolListChange.Added):
                await self._on_added(change.descriptor)
            elif isinstance(change, ToolListChange.Removed):
                await self._on_removed(change.descriptor)
            # NodeCountChanged: ignored — no observable effect on Hermes
```

`_on_added(desc)`:
1. Compute Hermes name: `desc.tool_id` if no collision; else `f"{prefix}{tool_id}"` or raise.
2. Lower schema: `schema = net.tool.openai.to_openai_tool(desc)["function"]` (strips the outer envelope).
3. Build the async handler (H-3 below).
4. Call `ctx.register_tool(name=hermes_name, toolset="net-mesh", schema=schema, handler=handler, is_async=True, description=desc.description)`.
5. Record `_registered[desc.tool_id] = hermes_name`.

`_on_removed(desc)`:
1. Look up in `_registered`, deregister via `registry.deregister(hermes_name)` (Hermes already supports this for MCP refresh — same code path).

**Cancellation.** `watch_tools` is async; the plugin's shutdown hook cancels the task, the stream closes cleanly via the existing AbortSignal-equivalent path.

**Acceptance.** Plugin starts; another process serves a tool; within 1–2 poll cycles the Hermes agent sees it in `hermes tools` output and the model can call it.

---

## H-3 — Tool handler bridge

**Rationale.** Hermes hands the registered handler `args: dict` (model-supplied) and expects a result. The handler must JSON-encode and dispatch via `call_tool_async`, then format the response back into Hermes's result-string shape.

**Shape.**
```python
def _make_mesh_tool_handler(rpc: AsyncTypedMeshRpc, tool_id: str) -> Callable:
    async def handler(args: dict, **kwargs) -> Any:
        try:
            resp = await call_tool_async(rpc, tool_id, args)
        except RpcError as e:
            return tool_error(f"net-mesh: {tool_id} failed: {e}")
        # Hermes accepts dict/str/list — return the decoded payload as-is.
        return resp
    return handler
```

**Streaming variant.** When `desc.streaming is True`, we wrap with `call_tool_streaming_async` and accumulate:
```python
async def streaming_handler(args, **kwargs):
    final = None
    async for event in call_tool_streaming_async(rpc, tool_id, args):
        if event["type"] == "result":
            final = event["data"]
            break
        if event["type"] == "error":
            return tool_error(f"net-mesh: {tool_id} returned error: {event['code']}: {event['message']}")
        # Drop progress/delta — H-7 wires these into Hermes's tool-feed
    return final
```

**Loop crossing.** Hermes calls the handler from its main event loop. The plugin's mesh runs on its own loop. The handler uses `asyncio.run_coroutine_threadsafe(_actual_handler(args), plugin_loop).result(timeout=N)` — Hermes already does this pattern for its async memory providers.

**Acceptance.** A registered handler returns the typed result on success, `tool_error(...)` on failure with a stable message format.

---

## H-4 — End-to-end demo (X-2)

`examples/agents/python-hermes-tools.py`:
- Process 1: Plain `net.tool` Python script that `serve_tool_async`s a `web_search` mock tool.
- Process 2: Hermes CLI launched with the `net-mesh` plugin enabled in config.
- The Hermes session prompt: "search for X using web_search". The model picks the tool, the adapter dispatches, Hermes shows the result.

**Acceptance.** README walks through the two-terminal setup; demo runs to completion in < 30s end-to-end.

---

## H-5 — Publish direction (`hermes net publish`)

**Rationale.** A Hermes user may want to expose one of their local tools (e.g. a custom MCP-backed search, a Hermes skill) to other mesh participants. CLI command + a small registry-walker do it.

**Design.**
- `hermes net publish <tool-name>` — looks up the tool by name in `hermes.tools.registry`, wraps its handler in `serve_tool_async`, holds the `ToolServeHandle` for the lifetime of the CLI process (or until `hermes net unpublish`).
- Descriptor synthesized from Hermes's schema: `name`, `description`, `parameters` → mesh `ToolDescriptor`. Reverse of H-2's lowering.

**Out of scope for H-5:** auto-publishing all registered tools (operator-explicit only — avoids accidentally exposing a `terminal` tool that runs arbitrary shell).

---

## H-6 — Tests + docs

**Tests** (`hermes/tests/plugins/test_net_mesh_adapter.py`):
- Mock-based: a fake `AsyncTypedMeshRpc` returning canned `ToolListChange` events; assert `ctx.register_tool` fires with the expected `(name, toolset, schema, handler)`.
- Mock-based: `_make_mesh_tool_handler` happy path + RpcError → `tool_error` path.
- Integration smoke (gated on `pytest -m mesh`): real two-mesh round-trip mirroring the existing `test_tool.py` async smoke.

**Docs.**
- `hermes/plugins/net-mesh/README.md` — config, troubleshooting, security note (PSK).
- Cross-link from `net/crates/net/docs/AGENT_TOOLS.md`'s "Bindings → Python" section + `agentskills.io` standard reference.

---

## H-7 — Incremental rendering (follow-up)

Pipe `ToolEvent.Delta` chunks into Hermes's `┊` activity feed so the user sees partial results as they arrive. Needs a small extension to Hermes's tool-callback surface (`ctx.tool_progress_callback` or similar). Defer until there's a real streaming-tool use case in the wild.

---

## H-8 — Name conflicts (low priority)

When `collision_policy == "prefix"`, prepend the configured prefix to every discovered tool name (`mesh.web_search` instead of `web_search`). Useful when a Hermes setup already has a built-in `web_search` and the mesh has a different implementation.

---

## Open questions

1. **Does Hermes pluginctx need a `register_tool_provider` higher-level hook?** Today plugins call `ctx.register_tool` per-tool. The mesh case wants to register N tools dynamically over time — `_on_added` from H-2 fits the per-tool surface, but it's worth a quick check that Hermes's tool-discovery cache doesn't memoize "all tools at startup" and miss late-arriving ones. (`registry.py`'s generation counter suggests it doesn't, but verify by reading `model_tools.get_tool_definitions`.)

2. **Hermes async tool handler contract.** `registry.register(is_async=True)` — does Hermes invoke the handler with `await`, or expect the handler to return a coroutine the agent loop awaits? Need to skim `model_tools.handle_function_call`. (H-3 design above assumes the model_tools layer awaits coroutines; verify before locking H-3.)

3. **Multi-process safety.** If two Hermes instances on the same host both enable the plugin, they'll start two mesh nodes. Is that a problem? Likely not — they'll just see each other as peers — but operators should know the cost. Document in H-6.

4. **PSK / auth.** Plugin reads PSK from env. Should we offer a "join existing mesh" mode where the plugin attaches to an already-running `net daemon` via a unix socket rather than spawning its own node? Cleaner for multi-Hermes setups; defer to H-1 v2.

5. **Tool descriptor versioning.** Mesh tools have `version` in their descriptor; Hermes doesn't model version (registry is one-tool-per-name). When the same tool ID exists at v1 and v2 in the mesh, the plugin needs a tiebreaker. Default: latest semver wins. Pin in H-2 design.

---

## Acceptance criteria

- `pip install net-mesh` + enabling the `net-mesh` plugin in `~/.hermes/config.yaml` makes mesh-hosted tools appear in `hermes tools` within one poll cycle.
- The Hermes model can invoke a mesh-hosted tool by name from a CLI prompt; the result round-trips back into the conversation.
- Dropping the host's tool registration removes the tool from Hermes within one poll cycle.
- `hermes net publish <name>` exposes a Hermes tool on the mesh; another `net.tool`-using process can `call_tool` it.
- No regression in Hermes's existing tool surface — every built-in tool still loads and is callable when the plugin is enabled.

---

## Deferred follow-ups

1. **MCP bridge daemon** — same item as the Net plan's deferred MCP bridge. Hermes already speaks MCP via `mcp_tool.py`; the Net plugin doesn't change that direction.
2. **Per-tool cost / latency telemetry** — the substrate already emits observer events; the plugin could surface a `mesh_tool_stats` panel in Hermes's existing observability plugin.
3. **Outside-the-mesh gateway** — same item as the Net plan; Hermes-side rendering of the gateway's exposed tools is unblocked once the gateway exists.
4. **Auto-skill creation from mesh tool use** — Hermes's existing skill-creation loop. Wire `mesh_tool_invocation` events into the skill-prompt trigger.
