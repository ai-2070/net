# MeshDB branch code review — pass 2 — 2026-05-13

Branch: `meshdb` (46 commits, +21,443 / -633 LOC vs `master`).
Baseline: `CODE_REVIEW_2026_05_13_MESHDB.md` (pass 1, closed).

This pass (1) verifies pass-1's "fix shipped" claims actually landed, and
(2) surfaces new findings pass 1 missed. Item IDs (`NEW-B1`, `NEW-M3`, …)
live in this doc only, per the no-review-IDs-in-code feedback rule.

## Status of pass 1

**Broadly truthful, with two systematic doc-rot patterns:**

- Every Blocker / Major audited (B1–B6, M1–M10) is materially present and
  tested in code. Minors / nits spot-checked clean.
- **M3 misattribution.** Pass 1 says "mix the executor's identity hash into
  call_id"; the actual fix uses a process-global `AtomicU64` counter at
  `federated.rs:121`. Functionally stronger than the doc claims, but the
  cited mechanism doesn't match the code.
- **B5 partial doc rot.** `ExecuteContext` is implemented (`meshdb.go:401`),
  but the older `Execute` doc-comment at `:380-390` still asserts the old
  lying contract.
- **Stale citations.** `n4` cites `federated.rs:294,297`; the actual
  `last_err = Some(err)` is at `:316,320`. `n3` test asserts hits
  behaviorally rather than via the counter the doc described. `m4` uses
  `MeshError::ExecutorError`, not the cited `TransportProtocol` variant
  (which doesn't exist).

None of these undermine the substantive claim of closure — but a future
reader treating pass 1 as ground truth will trip on the M3 mechanism in
particular.

## Blockers

### NEW-B1 — Go SDK `pumpIterRowsContext` truncates payload `size_t` to `C.int`

**Where:** `bindings/go/net/meshdb.go:529`.

`C.GoBytes(unsafe.Pointer(payloadPtr), C.int(payloadLen))`. `C.size_t` is
64-bit on a 64-bit host; `C.int` is 32-bit signed. Any payload `> INT_MAX`
silently truncates or sign-flips negative — `GoBytes` panics on negative
length. The FFI's `net_meshdb_payload_free(ptr, payload_len)` is still
called with the original `size_t`, so the dealloc is sound, but the
Go-side buffer is corrupt or the process aborts. MeshDB rows today are
small, so the bug is latent — but the ABI contract uses `size_t*` and the
SDK silently downgrades.

**Fix:** assert `payloadLen <= math.MaxInt32` (and route oversize to
`ErrMeshDBRuntime`), or replace with `bytes.Clone(unsafe.Slice(payloadPtr,
payloadLen))` which takes a Go `int` from a `size_t` more safely than the
cgo cast.

### NEW-B2 — `ExecuteContext` is not cancellable during the FFI execute call

**Where:** `bindings/go/net/meshdb.go:411, 485`.

`C.net_meshdb_runner_execute(r.ptr, query.ptr)` is called synchronously on
line 411, **before** the goroutine spawns on line 416. The FFI iter is
pre-materialized — `lib.rs:1262-1267` reads from
`iter_ref.rows[iter_ref.next_idx]`, all rows produced at execute time. A
long-running federated join under `ExecuteContext(deadline)` ignores the
deadline until the executor returns. The package preamble at `:16-31` and
the doc at `:395-400` both promise ctx-aware cancellation.

**Fix:** move the FFI execute into the goroutine and signal-back via a
setup channel so the caller still sees errors synchronously where
possible — or document explicitly that ctx affects row-pump only (which
defeats the point, since the iter is already in memory by then).

### NEW-B3 — `iter_next` and most factory FFI entry points are not panic-safe

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:1247` (`iter_next`), `:839`
(`decode_payload_json`), `:607` (`lineage_emit`), every `query_*` factory.

Pass-1 B4 wrapped only the two `runner_execute*` paths in `catch_unwind`.
`iter_next` allocates (`row.payload.clone()`, `into_boxed_slice`),
arithmetics `next_idx`, and dereferences a caller-owned `*mut MeshDbIter`.
Any allocator panic (or future panic added to row decode) unwinds across
`extern "C"` — undefined behaviour. Module-level docs at `:25-30` promise
the structured-error contract for every error; the contract holds only for
execute today.

**Fix:** factor a `ffi_guard!` macro that wraps every entry point in
`catch_unwind(AssertUnwindSafe(...))` and routes panics into
`set_last_error_from_panic`.

## Majors

### NEW-M1 — Go SDK error path drops the FFI structured `kind`

**Where:** `bindings/go/net/meshdb.go:537-542`.

When `iter_next` returns `NET_MESHDB_INVALID_ARG` / runtime error, the
wrapper emits the generic `ErrMeshDBInvalidArg` / `ErrMeshDBRuntime` and
never reads `net_meshdb_last_error_message` / `_kind` (which pass-1 B4
added). The package comment at `:50-53` even pre-promised "downstream
wrappers can call when the status is non-zero". Node has
`parseMeshDbErrorKind`; Go has nothing. Cross-SDK parity gap.

**Fix:** add a Go helper that pulls last-error + kind on every non-OK
status and wraps the sentinel error with the FFI-supplied detail.

### NEW-M2 — `walk_lineage_back` / `walk_lineage_forward` reject `max_depth = 0` on any non-leaf

**Where:** `planner.rs:843-848` (back), `planner.rs:874-881` (forward).

Both walks surface `LineageMaxDepthExceeded` when the frontier has
unvisited neighbors, even when the bound has been respected. A caller
asking for `max_depth = 0` (just-the-origin) gets a typed error if the
origin has any parent.

**Fix:** only error when the walk needs to advance **past** `max_depth`.
Equivalently: skip the error branch entirely when `max_depth == 0`.

### NEW-M3 — `parent_of` cross-node selection is `DashMap`-iteration-order dependent

**Where:** `planner.rs:919-948` and `CapabilityIndex::all_nodes` at
`capability.rs:3409-3411`.

Pass-1 M1 made intra-node tag selection deterministic
(`fork_candidates.sort_unstable()`). The outer loop short-circuits at the
first node it sees that hosts the child via a `causal:` tag.
`all_nodes()` iterates a `DashMap` — order is unstable across runs. If
two nodes both replicate the same chain origin and advertise different
`fork-of:<parent>` tags, the chosen parent depends on map iteration
order. Plan/cache-key drifts run-to-run.

**Fix:** collect candidates across all nodes, sort by `(parent_hash,
node_id)`, pick the first. `children_of` already does this — symmetry.

### NEW-M4 — `LruResultCache::insert` of an oversized result silently evicts itself

**Where:** `cache.rs:283-302` and `:369-374`.

When `result.approx_bytes() > max_bytes`, the entry is inserted at head,
then `evict_until_within_bounds` immediately evicts it from the tail
(it's the only entry). `insert` returns successfully; subsequent `get` is
a miss. Phase F `Permanent` is the named use case for "cache this
forever"; if the result happens to exceed `LRU_MAX_BYTES` (256 MiB), the
cache silently drops it and every subsequent `execute` re-runs the plan.

**Fix:** in `insert`, reject `bytes > max_bytes` up-front; do not insert,
optionally surface a typed signal so the executor can log "result too
large to cache." The entry-count bound already has equivalent protection.

### NEW-M5 — `Predicate::to_wire` recurses unboundedly (user-controlled DoS)

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:728` (`parse_predicate_value`)
and `predicate.rs:441` (`Predicate::to_wire` / `append_to_wire`).

Caller-supplied predicate JSON `{"kind":"not","child":{"kind":"not", ...}}`
~30k deep overflows the FFI thread stack before `serde_json`'s own depth
guard fires. Reachable from Go (parse path), Python, and Node (both build
the typed tree at the SDK layer but the Rust-side `to_wire` recurses on
every execute).

**Fix:** explicit depth bound (≤ 64) in `parse_predicate_value`; convert
`Predicate::to_wire` / `append_to_wire` to an iterative post-order walk.

### NEW-M6 — Validation null-returns in Go FFI don't populate `last_error`

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:231, 312, 323, 337, 401,
423, 450, 507, 544, 607, 702, 1027, 1056` — every `query_*` factory and
both `runner_new*` constructors.

All return `ptr::null_mut()` on validation failure **without** calling
`set_last_error_*`. Only `runner_execute*` set last-error. Go callers who
see a null from `net_meshdb_query_count(...)` cannot distinguish "bad
group_by" from "null inner" from "non-UTF-8 C string." Contradicts module
doc at `:25-35`.

**Fix:** set last-error on every null-returning path (typed
`invalid_arg` kind) — or document that factory errors are not
introspectable.

### NEW-M7 — Node `MeshQueryStream` AsyncIterable has no `return()` / `throw()`

**Where:** `bindings/node/meshdb.ts:37-53`; backing storage at
`bindings/node/src/meshdb.rs:1319-1321`
(`Arc<AsyncMutex<Vec<ResultRow>>>`).

The shim only defines `next()`. When a consumer does `for await (const r
of stream) { if (r.seq === 5n) break; }`, the iteration protocol's
`return()` hook is supposed to clean up — without it, the backing
`Vec<ResultRow>` (potentially 10k+ rows) stays pinned on the AsyncMutex
until JS GC eventually drops the `MeshQueryStream`. Same path on
exception unwind inside the loop.

**Fix:** add `return()` that locks + `std::mem::take`s the vec.
Optionally `throw()` for symmetry.

### NEW-M8 — `include/README.md` contradicts the C header on error reporting

**Where:** `include/README.md:912-914, 928-938, 949`.

- `:949` says "Factory functions return NULL on failure (no detail
  channel — use the Python / Node / Go SDKs which wrap this layer when
  structured errors matter)." But `net_meshdb.h:54-67, 463-477`
  documents `net_meshdb_last_error_message` / `_kind` / `clear` as the
  per-thread channel, with explicit `runtime_panic` kind from
  `catch_unwind`.
- The MeshDB operator-families table at `:928-938` omits these
  functions.
- Quickstart at `:912-914` prints with `%llx` / `%llu` and
  `(unsigned long long)` casts — same pattern pass-1 n1 fixed in
  `examples/meshdb.c` but not propagated here.

**Fix:** rewrite the error-reporting paragraph; add a row to the
operator-families table; migrate the quickstart to `PRIx64` / `PRIu64`.

### NEW-M9 — Go SDK `MeshDBQueryBuilder` source-reset drops accumulated `err` and can UAF aliases

**Where:** `bindings/go/net/meshdb.go:1146-1166`.

`At` / `Between` / `Latest` reset state and return a fresh builder,
dropping any prior `b.err` (e.g., from a `Filter` with a bad predicate).
Contradicts the comment at `:1109-1122` "builder records err and Build
surfaces the first error encountered."

Worse: aliased-builder use-after-free. `base := orig.Between(...); a :=
base.Count(); base.At(...)` — `base.At` calls `base.resetState()` which
`Free()`s the `Between` handle that `a.state` still references.

**Fix:** source methods preserve `b.err`. For the aliasing concern,
either (a) source-reset only mutates the new builder it returns and
leaves the receiver alone, or (b) document that builders are not safe to
alias across source-resets.

## Minors

### NEW-m1 — `JoinKeyMode::Origin` and `JoinKeyMode::Field("origin")` hash to different bytes

**Where:** `executor.rs:1004-1018` and mirror at `federated.rs:982-1000`.

`Origin` mode hashes on `row.origin.to_le_bytes()` (8 raw bytes);
`Field("origin")` hashes on the 16-char hex string. The planner routes
`"origin"` field references to `JoinKeyMode::Origin`
(`planner.rs:1520`), so no real query hits this — but the divergence is
a footgun for any future code path (manual `OperatorPlan` in tests,
future planner that emits `Field` directly). Two probe tables built
under different modes will not cross-correlate even though they encode
the same concept.

**Fix:** `try_encode_join_key` for `Field("origin")` / `Field("seq")`
should fall through to the row-intrinsic encoding (canonicalize at the
executor as defense-in-depth).

### NEW-m2 — `parseMeshDbErrorKind` regex disallows digits

**Where:** `bindings/node/meshdb.ts:90`.

`/^<<meshdb-kind:([a-z_]+)>>(.*)$/s` won't decode a future kind like
`protocol_v2_mismatch`. Current substrate kinds are all `[a-z_]`, so
this is forward-compat only.

**Fix:** `[a-z0-9_]+`.

### NEW-m3 — Python `test_join_accepts_watermark_secs_kwarg` asserts row count only

**Where:** `bindings/python/tests/test_meshdb.py:378-404`.

The test calls with `watermark_secs=2.5` and `NaN`, but only checks
`len(rows) == 1` (same regardless of watermark under snapshot semantics).
Doesn't actually verify the clamp fired. The header (line 286-289) and
the Rust shim clamp non-finite to 5.0 silently — no observable side
effect from Python. A regression that silently removed the clamp wouldn't
be caught.

**Fix:** acceptable as-is; flag in the test docstring that the clamp is
not asserted observable today. Real pinning requires substrate-level
introspection.

### NEW-m4 — Test gaps

Searched `test_meshdb.py` (895 lines, 67 tests) and `meshdb.test.ts`
(1025 lines):

- **Unicode in tag bodies / predicate values**: zero tests.
- **Very-large lineage chains**: largest entry list is 3
  (`test_lineage_emit_yields_one_row_per_entry`). No 10k-entry stress.
- **Malformed predicate JSON**: only Go has it
  (`ffi_filter_with_bad_json_returns_null` at `:1531`). Python / Node
  build the predicate as a typed object, so the JSON shape isn't
  user-facing there — but `predicate_to_inner` could still drop fields
  silently if e.g. a future kind requires multiple children.
- **Empty `group_by` list distinct from `null`**: no test pins the
  current behavior (`[]` == `None`, single-bucket aggregate).
- **Single-row aggregate**: avg / sum / percentile over a 1-row input
  not pinned anywhere. Percentile-on-singleton edge of nearest-rank is
  a classic off-by-one.

### NEW-m5 — C header const-correctness gaps

**Where:** `include/net_meshdb.h:335` (`runner_new(MeshDbReader*)`),
`:355-358` (`runner_execute(...MeshDbQuery*)`), `:372-378`
(`runner_execute_with(...MeshDbQuery*)`).

None of the three mutate the borrowed handle across the call. Composite
factories (`:240-323`) consistently use `const MeshDbQuery* inner`. C++
callers holding a `const MeshDbQuery*` must `const_cast` to execute.

**Fix:** add `const` (or document why non-const).

### NEW-m6 — C example doesn't exercise the cached runner

**Where:** `examples/meshdb.c:91`.

Only `net_meshdb_runner_new` — never `runner_new_cached` or
`runner_execute_with`. The header documents both as primary API surface
(`:343, :372`). Python / Node / Go tests cover the cached path; the C
example doesn't show callers how to opt in.

**Fix:** add a fourth section to `main()` that runs the same `Latest`
through a cached runner with `NET_MESHDB_CACHE_PERMANENT`.

### NEW-m7 — `MeshDbRunner.executor: Arc<...>` is single-owner across all three shims

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:157`,
`bindings/python/src/meshdb.rs:1363`,
`bindings/node/src/meshdb.rs:1253`.

The Arc lets the async closure capture-by-clone, but the runner owns the
only reference. A plain owned `LocalMeshQueryExecutor<...>` plus
`&self.executor` borrow in `execute` works everywhere. Not load-bearing.

### NEW-m8 — `LineageEntry.depth: u32` exposed as plain JS `number`, not BigInt

**Where:** `bindings/node/src/meshdb.rs:175`, `index.d.ts:2147`.

u32 fits in JS `number` (≤ 2^53), so no lossiness today. But the
asymmetry with `originHash` / `seq` (BigInt) is a footgun if `depth`
ever widens to u64. Flag as "depth stays u32 forever, or migrate to
BigInt now while no one depends on it."

## Categories with no new findings

- **TTL math / `Permanent` short-circuit / `TimeBound` `Instant::elapsed
  >= ttl`** — correct.
- **AtomicBool memory ordering** — `QueryHandle::cancel` uses `SeqCst`
  consistently, conservative-correct.
- **Serde tag/untagged misuse** — only known case (`PredicateWire`
  `#[serde(tag = "kind")]`) is already gated behind pass-1 B1's
  `Option<CacheKey>` bypass.
- **Double-free / dangling-ptr returns** — every returned `*mut` is
  `Box::into_raw`; every accepted `*mut` is consumed via `Box::from_raw`
  exactly once.
- **napi-rs `ThreadsafeFunction`** — not used; execute is plain `async
  fn` driven by napi-rs's runtime. AsyncIterable shim is the only
  callback surface, and m7 is the only issue.
- **C example UB / leaks** — every out-param initialized to 0/NULL
  before the FFI call; every NULL-return path correctly frees prior
  handles. The cached-runner gap (m6) is coverage, not leak.

## Recommended next steps

1. **NEW-B1**, **NEW-B2**, **NEW-B3** should land before any Go SDK
   consumer ships against this branch. All three are foundational
   contract violations the Go bindings make.
2. **NEW-M5** (predicate recursion DoS) is reachable from all three
   SDKs and has one substrate-side fix.
3. **NEW-M3** (`parent_of` cross-node nondeterminism) and **NEW-M4**
   (LRU oversized self-evict) can both silently corrupt the cache
   contract — small fixes, high leverage.
4. **NEW-M8** doc rot in `include/README.md` is a one-paragraph rewrite;
   easiest fix in the punch list.

## Out of scope for pass 2

- True mpsc-streaming in the Node binding (carried from pass 1's m8).
- Wire-subprotocol dispatch hookup (carried from pass 1, tracked by
  `MESHDB_PLAN.md`).
- FederatedMeshQueryExecutor exposure through Python / Node / Go
  (carried from pass 1's M9, M10).
