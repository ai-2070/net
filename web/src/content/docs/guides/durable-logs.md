# Durable Logs with RedEX

RedEX turns a channel into a durable, append-only log. Once a channel is opened as a RedEX file, every event published on it is appended in causal order, persisted to disk if you ask for it, and made available to consumers as a tail subscription. RedEX is the foundation that everything else in the storage stack — folded state, queries, replication — composes against.

The mental model is unromantic. A RedEX file is a named log. Producers append; consumers read; consumers can subscribe to the tail and receive new events as they land. There's no schema, no compaction, no transaction. The simplicity is the point — RedEX does one thing and the higher layers compose against that one thing.

## Opening a log

```rust
use net::adapter::net::redex::{Redex, RedexFileConfig, FsyncPolicy};
use std::sync::Arc;

let redex = Arc::new(Redex::new());

let cfg = RedexFileConfig::default()
    .with_persistent(true)
    .with_fsync_policy(FsyncPolicy::EveryN(100));

let file = redex.open_file("sensors/lidar/front", cfg)?;
```

`Redex` is the top-level manager: it owns the open files, handles the on-disk layout, and exposes per-channel handles. `open_file` either opens an existing log or creates a fresh one. The same call shape applies whether the channel is in-memory or disk-backed; the difference is the `with_persistent(true)` flag and a base directory configured on the `Redex` manager.

A persistent log is two on-disk files: an index file (20-byte records, one per event, fixed-size) and a data file (variable-size, packed). Recovery on reopen is bounded — the manager truncates any partial write at the tail of the data file and rebuilds the in-memory sequence map from the index — so a crash mid-write costs you nothing structural.

## Appending

Append is the only write path. Events go in monotonically by sequence; the log is single-writer, and the writer is the channel's authoritative publisher.

```rust
let seq = file.append(payload).await?;
```

`payload` is opaque bytes. RedEX doesn't decode it, doesn't validate it, doesn't transform it. The sequence number returned is what consumers reference when they want to resume from a specific point.

For higher-throughput append paths, use the batched variant — it amortizes the index write and the fsync:

```rust
let seqs = file.append_batch(&payloads).await?;
```

## Reading

Three reading patterns cover the cases that come up.

**Read a specific range.** Useful for inspection, audit, and re-derivation:

```rust
let events = file.read_range(100, 200).await?;
for event in events {
    process(event.seq, &event.payload);
}
```

**Tail subscription.** A long-lived stream that delivers events as they land. This is the path consumers use for reactive workloads:

```rust
use futures::StreamExt;

let mut tail = file.subscribe_tail().await?;
while let Some(event) = tail.next().await {
    process(event.seq, &event.payload);
}
```

**Resume from a cursor.** The tail-from-cursor flavor; useful for consumers that crash and restart and need to pick up where they left off:

```rust
let mut tail = file.subscribe_from(last_processed_seq).await?;
```

The cursor is just a sequence number. Persist it (in your own state, in another RedEX file, wherever fits) and you have at-least-once recovery for free.

## Choosing durability

`FsyncPolicy` controls when the log is fsynced to disk. Three options, each with a different trade-off:

| Policy            | Worst-case loss on crash                  | Use for                                        |
|-------------------|--------------------------------------------|------------------------------------------------|
| `Never`           | Tail since last `close()` or `sync()`     | Telemetry, caches, best-effort logs            |
| `EveryN(N)`       | ≤ N − 1 entries from the last sync point | Most application state (start with `EveryN(100)`) |
| `Interval(d)`     | ≤ d seconds of writes                      | State that must survive kernel panics          |

Two invariants are worth committing to memory:

- **`close()` always fsyncs**, regardless of policy. A clean shutdown loses nothing.
- **`file.sync()` always fsyncs.** It's the explicit barrier when you need a hard durability point — for example, before acknowledging a write to a caller that's relying on persistence.

Pick the loosest policy you can tolerate. The default (`Never`) is fastest and is the right answer for the large class of channels where loss-on-crash of the most recent events is acceptable.

## Retention

Logs grow without bound by default. To put a ceiling on size, configure retention on the file config:

```rust
let cfg = RedexFileConfig::default()
    .with_retention_max_bytes(Some(1024 * 1024 * 1024))  // 1 GB
    .with_retention_max_age(Some(Duration::from_secs(86400 * 7)));  // 7 days
```

Retention runs on append and on demand (`file.sweep_retention()`). Events past the retention horizon are evicted from the in-memory index — disk-backed events stay on disk until the next compaction pass, which is a separate concern. If you need hard caps, configure both bytes and age; the runtime applies whichever cuts first.

Retention only affects the head of the log. Active subscribers and recent readers don't lose events out from under them; the eviction targets data that's older than the cutoff and not actively held.

## Replication

A RedEX log on a single node lives and dies with that node's disk. To put a channel on more than one node, enable replication on the file config:

```rust
use net::adapter::net::redex::{ReplicationConfig, PlacementStrategy};

// First, install the replication plumbing on the Redex manager:
redex.enable_replication(mesh.clone());

// Then open the channel with replication configured:
let cfg = RedexFileConfig::default()
    .with_replication(Some(
        ReplicationConfig::new()
            .with_factor(3)
            .with_heartbeat_ms(500),
    ));
let file = redex.open_file("sensors/lidar/front", cfg)?;
```

`enable_replication` installs the per-`Redex` router on the mesh's replication subprotocol. After that, opening a channel with `replication: Some(_)` spawns a per-channel replication coordinator: leader election (deterministic by RTT and health), heartbeat-based liveness, and sync requests that bring replicas up to date.

Replication is a deep enough topic that it gets its own [reference page](../reference/replication-config) with the full list of knobs (placement strategies, bandwidth budgets, failure modes). The short version: turn it on per channel, set a replication factor, and the runtime handles the rest.

## When to reach for RedEX directly

Most application code doesn't open RedEX files by hand — it uses CortEX or NetDB, which open RedEX files on its behalf and give you a higher-level surface. You reach for RedEX directly in three cases.

The first is **append-only data that doesn't need a fold**. Audit logs, event sourcing for systems that don't have a current-state question to ask, raw telemetry that downstream consumers will fold themselves.

The second is **custom domain models**. CortEX ships with tasks and memories; if you need a different model with the same fold-driven shape, you implement `RedexFold<State>` against a RedEX log directly.

The third is **operational tooling**. Replaying a log, inspecting a range, exporting to another store, validating chain integrity — all of these are RedEX-level operations.

In all three cases, the surface is the same: open a file, append, read, subscribe. RedEX is small enough that the entire API fits on one screen.
