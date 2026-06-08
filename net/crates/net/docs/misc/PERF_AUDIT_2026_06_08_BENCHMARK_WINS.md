# Performance Audit — Benchmark Wins (2026-06-08)

Source data: `crates/net/benchmarks/BENCHMARK_RESULTS_14900K.md` (Intel i9-14900K, 32
logical cores). Investigation root-caused each hotspot against current `src/`. **No code
was changed** — this document is findings + concrete fix proposals.

The work is split into two buckets:

- **Real production wins** — code that is slow on a path that runs in production.
- **Benchmark artifacts** — headline numbers that are inflated by the bench harness
  (shared/growing fixtures, bench-only types, mislabeled benches) and do *not* reflect a
  production regression. Listed so nobody chases the wrong number.

Recommended order of attack is at the bottom.

---

## 1. (Highest leverage) Crypto runs on the software AEAD backend, not AVX2

**Symptom.** Every `encrypt` call carries ~1.0–1.1 µs of *fixed* cost, independent of
payload size:

| bench | time |
|---|---|
| `net_encryption/encrypt/64` | 1.13 µs |
| `net_encryption/encrypt/256` | 1.20 µs |
| `net_encryption/encrypt/1024` | 1.57 µs |
| `net_encryption/encrypt/4096` | 3.13 µs |
| `net_packet_build/build_packet/1` | 1.13 µs (dominated by the above) |

For ChaCha20-Poly1305 on a modern x86-64 core, 64 bytes should be ~80–200 ns. There is
~1 µs of fixed overhead before any per-byte work.

**Root cause — NOT the usual suspects.** All four common culprits were checked against
the code and ruled out:

- Cipher is built once and cached/reused. `ChaCha20Poly1305::new(...)` runs in
  `PacketCipher::new` / `with_shared_tx_counter` (`src/adapter/net/crypto.rs:600,621`);
  pooled builders are returned to the pool on drop, so the key schedule is reused.
- Nonce is a `session_prefix ++ AtomicU64 counter` built on the stack
  (`crypto.rs:632-649`) — no `OsRng`/`getrandom` syscall per call.
- Output is written into a reused `BytesMut`; `split().freeze()` retains spare capacity,
  so reallocation is amortized (~every 55 calls), not per-call.
- Key derivation (BLAKE2s) only runs once at handshake (`crypto.rs:341-356`), never on
  the encrypt path.

The actual cause: **the AEAD is compiled to the portable/software backend because the
build sets no CPU-feature flags.**

- `crates/net/.cargo/config.toml` has `rustflags = ["-C", "target-cpu=native"]`
  **commented out**, and no `RUSTFLAGS` is exported.
- Locked: `chacha20poly1305 0.10.1`, `chacha20 0.9.1`, `poly1305 0.8.0`. These RustCrypto
  crates only compile their AVX2 backend when the target enables AVX2 (via
  `target-cpu=native` or `-C target-feature=+avx2`). On the default x86-64 baseline,
  ChaCha20 uses the portable/SSE2 path and Poly1305 the software path.
- The software backend's per-call fixed cost (block-counter setup, Poly1305 init, MAC
  over the 56-byte AAD from `NetHeader::aad()` at `src/adapter/net/protocol.rs:347`, plus
  the finalize) dominates small payloads — exactly the observed ~1.1 µs floor.
- The existing `bench-native` cargo alias is just `bench --release`; it does **not**
  inject `target-cpu=native`, so it does not fix this.

**Hot path.** `src/adapter/net/crypto.rs:661` `encrypt_in_place`; caller
`src/adapter/net/pool.rs:181`.

**Fix.** Enable the SIMD backend — no source change:

- Uncomment in `.cargo/config.toml`:
  ```toml
  [build]
  rustflags = ["-C", "target-cpu=native"]
  ```
- Or set a portable floor for the x86-64 deploy class:
  `RUSTFLAGS="-C target-feature=+avx2"` (or `+avx2,+sse4.1`).

**Expected.** ~5–10× on the fixed cost: 64 B encrypt ~1.13 µs → ~100–200 ns; 4096 B
~3.13 µs → ~0.6–1 µs. This is on **every packet**, so it lifts the entire data path,
`net_packet_build`, and both `cipher_comparison` groups at once.

**Caveat.** Assumes the deploy target has AVX2. If the build host may differ from prod,
prefer explicit `-C target-feature=+avx2` over `target-cpu=native`.

---

## 2. (Cross-cutting) `DashMap::len()` is a 128-shard walk (~950 ns) — and it's on a hot path

`DashMap::len()` locks and sums every shard's length. Default shard count is the next
power of two of `4 × num_cpus` → **128 shards** on the 14900K. So every `.len()` is ~128
lock/atomic ops *independent of element count* — empirically ~977 ns. This is the floor
under several "count"/"stats" benches.

### 2a. `seen_pingwaves.len()` on every pingwave admission — **real production tax**

**File:** `src/adapter/net/swarm.rs:546`

```rust
if self.seen_pingwaves.len() >= MAX_SEEN_PINGWAVES {   // MAX = 262_144
    return None;
}
```

This runs on **every accepted pingwave**. In steady state, once `seen_pingwaves` is
non-trivial, every admission pays the ~977 ns shard-walk — at packet/heartbeat rates ×
peers this is a measurable hot-path tax.

This is what the bench surfaces as `local_graph/on_pingwave_duplicate` = 973 ns (20× the
47 ns insert path). Confirmed by standalone repro (`dashmap = 6`): `contains_key` miss
~15 ns + `len()` on a 262 k-entry map = 977 ns ≈ the reported 973 ns. (The sibling
`mesh_proximity` bench uses a separate `growth_graph`, so its dedup map stays tiny and it
does *not* show the cost — same `on_pingwave` code, different harness.)

**Fix.** Maintain an `AtomicUsize seen_count`, incremented on insert and decremented in
`evict_stale_pingwaves` (`swarm.rs:803`); check the atomic instead of `len()`.

**Expected.** ~973 ns → ~15–50 ns (**~20–60×**) on that path, and removes the shard-walk
from every production pingwave admission.

### 2b. Other `len()` / `node_count()` sites (~950 ns each)

Same root cause, same fix (AtomicUsize maintained on the existing mutation chokepoint):

| method | file:line | notes |
|---|---|---|
| `MetadataStore::len()` | `src/adapter/net/behavior/metadata.rs:1271` | also called by the capacity check at `metadata.rs:1034` |
| `LocalGraph::node_count()` / `edge_count()` | `src/adapter/net/swarm.rs:825,830` | `stats()` (swarm.rs:816) calls `.len()` 3× |
| `MeshProximityGraph::node_count()` | `src/adapter/net/behavior/proximity.rs:976` | `stats()` (proximity.rs:960) calls `.len()` |
| `RoutingTable::aggregate_stats()` `.len()` calls | `src/adapter/net/route.rs:745-763` | `routes.len()` + `stream_stats.len()` shard-walks |

**Expected.** ~50–60× each (950 ns → ~5–15 ns).

---

## 3. (One-word fix) Capability serialize allocates a String per comparison

**Symptom (backwards ratio — serialize slower than deserialize):**

| bench | time |
|---|---|
| `capability_set/serialize` | 65 µs |
| `capability_set/deserialize` | 8.3 µs |
| `capability_announcement/serialize` | 71 µs |
| `capability_set/serialize_compact` | 2.0 µs |

**Root cause.** `src/adapter/net/behavior/capability.rs:1851` in `sorted_tag_vec`:

```rust
fn sorted_tag_vec(tags: &HashSet<Tag>) -> Vec<Tag> {
    let mut v: Vec<Tag> = tags.iter().cloned().collect();
    v.sort_by_key(|a| a.to_string());   // <-- re-allocates a String on EVERY comparison
    v
}
```

`sort_by_key` does not cache the key — it re-invokes the closure on every comparison, and
`Tag::to_string()` heap-allocates each time. For ~35 tags that's ~180 comparisons × 2
allocations ≈ hundreds of short-lived heap allocations per serialize. The sort exists only
to make JSON byte-stable for signature verification (`capability.rs:1866-1872`). The
compact path (postcard, not human-readable) skips the sort entirely (`capability.rs:1893`)
— hence 2 µs vs 65 µs.

`CapabilityAnnouncement` embeds a `CapabilitySet`, so it inherits the same path — which is
why both are ~65–71 µs. Hot path: `CapabilitySet::to_bytes` (`capability.rs:1470`),
`CapabilityAnnouncement::to_bytes` (`capability.rs:2300`), field decl at `capability.rs:875`.

**Fix.** `capability.rs:1851`: `sort_by_key` → `sort_by_cached_key` (computes each key
once → N allocations instead of ~N·log N × 2). **Byte-identical output, wire- and
signature-safe.** Optionally also drop the `tags.iter().cloned().collect()` clone
(`capability.rs:1850`) by sorting `Vec<(String, &Tag)>` borrows.

**Expected.** ~5–10×, fixes both `capability_set` and `capability_announcement` serialize.

---

## 4. `stats()` recomputes full-map aggregates (+ a String alloc per element)

These methods full-scan the backing map on every call instead of maintaining
incrementally-updated counters. (Note: the *headline* ms-scale numbers for some of these
are bench artifacts — see §7 — but the methods themselves still benefit from O(1)
counters, and the per-element work below is real.)

### 4a. `MetadataStore::stats()` — `src/adapter/net/behavior/metadata.rs:1244-1268`

```rust
for entry in self.nodes.iter() {
    let meta = entry.value();
    *by_status.entry(meta.status).or_default() += 1;
    *by_tier.entry(meta.topology.tier).or_default() += 1;
    if let Some(ref loc) = meta.location {
        *by_continent.entry(loc.region.continent().to_string()).or_default() += 1;  // String alloc per node
    }
}
```

Full scan + a `String` allocation per node for the continent key. **Fix:** maintain
histogram counters in the existing `add_to_indexes`/`remove_from_indexes` chokepoints
(`metadata.rs:1330/1376`); `stats()` becomes a snapshot read.

### 4b. `FailureDetector` — `src/adapter/net/failure.rs`

- `stats()` (`failure.rs:329-351`) — full scan to tally `Healthy/Suspected/Failed`. Fix:
  three `AtomicUsize` status counters adjusted on every status transition.
- `check_all()` (`failure.rs:230-254`) — `NodeState::check` (`failure.rs:93-110`) calls
  `self.last_heartbeat.elapsed()` → a monotonic-clock read **per node**. Fix: read
  `Instant::now()` once before the loop and pass it in (~2–3× on the iteration), and a
  status-bucket index could let it skip already-`Failed` nodes.

### 4c. `RoutingTable::aggregate_stats()` — `src/adapter/net/route.rs:745-763`

Sums three per-stream atomics over a full `stream_stats` scan + two `.len()` shard-walks
(13 µs at the bench's stream count). **Fix:** (a) replace the two `.len()` with atomics
(see §2b); (b) optionally maintain table-level `AtomicU64` running totals updated wherever
`packets_in/out/drops` are bumped, making it fully O(1) (13 µs → tens of ns).

---

## 5. (Moderate) Capability query double-materializes the candidate set

**Symptom:** broad queries are hundreds of µs — `capability_fold_query/query_single_tag`
150 µs, `query_gpu_vendor` 491 µs, `find_best_simple` 302 µs.

**Important:** the ~40 ns/check fold-scan is **by design** (the fold index is built to scale
to millions of nodes) and is *not* flagged here. The win is purely constant-factor.

**Root cause.** Cost is proportional to the *result-set size*, and these benches select
half-to-all of 10 k nodes. The query path materializes candidate keys into a `HashSet`
(`resolve_candidate_keys`, `src/adapter/net/behavior/fold/capability.rs:465-569`), then
re-materializes into a `Vec<NodeId>` and does `sort_unstable + dedup`
(`find_nodes_matching`, `capability_bridge.rs:466-497`) — two full passes + redundant
hashing over a 5–10 k set. `group_union` (`capability.rs:595-603`) clones a whole 10 k
bucket just to seed a single-element group.

**Fix.** When there is exactly one selective dimension and no remaining `retain`
predicates, iterate that index bucket directly into the output `Vec` (skip the
intermediate `HashSet`); size the output from the bucket and skip `sort/dedup` in the
common class-unique case.

**Expected.** ~1.5–2× on broad single-axis queries. The hundreds-of-µs floor is largely
inherent to returning thousands of rows — this attacks only the duplicated
allocation/hashing.

---

## 6. (Minor) Cortex ingest allocates a serialize buffer per event

`cortex_ingest/tasks_create` = 214 ns, `memories_store` = 451 ns. Hot path `ingest_typed`
(`src/adapter/net/cortex/tasks/adapter.rs:484-505`,
`src/adapter/net/cortex/memories/adapter.rs:444-469`) does a per-call
`postcard::to_allocvec` (`Vec<u8>` alloc) + `String` field conversions + a checksum hash.
No syscall, no O(state) work — purely allocation-bound. (`memories_store` is ~2× because
it carries `content` + tags `Vec<String>` + `source`.)

**Fix.** Serialize into a reusable thread-local / per-adapter scratch buffer
(`postcard::to_slice`) then `Bytes::copy_from_slice`. Keep the checksum (integrity).
**Expected:** ~30–40% off the serialize/alloc portion.

---

## 7. NOT real wins — benchmark artifacts (do not chase these numbers)

- **`failure_detector/check_all` = 670 ms, `failure_detector/stats` = 198 ms,
  `metadata_store_basic/stats` = 168 ms, `metadata_store_basic/len` = 950 ns.**
  Inflated by **shared, growing Criterion fixtures**: the `heartbeat_new` (`benches/net.rs:1730`)
  and `upsert_new` (`benches/net.rs:2867`) closures insert a fresh key every iteration into
  a `detector`/`store` that the later `stats`/`check_all` closure *reuses*, ballooning the
  map to millions of entries before the aggregation runs. `mesh.rs` already avoids this with
  a dedicated `growth_graph` (`mesh.rs:116`); the `net.rs` metadata/failure groups don't.
  The §2/§4 counter fixes still help, but **fixing the benches** (dedicated growth fixture)
  removes 3–5 orders of magnitude from these headline numbers.

- **`pool_contention/shared_*` = 9.7–47 ms (vs `fast_*` 1.3–3.0 ms).** Real contention on a
  single shared `crossbeam ArrayQueue` in `PacketPool` (`src/adapter/net/pool.rs:393,458,570`)
  — but **no production path constructs it.** `NetSession` uses `ThreadLocalPool` exclusively
  (`src/adapter/net/session.rs:137-138`, via `shared_local_pool`); every non-test `PacketPool`
  construction is in benches/tests only. It's a bench-only anti-pattern baseline — consider
  deleting or clearly labeling it so it doesn't read as a shipping regression. Do not
  reintroduce shared `PacketPool` on any hot path (the `pool.rs:583` comment already warns).

- **`event/internal_event_new` = 212 ns.** Mislabeled — the bench (`benches/ingestion.rs:91`)
  actually calls `InternalEvent::from_value` (`json!` + `serde_json::to_vec`), not
  `InternalEvent::new`. The real `new` is the 26 ns `internal_event_from_bytes` line (a
  `Bytes` refcount bump + 3 field stores, already optimal). The 212 ns is the unavoidable
  `json!`-build + serialize for one-shot callers. Rename the bench; no code waste.

- **`capability_fold_insert/index_nodes` = 31 µs/node (linear).** `Fold::apply`
  (`src/adapter/net/behavior/fold/mod.rs:336-428`) is **O(1) per insert** — it merges only
  the single key, `index.on_insert` touches only that entry's buckets, no whole-state clone
  or re-fold. The 31 µs/node is bench-fixture cost charged inside `b.iter`:
  `sample_capability_set` (many `String` allocs) + `translate_announcement`'s `views()`
  re-deriving tag projections + per-tag `to_string()`. Re-bench with construction hoisted
  via `iter_batched` before claiming an insert regression; the real apply is sub-µs. (A
  small production win exists in trimming the unused `views()` projections in the
  legacy-announcement translate path, but it is not an O(state) bug.)

---

## Recommended order of attack

1. **Crypto AVX2 flag** (§1) — one config line, ~5–10× on the entire data path.
   *Verify the deploy-target CPU first; prefer `+avx2` over `native` if build host ≠ prod.*
2. **`sort_by_key` → `sort_by_cached_key`** (§3) — one word, ~5–10× on capability
   serialize, zero risk, wire-compatible.
3. **`AtomicUsize` for `seen_pingwaves` count** (§2a) — real hot-path tax, ~20–60×.
4. Roll the same atomic-counter pattern into the other `len()`/`node_count()`/`stats()`
   sites (§2b, §4).
5. Capability query single-axis fast path (§5) and cortex scratch buffer (§6).
6. Fix the bench fixtures (§7) so the 168/198/670 ms numbers reflect reality.

Items 1 and 2 are trivial, safe, and high-impact.

---

## Resolution (2026-06-08)

Implemented on branch `perf/benchmark-wins-2026-06-08`, one commit per concern,
each with tests; full lib suite (4192 tests) green and all benches compile.

- **§1 Crypto AVX2** — DONE. `.cargo/config.toml` now sets a portable `+avx2`
  floor; `bench-native` alias injects `target-cpu=native`.
- **§2 / §4 O(1) counters** — DONE across all five subsystems. `AtomicUsize`
  counters maintained on insert/remove/eviction replace `DashMap::len()` shard
  walks in `LocalGraph`, `ProximityGraph`, `MetadataStore`, `FailureDetector`,
  and `RoutingTable` (incl. the hot `seen_pingwaves` and `may_admit_stream`
  gates). `MetadataStore::stats()` now reads the inverted indexes instead of a
  full scan + per-node `String` alloc. `FailureDetector::check_all()` reads the
  clock once per sweep.
  - The `FailureDetector` per-status (healthy/suspected/failed) tally is left
    as a scan **by design**: it's observability-only, and node status is
    mutated in place (`get_mut().status = …`) by tests, so maintained
    per-status counters would silently drift. The scan is always exact.
- **§3 Capability serialize** — DONE (`sort_by_cached_key`).
- **§5 Capability query single-axis fast path** — **NOT done (deliberate).**
  `resolve_candidate_keys` is an intricate query planner whose `HashSet` +
  sort/dedup guarantee correctness when a node appears under multiple classes.
  The fold scan is by-design (see memory `capability-checks-use-folds`). The
  ~1.5–2× win applies only to broad queries returning thousands of rows; the
  regression risk to the capability-routing path outweighs it. Revisit only
  with a dedicated correctness harness.
- **§6 Cortex ingest scratch buffer** — **NOT done (no actual win).** On
  inspection `ingest_typed` already does a single `postcard::to_allocvec` whose
  `Vec` is moved into `Bytes` zero-copy (`Bytes::from`). A reused scratch buffer
  would force `Bytes::copy_from_slice` — adding a copy, not removing the alloc.
  The ~214 ns is postcard serialization + checksum, not avoidable allocation.
- **§7 Benchmark artifacts** — PARTIALLY addressed. The `stats`/`len`/
  `node_count` multi-hundred-ms/µs artifacts are now moot because those methods
  are O(1) regardless of map size (§2/§4). `check_all` is still O(n), so its
  bench fixture was fixed: `heartbeat_new` got a dedicated `growth_detector` so
  it no longer bloats the steady-state detector that `check_all` measures. The
  shared-`PacketPool` contention bench is a bench-only anti-pattern baseline
  (production uses `ThreadLocalPool`) and is kept as a documented contrast. The
  `event/internal_event_new` bench keeps its name (an explanatory comment
  already documents that it measures `from_value`); renaming would only break
  Criterion baseline continuity.
