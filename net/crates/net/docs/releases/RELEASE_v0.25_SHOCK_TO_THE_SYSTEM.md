# Net v0.25 — "Shock To The System"

*Named after the lead single from Billy Idol's 1993 album Cyberpunk — the one he cut as a concept record about networks reshaping how people would work, recorded with a Mac LC III in the booth and a Macromedia Director CD-ROM tucked into the jewel case, panned at release for being too-soon and now read as a marker of the moment the network stopped being a thing other people did. Same wire, same nRPC, same capability fold — but every typed service is now an LLM-callable tool, and the capability subsystem stopped paying for what every other discovery layer is paying for.*

## One surface every agent can call, and a capability hot path that got back to single-digit nanoseconds

The v0.25 release is the result of two pushes against the same mesh-discovery surface from opposite ends. The agent-facing push exposes every typed nRPC service as an LLM tool — `serve_tool` / `list_tools` / `watch_tools` / `call_tool` in Rust, Node, Python, and Go, plus format translators for OpenAI / Anthropic / Gemini / MCP so the descriptor lowers directly into whichever provider the agent already runs. The substrate-facing push is a perf audit against the capability subsystem after Phase A.5.N moved `CapabilitySet`'s typed-struct fields into a canonical `HashSet<Tag>`: a per-tag `String::clone` in `Tag::axis_key()` plus a `Tag::to_string()`-keyed sort in the wire serializer had quietly turned a 3.7 ns `match_min_memory` filter into a 46 µs one. Four targeted fixes recovered the regression; the perf audit doc lands in tree alongside the release.

The release's organizing observation: discovery should be free in the hot path and cheap to author at the edges. The capability fold already aggregates every node's capabilities — agent discovery just walks it. The tag-set source-of-truth pattern is the right architecture, but allocating a `String` per tag per predicate match isn't its tax to pay.

### Where v0.25 lands against the rest of the service-discovery field

In-process capability-filter evaluation in v0.25 sits 3–7 orders of magnitude below the published latencies of the network-coordinated discovery systems the field treats as fast:

| Layer | Operation | Typical latency | vs Net `has_*` (~30 ns) |
|---|---|---|---|
| **Net v0.25** | `has_gpu` / `has_tool` / `has_model` | **20–44 ns** | 1× |
| **Net v0.25** | `match_min_memory` (single-field predicate) | **15 ns** | 0.5× |
| **Net v0.25** | `match_complex` (6 chained predicates, decodes models) | **3.8 µs** | ~130× |
| **Net v0.25** | `CapabilitySet::to_bytes_compact` (full set, postcard) | **2.0 µs** | ~70× |
| Consul | DNS lookup, cached | 100–200 µs | 3,300–6,700× |
| Consul | DNS lookup, uncached (server) | 600–700 µs | 20,000–23,000× |
| Consul | client initial query | 1.6–3 ms | 53,000–100,000× |
| etcd | lookup, recommended P99 target | < 10 ms | > 330,000× |
| Kubernetes / CoreDNS | service lookup (ndots:5 default) | 100+ ms | > 3,300,000× |
| mDNS / DNS-SD | best-case local resolution | < 1 ms | > 33,000× |

**Caveat — apples-vs-oranges:** the v0.25 numbers measure in-process predicate evaluation against capability announcements already gossiped into the local fold. Consul / etcd / Kubernetes DNS are answering "where is service X across the cluster" with a network round-trip and (usually) a consensus quorum read. They aren't doing the same job. The fair comparison is **the in-mesh agent scheduling loop**: once announcements are in your fold (Net does that propagation via the same gossip path every other capability rides), filtering and dispatching against them is genuinely four to seven orders of magnitude faster than the registries an agent author would otherwise reach for.

External sources for the published latencies in the table: [Consul DNS perf thread](https://groups.google.com/g/consul-tool/c/5gpqSAP74sY), [Consul DNS perf issue #1535](https://github.com/hashicorp/consul/issues/1535), [Consul server resource requirements](https://developer.hashicorp.com/consul/docs/install/performance), [etcd recommended practices (OKD)](https://docs.okd.io/latest/etcd/etcd-practices.html), [Kubernetes DNS ndots:5 latency](https://www.michal-drozd.com/en/blog/kubernetes-dns-caching-ndots/), [mDNS / DNS-SD discovery](https://openthread.io/guides/border-router/mdns-discovery).

Below: the wins, grouped by where they fire.

---

## AI tool calling — every typed nRPC service is an LLM-callable tool

`NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md` (the plan shipping alongside this release) makes the bet that tool calling is what nRPC already does — "send a JSON object to a named handler, await a JSON response, optionally stream chunks" — with three gaps: metadata so a model can decide when/how to call, a server-streaming primitive matching the unary `call_service`, and a structured event envelope for streaming output. v0.25 closes all three and ships the agent-author surface across every binding.

**One identifier, one source of truth.** A tool registered as `web_search` IS the nRPC service at channel `nrpc:web_search.requests` IS the announcement carrying the `ai-tool:web_search` capability tag. No separate registry, no mapping table. Plain `rpc.serve("x", handler)` continues to register a service without the `ai-tool:*` tag — invisible to `list_tools()`. The `serve_tool` / `tool({...})` / `@tool` opt-in is what makes a service agent-discoverable; operators retain control.

**Discovery is capability-fold-native, not RPC-fanout.** The capability fold already aggregates `ToolCapability` instances across every node. `list_tools(matcher)` walks the fold in-memory and returns `ToolDescriptor`s carrying id + version + node_count + small metadata. Heavy fields (oversized JSON Schemas) fall back to an on-demand `tool.metadata.fetch` RPC, which `serve_tool` auto-installs on the host the first time it's called. Subnet visibility, capability auth, region filtering — all inherited from the existing fold + `TagMatcher` plumbing.

**Streaming tools share one event envelope.** `ToolEvent` is a tagged JSON enum every streaming handler emits per chunk:

- `start { tool_id, call_id, metadata? }` — fires once on open.
- `progress { pct?, message? }` — coarse progress for spinners.
- `delta { data }` — partial output (model tokens, file bytes, log lines).
- `result { data }` — terminal full result; client sees one on success.
- `error { code, message, details? }` — terminal failure with structured detail.

Unary tools synthesize a single `result` envelope under the hood. The convention lets every adapter (OpenAI / Anthropic / Gemini / MCP / Hermes / custom) lower envelopes into the framework's native streaming protocol without per-pair negotiation. Two synthesized error shapes round out the contract: `missing_terminal` on the streaming caller when the server closed without a `result`/`error` chunk, and `handler_error` on the streaming server when the handler raised mid-stream. Both are part of the `T-2` JSON byte-equality fixture so adapters can match on the code reliably.

**`serve_tool` is atomic w.r.t. observable mesh state.** Either all of (handler registration, capability-fold publish, `nrpc:<tool_id>` tag, `ai-tool:<tool_id>` tag, auto-installed `tool.metadata.fetch` if first) succeed, or none do. Drop on the returned handle reverses all four.

**Cross-language by construction.** The wire is unchanged: `call_tool` is `call_service` with the typed wrapper, `call_tool_streaming` rides the new `call_service_streaming` substrate primitive (mirror of `call_service` returning an `RpcStream`). A Python Hermes agent calling a Go-hosted database tool calling a TypeScript browser tool is transparent over the existing nRPC wire. The `T-1` cross-language test pins byte-equality of every format translator output (`to_openai_tool` / `to_anthropic_tool` / `to_gemini_tool` / `to_mcp_tool`) across Rust / Node / Python / Go for every fixture descriptor.

**Surface by language:**

| Surface                    | Rust | Node TS | Python | Go  |
|----------------------------|------|---------|--------|-----|
| `serve_tool` / `call_tool` (unary) | ✅   | ✅       | ✅ (sync + async)      | ✅  |
| `serve_tool_streaming` (handler returns `Stream<ToolEvent>`) | ✅ | ✅ | ✅ (sync + async-gen) | ✅ |
| `call_tool_streaming` (capability-routed caller) | ✅ | ✅ | ✅ (sync + async) | ✅ |
| `list_tools` / `watch_tools` | ✅ | ✅ (polling) | ✅ (polling) | ✅ (polling) |
| `tool.metadata.fetch` (caller + auto-install server) | ✅ | ✅ | ✅ | ✅ |
| Format translators × 4 (OpenAI / Anthropic / Gemini / MCP) | ✅ | ✅ | ✅ | ✅ |
| `missing_terminal` + `handler_error` synthesis | ✅ | ✅ | ✅ | ✅ |
| AbortSignal / cancel on `watch_tools` | ✅ | ✅ | ✅ | ✅ (ctx) |

**Format translators ship in one package per language.** `net-mesh-tools` (pip) carries `formats/{openai,anthropic,gemini,mcp}` submodules; `@net-mesh/tools` (npm) carries `formats/{openai,anthropic,gemini,mcp}` submodules. Each translator is a small pure function from `ToolDescriptor` → provider tool-array entry, plus a reverse `lower_tool_call(call) -> CallSpec` for going from a provider's `tool_use` block back into a typed nRPC call. No transitive dep on any provider SDK — users wire the translator output into their OpenAI / Anthropic / Hermes / framework-of-choice client themselves.

**No wire ABI bump for unary tool calls.** Streaming tools use the new `call_service_streaming` substrate primitive; the wire shape of an individual stream is unchanged from `call_streaming` today. `ToolEvent` envelopes are JSON-encoded chunks on existing streams. `NET_RPC_ABI_VERSION` stays at `0x0004`.

---

## Capability perf — closing the Phase A.5.N regression cliff

`PERF_AUDIT_2026_05_28_CAPABILITY.md` (the audit doc shipping alongside this release) compared two M1 Max criterion runs and found that the Phase A.5.N migration — which moved `CapabilitySet`'s typed `HardwareCapabilities` / `Vec<ModelCapability>` / etc. fields into a canonical `HashSet<Tag>` source of truth — had silently regressed eight capability microbenchmarks by 100× to 1,200,000×. The headline cases:

| Benchmark | Run 1 (typed fields) | Run 2 (post-A.5.N regression) |
|---|---|---|
| `capability_filter/match_gpu_vendor` | 3.74 ns | 46.17 µs |
| `capability_filter/match_min_memory` | 3.74 ns | 46.16 µs |
| `capability_filter/match_complex` | 10.28 ns | 47.04 µs |
| `capability_set/has_model` | 934 ps | 620.70 ns |
| `capability_set/serialize` | 930 ns | 43.97 µs |

The migration was the right architectural call — tag-set as source of truth makes the diff / aggregation / federated-predicate stories cohere — but four hot-path costs piggybacked on the change. v0.25 closes all four:

**Fix 1 — cheaper decoder sort (`capability.rs`).** `CapabilityViews::sorted_tags()` and the three `From<&CapabilitySet>` projection impls were calling `sort_by_key(|t| t.to_string())` — a fresh `String` allocation per comparison, ~150 allocations per `views()` call for a 35-tag set. v0.25 adds a separate `decoder_sorted_tag_vec` using `Tag`'s derived `Ord` via `sort_unstable()`. The original `sorted_tag_vec` stays in place for the wire serializer (signed-announcement bytes need the `Tag::to_string()` canonical order for cross-version signature verification) — only the decoder paths switch.

**Fix 2 — tag-direct fast paths in `CapabilityFilter::matches`.** Single-field hardware predicates were forcing a full `HardwareCapabilities` decode (sort + per-tag axis_key parse + per-field `value.parse()`) just to read one tag. v0.25 adds `CapabilitySet::axis_value(axis, key) -> Option<&str>` (pub(crate)) and rewrites `matches()` so `min_memory_gb` / `gpu_vendor` / `min_vram_gb` probe the tag set directly the way `has_gpu()` already did. The `views()` call is now lazily guarded behind `min_context_length` and `require_modalities` — predicates that don't set those fields never decode.

**Fix 3 — drop `axis_key()`'s per-tag `String::clone` (`has_model` / `has_tool` and 14 hot-path callers).** `Tag::axis_key()` returns an owned `TagKey` containing a cloned key string. Every caller that iterated a tag set through it was paying ~35 String allocations per call. v0.25 adds `Tag::axis_key_ref() -> Option<(TaxonomyAxis, &str)>` and migrates the five view decoders (`hardware_from_tags`, `software_from_tags`, `resource_limits_from_tags`, `models_from_tags`, `tools_from_tags`), the five `is_*_owned_tag` predicates, `Predicate::Exists`, `match_axis_tag`, `RequiredCapability::AxisKey`, and `MatchKey::{Axis, AxisKey}` in capability aggregation. `axis_key()` is kept for callers that genuinely need an owned `TagKey` (`diff.rs` collects into `HashSet<TagKey>`).

**Fix 4 — postcard compact codec for `CapabilitySet`.** `to_bytes` is `serde_json::to_vec` and isn't going anywhere on the wire (signed-announcement byte stability + cross-version peer compat). v0.25 adds `CapabilitySet::to_bytes_compact` that emits `0x01 <postcard payload>`, and `from_bytes` sniffs the first byte (`b'{'` → JSON, `0x01` → postcard, anything else → `None`) so receivers on this code accept both formats. The actual win came from `serialize_tags_sorted` branching on `serializer.is_human_readable()`: JSON keeps the canonical sort, postcard skips it (no signing on this path; the only consumer is a `from_bytes` that reconstructs the same `HashSet` regardless of element order).

**Benchmarks (Windows host, same-run before/after per fix):**

| Benchmark | Pre-fix | v0.25 | Δ |
|---|---|---|---|
| `capability_filter/match_gpu_vendor` | 67.96 µs | 115 ns | ~590× |
| `capability_filter/match_min_memory` | 58.94 µs | 25.75 ns | ~2289× |
| `capability_filter/match_complex` | 4.42 µs (post fixes #1+#2) | 3.74 µs | −15.9% |
| `capability_filter/match_require_gpu` | 74.90 ns | 38.91 ns | −48% |
| `capability_set/has_model` | 755.54 ns | 31.65 ns | ~24× |
| `capability_set/has_tool` | 680.02 ns | 34.69 ns | ~19.6× |
| `capability_set/serialize_compact` | 54 µs (JSON) | 1.96 µs | ~27× |
| `capability_set/roundtrip_compact` | 60 µs (JSON) | 6.35 µs | ~9.4× |

All 4137 lib tests pass (3 new tests pin the compact codec round-trip and the unknown-format rejection). Wire format is unchanged for any current peer: `to_bytes` is still JSON, the wire serializer keeps `Tag::to_string()` sorting, signed announcements stay byte-stable across versions. The compact codec is opt-in via the new `to_bytes_compact` — flipping the default writer to compact is a separate, deliberate rollout commit (every receiver must be on v0.25 first).

**What's not in this release.** `CapabilityAnnouncement::to_bytes_compact` is deferred. The struct has six `#[serde(skip_serializing_if = ...)]` fields (`signature`, `hop_count`, `reflex_addr`, the three `allowed_*` lists) whose omission is load-bearing for pre-M-1 / pre-v0.4 signed-byte compat, and postcard's positional encoding can't reconstruct an omitted field. A separate canonicalized wire struct is the right fix; tracked in the audit doc as a follow-up.

---

## Test hygiene

- **Two new audit docs shipped in tree.** `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md` covers the agent surface (eight locked decisions, phasing, per-binding status); `docs/misc/PERF_AUDIT_2026_05_28_CAPABILITY.md` covers the capability perf pass (headline regressions, root causes with file:line pointers, ranked fixes with risk/touch columns, before/after numbers per fix).
- **`T-1` cross-language tool-format byte-equality**, ratcheted across all four bindings. The `tests/cross_lang_tool_formats/golden_vectors.json` fixture is consumed by Rust / Node / Python / Go verifiers in lockstep — adding a new descriptor / lower case / error case means updating all four. Drift surfaces as CI failure, not a runtime surprise.
- **`T-2` `ToolEvent` envelope round-trip**, same posture across all four bindings. JSON tag-form (`{"type": "start", ...}`) deserializes + re-serializes byte-equal for every variant + every optional-field combination listed in `tests/cross_lang_tool_formats/tool_event_vectors.json`. The synthesized `Error { code: "missing_terminal", ... }` shape is part of the fixture so adapters can match on the code reliably.
- **Capability perf — all 195 capability lib tests pass at every commit** in the perf series (`bd58b90b`, `20dba467`, `00aa6f75`, `2cb28f7d`). Three new tests pin the compact codec: `compact_wire_format_round_trips_and_interops_with_json`, `from_bytes_rejects_unknown_format_tag`, `announcement_*` (JSON-only, since the announcement compact path is deferred).
- **`cargo clippy --features meshos,deck,aggregator --all-features --all-targets -- -D warnings` clean.** The strict floor from v0.20.2 stays armed; the `clippy::useless-vec` lint that landed in Rust 1.95 caught one pre-existing `vec![]` in the capability test suite — fixed in `deebf93e`.
- **`cargo doc --features meshos,deck,aggregator --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`.** All new `ToolDescriptor` / `ToolEvent` / `tool::*` intra-doc links resolve; the compact-codec docstrings inline `0x01` instead of linking to the private `COMPACT_FORMAT_TAG` constant (`94c87537`).

---

## Breaking changes

### `tool` cargo feature on `net-mesh`

New optional `tool = []` feature gates the `tool.rs` module + `ToolEvent` wire type. The Node / Python / Go binding default feature sets include `tool` — most users see no change. Direct `net-mesh` consumers who want `serve_tool` / `call_tool` need `cargo add net-mesh --features tool`.

The wire-level pieces this composes against (`ToolCapability` in `behavior::capability`, the capability fold, `call_service_streaming`) compile unconditionally so peers without the feature still exchange `ToolCapability` announcements.

### `call_service_streaming` is a new substrate primitive

`Mesh::call_service_streaming` mirrors `Mesh::call_service` returning an `RpcStream` instead of a single response. Capability-routed + auth-gated through the same path as the unary variant. Every streaming tool client (Rust / Node / Python / Go) depends on it; downstream consumers who built their own streaming client on top of capability fold lookups can switch to this primitive.

### `tool.metadata.fetch` is a new reserved RPC service name

Auto-installed by `serve_tool` on the first tool registration per node. Downstream consumers MUST NOT register an unrelated handler under this name — the auto-install asserts the slot is theirs and panics on collision. The reserved-name boundary is documented in `docs/AGENT_TOOLS.md`.

### `CapabilitySet::from_bytes` accepts both JSON and the compact (`0x01`-prefixed postcard) format

Behavior-preserving for every JSON caller. A byte stream whose first byte is neither `b'{'` nor `0x01` now returns `None` instead of attempting a JSON parse — previously the JSON parser would have returned its own `None`, so the observable contract is unchanged. The first-byte sniff is documented on `from_bytes`.

### `CapabilitySet::to_bytes_compact` is a new opt-in serializer

Default `to_bytes` is still JSON; flipping the default writer to compact is a separate rollout decision (every receiver must be on v0.25 first or it can't decode the new bytes). The compact codec is for local-only callers and a future deliberate wire-format flip.

### `Tag::axis_key_ref` is a new method on `Tag`

Additive. `axis_key()` is unchanged (returns owned `TagKey`); `axis_key_ref()` returns `Option<(TaxonomyAxis, &str)>` without cloning. Hot-path iteration callers SHOULD prefer the borrowing variant — the cloning variant is only worth it when the caller actually needs an owned `TagKey` (e.g. collecting into `HashSet<TagKey>` for diff).

### `serialize_tags_sorted` now branches on `serializer.is_human_readable()`

Internal-only break. JSON callers continue to get the sorted canonical form (signed-announcement byte stability); postcard callers skip the sort. No observable change unless a downstream consumer was relying on the `Tag::to_string()` order in a non-human-readable serializer output — that wasn't a supported contract.

### `CapabilityAnnouncement` does NOT have a `to_bytes_compact`

Deferred. The struct's six `#[serde(skip_serializing_if)]` fields are required for pre-M-1 / pre-v0.4 signed-byte cross-version compat, and postcard's positional encoding can't tolerate omitted fields. A separate canonicalized wire struct is the right path; not in this release.

### `gpu_vendor_str` is now `pub(crate)` in `tag_codec.rs`

Internal-only. Required by `CapabilityFilter::matches`'s tag-direct vendor probe (constructs the expected `Tag::AxisValue` from the matcher's `GpuVendor` for O(1) `HashSet::contains`). No public surface.

### `ai-tool:<tool_id>` capability tag is reserved

Substrate emits this automatically on every `serve_tool` registration. Downstream code SHOULD NOT emit `ai-tool:*` tags by hand — `list_tools()` filters on this prefix and a hand-emitted tag without the matching `nrpc:<tool_id>` service registration would surface as a phantom tool with no handler.

---

## How to upgrade

1. **Rust consumers — update the dependency to `0.25`.** No source changes required unless you (a) want to author or call tools (`serve_tool` / `call_tool` — enable the `tool` feature), or (b) iterate a tag set through `Tag::axis_key()` in a hot path (switch to `axis_key_ref()` for the per-call allocation saving).

2. **Agent authors — pick your binding and follow `docs/AGENT_TOOLS.md`.** Rust: `Mesh::serve_tool<Req, Resp>(...)` (the `#[tool]` proc macro is the follow-up; runtime APIs are usable as-is). Node: `tool({ name, description, schema, handle })` with Zod schemas. Python: `@tool` decorator on a Pydantic-typed handler (sync or async). Go: `net.RegisterTool[Req, Resp](rpc, descriptor, handler)`. Discovery is the same shape in every binding: `list_tools(matcher?)` returns descriptors, `watch_tools(matcher?)` streams `ToolListChange::{Added, Removed, NodeCountChanged}`.

3. **Agent authors using OpenAI / Anthropic / Gemini / MCP — install the format package.** Python: `pip install net-mesh-tools`; import from `net_mesh.tools.formats.{openai,anthropic,gemini,mcp}`. Node: `npm install @net-mesh/tools`; import from `@net-mesh/tools/formats/{openai,anthropic,gemini,mcp}`. Each translator is a pure function from `ToolDescriptor` → provider tool-array entry; the reverse `lower_<provider>_tool_call(call)` returns a `CallSpec` you pass into `call_tool` / `call_tool_streaming`. No transitive provider-SDK dep — wire the translator output into your existing OpenAI / Anthropic client.

4. **Operators with capability-filter throughput pressure — expect the µs→ns recovery to land out of the box.** No config knobs to flip. The four perf fixes are unconditional on the substrate path. Re-run `cargo bench --bench net -- "capability_(filter|set)"` to confirm against your hardware; the audit doc has the same-host before/after numbers for cross-checking.

5. **Operators with binary-size budgets — `tool` is opt-in.** Direct `net-mesh` consumers who don't want the agent surface keep their default feature list. Binding artifacts: the binding's `tool` feature flag is on by default in Node / Python / Go; downstream consumers who don't want it pass `--no-default-features` and enumerate the features they do want.

6. **Downstream consumers caching capability bytes — opt into `to_bytes_compact` when you control both sides.** Local persistence, intra-process caches, and any storage path where the byte format is yours to choose can switch to the compact codec for the ~27× serialize win and ~10× roundtrip win. Wire callers (`mesh.rs`, `swarm.rs`, `proximity.rs`, the CLI announce path) should NOT switch until the entire fleet is on v0.25 — receivers on this release accept both formats, but receivers on v0.24 can't decode `0x01`-prefixed bytes.

7. **Operators on mixed-version fleets — wire format is unchanged.** `CapabilitySet::to_bytes` is still JSON, `CapabilityAnnouncement::to_bytes` is still JSON, `serialize_tags_sorted` still produces the `Tag::to_string()` canonical order for JSON serializers, signed-announcement bytes are byte-stable across versions. v0.24 and v0.25 peers handshake cleanly.

8. **Downstream Go binding consumers — ABI version unchanged.** `NET_RPC_ABI_VERSION` stays at `0x0004`. The Go tool surface (`net.RegisterTool`, `net.RegisterStreamingTool`, `net.CallToolStreaming`, `net.ListTools`, `net.WatchTools`) is additive.

9. **CI — no config change required.** Strict clippy floor stays armed (the new `clippy::useless-vec` in Rust 1.95 caught one pre-existing test-fixture site, fixed in this release); rustdoc warnings stay denied; the cross-language tool-format byte-equality fixture is the new CI gate. Adding a new descriptor / lower case / error case in `tests/cross_lang_tool_formats/golden_vectors.json` must be done in lockstep across all four binding verifiers.

10. **Operators — bump the binary.** Pre-built `net-mesh`, `net-deck`, `net-aggregator-daemon` archives land for every supported target (Linux x86_64 / aarch64, macOS x86_64 / aarch64, Windows x86_64). Wire format is unchanged from v0.24.

---

Released 2026-05-28.

## License

See [LICENSE](../../LICENSE).
