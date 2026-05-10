# Code Review — `capability-system-2` vs `master` (2026-05-10)

Synthesized from a five-agent parallel review pass over the 99-file / ~37.6k-LOC-net diff between `master` and `capability-system-2` HEAD (`38612b61`). The branch lands the substrate capability/predicate/placement subsystem, the enhancement layer (axis schemas, lazy projections, diff, predicate-AST-in-nRPC, query planner, debug sessions), and the SDK + binding rollout (Rust net-sdk, Node, Python, Go, C SDK).

Two prior cubic-ai review passes already swept (commits `ab1b24df` "six bugs" and `38612b61` "14 bugs"). The items below are gaps those passes missed, verified by hand against the actual code on this branch.

Each item is tagged `[P1 | P2 | P3]`. P1 are merge-blockers (correctness / data-loss / build-break / sort-determinism). P2 should land before broad rollout. P3 are latent and can ship in follow-ups.

## Status

**All 28 items fixed on this branch.** 1872 unit tests passing
(up from 1860 pre-fixes — 12 new regression tests added).
Bindings (Node, Python, Go cgo) all build cleanly.

| ID | Pri | Area | Title | Status |
|----|-----|------|-------|--------|
| CR-1 | P1 | capability | `has_tag` separator-form mismatch | ✅ |
| CR-2 | P1 | capability | `RequiredCapability::Tag` separator-form mismatch | ✅ |
| CR-3 | P1 | capability | `CapabilitySet::diff` separator-form mismatch | ✅ |
| CR-4 | P1 | capability | Diff op order non-determinism (`HashMap`/`HashSet`) | ✅ |
| CR-5 | P1 | C SDK | `examples/capability.c` header-guard collision (build break) | ✅ |
| CR-6 | P1 | placement | NaN scores from custom `PlacementFilter` poison sort | ✅ |
| CR-7 | P2 | predicate | `query::traverse` lacks visited-set / cycle detection | ✅ |
| CR-8 | P2 | predicate | Trace labels leak raw metadata values | ✅ |
| CR-9 | P2 | placement | `saturating_score` propagates NaN through resource axis | ✅ |
| CR-10 | P2 | placement | Anti-affinity `<= threshold` admits NaN as over-threshold | ✅ |
| CR-11 | P2 | bindings | compute-ffi snapshot leak when (non-NULL ptr, len=0) | ✅ |
| CR-12 | P2 | bindings | Python `announce_capabilities` holds GIL across blocking call | ✅ |
| CR-13 | P2 | bindings | rpc-ffi `run_cancellable` register-after-spawn TOCTOU | ✅ |
| CR-14 | P2 | capability | Schema validator skips `metadata_reserved` / prefixes | ✅ |
| CR-15 | P2 | capability | Schema `Number` accepts negative integers | ✅ |
| CR-16 | P2 | capability | `with_metadata` has no reserved-prefix gate | ✅ |
| CR-17 | P2 | C SDK | `net_predicate_to_where_header` partial-write leak | ✅ |
| CR-18 | P3 | predicate | `eval_any_in_cost_order` uses And-mode cost on Or | ✅ |
| CR-19 | P3 | predicate | `redact_label` splits on first `=` (loses keys with `=`) | ✅ |
| CR-20 | P3 | placement | `placement_registry` invocation-counter precreate race | ✅ |
| CR-21 | P3 | placement | Phase-G v2 migration silently drops `LocalPreferred` | ✅ |
| CR-22 | P3 | placement | `IntentMatchPolicy::AnyOfLocalCapabilities` empty registry vetoes | ✅ |
| CR-23 | P3 | capability | Bloom filter `h2` even degrades when bit count is power-of-2 | ✅ |
| CR-24 | P3 | capability | `tag_codec` software runtime/framework names with `=`/`.`/`:` ambiguous | ✅ |
| CR-25 | P3 | bindings | Node + Python `fp16_tflops_x10` round-trips through f32 | ✅ |
| CR-26 | P3 | bindings | Go `Register/UnregisterPlacementFilter` race | ✅ |
| CR-27 | P3 | bindings | Go `tagKeyFromWire` swallows type-assert failures | ✅ |
| CR-28 | P3 | C SDK | `include/README.md` claims `net.h` + `net.go.h` compose (they don't) | ✅ |

Out of scope (pre-existing on `master`, not introduced by this branch):

- `go/net.h` declares all `origin_hash` parameters/returns as `uint32_t` while the canonical `net.go.h` and the Rust `extern "C"` signatures use `uint64_t`/`u64`. Filed as follow-up; this branch widens the surface area touching `origin_hash` (more cgo entry points) so the gap will become more painful in production.

---

## P1 — must fix before merge

### CR-1: `has_tag` separator-form mismatch

**Location:** `net/crates/net/src/adapter/net/behavior/capability.rs:1233-1238`

```rust
pub fn has_tag(&self, tag: &str) -> bool {
    let Ok(parsed) = Tag::parse(tag) else {
        return false;
    };
    self.tags.contains(&parsed)
}
```

`Tag::AxisValue` derives `PartialEq` over its `separator` field (`=` vs `:`). The typed encoders emit canonical separators, so a stored `software.os=linux` will fail `caps.has_tag("software.os:linux")` despite identical semantics. Same hazard the diff-engine fix in `38612b61` patched, but for the public membership API.

**Fix:** compare via `(axis, key, value)`, ignoring `separator`. The cleanest approach is to add a `Tag::semantically_eq` (or rely on `axis_key()` + `value()`).

**Regression test:** `has_tag_matches_across_separator_forms` in `capability.rs` tests.

### CR-2: `RequiredCapability::Tag` separator-form mismatch

**Location:** `net/crates/net/src/adapter/net/behavior/required_capability.rs:63`

```rust
Self::Tag(required) => ctx.tags.iter().any(|t| t == required),
```

Same Tag PartialEq hazard. A `require!("software.os:linux")` placed against a node whose canonical tag is `software.os=linux` evaluates `false` — silently filters out legitimate placement candidates.

**Fix:** compare via `axis_key()` + `value()`.

**Regression test:** `required_tag_evaluates_across_separator_forms`.

### CR-3: `CapabilitySet::diff` separator-form mismatch

**Location:** `net/crates/net/src/adapter/net/behavior/capability.rs:1352-1353`

```rust
let added_tags: HashSet<Tag> = self.tags.difference(&prev.tags).cloned().collect();
let removed_tags: HashSet<Tag> = prev.tags.difference(&self.tags).cloned().collect();
```

The structural `DiffEngine::diff` was patched in `38612b61`; this companion API (consumed by event-driven dashboards / delta propagation per the doc-comment at line 1345) was not. Two semantically-identical tags differing only in separator land as both `Added` and `Removed`.

**Fix:** apply the same `(axis, key)`-keyed comparison shape that `diff_tags` already uses.

**Regression test:** `capability_set_diff_ignores_separator_form`.

### CR-4: Diff op order is non-deterministic

**Location:** `net/crates/net/src/adapter/net/behavior/diff.rs:571-576, 580-635, 638-663, 666-733`

- `diff_tags`: `for tag in old_residual.difference(&new_residual)` iterates `HashSet<&Tag>` — random order each run.
- `diff_models`/`diff_tools`/`diff_software`: build `std::collections::HashMap` (random hasher) for old/new sides, then iterate.

Two senders with identical inputs produce different `Vec<DiffOp>` orderings; any signed/hashed diff envelope mismatches across processes (same class as the `3291b2c2` fix for top-level tag emission).

**Fix:** sort residual tags before pushing; replace the `HashMap`s with `BTreeMap`.

**Regression test:** `diff_op_order_is_stable_across_runs`.

### CR-5: `examples/capability.c` does not compile

**Location:** `net/crates/net/examples/capability.c:33-34`; header guards in `net/crates/net/include/net.h:16-17` and `net/crates/net/include/net.go.h:8-9`.

The example includes both headers; both use `#ifndef NET_SDK_H`. The second include is silently skipped, so `net_validate_capabilities`, `net_predicate_evaluate{,_with_trace}`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys`, `net_predicate_to_where_header` are undeclared at compile time. GCC 14+/Clang 16+ now error on implicit declaration; older toolchains miscompile `char*` returns.

**Fix:** drop the `net.h` include (the example uses only symbols from `net.go.h` plus `net_free_string` which `net.go.h` re-declares), or rename one guard.

**Regression check:** add a build-time CI check that compiles `capability.c`.

### CR-6: NaN scores from custom `PlacementFilter` poison sort determinism

**Location:** `net/crates/net/src/adapter/net/compute/scheduler.rs:382-397`

```rust
candidates.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
```

Only `StandardPlacement::compose_axis_scores` clamps NaN; FFI-registered filters resolved via `placement_registry` can return `Some(f32::NAN)` (JS/Python `NaN` round-trips trivially through napi/pyo3). NaN candidates compare `Equal` to everything → undefined `Vec::sort_by` order → different runs pick different winners.

**Fix:** filter NaN at the boundary in `pick_best_candidate`: `.filter(|(_, s)| s.is_finite())` (or clamp NaN→0.0 with a single boundary call).

**Regression test:** `placement_sort_filters_nan_scores`.

---

## P2 — fix soon

### CR-7: `query::traverse` lacks visited-set / cycle detection

**Location:** `net/crates/net/src/adapter/net/behavior/query.rs:412-463`

The fork-of walker iterates up to `max_depth` hops without tracking visited nodes. A malformed (or malicious) capability set where two nodes' `fork-of:` tags point at each other's chains will oscillate the walk between them, producing a path with duplicate `(node_id, tag)` entries up to `max_depth` long. `max_depth` bounds runtime, so wrong-result not infinite-loop.

**Fix:** track a `BTreeSet<NodeId>` of visited nodes, break when re-entering.

### CR-8: Predicate trace labels leak raw metadata values

**Location:** `net/crates/net/src/ffi/predicate_debug.rs:135-193`; redaction surface at `redact_label` lines 343-351

`ClauseTrace.label` for `MetadataEquals(api_key=sk-secret)` contains the literal secret. `redactMetadataKeys` only operates on `PredicateDebugReport.clause_stats`, not on `ClauseTrace`. C SDK debug-session helper offers no `redact_trace_metadata_keys` sibling.

**Fix:** add a recursive trace-redaction helper that rewrites `MetadataEquals(...)` / `MetadataMatches(...)` / `MetadataNumericAtLeast(...)` labels using the existing redaction-key set, and expose it through the FFI surface (`net_predicate_redact_trace_metadata_keys`) and per-binding wrappers.

### CR-9: `saturating_score` propagates NaN through resource axis

**Location:** `net/crates/net/src/adapter/net/behavior/placement.rs:946-951`

`if value <= 0.0` is `false` for NaN; `value/(value+ref) = NaN` slips through. `f64::from_str("NaN")` parses successfully and `Tag::AxisValue` stores raw strings. A single `hardware.cpu_cores=NaN` makes `score_compute_axis` return NaN → `compose_axis_scores` clamps to 0.0 → silent veto.

**Fix:** add `|| !value.is_finite()` to the early-return guard.

### CR-10: Anti-affinity `<= threshold` admits NaN as over-threshold

**Location:** `net/crates/net/src/adapter/net/behavior/placement.rs:838`

`concentration <= self.anti_affinity.leadership_concentration_threshold` is `false` when concentration is NaN, falling through to the penalty branch. The file already clamps the *penalty* against NaN; symmetric input guard is missing.

**Fix:** early-return `1.0` when `concentration.is_nan()`.

### CR-11: compute-ffi snapshot leak when Go returns (non-NULL ptr, len=0)

**Location:** `net/crates/net/bindings/go/compute-ffi/src/lib.rs:985`

Same shape as the cubic-ai fix in `parse_side` (commit `38612b61`), missed in `GoBridge::snapshot`. A misbehaving Go snapshot trampoline that hands back a valid `C.malloc` pointer with len=0 leaks one allocation per call.

**Fix:** mirror parse_side: when ptr non-NULL and len==0, `libc::free` and return None.

### CR-12: Python `announce_capabilities` holds GIL across blocking call

**Location:** `net/crates/net/bindings/python/src/lib.rs:1753-1759`

Every other Python thread blocks for the duration of the announcement. Sibling sync paths (`call`, `find_service_nodes`) correctly use `py.detach()`. Same pattern at `find_nodes`/`find_nodes_scoped` (lines 1792, 1807).

**Fix:** wrap `runtime.block_on` (and the parking-lot reads) in `py.detach`.

### CR-13: rpc-ffi `run_cancellable` register-after-spawn TOCTOU

**Location:** `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:660-665`

`runtime().spawn(fut)` runs *before* the abort handle is registered. A cancel issued between spawn and registry-insert is dropped on the floor and the task keeps running.

**Fix:** pre-insert under the token before spawning, or check a per-token "cancelled" flag inside the spawned future after registration.

### CR-14: Schema validator skips `metadata_reserved` / prefixes

**Location:** `net/crates/net/src/adapter/net/behavior/schema.rs:485-506`

`AxisSchema` declares `metadata_reserved` and `metadata_reserved_prefixes` and pins them in tests, but `validate_capabilities_against` only walks `caps.tags`. A user's `with_metadata("tool::foo::bar", …)` smuggling onto a substrate-reserved key emits no warning.

**Fix:** walk `caps.metadata` keys against `metadata_reserved` (warn on collision) and `metadata_reserved_prefixes` (warn on user-emitted reserved-prefix metadata).

### CR-15: Schema `Number` admits negative integers

**Location:** `net/crates/net/src/adapter/net/behavior/schema.rs:654`

`value.parse::<u64>().is_ok() || value.parse::<i64>().is_ok()` admits `-1` for `hardware.memory_mb`, `gpu.vram_mb`, `cpu_cores`, `limits.max_concurrent_requests` — none of which can be negative.

**Fix:** drop the `i64` fallback; accept only `u64` (or split into `Int`/`UInt` if signed values are ever needed).

### CR-16: `with_metadata` has no reserved-prefix gate

**Location:** `net/crates/net/src/adapter/net/behavior/capability.rs:1107-1110`

The tag write-path enforces reserved-prefix policy via `parse_user`; the metadata write-path has no equivalent gate. Accepts any string key including reserved-prefix patterns (`causal:`, `tool::evil::input_schema` overriding a real tool's schema, empty key).

**Fix:** filter against the reserved-metadata-prefix set in `with_metadata`, mirroring `Tag::parse_user`'s reserved-prefix rejection.

### CR-17: `net_predicate_to_where_header` partial-write leak

**Location:** `net/crates/net/src/ffi/predicate.rs:215-221`

After `write_string_out` succeeds for the header name, the second `write_string_out` for the value can fail (`CString::new` interior-NUL → `NetError::Unknown`). The function returns early without freeing the header-name CString or NULL-ing the out-pointer; caller can't tell `*out_header_name` is valid. Unreachable today (serde_json never emits NULs), but contract is fragile.

**Fix:** on second-call failure, recover the first allocation via `CString::from_raw(*out_header_name)`, NULL the out-pointer, zero the length, then return.

---

## P3 — latent

### CR-18: `eval_any_in_cost_order` uses And-mode cost on Or composites

**Location:** `net/crates/net/src/adapter/net/behavior/predicate.rs:1161-1165`

`eval_any_with_index` correctly switches to `dynamic_cost_or` for Or; the non-indexed planner path (`eval_any_in_cost_order`) calls `static_cost()`, which orders by And-mode cost. Result: Or short-circuit ordering when no index is available is the *opposite* of optimal (run rare-true clauses first instead of often-true). Boolean result unaffected; perf only.

**Fix:** introduce `static_cost_or` mirroring the dynamic-cost asymmetry, or document the static planner as And-optimized only.

### CR-19: `redact_label` splits on first `=` heuristic

**Location:** `net/crates/net/src/ffi/predicate_debug.rs:343-351`

`MetadataEquals(k=v=actual)` with a key `k=v` splits at the first `=` — redaction silently no-ops while the secret stays in the label. Same pattern affects `MetadataMatches` (` contains "` substring) and `MetadataNumericAtLeast` (` >= ` substring).

**Fix:** stop label-parsing for redaction; carry structured `(variant, key, value)` tuples through the report shape so the wire format isn't re-parsed by string-search heuristics.

### CR-20: `placement_registry` invocation-counter precreate race

**Location:** `net/crates/net/src/adapter/net/behavior/placement_registry.rs:128-130`

Two threads registering the same new binding both pass the `contains_key("X") == false` check, then both `insert("X", AtomicU64::new(0))`; the second insert overwrites whatever count the first had already accumulated via interleaved `get()`. Observability-only.

**Fix:** `entry().or_insert_with(...)` instead of contains+insert.

### CR-21: Phase-G v2 migration silently drops `LocalPreferred`

**Location:** `net/crates/net/src/adapter/net/compute/scheduler.rs:239-277`

Slice 8 promoted v2 to the default migration path. `LegacyPlacement::permissive` scores every eligible at 1.0; with `rtt_lookup: None` the tie-break falls through to step 3 (lex NodeId), so the smallest-NodeId remote always beats a higher-NodeId local even when local is eligible. Migrations newly hop the network even when local was fine.

**Fix:** retain the v1 `local_node_id != source_node && candidates.contains(&local_node_id)` fast-path before the score+tie-break call, or feed local-preference into the tie-break/score axis.

### CR-22: `IntentMatchPolicy::AnyOfLocalCapabilities` + empty registry vetoes everything

**Location:** `net/crates/net/src/adapter/net/behavior/placement.rs:686-697`

The axis returns `0.0` for an empty registry. Multiplicative composition propagates as cluster-wide 0.0 — operators who select the policy but forget to populate `intent_registry` silently block every placement. The doc-comment justifies it ("node must be useful for *something*"), but operators expect "empty config = pass-through".

**Fix:** treat empty registry as `1.0` (axis-disabled identity), or surface a constructor-time warning.

### CR-23: Bloom `h2` even-degradation when bit count is power-of-2

**Location:** `net/crates/net/src/adapter/net/behavior/bloom.rs:174`

`combined = h1 + i*h2 mod m`. Since `len_bits` rounds to a multiple of 64 (2^6 × something), an even `h2` halves the effective probe-cycle length; ≈50% of keys hit fewer distinct bits than `k` claims, raising the false-positive rate above the configured target.

**Fix:** `h2 |= 1` (the standard double-hashing remedy) so it's coprime with the modulus.

### CR-24: `tag_codec` software runtime/framework names with `=`/`.`/`:` round-trip ambiguous

**Location:** `net/crates/net/src/adapter/net/behavior/tag_codec.rs:309, 312, 318`

`software_to_tags` emits `software.runtime.{name}={version}` via raw `format!`. A name like `python=foo` produces `software.runtime.python=foo=3.11`, which `Tag::parse` splits at the first `=` (key = `runtime.python`, value = `foo=3.11`), so the round-tripped name silently truncates.

**Fix:** reject (or escape) names containing `=`/`:`/`.` at encode time, or document the constraint in the typed-struct doc comment.

### CR-25: Node + Python `fp16_tflops_x10` round-trips through f32

**Location:** `net/crates/net/bindings/node/src/capabilities.rs:234-238`; analogous Python path

JS/Python pass `tf` as a u32 representing `tflops × 10`; the binding does `tf as f32 / 10.0` then calls `with_fp16_tflops` which does `(scaled * 10.0)` rounded back to u32. Above 16,777,216 (f32 mantissa), values lose precision: `tf=20_000_005` round-trips to `20_000_004` or `20_000_008`.

**Fix:** bypass `with_fp16_tflops`; write `info.fp16_tflops_x10 = tf` directly (mirrors `model.parameters_b_x10` at line 311).

### CR-26: Go `RegisterPlacementFilter` / `UnregisterPlacementFilter` race

**Location:** `net/crates/net/bindings/go/net/placement.go:243-263, 302`

If a parallel `UnregisterPlacementFilter(id)` runs between the Go map insert (243) and the Rust `register` call (250), `placementFilters.Delete(id)` (302) drops the Go entry; the Rust `register` then succeeds and the substrate has a registration with no Go callable behind it. Future dispatches hit the unknown-id veto silently.

**Fix:** per-id `sync.Mutex` for register/unregister; or atomic compare-and-swap pattern.

### CR-27: Go `tagKeyFromWire` swallows type-assert failures

**Location:** `net/crates/net/bindings/go/net/capability.go:471-487`

`m["axis"].(string)` and `m["key"].(string)` use the comma-ok-discarded form. Malformed JSON like `{"axis": 5, "key": ["x"]}` silently produces `TagKey{Axis: "", Key: ""}` instead of an error. The Rust side rejects with serde error; cross-binding contract breaks.

**Fix:** assert with `, ok` and return an error on type-assert failure.

### CR-28: `include/README.md` claims `net.h` + `net.go.h` compose cleanly

**Location:** `net/crates/net/include/README.md:15`

README says `net_rpc.h` "Independent header guard (`NET_RPC_H`) so it composes cleanly with the others in a single TU" — implying the others compose. They don't (shared `NET_SDK_H` guard, see CR-5). Either fix the guards (preferred — would also unblock CR-5 cleanly) or rephrase to admit `net.h` and `net.go.h` are mutually exclusive in one TU.

---

## Pre-existing (out of scope, filed for follow-up)

**`go/net.h` `origin_hash` width drift.** All `net_compute_*_origin_hash`, `net_identity_origin_hash`, `net_tasks_adapter_open` `origin_hash`, `net_memories_adapter_open` `origin_hash` declared `uint32_t` in `go/net.h` but `uint64_t` in the canonical `net/crates/net/include/net.go.h` and the Rust `extern "C"` signatures. Verified existed on `master`; this branch did not introduce. Go cgo callers truncate the upper 32 bits silently.

## Diligent checks that came back clean

- Wire-format determinism after `3291b2c2` (top-level tag emission) — confirmed.
- Reserved-prefix routing across Node / Python / Go bindings after `4bad612e` — confirmed.
- `compose_axis_scores` NaN-clamp + first-zero short-circuit — confirmed.
- `tie_break_compare` strict total order on distinct NodeIds — confirmed.
- `ServeHandleC` / `RpcStreamHandleC` close-vs-free idempotency — confirmed.
- `run_cancellable` panic→typed-error conversion — confirmed.
- Cross-binding predicate-IR builder shape parity (kind strings, `root_idx`, post-order) — confirmed against fixtures.
- `EvalContext` borrow lifetimes inside `query::filter` / `aggregate` under the read-lock — confirmed.
- bench/placement.rs exercises the documented register→score→unregister path — confirmed.
- `aggregate` over an empty index correctly yields `Count=0` / `MaxNumericMetadata=None` / `UniqueAxisValues=[]` — confirmed.
- `nearest`'s `(Some, Some) → cmp().then(NodeId)` and `(None, None) → NodeId` tie-breaks — confirmed.
- And/Or short-circuit semantics in `evaluate_with_trace` match the cross-binding `predicate_trace.json` fixture (cost-order, dropped descendants) — confirmed.
