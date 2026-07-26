# RedEX — local append-only log

RedEX is the **per-channel local persistence layer**. Not a database. Not a cluster. Not a broker. A `Redex` handle owns a directory; an open `RedexFile` is a single channel's append-only log on that node. Each node decides what to keep; there's no shared retention policy and no consensus. Replication is opt-in per file via `RedexFileConfig::replication` — covered separately at the end.

The bus is transient. RedEX is what you reach for when "I need to survive a node restart" or "I need to fold this stream into a CortEX state."

---

## Mental model

- **A `Redex` is a manager**, like a connection pool. One per process. Created with `Redex::new()` (in-memory, default) or `Redex::with_persistent_dir(path)` (disk-backed).
- **A `RedexFile` is one channel.** Open via `redex.open_file(name, config)`. Cheap to keep open; cheap to clone (`RedexFile` is `Arc`-internal).
- **Sequence numbers are local to the file**, monotonically allocated via `AtomicU64::fetch_add` on `append`. The first event lands at `seq = 0`; tail subscribers see strict prev+1 ordering.
- **Retention is per-file**, configured at open time (`RedexFileConfig::with_retention_max_events / _max_bytes / _max_age`). The substrate doesn't run a global retention sweep — you call `RedexFile::sweep_retention()` (or `Redex::sweep_retention()` for all open files) on a cadence.
- **Persistence isn't the default.** `RedexFileConfig::with_persistent(false)` is the in-memory mode (events vanish on close); `with_persistent(true)` writes to `<base>/<channel_path>/{idx,dat}` and survives restart. The disk-backed mode requires `Redex::with_persistent_dir(path)` on the manager.
- **Reopen is idempotent** for the same `(name, config)` — `redex.open_file(...)` returns the existing handle if structurally equal. Reopen with a different `ReplicationConfig` returns a typed `RedexError::Channel`.
- **Tombstones exist** (`RedexFlags::TOMBSTONE` on append) — compaction sweeps drop them; readers see the absence. Tombstones don't auto-delete previous events under the same key — RedEX is keyless; the application decides what "the same record" means.

---

## When to reach for RedEX

| Need | Use |
|---|---|
| "Survive a node restart with the last hour of events" | `RedexFileConfig::with_persistent(true)` + `with_retention_max_age(1h)` |
| "Tail this channel + apply each event into in-memory state" | `RedexFile::tail()` + a fold loop (or `CortEX` — see `cortex.md`) |
| "Replay everything from offset N" | `RedexFile::read_range(N, end_seq)` |
| "Fan out to multiple consumers locally" | Each consumer calls `tail()` independently — they're independent cursors |
| "Need cross-node durability" | Pair with `ReplicationConfig` (see `net/README.md` § Replication) or use the Redis / JetStream adapter on the bus instead |
| "Need queryable state, not a log" | `cortex.md` — RedEX is the substrate for CortEX |

---

## Rust

```rust
use net::adapter::net::{Redex, RedexFile, RedexFileConfig};
use std::sync::Arc;
use futures::StreamExt;

let redex = Arc::new(Redex::with_persistent_dir("/var/lib/net/redex"));
let file = redex.open_file(
    "orders/audit".parse()?,
    RedexFileConfig::new()
        .with_persistent(true)
        .with_retention_max_events(1_000_000)
        .with_retention_max_age(std::time::Duration::from_secs(7 * 24 * 3600)),
)?;

file.append(b"event-1")?;
file.append(b"event-2")?;

// Cursor-based read
for event in file.read_range(0, file.len() as u64) {
    println!("seq={} payload={}", event.seq, event.payload.len());
}

// Hot tail subscription
let mut tail = file.tail(net::adapter::net::RedexTailConfig::default())?;
while let Some(event) = tail.next().await {
    let event = event?;
    // ...
}

file.sync()?;   // flush in-memory writes to disk
file.close()?;  // closes this handle; outstanding tails see Closed
```

**Key facts:**
- `Redex::new()` is in-memory. `Redex::with_persistent_dir(path)` is disk-backed but still requires `RedexFileConfig::with_persistent(true)` per file.
- `append(payload)` returns `Result<u64, RedexError>` — the allocated seq. `append_batch(&[payloads])` is per-batch atomic.
- `append_postcard(&value)` serializes a `serde::Serialize` value with postcard. `append_inline(&[u8; 24])` is the small-payload fast path (avoids the heap segment).
- `tail()` returns an async `Stream<Item = Result<RedexEvent, RedexError>>`. Each subscriber gets its own cursor; the publisher fans out to all live tails plus persists to disk.
- `read_range(start, end)` is a bounded scan. Cheap when both endpoints are in memory; lands on disk reads only when retention has pushed events out of the in-memory window.
- `sync()` is a manual flush. `FsyncPolicy` is **`Never` by default** — appends don't fsync (`close()` still syncs); lowest latency, fine for telemetry / best-effort logs. Tighter bounds opt in: `EveryN(u64)` fsyncs after every N appends (off the hot path), `Interval(Duration)` fsyncs on a timer, and `IntervalOrBytes { period, max_bytes }` fsyncs when either the interval elapses **or** that many bytes accumulate. There is no `Periodic` or `Always` variant.
- `sweep_retention()` is the manual retention-trim call; call on a cadence (a tokio task in your application) when you want trim to run.
- `RedexFold<T>` trait + `append_and_fold(fold, value, state)` is the in-line fold path — convenient when you want to update local state in the same call that appends.

## Python

```python
from net import Redex, RedexError

redex = Redex(persistent_dir='/var/lib/net/redex')
file = redex.open_file(
    'orders/audit',
    persistent=True,
    retention_max_events=1_000_000,
    retention_max_age_secs=7 * 24 * 3600,
)

file.append(b'event-1')
file.append(b'event-2')

for event in file.read_range(0, file.len()):
    print(event.seq, len(event.payload))

# Hot tail
with file.tail() as tail:
    for event in tail:
        # event.seq, event.payload
        pass

file.sync()
file.close()
```

**Key facts:**
- `Redex(persistent_dir=...)` is the only constructor. `Redex()` is in-memory only.
- `open_file` kwargs mirror the Rust `RedexFileConfig`: `persistent`, `fsync_policy`, `max_memory_bytes`, `retention_max_events`, `retention_max_bytes`, `retention_max_age_secs`, `tail_buffer_size`, `replication=…`.
- `tail()` supports both `for event in tail:` (sync) and `async for event in tail:` (asyncio). Pick one per instance.
- `RedexError` is the catchable; the prefix on error messages is stable (`"redex: "`).
- The `cortex` open / tail / watch paths release the GIL via `py.detach` — long-running tail loops don't starve other Python threads.

## Node

```ts
import { Redex } from '@net-mesh/core';

const redex = new Redex({ persistentDir: '/var/lib/net/redex' });
const file = await redex.openFile('orders/audit', {
  persistent: true,
  retentionMaxEvents: 1_000_000n,
  retentionMaxAgeSecs: 7n * 24n * 3600n,
});

await file.append(Buffer.from('event-1'));
const events = await file.readRange(0n, await file.len());
for (const event of events) {
  console.log(event.seq, event.payload.length);
}

const tail = await file.tail();
for await (const event of tail) {
  // event.seq, event.payload
}

await file.sync();  // async — disk I/O on the napi worker pool
await file.close(); // async — same
```

**Key facts:**
- `RedexFile.sync()` and `RedexFile.close()` are **async** (return `Promise<void>`). Awaiting them matters: an un-awaited `close()` can let the process exit before the fsync lands.
- `BigInt` for sequence numbers and retention caps — JS numbers lose precision past 2^53. Don't pass plain `Number`.
- Per-channel config naming is camelCase; the underlying core accepts both shapes but Node binding rejects snake_case for tagged enums (`'colocationStrict'`, not `'colocation_strict'`).

## Go

```go
import "github.com/ai-2070/net/go"

redex := net.NewRedex(net.RedexConfig{PersistentDir: "/var/lib/net/redex"})
defer redex.Close()

file, err := redex.OpenFile("orders/audit", &net.RedexFileConfig{
    Persistent:          true,
    RetentionMaxEvents:  1_000_000,
    RetentionMaxAgeSecs: 7 * 24 * 3600,
})
if err != nil { /* … */ }
defer file.Close()

if _, err := file.Append([]byte("event-1")); err != nil { /* … */ }

events, _ := file.ReadRange(0, file.Len())
for _, event := range events {
    fmt.Println(event.Seq, len(event.Payload))
}

// Hot tail
tail, _ := file.Tail()
for event := range tail.Events() {
    _ = event
}
tail.Close()
```

**Key facts:**
- `RedexFile.mu` is a `sync.RWMutex` so concurrent appends / reads don't serialize. Earlier versions used a plain `sync.Mutex` which defeated the substrate's reader-counter.
- `runtime.SetFinalizer` runs `Close()` on the GC thread; for predictable cleanup, call `Close()` explicitly. The finalizer is the last-resort safety net.
- `OpenFile` returns `ErrInvalidReplicationConfig` for shape errors (factor / heartbeat ranges, empty pinned list) vs. `ErrReplicationRequiresEnable` for "you passed `Replication` but didn't call `EnableReplication(mesh)` first."

## C

```c
#include "net.h"

RedexHandle* redex = net_redex_new();
RedexFileHandle* file;
const char* cfg =
    "{\"persistent\":true,"
    " \"retention_max_events\":1000000,"
    " \"retention_max_age_secs\":604800}";
int rc = net_redex_open_file(redex, "orders/audit", cfg, &file);
if (rc != 0) { /* NET_ERR_REDEX */ }

uint64_t seq;
net_redex_file_append(file, (const uint8_t*)"event-1", 7, &seq);

RedexEventBuffer events;
net_redex_file_read_range(file, 0, net_redex_file_len(file), &events);
for (size_t i = 0; i < events.count; i++) { /* events.items[i].seq, .payload, .payload_len */ }
net_redex_free_event_buffer(&events);

RedexTailHandle* tail;
net_redex_file_tail(file, &tail);
RedexTailEvent ev;
while (net_redex_tail_next(tail, &ev) == 0) { /* … */ }
net_redex_tail_free(tail);

net_redex_file_sync(file);
net_redex_file_close(file);
net_redex_file_free(file);
net_redex_free(redex);
```

**Key facts:**
- Every `*_t` typedef is opaque; pass handles by pointer, free with the matching `*_free` call.
- `net_redex_open_file` / `net_redex_file_tail` pre-zero `*out_handle` / `*out_cursor` on entry, so a cgo / C consumer reading the slot after a non-zero return sees null rather than stale stack data.
- The replication config rides the `RedexFileConfigJson.replication` field as a JSON object; binding-side validators or the FFI core enforce numeric ranges.

---

## Cross-binding gotchas

- **Reopen with different config rejects.** `redex.open_file("foo", cfg_a)` then `redex.open_file("foo", cfg_b)` where `cfg_a != cfg_b` returns `RedexError::Channel("different from the original")` (compared structurally; `None ↔ None` and `Some(rep_a) ↔ Some(rep_b)` with matching shape both succeed). Tests that opened a channel with one config and reopened expecting silent reuse will see the typed error.
- **`sweep_retention()` is manual.** The substrate doesn't run a global retention task; you call it on a cadence from your application. Forgetting this is the most common "why is my disk filling up?" misconfiguration.
- **`append_batch` is per-batch atomic, not cross-batch.** All events in a single call land contiguously or none do; nothing serializes two distinct `append_batch` calls.
- **`tail()` is hot.** New tails see events from "now" forward, plus whatever's still in the tail buffer (`tail_buffer_size`, default 1024 events). For full replay, combine `read_range(0, current_len)` then `tail(from_seq=current_len + 1)`.
- **`RedexEvent::payload` is `Bytes` (zero-copy) in Rust.** Other bindings receive `bytes` / `Buffer` / `[]byte` / `uint8_t*` with the same zero-copy intent where the binding allows.

---

## Replication

For cross-node durability, pair `RedexFileConfig::with_replication(Some(ReplicationConfig { … }))` with a prior `Redex::enable_replication(mesh)` call. The per-channel replication runtime runs a 4-state machine (`Idle / Replica / Candidate / Leader`) over the `SUBPROTOCOL_REDEX` wire codec; nearest-RTT election with NodeId tiebreak, pull-based catch-up, bandwidth-budget gated.

```rust
redex.enable_replication(mesh.clone());
let file = redex.open_file(
    "orders/audit".parse()?,
    RedexFileConfig::new()
        .with_persistent(true)
        .with_replication(Some(ReplicationConfig {
            factor: 3,
            placement: PlacementStrategy::Standard,
            heartbeat_ms: 500,
            ..ReplicationConfig::default()
        })),
)?;
```

Two things must happen. `enable_replication(mesh)` installs the per-`Redex` router on the mesh's `SUBPROTOCOL_REDEX` dispatch (`0x0E00`) — idempotent, safe from multiple call sites. Then each `open_file` with `replication: Some(_)` spawns **one Tokio task per channel** (the coordinator: election, heartbeats, sync). Use the *same* `RedexFileConfig` on every node hosting a replica.

### `ReplicationConfig` fields — ranges, defaults, and what they cost

| Field | Range | Default | Notes |
|---|---|---|---|
| `factor: u8` | `[1, 16]` | `3` | Replicas *including* the leader. `1` collapses to single-node-with-coordinator (useful for testing the daemon lifecycle without peers). The `16` ceiling is conservative — heartbeat fanout makes overhead superlinear above ~8. **When `placement = Pinned(nodes)`, `nodes.len()` wins over `factor`.** |
| `placement: PlacementStrategy` | — | `Standard` | `Standard` scores candidates on `metadata.intent`, `metadata.colocate-with`, `scope:` tags, proximity, and resource availability. `Pinned(Vec<NodeId>)` is manual. `ColocationStrict` requires every replica to sit on a node already holding the chain named by `metadata.colocate-with-strict`, refusing insufficient-coverage nodes. |
| `heartbeat_ms: u64` | `>= 100` | `500` | **Failure detection is `3 × heartbeat_ms`** (three-missed hysteresis) — so the default declares a silent leader dead in ~1.5 s. Don't go below 100 ms; heartbeat traffic starts dominating the channel's throughput. |
| `leader_pinned: Option<NodeId>` | — | `None` | `None` = the deterministic nearest-RTT election picks the lowest-RTT healthy replica. `Some(node)` favors `node` whenever it's healthy. If `placement = Pinned(set)`, the pinned leader **must** be in `set` or validation rejects. |
| `on_under_capacity: UnderCapacity` | — | `Withdraw` | See below. |
| `replication_budget_fraction: f32` | `(0.0, 1.0]` | `0.5` | Fraction of measured NIC peak that sync I/O may consume, as a token bucket. Leaders reject `SyncRequest` with a `Backpressure` NACK when the bucket empties; replicas back off and retry the same request. (The denominator is a 1 Gbps placeholder today — the proximity-graph throughput probe wires the real measured peak in a follow-up.) |
| `default_bandwidth_class` | — | `Foreground` | Per-channel default `BandwidthClass` stamped on emitted `SyncRequest`s. Receivers honor a per-request override in preference to this. |
| `background_fraction: f32` | `[0.0, 1.0)` | `0.3` | Admission gate: a `Background` request is admitted only when `available >= (1 - background_fraction) * capacity`. Hot channels set it low (more Foreground headroom); archival channels set it high. `1.0` is **rejected** — it would deny every Background request unconditionally. An anti-starvation hatch one-shot bypasses the gate after 60 s of starving regardless. |

`validate()` runs the whole invariant set; call it before handing a config to `Redex` so a malformed one can't reach the coordinator.

**`UnderCapacity`** decides what a replica does when its local file rejects an append under disk pressure:

- **`Withdraw`** (default) — drop the replica role. The coordinator goes `Idle`, the `causal:<hex>` capability tag is withdrawn, and peers re-resolve to a healthy replica via `find_chain_holders`. Reads re-route automatically.
- **`EvictOldest`** — sweep retention to free space, keep the role, retry on the next chunk. **Requires `retention_max_*` on the same `RedexFileConfig`** — without retention caps the sweep is a no-op and the next apply fails again. This is the most common misconfiguration of the pair.

Either branch increments `under_capacity_total`, so the operator metric reflects every disk-pressure event regardless of policy.

### Lifecycle

`Idle` → (placement selects this node) → `Replica` (advertises `causal:<hex>`) → (leader silent for `3 × heartbeat_ms`) → `Candidate` → `elect()` → `Leader` / back to `Replica` / stay `Candidate` and retry. `close_file` returns it to `Idle` and unregisters the router. In the steady state the leader heartbeats, replicas observe its `tail_seq`, and a behind replica pulls with `SyncRequest` → `SyncResponse`.

**There is no leader-election message on the wire.** Election is a deterministic function over each node's locally-known state (proximity-graph RTT, replica-set membership, `NodeId` ordering); what peers actually observe is the capability tag being announced or withdrawn by the resulting `transition_to`. That's why the replication subprotocol has no `LeaderElection` dispatch code.

### `SyncNack` — the four typed rejections and their retry policy

When a leader refuses a `SyncRequest` it answers with a typed code, and the replica's remediation differs per code. Don't collapse them into a generic retry:

| Code | Meaning | Replica does |
|---|---|---|
| `1` `NotLeader` | you asked the wrong node | re-resolve the leader via `Mesh::find_chain_holders`, retry |
| `2` `BadRange` | requested range no longer retained | trim local tail, retry from the leader's first available `seq` (this is what bumps `skip_ahead_total`) |
| `3` `Backpressure` | leader's bandwidth bucket is empty | exponential backoff, retry **the same** request |
| `4` `ChannelClosed` | the leader closed the channel | withdraw the replica role, emit the metric |

### Observability + failure modes

Per-channel counters via `ReplicationMetricsRegistry::snapshot().prometheus_text()`: `dataforts_replication_lag_seconds{channel,role}` (gauge), `..._sync_bytes_total`, `dataforts_leader_changes_total`, `..._under_capacity_total`, `..._skip_ahead_total`, `..._election_thrash_total`, `..._witness_withdrawals_total` (reserved for a future witness phase), and `..._announce_divergence_total`.

That last one is easy to miss and worth an alert: it bumps when a `* → Idle` transition's withdraw-chain call **fails after the state cell already flipped to `Idle`**. While it's non-zero, the mesh may still be advertising this node as a chain holder for a channel it no longer replicates — readers get routed to a node that will not serve them. Recovery is opportunistic on the next `transition_to`, so a stuck non-zero value is a real problem, not a blip.

The registry is bounded by `MAX_TRACKED_CHANNELS`; channels past the cap fold into a shared `__overflow__` bucket. If you see `__overflow__` in a scrape, per-channel attribution is already lost for some channels — that's a cardinality signal, not a channel name.

| Symptom | Likely cause | Move |
|---|---|---|
| `lag_seconds` climbing | leader's bandwidth budget exhausted, or the replica's mesh path is saturated | raise `replication_budget_fraction`; check the proximity-graph throughput probe |
| `leader_changes_total` bumping often | `heartbeat_ms` too aggressive for the link's RTT variance | raise `heartbeat_ms`, or `leader_pinned` |
| `under_capacity_total > 0` + a replica vanished | `Withdraw` fired | free disk, or switch to `EvictOldest` (**and add retention caps**) |
| `skip_ahead_total > 0` | replica fell past the leader's retained range | accept the loss or raise the leader's retention caps |
| `election_thrash_total` rising (> 1 per 30 s) | two replicas oscillating under flaky connectivity | investigate the proximity graph; the partition detector should fire if it's partition-shaped |

Per-channel introspection (current role, manual transition for recovery) is `Redex::replication_coordinator_for(channel_name)` → `Arc<ReplicationCoordinator>`.

### Limits and non-goals

- **One writer per channel.** The leader is the single writer; RedEX is append-only and monotonic on `seq`. Multi-writer topologies are out of scope — don't design around one.
- **The replication factor is a hard guarantee; individual replicas are best-effort under pressure**, falling back to their `UnderCapacity` policy when local storage saturates.
- **Skip-ahead is heap-only.** When the leader trims past a replica's local tail, in-memory replicas can `skip_to(first_seq)` and retry; **persistent files reject `skip_to` with a typed error** and fall back to NACK + heartbeat-cycle recovery while the persistent-tier rebuild path is still in development.

For the wire codec and the full election rules, see `net/README.md` § Replication and `src/adapter/net/redex/`.

---

## Common questions

**"Do I need RedEX if I'm using the Redis adapter?"** No — Redis handles its own persistence. RedEX is the alternative for "don't want to run Redis; want per-node files." The two surfaces aren't meant to layer.

**"How do I do exactly-once delivery?"** RedEX itself is at-least-once on replication; exactly-once is an application-level concern (idempotent operations + event-hash dedup at the consumer). The substrate doesn't claim exactly-once.

**"Can I rotate files manually?"** No log rotation in the substrate sense — retention knobs are the only built-in trim mechanism. If you need archive-and-truncate semantics (compress old segments off to S3), build it on top of `read_range` + `sweep_retention`.

**"What's the relationship to CortEX?"** CortEX is "RedEX tail + a fold function → in-memory state with a query API." `cortex.md` covers the full picture. For raw log access, stay in RedEX.
