# RedEX — local append-only log

RedEX is the **per-channel local persistence layer**. Not a database. Not a cluster. Not a broker. A `Redex` handle owns a directory; an open `RedexFile` is a single channel's append-only log on that node. Each node decides what to keep; there's no shared retention policy and no consensus. Replication (v0.14+) is opt-in per file via `RedexFileConfig::replication` — covered separately at the end.

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
- `sync()` is a manual flush; on `FsyncPolicy::Periodic` (default), the runtime flushes on a cadence. `FsyncPolicy::Always` syncs every append (durable but slow); `FsyncPolicy::Never` skips fsync entirely (fastest, no durability).
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
- `RedexFile.sync()` and `RedexFile.close()` are **async** (return `Promise<void>`). Pre-v0.14 sync call sites compile but generate orphan Promise warnings and may exit the process before the fsync lands.
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

- **`RedexFileConfig` field renames between versions.** v0.14 added `replication`; v0.15 didn't touch the file-config surface. If you have v0.13-era code, `with_persistent(true)` + retention knobs still work.
- **Reopen with different config rejects.** `redex.open_file("foo", cfg_a)` then `redex.open_file("foo", cfg_b)` where `cfg_a != cfg_b` returns `RedexError::Channel("different from the original")` (compared structurally; `None ↔ None` and `Some(rep_a) ↔ Some(rep_b)` with matching shape both succeed). Tests that opened a channel with one config and reopened expecting silent reuse will see the typed error.
- **`sweep_retention()` is manual.** The substrate doesn't run a global retention task; you call it on a cadence from your application. Forgetting this is the most common "why is my disk filling up?" misconfiguration.
- **`append_batch` is per-batch atomic, not cross-batch.** All events in a single call land contiguously or none do; nothing serializes two distinct `append_batch` calls.
- **`tail()` is hot.** New tails see events from "now" forward, plus whatever's still in the tail buffer (`tail_buffer_size`, default 1024 events). For full replay, combine `read_range(0, current_len)` then `tail(from_seq=current_len + 1)`.
- **`RedexEvent::payload` is `Bytes` (zero-copy) in Rust.** Other bindings receive `bytes` / `Buffer` / `[]byte` / `uint8_t*` with the same zero-copy intent where the binding allows.

---

## Replication (v0.14+)

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

Replication is a deep topic; this skill stays focused on the local log. For the wire codec, election rules, failover semantics, bandwidth budgets, and per-channel Prometheus metrics, point at `net/README.md` § Replication or `net/crates/net/docs/releases/RELEASE_v0.14_THE_WARRIORS.md`.

---

## Common questions

**"Do I need RedEX if I'm using the Redis adapter?"** No — Redis handles its own persistence. RedEX is the alternative for "don't want to run Redis; want per-node files." The two surfaces aren't meant to layer.

**"How do I do exactly-once delivery?"** RedEX itself is at-least-once on replication; exactly-once is an application-level concern (idempotent operations + event-hash dedup at the consumer). The substrate doesn't claim exactly-once.

**"Can I rotate files manually?"** No log rotation in the substrate sense — retention knobs are the only built-in trim mechanism. If you need archive-and-truncate semantics (compress old segments off to S3), build it on top of `read_range` + `sweep_retention`.

**"What's the relationship to CortEX?"** CortEX is "RedEX tail + a fold function → in-memory state with a query API." `cortex.md` covers the full picture. For raw log access, stay in RedEX.
