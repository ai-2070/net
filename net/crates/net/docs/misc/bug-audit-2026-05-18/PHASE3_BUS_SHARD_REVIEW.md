# Phase 3 — Bus & Shard Concurrency / Correctness Review

Scope: `net/crates/net/src/bus.rs`, `net/crates/net/src/shard/{mod,batch,mapper,ring_buffer}.rs`,
`net/crates/net/src/timestamp.rs`. Date: 2026-05-18.

## Findings (severity-ordered)

### F-1 — `finalize_draining` predicate is dead: `record_push` / `record_buffer_len` never run on the hot path

- **File:line:** `net/crates/net/src/shard/mod.rs:146-189` (the only push paths, `Shard::try_push_raw` /
  `Shard::try_push`); read site `net/crates/net/src/shard/mapper.rs:1185` (`current_len.load`) and
  `:1206` (`pushes_since_drain_start()`).
- **Severity:** critical
- **Bug class:** wrong-invariant / dead-code-on-hot-path
- **What:** `Shard::try_push_raw` / `Shard::try_push` increment `ShardCounters` but never call
  `self.metrics_collector.as_ref().map(|m| m.record_push(...))` or `record_buffer_len`. Grep confirms
  the only callers of `record_push` are tests (`mapper.rs:1447-1448`, `:1495`, `:2546`, `:2564`).
  Consequently `ShardMetricsCollector::current_len` stays at `0` forever, `events_in_window` stays at
  `0`, and `pushes_since_drain_start` stays at `0` for the entire lifetime of every production shard.
- **Scenario:** Caller does `mapper.scale_down(1)` → shard X transitions to `Draining` with
  `metrics.set_draining(true)`. After 100 ms `finalize_draining()` runs:
  `current_len == 0` and `pushes_after_drain == 0` are **always true**, so the predicate at
  `mapper.rs:1207` finalizes shard X immediately even if its ring buffer holds millions of events.
  The bus then calls `remove_shard_internal(X)` which DOES drain via `shard_manager.remove_shard`'s
  `pop_batch_into`, so the events are not literally lost on the manual / monitor path — but every
  doc-comment and regression test in this area (e.g. `mapper.rs:215-260`, `:1167-1207`) asserts the
  predicate is meaningful, and a careless future use of `is_draining` + the predicate to *skip* the
  stranded-flush would re-open the loss path. As a separate symptom, `evaluate_scaling` reads
  `last_metrics.fill_ratio` / `event_rate` (both `0` always), so the **underutilized** trigger
  matches every Active shard every tick — masked only by the `warmup` window and `cooldown`.
- **Fix sketch:** Make `Shard::try_push_raw` / `try_push` call
  `if let Some(m) = &self.metrics_collector { m.record_push(elapsed_ns); m.record_buffer_len(self.ring_buffer.len()); }`
  on the success path, and `m.record_buffer_len` after `evict_oldest` / on pop in `pop_batch_into`
  so the fill-ratio signal reflects reality.

---

### F-2 — `manual_scale_down` deadline uses wall-clock `std::time::Instant` inside a `tokio::time::sleep` loop

- **File:line:** `bus.rs:1680, 1684`
- **Severity:** medium
- **Bug class:** virtualization-of-time-divergence (also blocks `tokio::time::pause` tests)
- **What:** The outer 2 s deadline uses `std::time::Instant::now()` while the inner wait is
  `tokio::time::sleep`. The same anti-pattern was already fixed for the shutdown spin (see comment
  at `bus.rs:1388-1392` explaining the `tokio::time::Instant` migration), but `manual_scale_down`
  was missed.
- **Scenario:** A test calls `tokio::time::pause()` and `tokio::time::advance(Duration::from_secs(10))`
  expecting `manual_scale_down` to reach its deadline. The `sleep(50ms)` is virtualized; the
  `std::time::Instant::now()` deadline check is wall-clock. The loop spins forever (waiting for
  real time to advance) until the test's tokio time budget runs out. Same shape that caused
  `bus_shutdown_drain`-class flakes pre-fix.
- **Fix sketch:** Switch both `std::time::Instant::now()` calls in
  `manual_scale_down` to `tokio::time::Instant::now()`.

---

### F-3 — `drain_finalize_ready` deadline in the drain worker is `std::time::Instant`

- **File:line:** `bus.rs:2270, 2272` (drain worker's `finalize_deadline`)
- **Severity:** medium
- **Bug class:** same as F-2 — wall-clock deadline in virtualized-time path
- **What:** `let finalize_deadline = std::time::Instant::now() + DRAIN_FINALIZE_TIMEOUT;` is wall-clock
  while the surrounding loop awaits `tokio::time::sleep(1ms)`. Same problem as F-2; same fix
  rationale already documented at `bus.rs:1388-1392`.
- **Scenario:** A test that uses `tokio::time::pause()` to deterministically drive a drop-without-shutdown
  path will hang for the entire 10-second wall-clock `DRAIN_FINALIZE_TIMEOUT` because the
  virtualized `sleep` returns instantly and the wall-clock deadline never advances.
- **Fix sketch:** Switch to `tokio::time::Instant::now()` here too, or use
  `tokio::time::timeout(DRAIN_FINALIZE_TIMEOUT, ...)` around the inner wait.

---

### F-4 — `EventBus::Drop` calls `parking_lot::Mutex::lock()` on every shard via `total_pending_in_rings`; can deadlock against a still-running drain worker on the same thread

- **File:line:** `bus.rs:1793` (`self.shard_manager.total_pending_in_rings()`); reads
  `s.lock()` inside `shard/mod.rs:696`.
- **Severity:** medium (rare to trigger in async code; possible in FFI / blocking callers)
- **Bug class:** lock-acquired-during-Drop with re-entrancy hazard
- **What:** `Drop::drop` runs `total_pending_in_rings()` which calls
  `s.lock()` on each shard. If a drain worker — running on the same OS thread under a
  current-thread runtime — was preempted while holding `shard.lock()` and the bus is dropped from
  that same thread (e.g. a panic in a `block_on(shutdown)` path), the `lock()` deadlocks. The bus
  field doc says Drop is best-effort and async-free, so callers may legitimately drop from a
  non-tokio context; `parking_lot` is not re-entrant.
- **Scenario:** Single-thread tokio runtime; a test panics inside a `bus.shutdown().await` while the
  drain worker is mid-`with_shard(|s| s.pop_batch_into(...))` holding `shard.lock()`. Tokio unwinds
  the runtime; the `EventBus` is dropped on the same thread; `total_pending_in_rings()` blocks on
  `shard.lock()`. Multi-thread runtimes hide this because the lock holder runs on a different
  thread.
- **Fix sketch:** Replace the per-shard `lock()` with `try_lock_for(Duration::from_millis(50))` and
  fold a `None` result into a tracing warning rather than blocking; or use the lock-free
  `total.events_ingested - total.events_dropped` estimate from `ShardManager::stats()` which only
  touches atomics.

---

### F-5 — `add_shard_internal` publishes the new sender *before* the drain worker is spawned; producer hash routing to the new shard between `activate_shard` and `drain` spawn could enqueue events the drain hasn't started polling for

- **File:line:** `bus.rs:469-477` (`batch_senders.write().insert(new_id, tx.clone()); spawn_drain_worker_for_shard(...)`)
- **Severity:** low (transient: the drain worker starts within a few µs and the SeqCst gate of
  `activate_shard` is downstream, so producers cannot have selected the shard yet)
- **Bug class:** ordering-of-publish vs-consumer-spawn
- **What:** The drain worker spawn (line 471) happens between the bus's `batch_senders` insert
  (line 469) and `batch_workers.lock().insert` (line 479). Between those two points, the shard is
  still `Provisioning` so `select_shard` should skip it — but `ShardManager::ingest_raw` does
  `select_shard_by_hash` → resolve_idx → push, and in static mode `resolve_idx` just returns
  `shard_id as usize` without consulting state. In dynamic mode it consults `shard_index` (which
  the routing table already has the new shard in by then) but `select_shard` itself filters on
  `state == Active`. So a producer that already holds a stale `select_shard` result (computed
  before the rebuild) and pushes after `rebuild_table` is the only racy path. Not currently
  reachable, but it is an order-fragile pattern.
- **Scenario:** Theoretical: a producer in mid-`ingest_raw` had already passed `select_shard` and
  `resolve_idx` for a shard at idx that swap-removes will repurpose. The SeqCst load by
  `try_enter_ingest` is on `shutdown`, not on the routing table; arc_swap of the table happens
  while the producer holds the old `Arc<ShardTable>`. The producer's `shard.lock()` succeeds, push
  succeeds, but the corresponding drain worker has not started polling that shard's atomic state
  yet. Event is in the ring; drain worker exists, will eventually pop. Self-healing.
- **Fix sketch:** None required, but consider documenting the invariant that the drain worker spawn
  must happen *before* `activate_shard` (it does today, transitively, because `activate_shard` is
  step 3) — and add a debug assertion in the drain worker that its first iteration runs before any
  state observation of `Active`.

---

### F-6 — `EventBusStats::events_dropped` double-counts in the lossy-shutdown deadline path on the rare interleaving where a producer racing the deadline successfully pushed but the deadline thread observed `in_flight > 0`

- **File:line:** `bus.rs:1581-1599` (post-drain reconciliation)
- **Severity:** low (operator-observable but no data loss)
- **Bug class:** stats consistency
- **What:** The deadline-path reconciles `actual_drops = stranded.saturating_sub(post_deadline_ingests)`.
  But `stranded` is `in_flight_ingests` at deadline; `post_deadline_ingests` is the delta in
  `events_ingested`. A producer that incremented `in_flight_ingests`, observed `shutdown=false`,
  did NOT yet bump `events_ingested`, but completed its push before the final sweep, will be
  drained and dispatched — yet not counted in `post_deadline_ingests` (because the
  `events_ingested.fetch_add` happens in `ingest()` AFTER `shard_manager.ingest()` returns, and on
  the success path that's after the guard counts down — so the deadline thread sees the
  decrement but the new ingested count). The order in `ingest()` is:
  `shard_manager.ingest()` → `events_ingested += 1` → drop(guard) → `in_flight_ingests -= 1`. So
  `events_ingested` IS bumped before the in_flight decrement, meaning the deadline-snapshot already
  sees it. Re-reading: `stranded = in_flight_ingests.load()`, `ingested_now = events_ingested.load()`
  taken inside the deadline branch. If a producer is at the moment "just decremented
  in_flight_ingests, about to be observed as 0 on the next spin iteration" but `events_ingested`
  was bumped just before, the `ingested_at_deadline` already includes its bump but `stranded` counts
  it as in-flight. Result: `actual_drops` undercounts by 1 → fewer dropped recorded than reality.
  Plus the opposite: a producer that bumped `in_flight` but bailed on the shutdown check and so
  never bumped `events_ingested` would land in `stranded` correctly and is not in
  `post_deadline_ingests`, so it shows up as a true drop — good.
- **Scenario:** Heavy ingest at shutdown; a producer at exactly the deadline moment is between the
  `events_ingested += 1` and `drop(guard)` decrement. The reconciliation undercounts drops by 1 for
  every such in-flight worker. Negligible in practice.
- **Fix sketch:** Snapshot `events_ingested` at exactly the moment `in_flight_ingests` is observed
  > 0 (atomic-paired SeqCst loads in a single iteration), or shift the order in `ingest()` so
  `events_ingested += 1` runs as part of the IngestGuard drop only on success. Low priority.

---

### F-7 — `Shard::try_push_raw` / `try_push` count a backpressure rejection as `events_dropped` even when the bus's `BackpressureMode::DropOldest` then succeeds — `push_with_backpressure` corrects this with a decrement, but the window between bump and decrement is observable

- **File:line:** `shard/mod.rs:157-162` (the spurious bump) and `:501-510` (`push_with_backpressure`'s
  correction: `fetch_sub(1)` then `fetch_add(1)`).
- **Severity:** low
- **Bug class:** stats consistency / transient visibility
- **What:** The flow inside `DropOldest` is: try_push fails → `fetch_add(events_dropped, 1)` →
  caller does `fetch_sub(events_dropped, 1)` → `evict_oldest` → `fetch_add(events_dropped, 1)` →
  retry try_push succeeds. Net delta: `+1`. Correct. But a concurrent reader of
  `manager.stats().events_dropped` between the first `fetch_add` and the `fetch_sub` sees an
  inflated value. Stats are documented as snapshot-not-coherent, so this is doc-conformant; flag
  for observability noise only.
- **Scenario:** A monitoring scrape at 1 Hz captures the brief over-count under sustained DropOldest
  pressure. Bounded to one event per concurrent retry, so the bias is small.
- **Fix sketch:** Restructure so `try_push_raw` returns a discriminated Err and the counter bump is
  done by `push_with_backpressure` after the final outcome is known.

---

### F-8 — `ShardMetricsCollector::collect_and_reset` is not atomic across its four fields; a `record_push` interleaving with the swap-sequence can split a window's event from its latency

- **File:line:** `shard/mapper.rs:276-314`
- **Severity:** low (documented trade-off at `:267-275`, but the documentation is the only thing
  keeping `evaluate_scaling` correct)
- **Bug class:** atomic-coherence (acknowledged in comments)
- **What:** Independent swaps of `events_in_window`, `push_latency`, `flush_latency`,
  `current_len`. A `record_push` between the `events_in_window.swap(0)` and the
  `push_latency.swap(0)` increments the latter without the former — its event is counted in window
  N but its latency in window N+1. The packed `(count, sum)` change already closed the worst form
  of this (per `mapper.rs:114-127`), but the cross-field desync remains.
- **Scenario:** A scaling tick at high load reports `event_rate = K` with `avg_push_latency_ns`
  reflecting `K + delta` calls — a directionally-wrong avg under sustained load.
- **Fix sketch:** Acceptable today (commented), but a `RwLock`-free way would be to put all four
  counters behind a single `AtomicU64` "version" used as a seqlock from the reader side.

---

## Null results

- **Atomic ordering on the shutdown handshake** (F-1 conceptually but mechanically) — the
  `try_enter_ingest` SeqCst fetch_add + SeqCst load(shutdown) + SeqCst spin in `shutdown_via_ref` +
  SeqCst release of `drain_finalize_ready` is correct. The drain worker's `Acquire` load on
  `drain_finalize_ready` transitively orders the producer's push (under the shard mutex's release
  / acquire) before the drain worker's `pop_batch_into`. The deadline-elapsed path documents its
  data-loss window honestly. **No bug.**
- **`RingBuffer` SPSC atomics** — `head` Relaxed load on the producer is paired with `head` Acquire
  load on the consumer; consumer's `tail` Relaxed load is paired with producer's `tail` Acquire
  load. Stores are Release on both sides. The wrapping arithmetic is u64-safe (won't wrap in
  practice). `len()` reads both sides Acquire; possible to observe a transient len > capacity-1 if
  a producer is mid-push, but `is_full` uses `>=` and `try_push` is the only mutator. **No bug.**
- **Lock-across-await** — I searched for `.lock()` patterns crossing `.await` boundaries.
  `parking_lot::Mutex` is held briefly across non-await code (e.g. `batch_workers.lock()` is
  pulled out via `std::mem::take` before the await in shutdown). `batch_senders` is `parking_lot::RwLock`
  but only used for `.write()` insert / `.write()` remove / `.write()` take. Likewise the rebuild
  paths use `std::mem::take` / drop before await. **No bug.**
- **Tokio spawn lifecycle** — every `tokio::spawn` in the bus has its `JoinHandle` captured
  (batch worker, drain worker, scaling monitor) and either awaited in `shutdown` (with bounded
  timeouts and explicit JoinError logging) or held in `scaling_monitor: Mutex<Option<JoinHandle<()>>>`
  for `shutdown_via_ref` to await. Drop signals shutdown via the atomic flag rather than detaching,
  and surfaces stranded counts via `events_dropped`. **No bug.**
- **Lost wakeups** — no `Notify` / `AtomicWaker` is in scope; backpressure is purely synchronous
  via `try_push`. The drain loop uses `tokio::time::sleep(100µs)` rather than a notifier; the
  trade-off is documented (`bus.rs:2356-2363`). **No bug.**
- **AdaptiveBatcher saturating arithmetic** — all multiplications / adds are saturating; the
  `velocity_samples` deque is capped both by time and by `VELOCITY_SAMPLES_CAP`. **No bug.**
- **Mapper `scale_up` / `scale_down` cooldown ↔ mutation atomicity** — the rebuild lock and the
  `shards.write()` guard properly serialize the read-modify-write of `last_scaling` with the state
  transition (documented at `mapper.rs:430-446`). **No bug, but the comment is the load-bearing
  invariant — see field doc.**

---

## Priority summary

| ID  | Severity | Where                                                            |
|-----|----------|------------------------------------------------------------------|
| F-1 | critical | `shard/mod.rs` push paths never call metrics_collector hooks      |
| F-2 | medium   | `bus.rs::manual_scale_down` wall-clock deadline                  |
| F-3 | medium   | `bus.rs` drain worker `finalize_deadline` wall-clock              |
| F-4 | medium   | `EventBus::Drop` locks every shard via `total_pending_in_rings`   |
| F-5 | low      | `add_shard_internal` publish ordering (documented invariant only) |
| F-6 | low      | Lossy-shutdown reconciliation off-by-one under exact interleave   |
| F-7 | low      | DropOldest stats decrement-then-increment window                  |
| F-8 | low      | `collect_and_reset` cross-field non-atomic (commented)            |

Word count: ~1480.
