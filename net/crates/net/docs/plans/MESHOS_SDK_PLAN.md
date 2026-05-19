## MeshOS SDK — implementation plan

> Language bindings that let daemons plug into MeshOS's supervision contract. **Daemon-side only** — no cluster-control surface, no placement APIs, no admin events, no replica manipulation. The SDK is "how an application writes a daemon," not "how an application drives MeshOS." Five languages — Rust (canonical), Python (pyo3), Node / TypeScript (napi-rs), Go (cgo), C (raw FFI) — mirroring the precedent set by [`MESHDB_PLAN.md`](MESHDB_PLAN.md)'s binding strategy. Companion to [`MESHOS_PLAN.md`](MESHOS_PLAN.md) (whose substrate-side surface this plan binds against).

## Status

**Phase 1 (Rust SDK) shipped.** The canonical surface lives behind the `meshos` Cargo feature and is re-exported from `net_sdk::meshos` (`crates/net/sdk/src/meshos.rs:91-147`). What landed against the design below: `MeshOsDaemonSdk::start(config, dispatcher) → register_daemon(daemon, kp) -> MeshOsDaemonHandle`, `next_control() / try_next_control()`, `publish_log(level, message)`, `graceful_shutdown(grace)`, `metadata() -> &MetadataView`, the `<<meshos-sdk-kind:KIND>>MSG` error format on `SdkError`, and the `daemon_main!` macro. Surfaces that grew beyond the original §1 design — and that the four language bindings now need to reshape — are listed under **Surfaces that grew beyond §1** below; the §2–§5 binding sections are written against the actual Rust surface, not the original sketch. The capability-publish path remains a stub (`MeshOsDaemonHandle::publish_capabilities` returns `Ok(())` pending the chain commit; see substrate-side TODO) — every binding's `publishCapabilities`/`publish_capabilities`/`PublishCapabilities`/`net_meshos_publish_capabilities` MUST surface the same stub semantics so consumers don't write code against a contract the substrate doesn't yet honor.

**Phases 2–5 shipped.** All four bindings ship the full daemon-author surface against the Rust SDK contract:
- **Python** — slice 1 + slice 2 (capability routing + `MaintenanceStateView` decode) + slice 3 (async `anext_control` + `async for handle`).
- **Node** — slice 1 + slice 2 (real `CapabilitySet` conversion) + slice 3 (`health` / `saturation` / `requiredCapabilities` / `optionalCapabilities` JS routing through TSFNs).
- **Go** — slice 1 + slice 1b (cgo `//export` vtable bridge) + slice 2 (`Metadata()` / `PublishCapabilities` / `ControlEvents` channel) + slice 3 (`MaintenanceStateView` decode + `context.Context` plumbing on every blocking call).
- **C** — slice 1 + slice 1b (vtable bridge) + slice 2 (`net_meshos_metadata` JSON envelope + `net_meshos_publish_capabilities` stub-passthrough).

`publish_capabilities` remains substrate-side-stubbed across every binding (every layer transparently inherits the stub semantics; the SDK API is final).

**Two-tier packaging is set per language.** Python ships as `bindings/python` (pyo3 cdylib) + `sdk-py` (pure-Python ergonomic wrapper); Node ships as `bindings/node` (napi-rs cdylib) + `sdk-ts` (pure-TypeScript ergonomic wrapper). The MeshOS-SDK binding lands in the cdylib tier; the ergonomic surface (context manager / `for await` iterator / typed factories) lands in the wrapper tier. Go has only the cdylib + `bindings/go/net/` wrapper (no `sdk-go`); C has only the header + cdylib. Each per-language design section calls out which tier owns which piece.

**Surfaces that grew beyond §1** (Rust-canonical, must be mirrored by every binding):

- `MaintenanceStateView` — WASM-friendly projection of `MaintenanceState` carrying relative-ms timestamps + a `kind` discriminator (`Active`, `EnteringMaintenance`, `Maintenance`, `ExitingMaintenance`, `DrainFailed { reason }`, `Recovery`). Daemons read it through `handle.metadata().maintenance_state` to observe their own node's state without needing an `Instant`-anchored representation. Bindings cannot serialize `MaintenanceState` directly — they MUST go through `MaintenanceStateView`.
- `publish_log(level: LogLevel, message: &str)` on the handle — non-blocking `try_publish` semantics. Drops the line + increments a metric when the runtime's log ring is full rather than parking. Every binding exposes this as `publish_log` / `publishLog` / `PublishLog` / `net_meshos_publish_log`.
- `try_next_control() -> Option<DaemonControl>` non-blocking variant alongside the async `next_control()`. Bindings that don't have a native async story (C, synchronous Python) expose only `try_next_control`-equivalent + a blocking-with-timeout variant; async-native bindings (Tokio, asyncio, AsyncIterable, channels) expose both.
- `DEFAULT_GRACEFUL_SHUTDOWN: Duration = 5s` and `DEFAULT_CONTROL_CHANNEL_CAPACITY: usize = 8` — defaults the binding surface should expose as named constants so consumers can pass them through `.with_control_capacity(...)` / equivalent.
- `DaemonControlRouter` + `SdkRoutingDispatcher<D>` + `RouterControlSink` — substrate-internal plumbing that wraps the consumer dispatcher with daemon-control translation. Bindings don't expose these directly; they're internal to how `start(config, dispatcher)` is constructed and are only relevant when the binding's `start()` signature accepts a user dispatcher.
- Re-exported substrate seams: `RedexAdminAuditAppender`, `RedexFailureAppender`, `RedexLogAppender` (production persistence), `OrchestratorMigrationAborter` / `MigrationAborter` / `MigrationAbortError` (migration-abort dispatcher seam), `OrchestratorMigrationSnapshotSource` / `MigrationSnapshot` / `MigrationPhaseSnapshot` / `MigrationSnapshotSource` (ICE blast-radius snapshot source seam). These are **runtime-wiring** types, not daemon-author types — bindings that target the daemon-author surface only (the common case) can skip them; bindings targeting tenant operators that also build the runtime expose them as opaque handles with a `with_xxx` configuration step on the `MeshOsDaemonSdk` builder.

**Activation gate for Phases 2–5:** a real consumer workload — Hermes / Deck / a tenant-supplied daemon — that needs to write supervised daemons in a language other than Rust. The other four bindings ride the same surface shape behind language-native ergonomics, landing in dependency order as consumers arrive.

**Substrate prereqs** (all in code today):

- **`MeshDaemon` trait + `DaemonHealth` + `DaemonControl`** at `src/adapter/net/compute/daemon.rs`. Extended in `MESHOS_PLAN.md`'s Phase B work with `health()` / `saturation()` / `on_control()` — the trait surface this plan binds against.
- **`DaemonRegistry`** at `src/adapter/net/compute/registry.rs`. Per-node daemon lifecycle (register / replace / unregister + `DaemonLifecycleObserver` hook).
- **`DaemonHost`** at `src/adapter/net/compute/host.rs`. Snapshot / restore / event-routing per daemon.
- **`CapabilitySet`** at `src/adapter/net/behavior/capability.rs`. The advertise-and-update surface MeshOS reads for placement-side decisions.
- **`MeshOsControl` / `DaemonControl`** at `src/adapter/net/behavior/meshos/control.rs` and `compute/daemon.rs`. The supervisor → daemon side-channel. SDK ships the **`DaemonControl`** (WASM-friendly relative-ms) form on the binding surface; `MeshOsControl::to_daemon_control(now)` already bridges.
- **MeshDB Python / Node / Go / C SDKs** at `bindings/{python, node, go, …}`. Precedent for the cross-language packaging + serialization patterns this plan follows.

**Substrate gaps that the SDK does NOT close:**

- The SDK is **read-only on cluster state**. Daemons can sample metadata + their own state but cannot mutate MeshOS's view of the cluster.
- The SDK is **daemon-side only**. No "embed MeshOS in a Python process" surface — that's the runtime job (Rust). The SDK gives daemons access to the trait contract; running MeshOS itself stays Rust-only.

## Frame

The MeshOS pipeline is end-to-end functional in Rust. Daemons that want to be supervised by MeshOS — and gain crash-loop gating, graceful shutdown, drain coordination, capability-driven placement, snapshot-restore for migration — currently must implement the `MeshDaemon` trait in Rust. Every other language is locked out.

**The SDK fixes that asymmetry.** Each language gets a thin binding that:

- Lets a daemon implement the `MeshDaemon` contract in its native style.
- Routes `process()` events, `on_control()` signals, `health()` / `saturation()` polls into the binding-side daemon code.
- Lets the daemon publish + update its `CapabilitySet` so MeshOS's placement filter can score it.
- Exposes a read-only metadata view so the daemon can observe its own context (node id, daemon id, current maintenance state, etc.).
- Carries the snapshot / restore path so MeshOS-driven migrations preserve daemon state.

**What the SDK explicitly does not do:**

- Issue placement decisions or replica actions. MeshOS is the authority; the daemon is the subject.
- Trigger admin events (drain, cordon, maintenance, etc.). Those are operator-signed chain commits, never SDK-initiated.
- Override MeshOS's avoid-list, backpressure, scheduler, or maintenance state machine.
- Compose against MeshDB, federated executor, or any other "MeshApp"-tier surface. Those belong to a separate SDK (`MESHAPP_SDK_PLAN.md`, future).

This single restriction — **SDK is the daemon contract, not MeshOS-control** — keeps the cluster deterministic. A daemon written in Python can't accidentally tell MeshOS to drain a node, no matter how hard the developer tries.

## Why this exists

Four reasons for a written plan rather than "we'll just bind the trait when someone asks":

1. **The non-goals are load-bearing.** Each language has idiomatic patterns that, if applied naively, would leak cluster-control surface into the SDK. Python's "let me query the system" mindset; Node's "give me a full client object"; Go's "this looks like a regular library"; C's "FFI takes whatever I want to expose." The plan needs to call out what each language MUST refuse to expose, with rationale, so future contributors don't accidentally widen the surface.

2. **The lifecycle is non-trivial.** Snapshot / restore + control events + capability updates + health polling all happen on different cadences with different ownership semantics. Each language needs an idiomatic shape (Python sync callback, Node AsyncIterable, Go channel, C function-pointer table) that maps onto the same underlying Rust trait without diverging on semantics.

3. **The five bindings should share contracts.** Serialization, error mapping, control-event shapes, capability-set shapes — these are wire / FFI contracts. Designing them per-language ad-hoc produces drift; designing them up-front gives every binding a common floor.

4. **Activation order matters.** Rust first (it's the canonical surface). Python second (the most-requested non-Rust binding per MeshDB usage patterns). Node third (Deck integration). Go fourth (existing tenant infrastructure). C fifth (long-tail integrations). Sequencing the bindings + their slice-of-the-trait coverage means each language ships with a coherent contract from day one, not a "v1 missing snapshot/restore" partial surface.

## What ships

Five language bindings, in dependency order:

1. **Rust SDK** — the canonical surface. Wraps `MeshDaemon` + `DaemonRegistry` + `DaemonHost` behind a `MeshOsDaemonHandle` + a `daemon_main!` macro for one-call lifecycle. Lives in `src/adapter/net/sdk/meshos/`.
2. **Python SDK** — pyo3-based. Sync-first (matches MeshDB's binding precedent). Lives in `bindings/python/src/meshos.rs` + `bindings/python/python/net/meshos.py`.
3. **Node / TypeScript SDK** — napi-rs-based. Async-iterable control events; the rest of the surface is sync. Lives in `bindings/node/src/meshos.rs` + `bindings/node/index.d.ts`.
4. **Go SDK** — cgo-based. Channel-based control events; sync everything else. Lives in `bindings/go/meshos-ffi/` + `bindings/go/go/meshos/`.
5. **C SDK** — raw FFI. Function-pointer-table for daemon callbacks; manual lifetime management. Lives in `bindings/go/meshos-ffi/include/net_meshos.h` (shared header) + the meshos-ffi cdylib.

Each binding ships:

- **Daemon-trait surface** — language-native form of `MeshDaemon` (name, process, snapshot, restore, requirements, required_capabilities, optional_capabilities, health, saturation, on_control).
- **Registration handle** — register / unregister / publish-capabilities / current-metadata.
- **Control event channel** — language-native delivery of `DaemonControl` variants.
- **Capability sync** — publish / update.
- **Snapshot / restore** — opaque-bytes contract (daemon owns the encoding).
- **Metadata read-only view** — node id, daemon id, current maintenance state, peer health summary.
- **Logging + metrics hooks** (optional per binding) — adapters into substrate-side observability.

What this doc does NOT ship (deferred non-goals, per the scope brief):

🚫 **No placement APIs.** No language can call into MeshOS's reconcile, score holders, request placement / eviction, or read the scheduler config. The Phase D-1 scheduler is internal to MeshOS; SDK consumers see only the *consequences* (their daemon got migrated; their `on_control(Shutdown)` fired) via the regular control channel.

🚫 **No direct replica manipulation.** No `pull_replica` / `drop_replica` / `request_placement` SDK methods in any language. Replica decisions are MeshOS's job — the SDK exposes neither the action enum nor the underlying primitives.

🚫 **No admin-event issuance.** Drain / cordon / uncordon / maintenance / invalidate-placement are operator-key-signed chain commits. The SDK has no path to emit them, in any language. Operator tooling (a separate `MESHOS_OPS_PLAN.md`, future) covers the admin surface.

🚫 **No "control MeshOS" APIs.** Avoid lists, backpressure flags, drift signals, maintenance transitions, action admission — all opaque to the SDK. A daemon receives `BackpressureOn { level }`; it cannot read the queue depth that triggered it or override the threshold.

🚫 **No timers / batch jobs / remote execution / workflow / MeshDB-as-tool / federated.** These belong in higher-level surfaces (MeshApp SDK, MeshDB SDK, Hermes binding). The MeshOS SDK is specifically the **daemon supervision contract**, nothing more.

---

## Design

### 1. Rust SDK (canonical) — shipped

Lives at `crates/net/sdk/src/meshos.rs` (the customer-facing seam) re-exporting types whose implementation sits at `crates/net/src/adapter/net/behavior/meshos/sdk.rs`. The two-file split mirrors the MeshDB SDK pattern: the SDK module is a re-export curtain so consumers `use net_sdk::meshos::*` rather than reaching into substrate internals.

```rust
// Re-exports from substrate (actual import path)
pub use net::adapter::net::compute::{
    DaemonControl, DaemonError, DaemonHealth, MeshDaemon,
};
pub use net::adapter::net::behavior::capability::CapabilitySet;
pub use net::adapter::net::behavior::meshos::{
    MeshOsDaemonSdk, MeshOsDaemonHandle, MetadataView, MaintenanceStateView,
    SdkError, SdkRoutingDispatcher, DaemonControlRouter,
    DEFAULT_CONTROL_CHANNEL_CAPACITY, DEFAULT_GRACEFUL_SHUTDOWN,
};

// Entry point — runtime construction wraps a user-supplied dispatcher
// with daemon-control routing; the SDK owns lifecycle of both.
impl MeshOsDaemonSdk {
    pub fn start(config: MeshOsConfig, dispatcher: Arc<dyn ActionDispatcher>) -> Self;
    pub fn start_with_options(config: MeshOsConfig, dispatcher: Arc<dyn ActionDispatcher>,
                              opts: MeshOsDaemonSdkOptions) -> Self;
    pub fn from_runtime(runtime: MeshOsRuntime) -> Self;

    /// Register a daemon. Returns a handle owning its lifecycle.
    /// Drop the handle to unregister (graceful — the supervisor
    /// sees the unregister event via the lifecycle observer).
    pub fn register_daemon(
        &self,
        daemon: Box<dyn MeshDaemon>,
        keypair: EntityKeypair,
    ) -> Result<MeshOsDaemonHandle, SdkError>;

    pub async fn shutdown(self) -> Result<(), RuntimeShutdownError>;
}

impl MeshOsDaemonHandle {
    pub fn daemon_id(&self) -> u64;
    pub fn daemon_name(&self) -> &str;
    pub fn metadata(&self) -> &MetadataView;

    /// Async control-event receive. Parks until the supervisor
    /// emits a signal or the handle drops.
    pub async fn next_control(&mut self) -> Option<DaemonControl>;

    /// Non-blocking variant. Returns `None` if the channel is
    /// empty (vs. closed); bindings without async expose this.
    pub fn try_next_control(&mut self) -> Option<DaemonControl>;

    /// Publish a log line via the substrate's log ring.
    /// Non-blocking; drops the line + increments a metric when
    /// the ring is full.
    pub fn publish_log(&self, level: LogLevel, message: &str) -> Result<(), SdkError>;

    /// Stub today — returns `Ok(())` without committing. Will
    /// route through the capability chain once the substrate
    /// commit path lands. Bindings expose the same stub semantics.
    pub fn publish_capabilities(&self, caps: CapabilitySet) -> Result<(), SdkError>;

    pub async fn graceful_shutdown(self, grace: Duration) -> Result<(), SdkError>;
}

/// Read-only cluster context. `maintenance_state` is the
/// WASM-friendly `MaintenanceStateView` (relative-ms, no
/// `Instant`), not the substrate's `Instant`-anchored
/// `MaintenanceState`.
pub struct MetadataView {
    pub node_id: NodeId,
    pub daemon_id: u64,
    pub daemon_name: String,
    pub maintenance_state: MaintenanceStateView,
    pub peers: BTreeMap<NodeId, PeerSnapshot>,
}

/// WASM-friendly maintenance-state projection. Variants:
/// Active, EnteringMaintenance { deadline_in_ms }, Maintenance,
/// ExitingMaintenance { deadline_in_ms }, DrainFailed { reason },
/// Recovery { since_ms }. Every binding emits this shape.
pub enum MaintenanceStateView { /* … */ }
```

**`daemon_main!` macro** for the common case (single daemon per process):

```rust
daemon_main! {
    name: "my-telemetry",
    daemon: MyTelemetryDaemon::new(),
    capabilities: CapabilitySet::new().add_tag("software.telemetry"),
    on_control: |ev| match ev {
        DaemonControl::Shutdown { grace_period_ms } => { /* drain */ },
        DaemonControl::BackpressureOn { level } => { /* throttle */ },
        _ => {}
    },
}
```

Behind the macro: registers the daemon, spawns the control-event task, runs the daemon's main loop, handles graceful shutdown on `Shutdown` or SIGTERM.

**Substrate dependencies:** `MeshOsRuntime`, `MeshDaemon`, `DaemonRegistry`, `CapabilitySet`. No new substrate APIs; the SDK is purely a re-bundling.

### 2. Python SDK (pyo3) — design

**Tier split.** The pyo3 cdylib `bindings/python/src/meshos.rs` exposes the raw FFI types (`PyMeshOsDaemonSdk`, `PyMeshOsDaemonHandle`); the pure-Python wrapper `sdk-py/src/net_sdk/meshos.py` adds the ergonomic context-manager + protocol class. Precedent: `bindings/python/src/compute.rs` already wires `MeshDaemon` for the compute surface (without MeshOS supervision) — the MeshOS-SDK pyo3 module re-uses its `PyDaemonRuntime` wrapping strategy (per-callback `Python::with_gil`, factory marshalling).

```python
# sdk-py/src/net_sdk/meshos.py — ergonomic wrapper
from net._net import (
    MeshOsDaemonSdk as _RawSdk,
    MeshOsDaemonHandle as _RawHandle,
    DaemonControl, DaemonHealth, LogLevel,
    MaintenanceStateView,
)

class MeshOsDaemon:
    """Implement this to be supervised by MeshOS."""
    def name(self) -> str: ...
    def process(self, event: bytes) -> list[bytes]: return []

    # Optional methods — default impls match the Rust defaults
    def health(self) -> DaemonHealth: return DaemonHealth.Healthy
    def saturation(self) -> float: return 0.0
    def on_control(self, ev: DaemonControl) -> None: pass
    def snapshot(self) -> bytes | None: return None
    def restore(self, state: bytes) -> None: pass
    def required_capabilities(self) -> "CapabilitySet": return CapabilitySet()
    def optional_capabilities(self) -> "CapabilitySet": return CapabilitySet()

class MeshOsDaemonSdk:
    @classmethod
    def start(cls, config: "MeshOsConfig", dispatcher) -> "MeshOsDaemonSdk": ...
    def register_daemon(
        self, daemon: MeshOsDaemon, keypair: "EntityKeypair",
    ) -> "MeshOsDaemonHandle": ...
    def shutdown(self) -> None: ...
    def __enter__(self): return self
    def __exit__(self, *exc): self.shutdown()

class MeshOsDaemonHandle:
    @property
    def daemon_id(self) -> int: ...
    @property
    def daemon_name(self) -> str: ...
    @property
    def metadata(self) -> "MetadataView": ...

    def next_control(self, timeout_ms: int | None = None) -> DaemonControl | None:
        """Blocking with optional timeout. Returns None on timeout/closed."""
    def try_next_control(self) -> DaemonControl | None: ...
    def publish_log(self, level: LogLevel, message: str) -> None: ...
    def publish_capabilities(self, caps: "CapabilitySet") -> None:
        """Stub today — returns without committing (substrate TODO)."""
    def graceful_shutdown(self, grace_ms: int = 5_000) -> None: ...
    def __enter__(self): return self
    def __exit__(self, *exc): self.graceful_shutdown()
```

```python
# Usage
import net_sdk.meshos as meshos

class TelemetryDaemon(meshos.MeshOsDaemon):
    def name(self): return "telemetry"
    def process(self, event: bytes): return [b"out"]
    def health(self):
        return (meshos.DaemonHealth.Healthy if self.queue_depth < 1000
                else meshos.DaemonHealth.Degraded(reason="queue depth"))

with meshos.MeshOsDaemonSdk.start(config, dispatcher) as sdk:
    with sdk.register_daemon(TelemetryDaemon(), kp) as handle:
        handle.publish_log(meshos.LogLevel.Info, "started")
        while ev := handle.next_control():
            if ev.kind == "Shutdown":
                break
```

**Sync-first.** `next_control(timeout_ms)` is blocking per Python convention (with an explicit timeout — bindings without async still need cooperative cancellation). An `async def anext_control()` + `async for` lands when a consumer asks for the pyo3-asyncio shape.

**Trait routing.** pyo3 instantiates a `PyMeshOsDaemon` wrapper struct holding the Python object; the Rust side implements `MeshDaemon` by calling back into Python via `Python::with_gil` for each trait method. GIL is acquired once per call; control-event delivery uses a per-daemon mpsc that `register_daemon` feeds into the wrapper.

**Error surface.** Errors raise `MeshOsSdkError(kind: str, message: str)` with the substrate `<<meshos-sdk-kind:KIND>>MSG` discriminator parsed into `(kind, message)`. Known kinds today: `register_failed`, `queue_full`, `loop_closed` (additive — every binding tolerates unknown kinds as a passthrough). Routing precedent: the compute binding's `daemon: <kind>[: detail]` convention at `bindings/python/src/compute.rs:49`.

### 3. Node / TypeScript SDK (napi-rs) — design

**Tier split.** The napi-rs cdylib `bindings/node/src/meshos.rs` exposes raw classes (`MeshOsDaemonSdk`, `MeshOsDaemonHandle`) and a `nextControl(timeoutMs?)` napi-callable; the pure-TypeScript wrapper at `sdk-ts/src/meshos.ts` re-exports them and adds `AsyncIterable` + a typed `MeshOsDaemon` interface. Precedent: `bindings/node/src/compute.rs` wires `MeshDaemon` for compute via TSFN-marshalled factories — the same TSFN strategy carries here, plus an `EventDispatchBridge` analogue for control-event delivery (factor out as a shared helper).

```typescript
// sdk-ts/src/meshos.ts (ergonomic wrapper)
export interface MeshOsDaemon {
  name(): string;
  process(event: Buffer): Buffer[];

  health?(): DaemonHealth;
  saturation?(): number;
  onControl?(ev: DaemonControl): void;
  snapshot?(): Buffer | undefined;
  restore?(state: Buffer): void;
  requiredCapabilities?(): CapabilitySet;
  optionalCapabilities?(): CapabilitySet;
}

export class MeshOsDaemonSdk {
  static start(config: MeshOsConfig, dispatcher: ActionDispatcher): Promise<MeshOsDaemonSdk>;
  registerDaemon(daemon: MeshOsDaemon, keypair: EntityKeypair): Promise<MeshOsDaemonHandle>;
  shutdown(): Promise<void>;
}

export class MeshOsDaemonHandle {
  readonly daemonId: bigint;
  readonly daemonName: string;
  readonly metadata: MetadataView;     // { ..., maintenanceState: MaintenanceStateView }

  nextControl(timeoutMs?: number): Promise<DaemonControl | null>;
  tryNextControl(): DaemonControl | null;
  controlEvents(): AsyncIterable<DaemonControl>;   // TS shim over nextControl
  publishLog(level: LogLevel, message: string): void;
  publishCapabilities(caps: CapabilitySet): Promise<void>;   // stub
  gracefulShutdown(graceMs?: number): Promise<void>;
}
```

```typescript
import { MeshOsDaemonSdk, DaemonHealth } from '@net-mesh/sdk/meshos';

const daemon: MeshOsDaemon = {
  name() { return 'telemetry'; },
  process(ev) { return [Buffer.from('out')]; },
  health() {
    return this.queueDepth < 1000
      ? DaemonHealth.healthy()
      : DaemonHealth.degraded({ reason: 'queue depth' });
  },
};

const sdk = await MeshOsDaemonSdk.start(config, dispatcher);
const handle = await sdk.registerDaemon(daemon, kp);
for await (const ev of handle.controlEvents()) {
  if (ev.kind === 'Shutdown') break;
}
await handle.gracefulShutdown(5000);
await sdk.shutdown();
```

**AsyncIterable** matches the MeshDB Node binding's `for await` ergonomics — the same TS shim pattern in `sdk-ts/src/` that adds `Symbol.asyncIterator` over a raw `nextControl()` napi method. The cdylib never exposes the `AsyncIterable` directly; that's wrapper-tier only so the TS shim stays a pure-JS transform.

**Error surface.** Errors throw `MeshOsSdkError extends Error { kind: string }` with the discriminator parsed out of the substrate's `<<meshos-sdk-kind:KIND>>MSG` message. Precedent: `bindings/node/errors.ts` already implements the discriminator-aware `classifyError(message)` over the compute / MeshDB error namespace; the MeshOS error wrapper plugs into the same matcher.

### 4. Go SDK (cgo) — design

**Tier layout.** Cdylib at `bindings/go/meshos-ffi/` exporting `net_meshos_*` C functions; Go wrapper at `bindings/go/net/meshos.go` providing the idiomatic surface. There is no `sdk-go/` tier (the Go binding is single-tier; the wrapper file lives next to the existing `compute.go` equivalent). Precedent: `bindings/go/compute-ffi/src/lib.rs` (currently stage 6 of the daemon lifecycle) gives the FFI shape; the `.go` wrapper for it is the gap — the MeshOS-SDK binding can ship its own `meshos.go` ahead of the compute one or land both together.

```go
// bindings/go/net/meshos.go
package net

type MeshOsDaemon interface {
    Name() string
    Process(event []byte) ([][]byte, error)

    // Optional — embed DefaultDaemon to get defaults.
    Health() DaemonHealth
    Saturation() float32
    OnControl(ev DaemonControl)
    Snapshot() ([]byte, error)
    Restore(state []byte) error
    RequiredCapabilities() CapabilitySet
    OptionalCapabilities() CapabilitySet
}

type DefaultDaemon struct{}
func (DefaultDaemon) Health() DaemonHealth { return DaemonHealth{Kind: HealthHealthy} }
func (DefaultDaemon) Saturation() float32 { return 0 }
// ... etc

type MeshOsDaemonSdk struct{ /* opaque handle */ }

func StartMeshOsDaemonSdk(ctx context.Context, cfg MeshOsConfig, dispatcher ActionDispatcher,
                          opts ...MeshOsDaemonSdkOption) (*MeshOsDaemonSdk, error)
func (s *MeshOsDaemonSdk) RegisterDaemon(daemon MeshOsDaemon, kp *EntityKeypair) (*MeshOsDaemonHandle, error)
func (s *MeshOsDaemonSdk) Shutdown(ctx context.Context) error

type MeshOsDaemonHandle struct{ /* opaque handle */ }
func (h *MeshOsDaemonHandle) DaemonID() uint64
func (h *MeshOsDaemonHandle) DaemonName() string
func (h *MeshOsDaemonHandle) Metadata() *MetadataView
func (h *MeshOsDaemonHandle) ControlEvents() <-chan DaemonControl       // closes on shutdown/ctx-cancel
func (h *MeshOsDaemonHandle) TryNextControl() (DaemonControl, bool)
func (h *MeshOsDaemonHandle) PublishLog(level LogLevel, message string) error
func (h *MeshOsDaemonHandle) PublishCapabilities(caps CapabilitySet) error   // stub
func (h *MeshOsDaemonHandle) GracefulShutdown(ctx context.Context) error
```

```go
type Telemetry struct {
    net.DefaultDaemon
    queueDepth int
}
func (t *Telemetry) Name() string { return "telemetry" }
func (t *Telemetry) Process(ev []byte) ([][]byte, error) { return [][]byte{[]byte("out")}, nil }
func (t *Telemetry) Health() net.DaemonHealth {
    if t.queueDepth < 1000 { return net.DaemonHealth{Kind: net.HealthHealthy} }
    return net.DaemonHealth{Kind: net.HealthDegraded, Reason: "queue depth"}
}

sdk, _ := net.StartMeshOsDaemonSdk(ctx, cfg, dispatcher)
defer sdk.Shutdown(ctx)
handle, _ := sdk.RegisterDaemon(&Telemetry{}, kp)
for ev := range handle.ControlEvents() {
    if ev.Kind == net.ControlShutdown { break }
}
handle.GracefulShutdown(ctx)
```

**Pumping goroutine.** The Go wrapper spawns a per-handle goroutine that pumps control events from the cdylib over a `chan DaemonControl`. The channel closes on `ctx.Done()` or substrate shutdown — caller selects on the channel + `ctx.Done()` per standard Go cancellation idiom (lifted from `bindings/go/net/meshdb.go`'s `MeshDBQuery.ExecuteContext`).

**Error surface.** Errors come back as `MeshOsSdkError{Kind, Message}` parsed from the `<<meshos-sdk-kind:KIND>>MSG` envelope. Precedent: `bindings/go/compute-ffi/src/lib.rs:83-125` already formats SDK errors with this convention.

### 5. C SDK (raw FFI) — design

Header at `include/net_meshos.h` (joins the existing `net.h` / `net_meshdb.h` / `net_rpc.h` / `net.go.h` set under `crates/net/include/`); cdylib shared with Go at `bindings/go/meshos-ffi/`. Function-pointer-table for daemon callbacks; the consumer owns memory.

```c
// Vtable the consumer fills in
typedef struct {
    const char* (*name)(void* ctx);
    int (*process)(void* ctx,
                   const uint8_t* event, size_t event_len,
                   uint8_t*** outputs_out, size_t** output_lens_out, size_t* outputs_count);
    int (*health)(void* ctx, NetDaemonHealth* out);          // optional
    float (*saturation)(void* ctx);                          // optional
    void (*on_control)(void* ctx, const NetDaemonControl*);  // optional
    int (*snapshot)(void* ctx, uint8_t** out, size_t* len);  // optional
    int (*restore)(void* ctx, const uint8_t* state, size_t); // optional
} NetMeshOsDaemonVtable;

// SDK lifecycle (one per process; multi-instance allowed for tests)
NetMeshOsDaemonSdk* net_meshos_sdk_start(const NetMeshOsConfig*,
                                          const NetActionDispatcherVtable* dispatcher_vt,
                                          void* dispatcher_ctx);
int net_meshos_sdk_shutdown(NetMeshOsDaemonSdk*);
void net_meshos_sdk_free(NetMeshOsDaemonSdk*);

// Per-daemon lifecycle
NetMeshOsDaemonHandle* net_meshos_register_daemon(NetMeshOsDaemonSdk*,
                                                   const NetMeshOsDaemonVtable* vt, void* ctx,
                                                   const NetEntityKeypair* kp);
void net_meshos_handle_free(NetMeshOsDaemonHandle*);

// Control events — caller polls (blocking with timeout) or tries non-blocking
int net_meshos_next_control(NetMeshOsDaemonHandle*, NetDaemonControl* out, uint64_t timeout_ms);
int net_meshos_try_next_control(NetMeshOsDaemonHandle*, NetDaemonControl* out);

// Log emission (non-blocking; ring full ⇒ drop + metric)
int net_meshos_publish_log(NetMeshOsDaemonHandle*, NetLogLevel level, const char* message);

// Capabilities (stub today; returns 0 without committing)
int net_meshos_publish_capabilities(NetMeshOsDaemonHandle*, const NetCapabilitySet*);

// Metadata read-only — pointer valid for the handle's lifetime
const NetMetadataView* net_meshos_metadata(NetMeshOsDaemonHandle*);

// Graceful shutdown — drains the daemon, frees the handle on success
int net_meshos_graceful_shutdown(NetMeshOsDaemonHandle*, uint64_t grace_ms);

// Last-error surface (matches existing net.h / net_meshdb.h pattern)
const char* net_meshos_last_error_message(void);
const char* net_meshos_last_error_kind(void);
void net_meshos_clear_last_error(void);
```

**Patterns lifted from existing C headers.** `include/net_meshdb.h:467-480` already pins the thread-local `last_error_{message,kind}` + `clear_last_error` triple; the MeshOS header mirrors it byte-for-byte (only the prefix changes). The cdylib uses the same `ffi_guard!` macro wrapping every entry point for `catch_unwind`; sample code uses `<inttypes.h>` `PRIx64` / `PRIu64` macros.

**`MaintenanceStateView` C-form** is a tagged union: `typedef struct { uint32_t tag; union { struct { uint64_t deadline_in_ms; } entering_or_exiting; struct { const char* reason; } drain_failed; struct { uint64_t since_ms; } recovery; } body; } NetMaintenanceStateView;` with `tag` ∈ `{Active=0, EnteringMaintenance=1, Maintenance=2, ExitingMaintenance=3, DrainFailed=4, Recovery=5}`. The reason string lives in arena memory owned by the parent `NetMetadataView`; lifetime ends at handle drop.

### 6. Wire / FFI contracts (shared across bindings)

**`DaemonControl` serialization.** Postcard for the cross-binding wire; each language decodes to its native shape (`DaemonControl::Shutdown { grace_period_ms }` in Rust, `DaemonControl.Shutdown(grace_period_ms=5000)` in Python, `{ kind: 'Shutdown', gracePeriodMs: 5000n }` in TS, etc.). The `#[non_exhaustive]` attribute on `DaemonControl` already in `compute::daemon` means new variants don't break older language wrappers — they observe `"unknown"` kind and continue.

**`CapabilitySet` serialization.** Reuses the existing capability serialization (from `MESHDB_PLAN`'s precedent). Python passes Python dicts; Node passes JS objects; Go passes structs; C passes a typed struct.

**Snapshot / restore.** Opaque bytes. The daemon owns the encoding; MeshOS treats the snapshot as a blob. Postcard for Rust daemons by convention; other languages pick their own.

**Error kinds.** Substrate format is `<<meshos-sdk-kind:KIND>>MSG` (substrate-side regex matches MeshDB's). Kinds actually emitted by the Rust SDK today: `register_failed`, `queue_full`, `loop_closed`. Bindings parse the kind into a language-native field (Python `MeshOsSdkError.kind`, TS `err.kind`, Go `MeshOsSdkError{Kind}`, C `net_meshos_last_error_kind()`) and pass unknown kinds through unchanged so substrate-side additions don't require coordinated binding releases.

### 7. Tests

Each language SDK ships:

- **Per-binding lifecycle tests** — register / unregister / control-event-receive / capability-publish round trips.
- **Mock dispatcher / runtime fixture** — the SDK tests against a `MeshOsRuntime` spawned with `LoggingDispatcher`, no real substrate I/O. Same pattern as the existing pipeline integration tests at `tests/meshos_pipeline.rs`.
- **Non-goal enforcement tests** — try to call into placement / admin / scheduler from the SDK surface; assert there's no method to call. (Compile-time enforcement for typed languages; "no such method on the namespace" tests for Python / Node.)
- **Cross-binding parity test** — a small daemon written in each language registers, receives a `Shutdown` control event, and unregisters cleanly. Pinned via a shared test fixture per binding.

### 8. Documentation

Per-binding README under the binding's directory. Each README walks one realistic daemon end-to-end:

1. Implement the daemon trait in the language.
2. Register with MeshOS.
3. Handle a `DrainStart` control event (graceful work cessation).
4. Publish a capability update mid-lifetime.
5. Snapshot + restore (for migration).

The Python / Node README's match the MeshDB SDK README format (slice-based, explicit "what ships in v1 vs deferred"). The Go README matches the existing `bindings/go/README.md` style. The C SDK ships a runnable `examples/meshos.c` analogous to the existing `examples/meshdb.c`.

---

## Locked decisions

Lock these so phase implementations don't relitigate:

1. **SDK is daemon-side only.** No `MeshOsRuntime`-control surface in any binding. Daemons receive `DaemonControl` events; daemons cannot emit any.
2. **`DaemonControl` is the control wire form.** SDK consumers see `DaemonControl` (relative-ms deadlines, no `Instant`), never `MeshOsControl` (Instant-anchored). The conversion happens substrate-side.
3. **Trait surface is fixed at the start.** Adding methods to `MeshDaemon` after a binding ships requires either default impls (additive) or a major version bump on the binding. Treat the trait shape as a wire contract.
4. **Capability publishing rides the existing capability layer.** No SDK-specific capability shape — the `CapabilitySet` type the SDK exposes is the same one MeshOS reads. Cross-binding consistency follows for free.
5. **Snapshot / restore is opaque bytes.** The SDK never inspects daemon state. Encoding is the daemon's choice; postcard is the conventional default for Rust daemons but unenforced.
6. **Per-language async model is native.** Python: sync (with async wrappers as a follow-up slice). Node: async-iterable + async methods. Go: channels + `context.Context`. Rust: tokio async on the handle methods. C: blocking polls. No language adopts another's async style.
7. **Error kinds use the `<<meshos-sdk-kind:KIND>>MSG` discriminator.** Same parsing approach as MeshDB so consumers that use both SDKs share the regex.
8. **Control-event delivery is at-most-once.** If the daemon doesn't consume a control event before the next one fires, the older event is dropped + a metric increments. MeshOS doesn't queue control events indefinitely.
9. **No placement / admin / scheduler / replica / drift / avoid-list / backpressure-tuning surface in any binding, ever.** This isn't "v1 deferred" — it's "permanently out of scope for the daemon SDK." Operator tooling lives in a separate SDK.
10. **`DaemonRegistry` is internal; daemons reach it only through the SDK handle.** No language exposes a registry surface directly. The SDK is the daemon's only path in or out.

---

## Phases

Activation order, dependency-driven:

- **Phase 1 — Rust SDK. SHIPPED.** `net_sdk::meshos` re-exports + the `daemon_main!` macro live at `crates/net/sdk/src/meshos.rs`; the implementation lives at `crates/net/src/adapter/net/behavior/meshos/sdk.rs`. Surfaces landed: `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `MetadataView`, `MaintenanceStateView`, `SdkError` (`<<meshos-sdk-kind:KIND>>MSG`), `next_control` / `try_next_control` / `publish_log` / `graceful_shutdown`, RedEX appender re-exports (`RedexAdminAuditAppender` / `RedexFailureAppender` / `RedexLogAppender`), migration-abort seam (`OrchestratorMigrationAborter`), migration-snapshot source seam (`OrchestratorMigrationSnapshotSource`). Integration tests in-module + `tests/meshos_pipeline.rs` + `tests/compute_runtime.rs`. **Stub remaining:** `publish_capabilities` — substrate chain commit not yet wired; the SDK surface is final, the binding can ship the method now and consumers see no behavior change when the substrate lands the commit.
- **Phase 2 — Python SDK. SHIPPED.** `bindings/python/src/meshos.rs` + `sdk-py/src/net_sdk/meshos.py`. Slice 1 (lifecycle + control + log + shutdown), Slice 2 (`MaintenanceStateView` decode + snapshot/restore + `required_capabilities` / `optional_capabilities` routing — `list[str]` of tag identifiers resolved at registration time and cached as a `CapabilitySet`), Slice 3 (`await handle.anext_control(...)` + `async for ev in handle:` via pure-Python `asyncio` poll-with-sleep over `try_next_control`; a `threading.Lock` in the wrapper serializes concurrent `&mut self` borrows from an event-loop task + a thread-pool executor).
- **Phase 3 — Node / TypeScript SDK. SHIPPED.** `bindings/node/src/meshos.rs` + `sdk-ts/src/meshos.ts`. Slice 1 (lifecycle + `controlEvents()` AsyncIterable + log + shutdown), Slice 2 (real `CapabilitySet` round-trip; `requiredCapabilities` / `optionalCapabilities` as `string[]` property or `() => string[]` callable resolved on the JS thread during `FromNapiValue`), Slice 3 (typed `MaintenanceStateView` matchers + `health()` / `saturation()` JS callbacks via TSFNs; `health` accepts either a string discriminator or a `{kind, reason?}` object).
- **Phase 4 — Go SDK. SHIPPED.** `bindings/go/meshos-ffi/src/lib.rs` + `bindings/go/net/meshos.go`. Slice 1 (register / control / log / shutdown via lifecycle-only no-op daemon), Slice 1b (`MeshOsDaemon` interface + vtable-based cgo `//export` trampoline bridge for process / snapshot / restore / on_control / health / saturation), Slice 2 (`MetadataView` Go struct + `Metadata()` / `RefreshMetadata()` accessors + `PublishCapabilities(tags []string)` stub + `ControlEvents(ctx) <-chan MeshOsDaemonControl` pumping goroutine), Slice 3 (`MaintenanceStateView` Go decode as a tagged enum + `MetadataContext(ctx)` / `GracefulShutdownContext(ctx, grace)` context-aware variants).
- **Phase 5 — C SDK. SHIPPED.** `include/net_meshos.h` + `bindings/go/meshos-ffi/` cdylib. Slice 1 (`net_meshos_sdk_start` / `net_meshos_register_daemon` / `net_meshos_next_control` / `net_meshos_publish_log` / `net_meshos_graceful_shutdown` + last-error trio), Slice 1b (`NetMeshOsDaemonVtable` + `net_meshos_register_daemon_with_vtable` + `_process_emit` / `_snapshot_emit` helpers), Slice 2 (`net_meshos_metadata(handle)` / `net_meshos_refresh_metadata(handle)` returning the metadata as a JSON CString freed via `net_meshos_free_string`, with `maintenance_state` as a tagged JSON object covering all 6 substrate variants + a forward-compat `unknown` fallback; `net_meshos_publish_capabilities(handle, tags_json_ptr, tags_json_len)` stub-passthrough until substrate commits).

All four language bindings now reach "v1 done" for the daemon-author contract.

### Remaining work — cross-cutting only

The per-binding daemon-author surface is feature-complete. What's left is **cross-cutting polish** and doesn't gate the "v1 done" call:

- Runnable end-to-end examples per binding (`examples/meshos.{c,py,ts,go}`) walking the §8 daemon-author workflow (implement → register → drain → publish capability → snapshot/restore).
- Cross-binding parity test (§7) — one daemon per language registers, receives a `Shutdown`, unregisters cleanly against a shared substrate fixture.
- Substrate-side `publish_capabilities` chain commit (currently a stub on the Rust SDK; every binding inherits the stub transparently and will cut over without API changes when the substrate lands the commit).

---

## Non-goals

Per the scope brief, the SDK is not:

- A scheduler control surface.
- A replica-management surface.
- An admin-event issuance surface.
- A backpressure / avoid-list / maintenance override surface.
- A timer / batch-job / remote-execution / workflow surface.
- A MeshDB query surface (that's the MeshDB SDK).
- A federated-interaction surface (parked for a future MeshApp SDK).

The SDK is **the daemon contract, exposed in five languages**. Everything else stays inside MeshOS or in an adjacent SDK with its own plan.

---

## Interaction surfaces

The SDK interacts with two substrate systems per binding:

- **MeshOS** — for daemon registration, control-event reception, snapshot/restore, capability publishing. The SDK is the daemon's only path into the substrate's supervision contract.
- **Capability System** — for capability advertisement + updates. The SDK exposes `CapabilitySet` directly; updates ride the existing capability layer's chain commits.

The SDK explicitly does NOT interact with:

- **RedEX directly.** Daemons see events through `process()`, not raw RedEX appends.
- **MeshDB.** Querying chain history is a MeshDB SDK concern.
- **`PlacementFilter`.** Daemons advertise capabilities; MeshOS scores them; the daemon never sees the score.
- **The admin chain.** Drain / cordon / maintenance commits are operator-tool territory.

---

## Test surface

Following the MeshDB SDK precedent:

- **Per-binding unit tests** — language-native test runner (cargo test / pytest / vitest / `go test` / a small C harness).
- **Per-binding integration tests** — register a real daemon against an in-process `MeshOsRuntime`; drive control events; verify lifecycle.
- **Cross-binding parity test** — one daemon per language registers + receives a `Shutdown` + unregisters; pinned against a shared fixture.
- **Non-goal enforcement** — compile-time for typed languages (no method exists); runtime "no such attribute" for Python; "no exported function" greps for C. The SDK's surface is checked against an explicit allowlist in CI so a future contributor can't quietly widen it.

---

*Atomic Playboys (post-`MESHOS_PLAN.md`) release candidate. Gates on a real non-Rust daemon workload; the Rust SDK (Phase 1) lands once the daemon-trait extension settles in production. Sequencing of phases 2–5 follows consumer demand per the MeshDB SDK precedent.*
