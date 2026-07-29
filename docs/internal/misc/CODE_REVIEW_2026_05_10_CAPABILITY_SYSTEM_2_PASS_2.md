# Code Review — `capability-system-2` vs `cf86d986` (2026-05-10, second pass)

Second-pass review on top of `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2.md`. The earlier
sweep landed 28 fixes (CR-1..CR-28) plus 30+ follow-ups (P1-D..P3-S, Q1..Q19,
R1..R4); this pass surfaces what those passes missed. Each item is verified
against current HEAD (`b23ef82f`) on the live branch.

Tagged `[N1 | N2 | N3]`:
- N1 — correctness / wire-format divergence (cross-binding fixture rows would diverge).
- N2 — latent / DoS / asymmetric guard (current callers safe, future callers vulnerable).
- N3 — perf / observability.

## Status

| ID  | Pri | Area        | Title                                                                       | Status |
|-----|-----|-------------|-----------------------------------------------------------------------------|--------|
| N-1 | N1  | TS binding  | `evalLeaf` value-predicates spuriously match `AxisPresent` tags             | ✅ |
| N-2 | N1  | TS binding  | Numeric-leaf regex rejects scientific notation Rust accepts                 | ✅ |
| N-3 | N1  | TS binding  | `diffCapabilities` is not separator-agnostic (mirror of CR-3)               | ✅ |
| N-4 | N1  | placement   | `score_custom_filter_axis` runs inside `with_caps` shard lock (deadlock)    | ✅ |
| N-5 | N2  | Go binding  | Schema `Number` admits negatives + unbounded ints (asymmetric with R4)      | ✅ |
| N-6 | N2  | predicate   | `predicate_from_rpc_headers` does not enforce decode-side size cap          | ✅ |
| N-7 | N2  | scheduler   | `select_migration_target` lacks the CR-21 LocalPreferred fast-path          | ✅ |
| N-8 | N3  | bindings    | `net_compute_snapshot_bytes_free` leaks on `(non-NULL ptr, len=0)`          | ✅ |
| N-9 | N3  | predicate   | `dynamic_cost` casts `usize` cardinality through `u32` — wraps for huge sets | ✅ |
| N-10 | N3 | placement   | `placement_registry::register` pre-creates counter on collision             | ✅ |
| N-11 | N3 | placement   | `score_resource_axis::Both` no-data branches average to 1.0                 | ✅ |
| N-12 | N3 | placement   | `target_axis_value_numeric` lacks finite-range guard before f32 cast        | ✅ |
| N-13 | N2 | Py binding  | `_parse_semver` admits Unicode digits, rejects `+1`                         | ✅ |
| N-14 | N2 | Go binding  | `parseFloat` accepts hex floats and digit-separator underscores              | ✅ |
| N-15 | N2 | TS binding  | `parseSemver` mishandles `1.2.3+build-1`                                    | ⚪ |
| N-16 | N3 | Go binding  | Streaming RPC construction is not cancellable                               | ✅ |

---

## N1 — fix before broad rollout

### N-1: TS `evalLeaf` value-predicates spuriously match `AxisPresent`

**Location:** `net/crates/net/sdk-ts/src/capability-enhancements.ts:1062-1080, 1091-1148`

```ts
function axisTagValue(tags, key): string | undefined {
  const prefix = `${key.axis}.${key.key}`;
  for (const wire of tags) {
    if (wire === prefix) return '';   // AxisPresent → empty string
    ...
  }
}
case 'equals': {
  const v = axisTagValue(tags, pred.key);
  return v !== undefined && v === pred.value;   // matches "" === ""
}
```

Rust's `match_axis_tag` (`predicate.rs:1749-1757`) explicitly skips
`Tag::AxisPresent` for value predicates, with a doc-comment spelling out the
hazard: *"feeding `""` through `value_pred` would let an empty-string `Equals`
/ `StringPrefix` / `StringMatches` predicate spuriously match a presence-only
tag."* The TS doc-comment at lines 1054-1057 even claims the opposite.

**Fix:** make `axisTagValue` return `undefined` for the prefix-equals case;
introduce a separate `axisTagPresent` for the `exists` predicate (mirror of
Go's Q14 fix).

**Regression test:** TS test exercising `Equals(_, "")`, `StringPrefix(_, "")`,
`StringMatches(_, "")` against an `AxisPresent` target — must evaluate `false`.

### N-2: TS numeric leaf regex rejects values Rust f64 accepts

**Location:** `capability-enhancements.ts:1099, 1105, 1112-1113, 1176`

```ts
return Number.isFinite(n) && /^-?\d+(\.\d+)?$/.test(v) && n >= pred.threshold;
```

The regex pre-filter rejects `1e10`, `1.5e-3`, `+1`, `inf`, `NaN`, `.5`, `1.`,
`-0` — all of which Rust's `f64::from_str` (`predicate.rs:1093`) accepts. R1
fixed this on the Go side (commit `bab01616`); the TS regex was never
relaxed. A tag value `software.model.context_length=1e6` evaluates `>= 1000000`
as `true` in Rust and `false` in TS.

**Fix:** drop the regex; `Number.isFinite(n)` after `Number.parseFloat(v)` is
sufficient for `+`, scientific notation, and decimal forms. For `inf`/`NaN`
parity, accept them and let IEEE comparison decide (matches R1's reasoning).

**Regression test:** TS fixture row asserting `numericAtLeast(key, 1000)`
matches `key=1e6`, `key=+1500`, `key=1.5e3`.

### N-3: TS `diffCapabilities` is not separator-agnostic (mirror of CR-3)

**Location:** `capability-enhancements.ts:651-666`

```ts
const prevTagSet = new Set(prev.tags);
const currTagSet = new Set(curr.tags);
for (const t of currTagSet) {
  if (!prevTagSet.has(t)) added_tags.push(t);
}
```

CR-3 patched the Rust `CapabilitySet::diff` to compare semantically (axis/key/
value, ignoring `=` vs `:`). The TS rewrite did raw string-set comparison.
A peer that normalizes separator form between announcements emits phantom
Removed+Added pairs.

**Fix:** parse each wire string via `tagFromString`, compare on
`(kind, axis, key, value)` ignoring separator.

**Regression test:** TS test diffing `prev=[hardware.k=v]` vs `curr=[hardware.k:v]`
— must produce empty `added_tags` and `removed_tags`.

### N-4: `score_custom_filter_axis` runs inside `with_caps` — deadlocks on re-entrant FFI filters

**Location:** `placement.rs:485-536`; `with_caps` doc at `capability.rs:3111-3126`.

```rust
self.index.with_caps(*target, |target_caps| -> Option<f32> {
    if let Artifact::Daemon { required, .. } = artifact { ... }
    let custom = self.score_custom_filter_axis(target, artifact)?;   // FFI under shard lock
    ...
})
```

`with_caps` holds a per-shard read lock. Its doc warns: *"`f` MUST NOT acquire
any other index locks (e.g. via `find_nodes` / `query`), or it'll deadlock
against a concurrent insert."* The custom-filter call invokes externally
registered `&dyn PlacementFilter` impls — including FFI filters from JS / Python
/ Go (which may call back into the mesh) and Rust-side `LegacyPlacement` shims
that call `self.index.query(&self.filter)` directly.

**Fix:** lift `score_custom_filter_axis` outside the `with_caps` closure (it
takes only `target` + `artifact`, no `target_caps` dependency). Compose the
result into `compose_axis_scores` afterward.

**Regression test:** a custom filter that calls `index.query` from inside
`placement_score` must not deadlock with a concurrent `index.index(...)` from
another thread.

---

## N2 — fix soon

### N-5: Go schema `Number` admits negatives + unbounded integers (asymmetric with R4 / CR-15)

**Location:** `bindings/go/net/capability_schema.go:254-262, 301, 345-360`

```go
func isIntegerLiteral(s string) bool {
    if s == "" { return false }
    if s[0] == '-' { return isAllDigits(s[1:]) }
    return isAllDigits(s)
}
```

Rust uses `value.parse::<u64>()` (`schema.rs:704`); Python locks via
`^\+?[0-9]+$` regex + `u64::MAX` ceiling (R4). Go was never touched. Same shape
on indexed-collection index validation (`schema.go:345-360` uses `isAllDigits`,
unbounded; Rust uses `idx.parse::<u32>()`).

**Fix:** mirror Python's accepted set — `^\+?[0-9]+$` plus `strconv.ParseUint`
with the right bit-width (64 for `Number`, 32 for indexed-collection index).

**Regression test:** Go test asserting `validateCapabilities` errors on
`software.model.0.context_length=-1` and on `software.model.99999999999999.id`.

### N-6: `predicate_from_rpc_headers` does not enforce decode-side size cap

**Location:** `net/crates/net/src/adapter/net/behavior/predicate.rs:764-778`

The encode side caps at `MAX_PREDICATE_RPC_HEADER_VALUE_LEN`; the doc promises
symmetric enforcement. Decode has no length check. Cheap parse-bomb DoS shape if
transport caps are bypassed.

**Fix:** add a length check before `serde_json::from_slice`; introduce a new
`PredicateRpcDecodeError::Oversize` variant.

**Regression test:** assert `predicate_from_rpc_headers` returns
`Err(Oversize)` for a header value larger than the cap.

### N-7: `Scheduler::select_migration_target` lacks the CR-21 LocalPreferred fast-path

**Location:** `compute/scheduler.rs:262-267, 329-339`

CR-21 added the LocalPreferred short-circuit to `place_migration_v2`. But the
doc-comment at lines 233-238 explicitly directs RTT-aware operators to
`select_migration_target` directly — and that lower-level entry point bypasses
the fast-path. Operators feeding RTT data lose the CR-21 fix silently.

**Fix:** lift the LocalPreferred check into `select_migration_target` (gated on
`tie_break.local_node_id` carrying the local id, or accept `prefer_local` as
a parameter).

**Regression test:** `select_migration_target` with eligible local + remote
candidates and populated `rtt_lookup` must return the local node.

---

## N3 — latent

### N-8: `net_compute_snapshot_bytes_free` leaks on `(non-NULL ptr, len=0)`

**Location:** `bindings/go/compute-ffi/src/lib.rs:887-895`

CR-11 split this exact guard in `GoBridge::snapshot` and `parse_side`, but
the outbound free helper (declared in `net.go.h`, callable directly by Go)
kept the combined `ptr.is_null() || len == 0` check.

**Fix:** drop the `len == 0` half (`libc::free` reads malloc-header metadata,
doesn't need a length).

### N-9: `dynamic_cost` casts `usize` cardinality through `u32` — wraps

**Location:** `predicate.rs:1264, 1281, 1417, 1428`

```rust
static_c.saturating_div((cardinality as u32).max(1))
```

`axis_cardinality` returns `usize`; the cast wraps modulo `2³²`. A long-running
fleet with unbounded-cardinality metadata keys (session id, request id) trips
this; planner picks the *worst* ordering.

**Fix:** `u32::try_from(cardinality).unwrap_or(u32::MAX)`.

### N-10: `placement_registry::register` pre-creates counter on collision

**Location:** `placement_registry.rs:136-138`

The per-binding counter is inserted before the id-occupancy check; a failed
`register` call leaves a counter behind a binding label whose registration
never succeeded.

**Fix:** move the `entry().or_insert_with(...)` inside the `Entry::Vacant`
arm.

### N-11: `score_resource_axis::Both` no-data branches average to 1.0

**Location:** `placement.rs:806-816, 982-1004`

`Both` averages `score_compute_axis + score_storage_axis`, each returning
`1.0` for no-data. A no-data candidate ties a maxed-out one; lex-NodeId
tie-breaker biases toward (often misconfigured) lower-id peers.

**Fix:** track per-axis `had-data`; if exactly one had data, return that
score; if both had data, average; if neither, return 1.0.

### N-12: `target_axis_value_numeric` lacks finite-range guard before f32 cast

**Location:** `placement.rs:946-955`

CR-9 guarded NaN/inf, but a malformed `hardware.cpu_cores=1e308` parses as
finite f64, then `c as f32` saturates to `INFINITY`, then `saturating_score`
returns 0.0 — silently down-scoring the candidate.

**Fix:** range-check after the f64 parse before casting (`0.0..=1e15`).

### N-13: Python `_parse_semver` admits Unicode digits / rejects `+1`

**Location:** `sdk-py/src/net_sdk/capability.py:1032-1038`

R4 fixed only the schema validator. The predicate-side semver parser still
uses `parts[0].isdigit()`, which accepts Unicode digits Rust rejects and
rejects the `+1` Rust accepts.

**Fix:** apply the same `_U64_LITERAL` regex as `capability_schema.py:332`.

### N-14: Go `parseFloat` accepts hex floats and digit-separator underscores

**Location:** `bindings/go/net/capability.go:1245-1253`

`strconv.ParseFloat` accepts `"0x1p3"` (8.0) and `"1_000"` (1000); Rust's
`f64::from_str` rejects both. R1 only addressed `±inf`.

**Fix:** pre-screen with `strings.ContainsAny(s, "_xX")` before delegating.

### N-15: TS `parseSemver` mishandles `1.2.3+build-1`

**Location:** `capability-enhancements.ts:993-1023`

⚪ **No fix needed.** On closer inspection the agent's original claim was
wrong: `Math.min(dash, plus)` picks the *earlier* separator index, and in
`1.2.3+build-1` the `+` at index 5 comes before the `-` at index 11, so the
TS implementation correctly slices `core = "1.2.3"`. Rust's
`split_once('+').then split_once('-')` reaches the same answer. No reachable
input shape produces a divergence. Cross-binding fixture
`semver_strips_build_metadata_with_embedded_dash` pins the agreement so any
future refactor that flips the strip order surfaces immediately.

### N-16: Streaming RPC construction is not cancellable

**Location:** `bindings/go/rpc-ffi/src/lib.rs:1064-1098, 1399-1440`

`net_rpc_call_streaming{,_with_headers}` take no `cancel_token`; CR-13's
cancel discipline applies to unary variants only. Construction `block_on` is
unprotected against a peer-stalled initial-frame ACK.

**Fix:** add `cancel_token: u64` parameter, route construction through
`run_cancellable`. (Preserves binary-compat by adding a new `*_cancellable`
variant rather than breaking the existing signature, or bumping the major
version of the FFI surface — tracked separately.)
