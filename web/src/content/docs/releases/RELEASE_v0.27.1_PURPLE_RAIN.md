# Net v0.27.1 — "Purple Rain"

## A pure performance release — nothing on the wire moves

v0.27.1 ships no new systems, no new SDK surface, and no protocol changes. Every change either replaces an O(shards) operation with an O(1) atomic, swaps an O(n) full-scan for an index read, deletes an allocation, or corrects a benchmark fixture that was reporting fiction. The work is recorded in full in [`docs/misc/PERF_AUDIT_2026_06_08_BENCHMARK_WINS.md`](../misc/PERF_AUDIT_2026_06_08_BENCHMARK_WINS.md); this log is the operator-facing summary.

The organizing observation, the same shape as v0.27's: **the substrate was answering cheap questions expensively.** `len()`, `node_count()`, and `stats()` are called on admission gates and per-selection hot paths, and the default `DashMap` shards to `4 × num_cpus` (128 on a 32-thread host), so every one of those calls locked and summed 128 shards regardless of how few entries the map held — an ~950 ns fixed cost to read a number the code could have maintained as it went. v0.27.1 maintains it as it goes.

---

## `DashMap::len()` was a 128-shard walk on hot paths

The cross-cutting fix. Five subsystems carried `AtomicUsize` (and `AtomicU64`) counters that are now maintained exactly on every insert / remove / eviction, replacing the per-shard walk:

- **`LocalGraph`** (`swarm.rs`) — `num_nodes` / `num_edges` / `num_seen`. The hot one: the `seen_pingwaves` soft-cap gate ran on **every accepted pingwave**, paying the shard walk per admission. `local_graph/on_pingwave_duplicate` drops from **974 ns → 16 ns (~60×)**.
- **`ProximityGraph`** (`behavior/proximity.rs`) — `num_nodes` / `num_edges` / `num_seen`.
- **`MetadataStore`** (`behavior/metadata.rs`) — `node_count`, and `stats()` now reads its inverted indexes (status / tier / continent) instead of full-scanning every node with a `String` allocation per entry.
- **`FailureDetector`** (`failure.rs`) — `num_nodes`, plus `check_all()` now reads the monotonic clock **once per sweep** instead of once per node.
- **`RoutingTable`** (`route.rs`) — `num_routes` / `num_streams`, including the per-novel-stream admission gate.

`node_count()` / `len()` / `stats()` reads collapse from ~950 ns to a sub-nanosecond atomic load. The `FailureDetector` per-status (healthy / suspected / failed) tally is deliberately **kept as a scan** — it's observability-only and node status is mutated in place, so a maintained per-status counter would silently drift. The scan is always exact.

---

## Capability serialize — a one-word fix

`sorted_tag_vec` sorted capability tags with `sort_by_key(|t| t.to_string())`, which re-renders each `Tag` to a `String` on **every comparison** (~N log N allocations). Switched to `sort_by_cached_key`, which renders each tag exactly once (N allocations). Output order is byte-identical, so signed `CapabilityAnnouncement` bytes stay stable across peers — pinned by a regression test. `capability_set/serialize` drops **65.3 µs → 9.6 µs (~6.8×)**; `capability_announcement/serialize` **71.7 µs → 11.8 µs (~6.1×)**.

---

## API registry — O(1) counts, index-derived stats, allocation-free path match

`ApiRegistry` (`behavior/api.rs`) got the same treatment plus an allocation fix:

- `len()` / `is_empty()` / `stats().total_nodes` and the register capacity gate now read `node_count` / `total_endpoints` atomics. `api_registry_basic/len`: **1.42 µs → 0.20 ns**.
- `stats()` reads `apis_by_name` from the `by_api_name` inverted index (provider count per name, skipping empty buckets) rather than full-scanning every node and schema with a `String` clone per schema. `api_registry_basic/stats`: **~201 ms → ~7 µs**.
- `find_by_endpoint` called `matches_path(..).is_some()`, allocating two `Vec`s + a `HashMap` + a `String` per endpoint per node just to extract a bool. A new allocation-free `ApiEndpoint::path_matches() -> bool` replaces it at the three params-discarding call sites (the full scan is retained — it's correct for endpoints whose first path segment is a parameter, which a prefix index would miss). `api_registry_query/find_by_endpoint`: **6.98 ms → 1.88 ms (~3.7×)**, all from dropped allocation.

`stats()`'s `apis_by_name` is now *distinct provider nodes per API name* (the index is a provider set); this differs from the old per-schema-instance count only when one node advertises the same API name in two schemas — a degenerate case, documented and pinned by a test.

---

## Load balancer — snapshot selection, right-sized hash ring

`LoadBalancer::select` (`behavior/loadbalance.rs`) is a per-dispatch hot path in `GroupCoordinator`, and `get_available_endpoints` iterated the `endpoints` `DashMap` via `DashMap::iter` — a 128-shard walk regardless of endpoint count.

- **Endpoint snapshot.** The authoritative `DashMap` is kept for point lookups (reservation, health/metric updates); `select` / `stats` / `endpoints` / `endpoint_count` now iterate a flat `ArcSwap<Vec<Arc<EndpointState>>>` snapshot rebuilt only when the endpoint *set* changes. Per-endpoint atomic state (health, connections, circuit) stays live through the shared `Arc`s. `lb_strategies/round_robin`: **8.24 µs → ~340 ns (~24×)**; `lb_scaling/select/10`: **5.59 µs → ~370 ns (~15×)**.
- **Right-sized hash ring.** `consistent_hash` selection walks the separate `hash_ring` `DashMap`, which the snapshot doesn't cover; it was over-sharded the same way. Pinning it to 8 shards (`HASH_RING_SHARDS`) cut `lb_strategies/consistent_hash` **~20% (49.1 µs → 39.8 µs)**, no new invariants.

A documented experiment (in the audit, "Snapshot vs. right-sized DashMap") confirmed the snapshot is not over-engineering: replacing it with a merely right-sized `endpoints` `DashMap` **regressed `select` ~2×** (a wait-free `ArcSwap` load over a contiguous `Vec` beats locking even 8 shards over scattered HashMap buckets on the iterate-heavy path). The snapshot stays; only the ring — which it doesn't cover — was right-sized.

---

## Concurrency hardening (correctness, shipped with the perf work)

The dual-store and counter changes drew a review pass that closed five latent races before they could ship:

- **`LoadBalancer` membership lock** — `add_endpoint` / `remove_endpoint` now serialize the map mutation + snapshot rebuild under a `Mutex`, so concurrent membership changes can't store a stale snapshot last (which would silently drop a just-added endpoint from rotation). Off the hot path; `select` only reads.
- **Removed-endpoint flag** — an `EndpointState.removed` bit, set on removal and checked in `is_available()`, so a selector reading a snapshot taken just before a concurrent removal filters the gone endpoint out instead of burning a reservation retry into a transient false `NoEndpointsAvailable`.
- **`ApiRegistry::register` made atomic per node** — the read-old / re-index / insert sequence now runs under a single `nodes` entry lock (mirroring `MetadataStore::upsert`), so concurrent re-registration of the same node can't drift `total_endpoints` (which, decremented with `fetch_sub`, could otherwise underflow to a huge value).
- **`ApiRegistry::clear` drains instead of `store(0)`** — per-key decrement through the same chokepoints the live paths use, so a concurrent `unregister` racing `clear` can't underflow the counters.
- **`RoutingTable::get_stream_stats` gated on the cap** — it created a `stream_stats` entry for any id unconditionally, bypassing the `MAX_STREAM_STATS` soft cap the `record_*` paths enforce; now gated, returning `Option`.

All five carry regression tests (including multi-thread stress tests for the counter races).

---

## Benchmark fixtures — corrections, not wins

Three of the largest "before" numbers were never real production costs — they were shared, growing Criterion fixtures bleeding into each other. The audit's §7 records them so nobody chases the wrong number, and the O(1)/fixture work makes them moot:

- `failure_detector/check_all` (670 ms), `failure_detector/stats` (198 ms), and `metadata_store_basic/stats` (169 ms) were inflated by the `heartbeat_new` / `register_new` benches ballooning a *shared* detector/store that the later `stats`/`check_all` closures reused. `check_all` is genuinely O(n), so its bench got a dedicated `growth_detector`; the `stats`/`len` numbers are moot post-rework because those methods are now O(1) regardless of map size. Post-fix: **check_all 16.7 µs, stats 16 µs, metadata stats 15.9 µs.**

---

## Measured results

Full table in the audit doc. Headline figures (Intel i9-14900K, Criterion defaults):

| Benchmark | Before | After | Change |
|---|---|---|---|
| `local_graph/node_count` | 958 ns | 0.20 ns | ~4770× |
| `local_graph/stats` | 2.89 µs | 0.33 ns | ~8850× |
| `local_graph/on_pingwave_duplicate` | 974 ns | 16 ns | ~60× |
| `metadata_store_basic/len` | 956 ns | 0.20 ns | ~4750× |
| `routing_table/aggregate_stats` | 13.1 µs | 6.07 µs | ~2.2× |
| `capability_set/serialize` | 65.3 µs | 9.63 µs | ~6.8× |
| `api_registry_basic/len` | 1.42 µs | 0.20 ns | ~6970× |
| `api_registry_query/find_by_endpoint` | 6.98 ms | 1.88 ms | ~3.7× |
| `lb_strategies/round_robin` | 8.24 µs | ~340 ns | ~24× |
| `lb_scaling/select/10` | 5.59 µs | ~370 ns | ~15× |
| `lb_strategies/consistent_hash` | 50.6 µs | 39.8 µs | ~1.27× |

Absolute "after" figures on the sub-µs `select`/`lb` rows carry ±40–50% run-to-run variance on the dev box; they're representative, not precise, and the audit's re-verification note documents the spread. The multipliers and the order-of-magnitude wins are stable.

**SIMD crypto (documented, opt-in).** The audit's highest-leverage item — the ChaCha20-Poly1305 AEAD running on the software backend rather than AVX2 — is **documented but deliberately not enforced** in committed config: a baked-in `+avx2` floor would `SIGILL` on pre-AVX2 x86-64 and is meaningless on ARM. Operators opt in per target class via `RUSTFLAGS="-C target-feature=+avx2"` (or `target-cpu=native`); default builds keep the software path, so nothing regresses and the ~5–10× data-path win is unlocked per deploy. See §1 of the audit.

---

## Breaking changes

**None on the wire, and none to behavior.** v0.27.1 interoperates with v0.27.0 peers freely.

One minor **source-level** API refinement: `RoutingTable::get_stream_stats` now returns `Option<Ref<…>>` instead of `Ref<…>` (it returns `None` for a novel stream id once `MAX_STREAM_STATS` is reached, closing an unbounded-growth path). The type is re-exported, so an external caller would need to handle the `Option`; there are no in-tree callers outside tests.

---

## How to upgrade

Drop-in. Bump the dependency to `0.27.1` — no source changes required for the common case, no atomic peer roll, no config changes. The performance wins apply automatically. Two optional levers:

1. **SIMD crypto:** rebuild the x86-64 target class with `RUSTFLAGS="-C target-feature=+avx2"` to unlock the AEAD fast path. Default builds are unchanged.
2. `get_stream_stats` callers (if any exist downstream) add an `Option` match / `expect`.

---

## Dependency updates

Routine patch bumps only — no major or minor version changes, no behavioral surface change. The wasm-bindgen family and the `js-sys`/`web-sys` pair move together as usual:

**`http`** (1.4.1 → 1.4.2), **`js-sys`** (0.3.99 → 0.3.100), **`uuid`** (1.23.2 → 1.23.3), **`wasm-bindgen`** / **`wasm-bindgen-macro`** / **`wasm-bindgen-macro-support`** / **`wasm-bindgen-shared`** (all 0.2.122 → 0.2.123), **`web-sys`** (0.3.99 → 0.3.100). `Cargo.lock` carries the exact pinned versions.

---

Released 2026-06-09.

## License

See [LICENSE](../../LICENSE).
