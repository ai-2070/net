# Code Review — `nrpc-tools` branch (2026-05-27)

Scope: AI tool-calling layer across the Rust SDK, Node, Python, and Go
bindings. 79 files / ~20k insertions vs `master`. Three reviewer agents
ran in parallel (reuse, quality, efficiency) — this document consolidates
their findings.

## Summary

Fixed in this pass:

- Go `ToolListChange` wire-shape divergence (both Go trees + tests + docs)
- FFI `net_rpc_list_tools` hand-rolled JSON → derived `Serialize`
- `MeshNode::list_tools` redundant clone-then-overwrite
- `mustMarshal` symbol collision with `go/benchmark_test.go`

Outstanding — grouped by impact below. None block the branch, but several
should be addressed before this surface is taken to v1.

---

## Fixed

### F-1. Go `ToolListChange` wire-shape divergence (correctness)

`go/tool.go` and `net/crates/net/bindings/go/net/tool.go` emitted
`{type, tool_id, version, tool, old_count, new_count}`. Rust SDK, Node
TS, and Python all emit `{type, descriptor, prev_node_count}`. The
docstring claimed wire-compat that didn't exist. The `Removed` variant
also dropped the descriptor (other languages carry it).

Both Go trees, both test files, and `AGENT_TOOLS.md` updated to the
canonical shape.

### F-2. FFI `net_rpc_list_tools` rebuilt JSON from scratch (efficiency + maintenance)

`net/crates/net/bindings/go/rpc-ffi/src/lib.rs:3537-3554` did a 17-line
`serde_json::json!({...})` per descriptor. `ToolDescriptor` already
derives `Serialize` with snake_case field names. Replaced with
`serde_json::to_vec(&descriptors)`. Adding a field now needs one edit
instead of two.

### F-3. `MeshNode::list_tools` clone-then-overwrite (efficiency)

`net/crates/net/src/adapter/net/mesh.rs` did
`or_insert_with(|| (descriptor.clone(), HashSet::new()))` followed by
`bucket.0 = descriptor`. The clone was always discarded. Switched to
an `Entry::{Occupied,Vacant}` match so the vacant path moves the
descriptor directly. Hot path for `watch_tools` polls.

### F-4. `mustMarshal` redeclared in package `net` (compile error)

`go/mesh_rpc_typed.go:312` added `func mustMarshal(v interface{}) []byte`,
which collided with the pre-existing `func mustMarshal(v interface{}) string`
in `go/benchmark_test.go:43`. Renamed the new helper to
`mustMarshalBody`. The in-tree binding doesn't carry the benchmark file
and was left as-is.

---

## Outstanding — Correctness / contract drift

### C-1. Handler-error contract is inconsistent across languages

- Rust `serve_tool_streaming` (`sdk/src/tool.rs:391-427`) lets panics
  propagate to the runtime task.
- Node `tool.ts:347-362` catches via `try/catch` and emits a terminal
  `handler_error` envelope.
- Python `tool.py:925-936` catches `Exception` and emits a terminal
  envelope.
- Go `tool.go:444-464` uses `panic`/`recover` and emits a terminal
  envelope.

A panicking Rust handler kills the runtime task instead of surfacing a
typed error to the client. Pick one contract — recommended: all four
catch and convert to a terminal `handler_error` envelope, with the
Rust side wrapping the handler in `catch_unwind`.

### C-2. `MeshNode::list_tools` panics on matcher validation

`net/crates/net/src/adapter/net/mesh.rs` — `list_tools` panics on
`TagMatcher::validate` error, with the rationale "mirrors existing
capability_aggregation contract." But `list_tools` is reachable from
the public `Mesh::list_tools` SDK method; a user-supplied matcher
should get a `Result`, not a panic. Either both substrate methods
return `Result`, or the SDK wrapper validates up front.

### C-3. Per-rpc fetch-handler registries leak in Python and Go

- Python `python/python/net/tool.py:210, 252`: `_tool_registries[id(rpc)] = entry`
  is never removed. When an rpc is closed/GC'd, `id()` can be reused
  but the old entry stays in the dict, pinning the underlying nRPC
  handler.
- Go `go/tool.go:327-330, 373`: `toolRegistries[rpc] = entry` with no
  delete path. Long-lived processes that recycle rpc instances leak
  one registry + one nRPC handler per cycle.

Rust SDK (`sdk/src/tool.rs:559-592`) stores the slot on `Mesh` itself
so it dies with the `Mesh` — port that lifetime contract back to
Python and Go.

### C-4. Silent fetch-installer failure across all four languages

When `tool.metadata.fetch` auto-install fails, all four implementations
swallow the error and leave the handle as `None`/`nil`. Failure mode
visible to the calling agent is "NoRoute on fetch_tool_metadata" with
no diagnostic anywhere. At minimum, log/trace.

Files: Rust `sdk/src/tool.rs:559-592`, Node `tool.ts:239-256`, Python
`tool.py:243-250`, Go `tool.go:337-374`.

---

## Outstanding — Cross-binding duplication (needs FFI surface)

### D-1. Format translators reimplemented in three bindings (~600 LOC)

The Rust SDK has canonical `to_<provider>_tool(&ToolDescriptor) -> Value`
and `lower_<provider>_*(&Value) -> Result<ToolCallSpec, _>` in
`net_sdk::tool::formats` for OpenAI, Anthropic, MCP, and Gemini. The
FFI layer doesn't expose them, so each binding hand-rolls byte-identical
lowering logic:

- Node: `bindings/node/tool.ts:703-883`
- Python: `bindings/python/python/net/tool.py:612-813`
- Go: `go/tool.go:724-894`

The T-1 golden-vector fixture exists precisely because these four
implementations have to be re-pinned independently. Adding the eight
entry points to `rpc-ffi` (and skipping the C ABI for napi/pyo3 — call
the Rust crate directly) collapses ~180 LOC per binding to thin
marshalling wrappers.

### D-2. `watch_tools` polling+diff loop in four languages (~250 LOC)

Each binding hand-rolls the same baseline-then-poll-then-diff pattern,
keying by `(tool_id, version)` and emitting Added/Removed/NodeCountChanged.
The streaming FFI scaffold landed on this same branch (`net_rpc_serve_streaming` +
`net_rpc_set_streaming_handler_dispatcher`) — adding a `net_rpc_watch_tools`
that returns a stream handle lets all three bindings collapse the
polling+diff to a stream-decode loop.

### D-3. Two Go trees have diverged

`go/tool.go` (1094 LOC) vs `net/crates/net/bindings/go/net/tool.go`
(937 LOC) — 208-line delta in the working code. `mesh_rpc.go` differs
by ~270 lines, `mesh_rpc_typed.go` by ~20.

The intent (per "the two go trees are kept in sync") is silently
violated. Either pick one canonical tree and generate the other (codegen,
symlink, `go:embed`), or add a CI guard:

```bash
diff -q go/tool.go net/crates/net/bindings/go/net/tool.go || exit 1
```

### D-4. Stringly-typed constants repeated 4×

- `TOOL_METADATA_FETCH_SERVICE` ("tool.metadata.fetch") defined in
  Rust (`cortex/tool.rs:383`), Node (`tool.ts:633`), Python (`tool.py:368`),
  Go (`tool.go:479`).
- `"missing_terminal"` error code in Rust (`sdk/src/tool.rs:410`), Node
  (`tool.ts:435`), Python (`tool.py:360`), Go (`tool.go:597`).
- `"ai-tool:"` capability prefix in Node (`tool.ts:467`), Python
  (`tool.py:554`), Go (`tool.go:666`).
- Go also hard-codes `"tool::" + id + "::input_schema"` (and the four
  sibling key shapes) inline at `go/tool.go:675-695` instead of calling
  the substrate helpers. Rename the prefix in the substrate → Go
  silently breaks.

One rename → three drifts. Centralize the literals at minimum within
each language; expose substrate helpers through FFI where the constant
is wire-significant.

### D-5. `add_tool_capabilities_to_announce` hand-built in 3 bindings

Node `tool.ts:459-482`, Python `tool.py` (one definition), Go
`tool.go` (`AddToolCapabilitiesToAnnounce`). The Rust substrate already
does this server-side in `MeshNode::announce_capabilities_with`. The
binding-side implementations are documented v1 stopgaps until the
bindings can expose `tool_registry().insert/remove`. Track the sunset
explicitly so it doesn't fossilize.

### D-6. Python `serve_tool` × 4 variants duplicate options-coercion

`serve_tool` (L256), `serve_tool_async` (L1023), `serve_tool_streaming`
(L890), `serve_tool_streaming_async` (L948) each open with the same
12-line `if isinstance(...)` block coercing
`options_or_descriptor: ToolDescriptor | dict | str` into a
`ToolDescriptor`. The streaming variants also duplicate a 10-line
handler-error envelope shim. Extract `_coerce_descriptor` +
`_wrap_handler_for_streaming` private helpers; the four public
functions then become ~10 LOC each.

### D-7. Python `call_tool_streaming` inlines `is_terminal_event`

`python/python/net/tool.py:350` does
`event.get("type") in ("result", "error")` inline; the module already
exports `is_terminal_event` (L121) and Node/Go use the helper. Trivial
fix; eliminates a place where the terminal-event taxonomy can drift.

### D-8. Golden-vector test driver reimplemented in five files

Same fixture-load → `descriptor_from_fixture` → loop-cases pattern in
Rust SDK tests, Node tests, Python tests, and *both* Go trees. The
two Go test files differ only in the fixture-path string. Once D-1
lands the bindings tests collapse to thin "load fixture → call FFI →
assert" loops; until then, at minimum factor the fixture-path resolver.

---

## Outstanding — Efficiency

### E-1. `Mesh::list_tools` inner loop is a hot-path allocator (`mesh.rs:5426-5488`)

Runs once per `(class, node)` capability entry, hit by `watch_tools`
every poll. Remaining issues after F-3:

- L5449-5453: rebuilds the full `metadata` `HashMap<String,String>` by
  cloning every `(k, v)` from `membership.metadata` (a `BTreeMap`).
  Only `get(&format!(...))` reads from it — could borrow the BTreeMap
  directly.
- L5461 / L5468 / L5473 → `from_capability` (`cortex/tool.rs:149/155/159`):
  five `format!("tool::{tool_id}::…")` allocations per tool per entry
  per poll. Cache the three key suffixes once per `(tool_id, version)`
  bucket, or use `BTreeMap` prefix range, or stamp the keys at codec time.

With N nodes × M tools per node, this runs N×M times per `watch_tools`
tick. 50 nodes × 10 tools × 1s default poll = 500 inner-loop
iterations/sec of mostly avoidable allocation.

### E-2. O(N²) announce merge for tools

`mesh.rs:8113-8164` — in `announce_capabilities`, for every descriptor
in `tool_registry.snapshot()` (N entries) the loop does
`merged = merged.add_tool(cap)`. `add_tool` in `capability.rs:919-924`
clones the full `Vec<ToolCapability>` from `views().tools().clone()`,
pushes one, then `set_tools` clears all tool tags, walks/retains
metadata, re-encodes every tool, and re-inserts schemas into metadata
(`capability.rs:1261-1300`). One announce with N tools → O(N²) tag
rebuilds.

Add a batch `CapabilitySet::add_tools(impl IntoIterator<Item=ToolCapability>)`
that calls `set_tools` exactly once. Same for `with_metadata` chaining
at L8148-8161 — each call walks the reserved-prefix list
(`capability.rs:1164-1170`).

### E-3. `ToolMetadataRegistry::snapshot()` clones full Vec on every announce

`cortex/tool.rs:484-486` — `snapshot()` clones every descriptor; called
by `mesh.rs:8122` inside every `announce_capabilities`. With a stable
tool set this rebuilds N descriptors on every announce. An
`Arc<[ToolDescriptor]>` cached snapshot (rebuilt only on insert/remove)
would let the announce path iterate by reference. Combined with E-2
this is the worst announce-path hotspot.

### E-4. `ToolListWatch` uses unbounded mpsc

`cortex/tool.rs:287` — `tokio::sync::mpsc::unbounded_channel`. Slow
consumer + flapping fold = unbounded growth. Node and Go bindings
correctly use buffered channels (Node per-await sleep; Go cap=16 at
`tool.go:997`). Switch to `mpsc::channel(capacity)` with a documented
backpressure policy (lossy-drop-oldest or block-the-poll).

### E-5. Watchers don't short-circuit unchanged snapshots

All four watchers re-walk `list_tools` every interval and run the
full Added/Removed/NodeCountChanged diff regardless of whether
anything changed. Equal snapshots emit zero events (correct) but the
walk and three loops execute regardless. A byte-level snapshot
comparison or version stamp would skip the diff pass for the common
steady-state.

Files: Rust `mesh.rs:5570-5608`, Node `tool.ts:597-625`, Python
`tool.py:466-483`, Go `tool.go:1018-1019`.

### E-6. Fixed poll cadence, no backpressure, no jitter

All four watchers use a single fixed interval (default 1s) with no
adaptive backoff for quiet folds and no jitter to prevent
thundering-herd polls across multiple agent processes on the same
host. Add exponential backoff up to a ceiling when M consecutive
ticks produce no events; reset on first event.

### E-7. Python pyo3 `list_tools` sets 12 PyDict items per descriptor

`bindings/python/src/lib.rs:1914-1944` — 12 `entry.set_item(...)` calls
per descriptor, each a fallible call into CPython's `PyDict_SetItem`.
Either register `ToolDescriptor` as a `#[pyclass]` (parallel to the
Node `ToolDescriptorJs`), or serialize once via `serde_json::to_string`
and `json.loads` on the Python side (single FFI hop).

### E-8. Server-side `missing_terminal` not emitted by Node / Python wrappers

Rust `sdk/src/tool.rs:398-419` synthesizes a `missing_terminal` frame
server-side when a handler returns without a terminal event. Node
`tool.ts:346-362` and Python `tool.py:925-934` don't — they only
catch exceptions. Clients of the bindings get no terminal at all in
the "clean return without terminal" case unless the client itself
also synthesizes one (Node `tool.ts:431-440`, Python `tool.py:357-364`).
Asymmetric contract; non-binding clients (raw stream consumers, other
languages) see broken streams.

### E-9. Auto-installed `tool.metadata.fetch` handler never unregisters

All four bindings idempotently install the fetch handler on first
`serve_tool`, but no code path drops it when the last tool closes.
The empty-registry handler stays answering `NotFound` forever. Minor
(one nRPC handler row per process) but worth a `Drop`-on-last-close.

---

## Outstanding — Quality / style

### Q-1. SDK `Mesh::serve_tool` reaches into substrate registry internals

`sdk/src/tool.rs:282-333` — the SDK does
`self.inner().tool_registry().clone()` to grab the substrate's
mutex-protected registry directly, then manually `insert`s,
`remove`s on rollback, and on `Drop`. The substrate exposes the
registry handle just for this, but the SDK is now responsible for
atomic semantics that should live behind a single substrate method
like `tool_registry_install(descriptor, handler)`. A future change
to the registry (e.g. event emission on insert) needs to happen at
all SDK call sites instead of one substrate method.

### Q-2. Module-level narrative docstrings reference plan slice IDs

Every binding's `tool.{ts,py,go,rs}` opens with a 20-line block that
narrates "Wave 3 / B-1 starting point. v1 covers unary register +
invoke. Streaming (B-2) and discovery (B-3 list_tools / watch_tools)
follow once the underlying napi surface exposes them…" Examples:
`tool.ts:1-22`, `tool.py:1-20`, `go/tool.go:1-18`, `sdk/src/tool.rs:1-27`,
`cortex/tool.rs:1-23`. These reference plan slice IDs (`A-1`, `B-2`,
`T-1`) that go stale the instant the plan moves; per the repo's
no-narration rule these belong in the PR description, not source.

### Q-3. Narrative body comments

- `sdk/src/tool.rs:296-297`: "Step 1: registry insert. Done before
  the handler so the descriptor is observable…" — narrates obvious
  order.
- `sdk/src/tool.rs:574-584`: 10-line comment explaining why
  install-failure is silent.
- `cortex/tool.rs:438-441`: "`Default::default()` works too; keeping
  the named constructor so call sites read clearly." — explains a
  1-line `new()`.
- `go/tool.go:332-336`: restates what the function name says, then
  narrates the install.

Per the repo's strict no-narration rule: delete unless the WHY is
non-obvious.

### Q-4. Duplicate `ListTools` in Go

`go/tool.go:907` (package-level `ListTools(rpc)`) and `:918` (method
`(r *MeshRpc) ListTools()`) — the free function just delegates to
the method. Callers see two equivalent APIs returning the same data.
Pick one.

### Q-5. Redundant state in `ToolEventStream` (Go)

`go/tool.go:572-602` — three booleans (`sawTerminal`, `synthesized`,
plus `inner`'s own state) tracking one state machine. Collapse to
one `state` field or compute `synthesized` from
`sawTerminal == false && inner.exhausted`.

### Q-6. Go `ToolCallSpec.HasProviderCallID` should be `*string`

`go/tool.go:709-714` — `HasProviderCallID bool` + `ProviderCallID string`
instead of `*string`. Doesn't match Rust `Option<String>`, Python
`Optional[str]`, Node `string | undefined`. A Go consumer can't tell
from JSON alone if `""` means absent or empty.

### Q-7. `serve_tool_streaming` overrides `descriptor.streaming` post-construction

`sdk/src/tool.rs:373`, `go/tool.go:443`, `python/tool.py:923`,
`node/tool.ts:341` all overwrite the `streaming` flag with four
different idioms. Move the override into the builder (`descriptor_for(name, streaming=True)`)
so it lives in one place.

### Q-8. `Mesh::serve_tool` step-2 failure leaves auto-install state stuck

`sdk/src/tool.rs:271-272` notes "auto-install (if it happened in this
call) stays in place." Dead state if every subsequent `serve_tool`
also fails — a registry entry that says "fetch installed" with no
users. Acceptable for v1 but worth a more visible `TODO`.

### Q-9. Cortex feature gating is sprinkled rather than grouped

`net/crates/net/src/adapter/net/mesh.rs` has five new
`#[cfg(feature = "cortex")]` markers around `aggregator_registry`,
`set_aggregator_registry`, `aggregator_registry()`,
`index_self_with_local_services`, and the test, placed piecemeal as
retro-gates to fix a `--features net`-only build. Consider grouping
these under one `#[cfg(feature = "cortex")] impl MeshNode { ... }`
block so future devs see one boundary.

### Q-10. `tool_metadata_fetch` field requires four cfg lines

`sdk/src/mesh.rs:340-353` and `:1010-1011` — the
`tool_metadata_fetch: Arc<parking_lot::Mutex<Option<ServeHandle>>>`
field needs `#[cfg(feature = "tool")]` everywhere it's referenced
(`build`, `from_node_arc`, the field itself, plus the impl block in
`tool.rs`). Consider stashing it on the substrate's `MeshNode` so
the SDK doesn't add yet another optional struct member.

---

## Source

Three reviewer agents (reuse / quality / efficiency) ran in parallel.
Full per-agent transcripts available in the conversation that produced
this document (2026-05-27).
