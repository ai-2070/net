# NetDB — query façade over CortEX state

> **Looking for how to use this?** [`STORAGE_AND_CORTEX.md`](STORAGE_AND_CORTEX.md) is the user-facing narrative. This doc is the implementation plan.

## Status

Design only. Most of the *substance* is already shipped — what NetDB adds is a **named layer** and a **unified multi-model handle** (`db.tasks`, `db.memories`, …) across Rust / TS / Python. No storage format changes, no new wire protocol.

Companion to:
- [`REDEX_PLAN.md`](REDEX_PLAN.md) — the append-only log primitive
- [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) — single-node storage finish (§6 mentions the NetDB surface that this doc owns)
- [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) — the event → state fold driver

## The layer picture

```text
┌────────────────────────────────────────────────────────────────┐
│ NetDB — queryable view over folded state                       │
│   db.tasks.findMany({ status: 'pending' })                     │
│   db.memories.findMany({ tag: 'urgent' })                      │
│   db.tasks.watch({...})  → async stream of filter results      │
└────────────────────────────────────────────────────────────────┘
                               ▲
                               │ exposes read + watch surface of
                               │
┌────────────────────────────────────────────────────────────────┐
│ CortEX adapter — folds events into State                       │
│   CortexAdapter<TasksState> / CortexAdapter<MemoriesState>     │
│   TasksAdapter.create / .rename / .snapshot / .watch           │
│   Owns Arc<RwLock<State>>; RedexFold drives apply              │
└────────────────────────────────────────────────────────────────┘
                               ▲
                               │ appends EventEnvelope → EventMeta → payload
                               │
┌────────────────────────────────────────────────────────────────┐
│ RedEX — append-only event log (raw)                            │
│   RedexFile::append(bytes), tail(from_seq) → RedexEvent        │
│   THIS is where raw events / streams live                      │
└────────────────────────────────────────────────────────────────┘
                               ▲
                               │ rides encrypted UDP when distributed
                               │
┌────────────────────────────────────────────────────────────────┐
│ Net — transport, routing, identity, channels, continuity       │
└────────────────────────────────────────────────────────────────┘
```

## One-line frame

**NetDB is a database-like query façade.** It answers "what does state look like right now, what can I look up, what changes should I subscribe to?" It is **not** a generic event stream; it does **not** expose RedEX payloads verbatim.

If you need raw events / streams, drop down to RedEX or Net:
- `redex.open_file(name).tail(from_seq)` → individual `RedexEvent`s
- `node.subscribe_channel(peer, channel)` → live channel packets
- In v2, specific `watch*` helpers that derive from state, not from raw payloads

## Where NetDB ends

**NetDB owns:**
- A `NetDb` Rust struct that bundles one or more CortEX adapters under a single handle (`db.tasks`, `db.memories`, future `db.facts`).
- A Prisma-ish query surface on each "table": `get(id)`, `findMany(filter)`, `count(filter)`, `exists(filter)`, `watch(filter)`.
- A typed DTO surface in TS / Python, matching the existing `Task` / `Memory` types we already ship.
- `snapshot()` / `openFromSnapshot()` helpers that work across the whole db (snapshot every bundled model atomically).

**NetDB does NOT own:**
- The RedEX file layout, segment management, or persistence primitives.
- The CortEX adapter lifecycle (fold task, `wait_for_seq`, error policy).
- Event ingestion (still via `db.tasks.create(...)` → `TasksAdapter::create` → RedEX append).
- Raw event streams. If you want `RedexEvent`s, that's RedEX. If you want filter-result changes, that's the watcher (already shipped).
- Any wire / subprotocol layer — NetDB is strictly local.

## What's already shipped vs. what's new

### Already shipped (v1 slice)

| Surface | Rust | Node | Python |
|---------|------|------|--------|
| `tasks.get(id)` | `state.get(id)` | `listTasks().find(...)` | `list_tasks(...)` filter |
| `tasks.findMany(filter)` | `state.query().where_*().collect()` | `listTasks(filter)` | `list_tasks(**kwargs)` |
| `tasks.count(filter)` | `state.query()...count()` | `count()` *(no filter)* | `count()` *(no filter)* |
| `tasks.watch(filter)` | `adapter.watch().where_*().stream()` | `watchTasks(filter)` | `watch_tasks(**kwargs)` |
| `tasks.snapshot()` | `adapter.snapshot()` | `snapshot()` | `snapshot()` |
| `tasks.restore(blob)` | `open_from_snapshot(...)` | `openFromSnapshot(...)` | `open_from_snapshot(...)` |

All of this works today, one adapter per model.

### What v1 NetDB adds

1. **Unified `NetDb` handle** — one object that bundles the adapters you want, exposes them as fields/properties. Users don't juggle separate `TasksAdapter` + `MemoriesAdapter` handles; they get a `db` with `db.tasks` and `db.memories` on it.
2. **Prisma-ish method aliases** at the model level — `findMany`, `findUnique`, `count`, `exists` — wrapping the existing Rust query builder and SDK filter methods. Pure sugar; existing methods stay available.
3. **Named `feature = "netdb"`** that pulls in `cortex` (adapter + all shipped models) + eventually future model features. Callers enable one flag to get "the database."
4. **Whole-db snapshot** — single call that snapshots every model and returns a bundle; restore via a matching single call.

### What v2 NetDB adds (not in this plan's core scope)

- Cursor-based pagination on `findMany` (currently `limit` only).
- Secondary indices under `NetDb` (§5 of REDEX_V2 — `RedexIndex<K>`) surfaced as `db.tasks.findByTag('work')` etc. when a model registers an index.
- Live-query subscriptions with handler callbacks: `db.tasks.subscribe({ status: 'pending' }, (tasks) => { ... })` in addition to the existing AsyncIterator form.
- Aggregations on query results (count, groupBy) where the numbers matter (fast paths when an index is available).

## Rust API sketch

```rust
// src/adapter/net/netdb/mod.rs (feature = "netdb")

use std::sync::Arc;
use parking_lot::RwLock;

use super::cortex::memories::{MemoriesAdapter, MemoriesState, Memory};
use super::cortex::tasks::{TasksAdapter, TasksState, Task};
use super::redex::Redex;

/// Unified NetDB handle. Construct via [`NetDb::builder`].
pub struct NetDb {
    redex: Arc<Redex>,
    tasks: Option<TasksAdapter>,
    memories: Option<MemoriesAdapter>,
}

impl NetDb {
    pub fn builder(redex: Redex) -> NetDbBuilder { ... }

    /// Access the tasks model. Panics if the DB wasn't built with
    /// tasks enabled. Use `try_tasks()` for a checked accessor.
    pub fn tasks(&self) -> &TasksAdapter {
        self.tasks.as_ref().expect("NetDb::builder().with_tasks() was not called")
    }
    pub fn try_tasks(&self) -> Option<&TasksAdapter> { self.tasks.as_ref() }

    pub fn memories(&self) -> &MemoriesAdapter { ... }
    pub fn try_memories(&self) -> Option<&MemoriesAdapter> { ... }

    /// Underlying Redex handle (for lifecycle operations).
    pub fn redex(&self) -> &Arc<Redex> { &self.redex }

    /// Close every enabled adapter. Idempotent.
    pub fn close(&self) -> Result<(), NetDbError> { ... }

    /// Snapshot every enabled model atomically (holds each adapter's
    /// state lock during serialization; different models snapshot
    /// independently — there's no cross-model consistency guarantee
    /// because models are separate RedEX files).
    pub fn snapshot(&self) -> Result<NetDbSnapshot, NetDbError> { ... }
}

pub struct NetDbBuilder {
    redex: Redex,
    origin_hash: u32,
    persistent: bool,
    want_tasks: bool,
    want_memories: bool,
}

impl NetDbBuilder {
    pub fn origin(mut self, origin_hash: u32) -> Self { ... }
    pub fn persistent(mut self, persistent: bool) -> Self { ... }
    pub fn with_tasks(mut self) -> Self { ... }
    pub fn with_memories(mut self) -> Self { ... }
    pub fn build(self) -> Result<NetDb, NetDbError> { ... }
}

/// Portable bundle of all enabled models' snapshots.
pub struct NetDbSnapshot {
    pub tasks: Option<(Vec<u8>, Option<u64>)>,
    pub memories: Option<(Vec<u8>, Option<u64>)>,
}
```

Query-time ergonomic aliases on the domain states (layered on top of the existing `query()` builder):

```rust
// Thin wrappers around state.query() for Prisma-ish surface.
impl TasksState {
    pub fn find_unique(&self, id: TaskId) -> Option<&Task> { self.get(id) }
    pub fn find_many(&self, filter: TasksFilter) -> Vec<Task> { /* apply filter fields */ }
    pub fn count_where(&self, filter: TasksFilter) -> usize { /* apply filter fields */ }
    pub fn exists_where(&self, filter: TasksFilter) -> bool { /* apply filter fields */ }
}

// TasksFilter is a plain struct mirroring the existing builder:
pub struct TasksFilter {
    pub status: Option<TaskStatus>,
    pub title_contains: Option<String>,
    pub created_after_ns: Option<u64>,
    pub created_before_ns: Option<u64>,
    pub order_by: Option<TasksOrderBy>,
    pub limit: Option<usize>,
}
```

The existing fluent `state.query().where_*()` stays available. The filter struct is the shape SDK bindings already use — this just lands it back in Rust for parity.

## TS API sketch

```ts
import { NetDb, type Task, type Memory } from '@ai2070/net';

// Builder — mirrors Rust.
const db = await NetDb.builder()
  .persistentDir('/var/lib/myapp/redex')
  .origin(0xABCDEF01)
  .withTasks()
  .withMemories()
  .build();

// Per-model, Prisma-ish.
const t = await db.tasks.get(1n);                       // Task | null
const pending = await db.tasks.findMany({
  status: 'pending',
  orderBy: 'created_desc',
  limit: 50,
});                                                      // Task[]

const count = await db.tasks.count({ status: 'completed' });
const exists = await db.tasks.exists({ status: 'pending' });

// Mutations — same adapters, surfaced as model methods.
await db.tasks.create({ id: 42n, title: 'ship netdb', nowNs: now() });
await db.tasks.complete({ id: 42n, nowNs: now() });

// Subscribe to a live view.
for await (const currentPending of db.tasks.watch({ status: 'pending' })) {
  render(currentPending);
}

// Whole-db snapshot/restore.
const snap = db.snapshot();
await fs.writeFile('db.snap', snap.encode());

const db2 = await NetDb.openFromSnapshot(snap, { persistentDir: '...' });
```

Internally, `db.tasks.findMany({...})` just calls `tasksAdapter.listTasks({...})` under the hood — same BigInt handling, same filter shape. `db.tasks.get(id)` wraps `listTasks({ where_id_in: [id] })[0] ?? null`.

## Python API sketch

```python
from net._net import NetDb

db = NetDb.builder() \
    .persistent_dir('/var/lib/myapp/redex') \
    .origin(0xABCDEF01) \
    .with_tasks() \
    .with_memories() \
    .build()

# Per-model, keyword-only filter args (same as existing TasksAdapter API).
t = db.tasks.get(1)
pending = db.tasks.find_many(status='pending', order_by='created_desc', limit=50)
count = db.tasks.count(status='completed')

# Iterate a live view (sync Python iterator).
for current in db.tasks.watch(status='pending'):
    render(current)

# Whole-db snapshot/restore.
snap = db.snapshot()
Path('db.snap').write_bytes(snap.encode())

db2 = NetDb.open_from_snapshot(snap, persistent_dir='...')
```

## Implementation steps

1. **`src/adapter/net/netdb/mod.rs`** behind feature `netdb = ["cortex"]`.
2. **`NetDb` struct + `NetDbBuilder`** — thin aggregation of existing adapters; `with_tasks()` / `with_memories()` call the corresponding `open` / `open_with_config`.
3. **`NetDbSnapshot`** — `{ tasks: Option<(Vec<u8>, Option<u64>)>, memories: Option<(...)> }` with a `.encode() -> Vec<u8>` helper (bincode serialize the bundle).
4. **`NetDbError`** — just a `From<CortexAdapterError>` wrapper for now.
5. **Prisma-ish aliases** on `TasksState` / `MemoriesState`:
   - `find_unique(id)` → `get(id)`
   - `find_many(filter)` takes a plain `TasksFilter` / `MemoriesFilter` struct, applies fields via the existing query builder
   - `count_where(filter)`, `exists_where(filter)` similarly
6. **Rust integration tests** — build a NetDb with both models, do CRUD, assert `db.tasks.find_many(...)` + `db.memories.find_many(...)` return the expected results; whole-db snapshot → restore round-trip.
7. **Node SDK** — `NetDb` napi class with `builder()` factory returning a `NetDbBuilder`. `db.tasks` / `db.memories` expose the existing `TasksAdapter` / `MemoriesAdapter` handles directly (no wrapping needed; they already have `listTasks` etc.). The "Prisma-ish" TS methods (`findMany`, `findUnique`, `get`) are thin JS helpers added to the auto-generated `TasksAdapter` class via prototype augmentation in `index.js`, OR exposed as a separate `Tasks` proxy class.
8. **Python SDK** — `NetDb.builder()` PyO3 class with chained `.with_*()` methods. `db.tasks` returns a `Tasks` proxy with `get`/`find_many`/`count`/`watch`. Delegates to the existing `TasksAdapter` internally.
9. **Smoke tests** in both SDKs — open db, CRUD, query, watch, snapshot+restore.
10. **README updates** — new top-level bullet for NetDB; new section in the crate README positioning NetDB as the recommended caller surface.

## Tests

- **Rust**: NetDb builder with both models opened against one `Redex`, verify `db.tasks` / `db.memories` access works; writes via `db.tasks.create(...)` show up in `db.tasks.find_many(...)`; whole-db snapshot then `open_from_snapshot` restores both models; closing `db.close()` stops both fold tasks.
- **Rust**: `find_unique` / `find_many` / `count_where` / `exists_where` produce the same results as the fluent `query().*` chain.
- **Node SDK**: same scenarios via vitest — `db.tasks.findMany({ status: 'pending' })` returns the expected array; `db.memories.findMany({ tag: 'x' })` works.
- **Python SDK**: same via pytest — `db.tasks.find_many(status='pending')`, `db.tasks.watch(status='pending')` iteration.
- **Whole-db snapshot**: tasks + memories created, snapshot taken, new Db built from snapshot — state of both models matches pre-snapshot.

## Non-goals (v1)

- **Cross-model transactions.** Each model is its own RedEX file; there's no atomic write across models. Callers that need it should model it in one stream (e.g. an `Action` event that the tasks fold interprets AND updates a memory).
- **Schema enforcement.** NetDB doesn't check that events match types beyond what bincode does at append time. Corrupt payloads surface as per-entry deserialization errors in `tail` / `read_range`.
- **Query DSL or aggregations.** No SQL, no `groupBy`, no JOINs. Filter objects only.
- **Wire / remote NetDB.** NetDB is strictly local. To sync state across nodes, events flow through Net + RedEX (eventually replicated in REDEX v3+). NetDB observes the resulting converged state; it doesn't itself talk to other nodes.
- **Event subscription at the DB layer.** If you want to react to *raw events*, use `RedexFile::tail(from_seq)`. NetDB's `watch(filter)` re-evaluates the filter on state change — it does NOT hand you the event that caused the change.

## Risks and open questions

- **Method naming drift.** Today's TS surface is `listTasks(filter)` / `listMemories(filter)`. Prisma-ish rename to `findMany` on a `db.tasks` proxy is a different surface. Decision: **keep both.** The adapter-level methods stay as a lower-level API; the `db.tasks.findMany` proxy is the higher-level façade. Deprecate neither.
- **Proxy implementation in TS.** napi doesn't directly give us nested `db.tasks.findMany`. Either a JS-side `NetDb` wrapper class that composes the adapters, or a Rust-side aggregator. The JS-side wrapper is simpler and lighter — no new napi surface, just a small class in `index.js` or a companion file.
- **Feature gating.** `netdb = ["cortex"]` and `cortex` bundles adapter core + tasks + memories. As more models land they go inside `cortex` (or, if binary-size pressure appears, split into sub-features like `cortex-facts`). Decision for v1: **one flag per layer — `cortex` bundles every shipped model**; sub-flags only if pressure warrants.
- **Snapshot compatibility across schema changes.** If `Task` gains a field in a future version, old snapshots decode into a `Task` missing that field → bincode error. Need a serde-compatible migration path OR document "snapshots are tied to their app version." Flag as a future concern; bincode doesn't do schema evolution gracefully, so we may eventually switch to serde_json for snapshot blobs (trading size for forward-compat).
- **Watch semantics under NetDB.** Our existing `watch(filter).stream()` yields the full filter result on every change (deduplicated). Prisma-ish expectations might be "emit delta events" (added / removed / updated). We've explicitly chosen full-result-set emissions — callers diff if they care. Document this clearly.

## Summary

NetDB is a naming + composition layer. It:

- Positions the existing CortEX query/watch surfaces under a unified `db.tasks` / `db.memories` façade.
- Adds Prisma-ish method aliases (`findMany`, `findUnique`, `count`, `exists`) for familiarity.
- Ships a `NetDb` Rust struct + SDK companion classes that bundle adapters behind one handle.
- Gets a single `netdb` feature flag that turns on the full database.
- Does **not** expose raw events or streams — those stay at the RedEX / Net layer.

Implementation is mostly sugar + composition; the load-bearing infrastructure (RedEX, CortEX adapter, tasks + memories models with query + watch + snapshot) already ships. This plan is a one-session chunk per Rust / Node / Python, plus a docs pass.
