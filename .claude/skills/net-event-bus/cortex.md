# CortEX — folded queryable state on top of RedEX

CortEX takes a RedEX tail and folds each event into an **in-memory state struct**. The state lives in your process; queries are direct memory access (sub-microsecond on M1-class hardware), not a network round-trip. It's what you reach for when "I want SQLite-shaped queries over the event stream" or "I want to subscribe to changes to a derived state" — the substrate's answer to materialized views, but local.

Two adapters ship in-tree: **Tasks** (id + name + status timestamps) and **Memories** (id + content + tags). They're concrete examples; you write your own adapter the same way (`open(redex, channel, fold_fn) → CortexAdapter<MyState>`).

If you want to query across multiple CortEX adapters as one handle, see **NetDB** at the end.

---

## Mental model

- **A `CortexAdapter<State>` owns one RedEX file** and runs a fold loop over its tail. State is `Arc<RwLock<State>>`; queries read-lock, fold-step writes.
- **`State` is yours.** `Tasks` and `Memories` are concrete examples (`TasksState`, `MemoriesState`); your own adapter can hold anything — `HashMap`, `Vec`, a custom indexed structure.
- **The fold runs on a tokio task.** `applied_through_seq()` tracks how far the fold has applied; the read-your-writes machinery hangs off this watermark. The seq is locally monotonic per-RedEX-file.
- **State changes ride `changes()` / `changes_with_lag()`.** Hot subscribers see `u64` (or `ChangeEvent::Update { seq, lag }`) per applied seq — pair with `state.read()` to materialize the change.
- **Snapshots are first-class.** `adapter.snapshot()` returns `(bytes, applied_seq)`; `open_from_snapshot(bytes)` restores both the state and the applied watermark in one call. Pair with retention to avoid replaying every event on restart.
- **Watchers are cheap.** `tasks.watch()` returns a `TasksWatcher` that yields on every change; many concurrent watchers fan out from the same fold task.
- **No cross-adapter consistency.** Each adapter folds its own RedEX file; there's no global transaction across adapters. NetDB exposes them under one handle for query convenience, not for cross-adapter atomicity.

---

## When to reach for CortEX

| Need | Use |
|---|---|
| "I want SQLite-shaped queries on the event stream" | `CortexAdapter<MyState>` with a custom fold + indexed state |
| "I want to react to changes in derived state" | `adapter.changes()` (`u64` per applied seq) or `changes_with_lag()` |
| "Tasks list / memories list with status filters" | `Tasks` / `Memories` adapters directly |
| "Fast restart with hot state" | `adapter.snapshot()` → persist bytes; `CortexAdapter::open_from_snapshot(bytes)` on startup |
| "Read my own write deterministically" | `tasks.create(...)` returns a `WriteToken`; `tasks.wait_for_token(token, deadline)` (see `dataforts.md` § Read-your-writes) |

---

## Rust

```rust
use net::adapter::net::{Redex, RedexFileConfig};
use net::adapter::net::cortex::tasks::{Tasks, TasksState, TaskStatus};
use std::sync::Arc;

let redex = Arc::new(Redex::with_persistent_dir("/var/lib/net/redex"));
let tasks = Tasks::open(
    redex.clone(),
    "app/tasks".parse()?,
    0xDEAD_BEEF_u64,                 // origin_hash; binds WriteTokens
    RedexFileConfig::new().with_persistent(true),
)?;

// Ingest — fold runs asynchronously, returns a WriteToken
let result = tasks.create(1, "first", net::now_ns())?;
let _ = tasks.complete(1, net::now_ns())?;

// Read the in-memory state
{
    let state = tasks.state();
    let s = state.read();
    let pending: Vec<_> = s.find_many(&TasksFilter {
        status: Some(TaskStatus::Pending),
        ..Default::default()
    });
    println!("{} pending", pending.len());
}

// Wait for the fold to apply your write deterministically
tasks.wait_for_token(result.token, std::time::Duration::from_millis(250)).await?;

// React to changes
let mut changes = tasks.as_cortex().changes();
while let Some(seq) = changes.next().await {
    let snapshot = tasks.state().read().clone();
    // …
}

tasks.close()?;
```

**Key facts:**
- `Tasks::open` / `Memories::open` take `(redex, channel, origin_hash, RedexFileConfig)`. The `origin_hash` is what `WriteToken`s bind to — `wait_for_token` rejects tokens from a different origin.
- The fold loop runs on a tokio task; `tasks.is_running()` reports liveness. `tasks.close()` joins the task cleanly.
- `state()` returns `Arc<RwLock<TasksState>>`. Queries take `state.read()` and use methods like `find_unique` / `find_many(&filter)` / `count_where`. Writes go through `tasks.create / .complete / .delete / .rename` — never mutate `state.write()` directly (you'd diverge from the RedEX log).
- `changes()` returns a `Stream<Item = u64>`; `changes_with_lag()` returns `Stream<Item = ChangeEvent>` where `ChangeEvent::Lagged { skipped }` is emitted if the channel buffer overflowed. Use the latter when you can't tolerate dropped change notifications.
- `applied_through_seq()` vs. `folded_through_seq()`: the former is what RYW waits on (events that actually applied to state); the latter is what the fold task observed (including skipped-via-`FoldErrorPolicy` events).
- `snapshot_and_watch_*` is the atomic "give me the current state + a watcher that won't miss the next change" primitive. Avoid the `list_tasks() + watch_tasks()` pattern — a mutation between the two reads is silently lost.

## Python

```python
from net import Redex, Tasks, TaskStatus

redex = Redex(persistent_dir='/var/lib/net/redex')
tasks = Tasks.open(
    redex,
    channel='app/tasks',
    origin_hash=0xDEAD_BEEF,
    persistent=True,
)

result = tasks.create(1, 'first', now_ns())
tasks.complete(1, now_ns())

state = tasks.state()  # snapshot dict view
pending = state.find_many(status=TaskStatus.PENDING)
print(len(pending), 'pending')

# RYW
tasks.wait_for_token(result.token, deadline_ms=250)
# deadline_ms=0 is a non-blocking poll (raises CortexError if not yet applied)

# Changes
for seq in tasks.watch():
    snap = tasks.state()
    # …

tasks.close()
```

**Key facts:**
- `Tasks.open(redex, channel=..., origin_hash=..., persistent=..., retention_max_age_secs=...)`. Kwargs mirror the Rust `RedexFileConfig` shape.
- `tasks.state()` returns a snapshot (an immutable view into the current state). For change-aware queries pair with `tasks.watch()` / `tasks.snapshot_and_watch()`.
- `watch()` supports `for seq in watch():` (sync) and `async for seq in watch():` (asyncio). Sync inside `asyncio.to_thread(...)` is the recommended shape for asyncio apps — the FFI poll is per-step blocking and a direct `async for` on the same instance still calls the blocking poll per step.
- `wait_for_token(token, deadline_ms=0)` is a non-blocking poll (synchronous applied-vs-token check; raises `CortexError` if not applied yet).
- `CortexError` is the catchable for adapter-boundary failures (fold halted, RedEX I/O, decode failures).

## Node

```ts
import { Redex, Tasks, TaskStatus } from '@ai2070/net';

const redex = new Redex({ persistentDir: '/var/lib/net/redex' });
const tasks = await Tasks.open(redex, {
  channel: 'app/tasks',
  originHash: 0xDEAD_BEEFn,
  persistent: true,
});

const result = tasks.create(1n, 'first', BigInt(Date.now()) * 1_000_000n);
tasks.complete(1n, BigInt(Date.now()) * 1_000_000n);

const state = tasks.state();
const pending = state.findMany({ status: TaskStatus.PENDING });

await tasks.waitForToken(result.token, 250);  // deadlineMs; 0 = non-blocking poll

for await (const seq of tasks.watch()) {
  // tasks.state().findMany(...)
}

await tasks.close();
```

**Key facts:**
- `BigInt` everywhere ids / origin_hashes / timestamps live — JS `Number` loses precision past 2^53.
- `watch()` returns an async iterable; cancelling the for-await loop drops the subscription cleanly.
- `waitForToken(token, deadlineMs)` rejects with `CortexError` on timeout / fold-stopped; `deadlineMs === 0` is a non-blocking poll.

## Go

```go
import "github.com/ai-2070/net/go"

redex := net.NewRedex(net.RedexConfig{PersistentDir: "/var/lib/net/redex"})
defer redex.Close()

tasks, err := net.OpenTasks(redex, /*originHash*/ uint64(0xDEADBEEF), /*persistent*/ true)
if err != nil { /* … */ }
defer tasks.Close()

result, _ := tasks.Create(1, "first", uint64(time.Now().UnixNano()))
tasks.Complete(1, uint64(time.Now().UnixNano()))

state, _ := tasks.State()       // snapshot
// state.FindMany(...) — depends on the adapter's query method shape

// RYW
if err := tasks.WaitForToken(result.Token, 250*time.Millisecond); err != nil { /* … */ }
// PollForToken — non-blocking
if err := tasks.PollForToken(result.Token); err != nil { /* … */ }
// Context-aware variant (but see N-11 caveat)
ctx, cancel := context.WithTimeout(context.Background(), 250*time.Millisecond)
defer cancel()
if err := tasks.WaitForTokenContext(ctx, result.Token); err != nil { /* … */ }
```

**Key facts:**
- `WaitForToken(token, timeout)` blocks the calling goroutine; `PollForToken(token)` is a non-blocking applied-vs-token check.
- `WaitForTokenContext(ctx, token)` accepts a Go `context.Context`, but **context cancellation isn't propagated into the FFI wait** — the FFI's blocking call continues until the underlying deadline expires (N-11 doc note). Use it for ergonomics, not for sub-deadline cancellation.

## C

```c
#include "net.h"

CortexHandle* tasks;
net_tasks_open(redex, /*origin_hash*/ 0xDEADBEEFULL, /*persistent*/ 1, &tasks);

uint64_t seq;
uint64_t token_origin; uint64_t token_seq;
net_tasks_create(tasks, /*id*/ 1, "first", /*now_ns*/ 0, &seq, &token_origin, &token_seq);
net_tasks_complete(tasks, 1, 0, &seq);

// RYW
int rc = net_tasks_wait_for_token(tasks, token_origin, token_seq, /*deadline_ms*/ 250);
// deadline_ms == 0 is a non-blocking poll

net_tasks_close(tasks);
```

**Key facts:**
- The FFI wraps every `block_on` body in `std::panic::catch_unwind`; panics surface as `NET_ERR_PANIC` rather than unwinding across `extern "C"`.
- `timeout_ms == 0` is a non-blocking poll (D-23 fix); pre-v0.15 it was rewritten to 1 ms.

---

## NetDB — unified query façade

When you want to query across Tasks + Memories (or your own adapters) under one handle, use NetDB. It bundles enabled adapters behind per-model accessors and exposes a Prisma-ish surface on each.

```rust
use net::adapter::net::{NetDb, Redex, TasksFilter, TaskStatus};

let db = NetDb::builder(Redex::new())
    .origin(origin_hash)
    .with_tasks()
    .with_memories()
    .build()?;

db.tasks().create(1, "write plan", net::now_ns())?;
let pending = db.tasks().state().read().find_many(&TasksFilter {
    status: Some(TaskStatus::Pending),
    limit: Some(10),
    ..Default::default()
});

// Whole-db snapshot
let bundle = db.snapshot()?;
let bytes = bundle.encode();
// later …
let bundle = NetDbSnapshot::decode(&bytes)?;
let db = NetDb::builder(Redex::new()).origin(origin_hash).with_tasks().with_memories()
    .build_from_snapshot(&bundle)?;
```

**Key facts:**
- `NetDbBuilder::build` is failure-atomic: if the second adapter open fails after the first succeeded, the first is closed before the error propagates so no orphan fold task outlives the failed build.
- `db.snapshot()` walks every enabled model under its own state lock (consistent **per-model**; no cross-model consistency guarantee — each model backs a separate RedEX file).
- `NetDbSnapshot::encode()` produces a single postcard blob; `decode()` round-trips; `build_from_snapshot(&bundle)` restores every enabled model in one call. Models enabled via `with_*()` whose bundle entry is `None` are opened from scratch.
- Same surface on Rust, Node (`@ai2070/net` napi), and Python (`net._net` PyO3). Postcard is stable across the FFI boundary.

---

## Common gotchas

- **`tasks.state()` returns an `Arc<RwLock<State>>` (Rust) / a snapshot dict (Python) / a JS object (Node).** Don't write through it; writes diverge from the RedEX log. Use the adapter's mutating methods (`tasks.create / .complete / .delete / .rename`) — they go through `ingest_with_token`.
- **`applied_through_seq` ≠ `folded_through_seq`.** RYW waits on applied (events that ran through the fold); folded includes events the fold task observed but skipped under `FoldErrorPolicy::Skip`. The pre-v0.15 `wait_for_token` waited on folded and returned `Ok(())` for skipped events — fixed in D-17.
- **`FoldStopped` is a real error.** The fold task can crash under `FoldErrorPolicy::Stop`; `wait_for_token` now surfaces `WaitForTokenError::FoldStopped` rather than a silent `Ok(())` (D-18).
- **The fold loop runs on a tokio task you don't see.** If you're embedding in a non-tokio runtime, you need a tokio runtime alive in the process — the substrate uses `tokio::spawn` under the hood.
- **`open_from_snapshot` restores the applied watermark.** Subsequent events fold from there, not from seq 0. The applied watermark is the canonical "where am I in the log" pointer.
- **Memory cost is `O(state_size)`, not `O(events)`.** Retention trim on the underlying RedEX file doesn't shrink the in-memory state — that's the fold's job (e.g., delete a Task removes it from state).
- **`Tasks` and `Memories` are concrete examples, not the whole CortEX surface.** Custom adapters land via `CortexAdapter::open(redex, channel, fold_fn)` with your own `State` struct + `RedexFold<State>` impl. The Tasks / Memories source is the reference template.

---

## When you need more

- **For the underlying log mechanics** (retention, replication, tail subscription) — see `redex.md`. CortEX sits on top.
- **For read-your-writes details** (WriteToken construction, `applied_through_seq` vs. `folded_through_seq`, FoldStopped semantics, non-blocking poll) — see `dataforts.md` § Read-your-writes.
- **For custom adapter authoring** — point at `net/crates/net/src/adapter/net/cortex/tasks/` and `cortex/memories/` as templates. The `CortexAdapter::open` + `RedexFold<State>` pair is the contract.
- **For NetDB cross-binding semantics** — see `net/README.md` § NetDB.
