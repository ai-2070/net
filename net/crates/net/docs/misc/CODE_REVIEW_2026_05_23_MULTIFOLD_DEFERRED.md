# Code review — `multifolds` branch — deferred items (2026-05-23)

Branch base: `master`.
Initial branch tip after the cleanup pass: `d2a19e12` ("refactor(capability_aggregation): drop dead numeric_present field").
Scope of the pass: changes since `6ea31ba5` (~88 files, +9,876 / −7,889). Multifold phase 3b CapabilityIndex deletion + phase 6c capability-aggregation surface in Rust/TS/Python/Go/C.

Three review agents (reuse / quality / efficiency) were dispatched in parallel. Eight fixes landed in commits `b18e3840`..`d2a19e12`. The ten items below were initially deferred, then resolved in a follow-up pass — see commits `90f37d0f`..`a02ec3f9` for the per-item changes.

Tagged `[B | H | M | L]`:

- B — blocker, fix before merge.
- H — correctness / API-shape issue worth fixing before the next scale milestone.
- M — operator-visible footgun, latent regression, or duplication worth scheduling.
- L — hygiene, doc drift.

---

## Status

| ID    | Pri | Area                | Title                                                                            | Status |
|-------|-----|---------------------|----------------------------------------------------------------------------------|--------|
| MD-1  | H   | bridge              | `apply_legacy_announcement` silently drops `fold.apply` errors at ~30 callsites  | ✅ `90052eb1` (returns `Result<ApplyOutcome, FoldError>`; all callsites use `.expect`) |
| MD-2  | M   | sdk-go              | Two near-identical Go aggregation surfaces (336 LOC each)                        | ✅ `a02ec3f9` (dropped vendor-template duplicate; `/go/` is canonical) |
| MD-3  | M   | mesh hot path       | `find_best_node*` re-synthesizes caps twice per `max_by` comparison              | ✅ `64bd32c3` (memoize via `best_by_score` helper) |
| MD-4  | M   | placement           | `LegacyPlacement::placement_score` is quadratic in candidate count               | ✅ `b84698f0` (per-target `target_matches_filter`) |
| MD-5  | M   | planner             | `planner.rs::collect_coverage` takes N+1 `with_state` locks                      | ✅ `461711ac` (batched `capability_tags_for_all`) |
| MD-6  | L   | scope helper        | `scope_from_membership_tags` duplicates dead `scope_from_tags`                   | ✅ `18efa4e1` (deleted dead helper + its tests; redirected doc refs) |
| MD-7  | L   | aggregation         | `axis_value_for` and `string_value_for_axis_key` near-duplicates                 | ✅ `6ea489a8` (delegate via shared `split_axis_key`) |
| MD-8  | L   | aggregation         | `TagMatcher::Regex` recompiles regex per `matches_one` call                      | ✅ `bb09af8f` (precompile via `CompiledMatcher`) |
| MD-9  | L   | aggregation hot loop | Per-bucket aggregation match dispatched once per entry per bucket               | ✅ `fb84cd08` (hoist axis-key split via `CompiledAgg`) |
| MD-10 | L   | mod re-export       | `CapabilityIndexInner` exposed `pub` but only used inside `fold/`                | ✅ `90f37d0f` (dropped from `mod.rs` re-exports) |

---

## HIGH — fix before merge

### MD-1 — `apply_legacy_announcement` silently drops `fold.apply` errors

`net/crates/net/src/adapter/net/behavior/fold/capability_bridge.rs:169-172`:

```rust
pub fn apply_legacy_announcement(fold: &Fold<CapabilityFold>, ann: CapabilityAnnouncement) {
    let fold_ann = translate_announcement(&ann);
    let _ = fold.apply(fold_ann);
}
```

Docstring frames this as "primarily for test fixtures", but `rg apply_legacy_announcement` finds ~30 production call sites (`compute/scheduler.rs`, `compute/fork_group.rs`, `compute/replica_group.rs`, `dataforts/blob/migration.rs`, others). A failing `fold.apply` — invalid generation, signature mismatch, anything `FoldError` grows into — is now an invisible no-op.

Fix options:
- Return `Result<ApplyOutcome, FoldError>` and let callers `let _ =` explicitly where the no-op is intentional. Touches ~30 sites; not large but ripples the public bridge surface.
- Keep the void return but log at `warn!` on the error path inside the helper. Smaller change; preserves call-site shape.

Recommended: log-at-`warn` for this pass, then surface the `Result` in a follow-up that audits each callsite for the right policy. Either way drop the misleading "test fixtures" framing from the docstring.

---

## MEDIUM — schedule before next scale milestone

### MD-2 — Two near-identical Go aggregation files

`go/capability_aggregation.go` (336 LOC) and `net/crates/net/bindings/go/net/capability_aggregation.go` (336 LOC) are not byte-identical but the bodies are line-for-line clones:

- Same `TaxonomyAxis` constants (L61-66 / L68-73).
- Same `TagMatcher` / `GroupBy` / `Aggregation` / `CapacityQuery` / `CapacityRow` structs with identical JSON tags.
- Same six `Match*` factories, four `GroupBy*` factories, six `Agg*` factories.
- Same `json.Marshal` + `C.CString` + `net_capability_*` + `json.Unmarshal` flow.

Differences: doc-comment wording, test-file style (table-driven vs flat), and the call shape — `(*MeshNode).CapabilityAggregate` (receiver method) in `/go/` vs. package-level `CapabilityAggregate(meshArc, ...)` in `bindings/go/net/`.

Pick one canonical home (`bindings/go/net/` is the conventional location next to the rest of the cgo bindings) and have the top-level `go/` re-export or thin-wrap. Same applies to the `_test.go` pair (222 vs 214 LOC, same wire-pin assertions restructured).

### MD-3 — `find_best_node*` re-synthesizes caps twice per `max_by` comparison

`net/crates/net/src/adapter/net/mesh.rs:8814-8836` and `:8848-8869`. Legacy was a single `capability_index.find_best(req)` point lookup; the new path:

1. `find_nodes_matching` (full fold query, post-filter, dedupe).
2. `sort_unstable` on candidates.
3. `max_by` — comparator calls `synthesize_capability_set(fold, *a)` AND `synthesize_capability_set(fold, *b)` on every comparison, each taking a `with_state` lock and allocating a `CapabilitySet { tags: HashSet, metadata: BTreeMap }`.

For N candidates that's `2(N-1)` lock acquisitions + `2(N-1)` HashSet+BTreeMap allocations vs. the legacy O(1) point lookup. Latent — no rich-scoring caller exists in production today — but a real algorithmic regression. Fix: hoist caps out once per candidate (collect `(node_id, caps)` pairs, then `max_by_key` on the pair vec).

### MD-4 — `LegacyPlacement::placement_score` quadratic in candidate count

`net/crates/net/src/adapter/net/behavior/placement.rs:181-189`. `placement_score(target, _)` runs the full fold composite query, then linearly checks `candidates.contains(target)`. The scheduler calls `placement_score` once per candidate target, so over N targets that's `N × full-fold-query` — quadratic. `StandardPlacement` (`:504-538`) avoids this with `state.by_node.contains_key(target)` + `synthesize_capability_set(target)` for an O(1) lookup; `LegacyPlacement` should mirror that shape.

### MD-5 — `planner.rs::collect_coverage` N+1 lock pattern

`net/crates/net/src/adapter/net/behavior/meshdb/planner.rs:1111-1149`. Walks `state.by_node.keys()` under one `with_state` lock, then for each publisher calls `capability_tags_for(fold, node_id)` (`fold/capability.rs:454`) which takes another `with_state` lock. `1 + N` locks where a single batched walk would do. The legacy `CapabilityIndex` fed `(node, tags)` pairs out of a single shard read. Fix: extend `capability_tags_for` with a batch variant `capability_tags_for_all(fold) -> HashMap<NodeId, Vec<String>>` and consume it in one read.

---

## LOW — hygiene

### MD-6 — `scope_from_membership_tags` duplicates dead `scope_from_tags`

`capability_bridge.rs:410-443` derives a `CapabilityScope` from `&[String]` raw fold-payload tags. `behavior/capability.rs:688-724` does the same job from `&HashSet<Tag>` typed parsed tags. Bodies are structurally identical (same `scope:tenant:` / `scope:region:` / `scope:subnet-local` precedence).

The typed `scope_from_tags` is `#[allow(dead_code)]` with a docstring saying "all callers were removed in Phase 3B". Its only remaining callers are its own test suite in `capability.rs:3319-3517` (~200 LOC). Cleanest fix: delete the typed function AND its tests, leaving the string-driven `scope_from_membership_tags` as the single owner. Doc comments in `bindings/{node,python}/src/capabilities.rs` + `src/ffi/mesh.rs` reference `scope_from_tags` by name and would need a rename pass.

### MD-7 — `axis_value_for` and `string_value_for_axis_key` near-duplicates

`capability_aggregation.rs:602-610` (`axis_value_for`) and `:628-638` (`string_value_for_axis_key`) both unpack `Tag::AxisValue` and check `(axis, key)`. The second takes a `<axis>.<key>` dotted-string form and resolves it through `TaxonomyAxis::from_prefix`, then could delegate to the first. Extract the resolution step so `string_value_for_axis_key` is one parse + one delegated call.

### MD-8 — `TagMatcher::Regex` recompiles per `matches_one`

`capability_aggregation.rs:112-115`:

```rust
Self::Regex { pattern } => match regex::Regex::new(pattern) {
    Ok(re) => re.is_match(raw),
    Err(_) => false,
},
```

Called once per tag per entry. The doc-comment acknowledges this but offers no mitigation. For a fold with 10k entries × 5 tags per entry × a moderately complex regex, that's 50k compiles. Fix: pre-compile at construction time (validate-then-cache) or wrap with a `OnceCell<Option<Regex>>` keyed on the pattern.

### MD-9 — Per-bucket aggregation match dispatched per entry

`capability_aggregation.rs:309-332`. The nested `match &agg { ... | ... | ... => for raw in &membership.tags { ... } }` block fires once per matching entry per bucket. For each entry, the axis-key parse is repeated. Hoist the axis-key parse outside the per-entry loop (one parse per aggregation, not one per `(entry, tag)` pair).

### MD-10 — `CapabilityIndexInner` exposed publicly without consumer

`fold/mod.rs` re-exports `CapabilityIndexInner` `pub` but no caller outside `fold/` uses it (only `mod.rs` + `capability.rs`). Demote to `pub(crate)` or `pub(super)`. It's an implementation detail of the secondary index, not part of the bridge surface.
