# Performance Analysis: MeshDB (Federated Query Layer)

Supplemental to the unified report. Focuses on the meshdb subsystem — federated query planning, execution, caching, and the wire transport that fans queries out across the mesh. Items continue from #191.

**Important context up front:** MeshDB is documented as **disabled by default** behind the `meshdb` Cargo feature. The module doc says "activation requires a concrete consumer workload (Hermes telemetry + Deck metrics are the named candidates)... until a consumer drives semantics, Phase A's AST + planner skeleton is the only surface in code." So this subsystem is currently dormant in production.

That cuts two ways: (1) findings here matter less than for live subsystems, but (2) the code hasn't been pressure-tested against real workloads, so it's more likely to harbor unaudited overhead. Best to fix the high-impact items BEFORE a consumer activates the feature — easier to get right early than to refactor a hot consumer later.

---

## ✅ Fixed

| # | Item | Notes |
|---|------|-------|
| 195 | Federated hash-join left/right sub-fetches sequential → `tokio::try_join!` | Pre-fix the `FederatedMeshQueryExecutor` hash-join awaited `execute_uncached_with_handle(left)` and then `execute_uncached_with_handle(right)` back-to-back, serializing two independent network round-trips. For a remote-on-remote join with 50 ms RTT each: 100 ms wall vs 50 ms in parallel. Post-fix wraps both into one `tokio::try_join!` — both futures poll on the current task and the join resolves when both complete. `try_join!` short-circuits on the first error so the cancel-recheck between the legacy sequential awaits collapses naturally; the post-join `is_cancelled()` check still aborts before the local hash-join runs. All 189 meshdb feature-flagged tests continue to pass. |
| 198 | `MeshDbWireDispatcher` caller-side `Arc<RwLock<HashMap<u64, InflightCaller>>>` → `Arc<DashMap<u64, InflightCaller>>` | Pre-fix every `send` took a write lock to register the `call_id → tx` entry, every response route took a read lock to look it up, every send-error path took another write lock to remove, and the `ResponseStreamGuard::drop` took yet another write lock to clean up. Concurrent calls to different `call_id`s serialized on the whole-map lock. Post-fix is `DashMap` — sharded by `call_id`, so distinct calls hit distinct shards. Insert / remove / `get` all go through DashMap's per-shard locks. The server-side `Arc<RwLock<HashMap<(u64, u64), ServerCallHandle>>>` is a separate map keyed on `(peer, call_id)` and is left alone (doc #207 flagged it but in low-impact; not bundled with this fix). The 8 existing meshdb transport tests pass through the new shape unchanged. |
| 206 | `drain_rows` `Vec::new()` + grow → `Vec::with_capacity(128)` | Pre-fix the federated aggregator drained the row stream into a `Vec::new()` and grew it via `push → grow-by-doubling` (capacity 0 → 4 → 8 → 16 → ...). The natural upper bound (`AGGREGATE_MAX_BYTES / 64 ≈ 4M rows`) is too aggressive to preallocate — would burn ~32 MiB just on pointer slots. `DRAIN_INITIAL_CAPACITY = 128` skips the first several reallocations for the typical 100-row federated response at a ~4 KiB upfront cost; larger responses still grow on demand. |
| 209 | `CachedResult::approx_bytes` per-call row walk → cached at construction | Pre-fix the LRU's `bytes_used` bookkeeping called `approx_bytes()` per insert and once per candidate during `evict_until_within_bounds`, each call walking every row in the entry to recompute `payload.len() + row_overhead`. For a 10K-row entry that's a 10K-element walk per LRU op. Post-fix `CachedResult::new(rows, inserted_at, policy)` computes the value once at construction and stashes it in a private `approx_bytes: u64`; the method is now a single field load. Safe because `rows` is never mutated post-construction (the LRU only inserts / removes whole entries). All 5 construction sites (3 in tests, 2 production: `executor.rs::execute_with`, `federated.rs::execute_uncached_with_handle`) route through `::new`. |
| 211 | `chain_hex(origin_hash)` `format!("{:016x}")` → `HEX_NIBBLES` 16-shift unroll | Same lookup-table pattern as the dataforts #171 hex-decode fix, applied to the encode side of the planner's `causal:<hex>` tag stem. Pre-fix routed through `core::fmt::Formatter`; post-fix is a direct nibble-table walk into a 16-byte stack buffer surfaced as `String` via `from_utf8` (infallible by construction — `HEX_NIBBLES` only emits ASCII hex digits). `collect_coverage` already hoists the call to once per planning call (line 1104), so the per-call savings are sub-microsecond — the change is mostly pattern consistency with #171 and removes a `core::fmt` allocation from the planner hot path. Pinned by `chain_hex_matches_format_macro_byte_for_byte` — single-byte divergence would silently break `causal:<hex>` tag-stem matching across the federated query layer. |

---

## 🔴 High-impact

### 192. `synthetic_row_view` JSON-parses + tag-explodes EVERY row in EVERY filter

**Location:** `meshdb/row.rs:53-67`, called from `executor.rs:566` and `federated.rs:485`:

```rust
pub fn synthetic_row_view(row: &ResultRow) -> (Vec<Tag>, BTreeMap<String, String>) {
    let mut tags: Vec<Tag> = Vec::new();
    let mut metadata: BTreeMap<String, String> = BTreeMap::new();

    let origin_str = format!("{:016x}", row.origin);             // alloc 1
    let seq_str = row.seq.0.to_string();                          // alloc 2
    push_field(&mut tags, &mut metadata, "origin", &origin_str);  // 2 to_string + tag construction
    push_field(&mut tags, &mut metadata, "seq", &seq_str);        // 2 to_string + tag construction

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&row.payload) {  // full JSON parse
        flatten_json("", &value, &mut tags, &mut metadata);       // recursive walk, more allocs
    }

    (tags, metadata)
}
```

Called per row in `execute_filter` (both local and federated). For a filter over 10K rows:

- 10K JSON parses (heap-allocates serde_json::Value tree per row)
- 10K Vec<Tag> allocations
- 10K BTreeMap allocations
- Per matching field: 4 String clones (`key.to_string()` + `value.to_string()` × 2 — once in push_field for tag, once for metadata)
- For JSON arrays: recursive walk with `format!("{prefix}.{k}")` per object key

**This is the single most expensive per-row operation in the entire MeshDB executor.** It rebuilds a synthetic fact database per row, on every filter call, even when the predicate only references one field.

**Fix paths:**

1. **Field-aware predicate compilation:** at predicate-rebuild time (already happening once per query), compute the set of field names the predicate references. Per row, extract only those fields from the JSON — skip the full flatten.

2. **Direct JSON predicate evaluation:** evaluate the predicate directly against the `serde_json::Value` without translating to `(Vec<Tag>, BTreeMap)`. Skip half the allocations.

3. **Cache the parsed view on the row:** if the same row is filtered multiple times (cross-join, repeated filters), parse once.

4. **Replace `serde_json::Value` with `simd_json::OwnedValue` or `sonic_rs`:** 3-5× faster JSON parse with the same surface.

For consumer workloads expected to be filter-heavy (Hermes telemetry, Deck metrics — both named candidates), this dominates query latency. Pre-emptive fix before activation.

### 193. `LruResultCache::get` CLONES the entire CachedResult per cache hit

**Location:** `meshdb/cache.rs:267`:
```rust
fn get(&self, key: &CacheKey) -> Option<CachedResult> {
    let mut g = self.inner.lock();
    let idx = *g.by_key.get(key)?;
    if g.nodes[idx].value.is_expired() { ... }
    g.move_to_head(idx);
    Some(g.nodes[idx].value.clone())   // <-- full deep clone of Vec<ResultRow>
}
```

`CachedResult` contains `Vec<ResultRow>`; each `ResultRow` contains `Vec<u8>` (payload). The clone is recursive — every payload bytes is copied per get.

**This negates the entire point of the cache.** A cached 10K-row result with 1KB payloads = 10MB. Every cache hit allocates 10MB and copies it. The cache exists to avoid re-running the query; instead the cache makes you pay clone cost (often comparable to query cost) on every hit.

**Fix:** `Arc<CachedResult>` everywhere. Per get: one atomic refcount bump.

```rust
struct LruNode {
    key: CacheKey,
    value: Arc<CachedResult>,   // <-- wrap
    // ...
}

fn get(&self, key: &CacheKey) -> Option<Arc<CachedResult>> {
    // ...
    Some(Arc::clone(&g.nodes[idx].value))
}
```

Same fix as #149, #175, #193 (recurring pattern). The diff is mechanical.

For consumer workloads that depend on cache hits for tail latency (any dashboard / metrics query), this is a 100-1000× win on hit latency.

### 194. `CacheKey::for_plan` allocates a full postcard Vec just to hash it

**Location:** `meshdb/cache.rs:110-119`:
```rust
pub fn for_plan(plan: &ExecutionPlan, capability_version: u64) -> Option<Self> {
    use std::collections::hash_map::DefaultHasher;
    let bytes = postcard::to_allocvec(plan).ok()?;   // <-- full alloc
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(Self { plan_hash: hasher.finish(), capability_version })
}
```

Called per query (cache lookup AND insert). For a 1000-operator plan, postcard-serializes to multi-KB Vec, then hashes the Vec, then drops it.

**Fix:** Implement `Hash` directly on `ExecutionPlan`. Walk the structure, feed bytes into the hasher in-place. Zero allocation:
```rust
impl Hash for ExecutionPlan {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.root.hash(state);
        self.total_cost.hash(state);
    }
}
```

Per cache lookup: walks the structure once, hashes bytes inline. No intermediate Vec.

For high-rate query workloads (the kind a metrics consumer would generate), this saves a postcard encode + Vec alloc per query.

### 195. Federated hash join fetches left and right sub-plans SEQUENTIALLY

**Location:** `federated.rs:375-393`:
```rust
let left_running = Box::pin(self.execute_uncached_with_handle(...)).await?;
if handle.is_cancelled() { return Err(...); }
let right_running = Box::pin(self.execute_uncached_with_handle(...)).await?;
```

Two independent network fetches, serialized. For a hash join over remote left + remote right with 50ms RTT each: 100ms sequentially vs 50ms in parallel.

**Fix:** `tokio::join!` or `futures::future::try_join`:
```rust
let (left_running, right_running) = tokio::try_join!(
    self.execute_uncached_with_handle(left_plan, handle.clone()),
    self.execute_uncached_with_handle(right_plan, handle.clone()),
)?;
```

Pure 2× speedup on every federated hash join. No correctness change — sides are independent by construction.

### 196. Federated filter has NO predicate pushdown — ships every row over the network then filters at aggregator

**Location:** `federated.rs:452-494`. The filter operator:
1. Fetches the inner plan over the network (potentially fetching millions of rows)
2. Drains all rows into an aggregator Vec
3. Applies the predicate locally
4. Discards filtered-out rows

The remote holder ships every row the inner plan produces; the predicate runs at the aggregator. If the filter selectivity is 1% (typical for "find records matching X"), 99% of the shipped rows are wasted bandwidth + 99% of synthetic_row_view work is wasted.

**Fix:** Push the filter down to the holder. The wire request carries the predicate; the server executes the filter inside its local executor before shipping. Aggregator receives pre-filtered rows.

Implementation: extend `MeshDbRequest::Execute` to carry an optional filter on the leaf operator. The server's `LocalMeshQueryExecutor` already has `execute_filter`; just plumb the wire request through to it.

For high-cardinality / low-selectivity filters (the typical telemetry shape), this is a 10-100× bandwidth + latency reduction.

### 197. Planner `collect_coverage` walks every node in the capability index per query

**Location:** `meshdb/planner.rs:1103-1140`:
```rust
fn collect_coverage(&self, origin_hash: u64) -> Vec<HolderCoverage> {
    let hex = chain_hex(origin_hash);
    let mut out: Vec<HolderCoverage> = Vec::new();
    for node_id in self.capability_index.all_nodes() {
        let claim = self.capability_index
            .with_caps(node_id, |caps| {
                caps.tags.iter()
                    .filter_map(|t| parse_causal_claim(t, &hex))
                    .max_by(...)
            })
            .unwrap_or(None);
        if let Some(claim) = claim {
            out.push(HolderCoverage { ... });
        }
    }
    sort_by_proximity(&mut out);
    out
}
```

Per planning of an atomic operator (At/Between/Latest), walks every node in the mesh.

For a 1000-node mesh: 1000 `with_caps` shard-lock acquires + per-node tag-set walk + parse_causal_claim per tag.

Recurses through `plan_join` for composite queries: N joined operands × 1000 walks = O(N × nodes) per planning call.

**Two fixes:**

1. **`with_caps` shard batching:** Same as #114 — group node_ids by shard, acquire each shard lock once, walk all candidates in that shard, release. Lock ops drop from O(nodes) to O(shards).

2. **Reverse index: chain → holders:** Maintain `chain_holders: DashMap<u64 (origin_hash), HashSet<u64 (node_id)>>` in the capability index, updated on announcement. `collect_coverage` becomes O(holders_for_this_chain) instead of O(all_nodes). Most chains have a small holder set.

For a query against a chain held by 5 nodes out of 1000: 200× reduction in planner work.

### 198. `MeshDbWireTransport::send` takes a write lock on the inflight HashMap per send

**Location:** `meshdb/transport.rs:380, 406`:
```rust
let prev = self.inflight.write().insert(call_id, InflightCaller { tx, target_node: node });
// ...
if let Err(e) = send_result {
    self.inflight.write().remove(&call_id);   // <-- another write lock
    return Err(e);
}
```

Per send: write lock acquire + release. Every concurrent send serializes on this lock.

Drop guard (line 446) takes another write lock per stream drop.

**Fix:** `DashMap<u64, InflightCaller>` instead of `RwLock<HashMap>`. Sharded — concurrent sends to different call_ids hit different shards.

For high-concurrency meshdb consumer workloads (many parallel queries in flight), this is the dominant client-side contention point.

## 🟡 Medium-impact

### 199. `hash_join_one_sided` clones build + probe rows per emitted match

**Location:** `executor.rs:482-485` (and the symmetric `hash_join_full_outer` at line 529):
```rust
let (left, right) = if swap {
    (Some(p.clone()), Some(b.clone()))
} else {
    (Some(b.clone()), Some(p.clone()))
};
out.push(encode_joined_row(left, right)?);
```

Per matched pair in the join output: 2 full ResultRow clones (including payload Vecs). For a join producing 10K matches with 1KB payloads: 20MB of clone allocation + memcpy.

**Fix:** `Arc<ResultRow>` for the build side (the side stored in the hash table — multiple probes can match the same build row). Probe side passes through. For the symmetric encode, only one allocation matters since the other is the probe row (consumed once).

Or: `encode_joined_row` could take borrowed `&ResultRow` since postcard encoding doesn't need ownership.

### 200. `execute_with` clones the row vec for cache insertion AND streams

**Location:** `executor.rs:280`:
```rust
let rows = collect_operator_rows(&plan.root, self.reader.as_ref())?;
cache.insert(
    key,
    super::cache::CachedResult {
        rows: rows.clone(),   // <-- full clone for cache
        // ...
    },
);
// ...
let stream = stream_from_vec(rows, handle.clone());   // <-- original consumed
```

For a 10K-row miss-then-insert: the rows Vec is fully cloned (every ResultRow's payload Vec cloned) so one copy goes to cache and one goes to the stream.

**Fix:** Wrap as `Arc<Vec<ResultRow>>` from the start. Both cache and stream get Arc clones. Single allocation; zero data clone.

Same pattern as #193's cache fix; do both together.

### 201. `encode_joined_row` postcard-encodes one row at a time

**Location:** `federated.rs:425` (inside the per-pair loop):
```rust
for (l, r) in pairs {
    let payload = postcard::to_allocvec(&JoinedRowPayload { left: l, right: r }).map_err(...)?;
    out.push(Ok(ResultRow { origin: 0, seq: SeqNum(0), payload }));
}
```

Per joined output row: a fresh `to_allocvec` call (which allocates a Vec). For 10K joined rows: 10K postcard encoder invocations + 10K Vec allocations.

**Fix:** Pre-allocate a `BytesMut` buffer; postcard-encode into it; freeze + slice per row. Or: reuse one `Vec<u8>` across all rows, clearing between encodes. Reuses the allocation; one capacity for all encodes.

### 202. Federated executor sequentially tries each target on failure

**Location:** `federated.rs:307-326`:
```rust
for &target in &targets {
    match self.transport.send(target, request.clone()).await {
        Ok(s) => { response_stream = Some(s); break; }
        Err(_) => continue,
    }
}
```

Tries targets one at a time. For the common case of "all targets healthy, first one is slowest," you wait for the slow one before trying any of the others.

**Fix:** Hedged execution — send to target 1, set a timer T, if no response after T send to target 2 in parallel, take whichever responds first. Cancel the loser.

Tradeoff: hedging doubles wire traffic on slow-target queries. Configurable hedge timeout — disabled by default, enabled per-query via `ExecuteOptions::hedge_after: Option<Duration>`.

### 203. `request.clone()` per fallover attempt

**Location:** `federated.rs:312`. Each target attempt clones the whole request (including `plan: ExecutionPlan`). For a complex plan with deeply nested operators, deep clone per attempt.

**Fix:** `Arc<MeshDbRequest>` if the transport accepts it, or move the request into the first attempt and reconstruct only on retry. Most queries succeed on the first target, so this is overhead for the rare retry case.

### 204. `flatten_json` allocates `format!("{prefix}.{k}")` per nested object key

**Location:** `meshdb/row.rs:99-103`:
```rust
let next = if prefix.is_empty() {
    k.clone()
} else {
    format!("{prefix}.{k}")
};
flatten_json(&next, v, tags, metadata);
```

For a deeply nested JSON object, every object key triggers a `format!` allocation for the dotted path. For a 5-level-deep object with 10 keys per level: 50K format! calls per row.

**Fix:** Pass a `String` buffer down the recursion, push/pop the prefix segments instead of allocating per call:
```rust
fn flatten_json(prefix: &mut String, value: &Value, tags: &mut Vec<Tag>, metadata: &mut BTreeMap<String, String>) {
    let prefix_len = prefix.len();
    for (k, v) in map {
        if prefix_len > 0 { prefix.push('.'); }
        prefix.push_str(k);
        flatten_json(prefix, v, tags, metadata);
        prefix.truncate(prefix_len);
    }
}
```

One String allocation total, reused across the whole tree. Compounds with #192 — if the JSON flatten goes away entirely, this becomes moot.

### 205. `stream_results_cancellable` checks the cancel flag per row

**Location:** `federated.rs:1008-1010`:
```rust
let stream = futures::stream::iter(rows).map(move |item| {
    if handle.is_cancelled() { return Err(...); }
    // ...
});
```

Atomic load per yielded row. For 1M-row streams, 1M atomic loads. Negligible per row (~1ns each), but if the stream is consumed by `.collect()` immediately, it's pure overhead.

**Fix:** Check the cancel flag in batches — every N rows instead of every row. For a 1M-row stream with batch=256, drops from 1M to 4K atomic loads.

Or: yield to the runtime via `.poll_next` natural backpressure points and check cancel there.

Cleanup-grade; the atomic load is genuinely cheap.

### 206. `drain_rows` has no with_capacity hint

**Location:** `federated.rs:980`. Pre-known result size for many operators, but the drain uses `Vec::new()` and grows. For a 1M-row drain: O(log N) reallocations.

Comment notes the bound is `AGGREGATE_MAX_BYTES`, so a sane upper bound exists. `Vec::with_capacity(1024)` (start small, grow on demand) at minimum avoids the first few small reallocs.

## 🟢 Low-impact / cleanup

### 207. `MeshDbServer::dispatch_request` likely takes write lock on server inflight map

`transport.rs:539, 653`. Same DashMap fix as #198 applies to the server-side inflight tracking.

### 208. `LruResultCache::insert` runs `evict_until_within_bounds` after every insert

`cache.rs:293, 313`. Cap check + eviction sweep per insert. For caches operating well within bounds, this is a no-op — but it does check. Cheap, but checking every insert vs checking only when total_bytes is near the cap is a trivial optimization.

### 209. `CachedResult::approx_bytes` walks every row per call

`cache.rs:147-153`. Used by insert for bookkeeping. For a 10K-row insert, walks all 10K rows summing payload lengths. Cache it on the struct at construction time.

### 210. `parse_causal_claim` is called per-tag per-node in collect_coverage

`planner.rs:1122`. Per node × per tag × per planning call. Most tags don't match `causal:` prefix; the function presumably does an early-exit prefix check. If not, that's the fix.

### 211. `chain_hex(origin_hash)` formats hex per planning call

`planner.rs:1104`. Same `hex32`-style pattern as #171. Same lookup-table fix.

### 212. `select_targets_latest` allocates twice (with_tip Vec + out Vec)

`planner.rs:1180-1196`. Could collect into a single sorted Vec via partition + sort. Cleanup-grade; per-query frequency.

---

## What I'd actually do

Given MeshDB is dormant pending consumer activation, the optimization strategy is "prevent the hot consumer from inheriting performance debt." Fix the items that would matter as soon as someone turns on the feature.

**Pre-activation priority list:**

1. **#192 — fix `synthetic_row_view`.** Single biggest per-row cost. Will dominate any filter-heavy consumer. Choose between field-aware predicate compilation OR direct JSON evaluation; both avoid the per-row Vec+BTreeMap allocations.

2. **#193 + #200 — Arc-wrap CachedResult and the executor's row vec.** Cache hits become near-free instead of "as expensive as a re-run." Required if any consumer cares about caching at all.

3. **#196 — predicate pushdown to remote holders.** Filter at the source, not the aggregator. 10-100× bandwidth/latency reduction for selective filters.

4. **#195 — parallel left/right fetch in federated hash join.** Trivial fix, 2× win on every federated hash join.

5. **#197 — chain → holders reverse index in capability index.** Planner becomes O(holders) instead of O(all_nodes). Required to scale beyond ~100 nodes.

**Lower priority — do these if consumer activates:**

6. **#194 — Hash directly into the hasher, skip the postcard alloc.**
7. **#198 + #207 — DashMap for inflight tracking on both client + server transport.**
8. **#199 + #201 — Arc-wrap build-side rows in hash joins, reuse encode buffer.**

**Skip unless profiling justifies:**

The clock-flag-per-row items (#205), the with_capacity hints (#206), the cleanup items (#208-#212) — all genuinely small.

---

## Cross-cutting with prior findings

The patterns in MeshDB exactly mirror what's been found across the rest of the codebase:

- **`X.clone()` per cache hit / per read (#193, #199, #200)** — same as #11, #96, #149, #175. ArcSwap or Arc<T> uniformly.
- **`Vec<u8>` instead of `Bytes` for payloads (#192's ResultRow, #201's encode)** — same as #84, #128, #184.
- **Sequential I/O over independent operands (#195)** — same as #83, #172, #173.
- **DashMap walk + with_caps per node (#197)** — same as #114, #148, #179.
- **`RwLock::write` per operation (#198, #207)** — same pattern as #117 (auth cache).
- **No pushdown of predicates / selections (#196)** — analogous to the lack of pre-filtering in `get_available_endpoints` (#148).

A workspace-wide grep audit applying the same handful of patterns would land most of these mechanically.

---

## Honest expectation

If MeshDB stays dormant, none of this matters at runtime. If MeshDB activates with a real consumer:

- **Filter-heavy workloads** (the most likely consumer shape): #192 + #196 are the difference between "viable" and "DOA." Pre-emptive fix required.
- **Dashboard / metrics workloads** (cached frequently-repeated queries): #193 is the difference between "fast hit" and "as slow as a miss." Required for caching to be useful.
- **Cross-node joins** (e.g. correlating events across services): #195 + #196 + #199 collectively determine whether joined queries are usable interactively.
- **Large mesh** (1000+ nodes): #197 is required to keep planner cost bounded.

Of all the subsystems audited, MeshDB has the cleanest set of "fix before users notice" wins. Per-row JSON parsing (#192) and per-hit deep cloning (#193) are the kind of things that, once a consumer is in production, become very expensive to refactor — both because the consumer depends on the precise output shape AND because anyone optimizing has to deal with the consumer's expectations of behavior.

Recommendation: **prioritize fixing #192, #193, #196, #197 before MeshDB ships to a real consumer.** Everything else can wait until profiling tells you what matters.
