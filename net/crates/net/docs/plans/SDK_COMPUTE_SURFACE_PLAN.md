# SDK compute surface plan — daemons + MigrationOrchestrator

## Context

The `net` crate ships a compute-on-mesh layer that no SDK exposes today:

- **`MeshDaemon`** trait — a stateful processor that consumes causal events and emits outputs, with `snapshot` / `restore` hooks. Zero async (WASM-friendly); microsecond `process()` contract. The primary payload.
- **`DaemonHost`** — per-daemon runtime wrapping the daemon plus its causal chain builder, observed horizon, and stats.
- **`DaemonFactoryRegistry` + `DaemonRegistry`** — explicit factory registration for restore-by-origin_hash; no auto-discovery.
- **`Scheduler`** — placement-only (given a `CapabilityFilter`, pick a node). Not a persistent orchestrator.
- **`MigrationOrchestrator`** — six-phase state machine (`Snapshot → Transfer → Restore → Replay → Cutover → Complete`) over `SUBPROTOCOL_MIGRATION` (0x0500). Stateless source/target handlers, stateful orchestrator.

Design reference: [`COMPUTE.md`](COMPUTE.md) is the user-facing narrative. In-tree examples: `EchoDaemon` / `CounterDaemon` in `compute/host.rs` tests; end-to-end migration in `tests/three_node_integration.rs::test_migration_full_lifecycle_over_wire`.

This plan is **additive** on [`SDK_EXPANSION_PLAN.md`](SDK_EXPANSION_PLAN.md) and [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md). It depends on identity (Stage A of the security plan) because daemons have an `EntityKeypair` identity — there is no sensible "anonymous daemon."

## Scope

**In scope:**
- A way for SDK users to *implement* `MeshDaemon` in Rust, TS, Python, Go.
- Spawn / stop / snapshot / restore lifecycle on a local node.
- Placement via `Scheduler::place(filter)`.
- Migration — `start_migration`, `start_migration_auto`, observable phase progression, failure surface.
- Observability — `DaemonStats`, migration state queries.

**Out of scope:**
- `ForkGroup` / `ReplicaGroup` / `StandbyGroup` group semantics. They exist in the core but have no stable public API yet; deferred.
- Automatic migration triggers (load-based, heartbeat-timeout-based). The core leaves this to the application; the SDK does not add one.
- Scheduler as a persistent service. `Scheduler::place()` is a pure function today; keep it that way in the SDK.
- CortEX-backed daemons as a distinct type. Daemons are storage-agnostic; using `TasksAdapter` from inside a daemon is an application choice, not a protocol coupling.
- Permission-token gating on daemon execution. Capability matching is the v1 control mechanism per [`COMPUTE.md`](COMPUTE.md).

## Coverage today

| Feature | Rust SDK | TS SDK | Python SDK | Go SDK |
|---|---|---|---|---|
| Implement `MeshDaemon` | ✗ | ✗ | ✗ | ✗ |
| Register factory + spawn host | ✗ | ✗ | ✗ | ✗ |
| Deliver events to a local host | ✗ | ✗ | ✗ | ✗ |
| Snapshot / restore | ✗ | ✗ | ✗ | ✗ |
| Placement via `Scheduler` | ✗ | ✗ | ✗ | ✗ |
| Start a migration | ✗ | ✗ | ✗ | ✗ |
| Observe migration phase | ✗ | ✗ | ✗ | ✗ |
| `DaemonStats` | ✗ | ✗ | ✗ | ✗ |

Zero coverage across the board.

## Design principles

1. **Subclass-to-write, handle-to-use.** Users implement `MeshDaemon` in their own code; the SDK gives them a handle to everything else (registry, host, orchestrator). No base classes with magic — a trait/interface with four methods.
2. **Sync `process`, async lifecycle.** `process` is sync everywhere — that is the contract. Spawn / snapshot / migrate are async in the SDK because they hit the mesh. Don't invert this per-language.
3. **Migration is observable, not invisible.** Users get a `MigrationHandle` / watch stream with phase transitions. No silent "it worked" / "it didn't" — every outcome produces an event.
4. **Factories are data, not closures across FFI.** Cross-language factory registration uses a dispatcher pattern: the SDK side owns the `fn() -> Box<dyn MeshDaemon>` closure, the NAPI/PyO3/CGO side registers a *name* that the SDK dispatches on. Closures don't cross FFI cleanly.
5. **Failure is a first-class return.** `DaemonError` and `MigrationError` each map to typed error classes / sentinels in every SDK. `MigrationFailed` is a terminal state users can react to.

## Staged rollout

Shippable order:

1. **Stage 1 — Rust SDK daemon surface** (3–5 days). `MeshDaemon` re-export + `DaemonRuntime` handle bundling host, registry, factory. No migration yet.
2. **Stage 2 — Rust SDK migration** (2–3 days). `MigrationOrchestrator` re-export + `MigrationHandle` with a phase `Stream`.
3. **Stage 3 — TS daemon surface via JS-Rust dispatcher** (1 week). The hardest binding challenge; daemon code must run *from JS* on an event delivered *by Rust*. Dispatcher pattern with a `ThreadsafeFunction` per factory.
4. **Stage 4 — TS migration** (2–3 days). NAPI + TS wrapper for the orchestrator.
5. **Stage 5 — Python daemon + migration** (1 week). GIL-aware dispatcher; PyO3 `Py<PyAny>` hands into `MeshDaemon` impl via `call_method1`.
6. **Stage 6 — Go daemon + migration** (2+ weeks). C ABI additions; this is the biggest lift because Go code must run on Rust-delivered events through a callback table.

**Why this order:** Rust (1–2) is cheapest and validates the handle shape. TS (3–4) is the highest user-facing leverage and the hardest cross-FFI challenge; solving it here sets the pattern. Python (5) and Go (6) then follow the same dispatcher pattern. Migration always lags the daemon surface by one stage — no point migrating daemons you can't spawn.

---

## Stage 1 — Rust SDK daemon surface

### Feature flags

Extend `net/crates/net/sdk/Cargo.toml`:

```toml
[features]
compute = ["net/net", "identity"]   # compute requires identity
```

Daemons need `identity` (from [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage A); fail fast if the user enables `compute` without `identity`.

### Surface — new `sdk/src/compute.rs`

```rust
//! Compute surface — MeshDaemon + DaemonRuntime.
//!
//! Users implement `MeshDaemon` and hand it to a `DaemonRuntime` tied
//! to a Mesh node. Runtime handles factory registration, host
//! construction, snapshot-on-demand, and statistics.

pub use ::net::adapter::net::compute::{
    // Daemon trait + data types
    MeshDaemon, DaemonError, DaemonStats, CausalEvent, CausalLink,
    StateSnapshot,
    // Scheduler surface (placement)
    PlacementDecision, Scheduler, SchedulerError,
    // Configuration
    DaemonHostConfig,
};

/// Per-mesh compute runtime. Holds the factory registry and the
/// per-daemon `DaemonHost` map. Construct once per `Mesh`.
pub struct DaemonRuntime {
    inner: Arc<DaemonRuntimeInner>,   // factory registry + host registry
}

impl DaemonRuntime {
    /// Attach a runtime to an existing mesh. Takes an identity handle
    /// so every spawned daemon has a well-defined owner keypair
    /// (not the node's own identity — each daemon has its own).
    pub fn new(mesh: Mesh) -> Self;

    /// Register a factory that can construct daemons of a given kind.
    /// `kind` is a user-chosen string; migrations use it to find the
    /// factory on the target node. Returns an error if already
    /// registered.
    pub fn register_factory<F>(&self, kind: &str, factory: F)
        -> Result<(), DaemonError>
    where
        F: Fn() -> Box<dyn MeshDaemon> + Send + Sync + 'static;

    /// Spawn a daemon locally. `identity` owns the keypair; `kind`
    /// must match a registered factory. Returns a handle to the
    /// running host.
    pub async fn spawn(
        &self,
        kind: &str,
        identity: Identity,
        config: DaemonHostConfig,
    ) -> Result<DaemonHandle, DaemonError>;

    /// Spawn from a prior snapshot (reconstructs state before first
    /// event delivery).
    pub async fn spawn_from_snapshot(
        &self,
        kind: &str,
        identity: Identity,
        snapshot: StateSnapshot,
        config: DaemonHostConfig,
    ) -> Result<DaemonHandle, DaemonError>;

    /// Stop a daemon. Drops the host; subsequent events for this
    /// origin are rejected by the subprotocol handler.
    pub async fn stop(&self, origin_hash: u32) -> Result<(), DaemonError>;

    /// Scheduler placement decision for a filter. Thin passthrough.
    pub fn place(&self, filter: &CapabilityFilter)
        -> Result<PlacementDecision, SchedulerError>;

    /// Snapshot a running daemon by origin_hash.
    pub async fn snapshot(&self, origin_hash: u32)
        -> Result<Option<StateSnapshot>, DaemonError>;
}

/// Handle to a running daemon. Clone-safe; drop does not stop the
/// daemon (call `DaemonRuntime::stop` explicitly).
#[derive(Clone)]
pub struct DaemonHandle {
    pub origin_hash: u32,
    pub entity_id: EntityId,
    // internal Arc to the host
}

impl DaemonHandle {
    pub fn stats(&self) -> DaemonStats;
    pub async fn snapshot(&self) -> Result<Option<StateSnapshot>, DaemonError>;
}
```

### Exit criteria

- Doctest: implement an `EchoDaemon` (port the core test's version), register + spawn + deliver an event through the mesh, assert output.
- `DaemonHandle::stats()` reflects events processed.
- README: new "Daemons" section sitting between "Channels" and "CortEX."

---

## Stage 2 — Rust SDK migration

### Surface — extend `sdk/src/compute.rs`

```rust
pub use ::net::adapter::net::compute::{
    MigrationError, MigrationPhase, MigrationState, MigrationMessage,
    SUBPROTOCOL_MIGRATION,
};

impl DaemonRuntime {
    /// Migrate a daemon from `source_node` to `target_node`.
    /// The orchestrator runs on the node calling this method.
    pub async fn start_migration(
        &self,
        origin_hash: u32,
        source_node: NodeId,
        target_node: NodeId,
    ) -> Result<MigrationHandle, MigrationError>;

    /// Like `start_migration`, but picks a target via the scheduler
    /// (finds nodes matching the daemon's requirements AND advertising
    /// `subprotocol:0x0500`).
    pub async fn start_migration_auto(
        &self,
        origin_hash: u32,
        source_node: NodeId,
    ) -> Result<MigrationHandle, MigrationError>;
}

/// Observable migration. Drop the handle and the orchestrator
/// continues; use `cancel()` to abort in-flight.
pub struct MigrationHandle {
    pub origin_hash: u32,
    pub source: NodeId,
    pub target: NodeId,
}

impl MigrationHandle {
    /// Stream of phase transitions. Ends on Complete or Failed.
    pub fn phases(&self) -> impl Stream<Item = MigrationPhase>;

    /// Block until the migration reaches a terminal state.
    pub async fn wait(self) -> Result<(), MigrationError>;

    /// Request abort. Orchestrator emits `MigrationFailed`; both
    /// sides roll back. Best-effort — a migration past Cutover
    /// cannot be undone.
    pub async fn cancel(&self) -> Result<(), MigrationError>;
}
```

### Exit criteria

- Two-node integration test (ported from `three_node_integration.rs`): spawn a counter daemon on A → `start_migration(hash, A, B)` → phase stream emits all 6 phases → counter continues on B.
- Force a failure in Transfer (inject error in source handler) → phase stream ends with a Failed variant; both sides' state is clean.

---

## Stage 3 — TS daemon surface

The binding challenge: daemon code lives in JS. The mesh delivers events in Rust. We need to call JS code on a native thread safely.

### The dispatcher pattern

Factories don't cross FFI. Instead:

1. On the Rust side, `register_factory(kind, napi_callback)` stores `kind → ThreadsafeFunction<InitArgs, DaemonBridge>` in a table.
2. When the Rust runtime needs to construct a daemon, it looks up the `kind`, invokes the threadsafe function with `InitArgs`, and receives back a `DaemonBridge`.
3. `DaemonBridge` is a Rust struct holding three more `ThreadsafeFunction`s — one each for `process`, `snapshot`, `restore`. It implements `MeshDaemon` by dispatching to those functions.

The JS side never touches Rust state directly; Rust never holds a JS closure that outlives the threadsafe function.

### NAPI — `bindings/node/src/compute.rs` (new)

```rust
#[napi]
pub struct DaemonRuntime { inner: Arc<net_sdk::compute::DaemonRuntime> }

#[napi]
impl DaemonRuntime {
    #[napi(factory)]
    pub fn new(mesh: &NetMesh) -> Self;

    /// `factory` is a JS function `(initArgs) => { process, snapshot,
    /// restore }`. We hold a ThreadsafeFunction keyed by `kind`;
    /// `spawn` invokes it to produce a DaemonBridge.
    #[napi(ts_args_type = "kind: string, factory: (args: any) => DaemonImpl")]
    pub fn register_factory(
        &self,
        kind: String,
        factory: ThreadsafeFunction<JsUnknown, DaemonBridge>,
    ) -> napi::Result<()>;

    #[napi]
    pub async fn spawn(
        &self,
        kind: String,
        identity: &Identity,
        config: DaemonHostConfigJs,
    ) -> napi::Result<DaemonHandle>;

    // ... stop, snapshot, place (unchanged shape from Rust) ...
}
```

### TS SDK — `sdk-ts/src/compute.ts` (new)

```typescript
export interface DaemonImpl {
  /** Sync — do not await. Must return within microseconds. */
  process(event: CausalEvent): Buffer[];

  /** Optional. Return null for stateless daemons. */
  snapshot?(): Buffer | null;

  /** Optional. Mirror snapshot. */
  restore?(state: Buffer): void;
}

export interface CausalEvent {
  origin: number;            // origin_hash
  seq: bigint;
  payload: Buffer;
  // ... minimum fields consumers need; full struct mirrors CausalEvent ...
}

export class DaemonRuntime {
  constructor(mesh: MeshNode);

  registerFactory(kind: string, factory: (initArgs: unknown) => DaemonImpl): void;

  async spawn(kind: string, identity: Identity, config?: DaemonConfig): Promise<DaemonHandle>;
  async spawnFromSnapshot(kind: string, identity: Identity, snapshot: Buffer, config?: DaemonConfig): Promise<DaemonHandle>;
  async stop(originHash: number): Promise<void>;

  place(filter: CapabilityFilter): PlacementDecision;
  async snapshot(originHash: number): Promise<Buffer | null>;
}

export class DaemonHandle {
  readonly originHash: number;
  readonly entityId: Buffer;
  stats(): DaemonStats;
  async snapshot(): Promise<Buffer | null>;
}
```

### Cost of the dispatcher

Every `process` call does: NAPI callback → tokio thread → ThreadsafeFunction → JS event loop → user code → return to JS event loop → Buffer copy to Rust. This is **not** microseconds — it is tens to hundreds of microseconds depending on event loop backpressure. The SDK README must say explicitly:

> **Daemons implemented in JS run on the JS event loop. The underlying Rust `MeshDaemon::process` contract (microsecond latency) does NOT carry over to JS. If you need hot-loop processing, write the daemon in Rust and expose only control-plane methods to JS.**

### Exit criteria

- JS-implemented EchoDaemon: spawn, deliver, assert output within 10ms.
- Snapshot round-trip: spawn CounterDaemon, increment 100 times, snapshot, stop, spawn-from-snapshot, verify counter persisted.
- Memory test: spawn/stop 1000 daemons in a loop, heap usage stable.

---

## Stage 4 — TS migration

NAPI exposes `start_migration` / `start_migration_auto` returning a `MigrationHandle` with:
- `phase`: a property that reflects the latest phase (string enum: `'snapshot' | 'transfer' | 'restore' | 'replay' | 'cutover' | 'complete' | 'failed'`).
- `phases()`: `AsyncIterable<MigrationPhase>` following the same wrap-and-return pattern as CortEX watches.
- `wait(): Promise<void>` that resolves on Complete, rejects on Failed with a `MigrationError` carrying `.reason`.
- `cancel(): Promise<void>`.

### Exit criteria

- TS integration test mirroring Stage 2 Rust test: spawn JS daemon on node A, migrate to node B, watch phases, assert completion.
- Failure mid-migration → `wait()` rejects with typed `MigrationError { kind: 'failed', reason: string }`.

---

## Stage 5 — Python daemon + migration

### GIL-aware dispatcher

PyO3's challenge is the GIL. Daemon `process` is called from Rust; it needs to acquire the GIL to call the Python implementation:

```rust
struct PyDaemonBridge { obj: Py<PyAny> }

impl MeshDaemon for PyDaemonBridge {
    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Python::with_gil(|py| {
            let py_event = PyCausalEvent::from(event);
            let result = self.obj.call_method1(py, "process", (py_event,))
                .map_err(|e| DaemonError::ProcessFailed(e.to_string()))?;
            // unwrap list of bytes into Vec<Bytes>
        })
    }

    fn snapshot(&self) -> Option<Bytes> { /* same pattern */ }
    fn restore(&mut self, state: Bytes) { /* same pattern */ }
}
```

### Python SDK — `bindings/python/src/compute.rs` (new) + `python/net/compute.py`

```python
from net import MeshDaemon, DaemonRuntime, Identity

class EchoDaemon(MeshDaemon):
    def process(self, event) -> list[bytes]:
        return [event.payload]

    # snapshot / restore optional — leave unimplemented for stateless

rt = DaemonRuntime(mesh)
rt.register_factory("echo", lambda args: EchoDaemon())
handle = rt.spawn("echo", identity)
```

Same "JS event loop" caveat applies here: Python `process` holds the GIL; latency is not microsecond-bounded. Document.

### Exit criteria

- Python EchoDaemon spawn + deliver + assert.
- Migration lifecycle test: spawn on A, migrate to B, phase iterator yields all 6, counter persists.

---

## Stage 6 — Go daemon + migration

### C ABI dispatcher via callback table

Go can't receive Rust callbacks directly through CGO without indirection. Pattern:

1. Go registers a C-ABI-compatible function pointer per daemon kind: `net_daemon_factory_register("echo", cgoFactoryFn)`.
2. `cgoFactoryFn` returns an opaque `net_daemon_impl_t*` — a struct holding function pointers for `process`, `snapshot`, `restore`.
3. When Rust needs to spawn a daemon of `kind="echo"`, it calls the factory pointer, gets the daemon_impl pointer, and stores it.
4. `MeshDaemon::process` calls through the `process` function pointer; the C function re-enters Go via `//export` to call user code.

This is the standard CGO-callback pattern used by other embeddings (e.g. libp2p-go's native code bridges).

### Go surface

```go
package net

type MeshDaemon interface {
    Process(event CausalEvent) ([][]byte, error)
    Snapshot() ([]byte, error)
    Restore(state []byte) error
}

type DaemonRuntime struct{ /* opaque */ }

func NewDaemonRuntime(mesh *Mesh) *DaemonRuntime
func (rt *DaemonRuntime) RegisterFactory(kind string, factory func() MeshDaemon) error
func (rt *DaemonRuntime) Spawn(ctx context.Context, kind string, identity *Identity) (*DaemonHandle, error)
func (rt *DaemonRuntime) StartMigration(ctx context.Context, originHash uint32, source, target uint64) (*MigrationHandle, error)

type MigrationHandle struct{ /* opaque */ }
func (h *MigrationHandle) Phases() <-chan MigrationPhase
func (h *MigrationHandle) Wait() error
```

Same "not microsecond-latency" caveat.

### Exit criteria

- Go EchoDaemon + CounterDaemon tests.
- Migration lifecycle test over cgo.

---

## Critical files

### Stage 1–2 (Rust)
- `net/crates/net/sdk/Cargo.toml` — add `compute` feature.
- `net/crates/net/sdk/src/lib.rs` — module wiring.
- `net/crates/net/sdk/src/compute.rs` (new) — `DaemonRuntime`, `DaemonHandle`, `MigrationHandle`.

### Stage 3–4 (TS)
- `net/crates/net/bindings/node/src/compute.rs` (new) — NAPI `DaemonRuntime`, `DaemonBridge`, `MigrationHandle`; ThreadsafeFunction dispatcher.
- `net/crates/net/sdk-ts/src/compute.ts` (new) — `DaemonImpl` interface, `DaemonRuntime`, `DaemonHandle`, `MigrationHandle` classes.
- `net/crates/net/sdk-ts/src/errors.ts` — add `DaemonError`, `MigrationError`.

### Stage 5 (Python)
- `net/crates/net/bindings/python/src/compute.rs` (new) — PyO3 `DaemonRuntime`, `PyDaemonBridge` with `Python::with_gil` dispatcher.
- `net/crates/net/bindings/python/python/net/compute.py` — `MeshDaemon` ABC + wrappers.

### Stage 6 (Go)
- `net/crates/net/bindings/go/src/compute.rs` (new) — C ABI surface + callback table.
- `net/crates/net/bindings/go/include/net.h` — additions: opaque types, factory registration, callback-table struct.
- `net/crates/net/bindings/go/net/compute.go` (new) — `MeshDaemon` interface, `DaemonRuntime`, exported CGO callbacks.

---

## Open questions / risks

### API stability

- **`MeshDaemon` trait signature.** `process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>>` is the current contract. Any change breaks every SDK simultaneously. Freeze before Stage 3 ships; any future additions go behind a new trait method with a default (`fn on_migration_cutover(&mut self) { /* default no-op */ }`).
- **Phase enum.** Six phases today. SDKs must accept an unknown-phase variant (forward-compat) — freezing six string literals means a seventh phase in the core silently breaks SDK consumers.
- **`DaemonStats` shape.** Currently includes event count, last-processed seq, snapshot size, observed horizon depth. Add a version byte to any serialized form; expose as a read-only struct.
- **`StateSnapshot` layout.** Wire format today is Postcard + serde. Like `NetDbBundle` / `PermissionToken`, add a version byte before bindings ship — a user's snapshot persisted on SDK v0.1 must round-trip on v0.2.

### Performance

- **Event delivery latency in non-Rust SDKs.** JS / Python / Go daemons will *not* hit the microsecond core contract. Document prominently in every SDK README. Offer Rust as the recommended path for hot-loop compute; the non-Rust SDKs are for control-plane daemons (dashboards, operators, cluster services).
- **GIL contention in Python.** Multiple daemons on the same process share one GIL. If a user runs 100 Python daemons on one node, they serialize. Not fixable in v1; document. (Subinterpreters in PEP 684 are a future mitigation.)
- **ThreadsafeFunction queue depth in TS.** NAPI-RS `ThreadsafeFunction` has an internal queue; if the JS event loop is blocked, events back up. Expose queue-depth metric in `DaemonStats`; SDK user can shed load on backpressure.
- **CGO call cost.** Each Go daemon `Process` is ~200ns of C↔Go overhead minimum. Batch where possible. If users need submicrosecond processing in Go, write the daemon in Rust and call it from Go via a different FFI.

### Failure handling

- **Daemon panics in non-Rust code.** A JS `process` that throws, a Python one that raises, a Go one that panics — each must be caught at the FFI boundary and translated to a `DaemonError::ProcessFailed(msg)` rather than crashing the host. SDK tests must cover each case.
- **Migration abort partway.** The orchestrator emits `MigrationFailed`; source/target roll back. Users watching the phase stream see `Failed { reason }`. No automatic retry — the SDK offers `retry` as a user-level method (not a framework behavior).
- **Stuck migrations.** If the target never replies after `RestoreComplete`, the migration hangs indefinitely. Add a timeout parameter on `start_migration` / `start_migration_auto` that defaults to 60s and expires the orchestrator-side state. Plumb into the core via a new `MigrationOrchestrator::start_migration_with_timeout` if needed.

### Factory registration semantics

- **Kind strings are global per runtime.** A user cannot register `"echo"` twice with different factories. SDK error: `DaemonError::FactoryAlreadyRegistered`. Consistent across all SDKs.
- **Restore after process restart.** On restart the runtime has no factory table until the user re-registers. If migration is in-flight against a restart, the target errors `FactoryNotFound`. [`DAEMON_RUNTIME_READINESS_PLAN.md`](DAEMON_RUNTIME_READINESS_PLAN.md) closes this gap: it adds a `Registering → Ready` lifecycle on `DaemonRuntime` so the mesh can't accept migrations before factories are registered, structured reason codes on `MigrationFailed` so the source can distinguish "target is booting" from "target doesn't host this kind," and bounded source-side retry on the former. Until that plan lands, the user must guarantee all factories are registered before `Mesh::start()` returns.
- **Cross-SDK restore.** A daemon snapshotted on a Rust node, migrated to a JS node, depends on the JS side having a factory registered for the same `kind`. Recommend convention: use the same `kind` strings across languages for conceptually-equivalent daemons.

### Scope cuts

- **Group coordination (`ReplicaGroup` / `ForkGroup` / `StandbyGroup`) is deferred.** They exist in the core but their public API isn't settled. Once settled, a Stage 7 adds group surface to the SDKs.
- **Scheduler as a service.** `Scheduler::place()` is a pure function; the SDK exposes it as such. A persistent scheduler service (with watchlists, policies, triggers) is an application-level concern, not SDK-level.
- **Permission-gated daemon spawn.** Not in v1. Capability matching is the control. A follow-up could require `PermissionToken` on `spawn()` once the security surface settles; plan for it by leaving a `opts: SpawnOptions` parameter in the signature.

### Cross-cutting with the other plans

- **Identity ([`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage A).** Required. Daemons have keypairs. No anonymous daemons. *What happens to a daemon's keypair on migration* — whether it travels in the snapshot, whether the target can sign with it, whether the source wipes its copy — is a separate protocol question sized + specified in [`DAEMON_IDENTITY_MIGRATION_PLAN.md`](DAEMON_IDENTITY_MIGRATION_PLAN.md). Until that lands, `restore_snapshot` cannot produce a keypair matching the source's `origin_hash` and every migration attempt fails deterministically.
- **Capabilities ([same plan], Stage C).** Recommended. Without declaring capabilities, a daemon can't advertise `subprotocol:0x0500` and auto-migration won't find targets. The SDK validates: if `requirements()` references a capability tag the node hasn't announced, log a warning at spawn.
- **CortEX ([`SDK_EXPANSION_PLAN.md`](SDK_EXPANSION_PLAN.md) Stages 2–3).** Independent. A daemon may use a `TasksAdapter` internally; the SDK makes no assumption.
- **Channels ([same plan] Stages 6–7).** Independent at the protocol level, but a daemon that subscribes to a channel needs its subscription re-bound on migration. Automatic re-bind is sized + specified in [`DAEMON_CHANNEL_REBIND_PLAN.md`](DAEMON_CHANNEL_REBIND_PLAN.md) — it lives outside this plan because it cuts across migration semantics + the `StateSnapshot` wire format, and is a correctness fix rather than a pure SDK addition. Until that lands, the application must re-subscribe the daemon on the target node after every migration.

---

## Sizing

| Stage | SDKs touched | Est. effort |
|---|---|---|
| 1. Rust daemon surface | Rust | 3–5 days |
| 2. Rust migration | Rust | 2–3 days |
| 3. TS daemon via dispatcher | NAPI + TS | ~1 week |
| 4. TS migration | NAPI + TS | 2–3 days |
| 5. Python daemon + migration | PyO3 + Python | ~1 week |
| 6. Go daemon + migration | C ABI + Go | 2+ weeks |

Total: ~5–7 weeks of engineering time. Each stage is an independent PR.

## Dependencies

- Stages 1–6 depend on [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage A (identity). Without `Identity`, daemons can't be constructed.
- Stages 3–6 benefit from (but don't strictly require) [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage C (capabilities) — without it, `start_migration_auto` has no targets.
- No dependency on CortEX / channel stages.

## Out of scope (for this plan)

- `ReplicaGroup` / `ForkGroup` / `StandbyGroup` — separate follow-up plan after group API stabilizes in core.
- Automatic migration triggers — application policy, not SDK.
- Persistent scheduler service — application-level.
- Revocation / permission-gated daemon spawn — handled in follow-up once auth story firms up.
- Cross-node daemon discovery — daemons are addressed by `origin_hash`; directory/discovery is the caller's problem in v1.
