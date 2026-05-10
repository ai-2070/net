# Cross-binding capability fixtures

Golden-vector fixtures pinning the wire format of capability-system features that bindings (Node / Python / Go) consume. Same pattern as `tests/cross_lang_nrpc/golden_vectors.json` for nRPC; each binding's compat test loads the same fixture and asserts byte-identical encoding / structural equivalence with the Rust reference.

Phase 5.B + 1 of `docs/plans/CAPABILITY_ENHANCEMENTS_PLAN.md`. Surfaced through Phase 9 of `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md`.

## Files

### `predicate_nrpc_envelope.json`

Pins the `Predicate` → `PredicateWire` → JSON-encoded header value contract:

- The `wire` field of each case is the canonical JSON-encoded `PredicateWire` that lands in the `cyberdeck-where` request header (per `behavior::predicate::predicate_to_rpc_header`).
- Each binding's compat test:
  1. Loads the fixture entry.
  2. Deserializes the `wire` JSON into its native `PredicateWire` type.
  3. Re-serializes through its native encoder.
  4. Asserts byte-equal to the original.
- This pins both the encoder AND the decoder are byte-stable across bindings — a Node client encoding a predicate and a Go service decoding it interop without surprise.

The covered shapes span the full AST: leaves of every variant, `And` with mixed costs, `Or` short-circuit candidate, `Not` over a leaf, deeply-nested `And-of-Or-of-And + Not`. Adding a new `Predicate` variant requires extending this fixture in the same coordinated commit (per `CAPABILITY_SYSTEM_SDK_PLAN.md` Locked decision § "Predicate AST evolution lands cross-binding").

### `predicate_eval.json`

Pins the `Predicate::evaluate_unplanned(ctx)` boolean output for representative `(predicate, tags, metadata)` triples:

- `wire` is the canonical `PredicateWire` for the predicate.
- `tags` is a wire-format string array (`hardware.gpu`, `software.os=linux`, …).
- `metadata` is a `string→string` map.
- `expected` is the boolean the substrate returns from `evaluate_unplanned(ctx)`. The Rust integration test asserts the planner-equivalence: `evaluate(ctx) == evaluate_unplanned(ctx) == expected`.

Each binding loads the fixture, decodes the predicate, runs its host-language evaluator against the context, and asserts byte-identical boolean output. Pins:
- Leaf semantics — axis-tag matching across `AxisPresent` / `AxisValue` shapes; numeric coercion (rejects non-numeric values); semver triple parsing + caret-compatibility band; metadata lookup.
- Composite recursion — short-circuiting `And` / `Or`; `Not` inversion; arbitrary depth.
- Real-world predicates — `(GPU OR ≥64GB) AND has-intent AND NOT decommissioning AND python≥3.10`, with both passing and failing contexts, including the OR's memory-only branch.

Phase 9c of `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md`. SDKs that expose a host-language `evaluatePredicate(pred, tags, metadata)` consume this fixture in their per-binding test suites.

### `capability_validation.json`

Pins `validate_capabilities(caps)` output for representative `caps` payloads. Phase 9a of `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md`.

- Each case carries a wire-format `caps` (`{ "tags": [...], "metadata": {...} }`) plus the `expected_errors` + `expected_warnings` arrays a conformant validator emits.
- Bindings sort their output canonically (each list sorted by `JSON.stringify` / equivalent of each entry) before comparing.
- Coverage spans every `SchemaError` kind that fires today (`type_mismatch` for numeric / presence / bool; `index_malformed`) plus every `ValidationWarning` kind (`unknown_key`, `legacy_tag`). Reserved-prefix tags pass through unchecked. The `metadata_oversize` warning is exercised by per-binding unit tests rather than the fixture (a 5 KB padded metadata value is awkward to embed in JSON).

The fixture's top-level `schema_metadata_soft_cap_bytes` field pins the substrate's `METADATA_SOFT_CAP_BYTES` constant (`4096`) — bindings assert their soft-cap constant matches.

### `capability_set_diff.json`

Pins the `CapabilitySet::diff(prev)` output for representative `(prev, curr)` pairs:

- `prev` and `curr` are the wire-format JSON representation of `CapabilitySet` (`{ tags: [...], metadata: {...} }` post Phase A.5.N.3).
- `expected_added_tags` / `expected_removed_tags` are sorted-by-wire-form arrays of tag strings.
- `expected_metadata_changes` is a sorted-by-key array of changes with `{kind, key, ...}` shape; `kind` is `"added"` / `"removed"` / `"updated"` (the substrate's `MetadataChange` enum variants).
- Each binding computes `curr.diff(prev)`, normalizes its output (sort tag arrays by wire form; sort metadata changes by key), compares to `expected_*`.

Covers the load-bearing cases:
- Empty-vs-empty → empty diff.
- Tag added / removed.
- Metadata added / removed / updated.
- Combined tag + metadata changes.
- Key-rename surfaces as Removed + Added (NOT Updated — pins the contract that key identity changes are semantically distinct).
- Reserved-prefix tags + axis tags both diff correctly.

## Regeneration

For now, hand-maintained alongside the corresponding Rust reference test (`tests/cross_lang_capability_fixtures.rs`). When a future binding's CI starts cross-checking, the recommended pattern (per `CAPABILITY_SYSTEM_SDK_PLAN.md` Phase 2) is to add a `cargo run --example gen_capability_fixtures` binary that emits these files deterministically — diffs against the committed copies catch drift.

The Rust reference test loads each fixture entry and:
- For `predicate_nrpc_envelope.json`: round-trips through `PredicateWire` ↔ `Predicate` and re-serializes; asserts byte-equal to the fixture.
- For `capability_set_diff.json`: parses `prev` + `curr`, computes `curr.diff(prev)`, normalizes, compares.

Failing the Rust test means either (a) the wire format drifted (loud signal — fix the implementation or update the fixture in the same commit), or (b) the fixture is stale (regenerate).
