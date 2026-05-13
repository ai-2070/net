# MeshDB — implementation plan

> Federated distributed query layer above the Warriors-shipped capability-query primitives. Adds **time-travel queries**, **lineage walks via `fork-of:` graph traversal**, **cross-chain joins**, and **streaming aggregate analytics** as composable operators that span nodes and chains. Companion to [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) (whose `filter`/`match`/`traverse`/`aggregate`/`nearest` primitives MeshDB composes against) and [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) (whose replication makes historical-range queries tractable after origin compaction). **Atomic Playboys release** per [`RELEASE_ROADMAP.md`](RELEASE_ROADMAP.md); not in The Warriors or Rebel Yell.

## Status

**Phases A, C, D, E, F shipped behind the `meshdb` Cargo feature; Phase B is partial — federated executor in code, wire-subprotocol dispatch hookup outstanding.** AST, planner, local + federated executors, wire protocol envelopes, lineage walks, hash + sort-merge joins (row-intrinsic + payload-keyed, all four kinds), Count / Sum / Avg / Min / Max / DistinctCountExact / PercentileExact aggregates, Filter via synthetic-tag PredicateWire eval, tumbling-on-seq windowing, and the single-node LRU result cache with pull-based capability-version invalidation are all in code. Phase B's `FederatedMeshQueryExecutor` ships with a `LoopbackTransport` for integration tests; the real wire-side hookup that registers `SUBPROTOCOL_MESHDB` on `MeshNode` is the load-bearing remaining work to move federation out of test-only loopback. Other open items are bounded follow-ups: HLL p=14 / T-Digest c=100 sketch implementations behind `DistinctCountHll` / `PercentileTDigest`, and language bindings beyond the Python / Node / Go / C shims already shipped.

**Shipped surface** (gated behind `#[cfg(feature = "meshdb")]` at `src/adapter/net/behavior/meshdb/`):

- **Phase A — AST + planner skeleton.** `MeshQuery::V1(QueryV1)` versioned outer enum with 10 operator variants (At / Between / Latest / Lineage{Back,Forward} / Join / Filter / Aggregate / Project / OrderBy). Postcard + JSON round-trippable. `MeshQueryPlanner` translates atomic operators to typed `ExecutionPlan`s; composite operators wrap their planned children in `NotYetImplemented` placeholders so the tree still type-checks. (`query.rs`, `planner.rs`, `error.rs`.)
- **Phase B — Time-travel end-to-end.** 🚧 Partial: substrate complete, real wire-side hookup outstanding. Replica-aware routing via `CausalClaim` parsing of the three `causal:` tag forms (Presence / Tip / Range); proximity-ordered targets with most-specific-claim wins; `HistoricalRangeUnavailable` with per-replica available-range hints. `MeshQueryExecutor` async trait + `LocalMeshQueryExecutor<R: ChainReader>` walks atomic plans against a pluggable `ChainReader`. Wire protocol envelopes (`SUBPROTOCOL_MESHDB = 0x0F00`, `MeshDbRequest::{Execute, Resume, Cancel}`, `MeshDbResponse::{Batch, End, Error}`, `ResultBatch`, `ContinuationToken`) — defined; not yet registered on `MeshNode`'s subprotocol dispatch. `FederatedMeshQueryExecutor<T: MeshDbTransport>` fans atomic operators out with NoRoute failover; `LoopbackTransport` drives 3-node integration tests in-process. Cooperative cancellation via `QueryHandle::cancel`. (`executor.rs`, `protocol.rs`, `federated.rs`.)
- **Phase C — Lineage walks via `fork-of:` graph.** `OperatorPlan::LineageEmit { origin, direction, entries }` carries a materialized walk result. The planner walks the local capability-index snapshot at plan time (`parent_of` for back; BFS `children_of` lex-sorted for forward), with explicit visited-set cycle detection (`LineageCycleDetected`) and depth bounds (`LineageMaxDepthExceeded`). The executor emits one `ResultRow` per entry — payload empty; callers compose with `At` / `Between` for full event content. The federated executor handles `LineageEmit` locally (no remote dispatch needed; the walk already happened). (`planner.rs`, `executor.rs`, `federated.rs`.)
- **Phase D-1 — Inner hash-join on row-intrinsic keys.** `OperatorPlan::HashJoin { left, right, key_mode, kind, watermark }` with `JoinKeyMode::{Origin, Seq, OriginSeq}` for the join-key extraction modes Phase D-1 supports. The planner derives the mode from the `JoinKey` and surfaces `PlannerError` for payload-keyed or mismatched-field joins (deferred to Phase E). Both local and federated executors implement build-on-left / probe-on-right; the federated path recurses through itself so atomic leaves still dispatch via the transport. Joined rows are sentinel `ResultRow`s (`origin=0`, `seq=0`) whose payload is a postcard-encoded `JoinedRowPayload { left, right }`. `JoinMemoryExceeded` surfaces at the 256-MiB build-side bound.
- **Phase D-2 — Outer hash-joins + sort-merge + payload-keyed joins.** All four `JoinKind`s ship (Inner / LeftOuter / RightOuter / FullOuter). `JoinKeyMode::Field(String)` extends the join key surface to JSON payload paths via `row::extract_string_projection`; `try_encode_join_key` returns `Option<Vec<u8>>` so rows whose key field can't be resolved are silently dropped from both sides. `JoinStrategy::{HashBroadcast, SortMerge}` lets the planner pick between in-memory hashing (default; trips `JoinMemoryExceeded` past the bound) and sort-merge (sort both sides + two-pointer walk; memory-bounded by the inputs). Watermark is informational under snapshot semantics; streaming activation needs Phase F+.
- **Phase E — Filter + the aggregate surface + tumbling windows.**
  - **E-1 Count.** `OperatorPlan::AggregateCount` over row-intrinsic group keys (`origin`, `seq`, `(origin, seq)`).
  - **E-2 Filter.** Reuses the Capability System's `PredicateWire` via `row::synthetic_row_view`: every `ResultRow` projects to a synthetic `(Vec<Tag>, BTreeMap)` with `dataforts.origin`, `dataforts.seq`, and flat JSON-object payload fields. Non-JSON payloads are opaque; predicates against missing fields simply don't match.
  - **E-3 Sum / Avg.** `OperatorPlan::AggregateNumeric` over `row::extract_numeric` (JSON path → `f64`). Rows whose field fails to resolve are skipped; `Avg(None)` covers the empty-group case.
  - **E-4 Min / Max / DistinctCountExact / PercentileExact.** `OperatorPlan::AggregateReduction` (Min / Max / nearest-rank percentile via `f64::total_cmp`) + `OperatorPlan::AggregateDistinct` (canonical-string projection into a per-group `BTreeSet`). The HLL p=14 / T-Digest c=100 sketch variants remain `PlannerError` until a consumer drives the algorithmic complexity; the exact variants are the recommended path.
  - **E-5 Window.** `QueryV1::Window { inner, spec: WindowSpec::TumblingSeq { size } }` buckets rows into fixed-size half-open intervals on `seq`; the executor emits one sentinel `ResultRow` per non-empty bucket with a postcard-encoded `WindowBoundary { start, end, rows }`. Sliding + session windows extend cleanly via additional `WindowSpec` variants when a consumer drives the shape.
- **Phase F — Single-node LRU result cache.** Ships per the locked Phase F decisions:
  - **Global cache version, no query-shape classification.** `CapabilityIndex` carries an `AtomicU64 mutation_version` that bumps on every `index` / `remove` / `gc` mutation. The MeshDB cache pull-invalidates: lookups encode the live version into the key (`CacheKey { plan_hash, capability_version }`); any divergence misses. Aggressive invalidation by design — softening it is not the answer to churn, the bypass flag is.
  - **Default `CachePolicy::TimeBound { ttl: 5s }`** — mirrors the locked-decision-#2 join watermark. `Permanent` is the explicit-opt-in policy for queries over closed substrate ranges (`At`, bounded `Between`).
  - **`ExecuteOptions::bypass_cache`** skips both lookup and writeback (Deck operator-view authoritative reads; Hermes skill-routing under churn; diagnostics).
  - **Hand-rolled LRU** — `HashMap` + intrusive doubly-linked list over a `Vec<Node>`. Defaults: `LRU_MAX_ENTRIES = 1024`, `LRU_MAX_BYTES = 256 MiB`; either bound trips eviction of the LRU end. SipHash via `DefaultHasher` over postcard-encoded plan bytes; no new external dependency.
  - **Top-level only.** Sub-plan executes inside the federated path bypass the cache; recursive caching at HashJoin sides / Aggregate inner is a follow-up if profiling justifies the bookkeeping.
- **Spec correction during B-1.** `ChainRef::OriginHash` + `ResultRow.origin` are `u64` (16-char hex), not `[u8; 32]` — the original plan was wrong about origin width; substrate uses `u64` throughout.

**Substrate prereqs** (unchanged — all in code):

- **Capability System primitives** — `filter` / `match_axis` / `traverse` / `aggregate` / `nearest` ship as the `CapabilityQuery` trait with a reference impl on `CapabilityIndex` (`src/adapter/net/behavior/query.rs`). `fork-of:` is emitted via `CapabilitySet::from_fork` (`capability.rs:1093`), reserved-prefix-enforced at `tag.rs:126`, walked by `traverse` with cycle detection. Minor signature drift from this plan: `traverse` takes an explicit `start_node`, `nearest` takes an `rtt_lookup` closure, the aggregator trait uses `observe / finalize` instead of `fold`.
- **`REDEX_DISTRIBUTED_PLAN.md`** — all phases shipped; replication makes time-travel queries tractable today.
- **`CORTEX_ADAPTER_PLAN.md`** — shipped and exceeds spec; `CortexAdapter<State>` + `RedexFold<State>` + `watch` / `snapshot_and_watch` / `changes_with_lag()` all in code.

**Remaining work** is bounded follow-ups, all consumer-driven: HLL p=14 / T-Digest c=100 sketch implementations behind `DistinctCountHll` / `PercentileTDigest` (the exact variants ship today); the wire-subprotocol dispatch hookup that registers `SUBPROTOCOL_MESHDB` on `MeshNode` (the federated executor exists; only the wire-side framing remains to move it out of test-only loopback); language bindings beyond Rust. Net-new surfaces (push-down fold-on-relay; streaming activation of the watermark on Window-joined sub-trees; sub-plan caching) come with their respective consumers.

## Frame

CortEX gives every node a *local* query layer — fold RedEX events into reactive in-memory state, query that state via NetDB. The Warriors capability-query primitives give the substrate a *cross-node* query layer for capability metadata — find which nodes have which chains, with what intent, in what proximity zones.

**MeshDB is the layer that unifies these two.** It lets a single user-facing query span:

- **Time** (a chain's history at a specific seq, or across a seq range)
- **Lineage** (the `fork-of:` graph back to a common ancestor; sibling chains; descendant cohorts)
- **Multiple chains** (joins across causally-related but distinct chains; cross-chain analytics)
- **Multiple nodes** (the same query routed to several nodes that each hold relevant data; results aggregated)

Conceptually it's "a federated database whose tables are causal chains and whose rows are events." Queries compile to query plans that route sub-queries to the nodes nearest the relevant chains, execute locally on each, stream results back, and aggregate. **The capability index is the query planner; the proximity graph is the cost model; the local CortEX/RedEX state is the storage engine.**

The hard part is not the primitives (which Warriors ships). The hard part is **composing them into a query language** that handles real workloads correctly under partition / replication-lag / late-arriving events / bounded memory / streaming results, and exposes that composition as something a developer can actually write queries against.

## Why this exists

Three reasons this needs to be a written plan and not just "we'll figure it out when the time comes":

1. **The composition surface is large.** Time-travel + lineage + cross-chain joins + aggregates each have their own correctness constraints, and the constraints interact. Designing them in isolation produces a query language full of footguns; designing them together produces a coherent surface. Worth doing the design work upfront, even if implementation parks until activation.
2. **MeshDB's correctness depends on Warriors invariants.** Time-travel needs replicated historical ranges (Distributed RedEX); lineage traversal needs `fork-of:` propagation (Capability System); aggregate streams need predicate composition (Capability System). If MeshDB is designed before those primitives are fully understood, it ends up reinventing them or fighting them. This plan deliberately follows the Warriors plans and references their invariants.
3. **The activation triggers are visible from current workloads.** Industrial telemetry pilots (oil & gas incident replay, semiconductor inspection-chain auditing) and AI-fine-tuning across forked experiments are concrete near-term scenarios. Knowing what MeshDB needs to support before they activate avoids a panicked design phase under customer pressure.

## What ships

Six interlocking pieces, in dependency order:

1. **`MeshQuery` AST + planner** — the composable query language and the query-planning layer that translates queries into capability-index lookups + per-node execution plans.
2. **Time-travel operator** — `at(origin_hash, seq)` and `between(origin_hash, start_seq, end_seq)` queries against historical chain ranges, with replica-aware routing.
3. **Lineage-walk operator** — `lineage_back(origin_hash, max_depth)` and `lineage_forward(origin_hash, max_depth)` for `fork-of:` graph traversal, with cycle detection and depth bounds.
4. **Cross-chain join operator** — `join(chain_a, chain_b, on=correlation_key)` with bounded memory, partial-results streaming, and explicit late-arrival semantics.
5. **Streaming aggregate operator** — `aggregate(filter, agg_fn)` for sum/count/avg/percentile/distinct-count/quantile-sketch across many chains, with push-down execution toward data nodes.
6. **Result-streaming protocol** — bounded-memory result iteration via continuation tokens; backpressure-aware; per-query cancellation.

What this doc does NOT ship (deferred even from MeshDB):

- **SQL-flavored query parser.** MeshDB is a programmatic API + AST. A SQL-like surface (or GraphQL-like, or Cypher-like) is a separate concern; library users can build one on top if needed. The protocol-as-product principle: keep the substrate primitive and let downstream products choose their query syntax.
- **Strong-consistency cross-chain transactions.** MeshDB queries operate on causally-consistent eventually-converged state. Strong-consistency transactions across chains are a different problem (and arguably the wrong one — RedEX is append-only, not transactional).
- **Materialized view maintenance.** A view that's incrementally maintained as chains advance is genuinely useful but architecturally distinct from query execution; defer to a follow-up doc when needed.
- **Query result caching across nodes.** A single-node query cache (LRU on result hash) is in-scope. Distributed query cache + cache coherence is its own problem; defer.
- **Authorization at query plan level.** ACL is enforced at the chain-read level via existing AuthGuard; queries inherit this. Per-query-result redaction or row-level security is a follow-up.

---

## Design

### 1. The `MeshQuery` AST

A composable algebra. Lives in `behavior::meshdb::query`. The full grammar is small but composes powerfully.

```rust
pub enum MeshQuery {
    /// Atomic: read a chain at a specific seq.
    At { origin: ChainRef, seq: SeqNum },

    /// Atomic: read a chain across a seq range.
    Between { origin: ChainRef, start: SeqNum, end: SeqNum },

    /// Atomic: read a chain's current tip.
    Latest { origin: ChainRef },

    /// Composite: walk fork-of: parents.
    LineageBack { origin: ChainRef, max_depth: u32 },

    /// Composite: walk fork-of: descendants.
    LineageForward { origin: ChainRef, max_depth: u32 },

    /// Composite: join two chain queries.
    Join {
        left: Box<MeshQuery>,
        right: Box<MeshQuery>,
        on: JoinKey,
        kind: JoinKind,         // Inner | LeftOuter | RightOuter | FullOuter
    },

    /// Composite: filter results by predicate.
    Filter {
        inner: Box<MeshQuery>,
        predicate: Predicate,    // reuses Capability System's predicate language
    },

    /// Composite: aggregate results.
    Aggregate {
        inner: Box<MeshQuery>,
        group_by: Vec<Expr>,
        agg_fn: AggregateFn,    // Sum | Count | Avg | Min | Max | Percentile | Distinct
    },

    /// Composite: transform/project rows.
    Project {
        inner: Box<MeshQuery>,
        columns: Vec<Expr>,
    },

    /// Composite: limit + ordering.
    OrderBy { inner: Box<MeshQuery>, by: Vec<OrderKey>, limit: Option<u64> },
}

pub enum ChainRef {
    /// By origin hash directly.
    OriginHash([u8; 32]),
    /// By a metadata-tag-driven query — e.g., "all chains with intent:ml-training".
    Discovered(Predicate),
}
```

The AST is closed under composition. A user writes:

```rust
let q = MeshQuery::Aggregate {
    inner: Box::new(MeshQuery::Filter {
        inner: Box::new(MeshQuery::Between {
            origin: ChainRef::OriginHash(chain_x_hash),
            start: 1_000_000.into(),
            end: 1_100_000.into(),
        }),
        predicate: pred!(event.metadata.severity >= "warning"),
    }),
    group_by: vec![Expr::Field("operator_id".into())],
    agg_fn: AggregateFn::Count,
};
```

That's "between seqs 1M and 1.1M of chain X, count events with severity ≥ warning, grouped by operator_id." A single composable expression that the planner turns into a federated execution plan.

### 2. Query planning

`MeshQueryPlanner::plan(query: &MeshQuery) -> ExecutionPlan` translates the AST into a tree of operator nodes, each annotated with:

- **Where to execute** (which node(s) hold the relevant chain)
- **What capability to require** (e.g., a node that can serve `causal:X[1M..1.1M]`)
- **Bandwidth + latency cost estimate** (used to pick between alternative plans)
- **Result schema** (what rows look like coming out)

Planning steps:

1. **Resolve `ChainRef::Discovered` predicates** to concrete origin hashes via the Capability System's `match` operator. Time-bounded.
2. **For each origin hash, look up holders** via the capability index. Prefer nodes with the tightest seq range covering what the query needs; prefer in-proximity replicas; respect any `metadata.attract-to-scope` hints from the query author.
3. **Push-down predicates and projections** as far toward the data nodes as possible — let each holder do its own filtering before streaming results, so the caller node receives bounded, relevant rows.
4. **Choose join strategy** for cross-chain joins (broadcast, hash-partitioned, sort-merge) based on cardinality estimates from the capability index's `aggregate` primitive (counts of matching events).
5. **Insert bandwidth controls** — if a sub-query would return > N rows, insert a streaming pagination operator with continuation tokens.
6. **Validate plan correctness** against the AST; pin in tests.

Output is an `ExecutionPlan` tree the executor walks at run time. The planner is a pure function; same query + same capability-index state produces the same plan, which makes it deterministic and testable.

### 3. Time-travel operator

`At(origin, seq)` and `Between(origin, start, end)` — the foundational read primitives.

**Replica-aware routing.** The capability index advertises which nodes hold which seq ranges (`causal:X[start..end]` per the Capability System tag shapes). The planner picks the holder whose range tightly covers the query AND whose proximity is lowest. Falls back to wider-range holders if no tight one exists. **Failure mode:** no node holds the requested range — query returns `MeshError::HistoricalRangeUnavailable`, with a hint about what range *is* available.

**Replay semantics.** `At(origin, seq)` returns the *event at that seq*, not the *folded state at that seq*. To get folded state, the caller pairs the time-travel query with a CortEX fold:

```rust
let events = mesh_db.execute(MeshQuery::Between {
    origin: chain_x_hash,
    start: 0.into(),
    end: 12345.into(),
}).await?;

let state = MyFold::default();
for ev in events { state.apply(&ev)?; }
// state is now the folded state at seq 12345
```

This is intentional — folds are application-specific; the substrate doesn't materialize state, it streams events. Convenience helpers in the SDK can wrap common patterns.

**Compaction interaction.** RedEX retention may have evicted older events on the *origin* node. If a replica still holds the range, the query succeeds via the replica. If no replica holds it, the query returns `HistoricalRangeUnavailable`. **`REDEX_DISTRIBUTED_PLAN.md`'s replication factor + `UnderCapacity::EvictOldest` policy directly determines what historical ranges are queryable.**

### 4. Lineage-walk operator

`LineageBack(origin, max_depth)` walks the `fork-of:` graph backward toward ancestors. Each chain advertises `fork-of:<parent_hex>` if it forked from another; the operator follows these links recursively, depth-bounded.

```rust
pub fn lineage_back(
    &self,
    origin: [u8; 32],
    max_depth: u32,
) -> impl Stream<Item = (ChainHash, Depth, ForkLink)> {
    // 1. Look up origin's fork-of: tag in capability index
    // 2. If present, emit (parent_hash, depth=1, ForkLink { parent_seq, child_seq })
    // 3. Recurse on parent, depth+=1
    // 4. Stop at max_depth or when no further fork-of: tag
    // 5. Cycle detection: track visited; if we see one, log warning + stop
}
```

`LineageForward` is the inverse — walks descendants. Implementation is harder because there's no forward pointer in the chain itself; the planner queries the capability index for "all chains advertising `fork-of:<this_origin>`" via the `match_axis` Warriors primitive.

**Cycle detection.** In principle `fork-of:` should form a DAG (you can't fork into your own ancestor). In practice, broken applications might emit cycles. Detect via visited-set tracking; emit a warning event in the result stream; stop traversal.

**Depth bounds.** Default `max_depth = 32`. Lineage chains rarely go deeper than a few hops in real workloads; deep chains are usually a misuse. Caller can override with explicit higher bound.

**Use cases:**

- "Show me the experiment lineage of model-X — which fork branched from which fork going back to the root experiment." → `LineageBack(model_x_hash, 32)`
- "Find all sibling experiments that branched from the same parent at roughly the same time." → `LineageBack` then filter siblings by fork-time proximity
- "What happened in the parent chain right before this fork was created?" → `LineageBack` to get parent + seq, then `Between` on parent over the surrounding seq window

### 5. Cross-chain join operator

The most complex operator. Joins events across two chains on a correlation key.

```rust
pub enum JoinKind { Inner, LeftOuter, RightOuter, FullOuter }

pub struct JoinKey {
    pub left_field: Expr,   // e.g., event.metadata.request_id
    pub right_field: Expr,  // e.g., event.metadata.request_id
}
```

**Three execution strategies, planner picks based on cardinality estimates:**

| Strategy | Use when | How it works |
|---|---|---|
| **Broadcast join** | one side is small (~ < 100K rows estimated) | Stream the small side fully into memory at the executor; for each row of the large side, look up matches in the in-memory hash. |
| **Hash-partitioned join** | both sides are large; correlation key has high cardinality | Partition both sides by hash(key) % N; ship same-hash partitions to the same node; do in-memory join per partition. |
| **Sort-merge join** | both sides are large AND already sorted by correlation key (e.g., both are seq-ordered) | Stream both sides in sorted order; merge join in O(n+m). |

**Late-arriving events.** Joins on streaming data have a fundamental issue: a left-side event might arrive before its right-side match exists yet. Two semantics:

- **Bounded watermark.** Caller specifies "wait up to T for late matches"; after watermark passes, emit unmatched left-side events as outer-join nulls. Default T = 5 seconds for streaming queries; default T = ∞ for batch queries (over a closed seq range).
- **Async join via continuation token.** Initial query returns a partial result + a continuation token; calling back later with the token returns matches that have arrived since.

**Bounded memory.** All three strategies bound their working-set memory. Broadcast joins fail planning if the small side is estimated > a configurable threshold (default 1 GB). Hash-partitioned joins bound per-partition memory and may spill to disk. Sort-merge joins are constant memory (just two cursors).

### 6. Streaming aggregate operator

`Aggregate(inner, group_by, agg_fn)` — federated aggregation over rows.

**Push-down execution.** The planner decomposes aggregates into per-node *partial aggregates* + a single global *combine* step:

- `count` → each node returns local count; combiner sums
- `sum` → each node returns local sum; combiner sums
- `avg` → each node returns (sum, count); combiner computes ratio
- `min` / `max` → each node returns local min/max; combiner takes the extremum
- `percentile` / `quantile` → each node returns a sketch (T-Digest or KLL); combiner merges sketches and queries
- `distinct count` → each node returns a HyperLogLog sketch; combiner merges

This pattern is well-understood (it's essentially what every distributed analytics system does — Druid, ClickHouse, BigQuery, Spark). MeshDB inherits the patterns; the novelty is just doing it over causal chains rather than time-series tables.

**Group-by handling.** Each node returns partial aggregates per group; combiner unions groups and re-aggregates per group key. Bounded memory via streaming hash-aggregation; if cardinality exceeds threshold, spill to disk or return a partial result with a continuation token.

**Approximate vs exact.** Some aggregates (distinct count, percentiles) can use sketches that trade exactness for bounded memory; others (sum, count) are exact. The query language exposes both:

```rust
agg_fn: AggregateFn::DistinctCount { sketch: HllSketch::default() }   // approx
agg_fn: AggregateFn::DistinctCountExact                                // exact, bounded by memory
```

### 7. Result-streaming protocol

All MeshDB queries return `Stream<Item = ResultRow>`. Bounded-memory iteration via continuation tokens.

```rust
pub struct QueryHandle {
    pub plan: ExecutionPlan,
    pub continuation: Option<ContinuationToken>,
}

pub trait MeshQueryExecutor {
    async fn execute(&self, query: MeshQuery) -> Result<QueryHandle>;
    async fn next_batch(&self, handle: &QueryHandle, max_rows: usize) -> Result<(Vec<ResultRow>, Option<ContinuationToken>)>;
    async fn cancel(&self, handle: &QueryHandle) -> Result<()>;
}
```

The continuation token encodes the planner's progress through the execution plan. Subsequent `next_batch` calls resume where the previous batch left off. Cancel terminates the query and frees per-node executor state.

**Backpressure.** If the caller doesn't pull batches fast enough, per-node executors slow down their result emission. Reuses the existing reliable-stream flow control.

**Per-query cost limits.** Configurable per-channel (`ChannelConfig::query_max_rows`, `query_max_duration`, `query_max_bytes_scanned`). Queries that exceed their budget return `MeshError::QueryBudgetExceeded` with a partial result + continuation token if any rows were produced.

### 8. Caching (in-process LRU)

Per-node query result cache, keyed by `(query_hash, capability_index_version)`. Invalidates when the capability index version advances (i.e., new tags propagated, withdrawals, etc.).

- **Cache scope:** per-node only. Cross-node distributed cache is deferred.
- **Cache size:** configurable; default 1 GB / 10K entries / LRU eviction.
- **Cache key:** stable hash of the query AST plus the version-counter of the capability index at plan time. This is conservative — capability-index changes that don't actually affect this query also invalidate the cache — but it's correct and simple. Smarter invalidation is a follow-up.

### 9. Error semantics

```rust
pub enum MeshError {
    HistoricalRangeUnavailable { origin: [u8; 32], requested: Range<SeqNum>, available: Vec<Range<SeqNum>> },
    LineageMaxDepthExceeded { origin: [u8; 32], depth: u32 },
    LineageCycleDetected { origin: [u8; 32], cycle: Vec<[u8; 32]> },
    JoinMemoryExceeded { strategy: JoinStrategy, threshold_bytes: u64 },
    QueryBudgetExceeded { metric: BudgetMetric, used: u64, limit: u64 },
    PartialResult { rows: Vec<ResultRow>, continuation: ContinuationToken, reason: String },
    PlannerError { detail: String },
    ExecutorError { node: NodeId, detail: String },
    NoCapableHolder { origin: [u8; 32], requirement: Predicate },
    QueryCancelled,
}
```

`PartialResult` is the most important — many failure modes return *partial* results plus enough state to resume or recover, rather than aborting hard.

---

## Phasing

Six phases, in dependency order. Each is gated by activation of the workload that requires it; do not ship speculatively past phase 1.

### Phase A — `MeshQuery` AST + planner skeleton (3 weeks)

- Define the `MeshQuery` enum + supporting types.
- `MeshQueryPlanner::plan` for atomic operators (`At`, `Between`, `Latest`).
- Capability-index integration for resolving `ChainRef::Discovered`.
- Cost model stub (proximity + capability availability; no cardinality estimates yet).
- Unit tests: AST round-trip, planner produces valid plans, planner is deterministic.

### Phase B — Time-travel operator end-to-end (2-3 weeks)

- `MeshQueryExecutor` for `At`, `Between`, `Latest` operators.
- Replica-aware routing (consults Distributed RedEX's capability advertisements).
- `HistoricalRangeUnavailable` error path with hints about available ranges.
- Result streaming protocol (initial version; bounded batches via continuation tokens).
- Integration tests: 3-node mesh; one node holds historical range; query routes correctly.

### Phase C — Lineage-walk operators (2 weeks)

- `LineageBack` and `LineageForward` operators.
- Cycle detection via visited-set tracking.
- Depth bounds enforced.
- Integration with `fork-of:` capability tags from the Warriors-shipped Capability System.
- Tests: forked chain graphs of various depths; cycle injection; missing parent chain (lineage truncates gracefully).

### Phase D — Cross-chain join operator (4 weeks)

- All three join strategies (broadcast, hash-partitioned, sort-merge).
- Cardinality-estimate-driven strategy selection in the planner.
- Bounded memory enforcement; spill-to-disk for hash-partitioned joins.
- Late-arrival semantics with watermarks.
- This is the heaviest phase; ~30% of MeshDB's effort. Plan accordingly.

### Phase E — Streaming aggregate operator (2 weeks)

- `Aggregate` operator with push-down execution.
- Built-in agg functions: count, sum, avg, min, max, distinct (exact + HLL sketch), percentile (T-Digest sketch).
- Group-by handling with bounded memory.
- Tests: per-node partial aggregates merge correctly; sketches preserve approximate correctness within their guarantees.

### Phase F — Caching + cost limits + bindings (3 weeks)

- In-process LRU result cache with capability-index-version invalidation.
- Per-channel cost limits enforced at executor level.
- Per-query cancellation.
- Cross-binding API (Node, Python, Go, C). MeshDB is library-first; bindings expose the AST + executor.
- Documentation: user-facing guide, query examples, performance characteristics.

**Total: 16-19 focused weeks.** Bindings parallelize; Phases D and E can overlap if separate engineers work on them. Realistic delivery: **4-6 months** with 2 engineers; **6-9 months** with 1 engineer.

---

## Test strategy

### Unit

- **AST round-trip.** Every `MeshQuery` variant serializes/deserializes; nested compositions preserve semantics.
- **Planner determinism.** Same query + same capability-index state produces the same plan; no nondeterministic ordering.
- **Cycle detection.** Lineage walks with injected cycles terminate cleanly with `LineageCycleDetected`.
- **Sketch correctness.** HLL distinct-count and T-Digest percentiles within published error bounds (HLL: ±2% with 0.04-bit accuracy; T-Digest: ±0.5% on quantiles).
- **Cost-budget enforcement.** Queries that exceed configured budgets return `QueryBudgetExceeded` at the right boundary.

### Integration

- **3-node mesh, replicated chain.** Time-travel query routes to a replica when the origin's range is no longer available locally.
- **Forked-chain graph.** Lineage walks correctly traverse 5-deep fork graphs; partial paths handled gracefully when a parent chain is unreachable.
- **Cross-chain join.** Two chains with synthetic correlation keys; broadcast join under low cardinality; hash-partitioned join under high cardinality; sort-merge join when both are seq-ordered.
- **Aggregate over many chains.** 100 synthetic chains with 100K events each; per-node partial aggregates merge correctly; HLL sketch error within bounds.
- **Streaming + backpressure.** Slow consumer pulls batches; per-node executors throttle correctly; no memory blowup.
- **Cancellation mid-query.** Cancel during a long-running join; per-node executor state cleaned up; resources freed.
- **Late-arrival joins.** Inject events late on one side of a streaming join; outer-join nulls emitted after watermark; results stable.

### Property

- **Composition associativity.** `aggregate(filter(x, p), agg) == aggregate(filter(x, p2), agg)` when `p` and `p2` are equivalent predicates.
- **Push-down equivalence.** A query with predicates pushed down produces the same result set as the same query without push-down (modulo ordering).
- **Streaming completeness.** A query that completes via continuation tokens returns the same row set as a query that returns all rows in one batch.

### Performance

- **Time-travel latency.** Query for a chain at seq N completes in `proximity_rtt + chain_read_time` per benchmark on test mesh. Per-batch latency p99 < 50 ms for warm queries on 100K-row results.
- **Aggregate throughput.** Aggregate over 100 chains × 100K events with HLL distinct-count completes in < 5 seconds on test mesh.
- **Join scaling.** Hash-partitioned join over 1M × 1M rows completes in < 30 seconds on a 16-node mesh, partitioned across 8 nodes.
- **Cache hit rate.** Repeated identical queries hit cache; first-query latency vs. cached-query latency ≥ 100x speedup.

### DST (deterministic simulation)

- **Partition during query.** Inject network partition mid-execution; query either completes (if remaining nodes can finish) or returns `PartialResult` with explicit unreachable-node list.
- **Replica-leader-flap during time-travel.** Time-travel query against a chain whose replica leader fails over mid-query; query retries via different replica or returns `HistoricalRangeUnavailable` cleanly.
- **Continuation token resilience.** Resume a query via continuation token after the originating planner restarts; either resumes correctly or returns a clear error indicating the token expired.

---

## Locked decisions

All seven open design questions ratified. Each item describes the decision, the contract the implementation must hold, and the rationale.

### 1. AST stability across versions

**Decision:** `MeshQuery` is explicitly versioned at the enum top level:

```rust
pub enum MeshQuery {
    V1(QueryV1),
    // V2(QueryV2), V3(QueryV3), ... as the AST evolves
}
```

Contract:

- Unknown versions reject cleanly at decode time (`MeshError::PlannerError { detail: "unsupported query version" }`).
- Never silently drop fields. Forward-incompatible inputs return a typed error; they do not partially-decode.
- Version tag appears in both JSON and postcard encodings.
- Version increments **only** when adding or changing variant semantics. Adding a new operator variant inside an existing `Vn` is a non-bump if optional and unknown-to-old-planner-rejected.
- Bindings enforce exhaustive matching on the version tag (no fall-through wildcards).

Rationale: Maintains planner guarantees and prevents stringly-typed entropy. A query serialized today must either decode to the exact same plan years from now OR fail with a typed error — never silently degrade.

### 2. Default join watermark

**Decision:** Default `watermark = Duration::from_secs(5)` for streaming joins; overrideable per query.

```rust
Join {
    left: Box<MeshQuery>,
    right: Box<MeshQuery>,
    on: JoinKey,
    kind: JoinKind,
    watermark: Duration,    // default 5s; ∞ for batch over closed ranges
}
```

Contract:

- Default ships at 5s.
- Per-query override supported (any positive Duration; `Duration::MAX` ≡ ∞ for batch).
- Watermark surfaces in result metadata so consumers know what semantics they got.
- Real workloads (Hermes + Deck) generate telemetry post-ship; defaults retune ONLY after that data lands.

Rationale: Provides correctness without stalling implementation; matches the plan's calibration note. 5s is a reasonable starting point that real telemetry can refine.

### 3. Sketch interoperability

**Decision:** Single canonical encoding per sketch type, baked into the wire surface:

| Sketch | Parameter | Footprint | Error bound |
|---|---|---|---|
| HyperLogLog | `p = 14` | 16 KB | ±0.81 % distinct-count |
| T-Digest | `compression = 100` | compact | ±0.5 % on quantiles |

Contract:

- All nodes treat these as the only valid sketch shapes. Cross-version merges would silently corrupt — reject with a typed error if a peer advertises a non-canonical sketch.
- postcard encodes them as typed enum variants (`SketchPayload::HllP14(...)`, `SketchPayload::TDigestC100(...)`).
- JSON uses the same tagged-struct form (`{"kind": "hll_p14", "data": "..."}`).
- Cross-node merges are guaranteed identical semantics: a node receiving sketches from N other nodes computes the same merged result regardless of which subset it sees.

Rationale: Sketch parameter drift is the canonical "v1 and v2 see different aggregate results" footgun in distributed analytics. Lock the parameters now, version-bump if changed.

### 4. Distributed cache vs in-process only

**Decision:** Per-node in-process LRU only. No distributed cache in this phase.

Contract:

- No cross-node coherence protocol.
- No cache-state gossip.
- No "cache miss → ask peer's cache" routing.
- Cache invalidation keys on `(query_hash, capability_index_version)` and stays bound to the local process.
- Phase E or later MAY revisit distributed caching as its own substrate-level feature with a dedicated plan.

Rationale: Avoids solving a cache-coherence problem before it's justified. Per-node LRU is sufficient for the workloads in scope; cross-node cache coherence is its own multi-month research problem and would block Phase A on something the workloads don't need.

### 5. Query language surface

**Decision:** Programmatic-only. No SQL-, Cypher-, or GraphQL-flavored surface in core.

Bindings expose `MeshQuery` in their idiomatic shape:

| Binding | Surface |
|---|---|
| Rust | `MeshQuery` enum literal |
| Python | dataclass / dict (same shape as `OverflowConfig` ships) |
| Node/TypeScript | discriminated union (typed object form) |
| Go | struct with json tags |
| C | JSON blob via FFI |
| Wire (inter-node) | postcard |

Contract:

- The typed AST per binding is the canonical user-facing surface.
- External language surfaces (e.g. a community-built SQL-to-MeshQuery compiler) MAY be community libraries; they are NOT in the substrate.
- Cross-binding parity is verified by serialization round-trip — every binding's AST must round-trip through both postcard and JSON without semantic drift.

Rationale: Matches the protocol-as-product philosophy from the parent roadmap. A SQL-like surface is a downstream library concern; baking one into core would couple the substrate to a language design and create a second source of truth for query semantics. Preserves AST stability (locked decision #1) by keeping exactly one canonical form.

### 6. Per-query authentication

**Decision:** Queries inherit the session's auth context. No per-query auth override.

Contract:

- All chain reads gated via the existing chain-level `subscribe_caps` + `AuthGuard` machinery.
- The planner never embeds auth tokens or capability claims into the AST.
- Execution always uses the current `AuthGuard` from the requesting session.
- A query that touches chains the session doesn't have `subscribe_caps` for returns the same typed denial the underlying read would return — no new error path.

Rationale: Prevents auth complexity explosions in the AST + planner. Aligns with the existing RedEX subscriber model where ACL is enforced at chain-read time, not at query-construction time. Per-query-result redaction / row-level security stays as a follow-up doc.

### 7. Streaming-aggregate windowing

**Decision:** Add a `Window` operator in Phase E; not earlier.

```rust
Window {
    inner: Box<MeshQuery>,
    kind: WindowKind,      // Tumbling | Sliding | Session
    duration: Duration,
}
```

Contract:

- Composes with `Aggregate` cleanly — the canonical pattern is `Aggregate(Window(Filter(Between(...))), …)`.
- Required for telemetry + Deck metrics workloads (the activation triggers for streaming analytics).
- Ships AFTER basic folds, joins, and aggregation are stable in Phases A–D. Building windowing on an unstable aggregate substrate inverts the dependency.

Rationale: Time-windowed aggregates ("count events per minute") are an extremely common workload — but they're a natural extension of the aggregate operator, not a foundational primitive. Phase E with `Window` slotted in keeps the dependency order honest and lets Phases A–D land their tests against a smaller surface.

---

## Risks

- **Query optimizer rabbit hole.** Distributed query optimization is a research field. MeshDB needs *enough* optimization to be useful, not perfect optimization. **Mitigation:** ship with a simple cost model (proximity + cardinality estimates from capability index); accept that some queries will be sub-optimally planned; let real workloads drive optimization investment.
- **Cross-node failure modes are exponential.** A query touching 10 nodes can fail in 2^10 partial ways. **Mitigation:** rigorous use of `PartialResult` + continuation tokens; never abort hard when partial results are useful; document failure semantics explicitly.
- **Memory blowup under poorly-bounded queries.** A user-written aggregate without proper grouping or filtering can OOM the executor. **Mitigation:** per-channel cost limits; per-query memory budget; fail fast with `QueryBudgetExceeded`.
- **AST API churn.** Every new operator or feature changes the AST. **Mitigation:** version the AST; deprecate variants over multiple minor versions; never break serialized queries silently.
- **Sketch parameter drift.** If sketch params change between versions, cross-version merges produce wrong results. **Mitigation:** lock sketch parameters in v1; bake into wire format; version-bump if changed.
- **Late-arrival semantics surprise.** Streaming joins return different results depending on watermarks; users may expect deterministic results. **Mitigation:** document explicitly; provide deterministic-mode (`watermark=∞` for batch queries over closed ranges); make watermark behavior visible in result metadata.

---

## Effort

**16-19 focused weeks parallelized across 2 engineers; 24-32 weeks single-engineer.**

- ~5000 LoC core (AST + planner + 6 operators + cache + executor + bindings)
- ~5000 LoC tests (unit + integration + property + performance + DST)
- ~2 weeks documentation (user guide, query examples, performance characteristics, operator reference)

This is a research-grade phase. Treat it as such — design carefully, prototype first, iterate against real workloads. Premature shipping produces a query language with footguns that's harder to fix than to design correctly the first time.

---

## Activation gate

A workload that genuinely needs distributed queries beyond what local CortEX + Warriors-shipped primitives satisfy. Realistic triggers:

- **Incident-investigation tooling** for industrial telemetry (oil & gas pipeline incident replay, semiconductor fab quality investigation, AV fleet post-incident analysis). These workloads need cross-chain joins to correlate events across multiple sensor / inspection / vehicle data streams, plus time-travel to replay incidents from saved historical state.
- **Replay debugging on retained chain history.** "What was the state of the inference daemon's input chain at the moment it produced this corrupt output?" — time-travel + folded state replay.
- **Aggregate analytics over a fleet.** — federated aggregate with percentile sketches.
- **Lineage-aware audit + compliance.** "Show me the lineage of this AI model from training data through fork experiments to production deployment." — `LineageBack` + per-step audit metadata.
- **Cross-experiment AI fine-tuning.** "Compare experiment A's loss curves to experiment B's where they share a common ancestor experiment." — `LineageBack` to find common ancestor + cross-chain join on training-step keys.

Any of these activates Phase A; phases B-F follow as needed by the specific workload.

---

## Cross-cutting: relationship to other plans

**Builds on:**

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — MeshDB's planner consumes the federated query primitives (`filter`, `match_axis`, `traverse`, `aggregate`, `nearest`) + the predicate language. Without those, MeshDB has no foundation; with them, MeshDB is just composition.
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — replication makes time-travel queries tractable. If a chain's origin has compacted away historical events, replicas hold them. Without replication, time-travel queries return `HistoricalRangeUnavailable` for any range that's been compacted on origin. With it, queries route to replicas covering the requested range.
- [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) — local fold + query layer. MeshDB results stream through CortEX folds at the caller's side (or at intermediate nodes for push-down execution).

**Consumes (potentially):**

- The `metadata.intent` and `metadata.colocate-with` fields for query routing hints. A query that touches `intent:ml-training` chains naturally routes toward GPU-rich nodes; a query that touches colocated chains naturally routes to nodes holding the colocation target.

**Replaced or extended by future Atomic Playboys candidates:**

- **Mikoshi v2** (delta-based migration, continuous rebalancing) — orthogonal; doesn't affect MeshDB directly but shares the federated-query infrastructure for monitoring placement decisions.
- **Federated mesh-wide scheduler** — could compose against MeshDB's aggregate operator for "average load across the fleet" decisions, but would have its own separate plan.
- **Materialized view maintenance** — the natural follow-up to MeshDB. Once federated queries work, incrementally-maintained views (refreshing as chains advance) become tractable as a layer above MeshDB's executor.

---

## See also

- [`REDEX_PLAN.md`](REDEX_PLAN.md) — single-node v1 substrate
- [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) — single-node v2 (orthogonal)
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — Phase 2 of The Warriors; required for time-travel correctness
- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — Warriors phase; provides the primitives MeshDB composes
- [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) — local fold layer that MeshDB streams through
- [`NETDB_PLAN.md`](NETDB_PLAN.md) — local query façade above CortEX; MeshDB is the federated counterpart
- [`../misc/DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md) — original deferral context: Phase 6 was MeshDB; this doc is the implementation detail when Atomic Playboys activates
- `RELEASE_ROADMAP.md` — Atomic Playboys release context
