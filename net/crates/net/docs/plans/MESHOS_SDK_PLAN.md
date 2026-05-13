## MeshOS SDK — implementation plan

> Language bindings that let daemons plug into MeshOS's supervision contract. **Daemon-side only** — no cluster-control surface, no placement APIs, no admin events, no replica manipulation. The SDK is "how an application writes a daemon," not "how an application drives MeshOS." Five languages — Rust (canonical), Python (pyo3), Node / TypeScript (napi-rs), Go (cgo), C (raw FFI) — mirroring the precedent set by [`MESHDB_PLAN.md`](MESHDB_PLAN.md)'s binding strategy. Companion to [`MESHOS_PLAN.md`](MESHOS_PLAN.md) (whose substrate-side surface this plan binds against).

## Status

Design only. The MeshOS substrate ships behind the `meshos` Cargo feature (`MESHOS_PLAN.md` Phases A–G + executor + snapshot reader + source converters + scheduler + chain integration); this plan turns the daemon-facing slice into language bindings.

Activation gate: a real consumer workload — Hermes / Deck / a tenant-supplied daemon — that needs to write supervised daemons in a language other than Rust. The Rust SDK is the canonical surface; the other four bindings ride the same trait-shape behind language-native ergonomics, landing in dependency order as consumers arrive.

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

### 1. Rust SDK (canonical)

Lives in `src/adapter/net/sdk/meshos/`. Re-exports the relevant `compute::daemon` + `behavior::meshos` types under a clean SDK-facing module path so consumers don't import substrate internals.

```rust
// Re-exports from substrate
pub use crate::adapter::net::compute::daemon::{
    DaemonControl, DaemonError, DaemonHealth, MeshDaemon,
};
pub use crate::adapter::net::behavior::capability::CapabilitySet;

// SDK-side surface
pub struct MeshOsDaemonHandle {
    daemon_id: u64,
    registry: Arc<DaemonRegistry>,
    control_rx: tokio::sync::mpsc::Receiver<DaemonControl>,
    metadata: MetadataView,
}

impl MeshOsDaemonHandle {
    /// Register a daemon with a MeshOS runtime. Returns the
    /// handle that owns the daemon's lifecycle. Drop the
    /// handle to unregister (graceful — the supervisor sees
    /// the unregister event via the lifecycle observer).
    pub fn register(
        runtime: &MeshOsRuntime,
        daemon: Box<dyn MeshDaemon>,
        keypair: EntityKeypair,
    ) -> Result<Self, SdkError>;

    /// Receive the next supervisor control event. Async — parks
    /// until the supervisor emits a signal or the handle drops.
    pub async fn next_control(&mut self) -> Option<DaemonControl>;

    /// Snapshot of cluster metadata visible to this daemon.
    /// Read-only.
    pub fn metadata(&self) -> &MetadataView;

    /// Publish (or update) the daemon's CapabilitySet.
    pub fn publish_capabilities(&self, caps: CapabilitySet) -> Result<(), SdkError>;

    /// Graceful shutdown — calls `on_control(Shutdown)` then
    /// waits for the daemon's main loop to exit cleanly.
    pub async fn graceful_shutdown(self, grace: Duration) -> Result<(), SdkError>;
}

/// Read-only view of the cluster context the daemon can observe.
/// Snapshotted at construction; refresh via `runtime.snapshot()`
/// if the daemon needs fresher data.
pub struct MetadataView {
    pub node_id: NodeId,
    pub daemon_id: u64,
    pub maintenance_state: MaintenanceState,
    pub peers: BTreeMap<NodeId, PeerSnapshot>,
}
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

### 2. Python SDK (pyo3)

Lives in `bindings/python/src/meshos.rs` + `bindings/python/python/net/meshos.py`. Sync-first (matches MeshDB's precedent — Python is sync by default; async surfaces land in a follow-up).

```python
# bindings/python/python/net/meshos.py
from net._net import MeshOsHandle, DaemonControl, DaemonHealth

class MeshOsDaemon:
    """Implement this to be supervised by MeshOS."""
    def name(self) -> str: ...
    def process(self, event: bytes) -> list[bytes]:
        """Process one inbound causal event, return zero or more outputs."""
        return []

    # Optional methods — default impls match the Rust defaults
    def health(self) -> DaemonHealth: return DaemonHealth.Healthy
    def saturation(self) -> float: return 0.0
    def on_control(self, ev: DaemonControl) -> None: pass
    def snapshot(self) -> bytes | None: return None
    def restore(self, state: bytes) -> None: pass
    def required_capabilities(self) -> CapabilitySet: return CapabilitySet()
    def optional_capabilities(self) -> CapabilitySet: return CapabilitySet()

# Registration entry point — sync by default
def register(daemon: MeshOsDaemon, keypair: EntityKeypair) -> MeshOsHandle:
    """Register a daemon with MeshOS. Returns the handle that owns
    its lifecycle. Use as a context manager for graceful shutdown."""
    ...
```

```python
# Usage
import net.meshos as meshos

class TelemetryDaemon(meshos.MeshOsDaemon):
    def name(self): return "telemetry"
    def process(self, event):
        # ... handle event, return outputs ...
        return [b"out"]
    def health(self):
        return meshos.DaemonHealth.Healthy if self.queue_depth < 1000 else meshos.DaemonHealth.Degraded(reason="queue depth")

with meshos.register(TelemetryDaemon(), kp) as handle:
    while True:
        ev = handle.next_control()  # blocks until next control event
        if isinstance(ev, meshos.DaemonControl.Shutdown):
            break  # context manager drains on exit
```

**`next_control()`** is sync-blocking (per Python convention). An `async def anext_control()` lands when a consumer asks for the pyo3-asyncio shape.

**Trait routing.** pyo3 instantiates a `PyMeshOsDaemon` wrapper struct holding the Python object; the Rust side implements `MeshDaemon` by calling back into Python via `Python::with_gil` for each trait method. GIL is acquired once per call; control-event delivery uses a per-daemon mpsc that the SDK fed into the wrapper.

### 3. Node / TypeScript SDK (napi-rs)

Lives in `bindings/node/src/meshos.rs` + `bindings/node/index.d.ts`. AsyncIterable for control events; the rest sync.

```typescript
// Daemon implementor
export interface MeshOsDaemon {
  name(): string;
  process(event: Buffer): Buffer[];

  // Optional methods — defaults match Rust
  health?(): DaemonHealth;
  saturation?(): number;
  onControl?(ev: DaemonControl): void;
  snapshot?(): Buffer | undefined;
  restore?(state: Buffer): void;
  requiredCapabilities?(): CapabilitySet;
  optionalCapabilities?(): CapabilitySet;
}

// Registration
export function register(
  daemon: MeshOsDaemon,
  keypair: EntityKeypair,
): Promise<MeshOsHandle>;

export class MeshOsHandle {
  readonly daemonId: bigint;
  readonly metadata: MetadataView;
  publishCapabilities(caps: CapabilitySet): Promise<void>;
  controlEvents(): AsyncIterable<DaemonControl>;
  gracefulShutdown(graceMs: number): Promise<void>;
}
```

```typescript
// Usage
import { register, DaemonHealth, DaemonControl } from '@ai2070/net/meshos';

const daemon = {
  name() { return 'telemetry'; },
  process(ev: Buffer): Buffer[] { return [Buffer.from('out')]; },
  health() {
    return this.queueDepth < 1000
      ? DaemonHealth.healthy()
      : DaemonHealth.degraded({ reason: 'queue depth' });
  },
};

const handle = await register(daemon, kp);
for await (const ev of handle.controlEvents()) {
  if (ev.kind === 'Shutdown') break;
}
await handle.gracefulShutdown(5000);
```

**AsyncIterable** matches the MeshDB Node binding's `for await` ergonomics — same TS shim pattern that adds `Symbol.asyncIterator` over a raw `next()` method.

### 4. Go SDK (cgo)

Lives in `bindings/go/meshos-ffi/` (the cdylib) + `bindings/go/go/meshos/` (the Go wrapper). Channel-based control events; sync everything else.

```go
// Daemon implementor
type MeshOsDaemon interface {
    Name() string
    Process(event []byte) ([][]byte, error)

    // Optional methods — Go interfaces don't have defaults; the
    // SDK wraps + provides defaults via a default-impl base.
    Health() DaemonHealth
    Saturation() float32
    OnControl(ev DaemonControl)
    Snapshot() ([]byte, error)
    Restore(state []byte) error
    RequiredCapabilities() CapabilitySet
    OptionalCapabilities() CapabilitySet
}

// Convenience: embed to get defaults for everything except Name+Process.
type DefaultDaemon struct{}
func (DefaultDaemon) Health() DaemonHealth { return DaemonHealth{Kind: HealthHealthy} }
func (DefaultDaemon) Saturation() float32 { return 0 }
// ... etc

// Registration
func Register(daemon MeshOsDaemon, kp *EntityKeypair) (*MeshOsHandle, error)

// Handle
type MeshOsHandle struct { /* ... */ }
func (h *MeshOsHandle) DaemonID() uint64
func (h *MeshOsHandle) Metadata() *MetadataView
func (h *MeshOsHandle) ControlEvents() <-chan DaemonControl
func (h *MeshOsHandle) PublishCapabilities(caps CapabilitySet) error
func (h *MeshOsHandle) GracefulShutdown(ctx context.Context) error
```

```go
// Usage
type Telemetry struct {
    meshos.DefaultDaemon
    queueDepth int
}
func (t *Telemetry) Name() string { return "telemetry" }
func (t *Telemetry) Process(ev []byte) ([][]byte, error) {
    return [][]byte{[]byte("out")}, nil
}
func (t *Telemetry) Health() meshos.DaemonHealth {
    if t.queueDepth < 1000 { return meshos.DaemonHealth{Kind: meshos.HealthHealthy} }
    return meshos.DaemonHealth{Kind: meshos.HealthDegraded, Reason: "queue depth"}
}

handle, err := meshos.Register(&Telemetry{}, kp)
for ev := range handle.ControlEvents() {
    if ev.Kind == meshos.ControlShutdown { break }
}
handle.GracefulShutdown(ctx)
```

**Pumping goroutine.** The cdylib spawns a per-daemon goroutine that pumps control events from the Rust side over a `chan DaemonControl`. Caller selects on the channel + `ctx.Done()` per the standard Go cancellation idiom (lifted from `MeshDBQuery.ExecuteContext`).

### 5. C SDK (raw FFI)

Lives in `bindings/go/meshos-ffi/include/net_meshos.h` (shared with Go) + the meshos-ffi cdylib. Function-pointer-table for daemon callbacks; the consumer manages memory.

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

// Lifecycle
NetMeshOsHandle* net_meshos_register(const NetMeshOsDaemonVtable* vt, void* ctx,
                                      const NetEntityKeypair* kp);
void net_meshos_unregister(NetMeshOsHandle*);

// Control events — caller polls
int net_meshos_next_control(NetMeshOsHandle*, NetDaemonControl* out, uint64_t timeout_ms);

// Capabilities
int net_meshos_publish_capabilities(NetMeshOsHandle*, const NetCapabilitySet*);

// Metadata read-only
const NetMetadataView* net_meshos_metadata(NetMeshOsHandle*);

// Graceful shutdown
int net_meshos_graceful_shutdown(NetMeshOsHandle*, uint64_t grace_ms);

// Last-error surface (matches MeshDB FFI pattern)
const char* net_meshos_last_error_message(void);
const char* net_meshos_last_error_kind(void);
void net_meshos_clear_last_error(void);
```

**Patterns lifted from MeshDB FFI:** `ffi_guard!` macro wrapping every entry point for `catch_unwind`; thread-local `LAST_ERROR_*` surface; `<inttypes.h>` `PRIx64` / `PRIu64` for example code.

### 6. Wire / FFI contracts (shared across bindings)

**`DaemonControl` serialization.** Postcard for the cross-binding wire; each language decodes to its native shape (`DaemonControl::Shutdown { grace_period_ms }` in Rust, `DaemonControl.Shutdown(grace_period_ms=5000)` in Python, `{ kind: 'Shutdown', gracePeriodMs: 5000n }` in TS, etc.). The `#[non_exhaustive]` attribute on `DaemonControl` already in `compute::daemon` means new variants don't break older language wrappers — they observe `"unknown"` kind and continue.

**`CapabilitySet` serialization.** Reuses the existing capability serialization (from `MESHDB_PLAN`'s precedent). Python passes Python dicts; Node passes JS objects; Go passes structs; C passes a typed struct.

**Snapshot / restore.** Opaque bytes. The daemon owns the encoding; MeshOS treats the snapshot as a blob. Postcard for Rust daemons by convention; other languages pick their own.

**Error kinds.** Re-use the `<<meshdb-kind:KIND>>MSG` discriminator format from MeshDB FFI for cross-language error parsing. SDK errors land as kinds like `register_failed`, `daemon_not_found`, `capability_publish_failed`, etc.

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

- **Phase 1 — Rust SDK.** Re-export module under `src/adapter/net/sdk/meshos/`. `MeshOsDaemonHandle` + `daemon_main!` macro + integration tests against `MeshOsRuntime` with `LoggingDispatcher`. The canonical surface every other binding mirrors.
- **Phase 2 — Python SDK.** pyo3 wrapper. `class MeshOsDaemon` Python protocol + `net.meshos.register(...)`. Sync control-event delivery. README walkthrough; pytest integration tests.
- **Phase 3 — Node / TypeScript SDK.** napi-rs wrapper. AsyncIterable control events; TS shim layered on the `next_control()` napi method. README + vitest integration tests.
- **Phase 4 — Go SDK.** cgo via the meshos-ffi cdylib + Go wrapper at `bindings/go/go/meshos/`. Channel-based control events; `context.Context` for graceful shutdown. README + Go test fixtures.
- **Phase 5 — C SDK.** Vtable-based daemon registration + last-error surface. Header at `include/net_meshos.h`; runnable `examples/meshos.c`. Phase 5 is the smallest — it's mostly the cdylib's C-export surface that Phases 3–4 already require.

Phases 2–5 land independently of each other; only Phase 1 (Rust) is a hard prereq. Per-language slices can ship partial surface — e.g., Python ships register + control-event receive in slice 1, capability sync + snapshot in slice 2 — as long as the slice list converges on the full daemon-contract surface before declaring "v1 done."

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
