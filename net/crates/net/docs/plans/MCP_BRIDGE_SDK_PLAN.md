# Implementation Plan: MCP Bridge Adapter Library (Rust / TypeScript / Python / Go / C)

*Adapter library, not a native-node SDK. Apps and agents integrate through the general Net SDK; this library translates MCP protocol surfaces.*

**Position in the stack:** follows the merged MCP bridge + credential forwarding work. This plan exposes MCP adapter/shim internals and pure bridge helpers as public library surfaces. Hermes and OpenClaw native integrations depend on the **general Net SDK**; they call bridge helpers only when publishing MCP-backed tools. The CLI and the `net mcp serve` shim refactor into thin frontends over these public APIs.

**Goal:** expose `net-mesh-mcp` as the single Rust implementation of MCP interoperability — wrapping MCP servers, serving Net capabilities to MCP hosts, lowering MCP descriptors into Net descriptors, classifying bridge credential risk — bound across the matrix in the repo's established construction. Native apps/agents participate through the general Net SDK. One core, five faces, zero reimplementation.

**Established shape this follows (verified in-tree):** per-domain modules in `bindings/node` (napi) and `bindings/python` (pyo3 + typed pure-Python layer, `py.typed`, `.pyi` stubs, existing `async_bridge.rs` machinery); C headers per domain in `net/crates/net/include/` (`net.go.h` carries the Go ABI); Go via cgo over that header; cross-language golden vectors in CI. The MCP bridge SDK is **one new domain module per binding + `net_mcp.h`** — not a new construction.

---

## Doctrine

**The invariant: adapters attach; nodes participate. The MCP bridge translates protocol surfaces; Hermes/OpenClaw hold identity as Net participants.**

1. **Rust core is the only implementation.** `net-mesh-mcp` grows a public library API; every binding marshals into it. No logic in bindings — especially not the pin-store lock, credential classification, or validation. One lock implementation on one file, ever.
2. **The bridge is an adapter, not a participation path.** Adapters attach; nodes participate. Agent runtimes (Hermes, OpenClaw) are first-class Net nodes using the general Net SDK — they never route their identity through this crate. This SDK covers: **spawn mode** (lib spawns/speaks to a stdio MCP server — `net wrap`, headless supply), the **serve shim internals**, and **pure helper functions** (`lower(tools/list entry) -> ToolDescriptor`, `classify(server) -> credential status/tags`) that native nodes call before publishing MCP-backed tools through the normal SDK path. Attach mode (lib-owned descriptor+callback lifecycle) is **out of the critical path** — appendix/future, for hosts that can't run sidecars or embed a node.
3. **Secrets stay unrepresentable.** Forwarding internals (`ForwardedHeaderValue`, sealing, keychain) are **not bound**. Bindings see secret *refs* and policy surfaces only. The per-language negative test from the SDK matrix applies: no binding can accept, return, serialize, or log a secret value.
4. **Idiomatic in shape, identical in concepts** (SDK matrix rules apply wholesale): Python dual sync + async — the tool-handler async gap gets closed here, riding the existing `async_bridge` machinery; TS Promise-native; Go blocking + `context.Context`; C uses explicit handles and free-functions, with callback hooks only where shim/bridge event APIs require them — **no attach-mode tool-handler callbacks in v1**.
5. **The shim stays a binary.** Foreign MCP hosts spawn processes; `net mcp serve` is not bindable away. Its internals (gateway, validation, consent, pins, grouping) are the library; the binary is a frontend — same relationship the CLI `wrap` command gets.

## Surface to bind (from the merged code, by module)

| Core module | Bound API | Notes |
|---|---|---|
| `wrap` | `publish_server(cmd, opts) -> PublicationHandle`; `handle.withdraw()` | handle-scoped — multiple publications per process, no global withdraw; opts: allow-origins, credential flags; classification runs in core |
| `wrap::descriptor` | `lower(...)` as a **pure helper** | native nodes publish MCP-backed tools via the general SDK; this keeps lowering single-sourced |
| `wrap::credentials` | classification exposed read-only (`classify(...) -> status/tags`) | so agents can *display* risk before publishing |
| `serve::backend` | `search`/`describe`/`invoke` via `CapabilityGateway` DTOs | **MCP-host-facing consume path for `net mcp serve` only.** Native nodes consume through `net-mesh-sdk` directly — this row is shim plumbing, not an app API |
| `serve::pins` / `serve::consent` | **graduate together to `net-mesh-sdk`** (consent isn't MCP-specific); shim consumes them from there | shared store + lock hidden entirely; approvals stay out-of-band — model-driven callers request, never self-approve; decisions are structured enums, never re-derived binding-side |
| `serve::grouping` | grouped search results, provider lists | v0 node-namespaced, canonical `{capability, provider}` pairs |
| `spec` | version negotiation info | read-only |
| `forward` | **refs + policy only** (secret set via ref name → OS keychain path stays CLI/Rust) | per doctrine 3 |

## Phases

**P0 — Rust public API carve-out.** Promote the surfaces above to public, semver'd API; **move pins/consent into `net-mesh-sdk`**; expose `lower`/`classify` as pure helpers; refactor the CLI `wrap`/`mcp serve`/`mcp pin` onto the public APIs so the CLI proves the library daily. Acceptance: CLI behavior unchanged, zero private calls.

> **Status: P0 landed 2026-07-05** (branch `mcp-sdk`). `net_sdk::consent` + `net_sdk::pins` carry `CapabilityId`, the `CredentialStatus` vocabulary, `ConsentPolicy`/`ConsentDecision`, and the lock-protocol `PinStore`; the bridge re-exports them at the old paths, and a compile-time type-identity guard (`adapters/mcp/tests/dependency_boundary.rs`) makes a bridge-local reimplementation unbuildable. `wrap_server` became `ServerPublisher::publish_server(cmd, opts) -> PublicationHandle` (+ `handle.withdraw()`): a per-mesh publisher merges every publication's announcement and a per-publication-scoped describe catalog, which is what makes "multiple publications per process" hold over whole-node-set announcements (tool ids must stay distinct across a node's publications — a collision rolls the publish back). `lower_tool`/`classify` stand as the pure helpers. CLI `wrap` rides the publisher; `mcp pin`/`serve` consume the SDK surface directly. Fixture e2e, SDK, and CLI suites green; behavior unchanged.

*(P1/P2 are listed here only because they unblock the native integrations and bridge-helper parity; ownership of the general-SDK surfaces lives with the SDK matrix, not this adapter plan.)*

**P1 — Python Net SDK surfaces for the native node path** (this is general-SDK work, not bridge-module work): pins/consent in the Python binding, async publish/invoke handlers (closes the handler gap via `async_bridge`), plus thin bindings for the two pure bridge helpers. Acceptance: concurrent pin test — pin approved via `net mcp pin approve` visible from Python; pin written from Python honored by a running shim; concurrent access, no corruption. **Unlocks the Hermes native-node plan.**

> **Status: P1 landed 2026-07-05** (branch `mcp-sdk`). New `consent` + `mcp` binding features: `CapabilityId` / `ConsentPolicy` / `PinStore` / `AsyncPinStore` wrap `net_sdk::consent`+`pins` (path-scoped handle — every mutation is the Rust core's locked transaction, GIL released; states/decisions cross as the enums' stable strings), and `classify_mcp_server` / `lower_mcp_tool` bind the two pure helpers (JSON DTOs for cross-binding parity; secret-negative tested). Acceptance green: 8-thread concurrent mutations lose nothing; on-disk format identical both ways; live cross-process test against the real `net mcp pin` CLI (gated on `NET_MESH_BIN`). The async handler gap named above was already closed by the async-bridge waves — `PyAsyncRpcHandler` + `dispatch_handler_coro` dispatch `async def` handlers with cancel propagation, regression-tested in `tests/test_async_interop.py` across all four sync/async caller×server quadrants; remaining follow-up is an end-to-end `serve_tool`-with-`async def`-handler ergonomics test at the tool layer.

**P2 — TypeScript equivalents** (napi): same SDK surfaces + helpers, Promise-native. **Unlocks OpenClaw.**

**P3 — Go bridge helper bindings (cgo over `net_mcp.h`).** Spawn/wrap mode, `lower`/`classify` helpers, shim/gateway DTOs where needed, context cancellation for bridge calls. No attach-mode callbacks in v1. Acceptance: same suite.

**P4 — C.** `net_mcp.h` documented as public (it exists implicitly under Go from P3); callback contracts, free-functions, ownership rules spelled out like the existing headers. Acceptance: header-only consumer sample compiles and passes the fixture round-trip.

## Conformance (extends the golden-vector suite)

- DTO vectors: `CapabilityId`, pin records, consent decisions, descriptor + bridge tags — byte-identical across all five.
- Behavior vectors: credential classification parity (same inputs → same status/tags in every binding), validation-error parity (same bad args → same field-naming error).
- **Concurrent pin-store test in every binding** — the lock protocol is the contract; this is the test that keeps doctrine 1 honest.
- Secret negative test per binding (doctrine 3).
- Spawn-mode round-trip per binding against `net-mcp-fixture`, including the erroring/slow/schema-changing tools.
- Shim round-trip: a Net capability exposed through `net mcp serve` and invoked by a real MCP client.
- Helper parity: `lower(...)` and `classify(...)` produce identical DTOs/tags across all bindings.

## Non-goals

Binding the forwarding seal/keychain internals; binding the shim's JSON-RPC loop; any binding-side reimplementation of lock, classification, or validation; new constructions that deviate from the per-domain module pattern.

## Risks

| Risk | Mitigation |
|---|---|
| Matrix tax: every bridge API × 5 | Surface table above is the whole v1 API; additions need named consumers first (SDK matrix rule) |
| Attach mode creeps back into the critical path | It's appendix/future; nodes participate via the SDK, adapters attach — revisit only for hosts that can't embed a node |
| Adapter crate becomes a second SDK | Decided: pins/consent/validation live in `net-mesh-sdk`; `net-mesh-mcp` only consumes them. CI forbids direct store access from bridge bindings |
| Pin file lock reimplemented "just for tests" in some binding | Concurrent conformance test + review rule: bindings may not open the store file directly |
| Async handler gap resurfaces mid-P1 | It's a named P1 deliverable, not an assumption |
