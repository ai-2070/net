# MeshDB branch code review — 2026-05-13

Branch: `meshdb` (~20K LOC: Rust substrate + Python / Node / Go / C SDK + tests + docs).
Baseline: `master`.

Three parallel review passes covered the Rust substrate, the FFI shims, and the
high-level SDK / header / docs surface. Findings below — punch list grouped by
severity, with each item labeled for tracking in this doc only (per the
"no review-tracking IDs in code or commit messages" feedback rule).

## Status

**Closed.** Every Blocker / Major / Minor / Nit was either fixed
in-tree or — where the original claim turned out to be inaccurate
on inspection — pinned with a regression test and the doc updated
to record the corrected understanding. Tests added: substrate
(`error::tests::kind_discriminator_is_stable_across_variants`,
`cache::tests::key_for_plan_handles_filter_plans_without_panicking`,
`federated::tests::cancel_after_composite_aggregate_short_circuits_materialized_stream`,
`federated::tests::call_id_is_unique_across_federated_executors_on_same_host`,
`planner::tests::plan_chainref_discovered_multiple_origins_surfaces_ambiguous_error`,
`planner::tests::lineage_back_with_multiple_fork_of_tags_is_deterministic`),
Go FFI shim (`ffi_last_error_starts_null_and_clears_correctly`,
`ffi_null_handle_populates_last_error`,
`ffi_mesh_error_kind_round_trip_covers_known_variants`, instrumented
`ffi_cached_runner_round_trips`), Python tests
(`test_join_accepts_watermark_secs_kwarg`), Node tests
(`parseMeshDbErrorKind decodes the <<meshdb-kind:...>> prefix`,
`cachePolicyTimeBound rejects non-finite / negative ttlSeconds at the
factory`, `execute rejects a hand-rolled cachePolicy with a negative
ttlSeconds`, `execute rejects a hand-rolled cachePolicy with an
unknown kind`).

Two items deferred because they need surfaces the SDK doesn't expose
yet: M9 (federated SDK tests — requires plumbing
`FederatedMeshQueryExecutor` + `LoopbackTransport` through Python /
Node) and M10 partial (runner-side error-path tests — runtime
`MeshError` variants need capability-index gating / configurable
budgets / `Discovered` surface, none currently exposed). Both
documented inline.

## Blockers

### B1 — `CacheKey::for_plan` fragile to future un-encodable plans (downgraded from "panics")

**Where:** `src/adapter/net/behavior/meshdb/cache.rs:99`.

The original review claim was that `postcard::to_allocvec(plan).expect(...)`
panics on `Filter` / `Discovered` plans because `PredicateNodeWire` is
`#[serde(tag = "kind")]`. Empirical check: postcard *encodes*
internally-tagged enums fine — only `from_bytes` rejects them with
`WontImplement`. Since the cache only encodes (for hashing), the original
`expect` doesn't fire today.

**Fix shipped anyway (defence in depth):** `CacheKey::for_plan` now returns
`Option<CacheKey>`; cache call sites treat `None` as a transparent bypass.
This is safe-by-default for any future plan variant that becomes
un-encodable. Regression test: build a `Filter` plan, assert a stable
`Some(_)` round-trip today, with the Option return as the load-bearing
contract.

### B2 — `running.handle.cancel()` is a no-op for every composite federated query

**Where:** `src/adapter/net/behavior/meshdb/federated.rs` (HashJoin /
Aggregate* / Window / Filter branches around lines 178, 394, 445, 473, 523,
634, 718).

Each recursive `execute_uncached` allocates a fresh `QueryHandle`; the outer
stream is `futures::stream::iter(out)` built _after_ rows materialize, with no
cancellation wrapper. The local executor at least uses `stream_from_vec`
which honours the handle (`executor.rs:1089`); the federated composite path
doesn't.

**Fix:** wrap the materialized iter in a cancel-aware adapter, and thread the
outer handle's cancel `Arc<AtomicBool>` into recursive sub-calls so inner
sub-fetches abort too. Regression test: federated composite executor +
cancel-before-drain.

### B3 — Go FFI `reader_append` after `reader_free` is undefined behaviour

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:166-193` (doc at `:145`,
"subsequent reader_append calls are visible to this runner" claim at
`:873-875`).

Docs tell users they may free the reader once a runner is built; the runner
clones the `Arc<InMemoryStore>` but the _reader handle pointer_ is freed, so
any later `reader_append` is a use-after-free.

**Fix:** remove the "safe to free afterwards" promise; document that the
reader handle must outlive every runner derived from it (the underlying
`Arc<InMemoryStore>` will still be cheap to keep alive — the leak is the
small handle struct, not the row store).

### B4 — Panic across the C ABI + silent error swallowing in Go FFI execute

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:955-982, 1003-1053`.

User-controlled operators (aggregate div-by-zero, OOM join) can panic inside
`runtime.block_on(async {...})`; unwinding across `extern "C"` is undefined
behaviour. Separately, on `Err(_)` the error detail is dropped — the
module-level doc (`:25-30`) promises `net_last_error_message()` is populated;
nothing writes to it.

**Fix:** wrap the async closure body in `std::panic::catch_unwind` and map to
`NET_MESHDB_RUNTIME_ERR`. Plumb the structured `MeshError` (display + kind)
through a thread-local `LAST_ERROR_*` and expose `net_meshdb_last_error_*`
getters. Regression test: a runner whose iterator panics returns the runtime
error code and the message getter reports it.

### B5 — Go SDK `Execute` cancellation contract is a lie

**Where:** `bindings/go/net/meshdb.go:387-423, 487-521`.

Doc comment says "callers stop reading + drop the channel reference; the
goroutine notices the channel-send block, gives up, and frees the iterator."
Go has no way to signal a sender that the receiver was dropped — a dropped
consumer leaks the goroutine + FFI iterator forever, and the buffered-32
channel will stall once full.

**Fix:** take a `ctx context.Context`, `select` on `ctx.Done()` for each
send, and free the FFI iterator on cancellation. Regression test:
cancel-before-drain leaves no goroutine alive.

### B6 — Go SDK `MeshDBQueryBuilder` source methods leak the previous query

**Where:** `bindings/go/net/meshdb.go:1098-1113`.

`At` / `Between` / `Latest` discard `b.state` without `Free()`. Python / Node
rely on GC; Go's FFI handle does not.

**Fix:** reset must call `b.state.Free()` before constructing the fresh
builder. Regression test: build a chain `.At(...).Latest(...)` and check the
handle counter stays bounded.

## Major

### M1 — Planner non-determinism via `HashSet<Tag>` iteration

**Where:** `planner.rs:912/944` (`parent_of` / `children_of` "first wins")
and `:1068` (`collect_coverage` `max_by_key` tie-break on equal
specificity).

`caps.tags` is a `HashSet`, so "first `fork-of:`" varies run-to-run on
multi-fork-of hosts. The cache key is content-addressed off the plan, so two
runs of the same query produce different keys.

**Fix:** break ties on tag-body lex (`min` / `max` after collecting candidate
tag bodies into a sorted buffer). Regression test: insert two equally-ranked
candidates and assert identical plan/cache-key across N runs.

### M2 — `Discovered` resolution silently drops every match past the first

**Where:** `planner.rs:1043`.

Multi-match `Discovered` returns rows from one chain.

**Fix:** until Phase B fan-out lands, surface as a typed
`MeshError::AmbiguousDiscovery { matches }` instead of quietly truncating.
Regression test: two matching origins, expect typed error.

### M3 — `call_id` is per-executor, not per-(caller, executor)

**Where:** `federated.rs:133` / `executor.rs:246`; the wire contract at
`protocol.rs:135-141` says "unique per (caller, executor) pair while
in-flight".

Two federated executors on the same host hitting the same remote will
collide. LoopbackTransport doesn't demultiplex, so the bug is latent.

**Fix:** mix the executor's local identity hash into the call_id derivation.

### M4 — AST drift across the three FFI shims

**Where:**

- `JoinKeyMode` separator — Python / Node accept `"origin,seq"` and
  `"origin+seq"` (`python/src/meshdb.rs:852`, `node/src/meshdb.rs:542`);
  Go accepts `"origin,seq"` and `"seq,origin"`
  (`go/meshdb-ffi/src/lib.rs:462`).
- `group_by` — Python / Node take a list-of-strings, Go takes a flat
  comma-separated C string.

**Fix:** pick `"origin,seq"` as the only canonical form across all three;
reject everything else. For `group_by`, accept the canonical form in all
three (Go can keep a comma-separated wire format but Python / Node can pass
either a list or the canonical string — the test conformance suite will use
the same JSON shape). Document the canonical form in one place. Regression
tests: feed each rejected variant through each shim and expect the same
typed error.

### M5 — Error fidelity flattened to strings across all FFI shims

**Where:** `python/src/meshdb.rs:1409-1411`,
`node/src/meshdb.rs:1285,1291`, Go execute path.

`MeshError` has structured variants the SDK READMEs claim to expose
(`kind` discriminator) — none of the shims set one.

**Fix:** add a `mesh_error_kind(&MeshError) -> &'static str` helper in the
substrate; wire it through each shim so SDK callers can branch on
`error.kind`.

### M6 — Cache-policy validation drift in Node FFI

**Where:** `node/src/meshdb.rs:290-295,306`.

Python validates at the factory; Go clamps at the factory; Node defers all
validation to the converter, so a literal `{ kind: "time_bound",
ttlSeconds: -1 }` is silently rewritten to 5.0.

**Fix:** validate at the Node factory the same way Python / Go do.

### M7 — Watermark API parity (downgraded: Python already has it)

**Where:** Python `MeshQuery.join(...)` does not surface `watermark_secs`;
Node / Go / C do.

**Status:** false alarm. Empirical check:
`bindings/python/src/meshdb.rs:838` already declares
`#[pyo3(signature = (left, right, kind, key, strategy=None,
watermark_secs=5.0))]` — the kwarg is on the public surface and matches
Node / Go / C semantics (default 5.0, non-finite / negative clamped to
5 s). No code change needed; pinned with a Python regression test
covering both the default and the clamp.

### M8 — BFS in lineage walks uses `Vec::remove(0)` and double-evaluates `children_of`

**Where:** `planner.rs:868-869`.

`remove(0)` is O(n); `children_of(current)` is computed twice per step.

**Fix:** switch to `VecDeque::pop_front` and cache the children list. No
behaviour change, regression-test by parity with existing lineage tests.

### M9 — Federated execution untested at the SDK boundary

**Where:** Python / Node test suites; Go ships no SDK tests.

**Status:** deferred. Adding federated SDK tests requires first exposing
`FederatedMeshQueryExecutor` + `LoopbackTransport` through the Python / Node
FFI shims (and the Go cdylib). Neither shim does today — the surface is a
slice-sized addition. Substrate-level coverage of the federated path
(including the cancellation-after-composite-materialization regression
from B2 and the call_id uniqueness regression from M3) is solid;
cross-language smoke tests come with the future federated SDK slice.

### M10 — No runner-side error-path coverage in SDKs

**Where:** Python / Node tests only assert factory-time validation.

**Status:** partial. The runner-side `MeshError` variants the review
listed are not currently triggerable from the SDK surfaces today:

- `JoinMemoryExceeded` threshold is 256 MiB — not realistically tripped
  from a test.
- `QueryBudgetExceeded` needs configurable per-query budgets the SDK
  doesn't expose yet.
- `AmbiguousDiscovery` needs the `ChainRef::Discovered` surface, also
  not exposed.
- `HistoricalRangeUnavailable` requires capability-index gating; the
  Python / Node SDKs use a `ChainReader` directly with no caps index.

What did ship: M5 added the `kind` discriminator on `MeshError`, with
substrate tests pinning the variant→string mapping
(`error::tests::kind_discriminator_is_stable_across_variants`). The
Node SDK test `parseMeshDbErrorKind decodes the <<meshdb-kind:...>>
prefix` covers the SDK-side plumbing that decodes a runtime error
once one is raised. Real runtime triggers land with the future
slices that add capability-index gating, configurable budgets, and
the `Discovered` SDK surface.

## Minor

### m1 — `group_key_for` defensive fallback for `JoinKeyMode::Field`

**Where:** `executor.rs:937-945`, `federated.rs:577,695`.

Falls back to `GroupKey::Origin(row.origin)` for `JoinKeyMode::Field`; a
future planner path that emits `Field` group_by would silently collapse
rows.

**Fix:** `unreachable!()` with a descriptive message.

### m2 — `row_overhead: u64 = 64` magic constant

**Where:** `cache.rs:135`.

**Fix:** replace with `std::mem::size_of::<ResultRow>() as u64`.

### m3 — `DefaultHasher` comment claims SipHash

**Where:** `cache.rs:99`.

**Fix:** comment correction.

### m4 — `translate_responses` treats premature `None` as clean EOS

**Where:** `federated.rs:948`.

**Fix:** emit `ExecutorError::TransportProtocol` instead.

### m5 — Three-way duplicated hash-join body

**Where:** `executor.rs:467`, `federated.rs:740`, plus sort-merge variants.

**Fix:** extract a `join_build_side` helper.

### m6 — C header threading section is incomplete

**Where:** `include/net_meshdb.h:56-64`.

**Fix:** document that `MeshDbRunner` and `MeshDbIter` are safe to move
across threads but not safe to call concurrently from multiple threads
without external synchronisation. The Go binding (`meshdb.go:387`) already
crosses a goroutine boundary, so the contract is "yes, move-safe".

### m7 — `meshdb.ts` re-export downgrades types to `unknown`

**Where:** `bindings/node/meshdb.ts:59-60`.

**Fix:** drop the typed re-export; the shim only needs to install the
`AsyncIterable` side-effect.

### m8 — `MeshQueryRunner.execute` materializes rows inline on one tokio worker

**Where:** `bindings/node/src/meshdb.rs:1273-1302`.

**Fix:** acknowledge in the SDK README that streaming is the follow-up; not
worth refactoring in this pass (true mpsc-streaming requires a napi-streams
upgrade per the module doc).

### m9 — `Runtime::new()` per runner

**Where:** `python/src/meshdb.rs:1357`, `go/meshdb-ffi/src/lib.rs:886`.

**Fix:** swap to a shared `OnceLock<Runtime>` per shim.

### m10 — `MESHDB_PLAN.md` self-inconsistency

**Where:** `docs/plans/MESHDB_PLAN.md:7` (and surrounding "Status" block).

Claims all six phases shipped but lists the wire-subprotocol dispatch
hookup as remaining. Phase B is partial.

**Fix:** annotate Phase B as 🚧 wire-dispatch outstanding; split the
"remaining work" list to make this explicit.

### m11 — `CORTEX_ADAPTER_PLAN.md` cites file:line refs that rot

**Where:** `docs/plans/CORTEX_ADAPTER_PLAN.md:14-16`.

**Fix:** drop the line numbers; keep the path references.

### m12 — Go FFI `MeshDbRunner.runtime: Arc<Runtime>` is single-owner

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:62`.

**Fix:** rolled in with m9 (shared `OnceLock<Runtime>`).

## Nits

### n1 — `examples/meshdb.c` uses `%llx` / `(unsigned long long)` casts

**Fix:** switch to `<inttypes.h>` `PRIx64` / `PRIu64`.

### n2 — Python `lineage_emit` doc-comment misattached

**Where:** `bindings/python/src/meshdb.rs:802-814`.

**Fix:** move the docstring to the correct factory.

### n3 — Go FFI `ffi_cached_runner_round_trips` does not actually assert a cache hit

**Where:** `bindings/go/meshdb-ffi/src/lib.rs:1421-1456`.

**Fix:** instrument a counter on the cache (or check `len` after a hit) and
assert hit count grows.

### n4 — Duplicated translate_responses + last_err rebuild

**Where:** `federated.rs:294,297`.

**Fix:** `last_err = Some(err)`.

## Out of scope for this pass

- True mpsc-streaming in the Node binding (M8 is documentation only).
- Wire-subprotocol dispatch hookup (called out by the plan; tracked
  separately).
- Go SDK federated test (touched by M9 only to the extent of the
  cancellation regression test from B5).
