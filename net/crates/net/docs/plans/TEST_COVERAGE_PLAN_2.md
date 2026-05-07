# Test Coverage — Round 2 Gaps

## Context

The `test-coverage` branch has shipped the entire P1 block from `net/crates/net/docs/TEST_COVERAGE_PLAN.md` plus most P2 / P3 items (capability GC, multi-hop dedup, rendezvous staleness, peer-death cleanup, signed-but-invalid forwarding, sequential mapper fallback, malformed subprotocol, classify FSM, NAT-PMP partial-packet, channel/guard, identity/token, route, migration target failure, ABI stability for Go/Node/Python). What remains are higher-severity holes the original audit didn't enumerate — primarily in the FFI shutdown protocol, the bus drain path, recovery semantics on RedEX, and a small dead-code surface that's silently bug-prone. The goal of this round is to pin those specific invariants with named tests.

All seven gaps below were verified by reading the current source on this branch (file/line refs accurate as of HEAD). Estimated total effort: ~2 days.

---

## Scope and ordering

Tests written in priority order. Each one stands alone — feel free to ship as separate PRs or stack them on this branch.

### 1. FFI shutdown / in-flight Dekker handshake — **Priority 1, M**

**Surface:** `net/crates/net/src/ffi/mod.rs:120-156` (`FfiOpGuard::try_enter` + `Drop`) and `:820-859` (`net_shutdown` spin-wait).

**Gap:** the SeqCst Dekker handshake between `shutting_down` and `active_ops` is the single load-bearing primitive preventing use-after-free across every language binding. Verified zero coverage: no `#[test]` references `FfiOpGuard`, `active_ops`, or `shutting_down` in either `src/ffi/mod.rs::tests` (which only covers config parsing) or `tests/`. Bindings smoke-test happy paths but never race shutdown against in-flight ops.

**Test:** new file `net/crates/net/tests/ffi_shutdown_race.rs`.
- Build a `NetHandle` via `net_init(NULL)`.
- Spawn 8 OS threads, each calling `net_ingest_raw_ex` / `net_poll_ex` in a 2_000-iteration loop on the same handle.
- Spawn one shutdown thread that sleeps a randomized 0–5 ms and calls `net_shutdown`.
- After shutdown returns, every subsequent in-flight op MUST get `NET_ERR_SHUTTING_DOWN` (-8).
- Test passes if the process exits cleanly with no segfault and no thread observes a return code outside `{Success, ShuttingDown}`.
- Repeat the whole sequence 50 times to surface ordering races.

**Why M effort:** thread orchestration around extern "C" pointers requires care, but no fixtures.

---

### 2. `EventBus::shutdown` drain race / silent message loss — **Priority 1, M**

**Surface:** `net/crates/net/src/bus.rs:646-668` — the post-shutdown drain loop in `spawn_batch_worker`. The inline comment on lines 651-655 calls out the exact failure mode: "final events sent by the drain worker during its shutdown sweep could sit in the channel buffer and be silently dropped."

**Gap:** existing tests in `src/bus.rs` (`test_event_bus_basic` etc.) assert `events_ingested` counters but never compare them to **what the adapter actually received**. No test exercises the drain race or `dispatch_batch`'s retry branch. There's no in-tree counting adapter.

**Test:** new file `net/crates/net/tests/bus_shutdown_drain.rs`. Helper test fixture `CountingAdapter` (defined in the test file itself, `impl Adapter`) wraps an `Arc<AtomicU64>` and increments per event seen.

Cases:
- `shutdown_delivers_all_pending_events`: ingest 10_000 events on a 4-shard bus, immediately call `bus.shutdown().await`, assert counter == 10_000. Run 50 iterations.
- `shutdown_drains_after_flag`: throttle the adapter with a `tokio::time::sleep(1ms)` per batch to ensure events sit in the channel when shutdown fires. Same 10_000-event assertion.
- `dispatch_batch_retries_on_transient_error`: a `FlakyAdapter` returning `Err` for the first call per batch and `Ok` thereafter. Assert no event is duplicated AND no event is dropped — exact-once delivery for the configured `batch_retries`.

**Why M effort:** new test adapter fixtures, but pattern is mechanical.

---

### 3. `BackpressureMode::Sample` is dead-on-arrival — **Priority 2, S**

**Surface:** `net/crates/net/src/shard/mod.rs:367-370` and `:417-420`.

**Gap:** verified — only 4 source references for `BackpressureMode::Sample` / `IngestionError::Sampled` exist in the whole crate (the two return points, the `to_string` impl in `error.rs`, and the SDK pass-through in `sdk/src/config.rs:29`). Zero tests construct a `Sample{rate}`. The "Sampling is handled at a higher level" comment refers to a higher level that doesn't exist. Any user setting `Sample { rate: 0.5 }` gets `Sampled` errors 100% of the time when the buffer fills, indistinguishable from `Backpressure` to SDK callers.

**Test:** add `sample_mode_contract_is_pinned` to `src/shard/mod.rs::tests` (or wherever `ShardManager` tests live). Construct `ShardManager::new(1, 4, BackpressureMode::Sample { rate: 0.5 })`, fill the buffer, ingest one more event. Assert one of:
- (current behavior) returns `IngestionError::Sampled`, **and** add a one-line comment `// TODO: Sample is currently a rejection signal; revisit when sampling is wired through.`
- OR (if the team decides to actually implement sampling) returns `Ok` with rate-proportional probability — in which case the implementation has to land first.

The test makes the contract explicit either way. Followup work to wire real sampling or remove the variant is out of scope for this round.

**Why S effort:** ~30 min once the contract decision is made.

---

### 4. `ffi/cortex.rs` watch cursors + runtime `OnceLock` race — **Priority 2, M**

**Surface:** `net/crates/net/src/ffi/cortex.rs` — verified 0 `#[cfg(test)]` blocks across the entire 1481-line file with 37 `pub extern "C" fn` exports.

**Gap:** the cortex FFI is the persistence + memories + tasks plane consumed by the Go SDK. Bindings exercise happy paths but not: (a) `net_tasks_watch_next` after `net_tasks_watch_free`, (b) `redex_open_file` config validation (conflicting `fsync_every_n` AND `fsync_interval_ms`), (c) underlying `RedexHandle` drop while a watch cursor is live, (d) cold-start race on `runtime()` `OnceLock` from N threads.

**Test:** add `mod tests` at the bottom of `src/ffi/cortex.rs`, gated `#[cfg(all(test, feature = "netdb", feature = "redex-disk"))]`. Cases:
- `redex_open_file_rejects_conflicting_fsync_config` — table-driven over invalid config combinations, every row returns `NET_ERR_REDEX`.
- `tasks_watch_next_after_free_returns_stream_ended` — open watcher, free it, calling `watch_next` on the freed cursor returns `NET_ERR_STREAM_ENDED` cleanly (no panic, no UAF). Use a stale ptr deliberately, with `MIRI_BACKTRACE=full` documented in a comment.
- `tasks_watch_survives_underlying_handle_drop` — verify the `Arc` lifetime model: watcher keeps inner alive, `watch_next` returns events queued before drop, then `STREAM_ENDED`.
- `runtime_first_call_race` — 16 threads simultaneously calling `net_redex_new` (cold). All succeed; all `RedexHandle`s observe the same runtime instance pointer.

**Why M effort:** lots of unsafe pointer plumbing but each case is small.

---

### 5. RedEX externally-truncated dat with intervening inlines — **Priority 2, M**

**Surface:** `net/crates/net/src/adapter/net/redex/disk.rs:127-160` — the backward index walk that handles external dat truncation (the comment on lines 112-126 explicitly walks through this scenario).

**Gap:** existing tests cover `test_torn_idx_tail_is_truncated_on_reopen` (idx-side) and `test_disk_inline_entries_skip_dat_file` (inlines OK). The compound case — externally truncated dat with **intervening inlines** between the surviving heap entry and the truncated tail — is not pinned. Per the comment, the implementation walks backward and skips inlines, so an inline at index position `n` and a heap entry at position `n+1` whose dat is gone must drop only `n+1` while keeping the inline. Untested.

**Test:** add to existing `src/adapter/net/redex/disk.rs::tests`:
- `test_external_dat_truncation_drops_torn_heap_after_inline` — append `[heap1, inline1, heap2, inline2, heap3]`; close; truncate dat externally to `len(heap1) + len(heap2)` (kills heap3 only); reopen; assert recovered index == `[heap1, inline1, heap2, inline2]` AND dat file size == `len(heap1) + len(heap2)`.
- `test_external_dat_truncation_drops_all_subsequent_heap_keeps_inlines` — same setup, truncate dat to `len(heap1)` only; assert recovered index keeps `heap1` and any inlines BEFORE `heap2` (i.e., none in this layout, but verify the truncation reaches all the way back to `heap2`).

**Why M effort:** tmpdir + manual `set_len` on the dat file, fixtures already in place from the existing tests.

---

### 6. `Filter::matches` recursion DoS — **Priority 3, S**

**Surface:** `net/crates/net/src/consumer/filter.rs:88-99` — recursive `matches` with no depth bound; `Filter::from_json` (`:101-104`) deserializes untrusted input with no `recursion_limit` enforcement.

**Gap:** all 29 existing filter tests use depth ≤ 3 (verified — only matches for `nested` are JSON-path-nested, not filter-nested). `Filter::from_json` is reachable from any FFI / SDK path that accepts a filter, so an adversarial 10_000-deep `$not($not(...))` filter can crash the bus thread.

**Test:** in `src/consumer/filter.rs::tests`, add `deeply_nested_filter_does_not_overflow_stack`. Construct a depth-10_000 filter via repeated `Filter::Not { filter: Box::new(prev) }` wrapping. Spawn evaluation on a thread with a small stack (`std::thread::Builder::new().stack_size(256 * 1024)`). Assert one of:
- evaluation returns a `bool` without overflow (stack-safe today), OR
- `Filter::from_json` rejects the JSON form with a parse-depth error.

If both fail, the test exposes a real bug — fix in the same PR by adding a depth counter to `matches` (e.g., wrap in `matches_with_depth(d: u32)` capped at 256, the same as `lib.rs:55` `recursion_limit`).

**Why S effort:** ~1 hour, plus possible production fix.

---

### 7. Scaling cooldown race — **Priority 3, S-M**

**Surface:** `net/crates/net/src/shard/mapper.rs:519-527` (`scale_up` cooldown check) and `:594-602` (`scale_down`). Verified: the cooldown read is OUTSIDE the `shards.write()` lock, while the `max_shards` re-check is INSIDE. The double-check pattern was added for `max_shards` but never extended to `last_scaling`.

**Gap:** existing test `test_scale_up_max_shards_concurrent` covers the `max_shards` lock race. Cooldown semantics under concurrency are not tested. Two concurrent `scale_up` calls can both pass the cooldown check (read-lock-only, dropped) and both succeed, blowing past the cooldown and potentially the `max_shards` cap as a side effect (since both succeed before either updates `last_scaling`).

**Test:** in `src/shard/mapper.rs::tests`, add `cooldown_is_enforced_under_concurrent_scale_up`. Build a `ShardMapper` with `cooldown = Duration::from_millis(100)`, headroom for both calls. Spawn 2 threads each calling `scale_up(1)` simultaneously, with a barrier to maximize overlap. Assert: across 1_000 iterations, in every iteration exactly ONE call returns `Ok` and the other returns `InCooldown` (or both fail with `AtMaxShards` if at the cap). NEVER both `Ok`.

If the assertion fails (likely — there's no synchronization protecting cooldown), the production fix is to move the cooldown check inside the write lock alongside the `max_shards` re-check, and update the `last_scaling` store under the same lock. Land the fix and the test together.

**Why S-M effort:** ~2-3 hours including the production fix.

---

## Critical files

- `net/crates/net/src/ffi/mod.rs` (read for context, no edits)
- `net/crates/net/src/bus.rs` (read for context, no edits)
- `net/crates/net/src/shard/mod.rs` (test-only edits to the existing `mod tests`)
- `net/crates/net/src/shard/mapper.rs` (test-only edit; production fix likely)
- `net/crates/net/src/ffi/cortex.rs` (add `mod tests` at the bottom)
- `net/crates/net/src/consumer/filter.rs` (test-only edit; production fix possible)
- `net/crates/net/src/adapter/net/redex/disk.rs` (test-only edit to existing `mod tests`)

New integration test files:
- `net/crates/net/tests/ffi_shutdown_race.rs`
- `net/crates/net/tests/bus_shutdown_drain.rs`

## Reused patterns / fixtures

- Concurrent stress harness: `tests/reflex_override.rs::override_set_clear_is_atomic_with_announce_read` (the N-iteration toggler + observer pattern called out in `docs/TEST_COVERAGE_PLAN.md`).
- Mock adapter pattern: existing `NoopAdapter` in `src/adapter/noop.rs` — extend rather than reinvent for the `CountingAdapter` / `FlakyAdapter` test fixtures in gap #2.
- RedEX recovery test scaffolding: `src/adapter/net/redex/disk.rs::tests::test_torn_idx_tail_is_truncated_on_reopen` already builds the tmpdir + reopen flow.
- ABI feature-gating: see `src/ffi/mod.rs::tests` for the `#[cfg(...)]` guard convention.

## Verification

For each gap, run from the workspace root:

```bash
# Gap 1
cargo test -p net --test ffi_shutdown_race --release -- --test-threads=1 --nocapture
# Gap 2
cargo test -p net --test bus_shutdown_drain --release
# Gap 3 (and 7)
cargo test -p net --lib shard::
# Gap 4
cargo test -p net --lib --features "netdb redex-disk" ffi::cortex::tests
# Gap 5
cargo test -p net --lib redex::disk::tests
# Gap 6
cargo test -p net --lib consumer::filter::tests::deeply_nested
```

Whole-crate sanity: `cargo test -p net --all-features` and `cargo build -p net --all-features --release` should remain green.

For gaps that expose real bugs (#3 contract decision, #6 stack overflow, #7 cooldown race), the production fix lands in the same commit as the test that exposes it. Do not commit a failing test alone.

## Out of scope

- Loom / Shuttle deterministic concurrency testing (per the existing `TEST_COVERAGE_PLAN.md` non-goals).
- Property-based / fuzzing infrastructure (same).
- `event.rs::StoredEvent::Serialize` invalid-JSON branch (`src/event.rs:374-391`) — unreachable in practice; explicitly skipped in the audit.
- Coverage percentage targets — this plan only adds tests that pin specific invariants.
