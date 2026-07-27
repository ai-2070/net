# Bug Report: `net/crates/net/`

Audit of the Rust event-bus crate at `crates/net/src/`. Findings are based on the source tree at branch `performance-simple` (commits through `62fe0ef "Fix issues"`). Severity-ordered within each section. Highest-severity claims were spot-verified against the source; speculative findings that did not survive verification are listed in the "Dropped after verification" section for transparency.

---

## Summary

| # | Severity | File | Issue |
|---|---|---|---|
| 1 | CRITICAL | `ffi/mod.rs:133-148, 849-877` | Use-after-free in `net_shutdown` Dekker handshake |
| 2 | HIGH | `consumer/merge.rs:326-339` | Lazy filter drops matching events from un-inspected shards |
| 3 | HIGH | `config.rs` | Multiple unvalidated zero-divisors in `validate()` |
| 4 | HIGH | `adapter/redis.rs:336` | `poll_shard` cursor stalls when every entry fails to deserialize |
| 5 | HIGH | `shard/ring_buffer.rs:62-71` | Public `unsafe impl Sync for RingBuffer` is a silent-UB footgun |
| 6 | MEDIUM | `bus.rs:565-599` | Shutdown vs. concurrent `ingest` race strands events |
| 7 | MEDIUM | `shard/mapper.rs:564` | Shard ID reuse after `remove_stopped_shards` |
| 8 | MEDIUM | `shard/mod.rs:438-456` | `DropOldest` retry violates SPSC contract |
| 9 | MEDIUM | `shard/mapper.rs:688-712` | `finalize_draining` reads a destructively-reset counter |
| 10 | MEDIUM | `adapter/jetstream.rs` | Silent failures and bad transient classification |
| 11 | MEDIUM | `adapter/jetstream.rs:351, 363-376` | `poll_shard` overflow + silent skip on deserialize errors |
| 12 | MEDIUM | `adapter/redis.rs:198-228` | Pipeline timeout-then-retry produces duplicates |
| 13 | MEDIUM | `adapter/redis.rs:46-82` | `stream_key` cache uses `Vec` keyed by sparse shard_id |
| 14 | MEDIUM | `timestamp.rs:63` | `next()` returns raw TSC ticks; doc says nanoseconds |
| 15 | MEDIUM | `ffi/mesh.rs:1092-1105` | `collect_payloads` lacks per-entry null check |
| 16 | MEDIUM | `ffi/mod.rs:866-875` | `net_shutdown` spin-waits unboundedly on in-flight ops |
| 17 | MEDIUM | `bus.rs:647-654` | `EventBus::Drop` only flips the flag — in-flight events lost on implicit drop |
| 18 | LOW | `error.rs:21-22, 45-46` | `Serialization(String)` discards underlying serde error |
| 19 | LOW | `shard/batch.rs:220` | `next_sequence += events.len() as u64` is unchecked |
| 20 | LOW | `shard/mod.rs:413` | `hash % num_shards` panics if `num_shards == 0` |
| 21 | LOW | `error.rs` / `bus.rs:659-692` | `is_retryable()` is defined but `dispatch_batch` retries everything |
| 22 | LOW | `ffi/mod.rs:1102-1126, 1156-1180` | `NetEvent.id_len` / `raw_len` are publicly mutable from C — UB on size mismatch |
| 23 | MEDIUM | `consumer/merge.rs:340-353, 396-400` | Sort+truncate filter path strands matches on shards whose events all sort late |
| 24 | MEDIUM | `bus.rs:213-224, 565` | `shutdown()` is unreachable after `start_scaling_monitor()` (Arc ownership) |
| 25 | LOW | `shard/mod.rs:529-540` | `ingest_raw_batch` silently skips unresolvable shard IDs without counter update |

---

## 1. CRITICAL — `ffi/mod.rs:133-148, 849-877` — Use-after-free in `net_shutdown` Dekker handshake

**What's wrong.** The Dekker-style SeqCst handshake between `FfiOpGuard::try_enter` and `net_shutdown` correctly prevents an FFI op from *proceeding* past the shutdown gate, but does not prevent UAF on the increment itself.

`try_enter` (lines 133-148):

```rust
fn try_enter(handle: &'a NetHandle) -> Option<Self> {
    handle.active_ops.fetch_add(1, SeqCst);
    if handle.shutting_down.load(SeqCst) {
        handle.active_ops.fetch_sub(1, AcqRel);
        None
    } else { Some(Self { handle }) }
}
```

`net_shutdown` (lines 861-877):

```rust
let handle_ref = unsafe { &*handle };
handle_ref.shutting_down.store(true, SeqCst);
while handle_ref.active_ops.load(SeqCst) > 0 { spin_loop(); }
let handle = unsafe { Box::from_raw(handle) };  // FREE
```

Legal SeqCst-consistent interleaving:

1. Thread A: `shutting_down.store(true, SeqCst)`
2. Thread A: `active_ops.load(SeqCst)` → sees 0
3. Thread A: `Box::from_raw(handle)` — frees the storage (non-atomic, **not** in SeqCst total order)
4. Thread B: `handle.active_ops.fetch_add(1, SeqCst)` — **dereferences freed memory**

SeqCst gives a single total order over the atomic ops: if A's load saw 0, B's fetch_add must come *after* A's load in that order, which forces B to subsequently observe `shutting_down == true` and bail. That is what the existing comment relies on. But the `Box::from_raw` between A's load and B's fetch_add is non-atomic and is **not** ordered by SeqCst — A is free to deallocate before B's fetch_add ever runs.

The check that *acts* on `shutting_down == true` happens *after* the increment, so by the time B is told to bail it has already accessed freed memory. This is reachable from valid concurrent FFI use.

**Failure scenario.** Any C client that calls a non-shutdown FFI entry on one thread while another thread calls `net_shutdown` can hit the race. UAF is silent in release builds and may corrupt unrelated allocations or crash later.

**Fix.** Keep the handle alive across the increment. Two options:

- **Arc-clone before atomic access.** Box an `Arc<NetHandle>` and have every FFI entry clone the `Arc` from a `*const Arc<NetHandle>` before touching `active_ops`. `net_shutdown` then drops its `Arc`; the box is freed only when the last in-flight op drops its clone. The atomic `active_ops` becomes redundant.
- **Leak the handle.** Don't `Box::from_raw` in `net_shutdown` at all — just `bus.shutdown().await` and `Box::leak(handle)`. Trades a one-time leak per process for soundness.

---

## 2. HIGH — `consumer/merge.rs:326-339` — Lazy filter drops matching events from un-inspected shards

**What's wrong.** The `Ordering::None` filter loop breaks once `kept.len() >= limit + 1`:

```rust
for event in all_events.drain(..) {
    if kept.len() >= target {
        break;
    }
    if event_matches_filter(&event, filter) {
        kept.push(event);
    }
}
```

Because `all_events` is built shard-by-shard via `extend()`, hitting the target inside (say) shard 0 means shard 1/2/3 events are silently discarded by `Drain::drop` without ever being filtered.

**Failure scenario.** The cursor at line 382 starts from `new_cursor` (which already advanced past every fetched event for every polled shard) and is only overridden for shards present in the *returned* set. Shards that were never inspected keep the advanced position, so any matching events in their fetched range are lost forever. The infinite-loop regression test at line 1336 only happens to pass because its shard-1 events don't match the filter — flip the test so shard 1 also has `type:"token"` events and matching events are silently dropped.

**Fix.** In the `Ordering::None` filter path, drop the early `break` (use `retain` like the sort path), or only advance the cursor for shards whose events were fully drained.

---

## 3. HIGH — `config.rs` — Multiple unvalidated zero-divisors in `validate()`

**What's wrong.** Several config knobs that act as divisors are accepted as 0 by `EventBusConfig::validate`:

- `BackpressureMode::Sample { rate: 0 }` is accepted (`config.rs:220-223`); downstream sampling typically uses `counter % rate` → div-by-zero panic.
- `BatchConfig.velocity_window: Duration::ZERO` with `adaptive: true` (`config.rs:262-277`) div-by-zeros the throughput calculator.
- `ScalingPolicy.cooldown` / `metrics_window` / `scale_down_delay` zero values aren't rejected (`config.rs:636-656`). `metrics_window=0` panics in rate math; `cooldown=0` thrashes the scaler; `scale_down_delay=0` scales down on the first underutilized sample.
- `EventBusConfig.adapter_timeout: Duration::ZERO` is accepted (`config.rs:38, 69-90`); every adapter call then times out instantly.
- `RedisAdapterConfig` (`config.rs:326-355`) and `JetStreamAdapterConfig` have no `validate()` at all and aren't recursed into from `EventBusConfig::validate`. `pipeline_size: 0` ships through.

**Failure scenario.** A user constructs e.g. `RedisAdapterConfig { pipeline_size: 0, .. }`, the bus accepts it, then the Redis adapter spins forever or panics on first batch. Similar for the other configs.

**Fix.** Add the missing zero/non-zero checks in `validate()`, prefer `NonZeroU32` / `NonZeroDuration` types where possible, and recurse into adapter configs.

---

## 4. HIGH — `adapter/redis.rs:336` — `poll_shard` cursor stalls when every entry fails to deserialize

**What's wrong.** After `XRANGE` returns, the adapter computes the next cursor from the *deserialized* events:

```rust
let has_more = entries.len() > limit;
let next_id = events.last().map(|e| e.id.clone());
```

If every fetched entry fails `deserialize_event` (logged-and-skipped at lines 314-323), `events` is empty, so `next_id == None`. The merger only advances its per-shard cursor when `next_id` is `Some` (`consumer/merge.rs:290`), so the consumer re-fetches from the same start, hits the same corrupt entries, logs the same warnings forever — and never makes progress.

**Failure scenario.** A single corrupt or schema-mismatched run of XRANGE entries silently wedges a consumer in a hot infinite loop with no observable forward motion.

**Fix.** Track the last *raw entry id* seen during iteration (regardless of deserialize result) and use that as `next_id`. Skipping a corrupt event must still advance past it.

---

## 5. HIGH — `shard/ring_buffer.rs:62-71` — Public `unsafe impl Sync for RingBuffer` is a silent-UB footgun

**What's wrong.** `RingBuffer<T>` is documented and implemented as SPSC. The `Sync` impl is sound only under the SPSC contract, but the type is `pub` (re-exported from `shard/mod.rs:18`) and `try_push(&self, ..)` / `try_pop(&self) -> Option<T>` are safe-Rust methods. The `#[cfg(test)]` consumer-thread-id assertion catches mis-use only in test builds — release builds silently corrupt.

In production the buffer is wrapped in `parking_lot::Mutex<Shard>`, which serializes producer and consumer across mutex acquisitions, so the SPSC invariant holds inside this crate today. But the public surface advertises a lock-free SPSC buffer that any external caller can call `try_push` on from two threads simultaneously, with no `unsafe` token to flag the hazard.

**Failure scenario.** Any consumer of the crate that constructs an `Arc<RingBuffer<T>>` and shares it across threads compiles cleanly, runs the test suite cleanly (consumer-thread asserts compile out), and silently corrupts the head/tail atomics in release.

**Fix.** Pick one:

- Make the type `pub(crate)` (preferred — there's no documented external use).
- Drop the `Sync` impl and require external synchronization at the type level.
- Make `try_push` / `try_pop` `unsafe fn` and document the SPSC contract as a safety precondition.

---

## 6. MEDIUM — `bus.rs:565-599` — Shutdown vs. concurrent `ingest` race strands events

**What's wrong.** `ingest` does `shutdown.load(Relaxed)` and the drain worker uses `Relaxed` reads too. There is no happens-before relationship between "producer's ring-buffer push completes" and "drain worker observed empty + shutdown".

**Failure scenario.** A producer reads `shutdown==false`, is preempted, then pushes after the drain worker's last `pop_batch_into` returned 0 and exited. The ring buffer is later dropped wholesale with the event still in it. The shutdown comment at lines 559-564 claims the inverse ("every event the producer has handed to the bus is now in the per-shard mpsc channel").

**Fix.** Introduce a quiescence step — count in-flight ingests, and have the drain worker re-check the ring buffer after observing `shutdown=true && in_flight==0`. Alternatively, gate `ingest` behind an `Acquire` load and ensure the drain worker performs an `Acquire` of the same atomic before its final sweep.

---

## 7. MEDIUM — `shard/mapper.rs:564` — Shard ID reuse after `remove_stopped_shards`

**What's wrong.**

```rust
let max_id = shards.iter().map(|s| s.id).max().unwrap_or(0);
```

`max_id` is computed against the *current* shards vector, which excludes any shards already drained-and-removed. After removing the highest-ID shard, the next `scale_up` re-allocates the same ID.

**Failure scenario.** Scale up to 10, drain+remove shard 9, scale up by 1 → new shard also gets ID 9. `ShardManager.shard_index` (`mod.rs:661`) silently overwrites the stale entry, and any external metric/checkpoint that keys on shard ID will merge two unrelated shard lifetimes into one.

**Fix.** Maintain a monotonic `next_id: AtomicU16` on `ShardMapper` and `fetch_add` it for new shards.

---

## 8. MEDIUM — `shard/mod.rs:438-456` — `DropOldest` retry violates SPSC contract

**What's wrong.** `push_with_backpressure` calls `shard.try_pop()` from the producer's call stack to evict the oldest event when the buffer is full. The ring buffer is documented SPSC (`ring_buffer.rs:62-67`), and the test build asserts a single consumer thread ID — the legitimate consumer is the batch worker.

**Failure scenario.** In test builds, mixing producer-side `try_pop` with the batch worker's `try_pop` panics on the consumer-thread tracking assertion. In release, mutex-serialization happens to keep the atomics race-free today, but any future SPSC optimization (relaxing tail-side memory ordering on the assumption that only one thread touches it) would silently corrupt data.

**Fix.** Add a producer-side `evict_oldest()` on the ring buffer that bypasses consumer-thread tracking, or document that `DropOldest` requires the producer thread also to be the SPSC consumer (currently false, since the batch worker is the consumer).

---

## 9. MEDIUM — `shard/mapper.rs:688-712` — `finalize_draining` reads a destructively-reset counter

**What's wrong.** The "is the shard quiescent?" check uses `events_in_window`, but `collect_and_reset()` swaps that field to 0 on every metrics tick.

**Failure scenario.** A draining shard whose buffer transiently empties between two metrics ticks is finalized while a producer with a cached `shard_id` is still pushing. The 100ms grace window reduces but does not eliminate the race.

**Fix.** Track a separate "events since drain start" counter that is *not* reset by `collect_and_reset`. Use the ring buffer's actual `len()` plus the `draining` flag age as the empty signal.

---

## 10. MEDIUM — `adapter/jetstream.rs` — Silent failures and bad transient classification

**Three independent issues:**

1. **`shutdown` swallows `drain` error** (`L307-323`). `let _ = client.drain().await;` discards the error and reports `Ok(())`. Trait contract says shutdown should flush.
2. **`is_transient_error` is over-broad** (`L433-440`). Treats *every* error other than `WrongLastSequence` as transient — including auth, quota-exceeded, and stream-config errors. Combined with a naive caller retry loop, this becomes an infinite retry storm.
3. **Stream-creation race** (`L110-165`). Two cold-start `on_batch` calls for the same shard both fire `get_stream → create_stream` and both insert into the cache. Idempotent today, but extra RPCs on cold start, and a hazard if `create_stream` configs ever diverge between callers.

**Fix.** Surface the `drain` error as `AdapterError::Transient`; enumerate fatal error kinds in `is_transient_error`; gate cold-start with a per-shard `OnceCell` or per-shard mutex.

---

## 11. MEDIUM — `adapter/jetstream.rs:351, 363-376` — `poll_shard` overflow + silent skip on deserialize errors

**Two related issues on the JetStream poll path:**

1. **Unchecked multiplication in `max_seq` fallback** (line 351):

   ```rust
   Err(_) => start_seq.saturating_add(fetch_limit as u64 * 10),
   ```

   `fetch_limit as u64 * 10` is plain `*`, not `checked_mul`. `Adapter::poll_shard` is on a `pub trait` and `fetch_limit` derives from caller-supplied `limit`. The merger caps fetches at 10_000 today but anyone calling the adapter directly can overflow before `saturating_add` runs, producing a tiny `max_seq` that silently caps the poll.

2. **Silent skip on deserialize failure** (lines 363-376). When `deserialize_event` fails on a successful `direct_get`, the code logs and bumps `current_seq`, then continues. Combined with #1's possibly-too-small `max_seq`, a long run of corrupt sequences silently shrinks the effective fetch window, dropping events from results without surfacing an error.

**Fix.** Use `checked_mul` (or compute `max_seq` from `stream_info()` directly without the fallback). Track per-poll deserialize failures and either return them as a partial-error variant or surface a counter so silent corruption is observable.

---

## 12. MEDIUM — `adapter/redis.rs:198-228` — Pipeline timeout-then-retry produces duplicates

**What's wrong.** `tokio::time::timeout` over a `MULTI/EXEC` does not roll back bytes already on the wire. Redis can execute the EXEC after the future is dropped; the subsequent retry then double-publishes via XADD with auto-generated `*` IDs and no dedup.

**Failure scenario.** At-least-once degrades to *more*-than-once with no dedup. The "Improve Redis throughput" comment claiming "either all XADDs succeed or none do" is true within one `MULTI` but not across cancel-then-retry.

**Fix.** Include a per-event dedup token (e.g. `{shard_id}:{seq_start}:{i}`) and gate XADD via a Lua script that checks `SADD` of recent tokens. Otherwise, document at-least-once-with-duplicates explicitly and rely on consumer-side idempotency.

---

## 13. MEDIUM — `adapter/redis.rs:46-82` — `stream_key` cache uses a `Vec` keyed by shard_id

**What's wrong.**

```rust
while cache.len() <= idx { cache.push(...) }
```

If the first access is `shard_id = 65535`, this allocates 65536 placeholder entries.

**Failure scenario.** Sparse / hashed shard IDs cause O(max_shard_id) memory blowup on cold access.

**Fix.** Switch the cache to `HashMap<u16, Arc<str>>`.

---

## 14. MEDIUM — `timestamp.rs:63` — `next()` returns raw TSC ticks; doc says nanoseconds

**What's wrong.** `quanta::Clock::raw()` returns the raw TSC counter, not nanoseconds. The docstring at line 63 says "Returns a strictly monotonically increasing value (nanoseconds)". `insertion_ts` is plumbed into `StoredEvent`, serialized externally, and used cross-shard for sorting in `consumer/merge.rs:351`.

**Failure scenario.** Sorting works (TSC is invariant on modern x86 across cores), but external consumers reading the JSON `insertion_ts` field receive TSC ticks — about 3.5× larger than wall-clock-ns on a 3.5GHz core — breaking interop with anything expecting ns-since-epoch or correlating against ns from other sources. The `raw_to_nanos` helper exists but is never applied before storage.

**Fix.** Convert via `clock.delta_as_nanos(0, raw)` inside `next()` (preserving monotonicity by storing nanos), or rename the field/doc to `insertion_tsc` and document the unit honestly.

---

## 15. MEDIUM — `ffi/mesh.rs:1092-1105` — `collect_payloads` lacks per-entry null check

**What's wrong.** After verifying that the outer `payloads` / `lens` arrays are non-null, the function dereferences each `*payloads.add(i)` and feeds the result straight to `slice::from_raw_parts(ptr, lens[i])`. Per-entry pointers are not null-checked.

**Failure scenario.** Any C caller that passes an array containing a null entry — easy to do when batching optional payloads — produces instant UB on `from_raw_parts(null, len)`. The C contract gives the caller no way to convey "skip this entry," so a defensive null check is cheap and correct.

**Fix.** Null-check each per-entry pointer before constructing the slice; return an error code on nulls.

---

## 16. MEDIUM — `ffi/mod.rs:866-875` — `net_shutdown` spin-waits unboundedly

**What's wrong.** The atomic `FfiOpGuard` correctly prevents `Box::from_raw` while ops are in flight (modulo bug #1 above), but if a concurrent op blocks (e.g. `net_flush` against a hung adapter), `net_shutdown` busy-waits forever with no progress and no timeout.

**Failure scenario.** A hung adapter pins one CPU at 100% inside `net_shutdown` and the C client has no recovery path.

**Fix.** Bounded wait with a deadline; on timeout, return a `Busy` / `Timeout` error code. Or convert to `tokio::sync::Notify` so the wakeup is event-driven. Note: this fix should be designed jointly with the fix to bug #1, since both touch the same handshake.

---

## 17. MEDIUM — `bus.rs:647-654` — `EventBus::Drop` only flips the flag — in-flight events lost on implicit drop

**What's wrong.**

```rust
impl Drop for EventBus {
    fn drop(&mut self) {
        self.shutdown.store(true, AtomicOrdering::Release);
    }
}
```

`Drop` sets the shutdown flag but doesn't await drain workers, batch workers, or `adapter.shutdown()`. The documented contract is to call `shutdown().await`, but Rust gives no compile-time enforcement of "must call X before drop." If the runtime is dropped (or the bus dropped without an awaited shutdown), in-flight events in the per-shard mpsc channels and ring buffers are silently discarded — `adapter.flush()` and `adapter.shutdown()` never run.

**Failure scenario.** A test or short-lived process that forgets `bus.shutdown().await` loses any events that were ingested but not yet dispatched. `test_regression_eventbus_drop_signals_shutdown` only asserts that drop doesn't hang; it does not assert durability.

**Fix.** Either (a) document the requirement loudly and have `Drop` log a warning when in-flight work is observed at drop time, or (b) use a sync-blocking drain in `Drop` (e.g. `Handle::block_on` if a runtime handle is held), or (c) refactor to a typestate that makes `shutdown()` mandatory (`bus.into_shutdown_handle()` consumes the bus).

---

## 18. LOW — `error.rs:21-22, 45-46` — `Serialization(String)` discards underlying serde error

**What's wrong.** `Serialization(String)` instead of `Serialization(#[from] serde_json::Error)` loses category, line, column, and breaks the `source()` chain.

**Fix.** Change to `Serialization(#[from] serde_json::Error)`.

---

## 19. LOW — `shard/batch.rs:220` — `next_sequence += events.len() as u64` is unchecked

**What's wrong.** Plain `+=` on a `u64` running counter. Theoretical only — at 1B events/sec, u64 takes ~584 years to wrap — but if it ever does, it wraps silently.

**Fix.** Use `checked_add` and surface as a fatal error, or document the assumption explicitly.

---

## 20. LOW — `shard/mod.rs:413` — `hash % num_shards` panics if `num_shards == 0`

**What's wrong.**

```rust
(hash % num_shards as u64) as u16
```

Panics on division by zero if `num_shards` is ever 0. Static config validation enforces `num_shards >= 1`, and `scale_down` requires `current > min_shards >= 1`, so this is unreachable in current code.

**Fix.** Defense-in-depth `debug_assert!(num_shards > 0)` and either return shard 0 or return a typed error if ever reached.

---

## 21. LOW — `error.rs` / `bus.rs:659-692` — `is_retryable()` is defined but `dispatch_batch` retries everything

**What's wrong.** `AdapterError::is_retryable()` distinguishes `Connection` (non-retryable) from `Transient` (retryable). `dispatch_batch` ignores the flag and runs the full retry loop on any error. The `Connection` variant comment in `error.rs:138-145` even acknowledges "The batch dispatch path retries all errors regardless of this flag."

**Fix.** Either gate `dispatch_batch`'s retry loop on `err.is_retryable()`, or remove the API as dead code.

---

## 22. LOW — `ffi/mod.rs:1102-1126, 1156-1180` — `NetEvent.id_len` / `raw_len` are publicly mutable from C — UB on size mismatch

**What's wrong.** `Box::into_raw(bytes: Box<[u8]>) as *const c_char` strips the fat-pointer length; reconstruction relies on the separately stored `id_len` / `raw_len`. The struct is `#[repr(C)]` with public fields, so a C caller that mutates `id_len` between alloc and free causes `Box::from_raw(slice_from_raw_parts_mut(ptr, wrong_len))` to be UB (allocator size mismatch).

**Fix.** Document that the length fields are read-only after `net_poll`. For stronger guarantees, store the actual allocated length in a side table keyed by pointer, and ignore the C-visible `id_len` at free time.

---

## 23. MEDIUM — `consumer/merge.rs:340-353, 396-400` — Sort+truncate filter path strands matches on shards whose events all sort late

**What's wrong.** Same root cause as #2 but on the *other* filter branch (`Ordering::InsertionTs`). After `retain` keeps every match, the global `sort_by_key(|e| e.insertion_ts)` followed by `truncate(request.limit)` can drop *every* match from a shard whose matching events all sort later than `limit` other matches. The override loop at lines 396-400 only writes `final_cursor[shard]` for shards that appear in the (truncated) returned set — a shard whose matches were entirely truncated has no representative event left to override with, so `final_cursor[shard]` keeps the value `new_cursor` got at lines 289-291: the adapter's reported `next_id` (i.e. the *fetched* boundary). Subsequent polls for that shard start *after* the boundary, silently skipping every match that was retained-but-truncated.

The trade-off comment at lines 364-379 names this exact failure mode ("matching events that were over-fetched but not returned due to the limit — a correctness violation") and credits the override with preventing it — but the override only fires for shards present in the returned events, so the protection is asymmetric and incomplete.

**Failure scenario.** 2 shards, `Ordering::InsertionTs`, filter `type=="token"`, `limit=5`, `over_fetch_factor=3` ⇒ `per_shard_limit=9`. Shard 0 has 30 matching events with ts 10..300; shard 1 has 30 matching events with ts 1000..1290. First poll fetches 9 from each: 18 retained, sorted by ts puts shard 0 ahead of shard 1, `truncate(5)` keeps only shard 0's first 5. Override sets `cursor[0]="0-5"`; shard 1 is absent from returned events so `cursor[1]` stays at `new_cursor[1]="1-9"`. Next poll fetches shard 1 events 1-10..1-30 — events 1-1..1-9 (matching) are gone forever.

**Fix.** Same shape as the recommended fix for #2: don't advance `final_cursor[shard]` to the adapter's `next_id` when the shard had matches that didn't make it into the returned set. The straightforward implementation: for each shard in the polled set, compute "did this shard appear in returned events?" — if yes, override to the last returned id (current behavior); if no, **leave the cursor at the original `cursor[shard]`** (forcing re-fetch) rather than letting `new_cursor[shard]` win. This re-fetches non-matches for shards that had no matches at all, which is the trade-off the existing comment claims to make but actually doesn't.

---

## 24. MEDIUM — `bus.rs:213-224, 565` — `shutdown()` is unreachable after `start_scaling_monitor()`

**What's wrong.** The two API shapes are incompatible:

```rust
pub fn start_scaling_monitor(self: &Arc<Self>) {
    let bus = Arc::clone(self);
    let handle = tokio::spawn(async move { bus.run_scaling_monitor().await; });
    *self.scaling_monitor.lock() = Some(handle);
}

pub async fn shutdown(self) -> Result<(), AdapterError> { ... }
```

`start_scaling_monitor` requires `Arc<Self>` and the spawned task keeps a clone for the lifetime of the monitor loop. `shutdown(self)` consumes the bus by value — to call it you need a non-`Arc` `EventBus`, i.e. you need `Arc::try_unwrap(arc)` to succeed. But the spawned task is still holding a strong ref, so `try_unwrap` returns `Err` and there is no path to ever call `shutdown()`.

The monitor loop itself only exits on `self.shutdown.load(...) == true`, but the only public method that flips that flag is `shutdown(self)` — which you can't call. Setting it manually requires reaching into the private field.

**Failure scenario.** A user wires up dynamic scaling per the `start_scaling_monitor` API, then later wants to gracefully drain on process exit. There is no clean way to do it: the only options are (a) `drop(arc)` and rely on the `Drop` impl (which only flips the flag — see #17 for the durability hole) or (b) `std::process::exit` and lose in-flight events. None of the existing tests call `start_scaling_monitor` *and then* `shutdown`, so the deadlock-by-API isn't exercised in CI.

**Fix.** Either:
- Change `shutdown` to take `self: Arc<Self>` (and on entry, drop the monitor's `JoinHandle` after setting the flag, then `Arc::try_unwrap` once the monitor task observes the flag and exits), or
- Split out a `ShutdownHandle` returned from `start_scaling_monitor` that holds the cancellation primitive and a way to await monitor exit, so `shutdown(self)` can stay as-is and the caller drops the handle first.

---

## 25. LOW — `shard/mod.rs:529-540` — `ingest_raw_batch` silently skips unresolvable shard IDs without counter update

**What's wrong.** During the bucket-by-shard pass:

```rust
for event in events {
    let shard_id = self.select_shard_by_hash(event.hash());
    let Some(idx) = self.resolve_idx(&table, shard_id) else {
        continue;       // <- silently dropped
    };
    if let Some(g) = groups.get_mut(idx) {
        ...
    }
}
```

When the table loaded at the top of the function doesn't contain the chosen `shard_id` (e.g., the mapper returned a shard that was just removed in a concurrent scale-down), the event is `continue`d — no error, no counter update. `EventBus::ingest_raw_batch` (`bus.rs:461-480`) computes drops as `total - success`, so the bus-level `events_dropped` is correct, but the per-shard `events_dropped` counter aggregated across `ShardManager::stats()` undercounts: there's no shard to attribute the drop to (the shard wasn't in the table).

**Failure scenario.** Consumers reconciling per-shard `events_dropped` against bus-level `events_dropped` see a discrepancy that shows up only during scale-down events. Hard to diagnose because the discrepancy is real and proportional to scale-down activity.

**Fix.** Either (a) account these "no shard available" drops in a separate `events_unrouted` counter on `ShardManager` and surface it via `stats()`, or (b) treat unresolvable shard IDs as a fatal config invariant and `debug_assert!` on them — they should be impossible if the mapper and table are kept in sync.

---

## Dropped after verification

These were flagged by the audit but did not survive a check of the source. Documented here so future audits don't waste cycles on them.

- **`ring_buffer.rs:319-322` `len()` torn read.** Claimed that head/tail load ordering could underflow `wrapping_sub`. False under the SPSC contract: head only increases (producer), tail only increases up to head (consumer), so `head ≥ tail` always holds and `wrapping_sub` returns the correct small positive value.
- **`ring_buffer.rs:230-241` `pop_batch` panic safety.** Claimed that a panic between `assume_init_read` and the `tail.store` would leave moved-out slots reachable. False: the only operations between the read and the store are `Vec::push` calls, and the vec was pre-`reserve`d, so push is allocation-free and panic-free.
- **FFI panics across the C ABI are UB on every entry point.** False for shipped builds: `[profile.release]` in `Cargo.toml:225` sets `panic = "abort"`, so unwinding cannot cross the boundary in release artifacts. Test/debug builds still unwind across the boundary, but that is a test-only concern and the FFI is not exercised from C in those configurations.

---

## Recommended order of fixes

1. **`ffi/mod.rs` UAF in `net_shutdown` handshake (#1)** — memory-unsafety reachable from valid C usage; switch to `Arc`-based lifetime extension or leak-on-shutdown.
2. **`merge.rs:326-339` (#2)** — silent data loss; drop the early `break` or restrict cursor advance to fully-drained shards.
3. **`adapter/redis.rs:336` cursor stall (#4)** — single-line fix (track raw entry id, not deserialized event id) that prevents silent infinite-loop wedging.
4. **`config.rs` zero-divisor validation (#3)** — turn panics on misconfiguration into config errors.
5. **`shard/ring_buffer.rs` Sync footgun (#5)** — make the type `pub(crate)` or mark methods `unsafe`.
6. **`mapper.rs:564` shard ID monotonicity (#7)** — replace `max+1` with `next_id: AtomicU16`.
7. **`bus.rs::shutdown` quiescence step (#6)** + **`EventBus::Drop` durability (#17)** — close the concurrent-ingest stranding window and surface dropped-without-shutdown.
8. **`ffi/mesh.rs::collect_payloads` per-entry null check (#15)** + **`net_shutdown` bounded wait (#16)**.
9. **`jetstream.rs` `is_transient_error` enumeration + drain swallow (#10)** + **poll_shard overflow/silent-skip (#11)**.
10. Remaining MEDIUM items as time permits; LOW items are clean refactors.
