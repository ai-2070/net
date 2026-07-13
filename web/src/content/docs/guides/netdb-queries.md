# Querying with NetDB

NetDB is the query layer on top of CortEX. Where CortEX gives you one fold over one log with one state, NetDB bundles many folds under one handle, federates queries across them, and gives you a single snapshot/restore surface that covers your whole materialized state in one operation.

You reach for NetDB when "talk to the right CortEX adapter and read the right state" stops scaling — when you have queries that combine data from multiple folds, when you have multiple folds you want to manage as a unit, or when you want a single snapshot that captures everything at once.

## Opening a NetDB

```rust
use net::adapter::net::netdb::NetDb;

let db = NetDb::builder(redex)
    .with_tasks()
    .with_memories()
    .build()
    .await?;
```

A NetDB is a builder over a `Redex` manager, which it takes by value. `with_tasks()` and `with_memories()` opt in the two shipped models; the async `build` step opens their RedEX files and starts the fold tasks. (An optional `.origin(origin_hash)` stamps the producer identity on every event the bundled adapters append.) From that point on, the NetDB exposes a strongly-typed handle for each enabled model.

The two named models that ship — tasks and memories — get strongly-typed accessors:

```rust
db.tasks().state().read().count_where(&filter);
db.memories().watch().where_tag("important").stream();
```

The builder ships exactly these two models. A custom fold isn't registered through the builder — you drive it with a `CortexAdapter` directly (see the [CortEX guide](./cortex-folds)) and manage its handle alongside the NetDB.

## Queries

NetDB doesn't define a query language of its own. Each fold exposes its own query surface, and NetDB is the thing that keeps them composable. The two shipped folds give you a query builder pattern:

```rust
use net::adapter::net::cortex::tasks::{TaskStatus, OrderBy};
use net::adapter::net::cortex::memories::MemoriesFilter;

// Tasks: filter, order, paginate
let pending = db.tasks().state().read()
    .query()
    .where_status(TaskStatus::Pending)
    .order_by(OrderBy::CreatedDesc)
    .limit(50)
    .collect();

// Memories: filter by tag, lookup by id
let important = db.memories().state().read()
    .find_many(&MemoriesFilter {
        tag: Some("important".to_string()),
        ..Default::default()
    });
```

The queries take a brief read lock on the state and scan in memory. For the 10,000-entry, multi-field filter case, a typical query runs in tens to hundreds of microseconds — fast enough that "always query on access" is the right pattern for most flows.

## Watchers

`watch()` works the same way it does on a CortEX adapter, but the NetDB-level handles wrap it in a slightly more ergonomic surface:

```rust
let watcher = db.tasks()
    .watch()
    .where_status(TaskStatus::Pending)
    .order_by(OrderBy::CreatedDesc);

let (current, mut stream) = db.tasks().snapshot_and_watch(watcher);
render(&current);

while let Some(update) = stream.next().await {
    render(&update);
}
```

Watchers emit the current filter result on subscribe, then dedupe-emit on every state change that touches the filter. They're the substrate for live UIs, reactive dashboards, and anything else that needs to track state without polling.

## Federated queries (when they land)

The single-node query path is the foundation. The layer NetDB is building toward — federated queries that span folds across multiple nodes — uses the same query AST but compiles it down to a tree of fold reads and capability-routed RPCs:

```rust
// Federated query: every pending inference task across the GPU pool
let federated = db.federate()
    .where_capability(predicate!("hardware.gpu" exists))
    .query::<Tasks>()
    .where_status(TaskStatus::Pending)
    .collect()
    .await?;
```

Federated queries are portable structures. They travel to the nodes that have the data, execute there, and return results. There's no central coordinator and no global query plan — the federation primitive uses the same capability routing as nRPC, the same channel-roster mechanics as the bus, and the same identity guarantees as everything else.

Federation is a focused follow-up; the SDK surface is staged, and the API above will firm up in successive releases. The single-node query path is stable and is what you'd ship against today.

## Snapshots

`db.snapshot()` captures every registered fold's state into one postcard blob:

```rust
let bundle = db.snapshot()?;
write_to_disk("checkpoint.bin", &bundle.encode()?).await?;
```

The bundle records, per enabled model, its state's serialized bytes and the sequence number it was last folded to. On restore, the builder rehydrates each enabled model from the bundle and resumes its tail from where it left off:

```rust
use net::adapter::net::netdb::NetDbSnapshot;

let bundle = NetDbSnapshot::decode(&bytes)?;
let db2 = NetDb::builder(redex)
    .with_tasks()
    .with_memories()
    .build_from_snapshot(&bundle)
    .await?;
```

The whole-DB snapshot is the right primitive for backup, migration, and replication. It's smaller than the equivalent JSON by a healthy margin (60–70% in typical workloads), encodes in tens of microseconds at the 1k-entry scale, and round-trips deterministically.

The one limitation: postcard's encoding is tied to the field layout of your state types. If you change a fold's state struct between snapshot and restore — add a field, remove a field, change a type — the old bundle won't deserialize. The fix is to re-snapshot on upgrade; CortEX can replay from RedEX to rebuild the state, then snapshot again. There's no separate migration step because the log is the source of truth.

## When NetDB is the right level

The mental model: CortEX is the right level when you have one fold to manage. NetDB is the right level when you have many. Most production services start with a single CortEX adapter and grow into a NetDB when the second fold lands and the orchestration starts mattering.

Three signs you're ready for NetDB:

- **You're managing multiple `CortexAdapter` handles by hand.** NetDB is the bundling layer.
- **You want one snapshot for everything.** NetDB's whole-DB snapshot is the right tool.
- **You're about to write a query that crosses folds.** The federation surface is the right shape — even if you only use the single-node path today.

In all three cases, NetDB is additive over CortEX. The folds you wrote against CortEX directly slot into a NetDB without code changes; the migration is a builder call.
