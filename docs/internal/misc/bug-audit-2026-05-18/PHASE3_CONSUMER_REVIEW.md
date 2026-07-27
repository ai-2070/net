# Phase 3 — Consumer / Poll Layer Audit

**Scope:** `net/crates/net/src/consumer/` (`mod.rs`, `filter.rs`, `merge.rs`).
Cross-referenced `net/crates/net/src/bus.rs` (lines 1023-1056) and tests
`tests/ffi_poll_buffer.rs`, `tests/ffi_shutdown_race.rs`,
`tests/bus_shutdown_drain.rs`.

## Bug-class coverage summary

| Class | Verdict |
|---|---|
| 1. `tokio::select!` cancellation safety | **None present.** `grep` for `tokio::select`/`select!` across `src/consumer/` returns zero matches. Concurrency is `futures::future::join_all` (`merge.rs:513`), which is cancel-safe at the outer boundary. |
| 2. Partial-read state | **None reachable from consumer.** No buffered "committed but not returned" state — the cursor is computed from the full poll result before `poll()` returns. |
| 3. Ack / nack double-counting | **N/A.** Consumer layer is at-least-once *delivery* via cursor pagination; there is no ack/nack channel on this surface. |
| 4. Backpressure deadlocks | **None.** Consumer never `await`s a bus-side permit; it calls `adapter.poll_shard` and the bus calls `merger.poll`. No cycle. |
| 5. Poll-after-shutdown | **Handled at FFI.** `FfiOpGuard` (bus side, out of scope) gates this; pinned by `tests/ffi_shutdown_race.rs`. The merger itself has no shutdown coupling. |
| 6. Filter / merge correctness | See F-1, F-2, F-3 below. |
| 7. Cursor tracking | See F-4. Cursor advances through `compare_stream_ids` CAS in production `poll()` via the `nc.set(shard_id, next_id)` + Step 1/2 override; rebalance handled by bus `ArcSwap`. |
| 8. Resource leak on drop | **None observable in consumer.** Dropping a `poll()` future drops the inner `join_all`; per-shard adapter futures are cancelled at the next `.await`. Whether adapters leak permits is out of scope. |
| 9. `tokio::spawn` discipline | **None present.** `grep` for `tokio::spawn`/`spawn_blocking` across `src/consumer/` returns zero matches. |

The "51 unwrap/expect calls in merge.rs" cited in the brief are **all
inside `#[cfg(test)]`** except two infallible call sites in
`filter.rs:186, 195` (`FilterBuilder::build_and/build_or` after a
`len()==1` guard — unreachable). No adversarial-input panic path
exists.

## Findings (severity-ordered)

### F-1 — Duplicate `shard_id` in `request.shards` double-fetches and (with filter+ordering) double-counts
- **File:line:** `src/consumer/merge.rs:469-513` (`poll`).
- **Severity:** medium
- **Bug class:** filter/merge correctness, dedup.
- **What:** `let shards: Vec<u16> = request.shards.clone().unwrap_or_else(...)` is consumed verbatim. If a caller passes `vec![0, 0, 1]`, the merger spawns two identical `poll_shard(0, from, per_shard_limit)` calls. Both return the same events. After `extend()`, `all_events` contains every shard-0 event twice; the `retain` filter pass keeps both copies; `Ordering::InsertionTs` sort places the duplicates adjacent; `truncate(limit)` may return both. The cursor override step (`merge.rs:746-760`) sees the duplicate and writes the same id twice — idempotent on the cursor, but the caller still received duplicate events in this poll's payload.
- **Scenario:** Any caller (Rust API, FFI, SDK shim) that constructs `ConsumeRequest::shards(vec![…])` from a deduplicated source incorrectly — e.g. a router that fans out to shard 0 for two logical topics and merges via `ConsumeRequest`. Also reachable from an FFI client that user-supplies the shard list.
- **Fix sketch:** After cloning, `shards.sort_unstable(); shards.dedup();` before the `is_empty()` check. Cheap, semantically lossless.

### F-2 — `CompositeCursor::update_from_events` is `pub` but only callable by tests; production `poll()` uses a divergent code path
- **File:line:** `src/consumer/merge.rs:223-272` (definition) vs `merge.rs:558-560`, `746-760` (production cursor advance).
- **Severity:** medium
- **Bug class:** API hazard / divergence between documented and effective behavior.
- **What:** `update_from_events` is documented as the cursor-advance primitive and contains the only call site of `compare_stream_ids` for CAS (decade-rollover safe, format-mismatch-refuses). But the production `poll()` does **not** call it — it uses unconditional `nc.set(shard_id, next_id)` at `:559` (where `next_id` is whatever the adapter returns) and the Step 2 override at `:753` (`final_cursor.set(event.shard_id, event.id.clone())`). Neither of those calls goes through `compare_stream_ids`. The protections `update_from_events` advertises (no regression on out-of-order id slice, no advance across backend-format change) **do not apply to the cursors returned from `poll()`**. They apply only to a hypothetical caller using the cursor API by hand — which the test suite is the sole user of.
- **Scenario:** An adapter mid-migration that returns a `next_id` in Redis format while the prior cursor was JetStream numeric: production `poll()` writes the Redis id straight into `nc` and returns it. The pre-fix wedge that `update_from_events` defends against re-appears via the production path. The advertised "loud `tracing::error!` + refuse to advance" never fires.
- **Fix sketch:** Either route the production `nc.set` / Step 2 override through `update_from_events` (or a private equivalent that takes a single `(shard_id, id)`), or downgrade `update_from_events` to `pub(crate)` / `#[doc(hidden)]` and remove the "primary cursor advance" framing from its rustdoc so callers don't believe the protections apply.

### F-3 — `Ordering::None` + filter path can return events out of stream order, breaking caller's "monotone-by-cursor" expectation
- **File:line:** `src/consumer/merge.rs:605-611, 685-760`.
- **Severity:** low
- **Bug class:** ordering invariant.
- **What:** With `Ordering::None` and a filter, `all_events` is the concatenation of per-shard `events` lists in `join_all` order. After filtering, `truncate(limit)` keeps the first `limit` matches. The Step 2 override iterates `all_events.iter().rev()` and writes the last returned event id per shard to the cursor. If `join_all`'s output Vec order does not match stream order across shards (it does within a shard but not across), then a caller comparing event ids across consecutive polls observes an apparent non-monotone delivery — even though the cursor IS monotone per shard. This is not a delivery bug (no skip / dup), but `Ordering::None` is undocumented as "arbitrary across shards" and the cursor-based pagination tests assume the merger returns events in shard-grouped batches. A caller building a UI on top will see jitter.
- **Scenario:** Two-shard poll, both shards have a matching event, `limit=2`, `Ordering::None`. Poll N returns `[shard0_ev1, shard1_ev1]`; poll N+1 returns `[shard1_ev2, shard0_ev2]` (because `join_all` happened to finish shard 1 first this round). No correctness loss, just cross-shard non-determinism.
- **Fix sketch:** Document explicitly in `Ordering::None`'s rustdoc that cross-shard interleaving is non-deterministic across polls. Or add a stable per-shard secondary sort even in `None` mode.

### F-4 — `failed_shards` failure path silently widens the cursor's effective fetch window on the next poll
- **File:line:** `src/consumer/merge.rs:567-577, 685-720`.
- **Severity:** low
- **Bug class:** cursor-window inflation under partial failure.
- **What:** When shard X errors mid-poll, it is pushed to `failed_shards` and skipped. Its cursor entry in `cursor.positions` is preserved (never advanced for this poll, correct). But on the next poll the same fetch budget (`per_shard_limit` computed from `limit / shards.len()`) is applied to shard X starting at its old cursor — which may now have more accumulated events than `per_shard_limit`. If shard X stays errored for several polls and then recovers, the recovery poll's `per_shard_limit` is unchanged, so it under-delivers, and the caller doesn't know to retry with a larger limit. Combined with the silent `PER_SHARD_FETCH_CAP` clamp (now surfaced as `truncated_at_per_shard_cap` — that fix landed), the caller has the signal for cap-truncation but no signal for "this shard's backlog grew while errored." 
- **Scenario:** 4-shard poll, shard 2 errors continuously for 60 s while shards 0/1/3 keep ingesting. When shard 2 recovers, its backlog is far larger than `limit/4 * over_fetch_factor`; caller sees a slow drain proportional to poll cadence.
- **Fix sketch:** When `failed_shards` is non-empty, increase the surviving shards' or the recovering shards' `per_shard_limit` on subsequent polls. Or surface a `recovered_shard_backlog` hint on the response when a shard reports `has_more=true` *and* was in the prior poll's `failed_shards`. Minimum: document the recovery latency in `ConsumeResponse::failed_shards`'s rustdoc.

## What's solid

- Cursor compare (`compare_stream_ids`) is format-aware and decade-safe — verified by `cursor_does_not_wedge_on_jetstream_decade_rollover` and `cursor_does_not_wedge_on_redis_seq_decade_rollover`.
- Non-canonical shard-key alias attack (`"00"` vs `"0"`) is rejected at decode (`merge.rs:178-191`).
- `has_more` suppression-on-stall (`merge.rs:780`) correctly prevents the infinite-loop polled-from-same-cursor pattern.
- Filter-path rollback+override (`merge.rs:697-760`) drains correctly without re-delivery — pinned by `poll_merger_does_not_stall_on_single_shard_filter_under_cap` and the multi-shard variant.
- Per-shard cap truncation is surfaced (`truncated_at_per_shard_cap`).
- FFI `net_poll` buffer-size race and `net_free_poll_result` double-free, both pinned by `tests/ffi_poll_buffer.rs`, still hold by inspection of the documented preconditions.
- Filter recursion is bounded by `serde_json`'s default depth (128) and `lib.rs`'s `recursion_limit = "256"`; pinned by `from_json_rejects_adversarially_nested_filter` and `matches_handles_modest_depth_on_small_stack`.
- Empty-`And` filter no longer matches everything (`filter.rs:106-108` + `test_empty_and_filter`).

## Files inspected

- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/src/consumer/mod.rs`
- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/src/consumer/filter.rs`
- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/src/consumer/merge.rs`
- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/src/bus.rs` (lines 1023-1056 only)
- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/tests/ffi_poll_buffer.rs`
- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/tests/ffi_shutdown_race.rs`
- `C:/Users/chief/Desktop/github/cyberdeck/net/crates/net/tests/bus_shutdown_drain.rs`
