# RedEX v1 — local append-only streaming log

> **Looking for how to use this?** [`STORAGE_AND_CORTEX.md`](STORAGE_AND_CORTEX.md) is the user-facing narrative (API surface, durability trade-offs, restart behavior). This doc is the implementation plan.

## Status

Design only. RedEX does not exist yet.

**v1 is a thin local slice.** One node, in-memory by default with optional simple disk segment, append + tail + read_range. No replication, no dedicated control-plane subprotocol, no partition healing.

v2's additive roadmap (still single-node: tiering, time-based retention, cold tier, typed wrappers, indices, ordered-append helper) lives in [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md). The deterministic scheduler design (parked until there's a DST harness or replica protocol) lives in [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md).

The design decisions that carry forward unchanged — 20-byte records, inline+heap hybrid, file = `ChannelName` — are frozen here so v2 is additive, not a redesign.

## One-line frame

RedEX is a log-structured append-only event store that lives inside `MeshNode`. A "file" is a named monotonic log. Writes are local fire-and-forget appends; reads are live tails. No broker, no commit, no consensus. v1 is one node; distribution is a future, separate plan.

## What Net already provides (RedEX is glue, not greenfield)

- **`ChannelName` + `ChannelConfig`** — hierarchical named endpoints with per-channel policy (`docs/CHANNELS.md`). RedEX files map 1:1 to channel names.
- **`AuthGuard`** — wire-speed channel ACL check. Applied on local `append` and `tail` so the ACL surface is consistent when replication turns on later.
- **`CausalEvent` + `CausalLink`** — 24-byte causal chain. RedEX records are a 20-byte projection of this; the alignment keeps v2+ replication additive.

None of this needs to be built from scratch. RedEX is the glue.

---

## v1 scope — what ships

What ships in v1:

- `Redex` manager: `open_file` / `get_file`.
- `RedexFile` handle: `append`, `append_batch`, `append_bincode`, `tail`, `read_range`, `close`.
- `RedexEvent` yielded by tail/read_range.
- `RedexFileConfig` — explicit struct for persistence and retention policy.
- In-memory index (`Vec<RedexEntry>`) + in-memory payload segment (`Vec<u8>`).
- One optional disk-backed segment (simple append-only file per file, no mmap, no rollover) for durability-required callers.
- Per-channel ACL enforcement on `append` / `tail` via the existing `AuthGuard`.
- Basic retention: count-based (keep newest K events) and size-based (keep newest M bytes of payload).
- `RedexFold<State>` trait — the integration hook for CortEX / NetDB. Defined here, installed by the adapter layer; RedEX does not call it itself.
- Feature gate: `#[cfg(feature = "redex")]` on the core `net` crate.

What v1 does NOT do (by design, not oversight):

- No replication. `append` touches local state only.
- No `SUBPROTOCOL_REDEX`. No control-plane wire messages.
- No `RedexReplicaDaemon`. No remote tailing.
- No partition / conflict / forking logic.
- No cold-tier archive, no mmap warm tier, no time-based retention, no typed wrappers. See [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md).
- No deterministic scheduler. The raw `AtomicU64::fetch_add` seq allocator is good enough for v1's workloads (mostly one main writer, or a few threads in one process). See [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md) for why it's parked.

The goal is a primitive CortEX can build on. Distribution comes later, once the single-node shape is right.

---

## Design

### 1. The 20-byte event record

Every event occupies exactly 20 bytes of index space. Payloads live inline or in a separate segment (§2). Layout:

```rust
#[repr(C, packed)]
pub struct RedexEntry {
    /// Monotonic per-file sequence. Allocated by the local appender
    /// via an `AtomicU64`; never resets.
    pub seq: u64,                  // 8 bytes
    /// Byte offset into the file's payload segment. 32-bit gives 4 GB
    /// per live segment; v2 tiering rolls segments.
    pub payload_offset: u32,       // 4 bytes
    /// Payload length in bytes. `0` + `INLINE` flag means the payload
    /// rides in the record's own bytes (see below).
    pub payload_len: u32,          // 4 bytes
    /// High nibble: flags (INLINE, TOMBSTONE, COMPACTED, …).
    /// Low 28 bits: xxh3 truncation of the payload, for tamper/dedup.
    pub flags_and_checksum: u32,   // 4 bytes
}
// Total: 20 bytes.
```

**Inline payloads.** When `flags & INLINE != 0`, `payload_offset` and `payload_len` are reinterpreted as 8 inline payload bytes. Small structured events (sensor readings, tick counters) avoid the payload-segment indirection entirely.

**Cache-line geometry.** 20 bytes doesn't divide cleanly into 64-byte cache lines (3.2 records/line). Deliberate trade-off: we pay ~20% per-access cache efficiency vs 16-byte records in exchange for carrying a payload locator in every record. 24-byte records would be denser at 2.67/line but lose record atomicity on some wider loads. 20 is the sweet spot under the spec.

### 2. Payload storage — inline + heap hybrid

Each file has one or more **segments**. A segment is a contiguous byte region addressed by `(segment_id, offset)`. v1 backing modes:

- **In-memory heap** (default): `Vec<u8>` grown append-only.
- **Simple disk segment** (opt-in via `RedexFileConfig::persistent`): append-only file, no mmap, no rollover. Writes fsync in the background.

Records with the `INLINE` flag don't consume payload-segment bytes. Callers emitting small fixed-size events (8 bytes of inline capacity) hit zero per-event segment allocation.

### 3. `RedexEvent` — the tail/read_range yield type

```rust
pub struct RedexEvent {
    /// The 20-byte on-disk record, verbatim.
    pub entry: RedexEntry,
    /// The materialized payload. For INLINE entries this is the 8-byte
    /// inline region; for heap entries it's the slice read from the
    /// payload segment.
    pub payload: Bytes,
}
```

`seq`, `flags`, and `checksum` are reachable via `event.entry.seq`, `event.entry.flags_and_checksum`. Keeping `entry` whole means no duplicated fields and callers that want to project a flat view can do so trivially.

### 4. `RedexFileConfig`

```rust
pub struct RedexFileConfig {
    /// Heap-only (`false`) vs heap + simple disk segment (`true`).
    pub persistent: bool,
    /// Soft cap on heap payload bytes. Hit this and retention sweeps
    /// evict the oldest entries on the next heartbeat tick. v1 honors
    /// this as a retention trigger only; v2's warm-tier rollover is
    /// out of scope here.
    pub max_memory_bytes: usize,
    /// Keep only the newest K events. `None` = unbounded.
    pub retention_max_events: Option<u64>,
    /// Keep only the newest M bytes of payload. `None` = unbounded.
    pub retention_max_bytes: Option<u64>,
}
```

Time-based retention is v2. `RedexFileConfig` deliberately does not carry a `retention_max_age` field in v1 so callers can't rely on a semantic that isn't enforced.

### 5. `Redex` manager

```rust
pub struct Redex {
    // map from ChannelName -> RedexFile handle
}

impl Redex {
    /// Open (create if absent) a RedEX file bound to `name`. ACL is
    /// enforced against the existing ChannelConfig for `name` — the
    /// caller needs publish caps to open for append and subscribe caps
    /// to open for tail.
    pub fn open_file(
        &self,
        name: &ChannelName,
        config: RedexFileConfig,
    ) -> Result<RedexFile, RedexError>;

    /// Look up an already-opened file. Returns None if it's not open.
    pub fn get_file(&self, name: &ChannelName) -> Option<RedexFile>;
}
```

The manager is the only way to get a `RedexFile` handle. It owns the `ChannelName -> RedexFile` map and the per-file ACL-binding step so every callsite goes through the same gate.

### 6. Append semantics

```rust
impl RedexFile {
    /// Append one event. Returns the assigned sequence number.
    /// Fire-and-forget: local index and payload segment are updated
    /// before return. Durability is background (if `persistent`).
    pub fn append(&self, payload: &[u8]) -> Result<u64, RedexError>;

    /// Append many events. Per-batch atomic: all events land in the
    /// index contiguously or none do.
    pub fn append_batch(&self, payloads: &[Bytes]) -> Result<u64, RedexError>;

    /// Convenience: serialize with bincode and append. Very common
    /// callsite shape.
    pub fn append_bincode<T: serde::Serialize>(
        &self,
        value: &T,
    ) -> Result<u64, RedexError>;
}
```

Sequences come from a per-file `AtomicU64::fetch_add`. Concurrent appends race on that CAS; the winning order is whatever the CPU decides. Under v1's expected workload (mostly one main writer), this is fine. Under contention it's non-deterministic. See [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md) for the v2+ path to determinism.

Write latency target: one CAS (seq allocate) + one memcpy (payload to segment) + one store (index record) = **tens of nanoseconds** for inline payloads, low hundreds for heap payloads. No locks, no syscalls on the hot path.

### 7. Tail

```rust
impl RedexFile {
    /// Tailing subscription. Receives every event from `from_seq` onward.
    /// Slow local subscribers drift their offset; the channel is
    /// effectively unbounded in v1 since consumers are in-process.
    pub fn tail(
        &self,
        from_seq: u64,
    ) -> impl Stream<Item = Result<RedexEvent, RedexError>>;

    /// One-shot read of [start, end). Second-class; bounded to the hot
    /// tier. Cold reads are v2+.
    pub async fn read_range(&self, start: u64, end: u64) -> Vec<RedexEvent>;
}
```

**Implementation shape.**

```rust
struct TailWatcher {
    from_seq: u64,
    sender: mpsc::UnboundedSender<Result<RedexEvent, RedexError>>,
}

impl RedexFile {
    fn tail(&self, from_seq: u64) -> impl Stream<...> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.register_tail(TailWatcher { from_seq, sender: tx });
        ReceiverStream::new(rx)
    }

    // Called internally on each append.
    fn notify_watchers(&self, event: &RedexEvent) {
        for w in self.watchers.iter() {
            if event.entry.seq >= w.from_seq {
                let _ = w.sender.send(Ok(event.clone()));
            }
        }
    }
}
```

Simple to reason about; no over-optimization in v1. Closed watchers (receiver dropped) are swept on the next notify.

**No consumer offsets.** Subscribers pass `from_seq` on every `tail()` and track their own position. RedEX remembers nothing about who has read what.

**No ack, no commit.** The stream delivers events in sequence order; the caller observes them. Dropping the stream is the unsubscribe signal.

**Random access is second-class.** `read_range` exists but is bounded to the hot tier. Workloads that want full-history random access build an index on top — see §9 `RedexFold`.

### 8. File naming + ACL

A RedEX file name IS a `ChannelName`. Everything that applies to channels applies to files:

- Hierarchical naming (`/sensors/lidar/front`).
- Capability-gated append (`ChannelConfig::publish_caps`) and tail (`subscribe_caps`).
- Wire-speed `AuthGuard` bloom-filter check on every operation.
- Permission tokens (`PermissionToken`) work unchanged.

v1 applies ACL locally even without remote peers — keeps the surface consistent when replication turns on.

### 9. Retention

Per-file retention policy set at `open_file`. v1 ships count-based + size-based:

- **Count-based** (`retention_max_events`): keep newest K events.
- **Size-based** (`retention_max_bytes`): keep newest M bytes of payload.
- **Soft memory cap** (`max_memory_bytes`): retention trigger.

Retention runs as a background task on the heartbeat-loop tick. No retention check on the read path.

Time-based retention is v2.

### 10. `RedexFold` — the integration hook for CortEX / NetDB

```rust
pub trait RedexFold<State> {
    /// Apply one event to `state`. Called in seq order by whoever
    /// drives the fold (typically a CortEX adapter owning the tail
    /// stream).
    fn apply(
        &mut self,
        ev: &RedexEvent,
        state: &mut State,
    ) -> Result<(), RedexError>;
}
```

RedEX defines the trait; RedEX itself does not install a fold. CortEX (see [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md)) owns:

- Opening a `RedexFile` tail.
- Supplying an `impl RedexFold<CortexState>`.
- Driving `state` forward as events arrive.
- Exposing query surface over the materialized state.

v1's contract is: `tail` is expressive enough that a fold can run against it. That's it.

---

## v1 implementation steps

1. **`RedexEntry` codec** — 20-byte packed struct + `to_bytes` / `from_bytes`. Unit tests: round-trip, INLINE vs heap flag, checksum.
2. **`HeapSegment`** — `Vec<u8>` payload store with `append(payload) -> u32` returning offset. Unit tests: grow, bounds, capacity.
3. **`DiskSegment`** *(opt-in, sub-feature `redex-disk`)* — append-only file with background fsync.
4. **`RedexFileConfig` + `RedexEvent` + `RedexError`** — public types.
5. **`RedexFile` core** — per-file `AtomicU64` seq, `Vec<RedexEntry>` index, segment handle; `append`, `append_batch`, `append_bincode`.
6. **Tail watcher registry** — `Vec<TailWatcher>` behind a parking_lot Mutex; `register_tail`; `notify_watchers` on append.
7. **`RedexFile::tail`** — returns a `ReceiverStream`. Backfill from `from_seq` in a spawn task, then register the watcher for live events (boundary handled by holding the watcher lock across the backfill → register step).
8. **`RedexFile::read_range`** — bounded scan of the in-memory index.
9. **Retention background task** — count-based + size-based eviction on the heartbeat tick.
10. **`Redex` manager** — `open_file`, `get_file`; per-file ACL binding via `AuthGuard`.
11. **`RedexFold` trait** — defined in `redex::fold`; no in-crate users in v1.
12. **Docs** — `REDEX.md` (caller-facing contract). Link from `TRANSPORT.md` and `CHANNELS.md`.

## v1 tests

- **Unit**: 20-byte codec round-trip; `HeapSegment` append + read; retention eviction; INLINE vs heap payload decoding; ACL denial on append / tail; `append_bincode` round-trip.
- **Single-node integration**: open a file via `Redex::open_file`; append 10k events; tail from `seq=0` on a spawned task; assert every event is received in order with matching payload and checksum. Repeat with INLINE-only payloads.
- **Tail boundary**: open a file, append N events, then start a tail from seq 0; assert no duplicates and no drops across the backfill→live boundary.
- **Durability** *(with `redex-disk`)*: append, crash-simulate (drop handle without close), reopen, assert recovered index matches pre-crash minus unsynced tail.
- **Retention**: append beyond count cap; assert oldest events drop on the sweep; assert `read_range` over evicted range returns the expected short-read.
- **Fold smoke**: toy `impl RedexFold<u64>` that sums payload lengths; drive it from a `tail` stream; assert final state matches expected sum.
- **Benchmark**: append throughput (events/sec) for INLINE vs heap payloads; tail latency (append → subscriber observes).

## Non-goals (v1)

- **Strong consistency.** No consensus, no linearizability.
- **Cross-file transactions.** Two-file atomic append is not supported.
- **Schema registry.** Payloads are opaque bytes.
- **Log compaction by key** (Kafka compacted-topic style).
- **Automatic consumer offsets.** Subscribers track their own position.
- **Deterministic seq under concurrent writers.** Raw CAS is used; see scheduler plan for when/why we'd revisit.

## Risks and open questions

- **"Filesystem" framing vs reality.** RedEX has no POSIX semantics — no seek, no truncate, no fds. The name is the user's call; the docstring is precise about what it isn't. Consider a secondary "log store" label in callsite docs.
- **20 vs 24 bytes.** 24 aligns with `CausalLink` and 64-byte cache lines (2.67/line). 20 is the user's call; the ~20% cache-efficiency cost is documented.
- **Non-deterministic seq under contention.** The v1 CAS-seq allocator is nondeterministic when multiple threads append concurrently. Parked deliberately — no DST harness or replica protocol yet consumes determinism. See [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md) and revisit when either lands.
- **Tail backfill vs live race.** The register-watcher + backfill-from-seq sequence must hand off without gaps or dupes. Covered by the tail-boundary test.
- **CortEX coupling.** `RedexFold` is a trait only; CortEX owns the implementation. If CortEX grows a requirement that needs a shape change (e.g., back-pressure on the fold), we revisit — but unlikely.
- **Name collision.** RedEX and CortEX both use the `EX` suffix. Keep the aesthetic; callsite docs disambiguate (RedEX = storage primitive, CortEX = query plane).
- **Durability semantics.** Shipped with `FsyncPolicy` (see section below); the old `sync_on_append` placeholder is superseded. Default is `Never` — no append-path fsync, `close()` is the durability barrier. Callers opt into `EveryN(N)` or `Interval(d)` when they need tighter bounds.

## Durability & crash semantics (shipped)

Disk-backed files (`RedexFileConfig::persistent = true` + a `Redex` manager configured with `with_persistent_dir`) are governed by a per-file [`FsyncPolicy`](../src/adapter/net/redex/config.rs):

```rust
pub enum FsyncPolicy {
    Never,                 // default — no fsync on append
    EveryN(u64),           // fsync every N appends
    Interval(Duration),    // fsync on a timer
}
```

Set via `RedexFileConfig::default().with_fsync_policy(...)` at `Redex::open_file` time.

| Policy | Worst-case loss — process crash | Worst-case loss — kernel / power crash |
|--------|---------------------------------|---------------------------------------|
| `Never` (default) | Tail since last close / explicit `sync()` | Same |
| `EveryN(N)` | Up to `N − 1` entries from the last sync point | Same |
| `Interval(d)` | Up to `d` seconds of writes | Same |

### Invariants

- `close()` **always** fsyncs, regardless of policy. `Never` means "no fsync on the append path," not "no durability at all." A clean shutdown of a `Never`-configured file loses nothing.
- Explicit `RedexFile::sync()` always fsyncs, regardless of policy. It's the caller's on-demand durability barrier.
- Torn-write recovery is fsync-independent. The dat-before-idx write ordering plus reopen-time truncation handles arbitrary partial writes — no policy can leave the file unrecoverable, only shorter than you'd like.
- `EveryN(0)` and `EveryN(1)` both collapse to "fsync on every append." `EveryN(N)` for `N > 1` bounds loss at `N − 1` entries.
- `Interval(d)` spawns one tokio background task per file. `close()` cancels it cleanly; dropping a `RedexFile` without `close()` leaves the task alive until runtime shutdown (consistent with the rest of the file lifecycle).

### Choosing a policy

- `Never` — telemetry, best-effort logs, caches where losing the tail is acceptable. Lowest latency.
- `EveryN(100)` — most application state. Bounds loss at 99 entries; negligible overhead.
- `Interval(1s)` — anything that must survive kernel panic / power loss. Bounds loss at one second of writes regardless of append rate.
- `EveryN(1)` — journalled / audit-grade streams where every entry must be durable before the next write logically happens.



Scope intentionally held out of v1, tracked in separate plan docs:

- **v2 local scope** — [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md). Hot→warm tiering (mmap), time-based retention, cold-tier archive interface, typed file wrappers, `append_and_fold`, local indices, NetDB watch APIs, single-threaded `OrderedAppender` helper. All still single-node; no replication.
- **Deterministic scheduler** — [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md). Parked until there's a DST harness or a replica protocol that would consume determinism.
- **Replication (distributed RedEX)** — not planned yet. Needs real single-node usage, a clear DST story, and concrete requirements from pilots before we design it. Will be its own plan when the time comes, riding `ChannelPublisher` / `SubscriberRoster` / the causal-chain machinery.
- **CortEX adapter** — `EventMeta` projection, fold installation, NetDB query surface. See [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md); RedEX v1's responsibility is just to expose a `RedexFold` trait and a `tail` shape the adapter can drive.

## Summary

v1 is the smallest possible slice: `Redex` manager, `RedexFile` with `append` / `append_batch` / `append_bincode` / `tail` / `read_range`, in-memory by default with optional simple disk segment, file-as-channel ACL, count + size retention, and the `RedexFold` trait for CortEX to install against. One node. No replication, no subprotocol, no partition healing, no cold tier, no time retention, no scheduler.

The 20-byte record layout, INLINE + heap hybrid payload, and channel-name-as-file choices are frozen. Ship the local slice; layer v2 on top; let the scheduler and replication plans mature against real usage before building them.
