# BUG #153 + #154 — Stranded-Flush Sequence Collision and Premature Finalization

Resolves the two outstanding entries from `BUG_AUDIT_2026_04_30_CORE.md:908` and `:918`.
They share a code path (`bus::remove_shard_internal`), share a fix
(await the BatchWorker handle and capture its final `next_sequence`),
and ship together.

## Problem

### #153 — `sequence_start = 0` collision

`bus.rs:450` builds the stranded-flush batch with a hardcoded
`sequence_start = 0`:

```rust
let batch = crate::event::Batch::new(shard_id, stranded, 0);
```

Every `BatchWorker` for the same `shard_id` ALSO starts at sequence 0
(`shard/batch.rs:163`). The JetStream adapter builds
`Nats-Msg-Id = {nonce}:{shard_id}:{sequence_start}:{i}`
(`adapter/jetstream.rs:281`). The shard's original first batch wrote
msg-ids `{nonce}:{sid}:0:{i}`. The stranded-flush batch writes the
same ids. JetStream's default 2 min dedup window silently discards
them — the events that the post-#47 fix went out of its way to
recover are then thrown away by the adapter.

Severity: **high**. Extension of #9 / #17 / #56 to the
`remove_shard_internal` path, which post-#47 fix is the *primary*
place stranded events meet the adapter.

### #154 — `finalize_draining` predicate misses in-flight events

`shard/mapper.rs:957`:

```rust
if fill_ratio == 0.0 && pushes_after_drain == 0 { ... }
```

Both probes look only at the ring buffer / producer-side counter.
The drain worker pumps from the ring buffer into a per-shard mpsc
channel (`bus.rs:330`, capacity 1024), and the BatchWorker
assembles a `current_batch` from that channel. Neither the channel
depth nor `BatchWorker.current_batch.len()` enters the predicate.
A draining shard can therefore finalize → `on_shard_removed` fires
→ `remove_shard_internal` runs while the BatchWorker still has
events in flight. `remove_shard_internal` drops the BatchWorker's
`JoinHandle` without awaiting (`bus.rs:442`), so the worker can
also be cut short mid-`on_batch`.

Severity: **medium**.

## Fix

Both bugs fold into one change at `remove_shard_internal`. The new
shape:

1. Drop bus-side sender → drain worker eventually exits via
   `with_shard → None` (after step 2's unmap), which drops the
   sender's drain-side clone, which closes the channel.
2. Atomically pop ring-buffer remnants and unmap (`shard_manager.remove_shard`).
3. **NEW: `await` the BatchWorker's `JoinHandle`** so the worker
   processes any events still in the mpsc channel + its
   `current_batch` and dispatches them under their proper
   `next_sequence` values. This closes #154's race: by the time we
   proceed to step 5, no in-flight events remain.
4. **NEW: read the BatchWorker's final `next_sequence`** from a
   shared `Arc<AtomicU64>` populated by the worker on every flush.
   This is the closes #153's collision: the stranded batch uses
   `sequence_start = final_next_sequence`, which is strictly past
   every msg-id the worker emitted.
5. Construct the stranded batch with that `sequence_start` and
   dispatch.

## API and struct changes

| Symbol | Change |
|---|---|
| `BatchWorkerParams` (`bus.rs:1224`) | + `next_sequence: Arc<AtomicU64>` field |
| `BatchWorker` (`shard/batch.rs:140`) | `next_sequence: u64` → field stays as `u64` for hot-path compactness, but `flush()` writes the post-flush value to a new `Arc<AtomicU64>` field |
| `ShardWorkers` (`bus.rs`) | + `next_sequence: Arc<AtomicU64>` field, owned by the bus, cloned into the worker's params |
| `bus::remove_shard_internal` | await `workers.batch.await` before stranded-flush; load `next_sequence` from the atomic and pass as `sequence_start` |

The `Arc<AtomicU64>` is allocated in `add_shard_internal` (and the
constructor's identical block at `bus.rs:220`), cloned once into
`BatchWorkerParams`, and once into `ShardWorkers`. Cost on the hot
path: one extra atomic store per `flush` (release ordering). The
read happens once per shard removal — not hot.

## Files touched

Implementation:
1. `src/shard/batch.rs` — `BatchWorker::new` accepts the shared
   atomic; `flush()` stores post-flush `next_sequence` to it.
2. `src/bus.rs` — `BatchWorkerParams` carries the atomic;
   `ShardWorkers` carries the atomic; `add_shard_internal` and
   the initial-shard spawn block in `EventBus::new` allocate it;
   `remove_shard_internal` awaits + reads.
3. (no changes needed in `shard/mapper.rs::finalize_draining` —
   #154's race is closed by `remove_shard_internal` awaiting the
   worker. The premature-Stopped state is internal-only and not
   user-observable; a tighter predicate that probes channel +
   `current_batch` is a defense-in-depth followup, not required
   for correctness.)

Tests:
4. Unit (`shard/batch.rs::tests`):
   - `flush_stores_post_flush_next_sequence_to_shared_atomic` —
     after `flush`, atomic == `next_sequence` == `events.len()`.
   - `consecutive_flushes_keep_atomic_in_sync` — two flushes leave
     atomic at `len_0 + len_1`.
   - `empty_flush_does_not_advance_atomic` — flushing an empty
     batch leaves atomic unchanged (defensive — `flush()` shouldn't
     be called on empty in current code, but pin it).
5. Integration (`tests/bus_shutdown_drain.rs` or new file):
   - `remove_shard_internal_uses_post_worker_next_sequence_for_stranded_batch` —
     scale up + ingest a known number of events → wait for some
     flushes → manual_scale_down → assert the recorded test-adapter
     batches show the stranded batch's `sequence_start ==
     sum_of_prior_event_counts` (NOT 0). Uses a custom test
     adapter that records every batch's `(shard_id,
     sequence_start, len)`.
   - `events_in_flight_at_finalize_reach_adapter` — produce events,
     scale_down immediately while `current_batch` is partial /
     channel has buffered events, assert all events end up in the
     adapter (no silent loss). Pre-fix this would have lost
     anything still in the BatchWorker's queue or `current_batch`.
6. Integration regression test pinning #153 directly:
   - `stranded_flush_does_not_collide_with_first_batch_msg_ids` —
     synthetic adapter that REJECTS duplicate `(shard_id, seq, i)`
     tuples; produce events, scale_down, assert no duplicates.

## Implementation order

1. Add `Arc<AtomicU64>` plumbing through `BatchWorker::new` /
   `BatchWorkerParams` / `ShardWorkers` / both spawn sites.
2. Make `BatchWorker::flush` store post-flush `next_sequence`.
3. Update `remove_shard_internal` to `await` the worker handle and
   read the atomic. Lift the `JoinHandle` out via `take()` /
   `Option` since `await` consumes the handle.
4. Run the existing test suite — verify nothing regresses.
5. Add unit tests on `BatchWorker`.
6. Add integration tests pinning #153 + #154.
7. Update `BUG_AUDIT_2026_04_30_CORE.md`: mark both #153 and #154
   FIXED, move out of the Outstanding list.

## Non-goals

- Tightening the `finalize_draining` predicate to also probe
  channel depth + `current_batch.len()`. Defense-in-depth only;
  the await-the-handle approach is the actual correctness gate.
  Leave a comment in `mapper.rs::finalize_draining` referencing
  #154's resolution.
- Changing the JetStream `Nats-Msg-Id` schema to defend against
  same-process sequence collisions in general (e.g. by always
  including a per-spawn nonce). Out of scope; the immediate
  collision in the stranded-flush path is what #153 names.
- Bounding `remove_shard_internal`'s wait for the BatchWorker
  handle. The worker exits as soon as the channel closes (which
  follows from the bus-side sender drop + drain worker exit),
  bounded by `adapter_timeout * batch_retries` for any in-flight
  dispatch. If that's a problem in production, a `tokio::time::timeout`
  wrapper is a small follow-up.
