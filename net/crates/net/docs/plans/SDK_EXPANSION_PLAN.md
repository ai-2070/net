# SDK expansion plan — CortEX, RedEX, Channels across Rust/TS/Python/Go

## Context

The core `net` crate has three major user-facing capabilities that are not yet surfaced through the SDKs:

1. **CortEX `TasksAdapter` / `MemoriesAdapter` / `NetDb`** — event-sourced typed state with reactive watches and the `snapshot_and_watch` primitive whose race fix just landed on `v2`.
2. **RedEX file API** — the persistent typed log underneath CortEX. Useful to users who want a domain-agnostic event log without going through CortEX.
3. **Channels subprotocol** — cross-mesh distributed pub/sub with membership, ack policies, and (eventually) per-channel auth. Distinct primitive from the existing local typed-channel-over-bus.

This plan covers adding all three to the **Rust**, **TypeScript**, **Python**, and **Go** SDKs. It is additive; no existing SDK surface is rewritten. Each stage is independently shippable as its own PR.

## Coverage today

| Feature | Rust SDK | TS SDK | Python SDK | Go SDK |
|---|---|---|---|---|
| Event bus (emit/poll/subscribe) | ✓ | ✓ | ✓ | ✓ |
| Typed channels over bus | ✓ | ✓ | — | — |
| Memory / Redis / JetStream transports | ✓ | ✓ | ✓ | ✓ memory only |
| Encrypted mesh transport | ✓ | ✓ | ✓ (compiled, unreachable) | ✓ config pass-through |
| Mesh streams + backpressure | ✓ | ✓ | ✓ (compiled, unreachable) | ✗ |
| CortEX Tasks/Memories/NetDb | ✗ (core only) | ✗ (NAPI has it; no TS wrapper) | ✓ (sync iter watches) | ✗ |
| RedEX file (`openFile` / `tail`) | ✗ (core only) | ✗ | partial (via CortEX `Redex`) | ✗ |
| Channels subprotocol | ✗ | ✗ | ✗ | ✗ |
| `snapshot_and_watch` primitive | ✓ core, ✗ SDK | ✗ (NAPI missing — regresses v2 fix) | ✗ (same) | ✗ |

**Key observation:** Python's native layer already has CortEX — it just isn't exposed in `__init__.py` for the Mesh half. The TypeScript NAPI already has ~80% of the CortEX work done in `bindings/node/src/cortex.rs`, but has no TS SDK wrapper. Rust has nothing in the SDK crate; users reach past it into the core. Go has nothing beyond the event bus.

## Binding mechanisms (recap)

| SDK | Native boundary | Streaming idiom | Errors |
|---|---|---|---|
| Rust SDK | Re-export / thin wrapper over `net` crate | `impl Stream<Item = ...>` | `error::Result<T, SdkError>` |
| TypeScript | NAPI-RS `#[napi]` classes in `bindings/node/` | Pull-based `next()` handles, wrapped as `AsyncIterable` in SDK | `Error::from_reason("prefix: ...")`, prefix-dispatched to typed classes |
| Python | PyO3 `#[pyclass]` in `bindings/python/` | Sync `Iterator[T]` (`__next__` blocks with `py.detach()`) | `pyo3::create_exception!` class hierarchy |
| Go | CGO shim against `target/release/libnet` | JSON-over-C ABI, poll-only (no streaming today) | `net_error_t` codes → typed sentinel errors |

Each has a different "easy path" and a different "hard path." Python's CortEX is easy because PyO3 is already there; Channels is hard because nothing is. Go's CortEX is hard because the C ABI has no streaming model; Channels is hard *twice*.

## Staged rollout

Shippable order, with rationale:

1. **Stage 1 — Rust SDK re-exports for CortEX + RedEX** (1–2 days). Cheapest win, validates feature-flag shape, unblocks the other stages.
2. **Stage 2 — NAPI `snapshotAndWatch` + TS CortEX SDK wrapper** (3–5 days). Highest user-facing leverage; NAPI is 80% done. One NAPI addition is **mandatory** to avoid regressing the v2 race fix.
3. **Stage 3 — Python CortEX export wiring + `snapshot_and_watch` + NetDb stubs** (2–3 days). The native layer is already compiled; the surface gap is pure Python packaging + one PyO3 method.
4. **Stage 4 — RedEX file in NAPI + TS SDK + Python** (3–5 days). Reuses the watch-iterator shape from Stage 2/3. Python gets `Redex.open_file()` + `RedexFile.tail()` as a real iterator.
5. **Stage 5 — Go CortEX via JSON-over-C ABI** (1 week). Architectural break-in — add opaque handles + cursor-based `WatchNext` pattern. Ships behind its own ABI version bump.
6. **Stage 6 — Channels in Rust + TS** (1 week). Grafts onto `Mesh`/`MeshNode`.
7. **Stage 7 — Channels in Python + Go** (1 week). Gated on Stage 6's surface being stable.

**Why this order, not the obvious "ship one feature end-to-end across all SDKs":** Stages 1–3 are mostly packaging/wiring on top of already-compiled code. Stages 4–7 are real new code. Front-loading the cheap wins pays for the tier-1 SDKs (Rust + TS) before we commit to the harder binding work for Python (new `snapshot_and_watch` PyO3 method) and Go (full new C ABI surface).

---

## Stage 1 — Rust SDK re-exports

Goal: a Rust user can `use net_sdk::cortex::*` and reach `TasksAdapter`, `MemoriesAdapter`, `NetDb`, `Redex` without depending on the core `net` crate directly.

### Feature flags

Add to `net/crates/net/sdk/Cargo.toml`:

```toml
[features]
cortex = ["net/netdb", "net/redex-disk"]
channels = ["net/net"]   # channels live on MeshNode
```

Keep `full` meaning "everything"; bundle `cortex` and `channels` under it.

### Surface

`net/crates/net/sdk/src/lib.rs`:

```rust
#[cfg(feature = "cortex")]
pub mod cortex;
```

`net/crates/net/sdk/src/cortex.rs` (new):

```rust
//! CortEX + RedEX + NetDb re-exports with one ergonomic builder.

pub use ::net::adapter::net::cortex::tasks::{
    OrderBy as TasksOrderBy, Task, TaskStatus, TasksAdapter, TasksWatcher,
};
pub use ::net::adapter::net::cortex::memories::{
    Memory, MemoriesAdapter, MemoriesWatcher, OrderBy as MemoriesOrderBy,
};
pub use ::net::adapter::net::cortex::{EventEnvelope, EventMeta};
pub use ::net::adapter::net::netdb::{NetDb, NetDbSnapshot, TasksFilter, MemoriesFilter};
pub use ::net::adapter::net::redex::{
    FsyncPolicy, Redex, RedexError, RedexEvent, RedexFile, RedexFileConfig,
};

/// Fluent builder for `NetDb` matching the `Net::builder()` idiom.
pub struct NetDbBuilder { /* ... */ }

impl NetDbBuilder {
    pub fn new(origin_hash: u32) -> Self { /* ... */ }
    pub fn persistent_dir(mut self, dir: impl Into<PathBuf>) -> Self { /* ... */ }
    pub fn with_tasks(mut self) -> Self { /* ... */ }
    pub fn with_memories(mut self) -> Self { /* ... */ }
    pub fn fsync_policy(mut self, p: FsyncPolicy) -> Self { /* ... */ }
    pub fn build(self) -> Result<NetDb, SdkError> { /* ... */ }
}
```

No other wrappers. The core types are already `Result`-shaped and builder-shaped; anything else would be paper over paper.

### Exit criteria

- `net-sdk` with `--features cortex` re-exports cleanly.
- One doctest showing `NetDbBuilder::new(...).with_tasks().build()` + a task create + a `snapshot_and_watch` call.
- README update: add a "CortEX / NetDb" section alongside "Mesh Streams."

---

## Stage 2 — TypeScript CortEX SDK

Goal: TS users get an ergonomic `NetDb` with `AsyncIterable` watches. One NAPI addition closes the race-fix regression window.

### NAPI addition — `snapshotAndWatch`

**Mandatory.** The NAPI currently exposes `snapshot()` and `watchTasks()` / `watchMemories()` separately. A TS user calling them sequentially hits exactly the race the `v2` fix prevents. Add to `bindings/node/src/cortex.rs`:

```rust
#[napi]
impl TasksAdapter {
    /// Atomic snapshot-plus-watch. Returns the current filter result
    /// and a watch iterator; the iterator drops leading emissions that
    /// equal the returned snapshot (the `skip_while` fix from v2).
    #[napi]
    pub fn snapshot_and_watch(
        &self,
        filter: Option<TasksFilterJs>,
    ) -> napi::Result<TasksSnapshotAndWatchJs> { /* ... */ }
}

#[napi(object)]
pub struct TasksSnapshotAndWatchJs {
    pub snapshot: Vec<TaskJs>,
    pub iter: TaskWatchIter, // existing NAPI class
}
```

Mirror for memories. Reuse `TaskWatchIter` — no new iterator class.

### TS SDK — `sdk-ts/src/cortex.ts` (new)

Top-level:

```typescript
export class NetDb {
  static async open(config: {
    originHash: number;
    persistentDir?: string;
    withTasks?: boolean;
    withMemories?: boolean;
    fsyncPolicy?: FsyncPolicy;
  }): Promise<NetDb>;

  get tasks(): TasksAdapter;
  get memories(): MemoriesAdapter;
  async snapshot(): Promise<Buffer>;          // postcard bundle
  static async restore(bytes: Buffer): Promise<NetDb>;
  async close(): Promise<void>;
}

export class TasksAdapter {
  create(id: bigint, title: string, nowNs: bigint): bigint;
  rename(id: bigint, newTitle: string, nowNs: bigint): bigint;
  complete(id: bigint, nowNs: bigint): bigint;
  delete(id: bigint): bigint;

  list(filter?: TasksFilter): Task[];
  async waitForSeq(seq: bigint): Promise<void>;

  watch(filter?: TasksFilter): AsyncIterable<Task[]>;
  async snapshotAndWatch(filter?: TasksFilter): Promise<{
    snapshot: Task[];
    updates: AsyncIterable<Task[]>;
  }>;
}

export interface Task {
  id: bigint;
  title: string;
  status: 'pending' | 'completed';
  createdNs: bigint;
  updatedNs: bigint;
}

export interface TasksFilter {
  whereStatus?: 'pending' | 'completed';
  titleContains?: string;
  createdAfter?: bigint;
  orderBy?: 'idAsc' | 'updatedDesc' | 'createdDesc';
  limit?: number;
}
```

### `AsyncIterable` wrapping — the core ergonomic win

Every NAPI watch iter (`TaskWatchIter`, `MemoryWatchIter`) already has pull-based `.next()` + `.close()`. The SDK wraps it:

```typescript
function wrapWatchIter<T>(raw: { next(): Promise<T | null>; close(): void }): AsyncIterable<T> {
  return {
    [Symbol.asyncIterator]() {
      return {
        async next() {
          const v = await raw.next();
          return v === null ? { value: undefined, done: true } : { value: v, done: false };
        },
        async return() { raw.close(); return { value: undefined, done: true }; },
      };
    },
  };
}
```

The `return()` hook is the lifecycle contract: `for await (const batch of iter) { if (condition) break; }` releases native resources deterministically.

### Errors

New typed classes in `sdk-ts/src/errors.ts`:

```typescript
export class CortexError extends Error { /* prefix "cortex:" */ }
export class NetDbError extends Error { /* prefix "netdb:" */ }
export class RedexError extends Error { /* prefix "redex:" */ }
```

Dispatch via string prefix on the NAPI error message. The existing prefix contract in `bindings/node/src/cortex.rs` (lines ~42–53) is the authoritative source — codify those prefixes as constants.

### Exit criteria

- `@ai2070/net-sdk` exports `NetDb`, `TasksAdapter`, `MemoriesAdapter` with `snapshotAndWatch`.
- Integration test: create task → `snapshotAndWatch` → mutate concurrently → assert stream delivers the mutation (the v2 regression test pattern ported to TS).
- README update matching the Rust SDK's "CortEX / NetDb" section.

---

## Stage 3 — Python CortEX finishing

The native layer already has `Redex`, `TasksAdapter`, `MemoriesAdapter`, `NetDb` with sync watch iterators. What's missing:

1. `snapshot_and_watch` as a PyO3 method (same race-fix rationale as Stage 2).
2. Mesh classes (`NetMesh`, `NetStream`, `NetStreamStats`) are compiled but not exposed — add them to `bindings/python/python/__init__.py`.
3. Type stubs (`.pyi`) for `snapshot_and_watch` and the Mesh types.

### PyO3 addition — `snapshot_and_watch`

```rust
#[pymethods]
impl TasksAdapter {
    /// Atomic snapshot + watch. Returns (snapshot_list, iterator).
    /// The iterator yields subsequent filter results, skipping the
    /// watcher's first emission when it equals the snapshot.
    fn snapshot_and_watch<'py>(
        &self,
        py: Python<'py>,
        filter: Option<TasksFilterPy>,
    ) -> PyResult<(Vec<TaskPy>, TaskWatchIter)> { /* ... */ }
}
```

Python usage:

```python
snap, it = db.tasks.snapshot_and_watch(filter={"where_status": "pending"})
print(f"initial: {len(snap)} tasks")
for batch in it:            # sync iterator, blocks per call
    print(f"update: {len(batch)} tasks")
```

### `async` variant — optional, defer

Python's native iterator is sync (`__next__` with `py.detach()` releasing the GIL). Adding an async variant (`__aiter__`/`__anext__` backed by `asyncio`) is useful but not blocking — defer to a follow-up unless user demand.

### Errors

Keep the existing `CortexError` / `NetDbError` PyO3 exception classes. Add `RedexError` and (Stage 7) `ChannelError`, `ChannelAuthError`. No string prefixes — Python's exception hierarchy is the dispatch.

### Exit criteria

- `python -c "from net import NetDb; db = NetDb.open(...); snap, it = db.tasks.snapshot_and_watch()"` works.
- `NetMesh`, `NetStream`, `NetStreamStats` exported from `net.__init__`.
- Stubs updated; `mypy` green.
- One integration test mirroring the Rust/TS `snapshot_and_watch` regression test.

---

## Stage 4 — RedEX file across TS + Python

`Redex::open_file` + `RedexFile::append` / `tail` exposed as a standalone surface for users who want a domain-agnostic persistent log.

### NAPI additions — `bindings/node/src/redex.rs` (new)

```rust
#[napi]
pub struct RedexFile { inner: Arc<net::adapter::net::redex::RedexFile> }

#[napi]
impl RedexFile {
    #[napi]
    pub fn append(&self, bytes: Buffer) -> napi::Result<BigInt> { /* returns seq */ }

    #[napi]
    pub fn append_batch(&self, bytes_list: Vec<Buffer>) -> napi::Result<BigInt> { /* first seq */ }

    #[napi]
    pub fn tail(&self, from_seq: Option<BigInt>) -> napi::Result<TailIter> { /* ... */ }

    #[napi]
    pub fn read_range(&self, from_seq: BigInt, limit: u32) -> napi::Result<Vec<RedexEventJs>> { /* ... */ }

    #[napi] pub fn len(&self) -> BigInt { /* ... */ }
    #[napi] pub fn sync(&self) -> napi::Result<()> { /* ... */ }
    #[napi] pub fn close(&self) -> napi::Result<()> { /* ... */ }
}

#[napi]
impl Redex {
    #[napi]
    pub fn open_file(
        &self,
        channel_name: String,
        config: Option<RedexFileConfigJs>,
    ) -> napi::Result<RedexFile> { /* ... */ }
}

#[napi(object)]
pub struct RedexFileConfigJs {
    pub persistent: Option<bool>,
    pub fsync_policy: Option<FsyncPolicyJs>,     // 'never' | { everyN } | { intervalMs }
    pub retention_max_events: Option<BigInt>,
    pub retention_max_bytes: Option<BigInt>,
    pub retention_max_age_ms: Option<u32>,
}
```

`TailIter` follows the `TaskWatchIter` lifecycle pattern: pull-based `.next()`, explicit `.close()`, shutdown on drop.

### TS SDK — `sdk-ts/src/redex.ts` (new)

```typescript
export class Redex {
  static open(opts?: { persistentDir?: string }): Redex;
  openFile(name: string, config?: RedexFileConfig): RedexFile;
}

export class RedexFile {
  append(bytes: Buffer): bigint;
  appendBatch(batch: Buffer[]): bigint;
  tail(fromSeq?: bigint): AsyncIterable<{ seq: bigint; payload: Buffer; tsNs: bigint }>;
  async readRange(fromSeq: bigint, limit: number): Promise<RedexEvent[]>;
  len(): bigint;
  sync(): Promise<void>;
  close(): void;
}
```

No typed variant in TS — postcard-over-serde is Rust-specific. Users who want JSON layer it themselves (`JSON.stringify` → `Buffer.from` on the way in).

### Python addition — `bindings/python/src/redex.rs` touch-up

`Redex` already exists; add `open_file` + `RedexFile` class matching the NAPI shape. Python's iterator is sync.

### Exit criteria

- TS: `redex.openFile("events", { fsyncPolicy: { intervalMs: 100 } }).tail()` works as async-for.
- Python: `for seq, payload, ts in redex.open_file("events").tail(): ...`.
- Round-trip test: write 1k events, close, reopen, tail from seq 0, read all 1k.

---

## Stage 5 — Go CortEX

Go is the biggest break-in. Current C ABI (`bindings/go/net/net.go` + generated `net.h`) is poll-only JSON-over-handles. Adding CortEX requires three new patterns:

1. **Opaque handle types** for `NetDb`, `TasksAdapter`, `MemoriesAdapter`, `RedexFile` — same shape as `net_handle_t`.
2. **Cursor-based watch** — C ABI can't return a Go channel; use a pull handle with `net_tasks_watch_next(handle, timeout_ms, out_json, out_len)` returning `0` on timeout, `>0` on value, `<0` on error.
3. **Go-side channel adapter** — in `net.go`, wrap the cursor in `WatchTasks(ctx context.Context, filter *TaskFilter) <-chan TasksSnapshot`. Closes the channel on `ctx.Done()`, calls `net_tasks_watch_close`.

### C ABI additions — `bindings/go/include/net.h`

```c
typedef struct net_netdb_s* net_netdb_t;
typedef struct net_tasks_adapter_s* net_tasks_adapter_t;
typedef struct net_tasks_watch_s* net_tasks_watch_t;

int net_netdb_open(const char* config_json, net_netdb_t* out);
void net_netdb_close(net_netdb_t db);

int net_netdb_tasks(net_netdb_t db, net_tasks_adapter_t* out);
int net_tasks_create(net_tasks_adapter_t ta, uint64_t id, const char* title, uint64_t now_ns, uint64_t* out_seq);
int net_tasks_list(net_tasks_adapter_t ta, const char* filter_json, char** out_json, size_t* out_len);

/* Snapshot-and-watch — atomic, not snapshot-then-watch. */
int net_tasks_snapshot_and_watch(
    net_tasks_adapter_t ta,
    const char* filter_json,
    char** out_snapshot_json, size_t* out_snapshot_len,
    net_tasks_watch_t* out_watch
);
int net_tasks_watch_next(net_tasks_watch_t w, uint32_t timeout_ms, char** out_json, size_t* out_len);
void net_tasks_watch_close(net_tasks_watch_t w);
```

Every `char**` is caller-frees via `net_free(char*)` — matches the existing ABI.

### Go surface — `bindings/go/net/cortex.go` (new)

```go
type NetDb struct { /* opaque */ }

func OpenNetDb(cfg NetDbConfig) (*NetDb, error)
func (db *NetDb) Tasks() *TasksAdapter
func (db *NetDb) Memories() *MemoriesAdapter
func (db *NetDb) Close() error

type TasksAdapter struct { /* opaque */ }

func (ta *TasksAdapter) Create(id uint64, title string, nowNs uint64) (uint64, error)
func (ta *TasksAdapter) Rename(id uint64, newTitle string, nowNs uint64) (uint64, error)
func (ta *TasksAdapter) Complete(id uint64, nowNs uint64) (uint64, error)
func (ta *TasksAdapter) Delete(id uint64) (uint64, error)
func (ta *TasksAdapter) List(filter *TasksFilter) ([]Task, error)
func (ta *TasksAdapter) SnapshotAndWatch(ctx context.Context, filter *TasksFilter) (snapshot []Task, updates <-chan []Task, err error)
```

`SnapshotAndWatch` is the Go-idiomatic shape: snapshot comes back as a value, updates come through a channel, `ctx.Done()` triggers C-level close. No separate `Watch` method — always go through the atomic primitive.

### Errors

Extend the existing `errorFromCode` table:

```go
const (
    ErrCortexClosed  = C.NET_ERR_CORTEX_CLOSED   // new
    ErrCortexFold    = C.NET_ERR_CORTEX_FOLD     // new
    ErrRedexCorrupt  = C.NET_ERR_REDEX_CORRUPT   // new
    ErrNetDbMissing  = C.NET_ERR_NETDB_MISSING   // new
)
```

Errors stay as sentinel values — no string-prefix dispatch (wrong idiom for Go).

### Feature gating

The C ABI bakes in whatever Rust features were compiled. For v1, ship one flavor: `cortex + redex-disk + net` all on. Split into variants later if binary size becomes a problem — deferred.

### Exit criteria

- Go test: `OpenNetDb` → `Tasks().Create` → `SnapshotAndWatch` → receive initial snapshot + one update via `ctx.Done()` cancellation.
- `example/cortex/main.go` showing a minimal task CRUD + watch loop.
- Update `bindings/go/README.md`.

---

## Stage 6 — Channels in Rust + TS

Channels (the subprotocol, not the local typed channel over the bus) is the distributed pub/sub primitive. The usable surface hangs off `MeshNode`, so the SDK work is extending `Mesh` / `MeshNode`.

### Rust SDK — `sdk/src/mesh.rs`

```rust
impl Mesh {
    pub async fn register_channel(&self, config: ChannelConfig) -> Result<()>;
    pub async fn subscribe_channel(
        &self,
        channel: &ChannelName,
        timeout: Option<Duration>,
    ) -> Result<()>;
    pub async fn unsubscribe_channel(&self, channel: &ChannelName) -> Result<()>;
    pub async fn publish(
        &self,
        channel: &ChannelName,
        payload: Bytes,
        config: PublishConfig,
    ) -> Result<PublishReport>;
    pub async fn publish_many(
        &self,
        channel: &ChannelName,
        payloads: Vec<Bytes>,
        config: PublishConfig,
    ) -> Result<Vec<PublishReport>>;

    /// Receive stream for a subscribed channel.
    pub fn on_channel(
        &self,
        channel: &ChannelName,
    ) -> impl Stream<Item = (NodeId, Bytes)> + Send + 'static;
}
```

Re-export the core types via `sdk/src/channels.rs` (new, gated on `channels` feature): `ChannelConfig`, `ChannelName`, `Visibility`, `PublishConfig`, `OnFailure`, `PublishReport`, `AckReason`, `MembershipMsg`. **Not** re-exported in v1: `AuthGuard`, `AuthVerdict`, `CapabilityFilter`, `TokenCache` — those require a full auth story and are deliberately out of scope for the first ship.

### TS SDK — extend `sdk-ts/src/mesh.ts`

The TS shape mirrors the core `ChannelConfig` (`net/crates/net/src/adapter/net/channel/config.rs`) field-for-field so the SDK wrapper stays a thin pass-through. `PublishConfig` likewise mirrors the core struct.

```typescript
export type Visibility = 'subnet-local' | 'parent-visible' | 'exported' | 'global';
export type Reliability = 'reliable' | 'fire-and-forget';
export type OnFailure = 'best-effort' | 'fail-fast' | 'collect';

/** Mirror of core `ChannelConfig`. */
export interface ChannelConfig {
  /** Canonical channel name (crosses the boundary as a string, not the u16 hash). */
  name: string;
  visibility: Visibility;
  /** `CapabilityFilter` deferred to the security-surface plan; leave
   *  `undefined` to allow any node. */
  publishCaps?: CapabilityFilter;
  subscribeCaps?: CapabilityFilter;
  /** V1 ships `false`; flipping to `true` requires the identity/token
   *  surface in SDK_SECURITY_SURFACE_PLAN.md. */
  requireToken?: boolean;
  /** 0 = lowest. Default 0. */
  priority?: number;
  /** Default reliability mode for this channel's streams. */
  reliable?: boolean;
  /** Optional rate cap in packets per second. */
  ratePps?: number;
}

/** Mirror of core `PublishConfig`. */
export interface PublishConfig {
  reliability?: Reliability;        // default 'fire-and-forget'
  onFailure?: OnFailure;            // default 'best-effort'
  maxInflight?: number;             // default 32
}

export class MeshNode {
  // ... existing members ...

  async registerChannel(config: ChannelConfig): Promise<void>;
  async subscribeChannel(name: string, opts?: { timeoutMs?: number }): Promise<void>;
  async unsubscribeChannel(name: string): Promise<void>;

  async publish(
    name: string,
    payload: Buffer,
    config?: PublishConfig,
  ): Promise<PublishReport>;

  onChannel(name: string): AsyncIterable<{ fromNodeId: bigint; payload: Buffer }>;
}

export interface PublishReport {
  subscribersTotal: number;
  succeeded: number;
  failures: Array<{ nodeId: bigint; error: string }>;
}
```

`ChannelConfig.publishCaps` / `subscribeCaps` / `requireToken` exist on the TS surface from day one but are no-ops in v1 — the capability and token surfaces land with the security plan. Shipping the full shape now avoids a breaking change when auth turns on. New TS error class `ChannelAuthError` (maps `AckReason::Unauthorized`). Prefix: `"channel:unauthorized:"`.

### NAPI — `bindings/node/src/mesh.rs` additions

Mirror the Rust SDK surface. `onChannel` returns a `ChannelIter` with the `TaskWatchIter` lifecycle shape. `publish` returns a JSON-able `PublishReport` object.

### Canonical-name vs u16 hash

Critical: the bindings must cross the boundary as `ChannelName` strings (canonical form), **not** the `u16` on-wire hash. `redex/manager.rs:86-97` codifies this — auth keys on canonical name to avoid ACL bypass via 16-bit collision. Keep this invariant in the binding code; document in the SDK README.

### Exit criteria

- Two-node integration test: node A registers + publishes, node B subscribes + receives via `onChannel`.
- `PublishReport` semantics tested for `onFailure: 'fail_fast'` — one subscriber errors → error surfaces.
- Subscribe timeout honored.
- Both SDKs' READMEs get a "Channels" section.

---

## Stage 7 — Channels in Python + Go

Python: PyO3 methods on the existing `NetMesh` class (once it's exposed in `__init__.py` from Stage 3). `on_channel` returns a sync iterator; async variant deferred.

Go: extend the C ABI with `net_mesh_register_channel`, `net_mesh_publish`, `net_mesh_channel_watch_next` — same cursor pattern as tasks watch. `OnChannel(ctx)` returns `<-chan ChannelMessage`.

No new design here; it's a mechanical repeat of Stages 5 + 6 once the earlier work has settled.

---

## Critical files

### Stage 1 (Rust SDK re-exports)
- `net/crates/net/sdk/Cargo.toml` — add `cortex`, `channels` features.
- `net/crates/net/sdk/src/lib.rs` — module declaration + feature gates.
- `net/crates/net/sdk/src/cortex.rs` — new; re-exports + `NetDbBuilder`.

### Stage 2 (TS CortEX)
- `net/crates/net/bindings/node/src/cortex.rs` — add `snapshot_and_watch` NAPI method.
- `net/crates/net/sdk-ts/src/cortex.ts` — new; wrapper classes.
- `net/crates/net/sdk-ts/src/errors.ts` — add `CortexError`, `NetDbError`, `RedexError`.
- `net/crates/net/sdk-ts/src/index.ts` — add exports.

### Stage 3 (Python CortEX)
- `net/crates/net/bindings/python/src/cortex.rs` — add `snapshot_and_watch` PyO3 method.
- `net/crates/net/bindings/python/python/__init__.py` — export `NetMesh`, `NetStream`, `NetStreamStats`.
- `net/crates/net/bindings/python/python/*.pyi` — stubs for new methods.

### Stage 4 (RedEX TS/Python)
- `net/crates/net/bindings/node/src/redex.rs` — new; `RedexFile` NAPI class.
- `net/crates/net/sdk-ts/src/redex.ts` — new; TS wrapper.
- `net/crates/net/bindings/python/src/redex.rs` — add `open_file` + `RedexFile`.

### Stage 5 (Go CortEX)
- `net/crates/net/bindings/go/include/net.h` (or wherever the header lives) — new opaque types + watch ABI.
- Relevant Rust side of the C ABI — likely `net/crates/net/bindings/go/src/cortex.rs` (new).
- `net/crates/net/bindings/go/net/cortex.go` — new; `NetDb`, `TasksAdapter`, `SnapshotAndWatch`.

### Stage 6 (Channels Rust + TS)
- `net/crates/net/sdk/src/mesh.rs` — extend `Mesh` with channel methods.
- `net/crates/net/sdk/src/channels.rs` — new; re-exports.
- `net/crates/net/bindings/node/src/mesh.rs` — NAPI channel methods.
- `net/crates/net/sdk-ts/src/mesh.ts` — TS channel methods.
- `net/crates/net/sdk-ts/src/errors.ts` — add `ChannelError`, `ChannelAuthError`.

### Stage 7 (Channels Python + Go)
- `net/crates/net/bindings/python/src/mesh.rs` — PyO3 channel methods.
- `net/crates/net/bindings/go/src/mesh.rs` + Go C ABI — channel surface.
- `net/crates/net/bindings/go/net/channels.go` — new.

---

## Open questions / risks

### API stability

- **`NetDbBundle` snapshot format.** Currently bincode-of-`InnerNetDbSnapshot` with no version byte (see `bindings/node/src/cortex.rs:932`). Shipping `NetDb.snapshot()` as a stable API means a user can persist a bundle on SDK v0.1 and expect it to restore on v0.2. Decision: add a magic byte + version u8 to the bundle header before Stage 2 ships. Small diff, large long-term payoff.

- **Error prefix contract.** TS dispatches typed errors via string prefix on the NAPI `Error::from_reason` message. That's fragile — the "contract" is spread across `format!` calls in cortex.rs. Codify the prefixes as constants in one place (`bindings/node/src/errors.rs`, new) and document them in `STORAGE_AND_CORTEX.md`.

### Feature-flag interactions

- **NAPI bundles `cortex = ["net/netdb", "net/redex-disk"]`.** Prebuilt binaries ship one bundle. Splitting into `cortex` + `redex` separately means prebuild matrix doubles. Decision: keep bundled. A user who wants "just RedEX, no CortEX" calls only the RedEX methods — the CortEX surface is zero-cost if unused.

- **Go's C ABI has no feature flags.** All compiled in. If binary size becomes a problem, add a `-tags minimal` variant later; deferred.

### Scope cuts

- **Channel auth (v1) is off.** `AuthGuard`, `CapabilityFilter`, `TokenCache` are not bound in any SDK in this plan. Documented as explicit v1 limitation. When we add ACLs, the binding must cross with canonical `ChannelName` strings — never the u16 hash (see `redex/manager.rs:86-97`).

- **No async Python watch.** Sync `__iter__` only. Blocking per-`__next__`. Async variant deferred until user demand.

- **TS has no typed RedEX.** `TypedRedexFile<T>` is a postcard-over-serde Rust convenience. TS users layer JSON/MessagePack themselves.

### Implementation risks

- **`snapshot_and_watch` must land in every SDK.** Sequential `snapshot()` + `watch()` regresses the v2 race fix. This is mandatory scope for Stages 2, 3, 5, 7 — not an optimization.

- **Watch backpressure.** `TaskWatchIter` uses unbounded internal channels. A slow JS/Python/Go consumer can accumulate `Vec<Task>` snapshots. Dedup on `Vec<Task>` equality mitigates steady-state; bursty high-fanout filter results are the real risk. Document in each SDK's README; add a bounded-channel variant in a follow-up if a user complains.

- **Subscribe timeout.** `subscribeChannel` is async over the network — the `Subscribe` → `Ack` round-trip through `SUBPROTOCOL_CHANNEL_MEMBERSHIP` has no SDK-visible timeout today. Must add a timeout parameter to the SDK method and plumb it into the subprotocol handler; otherwise a partitioned subscriber hangs forever.

- **`MeshNode` lifecycle vs channels.** Channel methods require an active `MeshNode`. If the node is shut down mid-subscribe, `onChannel`'s stream must end cleanly (not panic). Reuse the Mesh-stream shutdown pattern already in place.

---

## Sizing

| Stage | SDKs touched | Est. effort |
|---|---|---|
| 1. Rust SDK re-exports | Rust | 1–2 days |
| 2. TS CortEX | NAPI + TS | 3–5 days |
| 3. Python CortEX finishing | PyO3 + Python | 2–3 days |
| 4. RedEX file (TS + Python) | NAPI + TS + PyO3 | 3–5 days |
| 5. Go CortEX | C ABI + Go | ~1 week |
| 6. Channels Rust + TS | SDK + NAPI + TS | ~1 week |
| 7. Channels Python + Go | PyO3 + C ABI + Go | ~1 week |

Total: ~5–6 weeks of engineering time, each stage a separate PR.

## Out of scope (for this plan)

- Hot→warm mmap tiering, `ColdStore`, archive reads. Separate track per `V2_CLOSEOUT_PLAN.md`.
- NetDB wire protocol — still core-side.
- Daemons / MigrationOrchestrator SDK surface — larger than channels, separate plan.
- Subnets / capabilities / identity surface — separate plan when auth story firms up.
