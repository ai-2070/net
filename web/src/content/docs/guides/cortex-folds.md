# Folded State with CortEX

CortEX is how you turn a log into queryable state. You write a fold — a function that consumes events one at a time and updates a piece of state — and CortEX runs it against a RedEX log, persists the state, and exposes it to readers. The pattern is event sourcing, made first-class.

The shape is small enough that the whole model fits in three pieces: a state type that holds the materialized view, a fold that says how each event updates the state, and an adapter that wires the two together against a RedEX file. The runtime does the rest.

## The fold

A fold is a Rust trait with one required method. Take an event, take the current state, produce the new state:

```rust
use net::adapter::net::cortex::RedexFold;

#[derive(Default, Clone)]
struct TaskCount {
    pending: usize,
    completed: usize,
}

struct TaskCountFold;

impl RedexFold<TaskCount> for TaskCountFold {
    fn apply(&self, state: &mut TaskCount, event: &Event) -> Result<(), FoldError> {
        match event.kind() {
            EventKind::TaskCreated  => state.pending += 1,
            EventKind::TaskComplete => {
                state.pending = state.pending.saturating_sub(1);
                state.completed += 1;
            }
            _ => {}
        }
        Ok(())
    }
}
```

The fold mutates the state in place. It's pure in spirit — the same sequence of events always produces the same state — but the runtime gives you `&mut State` rather than `(state, event) -> state` for the obvious performance reason. The discipline is on you to keep the fold deterministic.

## The adapter

`CortexAdapter` is what owns the state, drives the fold, and exposes the query surface. You construct it against a RedEX file and a fold:

```rust
use net::adapter::net::cortex::CortexAdapter;

let adapter = CortexAdapter::open(
    &redex,
    "tasks",
    origin_hash,
    TaskCountFold,
).await?;
```

Behind the scenes, the adapter opens the RedEX file, spawns a fold task that subscribes to the tail, applies events as they arrive, and persists the resulting state in an `Arc<RwLock<State>>`. Readers take a brief read lock to query; the fold task takes a write lock to mutate.

## Querying

The state is yours to query however makes sense. CortEX gives you the `Arc<RwLock<State>>` directly — there's no separate query API; you just read the state:

```rust
let state = adapter.state().read();
println!("pending: {}, completed: {}", state.pending, state.completed);
```

For more structured state types, the SDK ships a query builder pattern that scans the state with a fluent surface. The tasks adapter that comes with CortEX uses it:

```rust
let pending = db.tasks().state().read()
    .query()
    .where_status(TaskStatus::Pending)
    .order_by(OrderBy::CreatedDesc)
    .limit(20)
    .collect();

let found = db.tasks().state().read().find_unique(task_id);
```

Query performance is whatever your state implementation makes it. The tasks adapter, scanning 10,000 entries with a filter and a sort, runs in the low hundreds of microseconds on a modern laptop — cheap enough to call inside a hot loop.

## Reacting to changes

`adapter.watch()` returns a builder that emits the current filter result whenever it changes. The semantics are deliberately tuned for UI consumers and reactive workloads:

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
    render(&current_pending);
}
```

A few semantics worth knowing about:

- **The first emission is immediate.** No round trip, no wait — you get the current filter result the moment you call `.next()`.
- **Emissions are deduplicated.** An event that doesn't change the filter result doesn't wake the consumer.
- **The channel has a single slot.** If a fast fold produces multiple updates before the consumer reads, intermediate results are dropped in favor of the latest. "Drop intermediate, final state is correct."
- **Cancellation is automatic.** Drop the stream and the watcher's internal task notices and exits.

For the common "render current state then react to deltas" pattern, the SDK gives you `snapshot_and_watch`, which folds the initial emission and subsequent updates into a single helper:

```rust
let watcher = db.tasks().watch().where_status(TaskStatus::Pending);
let (snapshot, mut stream) = db.tasks().snapshot_and_watch(watcher);

render(&snapshot);  // current state, no waiting
while let Some(delta) = stream.next().await {
    render(&delta);  // subsequent updates only
}
```

## Snapshots and restore

A long-lived fold over a long log gets expensive to replay from genesis. Snapshots let you checkpoint the state and resume from the checkpoint plus the events that came after:

```rust
let (state_bytes, last_seq) = adapter.snapshot()?;
// Persist (state_bytes, last_seq) wherever fits.

let resumed = CortexAdapter::open_from_snapshot(
    &redex,
    origin_hash,
    TaskCountFold,
    &state_bytes,
    last_seq,
).await?;
```

The snapshot is the state's serialized bytes plus the sequence number of the last event folded into it. On restore, the adapter deserializes the state, opens the RedEX log, and starts the fold from `last_seq + 1` — so pre-snapshot events never re-fold, and the resumed adapter is byte-identical to where it left off.

For applications that use the higher-level NetDB facade, you get whole-stack snapshots in one call:

```rust
let bundle = db.snapshot()?;
let db2 = NetDb::open_from_snapshot(&redex, bundle, builder_config)?;
```

The bundle is a single postcard blob covering every adapter the NetDB knows about. Encode and decode run in tens of microseconds at the 1k-entry scale; the bundle is 60–70% smaller than the equivalent JSON.

## The two domain models that ship

CortEX comes with two reference folds: tasks and memories. Tasks model long-running workloads with explicit lifecycle (`Pending`, `Running`, `Completed`, `Failed`); memories model durable observations a daemon needs across restarts. Both are useful out of the box, and both are worked examples of how to write your own fold.

You don't have to use them. They live alongside any folds you write yourself; CortEX is happy to drive multiple folds against the same RedEX log, or different folds against different logs, and the NetDB facade composes them all under one query surface.

## Failure handling

Folds can fail. An event with a bad payload, a logic bug in the fold, an underflow on a counter — all of these manifest as an `Err` from `apply()`. The runtime's response is configurable:

| `FoldErrorPolicy` | Behavior on `apply()` error                                                   |
|-------------------|--------------------------------------------------------------------------------|
| `Stop` (default)  | Fold task exits. Subsequent `wait_for_seq` calls hang. Fail loudly.            |
| `LogAndContinue`  | Event is skipped; state keeps advancing. Useful for survivable bad-data cases. |

`Stop` is the right default for code that should be correct. `LogAndContinue` is for production cases where one bad event shouldn't take down the whole fold; pair it with a metric so you can see how often it fires.

## Read-after-write

CortEX is eventually consistent against its own producer — appending an event to RedEX and immediately reading the state can return a stale view, because the fold task hasn't gotten there yet. For workflows that need read-your-writes, `adapter.wait_for_seq(seq)` is the barrier:

```rust
let seq = adapter.append(payload).await?;
adapter.wait_for_seq(seq).await;

// State now reflects the just-appended event.
let view = adapter.state().read();
```

`wait_for_seq` resolves when the fold task has applied every event up to and including `seq`. For most flows you won't need it — the read-vs-write race is small enough that polling-style consumers don't notice — but when you do need it, it's the right primitive.

## When CortEX is the wrong tool

Two cases:

**You don't need queries.** If your application reads events directly from the bus and doesn't materialize state, you're using the bus and RedEX without needing CortEX on top. Adding a fold to a workload that doesn't query state is dead weight.

**Your state doesn't compose with event sourcing.** Folds are powerful, but they aren't free — every read of the state pays the cost of having computed it. If your "state" is a derived view that you'd be better served computing on demand against a database, do that instead. CortEX is for state where the fold makes sense; some state is better materialized differently.

The common case sits in the middle: you have events, you have a question to ask about the cumulative result, and a fold is the cleanest way to keep an answer to that question always available. That's the sweet spot.
