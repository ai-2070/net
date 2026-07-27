# v2-closeout plan — FsyncPolicy, RedexIndex, NetDb watch passthrough

## Context

The v2 backpressure work has landed. Kyra's "perfect v2" scope (conversation today) names a short list of remaining items that would make v2 genuinely done for single-node deployment, deliberately **excluding** the bigger hot→warm mmap tiering + ColdStore work (deferred). This plan covers the three items in her ordering:

1. **`FsyncPolicy` + documented crash semantics** — smallest scope, biggest reliability lift. Today RedEX disk-backed files fsync **only on `close()`**; unsynced writes live in the OS page cache and are lost on process crash with no tunable.
2. **`RedexIndex<K, V>`** — unblock cleaner CortEX query patterns. Every CortEX query today is a full-table scan (tasks by status, memories by tag); a generic tail-driven index helper lets us fix the O(n) hotspots without reinventing the pattern per model.
3. **NetDb watch passthrough + SDK façade + unified doc** — finish the NetDb surface in Node/Python bindings + TS/Python SDKs (today they bind adapters directly, no `NetDb` class), add a `snapshot_and_watch(filter)` convenience, and write one user-facing "Storage & CortEX" narrative doc (the existing docs are implementation plans, not how-to).

Each stage is independently shippable as its own PR. Stage 1 touches the most users silently; Stage 2 has a concrete perf payoff on memories-by-tag queries; Stage 3 closes the SDK+docs mismatch Kyra flagged.

**Explicitly out of scope**: hot→warm mmap tiering, `ColdStore` trait, `read_cold(start, end)` archive-aware reads. These are the big architectural shift Kyra flagged as deferrable; we revisit them after Stage 3 lands.

## Stage 1 — `FsyncPolicy` + crash semantics

**Goal**: make disk durability tunable and document exactly what you lose under each policy.

### API shape

```rust
// net/crates/net/src/adapter/net/redex/config.rs
pub enum FsyncPolicy {
    /// Never fsync on append. `close()` still syncs. Writes live in
    /// the OS page cache; a process crash loses the unsynced tail,
    /// a kernel / power crash loses everything since the last close.
    /// Lowest latency; fine for observability / best-effort logs.
    Never,
    /// Fsync after every N successful appends. Bounds worst-case
    /// loss at (N-1) × entry-size bytes.
    EveryN(u64),
    /// Fsync on a timer, independent of append rate. Bounds worst-
    /// case loss at `interval` seconds of writes. Uses a per-file
    /// background tokio task; cancelled on `close()`.
    Interval(std::time::Duration),
}

impl Default for FsyncPolicy {
    // Matches current shipped behavior: no append fsync, close syncs.
    fn default() -> Self { FsyncPolicy::Never }
}
```

Add a `fsync_policy: FsyncPolicy` field to `RedexFileConfig`; thread it through `Redex::with_persistent_dir(...)` → `build_file()` → `RedexFile::open_persistent(...)`.

### Implementation notes

- **Never**: no code change on append path (matches today). Close still calls `sync_all()`.
- **EveryN(n)**: after each successful `DiskSegment::append_entry`, increment a per-file `appends_since_sync: AtomicU64`; when it reaches `n`, call `disk.sync()` and reset. Keep the dat-before-idx order invariant — the existing `DiskSegment::sync()` already does dat then idx.
- **Interval(d)**: spawn a tokio task at `open_persistent()` that loops `tokio::time::sleep(d)` + `file.sync()` until a shutdown notify fires. `close()` triggers the notify and awaits task exit. This is the only place that needs new concurrency; the manager-level retention sweep is synchronous and stays that way.
- **Torn-dat recovery is fsync-independent** (confirmed by exploration): dat-before-idx write order + the existing reopen-time truncation logic handles arbitrary partial writes. No recovery-path changes.

### Crash semantics — new doc section

Add a "Durability & crash semantics" subsection to `REDEX_PLAN.md` (and cross-link from the Stage 3 unified doc):

| Policy | Process crash | Kernel / power crash |
|--------|---------------|---------------------|
| `Never` | Loses the tail since last close / explicit `sync()` | Loses everything since last close / explicit `sync()` |
| `EveryN(N)` | Loses ≤ (N−1) entries | Loses ≤ (N−1) entries from the last sync point |
| `Interval(d)` | Loses ≤ `d` seconds of writes | Loses ≤ `d` seconds of writes |

The table is the contract; tests below enforce it.

### Critical files

- `net/crates/net/src/adapter/net/redex/config.rs` — new enum + config field (currently ~45 lines; this stays a single file).
- `net/crates/net/src/adapter/net/redex/file.rs:199-244` (append paths), `:704-730` (sync / close) — gate fsync calls on the policy, wire the EveryN counter, spawn the Interval task.
- `net/crates/net/src/adapter/net/redex/disk.rs:194-211` — the `append_entry` hot path; policy-aware sync call sites.
- `net/crates/net/src/adapter/net/redex/manager.rs:62-133` — thread the new config field through.

### Tests

- **Unit** (per-policy append + sync + close): three tests, each opens a persistent file with a specific policy, appends N entries, asserts the observed fsync cadence (via a test-only counter on a `DiskSegment` mock) matches the policy's contract.
- **Integration** — extend `integration_redex.rs:618 test_persistent_crash_recovery_drops_without_close`:
  - `Never` case: today's behavior, unchanged.
  - `EveryN(10)` case: 17 appends, drop without close, reopen; expect exactly 10 recovered (the ones before the last sync point).
  - `Interval(50ms)` case: append 20 entries fast, sleep 100 ms, drop without close, reopen; expect all 20 recovered.
- **Regression** — `test_regression_close_still_syncs_under_never_policy`: opens Never, appends, calls `close()`, reopens in a new file handle, expects all entries intact. Guards the invariant "Never ≠ no durability at all."

### Defaults

`FsyncPolicy::Never` as the default keeps today's observed behavior identical for existing callers (no silent perf regression from a switch to EveryN). New callers who care about durability opt into `EveryN` / `Interval`; this is documented in the REDEX_PLAN doc.

## Stage 2 — `RedexIndex<K, V>` + memories tag index

**Goal**: ship a generic tail-driven secondary index and wire it under the memories by-tag query, which is the hottest O(n·m) scan in CortEX today.

### API shape

```rust
// net/crates/net/src/adapter/net/redex/index.rs (new file)

/// One mutation the projection function can emit for each event.
#[derive(Debug, Clone)]
pub enum IndexOp<K, V> {
    Insert(K, V),
    Remove(K, V),
}

/// Eventually-consistent secondary index driven from a RedexFile
/// tail. Multiple mutations per event are supported; application
/// order mirrors the file's sequence order.
///
/// Reads are lock-free (DashMap). Writes run on a dedicated tail
/// task; the index lags the file by at most one fold tick.
pub struct RedexIndex<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Hash + Eq + Clone + Send + Sync + 'static,
{
    inner: Arc<DashMap<K, HashSet<V>>>,
    // tail task handle held for cancellation on Drop
}

impl<K, V> RedexIndex<K, V> {
    /// Spawn a tail task that decodes events as `T` and applies the
    /// projection's `IndexOp`s. `start` mirrors `RedexTailStart`.
    pub fn open<T, F>(
        file: &RedexFile,
        start: StartPosition,
        project: F,
    ) -> Self
    where
        T: DeserializeOwned + Send + 'static,
        F: Fn(&T) -> Vec<IndexOp<K, V>> + Send + Sync + 'static;

    pub fn get(&self, key: &K) -> Option<HashSet<V>>;
    pub fn contains(&self, key: &K, value: &V) -> bool;
    pub fn keys(&self) -> Vec<K>;  // snapshot of current keys
    pub fn len(&self) -> usize;
}
```

### Design choices (from exploration)

- **Separate struct, not a field on `State`**. Kyra's wording ("driven from a tail") matches this shape. Also avoids the `DashMap` serde headache on snapshot: indices rebuild naturally on restore because the fold replays the tail anyway. The index never lives on the snapshot path.
- **`Arc<DashMap<K, HashSet<V>>>`** for reads. Watchers `.get(key)` is lock-free; the tail task owns the mutation side. Index lag vs the main state is bounded by "one fold tick" (the tail task runs sequentially just like the CortEX fold).
- **Cancel on Drop** — the tail task listens on a `tokio::sync::Notify`; the `RedexIndex` Drop notifies it. Identical shape to the existing `CortexAdapter` shutdown path (reuse if possible).

### First real use case — `MemoriesState::by_tag`

Today (`memories/query.rs:72-85`):
```rust
if let Some(tag) = &self.require_tag {
    if !m.tags.iter().any(|t| t == tag) { return false; }  // O(n·m)
}
```

After Stage 2:
```rust
// adapter owns `tag_index: RedexIndex<String, MemoryId>`
let candidates = self.tag_index.get(tag)?;       // O(1)
candidates.iter().filter_map(|id| state.memories.get(id)).cloned()
```

Wire the index in `MemoriesAdapter::open` — build the `RedexIndex` from the same RedEX file the adapter already tails, with a projection that emits `Insert(tag, id)` on store/retag and `Remove(tag, id)` on retag/delete. Keep it behind a boolean flag on `MemoriesAdapter::with_tag_index(bool)` so workloads that don't need it pay no background-task cost.

Snapshot: the index isn't serialized. On `MemoriesAdapter::open_from_snapshot(...)`, the index is rebuilt by the tail replay from the snapshot's `last_seq`. No extra code.

### Critical files

- `net/crates/net/src/adapter/net/redex/index.rs` — new file, ~200 LoC including Drop cancellation + tests.
- `net/crates/net/src/adapter/net/redex/mod.rs` — `pub use index::{IndexOp, RedexIndex};`.
- `net/crates/net/src/adapter/net/cortex/memories/adapter.rs` — add optional `tag_index: Option<RedexIndex<String, MemoryId>>`, wire to query path.
- `net/crates/net/src/adapter/net/cortex/memories/query.rs:72-85` — swap the O(n·m) tag-filter loop for index-first lookup (with scan fallback when the index is disabled).

### Tests

- **Unit** (`redex/index.rs` tests mod) — three tests: append + get, Insert + Remove symmetry, multiple `IndexOp`s per event.
- **Regression** — `test_memories_tag_filter_uses_index_when_enabled`: store 10k memories with 100 distinct tags; run a tag-filter query; assert the index path is taken (via a debug counter) and that the result matches the scan path.
- **Performance smoke** (optional, behind `#[ignore]` by default): assert tag-filter at 10k memories is sub-100 µs with the index enabled, vs 19.6M elements/sec scan rate today.

## Stage 3 — NetDb watch passthrough + SDK façade + unified doc

**Goal**: finish the NetDb surface in the Node/Python bindings + TS/Python SDKs, add a `snapshot_and_watch(filter)` convenience pattern, and ship one user-facing "Storage & CortEX" doc.

### 3a. Napi + PyO3 `NetDb` classes

Today the bindings expose `TasksAdapter` / `MemoriesAdapter` directly; the Rust `NetDb` façade has no counterpart binding. Add:

- `bindings/node/src/netdb.rs` (new) — napi `NetDb` class mirroring the Rust builder (`NetDb.open({...})`). Getters `.tasks` / `.memories` return `Option<TasksAdapter>` / `Option<MemoriesAdapter>` napi classes. Also expose `snapshot()` / `openFromSnapshot(bundle, config)` so the bundle round-trip is one call from JS.
- `bindings/python/src/netdb.rs` (new) — PyO3 `NetDb` class with the same shape. `@property tasks` / `@property memories`. `@staticmethod open(origin_hash, with_tasks=True, with_memories=True, ...)`.

### 3b. TS + Python SDK wrappers

- `sdk-ts/src/netdb.ts` (new) — TS `NetDb` class wrapping the napi class. `open(config): Promise<NetDb>`; properties `.tasks` / `.memories` returning `TasksAdapter` / `MemoriesAdapter` TS wrappers (these already exist in the cortex binding paths).
- `sdk-py/src/net_sdk/netdb.py` (new) — Python `NetDb` class wrapping the PyO3 class. Re-export `NetDb` from `net_sdk/__init__.py`.

### 3c. `snapshot_and_watch(filter)` convenience

Added on `TasksAdapter` / `MemoriesAdapter` (Rust), then plumbed through bindings + SDKs:

```rust
// tasks/adapter.rs
/// One-shot combo: a snapshot of the current filter result PLUS a
/// watcher that emits every change to that filter from this point
/// forward. No dedup gap between the snapshot and the first
/// streamed update.
pub fn snapshot_and_watch(
    &self,
    filter: TasksFilter,
) -> (Vec<Task>, TasksStream) { /* ... */ }
```

Implementation: acquire the state read lock once, compute the filter result for the snapshot, hand off to `watch().where_*(...).stream()` for the live side, release the lock. The watcher's first emission is already the current filter result (deduplicated vs the snapshot), so the caller gets one initial `Vec<Task>` + a deduplicated stream of deltas.

Same shape in TS (`Promise<[Task[], AsyncIterable<Task[]>]>`) and Python (`Tuple[List[Task], Iterator[List[Task]]]`).

### 3d. Unified "Storage & CortEX" doc

New file: `net/crates/net/docs/STORAGE_AND_CORTEX.md`. Target audience: an engineer opening a daemon that reads + writes mesh state. **Not** a design plan.

Outline:

1. **Layering** — one diagram: RedEX (log) → CortEX adapter (fold) → NetDB (query façade). One paragraph per layer.
2. **Choosing durability** — the `FsyncPolicy` table from Stage 1, reproduced here. "You want Never for telemetry, EveryN(100) for most app state, Interval(1s) for anything that must survive kernel panic."
3. **Querying** — `find_unique` / `find_many` / `count_where` / `exists_where`. Tag/status filters. "When should I open a `tag_index`?" (the Stage 2 knob.) Perf numbers from the README.
4. **Watching** — `adapter.watch()` vs `adapter.snapshot_and_watch(filter)`. Single-slot semantics (consumer lag drops intermediate results). Cancellation via `close()`.
5. **Snapshot + restore** — `NetDb.snapshot()` round-trip, per-model independence, restart replay semantics, known limitation: postcard schema breaks on field-type changes (re-snapshot required on upgrade).
6. **What's next (v3 territory)** — hot→warm mmap, `ColdStore`, cross-node replication. One paragraph each so readers know the shape of future work without it blocking v2 use.

Cross-link: `REDEX_PLAN.md` / `CORTEX_ADAPTER_PLAN.md` / `NETDB_PLAN.md` get a top-of-file pointer → "For a user-level narrative see `STORAGE_AND_CORTEX.md`; this doc is the implementation plan."

### Critical files (Stage 3)

| File | Change |
|------|--------|
| `net/crates/net/src/adapter/net/cortex/tasks/adapter.rs` | +`snapshot_and_watch` |
| `net/crates/net/src/adapter/net/cortex/memories/adapter.rs` | +`snapshot_and_watch` |
| `net/crates/net/bindings/node/src/netdb.rs` | new — napi `NetDb` class + `snapshotAndWatch` bindings |
| `net/crates/net/bindings/python/src/netdb.rs` | new — PyO3 `NetDb` class + `snapshot_and_watch` |
| `net/crates/net/sdk-ts/src/netdb.ts` | new — TS `NetDb` wrapper |
| `net/crates/net/sdk-py/src/net_sdk/netdb.py` | new — Python `NetDb` wrapper |
| `net/crates/net/sdk-py/src/net_sdk/__init__.py` | re-export `NetDb` |
| `net/crates/net/docs/STORAGE_AND_CORTEX.md` | new — unified user doc |
| `net/crates/net/docs/REDEX_PLAN.md` | add durability-table subsection + pointer to STORAGE_AND_CORTEX |
| `net/crates/net/docs/NETDB_PLAN.md` | add pointer to STORAGE_AND_CORTEX |

### Tests (Stage 3)

- Napi: extend `bindings/node/test/netdb.test.ts` with `NetDb.open() → db.tasks.snapshotAndWatch(...)` round-trip.
- PyO3: extend `bindings/python/tests/test_netdb.py` with the Python-iterator variant.
- Rust: `test_snapshot_and_watch_no_gap` — stores a task, opens the combo, verifies the snapshot contains it AND the first streamed emission matches (no duplicate, no drop).
- Doc: `cargo doc` clean with `-D warnings` (already in CI).

## Order of shipping

Three PRs, in order:

1. **PR-1 (Stage 1 — FsyncPolicy)** — ~150 LoC core + ~150 LoC tests + doc table. Lowest risk, highest reliability lift.
2. **PR-2 (Stage 2 — RedexIndex + memories tag index)** — ~200 LoC index module + ~50 LoC memories adapter wiring + tests + perf smoke. Real query-perf payoff on the one O(n·m) hotspot.
3. **PR-3 (Stage 3 — NetDb SDK surface + unified doc)** — bigger-scoped (4 new binding/SDK files + doc) but entirely additive. No behavior change for existing callers.

PR-1 and PR-2 are independently mergeable. PR-3 depends on nothing but is largest; I'd review it in two passes (bindings + SDKs first, doc second).

## Verification

- `cargo build --features "net redex redex-disk cortex netdb"` — feature-matrix smoke.
- `cargo test --features "net redex redex-disk cortex netdb"` — lib + integration.
- `cargo clippy --all-features --all-targets -- -D warnings` — existing CI bar.
- `RUSTDOCFLAGS="-D warnings" cargo doc --features net --no-deps` — existing CI bar.
- Node: `npm ci && npm run build:debug -- --no-default-features --features net,cortex && npm test` in `bindings/node`.
- Python: `python -m venv .venv && pip install maturin pytest && maturin develop --no-default-features --features net,cortex && pytest -v` in `bindings/python`.

End-to-end: open a NetDb with both models, write some state, call `snapshot_and_watch` on tasks, close / reopen under each `FsyncPolicy` to confirm the durability table matches observation.

## Explicitly deferred (not in any of PR-1/2/3)

- **Hot → warm mmap tiering**: separate segment for frozen history, mmap-based reads. The current heap-segment+disk model is fine for the target workloads (confirmed by the perf README); mmap is a v3 optimization, not a correctness fix.
- **`ColdStore` trait + `read_cold(start, end)`**: archive-tier interface for cross-node history retrieval. Lives at the distribution layer, not single-node storage.
- **Whole-DB watch** (`db.watch()` that streams changes across both tasks + memories in one stream): composable from two `.watch()` calls today; the interleaving semantics are a separate design decision.
