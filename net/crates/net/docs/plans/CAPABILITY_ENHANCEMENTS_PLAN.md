# Capability Enhancements — implementation plan

> Local-only DX, performance, and tooling layered on top of the [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) substrate. Phase A of that plan locked the canonical storage shape (`CapabilitySet { tags: HashSet<Tag>, metadata: BTreeMap<String, String> }`); this plan enriches the **caller-side experience** without touching the wire byte. Companion to the SDK plan ([`CAPABILITY_SYSTEM_SDK_PLAN.md`](CAPABILITY_SYSTEM_SDK_PLAN.md)) and downstream of [`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md). Does not replace any of them.

## Status

**Draft.** Gates on Phase A of `CAPABILITY_SYSTEM_PLAN.md` being complete on `master` (it is, on `capability-system-2`; this plan picks up post-merge). Each of the seven enhancements is independently landable; no single commit blocks the others. Activation gate is per-enhancement: ship when a downstream consumer (Rebel Yell query workloads, RedEX placement, Atomic Playboys workload filters) has a concrete dependency on it.

## Frame

Phase A of the capability system gave the substrate a tiny wire shape and clean projections:

- **Wire format:** `{ tags, metadata }` — just two fields, both flat collections of opaque-from-the-substrate values.
- **Storage:** identical to wire format.
- **Reads:** projections via `caps.views()` reconstruct `HardwareCapabilities` / `SoftwareCapabilities` / `Vec<ModelCapability>` / `Vec<ToolCapability>` / `ResourceLimits` from the canonical tag set + metadata.
- **Writes:** typed setters re-encode into the canonical tag set.

That's the right substrate shape. It's also a fairly **bare** developer experience: predicates are evaluated AST-order, projections are eagerly computed, schemas exist only in the implementer's head, and the only debugging path is `dbg!()` over the result.

This plan layers seven enhancements that turn the bare baseline into a Kubernetes-CRD-level developer experience without expanding the substrate. The substrate stays small; the bindings get smart.

## Why this exists

Three load-bearing reasons:

1. **Phase A optimized for wire correctness, not DX.** Operators authoring placement intent, application developers building federated queries, and dashboards consuming change events all want richer affordances than `caps.views().hardware.gpu.is_some()`. Without these, every consumer reinvents the same patterns (cached projections, predicate builders, change diffs) in their own callsite.

2. **The predicate AST is already cross-binding deterministic.** It was designed to evaluate against a `(tags, metadata)` `EvalContext` for capability queries, but the same AST can drive nRPC request filters, workload selection, dashboard filters, and debug sessions. Promoting it from "capability-query internal" to "the canonical filter language across the mesh" unlocks predicate pushdown, predicate reuse, and one debugging surface for everything that filters.

3. **Lazy / memoized projections are a cheap perf win.** Phase A's `views()` recomputes all five projections every call. Hot paths (capability-index `score()`, FFI conversion, repeated `views().hardware` reads in a loop) pay that cost unnecessarily. A small layer of laziness keeps the API identical but removes the pessimization.

The eternal rule below ensures these enhancements stay surgical — no semantic growth at the substrate level, no new wire bytes, no cross-binding drift.

## What ships

Seven independently-landable enhancements:

1. **Axis schemas** — per-binding type definitions for `hardware.*` / `software.*` / `devices.*` / `dataforts.*` keys. Drives auto-completion, static type-checking, runtime validation, predicate-builder generation.
2. **Lazy memoized projections** — `views()` returns a handle whose fields are computed-and-cached on first access.
3. **Predicate AST in nRPC filters** — embed the existing `Predicate` AST in nRPC request payloads so services can filter at predicate-pushdown speed.
4. **Predicate query planner** — selectivity-aware reordering of an AST's leaves before evaluation. Same semantics, faster.
5. **`CapabilitySet::diff`** — cheap before/after change detection emitting `{added_tags, removed_tags, changed_metadata}`.
6. **Chain composition helpers** — `requireChain` / `requireAnyChain` / `excludeChain` syntactic sugar over the existing `causal:` / `fork-of:` tag shapes.
7. **Predicate recording / replay** — debug session that runs a predicate against a candidate set and reports per-clause hit / miss / cost stats.

What this doc does NOT ship:

- **No new tag shapes.** All seven items work against the four-axis ontology + reserved prefixes locked in `CAPABILITY_SYSTEM_PLAN.md` §1–2.
- **No new wire bytes.** Predicate AST in nRPC rides through nRPC's existing payload typing — postcard-encoded, opaque to the substrate's capability path.
- **No global consensus.** Query planning is local-only (Locked decision 3 of `CAPABILITY_SYSTEM_PLAN.md`); record/replay is local-only.
- **No SDK code generation.** Axis schemas drive type checking, not code generation. Codegen is a future SDK plan extension if it earns its keep.

---

## Design

### 1. Axis-typed Capability Modeling (binding schemas, not wire)

The substrate emits and ingests `Tag` values whose meanings are encoded in their string form (`hardware.gpu`, `hardware.gpu.vram_gb=80`). Today, those meanings live as **convention** — every binding hand-codes the codec functions, every consumer has to remember which keys exist under which axis.

This enhancement introduces **axis schemas** as a per-binding declarative layer:

```text
axis hardware {
    cpu: {
        cores: number
        threads: number
        vendor: string
    }
    gpu: {
        vram_gb: number
        architecture: string
        vendor: string
    }
    memory_gb: number
    storage_gb: number
    network_gbps: number
    limits: {
        max_concurrent_requests: number
        max_tokens_per_request: number
        rate_limit_rpm: number
        max_batch_size: number
    }
}

axis software {
    os: string
    os_version: string
    cuda_version: string
    runtime: { ... }
    framework: { ... }
    model: indexed { id: string, family: string, ... }
    tool: indexed { tool_id: string, name: string, ... }
}
```

The schema is **purely local**, expressed in whatever shape each binding's host language idiomatically supports:

| Binding | Schema form |
|---|---|
| Rust | `const`-eval'd `AxisSchema` struct in `behavior::schema` |
| TypeScript | `.d.ts` types over the `caps.views().hardware` shape |
| Python | TypedDict / Pydantic models for views() output |
| Go | Code-generated structs from a YAML schema spec |

Each binding's schema agrees on **the same key set + field types** because they're all generated from the same canonical shape definition. The shape definition lives in `CAPABILITY_SYSTEM_PLAN.md` §1 plus `tag_codec.rs` — already authoritative.

What schemas unlock:

- **IDE auto-completion** — `caps.views().hardware.gpu.` suggests `vram_gb`, `architecture`, `vendor`.
- **Static type-checking** — `caps.views().hardware.memory_gb + 1` is `number + number` in TS, not `unknown`.
- **Runtime validation** — `validate_capabilities(caps, schema)` flags out-of-axis keys, type mismatches, value-range violations.
- **Predicate-builder auto-completion** — `pred.numericAtLeast("hardware.memory_gb", ...)` knows the key path is valid; `pred.numericAtLeast("hardware.does_not_exist", ...)` is a compile-time error in TS / Rust, runtime warning in Python / Go.
- **CLI introspection** — `cyberdeck caps describe hardware.gpu` prints the field list + types from the schema.

What schemas do NOT do:

- They do NOT change wire format. Tags still ride as opaque strings; the substrate is schema-agnostic.
- They do NOT enforce on incoming data. A peer emitting `hardware.future_field=42` continues to round-trip through the canonical tag set; the schema layer treats unknown axes/keys as forward-compat ride-throughs (matches Phase A's existing forward-compat decoder).
- They do NOT version. Schema changes are binding-version concerns; the wire stays stable across schema bumps.

**Schema source-of-truth file:** `net/crates/net/docs/CAPABILITIES_SCHEMA.md` (new). Each binding regenerates from this canonical doc whenever it's bumped; CI pins binding-vs-doc agreement (build fails if a binding's generated schema drifts from the canonical).

### 2. View Projections as Lazy / Memoized Partial Evaluations

Phase A's `views()`:

```rust
pub fn views(&self) -> CapabilityViews {
    let sorted = sorted_tag_vec(&self.tags);
    let hardware = hardware_from_tags(&sorted);
    let software = software_from_tags(&sorted);
    let resource_limits = resource_limits_from_tags(&sorted);
    let models = models_from_tags(&sorted);
    let tools = tools_from_tags(&sorted);
    // ... layer schemas from metadata onto tools
    CapabilityViews { hardware, software, resource_limits, models, tools }
}
```

Every call materializes all five projections. Hot paths that read only `views().hardware` pay for software / models / tools / limits decoders too. Profile-driven; the eager design was chosen for simplicity in Phase A and is a known leave-money-on-the-table.

This enhancement introduces a **lazy view handle**:

```rust
pub struct CapabilityViews<'a> {
    caps: &'a CapabilitySet,
    sorted_tags: OnceCell<Vec<Tag>>,
    hardware: OnceCell<HardwareCapabilities>,
    software: OnceCell<SoftwareCapabilities>,
    resource_limits: OnceCell<ResourceLimits>,
    models: OnceCell<Vec<ModelCapability>>,
    tools: OnceCell<Vec<ToolCapability>>,
}

impl CapabilityViews<'_> {
    pub fn hardware(&self) -> &HardwareCapabilities {
        self.hardware.get_or_init(|| {
            hardware_from_tags(self.sorted_tags())
        })
    }
    // ... software / resource_limits / models / tools accessors
}
```

`caps.views().hardware()` decodes only hardware tags. `caps.views().models()` separately decodes only model tags (sharing the sorted-tag scratch via the inner `sorted_tags` cell). Repeated `views().hardware()` calls within the same `CapabilityViews` lifetime hit the cache.

**Compatibility:** the existing `views().hardware` field-access API stays available via a borrowing accessor pattern. TS / Python / Go bindings expose the lazy handle natively; Rust callers use the method form.

**Selective projections (post-Phase 1):** `caps.views().hardware()` already exposes selectivity at the call site. A future micro-enhancement could add `caps.views_hardware_only()` etc. as short-circuits that skip the handle's `OnceCell` overhead entirely.

**Streaming projections (post-Phase 2):** if metadata grows large in distributed CAS lookups, the lazy handle can layer in lazy metadata reads (`caps.views().tool_schema(id)` instead of materializing all tool schemas up-front). Out of scope for the initial commit; the laziness foundation enables it.

### 3. Predicate Composability Across Services (nRPC integration)

The `Predicate` AST shipped in Phase A.2 of `CAPABILITY_SYSTEM_PLAN.md` evaluates against an `EvalContext { tags: &HashSet<Tag>, metadata: &BTreeMap<String, String> }`. It's already serializable, deterministic, and cross-binding-stable — the only thing keeping it scoped to capability queries is convention.

This enhancement promotes the AST to the **canonical mesh-wide filter language** by:

1. **Defining `RpcPredicateContext`** — a trait services implement to expose their per-row data to a predicate AST. Workload services map `metadata.intent`, `tags.training-job`, etc. into the AST's evaluation surface.

2. **Embedding `Predicate` in nRPC payloads** — the existing nRPC request envelope gains a `where: Option<Predicate>` field. Postcard-encoded, opaque to substrate routing.

3. **Predicate pushdown helpers** — `pred!{ ... }` macro produces an AST that nRPC handlers can consume directly. Services that don't want predicate support ignore the `where` field; services that do filter their result stream against it before responding.

Example callsite:

```text
mesh.call("ScanTrainingJobs", {
    where: pred!{ exists("hardware.gpu")
                  AND numeric_at_least("hardware.gpu.vram_gb", 48)
                  AND metadata_equals("intent", "ml-training") }
})
```

What this gives:

- **Same AST for capability queries and workload queries.** A predicate that filters peers ("which nodes have ≥48 GB VRAM?") composes with predicates that filter records ("which training jobs were tagged ml-training?") — same evaluation engine, same operator semantics, same debugging tools.
- **Predicate pushdown.** A 10K-row training-job query that ends up filtering to 12 rows ships 12 rows over the wire instead of 10K + filter-on-receive. The predicate AST is a few hundred bytes; the savings scale with data volume.
- **One canonical filter language for the whole mesh.** Operators learn one AST instead of one per service.

What this is NOT:

- This is NOT a query language. The AST is a filter, not a join / aggregate / project surface. Services that need richer composition use the federated query operators from `CAPABILITY_SYSTEM_PLAN.md` Phase E.
- This is NOT cross-mesh. Predicate pushdown is per-call; aggregating predicate filters across federated meshes is its own problem (Atomic Playboys territory).
- This is NOT mandatory. Services opt in by handling the `where` field; substrate-level nRPC routing doesn't care.

### 4. Query Planning + Cost-Based Ordering (still local-only)

The Phase A.2 `Predicate::evaluate` walks the AST in declaration order. For an AST like:

```text
And(numeric_at_least("hardware.memory_gb", 64),  // selectivity ~30%
    metadata_equals("intent", "embedding-cache"),    // selectivity ~5%
    exists("hardware.gpu"))                          // selectivity ~80%
```

evaluating left-to-right means the cheapest-and-most-selective `metadata_equals` branch runs LAST — every node that has memory but isn't an embedding-cache wastes cycles passing the memory check. Reordering the AST to evaluate `metadata_equals` first short-circuits ~95% of candidates after one comparison.

This enhancement adds a **selectivity-aware planner** that:

1. **Tracks per-axis cardinality** — `CapabilityIndex` exposes `axis_cardinality(TagKey)` returning a count of distinct values seen for that key. Cardinality informs selectivity: a key with 1000 distinct values ranges over a high-selectivity space; a key with 3 (e.g. `hardware.gpu.vendor`: nvidia / amd / intel) is low-selectivity.

2. **Reorders AST clauses pre-evaluation** — children of `Predicate::And` are reordered ascending by estimated selectivity (cheap-and-rare first). `Predicate::Or` likewise reorders to evaluate cheap-and-common first (so positive matches short-circuit fast). `Predicate::Not` is order-invariant.

3. **Bloom-aware ordering** — when a predicate involves a bloom-filter-aggregated tag (Phase D of `CAPABILITY_SYSTEM_PLAN.md`), the planner pushes the bloom probe earlier so most candidates are eliminated before precise checks.

4. **Cached cardinality estimates** — capability-index queries have a known fast-path for cardinality (already O(1) per key via the inverted indexes). Cache estimates with a heartbeat-period TTL to avoid recomputation on every plan.

**Same deterministic semantics.** Reordering preserves the AST's semantic meaning — it's a pure local optimization. Two nodes evaluating the same AST produce identical match decisions; only execution speed differs. Pin in property tests: `evaluate(plan(ast), ctx) == evaluate(ast, ctx)` for all `(ast, ctx)`.

**Same API contract.** Callers don't see the planner; they call `predicate.evaluate(ctx)` and the planner runs internally. Opt-out via `predicate.evaluate_unplanned(ctx)` for benchmarking / debugging.

### 5. `CapabilitySet::diff` (cheap change detection)

Today, comparing two `CapabilitySet` values to detect changes requires either:

- Diffing the typed projections (5 separate diffs, expensive)
- Manually scanning tag sets and metadata maps

This enhancement adds:

```rust
impl CapabilitySet {
    pub fn diff(&self, prev: &CapabilitySet) -> CapabilitySetDiff;
}

pub struct CapabilitySetDiff {
    pub added_tags: HashSet<Tag>,
    pub removed_tags: HashSet<Tag>,
    pub changed_metadata: Vec<MetadataChange>,
}

pub enum MetadataChange {
    Added { key: String, value: String },
    Removed { key: String, prev_value: String },
    Updated { key: String, prev_value: String, new_value: String },
}
```

Implementation is two `HashSet::difference` operations + a `BTreeMap` walk. Same cost as `DiffEngine::diff` from `behavior::diff` (which produces typed `DiffOp` ops); this one returns the raw set/map diff for consumers that don't need the structural ops.

What this enables:

- **Daemon capability-change events.** Event-driven placement updates: when a daemon's capability set changes, the placement engine re-evaluates affected daemons. Today this requires a polling comparison; with `diff` it's a single call against the cached previous state.
- **Capability-aware dashboards.** "Show me what tags peer N gained / lost in the last 5 minutes." Single call per heartbeat instead of full-set diff.
- **Delta-based metadata propagation.** When metadata gets large (Phase D bloom-filter aggregation, Phase C metadata-rich announcements), shipping deltas saves bandwidth. Receivers apply deltas via the existing diff-apply path.

`DiffEngine::diff` (which computes structural `DiffOp`s) and `CapabilitySet::diff` (which computes raw set/map diffs) are complementary. Same data, two surfaces — pick the one that matches the consumer's shape.

### 6. Extremely ergonomic chain composition helpers

The `causal:` / `fork-of:` reserved-prefix tags from Phase A.1 are powerful but verbose to assemble:

```rust
caps = caps
    .add_reserved_tag(format!("causal:{chain_hash}"))
    .add_reserved_tag(format!("fork-of:{parent_hash}"));
```

Predicate-side is similarly verbose:

```rust
pred!{ exists("scope:tenant:foo")
       AND any_of(reserved_with_prefix("causal:"))
       AND not(reserved_equals(format!("causal:{exclude_hash}"))) }
```

This enhancement adds binding-native helpers across Rust / TS / Python / Go:

```text
caps.requireChain(hash)                                     # add `causal:<hash>` reserved tag
caps.requireChain(hash, { minSeq: 100, maxSeq: 200 })       # add range form
caps.requireAnyChain([hash1, hash2])                        # OR-style
caps.excludeChain(hash)                                     # NOT-style; emits a marker the predicate consumes
caps.fromFork(parent_hash)                                  # add `fork-of:<parent_hash>`
caps.heatLevel(0.85)                                        # add `heat:0.85`
```

And the predicate side:

```text
pred.requireChain(hash)
pred.requireAnyChain([hash1, hash2])
pred.excludeChain(hash)
pred.minSeq("hardware.foo.seq", 100)
```

Pure syntactic sugar over the underlying tag emit / Predicate AST. Each helper is one or two lines of binding code. The DX improvement is significant — intent reads off the page rather than buried in string formatting.

### 7. Predicate Recording / Replay for Debugging

Today, debugging "why did my federated query return 0 results when I expected 50?" is detective work: log all candidate node IDs, hand-evaluate the predicate against each, find the clause that's wrong. Painful and slow.

This enhancement adds a debug session:

```rust
let report = mesh.debug_predicate(&predicate)
    .against_capability_index()
    .run();

report.print();
```

Output:

```text
Predicate evaluation report (1042 candidates)
─────────────────────────────────────────────
Predicate: And [
  exists("hardware.gpu")               -> 312 matched, 730 filtered (selectivity 30%)
  numeric_at_least("hardware.gpu.vram_gb", 48)
                                       -> 89 matched, 223 filtered (selectivity 29%)
  metadata_equals("intent", "ml-training")
                                       -> 12 matched, 77 filtered (selectivity 13%)
]

Final result: 12 nodes match.
Filtered breakdown:
  730 nodes lacked hardware.gpu        (clause 0)
  223 nodes had GPU but < 48 GB VRAM   (clause 1)
   77 nodes met hardware bar but
      didn't have intent=ml-training   (clause 2)

Per-clause cost: 0.42ms / 0.31ms / 0.18ms (avg per node)
Bloom-filter probes: 0 (no bloom-aggregated keys in predicate)
```

What this gives:

- **Per-clause hit/miss stats.** "Why did my filter return 0?" → "Clause 2 filtered every candidate; let me check that clause."
- **Selectivity diagnostics.** Clauses with very low selectivity (filter <1%) flag that the user's mental model of their data may not match reality.
- **Per-clause cost.** Identifies which clauses are slow (e.g. semver comparisons over a long version string vs. boolean exists checks).
- **Bloom hit vs. fallback diagnostics.** When Phase D's bloom-aggregated tags are in the predicate, distinguishes between "bloom said no, skipped precise check" vs. "bloom said yes, precise check confirmed match" vs. "bloom said yes, precise check refuted match" (false positive).

Implementation reuses the query planner's evaluation engine — same AST, same cardinality cache, same evaluation order — but instruments each clause with hit/miss counters. Adds ~5% overhead to predicate evaluation in debug-record mode; opt-in only.

**Replay:** save the debug session to disk (`mesh.debug_predicate(&pred).record_to("session.json")`); load it later (`PredicateDebugReport::from_file("session.json").print()`) for offline analysis.

---

## Phasing

Each enhancement is independently landable. Recommended sequencing (by ROI):

### Phase 1 — Lazy projections + `CapabilitySet::diff` (3 days)

The two cheapest, highest-leverage enhancements. Lazy projections are a pure perf win with no API change; `diff` is a new method that doesn't perturb anything.

- `CapabilityViews` becomes a borrowing handle with `OnceCell`-cached projections.
- `views().hardware` field-access pattern preserved via accessor methods (Rust) and natural property accessors (TS / Python / Go).
- `CapabilitySet::diff(prev)` ships alongside.
- Performance baseline pinned: hot-path `caps.views().hardware.X` < 50ns post-cache (down from ~5µs eager).

### Phase 2 — Axis schemas (1 week)

Authoritative schema doc + per-binding generation. Drives auto-completion / type-checking / runtime validation across all four bindings.

- `net/crates/net/docs/CAPABILITIES_SCHEMA.md` is the canonical source.
- Rust: `behavior::schema::AXIS_SCHEMA` const-eval'd struct.
- TS: `.d.ts` types over `views()` output.
- Python: TypedDict/Pydantic models.
- Go: code-generated structs from the schema spec.
- CI: build fails if any binding's generated schema diverges from the canonical doc.

### Phase 3 — Chain composition helpers (3 days)

Pure syntactic sugar. Lands across all four bindings in lockstep.

- `caps.requireChain` / `requireAnyChain` / `excludeChain` / `fromFork` / `heatLevel`.
- Predicate equivalents: `pred.requireChain` etc.
- Documentation in each binding's SDK guide.

### Phase 4 — Query planning (1 week)

Selectivity-aware AST reordering. Drops in under the existing `Predicate::evaluate` API; opt-out via `evaluate_unplanned` for benchmarking.

- `CapabilityIndex::axis_cardinality(TagKey)` exposed.
- `Predicate::plan()` reorders And/Or children by estimated selectivity.
- Bloom-aware path lights up when Phase D of `CAPABILITY_SYSTEM_PLAN.md` lands.
- Property test: `evaluate(plan(ast), ctx) == evaluate(ast, ctx)` for all `(ast, ctx)`.
- Benchmark: 10x speedup on a worst-case predicate (high-selectivity clause buried last).

### Phase 5 — Predicate AST in nRPC filters (2 weeks)

The biggest enhancement. Promotes the predicate AST to the canonical mesh-wide filter language.

- `RpcPredicateContext` trait — services implement to expose row data.
- nRPC request envelope gains `where: Option<Predicate>` field.
- Per-binding `pred!{ ... }` macros / fluent builders / Pydantic-style helpers.
- Predicate pushdown demo: a 10K-row scan filtered to 12 rows over the wire (vs. 10K rows + filter-on-receive).
- Composability tests: same predicate AST runs against a `CapabilityIndex` and a workload service's row stream, produces consistent results.

### Phase 6 — Predicate recording / replay (1 week)

Debug session over the planner's evaluation engine. Per-clause hit/miss/cost stats; bloom hit vs. fallback diagnostics.

- `mesh.debug_predicate(&pred).run()` returns a `PredicateDebugReport`.
- `.record_to(path)` / `PredicateDebugReport::from_file(path)` for replay.
- ~5% overhead in debug-record mode; opt-in only.
- Documentation on common debug patterns ("my filter returns 0", "my filter is slow", "my bloom is over-matching").

**Total: ~5 weeks** sequential. Phases 1, 2, 3 are parallelizable; Phase 4 unblocks Phase 6; Phase 5 is independent of all others.

---

## Test strategy

### Unit

- **Lazy projections (Phase 1).** `views().hardware()` decoded once and cached; second call hits cache. `views()` calls don't materialize unread projections (instrument `*_from_tags` callsites with a counter; assert zero calls for unread axes).
- **`CapabilitySet::diff` (Phase 1).** Empty-vs-empty produces empty diff; X-vs-empty produces full added; metadata key-renames correctly reported as Removed+Added (not as Updated, since key identity changed).
- **Axis schema validation (Phase 2).** `validate_capabilities(caps, schema)` flags out-of-axis keys with `SchemaError::UnknownAxis`, type mismatches with `SchemaError::TypeMismatch`. Forward-compat: unknown keys under known axes pass with a warning, not a hard error.
- **Chain helpers (Phase 3).** `caps.requireChain(hash)` produces a `Tag::Reserved { prefix: "causal:", body: hash }`. `caps.requireChain(hash, {min: 100})` produces the indexed-range form per `CAPABILITY_SYSTEM_PLAN.md` §2.
- **Query planner (Phase 4).** `plan(ast)` reorders And children ascending by selectivity; Or children descending by positive selectivity; Not children unchanged. Property: `evaluate(plan(ast), ctx) == evaluate(ast, ctx)`.
- **Predicate AST in nRPC (Phase 5).** Round-trip serialize/deserialize of `Predicate` via nRPC postcard payload; bytes are stable across runs.
- **Debug report (Phase 6).** Per-clause hit/miss counters reflect actual evaluation; bloom-probe stats appear iff predicate references bloom-aggregated keys.

### Integration

- **nRPC predicate pushdown end-to-end (Phase 5).** Service exposes 10K rows; client sends a high-selectivity predicate; only matching rows arrive over the wire. Pin both correctness (matched rows agree with local-evaluation reference) and bandwidth (wire bytes ≪ full-stream bytes).
- **Schema CI guard (Phase 2).** Each binding's regenerated schema matches the canonical doc byte-for-byte. Fail loudly on drift.
- **Diff-driven placement re-evaluation (Phase 1).** Daemon's capability set changes; placement engine receives the diff event; reschedule decision matches what a from-scratch placement would compute.
- **Debug replay (Phase 6).** Record a session against a live mesh; load offline; per-clause stats reproduce exactly.

### Property

- **Planner determinism.** For randomly-generated AST + cardinality estimates, `plan(ast)` is deterministic across runs; `plan(plan(ast)) == plan(ast)` (idempotent).
- **Predicate equivalence.** `evaluate(plan(ast), ctx) == evaluate(ast, ctx)` for randomly-generated ASTs and contexts.
- **Diff round-trip.** `apply_diff(prev, prev.diff(curr)) == curr` (composes with `DiffEngine` from `behavior::diff`).

### Performance

- **Lazy projections.** `caps.views().hardware.memory_gb` repeated 1M times: < 50ns per call after first (vs. ~5µs eager). Pinned via Criterion bench.
- **Query planner.** Worst-case AST (high-selectivity clause buried last among 5 clauses) speeds up 10x or more over unplanned. Avg-case overhead vs. unplanned: < 5%.
- **`CapabilitySet::diff`.** O(n) in `|tags| + |metadata|`; < 10µs for 100-tag sets.
- **Predicate pushdown.** 10K-row scan with 0.1% selectivity: > 100x bandwidth reduction vs. filter-on-receive.

---

## Locked decisions

The eternal rule below is binding for every enhancement; ignoring it is a plan-revision concern, not a per-phase implementation concern.

### The eternal rule

All seven enhancements MUST preserve:

1. **Wire = `tags + metadata` only.** No new fields on `CapabilitySet`'s wire shape. Predicate AST in nRPC rides as the *call's* payload, not as part of `CapabilityAnnouncement`.
2. **All smarts local to callers.** Schemas, lazy projections, query planning, debug sessions — every enhancement runs in the caller's process. No new substrate-side state.
3. **Cross-binding deterministic AST.** Predicate AST evaluation produces identical results across Rust / TS / Python / Go for any `(ast, ctx)`. Schemas agree on the same key set across bindings (CI-enforced).
4. **No semantic growth at the substrate.** `CapabilitySet`, `Tag`, `TaxonomyAxis`, `Predicate` keep their Phase A semantics. New methods, new helpers, new debugging tools — but no new wire-affecting concepts.

If a proposed enhancement breaks any of these four, it goes into a different plan (Atomic Playboys territory: federated query, mesh-wide debugging, distributed scheduling).

### Schema is binding-local

Schemas are not part of the wire. A peer running an older schema continues to interop with a peer running a newer one — both see opaque tag strings. Forward-compat decoders (which Phase A's `tag_codec` already uses) handle unknown keys gracefully.

This means: schema bumps are *binding-version concerns*, not protocol concerns. Cyberdeck binding 0.13 may add new axis keys; cyberdeck binding 0.12 sees those keys as forward-compat ride-throughs.

### Predicate AST is the canonical filter language

`CAPABILITY_SYSTEM_PLAN.md` §6a defines the AST. This plan promotes it to nRPC-level usage (Phase 5) without forking. New predicate variants are AST-version concerns: every binding agrees on the variant set; adding a variant is a coordinated bump across all four.

This implies a soft contract: predicate-AST evolution lands together across bindings. A peer with binding 0.13 sending a Phase-6 variant to a peer with binding 0.12 will receive a `PredicateDecodeError` — handled gracefully (filter not applied, all rows return), but a useful signal that the cluster is mid-upgrade.

### Lazy projections must produce identical results to eager

Pinned in property tests. The lazy implementation MUST be a pure performance optimization: any callsite that observed the eager `views()` output continues to observe the same value. Equivalently, Phase 1 has zero observable behavior change beyond timing.

### All enhancements opt-in

- Lazy projections: API-compatible drop-in (no opt-in needed; ships as the default).
- `CapabilitySet::diff`: new method; doesn't affect existing callers.
- Axis schemas: validation is opt-in (`validate_capabilities(caps, schema)`); auto-completion is IDE-driven, no runtime cost.
- Chain helpers: pure additions to the binding API.
- Query planner: opt-out via `evaluate_unplanned` for benchmarking; default is planned.
- Predicate AST in nRPC: services opt in by handling the `where` field. Substrate routing doesn't care.
- Debug record/replay: opt-in only. Production hot paths don't pay the ~5% record overhead.

---

## Risks

- **Schema drift.** A binding's schema diverges from the canonical doc (e.g. someone adds a Rust-side axis key without updating the doc). *Mitigation:* CI guard that regenerates each binding's schema from the doc and diffs against the committed schema; build fails on mismatch.

- **Predicate AST evolution outpaces bindings.** A new variant lands in Rust before TS / Python / Go catch up; cross-binding queries break. *Mitigation:* AST evolution is a coordinated multi-binding commit (same as wire-format changes; locked decision in `CAPABILITY_SYSTEM_SDK_PLAN.md`). Decoders fall back gracefully on unknown variants.

- **Cache coherency under concurrent mutation (lazy projections).** Caller mutates `caps` between two `views().hardware()` calls; second call returns stale cached projection. *Mitigation:* `views()` returns a borrowing handle with lifetime tied to `&caps`; mutation requires `&mut caps`, which invalidates the handle. Compiler-enforced.

- **Query planner mis-estimates cardinality, makes wrong reorder decision.** Planner's reorder is worse than declaration-order for some adversarial AST + data shape. *Mitigation:* per-call max-overhead bound (< 5% vs. unplanned in worst case); benchmark suite covers adversarial cases. Opt-out via `evaluate_unplanned` for callers that want guaranteed declaration-order.

- **Predicate pushdown blows up service-side eval cost.** A naive service implementation evaluates an expensive predicate against every candidate row when an index lookup would have been cheaper. *Mitigation:* nRPC `where` is advisory; services can choose to use it as a hint and combine with their own indexing (e.g. evaluate against an indexed candidate set, not full scan). Documentation calls out the pattern.

- **Debug record/replay leaks sensitive metadata.** Recorded sessions may capture metadata values (intent, colocation hints, etc.) that the operator considers sensitive. *Mitigation:* record format includes a redaction step (`session.redact_metadata_keys(&["password", "tenant-id"])`); CI / docs encourage redaction before persisting sessions outside the recorder's process.

---

## Effort

**~5 focused weeks parallelisable.**

- Phase 1 (lazy projections + diff): 3 days
- Phase 2 (axis schemas + CI): 1 week
- Phase 3 (chain helpers): 3 days
- Phase 4 (query planner): 1 week
- Phase 5 (predicate AST in nRPC): 2 weeks
- Phase 6 (debug record/replay): 1 week

Phases 1, 2, 3 are parallelisable. Phase 4 unblocks Phase 6 (debug session reuses planner's eval engine). Phase 5 is independent of all others.

LoC estimates:
- ~1500 LoC core (lazy `OnceCell` projections + diff + planner + debug session)
- ~600 LoC predicate AST plumbing in nRPC
- ~2000 LoC binding-side glue (Rust / TS / Python / Go × 7 enhancements)
- ~2500 LoC tests (unit + integration + property + perf benchmarks)

Bindings parallelise across four engineers per phase; one engineer can serialise to ~7 weeks.

---

## Activation gate

Per-enhancement activation:

| Enhancement | Activation gate |
|---|---|
| Lazy projections | Phase A complete; ship immediately. |
| `CapabilitySet::diff` | Phase A complete; ship immediately. |
| Axis schemas | Demand from binding consumers (auto-completion request, runtime validation request). |
| Chain helpers | Phase B (tag shapes for discovery) of `CAPABILITY_SYSTEM_PLAN.md` ready, so chain tags exist on the wire. |
| Query planner | Workload that benefits demonstrably (high-selectivity-clause-buried-last predicate from a real consumer). |
| Predicate AST in nRPC | First service that wants predicate pushdown (likely Rebel Yell training-job scan or Atomic Playboys workload selector). |
| Debug record/replay | Operator pain point (someone files a "why did my query return 0?" support request). |

Enhancements ship independently as their gates fire. The eternal rule above is the only cross-cutting invariant.

---

## Cross-cutting: relationship to other plans

**Builds on:**

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — Phase A's `{tags, metadata}` storage shape is the substrate this plan layers on. Predicate AST primitives from §6a are extended (Phase 5) to nRPC-wide usage.
- [`CAPABILITY_SYSTEM_SDK_PLAN.md`](CAPABILITY_SYSTEM_SDK_PLAN.md) — per-binding ergonomics. This plan's chain helpers (Phase 3) and axis schemas (Phase 2) are SDK-layer enhancements that fit naturally into the existing SDK structure.
- [`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md) — broadcast machinery. This plan's `CapabilitySet::diff` (Phase 1) is the natural input for a future delta-based broadcast optimization.

**Consumed by:**

- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — `PlacementFilter` scoring uses `CapabilitySet::views()` extensively. Lazy projections (Phase 1) keep that path cheap. Predicate AST in nRPC (Phase 5) lets RedEX leaders push placement filters into replica candidate scans.
- [`misc/DATAFORTS_PLAN.md`](misc/DATAFORTS_PLAN.md) Phase 1 — chain composition helpers (Phase 3) + predicate AST in nRPC (Phase 5) compose naturally with the greedy-LRU placement filter.
- Future Atomic Playboys candidates — full federated MeshDB, full federated scheduler, mesh-wide debugging — all build on the predicate-AST-as-canonical-language foundation laid by Phase 5 here.

**Supersedes:** none. Layers additively on top of Phase A; doesn't replace any existing primitive.

---

## See also

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — the substrate this plan enriches.
- [`CAPABILITY_SYSTEM_SDK_PLAN.md`](CAPABILITY_SYSTEM_SDK_PLAN.md) — per-binding SDK structure.
- [`CAPABILITIES.md`](../CAPABILITIES.md) — operator-facing capability documentation (will incorporate axis schemas from Phase 2).
- [`COMPUTE.md`](../COMPUTE.md) — Mikoshi integration notes; consumer of `CapabilitySet::diff` for placement re-evaluation triggers.
