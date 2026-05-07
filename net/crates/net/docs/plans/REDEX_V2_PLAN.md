# RedEX v2 — local storage story, finished

## Status

Design only. Scheduled after RedEX v1 ships and sees real single-node usage. Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md).

**v2 is still local-only.** No replication, no `SUBPROTOCOL_REDEX`, no multi-node convergence. The frame is *"finish the local storage story and make it nicer to use"* — better durability and memory footprint on one node, more ergonomic APIs, local query helpers. Cross-node RedEX is a later, explicit "distributed RedEX" plan once we have real single-node usage, a clear DST story, and concrete pilot requirements.

## What's in scope

1. Hot → warm tiering with mmap (§1).
2. Time-based retention (§2).
3. Cold-tier archival interface (§3).
4. Ergonomic APIs: typed file wrappers, `append_and_fold` (§4).
5. Local secondary indices (§5).
6. NetDB enhancements — more models, watch APIs, snapshot + tail (§6).
7. Optional single-threaded `OrderedAppender` determinism helper (§7).

Explicit **non-goals** for v2 (§8): replication, subprotocol, replica convergence, fork handling. Those belong to a distinct future plan.

---

## 1. Hot → warm tiering with mmap

v1: heap segment + optional simple append-only disk file.
v2: flesh out the hot/warm split cleanly.

- **Mmap warm segments.** When a heap segment exceeds `max_memory_bytes`, roll it into an immutable disk file, mmap'd for reads. New appends go to a fresh heap segment.
- **Background sync policy.** Configurable:

```rust
pub enum FsyncPolicy {
    Never,
    EveryN(u64),
    Interval(Duration),
}
```

A background task handles fsyncs for disk segments; the write path stays a memcpy + `Vec::push`. No fsync on the hot path, ever.

- **Segment rollover.** Closed segments are immutable; read path checks live heap first, then mmap'd warm segments in seq-range order.

## 2. Time-based retention

v1: count-based and size-based.
v2: add proper time-based.

```rust
pub enum Retention {
    Events(u64),
    Bytes(u64),
    Age(Duration),
    Infinite,
}
```

Retention sweep:

- Uses per-entry timestamps stored in payload, or derived from `seq_or_ts` if the caller packs the timestamp into the seq namespace.
- Marks old segments as droppable.
- Deletes them on the next maintenance cycle.

Still local only; useful for logs and telemetry deployments.

## 3. Cold tier — archival, local or off-box

No replication, but allow moving old segments out of the hot path.

```rust
pub trait ColdStore {
    fn archive_segment(
        &self,
        file: &ChannelName,
        segment_id: u64,
        data: &[u8],
    ) -> io::Result<()>;

    fn fetch_segment(
        &self,
        file: &ChannelName,
        segment_id: u64,
    ) -> io::Result<Vec<u8>>;
}
```

Implementations:

- **Local disk directory** (default).
- S3 / HTTP / other — later, all initiated by this node.

Caller-facing API:

```rust
impl RedexFile {
    pub async fn read_cold(
        &self,
        start_seq: u64,
        end_seq: u64,
    ) -> Result<Vec<RedexEvent>, RedexError>;
}
```

Archival is per-node; no cluster semantics. A node can archive to its own disk or to its own S3 bucket, but it doesn't coordinate with other nodes about what's archived where.

## 4. Ergonomic APIs

Without touching protocol or replication, v2 makes RedEX and NetDB much nicer to use.

### 4a. Typed file wrappers

```rust
pub struct TypedRedexFile<T> {
    inner: RedexFile,
    _marker: PhantomData<T>,
}

impl<T> TypedRedexFile<T>
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de>,
{
    pub fn append_typed(&self, value: &T) -> Result<u64, RedexError>;

    pub fn tail_typed(
        &self,
        from_seq: u64,
    ) -> impl Stream<Item = Result<(u64, T), RedexError>>;
}
```

Callers stop (de)serializing manually in every place.

### 4b. `append_and_fold`

Common pattern: append event, immediately fold into in-mem state. Wrap it:

```rust
impl RedexFile {
    pub fn append_and_fold<T, F, S>(
        &self,
        value: &T,
        state: &mut S,
        fold: F,
    ) -> Result<u64, RedexError>
    where
        T: serde::Serialize,
        F: Fn(&T, &mut S),
    {
        let bytes = bincode::serialize(value).map_err(RedexError::Encode)?;
        let seq = self.append(&bytes)?;
        fold(value, state);
        Ok(seq)
    }
}
```

NetDB / CortEX can build more on this, but it stays local.

## 5. Local secondary indices

Still purely local, but give callers better query tools over RedEX.

```rust
pub struct RedexIndex<K> {
    // K -> Vec<seq>
}

impl<K: Eq + Hash + Clone> RedexIndex<K> {
    pub fn apply_event(&mut self, key: &K, seq: u64);
    pub fn lookup(&self, key: &K) -> &[u64];
}
```

Examples:

- Index tasks by `kind` or `entity_id`.
- Index memory events by fact id.

Not a new DB engine; just local helpers that make "find all seqs for key X" cheap. The index is driven from a tail stream + a user-supplied key extractor, same shape as the v1 `RedexFold` trait.

## 6. NetDB enhancements

Still local:

- **More models**: MemEX facts/edges, continuity/causal chains, whatever CortEX is folding.
- **Watch APIs**: `db.tasks.watch({ status: 'pending' }, handler)` (TS) — built on top of tail streams, entirely local. No server-side filter pushdown; filter in the consumer.
- **Snapshot + tail**: "give me current state plus a subscription":
  1. `read_range(0, last_seq)` to build state,
  2. `tail(last_seq + 1)` for changes.

Still one node; Net is only used to bring events *into* that node.

## 7. Single-threaded `OrderedAppender` — a small determinism lever

Without building the full scheduler (parked in [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md)), offer a lib-level helper for "single-threaded appender replays":

```rust
pub struct OrderedAppender<'a> {
    redex: &'a RedexFile,
    seq: u64,
}

impl<'a> OrderedAppender<'a> {
    /// Create an ordered appender that takes over seq allocation for
    /// this file. Reads the file's last seq and owns the counter from
    /// there. Safe only if used from a single thread.
    pub fn new(redex: &'a RedexFile) -> io::Result<Self>;

    /// Append with deterministic, locally-ordered seq. No `AtomicU64`
    /// contention because there is only one writer.
    pub fn append_ordered(
        &mut self,
        payload: &[u8],
    ) -> Result<u64, RedexError>;
}
```

Gives deterministic seq for callers that can serialize their writes through one appender, without the full MPSC + sort-drain machinery. The full scheduler stays parked.

## 8. What stays out of v2

Explicitly not in scope:

- Replication / multi-node RedEX semantics.
- `SUBPROTOCOL_REDEX`.
- Replica convergence or fork handling.
- Cluster-wide archival coordination.
- The full deterministic scheduler (stays in [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md) as parked).

Those belong to a future, explicit "distributed RedEX" plan once there's:

- Real single-node usage.
- A clear DST story.
- Concrete requirements from pilots.

---

## v2 implementation steps (ordered)

1. **Tiering primitives.** `MmapSegment` with read-only random access; `HeapSegment::freeze()` to convert to mmap; segment registry per file.
2. **`FsyncPolicy`.** Plumb into `DiskSegment`; background task per node-instance (not per file) handles fsync timing.
3. **Time retention.** `Retention::Age(Duration)`; eviction sweep walks segments and drops whole segments whose max-seq timestamp is older than the cutoff.
4. **`ColdStore` trait + local disk impl.** `read_cold` wired through on `RedexFile`.
5. **`TypedRedexFile<T>` + `append_bincode` / `append_json` parity.**
6. **`append_and_fold`** on `RedexFile`.
7. **`RedexIndex<K>`** as a standalone helper; reference impl that drives from a tail.
8. **NetDB watch API + snapshot+tail convenience.**
9. **`OrderedAppender`** behind a feature gate or as an opt-in helper.
10. **Docs.** Update `REDEX.md` and add a v2 "Storage tiers" subsection.

## v2 tests

- **Tiering**: append across rollover boundary; assert reads return correct events from heap + mmap segments; mmap segment read after process restart.
- **FsyncPolicy**: `Never` vs `Interval` vs `EveryN` — background task behavior under each; crash-recover tests verify the expected durability boundary.
- **Time retention**: age-evicted segment disappears from `read_range`; still reachable via `read_cold` if archived.
- **Cold tier**: `archive_segment` + `fetch_segment` round-trip; `read_cold` assembles the correct event list from archived data.
- **Typed file**: `TypedRedexFile<Foo>::append_typed` + `tail_typed` round-trip for `Foo: Serialize + DeserializeOwned`.
- **`RedexIndex`**: drive from a tail stream with a key extractor; `lookup(k)` returns the expected seq list.
- **`OrderedAppender`**: 1M events on a single thread; determinism regression — two runs produce byte-identical segment files.

## Risks and open questions

- **mmap portability.** `memmap2` is fine on Linux/macOS; Windows needs care around file handles and read-only mapping. Scope question — does RedEX need Windows support in v2?
- **Timestamp source for `Retention::Age`.** If the caller packs a timestamp into payload, retention reads it from there; otherwise the file needs a parallel `Vec<u64>` of per-seq wall times. Payload-carried is cleaner; pick that.
- **Cold tier API async-ness.** `ColdStore::archive_segment` is sync in the sketch; S3 impls want async. Pick async for the trait and make the local-disk impl spawn-blocking; saves a v3 refactor.
- **Index staleness.** `RedexIndex` driven from a tail stream is eventually consistent with the log. Callers that need strict consistency (rare for v2 workloads) need to await a specific seq — or the index exposes a "current-tail-seq" watermark.
- **NetDB watch API shape.** TS-side ergonomics don't match Rust-side ergonomics 1:1; need a small RFC-grade pass when we cross-check against NetDB's existing surface.
- **`OrderedAppender` safety.** "Single-threaded" is enforced by `!Send`, not by runtime check. A caller that manually sends it across threads breaks determinism. Documented, not policed.

## Summary

v2 finishes the local storage story without touching replication:

- Hot → warm tiering (mmap) + configurable fsync.
- Time-based retention + cold-tier archive interface (local disk first; remote later, still per-node).
- Typed file wrappers + `append_and_fold` + local indices + NetDB watch / snapshot+tail.
- `OrderedAppender` for cheap deterministic single-writer replays.

All single-node. Replication, the full deterministic scheduler, and multi-node convergence remain future, explicit plans.
