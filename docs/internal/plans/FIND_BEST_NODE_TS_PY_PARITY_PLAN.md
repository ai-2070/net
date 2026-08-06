# Implementation Plan: `find_best_node` — TS + Python binding parity

**Implements:** the last capability-discovery parity gap in the binding
matrix (`docs/data/capabilities/event-bus.yaml`, "Capability discovery"):
`find_best_node` / `find_best_node_scoped` exist in Rust
(`net/crates/net/sdk/src/mesh.rs:1161`), Go (`go/capabilities.go` —
`FindBestNode` / `FindBestNodeScoped`) and C (`net_mesh_find_best_node` /
`net_mesh_find_best_node_scoped`, `net/crates/net/src/ffi/mesh.rs:3761`),
but neither the NAPI binding (`net/crates/net/bindings/node`) nor the PyO3
binding (`net/crates/net/bindings/python`) exposes them. The coverage doc
records the gap explicitly ("there is no `findBestNode`" —
`.claude/skills/net-event-bus/bindings/coverage.md` § Same operation,
different shape).

**The sentence:** Node and Python gain the single-winner discovery call the
other three bindings already have — a marshaling layer over the core's
`MeshNode::find_best_node(&CapabilityRequirement)` scoring, deciding
nothing itself — so a TS or Python placement node no longer has to
re-implement the selection policy caller-side or drop to Rust / Go.

---

## Ground truth (as surveyed 2026-08-06)

| Surface | Rust | Go | C | Node / TS | Python |
|---|---|---|---|---|---|
| `find_nodes` (+ scoped) | ✅ | ✅ | ✅ | ✅ `findNodes` / `findNodesScoped` | ✅ `find_nodes` / `find_nodes_scoped` |
| `find_best_node` (+ scoped) | ✅ | ✅ | ✅ | ❌ | ❌ |

Structural facts that shape the work:

1. **Neither binding goes through the C FFI.** Both call the core
   `MeshNode` directly — NAPI's `find_nodes` calls
   `node.find_nodes_by_filter(&core)`
   (`bindings/node/src/lib.rs:2125`), PyO3's calls the same
   (`bindings/python/src/lib.rs:2444`). `find_best_node` is one more
   method on the same object; no FFI or header work exists.
2. **All conversion scaffolding already exists.** NAPI has
   `CapabilityFilterJs` / `capability_filter_from_js` /
   `ScopeFilterJs` / `scope_filter_from_js` / `with_scope_filter`
   (`bindings/node/src/capabilities.rs`); Python has
   `capability_filter_from_py` / `scope_filter_from_py` /
   `with_scope_filter` (`bindings/python/src/capabilities.rs`). The only
   new conversion is the requirement wrapper: filter + four weight
   fields.
3. **The weight semantics are decided in core.**
   `CapabilityRequirement` (`net/crates/net/src/adapter/net/behavior/capability.rs:2962`)
   carries `prefer_more_memory` / `prefer_more_vram` /
   `prefer_faster_inference` / `prefer_loaded_models`, each clamped to
   `[0.0, 1.0]` by the builder (`from_filter(..).prefer_memory(w)…`).
   Bindings pass weights through and let the clamp happen Rust-side —
   exactly what the C FFI does (`ffi/mesh.rs:3739-3749`) and what Go
   documents ("values outside the range are silently capped").
   *Disambiguation:* there is a second, unrelated `CapabilityRequirement`
   in `adapter/net/cortex/workflow/step.rs:44` (the scheduler claim
   seam). Do not touch it; everything here is the behavior/capability
   one.
4. **"No match" vs node id 0 needs no out-param trick here.** Go and C
   needed `(uint64, bool)` / `*out_has_match` because 0 is a valid node
   id. Node returns `bigint | null` and Python `int | None` — the
   language-native `Option` projection of Rust's `Option<u64>`.
5. **Python discovery is core-only by design** (coverage matrix mode
   `core-only`): the surface lives on `net._net.NetMesh`, and
   `net_sdk.MeshNode` does not re-export it. This plan keeps that
   placement; lifting discovery into `net_sdk` is a separate decision
   (Open Decision 1).

## Doctrine (unchanged)

- **No logic in bindings.** Scoring, tie-breaking, and weight clamping
  are core behavior. Bindings marshal a requirement in and a
  `Option<u64>` out.
- **One wire shape, three spellings.** The requirement is the same
  object everywhere: a base filter plus four optional weights. Python
  dicts use the snake_case keys the C/Go JSON contract already fixed
  (`prefer_more_memory`, …); the TS POJO uses the camelCase
  (`preferMoreMemory`, …) that napi-rs derives, same as the existing
  filter/scope shapes.
- **Local index, sync call.** Like `find_nodes`, this reads the local
  capability fold — no network, no `await`. It stays sync in both
  bindings (including on `AsyncNetMesh`, whose sync helpers are already
  documented as staying sync).

## The parity target (what this plan closes)

| Call | Rust | Go | C | Node / TS → | Python → |
|---|---|---|---|---|---|
| `find_best_node(req)` | ✅ | ✅ | ✅ | **A1/B1** | **C1** |
| `find_best_node_scoped(req, scope)` | ✅ | ✅ | ✅ | **A1/B1** | **C1** |
| Docs / coverage record truthful again | — | — | — | **D** | **D** |

---

## Part A — NAPI binding (`net/crates/net/bindings/node`)

### A1 — Requirement type + the two methods

- [ ] `CapabilityRequirementJs` in `bindings/node/src/capabilities.rs`:
  `#[napi(object)]` with `filter: CapabilityFilterJs` and four
  `Option<f64>` weights (`prefer_more_memory`, `prefer_more_vram`,
  `prefer_faster_inference`, `prefer_loaded_models` — napi renders them
  camelCase). Plus `capability_requirement_from_js` building
  `CapabilityRequirement::from_filter(capability_filter_from_js(f))
  .prefer_memory(..).prefer_vram(..).prefer_speed(..).prefer_loaded(..)`
  with missing weights defaulting to `0.0` — mirroring
  `capability_requirement_from_json` in `ffi/mesh.rs`.
- [ ] `find_best_node(&self, req: CapabilityRequirementJs) ->
  Result<Option<BigInt>>` and `find_best_node_scoped(&self, req, scope:
  ScopeFilterJs) -> Result<Option<BigInt>>` as `#[napi]` methods next to
  `find_nodes` / `find_nodes_scoped` (`bindings/node/src/lib.rs:2125` /
  `:2145`), calling `node.find_best_node(&core)` /
  `with_scope_filter(&owned, |f| node.find_best_node_scoped(&core, f))`.
  Same `load_node` guard, same doc-comment style (state the scoped
  scope-tag semantics the `find_nodes_scoped` comment states).
- [ ] Rust unit test for the conversion (weight passthrough, absent
  weights → 0.0), following the existing in-crate test style
  (cf. the `saturating_u16` regression test in the same crate).

## Part B — TS SDK (`net/crates/net/sdk-ts`)

### B1 — Ergonomic surface

- [ ] `CapabilityRequirement` interface +
  `capabilityRequirementToNapi` in `sdk-ts/src/capabilities.ts`, next to
  `CapabilityFilter` / `capabilityFilterToNapi`: `{ filter:
  CapabilityFilter; preferMoreMemory?: number; preferMoreVram?: number;
  preferFasterInference?: number; preferLoadedModels?: number }`. Doc
  comment states the `[0, 1]` range and the Rust-side clamp.
- [ ] `findBestNode(req: CapabilityRequirement): bigint | null` and
  `findBestNodeScoped(req, scope: ScopeFilter): bigint | null` on
  `MeshNode` (`sdk-ts/src/mesh.ts`, next to `findNodes:702` /
  `findNodesScoped:775`), delegating to the native methods. Docstrings:
  one-winner semantics, `null` = no match (and that a `0n` result is a
  valid node id, not a sentinel), bigint caveat already documented for
  `findNodes` applies.
- [ ] Tests in `sdk-ts/test/capabilities.test.ts` (conversion unit
  tests) plus an e2e in the existing capability e2e style: announce two
  nodes with different VRAM, assert the VRAM-weighted requirement picks
  the bigger one, a scope filter narrows the candidate set, and a
  non-matching filter returns `null`. Follow whatever
  announce-injection path the existing `findNodes` tests use.

## Part C — PyO3 binding (`net/crates/net/bindings/python`)

### C1 — Methods on `NetMesh` (+ `AsyncNetMesh`)

- [ ] `capability_requirement_from_py(&Bound<'_, PyDict>)` in
  `bindings/python/src/capabilities.rs`: accepts `{"filter": {...},
  "prefer_more_memory": 0.5, ...}` — snake_case keys, all optional,
  filter defaulting to empty, weights to `0.0`; routes through the same
  core builder as A1. Reject non-numeric weights with `TypeError`
  (consistent with the crate's existing strictness on dict shapes).
- [ ] `find_best_node(&self, requirement: &Bound<'_, PyDict>) ->
  PyResult<Option<u64>>` and `find_best_node_scoped(requirement, scope)`
  on `NetMesh` next to `find_nodes` (`bindings/python/src/lib.rs:2444`);
  `find_best_node` also on `AsyncNetMesh` next to its `find_nodes`
  (`lib.rs:3488`) — sync, like the other local-index helpers there.
- [ ] Stubs in `bindings/python/python/net/_net.pyi` for the new
  methods (`def find_best_node(self, requirement: dict) -> int | None`).
  **Drive-by:** the stub file has no `find_nodes_scoped` entry even
  though the binding exposes it (only `find_nodes` at `:902` / `:2420`)
  — add it while in the file.
- [ ] Tests in `bindings/python/tests/test_capabilities.py` using
  `test_inject_capability_announcement` (`lib.rs:2435`): weighted pick,
  scoped narrowing, no-match → `None`, and a node-id-0 injection to
  prove `None` vs `0` disambiguation.
- [ ] Rust unit test for `capability_requirement_from_py` (weight
  passthrough, defaults, type errors).

## Part D — Docs, coverage record, skills

Sequencing: land A–C first; every doc claim below must anchor to a
symbol that exists, and `check-skills.sh` proves exactly that.

- [ ] `docs/data/capabilities/event-bus.yaml`, "Capability discovery":
  flip the Node/TS anchor `findNodes` → `findBestNode` and the Python
  anchor `find_nodes` → `find_best_node` so the record's evidence proves
  the *new* symbols (statuses stay `supported`, Python keeps
  `core-only`). Regenerate the skill's matrix copy via
  `capability_records.py` so `check-skills.sh`'s
  `capability_records.py --check` stays green.
- [ ] `.claude/skills/net-event-bus/bindings/coverage.md`: rewrite the
  "Discovery returns one node in Rust and Go, a list in Node and
  Python" paragraph (§ Same operation, different shape) — all five
  bindings now have both shapes; keep the note that the *names* differ
  per binding.
- [ ] `.claude/skills/net-event-bus/capabilities.md`: delete the two
  negative bullets ("No `findBestNode` in the TS SDK or NAPI binding
  today", `:316`; "No `find_best_node` in the PyO3 binding today",
  `:341`) and replace with the new signatures; update the "Both have
  the full surface" framing (`:346`) now that it's all five.
- [ ] `docs/data/spine-symbols.yaml` `discover` lens (`:69-75`): add
  `findBestNode` / `findBestNodeScoped` to the typescript list and
  `find_best_node` / `find_best_node_scoped` to the python list.
- [ ] `web/src/content/docs/sdk/discover/typescript.md` and
  `python.md`: add the best-node paragraph the Rust page already has
  (`rust.md:44-46`), in each page's own idiom.
- [ ] Sweep for stale absence claims:
  `grep -ri "findBestNode\|find_best_node" docs web .claude | grep -i "no \|not \|absent"`.

## Verification

- [ ] `cargo test` in `bindings/node` and `bindings/python` (conversion
  unit tests compile + pass).
- [ ] Node: build + `npm test` in `bindings/node` (regenerates the napi
  typings — confirm `findBestNode` appears); `npm test` in `sdk-ts`.
- [ ] Python: maturin build + pytest in `bindings/python`
  (`test_capabilities.py`).
- [ ] `.github/scripts/check-skills.sh` — symbol list (`:199` already
  names `find_best_node`; satisfied by Rust today, unchanged), coverage
  record check, vocab check against the updated
  `spine-symbols.yaml`.
- [ ] Optional: extend `bindings/node/test/cross_lang_compat.test.ts`
  if it covers discovery shapes — same requirement, same winner across
  bindings.

## Open decisions

1. **Lift discovery into `net_sdk` (Python) / keep TS SDK-only?** Python
   discovery stays `core-only` here (placement parity with `find_nodes`;
   changing the mode is a bigger, separate decision that would touch the
   whole discovery row, cf. `DOCS_SDK_SPINE_PLAN.md`). TS gets the
   ergonomic method because `findNodes` already lives on the SDK
   `MeshNode` there — no mode change either way.
2. **`AsyncNetMesh` scoped variants.** `AsyncNetMesh` today has
   `find_nodes` but not `find_nodes_scoped` — a pre-existing asymmetry.
   This plan adds `find_best_node` there to match its `find_nodes`;
   whether to also add the two scoped variants to `AsyncNetMesh` is
   cheap but widens the diff. Default: add all of them in C1 unless
   review prefers the minimal surface.

## Non-goals

- No change to core scoring, `CapabilityRequirement`, the C ABI, the C
  headers, or the Go module — all already correct.
- No multi-hop or remote discovery semantics — `find_best_node` reads
  the local index, same as `find_nodes`.
- No `net_sdk` (sdk-py) wrapper re-export (Open Decision 1).
