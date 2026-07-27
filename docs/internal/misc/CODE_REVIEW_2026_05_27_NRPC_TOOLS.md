# Code Review — `nrpc-tools` branch (2026-05-27)

Scope: AI tool-calling layer across the Rust SDK, Node, Python, and Go
bindings. 79 files / ~20k insertions vs `master`. Three reviewer agents
ran in parallel (reuse, quality, efficiency) — this document consolidates
their findings.

## Summary

Fixed in this pass (commits on the `nrpc-tools` branch):

| Tag | Commit | What |
|---|---|---|
| F-1 | `68c294c9` | Go `ToolListChange` wire-shape divergence (both Go trees + tests + docs) |
| F-2 | (earlier) | FFI `net_rpc_list_tools` hand-rolled JSON → derived `Serialize` |
| F-3 | (earlier) | `MeshNode::list_tools` redundant clone-then-overwrite |
| F-4 | (earlier) | `mustMarshal` symbol collision with `go/benchmark_test.go` |
| Q-5, D-7 | `cdf10dec` | Collapse `ToolEventStream` state + use `is_terminal_event` helper |
| E-4 | `5c9b7256` | Bounded mpsc for `ToolListWatch` |
| E-1 | `3c541775` | `list_tools` borrows membership `BTreeMap` directly (no rebuild) |
| D-4 | `ca9e3e61` | Centralize tool capability tag + metadata-key literals (Go) |
| D-6 | `7e6cd343` | Extract Python `_coerce_descriptor` helper |
| E-8 | `3ff81549` | Server-side `missing_terminal` synthesis in Node + Python |
| C-3, E-9 | `1e644bb0` | Drop per-rpc tool registry + fetch handler on last close (Python + Go) |
| E-3 | `8fdac609` | Cache `ToolMetadataRegistry::snapshot()` as `Arc<[T]>` |
| Q-6 | `de91a0c8` | Go `ToolCallSpec.ProviderCallID` switched to `*string` |
| E-2 | `624fc67a` | `CapabilitySet::add_tools` batch API; announce path now O(N) |
| E-7 | `5becd1c9` | Python `list_tools` returns JSON string (drops 12-set_item-per-descriptor) |

Outstanding — grouped by impact below. The remaining items are either
larger refactors, design decisions, or high-churn cleanups; none block
the branch.

---

## Outstanding — Correctness / contract drift

### C-1. Handler-error contract is inconsistent across languages

- Rust `serve_tool_streaming` (`sdk/src/tool.rs:391-427`) lets panics
  propagate to the runtime task.
- Node `tool.ts` catches via `try/catch` and emits a terminal
  `handler_error` envelope.
- Python `tool.py` catches `Exception` and emits a terminal envelope.
- Go `tool.go` uses `panic`/`recover` and emits a terminal envelope.

A panicking Rust handler kills the runtime task instead of surfacing a
typed error to the client. Pick one contract — recommended: all four
catch and convert to a terminal `handler_error` envelope, with the
Rust side wrapping the handler in `catch_unwind`.

**Status:** needs design discussion before fix.

### C-2. `MeshNode::list_tools` panics on matcher validation

`net/crates/net/src/adapter/net/mesh.rs` — `list_tools` panics on
`TagMatcher::validate` error, with the rationale "mirrors existing
capability_aggregation contract." The substrate's `aggregate()` also
panics, so the leak propagates: a fix would change the contract at
the substrate level, not just `list_tools`.

**Status:** substrate-wide contract change, deferred.

### C-4. Silent fetch-installer failure across all four languages

When `tool.metadata.fetch` auto-install fails, all four implementations
swallow the error and leave the handle as `None`/`nil`. Failure mode
visible to the calling agent is "NoRoute on fetch_tool_metadata" with
no diagnostic anywhere.

**Status:** the SDK and bindings currently have no logging
infrastructure (no `tracing` dep in the SDK crate, no `logging` use
elsewhere in Python/Node tool.* paths). Adding logging just for this
single warning is out of scope; would need a project-wide observability
decision first.

---

## Outstanding — Cross-binding duplication (needs FFI surface)

### D-1. Format translators reimplemented in three bindings (~600 LOC)

The Rust SDK has canonical `to_<provider>_tool(&ToolDescriptor) -> Value`
and `lower_<provider>_*(&Value) -> Result<ToolCallSpec, _>` in
`net_sdk::tool::formats` for OpenAI, Anthropic, MCP, and Gemini. The
FFI layer doesn't expose them, so each binding hand-rolls byte-identical
lowering logic:

- Node: `bindings/node/tool.ts`
- Python: `bindings/python/python/net/tool.py`
- Go: `go/tool.go`

**Status:** assessed during the refactor pass; **deferred with
rationale**. Each binding's implementation is ~180 LOC of trivial
pure-logic translator code, and the T-1 golden-vector fixture catches
any cross-language drift on first CI run. Routing through FFI would
add JSON serialize/deserialize on every call (these are hot — once
per LLM request to a provider), tighten the TS/Python package coupling
to the published cdylib, and replace TS/Python logic with FFI plumbing
of roughly equivalent LOC. The net codebase win is small; the runtime
cost is non-trivial. Keep four parallel implementations pinned by
golden vectors.

### D-2. `watch_tools` polling+diff loop in four languages (~250 LOC)

Each binding hand-rolls baseline-then-poll-then-diff, keying by
`(tool_id, version)` and emitting Added/Removed/NodeCountChanged. The
streaming FFI scaffold landed on this same branch — adding a
`net_rpc_watch_tools` that returns a stream handle would let bindings
collapse the polling+diff to a stream-decode loop.

**Status:** assessed during the refactor pass; **deferred with
rationale**. Each binding still needs its own idiomatic cancellation
wrapper around the FFI stream (Node `AbortSignal`, Python async
iterator close, Go `context.Context`), so the realistic LOC savings
are closer to ~150 across three bindings, not 250. The polling
behavior itself is identical to the local-diff approach (substrate
`watch_tools` also polls). Net codebase win is real but not large;
keep the per-binding implementations.

### D-3. Two Go trees have diverged

`go/tool.go` (~1127 LOC, the published `net.go.dev` module) vs
`net/crates/net/bindings/go/net/tool.go` (~968 LOC, the in-tree binding
scaffold). The repo-root tree carries FFI-backed `ListTools` and the
flattened `ToolEvent` envelope; the in-tree tree is the older Wave-3
starting point.

The intent (per "the two go trees are kept in sync") is silently
violated. A `diff -q` CI guard would fail today — the right fix is
either to designate one tree canonical and generate the other (codegen,
symlink, `go:embed`), or to formalize which surface lives where.

**Status:** needs strategy decision before fix.

### D-5. `add_tool_capabilities_to_announce` hand-built in 3 bindings

Documented v1 stopgap until the bindings can expose
`tool_registry().insert/remove`. The Rust substrate already does this
server-side. Tracking the sunset; no action this pass.

### D-8. Golden-vector test driver reimplemented in five files

Resolves on its own once D-1 lands (the bindings' tests collapse to
thin "load fixture → call FFI → assert" loops). Until then, keep the
parallel test drivers.

---

## Outstanding — Efficiency

### E-2. O(N²) announce merge for tools (RESOLVED — commit `624fc67a`)

`mesh.rs` previously called `merged.add_tool(cap)` in a loop. Added
batch `CapabilitySet::add_tools(impl IntoIterator<...>)` that calls
`set_tools` exactly once; the announce path now builds the
`Vec<ToolCapability>` from the registry snapshot upfront and merges
in one call. Total cost dropped from O(N²) → O(N). `with_metadata`
chaining is a no-op walk today because `METADATA_RESERVED_PREFIXES`
is empty.

### E-5. Watchers don't short-circuit unchanged snapshots

All four watchers re-walk `list_tools` every interval and run the
Added/Removed/NodeCountChanged diff regardless of change. Equal
snapshots emit zero events (correct) but the walk and three loops
execute regardless.

**Status:** a cheap "equal snapshots" check isn't strictly cheaper than
the existing diff without maintained incremental state. Deferred.

### E-6. Fixed poll cadence, no backpressure, no jitter

All four watchers use a single fixed interval (default 1s) with no
adaptive backoff for quiet folds and no jitter to prevent
thundering-herd polls across multiple agent processes.

**Status:** design needed (interval policy, jitter scheme).

### E-7. Python pyo3 `list_tools` sets 12 `PyDict` items per descriptor (RESOLVED — commit `5becd1c9`)

The pyo3 binding now does a single `serde_json::to_string` of the full
`Vec<ToolDescriptor>` and the Python wrapper parses it once with
`json.loads`. The downstream `ToolDescriptor` dataclass construction
in `net.tool.list_tools` is unchanged; only the FFI hop is cheaper.
Chose the serde-string path over a `#[pyclass]` registration: same
end shape with much less Rust boilerplate, and the cost-vs-benefit is
clear when the caller is already going to `json.loads` if it needs
the parsed schemas anyway.

---

## Outstanding — Quality / style

### Q-1. SDK `Mesh::serve_tool` reaches into substrate registry internals

`sdk/src/tool.rs:282-333` — the SDK does
`self.inner().tool_registry().clone()` to grab the substrate's
mutex-protected registry directly. A future change to the registry
(e.g. event emission on insert) needs to happen at all SDK call sites
instead of one substrate method.

**Status:** needs substrate-side `tool_registry_install(descriptor, handler)`
method. Deferred.

### Q-2. Module-level narrative docstrings reference plan slice IDs

Every binding's `tool.{ts,py,go,rs}` opens with a 20-line block that
narrates "Wave 3 / B-1 starting point. v1 covers unary register +
invoke. Streaming (B-2) and discovery (B-3 list_tools / watch_tools)
follow…" These reference plan slice IDs (`A-1`, `B-2`, `T-1`) that
go stale the instant the plan moves.

**Status:** high-churn cleanup, deferred. (Strip pre-v1.)

### Q-3. Narrative body comments

- `sdk/src/tool.rs:296-297`, `:574-584`
- `cortex/tool.rs:438-441`
- `go/tool.go:332-336`

**Status:** high-churn cleanup, deferred.

### Q-4. Duplicate `ListTools` in Go

After investigation: the package-level `ListTools(rpc)` takes
`*TypedMeshRpc` and the method `(r *MeshRpc) ListTools()` is on the
raw type — different receivers, not pure duplication. The reviewer's
"pick one" call would force users to either expose the `raw` field
or live with two APIs. No clean fix without API design work.

**Status:** false positive on second look.

### Q-7. `serve_tool_streaming` overrides `descriptor.streaming` post-construction

Four bindings use four idiomatic mutation patterns. Moving into a
builder requires changing `descriptor_for` / `descriptorFrom` /
`ToolDescriptor::builder()` signatures across all four languages.

**Status:** larger API change, deferred.

### Q-8. `Mesh::serve_tool` step-2 failure leaves auto-install state stuck

Documented in the source as acceptable for v1 (low cost, recoverable).
No action.

### Q-9. Cortex feature gating is sprinkled rather than grouped

Five `#[cfg(feature = "cortex")]` markers added piecemeal as
retro-gates. Cosmetic; deferred.

### Q-10. `tool_metadata_fetch` field requires four cfg lines

The `Arc<parking_lot::Mutex<Option<ServeHandle>>>` field needs
`#[cfg(feature = "tool")]` everywhere it's referenced. Moving it to
the substrate's `MeshNode` would clean this up.

**Status:** substrate-side change, deferred.

---

## Source

Three reviewer agents (reuse / quality / efficiency) ran in parallel
on 2026-05-27. Each fix above includes the originating finding tag
(F-1, C-3, E-3, etc.) in its commit message so future readers can
trace any change back to its review-pass entry.
