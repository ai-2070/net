# SDK Capability System Surface Plan

Bring the Warriors-release capability work — typed taxonomy, tag shapes, metadata field, bloom-filter aggregation, federated query primitives, `PlacementFilter` + `StandardPlacement` + `IntentRegistry`, and the `MeshDaemon` capability-authoring extension — into the `net-sdk` Rust surface and through to the Node, Python, and Go bindings + their high-level SDKs (`sdk-ts`, `sdk-py`). Companion to [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md), which specifies the substrate-level contracts; this doc plans the user-facing wrapper layer above the substrate.

The substrate plan's Phase H ("Bindings, 1 week, parallelisable") is the napi / pyo3 / cgo *plumbing* that exposes raw types across the FFI boundary. This plan adds the *ergonomic wrapper layer* on top — the typed builders, the language-idiomatic Predicate DSL, the FFI callback infrastructure for application-supplied `PlacementFilter` impls, and the cross-binding compat test suite that pins the contract.

## Status

**Design only; activation gate is the substrate plan.** This plan ships in lockstep with `CAPABILITY_SYSTEM_PLAN.md`'s Phase H (bindings) and the dependent Mikoshi-integration phase G. Independent of `REDEX_DISTRIBUTED_PLAN.md`.

## Goals

- One ergonomic surface per language for: building a `CapabilitySet` (tags + metadata), parsing/building `Tag` values, reading typed-struct projections (`HardwareCapabilities::from(&caps)` etc.), advertising / withdrawing chains (`announce_chain` / `withdraw_chain` / `find_chain_holders`), composing predicates, running federated queries, and consuming or supplying `PlacementFilter`.
- Parity across Node (NAPI + sdk-ts), Python (PyO3 + sdk-py), Go (CGO). Same behavior, same structured errors, same FFI callback shape for application-supplied filters.
- Typed `CapabilityError` surface on every language, with kind discrimination (`reserved-prefix` / `metadata-too-large` / `unknown-axis` / `predicate-malformed` / `placement-evaluation-failed`). Mirrors the `MigrationError` / `GroupError` pattern.
- Backward-compat shims for the `CapabilitySet` view-projection migration: legacy accessors (`caps.hardware`) keep working for one minor version, emitting deprecation warnings, while the new helper-based surface (`HardwareCapabilities.from(caps)`) is the documented path forward.
- Cross-binding compat fixtures pinning the wire format of the new metadata field, the bloom-filter encoding, and the predicate AST serialization — same shape as `tests/cross_lang_nrpc/golden_vectors.json` already used for nRPC.

## Non-goals

- **Changing core semantics.** The substrate's `CapabilitySet`, `Tag`, `Predicate`, `PlacementFilter`, `IntentRegistry`, `MeshDaemon` extensions stay exactly as `CAPABILITY_SYSTEM_PLAN.md` defines them. SDK code is wrap-and-forward, not redesign.
- **A user-facing query language.** Per the substrate plan's "What ships NOT": this plan exposes the predicate AST + composition operators, not a string-DSL parser. The string-DSL (Atomic Playboys territory) parks until a workload demands it.
- **Continuous rebalancing or auto-placement supervisors.** Same as the groups plan's Stage-N+1 deferral — the SDK exposes `PlacementFilter` invocation; it doesn't run a background "placement-aware rescheduler."
- **Encrypted-metadata transport.** Per substrate plan's deferral.
- **Public release wheels carrying every Capability feature.** Feature gating mirrors `compute` / `groups`: `capability-system-v2` (or similar; final name TBD with the substrate gate) ships behind a flag in the release workflows. Pilots opt in.

---

## What ships

Eight things across the SDK + binding surface, in dependency order:

1. **Typed-taxonomy + view-projection helpers** — `Tag`, `TagKey`, `TaxonomyAxis`, `CapabilitySet`-with-`metadata` exposed in every binding; `HardwareCapabilities::from(&caps)` (and the four other view projections) callable from every SDK. Migration-shim for the legacy field-access pattern.
2. **Tag-shape builders** — language-idiomatic constructors for `causal:<hex>`, `causal:<hex>:<tip_seq>`, `causal:<hex>[<start>..<end>]`, `fork-of:<parent_hex>`, `heat:<chain_hex>=<rate>`, `scope:<label>`. Reserved-prefix enforcement at the builder level; user-emitted tags with reserved prefixes return a typed error before the call hits the wire.
3. **Discovery primitives** — `Mesh::announce_chain` / `announce_chain_range` / `withdraw_chain` / `find_chain_holders` exposed across bindings + SDKs. Async where the underlying SDK is async (Rust, TS, Go contexts); sync with GIL-release where the SDK is sync (Python).
4. **Bloom-filter aggregation surface** — `CapabilitySet::chain_bloom` round-trips via serde; the bloom is opaque on the SDK surface (no application-level construction; it's auto-built by the substrate at the threshold). Surface-level, only diagnostic helpers (`is_bloom_active()`, `chain_bloom_stats()`).
5. **Federated query primitives** — the five operators (`filter`, `match_axis`, `traverse`, `aggregate`, `nearest`) callable from each language; the `Predicate` AST built via language-idiomatic builders that compile down to the same portable AST. Local-only by default; `Federated` opt-in wrapper for cross-node execution.
6. **`PlacementFilter` consumption + custom impls** — application code in any binding can either configure a `StandardPlacement` (recommended; no FFI callback overhead) or supply a custom `PlacementFilter` via the existing FFI-callback pattern (same trampolines as `BlobAdapter` and the migration factory model).
7. **`IntentRegistry::register`** — custom-intent registration callable from each language. Defaults ship from substrate; per-deployment overrides land via the SDK.
8. **`MeshDaemon::required_capabilities` / `optional_capabilities`** — daemon authors in any language declare requirements/optionals. The shape mirrors the existing `factory` callback infrastructure used for migration-target reconstruction.

What this doc does NOT ship (deferred):

- **String-form predicate DSL.** Per `CAPABILITY_SYSTEM_PLAN.md`'s "What ships NOT" — the AST + composition operators ship; the parsed string DSL waits.
- **Cross-binding `Aggregator` registration.** The trait + concrete impls (`CountAggregator`, `SumAggregator`, `MaxAggregator`) ship in Rust core (substrate plan); only the *consumption* surface ships in this SDK plan. A custom-aggregator-via-callback pattern would mirror `PlacementFilter`'s trampolines but is deferred until a workload asks for it.
- **Live capability-watch streams.** `find_nodes` returns a snapshot; subscribing to capability-change events via a channel is a follow-up if a real consumer surfaces.

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

### 1. View projections are SDK-owned, not napi/pyo3-class-shaped

The substrate ships `HardwareCapabilities::from(&CapabilitySet)` etc. as Rust functions. Exposing those as napi/pyo3 classes (with constructors taking `&CapabilitySet`) would require five new class declarations × three bindings = 15 boilerplate FFI surfaces.

**Decision:** the binding-side `CapabilitySet` exposes a `views` accessor returning a struct/object with the five projections pre-computed:

```rust
// Rust core (substrate)
pub struct CapabilityViews<'a> {
    pub hardware: HardwareCapabilities,
    pub software: SoftwareCapabilities,
    pub resource_limits: ResourceLimits,
    pub models: Vec<ModelCapability>,
    pub tools: Vec<ToolCapability>,
}
impl CapabilitySet {
    pub fn views(&self) -> CapabilityViews<'_> { /* runs From<&CapabilitySet> for each */ }
}
```

```typescript
// sdk-ts
const caps = mesh.localCapabilities()
const { hardware, software, resourceLimits, models, tools } = caps.views()
console.log(hardware.gpu?.vramMb)
```

```python
# sdk-py
caps = mesh.local_capabilities()
views = caps.views()
print(views.hardware.gpu.vram_mb if views.hardware.gpu else None)
```

```go
// Go binding
caps, _ := mesh.LocalCapabilities()
views := caps.Views()
if views.Hardware.GPU != nil {
    fmt.Println(views.Hardware.GPU.VRAMMB)
}
```

One method per binding (`views()`); the projection happens once and yields a struct/object the consumer destructures. No per-projection class boilerplate.

### 2. Backward-compat shim for legacy field access

Pre-Warriors code reads `caps.hardware.gpu.vram_mb` as a direct field. The substrate plan's Phase A removes those fields from the wire-format struct, but binding consumers will trip immediately if the SDK doesn't preserve the access pattern through one minor version.

**Decision:** per-binding compatibility shim that emits a deprecation warning on first access:

- **Rust SDK**: `#[deprecated(note = "use caps.views().hardware instead")]` on a `pub fn hardware(&self) -> HardwareCapabilities` method that returns the projection. Compiler-time warning; users have a release window to migrate.
- **Node/Python/Go**: a `Proxy` / `__getattr__` / receiver method that returns the projected struct AND emits a runtime deprecation log on first read per process (one-time hysteresis to avoid log spam). Same release window as Rust.
- After the deprecation window, the shim is removed and the field accessor returns `undefined` / raises `AttributeError` / fails to compile, depending on language.

Pin the deprecation behavior in cross-binding tests so all three languages emit the warning consistently.

### 3. Predicate DSL: language-idiomatic builders → portable AST

The substrate ships the `Predicate` enum + the `pred!` macro. Other languages need ergonomic equivalents that produce the same AST (which crosses the FFI boundary as serde-encoded bytes for federated execution).

**Decision:** each binding ships a fluent builder that mirrors the AST shape:

```typescript
// sdk-ts — fluent / chainable
import { p } from '@ai2070/net/capability'

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
import { IntentRegistry, requireTag, requireAxisValue } from '@ai2070/net/capability'

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

Three new fixtures land alongside the substrate's Phase A:

- `tests/cross_lang_capability/metadata_round_trip.json` — `(tags, metadata)` round-trips byte-for-byte across all four bindings.
- `tests/cross_lang_capability/predicate_ast.json` — N predicates as JSON; each binding decodes, evaluates against a stub `CapabilitySet` table, and asserts results match.
- `tests/cross_lang_capability/placement_score.json` — N `(StandardPlacement-config, candidate-set)` pairs with expected scores. Each binding evaluates and asserts the score matrix matches Rust's reference output to within `1e-6`.

The fixtures land BEFORE Phase H of the substrate ships, so binding work has a contract to test against from day one.

---

## Phasing

Eight phases, parallelisable within each language after the substrate's Phase A lands:

### Phase 1 — Rust SDK (`net-sdk`) surface (1 week)

- Re-export `Tag`, `TagKey`, `Predicate`, `PlacementFilter`, `StandardPlacement`, `IntentRegistry`, `RequiredCapability`, `Aggregator` trait under `net_sdk::capability` and `net_sdk::placement`.
- `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` (and the range variant) added as async methods on `net_sdk::mesh::Mesh`.
- `CapabilitySet::views()` accessor + `pub fn hardware()` deprecated shim.
- `pred!` macro re-exported from substrate.
- Documentation: `net-sdk/README.md` adds a "Capability System" section with the full set of examples.

### Phase 2 — Cross-binding compat fixtures (3 days, lands BEFORE Phase 3-5)

- Three fixtures defined in [§9](#9-cross-binding-compat-fixtures). Generated from a `cargo run --example gen_capability_fixtures` binary so they regenerate deterministically on schema changes.
- Documented in `tests/cross_lang_capability/README.md`.

### Phase 3 — Node binding + sdk-ts (1 week, parallelises with 4 + 5)

- `bindings/node/src/capability.rs`: napi class wrappers for `CapabilitySet`, `Tag`, `Predicate`, `StandardPlacement`, `IntentRegistry`. Binding-level reserved-prefix enforcement.
- `bindings/node/capability.ts` typed wrappers exporting the fluent `p.*` builder, `requireTag` / `requireAxisValue`, the `StandardPlacement` builder, `placementFilterFromFn` callback factory.
- `sdk-ts/src/capability.ts` with the user-facing surface (re-exporting from binding + adding higher-level helpers).
- Tests against the three fixtures from Phase 2.
- Migration shim: deprecation `Proxy` on `CapabilitySet` for legacy field access.

### Phase 4 — Python binding + sdk-py (1 week, parallelises with 3 + 5)

- `bindings/python/src/capability.rs`: PyO3 class wrappers (parallel to Node).
- `bindings/python/python/net/capability.py` with the fluent `p.*` builder using snake_case methods (`p.numeric_at_least` etc.); `RequiredCapability` builders; `StandardPlacement` builder; `placement_filter_from_fn`.
- `sdk-py/src/net_sdk/capability.py` with the user-facing surface.
- Tests against fixtures.
- Migration shim: `__getattr__`-based deprecation on `CapabilitySet` for legacy field access.

### Phase 5 — Go binding (1 week, parallelises with 3 + 4)

- `bindings/go/capability-ffi/`: cgo C-ABI for `CapabilitySet`, `Tag`, `Predicate` round-trip, `StandardPlacement` config, `IntentRegistry`. Predicate AST and `StandardPlacement` config cross the FFI boundary as postcard-encoded bytes (mirrors `compute-ffi`'s Daemon-snapshot pattern).
- `bindings/go/net/capability.go` with the user-facing surface: `pred.*` package providing `pred.And`, `pred.Exists`, etc.; `StandardPlacement` builder; `PlacementFilterFromFn` callback factory.
- `bindings/go/net/capability_views.go` with the view-projection structs.
- Tests against fixtures.
- Migration shim: a deprecated `*CapabilitySet` method that wraps `.Views().Hardware` etc. with a one-time log warning per process.

### Phase 6 — `MeshDaemon` capability authoring (3 days per binding, parallelisable)

- Add `requiredCapabilities()` / `optionalCapabilities()` to the `MeshDaemon` factory-callback contract in each binding (Node / Python / Go). Daemon authors override; default is empty.
- The factory trampoline already resolves `kind → factory`; add two more callbacks per kind for the capability methods. Reuse the existing factory infrastructure — no new dispatcher.
- Tests: a daemon declaring `hardware.gpu` requirement is migrated only to GPU nodes (depends on substrate Phase G).

### Phase 7 — Custom `PlacementFilter` callback (3 days per binding, parallelisable)

- Implement the `placement_filter_from_fn` factory in each binding using the existing trampoline pattern (Node TSFN, Python `Python::attach`, Go `cgo.Handle`).
- Counter metric `dataforts_placement_callback_invocations_total{binding}`.
- Cross-binding compat test: the same callback (encoded as a deterministic scoring function from a configuration JSON) produces the same score sequence across all three bindings against the placement-score fixture.

### Phase 8 — Documentation + migration guides (3 days, parallelisable)

- Per-binding README sections demonstrating the full surface: building a `CapabilitySet`, parsing `Tag`s, advertising chains, running predicate queries, configuring `StandardPlacement`, registering custom intents, declaring daemon capabilities.
- A `CAPABILITY_SYSTEM_MIGRATION.md` guide covering the legacy-tag → typed-axis migration AND the field-access → view-projection migration. Versioned screenshots of the deprecation log output.

**Total: 4–5 weeks.** Phase 1 + Phase 2 are sequential preconditions (~10 days). Phases 3–5 parallelise (1 week, 3 engineers, or 3 weeks serial). Phases 6–7 parallelise within each binding (~3 days each). Phase 8 parallelises with the others. Single engineer on the SDK serialises to ~5 weeks; three engineers parallel-process to ~3.

---

## Test strategy

### Unit (per binding)

- `Tag::parse` round-trips for each axis-prefixed shape; reserved-prefix rejection at the builder level returns the typed error.
- `Predicate` AST construction via the fluent builder for each variant; serialization round-trips.
- `StandardPlacement` configuration: each field accepts the documented value range; invalid values return typed errors.
- `IntentRegistry::register` accepts the `RequiredCapability` values produced by the binding's helpers; defaults match the substrate.
- View projection: `caps.views().hardware` returns the same data the legacy field access would have returned (compatibility-shim correctness).
- Migration shim: legacy field access emits the deprecation log exactly once per process.

### Cross-binding compat (golden fixtures)

The three fixtures from [§9](#9-cross-binding-compat-fixtures), each consumed by all four bindings + the Rust SDK. Failures fail-stop the corresponding binding's CI; a divergence between bindings is the load-bearing regression signal.

### Integration

- **End-to-end announcement.** 4-node mesh; node A announces a chain via `Mesh::announce_chain`; nodes B/C/D observe via `Mesh::find_chain_holders` within heartbeat interval. Same scenario in each binding.
- **Federated query across bindings.** 3-node mesh, each node running a different binding (Node, Python, Go). All three nodes announce capabilities via their binding's surface; a query issued from the Rust SDK against the federated index returns matches authored from any binding.
- **Custom `PlacementFilter` callback.** Application supplies a `placement_filter_from_fn` callback; daemon migration via Mikoshi consults the callback; assert the daemon lands on the callback's preferred node. Run in each binding.
- **Daemon capability declaration → migration.** A daemon authored in language X with `required_capabilities` = `hardware.gpu` migrates only to GPU nodes. Cross-language: daemons authored in Node / Python / Go all observe the same migration target selection.
- **Deprecation shim end-to-end.** A pre-Warriors application reading `caps.hardware.gpu.vram_mb` directly continues to work for one minor version and emits exactly one deprecation log per process. Pin in each binding.

### Performance

- **Per-call FFI overhead.** Configuration-driven `StandardPlacement::placement_score` ≤ 5 μs across 100 candidate nodes (matches the substrate plan's budget). Callback-driven `PlacementFilter` ≤ 50 μs per call across the FFI boundary; pin in tests so a regression is loud.
- **Predicate evaluation under fan-out.** A 5-clause predicate evaluated against 10K-node `CapabilityIndex` ≤ 1 ms in any binding.
- **`announce_chain` throughput.** A node announcing 1K chains in a tight loop maintains ≤ 2× the per-announcement budget called out in `CAPABILITY_SYSTEM_PLAN.md`'s announcement-budget regression test.

---

## Locked decisions

The plan is implementation-ready once the substrate plan's Phase A precondition (typed-struct migration story + missing-primitives surface) lands. These additional decisions ratify the SDK-specific call-outs above:

### View projections

`CapabilitySet::views()` is the single entry point. Per-projection accessors (`caps.hardware`, `caps.software`, etc.) are deprecated shims that emit a one-time warning per process and remove after the next major. Same in every binding.

### Predicate DSL

Language-idiomatic builders compile to the substrate's `Predicate` AST. The AST serializes as postcard bytes for cross-binding wire format; JSON is the debugging / fixture format. **No string-form parsing in the SDK** for The Warriors release — the user-facing query language is Atomic Playboys territory.

### `PlacementFilter` paths

Path A (`StandardPlacement` builder) is the default; Path B (FFI-callback `placement_filter_from_fn`) is the escape hatch. SDK documentation leads with Path A; Path B is documented in a sub-section. Counter metric `dataforts_placement_callback_invocations_total{binding}` tracks Path-B usage so regressions to "everyone uses callbacks" are visible in operator dashboards.

### Backward compat window

One minor version of legacy field-access compatibility (`caps.hardware`, `caps.software`, etc.). After that, the shim is removed in lockstep across all bindings + SDKs. Substrate plan's Phase A decision (also one minor version) is the timing source of truth; SDK shims are removed in the same release.

### Cross-binding fixture format

Three golden-vector fixtures (`metadata_round_trip.json`, `predicate_ast.json`, `placement_score.json`) under `tests/cross_lang_capability/`. Generated by a `cargo run --example gen_capability_fixtures` binary so they regenerate deterministically on schema changes. Same pattern as nRPC's `tests/cross_lang_nrpc/golden_vectors.json`.

### Scope of `MeshDaemon` extension

The new `requiredCapabilities()` / `optionalCapabilities()` methods have empty defaults; existing daemon implementations across all three non-Rust bindings keep working without modification. New daemons that want capability-driven placement override; old ones don't have to.

### `chain_bloom` is opaque

SDK surfaces `is_bloom_active()` and `chain_bloom_stats()` only. Application code never constructs or inspects the bloom directly — that responsibility lives in the substrate.

### Custom `Aggregator` is deferred

The substrate's `Aggregator` trait + concrete impls (`CountAggregator`, `SumAggregator`, `MaxAggregator`) are consumed via the binding's `aggregate()` operator. Custom-aggregator-via-callback ships when a workload asks for it (parallel to the `BlobAdapter` callback pattern; mechanical work but not in the critical path for the Warriors release).

---

## Risks

- **FFI callback overhead in `PlacementFilter::Custom`.** Application-supplied filters cross the FFI boundary on every scoring call. Mitigation: lead with `StandardPlacement` (no callback); document Path B as advanced; track with the per-binding metric to spot avoidable usage. If a workload genuinely needs Path B, the trampolines are well-understood (same as `BlobAdapter` and migration factory).
- **Predicate AST versioning.** Adding a new `Predicate` variant in a future release breaks cross-binding compat for old peers receiving a predicate they can't decode. Mitigation: pin the AST schema version in the postcard envelope; old bindings reject unknown variants with a clear error rather than silently mis-parsing. Cross-binding fixtures version-stamped.
- **Migration-shim divergence.** Three bindings × one shim implementation each = three opportunities for the deprecation behavior to drift. Mitigation: cross-binding compat test pinning the deprecation-log shape and one-time-per-process semantics; failure to emit the log (or emitting it twice) fails the test.
- **`MeshDaemon` factory expansion.** Daemons that override `required_capabilities` allocate a new `CapabilitySet` on every call. Mitigation: SDKs document caching the `CapabilitySet` on the daemon struct; benchmarks pin per-call cost ≤ 1 μs (it's a struct copy).
- **Predicate-builder ergonomics divergence.** Each language's idiomatic builder shape differs (chainable in TS, function-call in Go, etc.). Mitigation: cross-binding compat test ensures the *resulting AST* is identical; the surface ergonomics are explicitly per-language, not pinned uniform.
- **`announce_chain` storm under churn.** A flapping replica that re-announces on every heartbeat would burn announcement budget. Mitigation: the substrate's existing re-announcement throttle (`CapabilityAnnouncementPolicy`) covers this; SDK surfaces `set_announcement_throttle` for operators to tune.

---

## Effort

**4–5 focused weeks parallelised.**

- ~3500 LoC across bindings + SDK wrappers (Rust SDK ~600, Node binding+sdk-ts ~900, Python binding+sdk-py ~900, Go binding ~900, fixtures + tests ~600)
- ~1500 LoC tests (unit + integration + cross-binding compat against the three fixtures + performance)
- ~1 week documentation (`MIGRATION.md` + per-binding README sections + worked examples)

Bindings are fully parallelisable. The Rust SDK + cross-binding fixtures (Phases 1–2) are the only sequential prerequisite.

---

## Activation gate

Ships in lockstep with the substrate plan's Phase H. The substrate's activation gate (a workload requesting durability beyond single-node, or a query workload needing federated capability primitives) drives both — they ship as one coherent release because the SDK plan has nothing to expose if the substrate isn't built, and the substrate is unconsumable from non-Rust callers if the SDK plan isn't built.

---

## Cross-cutting: relationship to other plans

**Builds on:**

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — the substrate contract this plan wraps. Every type, trait, and primitive ships there; this plan exposes them ergonomically per language.
- [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) — the daemon factory + trampoline infrastructure this plan extends for `requiredCapabilities` / `optionalCapabilities` and the custom `PlacementFilter` callback.
- [`SDK_GROUPS_SURFACE_PLAN.md`](SDK_GROUPS_SURFACE_PLAN.md) — the cross-binding wrapper-pattern + factory-callback infrastructure model this plan mirrors. Same `Arc<Mutex<...>>` interior-mutability decision.
- [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) — the cross-binding compat-fixture pattern for capability-related ACL work.

**Consumed by:**

- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — RedEX's `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` calls and `PlacementFilter`-driven replica selection (Phase C) are surfaced through the bindings exposed by this plan. RedEX consumes the SDK surface; this SDK plan does not directly depend on RedEX.
- Future Atomic Playboys candidates — string-form predicate DSL, custom-aggregator callbacks, capability-watch streams — all build on the wrapper layer this plan ships.

---

## See also

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — substrate plan; the contract this SDK plan wraps
- [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) — sister SDK plan for the compute surface; same patterns
- [`SDK_GROUPS_SURFACE_PLAN.md`](SDK_GROUPS_SURFACE_PLAN.md) — sister SDK plan for the groups surface; same patterns
- [`SDK_PYTHON_PARITY_PLAN.md`](SDK_PYTHON_PARITY_PLAN.md) — Python-specific SDK parity considerations
- [`SDK_GO_PARITY_PLAN.md`](SDK_GO_PARITY_PLAN.md) — Go-specific SDK parity considerations
- [`misc/NRPC_DESIGN.md`](misc/NRPC_DESIGN.md) — pattern for cross-binding wrapper layers; `tests/cross_lang_nrpc/golden_vectors.json` is the fixture-format precedent this plan mirrors
- `RELEASE_ROADMAP.md` — The Warriors release context
