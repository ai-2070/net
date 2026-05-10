# Net v0.13 — "Chippin' In"

v0.13 lands the **capability system** end-to-end across the substrate and all five bindings. v0.12 ("Firestarter") shipped nRPC; v0.13 makes capability the load-bearing layer underneath. The `Tag` placeholder in v0.10 / v0.11, and the untyped `Vec<String>` shape v0.12 still carried, both go away — `CapabilitySet` is now a `{ tags: HashSet<Tag>, metadata: BTreeMap }` typed-taxonomy wire shape, every binding ships the same `Predicate` AST + evaluator + validator + diff + trace + debug-report aggregator, and predicates ride nRPC request headers (`cyberdeck-where:`) so server-side filtering picks the right candidate without re-running the predicate per hop. Three plans landed in lockstep: `CAPABILITY_SYSTEM_PLAN.md` (the substrate), `CAPABILITY_ENHANCEMENTS_PLAN.md` (the lazy-projection / predicate-pushdown / debug-session refinements), and `CAPABILITY_SYSTEM_SDK_PLAN.md` (the per-binding surface). Plus the closure of the 28-item `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2` audit before merge.

The hardening posture from the Black Diamond line is intact — every new surface ships with handle-lifetime, panic-safety, and FFI-soundness guarantees consistent with v0.11 / v0.12 — but this release is about replacing the placeholder with the real thing.

---

## Capability System (substrate)

The eight phases from `CAPABILITY_SYSTEM_PLAN.md` ship complete. Each phase landed independently; the per-phase commits build on each other, and the wire format only froze at Phase A.5.N.3.

### Typed taxonomy (Phase A)

The flat tag namespace becomes a four-axis ontology — `hardware` / `software` / `devices` / `dataforts` — backed by a typed `Tag` enum:

```rust
pub enum Tag {
    AxisPresent { axis: TaxonomyAxis, key: String },
    AxisValue   { axis: TaxonomyAxis, key: String, value: String, separator: AxisSeparator },
    Reserved    { prefix: String, body: String },   // scope:* / causal:* / fork-of:* / heat:*
    Legacy(String),                                  // pre-A.5 untyped strings
}
```

`Tag::parse(s)` accepts every shape including reserved-prefix tags (the deserializer + substrate-internal callers); `Tag::parse_user(s)` rejects reserved prefixes for application input. `TagKey` (`(axis, key)`) is the half-form `Predicate` matches on. `TaxonomyAxis::all()` enumerates the four axes for iteration.

Axis values accept either `=` or `:` as the separator on the wire (`hardware.gpu.vram_mb=24576` and `hardware.gpu:nvidia` both parse). The `separator` is preserved through `Tag::Eq` for byte-stable round-trips, and `tag.semantic_eq(other)` is the separator-agnostic comparison for tag matching.

### Tag shapes for discovery (Phase B)

Reserved-prefix tag shapes flesh out the discovery primitive. `causal:<hex>` / `causal:<hex>:<tip_seq>` / `causal:<hex>[<range>]` for chain holders; `fork-of:<parent_hex>` for chain ancestry; `heat:<chain_hex>=<rate>` for hot-chain advertisement; `scope:tenant:<id>` / `scope:region:<name>` / `scope:subnet-local` (`scope:*` was already in v0.12, now formally part of the taxonomy). `RESERVED_PREFIXES` constant exposes the full list for binding-level enforcement.

### Metadata field (Phase A.5.N — A.5.N.3)

`CapabilitySet` storage shape collapses to two fields:

```rust
pub struct CapabilitySet {
    pub tags: HashSet<Tag>,
    pub metadata: BTreeMap<String, String>,
}
```

`HardwareCapabilities` / `SoftwareCapabilities` / `Vec<ModelCapability>` / `Vec<ToolCapability>` / `ResourceLimits` are *projections* — derived on demand via `caps.views()`. Encoding scheme: `hardware.cpu_cores=N` / `hardware.gpu` / `hardware.gpu.vram_mb=N` / `software.os=linux` / `software.model.0.id=...` / `hardware.limits.max_concurrent_requests=N`. Tool JSON-Schema strings (which can't safely round-trip through the tag wire format) live in `metadata` under `tool::<id>::input_schema` / `tool::<id>::output_schema`. Application-defined metadata keys propagate as opaque pairs (subject to a 4 KB soft cap with a `MetadataOversize` warning at the validator layer).

Wire format emits tags in sorted `Tag::to_string()` order — the `HashSet` keeps O(1) membership for in-memory lookups; the `serialize_with` hook flattens to a sorted `Vec` on the way out. Without this, two ends of a signed announcement round-trip would produce different bytes (HashSet iteration is process-local random) and the verifier would reject as `InvalidSignature`.

### Bloom-filter primitive (Phase D)

`behavior::bloom::BloomFilter` (`{ len_bits, k, bits: Vec<u64> }`) backs compact chain-tag membership probes via xxh3-128 double-hashing. ~1% FPR at 10 K items in ≤ 500 KB per the substrate sizing target. Probe pattern: callers that match the bloom run a follow-up precise lookup (existing `causal:<hex>` tag membership) before issuing real reads — false positives become recoverable misses, false negatives are impossible by construction. Domain-separated via `BLOOM_HASH_SEED = 0xB100_F1AC_DEAD_CAFE` so callers using xxh3 elsewhere don't accidentally collide.

`BloomFilter::new(expected_items, false_positive_rate)` clamps degenerate inputs (`expected_items == 0` → 1, `p` clamped to `(1e-9, 0.5)`); `BloomFilter::with_params(len_bits, k)` is the explicit-parameters constructor for cross-binding fixtures. Round-trips via serde with explicit deserialize-side validation (rejects out-of-range `k`, mismatched `len_bits`/`bits.len() * 64`).

### Federated query primitives (Phase E)

`behavior::query::CapabilityQuery` lifts five composable ops over `CapabilityIndex`:

- `filter(predicate)` — predicate-driven candidate set.
- `match_axis(axis, key)` — axis-shaped tag scan.
- `aggregate(key, reduction)` — per-key cardinality / numeric reductions.
- `traverse(seed, edge_fn, depth)` — graph-style join over peer capability links.
- `nearest(predicate, k, proximity)` — combine with proximity to score the top-K best matches.

Implementations on `CapabilityIndex` are O(log n) for indexed predicates and O(n) for the residual scan. The `Predicate` AST and these five ops together are what `Mesh::find_nodes_by_filter` / `find_best_node_scoped` flow through.

### `PlacementFilter` trait + `StandardPlacement` (Phase F)

`PlacementFilter::placement_score(target, artifact) -> Option<f32>` is the substrate-level placement primitive. `Some(score)` admits the candidate at a fitness in `[0, 1]`; `None` is a hard veto. `Artifact` carries the workload type — `Chain` (causal-chain placement), `Replica` (channel replica placement), `Daemon` (compute placement, with `required` + `optional` capability sets).

`StandardPlacement` is the multi-axis reference implementation: scope filter, proximity max-RTT, intent matching (`AnyOfLocalCapabilities` / `StrictMatch` / `Custom`), colocation policy (`Ignore` / `SoftPreference` / `StrictRequired`), resource axis (`Storage` / `Compute` / `Both`), anti-affinity config (leadership-concentration penalty), and a custom-filter axis that consumes a registered host-language `PlacementFilter` via `with_custom_filter_id(id)`. Axes compose multiplicatively; `None` on any axis is a hard veto. Per-axis tie-breaking via the locked RTT → free-resource → lexicographic-NodeId chain (`tie_break_compare`).

`IntentRegistry::register(intent, &[required])` registers per-intent placement requirements built from the `require!` / `require_axis!` / `require_axis_value!` macros. Substrate ships defaults for the four canonical intents (`ml-training`, `inference`, `embedding-cache`, `tool-call`); per-deployment overrides land via the SDK.

`global_placement_filter_registry()` is the process-wide singleton mapping registered IDs to `Arc<dyn PlacementFilter>`. Bindings register their language-specific wrappers here; the scheduler resolves an SDK ID to an impl before scoring. Registration is open-by-default — the registry refuses overwrites of an existing ID (`register` returns `false`) so two bindings can't accidentally clobber each other's filters.

### Mikoshi integration (Phase G)

`Mikoshi::select_migration_target(daemon, scope)` consults `PlacementFilter` end-to-end. `LegacyPlacement` preserves the v0.12 ad-hoc selection under a feature flag for one minor version; new daemons should target `StandardPlacement`. `ReplicaGroup::select_member_node` and `StandbyGroup::select_promotion_target` route through the same scorer so replication / hot-standby promotion get the same axis-composed verdict as initial placement.

Daemon authors declare `MeshDaemon::required_capabilities()` and `optional_capabilities()`; the runtime publishes both as part of the daemon's identity-bound announcement so the placement scheduler — and any custom filter — can consult them. Bindings expose the same hook through their daemon-caps dispatcher (`net_compute_set_daemon_caps_dispatcher` at the C ABI; the equivalent Python / TS / Go callback during factory registration).

---

## Capability Enhancements (substrate refinements)

The seven independently-landable enhancements from `CAPABILITY_ENHANCEMENTS_PLAN.md`. None of these change the wire format — they sit on top of the typed-taxonomy primitive and pay for themselves at the application layer.

### Lazy view projections + diff (Phase 1)

`caps.views()` returns a `CapabilityViews` handle whose per-axis fields decode-and-cache on first access. Hot-path `caps.views().hardware().memory_mb` is < 50 ns post-cache; first call is the per-tag scan. Cache invalidates compiler-enforced via the `&caps` borrow held by `views()`.

`caps.diff(prev)` returns `CapabilitySetDiff { added_tags, removed_tags, changed_metadata }` for cheap before/after change detection. `MetadataChange::{Added, Removed, Updated}` per-key with old/new values. Powers event-driven placement, capability-change dashboards, and delta-based metadata propagation.

### Axis schemas (Phase 2)

`AXIS_SCHEMA` is the canonical per-axis schema baked into the substrate at build time: known keys per axis, value types (`Presence` / `Number` / `String` / `Enumeration` / `Bool` / `Csv`), indexed-collection shapes (`software.model.<i>.*` / `software.tool.<i>.*` / `hardware.accelerator.<i>.*`). `validate_capabilities(caps)` runs the schema against a `CapabilitySet` and returns a `ValidationReport` of `errors` (operator-must-fix: `UnknownAxis`, `TypeMismatch`, `IndexMalformed`) + `warnings` (forward-compat / hygiene: `UnknownKey`, `MetadataOversize`, `LegacyTag`). Both lists are sorted by JSON-stringified entry so cross-binding fixture comparisons stay order-independent. Each binding regenerates its language-side schema from the same authoritative `CAPABILITIES_SCHEMA.md` doc.

### Predicate AST + nRPC headers (Phase 3 / 5)

`behavior::predicate::Predicate` is the typed AST. Variants: `Exists` / `Equals` / `NumericAtLeast` / `NumericAtMost` / `NumericInRange` / `SemverAtLeast` / `SemverAtMost` / `SemverCompatible` / `StringPrefix` / `StringMatches` / `MetadataExists` / `MetadataEquals` / `MetadataMatches` / `MetadataNumericAtLeast` / `And` / `Or` / `Not`. Built via the `pred!` macro in Rust, language-idiomatic builders in every other binding (`p.and([...])`, `p.exists(tagKey('hardware', 'gpu'))`, etc.). Evaluated against an `EvalContext` constructed from any `(tags, metadata)` pair.

Predicates encode losslessly to a `cyberdeck-where:` nRPC header pair via `predicate_to_rpc_header`; the receiver decodes via `predicate_from_rpc_headers` (consumes any iterable of `(name, value_bytes)` pairs through the `AsRpcHeader` trait). Pair with `net_rpc_call_with_headers` / `_call_service_with_headers` / `_call_streaming_with_headers` (Phase 9b at the C ABI) so server-side filtering picks the right candidate without re-running the predicate per hop. Wire format pinned by `tests/cross_lang_capability/predicate_nrpc_envelope.json`.

### Query planner (Phase 4)

`predicate.evaluate(ctx)` runs the planned (selectivity-reordered) AST by default; `predicate.evaluate_unplanned(ctx)` exposes the raw declaration-order path for benchmarking. Planner consumes `CardinalityProvider` (a TTL-cached lookup over `by_axis_key` / `by_metadata` indexes — Phase 4 follow-on `CapabilityIndex::axis_cardinality`). Cost-based AND short-circuits cheap-false-first, cost-based OR cheap-true-first; structurally-equal clauses merge so duplicate work is single-counted.

### Chain composition helpers (Phase 6)

`caps.requireChain(hash)` / `requireAnyChain([hashes])` / `excludeChain(hash)` / `fromFork(parent)` / `heatLevel(rate)` are syntactic sugar over the underlying reserved-prefix tags (TS / Python builder shapes; the Rust `require_axis_value!` macro covers the same). Predicate-side equivalents on the `pred.*` builder.

### Predicate debug sessions (Phase 7)

`Predicate::evaluate_with_trace(ctx)` returns `(bool, ClauseTrace)` — every clause's verdict + skipped children for short-circuit AND/OR. `PredicateDebugReport::from_evaluations(&pred, contexts)` aggregates per-clause hit / miss / cost stats across a corpus; `report.render()` renders a multi-line text summary. Bindings ship a `redact_metadata_keys(report, keys)` helper for safe persistence — scrubs metadata-equality / -matches values before the report goes to disk or analytics. Wire format pinned by `tests/cross_lang_capability/predicate_trace.json` and `predicate_debug_report.json`.

---

## SDK Capability System Surface

The nine-phase rollout from `CAPABILITY_SYSTEM_SDK_PLAN.md` ships in full. Each phase landed independently per binding; all phases pass their per-binding suites and the cross-binding wire-format compat fixtures. Total ~14 K LoC across the substrate + SDK + bindings + tests, of which the binding surface accounts for ~7 K.

| Phase | Scope | Bindings |
|-------|-------|----------|
| **1** | `net-sdk` substrate-layer surface (`Tag`, `TagKey`, `CapabilitySet`, `CapabilityViews`, `Predicate`, `pred!` macro, `ValidationReport`, `CapabilitySetDiff`, `RequiredCapability` + `require!` macros). | Rust |
| **2** | Cross-binding compat fixtures under `tests/cross_lang_capability/`. Versioned via `abi_version_expected: 1`. Eight fixtures pin: `predicate_eval`, `capability_set_diff`, `capability_validation`, `predicate_trace`, `predicate_debug_report`, `predicate_debug_report_redacted`, `predicate_nrpc_envelope`, `placement_score`. | n/a |
| **3** | Node binding + `sdk-ts` capability-enhancements surface (`tagFromUserString`, `RESERVED_PREFIXES`, `requireTag`, `withMetadata`, `p` builder, `evaluatePredicate`, `predicateToRpcHeader` / `predicateFromRpcHeader`, `validateCapabilities`, `diffCapabilities`, `evaluatePredicateWithTrace`, `predicateDebugReport`, `redactMetadataKeys`, `renderDebugReport`). | TypeScript |
| **4** | Python binding + `sdk-py` parallel surface (`tag_from_user_string`, `p`, `evaluate_predicate`, `predicate_to_rpc_header`, `validate_capabilities`, `diff_capabilities`, `evaluate_predicate_with_trace`, `predicate_debug_report`, `redact_metadata_keys`). | Python |
| **5** | Go binding parallel surface (`Tag`, `Predicate{}`, `EvaluatePredicate`, `PredicateToWhereHeader`, `ValidateCapabilities`, `DiffCapabilities`, `EvaluatePredicateWithTrace`, `PredicateDebugReport`). | Go |
| **6** | `MeshDaemon` capability authoring per binding — daemons declare `required_capabilities` / `optional_capabilities`. Substrate's `net_compute_set_daemon_caps_dispatcher` + per-binding factory hooks. | All five |
| **7** | Custom `PlacementFilter` callback surface — `placement_filter_from_fn(fn)` (TS / Python / Go) + `global_placement_filter_registry().register(...)` (Rust). C ABI: `net_compute_set_placement_filter_dispatcher` + `_register_placement_filter` + `_unregister_placement_filter`. | All five |
| **9a** | Stateless `CapabilitySet` validator at the C ABI (`net_validate_capabilities`). Wire-format caps in, JSON `ValidationReport` out. Pinned by `capability_validation.json`. | C |
| **9b** | Header-bearing nRPC call variants — `net_rpc_call_with_headers`, `net_rpc_call_service_with_headers`, `net_rpc_call_streaming_with_headers`. Pair with `net_predicate_to_where_header` for predicate pushdown. | C |
| **9c** | Stateless predicate evaluator at the C ABI (`net_predicate_evaluate`) — same boolean every binding produces from the same wire-format predicate + `(tags, metadata)` context. Pinned by `predicate_eval.json`. | C |
| **9d** | Predicate debug-session helpers at the C ABI (`net_predicate_evaluate_with_trace`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys`). | C |

Cross-cutting decisions enforced by the fixture and the per-binding compat suites:

- **Wire format is byte-identical across Rust / TS / Python / Go / C.** A predicate authored in TS and shipped to a Go service via the `cyberdeck-where:` header decodes losslessly; a `CapabilitySet::diff` on Python reproduces the identical `added_tags` / `removed_tags` / `changed_metadata` shape Rust would. Drift in any binding fails that binding's own CI.
- **Reserved-prefix tags only via dedicated helpers.** `add_tag(s)` parses through `Tag::parse_user`, which rejects reserved prefixes — applications that try to emit a `scope:tenant:foo` via `add_tag` get the tag silently dropped. Use `with_tenant_scope("foo")` / `with_region_scope` / `with_subnet_local_scope` / etc. The Node + Python bindings opt into the unrestricted `Tag::parse` path (binding-layer is trusted SDK consumer; per-binding test contracts require reserved tags to round-trip through `tags: [...]`).
- **`MeshDaemon::process` panic surfaces as `RpcStatus::Internal`** — same hardening posture as v0.12's nRPC fold, applied through the daemon-caps dispatcher when caps extraction itself panics.
- **`AttributeError` is the only silently-swallowed Python error.** Every other exception from a `@property` getter for `required_capabilities` / `optional_capabilities` propagates so operators see real failures instead of phantom-empty-cap daemons.

---

## Code review hardening — `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2`

The 28-item audit ran against the `capability-system-2` branch before merge. All P1 items (must-fix-before-merge) and all P2 items (fix-soon) closed in-branch with regression tests where reasonable; the P3 items (latent) closed in the same window.

**P1 — six items.**

- **CR-1**: `CapabilitySet::has_tag` is now separator-agnostic. `caps.has_tag("software.os:linux")` matches a stored `software.os=linux`. Pre-fix, the legacy-formatted query string was compared byte-for-byte against the wire-form rendering, missing the canonical separator.
- **CR-2**: `RequiredCapability::Tag` evaluation uses `Tag::semantic_eq` instead of `PartialEq` so a `require!("software.os:linux")` hits a stored `software.os=linux` and vice versa. The `separator` field is a wire-form detail, not part of identity.
- **CR-3**: `CapabilitySet::diff` residual filter now uses `(axis, key)` membership, not exact `Tag::Eq`. Pre-fix, an input tag with `:` separator that re-encoded canonically as `=` would land in the residual diff and ship a spurious `RemoveTag` without a compensating `UpdateSoftware` — apply on the receiver dropped the tag entirely.
- **CR-4**: `DiffEngine::diff` op order is now deterministic. Residual `AddTag` / `RemoveTag` ops sort by `Tag::to_string()` before emission so `prev.diff(next)` produces byte-stable output across HashSet iteration orders.
- **CR-5**: `examples/capability.c` now compiles. Stale references to pre-Phase-A.5.N field accessors (`caps.hardware.gpu_vram_mb`) replaced with the post-A.5.N.3 view-projection path.
- **CR-6**: Custom `PlacementFilter` impls returning `NaN` are now treated as a hard veto. Pre-fix, NaN scores poisoned the sort comparator and the highest-scoring candidate could rotate non-deterministically across runs.

**P2 — eleven items.**

- **CR-7**: `CapabilityQuery::traverse` grew a visited-set to break cycles in the peer-capability graph.
- **CR-8**: Predicate trace labels redact raw metadata values by default; the `redact_metadata_keys` helper takes the explicit allow-list.
- **CR-9 / CR-10**: `StandardPlacement::saturating_score` and the anti-affinity threshold both clamp NaN to a safe value (`Some(0.0)` for the score axis, "over threshold" for anti-affinity) before composition.
- **CR-11**: compute-ffi's `parse_side` no longer leaks the consumer's malloc'd buffer when `(ptr non-NULL, len == 0)`.
- **CR-12**: Python `announce_capabilities` releases the GIL across the blocking call.
- **CR-13**: rpc-ffi's `run_cancellable` register-after-spawn TOCTOU closes — the cancel-token registers before the underlying task spawns.
- **CR-14**: Schema validator covers `metadata_reserved` keys (`tool::*::input_schema` etc.) and the four reserved tag prefixes.
- **CR-15**: Schema `ValueType::Number` rejects negative integers for fields explicitly typed unsigned.
- **CR-16**: `with_metadata(key, value)` rejects reserved-prefix keys at the builder.
- **CR-17**: `net_predicate_to_where_header` no longer leaks the partial-write buffer on encode failure.

**P3 — eleven items.** All closed: `eval_any_in_cost_order` Or-mode cost; `redact_label` correctness on metadata equality with `=` in the value; `placement_registry` invocation-counter precreate race; Phase-G v2 migration `LocalPreferred` propagation; `IntentMatchPolicy::AnyOfLocalCapabilities` empty-registry edge; bloom `h2` even-degradation on power-of-2 bit counts; `tag_codec` software-runtime / -framework names with `=`/`.`/`:` round-trip; Node + Python `fp16_tflops_x10` f32 round-trip precision; Go `RegisterPlacementFilter` / `UnregisterPlacementFilter` race; Go `tagKeyFromWire` type-assert error surfacing; `include/README.md` claim about composing `net.h` + `net.go.h` cleanly.

Each P1 + P2 fix landed with a regression test. The cross-binding fixtures gained `predicate_debug_report_redacted.json` and `placement_score.json` to pin CR-8 + CR-6 + CR-9 + CR-10 across all five bindings.

---

## Test hygiene

- **Cross-binding wire-format fixtures.** Eight golden-vector fixtures under `tests/cross_lang_capability/`, all versioned via `abi_version_expected: 1`. Drift in any binding's encode / decode / evaluate path fails that binding's CI. Each fixture drives parallel suites in Rust integration tests + Node Vitest + Python pytest + Go go-test.
- **Integration tests for the load-bearing user flows.** `integration_nrpc_predicate_header.rs` (4 tests) composes Phase 9b header passthrough with Phase 9c stateless evaluator over a real two-node mesh — pins that the predicate-as-`cyberdeck-where:`-header → server-side filter flow works end-to-end. `integration_placement_filter_callback.rs` (3 tests) registers a custom `PlacementFilter` via `global_placement_filter_registry()`, builds `StandardPlacement::with_custom_filter_id` over a populated `CapabilityIndex`, verifies the filter's verdict reaches the composed score, and unregister-mid-flight collapses to a hard veto.
- **Per-bug regression coverage from the audit.** P1 and P2 fixes each ship a regression test. The substrate gains 5 unit-level regressions (separator-canonicalization in diff, AxisPresent vs value predicates, comparator parsing order, `parse_tag_key` whitespace trim, semver `0.0.x` exact-only); rpc-ffi gains the headers-NULL-pointer guard test; bloom gains the rounding-saturation test; Python binding gains the property-getter-error-propagation test; sdk-py gains the non-string-metadata coercion + NaN/inf format test; sdk-ts gains the four `metadata*` predicates' `hasOwnProperty` parity tests.
- **Lib suite at 2314 tests** (was 2289 at v0.12 release). 25 net new tests across the regression + integration paths.
- **`cargo clippy --all-features --all-targets -D warnings` clean** across substrate + every binding crate.

---

## Breaking changes

### Wire format — `CapabilitySet` shape change

**v0.13 breaks wire compatibility with v0.12 for `CapabilityAnnouncement` / `CapabilityDiff` / any payload carrying a `CapabilitySet`.** The storage shape collapsed from seven fields (`hardware`, `software`, `models`, `tools`, `tags`, `limits`, `metadata`) to two (`tags`, `metadata`); typed projections decode lazily through `views()`. Old peers can't decode new announcements; new peers can't decode old. Per locked decision in `CAPABILITY_SYSTEM_PLAN.md` ("no backward-compatibility shim"), a synchronous fleet-wide upgrade is required for any deployment that uses capability announcements.

Forward-compat preserved within the new shape:

- **Unknown axis-prefixed tags pass through as `Tag::Legacy` on parse** for forward-compat with future schema additions. The validator emits `LegacyTag` warnings rather than errors.
- **Unknown metadata keys propagate as opaque pairs** subject to the 4 KB soft cap.
- **Reserved-prefix tag set is closed at v0.13** (`scope:` / `causal:` / `fork-of:` / `heat:`). Future reserved prefixes will land in v0.14+; v0.13 receivers will route them through `Tag::Legacy` until upgrade.

The `signed_payload()` envelope round-trip is **byte-stable across processes** thanks to the sorted-tag wire format — pre-fix, signature verification rejected announcements crossing between two processes (different RandomState seeds), silently dropping every multi-tag announcement at the receiver.

`MembershipMsg`, `IdentityEnvelope`, `EventMeta`, `CausalLink`, `OriginStamp`, `NetHeader`, RedEX on-disk layout, per-event checksum format, and every nRPC dispatch / header from v0.12 — all unchanged.

### Rust core (`net` crate) — API surface

- **`CapabilitySet`'s typed-struct fields are gone.** `caps.hardware`, `caps.software`, `caps.models`, `caps.tools`, `caps.limits` no longer exist as fields. Read through `caps.views().hardware()` (etc.) — the projection is per-axis OnceCell-cached. Write through `caps.set_hardware(hw)` / `set_software` / `set_models` / `set_tools` / `set_limits` — these clear axis-owned tags and re-emit via the codec. The `with_*` builders are thin wrappers.
- **`CapabilitySet::tags` field type changes from `Vec<String>` to `HashSet<Tag>`.** Iterations over `caps.tags` now yield typed `Tag` values; render to wire form via `t.to_string()`. Use `caps.add_tag(s)` for application-facing additions (parses through `Tag::parse_user`, rejects reserved prefixes); `caps.with_tenant_scope` / `with_region_scope` / `with_subnet_local_scope` for the dedicated reserved-tag builders.
- **`adapter::net::behavior::tag` is a new public module** re-exporting `Tag`, `TagKey`, `TaxonomyAxis`, `AxisSeparator`, `RESERVED_PREFIXES`, `CapabilityTagError`.
- **`adapter::net::behavior::tag_codec` is a new public module** re-exporting the round-trip codecs (`hardware_to_tags` / `hardware_from_tags` / `software_to_tags` / `software_from_tags` / `models_to_tags` / `models_from_tags` / `tools_to_tags` / `tools_from_tags` / `resource_limits_to_tags` / `resource_limits_from_tags`) plus the axis-owned-tag predicates (`is_hardware_owned_tag` / etc.).
- **`adapter::net::behavior::predicate` is a new public module** re-exporting `Predicate`, `EvalContext`, `ClauseTrace`, `PredicateDebugReport`, `predicate_to_rpc_header`, `predicate_from_rpc_headers`, `RPC_WHERE_HEADER`, `MAX_PREDICATE_RPC_HEADER_VALUE_LEN`, `AsRpcHeader`, `PredicateRpcEncodeError`, `PredicateRpcDecodeError`, `PredicateWire`, `PredicateNodeWire`, `RpcPredicateContext`, `filter_by_predicate`. Plus the `pred!` macro re-exported at the crate root.
- **`adapter::net::behavior::required_capability` is a new public module** re-exporting `RequiredCapability`, `RequireParseError`, plus the `require!` / `require_axis!` / `require_axis_value!` macros at the crate root.
- **`adapter::net::behavior::schema` is a new public module** re-exporting `validate_capabilities`, `ValidationReport`, `SchemaError`, `ValidationWarning`, `ValueType`, `KeyEntry`, `AxisSchema`, `AXIS_SCHEMA`, `METADATA_SOFT_CAP_BYTES`.
- **`adapter::net::behavior::bloom` is a new public module** re-exporting `BloomFilter`.
- **`adapter::net::behavior::query` is a new public module** re-exporting the `CapabilityQuery` trait.
- **`adapter::net::behavior::placement` is a new public module** re-exporting `PlacementFilter`, `Artifact`, `StandardPlacement`, `LegacyPlacement`, `IntentRegistry`, `IntentMatchPolicy`, `ColocationPolicy`, `ResourceAxis`, `AntiAffinityConfig`, `PlacementMetadataKeys`, `compose_axis_scores`, `tie_break_compare`, `LeadershipStatsLookup`, `RttLookup`, `ScopeLabel`, `TieBreakContext`, `NodeId as PlacementNodeId`.
- **`adapter::net::behavior::placement_registry` is a new public module** re-exporting `global_placement_filter_registry()`, `PlacementFilterRegistry`.

### Rust SDK (`net-sdk`)

The SDK's capability surface is entirely additive over the substrate re-exports — no existing SDK API changes outside the `CapabilitySet` shape change.

- **`net_sdk::capabilities::*` re-exports the substrate capability surface end-to-end.** New entries since v0.12: `Tag`, `TagKey`, `TaxonomyAxis`, `RESERVED_PREFIXES`, `CapabilityViews`, `CapabilitySetDiff`, `MetadataChange`, `CardinalityCache`, `CardinalityProvider`, `RequiredCapability`, `RequireParseError`, `LegacyPlacement`, `StandardPlacement`, `Artifact`, `PlacementFilter`, `IntentRegistry`, `IntentMatchPolicy`, `ColocationPolicy`, `ResourceAxis`, `AntiAffinityConfig`, `PlacementMetadataKeys`, `LeadershipStatsLookup`, `RttLookup`, `ScopeLabel`, `TieBreakContext`, `compose_axis_scores`, `tie_break_compare`, `global_placement_filter_registry`, `PlacementFilterRegistry`.
- **New submodule `net_sdk::capabilities::predicate`** re-exports `Predicate`, `EvalContext`, `ClauseTrace`, `ClauseStats`, `PredicateDebugReport`, `predicate_to_rpc_header`, `predicate_from_rpc_headers`, `AsRpcHeader`, `RpcPredicateContext`, `filter_by_predicate`, `MAX_PREDICATE_RPC_HEADER_VALUE_LEN`, `RPC_WHERE_HEADER`, plus encode / decode / wire types.
- **New submodule `net_sdk::capabilities::schema`** re-exports `validate_capabilities`, `ValidationReport`, `SchemaError`, `ValidationWarning`, `ValueType`, `KeyEntry`, `AxisSchema`, `AXIS_SCHEMA`, `METADATA_SOFT_CAP_BYTES`.
- **The `pred!` / `require!` / `require_axis!` / `require_axis_value!` macros are re-exported at the SDK crate root.**

### FFI / bindings

| Binding | Change |
|---------|--------|
| **All** | New capability-enhancements surface — typed `Tag`, predicate AST + builders, validator, diff, trace, debug-report aggregator, redaction. Cross-binding wire format is byte-identical and pinned by the eight golden-vector fixtures. |
| **All** | Reserved-prefix tag passthrough at the binding boundary now uses `Tag::parse` (not `parse_user`). SDK consumers can supply `scope:*` / `causal:*` / `fork-of:* / heat:*` via the `tags: [...]` shape; pre-fix they were silently dropped at the binding boundary. |
| **All** | `placement_filter_from_fn(fn)` / `placementFilterFromFn(fn)` registers a host-language predicate as a custom placement-filter callback. Pair with `standardPlacement(custom_filter_id=...)` / `StandardPlacement::with_custom_filter_id` to install. Substrate calls back per candidate. |
| **All** | `MeshDaemon` capability authoring — daemons declare `required_capabilities` / `optional_capabilities` via per-binding callbacks during factory registration. Substrate's `net_compute_set_daemon_caps_dispatcher` plus per-binding adapter. |
| **Node** | New SDK module `capability-enhancements.ts` exports the full surface (`tagFromUserString`, `RESERVED_PREFIXES`, `requireTag`, `requireAxisValue`, `withMetadata`, `emptyCapabilities`, `p`, `evaluatePredicate`, `predicateToRpcHeader` / `predicateFromRpcHeader`, `RPC_WHERE_HEADER`, `validateCapabilities`, `isReportValid`, `diffCapabilities`, `evaluatePredicateWithTrace`, `predicateDebugReport`, `redactMetadataKeys`, `renderDebugReport`, `placementFilterFromFn`, `standardPlacement`, plus the typed wire shapes). NAPI binding rebuild required for the post-Phase-A.5.N.3 storage shape. |
| **Python** | New module `net_sdk` exports the parallel surface (`tag_from_user_string`, `p`, `evaluate_predicate`, `predicate_to_rpc_header`, `validate_capabilities`, `diff_capabilities`, `evaluate_predicate_with_trace`, `predicate_debug_report`, `redact_metadata_keys`, `placement_filter_from_fn`, `standard_placement`). The `net._net` PyO3 binding adds `extract_optional_caps`, daemon caps dispatcher, placement-filter callback. Rebuild via `maturin develop --release` for the storage-shape change. |
| **Go** | `bindings/go/net/` adds the typed surface (`Tag`, `Predicate{}`, `EvaluatePredicate`, `PredicateToWhereHeader`, `ValidateCapabilities`, `DiffCapabilities`, `EvaluatePredicateWithTrace`, `PredicateDebugReport`, `RegisterPlacementFilter`, `UnregisterPlacementFilter`). The compute-ffi C ABI gains the placement-filter dispatcher entry points. |
| **C** | `net.go.h` exports the new error codes (`NET_COMPUTE_ERR_NO_DISPATCHER = -4`, `NET_COMPUTE_ERR_INVALID_UTF8 = -5`) and switches `mesh_arc` from `void*` to the typed opaque handle `net_compute_mesh_arc_t*`. Phase 9a / 9b / 9c / 9d entry points: `net_validate_capabilities`, `net_predicate_to_where_header`, `net_predicate_evaluate`, `net_predicate_evaluate_with_trace`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys`, `net_rpc_call_with_headers` / `_call_service_with_headers` / `_call_streaming_with_headers`. |

### Behavioral fixes that may surface as test breakage

- **`CapabilitySet` field reads now decode lazily through `views()`.** Tests that did `caps.hardware.memory_mb` directly fail to compile; rewrite as `caps.views().hardware().memory_mb`. Same for `software` / `models` / `tools` / `limits`.
- **`caps.tags.contains(&"gpu".to_string())` no longer compiles.** `tags: HashSet<Tag>` carries typed values; use `caps.has_tag("hardware.gpu")` (which is now separator-agnostic) or `caps.tags.iter().any(|t| t.to_string() == "hardware.gpu")` for the substring-style check.
- **`add_tag("scope:tenant:foo")` silently drops** at the application layer. Use `caps.with_tenant_scope("foo")`. The binding-side passthrough via `tags: [...]` works because bindings parse via the unrestricted `Tag::parse`.
- **`CapabilitySet::diff` ops now sort deterministically.** Tests that asserted specific diff-op insertion order under `Vec` semantics will see lexicographic-by-tag ordering instead.
- **`PlacementFilter::placement_score` returning `None` is a hard veto.** Pre-fix, custom impls returning `Some(0.0)` and `None` produced indistinguishable scheduler behavior; v0.13 makes `None` the explicit "exclude from ranking" signal and `Some(0.0)` the "score floor" signal. Tests asserting "filter returns None → scheduler ranks among others" will see the candidate excluded.
- **Custom `PlacementFilter` impls returning NaN are now treated as a hard veto.** Tests that injected NaN to observe sort behavior will see a deterministic exclusion.
- **`require!("software.id == v>=1.0")` parses as `Equals`, not `NumericAtLeast`.** The `==` branch now precedes `>=` / `<=` in the require-parser to handle equality values containing comparison substrings. Tests asserting the legacy "`>=` claims the split first" behavior will fail.
- **`parse_tag_key` trims whitespace around the dot.** `require!("hardware. gpu == nvidia")` now produces `TagKey::new(Hardware, "gpu")` instead of `TagKey::new(Hardware, " gpu")` — the latter silently mismatched every real tag.
- **`semver_compatible` treats `0.0.x` as exact-only.** Tests that asserted "`^0.0.1` matches `0.0.2`" will see the rejection.
- **`Tag::AxisPresent` no longer matches value-bearing predicates.** `Equals(_, "")` / `StringPrefix(_, "")` / `StringMatches(_, "")` no longer accept presence-only tags. Use `Predicate::Exists` for key-presence checks.
- **Forward-compat axis tags survive `CapabilitySet::diff`.** Pre-fix, `is_*_owned_tag` over-claimed unknown forward-compat keys (`hardware.future_field=v2`) and the residual filter dropped them; the typed `Update*` ops didn't capture them either. Real changes to forward-compat tags now ship as `AddTag` / `RemoveTag`.
- **Capability announcements emit tags in sorted wire order.** Tests asserting HashSet-iteration-order on the wire will see lexicographic ordering instead. Symptom for cross-process verification: the sorted form is what makes signature verification stable.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.13 line.** Recompile / rebuild the binding cdylib (NAPI for Node, maturin for Python, `cargo build -p net-compute-ffi` + `-p net-rpc-ffi` for Go).
2. **CapabilitySet field-access migration.** Direct field reads (`caps.hardware`, `caps.software`, etc.) move to `caps.views().hardware()` / `software()` / etc. Use `cargo build` to drive the rewrite — the compiler errors name every site. The view handle is per-axis OnceCell-cached (< 50 ns post-cache); same hot-path cost as the old direct field access.
3. **Tag iteration changes from `&str` to `&Tag`.** Render to wire form via `tag.to_string()` (the canonical `Display` impl), or pattern-match on the typed variants. `caps.has_tag("...")` works with either separator form.
4. **Reserved-prefix tag emission moves to dedicated builders.** Replace `caps.add_tag("scope:tenant:foo")` with `caps.with_tenant_scope("foo")`, etc. Application code passing reserved tags through `caps.add_tag` was already silently dropping them in v0.12 prerelease builds.
5. **Fleet-wide upgrade required for capability announcements.** v0.12 ↔ v0.13 mixed fleets cannot exchange `CapabilityAnnouncement` / `CapabilityDiff` payloads — the storage shape change is intentional. Pub/sub, mesh transport, channels, identity, subnets, NAT traversal, nRPC (the v0.12 surface) all continue to work cross-version. Recommend lockstep upgrade.
6. **For the new capability surface** — the typed taxonomy + predicate evaluator + validator + diff + trace + debug report are opt-in. Read `net/crates/net/README.md#capabilities` for the high-level surface, then per-binding READMEs for language-idiomatic usage:
   - **Rust SDK** — `net/crates/net/sdk/README.md` § "Capability enhancements (typed taxonomy + predicates + validation)". `pred!` macro + `require!` family in scope under `net_sdk::capabilities`.
   - **Node** — `net/crates/net/sdk-ts/README.md` § "Capability enhancements". Import from `@ai2070/net-sdk`.
   - **Python** — `net/crates/net/sdk-py/README.md` § "Capability enhancements". Import from `net_sdk`.
   - **Go** — `bindings/go/net/` exports the parallel surface. C-ABI entry points documented in `net/crates/net/include/README.md`.
   - **C** — `net/crates/net/include/README.md` § "Mesh function families" rows "Predicate evaluation", "Predicate `where:` header", "Capability validation", "Predicate debug session".
   Worked examples: `net/crates/net/docs/CAPABILITY_ENHANCEMENTS_USAGE.md`.
7. **Predicate-as-`cyberdeck-where:`-header → server-side filter.** Pair `predicate_to_rpc_header` with the header-bearing nRPC call variants from v0.12 (`net_rpc_call_with_headers` and friends; same surface in every binding). Server's nRPC handler decodes via `predicate_from_rpc_headers` and filters candidates with `evaluate_predicate`. The `cyberdeck-where:` header name is exported as `RPC_WHERE_HEADER` from every binding.
8. **Daemon capability authoring.** Daemons that want to participate in capability-driven placement implement `required_capabilities` / `optional_capabilities`. The runtime publishes both as part of the daemon's identity-bound announcement. Per-binding integration via the daemon-caps dispatcher (TS / Python: factory callback; Go: `RegisterDaemonCaps`; C: `net_compute_set_daemon_caps_dispatcher`).
9. **Custom placement-filter callbacks.** When the built-in `StandardPlacement` axes don't fit a placement rule, plug a host-language predicate via `placement_filter_from_fn(closure)` (TS / Python / Go) or implement `PlacementFilter` directly + register via `global_placement_filter_registry()` (Rust). Pair with `StandardPlacement::with_custom_filter_id(id)`.
10. **Cross-binding consumers** — every binding's wire format is pinned by the eight golden-vector fixtures under `tests/cross_lang_capability/`. If you're integrating predicates / capability sets / debug reports across language boundaries, your wire-level compatibility is enforced at the binding's own CI. Fixtures versioned via `abi_version_expected: 1`.
11. **If you wired your own placement scoring around `Mikoshi::select_migration_target` or scheduler internals** — the v0.13 path consults `StandardPlacement` with optional custom-filter callback. `LegacyPlacement` preserves v0.12 behavior under a feature flag for one minor version; new code should target `StandardPlacement`.
12. **If you have caches keyed off the old `CapabilitySet` shape on disk** — the storage shape changed. Bust the cache or rewrite via the new shape. The view-projection layer is read-only over the typed tags + metadata, so encoding via `set_hardware(hw)` etc. produces the canonical tag set; subsequent `views().hardware()` reads back identically.

---

Released 2026-05-10.

## License

See [LICENSE](../../LICENSE).
