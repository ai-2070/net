# Multifold Phase 3b — CapabilityIndex Cutover Plan

Concrete checklist for the same-PR cutover: rewire every caller of
the legacy `behavior::capability::CapabilityIndex` to
`Fold<behavior::fold::CapabilityFold>`, then delete the legacy
module. Per the stripped multifold plan, this is one atomic PR;
the framework branch (`multifolds`) is the foundation it lands on.

This document scopes the work into discrete sub-steps so the
cutover doesn't discover scope mid-cut. Each sub-step is
independently testable; the FINAL commit deletes the legacy
module + flips any remaining references in one atomic move.

---

## Scope

**Source of truth in scope:**
- `behavior::capability::CapabilityIndex` (~7550 LOC in `capability.rs`)
- 23 caller files across `adapter::net::{behavior, compute, dataforts, subprotocol}` + `mesh.rs`

**Target replacement:**
- `behavior::fold::Fold<behavior::fold::CapabilityFold>` (lands on the `multifolds` branch as Phase 3a)
- The framework's `FoldRegistry` + `set_fold_router` plumbing (Phase 2B)
- Publisher path via `MeshNode::publish_fold_broadcast` (Phase 2C)

**NOT in scope** (separate phases):
- `RoutingTable` deletion → Phase 4b
- Operator CLI / Deck panel → Phase 6b
- Pingwave repurpose → Phase 4b

---

## Public API surface map

Every public method on `CapabilityIndex` needs a fold equivalent
or a documented decision to drop it.

| Legacy method | Fold equivalent | Notes |
|---|---|---|
| `index(ann: CapabilityAnnouncement)` | `fold.apply(SignedAnnouncement<CapabilityMembership>)` | Wire-shape translation: `CapabilityAnnouncement` → `CapabilityMembership`. The `version` field on the legacy ann maps to `generation` on the wire envelope. Tags translate via `Tag::to_string()`. |
| `query(filter: &CapabilityFilter) -> Vec<u64>` | `fold.query(CapabilityQuery::Composite(...))` | Returned `(class, node_id)` pairs project to `node_id` only. Filter struct shapes diverge — see "Filter translation" below. |
| `find_nodes_scoped(filter, scope_filter, same_subnet_lookup) -> Vec<u64>` | **Bridge required** | Folds don't have native scope/subnet semantics. The fold's query returns the candidate set; the bridge layer applies `scope_filter` + `same_subnet_lookup` over it. **Risk:** behavioral subtlety in the existing scope rules — need a per-rule semantic translation test. |
| `find_best_node_scoped(req, scope_filter, same_subnet_lookup) -> Option<u64>` | **Bridge required** | Same as above + the scoring layer. Scoring lives in `behavior::placement` today; the fold's query returns candidates and the placement scorer runs on the result. |
| `get(node_id: u64) -> Option<CapabilitySet>` | `fold.with_state(\|s\| s.entries.iter().find(...))` | Slow path (no index by node_id alone; the fold key is `(class, node_id)`). For per-node lookups, walk all classes and aggregate — acceptable because `get` is a control-plane probe, not a hot path. |
| `get_by_origin_hash(origin_hash) -> Option<CapabilitySet>` | **Bridge or new index** | Existing `by_origin_hash` reverse map has no fold-side equivalent. Two options: (a) add `by_origin_hash` to `CapabilityIndexInner` as a new inverted index, (b) thread origin_hash → node_id via the existing `behavior::peers` map. **Recommend (a)** — purely additive, no cross-module coupling. |
| `axis_cardinality(key) -> usize` | **Out of scope for cutover** | Used by `behavior::query` and a few cap tests. Migrate as a follow-up; the legacy method can survive on a `CapabilityIndexShim` wrapper if no caller breaks. |
| `collision_count() -> u64` | **Drop or stub** | Tracks wire-hash collisions on the legacy index's u32 hash. Folds don't have this concern at the framework level. Decision: drop the metric in Phase 3b; if operator dashboards depend on it, add a stub returning 0 with a deprecation warning. |
| `all_nodes() -> Vec<u64>` (private?) | `fold.with_state(...)` | Internal helper used by `collect_coverage` etc. Translate via the same `with_state` walk pattern. |

### Filter translation

Legacy `CapabilityFilter` fields and their `fold::CapabilityFilter` equivalents:

| Legacy field | Fold field | Notes |
|---|---|---|
| `required_capabilities: HashSet<String>` | `tags_all` | Direct port. Tag values are canonical strings on the fold side. |
| `optional_capabilities: HashSet<String>` | (drop) | Optional tags don't gate candidates; they only affect scoring (which runs in `behavior::placement`, not the fold). |
| `forbidden_capabilities: HashSet<String>` | **Bridge required** | Folds don't have NOT-tag semantics. Apply at the bridge layer: query without `forbidden`, then filter out matches that carry any forbidden tag. |
| `required_class: Option<u64>` | `class` | Direct port. |
| `min_memory_gb: Option<u32>` | **Bridge required** | Filter against `CapabilityMembership::hardware.memory_gb` post-query. Folds don't have range predicates at the index level. |
| `required_gpu_vendor: Option<GpuVendor>` | **Bridge required** | Same — post-query filter on `hardware.gpu_vendor`. |
| `required_models: Vec<String>` | `tags_all` | Encode as `model:<name>` tags before querying. |
| `required_tools: Vec<String>` | `tags_all` | Encode as `tool:<name>` tags before querying. |

**Risk: the legacy filter has range predicates (min_memory_gb) and negative predicates (forbidden_capabilities) the fold's index doesn't natively support.** The bridge translation is correct but executes them as post-query filters in the hot path. For the typical scheduler workload this is fine (the indexed predicates do most of the selectivity work). For a query with ONLY range predicates and no indexed predicate, performance regresses to "walk every entry." Mitigation: add `by_memory_gb` / `by_gpu_vendor` inverted indices to `CapabilityIndexInner` if profiling shows a hot path; defer until proven necessary.

---

## Caller inventory

23 files reference `CapabilityIndex`. Grouped by criticality:

### Hot path — must be perfect (8 files)

| File | Surface | Risk |
|---|---|---|
| `compute/scheduler.rs` | `place_*` functions consume `find_nodes_scoped` + `find_best_node_scoped` per placement decision | Highest. Scheduler is on every daemon spawn. Wrong filter translation = wrong placement. |
| `compute/replica_group.rs` | `place_with_spread` via `Scheduler` | Same risk as scheduler. |
| `compute/fork_group.rs` | Same | Same. |
| `compute/standby_group.rs` | Same | Same. |
| `compute/group_coord.rs` | Bridge through `Scheduler` | Same. |
| `compute/orchestrator.rs` | Placement decisions during migration | Same. |
| `mesh.rs` | `find_service_nodes(svc)` uses `query` with a `nrpc:<svc>` tag predicate | High. Every nRPC dispatch. |
| `behavior/placement.rs` | Reads `by_tag` / `by_state` via the legacy `CapabilityIndex` ref for scoring | Bridge layer's job — the fold provides the candidates, placement does the scoring. |

### Slow path / control plane (8 files)

| File | Surface | Risk |
|---|---|---|
| `behavior/proximity.rs` | Walks all_nodes for proximity graph | Medium. Periodic; not per-event. |
| `behavior/query.rs` | Some legacy query utilities — likely subsumed by fold queries | Low if dropped, medium if migrated piecemeal. |
| `behavior/predicate.rs` | Filter shape translation utilities | Low — could be reused for the bridge. |
| `behavior/meshdb/planner.rs` | `collect_coverage` walks `all_nodes` for chain-holder discovery | Medium. The chain-tag use case; not yet feature-flag-on by default but matters once meshdb is. |
| `behavior/meshdb/cache.rs` | Capability-version snapshot for cache invalidation | Low. The version counter migrates as a separate atomic. |
| `behavior/meshdb/executor.rs` | Same | Low. |
| `behavior/meshos/sdk.rs` | Exposes capability lookups through the Deck client | Medium — the Deck SDK is operator-facing; signature change is visible to bindings. |
| `subprotocol/migration_handler.rs` | Looks up the migration target's caps | Medium. Cold-ish path. |

### Adjacent integrations (7 files)

| File | Surface | Risk |
|---|---|---|
| `behavior/mod.rs` | Re-exports | Trivial — update the re-export list. |
| `subprotocol/registry.rs` | Capability matching for subprotocol routing | Low. |
| `dataforts/blob/migration.rs` | Uses `axis_cardinality` for migration policies | Low — `axis_cardinality` is documented out-of-scope for the cutover. |
| `dataforts/blob/overflow.rs` | Picks blob-storage-enabled nodes via `query` | Medium. Apply on every blob admission. |
| `behavior/fold/capability.rs` | Test references only | Trivial. |
| `behavior/fold/mod.rs` | Doc references | Trivial. |
| `behavior/capability.rs` | The module itself — DELETED in the final commit | Self. |

---

## Sequenced sub-steps

Each sub-step lands as its own commit on the cutover branch. The
**FINAL** sub-step deletes the legacy module + flips remaining
references; everything before is purely additive (the new fold
runs alongside the legacy index without conflict).

### Sub-step 1: Wire installation infrastructure (additive)

**Deliverable:** `MeshNode::set_capability_fold(Arc<Fold<CapabilityFold>>)` — registers the fold with the `FoldRegistry`, installs the registry as the channel router. After this lands, callers can opt into the new fold WITHOUT removing the legacy index.

**Test:** End-to-end signed-envelope round-trip through `MeshNode::publish_fold_broadcast` + the inbound dispatch arm.

**Risk:** None — additive.

### Sub-step 2: Bridge layer — `fold_capability::find_nodes_scoped` etc.

**Deliverable:** New helper module `behavior::fold::capability_bridge` that exposes the legacy method signatures (`find_nodes_scoped(filter, scope_filter, same_subnet_lookup)` etc.) but routes through the fold. This is the SHIM that callers swap to without changing their call shapes; the legacy `CapabilityIndex` reference becomes a `&Fold<CapabilityFold>` reference.

**Test:** Carry the existing capability test suite assertions forward against the bridge (semantic intent preserved, syntax updated).

**Risk:** Medium. The scope_filter + min_memory_gb + forbidden_capabilities translations are subtle; needs a per-rule test.

### Sub-step 3: Caller rewires (one PR per criticality tier, all additive)

**3a — Hot path callers:** `compute/scheduler.rs` + the four group modules + `mesh.rs::find_service_nodes` + `behavior/placement.rs`. Each call site swaps `&CapabilityIndex` → `&Fold<CapabilityFold>` (via the bridge module). Existing tests carry forward.

**3b — Slow path callers:** `behavior::proximity`, `behavior::query`, `behavior::predicate`, the meshdb planner/cache/executor, `behavior::meshos::sdk`, `subprotocol::migration_handler`.

**3c — Adjacent integrations:** `subprotocol::registry`, `dataforts::blob::overflow`, `behavior::mod` re-exports.

**Test per sub-step:** Run the full lib test suite + the integration test the rewired site relies on. Pre-merge: full benchmark suite against the new fold; regression vs pre-deletion baseline blocks merge.

**Risk:** Low per call site (the bridge has the same signature); medium aggregate (one wrong translation in one site could mask).

### Sub-step 4: Population path — supervisor populates the fold from inbound announcements

**Deliverable:** Replace the existing `CapabilityIndex::index(...)` call site in the inbound capability-announcement handler with `Fold::apply(...)` via a translation shim. The legacy `CapabilityIndex` stops receiving updates; the fold takes over.

**Test:** Carry the existing capability-handler integration test forward against the fold. Pin "after a capability announcement arrives, the fold's query reports the right candidate set."

**Risk:** High — this is the cutover moment. The legacy `CapabilityIndex` will from this commit forward be stale and unused. Any caller still reading from it (vs the fold) sees frozen data.

### Sub-step 5: Delete `behavior::capability::CapabilityIndex` + the related helper types

**Deliverable:** Remove the legacy module entirely. Remove the re-exports from `behavior::mod`. Remove any `use behavior::capability::*` that still references the deleted types.

**Test:** Full lib test suite passes; full strict-clippy passes; the benchmark suite shows no regression vs pre-cutover baseline.

**Risk:** Compile errors at any caller that wasn't rewired in steps 3a-3c. Mitigation: at the start of sub-step 5, `grep -rn 'CapabilityIndex' src/` MUST return empty (excluding the to-be-deleted module + its test file). If anything else matches, fix it in sub-step 3 first.

---

## Test strategy

The framework already has 78 multifold-specific tests pinning the `Fold<K>` shape; sub-step 5 should leave that count intact. Phase 3b adds:

- **Bridge equivalence tests** (sub-step 2): every legacy filter shape produces the same node-set as the legacy `CapabilityIndex::find_nodes_scoped` would have. Mirror the existing capability test suite's coverage.
- **Hot-path regression test** (sub-step 3a): a synthetic scheduler benchmark places 100 daemons across 1000 candidates; measure per-placement latency before and after. Block merge on > 1.5× regression.
- **End-to-end cutover test** (sub-step 4): two-node mesh, publisher emits a `SignedAnnouncement<CapabilityMembership>`, receiver's scheduler picks it via the new fold path.
- **Post-deletion sweep** (sub-step 5): full lib test suite — every existing test that exercised `CapabilityIndex` either passes against the fold or has been deleted as redundant.

---

## Estimated effort

- **Sub-step 1:** 1 session.
- **Sub-step 2:** 1-2 sessions (the bridge translation has subtle parts).
- **Sub-step 3a:** 1-2 sessions (hot path, careful per-caller verification).
- **Sub-step 3b + 3c:** 1 session combined.
- **Sub-step 4:** 1 session.
- **Sub-step 5:** 1 session including the post-deletion sweep.

**Total:** 6-8 sessions of focused work. This is in the right ballpark vs the stripped plan's "Phase 3: 1-2 weeks" — same scope, broken into atomically-reviewable commits.

---

## Risks and mitigations

**Risk: Hot-path latency regression.** The fold's secondary index is good for tag lookups but loses to the legacy `CapabilityIndex` on range predicates (memory_gb) because the bridge layer post-filters. **Mitigation:** add `by_memory_gb` and `by_gpu_vendor` inverted indices to `CapabilityIndexInner` if profiling shows a regression in step 3a. Defer until proven necessary.

**Risk: Behavioral drift on `find_nodes_scoped` scope-filter rules.** The scope/subnet machinery has subtle edge cases (subnet warmup, scope=SameSubnet with unknown peer subnet, etc.). **Mitigation:** sub-step 2's bridge has 1:1 test coverage of each rule before any caller swaps over. The existing `find_nodes_scoped` tests in `behavior::capability::tests` carry forward verbatim.

**Risk: `axis_cardinality` and `collision_count` callers break post-deletion.** **Mitigation:** sub-step 5 must run a final grep for these method names before deleting the module. If callers exist that the cutover didn't migrate, either (a) migrate them in a final sub-step 3d, (b) wrap them on a `CapabilityIndexShim` that returns const values, or (c) decide they're dead code and remove the call site too.

**Risk: Inbound announcement-handler change in sub-step 4 desyncs the fold from the legacy index during the window between sub-step 4 and sub-step 5.** **Mitigation:** sub-step 4 lands AFTER every caller has been rewired in steps 3a-3c. No caller reads from the legacy index after sub-step 4, so its staleness is invisible. Sub-step 5 then deletes it cleanly.

---

## Why "one PR" is one logical PR but multiple commits

The stripped plan calls for atomic same-PR cutovers. That's correct for code review (operators want to see the deletion + rewires in one diff) but doesn't preclude breaking the work into reviewable commits within the PR. Each sub-step above is one commit; the PR's `git log --oneline` reads as a clean sequence:

```
fix(scheduler): rewire placement to Fold<CapabilityFold>
fix(compute/groups): rewire replica/fork/standby placement to fold
fix(mesh): find_service_nodes via fold
...
multifold(phase-3b): delete CapabilityIndex; cutover complete
```

This shape gives reviewers a step-by-step walk without losing the atomicity of "the PR either lands the full cutover or nothing."

---

## Sign-off checklist before opening the PR

- [x] All sub-step commits pass `cargo test --lib --features meshdb` (3979 lib tests green).
- [x] `cargo clippy --lib --features meshdb -- -D warnings` clean on the merged tree.
- [ ] Scheduler benchmark < 1.5× regression on per-placement latency — benches compile (`bench_capability_fold_*` in `benches/net.rs`, `bench_placement_score` in `benches/placement.rs`) but the empirical pre/post comparison hasn't been run.
- [x] `grep CapabilityIndex src/` returns only doc-comment historical references; the struct + impls + supporting types are gone.
- [x] The capability-announcement integration test passes end-to-end (covered by `capability_multihop`, `capability_broadcast`).
- [x] Plan doc updated — see "Deviations from the original plan" below.

---

## Deviations from the original plan

The cutover landed end-to-end, but five things differ from the plan as
originally written. None block sign-off; they're recorded so reviewers
can see the gap between intent and shipped code.

### 1. `may_execute` was ported, not deferred

The original plan didn't enumerate `CapabilityIndex::may_execute` (the
v0.4 capability-auth gate) in the public-API table. It surfaced as a
production caller in `mesh_rpc.rs` during sub-step 3 and had to be
reimplemented on the fold path. The fix:

- Added `allowed_nodes` / `allowed_subnets` / `allowed_groups` fields
  to `CapabilityMembership` so the auth lists ride the same signed
  envelope they did on the legacy `CapabilityAnnouncement`.
- Added `capability_bridge::may_execute(fold, target, tag, caller)`
  with the same union semantics (node OR subnet OR group OR
  permissive-default) the legacy gate had.
- `mesh_rpc.rs`'s two callsites (the per-RPC gate + the call_service
  candidate retain) route through the bridge.

### 2. `metadata: BTreeMap` carried through the fold envelope

The plan flagged "the fold's `CapabilityMembership` doesn't carry the
legacy `metadata` BTreeMap; callers that need metadata access keep the
legacy `CapabilityIndex::get` path until the fold payload is extended"
as a known gap. Testing surfaced that the
`predicate_eval_fixture_matches_via_placement_filter_callback`
cross-binding fixture exercises `metadata_exists` / `metadata_equals`
predicates — without metadata propagation, half the fixture cases fail.

Resolution: added `metadata: BTreeMap<String, String>` to
`CapabilityMembership`. `translate_announcement` populates it from the
legacy ann; `synthesize_capability_set` merges metadata maps across
the publisher's per-class entries.

### 3. `require_models` / `require_tools` filter encoding

The plan's filter-translation table says "encode as `model:<name>` /
`tool:<name>` tags before querying." That's wrong — the canonical wire
form is the multi-tag bundle (`software.model.<i>.id=<name>` etc.),
which a `model:<name>` `tags_all` predicate never matches. Surfaced as
a Node binding test failure (`capabilities.test.ts > round-trips a
complex POJO`).

Resolution: `translate_filter` drops models / tools from `tags_all`;
`membership_passes_post_filter` reuses `CapabilitySet::has_model` /
`has_tool` against a synthesized set per candidate. Same union ("any
must match") semantics the legacy `CapabilityFilter::matches` impl
used.

### 4. Wire `origin_hash` u32 truncation

`CapabilityIndex::by_origin_hash` keyed entries on
`(eid.origin_hash() as u32) as u64` to match the receiver-side
`parsed.header.origin_hash.into()` projection. 3B-2b ported the
reverse index to `MeshNode::origin_hash_to_node` without carrying the
truncation, so post-deletion lookups silently missed and the greedy
admission's `chain_caps` collapsed to empty. Restored in
`b1a0be72`.

The underlying limitation (DoS-by-collision-suppression in dataforts,
~2³² work, no auth-bypass) is documented in
`WIRE_ORIGIN_HASH_64BIT.md` as the wire-format-break follow-on.

### 5. `axis_cardinality` lost; richer queries deferred to Phase 6c

The plan's API-surface table marked `axis_cardinality` as "out of
scope for cutover; migrate as a follow-up." The cutover deleted the
method along with `CapabilityIndex`. The `CardinalityProvider` trait
stays in `behavior/capability.rs` (`HugeCardinality` test mock
satisfies the bound for `predicate.rs` planner tests). Real
cardinality counts against fold state aren't restored — see
**Phase 6c (Capacity Aggregation)** in
`MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`, which lands the
`Fold::aggregate` + `Fold::capacity_ranking` surface and replaces the
legacy `axis_cardinality` semantics with `Aggregation::DistinctValues`
+ `Aggregation::Count` on the fold directly.

This means the bridge stays a thin legacy-shape compatibility layer
forever. Richer query work happens on the fold via 6c; the bridge
doesn't grow `axis_cardinality` / `find_best` / etc. as follow-ups.

### 6. Test surface shape

The original plan said tests "either pass against the fold or have
been deleted as redundant." The shipped shape uses three
`test_*` helpers on `MeshNode`
(`test_inject_capability_announcement`, `test_capability_fold_has`,
`test_capability_fold_get`) so the existing tests substitute calls
mechanically rather than each fixture re-deriving the
fold-translation. The helpers are `#[doc(hidden)]` but `pub` so
binding-side integration tests (Node / Python / Go FFI smoke tests)
can use them too. Roughly 130 callsites across 14 integration test
files + 8 internal lib-test modules migrated through these helpers.

---
