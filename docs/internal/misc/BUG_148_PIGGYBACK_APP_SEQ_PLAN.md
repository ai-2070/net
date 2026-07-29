# BUG #148 — Piggyback `app_seq` Discovery on the Fold Task

Resolves the deferred entry from `BUG_AUDIT_2026_04_30_CORE.md:870`:

> `Tasks/MemoriesAdapter::open_from_snapshot` redundantly re-reads events the
> inner fold task already folded — doubles startup IO and CPU on large logs.

## Problem

`TasksAdapter` and `MemoriesAdapter` keep a per-origin monotonic counter
`app_seq` that gets stamped on every `EventMeta::seq_or_ts`. After
`open_from_snapshot`, the counter must satisfy
`app_seq > max(seq_or_ts of any in-log event for our origin)` before the first
`ingest_typed` runs, otherwise the next ingest can stamp a duplicate
`seq_or_ts` (data corruption — two distinct events with the same
per-origin sequence number).

Today both adapters do that discovery via a synchronous `read_range` walk
in `open_from_snapshot_with_config` (tasks: `adapter.rs:308-322`, memories:
`adapter.rs:286-301`). The fold task ALSO walks the same events. For a log
with N replayable events, both passes call `materialize()` — N full payload
reads, N checksum verifications, 2N `Bytes` copies. On disk-backed Redex
files the doubled materialization is N redundant disk reads.

The same hazard latently exists in `open_with_config` too: it sets
`app_seq = 0` regardless of file contents, so reopening a persistent log
with existing same-origin events will produce duplicate `seq_or_ts` on the
first ingest.

## Fix

Piggyback `app_seq` discovery onto the fold callback so each event is
read once. Wrap the user fold (`TasksFold` / `MemoriesFold`) in a
`WatermarkingFold<S, F>` that:

1. Delegates `apply` to the inner fold.
2. After a successful inner apply, parses the leading `EventMeta` from
   the event's payload, and if `meta.origin_hash == self.origin_hash`,
   bumps a shared `Arc<AtomicU64>` via
   `fetch_max(meta.seq_or_ts.saturating_add(1))`.

The adapter holds a clone of the same `Arc<AtomicU64>` and reads /
CAS-commits against it from `ingest_typed`. The synchronous `read_range`
loop is removed entirely.

### Catch-up barrier

The wrapper fold updates `app_seq` asynchronously as the fold task
catches up. Until catch-up completes, `app_seq` may be lower than the
true highest in-log value, so a premature `ingest_typed` can stamp a
duplicate.

To preserve the existing "adapter is ready when the constructor returns"
contract, every constructor (`open`, `open_with_config`,
`open_from_snapshot`, `open_from_snapshot_with_config`) becomes
`async fn` and awaits `inner.wait_for_seq(replay_end - 1)` before
returning, where `replay_end = file.next_seq()`. Skipped when
`replay_end == 0` (empty file). This is the API break the audit's
deferral note flagged; it's acceptable here because the library has
not shipped a stable release yet.

## API changes

The following methods become `async fn`:

| File | Method |
|---|---|
| `cortex/tasks/adapter.rs` | `TasksAdapter::open`, `open_with_config`, `open_from_snapshot`, `open_from_snapshot_with_config` |
| `cortex/memories/adapter.rs` | `MemoriesAdapter::open`, `open_with_config`, `open_from_snapshot`, `open_from_snapshot_with_config` |
| `netdb/db.rs` | `NetDbBuilder::build`, `build_from_snapshot` |

Bindings + SDK wrappers cascade async wherever they call the above.
PyO3 / NAPI both already model async at the surface, so the Node /
Python signatures stay the same; only the Rust glue inside the binding
modules changes.

## Files touched

Implementation:
1. `src/adapter/net/cortex/watermark.rs` (new) — `WatermarkingFold<S, F>`,
   shared between tasks and memories.
2. `src/adapter/net/cortex/mod.rs` — `mod watermark;` (private).
3. `src/adapter/net/cortex/tasks/adapter.rs` — `app_seq:
   Arc<AtomicU64>`, async constructors, wrapper fold installed,
   `read_range` loop removed.
4. `src/adapter/net/cortex/memories/adapter.rs` — same as tasks.
5. `src/adapter/net/netdb/db.rs` — `build` + `build_from_snapshot` go
   async.

Tests:
6. `tests/integration_cortex_tasks.rs` — `await` the constructors.
7. `tests/integration_cortex_memories.rs` — same.
8. `tests/integration_netdb.rs` — same.
9. `sdk/tests/cortex_surface.rs` — same.

Bindings + SDK:
10. `sdk/src/cortex.rs` — async cascade where it wraps the adapters.
11. `bindings/node/src/cortex.rs` — `napi` already wraps async; just
    `.await` the new signatures.
12. `bindings/python/src/cortex.rs` — `pyo3-async` likewise; verify
    no sync surface is exposed.

Regression tests to add:
- `cortex::tasks::adapter::tests::open_replay_advances_app_seq_via_fold` —
  open a fresh `TasksAdapter` against a Redex with pre-existing same-origin
  events; confirm the post-`open` `app_seq` is `max(seq_or_ts) + 1` and
  that the next ingest does not collide.
- `cortex::tasks::adapter::tests::open_from_snapshot_skips_redundant_read` —
  same, via the snapshot path; pin that no second pass over the
  payloads happens (probe via a `RedexFile::tail_subscriber_count` or
  similar — TBD whether a direct probe is feasible; otherwise pin
  behavior with a counted-fold-call assertion).
- Mirrors of both for `MemoriesAdapter`.

## Implementation order

1. Add `WatermarkingFold` + `mod watermark` (compiles standalone).
2. Convert `TasksAdapter` constructors. Verify with `cargo check
   --features cortex`.
3. Convert `MemoriesAdapter`. Same verification.
4. Convert `NetDbBuilder`. Verify.
5. Update integration tests. Run.
6. Update SDK wrappers + FFI bindings. Run their test suites.
7. Add regression tests.
8. Mark #148 as fixed in `BUG_AUDIT_2026_04_30_CORE.md`. Move from
   the "Outstanding" list to the "Fixed on YYYY-MM-DD" block.

## Non-goals

- Optimizing the underlying `materialize()` path (checksum-on-read,
  zero-copy from segment buffer). That belongs in a separate Redex-side
  improvement.
- Changing `CortexAdapter::open` / `open_from_snapshot` signatures —
  they remain synchronous. Only the typed wrappers and `NetDbBuilder`
  become async.
- Eliminating the second `redex.open_file(...)` call inside the typed
  constructor. `open_file` is idempotent; the redundant call is cheap.
