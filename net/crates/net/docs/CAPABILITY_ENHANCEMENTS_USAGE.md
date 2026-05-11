# Capability Enhancements — Worked Examples

Concrete usage patterns for the features shipped under [`plans/CAPABILITY_ENHANCEMENTS_PLAN.md`](plans/CAPABILITY_ENHANCEMENTS_PLAN.md). Companion to the canonical schema at [`CAPABILITIES_SCHEMA.md`](CAPABILITIES_SCHEMA.md) and the substrate spec at [`plans/CAPABILITY_SYSTEM_PLAN.md`](plans/CAPABILITY_SYSTEM_PLAN.md). Every example below is Rust against `behavior::*`; per-binding surfaces (TS / Python / Go) ship under [`plans/CAPABILITY_SYSTEM_SDK_PLAN.md`](plans/CAPABILITY_SYSTEM_SDK_PLAN.md) Phase 9 and re-export the same shapes.

The recipes assume `use net::adapter::net::behavior::*;` unless noted.

---

## Table of contents

1. [Lazy view projections](#1-lazy-view-projections)
2. [`CapabilitySet::diff` — change detection](#2-capabilitysetdiff--change-detection)
3. [Axis schemas + validation](#3-axis-schemas--validation)
4. [Chain composition helpers](#4-chain-composition-helpers)
5. [Predicate construction + evaluation](#5-predicate-construction--evaluation)
6. [Cardinality-aware planner](#6-cardinality-aware-planner)
7. [Predicate debug sessions](#7-predicate-debug-sessions)
8. [PredicateWire — flat-tree IR for serialization](#8-predicatewire--flat-tree-ir-for-serialization)
9. [nRPC `net-where` request filters](#9-nrpc-net-where-request-filters)
10. [Service-side row filtering](#10-service-side-row-filtering)
11. [Index-driven discovery — `find_nodes_matching`](#11-index-driven-discovery--find_nodes_matching)

---

## 1. Lazy view projections

Phase 1 of the enhancement plan made `caps.views()` a borrowing handle with `OnceCell`-cached projections. Hot-path access is < 50 ns post-cache; reading only one axis no longer pays for the others.

```rust
let caps = CapabilitySet::new()
    .with_hardware(
        HardwareCapabilities::new()
            .with_memory(65536)
            .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "h100", 81920)),
    )
    .with_metadata("intent", "ml-training");

let v = caps.views();
let mem = v.hardware().memory_mb;        // first call: decodes hardware tags
let gpu = v.hardware().gpu.is_some();    // cached; pointer load only
let model_count = v.models().len();      // first call: decodes model.* tags
```

Mutations to `caps` invalidate the handle (compiler-enforced — `views()` borrows `&caps`, mutators take `&mut caps`).

For hot loops over many capability sets, materialize tags once and call `evaluate()` directly:

```rust
for caps in capability_sets {
    let tags: Vec<Tag> = caps.tags.iter().cloned().collect();
    let ctx = EvalContext::new(&tags, &caps.metadata);
    if predicate.evaluate(&ctx) { /* ... */ }
}
```

---

## 2. `CapabilitySet::diff` — change detection

Phase 1. Cheap before/after comparison emitting tag deltas + per-key metadata changes.

```rust
let prev = CapabilitySet::new()
    .add_tag("software.os=linux")
    .with_metadata("intent", "embedding-cache");

let curr = prev
    .clone()
    .add_tag("hardware.gpu")
    .with_metadata("intent", "ml-training")  // value changed
    .with_metadata("owner", "alice");        // new key

let diff = curr.diff(&prev);

assert_eq!(diff.added_tags.len(), 1);   // hardware.gpu added
assert!(diff.removed_tags.is_empty());

for change in &diff.changed_metadata {
    match change {
        MetadataChange::Added { key, value } => println!("added: {key}={value}"),
        MetadataChange::Removed { key, prev_value } => {
            println!("removed: {key}=(was {prev_value})");
        }
        MetadataChange::Updated { key, prev_value, new_value } => {
            println!("updated: {key}={new_value} (was {prev_value})");
        }
    }
}
```

Powers event-driven placement updates ("daemon's caps changed → re-evaluate placement"), capability-aware dashboards, and delta-based metadata propagation. Composes with `behavior::diff::DiffEngine` — `diff` returns the raw set/map shape; `DiffEngine` returns structural ops for the propagation path.

A key rename surfaces as `Removed + Added`, NOT `Updated` (key identity changes are semantically distinct from value changes).

---

## 3. Axis schemas + validation

Phase 2. Canonical schema lives at [`CAPABILITIES_SCHEMA.md`](CAPABILITIES_SCHEMA.md); Rust mirror at `behavior::schema::AXIS_SCHEMA`.

```rust
use net::adapter::net::behavior::{validate_capabilities, ValidationReport};

let caps = CapabilitySet::new()
    .with_hardware(HardwareCapabilities::new().with_memory(65536))
    .with_metadata("intent", "ml-training")
    .add_tag("hardware.future_field=42")     // unknown key: forward-compat warning
    .add_tag("nat:full-cone");                 // legacy: warning

let report = validate_capabilities(&caps);

if !report.is_valid() {
    for err in &report.errors {
        eprintln!("schema error: {err:?}");
    }
}
for warning in &report.warnings {
    println!("warning: {warning:?}");
}
```

Validator categories (per `CAPABILITIES_SCHEMA.md` "Validation behavior"):

- **Errors** (`SchemaError`) — `UnknownAxis`, `TypeMismatch`, `IndexMalformed`. Operator should fix.
- **Warnings** (`ValidationWarning`) — `UnknownKey` (forward-compat ride-through), `MetadataOversize` (4 KB soft cap), `LegacyTag`. Hygiene; usually leave alone.

Use `validate_capabilities_against(&caps, &custom_schema)` to layer application-specific schema extensions on top of the substrate's canonical schema.

---

## 4. Chain composition helpers

Phase 3. Sugar over `causal:` / `fork-of:` / `heat:` reserved-prefix tags.

```rust
let caps = CapabilitySet::new()
    .require_chain("origin-hash-abc")                       // causal:origin-hash-abc
    .require_chain_tip("chain-with-tip", 1024)              // causal:chain-with-tip:1024
    .require_chain_range("range-chain", 100, 500)           // causal:range-chain[100..500]
    .require_any_chain(["alt-1", "alt-2"])                  // two more causal: tags
    .from_fork("parent-hash")                               // fork-of:parent-hash
    .heat_level("origin-hash-abc", 0.85);                   // heat:origin-hash-abc=0.85
```

Helpers silently drop empty / blank inputs (matches the scope-helper convention). `require_chain_range` drops inverted or zero-length ranges. `heat_level` clamps to `[0.0, 1.0]` and emits two-decimal precision; non-finite rates are dropped.

All idempotent — repeated calls with the same arguments don't duplicate (`HashSet` semantics).

Predicate-side helpers (`pred.requireChain` / `excludeChain` / etc.) are in the SDK plan's Phase 5 — they need `Predicate` AST extensions for reserved-tag matching.

---

## 5. Predicate construction + evaluation

Phase A foundation; Phase 4/6 of the enhancement plan add the planner and debug session.

### Builder via `pred!` macro

```rust
let pred = pred!(
    exists "hardware.gpu"
    && "hardware.memory_mb" >= 65536
    && metadata "intent" == "ml-training"
);
```

### Builder via constructor methods (cross-binding-stable)

```rust
let pred = Predicate::And(vec![
    Predicate::Exists {
        key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
    },
    Predicate::NumericAtLeast {
        key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
        threshold: 65536.0,
    },
    Predicate::MetadataEquals {
        key: "intent".into(),
        value: "ml-training".into(),
    },
]);
```

### Evaluation against a context

```rust
let tags: Vec<Tag> = caps.tags.iter().cloned().collect();
let ctx = EvalContext::new(&tags, &caps.metadata);

if pred.evaluate(&ctx) {
    /* matched */
}

// Or shortcut:
if pred.matches_capability_set(&caps) { /* ... */ }
```

`pred.evaluate(&ctx)` runs the planner-reordered AST; `pred.evaluate_unplanned(&ctx)` runs declaration order (debugging / benchmarking only). Both produce identical results.

---

## 6. Cardinality-aware planner

Phase 4 + follow-ons. When the index is available, `evaluate_with_index` uses per-key distinct-value counts to refine clause ordering: high-cardinality clauses (rare-true) sort first in `And`; low-cardinality clauses (often-true) sort first in `Or`.

```rust
let index = CapabilityIndex::new();
// ...populate index from announcements...

let pred = Predicate::And(vec![
    Predicate::MetadataEquals {                        // low static cost, high cardinality
        key: "owner".into(),
        value: "alice".into(),
    },
    Predicate::SemverCompatible {                      // high static cost, low cardinality
        key: TagKey::new(TaxonomyAxis::Software, "runtime.python"),
        version: "3.11".into(),
    },
]);

let result = pred.evaluate_with_index(&ctx, &index);
```

The planner reads `index.axis_cardinality(key)` (O(1) via `by_axis_key`) and `index.metadata_value_cardinality(key)` (O(1) via `by_metadata`) per leaf to compute `dynamic_cost = static_cost / cardinality` (And-mode) or `dynamic_cost_or = static_cost × cardinality` (Or-mode).

Equivalence is pinned: `evaluate_with_index(&ctx, &index) == evaluate_unplanned(&ctx)` for any `(pred, ctx, index)`. The reordering is pure performance; semantics never change.

Direct cardinality lookups are also exposed:

```rust
let intent_cardinality = index.metadata_value_cardinality("intent");
let memory_cardinality = index.axis_cardinality(
    &TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
);
```

---

## 7. Predicate debug sessions

Phase 6. Per-clause hit/miss diagnostics over the planner's evaluation engine. Answers "why did my filter return zero?" without `dbg!`-then-rerun cycles.

```rust
let candidates: Vec<EvalContext> = /* ... */;
let report = PredicateDebugReport::from_evaluations(&pred, candidates);

println!("{}", report.render());
// Predicate evaluation report
// ─────────────────────────────────────────
// Total candidates: 1042
// Matched:          12 (1.2%)
//
// Per-clause stats (alphabetical):
//   And(3 clauses)                                           evaluated  1042, matched    12 ( 1.2%)
//   Exists(hardware.gpu)                                      evaluated  1042, matched   312 (29.9%)
//   MetadataEquals(intent=ml-training)                        evaluated   312, matched    12 ( 3.8%)
//   NumericAtLeast(hardware.memory_mb >= 65536)               evaluated  1042, matched   441 (42.3%)
```

The aggregator is `BTreeMap`-ordered (deterministic across runs). Each clause's `evaluated` count reflects how many candidates actually reached it — short-circuited candidates aren't included, so an operator can see at a glance "the And cut traffic to 312 by clause 1, then to 12 by clause 2".

For per-context inspection (instead of aggregate), use:

```rust
let (matched, trace) = pred.evaluate_with_trace(&ctx);
walk_trace(&trace);  // your own diagnostic walker
```

`ClauseTrace` is a tree mirroring the AST; short-circuited siblings are dropped from the trace.

---

## 8. `PredicateWire` — flat-tree IR for serialization

Phase 5.A. The recursive `Predicate` shape can't ride serde-derive cleanly (recursion-limit explosion compounding with the substrate's event serializer). `PredicateWire` flattens the AST into a `Vec<PredicateNodeWire>` with `u32` child indices.

```rust
let pred = Predicate::And(vec![
    Predicate::Exists { key: TagKey::new(TaxonomyAxis::Hardware, "gpu") },
    Predicate::MetadataEquals {
        key: "intent".into(),
        value: "ml-training".into(),
    },
]);

// Predicate → wire
let wire: PredicateWire = pred.to_wire();
let json = serde_json::to_string(&wire).unwrap();

// Wire → JSON → wire → Predicate
let parsed: PredicateWire = serde_json::from_str(&json).unwrap();
let rebuilt: Predicate = parsed.into_predicate().unwrap();
assert_eq!(pred, rebuilt);
```

Round-trip is byte-stable (post-order serialization; root at the highest index). Cross-binding fixture: `tests/cross_lang_capability/predicate_nrpc_envelope.json`.

`PredicateWire::into_predicate()` validates structural integrity:

- `Empty` — empty `nodes` table.
- `RootOutOfBounds` — `root_idx ≥ nodes.len()`.
- `ChildOutOfBounds` — composite references a non-existent child index.
- `CycleDetected` — child index ≥ parent index (post-order requires children at lower indices; defends against malformed/malicious payloads).

---

## 9. nRPC `net-where` request filters

Phase 5.B. Predicates ride as a JSON-encoded `PredicateWire` in the canonical `net-where` request header. Substrate (`cortex/rpc`) carries the header opaquely — services that opt in look up the header on receive.

### Client side

```rust
let pred = Predicate::And(vec![
    Predicate::Exists { key: TagKey::new(TaxonomyAxis::Hardware, "gpu") },
    Predicate::NumericAtLeast {
        key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
        threshold: 32768.0,
    },
]);

let header = predicate_to_rpc_header(&pred)?;
// (header.0 == RPC_WHERE_HEADER, header.1 == JSON-encoded PredicateWire bytes)

let mut request_headers = vec![
    ("trace-id".to_string(), trace_id_bytes),
    header,  // net-where
];
mesh.send_rpc(svc, body, request_headers).await?;
```

Header-value cap is 4 KB (`MAX_PREDICATE_RPC_HEADER_VALUE_LEN`); `predicate_to_rpc_header` returns `Err(TooLarge)` if the encoded predicate exceeds it. Typical predicates encode well under 1 KB.

### Service side

```rust
fn handle_request(headers: &[(String, Vec<u8>)], rows: Vec<MyRow>) -> Vec<MyRow> {
    let pred_opt: Option<Predicate> =
        match predicate_from_rpc_headers(headers) {
            None => None,                // no filter — return all rows
            Some(Ok(pred)) => Some(pred),
            Some(Err(e)) => {
                // Decoder failures (malformed JSON, structural cycle)
                // should be surfaced — silent fallback to no-filter
                // would leak rows the caller intended to filter out.
                return reject_request(e);
            }
        };
    filter_by_predicate(rows, pred_opt.as_ref()).collect()
}
```

The decoder explicitly distinguishes "no header" (`None`) from "header present but malformed" (`Some(Err)`). Don't conflate them — the malformed case is a confidentiality concern in some workloads.

---

## 10. Service-side row filtering

Phase 5.B follow-on. `RpcPredicateContext` lets application rows expose their tags + metadata to the predicate evaluator.

```rust
struct TrainingJob {
    id: u64,
    tags: Vec<Tag>,
    metadata: BTreeMap<String, String>,
    payload: Vec<u8>,
}

impl RpcPredicateContext for TrainingJob {
    fn rpc_predicate_tags(&self) -> &[Tag] { &self.tags }
    fn rpc_predicate_metadata(&self) -> &BTreeMap<String, String> { &self.metadata }
}

// One-line filter:
let matched: Vec<TrainingJob> =
    filter_by_predicate(jobs, pred_opt.as_ref()).collect();
```

For row types that are themselves `CapabilitySet`-shaped, use `pred.matches_capability_set(&caps)` directly.

---

## 11. Index-driven discovery — `find_nodes_matching`

Phase 5.B follow-on. Bridges `CapabilityIndex` with `Predicate` so applications can do predicate-based discovery without building a `CapabilityFilter`.

```rust
let index = CapabilityIndex::new();
// ...populate from announcements...

let pred = Predicate::And(vec![
    Predicate::Exists { key: TagKey::new(TaxonomyAxis::Hardware, "gpu") },
    Predicate::MetadataEquals {
        key: "intent".into(),
        value: "ml-training".into(),
    },
]);

let matched: Vec<u64> = index.find_nodes_matching(&pred);
```

Linear scan over indexed nodes; each is evaluated against the predicate via the cardinality-aware planner (the same index's cardinality data drives clause ordering). For predicates that include axis-tag clauses already covered by `CapabilityIndex::query(&filter)`'s inverted-index pre-filtering, callers may want to combine both — but that requires `Predicate ↔ CapabilityFilter` translation, which isn't shipped yet.

---

## See also

- [`plans/CAPABILITY_ENHANCEMENTS_PLAN.md`](plans/CAPABILITY_ENHANCEMENTS_PLAN.md) — the canonical plan.
- [`CAPABILITIES_SCHEMA.md`](CAPABILITIES_SCHEMA.md) — authoritative key + value-type spec.
- [`plans/CAPABILITY_SYSTEM_PLAN.md`](plans/CAPABILITY_SYSTEM_PLAN.md) — substrate plan; the foundation.
- [`plans/CAPABILITY_SYSTEM_SDK_PLAN.md`](plans/CAPABILITY_SYSTEM_SDK_PLAN.md) — per-binding API surfaces (Phase 9).
- `tests/cross_lang_capability/` — golden-vector fixtures pinning wire format for binding consumers.
- `tests/cross_lang_capability_fixtures.rs` — Rust reference test loading the fixtures.
