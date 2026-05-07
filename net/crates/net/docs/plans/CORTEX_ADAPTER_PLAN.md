# CortEX adapter — RedEX fold driver + NetDB surface

> **Looking for how to use this?** [`STORAGE_AND_CORTEX.md`](STORAGE_AND_CORTEX.md) is the user-facing narrative. This doc is the implementation plan.

## Status

Design only. Lands after CortEX itself has a concrete v1 scope (not in this plan) and RedEX v1 has seen pilot usage. Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md) and [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md).

**Scope:** this plan covers the *adapter boundary* — the glue between CortEX (the query / fold plane) and RedEX (the storage primitive). It does **not** specify:

- CortEX's internal state model (task shape, fact schema, continuity model). Those belong in CortEX's own plan.
- NetDB's wire protocol or TS/Python client surface. Those belong in CortEX's plan when it lands.

If the adapter is *right*, CortEX stays small and RedEX stays small and the fold contract is the only thing keeping them aligned.

## One-line frame

The CortEX adapter:

1. Takes a Net `EventEnvelope` from the caller.
2. Projects it into a 20-byte `EventMeta` prefix.
3. Appends `(EventMeta || app_payload)` to a RedEX file.
4. Tails the same file, drives a caller-supplied `RedexFold<State>` as events arrive, maintains materialized state.
5. Exposes the materialized state as the read-side NetDB handle.

One file per "thing CortEX folds" (tasks, facts, continuity chains). One adapter instance per file.

## Where the adapter ends

**The adapter owns:**

- The `RedexFile` handle (opened through `Redex::open_file`).
- The fold task (`tokio::spawn` that consumes the tail stream).
- The materialized state behind a reader-friendly handle.

**The adapter does NOT own:**

- CortEX's event model. `EventEnvelope` is a type from CortEX; the adapter projects it, doesn't define it.
- Routing or delivery. Events arrive from whatever produced them (Net daemons, local ingestion, a replay job). The adapter only knows "here's an envelope, append it."
- Multi-file query semantics. Each adapter instance is scoped to one RedEX file. Cross-file queries are a CortEX-layer concern.

## 1. `EventMeta` — the 20-byte fixed prefix

Every event stored through the adapter carries a 20-byte `EventMeta` header at the start of its RedEX payload. Fold implementations parse this header to route events by dispatch / origin without deserializing the type-specific tail.

```rust
/// Fixed 20-byte prefix on every CortEX-adapted RedEX payload.
///
/// Layout (little-endian, 20 bytes total):
///
/// | Offset | Field                 | Size | Notes                                   |
/// |--------|-----------------------|------|-----------------------------------------|
/// | 0      | dispatch              | u8   | Event classifier (task.created, ...)    |
/// | 1      | flags                 | u8   | Causal / continuity / proof bits        |
/// | 2      | _pad                  | 2    | Reserved, must be zero                  |
/// | 4      | origin_hash           | u32  | Producer identity (xxh3 of origin)      |
/// | 8      | seq_or_ts             | u64  | Per-origin monotonic seq OR unix nanos  |
/// | 16     | checksum              | u32  | xxh3-trunc of the type-specific tail    |
#[repr(C, packed)]
pub struct EventMeta {
    pub dispatch: u8,
    pub flags: u8,
    pub _pad: [u8; 2],
    pub origin_hash: u32,
    pub seq_or_ts: u64,
    pub checksum: u32,
}
```

### Why 20 bytes, specifically

- Matches `REDEX_ENTRY_SIZE` — the "index record space" RedEX already talks about. A future adapter optimization can hoist `EventMeta` out of the payload and into an inline `RedexEntry` variant (the 8-byte INLINE slot plus the 12-byte entry tail) without changing the fold contract.
- 20 bytes fits the fields the fold runner needs on every event to decide "do I care about this?" without touching the type-specific tail.

### Dispatch space

- `0x00..0x7F` — CortEX-internal dispatches (task lifecycle, fact assertions, continuity events). Allocated in CortEX's plan.
- `0x80..0xFF` — application / vendor dispatches.
- `0x00` is reserved for "raw", used by adapters that don't need dispatch routing.

### `seq_or_ts`

The adapter does **not** interpret this field. Whoever builds the envelope decides: per-origin monotonic counter for deterministic fold order; unix nanos for wall-clock ordering. For CortEX's initial pilots, per-origin counter is the expected choice (matches the deterministic scheduler design parked in [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md)).

### Relationship to `RedexEntry.seq`

- `RedexEntry.seq` is **file-local storage identity** — allocated by the RedEX appender, monotonic per-file, never interpreted by CortEX.
- `EventMeta.seq_or_ts` is **application identity** — set by CortEX, carries origin-level ordering.

The two are orthogonal and serve different layers. A fold can use either or both (e.g. "skip events with `seq_or_ts < checkpoint`" uses app identity; "I've folded everything up through RedEX seq 1234" uses storage identity).

## 2. Adapter API

```rust
/// One-shot configuration for an adapter instance.
pub struct CortexAdapterConfig {
    /// Where to start folding.
    pub start: StartPosition,
    /// What to do on fold error.
    pub on_fold_error: FoldErrorPolicy,
}

pub enum StartPosition {
    /// Replay from the beginning of the file. Default.
    FromBeginning,
    /// Start live-only; skip backfill. Use when the state is
    /// rehydrated from an external snapshot.
    LiveOnly,
    /// Start at a caller-supplied checkpoint (the last RedEX seq that
    /// was known to be folded into the caller's persisted state).
    FromSeq(u64),
}

pub enum FoldErrorPolicy {
    /// First error stops the adapter; subsequent state reads still
    /// return the pre-error state. Default; correctness-first.
    Stop,
    /// Log + skip. The offending event is not folded; the adapter
    /// continues. Visible via `CortexAdapter::fold_errors()`.
    LogAndContinue,
}

/// The adapter: one RedEX file + one fold + one materialized state.
pub struct CortexAdapter<State> { /* ... */ }

impl<State: Send + Sync + 'static> CortexAdapter<State> {
    /// Open an adapter against a RedEX file. Spawns a background
    /// task that tails the file and drives `fold`.
    pub fn open<F>(
        redex: &Redex,
        name: &ChannelName,
        redex_config: RedexFileConfig,
        adapter_config: CortexAdapterConfig,
        fold: F,
        initial_state: State,
    ) -> Result<Self, CortexAdapterError>
    where
        F: RedexFold<State> + Send + 'static;

    /// Append an envelope. Projects to `EventMeta`, serializes,
    /// and calls `RedexFile::append`. Returns the assigned RedEX seq.
    pub fn ingest(&self, envelope: EventEnvelope) -> Result<u64, CortexAdapterError>;

    /// Read-only access to the materialized state. Returned `Arc` is
    /// cheap to clone; readers and writers share the same `RwLock`.
    pub fn state(&self) -> Arc<RwLock<State>>;

    /// Cumulative fold error count (always 0 under `FoldErrorPolicy::Stop`
    /// once any error has occurred, since the adapter stopped).
    pub fn fold_errors(&self) -> u64;

    /// Highest RedEX seq that has been folded into state. Use as a
    /// read-after-write synchronization point when caller ingests
    /// and then wants to query state reflecting that ingest.
    pub async fn wait_for_seq(&self, seq: u64);

    /// Close the adapter. Stops the fold task, closes the RedexFile
    /// handle. State handle remains readable after close.
    pub fn close(&self) -> Result<(), CortexAdapterError>;
}
```

### `EventEnvelope`

`EventEnvelope` is a CortEX-owned type. The adapter requires one trait implementation from it:

```rust
pub trait IntoRedexPayload {
    /// Project into `(EventMeta, tail_bytes)`. The adapter
    /// concatenates the two and appends the result to RedEX.
    fn into_redex_payload(self) -> (EventMeta, Bytes);
}
```

CortEX's plan specifies how `EventEnvelope` implements this. The adapter is agnostic to what's inside the tail.

### `wait_for_seq` — the read-after-write hook

A common callsite shape is "ingest, then query state that should reflect the ingest." Because fold runs asynchronously, the state might not yet include the ingested event. `wait_for_seq(seq)` returns once the fold has caught up to `seq`. Use:

```rust
let seq = adapter.ingest(envelope)?;
adapter.wait_for_seq(seq).await;
let state = adapter.state().read();
// state now reflects the ingest.
```

Internal implementation: an `AtomicU64` high-watermark updated after every successful `fold.apply`, plus a `Notify` that `wait_for_seq` races against.

## 3. NetDB surface (v1)

v1's NetDB is the thinnest thing that works: **`state()` is the query handle.** Callers hold the `Arc<RwLock<State>>` and read whatever CortEX's state type exposes. No wire protocol, no query language, no watch API.

```rust
// CortEX-defined state type (in CortEX's plan):
pub struct CortexTasksState { /* task map, indices, ... */ }

// CortEX-defined query methods on that type:
impl CortexTasksState {
    pub fn get(&self, id: TaskId) -> Option<&Task> { ... }
    pub fn pending(&self) -> impl Iterator<Item = &Task> { ... }
}

// Adapter surface:
let adapter: CortexAdapter<CortexTasksState> = CortexAdapter::open(...)?;
let state = adapter.state().read();
let pending = state.pending().collect::<Vec<_>>();
```

What v1 NetDB is **not** in this plan:

- No `db.watch({ status: 'pending' }, handler)` — that's [REDEX_V2_PLAN.md §6](REDEX_V2_PLAN.md).
- No TS/Python client wrapper — that's the SDK layer, landing after the Rust surface is real.
- No snapshot+tail helper — callers compose it from `read_range` + `tail` today; v2 will offer a convenience.

Keeping the v1 NetDB surface at "just an `Arc<RwLock<State>>`" means we don't prematurely canonicalize a query API before we know what CortEX actually needs.

## 4. Lifecycle and the backfill → live handoff

Opening the adapter:

1. `CortexAdapter::open` calls `Redex::open_file` (creates if absent).
2. Starts a tail via `RedexFile::tail(start_seq)` where `start_seq` comes from `CortexAdapterConfig::start`.
3. The tail stream is consumed by a spawned task — it backfills from the file into the state under the same atomic boundary RedEX v1 provides (no gaps, no dupes across backfill → live).
4. For each `RedexEvent`, the fold task:
   a. Decodes `EventMeta` from the first 20 bytes.
   b. Constructs a typed view for the fold implementation.
   c. Calls `fold.apply(&event, &mut state.write())` under the state lock.
   d. Updates the high-watermark (`folded_through_seq`) and notifies `wait_for_seq` waiters.
5. On close: task receives a shutdown signal, drains the in-flight event, unwraps the state lock.

### Closing behavior

- `close()` is idempotent.
- On close, the fold task finishes any in-progress `apply` then exits.
- The RedexFile is closed via the manager's `close_file`.
- The `Arc<RwLock<State>>` survives close — callers can still read it, just won't see new events.

## 5. Fold error handling

Under `FoldErrorPolicy::Stop`:

- First `RedexFold::apply` that returns `Err(_)` is logged with the event's `seq`.
- The fold task exits.
- Subsequent `state()` calls return the state as of the last successful apply.
- Subsequent `ingest` calls still succeed — they go to RedEX just fine; they're simply not folded. This matters: the log is the source of truth; a broken fold is a bug in the fold, not in the data.
- A later process instance with a fixed fold can replay from the beginning and succeed.

Under `FoldErrorPolicy::LogAndContinue`:

- Same log + skip, but the task continues with the next event.
- `fold_errors()` returns the cumulative skip count.
- Useful for development; production CortEX should default to `Stop` so bugs don't silently corrupt derived state.

## 6. Checkpoint support (v1.1, out of scope for v1)

For long-lived files, replaying from seq 0 on every process restart is expensive. v1.1 will add:

- `fold.checkpoint(&state) -> Bytes` — caller serializes state.
- External snapshot storage (CortEX picks the backend).
- `StartPosition::FromCheckpoint(checkpoint_bytes, last_seq)` — decodes the snapshot into `State`, resumes folding at `last_seq + 1`.

v1 deliberately ships without this — the first CortEX pilot will tell us the right checkpoint cadence + format. Pre-committing now would lock in the wrong shape.

## 7. Worked example — tasks adapter

To ground the API, here's a hypothetical task-tracking CortEX adapter. This is sketch-only; the actual task dispatch + state types are CortEX's call.

```rust
// CortEX side:
pub struct TaskState {
    tasks: HashMap<TaskId, Task>,
}

pub struct TasksFold;

impl RedexFold<TaskState> for TasksFold {
    fn apply(&mut self, ev: &RedexEvent, state: &mut TaskState) -> Result<(), RedexError> {
        let meta = EventMeta::from_bytes(&ev.payload[..20])
            .ok_or_else(|| RedexError::Encode("bad EventMeta".into()))?;
        let tail = &ev.payload[20..];

        match meta.dispatch {
            DISPATCH_TASK_CREATED => {
                let task: Task = bincode::deserialize(tail)
                    .map_err(|e| RedexError::Encode(e.to_string()))?;
                state.tasks.insert(task.id, task);
            }
            DISPATCH_TASK_COMPLETED => {
                let id: TaskId = bincode::deserialize(tail)
                    .map_err(|e| RedexError::Encode(e.to_string()))?;
                if let Some(t) = state.tasks.get_mut(&id) {
                    t.completed = true;
                }
            }
            _ => { /* unknown dispatch — ignore */ }
        }
        Ok(())
    }
}

// Application side:
let redex = Redex::new();
let adapter = CortexAdapter::open(
    &redex,
    &ChannelName::new("cortex/tasks").unwrap(),
    RedexFileConfig::default(),
    CortexAdapterConfig {
        start: StartPosition::FromBeginning,
        on_fold_error: FoldErrorPolicy::Stop,
    },
    TasksFold,
    TaskState { tasks: HashMap::new() },
)?;

// Publish:
let seq = adapter.ingest(task_created_envelope(task))?;
adapter.wait_for_seq(seq).await;

// Query:
let state = adapter.state().read();
let pending = state.tasks.values().filter(|t| !t.completed).collect::<Vec<_>>();
```

The adapter is 100% of what sits between CortEX and RedEX. CortEX owns `TaskState`, `TasksFold`, the dispatch constants. RedEX owns storage. The adapter owns the wire-up.

## 8. Non-goals (v1 adapter)

- **Multi-file queries.** One adapter = one file. Joining across files is a CortEX-layer job.
- **Remote CortEX.** v1 is local-only, same as RedEX v1. Distributed CortEX rides distributed RedEX — neither is planned yet.
- **Automatic checkpoint / resume.** See §6; deferred to v1.1.
- **Wire-format NetDB.** The query surface is a Rust `Arc<RwLock<State>>`. TS/Python surfaces are SDK work, not adapter work.
- **Schema evolution.** `EventMeta` carries a `dispatch` byte and implicit versioning through unknown-dispatch-ignored fold impls. Real versioning is CortEX's call.
- **Total event ordering across origins.** RedEX v1 uses a CAS seq allocator, so concurrent appenders race; ordering within `EventMeta.seq_or_ts` per origin is preserved, cross-origin is not. Deterministic cross-origin ordering needs [`REDEX_SCHEDULER_PLAN.md`](REDEX_SCHEDULER_PLAN.md) (parked v2+).

## 9. Implementation steps

1. **`EventMeta` codec** — `to_bytes` / `from_bytes` with explicit LE encoding. Unit tests: roundtrip, field boundaries, unknown-dispatch tolerance.
2. **`IntoRedexPayload` trait** — define in the adapter module. Reference impl for a toy `EventEnvelope` type used only by tests.
3. **`CortexAdapterConfig` + `StartPosition` + `FoldErrorPolicy`** — public types.
4. **`CortexAdapter::open`** — opens RedexFile, spawns the fold task, returns the handle. Initial state: `StartPosition::FromBeginning` and `FoldErrorPolicy::Stop`.
5. **Fold task** — consumes the tail stream, decodes `EventMeta`, drives the fold, updates `folded_through_seq` + `Notify`.
6. **`CortexAdapter::ingest`** — projects envelope, builds payload, calls `redex_file.append`. Returns the RedEX seq.
7. **`CortexAdapter::wait_for_seq`** — `tokio::select!` on the `Notify` and an atomic watermark check.
8. **`CortexAdapter::close`** — shuts down the fold task, closes the RedexFile.
9. **`FoldErrorPolicy::LogAndContinue`** — branch in the fold task; counter wired to `fold_errors()`.
10. **`StartPosition::LiveOnly`** and **`FromSeq(u64)`** — passed through to `RedexFile::tail(from_seq)`.
11. **Feature gate** — `cortex = ["redex"]` on the `net` crate. Gate the module in `lib.rs`.
12. **Docs** — `CORTEX_ADAPTER.md` caller-facing, link from `TRANSPORT.md` Stack table once CortEX itself has any shipped surface.

## 10. Tests

- **Unit: `EventMeta` codec** — roundtrip every field; unknown `dispatch` parses fine; bad padding bits tolerated but noted.
- **Unit: fold stop-on-error** — inject a fold that fails on seq 5; assert adapter stops, state reflects seqs 0..5, `fold_errors()` is 1.
- **Unit: fold continue-on-error** — same fold; `LogAndContinue` policy; assert state skips seq 5 and folds seqs 6..N.
- **Integration: ingest + wait_for_seq + query** — ingest N envelopes sequentially; for each, `wait_for_seq` returns before state reflects it; verify state matches expected post-fold value.
- **Integration: replay from `FromBeginning`** — ingest N events to a RedEX file, close adapter; reopen with a fresh state; assert state reconstructs to the same value.
- **Integration: `FromSeq` checkpoint** — ingest N, checkpoint at seq K, reopen with `FromSeq(K + 1)` and a pre-filled state; assert only K+1..N are folded.
- **Integration: `LiveOnly`** — open with a pre-populated file; assert backfill is skipped and only post-open appends are folded.
- **Integration: tasks example** — the §7 sketch as a full test. Ingest 100 task events across create/complete/reassign, assert final state matches expected.
- **Integration: close flushes the fold** — ingest, close without waiting; reopen; assert the closed adapter's state reflects all pre-close ingests (via fold state being re-derived) and that close itself didn't drop events.
- **Durability (with `redex-disk`)** — open persistent RedEX file; ingest; close; reopen adapter; assert state replay from disk reconstructs correctly.

## 11. Risks and open questions

- **`EventMeta` layout lock-in.** Once CortEX pilots store 20-byte prefixes, changing the layout breaks recovery. Mitigation: reserve `_pad` bytes (we do), and pick field sizes conservatively now. A `dispatch` byte is more future-proof than a `dispatch` enum.
- **Fold latency under slow readers.** One state lock, many readers. If readers hold the lock long, fold stalls and the RedEX tail buffer grows. v1 accepts this; v2's watch API (REDEX_V2_PLAN §6) decouples readers from the fold.
- **`seq_or_ts` ambiguity.** The adapter doesn't enforce whether this is a counter or a timestamp. Mixing within one file breaks fold ordering assumptions. Mitigation: CortEX's plan should freeze the convention per file type, and this plan documents "per-file, pick one."
- **Replay cost at scale.** Files with millions of entries take real time to replay. v1 accepts this; CortEX pilots at the 100k–1M events/file scale are the first real test. Checkpoint support (v1.1) lands when a pilot hits the pain point.
- **`FoldErrorPolicy::Stop` vs "poison the state".** Under `Stop`, state stays readable after error. That's correct semantically (the log is the source of truth) but surprising if a reader doesn't know the fold stopped. Mitigation: `fold_errors()` plus a status field (`is_running() -> bool`) on the handle.
- **Back-pressure on `ingest`.** `RedexFile::append` inherits the heap segment's 3 GB hard cap. A pathological producer could fill the file faster than the fold consumes, but since the heap holds the full payload regardless of fold progress, the real back-pressure is "RedEX returned `PayloadTooLarge`." That's acceptable v1 behavior; v2's credit-window backpressure on streams ([`STREAM_BACKPRESSURE_PLAN_V2.md`](STREAM_BACKPRESSURE_PLAN_V2.md)) is the layer that would extend the signal here.
- **Single-writer assumption per file.** The adapter does not coordinate multiple CortEX adapters writing to the same RedEX file. RedEX supports concurrent writers, but `EventMeta.seq_or_ts` semantics usually imply single-writer-per-origin. Multiple adapters on the same file must either use distinct `origin_hash`es or coordinate externally. Document as a known constraint.

## Summary

The CortEX adapter is small on purpose:

- **In:** Net `EventEnvelope`.
- **Out:** materialized `State` behind an `Arc<RwLock<_>>`.
- **In between:** 20-byte `EventMeta` prefix, `RedexFile` for durability, `RedexFold<State>` trait driving the state forward.
- **Lifecycle:** one file per adapter, backfill from seq 0 by default, fold on a spawned task, `wait_for_seq` for read-after-write.

NetDB v1 is "read the state." Watch APIs, snapshot+tail, wire clients are all deferred — `REDEX_V2_PLAN.md` §6 covers the ergonomic layer once v1 CortEX has shipped and we know the shape.

The plan's contract: if CortEX provides an `EventEnvelope` + `RedexFold<State>` + `State`, the adapter turns that into a live, durable, replayable, query-able slice of the mesh. Everything above is CortEX; everything below is RedEX; this doc is the seam.
