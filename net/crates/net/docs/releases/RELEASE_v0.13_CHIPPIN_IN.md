# Net v0.13 — "Chippin' In"

v0.13 lands the **capability system** end-to-end across the substrate and all five bindings. v0.12 ("Firestarter") shipped nRPC; v0.13 makes capability the load-bearing layer underneath. The `Tag` placeholder in v0.10 / v0.11, and the untyped `Vec<String>` shape v0.12 still carried, both go away — `CapabilitySet` is now a `{ tags: HashSet<Tag>, metadata: BTreeMap }` typed-taxonomy wire shape, every binding ships the same `Predicate` AST + evaluator + validator + diff + trace + debug-report aggregator, and predicates ride nRPC request headers (`cyberdeck-where:`) so server-side filtering picks the right candidate without re-running the predicate per hop.

The hardening posture from the Black Diamond line is intact — every new surface ships with handle-lifetime, panic-safety, and FFI-soundness guarantees consistent with v0.11 / v0.12 — but this release is about replacing the placeholder with the real thing.

---

## Capability System (substrate)

### Typed taxonomy

The flat tag namespace becomes a four-axis ontology — `hardware` / `software` / `devices` / `dataforts` — backed by a typed `Tag` enum:

```rust
pub enum Tag {
    AxisPresent { axis: TaxonomyAxis, key: String },
    AxisValue   { axis: TaxonomyAxis, key: String, value: String, separator: AxisSeparator },
    Reserved    { prefix: String, body: String },   // scope:* / causal:* / fork-of:* / heat:*
    Legacy(String),                                  // untyped strings outside the typed taxonomy
}
```

`Tag::parse(s)` accepts every shape including reserved-prefix tags (the deserializer + substrate-internal callers); `Tag::parse_user(s)` rejects reserved prefixes for application input. `TagKey` (`(axis, key)`) is the half-form `Predicate` matches on. `TaxonomyAxis::all()` enumerates the four axes for iteration.

Axis values accept either `=` or `:` as the separator on the wire (`hardware.gpu.vram_mb=24576` and `hardware.gpu:nvidia` both parse). The `separator` is preserved through `Tag::Eq` for byte-stable round-trips, and `tag.semantic_eq(other)` is the separator-agnostic comparison for tag matching.

### Tag shapes for discovery

Reserved-prefix tag shapes flesh out the discovery primitive. `causal:<hex>` / `causal:<hex>:<tip_seq>` / `causal:<hex>[<range>]` for chain holders; `fork-of:<parent_hex>` for chain ancestry; `heat:<chain_hex>=<rate>` for hot-chain advertisement; `scope:tenant:<id>` / `scope:region:<name>` / `scope:subnet-local` (`scope:*` was already in v0.12, now formally part of the taxonomy). `RESERVED_PREFIXES` constant exposes the full list for binding-level enforcement.

### Metadata field

`CapabilitySet` storage shape collapses to two fields:

```rust
pub struct CapabilitySet {
    pub tags: HashSet<Tag>,
    pub metadata: BTreeMap<String, String>,
}
```

`HardwareCapabilities` / `SoftwareCapabilities` / `Vec<ModelCapability>` / `Vec<ToolCapability>` / `ResourceLimits` are *projections* — derived on demand via `caps.views()`. Encoding scheme: `hardware.cpu_cores=N` / `hardware.gpu` / `hardware.gpu.vram_mb=N` / `software.os=linux` / `software.model.0.id=...` / `hardware.limits.max_concurrent_requests=N`. Tool JSON-Schema strings (which can't safely round-trip through the tag wire format) live in `metadata` under `tool::<id>::input_schema` / `tool::<id>::output_schema`. Application-defined metadata keys propagate as opaque pairs (subject to a 4 KB soft cap with a `MetadataOversize` warning at the validator layer).

Wire format emits tags in sorted `Tag::to_string()` order — the `HashSet` keeps O(1) membership for in-memory lookups; the `serialize_with` hook flattens to a sorted `Vec` on the way out. Without this, two ends of a signed announcement round-trip would produce different bytes (HashSet iteration is process-local random) and the verifier would reject as `InvalidSignature`.

### Bloom-filter primitive

`behavior::bloom::BloomFilter` (`{ len_bits, k, bits: Vec<u64> }`) backs compact chain-tag membership probes via xxh3-128 double-hashing. ~1% FPR at 10 K items in ≤ 500 KB per the substrate sizing target. Probe pattern: callers that match the bloom run a follow-up precise lookup (existing `causal:<hex>` tag membership) before issuing real reads — false positives become recoverable misses, false negatives are impossible by construction. Domain-separated via `BLOOM_HASH_SEED = 0xB100_F1AC_DEAD_CAFE` so callers using xxh3 elsewhere don't accidentally collide.

`BloomFilter::new(expected_items, false_positive_rate)` clamps degenerate inputs (`expected_items == 0` → 1, `p` clamped to `(1e-9, 0.5)`); `BloomFilter::with_params(len_bits, k)` is the explicit-parameters constructor for cross-binding fixtures. Round-trips via serde with explicit deserialize-side validation (rejects out-of-range `k`, mismatched `len_bits`/`bits.len() * 64`).

### Federated query primitives

`behavior::query::CapabilityQuery` lifts five composable ops over `CapabilityIndex`:

- `filter(predicate)` — predicate-driven candidate set.
- `match_axis(axis, key)` — axis-shaped tag scan.
- `aggregate(key, reduction)` — per-key cardinality / numeric reductions.
- `traverse(seed, edge_fn, depth)` — graph-style join over peer capability links.
- `nearest(predicate, k, proximity)` — combine with proximity to score the top-K best matches.

Implementations on `CapabilityIndex` are O(log n) for indexed predicates and O(n) for the residual scan. The `Predicate` AST and these five ops together are what `Mesh::find_nodes_by_filter` / `find_best_node_scoped` flow through.

### `PlacementFilter` trait + `StandardPlacement`

`PlacementFilter::placement_score(target, artifact) -> Option<f32>` is the substrate-level placement primitive. `Some(score)` admits the candidate at a fitness in `[0, 1]`; `None` is a hard veto. `Artifact` carries the workload type — `Chain` (causal-chain placement), `Replica` (channel replica placement), `Daemon` (compute placement, with `required` + `optional` capability sets).

`StandardPlacement` is the multi-axis reference implementation: scope filter, proximity max-RTT, intent matching (`AnyOfLocalCapabilities` / `StrictMatch` / `Custom`), colocation policy (`Ignore` / `SoftPreference` / `StrictRequired`), resource axis (`Storage` / `Compute` / `Both`), anti-affinity config (leadership-concentration penalty), and a custom-filter axis that consumes a registered host-language `PlacementFilter` via `with_custom_filter_id(id)`. Axes compose multiplicatively; `None` on any axis is a hard veto. Per-axis tie-breaking via the locked RTT → free-resource → lexicographic-NodeId chain (`tie_break_compare`).

`IntentRegistry::register(intent, &[required])` registers per-intent placement requirements built from the `require!` / `require_axis!` / `require_axis_value!` macros. Substrate ships defaults for the four canonical intents (`ml-training`, `inference`, `embedding-cache`, `tool-call`); per-deployment overrides land via the SDK.

`global_placement_filter_registry()` is the process-wide singleton mapping registered IDs to `Arc<dyn PlacementFilter>`. Bindings register their language-specific wrappers here; the scheduler resolves an SDK ID to an impl before scoring. Registration is open-by-default — the registry refuses overwrites of an existing ID (`register` returns `false`) so two bindings can't accidentally clobber each other's filters.

### Mikoshi integration

`Mikoshi::select_migration_target(daemon, scope)` consults `PlacementFilter` end-to-end. `LegacyPlacement` preserves the v0.12 ad-hoc selection under a feature flag for one minor version; new daemons should target `StandardPlacement`. `ReplicaGroup::select_member_node` and `StandbyGroup::select_promotion_target` route through the same scorer so replication / hot-standby promotion get the same axis-composed verdict as initial placement.

Daemon authors declare `MeshDaemon::required_capabilities()` and `optional_capabilities()`; the runtime publishes both as part of the daemon's identity-bound announcement so the placement scheduler — and any custom filter — can consult them. Bindings expose the same hook through their daemon-caps dispatcher (`net_compute_set_daemon_caps_dispatcher` at the C ABI; the equivalent Python / TS / Go callback during factory registration).

---

## Capability Enhancements (substrate refinements)

None of these change the wire format — they sit on top of the typed-taxonomy primitive and pay for themselves at the application layer.

### Lazy view projections + diff

`caps.views()` returns a `CapabilityViews` handle whose per-axis fields decode-and-cache on first access. Hot-path `caps.views().hardware().memory_mb` is < 50 ns post-cache; first call is the per-tag scan. Cache invalidates compiler-enforced via the `&caps` borrow held by `views()`.

`caps.diff(prev)` returns `CapabilitySetDiff { added_tags, removed_tags, changed_metadata }` for cheap before/after change detection. `MetadataChange::{Added, Removed, Updated}` per-key with old/new values. Powers event-driven placement, capability-change dashboards, and delta-based metadata propagation.

### Axis schemas

`AXIS_SCHEMA` is the canonical per-axis schema baked into the substrate at build time: known keys per axis, value types (`Presence` / `Number` / `String` / `Enumeration` / `Bool` / `Csv`), indexed-collection shapes (`software.model.<i>.*` / `software.tool.<i>.*` / `hardware.accelerator.<i>.*`). `validate_capabilities(caps)` runs the schema against a `CapabilitySet` and returns a `ValidationReport` of `errors` (operator-must-fix: `UnknownAxis`, `TypeMismatch`, `IndexMalformed`) + `warnings` (forward-compat / hygiene: `UnknownKey`, `MetadataOversize`, `LegacyTag`). Both lists are sorted by JSON-stringified entry so cross-binding fixture comparisons stay order-independent. Each binding regenerates its language-side schema from the same authoritative `CAPABILITIES_SCHEMA.md` doc.

### Predicate AST + nRPC headers

`behavior::predicate::Predicate` is the typed AST. Variants: `Exists` / `Equals` / `NumericAtLeast` / `NumericAtMost` / `NumericInRange` / `SemverAtLeast` / `SemverAtMost` / `SemverCompatible` / `StringPrefix` / `StringMatches` / `MetadataExists` / `MetadataEquals` / `MetadataMatches` / `MetadataNumericAtLeast` / `And` / `Or` / `Not`. Built via the `pred!` macro in Rust, language-idiomatic builders in every other binding (`p.and([...])`, `p.exists(tagKey('hardware', 'gpu'))`, etc.). Evaluated against an `EvalContext` constructed from any `(tags, metadata)` pair.

Predicates encode losslessly to a `cyberdeck-where:` nRPC header pair via `predicate_to_rpc_header`; the receiver decodes via `predicate_from_rpc_headers` (consumes any iterable of `(name, value_bytes)` pairs through the `AsRpcHeader` trait). Pair with `net_rpc_call_with_headers` / `_call_service_with_headers` / `_call_streaming_with_headers` at the C ABI so server-side filtering picks the right candidate without re-running the predicate per hop. Decode-side enforces the encode-side size cap symmetrically — oversize payloads surface as `PredicateRpcDecodeError::Oversize` instead of walking serde's recursive parse on attacker-shaped input. Wire format pinned by `tests/cross_lang_capability/predicate_nrpc_envelope.json`.

### Query planner

`predicate.evaluate(ctx)` runs the planned (selectivity-reordered) AST by default; `predicate.evaluate_unplanned(ctx)` exposes the raw declaration-order path for benchmarking. Planner consumes `CardinalityProvider` (a TTL-cached lookup over `by_axis_key` / `by_metadata` indexes via `CapabilityIndex::axis_cardinality`). Cost-based AND short-circuits cheap-false-first, cost-based OR cheap-true-first; structurally-equal clauses merge so duplicate work is single-counted. Cardinality casts saturate on `u32::MAX` so fleets with unbounded-cardinality metadata keys (session id, request id) don't wrap and mis-rank the most-selective key.

### Chain composition helpers

`caps.requireChain(hash)` / `requireAnyChain([hashes])` / `excludeChain(hash)` / `fromFork(parent)` / `heatLevel(rate)` are syntactic sugar over the underlying reserved-prefix tags (TS / Python builder shapes; the Rust `require_axis_value!` macro covers the same). Predicate-side equivalents on the `pred.*` builder.

### Predicate debug sessions

`Predicate::evaluate_with_trace(ctx)` returns `(bool, ClauseTrace)` — every clause's verdict + skipped children for short-circuit AND/OR. `PredicateDebugReport::from_evaluations(&pred, contexts)` aggregates per-clause hit / miss / cost stats across a corpus; `report.render()` renders a multi-line text summary. Bindings ship a `redact_metadata_keys(report, keys)` helper for safe persistence — scrubs metadata-equality / -matches values before the report goes to disk or analytics. Wire format pinned by `tests/cross_lang_capability/predicate_trace.json` and `predicate_debug_report.json`.

---

## SDK Capability System Surface

Every binding ships the same capability surface. Total ~14 K LoC across the substrate + SDK + bindings + tests, of which the binding surface accounts for ~7 K. The substrate primitives (`Tag`, `TagKey`, `CapabilitySet`, `CapabilityViews`, `Predicate`, `pred!` macro, `ValidationReport`, `CapabilitySetDiff`, `RequiredCapability` + `require!` macros) re-export through `net-sdk::capabilities`. Per-binding surfaces:

| Binding | Surface |
|---------|---------|
| **Node / TypeScript** | `sdk-ts` exports `tagFromUserString`, `RESERVED_PREFIXES`, `requireTag`, `withMetadata`, the `p` predicate builder, `evaluatePredicate`, `predicateToRpcHeader` / `predicateFromRpcHeader`, `validateCapabilities`, `diffCapabilities`, `evaluatePredicateWithTrace`, `predicateDebugReport`, `redactMetadataKeys`, `renderDebugReport`, `placementFilterFromFn`, `standardPlacement`. |
| **Python** | `sdk-py` exports the parallel surface as `tag_from_user_string`, `p`, `evaluate_predicate`, `predicate_to_rpc_header`, `validate_capabilities`, `diff_capabilities`, `evaluate_predicate_with_trace`, `predicate_debug_report`, `redact_metadata_keys`, `placement_filter_from_fn`, `standard_placement`. |
| **Go** | `bindings/go/net/` exports `Tag`, `Predicate{}`, `EvaluatePredicate`, `PredicateToWhereHeader`, `ValidateCapabilities`, `DiffCapabilities`, `EvaluatePredicateWithTrace`, `PredicateDebugReport`, `RegisterPlacementFilter`, `UnregisterPlacementFilter`. |
| **C ABI** | Stateless evaluator (`net_predicate_evaluate`), stateless validator (`net_validate_capabilities`), debug-session helpers (`net_predicate_evaluate_with_trace`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys`), `cyberdeck-where:` header builder (`net_predicate_to_where_header`), and header-bearing nRPC call variants (`net_rpc_call_with_headers`, `net_rpc_call_service_with_headers`, `net_rpc_call_streaming_with_headers` plus cancellable streaming variants). |
| **All bindings** | `MeshDaemon` capability authoring — daemons declare `required_capabilities` / `optional_capabilities` via per-binding factory hooks plumbed through `net_compute_set_daemon_caps_dispatcher`. Custom `PlacementFilter` callbacks via `placement_filter_from_fn(fn)` (TS / Python / Go) or `global_placement_filter_registry().register(...)` (Rust). |

Eight cross-binding wire-format fixtures under `tests/cross_lang_capability/` (`predicate_eval`, `capability_set_diff`, `capability_validation`, `predicate_trace`, `predicate_debug_report`, `predicate_debug_report_redacted`, `predicate_nrpc_envelope`, `placement_score`) pin the byte-identical contract across Rust / TS / Python / Go / C and are versioned via `abi_version_expected: 1`.

Cross-cutting invariants the fixtures and per-binding compat suites enforce:

- **Wire format is byte-identical across Rust / TS / Python / Go / C.** A predicate authored in TS and shipped to a Go service via the `cyberdeck-where:` header decodes losslessly; a `CapabilitySet::diff` on Python reproduces the identical `added_tags` / `removed_tags` / `changed_metadata` shape Rust would. Drift in any binding fails that binding's own CI.
- **Numeric / semver parse semantics agree with Rust.** Every binding's `f64` parser accepts exactly Rust's `f64::from_str` set (decimal, scientific, leading `+`, `.5`, `1.`, `inf`, `infinity`, `NaN`) and rejects hex floats / digit-separator underscores. Every binding's semver parser accepts only ASCII digits with optional leading `+`. Validators bound `Number` values at `u64::MAX` and reject negatives; indexed-collection indices bound at `u32::MAX`.
- **`AxisPresent` tags don't satisfy value predicates.** `Equals(_, "")` / `StringPrefix(_, "")` / `StringMatches(_, "")` never spuriously match a presence-only tag — only the `Exists` predicate does. `CapabilitySet::diff` is separator-agnostic on `AxisValue` tags (`hardware.k=v` and `hardware.k:v` carry identical semantics).
- **Reserved-prefix tags only via dedicated helpers.** `add_tag(s)` parses through `Tag::parse_user`, which rejects reserved prefixes — applications that try to emit a `scope:tenant:foo` via `add_tag` get the tag silently dropped. Use `with_tenant_scope("foo")` / `with_region_scope` / `with_subnet_local_scope` / etc. Bindings opt into the unrestricted `Tag::parse` path so reserved tags round-trip through `tags: [...]`. Metadata writers gate on the same reserved-prefix list. The schema validator surfaces collisions and oversize as warnings.
- **`MeshDaemon::process` panic surfaces as `RpcStatus::Internal`** — same hardening posture as v0.12's nRPC fold, applied through the daemon-caps dispatcher when caps extraction itself panics.
- **`AttributeError` is the only silently-swallowed Python error.** Every other exception from a `@property` getter for `required_capabilities` / `optional_capabilities` propagates so operators see real failures instead of phantom-empty-cap daemons.

---

## Hardening

The capability surface landed alongside two parallel audits whose fixes are integrated into the surface descriptions above. The substantive results, grouped by area:

### Wire-format determinism and separator agnosticism

- `CapabilitySet::has_tag` and `RequiredCapability::Tag` evaluate via `Tag::semantic_eq` so `caps.has_tag("software.os:linux")` matches a stored `software.os=linux` and vice versa. The `separator` field is a wire-form detail, not part of identity.
- `CapabilitySet::diff` is separator-agnostic and emits ops in deterministic lexicographic-by-tag order. Pre-fix HashMap iteration randomized the op order, and an input tag with `:` separator that re-encoded canonically as `=` shipped a phantom `RemoveTag` without a compensating `UpdateSoftware` — receivers dropped the tag entirely. Same fix applied to the TS `diffCapabilities` rewrite (semantic comparison on `(kind, axis, key, value)`).
- Capability announcements emit tags in sorted wire order so signed announcements verify byte-stably across processes (HashSet iteration is process-local random; pre-fix verification rejected multi-tag announcements crossing between two processes).
- Forward-compat axis tags survive `CapabilitySet::diff` as `AddTag` / `RemoveTag`; the `is_*_owned_tag` predicates no longer over-claim unknown forward-compat keys.

### Predicate / placement correctness

- Custom `PlacementFilter` impls returning `None` or `NaN` are hard vetoes — pre-fix NaN scores poisoned the sort comparator and the highest-scoring candidate could rotate non-deterministically. `StandardPlacement::saturating_score`, the anti-affinity threshold, and `target_axis_value_numeric` all clamp NaN / out-of-range values before composition; `score_resource_axis::Both` collapses to whichever axis carried data (rather than diluting against a permissive `1.0` placeholder for a no-data axis).
- `score_custom_filter_axis` resolves outside the `with_caps` closure so an FFI-registered filter that calls back into the index (`index.query(...)` from a `LegacyPlacement` shim, JS callback hitting `find_nodes`) can't deadlock against a concurrent `index.index(...)` insert.
- `Scheduler::select_migration_target` carries the LocalPreferred fast-path so RTT-aware operators feeding their own `TieBreakContext` don't silently lose the network-hop-avoidance behavior. `place_migration_v2` derives the right `PlacementReason` from the returned node id.
- `CapabilityQuery::traverse` carries a visited-set so cycles in the peer-capability graph terminate. `eval_any_in_cost_order` ranks Or composites cheap-true-first; `redact_label` searches every separator position so metadata-equality values containing `=` round-trip cleanly.
- `Tag::AxisPresent` no longer matches value-bearing predicates. `Equals(_, "")` / `StringPrefix(_, "")` / `StringMatches(_, "")` only match `AxisValue` tags; `Predicate::Exists` is the dedicated presence-check path in every binding.

### Cross-binding numeric / semver agreement

- Every binding's `f64` parser accepts exactly Rust's `f64::from_str` accepted-set (decimal, scientific, leading `+`, `.5`, `1.`, `inf`, `infinity`, `NaN`) and rejects hex floats (`0x1p3`) and digit-separator underscores (`1_000`) that Go's `strconv.ParseFloat` and Python's `float()` would otherwise accept. Numeric leaves run through IEEE comparison so NaN never matches and ±inf compare correctly across bindings.
- Schema `Number` validators bound at `u64::MAX` and reject negatives; indexed-collection indices bound at `u32::MAX`. ASCII digits only with optional leading `+` — Unicode digits (Arabic-Indic, fullwidth) parse cleanly under Python's `int()` but Rust's `u64::from_str` rejects them, so the predicate-side and schema-side parsers both lock to `^\+?[0-9]+$`.
- Semver parsers reject Unicode digits in the version components; `0.0.x` is exact-only (every patch is a breaking change boundary per Cargo's caret rule); `0.x.y` requires `lhs.major == 0`.
- `parse_tag_key` trims whitespace around the dot, `require!` parses `==` before `>=` / `<=` so equality values containing comparison substrings parse correctly. `Tag::parse_user` rejects reserved prefixes consistently across bindings; `with_metadata` filters reserved-prefix keys at the writer.

### FFI / binding hardening

- `predicate_from_rpc_headers` enforces the decode-side size cap symmetrically with the encode side — parse-bomb-shaped payloads surface as `PredicateRpcDecodeError::Oversize` instead of walking serde's recursive parse.
- `dynamic_cost` / `dynamic_cost_or` saturate `usize` cardinality to `u32::MAX` so long-running fleets with unbounded-cardinality metadata keys (session id, request id) don't trip the planner into treating the most-selective key as if it had only one distinct value.
- `placement_registry::register` pre-creates the per-binding invocation counter only on successful insertion — id-collision register-fail paths don't leak phantom Prometheus binding-counters.
- Bloom-filter `h2` forces odd-only so power-of-2 bit-count probe cycles cover the full bit range; the rounding-saturation path is unit-tested.
- compute-ffi's `parse_side` and `net_compute_snapshot_bytes_free` correctly free `(non-NULL ptr, len == 0)` malloc'd buffers.
- rpc-ffi's `run_cancellable` carries a `cancelled` flag for register-after-spawn ordering; the cancel-token registry evicts stale orphan entries; `net_predicate_to_where_header` recovers from partial-write failure. Streaming-call construction is cancellable end-to-end via `net_rpc_call_streaming_cancellable` and `net_rpc_call_streaming_with_headers_cancellable` (pre-existing non-cancellable variants kept for back-compat).
- Python `announce_capabilities` releases the GIL across the blocking call. Python-binding property-getter errors propagate (except `AttributeError`) so misbehaving daemon-caps callbacks surface real failures instead of phantom-empty-cap daemons. The Python `_try_parse_float` rejects whitespace-padded inputs to match Rust's strictness.
- Go binding's `RegisterPlacementFilter` / `UnregisterPlacementFilter` serialize on the same id to close a registry-vs-substrate race; `tagKeyFromWire` surfaces type-assert failures.
- Node + Python `fp16_tflops_x10` bypasses the f32 round-trip that previously lost precision above 2²⁴ for direct large-value passthrough.
- `tag_codec` rejects software runtime / framework / driver names containing the separator characters `=` / `:` / `.` so round-trips through the canonical wire format don't silently truncate.

### Go cgo surface widening — `origin_hash` uint32 → uint64

`go/net.h` declared every `origin_hash` parameter and return type as `uint32_t`, while the canonical `net.go.h` and the Rust `extern "C"` signatures use `uint64_t` / `u64`. Pre-fix the cgo boundary silently truncated the upper 32 bits of every origin_hash. Closed before merge:

- **C header** — `net_identity_origin_hash`, `net_compute_daemon_handle_origin_hash`, `net_compute_migration_handle_origin_hash`, `net_compute_fork_group_parent_origin`, `net_compute_standby_group_active_origin` (all now `uint64_t` return). `net_tasks_adapter_open`, `net_memories_adapter_open`, `net_compute_runtime_stop`, `net_compute_runtime_deliver`, `net_compute_runtime_snapshot`, `net_compute_start_migration`, `net_compute_expect_migration`, `net_compute_migration_phase`, `net_compute_replica_group_route_event` (`out_origin`), `net_compute_standby_group_promote` (`out_origin`), `net_compute_fork_group_spawn` (`parent_origin`) (all now `uint64_t` parameter / out-parameter).
- **Production Go binding** — `Identity.OriginHash() uint64`, `DaemonHandle.OriginHash() uint64`, `MigrationHandle.OriginHash() uint64`, `ForkGroup.ParentOrigin() uint64`, `StandbyGroup.ActiveOrigin() uint64`, `StandbyGroup.Promote() uint64`, `ReplicaGroup.RouteEvent() uint64`. `DaemonRuntime.{Stop, Snapshot, Deliver, StartMigration, ExpectMigration, MigrationPhase}` parameters, `NewForkGroup`'s `parentOrigin`, `OpenTasks` / `OpenMemories`'s `originHash` parameter (all `uint64`).
- **Public Go types** — `CausalEvent.OriginHash` is `uint64` (changed from `uint32`); `GroupMemberInfo.OriginHash` is `uint64`; `GroupForkRecord.{OriginalOrigin, ForkedOrigin}` are `uint64`.

**Breaking change for downstream Go consumers.** Code calling `daemon.OriginHash()` and assigning to a `uint32` variable will fail to compile; drop the explicit `uint32(...)` cast or convert to `uint64`. The widening matches the Rust substrate's `u64` shape.

### Regression coverage

Every correctness fix above ships with a regression test. The cross-binding fixture corpus grew from five JSON files at branch start to thirteen: `predicate_eval`, `capability_set_diff`, `capability_validation`, `predicate_trace`, `predicate_debug_report`, `predicate_debug_report_redacted`, `predicate_nrpc_envelope`, `placement_score`, plus five new rows pinning numeric-parser parity, separator-strip parity, and schema range-check agreement across Rust / TS / Python / Go / C.

---

## Test hygiene

- **Cross-binding wire-format fixtures.** Thirteen golden-vector fixtures under `tests/cross_lang_capability/`, all versioned via `abi_version_expected: 1`. Drift in any binding's encode / decode / evaluate path fails that binding's CI. Each fixture drives parallel suites in Rust integration tests + Node Vitest + Python pytest + Go go-test.
- **Integration tests for the load-bearing user flows.** `integration_nrpc_predicate_header.rs` (4 tests) composes header-bearing nRPC call variants with the stateless evaluator over a real two-node mesh — pins that the predicate-as-`cyberdeck-where:`-header → server-side filter flow works end-to-end. `integration_placement_filter_callback.rs` (3 tests) registers a custom `PlacementFilter` via `global_placement_filter_registry()`, builds `StandardPlacement::with_custom_filter_id` over a populated `CapabilityIndex`, verifies the filter's verdict reaches the composed score, and unregister-mid-flight collapses to a hard veto.
- **Lib suite at 2330+ tests** (was 2289 at v0.12 release). 40+ net new tests across the regression + integration paths, every correctness fix above shipping with at least one regression.
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
| **Node** | New SDK module `capability-enhancements.ts` exports the full surface (`tagFromUserString`, `RESERVED_PREFIXES`, `requireTag`, `requireAxisValue`, `withMetadata`, `emptyCapabilities`, `p`, `evaluatePredicate`, `predicateToRpcHeader` / `predicateFromRpcHeader`, `RPC_WHERE_HEADER`, `validateCapabilities`, `isReportValid`, `diffCapabilities`, `evaluatePredicateWithTrace`, `predicateDebugReport`, `redactMetadataKeys`, `renderDebugReport`, `placementFilterFromFn`, `standardPlacement`, plus the typed wire shapes). NAPI binding rebuild required for the new storage shape. |
| **Python** | New module `net_sdk` exports the parallel surface (`tag_from_user_string`, `p`, `evaluate_predicate`, `predicate_to_rpc_header`, `validate_capabilities`, `diff_capabilities`, `evaluate_predicate_with_trace`, `predicate_debug_report`, `redact_metadata_keys`, `placement_filter_from_fn`, `standard_placement`). The `net._net` PyO3 binding adds `extract_optional_caps`, daemon caps dispatcher, placement-filter callback. Rebuild via `maturin develop --release` for the storage-shape change. |
| **Go** | `bindings/go/net/` adds the typed surface (`Tag`, `Predicate{}`, `EvaluatePredicate`, `PredicateToWhereHeader`, `ValidateCapabilities`, `DiffCapabilities`, `EvaluatePredicateWithTrace`, `PredicateDebugReport`, `RegisterPlacementFilter`, `UnregisterPlacementFilter`). The compute-ffi C ABI gains the placement-filter dispatcher entry points. |
| **Go** | **`origin_hash` widened from `uint32` to `uint64` end-to-end.** Public methods (`Identity.OriginHash()`, `DaemonHandle.OriginHash()`, `MigrationHandle.OriginHash()`, `ForkGroup.ParentOrigin()`, `StandbyGroup.{ActiveOrigin, Promote}()`, `ReplicaGroup.RouteEvent()`) return `uint64`; `DaemonRuntime.{Stop, Snapshot, Deliver, StartMigration, ExpectMigration, MigrationPhase}` parameters and `NewForkGroup`'s `parentOrigin` take `uint64`; `CausalEvent.OriginHash`, `GroupMemberInfo.OriginHash`, `GroupForkRecord.{OriginalOrigin, ForkedOrigin}` are `uint64`. Pre-fix the cgo boundary silently truncated the upper 32 bits of every origin_hash. Same widening applied to the `cortex` adapters (`OpenTasks` / `OpenMemories` take `uint64` `originHash`). Breaking change for downstream Go consumers — `uint32` callsites need explicit `uint64(...)` conversion. |
| **Go** | **Cancellable streaming-call entry points.** `net_rpc_call_streaming_cancellable` and `net_rpc_call_streaming_with_headers_cancellable` add a `cancel_token` parameter so a parallel `net_rpc_cancel_call` can abort the construction `block_on` before the stream handle materializes. Pre-existing non-cancellable variants kept for back-compat. |
| **C** | `net.go.h` exports the new error codes (`NET_COMPUTE_ERR_NO_DISPATCHER = -4`, `NET_COMPUTE_ERR_INVALID_UTF8 = -5`) and switches `mesh_arc` from `void*` to the typed opaque handle `net_compute_mesh_arc_t*`. New capability entry points: `net_validate_capabilities`, `net_predicate_to_where_header`, `net_predicate_evaluate`, `net_predicate_evaluate_with_trace`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys`, `net_rpc_call_with_headers` / `_call_service_with_headers` / `_call_streaming_with_headers`. |

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
10. **Cross-binding consumers** — every binding's wire format is pinned by the thirteen golden-vector fixtures under `tests/cross_lang_capability/`. If you're integrating predicates / capability sets / debug reports across language boundaries, your wire-level compatibility is enforced at the binding's own CI. Fixtures versioned via `abi_version_expected: 1`.
11. **If you wired your own placement scoring around `Mikoshi::select_migration_target` or scheduler internals** — the v0.13 path consults `StandardPlacement` with optional custom-filter callback. `LegacyPlacement` preserves v0.12 behavior under a feature flag for one minor version; new code should target `StandardPlacement`.
12. **If you have caches keyed off the old `CapabilitySet` shape on disk** — the storage shape changed. Bust the cache or rewrite via the new shape. The view-projection layer is read-only over the typed tags + metadata, so encoding via `set_hardware(hw)` etc. produces the canonical tag set; subsequent `views().hardware()` reads back identically.
13. **Go consumers — `origin_hash` widened to `uint64`.** Callsites assigning `daemon.OriginHash()` (or `Identity.OriginHash()` / `migration.OriginHash()` / `replica.RouteEvent()` / `fork.ParentOrigin()` / `standby.{ActiveOrigin, Promote}()`) to a `uint32` variable fail to compile. Drop the explicit cast (or convert to `uint64`); the canonical Rust shape is u64 and the Go binding's previous u32 silently truncated the upper 32 bits. `CausalEvent.OriginHash`, `GroupMemberInfo.OriginHash`, `GroupForkRecord.{OriginalOrigin, ForkedOrigin}` are now `uint64`; `DaemonRuntime.{Stop, Snapshot, Deliver, StartMigration, ExpectMigration, MigrationPhase}` parameters and `OpenTasks` / `OpenMemories` / `NewForkGroup`'s `originHash` / `parentOrigin` take `uint64`.
14. **Streaming RPC consumers wanting cancellation during construction** — switch from `net_rpc_call_streaming` / `net_rpc_call_streaming_with_headers` to the new `*_cancellable` variants and pass a `cancel_token` from `net_rpc_reserve_cancel_token`. A parallel `net_rpc_cancel_call(token)` now aborts the construction `block_on` (peer-stalled initial-frame ACK), where pre-fix `net_rpc_stream_close` only took effect after the stream handle was already constructed. Existing non-cancellable variants kept for back-compat.

---

Released 2026-05-11.

## License

See [LICENSE](../../LICENSE).
