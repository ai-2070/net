# SDK Capability System Surface Plan

Bring the Warriors-release capability work — typed taxonomy, tag shapes, metadata field, bloom-filter aggregation, federated query primitives, `PlacementFilter` + `StandardPlacement` + `IntentRegistry`, and the `MeshDaemon` capability-authoring extension — into the `net-sdk` Rust surface and through to the Node, Python, and Go bindings + their high-level SDKs (`sdk-ts`, `sdk-py`). Companion to [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md), which specifies the substrate-level contracts, and [`CAPABILITY_ENHANCEMENTS_PLAN.md`](CAPABILITY_ENHANCEMENTS_PLAN.md), which layers ergonomics / performance / DX on top of the substrate. This doc plans the user-facing wrapper layer above both.

The substrate plan's Phase H ("Bindings, 1 week, parallelisable") is the napi / pyo3 / cgo *plumbing* that exposes raw types across the FFI boundary. This plan adds the *ergonomic wrapper layer* on top — the typed builders, the language-idiomatic Predicate DSL, the FFI callback infrastructure for application-supplied `PlacementFilter` impls, the cross-binding compat test suite that pins the contract, and the per-binding surfaces for the enhancement-plan items (axis schemas, lazy projections, `CapabilitySet::diff`, chain helpers, predicate-in-nRPC, query planner exposure, debug record/replay).

## Status

**Design only; activation gate is the substrate plan.** Substrate Phase A is complete on `capability-system-2`; this plan ships in lockstep with `CAPABILITY_SYSTEM_PLAN.md`'s Phase H (bindings) and the dependent Mikoshi-integration phase G. The enhancement-plan items in this doc gate on their respective `CAPABILITY_ENHANCEMENTS_PLAN.md` phases (Phase 1 of that plan unblocks lazy projections + diff in this plan; Phase 5 there unblocks predicate-in-nRPC here; etc.). Independent of `REDEX_DISTRIBUTED_PLAN.md`.

## Goals

- One ergonomic surface per language for: building a `CapabilitySet` (tags + metadata), parsing/building `Tag` values, reading typed-struct projections (`HardwareCapabilities::from(&caps)` etc.), advertising / withdrawing chains (`announce_chain` / `withdraw_chain` / `find_chain_holders`), composing predicates, running federated queries, and consuming or supplying `PlacementFilter`.
- Parity across Node (NAPI + sdk-ts), Python (PyO3 + sdk-py), Go (CGO). Same behavior, same structured errors, same FFI callback shape for application-supplied filters.
- Typed `CapabilityError` surface on every language, with kind discrimination (`reserved-prefix` / `metadata-too-large` / `unknown-axis` / `predicate-malformed` / `placement-evaluation-failed`). Mirrors the `MigrationError` / `GroupError` pattern.
- Cross-binding compat fixtures pinning the wire format (`tags + metadata`), the bloom-filter encoding, and the predicate AST serialization — same shape as `tests/cross_lang_nrpc/golden_vectors.json` already used for nRPC.
- **Enhancement-layer parity** (per `CAPABILITY_ENHANCEMENTS_PLAN.md`): every binding ships axis schemas (auto-completion + validation), lazy memoized projections, `CapabilitySet::diff`, chain composition helpers (`requireChain` / `excludeChain` / etc.), predicate-AST embedding in nRPC request envelopes, opt-out for the query planner, and predicate debug session APIs.

## Non-goals

- **Changing core semantics.** The substrate's `CapabilitySet`, `Tag`, `Predicate`, `PlacementFilter`, `IntentRegistry`, `MeshDaemon` extensions stay exactly as `CAPABILITY_SYSTEM_PLAN.md` defines them. SDK code is wrap-and-forward, not redesign.
- **A user-facing query language.** Per the substrate plan's "What ships NOT": this plan exposes the predicate AST + composition operators, not a string-DSL parser. The string-DSL (Atomic Playboys territory) parks until a workload demands it.
- **Continuous rebalancing or auto-placement supervisors.** Same as the groups plan's Stage-N+1 deferral — the SDK exposes `PlacementFilter` invocation; it doesn't run a background "placement-aware rescheduler."
- **Encrypted-metadata transport.** Per substrate plan's deferral.
- **Public release wheels carrying every Capability feature.** Feature gating mirrors `compute` / `groups`: `capability-system-v2` (or similar; final name TBD with the substrate gate) ships behind a flag in the release workflows. Pilots opt in.

---

## What ships

Two layers of surface, both shipped per binding:

### Substrate-layer surface (gates on `CAPABILITY_SYSTEM_PLAN.md` Phase H)

1. **Typed-taxonomy + view-projection helpers** — `Tag`, `TagKey`, `TaxonomyAxis`, `CapabilitySet` (with `tags: HashSet<Tag>` + `metadata: BTreeMap`) exposed in every binding; `caps.views()` returning the five typed projections (lazy-evaluated per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 1).
2. **Tag-shape builders** — language-idiomatic constructors for `causal:<hex>`, `causal:<hex>:<tip_seq>`, `causal:<hex>[<start>..<end>]`, `fork-of:<parent_hex>`, `heat:<chain_hex>=<rate>`, `scope:<label>`. Reserved-prefix enforcement at the builder level; user-emitted tags with reserved prefixes return a typed error before the call hits the wire.
3. **Discovery primitives** — `Mesh::announce_chain` / `announce_chain_range` / `withdraw_chain` / `find_chain_holders` exposed across bindings + SDKs. Async where the underlying SDK is async (Rust, TS, Go contexts); sync with GIL-release where the SDK is sync (Python).
4. **Bloom-filter aggregation surface** — `CapabilitySet::chain_bloom` round-trips via serde; the bloom is opaque on the SDK surface (no application-level construction; it's auto-built by the substrate at the threshold). Surface-level, only diagnostic helpers (`is_bloom_active()`, `chain_bloom_stats()`).
5. **Federated query primitives** — the five operators (`filter`, `match_axis`, `traverse`, `aggregate`, `nearest`) callable from each language; the `Predicate` AST built via language-idiomatic builders that compile down to the same portable AST. Local-only by default; `Federated` opt-in wrapper for cross-node execution.
6. **`PlacementFilter` consumption + custom impls** — application code in any binding can either configure a `StandardPlacement` (recommended; no FFI callback overhead) or supply a custom `PlacementFilter` via the existing FFI-callback pattern (same trampolines as `BlobAdapter` and the migration factory model).
7. **`IntentRegistry::register`** — custom-intent registration callable from each language. Defaults ship from substrate; per-deployment overrides land via the SDK.
8. **`MeshDaemon::required_capabilities` / `optional_capabilities`** — daemon authors in any language declare requirements/optionals. The shape mirrors the existing `factory` callback infrastructure used for migration-target reconstruction.

### Enhancement-layer surface (gates on `CAPABILITY_ENHANCEMENTS_PLAN.md` per-phase)

9. **Axis schemas** — per-binding type definitions (Rust const-eval `AxisSchema`, TS `.d.ts`, Python TypedDict / Pydantic, Go codegen). Drives auto-completion, static type-checking, and runtime validation. Authoritative source: `net/crates/net/docs/CAPABILITIES_SCHEMA.md`; CI fails on drift.
10. **Lazy memoized projection handles** — `caps.views()` returns a borrowing handle whose fields are computed-and-cached on first access. Hot-path `caps.views().hardware.X` < 50 ns post-cache. Same API surface; pure performance upgrade.
11. **`CapabilitySet::diff(prev)`** — `{added_tags, removed_tags, changed_metadata}` change-detection across all four bindings. Powers event-driven placement, capability-change dashboards, delta-based metadata propagation.
12. **Chain composition helpers** — `caps.requireChain(hash, opts?)`, `caps.requireAnyChain([hashes])`, `caps.excludeChain(hash)`, `caps.fromFork(parent)`, `caps.heatLevel(rate)` syntactic sugar over the underlying reserved-prefix tags. Predicate-side equivalents on the `pred.*` builder.
13. **Predicate AST in nRPC request filters** — `mesh.call(svc, { where: pred.X })` ships the predicate AST as part of the request envelope; services that handle the `where` field get predicate pushdown. Same AST powering capability queries; same fluent builders per binding.
14. **Query-planner opt-out** — `predicate.evaluate()` runs the planned (selectivity-reordered) AST by default; `predicate.evaluate_unplanned()` exposes the raw declaration-order path for benchmarking / debugging.
15. **Predicate debug sessions** — `mesh.debugPredicate(pred).run()` returns a per-clause hit/miss/cost report; `.recordTo(path)` saves a session for offline replay; `PredicateDebugReport.fromFile(path).print()` analyzes outside the recording process. Per-binding redaction helpers for sensitive metadata before persisting.

What this doc does NOT ship (deferred):

- **String-form predicate DSL.** Per `CAPABILITY_SYSTEM_PLAN.md`'s "What ships NOT" — the AST + composition operators ship; the parsed string DSL waits.
- **Cross-binding `Aggregator` registration.** The trait + concrete impls (`CountAggregator`, `SumAggregator`, `MaxAggregator`) ship in Rust core (substrate plan); only the *consumption* surface ships in this SDK plan. A custom-aggregator-via-callback pattern would mirror `PlacementFilter`'s trampolines but is deferred until a workload asks for it.
- **Live capability-watch streams.** `find_nodes` returns a snapshot; subscribing to capability-change events via a channel is a follow-up if a real consumer surfaces.
- **Schema-driven codegen for capability builders.** Axis schemas drive type-checking + validation; auto-generating `caps.requireHardwareGpu(vramGb)` etc. from the schema is a future SDK extension if it earns its keep.
- **String-form predicate parsing in nRPC `where` envelopes.** The AST crosses the wire as postcard bytes; a future enhancement may layer a string-DSL parser on top of the same AST. Not in scope for the Warriors release.

---

## The substrate surface (context)

(Defined fully in [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md). Brief recap so this plan reads standalone.)

### `CapabilitySet`, `Tag`, `metadata`

Wire format: only `tags: HashSet<Tag>` + `metadata: BTreeMap<String, String>`. Typed-struct shapes (`HardwareCapabilities` / `SoftwareCapabilities` / `ResourceLimits` etc.) are local view projections, not stored or propagated. Same pattern Kubernetes uses for typed constraint helpers over labels.

### Predicate language

Pure AST (`Predicate` enum: 13 variants) + the five federated query operators (`filter`, `match_axis`, `traverse`, `aggregate`, `nearest`). The `pred!` macro in Rust is parse-time sugar producing `Predicate` AST; cross-binding builders ship in this SDK plan.

### `PlacementFilter` + `StandardPlacement`

Substrate-level placement primitive. Trait method `placement_score(target, &Artifact) -> Option<f32>`; multiplicative composition across axes (`0.0` anywhere → `0.0` final per the locked rule); deterministic three-step tie-breaking (RTT → free-resource → lexicographic NodeId).

### `Mesh` discovery primitives

`Mesh::announce_chain(origin_hash, tip_seq)`, `announce_chain_range(origin_hash, range)`, `withdraw_chain(origin_hash)`, `find_chain_holders(origin_hash) -> Vec<NodeId>` — wraps the existing capability-index query, nearest-first by proximity.

---

## SDK design decisions

### 1. View projections are SDK-owned lazy handles, not napi/pyo3-class-shaped

The substrate ships `HardwareCapabilities::from(&CapabilitySet)` etc. as Rust functions. Exposing those as napi/pyo3 classes (with constructors taking `&CapabilitySet`) would require five new class declarations × three bindings = 15 boilerplate FFI surfaces.

**Decision:** the binding-side `CapabilitySet` exposes a `views` accessor returning a **lazy handle** whose five projection fields are computed and cached on first access (per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 1):

```rust
// Rust core (substrate)
pub struct CapabilityViews<'a> {
    caps: &'a CapabilitySet,
    sorted_tags: OnceCell<Vec<Tag>>,
    hardware: OnceCell<HardwareCapabilities>,
    software: OnceCell<SoftwareCapabilities>,
    resource_limits: OnceCell<ResourceLimits>,
    models: OnceCell<Vec<ModelCapability>>,
    tools: OnceCell<Vec<ToolCapability>>,
}
impl CapabilitySet {
    pub fn views(&self) -> CapabilityViews<'_> { /* borrowing handle, fields lazy */ }
}
impl CapabilityViews<'_> {
    pub fn hardware(&self) -> &HardwareCapabilities { /* OnceCell-cached */ }
    // ...software / resource_limits / models / tools accessors
}
```

```typescript
// sdk-ts — accessor properties; first read decodes hardware tags, second hits cache
const caps = mesh.localCapabilities()
const v = caps.views()
console.log(v.hardware.gpu?.vramGb)   // first read: decode hardware tags
console.log(v.hardware.memoryGb)       // cached; no re-decode
console.log(v.models.length)           // first read: decode model.* tags
```

```python
# sdk-py — same lazy-property semantics
caps = mesh.local_capabilities()
v = caps.views()
print(v.hardware.gpu.vram_gb if v.hardware.gpu else None)
print(v.hardware.memory_gb)            # cached
print(len(v.models))                   # first read: decode model.* tags
```

```go
// Go binding — accessor methods on the views handle
caps, _ := mesh.LocalCapabilities()
v := caps.Views()
if v.Hardware().GPU != nil {            // first call: decode hardware tags
    fmt.Println(v.Hardware().GPU.VRAMMB) // cached
}
fmt.Println(len(v.Models()))             // first call: decode model.* tags
```

One method per binding (`views()`); each projection is decoded at most once per handle lifetime. Hot-path code that reads only one or two projections (e.g. `caps.views().hardware`) doesn't pay for the others. Performance contract: hot-path `caps.views().hardware.X` < 50 ns after first access (vs. ~5 µs eager); pinned in Criterion bench across all bindings. The lazy implementation is observably equivalent to eager for any callsite — pure performance optimization, no API change.

### 2. No backward-compat shim — the substrate broke wire format intentionally

Earlier drafts of this plan called for a per-binding deprecation shim preserving `caps.hardware.gpu.vram_gb` field access for one minor version. That plan assumed the substrate's typed-struct removal would happen in lockstep with a soft binding migration window.

**Decision:** the substrate's Phase A.5.N.3 removed the typed-struct fields outright (no compat layer; old peers can't decode new announcements). Per `CAPABILITY_ENHANCEMENTS_PLAN.md`'s "eternal rule" and the locked "no backwards compatibility" decision in the substrate plan, this SDK plan ships **no deprecation shim**. The migration is binary:

- Pre-A.5.N.3 callers (`caps.hardware.X`) won't compile in Rust / fail to type-check in TS / `AttributeError` in Python / fail to compile in Go.
- Post-A.5.N.3 callers (`caps.views().hardware.X`) work everywhere.
- Migration is mechanical: codemod each `caps.<axis>.<field>` → `caps.views().<axis>.<field>` (or `.X()` accessor in Go). No release-window timing concerns.

Operators upgrade peers in lockstep — same constraint already imposed by the substrate's wire-format break. Cross-binding compat tests assert the **post-migration** API across all four bindings; there is no pre-migration codepath to test.

### 3. Predicate DSL: language-idiomatic builders → portable AST

The substrate ships the `Predicate` enum + the `pred!` macro. Other languages need ergonomic equivalents that produce the same AST (which crosses the FFI boundary as serde-encoded bytes for federated execution).

**Decision:** each binding ships a fluent builder that mirrors the AST shape:

```typescript
// sdk-ts — fluent / chainable
import { p } from '@net-mesh/core/capability'

const pred = p.and(
  p.exists('hardware.gpu'),
  p.numericAtLeast('hardware.gpu.vram_gb', 24),
  p.semverCompatible('software.runtime', '12.0'),
  p.metadataEquals('intent', 'ml-training'),
)
const matches = await mesh.findNodes(pred)
```

```python
# sdk-py — fluent / chainable; same shape
from net_sdk.capability import p

pred = p.and_(
    p.exists("hardware.gpu"),
    p.numeric_at_least("hardware.gpu.vram_gb", 24),
    p.semver_compatible("software.runtime", "12.0"),
    p.metadata_equals("intent", "ml-training"),
)
matches = mesh.find_nodes(pred)
```

```go
// Go binding — function-call composition
import "net/capability/pred"

p := pred.And(
    pred.Exists("hardware.gpu"),
    pred.NumericAtLeast("hardware.gpu.vram_gb", 24),
    pred.SemverCompatible("software.runtime", "12.0"),
    pred.MetadataEquals("intent", "ml-training"),
)
matches, _ := mesh.FindNodes(ctx, p)
```

```rust
// net-sdk — pred! macro from substrate, re-exported
use net_sdk::capability::pred;

let p = pred!("hardware.gpu" && "hardware.gpu.vram_gb" >= 24
              && semver "software.runtime" ~= "12.0"
              && metadata "intent" == "ml-training");
let matches = mesh.find_nodes(&p).await?;
```

All four builders produce the same `Predicate` AST. The AST serializes via serde (postcard for cross-binding wire format, JSON for sdk-ts↔sdk-py debugging fixtures) and round-trips deterministically. **Pin the round-trip in a cross-binding compat test** (same pattern as nRPC's golden vectors): a fixture file with N predicates expressed as JSON; each binding decodes, evaluates against a stub `CapabilitySet`, and asserts the result matches.

**Cross-service usage (per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 5).** The same `Predicate` AST + same `p.*` builders also drive nRPC request filters. Every binding's `mesh.call(svc, opts)` accepts an optional `where: Predicate` field that rides through nRPC's existing payload typing as postcard-encoded bytes. Services that handle the field get predicate pushdown:

```typescript
// sdk-ts — same `p.*` builder, used as an nRPC filter
const jobs = await mesh.call('ScanTrainingJobs', {
  where: p.and(
    p.exists('hardware.gpu'),
    p.numericAtLeast('hardware.gpu.vram_gb', 48),
    p.metadataEquals('intent', 'ml-training'),
  ),
})
```

```python
# sdk-py — identical predicate AST, identical wire bytes
jobs = mesh.call("ScanTrainingJobs", where=p.and_(
    p.exists("hardware.gpu"),
    p.numeric_at_least("hardware.gpu.vram_gb", 48),
    p.metadata_equals("intent", "ml-training"),
))
```

One canonical filter language across capability queries AND workload queries. Services that don't handle the `where` field ignore it; predicate pushdown is opt-in per service. Cross-binding compat fixture (`tests/cross_lang_capability/predicate_nrpc_envelope.json`) pins the postcard-encoded shape so a Node client and a Go service interop without surprise.

**Query planner (per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 4).** Each binding's `predicate.evaluate(ctx)` runs the planned (selectivity-reordered) AST by default. `predicate.evaluateUnplanned(ctx)` (camelCase / snake_case / Go-style per binding) exposes the raw declaration-order path for benchmarking or debugging. Property test pinned across bindings: `evaluate(plan(ast), ctx) == evaluate(ast, ctx)` for any `(ast, ctx)`.

### 4. `PlacementFilter`: configure `StandardPlacement` by default, FFI callback for custom

Two paths, with the configuration-driven path strongly preferred for the same FFI-overhead reasons the groups plan called out:

**Path A — `StandardPlacement` builder (recommended).** Application configures one of the existing axes via a builder; no FFI callbacks. Per-call cost: zero crossings.

```typescript
const placement = new StandardPlacement({
  scopeFilter: ['prod', 'us-east'],
  proximityMaxRtt: 50,                       // ms
  intentMatch: { strict: intentRegistry },
  colocationPolicy: 'soft-preference',
  resourceAxis: 'compute',
  antiAffinity: { leadershipConcentrationThreshold: 0.30, leadershipConcentrationPenalty: 0.4 },
})
const target = await mikoshi.selectMigrationTarget(daemon, placement)
```

**Path B — Custom `PlacementFilter` via FFI callback (advanced).** Application supplies a filter function via the binding's `placementFilterFromFn` builder, which marshals candidate `(NodeId, Artifact, CapabilitySet)` tuples across the FFI boundary, invokes the application callback, returns a score. Same trampoline pattern as `BlobAdapter` and the migration factory.

```typescript
const placement = placementFilterFromFn((nodeId, artifact, caps) => {
  // Pure JS scoring logic.
  if (caps.tags.has('decommissioning')) return null  // veto
  return Math.min(1.0, caps.metadata.reliabilityScore ?? 0.5)
})
```

**Decision:** the SDK documents Path A as the default and Path B as the escape hatch. A counter metric (`dataforts_placement_callback_invocations_total{binding}`) tracks Path-B usage in production so we can spot when applications are paying the FFI tax avoidably.

### 5. `IntentRegistry::register` ergonomics

The substrate ships `IntentRegistry::defaults()` with a baseline mapping. Per-deployment customization happens via `register()`. Bindings expose this directly:

```typescript
import { IntentRegistry, requireTag, requireAxisValue } from '@net-mesh/core/capability'

const registry = IntentRegistry.defaults()
registry.register('quantum-research', [
  requireTag('hardware.qpu'),
  requireAxisValue('software', 'simulator'),
])
const placement = new StandardPlacement({ intentMatch: { strict: registry } })
```

The `requireTag` / `requireAxisValue` helpers in each binding produce `RequiredCapability` values matching the substrate's macro output (`require!`, `require_axis!`, `require_axis_value!`).

### 6. `MeshDaemon::required_capabilities` authoring

Daemons in non-Rust bindings already ride through factory callbacks; extending them for capability declaration uses the same trampoline:

```python
# sdk-py
from net_sdk.compute import MeshDaemon, CapabilitySetBuilder

class InferenceDaemon(MeshDaemon):
    def required_capabilities(self) -> CapabilitySet:
        return (CapabilitySetBuilder.new()
                .require_tag("hardware.gpu")
                .require_tag("hardware.gpu.vram_gb >= 80")
                .with_metadata("intent", "ml-training")
                .build())

    def optional_capabilities(self) -> CapabilitySet:
        return (CapabilitySetBuilder.new()
                .with_tag("hardware.gpu.architecture:hopper")
                .build())

    def process(self, event):
        ...
```

The factory callback that builds the daemon already crosses the FFI boundary; adding two more methods to the daemon trait is mechanical. **The new methods have empty defaults** so existing daemon implementations keep working without overrides.

### 7. Discovery primitives stay on `Mesh`, not on a new `ChainHolder` class

The substrate exposes `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` as `Mesh` methods. SDK surfaces match: each binding's existing `Mesh` / `MeshNode` class gains the four methods. No new top-level class.

```typescript
// sdk-ts
await mesh.announceChain(originHash, tipSeq)
await mesh.announceChainRange(originHash, { start: 0n, end: 1024n })
await mesh.withdrawChain(originHash)
const holders = await mesh.findChainHolders(originHash)  // sorted nearest-first
```

```python
# sdk-py
mesh.announce_chain(origin_hash, tip_seq)
mesh.announce_chain_range(origin_hash, start=0, end=1024)
mesh.withdraw_chain(origin_hash)
holders = mesh.find_chain_holders(origin_hash)  # sorted nearest-first
```

```go
// Go
mesh.AnnounceChain(ctx, originHash, tipSeq)
mesh.AnnounceChainRange(ctx, originHash, 0, 1024)
mesh.WithdrawChain(ctx, originHash)
holders, _ := mesh.FindChainHolders(ctx, originHash)
```

```rust
// net-sdk
mesh.announce_chain(origin_hash, tip_seq).await?;
mesh.announce_chain_range(origin_hash, 0..1024).await?;
mesh.withdraw_chain(origin_hash).await?;
let holders = mesh.find_chain_holders(origin_hash).await?;
```

### 8. `chain_bloom` is opaque at the SDK surface

The bloom filter aggregates many `causal:` tags; the substrate auto-builds it at the 256-tag threshold. SDK applications should not construct or inspect the bloom directly — false-positive handling is the substrate's job (probe-then-precise pattern).

**Decision:** SDKs expose only diagnostic helpers:

```typescript
const stats = caps.chainBloomStats()  // { active, sizeBytes, fpr, tagCount }
const isActive = caps.isBloomActive()
```

`chainBloomStats()` returns a struct/dict of pure data; no bloom-filter internals leak across the FFI boundary.

### 9. Cross-binding compat fixtures

Pin the wire format of the new metadata field, the bloom-filter encoding, and the predicate AST serialization in golden-vector fixtures consumed by every binding's compat test. Same pattern as `tests/cross_lang_nrpc/golden_vectors.json`.

Five fixtures land alongside the substrate's Phase A + the enhancement plan's Phase 5:

- `tests/cross_lang_capability/metadata_round_trip.json` — `(tags, metadata)` round-trips byte-for-byte across all four bindings.
- `tests/cross_lang_capability/predicate_ast.json` — N predicates as JSON; each binding decodes, evaluates against a stub `CapabilitySet` table, and asserts results match.
- `tests/cross_lang_capability/placement_score.json` — N `(StandardPlacement-config, candidate-set)` pairs with expected scores. Each binding evaluates and asserts the score matrix matches Rust's reference output to within `1e-6`.
- `tests/cross_lang_capability/predicate_nrpc_envelope.json` — N nRPC request envelopes carrying `where: Predicate` payloads as postcard-encoded bytes. Each binding decodes, evaluates against a stub row stream, and asserts the resulting filtered row IDs match.
- `tests/cross_lang_capability/capability_set_diff.json` — N `(prev, curr)` `CapabilitySet` pairs with expected `{added_tags, removed_tags, changed_metadata}` outputs. Each binding's `caps.diff(prev)` must produce identical results.

The fixtures land BEFORE Phase H of the substrate ships, so binding work has a contract to test against from day one.

### 10. Axis schemas as binding-local typing layer

Per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 2: a per-binding declarative schema describing the four-axis shape (`hardware.cpu.cores: number`, `hardware.gpu.vram_gb: number`, `software.runtime: indexed { name: string, version: string }`, etc.). The schema is **purely local** — wire format stays opaque; the schema drives auto-completion, static type-checking, and runtime validation per binding.

**Decision:** the canonical schema lives at `net/crates/net/docs/CAPABILITIES_SCHEMA.md` (new). Each binding regenerates from this canonical doc on every release; CI guard fails the build if any binding's generated schema diverges from the canonical.

```rust
// Rust — const-eval'd schema in net_sdk::capability::schema
const AXIS_SCHEMA: AxisSchema = AxisSchema {
    hardware: HardwareSchema { /* ... */ },
    software: SoftwareSchema { /* ... */ },
    devices: DevicesSchema { /* ... */ },
    dataforts: DatafortsSchema { /* ... */ },
};
```

```typescript
// sdk-ts — `.d.ts` types over `caps.views()` output; tsc enforces at build time
import type { HardwareView, SoftwareView } from '@net-mesh/core/capability/schema'
const v = caps.views()
const vram: number | null = v.hardware.gpu?.vramGb ?? null   // typed
```

```python
# sdk-py — Pydantic models for views(); runtime type-check at IDE / mypy boundary
from net_sdk.capability.schema import HardwareView, SoftwareView
v = caps.views()
vram: int | None = v.hardware.gpu.vram_gb if v.hardware.gpu else None
```

```go
// Go binding — code-generated structs from the YAML schema spec
v := caps.Views()
vram := 0
if gpu := v.Hardware().GPU; gpu != nil {
    vram = gpu.VRAMGB
}
```

**Validation API:** each binding ships `validate_capabilities(caps, schema?) -> ValidationReport` that flags out-of-axis keys (`SchemaError::UnknownAxis`), type mismatches (`SchemaError::TypeMismatch`), and value-range violations (`SchemaError::ValueOutOfRange`). Schema parameter optional — defaults to the binding's bundled canonical schema. Forward-compat: unknown keys under known axes pass with a warning, not a hard error (matches the substrate's forward-compat decoder).

Schemas do NOT change wire format; do NOT enforce on incoming peer data; do NOT version separately from the substrate. Schema bumps are binding-version concerns.

### 11. `CapabilitySet::diff` ergonomics

Per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 1: a cheap before/after change-detection method emitting `{added_tags, removed_tags, changed_metadata}`.

**Decision:** every binding ships `caps.diff(prev)` returning a structured diff with three collections. `MetadataChange` is an enum/tagged-union with `Added` / `Removed` / `Updated` variants.

```rust
// Rust SDK
let diff = caps.diff(&prev_caps);
for tag in &diff.added_tags { /* ... */ }
for change in &diff.changed_metadata {
    match change {
        MetadataChange::Added { key, value } => { /* ... */ }
        MetadataChange::Removed { key, prev_value } => { /* ... */ }
        MetadataChange::Updated { key, prev_value, new_value } => { /* ... */ }
    }
}
```

```typescript
// sdk-ts
const diff = caps.diff(prevCaps)
diff.addedTags.forEach((t) => { /* ... */ })
diff.changedMetadata.forEach((c) => {
    if (c.kind === 'added')   { /* c.key, c.value */ }
    if (c.kind === 'removed') { /* c.key, c.prevValue */ }
    if (c.kind === 'updated') { /* c.key, c.prevValue, c.newValue */ }
})
```

```python
# sdk-py
diff = caps.diff(prev_caps)
for tag in diff.added_tags: ...
for change in diff.changed_metadata:
    match change:
        case MetadataChange.Added(key, value): ...
        case MetadataChange.Removed(key, prev_value): ...
        case MetadataChange.Updated(key, prev_value, new_value): ...
```

```go
// Go
diff := caps.Diff(prevCaps)
for _, tag := range diff.AddedTags { /* ... */ }
for _, change := range diff.ChangedMetadata {
    switch c := change.(type) {
    case MetadataAdded:    /* c.Key, c.Value */
    case MetadataRemoved:  /* c.Key, c.PrevValue */
    case MetadataUpdated:  /* c.Key, c.PrevValue, c.NewValue */
    }
}
```

Cross-binding compat fixture (`capability_set_diff.json`) pins the diff output for N `(prev, curr)` pairs. Composes with `DiffEngine` from substrate `behavior::diff` — `apply_diff(prev, prev.diff(curr)) == curr` round-trip pinned in property tests.

### 12. Chain composition helpers

Per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 3: syntactic sugar over the underlying `causal:` / `fork-of:` / `heat:` reserved-prefix tags.

**Decision:** five helpers per binding on both the `CapabilitySet` builder and the `pred.*` predicate builder. Pure sugar — each helper is one or two lines of binding code that delegates to the existing reserved-prefix tag emission / predicate construction.

```typescript
// sdk-ts — caps-side helpers
caps = caps.requireChain(originHash)
caps = caps.requireChain(originHash, { minSeq: 100, maxSeq: 200 })
caps = caps.requireAnyChain([hashA, hashB])
caps = caps.excludeChain(hashC)
caps = caps.fromFork(parentHash)
caps = caps.heatLevel(0.85)

// pred-side helpers
const filter = p.and(
    p.requireChain(originHash),
    p.minSeq('hardware.foo.seq', 100),
    p.excludeChain(blocklistHash),
)
```

```python
# sdk-py — same shapes
caps = caps.require_chain(origin_hash)
caps = caps.require_chain(origin_hash, min_seq=100, max_seq=200)
caps = caps.require_any_chain([hash_a, hash_b])
caps = caps.exclude_chain(hash_c)
caps = caps.from_fork(parent_hash)
caps = caps.heat_level(0.85)
```

```go
// Go — function-call composition
caps = caps.RequireChain(originHash)
caps = caps.RequireChainRange(originHash, 100, 200)
caps = caps.RequireAnyChain([]string{hashA, hashB})
caps = caps.ExcludeChain(hashC)
caps = caps.FromFork(parentHash)
caps = caps.HeatLevel(0.85)
```

```rust
// Rust SDK
let caps = caps
    .require_chain(origin_hash)
    .require_chain_range(origin_hash, 100..200)
    .require_any_chain([hash_a, hash_b])
    .exclude_chain(hash_c)
    .from_fork(parent_hash)
    .heat_level(0.85);
```

Cross-binding compat fixture (`predicate_ast.json`) gains additional entries exercising the chain helpers — every binding's `caps.requireChain(h)` produces an identical `Tag::Reserved { prefix: "causal:", body: h }` value.

### 13. Predicate debug sessions

Per `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 6: `mesh.debugPredicate(pred).run()` returns a `PredicateDebugReport` with per-clause hit/miss/cost stats.

**Decision:** each binding ships a `mesh.debugPredicate(pred)` (or snake_case / Go-style equivalent) that returns a session builder. The session evaluates the predicate against a target source (capability index, nRPC service, or a synthetic candidate set), records per-clause stats, and returns a structured report.

```typescript
// sdk-ts
const report = await mesh.debugPredicate(pred)
    .againstCapabilityIndex()
    .run()
report.print()                           // human-readable per-clause breakdown
report.recordTo('session.json')          // persist for offline replay
report.redactMetadataKeys(['tenant-id']) // strip sensitive values before save
```

```python
# sdk-py
report = mesh.debug_predicate(pred).against_capability_index().run()
report.print()
report.record_to("session.json")
report.redact_metadata_keys(["tenant-id"])
```

```go
// Go
report, _ := mesh.DebugPredicate(pred).AgainstCapabilityIndex().Run(ctx)
report.Print()
report.RecordTo("session.json")
report.RedactMetadataKeys([]string{"tenant-id"})
```

**Replay** — `PredicateDebugReport.fromFile(path)` (or equivalent) loads a recorded session for offline analysis. Same `print()` / `redactMetadataKeys()` API.

**Per-clause cost** — bindings expose timing in their host language's idiomatic unit (ms in TS / Python / Go; `Duration` in Rust). The underlying substrate timer is monotonic-ns; bindings convert.

**~5% overhead** on the predicate evaluation path when debug-record mode is active. Opt-in only — production hot paths don't pay the cost. Opt-out enforced at the binding layer (each binding's standard `mesh.findNodes(pred)` does NOT instrument unless `mesh.debugPredicate(pred)` is explicitly called).

---

## Phasing

Phasing is layered: substrate-track phases (1–8) ship the wrapper for `CAPABILITY_SYSTEM_PLAN.md`'s Phase H, plus an enhancement-track phase (9) that bundles the per-binding surfaces for the items in `CAPABILITY_ENHANCEMENTS_PLAN.md`. Phases parallelise within each language after the substrate's Phase A has landed (it has, on `capability-system-2`).

### Phase 1 — Rust SDK (`net-sdk`) surface (1 week)

- Re-export `Tag`, `TagKey`, `Predicate`, `PlacementFilter`, `StandardPlacement`, `IntentRegistry`, `RequiredCapability`, `Aggregator` trait under `net_sdk::capability` and `net_sdk::placement`.
- `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` (and the range variant) added as async methods on `net_sdk::mesh::Mesh`.
- `CapabilitySet::views()` returning the lazy handle (cached on first per-projection access).
- `pred!` macro re-exported from substrate.
- `CapabilitySet::diff` re-exported (lands as part of `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 1).
- Chain composition helpers (`require_chain` / `require_any_chain` / `exclude_chain` / `from_fork` / `heat_level`) on the SDK builder; predicate-side equivalents on `pred::*`.
- Documentation: `net-sdk/README.md` adds a "Capability System" section with the full set of examples.

### Phase 2 — Cross-binding compat fixtures (3 days, lands BEFORE Phase 3-5)

- Five fixtures defined in [§9](#9-cross-binding-compat-fixtures). Generated from a `cargo run --example gen_capability_fixtures` binary so they regenerate deterministically on schema changes.
- The two enhancement-driven fixtures (`predicate_nrpc_envelope.json` and `capability_set_diff.json`) land in this phase as stubs and get populated in Phase 9 once the corresponding enhancement-plan substrate work is in place.
- Documented in `tests/cross_lang_capability/README.md`.

### Phase 3 — Node binding + sdk-ts (1 week, parallelises with 4 + 5)

- `bindings/node/src/capability.rs`: napi class wrappers for `CapabilitySet`, `Tag`, `Predicate`, `StandardPlacement`, `IntentRegistry`. Binding-level reserved-prefix enforcement.
- `bindings/node/capability.ts` typed wrappers exporting the fluent `p.*` builder, `requireTag` / `requireAxisValue`, the `StandardPlacement` builder, `placementFilterFromFn` callback factory.
- `caps.views()` exposed as a lazy handle (per-property accessors back the OnceCell-cached projections).
- `caps.diff(prev)` exposed.
- Chain composition helpers exposed on `CapabilitySet` builder + `p.*` predicate builder.
- Axis schemas: `.d.ts` types over `views()` output, generated from the canonical schema doc.
- `sdk-ts/src/capability.ts` with the user-facing surface (re-exporting from binding + adding higher-level helpers).
- Tests against the fixtures from Phase 2.

### Phase 4 — Python binding + sdk-py (1 week, parallelises with 3 + 5)

- `bindings/python/src/capability.rs`: PyO3 class wrappers (parallel to Node).
- `bindings/python/python/net/capability.py` with the fluent `p.*` builder using snake_case methods (`p.numeric_at_least` etc.); `RequiredCapability` builders; `StandardPlacement` builder; `placement_filter_from_fn`.
- `caps.views()` lazy handle with property accessors.
- `caps.diff(prev)` exposed.
- Chain composition helpers on `CapabilitySet` builder + `p.*` predicate builder.
- Axis schemas: TypedDict / Pydantic models over `views()` output.
- `sdk-py/src/net_sdk/capability.py` with the user-facing surface.
- Tests against fixtures.

### Phase 5 — Go binding (1 week, parallelises with 3 + 4)

- `bindings/go/capability-ffi/`: cgo C-ABI for `CapabilitySet`, `Tag`, `Predicate` round-trip, `StandardPlacement` config, `IntentRegistry`. Predicate AST and `StandardPlacement` config cross the FFI boundary as postcard-encoded bytes (mirrors `compute-ffi`'s Daemon-snapshot pattern).
- `bindings/go/net/capability.go` with the user-facing surface: `pred.*` package providing `pred.And`, `pred.Exists`, etc.; `StandardPlacement` builder; `PlacementFilterFromFn` callback factory.
- `bindings/go/net/capability_views.go` with code-generated view-projection structs from the canonical schema doc.
- `caps.Views()` accessor methods backed by per-projection `sync.Once` caching.
- `caps.Diff(prev)` exposed.
- Chain composition helpers exposed on `CapabilitySet` builder + `pred` package.
- Tests against fixtures.

### Phase 6 — `MeshDaemon` capability authoring (3 days per binding, parallelisable)

- Add `requiredCapabilities()` / `optionalCapabilities()` to the `MeshDaemon` factory-callback contract in each binding (Node / Python / Go). Daemon authors override; default is empty.
- The factory trampoline already resolves `kind → factory`; add two more callbacks per kind for the capability methods. Reuse the existing factory infrastructure — no new dispatcher.
- Tests: a daemon declaring `hardware.gpu` requirement is migrated only to GPU nodes (depends on substrate Phase G).

### Phase 7 — Custom `PlacementFilter` callback (3 days per binding, parallelisable)

- Implement the `placement_filter_from_fn` factory in each binding using the existing trampoline pattern (Node TSFN, Python `Python::attach`, Go `cgo.Handle`).
- Counter metric `dataforts_placement_callback_invocations_total{binding}`.
- Cross-binding compat test: the same callback (encoded as a deterministic scoring function from a configuration JSON) produces the same score sequence across all three bindings against the placement-score fixture.

### Phase 8 — Documentation + migration guides (3 days, parallelisable)

- Per-binding README sections demonstrating the full surface: building a `CapabilitySet`, parsing `Tag`s, advertising chains, running predicate queries, configuring `StandardPlacement`, registering custom intents, declaring daemon capabilities, using chain composition helpers, reading lazy projections, computing diffs.
- A `CAPABILITY_SYSTEM_MIGRATION.md` guide covering the field-access → `views()` migration codemod patterns (`caps.hardware.X` → `caps.views().hardware.X` / `caps.Views().Hardware().X`).

### Phase 9 — Enhancement-track bindings (gated per-enhancement; parallelisable)

Bundles per-binding surfaces for the enhancement-plan items that gate on substrate-side enhancement work landing. Lands incrementally as each of `CAPABILITY_ENHANCEMENTS_PLAN.md`'s phases ships.

**9a. Axis schemas (1 week per binding, parallelisable)** — gates on `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 2.

- Canonical `net/crates/net/docs/CAPABILITIES_SCHEMA.md` authored.
- Rust: `net_sdk::capability::schema::AXIS_SCHEMA` const.
- Node: `.d.ts` types + `validateCapabilities(caps)` function.
- Python: TypedDict / Pydantic models + `validate_capabilities(caps)`.
- Go: codegen tool reading the YAML schema spec; `validate_capabilities(caps)`.
- CI guard: build fails if any binding's regenerated schema diverges from the canonical doc.

**9b. Predicate AST in nRPC `where` envelopes (3 days per binding, parallelisable)** — gates on `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 5.

- Each binding's `mesh.call(svc, opts)` accepts an optional `where: Predicate` field.
- AST serializes as postcard bytes inside the nRPC request envelope; substrate routing untouched.
- Cross-binding compat fixture (`predicate_nrpc_envelope.json`) populated with N envelopes; each binding round-trips bytes-identically.
- Documentation: predicate-pushdown patterns for service authors who want to honor the field.

**9c. Predicate query planner opt-out (1 day per binding, parallelisable)** — gates on `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 4.

- `predicate.evaluate(ctx)` runs planned by default; `predicate.evaluateUnplanned(ctx)` exposed for benchmarks.
- Property test pinned across bindings: `evaluate(plan(ast), ctx) == evaluate(ast, ctx)` for fuzz-generated `(ast, ctx)`.

**9d. Predicate debug session API (3 days per binding, parallelisable)** — gates on `CAPABILITY_ENHANCEMENTS_PLAN.md` Phase 6.

- `mesh.debugPredicate(pred).run()` returns a `PredicateDebugReport`.
- `.recordTo(path)` / `PredicateDebugReport.fromFile(path)` for save/replay.
- `.redactMetadataKeys(keys)` to scrub sensitive metadata before persisting.
- ~5% overhead on instrumented eval path; opt-in only.

**Total: 4–6 weeks.** Phase 1 + Phase 2 are sequential preconditions (~10 days). Phases 3–5 parallelise (1 week, 3 engineers, or 3 weeks serial). Phases 6–7 parallelise within each binding (~3 days each). Phase 8 parallelises with the others. Phase 9 lands incrementally as each enhancement plan phase fires; full Phase 9 across all bindings is ~3 weeks parallel / ~1 week serial per item per binding. Single engineer on the SDK serialises to ~6 weeks; three engineers parallel-process to ~4.

---

## Test strategy

### Unit (per binding)

- `Tag::parse` round-trips for each axis-prefixed shape; reserved-prefix rejection at the builder level returns the typed error.
- `Predicate` AST construction via the fluent builder for each variant; serialization round-trips.
- `StandardPlacement` configuration: each field accepts the documented value range; invalid values return typed errors.
- `IntentRegistry::register` accepts the `RequiredCapability` values produced by the binding's helpers; defaults match the substrate.
- **Lazy view projections (Phase 1 of enhancements).** `caps.views().hardware` returns hardware projection on first read; second read hits cache (instrument `*_from_tags` callsites with a counter; assert one decode per axis per handle). Reading `views().hardware` does NOT force `software` / `models` / `tools` / `limits` decoders.
- **`CapabilitySet::diff`.** Empty-vs-empty produces empty diff; X-vs-empty produces full added; metadata key-renames correctly reported as Removed+Added (not as Updated, since key identity changed). Round-trip with `DiffEngine`: `apply_diff(prev, prev.diff(curr)) == curr`.
- **Chain composition helpers.** `caps.requireChain(h)` produces `Tag::Reserved { prefix: "causal:", body: h }`; `caps.requireChain(h, {min, max})` produces the indexed-range form. Predicate-side `pred.requireChain(h)` evaluates to the same boolean as `pred.exists(format!("causal:{h}"))`.
- **Axis schemas (Phase 9a).** `validate_capabilities(caps)` flags out-of-axis keys with `SchemaError::UnknownAxis`; type mismatches with `SchemaError::TypeMismatch`. Forward-compat: unknown keys under known axes pass with a warning.
- **Predicate AST in nRPC `where` envelope (Phase 9b).** Round-trip serialize/deserialize across postcard boundary; identical predicate AST decoded on every binding from a shared fixture.
- **Query planner opt-out (Phase 9c).** `predicate.evaluate_unplanned(ctx)` produces identical results to `predicate.evaluate(ctx)` for fuzzed `(ast, ctx)`.
- **Predicate debug session (Phase 9d).** Per-clause hit/miss counters reflect actual evaluation; redaction (`redactMetadataKeys`) scrubs the named keys before persisting.

### Cross-binding compat (golden fixtures)

The five fixtures from [§9](#9-cross-binding-compat-fixtures), each consumed by all four bindings + the Rust SDK. Failures fail-stop the corresponding binding's CI; a divergence between bindings is the load-bearing regression signal.

### Integration

- **End-to-end announcement.** 4-node mesh; node A announces a chain via `Mesh::announce_chain`; nodes B/C/D observe via `Mesh::find_chain_holders` within heartbeat interval. Same scenario in each binding.
- **Federated query across bindings.** 3-node mesh, each node running a different binding (Node, Python, Go). All three nodes announce capabilities via their binding's surface; a query issued from the Rust SDK against the federated index returns matches authored from any binding.
- **Custom `PlacementFilter` callback.** Application supplies a `placement_filter_from_fn` callback; daemon migration via Mikoshi consults the callback; assert the daemon lands on the callback's preferred node. Run in each binding.
- **Daemon capability declaration → migration.** A daemon authored in language X with `required_capabilities` = `hardware.gpu` migrates only to GPU nodes. Cross-language: daemons authored in Node / Python / Go all observe the same migration target selection.
- **nRPC predicate pushdown end-to-end (Phase 9b).** Service exposes 10K rows; client sends a high-selectivity predicate via `mesh.call(svc, { where })`; only matching rows arrive over the wire. Pin both correctness (matched rows agree with local-evaluation reference) and bandwidth (wire bytes ≪ full-stream bytes). Run with each pair of (client binding, service binding) — 16 combinations across the four bindings.
- **Diff-driven placement re-evaluation (Phase 1 enhancements).** Daemon's capability set changes; placement engine receives the diff event; reschedule decision matches what a from-scratch placement would compute. Run in each binding.
- **Schema CI guard (Phase 9a).** Each binding's regenerated schema matches the canonical doc byte-for-byte. Fail loudly on drift.

### Performance

- **Per-call FFI overhead.** Configuration-driven `StandardPlacement::placement_score` ≤ 5 μs across 100 candidate nodes (matches the substrate plan's budget). Callback-driven `PlacementFilter` ≤ 50 μs per call across the FFI boundary; pin in tests so a regression is loud.
- **Lazy projection hot-path.** `caps.views().hardware.X` repeated 1M times: < 50 ns per call after first across all four bindings (pinned via Criterion bench in Rust + equivalent in TS / Python / Go). Worst-case full-handle initialization < 5 µs.
- **Predicate evaluation under fan-out.** A 5-clause predicate evaluated against 10K-node `CapabilityIndex` ≤ 1 ms in any binding. Planned variant ≥ 10× faster than unplanned on a worst-case AST (high-selectivity clause buried last).
- **`CapabilitySet::diff`.** O(n) in `|tags| + |metadata|`; < 10 µs for 100-tag sets across all bindings.
- **`announce_chain` throughput.** A node announcing 1K chains in a tight loop maintains ≤ 2× the per-announcement budget called out in `CAPABILITY_SYSTEM_PLAN.md`'s announcement-budget regression test.
- **Predicate pushdown bandwidth (Phase 9b).** 10K-row scan with 0.1% selectivity: > 100× bandwidth reduction vs. filter-on-receive.
- **Debug session overhead (Phase 9d).** Recording-instrumented predicate evaluation costs ≤ 5% over unrecorded evaluation; pin per binding so a regression in the instrumentation path is loud.

---

## Locked decisions

The plan is implementation-ready once the substrate plan's Phase A precondition (typed-struct migration story + missing-primitives surface) lands. These additional decisions ratify the SDK-specific call-outs above:

### The eternal rule (from `CAPABILITY_ENHANCEMENTS_PLAN.md`)

Every SDK addition MUST preserve:

1. **Wire = `tags + metadata` only.** No new fields on `CapabilitySet`'s wire shape. Predicate AST in nRPC rides as the *call's* payload, not as part of `CapabilityAnnouncement`.
2. **All smarts local to callers.** Schemas, lazy projections, query planning, debug sessions — every enhancement runs in the caller's process.
3. **Cross-binding deterministic AST.** Predicate AST evaluation produces identical results across all four bindings for any `(ast, ctx)`. Schemas agree on the same key set across bindings (CI-enforced).
4. **No semantic growth at the substrate.** The SDK is a wrapper; new affordances live in the binding layer.

This rule is binding on every Phase 9 enhancement; ignoring it is a plan-revision concern, not a per-phase implementation concern.

### View projections

`CapabilitySet::views()` is the single entry point. The handle is **lazy** — each projection field decodes on first access and caches via `OnceCell` (Rust) / lazy property (TS / Python) / `sync.Once` (Go). Hot-path `caps.views().hardware.X` < 50 ns post-cache; pinned in Criterion benches across all bindings.

### Predicate DSL

Language-idiomatic builders compile to the substrate's `Predicate` AST. The AST serializes as postcard bytes for cross-binding wire format; JSON is the debugging / fixture format. **No string-form parsing in the SDK** for The Warriors release — the user-facing query language is Atomic Playboys territory.

The same builders also drive nRPC `where:` filters (Phase 9b). The AST is the canonical filter language across capability queries AND workload queries.

### `PlacementFilter` paths

Path A (`StandardPlacement` builder) is the default; Path B (FFI-callback `placement_filter_from_fn`) is the escape hatch. SDK documentation leads with Path A; Path B is documented in a sub-section. Counter metric `dataforts_placement_callback_invocations_total{binding}` tracks Path-B usage so regressions to "everyone uses callbacks" are visible in operator dashboards.

### No backward-compat shim

Substrate Phase A.5.N.3 broke wire format outright; field-access (`caps.hardware.X`) does not exist post-migration. There is no deprecation window because there is no rolling-upgrade path across the wire-format break. SDK consumers codemod once: `caps.<axis>.<field>` → `caps.views().<axis>.<field>`.

### Schema is binding-local (Phase 9a)

Axis schemas describe the four-axis shape per binding. Schemas drive auto-completion / type-checking / runtime validation; they do NOT change wire format and do NOT enforce on incoming peer data (forward-compat: unknown keys under known axes pass with warnings). Schema bumps are binding-version concerns.

The canonical source-of-truth lives at `net/crates/net/docs/CAPABILITIES_SCHEMA.md`. Each binding regenerates from this canonical doc on every release; CI guard fails the build on any binding-vs-doc drift.

### Cross-binding fixture format

Five golden-vector fixtures under `tests/cross_lang_capability/`:

- `metadata_round_trip.json` — `(tags, metadata)` byte-identical round-trip.
- `predicate_ast.json` — predicate AST + evaluation results.
- `placement_score.json` — `StandardPlacement` scoring matrix.
- `predicate_nrpc_envelope.json` — predicate-in-nRPC postcard wire-format pin (Phase 9b).
- `capability_set_diff.json` — `caps.diff(prev)` output pin (Phase 1 enhancement).

Generated by a `cargo run --example gen_capability_fixtures` binary so they regenerate deterministically on schema changes. Same pattern as nRPC's `tests/cross_lang_nrpc/golden_vectors.json`.

### Scope of `MeshDaemon` extension

The new `requiredCapabilities()` / `optionalCapabilities()` methods have empty defaults; existing daemon implementations across all three non-Rust bindings keep working without modification. New daemons that want capability-driven placement override; old ones don't have to.

### `chain_bloom` is opaque

SDK surfaces `is_bloom_active()` and `chain_bloom_stats()` only. Application code never constructs or inspects the bloom directly — that responsibility lives in the substrate.

### Custom `Aggregator` is deferred

The substrate's `Aggregator` trait + concrete impls (`CountAggregator`, `SumAggregator`, `MaxAggregator`) are consumed via the binding's `aggregate()` operator. Custom-aggregator-via-callback ships when a workload asks for it (parallel to the `BlobAdapter` callback pattern; mechanical work but not in the critical path for the Warriors release).

### Predicate AST evolution lands cross-binding

Adding a new `Predicate` variant requires a coordinated commit across all four bindings + the cross-binding fixtures. Old bindings receiving an unknown variant in an nRPC `where:` envelope return a typed `PredicateDecodeError`; receivers handle gracefully (filter not applied). The fixture-generated postcard envelope is version-stamped so silent mis-parsing is impossible.

### Predicate debug sessions are opt-in only

Production hot paths must NOT pay the ~5% instrumentation overhead. Each binding's standard predicate evaluation path (`mesh.findNodes(pred)`, `mesh.call(svc, { where })`) does NOT instrument. Only the explicit `mesh.debugPredicate(pred)` builder enables recording. Pinned in unit tests per binding.

---

## Risks

- **FFI callback overhead in `PlacementFilter::Custom`.** Application-supplied filters cross the FFI boundary on every scoring call. Mitigation: lead with `StandardPlacement` (no callback); document Path B as advanced; track with the per-binding metric to spot avoidable usage. If a workload genuinely needs Path B, the trampolines are well-understood (same as `BlobAdapter` and migration factory).
- **Predicate AST versioning.** Adding a new `Predicate` variant in a future release breaks cross-binding compat for old peers receiving a predicate they can't decode. Mitigation: pin the AST schema version in the postcard envelope; old bindings reject unknown variants with a clear error rather than silently mis-parsing. Cross-binding fixtures version-stamped. AST-evolution commits are coordinated multi-binding rolls.
- **`MeshDaemon` factory expansion.** Daemons that override `required_capabilities` allocate a new `CapabilitySet` on every call. Mitigation: SDKs document caching the `CapabilitySet` on the daemon struct; benchmarks pin per-call cost ≤ 1 μs (it's a struct copy).
- **Predicate-builder ergonomics divergence.** Each language's idiomatic builder shape differs (chainable in TS, function-call in Go, etc.). Mitigation: cross-binding compat test ensures the *resulting AST* is identical; the surface ergonomics are explicitly per-language, not pinned uniform.
- **`announce_chain` storm under churn.** A flapping replica that re-announces on every heartbeat would burn announcement budget. Mitigation: the substrate's existing re-announcement throttle (`CapabilityAnnouncementPolicy`) covers this; SDK surfaces `set_announcement_throttle` for operators to tune.
- **Lazy-projection cache coherency under mutation.** Caller mutates `caps` between two `views().hardware()` reads via the same handle; second read returns stale cached projection. Mitigation: `views()` returns a borrowing handle whose lifetime is tied to `&caps`; mutation requires `&mut caps` which invalidates the handle (Rust). TS / Python / Go bindings document the same constraint and pin in tests that follow the borrow pattern (mutation invalidates the handle reference).
- **Schema drift between canonical doc and bindings.** A binding's regenerated schema diverges from `CAPABILITIES_SCHEMA.md` (e.g. someone adds a Rust-side axis key without updating the doc). Mitigation: CI guard regenerates each binding's schema from the doc and diffs against the committed schema; build fails on mismatch. Phase 9a's per-binding generators are codepath-deterministic so spurious diffs aren't possible.
- **Predicate debug session metadata leakage.** Recorded sessions may capture metadata values (`intent`, colocation hints, etc.) the operator considers sensitive. Mitigation: `redactMetadataKeys(keys)` API ships with every binding's debug session; documentation calls out redaction-before-persistence as the recommended pattern; sample CLI redaction filters provided.
- **nRPC `where:` envelope size in callsites with deep predicates.** Postcard-encoded predicates with 100+ clauses produce multi-kilobyte payloads riding alongside every nRPC call. Mitigation: per-binding warning when the encoded `where:` exceeds a threshold (default 4 KB); operators tune via `mesh.set_predicate_size_warn_threshold(bytes)`. Substrate-level enforcement is out of scope (eternal rule §4: no semantic growth).

---

## Effort

**5–7 focused weeks parallelised** (substrate track Phases 1–8 + enhancement track Phase 9 for the core bindings).

- ~4500 LoC across bindings + SDK wrappers (Rust SDK ~700, Node binding+sdk-ts ~1100, Python binding+sdk-py ~1100, Go binding ~1100, fixtures + tests ~500)
- ~2000 LoC tests (unit + integration + cross-binding compat against the five fixtures + performance benches for lazy projections / planner / debug session)
- ~1 week documentation (`MIGRATION.md` + per-binding README sections + worked examples + axis-schema canonical doc)

Bindings are fully parallelisable within each phase. The Rust SDK + cross-binding fixtures (Phases 1–2) are the only sequential prerequisite. Phase 9 sub-phases (9a/9b/9c/9d) gate independently on their corresponding `CAPABILITY_ENHANCEMENTS_PLAN.md` substrate phase landing.

---

## Activation gate

The substrate-track phases (1–8) ship in lockstep with `CAPABILITY_SYSTEM_PLAN.md`'s Phase H. The substrate's activation gate (a workload requesting durability beyond single-node, or a query workload needing federated capability primitives) drives both — they ship as one coherent release because the SDK plan has nothing to expose if the substrate isn't built, and the substrate is unconsumable from non-Rust callers if the SDK plan isn't built.

The enhancement-track phase (9) lands incrementally as each `CAPABILITY_ENHANCEMENTS_PLAN.md` substrate phase fires. Per-enhancement activation:

- **Phase 9a (axis schemas)** — gates on enhancement plan Phase 2 + a binding consumer requesting auto-completion / runtime validation.
- **Phase 9b (predicate-in-nRPC)** — gates on enhancement plan Phase 5 + the first service that wants predicate pushdown (likely Rebel Yell training-job scan or Atomic Playboys workload selector).
- **Phase 9c (planner opt-out)** — gates on enhancement plan Phase 4 landing; trivial to ship, no consumer demand needed.
- **Phase 9d (debug sessions)** — gates on enhancement plan Phase 6 + an operator pain point (someone files a "why did my query return 0?" support request).

---

## Cross-cutting: relationship to other plans

**Builds on:**

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — the substrate contract this plan wraps. Every type, trait, and primitive ships there; this plan exposes them ergonomically per language.
- [`CAPABILITY_ENHANCEMENTS_PLAN.md`](CAPABILITY_ENHANCEMENTS_PLAN.md) — the enhancement-layer substrate work that this plan's Phase 9 surfaces per binding (lazy projections, axis schemas, `CapabilitySet::diff`, chain helpers, predicate-in-nRPC, query planner exposure, debug sessions). Phase 9 sub-phases gate one-to-one on the enhancement plan's phases.
- [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) — the daemon factory + trampoline infrastructure this plan extends for `requiredCapabilities` / `optionalCapabilities` and the custom `PlacementFilter` callback.
- [`SDK_GROUPS_SURFACE_PLAN.md`](SDK_GROUPS_SURFACE_PLAN.md) — the cross-binding wrapper-pattern + factory-callback infrastructure model this plan mirrors. Same `Arc<Mutex<...>>` interior-mutability decision.
- [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) — the cross-binding compat-fixture pattern for capability-related ACL work.

**Consumed by:**

- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — RedEX's `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` calls and `PlacementFilter`-driven replica selection (Phase C) are surfaced through the bindings exposed by this plan. RedEX consumes the SDK surface; this SDK plan does not directly depend on RedEX.
- Future Atomic Playboys candidates — string-form predicate DSL, custom-aggregator callbacks, capability-watch streams — all build on the wrapper layer this plan ships.

---

## See also

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — substrate plan; the contract this SDK plan wraps
- [`CAPABILITY_ENHANCEMENTS_PLAN.md`](CAPABILITY_ENHANCEMENTS_PLAN.md) — substrate-side enhancement work this plan's Phase 9 surfaces
- [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) — sister SDK plan for the compute surface; same patterns
- [`SDK_GROUPS_SURFACE_PLAN.md`](SDK_GROUPS_SURFACE_PLAN.md) — sister SDK plan for the groups surface; same patterns
- [`SDK_PYTHON_PARITY_PLAN.md`](SDK_PYTHON_PARITY_PLAN.md) — Python-specific SDK parity considerations
- [`SDK_GO_PARITY_PLAN.md`](SDK_GO_PARITY_PLAN.md) — Go-specific SDK parity considerations
- [`misc/NRPC_DESIGN.md`](misc/NRPC_DESIGN.md) — pattern for cross-binding wrapper layers; `tests/cross_lang_nrpc/golden_vectors.json` is the fixture-format precedent this plan mirrors
- `RELEASE_ROADMAP.md` — The Warriors release context
