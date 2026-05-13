# Net v0.16 — "Eye of the Tiger"

## MeshDB

MeshDB in Net is the query layer that grows on top of the substrate, and v0.16 is where it lands. Every prior approach to "query the cluster" presupposes a homogeneous shape — a SQL warehouse holds rows in tables, a graph database holds nodes in indexes, a search engine holds documents in shards. There is a query language, and there is data, and the language is shaped to the data. MeshDB inverts the relation. The data is causal chains of events across nodes; the query language composes operators against those chains; the capability index is the planner, the proximity graph is the cost model, the local RedEX file is the storage engine. There is no central catalog. There is no schema service. There is no shuffle plan.

A query in MeshDB is a tree of operators that traverse three axes the substrate already exposes — **time** (a chain's history at a specific seq, or across a seq range), **lineage** (the `fork-of:` graph back to a common ancestor, sibling chains, descendant cohorts), and **chains** (joins across causally-related but distinct chains, aggregates folded across them). The planner reads the capability index to discover which nodes hold which chains, walks the proximity graph to pick the cheapest holder, and emits an execution plan whose root operator is the data and whose leaves are remote sub-queries. Atomic operators (`At` / `Between` / `Latest`) read events from the substrate; composite operators (`Join` / `Filter` / `Aggregate` / `Window` / `LineageEmit`) compose against atomic results without owning state of their own. The runtime is per-node; the plan is per-query; the substrate is unchanged.

The same primitives that let The Warriors find a chain's holders let MeshDB find a chain's history. The same `fork-of:` propagation that lets Distributed RedEX replicate a chain forward lets MeshDB walk a chain's parents backward. The same `PredicateWire` that the Capability System uses to filter peer capabilities lets MeshDB filter rows. Hash-joins and sort-merge joins, exact-min / exact-max / exact-distinct-count / nearest-rank percentile aggregates, tumbling windows on seq, and a single-node LRU result cache all compose without a new wire protocol — every operator either rides the existing capability index, the existing RedEX read path, or a new `SUBPROTOCOL_MESHDB` envelope between federated executors. Plans are byte-deterministic; cache keys are content-addressed off the plan; cache invalidation is pull-based against a global `CapabilityIndex` mutation counter that bumps on every announcement / removal / GC sweep.

Federated execution arrives in code with the substrate. The `FederatedMeshQueryExecutor` fans atomic operators out to remote `target_nodes` via a pluggable `MeshDbTransport`; the `LoopbackTransport` drives three-node integration tests in-process. The wire-side hookup that registers the new `SUBPROTOCOL_MESHDB = 0x0F00` on `MeshNode`'s subprotocol dispatcher is the one piece that stays parked for a consumer to drive — the envelope shapes, the cancellation model, and the cross-node call multiplexing all ship in v0.16. The same model lifts MeshDB out of test-only loopback the moment a real subprotocol consumer (Hermes telemetry replay; Deck cross-rack metrics; AI fine-tuning across forked experiments) wires the dispatch.

The bindings ship in lockstep. Python, Node, Go, and C SDKs all expose the full operator surface — `MeshQuery.at(...)` through `MeshQuery.join(...)`, the typed `Predicate` builder for filters, the fluent `QueryBuilder` for chained pipelines, the Phase F `CachePolicy { Permanent | TimeBound { ttl } }` knobs — plus a sentinel-envelope decoder that turns aggregate / joined / window result rows into host-language objects. Errors carry a structured `kind` discriminator (`planner_error`, `executor_error`, `join_memory_exceeded`, `ambiguous_discovery`, `query_cancelled`, `runtime_panic`, …) so callers can branch without parsing message strings. The substrate's `MeshError` is the single source of truth; every binding reflects it.

There is no separate query service to provision. There is no catalog to maintain. The query plan is on the mesh because the substrate is the database.

---

*Named after Survivor's 1982 Rocky III anthem — a release that asks the substrate to *see*, after Rebel Yell asked it to hold. v0.15 made the Dataforts data plane stand up — content-addressed blobs, heat-driven gravity, read-your-writes. v0.16 stacks the MeshDB query plane on top: a federated AST + planner + executor that composes against the existing capability index, proximity graph, and `causal:` / `fork-of:` tag layer the Warriors substrate ships. No new substrate primitive — every operator rides what was already there. The `meshdb` Cargo feature gates whether the surface compiles at all; the substrate path is unchanged on non-meshdb builds.*

v0.16 lands **the full MeshDB roadmap from [`MESHDB_PLAN.md`](../plans/MESHDB_PLAN.md)** — Phases A, C, D, E, F shipped behind the `meshdb` Cargo feature; Phase B is partial (the federated executor + transport-plug live in code with a `LoopbackTransport` driving in-process integration tests, but the wire-subprotocol hookup that registers `SUBPROTOCOL_MESHDB` on `MeshNode`'s dispatcher waits for a consumer to drive). AST + planner, local + federated executors, lineage walks, hash + sort-merge joins (row-intrinsic + payload-keyed, all four `JoinKind`s), Count / Sum / Avg / Min / Max / DistinctCountExact / PercentileExact aggregates, Filter via synthetic-tag `PredicateWire` evaluation, tumbling-on-seq windowing, and the single-node LRU result cache with pull-based capability-version invalidation are all in code. The full surface ships across Rust core and Python / Node / Go / C SDKs.

The hardening posture from the Black Diamond / Rebel Yell line continues. **Two coordinated code-review passes** landed before the v0.16 branch cut: pass 1 ([`docs/misc/CODE_REVIEW_2026_05_13_MESHDB.md`](../misc/CODE_REVIEW_2026_05_13_MESHDB.md)) surfaced 32 items (6 Blockers, 10 Majors, 12 Minors, 4 Nits); pass 2 ([`docs/misc/CODE_REVIEW_2026_05_13_MESHDB_PASS_2.md`](../misc/CODE_REVIEW_2026_05_13_MESHDB_PASS_2.md)) verified pass-1 closure and surfaced 20 new items (3 Blockers, 9 Majors, 8 Minors). Every Blocker and Major closed in-tree with regression tests; two pass-1 Majors and four pass-2 Minors deferred with rationale (substrate-side coverage is solid; the deferred items need SDK surfaces — `FederatedMeshQueryExecutor`, configurable budgets, `Discovered` — that ship with their respective future slices).

Alongside MeshDB, v0.16 carries a substrate-level **routed-handshake replay-guard fix** that was masking as a flaky NAT-traversal test. The guard previously refused any legitimate re-handshake from a peer with the same Noise static, indistinguishable from a passive attacker replaying captured msg1 bytes. The fix tracks the initiator's Noise ephemeral (in the clear at the front of NKpsk0 msg1) and only refuses replays that match BOTH static and ephemeral — a fresh ephemeral can only be produced by the static + PSK holder, per the Noise threat model. Plus a Duration::MAX-sentinel handling fix in the periodic sweep loops (`spawn_token_sweep_loop`, `spawn_capability_gc_loop`) that previously panicked on Instant-overflow when the documented "disable the sweep" sentinel was used.

The toolchain moves forward: Go 1.26, CI reads the Go version from `go/go.mod` (no more divergence between the local toolchain and the CI matrix), and the cross-binding cgo integration test creates responder / initiator nodes in parallel — eliminating the pre-fix handshake deadlock that randomly flaked the suite. Dependency bumps land cleanly: `ctor` 0.11.1 → 1.0.5, `napi` 3.8.6 → 3.9.0, `napi-build` 2.3.1 → 2.3.2, `napi-derive` 3.5.5 → 3.5.6.

---

## Phase A — `MeshQuery` AST + planner skeleton

The composable query language and the planner that translates queries into typed `ExecutionPlan`s. Lives in `src/adapter/net/behavior/meshdb/{query,planner,error}.rs`.

### `MeshQuery` versioned outer enum

```rust
pub enum MeshQuery {
    V1(QueryV1),
}

pub enum QueryV1 {
    At      { origin: ChainRef, seq: SeqNum },
    Between { origin: ChainRef, start: SeqNum, end: SeqNum },
    Latest  { origin: ChainRef },
    LineageBack    { origin: ChainRef, max_depth: u32 },
    LineageForward { origin: ChainRef, max_depth: u32 },
    Join { left: Box<MeshQuery>, right: Box<MeshQuery>,
           on: JoinKey, kind: JoinKind,
           strategy: JoinStrategy, watermark_secs: f64 },
    Filter { inner: Box<MeshQuery>, predicate: PredicateWire },
    Aggregate { inner: Box<MeshQuery>, group_by: Vec<Expr>,
                agg_fn: AggregateFn },
    Window  { inner: Box<MeshQuery>, spec: WindowSpec },
    Project { inner: Box<MeshQuery>, columns: Vec<Expr> },
    OrderBy { inner: Box<MeshQuery>, by: Vec<Expr>, limit: Option<u32> },
}
```

The `MeshQuery::V1(...)` wrapper is the locked-decision-#1 stability hatch — postcard + JSON round-trip carries the version tag at the front of every wire encoding. `ChainRef` separates direct origin-hash references (`OriginHash(u64)`) from capability-predicate references (`Discovered(PredicateWire)`); the planner resolves `Discovered` against the capability index at plan time and surfaces a typed `MeshError::AmbiguousDiscovery { matches }` when multiple origins match (deferring multi-origin fan-out until Phase B+ ships it explicitly, rather than silently truncating to the first match).

### `MeshQueryPlanner`

```rust
impl<'a, F: Fn(NodeId) -> Option<Duration>> MeshQueryPlanner<'a, F> {
    pub fn new(index: &'a CapabilityIndex, rtt_lookup: F) -> Self { ... }
    pub fn plan(&self, q: &MeshQuery) -> Result<ExecutionPlan, MeshError> { ... }
}
```

Translates atomic operators to typed `ExecutionPlan`s with proximity-ordered `target_nodes` (RTT-asc, lex-NodeId tiebreak). Composite operators wrap their planned children in `NotYetImplemented` placeholders so the tree still type-checks for variants outside this release's executor coverage (`Project`, `OrderBy`).

Plans are **byte-deterministic** — pass-2 review surfaced two non-determinism leaks that the planner closed in this release: (1) `caps.tags` is a `HashSet` whose iteration order is RNG-stable across a single process but not across runs, so `parent_of` / `children_of` / `collect_coverage` collect every candidate, sort numerically, and pick the smallest; (2) `CapabilityIndex::all_nodes` iterates a `DashMap` whose order is unstable, so cross-replica fork-of selection now collects across all hosting nodes before picking. The cache key is content-addressed off the plan, so byte determinism is load-bearing for cache hit rate.

---

## Phase B — Time-travel end-to-end 🚧

🚧 **Partial.** Substrate complete; wire-side subprotocol dispatch hookup outstanding.

### `MeshQueryExecutor` async trait + `LocalMeshQueryExecutor`

```rust
#[async_trait]
pub trait MeshQueryExecutor: Send + Sync {
    async fn execute(&self, plan: ExecutionPlan)
        -> Result<RunningQuery, MeshError>;
    async fn execute_with(&self, plan: ExecutionPlan, options: ExecuteOptions)
        -> Result<RunningQuery, MeshError>;
}

pub struct RunningQuery {
    pub handle: QueryHandle,        // cooperative cancellation
    pub rows: ResultStream,         // Box::pin(Stream<Item = Result<ResultRow>>)
}
```

`LocalMeshQueryExecutor<R: ChainReader>` walks atomic plans against a pluggable `ChainReader` (in-memory store for tests; the integration layer wires it to RedEX). Cancellation flows via `QueryHandle::cancel` which flips an `Arc<AtomicBool>` checked at every row boundary.

### Replica-aware routing — `CausalClaim` parsing

Three `causal:` tag forms get parsed into typed coverage claims: `causal:<hex>` (Presence — no range, permissive fallback), `causal:<hex>:<tip_seq>` (Tip — covers `[0, tip_seq + 1)`), `causal:<hex>[start..end]` (Range — covers `[start, end)`). The planner picks the most-specific-claim winner per holder (`Range` > `Tip` > `Presence`) with a deterministic tie-break key, then filters holders by `covers_seq` / `covers_range`. `HistoricalRangeUnavailable` carries per-replica available-range hints so callers can negotiate.

### Wire protocol envelopes

```rust
pub const SUBPROTOCOL_MESHDB: u16 = 0x0F00;

pub enum MeshDbRequest {
    Execute { call_id: u64, plan: ExecutionPlan },
    Resume  { call_id: u64, token: ContinuationToken },
    Cancel  { call_id: u64 },
}

pub enum MeshDbResponse {
    Batch { call_id: u64, batch: ResultBatch },
    End   { call_id: u64 },
    Error { call_id: u64, error: MeshError },
}
```

Envelopes are defined and round-trip cleanly; `MeshNode::register_subprotocol_handler(SUBPROTOCOL_MESHDB, ...)` is the one piece that ships unwired until a consumer drives it. Substrate-side `FederatedMeshQueryExecutor<T: MeshDbTransport>` already speaks this protocol against `LoopbackTransport` in three-node in-process integration tests.

### `FederatedMeshQueryExecutor` + `LoopbackTransport`

Fans atomic operators out to their proximity-ordered `target_nodes` over `MeshDbTransport`. On `TransportError::NoRoute(target)` the executor falls through to the next target; any other transport error bubbles up inside `MeshError::ExecutorError`. Composite operators (`HashJoin` / `Aggregate*` / `Window` / `Filter`) recurse on the federated executor so atomic leaves still dispatch via the transport.

**Cancellation correctness landed in pass 1.** Pre-fix, each recursive `execute_uncached` allocated a fresh `QueryHandle`; the outer `running.handle.cancel()` was a no-op against the materialized `futures::stream::iter(out)` output of composite operators. Post-fix, one outer handle is allocated in `execute_with` and threaded through `execute_uncached_with_handle` into every recursive sub-fetch, and a `stream_results_cancellable` adapter re-checks the cancel flag per emitted row.

**Call-ID uniqueness landed in pass 1.** The wire contract says `call_id` is "unique per (caller, executor) pair while in-flight". Pre-fix, each `FederatedMeshQueryExecutor` drew IDs from its own `AtomicU64`, so two federated executors on the same caller could collide at a shared remote demultiplexer. Post-fix, a process-global `FEDERATED_CALL_ID_COUNTER` trivially satisfies the contract.

### Replay-guard fix in the mesh's routed-handshake path

Pass-1 work surfaced a routed-handshake replay guard that flagged any legitimate re-handshake from a peer with the same Noise static as a passive replay attack — `connect_direct(peer, via = X)` against an existing session via R would time out at B's side because B refused the new handshake. The fix tracks the initiator's Noise ephemeral (in the clear at the front of NKpsk0 msg1 by Noise pattern) and only `DropReplay`s when BOTH the static AND the ephemeral match. A fresh ephemeral can only be produced by the static + PSK holder (the legitimate peer); a captured-and-replayed msg1 has the original ephemeral verbatim.

```rust
struct PeerInfo {
    node_id: u64,
    addr: SocketAddr,
    session: Arc<NetSession>,
    remote_static_pub: [u8; 32],
    last_initiator_ephemeral: Option<[u8; 32]>, // new
}

fn routed_rotation_outcome(
    existing: &PeerInfo,
    new_static: &[u8; 32],
    new_ephemeral: &[u8; 32],
    session_timeout: Duration,
) -> RoutedRotationOutcome {
    if existing.remote_static_pub == *new_static {
        if existing.last_initiator_ephemeral.as_ref() == Some(new_ephemeral) {
            return RoutedRotationOutcome::DropReplay;
        }
        return RoutedRotationOutcome::AcceptRotation;
    }
    if existing.session.is_timed_out(session_timeout) {
        RoutedRotationOutcome::AcceptRotation
    } else {
        RoutedRotationOutcome::RefuseFresh
    }
}
```

---

## Phase C — Lineage walks via `fork-of:` graph

`OperatorPlan::LineageEmit { origin, direction, entries }` carries a materialized walk result. The planner walks the local capability-index snapshot at plan time — `parent_of` for back, BFS `children_of` lex-sorted for forward, both deterministic across runs after pass-1 M1 and pass-2 M3. Cycle detection ships as explicit visited-set guards (`MeshError::LineageCycleDetected { origin, cycle }` with the path through the cycle for debugging). Depth bounds surface as `MeshError::LineageMaxDepthExceeded { origin, depth }`.

The executor emits one `ResultRow` per entry — payload empty, `origin = entry.origin`, `seq = entry.tip_seq.unwrap_or(SeqNum(0))`. Callers compose with `At` / `Between` to fetch event content for each ancestor / descendant. The federated executor handles `LineageEmit` locally (no remote dispatch needed; the walk already happened at plan time).

**Pass 2 fix:** `max_depth = 0` is now correctly handled as "just-the-origin", not as a bound violation. Both walks previously surfaced `LineageMaxDepthExceeded` whenever the start origin had any unvisited neighbour, even when the caller explicitly asked for zero steps.

---

## Phase D — Cross-chain joins

### D-1 — Inner hash-join on row-intrinsic keys

`OperatorPlan::HashJoin { left, right, key_mode, kind, strategy, watermark }` with `JoinKeyMode::{Origin, Seq, OriginSeq}` for the join-key extraction modes Phase D-1 covers. Both local and federated executors implement build-on-left / probe-on-right; the federated path recurses through itself so atomic leaves still dispatch via the transport. Joined rows are sentinel `ResultRow`s (`origin = 0`, `seq = 0`) whose payload is a postcard-encoded `JoinedRowPayload { left, right }`. `MeshError::JoinMemoryExceeded` surfaces at the 256-MiB build-side bound.

### D-2 — Outer joins + sort-merge + payload-keyed

All four `JoinKind`s ship: `Inner` / `LeftOuter` / `RightOuter` / `FullOuter`. `JoinKeyMode::Field(String)` extends the join-key surface to JSON payload paths via `row::extract_string_projection`; `try_encode_join_key` returns `Option<Vec<u8>>` so rows whose key field can't be resolved are silently dropped from both sides. `JoinStrategy::{HashBroadcast, SortMerge}` lets the planner pick between in-memory hashing (default; trips `JoinMemoryExceeded` past the bound) and sort-merge (sort both sides + two-pointer walk; memory-bounded by the inputs).

**Pass 1 m5:** the three-way duplicated hash-join body (local one-sided + local full-outer + federated mirror) factored into a shared `build_hash_join_table(rows, key_mode, strategy_label) -> Result<HashJoinTable, MeshError>` helper. **Pass 2 m1:** `try_encode_join_key` canonicalizes `JoinKeyMode::Field("origin"|"seq"|"origin,seq")` to the matching row-intrinsic encoding so probe tables built under `Origin` and `Field("origin")` cross-correlate.

Watermark is informational under snapshot semantics; streaming activation needs a future windowed-join slice. The locked-decision-#2 default is 5 s.

---

## Phase E — Filter + aggregates + tumbling windows

### E-1 Count

`OperatorPlan::AggregateCount { input, group_by }` over row-intrinsic group keys (`Origin`, `Seq`, `OriginSeq`). Sentinel `ResultRow` per group with a postcard-encoded `AggregateRowPayload { group, value: Count(u64) }`.

### E-2 Filter

Reuses the Capability System's `PredicateWire`. Every `ResultRow` projects to a synthetic `(Vec<Tag>, BTreeMap)` view via `row::synthetic_row_view` — `dataforts.origin`, `dataforts.seq`, plus flat JSON-object payload fields. Non-JSON payloads are opaque; predicates against missing fields simply don't match.

**Pass 2 NEW-M5:** the FFI's JSON predicate parser bounds caller-supplied recursion at 64 deep (`PREDICATE_PARSE_MAX_DEPTH`); the substrate's `Predicate::to_wire` converts from recursion to a heap-allocated work stack so 10k+-deep typed predicates from Python / Node factories don't overflow the Rust thread stack on every execute.

### E-3 Sum / Avg

`OperatorPlan::AggregateNumeric { input, group_by, field_path, kind: Sum | Avg }` over `row::extract_numeric` (JSON path → `f64`). Rows whose field fails to resolve are skipped; `Avg(None)` covers the empty-group case.

### E-4 Min / Max / DistinctCountExact / PercentileExact

`OperatorPlan::AggregateReduction { kind: Min | Max | Percentile { p } }` over `f64::total_cmp` (so `NaN` ordering is well-defined) + `OperatorPlan::AggregateDistinct { field_path }` (canonical-string projection into a per-group `BTreeSet`). Nearest-rank percentile per locked-decision-#3. The HLL p=14 / T-Digest c=100 sketch variants (`DistinctCountHll`, `PercentileTDigest`) remain `PlannerError` until a consumer drives the algorithmic complexity; the exact variants are the recommended path today.

### E-5 Window

`QueryV1::Window { inner, spec: WindowSpec::TumblingSeq { size } }` buckets rows into fixed-size half-open intervals on `seq`; the executor emits one sentinel `ResultRow` per non-empty bucket with a postcard-encoded `WindowBoundary { start, end, rows }`. Sliding + session windows extend cleanly via additional `WindowSpec` variants when a consumer drives the shape (locked-decision-#7).

---

## Phase F — Single-node LRU result cache

### `CachePolicy` + `ExecuteOptions`

```rust
pub enum CachePolicy {
    Permanent,                   // hold until LRU eviction
    TimeBound { ttl: Duration }, // TTL-bounded; default 5 s
}

pub struct ExecuteOptions {
    pub bypass_cache: bool,             // skip both lookup AND writeback
    pub cache_policy: CachePolicy,
}
```

`TimeBound { ttl: 5s }` is the default policy (mirroring the locked-decision-#2 join watermark). `Permanent` is the explicit-opt-in for queries over closed substrate ranges (`At`, bounded `Between` with `end ≤ current_tip`). `bypass_cache` skips both lookup and writeback (Deck operator-view authoritative reads; Hermes skill-routing under churn; diagnostics).

### Global cache version, pull-based invalidation

`CapabilityIndex` carries an `AtomicU64 mutation_version` that bumps on every `index` / `remove` / `gc` mutation. The MeshDB cache key encodes the live version into `CacheKey { plan_hash: u64, capability_version: u64 }`; any divergence misses. Aggressive invalidation by design — softening it is not the answer to churn, the `bypass_cache` flag and the `Permanent` policy together cover the cases where staleness is preferable.

### `CacheKey::for_plan` is encode-failure-safe

```rust
impl CacheKey {
    pub fn for_plan(plan: &ExecutionPlan, capability_version: u64) -> Option<Self>;
}
```

Returns `None` when the plan can't be postcard-encoded (currently: any plan variant carrying a `PredicateWire`, because `PredicateNodeWire` uses `#[serde(tag = "kind")]` which postcard rejects on decode). Cache call sites treat `None` as a transparent bypass rather than a panic — defence-in-depth against future plan variants that become un-encodable.

### Hand-rolled LRU

`HashMap<CacheKey, Node>` + intrusive doubly-linked list over a `Vec<Node>`. Defaults: `LRU_MAX_ENTRIES = 1024`, `LRU_MAX_BYTES = 256 MiB`; either bound trips eviction of the LRU end. `DefaultHasher` over postcard-encoded plan bytes; no new external dependency.

**Pass 2 NEW-M4:** `insert` of an oversized result (`approx_bytes() > max_bytes`) now refuses up-front instead of inserting at head and immediately evicting itself from the tail. Pre-fix, a `Permanent`-policy cache call for an oversized result silently re-ran the plan on every subsequent execute; post-fix the no-op insert leaves the cache entry-count + byte-count untouched and the prior entry at the same key (if any) survives.

Top-level only — sub-plan executes inside the federated path bypass the cache. Recursive caching at HashJoin sides / Aggregate inner is a follow-up if profiling justifies the bookkeeping.

---

## SDK shims — Python / Node / Go / C

Every binding ships the full operator surface in lockstep: atomic factories (`at` / `between` / `latest`), composite factories (`window` / `count` / `numeric_agg` / `percentile` / `join` / `filter` / `lineage_emit`), the typed `Predicate` builder, the fluent `QueryBuilder`, the Phase F cache options, and a sentinel-envelope decoder that turns aggregate / joined / window result rows into host-language objects. The substrate's `MeshError` reflects through every shim with a structured `kind` discriminator.

### Python — pyo3 + maturin

`MeshQuery` / `MeshQueryRunner` / `ResultRow` / `Predicate` / `QueryBuilder` ship as `#[pyclass]` types in the `_net` extension module, re-exported from the `net` Python package behind the `dataforts` / `meshdb` extras. The sync `MeshQueryRunner.execute(query, options)` returns `list[ResultRow]`; aggregate / joined / window payloads decode via `ResultRow.decode_aggregate()` / `decode_joined()` / `decode_window()`.

`MeshDbError` carries a structured `kind` attribute set via PyO3 `setattr` on the raised instance — callers branch on `except MeshDbError as e: if e.kind == "join_memory_exceeded": ...`.

### Node — napi-rs

`MeshQuery` / `MeshQueryRunner` / `MeshQueryStream` / `ResultRow` / `Predicate` ship through napi-rs 3.9. `runner.execute(query, options)` returns a `Promise<MeshQueryStream>`; the TS shim at `bindings/node/meshdb.ts` attaches `Symbol.asyncIterator` so `for await (const row of stream)` works.

**Pass 2 NEW-M7:** the AsyncIterable shim now defines `return()` and `throw()` hooks that call `MeshQueryStream::release()` on a `break` / exception unwind, freeing the backing `Vec<ResultRow>` immediately rather than pinning it on the AsyncMutex until JS GC fires.

Node errors embed the kind discriminator in the reason string via a `<<meshdb-kind:KIND>>MSG` prefix; the SDK ships `parseMeshDbErrorKind(err) -> { kind, message } | null` to decode it.

### Go — cgo + reference SDK contract

`net-meshdb-ffi` is a cdylib exporting the C ABI (`net_meshdb_*` symbols); the Go-side reference contract at `bindings/go/net/meshdb.go` wraps it in a cgo-importing package with `MeshDBReader` / `MeshDBQuery` / `MeshDBRunner` / `MeshDBQueryStream` / `MeshDBPredicate` types. `Execute` returns a `<-chan MeshDBResult`; the fluent `MeshDBQueryBuilder` chains source / filter / aggregate / window / join steps.

**Pass 2 NEW-B1 / NEW-B2 / NEW-B3 / NEW-M1 / NEW-M6 / NEW-M9** all closed for the Go SDK and the underlying FFI cdylib:

- Safe `size_t → int` payload conversion via `unsafe.Slice` + `bytes.Clone` — refuses payloads above `math.MaxInt` with `ErrMeshDBRuntime` rather than letting `C.GoBytes`'s `C.int` cast silently truncate.
- `ExecuteContext` / `ExecuteWithContext` run the FFI execute call inside the spawned goroutine; the caller is never blocked on cgo, and `ctx.Done()` races the executor concurrently with row pumping.
- An `ffi_guard!` macro wraps every FFI entry point in `catch_unwind`; panics across the C ABI become `null_mut()` returns with kind `runtime_panic` populated on the thread-local last-error pair.
- Every factory validation null-return populates `net_meshdb_last_error_message` / `_kind` with a descriptive `invalid_arg` message; Go-side `wrapMeshDBError(sentinel)` reads both into a `MeshDBError` that wraps `ErrMeshDBInvalidArg` / `ErrMeshDBRuntime` for `errors.Is` routing.
- `MeshDBQueryBuilder` source-resets (`.At` / `.Between` / `.Latest`) preserve the accumulated `b.err` so Build still surfaces the first error in the chain; deterministically free the prior `*MeshDBQuery` handle in place; aliasing semantics documented explicitly.

### C — `libnet_meshdb` cdylib + `net_meshdb.h`

The C header at `include/net_meshdb.h` documents every entry point: opaque handles (`MeshDbReader` / `MeshDbQuery` / `MeshDbRunner` / `MeshDbIter`), atomic + composite factories, runner + execute, the sentinel-envelope decoder, and the per-thread last-error trio (`net_meshdb_last_error_message` / `_kind` / `_clear_last_error`). A runnable example at `examples/meshdb.c` walks the canonical lifecycle — reader populate → atomic / composite / lineage query → execute → drain — plus a fourth section exercising the cached runner under `NET_MESHDB_CACHE_PERMANENT`.

**Pass 2 NEW-M5 + NEW-m5:** `runner_new` / `runner_new_cached` / `runner_execute` / `runner_execute_with` take their borrowed handles by `const T*` for C++ const-correctness; Rust FFI signatures match (`*const T`).

---

## Hardening — MeshDB two-pass code review

Two coordinated code-review passes landed before the v0.16 branch cut.

### Pass 1

[`docs/misc/CODE_REVIEW_2026_05_13_MESHDB.md`](../misc/CODE_REVIEW_2026_05_13_MESHDB.md). 32 items: 6 Blockers, 10 Majors, 12 Minors, 4 Nits. Every Blocker and Major closed in-tree with regression tests; two Majors (M9 federated SDK tests, M10 runner-side error-path coverage) deferred with rationale (the SDK surfaces needed to express the tests — federated executor exposure, configurable budgets, `Discovered` resolution — ship with future slices).

**Blockers** (all closed):
- **B1** — `CacheKey::for_plan` now returns `Option<CacheKey>` (defence-in-depth against future un-encodable plans); pinned with a regression test verifying current Filter plans still encode.
- **B2** — Federated `handle.cancel()` no longer no-ops on composite-operator output streams; the outer handle is threaded through every recursive sub-fetch and the materialized output wraps in a cancel-aware adapter.
- **B3** — Go FFI reader / runner lifetime contract documented: snapshot-then-free vs keep-alive, never free-then-append.
- **B4** — Every Go FFI execute path traps panics via `catch_unwind`; the structured `MeshError` (display + kind) flows through a thread-local `LAST_ERROR_*` and three getters.
- **B5** — Go SDK `ExecuteContext` / `ExecuteWithContext` take `context.Context`; pumping goroutine `select`s on `ctx.Done()` per send. Drop-the-channel-to-cancel was a documented lie.
- **B6** — `MeshDBQueryBuilder` source-resets free the prior `*MeshDBQuery` handle deterministically.

**Majors** (all but M9 / M10 closed; M9 / M10 deferred with rationale).

### Pass 2

[`docs/misc/CODE_REVIEW_2026_05_13_MESHDB_PASS_2.md`](../misc/CODE_REVIEW_2026_05_13_MESHDB_PASS_2.md). 20 new items: 3 Blockers, 9 Majors, 8 Minors. Every Blocker and Major closed; four Minors (NEW-m3, NEW-m4, NEW-m7, NEW-m8) deferred as acceptable.

**Pass-2 Blockers** (all closed):
- **NEW-B1** — Go SDK `pumpIterRowsContext` was truncating `size_t` payloads to `C.int`. `C.size_t` is 64-bit, `C.int` is 32-bit signed; `C.GoBytes` would silently sign-flip or truncate. Replaced with `unsafe.Slice` + `bytes.Clone` + a `math.MaxInt` guard that surfaces `ErrMeshDBRuntime` on oversized payloads.
- **NEW-B2** — `ExecuteContext` was calling the FFI execute synchronously on the caller's goroutine before spawning the pump; long-running joins under `ctx.Done()` ignored the deadline until execute returned. Restructured: the FFI execute call now runs inside the pumping goroutine; the caller selects on `ctx.Done()` against the channel concurrently.
- **NEW-B3** — Every FFI entry point (not just the two `runner_execute*` paths from pass-1 B4) wraps its body in `catch_unwind` via a new `ffi_guard!($default, { ... })` macro. Panics become `null_mut()` / `NET_MESHDB_RUNTIME_ERR` with kind `runtime_panic` populated.

**Pass-2 Majors** (all closed):
- **NEW-M1** — Go SDK wraps every non-OK FFI return with `MeshDBError { Sentinel, Kind, Message }` that reads the thread-local last-error pair. Brings Go to parity with Python's `MeshDbError.kind` and Node's `parseMeshDbErrorKind`.
- **NEW-M2** — Lineage walks accept `max_depth = 0` as "just-the-origin"; previously a present parent / child tripped `LineageMaxDepthExceeded`.
- **NEW-M3** — `parent_of` collects across all replica hosts before picking the lex-smallest parent. Pre-fix the outer DashMap iteration short-circuited on the first hosting node, drifting the plan + cache key across runs.
- **NEW-M4** — `LruResultCache::insert` of an oversized result refuses up-front instead of silently evicting itself.
- **NEW-M5** — JSON predicate parsing bounds depth at 64; `Predicate::to_wire` converts to an iterative heap-allocated work stack.
- **NEW-M6** — Every Go FFI factory's validation null-return populates `last_error_*` with a descriptive `invalid_arg` message.
- **NEW-M7** — Node AsyncIterable shim defines `return()` / `throw()` that release the backing `Vec<ResultRow>` via a new `MeshQueryStream::release()` napi method.
- **NEW-M8** — `include/README.md` error-reporting paragraph rewritten to match the actual `net_meshdb_last_error_*` contract; operator-families table gains the last-error row; quickstart migrated to `<inttypes.h>` `PRIx64` / `PRIu64`.
- **NEW-M9** — `MeshDBQueryBuilder` source-resets preserve `b.err`; aliasing across source-resets documented explicitly (reviewer's option b).

### Substrate-side hardening (alongside the MeshDB passes)

- **Routed-handshake replay guard now tracks the initiator's Noise ephemeral.** Pre-fix, the guard refused any same-static re-handshake — indistinguishable from a passive attacker replaying captured msg1 bytes. The `connect_direct(peer, via = X)` retarget path (`connect_direct_retargets_coordinator_does_not_short_circuit_on_stale_session`) failed with a handshake-timeout against an existing session. Post-fix, `routed_rotation_outcome` only `DropReplay`s when BOTH the static AND the initiator's ephemeral match.
- **`Duration::MAX` sentinel handled in periodic sweep loops.** `spawn_token_sweep_loop` and `spawn_capability_gc_loop` both documented `Duration::MAX` as "disable the loop". The implementations forwarded that value to `tokio::time::interval(MAX)`, which panics on `Instant + MAX` overflow. Both loops now short-circuit to `shutdown_notify.notified().await` when the interval is `MAX`.

---

## Toolchain + dependency upgrades

### Go 1.26

The Go toolchain bumps from 1.21 to 1.26. CI now reads the Go version directly from `go/go.mod` (`go-version-file:` in `actions/setup-go@v5`) so the local toolchain and the CI matrix can't drift. The bump unlocks Go's improved `unsafe.Slice` ergonomics that the pass-2 NEW-B1 fix uses for safe `size_t → int` payload conversion.

### Integration-test parallel handshake setup

The cross-binding cgo integration test (`go/integration_test.go`) refactored to create responder and initiator nodes in parallel via `errgroup.Group`. Pre-fix, sequential construction would occasionally deadlock when both nodes' handshake state machines waited on each other's first packet; the parallel construction breaks the cycle and reduces flakiness across CI runs.

### Dependency bumps

- **`ctor`** 0.11.1 → 1.0.5 (Rust constructor / destructor attributes; cleaner 1.x API for the static-init registration paths).
- **`napi`** 3.8.6 → 3.9.0 (napi-rs runtime — Node binding surface).
- **`napi-build`** 2.3.1 → 2.3.2 (napi-rs build script).
- **`napi-derive`** 3.5.5 → 3.5.6 (napi-rs derive macros).

No source-level changes in the bindings — straight `Cargo.lock` refresh.

---

## Test hygiene

- **Lib suite at 2705+ tests** (was 2645+ at v0.15 release). 60+ net new tests across the MeshDB phases + cross-cutting fixes; every numbered review item from both passes ships with at least one regression where the shape made one possible. Notable additions:
  - **Substrate:** `error::tests::kind_discriminator_is_stable_across_variants`, `cache::tests::lru_rejects_oversized_entry_instead_of_self_evicting`, `cache::tests::key_for_plan_handles_filter_plans_without_panicking`, `federated::tests::cancel_after_composite_aggregate_short_circuits_materialized_stream`, `federated::tests::call_id_is_unique_across_federated_executors_on_same_host`, `planner::tests::plan_chainref_discovered_multiple_origins_surfaces_ambiguous_error`, `planner::tests::lineage_back_with_multiple_fork_of_tags_is_deterministic`, `planner::tests::lineage_back_across_multiple_replica_hosts_is_deterministic`, `planner::tests::lineage_{back,forward}_with_max_depth_zero_returns_only_start_no_error`, `predicate::tests::to_wire_handles_deep_nesting_without_stack_overflow`, `executor::tests::join_key_field_origin_canonicalizes_to_intrinsic_encoding`, `mesh::*::routed_rotation_outcome_accepts_reinit_with_fresh_ephemeral`.
  - **Go FFI:** `ffi_guard_traps_panics_and_records_last_error`, `ffi_factory_validation_failure_populates_last_error`, `ffi_filter_with_pathologically_deep_predicate_returns_null`, `ffi_null_handle_populates_last_error`, `ffi_mesh_error_kind_round_trip_covers_known_variants`, instrumented `ffi_cached_runner_round_trips`.
  - **Python:** `test_join_accepts_watermark_secs_kwarg`.
  - **Node:** `parseMeshDbErrorKind decodes the <<meshdb-kind:...>> prefix`, `cachePolicyTimeBound rejects non-finite / negative ttlSeconds at the factory`, `execute rejects a hand-rolled cachePolicy with a negative ttlSeconds`, `execute rejects a hand-rolled cachePolicy with an unknown kind`, `break inside for-await releases the backing row buffer`, `exception inside for-await releases the backing row buffer`.
- **`cargo clippy --all-features --all-targets -D warnings` clean** across substrate + every binding crate. The MeshDB executor's hash-join probe-table type alias (`HashJoinTable`) lands to silence `clippy::type_complexity` on the shared helper.
- **`cargo doc --features meshdb --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — pass-1 fixes the broken intra-doc links in `cache.rs` (`DefaultHasher` / `PredicateWire`) and in `redex/config.rs` (the dataforts-gated `BlobAdapter` / `RedexFile::resolve_one` references that don't resolve under meshdb-only builds).
- **CI nextest groups + non-cascading test failures** so a flake in one integration test doesn't take down unrelated suites. The connect_direct retarget test that was masking the routed-handshake replay-guard bug now passes reliably.

---

## Breaking changes

### API — MeshDB surface is new

`MeshQuery` AST + `MeshQueryRunner` + `MeshQueryPlanner` + `FederatedMeshQueryExecutor` + `MeshDbTransport` + `LoopbackTransport` + `CachePolicy` + `ExecuteOptions` + `MeshError` + every operator family (`AggregateCount` / `AggregateNumeric` / `AggregateReduction` / `AggregateDistinct` / `HashJoin` / `Window` / `Filter` / `LineageEmit`) are all new in v0.16. Behind the `meshdb` Cargo feature; non-meshdb builds see the substrate path unchanged.

The bindings ship the same surface under the `meshdb` extra / feature flag. Python / Node / Go SDKs guard imports so the binding still loads without the feature compiled in (symbols simply don't appear).

### Wire format — `SUBPROTOCOL_MESHDB = 0x0F00`

A new subprotocol identifier is reserved on the wire for MeshDB federated queries. The dispatcher hookup that registers `SUBPROTOCOL_MESHDB` on `MeshNode` is parked until a consumer drives it; the envelope shapes are stable. No existing protocol changes.

### Capability index — `mutation_version`

`CapabilityIndex` gains an `AtomicU64 mutation_version` that bumps on every `index` / `remove` / `gc` mutation. Public surface: `CapabilityIndex::mutation_version() -> u64`. Used by the MeshDB result cache for pull-based invalidation. Source-compatible — no existing call site changes.

### `MeshError::AmbiguousDiscovery` is new

`MeshError` gains an `AmbiguousDiscovery { matches: Vec<u64>, requirement: String }` variant for the case where `ChainRef::Discovered` resolves to more than one origin. The variant is gated under the `#[non_exhaustive]` attribute that already applies to `MeshError`; matches that explicitly cover every variant get a compile error and need a `_ =>` arm or the new arm added.

### Behavioral fixes that may surface as test breakage

- **Routed-handshake replay guard now accepts same-static / fresh-ephemeral re-handshakes.** Tests that asserted `RoutedRotationOutcome::DropReplay` on bare `(static_a, static_a)` will see `AcceptRotation`; pass the new 4-arg signature with matching ephemerals to pin the replay-detection behaviour.
- **`Duration::MAX` sweep interval no longer panics.** Tests that asserted `tokio::time::interval(MAX)` would surface an Instant-overflow panic in the spawned task will see the loop park on `shutdown_notify` instead.
- **`MeshError` kind discriminator on the Python `MeshDbError` exception** — Python callers can read `e.kind` (set via PyO3 `setattr`); tests that asserted `MeshDbError` has no extra attributes will need updating.
- **Node FFI error messages carry the `<<meshdb-kind:KIND>>` prefix.** Tests that asserted on bare error messages need to either consume `parseMeshDbErrorKind(err).message` or update their substring matches.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.16 line.** Recompile / rebuild the binding cdylib (NAPI for Node, maturin for Python, `cargo build -p net-meshdb-ffi` for Go) with the `meshdb` Cargo feature on when you want the MeshDB surface; without it, the substrate is unchanged from v0.15.
2. **Go toolchain.** Bump to Go 1.26. CI now reads the version from `go/go.mod` — set the version there and `actions/setup-go@v5`'s `go-version-file:` picks it up automatically. Local toolchains should match.
3. **MeshDB opt-in.** Channels that want federated queries: build the substrate with `--features meshdb` and construct a `LocalMeshQueryExecutor::new(reader)` against a `ChainReader` that walks RedEX. Compose plans via the typed `MeshQuery::V1(QueryV1::*)` AST or the host-language SDK factories.
4. **Result-cache opt-in.** Wrap the local executor with `LocalMeshQueryExecutor::with_cache(reader, Arc::new(LruResultCache::default()), Arc::new(|| capability_index.mutation_version()))`. Same shape for `FederatedMeshQueryExecutor::with_cache`.
5. **Federated executor.** Construct `FederatedMeshQueryExecutor::new(transport)` against a `MeshDbTransport` impl. `LoopbackTransport` ships for in-process integration tests; a real `MeshNode`-backed transport that registers `SUBPROTOCOL_MESHDB = 0x0F00` on the dispatcher is the next slice once a consumer drives it.
6. **Cross-binding consumers.** Python imports `from net import MeshQuery, MeshQueryRunner, ExecuteOptions, CachePolicy`; Node `import { MeshQuery, MeshQueryRunner, cachePolicyPermanent, cachePolicyTimeBound } from '@ai2070/net'` plus `import '@ai2070/net/meshdb'` for the `for await` shim; Go imports `github.com/ai-2070/net/go` and uses `MeshDBQuery` / `MeshDBRunner` / `MeshDBQueryBuilder`. C consumers include `<net_meshdb.h>` and link `-lnet_meshdb`.
7. **Error handling.** Python: `except MeshDbError as e: e.kind`. Node: `import { parseMeshDbErrorKind } from '@ai2070/net/meshdb'; const { kind, message } = parseMeshDbErrorKind(err)`. Go: `var mde *MeshDBError; if errors.As(err, &mde) { mde.Kind }`. C: `net_meshdb_last_error_kind()` + `net_meshdb_last_error_message()` per-thread, with `net_meshdb_clear_last_error()` to reset.
8. **NAT-traversal consumers.** The routed-handshake replay-guard fix is transparent — legitimate re-handshakes from the same peer now succeed where they previously timed out. If your application explicitly tested the prior `DropReplay`-on-same-static behaviour, update to assert against `(static, ephemeral)` pairs.
9. **`Duration::MAX` sweep configs.** If you intentionally set `token_sweep_interval` or `capability_gc_interval` to `Duration::MAX` to disable a loop, the behaviour is now what the docs promised — the spawned task parks on shutdown notification without ticking. No code change required, but the pre-fix Instant-overflow panic noise disappears from logs.

---

Released 2026-05-13.

## License

See [LICENSE](../../LICENSE).
