# Implementation Plan: `find_best_node` — core witness + TS/Python parity

**Implements:** the last capability-discovery surface gap in the binding matrix
(`docs/data/capabilities/event-bus.yaml`, "Capability discovery"):
`find_best_node` / `find_best_node_scoped` exist in Rust
(`net/crates/net/sdk/src/mesh.rs:1161`), Go (`go/capabilities.go` —
`FindBestNode` / `FindBestNodeScoped`) and C (`net_mesh_find_best_node` /
`net_mesh_find_best_node_scoped`, `net/crates/net/src/ffi/mesh.rs:3761`), but
neither the NAPI binding (`net/crates/net/bindings/node`) nor the PyO3 binding
(`net/crates/net/bindings/python`) exposes them.

**The sentence:** first prove that the canonical tag-backed capability fold still
makes the four placement weights load-bearing, then give Node/TS and Python the
same single-winner local-discovery call as Rust, Go and C. The bindings only
marshal a requirement and project `Option<u64>`; filtering, scoring,
tie-breaking and scope evaluation remain core behavior.

---

## Ground truth (surveyed 2026-08-06)

| Surface | Rust | Go | C | Node / TS | Python |
|---|---|---|---|---|---|
| `find_nodes` (+ scoped) | ✅ | ✅ | ✅ | ✅ `findNodes` / `findNodesScoped` | ✅ `find_nodes` / `find_nodes_scoped` on `NetMesh`; only unscoped on `AsyncNetMesh` |
| `find_best_node` (+ scoped) | ✅ | ✅ | ✅ | ❌ | ❌ |

Structural facts that shape the work:

1. **Neither binding goes through the C FFI.** Both call the core `MeshNode`
   directly. NAPI's `find_nodes` calls `node.find_nodes_by_filter(&core)`
   (`bindings/node/src/lib.rs:2125`), and PyO3's does the same
   (`bindings/python/src/lib.rs:2444`). No C ABI or header change belongs in
   this plan.
2. **The fold is tag-backed, and weighted scoring is intended to remain
   functional.** `capability_bridge::synthesize_capability_set` merges each
   candidate's canonical tags into a `CapabilitySet`
   (`behavior/fold/capability_bridge.rs:687-748`).
   `CapabilityRequirement::score` reads `CapabilitySet::views()`, whose lazy
   hardware/model projections decode those canonical tags through
   `hardware_from_tags` and `models_from_tags`
   (`behavior/capability.rs:1975-2047`). Memory, VRAM, tokens/sec and loaded
   model facts therefore do not require a second rich-state sidecar.
3. **The current core comments, evidence and snapshot shape are
   stale/inadequate.**
   `MeshNode::find_best_node`, `find_best_node_scoped` and `best_by_score`
   (`adapter/net/mesh.rs:30658-30731`) still claim those decoded values are zero
   and every score ties. That contradicts the canonical `views()` path. The Go
   test uses one candidate and explicitly says its weights are not
   load-bearing (`go/capabilities_test.go:297-303`). In addition, current
   `find_best_node*` first obtains candidate ids from one fold read, then
   `best_by_score` re-enters the fold once per candidate to synthesize scoring
   input. A replacement/removal can therefore mix candidate membership from
   one fold state with scores from later states. Before exposing two more
   bindings, Part 0 must make candidate filtering + scoring one coherent fold
   snapshot, replace the stale comments, and add inverse two-candidate core
   witnesses. If a witness fails, stop: this becomes a core scoring repair,
   not a binding-parity patch.
4. **All binding conversion scaffolding already exists.** NAPI has
   `CapabilityFilterJs` / `capability_filter_from_js` / `ScopeFilterJs` /
   `scope_filter_from_js` / `with_scope_filter`
   (`bindings/node/src/capabilities.rs`). Python has
   `capability_filter_from_py` / `scope_filter_from_py` /
   `with_scope_filter` (`bindings/python/src/capabilities.rs`). The only new
   conversion is the requirement wrapper: filter plus four weights.
5. **The requirement type is the behavior/capability one.**
   `CapabilityRequirement` (`adapter/net/behavior/capability.rs:2962`) carries
   `prefer_more_memory`, `prefer_more_vram`, `prefer_faster_inference` and
   `prefer_loaded_models`, each stored as `f32` and clamped to `[0.0, 1.0]` by
   the builder. The unrelated scheduler-claim type in
   `adapter/net/cortex/workflow/step.rs` is out of scope.
6. **Finite-number behavior must be explicit at dynamic-language boundaries.**
   C receives JSON and cannot represent `NaN` or infinities. JS and Python can.
   NAPI and PyO3 reject non-finite weights before narrowing to `f32`; finite
   values outside `[0.0, 1.0]` pass to the core builder and are clamped there.
7. **No-match needs no out-param trick here.** Go and C need `(uint64, bool)` /
   `*out_has_match` because node id `0` is valid. Node returns
   `bigint | null`; Python returns `int | None`.
8. **Python discovery remains core-only.** The surface lives on
   `net._net.NetMesh` / `AsyncNetMesh`; `net_sdk.MeshNode` does not re-export
   discovery. This plan does not change that coverage mode.

## Locked decisions

1. **No `net_sdk` Python lift.** Keep discovery `core-only`; lifting the whole
   discovery row is separate work.
2. **Complete `AsyncNetMesh` local-discovery parity.** Add all four synchronous
   helpers: `find_nodes`, `find_nodes_scoped`, `find_best_node`, and
   `find_best_node_scoped`. `find_nodes` already exists; this patch adds the
   other three and their stubs.
3. **Reject non-finite weights.** NAPI and PyO3 return boundary type/value
   errors for `NaN`, `+∞`, and `-∞`. Finite out-of-range values retain the
   existing Rust/C/Go clamp contract.
4. **Use real announcements for binding E2E tests.** Existing Python test-only
   injectors accept only an empty capability set or tags; they cannot stage
   candidates with different VRAM. Do not widen production or test-only APIs
   merely for this patch.

## Doctrine

- **One scoring authority.** Bindings do not score, sort, break ties or decode
  capability tags. They call core `MeshNode::find_best_node*`.
- **One semantic shape, idiomatic spellings.** A requirement is a base filter
  plus four optional weights. Python dicts use snake_case; TS uses camelCase.
- **Local index, synchronous call.** Like `find_nodes`, these methods read the
  local fold. They perform no network operation and return no awaitable,
  including on `AsyncNetMesh`.
- **No false parity.** Declaration/import tests are insufficient. A
  two-candidate core inverse test must prove that a weight can change the
  winner before docs call the operation weighted selection.
- **One coherent selection snapshot.** Candidate membership, scope filtering,
  tag projection and scores come from one fold state. Do not select ids, drop
  the fold locks, and re-read each candidate for scoring.

---

## Part 0 — prove the core contract before widening it

### 0.1 — Make filtering and scoring one coherent fold snapshot

- [ ] Add a capability-bridge selection helper that performs candidate
  filtering, optional scope evaluation, candidate tag/metadata projection and
  scoring inside one `Fold::with_state` read. Extract/reuse a state-local
  synthesis helper; do not call `synthesize_capability_set` from inside the
  snapshot because it would re-enter the fold.
- [ ] Preserve the existing announcement-path invariant correctly: a
  publisher currently owns one class-0 entry, but the state-local synthesis
  must still merge every key in `state.by_node[node_id]` so future per-class
  sharding does not make the score depend on whichever entry matched first.
- [ ] Keep `SameSubnet` evaluation fold-pure and based on the selected entry's
  borrowed tags, exactly as `find_nodes_matching_scoped` does today. The
  closure may read captured local policy, but must not read the fold or the
  `peer_subnets` sidecar.
- [ ] Return only the winning node id after the snapshot closes. Preserve
  descending score and ascending node-id tie-break.
- [ ] Rewire both `MeshNode::find_best_node` and
  `MeshNode::find_best_node_scoped` through this helper. Do not compose
  `find_nodes_*` followed by per-candidate fold reads.

### 0.2 — Replace stale scoring comments

- [ ] Update only the comments on `MeshNode::find_best_node`,
  `find_best_node_scoped`, and `best_by_score` in
  `net/crates/net/src/adapter/net/mesh.rs:30658-30731`.
- [ ] State the actual path: candidate membership comes from one canonical
  capability-fold snapshot; state-local synthesis merges canonical tags;
  `CapabilitySet::views()` lazily reconstructs hardware/models; scoring uses
  those decoded projections; ties resolve by ascending node id.
- [ ] Do not add a hardware/model sidecar or restore removed duplicate fields.
  Canonical tags remain the source of truth.

### 0.3 — Add inverse core witnesses

Add focused Rust tests beside the existing mesh/capability-fold discovery tests:

- [ ] Inject two candidates whose node-id ordering is opposite their capacity
  ordering: lower node id has less VRAM, higher node id has more VRAM.
- [ ] With all weights zero, assert the documented ascending-node-id tie-break.
- [ ] With `prefer_vram(1.0)`, assert the higher-VRAM candidate wins.
- [ ] Add equivalent independent witnesses for memory, maximum model
  tokens/sec, and loaded-model ratio. Each witness must have at least two
  matching candidates; no single-candidate scoring smoke test counts.
- [ ] For scoped selection, stage an out-of-scope candidate with the strongest
  score and prove filtering occurs before scoring.
- [ ] Add a mutation/replacement witness around the new bridge helper proving
  a selection cannot retain membership from one fold generation while scoring
  replacement or missing data from another. Prefer a deterministic helper-
  level test over a timing-dependent concurrent test.
- [ ] Prove no match returns `None` and node id `0` remains a valid winner.
- [ ] Where practical, mutate only the preference or candidate fact between
  paired assertions so a dead weight makes the inverse test fail.

**Stop gate:** if any weighted witness shows lexicographic selection instead of
capacity/model selection, do not proceed to Parts A-D. Repair the canonical
fold-to-views scoring path first and rerun all Part 0 witnesses.

---

## Part A — NAPI binding (`net/crates/net/bindings/node`)

### A1 — Requirement DTO and conversion

- [ ] Add `CapabilityRequirementJs` in
  `bindings/node/src/capabilities.rs` as `#[napi(object)]`:
  `filter: CapabilityFilterJs` plus four `Option<f64>` fields
  (`prefer_more_memory`, `prefer_more_vram`,
  `prefer_faster_inference`, `prefer_loaded_models`; napi-rs emits camelCase).
- [ ] Add `capability_requirement_from_js` returning
  `Result<CapabilityRequirement>`:
  1. reject any supplied non-finite `f64` with `InvalidArg`;
  2. narrow finite values to `f32`;
  3. build through `CapabilityRequirement::from_filter(...)` and the four core
     preference builders;
  4. default absent weights to `0.0`.
- [ ] Add Rust unit tests for defaults, exact finite passthrough, finite clamp
  behavior, and rejection of `NaN` / infinities.

### A2 — Native methods

- [ ] Add `find_best_node(&self, req: CapabilityRequirementJs) ->
  Result<Option<BigInt>>` and `find_best_node_scoped(&self, req,
  scope: ScopeFilterJs) -> Result<Option<BigInt>>` beside `find_nodes` /
  `find_nodes_scoped` in `bindings/node/src/lib.rs`.
- [ ] Use the same `load_node` guard and the existing scope borrow trampoline.
  Convert `Some(id)` with `BigInt::from(id)`; preserve `None` as `null`.
- [ ] Doc comments must say local/synchronous, `null` means no match, `0n` is a
  valid node id, weights are finite numbers clamped to `[0, 1]`, and scope is
  applied before scoring.

---

## Part B — TypeScript SDK (`net/crates/net/sdk-ts`)

### B1 — Ergonomic DTO and wrapper

- [ ] Add exported `CapabilityRequirement` and internal
  `capabilityRequirementToNapi` in `sdk-ts/src/capabilities.ts` next to
  `CapabilityFilter` / `capabilityFilterToNapi`:

  ```ts
  export interface CapabilityRequirement {
    filter: CapabilityFilter;
    preferMoreMemory?: number;
    preferMoreVram?: number;
    preferFasterInference?: number;
    preferLoadedModels?: number;
  }
  ```

- [ ] Add `findBestNode(req): bigint | null` and
  `findBestNodeScoped(req, scope): bigint | null` on `MeshNode`, delegating to
  the native methods after DTO conversion.
- [ ] Document winner/no-match semantics, the valid `0n` result, finite-number
  requirement, Rust-side finite clamp, local/synchronous behavior, and scoped
  filtering before scoring.

### B2 — Tests

- [ ] Conversion tests cover all fields and absence/default behavior.
- [ ] Boundary tests prove `NaN` and infinities are rejected through the native
  call; finite negative and >1 values are accepted and clamped.
- [ ] E2E: create real connected nodes, announce two matching capability sets
  with node-id ordering recorded at runtime, then choose capacity values so the
  high-VRAM winner is not merely the ascending-id tie winner. Wait until the
  querying node sees both announcements before asserting.
- [ ] Add scoped narrowing and no-match → `null` assertions.
- [ ] Do not duplicate all four core scoring-axis witnesses in TS; Part 0 owns
  scoring semantics. TS proves DTO/native/wrapper continuity with VRAM.

---

## Part C — PyO3 binding (`net/crates/net/bindings/python`)

### C1 — Requirement conversion

- [ ] Add a float helper and
  `capability_requirement_from_py(&Bound<'_, PyDict>)` in
  `bindings/python/src/capabilities.rs` accepting:

  ```python
  {
      "filter": {...},
      "prefer_more_memory": 0.5,
      "prefer_more_vram": 1.0,
      "prefer_faster_inference": 0.0,
      "prefer_loaded_models": 0.0,
  }
  ```

- [ ] Default a missing `filter` to an empty filter and missing weights to
  `0.0`, matching the C JSON DTO.
- [ ] Reject non-numeric values with `TypeError`; reject non-finite numeric
  values with `ValueError`; narrow finite values to `f32` and route through the
  core builders so finite out-of-range values clamp centrally.
- [ ] Add Rust unit tests for defaults, passthrough, clamp, wrong types and
  non-finite values.

### C2 — `NetMesh`, `AsyncNetMesh`, and stubs

- [ ] Add synchronous `find_best_node(requirement) -> Option<u64>` and
  `find_best_node_scoped(requirement, scope) -> Option<u64>` on `NetMesh`
  beside the existing discovery methods.
- [ ] Add synchronous `find_nodes_scoped`, `find_best_node`, and
  `find_best_node_scoped` on `AsyncNetMesh`, using the same converters and
  scope borrow trampoline as `NetMesh`.
- [ ] Update both class sections in
  `bindings/python/python/net/_net.pyi`. Include the pre-existing missing
  `NetMesh.find_nodes_scoped` stub and the three new `AsyncNetMesh` methods.
  Use `dict` / `Dict[str, Any]` consistently with each surrounding class and
  `int | None` / `Optional[int]` consistently with that section's style.

### C3 — Python tests

- [ ] Use real `NetMesh` peers and `announce_capabilities` to stage candidates
  with different VRAM. The helpers around `lib.rs:2393-2437` are unsuitable:
  they inject empty or tag-only sets and do not accept arbitrary capability
  dicts.
- [ ] Wait for both announcements before asserting a VRAM-weighted winner whose
  node-id order would otherwise choose the other candidate.
- [ ] Add scoped narrowing, no-match → `None`, and node-id-0 disambiguation
  where an existing safe test injector can represent it without weakening the
  production surface.
- [ ] Add wrong-type, `NaN`, and infinity refusal tests plus finite clamp tests.
- [ ] Exercise the new synchronous methods on `AsyncNetMesh`; do not `await`
  them.

---

## Part D — docs, coverage records, and generated skills

Sequence: land and verify Parts 0 and A-C first. Every claim below must anchor
to a symbol that exists, and weighted-selection prose is allowed only while the
Part 0 inverse witnesses stay green.

- [ ] `docs/data/capabilities/event-bus.yaml`, "Capability discovery": change
  the Node/TS anchor from `findNodes` to `findBestNode` and the Python anchor
  from `find_nodes` to `find_best_node`; statuses remain `supported`, Python
  remains `core-only`. Regenerate the generated skill matrix with
  `.github/scripts/capability_records.py`.
- [ ] `.claude/skills/net-event-bus/bindings/coverage.md`: remove the stale
  one-node-vs-list asymmetry; all bindings now expose both list and
  single-winner discovery with idiomatic names.
- [ ] `.claude/skills/net-event-bus/capabilities.md`: replace the Node and
  Python absence bullets with exact signatures and retain the local-index /
  finite-weight contract.
- [ ] `docs/data/spine-symbols.yaml` `discover` lens: add
  `findBestNode` / `findBestNodeScoped` to TypeScript and
  `find_best_node` / `find_best_node_scoped` to Python.
- [ ] Update `web/src/content/docs/sdk/discover/rust.md` together with the new
  TypeScript and Python best-node paragraphs. The Rust page currently says
  “highest-scoring” without describing the canonical tag-backed projection;
  make all three pages agree on local index, finite clamped weights, tie-break,
  scope-before-score, and no-match behavior.
- [ ] Sweep for stale absence and stale “weights always tie” claims:

  ```bash
  git grep -n -i -E 'no .*findBestNode|no .*find_best_node|find_best_node.*(zero|tie|lex)|weights.*not load-bearing' -- docs web .claude net go
  ```

---

## Verification

### Core gate

- [ ] Run the focused Part 0 Rust tests and record the exact test names/results.
- [ ] Run the containing capability-fold / mesh discovery test target.

### Node / TypeScript

From `net/crates/net/bindings/node`:

```bash
npm run build
npm run typecheck
npm run typecheck:tests
npm test
```

- [ ] Confirm regenerated `index.d.ts` contains both native methods and the
  expected `CapabilityRequirementJs` shape.

From `net/crates/net/sdk-ts`:

```bash
npm run build
npm test
```

### Python

From `net/crates/net/bindings/python`, inside the repository's Python virtual
environment:

```bash
cargo test
maturin develop
pytest tests/test_capabilities.py tests/test_stub_drift.py tests/test_pyi_stub_coverage.py
```

`maturin build` alone is not sufficient for pytest because it creates a wheel
without installing the rebuilt extension.

### Repository synchronization

```bash
.github/scripts/capability_records.py --check
.github/scripts/check-skills.sh
git diff --check
git status --short
```

- [ ] Confirm only intended implementation, tests, generated records, docs and
  this plan changed.
- [ ] Confirm package declarations/stubs are synchronized with the runtime
  surfaces.
- [ ] Confirm exact-head CI is green before merge.

## Non-goals

- No change to the C ABI, C headers or Go API.
- No second rich capability-state sidecar; canonical fold tags and
  `CapabilitySet::views()` remain the projection path.
- No multi-hop or remote-query semantics; discovery reads the local fold.
- No Python `net_sdk` wrapper re-export.
- No new scoring dimensions or normalization constants beyond proving and
  preserving the four existing core weights.
