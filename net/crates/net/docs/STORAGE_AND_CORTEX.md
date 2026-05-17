# Storage & CortEX — how to use it

This doc is the **user-level narrative** for Net's single-node storage
stack: RedEX (the log), CortEX (the fold driver and domain models), and
NetDB (the query façade). For the design rationale + implementation
plans see [`REDEX_PLAN.md`](REDEX_PLAN.md), [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md),
and [`NETDB_PLAN.md`](NETDB_PLAN.md). This doc is "how do I *use* it and
what do I need to know when I do."

Target reader: an engineer writing a daemon that reads + writes
mesh-bound state.

## The three layers

```
   ┌──────────────────────┐
   │  NetDB (query / watch façade)        db.tasks / db.memories
   └──────────┬───────────┘
              │   query filters, snapshot, snapshot_and_watch
   ┌──────────┴───────────┐
   │  CortEX (fold driver + domain models)
   └──────────┬───────────┘
              │   RedexFold<State>  + change broadcast
   ┌──────────┴───────────┐
   │  RedEX (append-only log)
   └──────────────────────┘
```

- **RedEX** is the primitive: a named monotonic log (`ChannelName` →
  `RedexFile`). 20-byte index records, inline-or-heap payloads, a
  tail API that delivers events in order. Single-node. Optionally
  disk-backed via the `redex-disk` feature.
- **CortEX** drives folds over RedEX tails. A domain model (`tasks`,
  `memories`) implements `RedexFold<State>`, which mutates state as
  events arrive. The adapter owns the `Arc<RwLock<State>>` and a
  change-broadcast channel for reactive consumers.
- **NetDB** bundles CortEX adapters under one handle with a unified
  snapshot / restore surface. Queries go through the state; writes
  go through the adapter's typed API.

Stream API surface (names as they appear in Rust; TS/Python mirror
them with language-native conventions):

| Concern | Method | Returns |
|--------|--------|---------|
| Query current state | `state.query().where_*().collect()` | `Vec<T>` |
| One-shot lookup by id | `state.find_unique(id)` | `Option<&T>` |
| Filter → set | `state.find_many(&filter)` | `Vec<T>` |
| Count / exists | `state.count_where(&f)` / `state.exists_where(&f)` | `usize` / `bool` |
| React to changes | `adapter.watch().where_*().stream()` | `Stream<Item = Vec<T>>` |
| Snapshot + react | `adapter.snapshot_and_watch(watcher)` | `(Vec<T>, Stream<Item = Vec<T>>)` |
| Full-state capture | `adapter.snapshot()` | `(Vec<u8>, Option<u64>)` |
| NetDB whole-stack | `NetDb.snapshot()` / `NetDb.open_from_snapshot(...)` | one postcard blob covering every enabled model |

## Choosing durability (RedEX `FsyncPolicy`)

Set via `RedexFileConfig::default().with_fsync_policy(...)`:

```rust
use net::adapter::net::{FsyncPolicy, RedexFileConfig};
use std::time::Duration;

let cfg = RedexFileConfig::default()
    .with_persistent(true)
    .with_fsync_policy(FsyncPolicy::EveryN(100));
```

| Policy | Worst-case loss on crash | Use for |
|--------|-------------------------|---------|
| `Never` (default) | Tail since last close / explicit `sync()` | Telemetry, caches, best-effort logs |
| `EveryN(N)` | ≤ `N − 1` entries from the last sync point | Most application state (try `EveryN(100)`) |
| `Interval(d)` | ≤ `d` seconds of writes | Anything that must survive kernel panic |

Invariants you can rely on:

- `close()` **always** fsyncs, regardless of policy.
- `RedexFile::sync()` always fsyncs — the explicit durability barrier.
- Torn-write recovery is fsync-independent: the dat-before-idx write
  order + reopen-time truncation handle arbitrary partial writes.
- `EveryN(0)` and `EveryN(1)` both mean "fsync every append."
- `Interval` spawns one tokio background task per file, cancelled on
  `close()`.

## Querying

`NetDb` exposes per-model state handles via `db.tasks()` / `db.memories()`
(Rust) or `db.tasks` / `db.memories` (TS/Python property accessors). The
state behind each is held behind an `Arc<RwLock<State>>`; queries take a
brief read lock, scan the state, and return owned values.

```rust
let pending = db.tasks().state().read()
    .query()
    .where_status(TaskStatus::Pending)
    .order_by(OrderBy::CreatedDesc)
    .limit(20)
    .collect();

let found = db.tasks().state().read().find_unique(id);
let count = db.memories().state().read().count_where(&filter);
```

Query performance at a glance (Apple M1 Max, `cargo bench --bench cortex`):

- `find_unique`: ~9 ns.
- `find_many` on 1 K tasks (status filter): ~8 µs.
- `find_many` on 10 K tasks: ~140 µs.
- `count_where` on 10 K tasks: ~30 µs.
- `find_many` on 1 K memories (tag filter): ~50 µs.

At 10 K state size, filter queries stay in double-digit microseconds —
cheap enough to call inside a hot loop. Write performance is separate:
tasks ingest runs at ~3.6 M events/sec before consumer backpressure.

## Watching

`adapter.watch()` returns a builder. Chain filters the same way you
chain query filters; then call `.stream()` to start emitting.

```rust
use futures::StreamExt;

let mut stream = Box::pin(
    db.tasks()
        .watch()
        .where_status(TaskStatus::Pending)
        .order_by(OrderBy::CreatedDesc)
        .stream(),
);

while let Some(current_pending) = stream.next().await {
    // current_pending is the freshly-evaluated filter result.
    // Only delivered when the result actually changed.
}
```

Semantics:

- **Initial emission**: the first `.next()` returns the current filter
  result (immediately, no network round trip).
- **Deduplication**: subsequent emissions only fire when the filter
  result differs from the previous one — renames on irrelevant rows
  don't wake your consumer.
- **Single-slot channel**: if the consumer falls behind a fast fold
  task, intermediate filter results are dropped; the consumer sees
  the latest state on the next poll. "Drop intermediate, final state
  is correct."
- **Default ordering**: if you don't set `order_by`, the watcher
  defaults to `IdAsc` so Vec-equality dedup is deterministic.
- **Cancellation**: drop the stream to stop. The watcher's internal
  task observes `tx.closed()` and exits.

### Snapshot + watch combo

UI consumers usually want "paint what's there now, then react to
changes." `adapter.snapshot_and_watch(watcher)` is the one-liner:

```rust
let watcher = db.tasks().watch().where_status(TaskStatus::Pending);
let (snapshot, mut stream) = db.tasks().snapshot_and_watch(watcher);

// `snapshot` is the current filter result.
render(&snapshot);

// `stream` emits deltas from this point forward — no duplicate of
// the initial state.
while let Some(delta) = stream.next().await {
    render(&delta);
}
```

The initial emission is consumed internally (via `skip(1)`) so the
caller doesn't double-render.

## Snapshot + restore

Per-model:

```rust
let (state_bytes, last_seq) = db.tasks().snapshot()?;
// Persist (bytes, last_seq) somewhere durable (disk, cloud, etc.)

let tasks = TasksAdapter::open_from_snapshot(
    &redex,
    origin_hash,
    &state_bytes,
    last_seq,
)?;
// Replay picks up at last_seq + 1; earlier events don't re-fold.
```

Whole-DB (covers every enabled model in one postcard blob):

```rust
let bundle = db.snapshot()?;
let db2 = NetDb::open_from_snapshot(&redex, bundle, builder_config)?;
```

Bundle encode/decode timings (postcard, 1 K entries): ~23 µs encode,
~27 µs decode. Bundles are 60-70% smaller than the pre-postcard
bincode format.

Known limitation: **postcard schema breaks on field-type changes.**
If you add or remove a field on `Task` / `Memory` / a new CortEX
model, old snapshots don't deserialize. Re-snapshot on upgrade.

## Restart behavior

On process restart with a persistent RedEX file:

1. `Redex::with_persistent_dir(...)` reconnects to the base
   directory.
2. `TasksAdapter::open(&redex, origin_hash)` (or `open_from_snapshot`
   if you saved state bytes) reads the tail from RedEX.
3. The fold task replays events — **every event if there's no
   snapshot**; only post-snapshot events if you restored from one.
4. `adapter.wait_for_seq(seq)` is a read-after-write barrier — await
   it before querying to guarantee the fold caught up.

Design choices that matter here:

- RedEX files default to strictly local — there's no cross-node
  fallback if local disk is wiped. Opt in to cross-node replication
  per channel via
  `RedexFileConfig::with_replication(Some(ReplicationConfig::new()))`;
  see [`CONFIG_REPLICATION.md`](CONFIG_REPLICATION.md) for the full
  operator surface.
- Retention evicts head events from memory but (in `redex-disk`
  today) leaves them on disk. v2's mmap tier reconciles the two.
- Age-based retention (`retention_max_age_ns`) resets on persistent
  reopen: recovered entries get "now" as their timestamp. If age
  retention matters across restarts, include a timestamp in the
  payload.

## What's coming (v3 territory)

The following are designed but not shipped in v2. Knowing the shape
helps you structure daemons so adopting them later is straightforward.

- **Hot → warm mmap tiering** ([`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md)).
  A separate mmap-backed segment for frozen history, with transparent
  reads across hot heap + warm mmap. Reduces memory pressure on
  long-lived files; unblocks ~billions of retained events.
- **`ColdStore` trait** + `read_cold(start, end)`. Archive-tier
  interface that reopens old segments from e.g. object storage. Per
  the v2 closeout plan, held out of v2 pending real-world demand.
- **Secondary indices on CortEX models**. The `RedexIndex<K, V>`
  primitive exists today (see `redex::index`); wiring it into
  `MemoriesAdapter` as a tag index is pending a domain-model tweak
  (`MemoryRetaggedPayload` needs to carry old + new tags so the
  projection can emit symmetric insert/remove ops).
- **NetDB watchers in the Go / C bindings** (`db.tasks.watch(filter)` /
  `snapshotAndWatch`). The Rust surface exists today
  (`adapter.watch()` / `adapter.snapshot_and_watch(...)`) and the napi
  + pyo3 wrappers ship in `bindings/node` + `bindings/python`; the Go
  and C bindings don't have a NetDB adapter surface at all yet, so
  this is gated on a `cortex-ffi` crate landing first.

## FAQ

**Q: Do I need CortEX if I just want an append-only log?**
No. RedEX alone gives you `append` + `tail` + `read_range`. CortEX
exists to materialize folded state from the log, which you only need
if you want to query "what does the current state look like" rather
than "what events arrived."

**Q: Can two processes share a persistent RedEX directory?**
No. v1 assumes single-writer. The manager doesn't file-lock; multiple
writers corrupt the idx/dat pair.

**Q: How do I reset a corrupted file?**
Close the adapter, delete the `<base>/<channel_path>/{idx,dat}` pair,
reopen. RedEX creates fresh files.

**Q: What happens if the fold panics?**
Under `FoldErrorPolicy::Stop` (default), the fold task exits and
subsequent `wait_for_seq` calls hang. Under `LogAndContinue`, the
event is skipped and state keeps advancing. Choose based on whether
you'd rather fail fast or survive bad events.

**Q: Can I swap `FsyncPolicy` on a live file?**
Not today. The policy is set at `open_file` time and frozen for the
file's lifetime. Close + reopen with a different policy.

**Q: Where do benchmarks live?**
Top-level `README.md` and `net/crates/net/README.md` both carry the
perf tables. Source: `cargo bench --bench cortex` /
`cargo bench --bench redex`.
