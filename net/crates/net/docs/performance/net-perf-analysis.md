# Performance Analysis: `ai-2070/net`

Scope: `net/crates/net` — the core event bus, mesh adapter, RedEX append-only log, CortEX adapter + RPC stack.

Stated targets (from `lib.rs`): **≥10M events/sec sustained**, **≥100M events/sec microburst**, **<1µs p99 ingest latency**.

Findings ordered by expected impact within each section. Item numbers run continuously across all subsystems for cross-referencing.

---

## ✅ Fixed

| # | Item | Subsystem | Notes |
|---|------|-----------|-------|
| 13 | `dispatch_batch` retry `batch.clone()` → `Arc<Batch>` | Core bus | `Adapter::on_batch` now takes `Arc<Batch>`; retries are a refcount bump instead of a deep `Vec` clone. Mesh `send_to_peer`/`send_routed` switched to `&Batch` (they never consumed). Pinned by `dispatch_batch_retries_share_the_same_arc_allocation`. |
| 2 | `Mapper::select_shard` per-event allocs → `ArcSwap<SelectionTable>` | Core bus | Hot path is now a single `ArcSwap::load` + Lemire mapping. Two `Vec` allocs and a parking_lot read lock per event are gone (at 10M ev/s that was 20M allocs/sec). Routable subset is pre-computed at every shard-state mutation (`collect_metrics`, `scale_up`, `activate`, `drain_specific`, `scale_down`, `finalize_draining`, `remove_*_stopped_shard`). Pinned by `selection_table_reflects_active_subset_after_state_transitions` and `select_shard_does_not_acquire_shards_lock`. |
| 17 / 32 | `ThreadLocalPool::{acquire, release}` per-call retain walk → amortized over `REAP_INTERVAL` calls | Mesh | The `HashMap::retain` + `Weak::strong_count` walk that fired on every TLS access is now gated by a per-thread counter. Walks every 4096th call instead of every call — the published "thread-local is 2× slower than shared" benchmark anomaly should disappear. Leak-free invariant preserved: dead entries are reclaimed within `REAP_INTERVAL` accesses. Pinned by `dropped_thread_local_pool_evicts_tls_entry_within_reap_interval` (long-tail correctness) and `dead_tls_entry_lingers_until_amortized_reap` (amortization contract — regression catches a return-to-per-call walk). |
| 51 | `HeapSegment::read` → zero-copy `Bytes::slice` | RedEX | Internal buffer is now `Bytes` (was `Vec<u8>`); `read` returns a refcount-bumped slice instead of a fresh `Bytes::copy_from_slice`. Appends use `Bytes::try_into_mut`: O(1) when no reader holds an outstanding slice (the steady state), single-copy fallback when one does — existing slices stay valid via their own refcount. Pinned by `read_returns_zero_copy_slice_of_underlying_buffer` (compares raw `as_ptr` offsets to prove the slice shares the segment's allocation). For a 4KB-payload watcher at 100K ev/s this was 400 MB/s of pure memcpy on every materialized event. |
| 52 | `RedexFile::read_one` / `read_range` → binary search via `partition_point` | RedEX | The `index` is sorted by `seq` by construction; pre-fix both methods walked it linearly even for trivially-absent seqs. Post-fix `read_one` is one `partition_point` + bounds check (O(log N)); `read_range` is two `partition_point`s framing a tight slice over the live range. Pinned by `read_one_and_read_range_resolve_via_binary_search_semantics`. |
| 15 / 16 | Precompiled filter for filtered-poll retain | Consumer | New `CompiledFilter` + `Filter::compile()` pre-splits every dot-path and pre-parses each segment as `usize`. Caller in `consumer/merge.rs` compiles once per poll and passes `&CompiledFilter` into the retain loop; pre-fix the path-split + per-segment `parse::<usize>()` ran on every event (perf #16). For #15, the compiled filter is the doc's recommended cheapest mitigation — a path-targeted byte parser (simd-json) is a follow-up. Pinned by `compiled_filter_matches_raw_filter_semantically` (exhaustive parity vs `Filter::matches` across And/Or/Not/Eq/EqWrapped/numeric-index shapes) and `compile_caches_array_index_parse_per_segment` (proves the integer parse moved to compile time). |
| 84 | RPC body `Vec<u8>` → `Bytes` | CortEX RPC | `RpcRequestPayload` / `RpcRequestChunkPayload` / `RpcResponsePayload` `body` field is now `Bytes` (was `Vec<u8>`). All three `decode` methods take `Bytes` and use `data.slice(..)` for zero-copy body extraction; pre-fix each decoded frame ran `data[body_start..body_end].to_vec()` — one memcpy per RPC frame. The `Bytes::from(resp.body)` / `Bytes::from(payload.body)` wrappers in the streaming and reply paths become no-ops and are removed. Wide refactor — every adapter / SDK / binding / integration test that constructs these payloads or compares `body` to a byte literal — but mechanical. For high-RPS systems doing 100K+ RPCs/sec with 1 KB+ bodies that's 100+ MB/sec of memcpy saved per the perf doc. |
| 30 | `redis::serialize_event` String allocs → `write!` | Redis adapter | `event.insertion_ts.to_string().as_bytes()` and `event.shard_id.to_string().as_bytes()` patterns replaced with `write!(&mut buf, "{}", val)` via `std::io::Write`. The macro formats into the existing `Vec<u8>` using an internal stack buffer for the digit conversion — zero heap. Pre-fix that was 2 `String` allocations per event; for a 10K-event batch, 20K wasted allocs. Existing `test_serialize_event` re-validates the JSON shape. |
| 31 | `parse_xrange_response` `id.to_string()` per discarded entry | Redis adapter | Pre-fix `last_seen_id: Option<String>` was set on every iteration even though only the LAST value matters — a 10K-entry response did 9999 wasted String allocs + drops. Post-fix tracks `last_seen_idx: Option<usize>` in the loop and materializes the id once after the loop from `entries[idx]`. Existing tests (`test_poll_shard_advances_cursor_on_all_corrupt_entries`, `test_poll_shard_advances_past_trailing_corrupt_entries`) still pass; semantics unchanged. |
| 18 | `Router::route_packet` body copy → `Bytes::try_into_mut` fast path | Router | New `RoutingHeader::write_at(&self, &mut [u8])` overwrites a fixed-size header slice in place. `route_packet` tries `Bytes::try_into_mut` on the inbound packet: on success (sole-owned, the common UDP case), overwrites the first 18 bytes in place and freezes — refcount bump only. On failure (outstanding clone), falls back to the prior allocate-and-copy. Pre-fix every forwarded packet allocated a fresh buffer and full-copied the body to flip TTL — bandwidth-class waste at relay scale. Pinned by `write_at_matches_write_to_byte_for_byte` (byte-for-byte parity with `write_to`) and `write_at_panics_on_short_slice`. |
| 40 | `is_transient_error` `to_string().to_uppercase()` → `detail().starts_with` | Redis adapter | The fallback path for `Server(ResponseError)` / `Extension` errors no longer allocates two `String`s per classified error. Uses `RedisError::detail()` (`Option<&str>`, zero-alloc) and checks for the keyword as a `starts_with` prefix — Redis server errors are uppercase by protocol and always begin with the keyword. Hot in degraded-broker states (BUSY/LOADING) where every command surfaces here. Existing `is_transient_error_recognizes_cluster_recoverables` test continues to pin both typed and `NOREPLICAS` extension paths. |
| 42 | `try_acquire_tx_credit_inner` re-reads `state.epoch()` | Session | Cache `current_epoch` once at the top of the `Some(state) => { ... }` arm and reuse for both the epoch-mismatch check and the returned tuple. Trivial field access today but the cache makes "both checks see the same snapshot" obvious — a defensive read against any future change that makes `epoch` mutable under `&self`. |
| 72 | `apply_sync_response` calls `file.next_seq()` three times | RedEX replication | The non-empty path computes the post-append tail by adding `payloads.len()` to the already-captured `local_next` instead of re-locking the file via `next_seq()`. `next_seq()` takes a parking_lot mutex + atomic load; saves one lock+unlock per applied sync chunk. The empty-batch path's single `next_seq()` is unchanged. |

---

## TL;DR — Top 10 Wins (Combined Priority)

| # | Item | Subsystem | Why it matters |
|---|------|-----------|----------------|
| 1 | `in_flight_ingests` SeqCst | Core bus | Biggest multi-producer scaling cliff |
| 2 | `HeapSegment::read` copies payload | RedEX | Full memcpy on every read |
| 3 | `MemoriesWatcher` re-executes full query per event | CortEX | N × M × K cost per second |
| 4 | `dispatch_batch.clone` → `Arc<Batch>` | Core bus | Wasted clone on every batch dispatch |
| 5 | `MemoriesQuery` linear scan, no indices | CortEX | Read-path scaling cliff |
| 6 | `to_lowercase()` per memory in content search | CortEX | Allocation amplification |
| 7 | `handle_sync_request` over-reads entire range | RedEX | 10-100× waste on catchup |
| 8 | RPC body `Vec<u8>` copy on decode | CortEX RPC | Full memcpy per RPC frame |
| 9 | `ThreadLocalPool` retain on every call | Mesh | Explains benchmark anomaly |
| 10 | `read_one` / `read_range` linear scan | RedEX | O(N) → O(log N) on every read |

The combined effect of just these 10 would be transformative for the published benchmark profile.

---

## Cross-Cutting Patterns

Themes that recur across subsystems. Fixing the underlying pattern produces compounding wins:

1. **`SeqCst` where weaker orderings work.** `in_flight_ingests`, `FfiOpGuard::active_ops`, shutdown flags. Audit all `Ordering::SeqCst` and downgrade to `AcqRel` + targeted fences.

2. **`.to_string()` to get bytes from primitives.** `serialize_event`, `parse_xrange_response`, every `id.to_string()`. A workspace-wide grep for `to_string().as_bytes()` catches most.

3. **Allocating a Vec to forward to a method that consumes it.** `events.into_iter().map(...).collect()` patterns, `dispatch_batch.clone()` on every retry, `add_events(vec![])` for timeout signaling. Prefer iterators or `Arc<T>` over owned Vec where the consumer just reads.

4. **`Bytes::copy_from_slice` where slice/refcount would do.** `segment.read`, `materialize`, watcher delivery, router forwarding. The whole point of `Bytes` is zero-copy.

5. **Per-call retain/sweep on hot paths.** `ThreadLocalPool::{acquire,release}` doing weak-ref reaps per call. Amortize over N calls or background.

6. **`Instant::now()` on every packet/event.** Even vdso-cheap is ~10ns × packet rate. Centralize timekeeping with a coarse clock ticker.

7. **Atomics where SPSC contract makes them unnecessary.** Per-shard counters that only the shard's producer touches don't need atomic ops — `Cell<u64>` + occasional snapshot.

8. **Linear scan where binary search applies.** `read_one`, `read_range`, `replica_set.contains`, retention size walk. The data is sorted; the search algorithm isn't.

9. **Eager materialization of full result then trim.** `handle_sync_request` reads everything then caps to budget. Stream + early stop instead.

10. **Sub-optimal syscall counts.** 3 metadata calls per append for rollback, 3 write_all instead of write_vectored, separate mutexes for files always taken together.

11. **`Vec<u8>` where `Bytes` would zero-copy.** RPC payloads carried as `Vec<u8>` through layers that have `Bytes` on entry.

12. **Full state scan with no secondary indices.** CortEX queries walk all memories/tasks on every call. Inverted indices on tag/source/id would be O(matching) instead of O(state size).

13. **Eager clone on read.** `cloned()` on every matched memory/task. Arc-wrapping inner records eliminates this with COW on write.

14. **Watch streams re-execute on every event.** No delta information, no selectivity gate. The fold knows what changed but doesn't tell the watcher.

---

## Section 1: Core Event Bus

### 🔴 High-impact

#### 1. `try_enter_ingest` does two SeqCst RMWs per event on a globally shared atomic

**Location:** `bus.rs:910` and `IngestGuard::drop`.

Every single `ingest()` does:
```rust
self.in_flight_ingests.fetch_add(1, SeqCst);   // entry
// ... ingest work ...
self.in_flight_ingests.fetch_sub(1, SeqCst);   // drop
```

`SeqCst` on a contended counter is the worst-case scaling pattern — every producer core has to flush its store buffer and acquire exclusive cache-line ownership of the same line, twice. With N producer threads at high rate this collapses to roughly single-thread throughput regardless of how many cores you throw at it. At a target of 10M ev/s this is almost certainly the single biggest bottleneck on multi-producer ingestion.

The actual ordering need is just "producers entering after `shutdown=true` is set must not start; producers in-flight must be visible to the shutdown waiter."

**Fix options:**
- **Cheapest:** sharded/striped counters. Hash producer thread (or use `[CachePadded<AtomicU64>; N]` indexed by `thread_id % N`). Shutdown sums across them. Zero false-sharing, zero contention in steady state.
- **Even better:** epoch-based reclamation (`crossbeam-epoch`), where producers pin an epoch (thread-local, ~1ns) instead of bumping a global counter.
- **Minimum-change:** drop SeqCst to `AcqRel` on fetch_add, use a `fence(SeqCst)` between fetch_add and shutdown load, plus `Release`/`Acquire` on the shutdown CAS. x86 still emits mfence so the win is mostly on ARM.

**Expected impact:** 2-10× on multi-producer ingest throughput depending on producer count.

#### 2. `Mapper::select_shard` allocates two `Vec`s and takes a read lock — per event

**Location:** `shard/mapper.rs:554`. In dynamic-scaling mode this runs on every ingest:

```rust
let shards = self.shards.read();                          // RwLock acquire
let active: Vec<_> = shards.iter().filter(...).collect(); // heap alloc #1
let min_weight = active.iter().map(...).fold(...);
let candidates: Vec<_> = active.iter().filter(...).collect(); // heap alloc #2
```

At 10M ev/s that's 20M allocations/sec plus a parking_lot RwLock acquire each time.

**Fix:** Pre-compute on weight update, not on every select. When `collect_metrics` updates weights, snapshot `(min_weight, candidate_ids: SmallVec<[u16; 8]>)` into an `ArcSwap<SelectionTable>`. Hot path becomes `arc_swap.load() + Lemire-mod`. Zero alloc, zero lock.

#### 3. `RingBuffer::try_push` / `try_pop` re-deref with bounds checks

**Location:** `shard/ring_buffer.rs:254, 321, 358, 421`.

```rust
let index = (head & self.mask as u64) as usize;
unsafe { (*self.buffer[index].get()).write(value); }
```

`self.buffer[index]` is a `Box<[T]>` index op = runtime bounds check. The mask guarantees `index < capacity == buffer.len()`, but the compiler may not prove it across the load+mask. Use `get_unchecked` (already in unsafe context, contract already satisfied).

**Expected impact:** ~5-15% on the ring hot path. Small in absolute ns, but the published benches put push at ~18ns so the ratio matters.

#### 4. `ingest_raw_batch` allocates a `Vec<Vec<Bytes>>` per call, sized to shard count

**Location:** `shard/mod.rs:644`:

```rust
let mut groups: Vec<Vec<Bytes>> = (0..table.shards.len()).map(|_| Vec::new()).collect();
let mut group_ids: Vec<u16> = vec![0; groups.len()];
```

Plus a `Vec<Bytes>` per shard that grows as events route there. Then `for (idx, group) in groups.into_iter()` drops every empty Vec. On every batch call.

**Fix options:**
- **Single-pass push-while-grouping:** acquire each shard lock the first time you see a routing decision for it, hold across all events for that shard. Trade-off: lock hold time.
- **Reusable scratch:** thread-local keyed by shard count, clear on entry, reuse capacity.
- **SmallVec:** `SmallVec<[Bytes; 8]>` per bucket — most batches in most shards fit inline.

Also at `bus.rs:994`: `events.into_iter().map(|e| e.into_raw()).collect()` allocates the full intermediate `Vec<RawEvent>` just to forward. Pass an iterator instead.

### 🟡 Medium-impact

#### 5. `RawEvent::bytes()` clones unconditionally; pushed bytes are then cloned again on `DropOldest`

**Location:** `event.rs:180` + `shard/mod.rs:513` (`shard.try_push_raw(raw.clone())` on every push when `DropOldest` is set — even on the success path).

**Fix:** Restructure to clone only on the retry branch. Use `is_full()` check (exact under SPSC since producer holds the shard lock) to pre-evict.

#### 6. `TimestampGenerator::next` does TSC read + CAS even per-shard

**Location:** `timestamp.rs:100`. With per-shard generators, contention should be zero (single producer per shard), but the code uses a contended CAS loop. `lock cmpxchg` on x86 is ~25ns.

**Fix:** Add a single-threaded variant for the SPSC case:
```rust
#[inline(always)]
pub fn next_st(&self) -> u64 {
    let raw = self.clock.raw();
    let now = self.clock.delta_as_nanos(self.baseline_raw, raw);
    let last = self.last.load(Relaxed);
    let ts = now.max(last + 1);
    self.last.store(ts, Relaxed);
    ts
}
```

Published benches show 6-12ns/call; this could realistically be 2-4ns. At 10M ev/s that's 40-80ms of CPU/sec saved per producer.

#### 7. `select_shard_by_hash`: `hash % num_shards` for static mode

**Location:** `shard/mod.rs:488`. Modulo by a non-constant `u64` is ~20-25 cycles.

**Fix:** Use Lemire's reduction (already used elsewhere in the codebase at mapper.rs:610): `((hash as u128 * n as u128) >> 64) as u16`. Multiply-shift is faster than `div` on every modern uarch and unbiased.

#### 8. `PollMerger::poll` allocates several HashMaps + HashSets per poll

**Location:** `consumer/merge.rs:622, 763, 804, 854`:
```rust
let mut format_refused_shards: HashSet<u16> = HashSet::new();
let mut matched_per_shard: HashMap<u16, usize> = HashMap::new();
let mut rolled_back: HashSet<u16> = HashSet::new();
let mut seen_shards: HashSet<u16> = HashSet::with_capacity(shards.len());
```

For typical shard counts (4-64), a `SmallVec<[u16; 16]>` + linear scan is faster than HashMap. Or for `seen_shards` specifically, a `u64` bitmask indexed by shard_id if shard ids are bounded.

### 🟢 Low-impact / cleanup

#### 9. `pop_batch` returns `Vec<T>` (forces alloc); `_into` variant is the actual hot path

**Location:** `shard/ring_buffer.rs:336`. Deprecate or `#[doc(hidden)]` the allocating variant.

#### 10. `is_full()` uses `len()` which does 2 Acquire loads

**Location:** `shard/ring_buffer.rs:451`. Producer already holds `head` exclusively (SPSC), so `head` only needs `Relaxed` on the producer side. A `try_push_fast` returning the new length could let `Shard::try_push_raw` skip the second `ring_buffer.len()` call.

#### 11. Debug-only `InProgressGuard` fires AtomicBool RMWs

**Location:** `shard/ring_buffer.rs:235, 309, 338, 404`. Cfg-gated, but note that benchmarks must use release builds.

#### 12. `events.into_iter().map(|e| e.into_raw()).collect()` in `ingest_batch`

**Location:** `bus.rs:994`. Pure forwarding; the intermediate `Vec` is wasted.

#### 13. `dispatch_batch` clones the entire `Batch` on every retry attempt, even the first

**Location:** `bus.rs:2126`:
```rust
for attempt in 0..retries {
    match tokio::time::timeout(timeout, adapter.on_batch(batch.clone())).await {
        ...
    }
}
// Final attempt moves:
match tokio::time::timeout(timeout, adapter.on_batch(batch)).await { ... }
```

Comment claims "the final attempt moves it, saving one clone per dispatch." But this clones the batch on the very first attempt even though most batches succeed on attempt 0. For a 1000-event batch that's 1000 Bytes refcount bumps + one Vec alloc per dispatch, almost always wasted.

**Fix:** **`Arc<Batch>` instead of `Batch`** — change `Adapter::on_batch(&self, batch: Arc<Batch>)`. Retries are then a single Arc clone. The mesh adapter (`send_to_peer(peer_addr, batch)`) genuinely consumes — it can `Arc::try_unwrap` on first try.

#### 14. Drain → batch_worker pipeline allocates a Vec per cycle then *immediately discards it*

**Location:** `bus.rs:2492` (drain) → `shard/batch.rs:316` (worker `extend`).

Per pipeline cycle: drain allocates fresh `Vec<InternalEvent>(cap 1000)`, fills it, sends down mpsc, batch worker copies into `current_batch`, source Vec deallocates.

**Fix options:**
- Vec recycling channel: second mpsc back from worker → drain that returns empty Vecs.
- `mem::swap` into `current_batch` when current_batch is empty.
- Have the worker own the scratch; drain calls a function/`&mut Vec`.

#### 23. `BatchWorker` `tokio::time::timeout` registers a fresh timer per cycle

**Location:** `bus.rs:2258`. Each `timeout(recv_timeout, rx.recv()).await` registers + cancels a timer. A `select!` with a long-lived `Pin<&mut Sleep>` that you `reset(new_deadline)` avoids re-registration.

#### 24. `select_shard` (the `JsonValue` variant) serializes the value just to hash it

**Location:** `shard/mod.rs:464`. Bytes discarded after hashing. Tree-walking hash on `JsonValue` directly avoids serialization. Less important: anything serious uses `ingest_raw`.

#### 25. `Batch::clone` is `#[derive(Clone)]` so deep-clones the events Vec

Already covered by #13's `Arc<Batch>` fix. Listed for completeness.

---

## Section 2: Consumer / Merge / Filter

### 🔴 High-impact

#### 15. `event_matches_filter` parses every event's full JSON to check a single path

**Location:** `consumer/merge.rs:477` + `consumer/filter.rs:97-114`.

```rust
fn event_matches_filter(event: &StoredEvent, filter: &Filter) -> bool {
    match event.parse() {              // <-- full serde_json::from_slice
        Ok(value) => filter.matches(&value),  // then JSON tree walk
        Err(_) => false,
    }
}
```

For a filter `{path: "user.role", value: "admin"}`, you parse the entire JSON document, build the full `serde_json::Value` tree, walk down "user" → "role", compare, and drop the entire parsed tree.

Over-fetch factor is 3 when filter is set, so for every event returned, ~3 are filtered out. At a 10K-event response, that's 30K JSON parses.

**Fix options:**
- **Path-targeted byte parser** like `simd-json` or `serde_json_path`. Even rolling a tiny `find_path_value` over raw bytes is dramatically faster for shallow paths.
- **Cache parsed values** for events that pass the filter so they don't get parsed again downstream.
- **Precompiled filter** (#16 below).

#### 16. `Filter::matches` re-splits the path on `'.'` for every event

**Location:** `consumer/filter.rs:151`:
```rust
for segment in path.split('.') {
    ...
    let idx: usize = segment.parse().ok()?;  // re-parsed every time too
    ...
}
```

Filter constructed once per poll, applied to N events. Path split + speculative integer parse happens N times.

**Fix:**
```rust
enum CompiledSegment { Field(String), Index(usize) }
struct CompiledEq {
    segments: Vec<CompiledSegment>,
    value: JsonValue,
}
```

Compile once at filter construction (or first `matches` call via OnceCell), reuse N times.

### 🟢 Low-impact / cleanup

#### 41. `Filter::matches` lacks short-circuit specialization for common shapes

For long `$and` chains, ordering of conditions matters. Production usage: callers can manually order filters by selectivity, or sort the `filters` vec at construction by cheap heuristic (path length, value type) so highly-selective filters fire first.

#### 48. `compare_stream_ids` tries multiple format parses per call

**Location:** `consumer/merge.rs:49`. Only used as third tiebreaker in sort, mostly dead in practice. If you ever hit a contended sort path (many events with same `insertion_ts` + `shard_id`), the per-comparison `split_once` + parse adds up.

---

## Section 3: Adapters (Redis / JetStream / Mesh)

### 🔴 High-impact

#### 29. `redis::Value` tree is fully allocated before adapter parses it

**Location:** `adapter/redis.rs:213`.

For a 10K-event XRANGE response with ~6 fields per event:
- 1 `Vec<Value>` for entries
- 10K `Vec<Value>` for each entry's [id, fields]
- 10K `Vec<Value>` for the fields array
- 60K `Value::BulkString(Vec<u8>)` for field names + values

That's ~80K allocations and a massive owned tree, **before** `parse_xrange_response` even runs.

**Fix:** Use redis-rs's `FromRedisValue` to deserialize directly into your target type, skipping the intermediate `Value` tree. The typed `xrange` helper is the path of least resistance. Eliminates ~80% of allocations on the read hot path.

#### 30. `redis::serialize_event` allocates Strings just to get bytes from u64/u16

**Location:** `adapter/redis.rs:144-146` (and `jetstream.rs:106-108`):
```rust
buf.extend_from_slice(event.insertion_ts.to_string().as_bytes());
// ...
buf.extend_from_slice(event.shard_id.to_string().as_bytes());
```

Per event written, two `String` allocations + drop. For a 10K-event batch, 20K wasted allocations.

**Fix** with `itoa::Buffer` (stack buffer, zero alloc) or `write!(buf, ...)`. Same code copy-pasted in jetstream — factor into shared helper.

#### 31. `parse_xrange_response`: `id.to_string()` per entry, even for entries that get discarded

**Location:** `adapter/redis.rs:237`. Only the **last** `last_seen_id` value matters. So 9999 of 10000 String allocations per poll are immediately thrown away.

**Fix:**
```rust
let mut last_seen_idx: Option<usize> = None;
for (idx, entry) in entries.iter().take(limit).enumerate() {
    // ... use id by reference ...
    last_seen_idx = Some(idx);
}
let last_seen_id = last_seen_idx.and_then(|i| extract_id(&entries[i]));
```

### 🟡 Medium-impact

#### 18. `Router::route_packet` copies the entire packet body to mutate a 28-byte header

**Location:** `adapter/net/router.rs:535`:
```rust
let mut new_data = BytesMut::with_capacity(data.len());
let mut fwd_header = routing_header;
fwd_header.forward();
fwd_header.write_to(&mut new_data);
new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);
```

Forwarding any packet allocates a fresh buffer and full-copies the body to flip TTL and rewrite a header. For a relay node moving GB/s of packets, bandwidth-class waste.

**Fix options:**
- **`Bytes::try_into_mut` fast path.** If refcount is 1 (very likely for UDP packets just received), in-place mutate.
- **Vectored send.** Send `[new_header_bytes, &data[ROUTING_HEADER_SIZE..]]` as two slices via `sendmsg`.
- **Per-thread BytesMut pool** for forward buffers.

#### 33. `Router::lookup` calls `Instant::elapsed()` per packet to check route freshness

**Location:** `adapter/net/route.rs:538`. Called per packet routed. At 10M pps that's 100ms of CPU/sec spent reading the clock just for staleness checks on routes that change once per heartbeat (1s+).

**Fix:**
- **Sweep-based freshness.** If sweep is frequent enough, anything in the map is fresh.
- **Coarse clock** updated every N packets or by background ticker.
- **TTL in seconds** as u32, compared with `>=`.

#### 34. `redis::Value::BulkString` field-name match has 4 arms per field type

**Location:** `adapter/redis.rs:252-280`. Per event, the loop iterates over every field doing 4 branch evaluations.

**Fix:** When issuing XRANGE, request fields explicitly, or trust the producer's field ordering and access by index. Cache field order indices on first call.

#### 21. `dispatch_packet` in mesh allocates a `String::with_capacity(24)` + write! per delivered event

**Location:** `adapter/net/mesh.rs:4272`:
```rust
for (i, event_data) in events.into_iter().enumerate() {
    let mut event_id = String::with_capacity(24);
    let _ = write!(event_id, "{}:{}", seq, i);
    queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
}
```

One String alloc per event end-to-end from network.

**Fix options:**
- `smallstr::SmallString<[u8; 24]>` — 24-byte ids stay on stack.
- Defer id materialization: store `(seq, i)` as `(u64, u32)`, format only when consumer reads `event.id`.
- Pack into single u64: `(seq << 16) | i` — change `StoredEvent::id` to allow numeric ids.

### 🟢 Low-impact / cleanup

#### 22. `total_pending_in_rings` acquires every shard lock just to sum lengths

**Location:** `shard/mod.rs:730`. The Mutex is unnecessary — Acquire loads give a consistent snapshot. Lock blocks producers/consumers.

#### 39. `try_join_all` in jetstream `on_batch` allocates a Vec for futures + results

**Location:** `adapter/jetstream.rs:473`. For very large batches matters; for typical 100-1000 event batches, less so.

#### 40. `redis::is_transient_error` allocates + uppercases the error message on every error

**Location:** `adapter/redis.rs:628`:
```rust
let msg = e.to_string().to_uppercase();
msg.contains("LOADING") || msg.contains("BUSY") || ... (9 substring searches)
```

Error path, cold. But: in a degraded state (BUSY/LOADING), every failure goes through here.

**Fix:** Byte-level prefix match (Redis errors start with the keyword): `msg.starts_with(b"LOADING ")` etc., no uppercase needed.

#### 42. Session `try_acquire_tx_credit_inner` re-reads `state.epoch()` after the check

**Location:** `adapter/net/session.rs:283`. Three accesses through the `Ref`; `epoch` was already loaded above. Cache it once.

#### 43. `redis::stream_key` uses RwLock with format!-per-miss

**Location:** `adapter/redis.rs:117`. Fast path fine. If shard ids are known at startup, pre-fill the cache.

#### 44. `redis::poll_shard`'s `start = format!("({}", id)` allocates per poll

**Location:** `adapter/redis.rs:520`. Once per poll, low impact. Stack-buffered formatter.

#### 46. `mesh::dispatch_packet` checks `data.len()` then does `data[0]` (bounds-checked) multiple times

**Location:** `adapter/net/mesh.rs:2789, 2806`. The length check is correct but the subsequent indexing isn't elided in all branches. Worth measuring before changing.

#### 47. `reliability::on_send` calls `Instant::now()` per packet sent

**Location:** `adapter/net/reliability.rs:360`. Same pattern as #33.

---

## Section 4: ThreadLocalPool / PacketPool

### 🔴 High-impact

#### 17. `ThreadLocalPool::acquire` does a full HashMap walk + atomic load per entry, every call

**Location:** `adapter/net/pool.rs:721`. **This is the smoking gun for "thread-local is 2× slower than shared" in the published benchmarks (82ns vs 38ns):**

```rust
pub fn acquire(&self) -> PacketBuilder {
    LOCAL_BUILDERS.with(|pools| {
        let mut pools = pools.borrow_mut();
        pools.retain(|_, (weak, _)| weak.strong_count() > 0);  // <-- HashMap walk + atomic per entry
        let entry = pools.entry(self.pool_id).or_insert_with(...);  // <-- HashMap lookup
        // ... then the actual work
    })
}
```

The retain walks every entry in the per-thread HashMap on every acquire, calling `Weak::strong_count` (atomic load) on each. HashMap entry lookup adds a hash + probe. By the time you do the actual local pop, you've spent more cycles than a single ArrayQueue CAS.

Compare to `PacketPool::get`: just one `ArrayQueue::pop()` (a single CAS) → done.

The shared pool wins because thread-local's overhead (retain + HashMap entry lookup + RefCell borrow) > the cost of a single ArrayQueue CAS.

**Fix:**
- **Reap periodically, not per-call.** Per-thread counter; reap every 4096 calls (or on `release`, which is colder).
- **Avoid HashMap for the common case.** Most threads talk to one pool at a time. Use `Cell<Option<(u64, Vec<...>)>>` directly in TLS as fast-path cache.
- **Skip the Weak/Arc liveness scheme entirely.** Pools could be `'static` (or hold a Drop hook that explicitly clears TLS entries via an inventory pattern).

If you fix this, thread-local should comfortably beat shared (zero atomics on hot path), which is the whole point of TLS pooling.

#### 32. `ThreadLocalPool::release` ALSO does the per-call retain walk + HashMap lookup

**Location:** `adapter/net/pool.rs:780`. Mirror of #17 — same pattern on release/drop path. Every full cycle (acquire + release) pays the cost twice. Fix from #17 needs to apply to both methods.

---

## Section 5: Metrics Collector

### 🟡 Medium-impact

#### 35. `ShardMetricsCollector::record_push` does 3 atomics + a CAS loop per event

**Location:** `shard/mapper.rs:170`. When metrics are enabled (i.e., dynamic scaling), every event push pays:
- `events_in_window.fetch_add(1, Relaxed)` — atomic RMW
- `push_latency.fetch_update(...)` — **CAS loop** with pack/unpack
- `pushes_since_drain_start.fetch_add(1, Relaxed)` — atomic RMW

Plus `Instant::now()` + `.elapsed()` in `Shard::try_push_raw`. Enabling dynamic scaling roughly doubles the per-event cost.

Producer side is single-threaded by SPSC contract. Atomics exist only because the metrics ticker reads them.

**Fix:** Producer maintains plain `u64` counters in shard-local memory. Metrics ticker reads with `Relaxed` load (incoherent OK), or under the shard mutex (which it already takes). Saves 3 atomic RMWs per event ingest when scaling is on.

#### 36. `record_push`'s `events_in_window` increments per event rather than per batch

**Location:** `shard/mapper.rs:171`. Drain worker could `record_drained(batch.len())` once per cycle. Cuts atomics from per-event to per-batch (1000:1 reduction at default batch size).

#### 37. `AdaptiveBatcher::record_events` does VecDeque push + time-window eviction per call

**Location:** `shard/batch.rs:60`. Called per drain cycle. The deque calculation only uses front and back.

**Fix:**
```rust
struct VelocitySample {
    window_start: Instant,
    events_at_window_start: u64,
}
// Update window_start lazily when recalculating.
```

Removes VecDeque entirely. Two `u64`s + one `Instant`.

#### 38. `BatchWorker::add_events(vec![])` allocates an empty Vec to signal timeout

**Location:** `bus.rs:2311`. Cleaner: `worker.check_timeout_flush()` direct call. Removes documented footgun.

---

## Section 6: FFI

### 🟡 Medium-impact

#### 19. FFI `enter_ffi_op` has the same SeqCst contention pattern as bus #1

**Location:** `ffi/mod.rs:340`. Every FFI ingest call goes through this AND the bus-level `try_enter_ingest`. Two layers of SeqCst contention per ingest from FFI callers (most production users — Python, Go, Node SDKs).

Same fix as #1: AcqRel + fence, or sharded counters.

#### 20. `net_ingest_raw` does UTF-8 validation, then byte copy, then hash

**Location:** `ffi/mod.rs:822`:
```rust
let json_str = match std::str::from_utf8(json_bytes) { ... };
let raw = RawEvent::from_str(json_str);
```

`RawEvent::from_str` does `Bytes::copy_from_slice(s.as_bytes())`. Three passes over the data per FFI ingest:
1. UTF-8 validation
2. Memory copy
3. xxh3 hash

JSON validation isn't enforced anyway. **Fix:** Replace with:
```rust
let json_bytes = unsafe { std::slice::from_raw_parts(json as *const u8, len) };
let raw = RawEvent::from_bytes(Bytes::copy_from_slice(json_bytes));
```

For 4KB events that's ~4KB of memory bandwidth × ingest rate saved.

---

## Section 7: RedEX

### 🔴 High-impact

#### 51. `HeapSegment::read` copies the payload on every read

**Location:** `adapter/net/redex/segment.rs:105`:
```rust
pub fn read(&self, offset: u64, len: u32) -> Option<Bytes> {
    Some(Bytes::copy_from_slice(&self.buf[rel..end]))
}
```

The whole point of `Bytes` is zero-copy slicing of a shared buffer. Here the segment owns a `Vec<u8>` and copies a slice on every read. For every event materialized through `RedexFile::tail`, `read_range`, `read_one`, replication shipping, watcher delivery — the payload gets a full memcpy.

For a watcher subscribed to a file with 4KB payloads at 100K events/sec: **400MB/sec of pure memory bandwidth wasted on the copy.**

**Fix:** Make `HeapSegment::buf` a `Bytes` (or hold a `BytesMut` for appends and convert to `Bytes` on demand). Then `read` becomes `self.buf.slice(rel..end)` — refcount bump only. When eviction compacts the head, allocate a new `Bytes` for the new live range; existing slices keep their portion alive via refcount.

**Hands-down the biggest single win in the entire RedEX subsystem on the read hot path.**

#### 52. `RedexFile::read_one` and `read_range` do linear scans instead of binary search

**Location:** `adapter/net/redex/file.rs:1069-1105`:
```rust
pub fn read_one(&self, seq: u64) -> Option<RedexEvent> {
    let state = self.inner.state.lock();
    for entry in state.index.iter() {           // <-- O(N) walk
        if entry.seq < seq { continue; }
        if entry.seq > seq { break; }
        return materialize(entry, &state.segment);
    }
    None
}
```

The index is `Vec<RedexEntry>` and sorted by seq by construction. The neighboring `tail()` method correctly uses `state.index.partition_point(|e| e.seq < from_seq)`. `read_one` and `read_range` don't.

For a file with 1M retained entries:
- `read_one`: 1M comparisons + linear cache scan (~20MB)
- `read_range(start, end)`: same scan to find `start`, then scan to `end`

**Fix:**
```rust
let lo = state.index.partition_point(|e| e.seq < seq);
// O(1) check: index[lo].seq == seq → materialize, else None
```

Even better: since `seq` is dense (`lowest_retained_seq + i`), compute directly: `index[(seq - lowest_retained_seq) as usize]` for O(1).

#### 53. `handle_sync_request` over-reads the entire range before applying the byte budget

**Location:** `adapter/net/redex/replication_catchup.rs:238`:
```rust
let events = file.read_range(request.since_seq, local_next);
// ... then loop with byte budget and break early
```

`read_range` materializes ALL events from `since_seq` to `local_next`, then the loop reads only `effective_budget` bytes worth and drops the rest. For a replica behind by 1M events with a 64MB budget holding ~10K events, you materialize 100× more than you ship.

Each materialized event allocates `Bytes::copy_from_slice` (#51) + computes `xxh3_64` checksum (#57). For 1M-event over-read with 4KB avg payload: ~4GB of allocations + hashing.

**Fix:** Pass byte budget into `read_range`, or expose a streaming iterator:
```rust
fn read_range_until<F: FnMut(&RedexEvent) -> bool>(&self, start: u64, end: u64, mut take: F) -> Vec<RedexEvent>;
// or
fn iter_range(&self, start: u64, end: u64) -> impl Iterator<Item = RedexEvent> + '_;
```

Single biggest perf win on the replication catchup path. Combined with #51 and #52 it's transformative.

#### 54. `notify_watchers` clones the event and iterates all watchers per event in a batch

**Location:** `adapter/net/redex/file.rs:1544` + batch loop at `file.rs:696`:
```rust
// in append_batch:
for event in &events {
    notify_watchers(&mut state.watchers, event);
}

// in notify_watchers:
watchers.retain(|w| {
    // ... try_send(Ok(event.clone())) ...
});
```

For a batch of B events with W watchers:
- B × W `try_send` calls
- B × W `event.clone()` calls
- B `retain` walks over the watcher list (O(W) each), mutating the Vec

Worse: each `retain` call holds the state lock the entire time. Producers can't append.

**Fix:**
- Hoist dead-watcher filter outside the batch loop: do `retain(|w| w.sender.is_open())` once before, then plain `for-each` inside.
- For each watcher, ship whole batch in one `try_send` of `Arc<[RedexEvent]>` — one channel push per watcher instead of B.
- Or use a broadcast channel where one send fans out to N receivers.

At 10 watchers × 1000-event batch, this is a 10K → 10 reduction in channel operations.

#### 55. `append_batch` allocates `vec![ts; pairs.len()]` to give the disk path identical timestamps

**Location:** `adapter/net/redex/file.rs:676`:
```rust
if let Err(e) = disk.append_entries_at(&pairs, &vec![ts; pairs.len()]) { ... }
```

Every batch append allocates a Vec of `payloads.len()` identical timestamps. For 1000-event batch that's an 8KB Vec just to pass the same `u64` repeated.

**Fix:** Add `disk.append_entries_at_uniform(&pairs, ts)`.

#### 56. `append_batch` builds a `Vec<RedexEvent>` eagerly for both disk and memory commit

**Location:** `adapter/net/redex/file.rs:649-666`. Even without watchers, the `events` Vec is built. Restructure to compute entries only (no payload clone), then iterate entries + payloads in lockstep for disk write, iterate entries for memory commit, build `RedexEvent` lazily only when watchers exist.

#### 57. `materialize` recomputes the payload checksum on every read

**Location:** `adapter/net/redex/file.rs:1527`:
```rust
let computed = super::entry::payload_checksum(&payload);
if stored != computed { ... }
```

`xxh3_64` over the full payload on every materialize. For tail subscribers + replication catchup + reads, the same in-memory payload gets hashed over and over. The checksum exists to detect on-disk corruption — defensive against bit rot, not against in-memory corruption.

For a 4KB payload at GB/s scan rates, this is a real cost.

**Fix:**
- Skip the check when reading from the heap segment (data hasn't left RAM since write).
- Verify only on disk-read path during recovery / cold reads.
- Per-segment "verified" flag; first materialize after load verifies, subsequent skip.
- Make verification opt-in via config.

#### 58. `RedexIndex::get` clones the entire HashSet

**Location:** `adapter/net/redex/index.rs:294`:
```rust
pub fn get(&self, key: &K) -> Option<HashSet<V>> {
    self.inner.get(key).map(|e| e.value().clone())
}
```

For an index that fans out wide (1 key → many values, common for inverted indices), every `get` does a full HashSet clone. For a key with 10K values, that's 10K hash inserts + a fresh hash table allocation per read.

**Fix:** Store `Arc<HashSet<V>>` in the DashMap. Update path: `entry.replace(Arc::new(new_set))`. Read path: `entry.value().clone()` is one atomic refcount bump. Trade-off: updates become copy-on-write — fine when reads >> writes.

#### 59. `disk.append_entry_inner` does 3 separate `metadata()` syscalls per append for rollback

**Location:** `adapter/net/redex/disk.rs:786`. Per single (non-batched) append:
- `dat.metadata()` to get pre_len → 1 syscall
- `idx.metadata()` to get pre_len → 1 syscall
- `ts.metadata()` to get pre_len → 1 syscall
- Plus 3 `write_all` (3 more syscalls) + 3 mutex lock pairs

**Fix:** Cache the file length in `DiskSegment`, update on every write, use cached value for rollback. Drops 3 metadata syscalls to 0.

### 🟡 Medium-impact

#### 60. `disk.append_entries_inner` could use vectored writes (`pwritev`) instead of buffer assembly

**Location:** `adapter/net/redex/disk.rs:1027-1042`. Current path: three Vec assemblies, each doing a full memcpy of all the data. Then one `write_all` per file.

`write_vectored` accepts `&[IoSlice]` and lets the kernel do the gather. Pass N 20-byte index slices directly without assembling.

For 10K-event batch, that's 200KB of memcpy gone per write.

#### 61. RedEX disk uses 3 separate `Mutex<File>` instead of one `Mutex<TripleFile>`

**Location:** `adapter/net/redex/disk.rs`. Every disk append takes 3 separate Mutex acquisitions (dat, idx, ts) always taken in same order. Combine into one `Mutex<DiskFiles>`. Saves 4 atomic ops per append.

#### 62. `disk::read_index` reads the entire file into a Vec then iterates byte chunks

**Location:** `adapter/net/redex/disk.rs:2096`. For a 1GB index file (50M × 20 bytes), allocates 1GB upfront, then iterates. Recovery scales linearly with file size.

**Fix:** mmap the file (read-only) and decode `from_bytes` directly. Or stream-read in chunks.

#### 63. `disk::read_payload` reads the entire dat file at recovery

**Location:** `adapter/net/redex/disk.rs:2150`. Same `read_to_end` pattern for the payload file, which can be up to 3GB live. mmap the file and create a `Bytes` over the mapped region.

#### 64. `apply_sync_response` allocates `Vec<Bytes>` then clones each payload from `Vec<u8>`

**Location:** `adapter/net/redex/replication_catchup.rs:407`:
```rust
let payloads: Vec<Bytes> = response
    .events
    .iter()
    .map(|e| Bytes::from(e.payload.clone()))
    .collect();
file.append_batch(&payloads)?;
```

`Vec<u8>::clone()` allocates fresh bytes. Net: one allocation per event purely because `SyncEvent::payload` is `Vec<u8>` rather than `Bytes`.

**Fix:** Change `SyncEvent::payload: Vec<u8>` → `Bytes`. Change `append_batch(&[Bytes])` → `append_batch(impl IntoIterator<Item=Bytes>)`.

#### 65. `RedexIndex::project` returns `Vec<IndexOp>` per event

**Location:** `adapter/net/redex/index.rs:124`. For an index where each event produces 0 or 1 op, a heap allocation per event purely to wrap a single op.

**Fix:** `F: Fn(&T) -> impl Iterator<Item = IndexOp<K, V>>` or `SmallVec<[IndexOp; 2]>`.

#### 66. `now_ns()` calls `SystemTime::now()` per single append

**Location:** `adapter/net/redex/file.rs:1585`. `append_batch` correctly hoists outside the loop. Single-append paths each pay it per call. A coarse timestamp (updated by 1ms ticker) would be fine for retention purposes.

#### 67. `ReplicationMetricsRegistry::for_channel` calls `channels.len()` per slow-path insert

**Location:** `adapter/net/redex/replication_metrics.rs:255`. `DashMap::len()` walks every shard.

**Fix:** Maintain `channel_count: AtomicUsize` next to the DashMap. Increment on real insert, decrement on remove.

#### 68. `replication::on_inbound` does `replica_set.contains(&from)` per inbound packet

**Location:** `adapter/net/redex/replication_runtime.rs:1098`. Linear scan per inbound packet. For typical 3-5 replica deployments, fine. For 50+, scan dominates.

**Fix:** `HashSet<NodeId>` or sorted Vec with binary search.

#### 69. `compute_eviction_count` walks all entries for size + age policies

**Location:** `adapter/net/redex/retention.rs`. Both O(N) per sweep. For a file with 1M entries swept every second, that's 1M comparisons/sec for retention alone.

**Fix:**
- Age: timestamps are roughly monotonic; use `partition_point` to find cutoff in O(log N).
- Size: maintain a running total in `RedexFile` state (updated on append + eviction). Eviction count becomes O(log N) or O(1).

#### 70. `RedexEntry::to_bytes` / `from_bytes` aren't `#[inline]`

**Location:** `adapter/net/redex/entry.rs:145, 159`. Called in inner loop of disk read/write paths. Mark `#[inline(always)]`.

### 🟢 Low-impact / cleanup

#### 71. `clear_leader_belief_and_tokens` takes the tracker lock twice in a row

**Location:** `adapter/net/redex/replication_runtime.rs:819-820`. Merge into one lock.

#### 72. `apply_sync_response` calls `file.next_seq()` three times

**Location:** `adapter/net/redex/replication_catchup.rs:350, 382, 436`. Three atomic loads where one would do.

#### 73. `file.rs` materialize allocates a fresh `Bytes::copy_from_slice` for inline payloads

**Location:** `adapter/net/redex/file.rs:1521`. For 8-byte inline payloads, allocates a fresh Bytes. Fix: introduce `EventPayload::Inline([u8; 8]) | Heap(Bytes)`.

#### 75. `RedexIndex` snapshot iteration during `keys()` clones every key

Cold metrics path, fine.

#### 76. `for_channel` allocates `channel.to_string()` on miss

Only on first observation per channel — amortized to zero in steady state.

#### 77. `tail` uses `mpsc::channel` per subscriber

Subscriptions are long-lived, so per-subscription not per-event. Fine.

#### 79. `read_range` allocates `Vec::new()` and grows incrementally

Pre-size with the partition_point bounds (once #52 is fixed).

#### 80. `RedexFile::sync` per replication chunk applied

Documented as intentional. Operational tradeoff.

---

## Section 8: CortEX

### 🔴 High-impact

#### 81. `MemoriesQuery::matches` calls `m.content.to_lowercase()` per memory

**Location:** `adapter/net/cortex/memories/query.rs:68`:
```rust
if let Some(needle) = &self.content_contains {
    if !m.content.to_lowercase().contains(needle) {
```

The needle is pre-lowercased once at filter construction (good). But `m.content.to_lowercase()` allocates a fresh String per memory and does Unicode case-folding. For a state with 100K memories and 4KB avg content, a content search allocates 100K Strings totaling ~400MB and case-folds 400MB of text.

**Fix options:**
- ASCII fast path: `m.content.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))` — no allocation, no Unicode folding. Detect non-ASCII once at filter construction.
- Use `aho-corasick` with case-insensitive build.
- Char-by-char walk with lowercased comparison — no String allocation.

Same issue in `tasks/query.rs:85` (`t.title.to_lowercase()`). Single fix applies to both.

#### 82. `MemoriesQuery::execute` is a linear scan; no secondary indices

**Location:** `adapter/net/cortex/memories/query.rs:126`:
```rust
pub(super) fn execute(&self, state: &MemoriesState) -> Vec<Memory> {
    let mut out: Vec<Memory> = state
        .memories
        .values()
        .filter(|m| self.matches(m))
        .cloned()
        .collect();
    // ... sort, truncate
}
```

Every query walks the entire memory state. For a state with 1M memories and a query selecting 10 by tag, that's 1M `matches` calls. With #81's `to_lowercase()`, catastrophic.

The `RedexIndex` infrastructure exists for exactly this. Memories adapter could maintain:
- `tag → Set<MemoryId>`
- `source → Set<MemoryId>`
- `BTreeMap<created_ns, Set<MemoryId>>` for range queries

Trade-off: write path gets heavier. For read-heavy cortex workloads, right trade.

Same applies to `tasks/query.rs`.

#### 83. `MemoriesWatcher::stream` re-executes the full query on every change

**Location:** `adapter/net/cortex/memories/watch.rs:185`:
```rust
maybe_seq = changes.next() => {
    let Some(_seq) = maybe_seq else { return };
    let current = {
        let guard = state.read();
        spec.execute(&guard)        // <-- full query execute on every event
    };
    if current != last {
        // ... 
    }
}
```

Per fold event, the watcher:
1. Acquires state read lock
2. Runs full query (linear scan, #82)
3. Compares to last via Vec equality (O(N × content_size))
4. If different, clones and emits

For N watchers, fold rate F, memory count M, content size C: cost ≈ N × F × M × C per second. At realistic numbers (10 watchers, 1K events/sec, 100K memories, 1KB content): 10^12 byte ops/sec.

**Fix paths:**
- **Push delta info through change stream.** Currently emits `u64` (seq); emit `(seq, Vec<MemoryId> changed)`. Watchers skip re-execute when their filter doesn't intersect changed ids.
- **Watcher-side debouncing.** Batch over 10ms window.
- **Selectivity routing.** Watchers register filters; fold dispatches only to watchers whose filter intersects.

The change-stream-with-deltas approach is cleanest.

#### 84. `RpcRequestPayload::decode` and `RpcResponsePayload::decode` copy the body to `Vec<u8>`

**Location:** `adapter/net/cortex/rpc.rs:551`:
```rust
let body = data[body_start..body_end].to_vec();
```

Per decoded RPC frame, the body is memcpy'd from input bytes to a fresh `Vec<u8>`. For a 1KB body that's a 1KB copy purely because the input is `&[u8]` and the field is `Vec<u8>`.

The actual data source (`RpcInboundEvent::payload`) is already `Bytes` — we have refcount-able input but throw it away by going through `&[u8]`.

**Fix:**
1. Change `RpcRequestPayload::body: Vec<u8>` → `body: Bytes` (and same for `RpcResponsePayload`).
2. Change `decode(data: &[u8])` → `decode(data: Bytes)` so the function can `slice_ref` zero-copy.
3. Update call sites: `RpcRequestPayload::decode(ev.payload.slice(EVENT_META_SIZE..))` — refcount bump only.

Bonus: at `bytes::Bytes::from(resp.body)` (lines 3648, 3653, 3671 in client `dispatch_streaming_chunk`) becomes no-op.

For high-RPS systems doing 100K+ RPCs/sec with 1KB+ payloads: **100MB+/sec of memcpy saved.**

#### 85. `CortexAdapter::ingest` copies the tail twice into successive buffers

**Location:** `adapter/net/cortex/adapter.rs:445`:
```rust
let (meta, tail) = envelope.into_redex_payload();
let mut buf = Vec::with_capacity(EVENT_META_SIZE + tail.len());
buf.extend_from_slice(&meta.to_bytes());
buf.extend_from_slice(&tail);              // <-- copy 1
Ok(self.inner.file.append(&buf)?)          // file.append internally copies into segment → copy 2
```

Tail bytes copied **twice** per ingest.

**Fix:**
- Change `RedexFile::append` to accept `&[&[u8]]` (vectored).
- Or `RedexFile::append_pair(meta_bytes: &[u8], tail: &[u8])`.
- Or pass tail as `Bytes` and store payloads as `Bytes` in the segment (also fixes #51).

At 100K events/sec with 4KB tails: ~400MB/sec of redundant memcpy gone.

#### 86. `MemoriesAdapter::ingest_typed` allocates the tail Vec then re-buffers it in `CortexAdapter::ingest`

**Location:** `adapter/net/cortex/memories/adapter.rs:442`. Sequence per typed ingest:
1. `postcard::to_allocvec` allocs Vec<u8> for tail
2. `Bytes::from(tail)` wraps it
3. `inner.ingest` calls `envelope.into_redex_payload()` returning `(meta, Bytes)`
4. inner.ingest builds a fresh Vec(EVENT_META_SIZE + tail.len()), copies meta in, copies tail bytes back out of the Bytes

Tail bytes touched 3× per ingest.

**Fix:** Build the final wire buffer in one pass:
```rust
let mut buf = Vec::with_capacity(EVENT_META_SIZE + 128);
buf.resize(EVENT_META_SIZE, 0);                         // reserve meta slot
postcard::to_io(payload, &mut buf)?;                    // append tail
let tail = &buf[EVENT_META_SIZE..];
let cksum = compute_checksum_with_meta_in_place(&meta_no_cksum, tail);
let mut meta_final = meta;
meta_final.checksum = cksum;
buf[..EVENT_META_SIZE].copy_from_slice(&meta_final.to_bytes());
file.append(&buf)
```

One allocation, one fill. Cuts ingest-path allocations from 3 to 1.

### 🟡 Medium-impact

#### 87. RPC server fold: double `in_flight` lock (contains_key → re-lock → insert)

**Location:** `adapter/net/cortex/rpc.rs:1522, 1540`. Use `entry()`:
```rust
match self.in_flight.lock().entry(key) {
    Entry::Occupied(_) => { /* refuse + emit error response */ return Ok(()); }
    Entry::Vacant(v) => { v.insert(cancellation.clone()); }
}
```

Eliminates TOCTOU. Same pattern in `RpcServerStreamingFold::apply` at lines 2114, 2142.

#### 88. `RpcServerFold` makes 3 Arc clones per spawned handler

**Location:** `adapter/net/cortex/rpc.rs:1541-1543`:
```rust
let handler = self.handler.clone();
let emit = self.emit.clone();
let in_flight = self.in_flight.clone();
```

Bundle into one `RpcSpawnCtx` struct. One Arc clone per spawn.

#### 89. `MemoriesQuery::execute` sorts then truncates instead of top-K

**Location:** `adapter/net/cortex/memories/query.rs:126-138`. For a query `.order_by(CreatedDesc).limit(10)` against 100K matches, you sort 100K then keep 10 — `O(N log N)` for `O(N log K)` semantics.

**Fix:** `select_nth_unstable_by_key` (O(N)) + sort the chosen prefix (O(K log K)). Or `BinaryHeap` during filter pass, capped at `limit`.

For typical queries with small `limit` (top 10/100), ~1000× algorithmic improvement.

#### 90. `MemoriesFilterSpec::id_in` uses `Vec<MemoryId>` with linear contains

**Location:** `adapter/net/cortex/memories/query.rs:58`. For `where_id_in([1..100])` against 100K memories, 100K × 100 = 10M comparisons.

**Fix:** When > ~8 ids, store as `HashSet<MemoryId>`.

#### 91. `compute_checksum_with_meta` always computed even on legacy-only files

**Location:** `adapter/net/cortex/memories/fold.rs:50-56`. For a file written entirely by pre-v2 adapters, every fold pays v2 first, then v1. Two full hashes per event.

**Fix:** Track which checksum version this file uses on first successful verify (sticky flag). Subsequent events try recorded version first.

#### 92. `compute_checksum_with_meta` uses streaming xxh3 for ~30-byte input

**Location:** `adapter/net/cortex/meta.rs:201`:
```rust
let mut h = xxhash_rust::xxh3::Xxh3::new();
h.update(&meta.for_checksum_bytes());      // 24 bytes
h.update(tail);                             // typically tens to thousands of bytes
h.digest() as u32
```

`Xxh3::new() + update + digest` has more overhead than `xxh3_64(contiguous_buffer)` for short inputs.

**Fix:** For tails < ~1KB, assemble into stack `[u8; 1024]` and one-shot hash. For larger, streaming wins.

#### 93. `CortexAdapter::wait_for_seq` wakes every waiter on every fold event

**Location:** `adapter/net/cortex/adapter.rs:324, 361`. Multiple concurrent waiters all register with same `Notify`. Every fold event calls `notify.notify_waiters()`, waking ALL. Each waiter loads watermark, decides not ready, re-registers.

For N waiters and M events: O(N × M) wakeups + atomic loads + re-registrations.

**Fix:** Seq-keyed waiter queue. `BTreeMap<u64, Vec<oneshot::Sender<()>>>` of pending waiters. On fold completion, drain entries with `key <= new_watermark`.

Bigger structural change but materially improves high-RYW-load workloads.

#### 94. Cortex fold task `Lagged` recovery uses linear-scan `read_range`

**Location:** `adapter/net/cortex/adapter.rs:813`. Inherits #51 + #52 from RedEX. Lag recovery becomes O(N²).

Fixed indirectly by addressing #52.

#### 95. `RpcResponsePayload::headers: Vec<(String, Vec<u8>)>` forces static header names to allocate

**Location:** `adapter/net/cortex/rpc.rs:2084-2087`:
```rust
headers: vec![(
    HEADER_NRPC_STREAMING.to_string(),   // <-- static str → heap String per call
    HEADER_NRPC_STREAMING_END.to_vec(),  // <-- static slice → heap Vec per call
)],
```

`HEADER_NRPC_STREAMING` is `&'static str` but the field forces owned. Per streaming chunk emitted (thousands per RPC call), 1-2 Strings + 1-2 Vecs allocated for header bookkeeping that's structurally always the same.

**Fix:** Change header type to `Vec<(Cow<'static, str>, Cow<'static, [u8]>)>` or `Vec<RpcHeaderEntry>` with an enum variant for static-string headers.

#### 96. `MemoriesState::memories.values().cloned()` deep-clones every matched Memory

`Memory` contains `String content`, `Vec<String> tags`, `String source`. 3+ allocations per matched memory. For a 1000-result query: 3000+ allocations.

**Fix:** Store `Arc<Memory>` in `state.memories`. Reads return `Vec<Arc<Memory>>` — one atomic refcount bump per result.

Same in `tasks`.

#### 97. `RpcResponsePayload::body` allocated as `b"..."`.to_vec() for static error messages

**Location:** `adapter/net/cortex/rpc.rs:1508, 1533, 1619`:
```rust
body: b"deadline already passed when request landed".to_vec(),
```

**Fix:** Change `body: Vec<u8>` → `body: Bytes`, use `Bytes::from_static(b"...")`. Zero alloc. Combines naturally with #84.

### 🟢 Low-impact / cleanup

#### 98. `RpcClientFold::apply` duplicates `apply_inbound` body

`adapter/net/cortex/rpc.rs:3762 vs 3842`. Refactor to share. Maintenance hazard.

#### 99. `for_checksum_bytes` does `to_bytes` then zeroes 4 bytes

`adapter/net/cortex/meta.rs:116`. Two memcpys; could write fields directly into a stack array with checksum slot already zero.

#### 100. `EventMeta::to_bytes` / `from_bytes` aren't `#[inline]`

Same as RedEX `RedexEntry`. Mark `#[inline(always)]`.

#### 101. `format!("{:#x}", origin_hash)` in tracing macros

String allocation per log call even when subscribers don't accept the event.

**Fix:** `tracing::warn!(caller_origin = origin_hash, ...)` — pass u64 directly; subscriber formats. Or `format_args!`.

#### 102. RPC trace context extraction per RPC call

`adapter/net/cortex/rpc.rs:1552, 2160`. Gated by flag check (good — common path is zero work). Just confirming the gate is the right design.

#### 103. RPC `RpcInboundEvent` is `Clone`

`adapter/net/cortex/rpc.rs:915`. Worth auditing whether Clone is actually needed in hot paths.

#### 104. Tasks adapter mirrors all the memories adapter patterns

Every perf fix to memories should be mirrored to tasks. A generic `KeyedAdapter<K, V>` could share the implementation.

---

## Recommended Fix Order

Top items combine clear wins, small contained diffs, and benchmark-visible impact:

1. **#1 `in_flight_ingests` SeqCst** — biggest scaling cliff
2. **#13 `dispatch_batch.clone` → `Arc<Batch>`** — mechanical, big win
3. **#17 + #32 `ThreadLocalPool` retain** — explains the benchmark anomaly
4. **#51 `HeapSegment::read` zero-copy** — biggest RedEX read-path win
5. **#15 + #16 filter parse + path-split** — orders of magnitude on filtered polls
6. **#52 binary search in `read_one`/`read_range`** — `tail()` already shows how
7. **#29 redis Value tree** — biggest redis read-path win
8. **#82 + #81 CortEX indices + ASCII case-insensitive search** — read scaling
9. **#84 RPC body Bytes** — clean type refactor, big win
10. **#2 mapper allocs** — easy, no semantic change

After that the cross-cutting patterns (esp. `Bytes` zero-copy, secondary indices, coarse clock) become the meta-fix that catches many remaining items.
