# RedEX Disk — Throughput Plan

> Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md). Targets the persistent-segment hot path under `net/crates/net/src/adapter/net/redex/disk.rs`. Out of scope: anything in [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) (warm tier, mmap, replication).

## Status

| Phase | State | Notes |
|---|---|---|
| 1 — Coalesce batch syscalls | ✅ Shipped | `disk.rs::append_entries_inner` rewritten; bench `bench_append_batch_disk` added |
| 2 — Heap-side `append_many` | ✅ Shipped | `HeapSegment::append_many(&[Bytes])` added; `append_batch` and `append_batch_ordered` rewired |
| 3 — `EveryN` fsync off the appender | ✅ Shipped | `fsync_signal` Notify; worker spawned in `open_persistent` |
| 4 — Byte-threshold trigger / `IntervalOrBytes` | ✅ Shipped | Additive variant; signal-driven (not polling); `bytes_since_sync` reset inside `sync()` |
| 5 — Single-append coalescer | ⏸ Deferred | Gated, speculative; do not start until 1–4 are benched |

Test count after phases 1–4: **91 redex tests passing** (was 75 before Phase 1).
Benches added but **not yet run** for before/after capture.

## Goals

1. **Batch appends** — group many serialized entries into a single `write()` per file (`dat`, `idx`, `ts`); amortize syscalls and let the kernel coalesce I/O. Plays well with `O_APPEND`.
2. **Strictly sequential writes** — keep `dat` append-only; keep `idx` append-only; defer compaction/merge to background tasks. No seeks on the hot path.
3. **Minimize fsyncs on the hot path** — use `FsyncPolicy` (`Never` / `Interval` for high-throughput, `EveryN` for tighter durability) and move fsyncs to a background task that flushes when **either** a time interval passes **or** buffered bytes exceed a threshold.

## Baseline (pre-Phase-1, for reference)

The bullets below describe the codebase **before** Phases 1–4. The Phase sections describe what changed.

- `disk.rs::append_entries_inner` held each file's `Mutex<File>` and looped `write_all` per entry. A batch of N events emitted **3 · N** syscalls (one per entry per file) instead of 3.
- `disk.rs::append_entry_inner` issued 3 sequential `write_all` calls per logical event (one per file).
- `dat`, `idx`, `ts` were already append-only on the hot path. The only `set_len` calls are rollback paths after a write error and the recovery-time tail-truncation in `DiskSegment::open`.
- `maybe_sync_after_append` called `self.sync()` **synchronously on the appender thread** when `EveryN` fired.
- `FsyncPolicy::Interval(d)` was driven by a per-file tokio task that wakes every `d` and calls `disk.sync()`. There was no byte-threshold trigger.
- `HeapSegment::append` (`segment.rs:66`) took one payload at a time. `file.rs::append_batch` wrote one batched syscall to disk via `append_entries_at`, then looped over the heap side per entry.

---

## Phase 1 — Coalesce batch syscalls into one `write_all` per file ✅

**Shipped:** `disk.rs::append_entries_inner` rewritten as designed. `dat_pre_len` is now `Option<u64>` so all-inline batches skip the dat lock entirely. Write order, lock order, and rollback discipline preserved.

**Tests added:**
- `test_disk_batch_mixed_heap_and_inline_roundtrip` — pins inline payloads stay out of `dat_buf`.
- `test_disk_batch_all_inline_skips_dat` — exercises the new `Option::None` branch.

**Bench added:** `bench_append_batch_disk` (BATCH=64 × 64 B and × 1 KiB) — wired into the `redex-disk` criterion group. Not yet run for before/after numbers.

**Touches:** `disk.rs::append_entries_inner` only.

Rewrite the per-entry loops to build one contiguous buffer per file in a single pass, then issue one `write_all` per file:

```rust
let total_payload: usize = entries_and_payloads.iter()
    .filter(|(e, _)| !e.is_inline())
    .map(|(_, p)| p.len()).sum();
let mut dat_buf = Vec::with_capacity(total_payload);
let mut idx_buf = Vec::with_capacity(entries_and_payloads.len() * REDEX_ENTRY_SIZE);
let mut ts_buf  = Vec::with_capacity(timestamps.len() * 8);
for ((e, p), t) in entries_and_payloads.iter().zip(timestamps) {
    if !e.is_inline() { dat_buf.extend_from_slice(p); }
    idx_buf.extend_from_slice(&e.to_bytes());
    ts_buf.extend_from_slice(&t.to_le_bytes());
}
// dat.write_all(&dat_buf), idx.write_all(&idx_buf), ts.write_all(&ts_buf)
```

Constraints to preserve:

- **Write order:** `dat → idx → ts`. The recovery torn-tail logic in `DiskSegment::open` (lines 142–186, 207–212) depends on dat being durable before idx/ts; do not reorder.
- **Rollback discipline:** capture `pre_len` per file, `set_len` back on error. Whether `write_all` is one syscall or many, the partial-write hazard is the same — a single `write_all` can still be partially flushed if the kernel returns short.
- **Per-batch lock acquisition order:** `dat → idx → ts` (matches today's pattern; do not introduce overlapping locks).

**Win:** `append_batch(64)` drops from 192 → 3 syscalls.
**Risk:** None functionally; pure refactor.
**Test/bench:**
- Existing `test_disk_append_and_recover`, `test_external_dat_truncation_*`, and `append_failure_after_dat_write_rolls_back_dat` cover correctness.
- Add `bench_append_batch_disk` to `net/crates/net/benches/redex.rs` (mirror `bench_append_batch` at line 131 with `feature = "redex-disk"`).

---

## Phase 2 — Heap-side `append_many` ✅

**Shipped:** `HeapSegment::append_many(&[Bytes]) -> Result<u64, RedexError>` added. Single bounds check, single `reserve`, fused extend loop. Returns the offset of the first payload. Both `append_batch` and `append_batch_ordered` now make one `append_many` call instead of looping per-event.

**Deviation from plan:** the plan suggested an `impl IntoIterator<Item = &'a [u8]>` API and turning `append` into a wrapper around `append_many(once(payload))`. Shipped instead: `append_many` takes `&[Bytes]` directly because both callsites already have that type as their public input, and `append` stays as its own implementation (a one-payload wrapper would have required allocating a 1-element `Bytes` slice or carrying a more complex iterator constraint). Functionally equivalent; cleaner at the callsites.

**Tests added (in `segment.rs`):**
- `test_append_many_basic` — offsets correct after a non-zero starting buffer.
- `test_append_many_capacity_exceeded` — 3 GiB total bounds check rejects without partial extension.
- `test_append_many_empty_returns_current_end` — empty batch is a no-op.

**Touches:** `segment.rs` (new API), `file.rs::append_batch` (line 450), `file.rs::append_batch_ordered`.

After Phase 1, disk-side batching is one syscall per file. The heap-side loop in `file.rs:450–457` is now the bottleneck for large batches.

Add to `HeapSegment`:

```rust
pub fn append_many<'a>(
    &mut self,
    payloads: impl IntoIterator<Item = &'a [u8]>,
) -> Result<(), RedexError>
```

One `reserve` for the total bytes, one fused `extend` pass. `append` becomes a thin wrapper around `append_many(std::iter::once(payload))`.

Update `append_batch` and `append_batch_ordered` to call `append_many` once per batch instead of looping. Watcher notification (the second loop in `append_batch`) stays per-event — it's unrelated to the segment write.

**Risk:** Low; mechanical.
**Tests:** existing batch tests cover; add a microbench comparing `append_many(N)` vs `for _ in 0..N { append() }`.

---

## Phase 3 — Move `EveryN` fsync off the appender thread ✅

**Shipped:**
- `DiskSegment.fsync_signal: Arc<Notify>` added (always allocated; only signaled when a worker is listening).
- `maybe_sync_after_append` notifies instead of running `sync()` inline.
- Worker spawned in `RedexFile::open_persistent` for `FsyncPolicy::EveryN(_)` — selects over `task_signal.notified()` and `task_shutdown.notified()`.
- Renamed `interval_shutdown` → `fsync_shutdown` (one field, both Interval and EveryN workers share the lifecycle).
- `config.rs` rustdoc on `FsyncPolicy::EveryN` updated with the new loss bound.

**Deviation from plan:** plan said `close()` "fires `shutdown.notify_one()` and joins the task." We don't join; `close()` fires the notify and the worker observes it on its next select iteration. Joining would require `tokio::task::JoinHandle` plumbing (not currently held) and would block close on the worker's next poll — unnecessary because the worker drops its `Arc<DiskSegment>` cleanly on exit. The `test_close_releases_worker_disk_reference` test pins this.

**Tests added:**
- `test_fsync_policy_every_n_does_not_block_appender` — 100 EveryN(1) appends in <50 ms (the headline non-blocking invariant).
- `test_fsync_policy_every_n_coalesces_under_burst` — 50 burst notifies → 1 worker sync (pins single-permit semantics).
- `test_fsync_policy_every_n_worker_survives_sync_error` — armed sync failure: worker logs, doesn't terminate, recovers on the next notify. Required adding `fail_next_sync` injection + `arm_next_sync_failure` test helper to `DiskSegment`.
- `test_close_releases_worker_disk_reference` — `Arc::strong_count` drops from 3 → 2 after close (worker released its clone).

Existing `test_fsync_policy_every_n_syncs_on_cadence` and `_clamps_zero_to_one` were converted to `#[tokio::test(flavor = "current_thread")]` with explicit yields between appends; added a `wait_for_sync_count` helper.

**Touches:** `disk.rs::DiskSegment` (new field), `disk.rs::maybe_sync_after_append`, `disk.rs::DiskSegment::open` (spawn task), `disk.rs` close path, `config.rs` rustdoc on `FsyncPolicy::EveryN`.

Today an `EveryN(N)` policy blocks every Nth appender for the duration of three `fsync_all` syscalls — milliseconds on rotational disks, hundreds of µs on NVMe. The page-cache write is already done; the appender's caller doesn't gain anything from waiting on the fsync.

Plumbing:

- Add `fsync_signal: Option<Arc<Notify>>` and `fsync_shutdown: Option<Arc<Notify>>` to `DiskSegment`.
- `maybe_sync_after_append` does `signal.notify_one()` instead of calling `sync()` inline. Returns immediately.
- Background task spawned at `DiskSegment::open` time when `fsync_every_n > 0`:
  ```
  loop {
      tokio::select! {
          _ = signal.notified() => { let _ = self.sync(); }
          _ = shutdown.notified() => break,
      }
  }
  ```
  Single in-flight fsync; if multiple appenders signal during one fsync, the next iteration coalesces them — `Notify` is a permit, not a counter, which is exactly the semantics we want.
- `close()` fires `shutdown.notify_one()` and joins the task.

Document the timing change in `config.rs:29` (`FsyncPolicy::EveryN`):

> Fsync is enqueued for a background worker, not run synchronously on the appender. Worst-case loss bound stays at (N − 1) entries since the last sync **point**, plus the (small) window of an in-flight fsync that the crash interrupts.

**Risk:**
- `sync_count` (test-only at line 100) is incremented inside `sync()`, which the background task still calls — semantics preserved.
- A burst of appends followed by an immediate process kill could lose the trailing batch even at small N. The bound is `(N - 1) + in_flight_window` rather than `N - 1`. Document; do not pretend otherwise.

**Tests:**
- New `everyn_does_not_block_appender`: arm a slow-fsync mock, assert `append_entry` returns within microseconds at N=1.
- New `everyn_coalesces_under_burst`: hammer 10k appends at N=1, assert observed `sync_count` is bounded by what the worker could complete in wall time (not 10k).
- Existing tests that assert `sync_count == k` after a known number of appends need a `flush_pending_syncs()` test helper that drives the worker to quiescence first.

---

## Phase 4 — Byte-threshold trigger for `Interval` ✅

**Shipped:**
- `DiskSegment.bytes_since_sync: AtomicU64` and `fsync_max_bytes: u64` added; `DiskSegment::open` signature now takes both `(fsync_every_n, fsync_max_bytes)`.
- `maybe_sync_after_append(applied, bytes_written)` bumps both counters and notifies if **either** crosses its threshold.
- Single-entry path computes `bytes_written = idx(20) + ts(8) + (inline ? 0 : payload.len())`.
- Batch path computes `bytes_written = dat_buf.len() + idx_buf.len() + ts_buf.len()`.
- `sync()` resets `bytes_since_sync` to 0 after the fsyncs succeed (required so a timer-driven sync doesn't leave the byte counter stale).
- `FsyncPolicy::IntervalOrBytes { period, max_bytes }` variant added; new spawn arm in `open_persistent` selects over shutdown / timer / `fsync_signal`.

**Deviation from plan:** plan said the worker should use a "short polling sleep that checks `bytes_since_sync >= max_bytes`." Shipped instead: signal-driven via the existing `fsync_signal` `Notify` (the same channel EveryN already uses). The appender checks the threshold and notifies; the worker awaits the signal. No polling loop; sub-microsecond response when the threshold crosses; cleaner and reuses Phase 3's infrastructure.

**Tests added:**
- `test_fsync_policy_interval_or_bytes_byte_threshold_fires` — 6 yielding appends of 78 B each → exactly 2 byte-threshold syncs at `max_bytes=200`.
- `test_fsync_policy_interval_or_bytes_timer_fires` — `period=50 ms`, `max_bytes=10 MiB`, advance virtual time → ≥ 2 timer syncs.
- `test_fsync_policy_interval_or_bytes_timer_resets_byte_counter` — confirms a timer-driven sync resets the byte counter (sub-threshold append after the tick doesn't accidentally re-trigger).
- `test_fsync_policy_interval_or_bytes_byte_threshold_counts_inline` — pins inline path: 28 B/append, no dat charge; 4 inline appends (112 B) cross at `max_bytes=100`.
- `test_fsync_policy_interval_or_bytes_byte_threshold_counts_batch` — pins batch-path aggregation: 3 × 50 B → 234 total bytes triggers at `max_bytes=200`.
- `test_fsync_policy_interval_or_bytes_zero_max_bytes_disables_byte_arm` — 100 KiB written with `max_bytes=0` produces zero auto-syncs.
- `test_fsync_policy_interval_or_bytes_zero_period_no_worker` — `period=ZERO` falls through the spawn match's guard; `Arc::strong_count == 2` (file + test, no worker), file still functional, `close()` still syncs.

**Touches:** `config.rs` (new variant), `disk.rs` (counter), `file.rs` (background task select).

Add `bytes_since_sync: AtomicU64` to `DiskSegment`:

- Bump by `idx_buf.len() + dat_buf.len() + ts_buf.len()` after a successful append (single or batch).
- Reset to 0 inside `sync()` after the fsyncs succeed.

Extend the policy additively (no breaking change to existing `Interval(d)` callers):

```rust
pub enum FsyncPolicy {
    Never,
    EveryN(u64),
    Interval(Duration),
    /// Fsync when **either** `period` elapses OR `max_bytes` of writes
    /// have accumulated since the last sync, whichever comes first.
    IntervalOrBytes { period: Duration, max_bytes: u64 },
}
```

In the Interval task in `file.rs` (currently a `sleep(period)` loop), replace with a `select!` over a periodic timer and a short polling sleep that checks `bytes_since_sync >= max_bytes` (relaxed load; cheap). Bytes-threshold accuracy is upper-bound, not exact — that's fine; the contract is "at most `max_bytes` of unsynced data."

**Risk:** None for existing `Interval(d)` callers (unchanged variant).
**Tests:**
- `interval_or_bytes_fires_on_byte_threshold`: tiny period + small max_bytes, hammer appends, observe sync count tracks bytes.
- `interval_or_bytes_fires_on_period`: large max_bytes, idle → tick, observe one sync per period.

---

## Phase 5 — Single-append coalescer (speculative, gated)

**Touches:** `file.rs::append`, `file.rs::append_inline`, `disk.rs` (writer task + queue), `config.rs` (new flag).

For workloads with many concurrent **single** appends (no batching at the API level), the disk-side mutex on `dat_file` / `idx_file` / `ts_file` serializes writers. A small per-`DiskSegment` queue can fold concurrent singles into one batched write:

- Each `append_entry_at` enqueues `(entry, payload, ts, oneshot::Sender<Result<()>>)`.
- A dedicated writer task drains up to `K` items (or up to a max-buffered-bytes cap), calls Phase-1-batched `append_entries_inner`, fans out the result to each waiter.

Behavioral changes that matter:

- **Per-call latency goes up** for the un-contended path (one channel hop + scheduler tick) — typically 1–10 µs added.
- The **failure-atomicity contract** in `file.rs:299–305` runs on the appender thread today; with coalescing the disk write happens elsewhere, and the seq-rollback path needs to handle a remote error. Either: (a) appender awaits the oneshot and rolls back seq itself on error, or (b) the writer task signals the file layer to roll back the contiguous seq range. Sketch in design before coding.
- Watcher notify is already off the disk path; unaffected.

Recommendation: build it **gated** behind `RedexFileConfig::coalesce_appends: bool` (default `false`). Ship Phase 1–4 first; flip Phase 5 on only after a bench shows it wins for the target workload.

**Tests:**
- Concurrency stress: 64 tasks × 10k single appends, assert all seqs land contiguously and durably.
- Error fan-out: arm a one-shot disk failure mid-batch, assert every waiter in that batch sees `Err` and seq rolls back exactly.

---

## Cross-cutting: invariants to assert in code review

These are constraints today's code already obeys; future patches must not regress:

1. **No seeks on the hot path.** All append handles use `OpenOptions::new().append(true)`. `set_len` only fires on rollback or recovery, both off the happy path.
2. **Write order is `dat → idx → ts`.** Recovery's torn-tail logic depends on this; reordering is a silent corruption risk.
3. **Lock acquisition order is `dat → idx → ts`.** Acquiring out of order risks deadlock when a future change holds two simultaneously.
4. **`close()` and explicit `RedexFile::sync()` always fsync, regardless of policy.** Phases 3–5 move *append-path* fsyncs around; the explicit barriers stay synchronous.

Add these to the module rustdoc in `disk.rs` so they are visible at the top of the file.

---

## Validation methodology

In `net/crates/net/benches/redex.rs`:

- ✅ `bench_append_disk` — pre-existing single-append baseline (`Never` policy).
- ✅ `bench_append_batch_disk` — Phase 1: 64 × 64 B and 64 × 1 KiB at `Never`.
- ✅ `bench_append_disk_policies` — Phase 3 + 4: single 256 B append across `Never`, `EveryN(1)`, `EveryN(64)`, `Interval(50ms)`, `IntervalOrBytes(50ms, 1 MiB)`. The non-`Never` rows should track `Never` closely; a regression that re-introduced synchronous fsync on the appender would show as a 10x–100x latency jump.
- ✅ `bench_append_batch_disk_policies` — combined Phase 1 + 3 + 4: `BATCH=64 × 64 B` at `Never`, `EveryN(1)`, and `IntervalOrBytes` with a tight `max_bytes=1 KiB` so the byte arm fires every batch.
- ⏳ `bench_append_single_disk_concurrent` — N tasks × M appends, varying N. Phase 5 target.

**None of the disk benches have been run yet for before/after capture.** Plan: run on the merged branch and post numbers in the PR description.

Capture before/after numbers per phase in the PR description. Expected:

- **Phase 1:** single-digit-multiplier win on `bench_append_batch_disk` (3× to 10× depending on payload size; smaller payloads benefit more from syscall amortization).
- **Phase 2:** modest win (1.2–2×) on the same bench; more important as a correctness/clarity improvement.
- **Phase 3:** p99 single-append latency drops by the fsync duration. p50 unchanged on already-fast disks; large drop on slow disks.
- **Phase 4:** no throughput change; durability bound becomes "either time or bytes," giving operators a knob for bursty workloads.
- **Phase 5:** wins only above some concurrency threshold (likely 8+ concurrent single-appenders); under-contended workloads should not regress because the flag defaults off.

Re-run the full disk recovery test suite after each phase:

```
cargo test -p net --features redex-disk --test redex_disk -- --nocapture
```

---

## Out of scope

- **Compaction off the state lock.** `sweep_retention` holds the file-state lock across `compact_to`. Real win to address, but it's a retention problem, not a hot-path-write problem. Track separately.
- **Replacing `parking_lot::Mutex<File>` with a lock-free ring.** Unnecessary; after Phase 1 the lock is held for one `write_all` per batch, not per entry.
- **Switching to `O_DIRECT` / `io_uring`.** Big architectural shift; revisit only if Phase 1–5 leave throughput on the table.
- **Persisting heap-side timestamps differently.** The `ts` sidecar already covers this; v2's mmap tier supersedes the question.

---

## Sequencing — as shipped

Phases 1–4 landed in a single working tree on the `redex-disk` branch rather than as four separate PRs (the original recommendation). The phases are independently rebaseable if a reviewer wants to split them; no later phase depends on a structural choice made in an earlier one that wouldn't hold up under bisection.

Remaining work:

- **Run benches** to capture Phase 1 / 2 / 3 before-after numbers (the test suite pins correctness; benches quantify the win).
- **Phase 5 (gated, opt-in)** — separate design discussion before code; do not start until benches show whether the existing batched-disk path leaves single-append throughput on the table under contention.
