# Code review — `subnet-scaling` branch, pass 3 (2026-05-23)

Branch base: `master`.
Scope: the 9 commits ahead of master AFTER pass 2 (the substrate
work landed on master via merge `d812c27b`). This pass covers the
NEW work: SDK `net_sdk::aggregator` module + `BoundRegistryClient`,
C FFI ABI (`ffi/aggregator.rs` + `include/net.h`), language
bindings (Node NAPI / Python PyO3 / Go cgo), aggregator-daemon
`--print-bootstrap` flag, top-level Go consumer wrapper, and CI
binding-feature wiring. ~4,800 LOC across 32 files.

Three review agents (reuse / quality / efficiency) were dispatched in
parallel. Findings below are organised by severity, then category. File
paths are relative to repo root; line numbers reflect the branch tip
and may drift.

---

## HIGH — correctness / safety / data-race risks

### T1 — `set_deadline` / `set_ttl` rebuild the client, clobbering the warm cache; FFI mutation is UB under concurrent ops

`net/crates/net/src/ffi/aggregator.rs:166-181, 457-484` plus the
matching Node `bindings/node/src/aggregator.rs:279-295` and Python
`bindings/python/src/aggregator.rs:351-364`.

Each `set_deadline` / `set_ttl` does
`h.client = Client::new(h.mesh.clone()).with_*(...)`. Three problems
compounding:

- **Cache loss.** `FoldQueryClient::new` allocates a fresh
  `Arc<RwLock<HashMap<CacheKey, CacheEntry>>>`
  (`query_client.rs:100-107`) — every warmed entry is discarded.
- **UB.** `&mut *handle` is taken with no synchronization while ops
  (`list`/`spawn`/`unregister`/`query_*`) only take `&*handle`.
  Concurrent `_set_deadline` + `_list` on the same handle is undefined
  behavior.
- **Permanent cache disable for Go callers.** The Go wrapper calls
  `set_deadline` on **every** RPC via `honorContextDeadline`
  (`bindings/go/net/aggregator.go:441-456`,
  `go/aggregator.go:407-418`) under an *RLock*. Two RLock holders race
  on FFI handle mutation. Any Go consumer that passes `ctx.WithDeadline`
  permanently disables the cache *and* trips the race.

Fix: change substrate `RegistryClient` / `FoldQueryClient` to expose
`set_deadline_mut(&mut self, _)`. Wrap the inner client in
`parking_lot::RwLock` inside the FFI handle so reads/writes
synchronize. Or push deadline behind `AtomicU64<millis>` read per
call. Drop the now-redundant `mesh: Arc<MeshNode>` field on the
handle (T8) once rebuild isn't needed.

### T2 — `go/aggregator.go` is a 591-line near-verbatim copy of `bindings/go/net/aggregator.go`

`go/aggregator.go` (591 lines) vs
`net/crates/net/bindings/go/net/aggregator.go` (644 lines).

The diff is mostly comments + a 3-line local-variable cleanup. Every
type, every cgo declaration, every method body, every error-kind
constant, the `honorContextDeadline` helper, the `lastErrorDetail`
helper — all duplicated. Will drift on the next bug fix to either
copy.

Fix: delete one. If both must exist (reference impl vs trimmed
consumer impl), have the top-level wrapper `import` + re-export from
`bindings/go/net/aggregator.go` instead of fork-and-trim.

### T3 — Five hand-maintained error-kind enum copies; no generator or mirror test

`net/crates/net/src/ffi/aggregator.rs:60-81`,
`net/crates/net/include/net.h:368-376`,
`net/crates/net/bindings/go/net/aggregator.go:60-69`,
`go/aggregator.go:31-40`, plus Python string discriminators at
`python/src/aggregator.rs:113-124` and Node TS unions at
`aggregator.ts:33-46`.

The number `7` (`UNKNOWN_KIND`) is defined out of order in the Rust
source (line 60) vs all four mirrors (last). Future variants drift
silently — there is no consistency test like the one referenced for
`net_error_t` at `net.h:36-38`.

Fix: generate `NET_REGISTRY_ERR_*` block + Go const block via
`build.rs` from the Rust constants, or at minimum add
`tests/error_kind_mirror.rs` that scans `include/net.h` and asserts
each macro value matches its Rust constant. Node TS unions + Python
kind strings should similarly be string-asserted in a binding test.

---

## HIGH — duplication / correctness gap

### T4 — `ffi/aggregator.rs` hand-rolls a 119-line JSON encoder for types that already `derive(Serialize)`

`net/crates/net/src/ffi/aggregator.rs:631-769`
(`groups_to_json` / `group_to_json` / `summaries_to_json` /
`summary_to_json` / `json_string`).

`RegistryGroupSummary`, `RegistryReplicaSummary`,
`SummaryAnnouncement` all derive `Serialize`
(`registry_service.rs:100,111`, `summarizer.rs:26`). The "avoid a
serde dep" rationale in the docstring is contradicted by `ffi/mesh.rs`
which uses `serde_json` in 19 places (and exports a
`write_json_out::<T: Serialize>` helper at line 289).

Per-byte `format!("{b:02x}")` allocations for hex encoding (3 sites);
incomplete escape table in `json_string` (no `\b`/`\f`/` `/
` ` for JS consumers).

Fix: replace all five helpers with `serde_json::to_string(&groups)`.
~130 lines deletable including the dependent
`json_string_escapes_control_characters` +
`group_to_json_includes_every_documented_field` +
`summary_to_json_includes_every_documented_field` tests pinning the
hand-roll.

### T5 — Node `bigint_u64` local copy has weaker validation than `common::bigint_u64`

`net/crates/net/bindings/node/src/aggregator.rs:358-367`.

`common.rs:50` already exports `pub(crate) fn bigint_u64(b: BigInt) ->
Result<u64>` used by `meshdb.rs`, `cortex.rs`, etc. — with stricter
validation (rejects multi-word, accepts empty-words-as-zero, has unit
tests).

The new local copy calls `value.get_u64()` without checking
`words` length, so **multi-word BigInts silently wrap to u64**.

Fix: `use crate::common::bigint_u64;`. Drop the second arg (`name`)
or add a `bigint_u64_named` wrapper in `common.rs` if per-arg error
messages matter.

### T6 — Three bindings, three different semantics for `with_deadline`

- **JS** (`bindings/node/src/aggregator.rs:203-209`) returns a fresh
  clone.
- **Python** (`bindings/python/src/aggregator.rs:253-258`) mutates in
  place under `RwLock::write` and returns `PyRef<'_, Self>` despite
  the docstring claiming "for chaining."
- **Go** (`bindings/go/net/aggregator.go:441-456`) calls FFI
  `set_deadline` on every RPC.

Operators writing cross-language tooling will hit footguns. Three
bindings each surprise a different way.

Fix: pick one shape (in-place mutation matches Go's expectation and
removes the data race once T1 lands) and align all bindings. Update
each binding's doc to match.

---

## MEDIUM — quality / hygiene

### T7 — Massive copy-paste across FFI op handlers

`net/crates/net/src/ffi/aggregator.rs:190-340` plus the matching
Node + Python binding wrappers.

`_list`, `_spawn`, `_unregister` each replicate the null-check +
CStr-parse + block_on + classify + store_error_detail block. `_spawn`
does the inconsistent dance of checking `!out_error_kind.is_null()`
six times; `_list` requires it non-null up front. Same shape in Node
+ Python: each binding's three RPC methods are near-identical modulo
arg names.

Fix: extract `with_handle(handle, out_error_kind, |h| Result<T, _>)`
that does the null-checks once and dispatches the classify / store /
write-out path. Mirror the helper in Node + Python.

### T8 — `RegistryClientHandle.mesh: Arc<MeshNode>` is redundant once T1 lands

`net/crates/net/src/ffi/aggregator.rs:119-129` (and parallel
`FoldQueryClientHandle:416-422`, Node `RegistryClient:180`, Python
`PyRegistryClient:229`).

Held *only* so `set_deadline` can rebuild the client. After T1
lands, the inner `RegistryClient` already holds its own
`Arc<MeshNode>` and the handle's `mesh` field can be dropped.

### T9 — `classify` + `classify_fold_query` are parallel nested matches

`net/crates/net/src/ffi/aggregator.rs:607-621, 665-691`.

Two parallel nested matches; Transport/Codec arms are identical, only
Server differs. Minor smell, not worth a refactor on its own.

Fix: extract
`fn classify_transport_codec<E: Display>(t: &Transport, c: &str) -> (i32, String)`
or accept the duplication.

### T10 — `--print-bootstrap` uses hand-formatted JSON via `println!` with `{{` escaping

`net/crates/net/aggregator-daemon/src/lib.rs:248-264`.

Hand-formatted via `println!` with literal `{{`/`}}` escaping. No
escaping for `bound_addr` (IP:port is safe today, but breaks if
`bind_addr` ever supports Unix sockets / paths). The matching test
asserts via `contains("\"bound_addr\":\"127.0.0.1:")` — substring
assertions miss field-order or extra-comma bugs.

Fix: `serde_json::json!({...}).to_string()`. Assert in test via
`serde_json::from_str::<Value>(line).unwrap()`.

### T11 — `net_visibility_t` lacks an exhaustiveness check in the opposite direction

`net/crates/net/src/ffi/aggregator.rs:90-113`,
`net/crates/net/include/net.h:42-47`.

`NetVisibility::from_raw` is the only mapping. If substrate
`Visibility` gains a variant (e.g. `Visibility::Custom(u8)`), this
`#[repr(i32)]` enum + `from_raw` matching becomes a silent bug. Doc
comment claims values are "representation-stable across SDK releases"
but nothing enforces that statement.

Fix: add a compile-time `match Visibility { ... }` returning
`NetVisibility` to force exhaustiveness — or just a `#[deny(unreachable_patterns)]`
match in the opposite direction.

---

## LOW — efficiency / cosmetic

### T12 — Fixed `sleep(50ms)` for handshake ordering in round-trip test

`net/crates/net/sdk/tests/aggregator_registry_client_round_trip.rs:43`.

Same brittleness pattern called out in prior reviews; should
`join_all` the accept + connect rather than artificial delay.

### T13 — Heavy WHAT-narrating doc comments

`ffi/aggregator.rs:33-39, 117-129, 343-350, 700-706`, binding `.ts`
`:1-29`, `aggregator.go:1-48, 240-275`, Python module preamble
`:1-27`. Many restate the API shape visible one line below. Trim
~30–40% to focus on contracts/invariants. Keep the lifetime contract
on `last_error_detail` (genuinely non-obvious).

### T14 — Per-byte `format!("{b:02x}")` hex encoding in three sites

`ffi/aggregator.rs:720, 173`-ish; matching Python `:173` and Node
`:120`.

Allocates one small `String` per byte. Swap for `hex::encode` (already
a workspace dep). Negligible per-call cost but tidier and consistent
with the `hex::decode` migrations from pass 1.

---

## False positives noted during the pass

- **`block_on` allocates a fresh tokio runtime per FFI call:**
  confirmed false. `ffi/mesh.rs:205-222` uses a single static
  `OnceLock<Arc<Runtime>>` re-exposed via `pub(super) fn block_on`,
  reused by `ffi/aggregator.rs:577-579`.
- **`mesh_node_arc` round-trips through `Arc::increment_strong_count` +
  `Arc::from_raw`:** confirmed false. `ffi/mesh.rs:632-634` is one
  `Arc::clone`.
- **`--print-bootstrap` runs hot on startup:** confirmed false. Runs
  *after* `mesh.start()`, prints + flushes once, no hot-path cost.
- **`last_error_detail` mutex is contended:** confirmed false.
  `parking_lot::Mutex`, only contended when an op fails AND another
  thread is reading the detail. Uncontended hot path is one atomic
  CAS.
- **`HealthMonitor` exponential-backoff math is per-tick overhead:**
  confirmed false. Only the test was reformatted in this PR; no math
  change.
- **JSON marshalling is hot:** false for `list` / `query_*` (operator
  tooling, not high-rate). Worth revisiting only if a binding starts
  polling.

---

## Clean areas

- **Shared tokio runtime for FFI** — `ffi/mesh.rs:205-222`, reused by
  `ffi/aggregator.rs`. No per-call runtime construction.
- **`mesh_node_arc()` helper** — one `Arc::clone`, no FFI round-trip
  via `_arc_clone` / `_arc_free`.
- **SDK `BoundRegistryClient`** — genuinely new wrapper; no
  pre-existing "bind a client to a node id" pattern to consolidate
  against. Fine as introduced.
- **Per-language error classification** (`registry_err` /
  `fold_query_err` in Node/Python/Go) — genuine per-language work
  (NAPI `Error`, PyO3 `PyErr` w/ exception hierarchy, Go typed-error
  struct). No shared substrate primitive could have replaced this.
- **Wire-shape POJOs** (`RegistryGroupSummaryJs`, `summary_to_dict`,
  Go struct) — unavoidable in PyO3/NAPI/Go-json. Not duplication
  beyond T2.
- **Round-trip integration test** is solid; the one fixed sleep
  (T12) is the only nit.

---

## Suggested fix order

1. **T1 + T6 + T8 in one stroke** — substrate
   `set_deadline_mut(&mut self, _)`; FFI handle wraps client in
   `parking_lot::RwLock`; align all three bindings to in-place
   mutation; drop the redundant `mesh` field. Highest-impact, removes
   the UB AND the cache-blow AND the cross-binding inconsistency.
2. **T2** — collapse `go/aggregator.go` to import from
   `bindings/go/net/aggregator.go`. One file deletion.
3. **T5** — swap Node's local `bigint_u64` for `crate::common::bigint_u64`
   (silent-wrap bug fix).
4. **T11** — add the exhaustiveness check for `Visibility`.
5. **T3** — error-kind mirror test (or generator if you're feeling
   ambitious).
6. **T4** — replace hand-rolled JSON encoder with `serde_json`. 130
   LOC deleted.
7. **T7** — FFI op-handler helper.
8. **T9, T10, T12–T14** — cosmetic / defer.
