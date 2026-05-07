# Daemon runtime readiness — factory registration before migrations land

## Context

[`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) § *Factory registration semantics* acknowledges the problem in one sentence:

> On restart the runtime has no factory table until the user re-registers. If migration is in-flight against a restart, the target errors `FactoryNotFound`. Document: factories must be registered before the user starts accepting migrations.

"Document" is not an API. The shape of the API the plan specifies *creates* the race rather than preventing it. Concretely:

```rust
// Today's intended surface (from SDK_COMPUTE_SURFACE_PLAN.md § Stage 1)
let mesh = Mesh::builder(...).build().await?;
mesh.start();                    // migration subprotocol goes live
let rt = DaemonRuntime::new(mesh);
rt.register_factory("echo", ...);  // ← anything that arrived in the
                                   //   intervening window hard-fails
```

Two failure modes that look identical from the caller's view but aren't:

1. **"I don't know this kind."** User never registered `"echo"` because they genuinely don't host that daemon type. Right answer: terminal `FactoryNotFound`, no retry.
2. **"I haven't finished starting."** User *is going to* register `"echo"` any moment, but hasn't yet. Right answer: "come back later," source retries with bounded backoff.

Today both come back as `MigrationFailed { reason: String }`, which the source orchestrator treats as terminal. A legitimate cold-boot migration from a peer is indistinguishable from a mis-targeted one.

Worse, `MigrationMessage::MigrationFailed` carries a free-form `reason: String`. The source pattern-matches on reason strings to decide what to do — today it doesn't pattern-match at all, just surfaces the string to the user. Adding retry logic by string-matching substrings is the wrong tool; this plan introduces structured reason codes that the source can dispatch on.

## Scope

**In scope**

- `DaemonRuntime` owns the mesh lifecycle when compute is in use; separate "register factories" and "accept migrations" phases.
- Structured reason codes on `MigrationFailed` — at minimum `NotReady` (retriable) vs `FactoryNotFound` (terminal).
- Source-side bounded retry on `NotReady`.
- Non-daemon mesh users: a mesh that never constructs a `DaemonRuntime` rejects migrations cleanly with a distinct reason code, so confused orchestrators give up quickly instead of retrying indefinitely.

**Out of scope**

- **Persistent factory tables across restarts.** Factories are still user-registered on every boot — that's a deliberate simplicity choice from the compute plan. This plan fixes the *timing* of registration, not the *durability*.
- **Lazy factory discovery.** "Look up a factory by kind from somewhere" (filesystem, registry service, etc.) is a larger topic; v1 sticks with explicit in-process registration.
- **Warm-restart daemon resurrection.** If a daemon was running at the moment of a source-node restart, restoring it on restart requires persistent snapshot storage + replay — out of scope here.
- **Dynamic factory unregistration while in-flight migrations exist.** Deferred; the only lifecycle transitions are "add" and "whole-runtime shutdown."

## Design invariants

1. **`mesh.start()` never happens with a `DaemonRuntime` attached but no factories registered.** The API shape makes it impossible: the runtime consumes an unstarted mesh, and the start happens through the runtime.
2. **A not-ready target retries, a wrong-kind target gives up.** Two distinct reason codes on the wire. Source dispatches, never string-matches.
3. **Retry is bounded.** The source does not wait forever for a target that may never boot. Default 30 s total, exponential backoff, final failure is still `MigrationFailed` with a transparent "gave up after N retries" reason.
4. **Non-compute nodes fail fast.** A mesh that doesn't run daemons at all responds with `ComputeNotSupported` — a terminal code distinct from `NotReady`, so the source gives up immediately rather than burning its retry budget.

## Current failure-mode trace

```text
t=0    Node B boots, mesh.start() runs. SUBPROTOCOL_MIGRATION (0x0500) handler is live.
       DaemonRuntime has not been constructed yet; migration_target is None.
t=1    Node A (source) sends TakeSnapshot/SnapshotReady for daemon "echo".
t=2    B's handler: migration_target is None. Message silently dropped.
       [or, if migration_target is Some but no factory is registered:]
       factory lookup returns None → emits MigrationFailed { reason: "factory not found: echo" }
t=3    A's orchestrator receives MigrationFailed. Emits MigrationPhase::Failed to the phase
       stream. User sees it as "migration failed", no distinction between "target is still
       initializing" and "target will never accept this daemon kind."
t=4    A's migration state is cleaned up. If B was about to become ready 500 ms later,
       no mechanism retries; the user must manually kick off another migration.
```

## Surface

### Structured `MigrationFailureReason` on the wire

Replace the free-form `reason: String` on `MigrationMessage::MigrationFailed` with a code + optional detail:

```rust
pub enum MigrationMessage {
    // ... existing variants ...
    MigrationFailed {
        daemon_origin: u32,
        reason: MigrationFailureReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationFailureReason {
    /// Target runtime exists but hasn't called `start()` yet. Retriable.
    NotReady,
    /// Target has no factory registered for the daemon's kind. Terminal.
    FactoryNotFound { kind: String },
    /// Target doesn't run a compute runtime at all. Terminal.
    ComputeNotSupported,
    /// Generic snapshot / restore failure. Terminal.
    StateFailed(String),
    /// Already migrating the same origin; caller should not retry. Terminal.
    AlreadyMigrating,
    /// Identity envelope failure (see DAEMON_IDENTITY_MIGRATION_PLAN).
    IdentityTransportFailed(String),
    /// Source gave up after exhausting retries on NotReady. Terminal.
    NotReadyTimeout { attempts: u8 },
}
```

Classification:

| Reason | Retriable | Notes |
|---|---|---|
| `NotReady` | yes | Target booting, will be ready soon |
| `FactoryNotFound { kind }` | no | Target config mismatch; retry won't help |
| `ComputeNotSupported` | no | Target is not a compute node |
| `StateFailed(_)` | no | Something's corrupted; the user should look |
| `AlreadyMigrating` | no | Caller error; don't compound it |
| `IdentityTransportFailed(_)` | no | Seal/attestation failure; see identity plan |
| `NotReadyTimeout` | no | Retry budget already spent |

Wire format: the tag byte stays `MSG_FAILED`. After the `daemon_origin` (u32), encode `code: u16` + variant-specific payload. This is a **minor bump** of the migration subprotocol; pre-fix nodes won't decode the new shape, so rolling-upgrade ordering matters. Call out in the migration subprotocol doc.

### `DaemonRuntime` lifecycle — two explicit phases

```rust
pub struct DaemonRuntime {
    inner: Arc<DaemonRuntimeInner>,
    state: AtomicU8,     // 0 = Registering, 1 = Ready, 2 = ShuttingDown
}

impl DaemonRuntime {
    /// Attach a runtime to an **unstarted** mesh. The runtime takes
    /// ownership — starting the mesh happens via `Self::start`.
    /// Migrations inbound before `start()` completes get `NotReady`.
    pub fn new(mesh: Mesh) -> Self;

    /// Register a factory. Valid in both Registering and Ready states —
    /// the Ready transition does not freeze the factory table (a user
    /// may discover a new kind at runtime and register it on the fly).
    pub fn register_factory(&self, kind: &str, factory: F) -> Result<(), DaemonError>;

    /// Promote to Ready: start the underlying mesh, wire the migration
    /// handler against the current factory table, and begin accepting
    /// inbound migrations. Idempotent; second call is a no-op.
    pub async fn start(&self) -> Result<(), DaemonError>;

    /// Reverse `start`. Stops the mesh, completes in-flight migrations
    /// if any, rejects new ones. After shutdown the runtime cannot be
    /// re-started — construct a new one.
    pub async fn shutdown(&self) -> Result<(), DaemonError>;

    /// Readiness snapshot for tests + operators.
    pub fn is_ready(&self) -> bool;
}
```

Enforced lifecycle:

```text
Registering  ─── start() ───▶  Ready  ─── shutdown() ───▶  ShuttingDown
     │                           │
     │                           └── register_factory() still allowed
     │                               (new kinds can appear at runtime)
     │
     └── register_factory() normal path
         spawn / start_migration refused with DaemonError::NotReady
```

Behaviour of inbound migration messages indexed by runtime state:

| State | Migration inbound | Response |
|---|---|---|
| No runtime attached to mesh | (ignored subprotocol handler) | `ComputeNotSupported` |
| `Registering` | factory exists for kind | Respond `NotReady` (runtime not yet accepting) |
| `Registering` | factory doesn't exist | Respond `NotReady` (don't leak the lookup until Ready) |
| `Ready` | factory exists | Proceed normally |
| `Ready` | factory doesn't exist | Respond `FactoryNotFound { kind }` |
| `ShuttingDown` | any | Respond `NotReady` (orchestrator will retry against whoever else owns the target) |

Rationale for the `Registering + missing factory → NotReady` choice (not `FactoryNotFound`): the user may be about to register that kind. We can't tell the difference in the Registering state, so conservative retry is safer than terminal reject.

### `MigrationOpts::retry_not_ready`

```rust
pub struct MigrationOpts {
    // ... existing fields from identity plan ...

    /// Retry budget when the target responds with `NotReady`. `None`
    /// disables retry (migration fails immediately on the first
    /// `NotReady`). `Some(d)` gives up after the total elapsed time
    /// exceeds `d`. Default: `Some(Duration::from_secs(30))`.
    pub retry_not_ready: Option<Duration>,
}
```

Source-side retry state machine (lives in `MigrationOrchestrator`):

```text
on receive MigrationFailed { reason: NotReady }:
  if retry_budget_exhausted:
    emit MigrationPhase::Failed { NotReadyTimeout { attempts: n } }
  else:
    sleep(backoff(n))    // 500ms, 1s, 2s, 4s, 8s, capped at 16s
    re-send the message that triggered the failure
    attempts += 1

on receive MigrationFailed { reason: !NotReady }:
  emit MigrationPhase::Failed { <reason> } — no retry
```

Backoff cap at 16 s keeps the retry from sitting idle for minutes near the budget boundary. With default 30 s budget, expected retry count is 4–5.

### Subprotocol handler when runtime is `None`

Today `MeshNode` would need to know about compute at all to respond with `ComputeNotSupported`. Two options:

- **A)** `MeshNode` ships a default migration handler that always returns `ComputeNotSupported`. `DaemonRuntime::start()` replaces it with the real one. Pre-attach / non-compute users get the default.
- **B)** `MeshNode` has no migration handler at all until a runtime attaches. Messages are dropped. Source orchestrator sees no response, times out.

Go with **A** — a short, typed error is better than a silent drop, which is indistinguishable from a network partition.

## Staged rollout

### Stage 1 — `MigrationFailureReason` enum + wire encoding (~2 d)

- Replace `reason: String` with `reason: MigrationFailureReason`.
- Wire encoding: `MSG_FAILED | daemon_origin:u32 | code:u16 | variant_payload`.
- Variant payloads:
  - `FactoryNotFound { kind }`: `u16` length + kind bytes.
  - `StateFailed / IdentityTransportFailed`: `u16` length + message bytes.
  - `NotReadyTimeout { attempts }`: `u8`.
  - Others: zero bytes.
- Round-trip tests per variant.
- **Breaking wire change.** Migration-subprotocol version bumped; both sides of a rolling deploy must be on the new code before in-flight migrations are safe.

### Stage 2 — `DaemonRuntime` state machine (~2 d)

- `AtomicU8` state field.
- `new` / `register_factory` / `start` / `shutdown` / `is_ready` implementations.
- `new` consumes `Mesh` unstarted. `start` invokes `mesh.start()` internally.
- Unit tests: state transitions, `register_factory` works in both states, double-`start` is idempotent.

### Stage 3 — Subprotocol handler wired to runtime state (~1 d)

- Inbound migration dispatch checks `AtomicU8` state first.
- `Registering` / `ShuttingDown` → `NotReady`.
- `Ready` + missing factory → `FactoryNotFound`.
- Default handler (no runtime) → `ComputeNotSupported`.
- Unit: inject each state, assert response code.

### Stage 4 — Source-side retry (~2 d)

- `MigrationOrchestrator` tracks attempt count + next-backoff per daemon-origin.
- On `MigrationFailed::NotReady`, schedule re-send after backoff.
- On other reasons, terminate as today.
- On budget exhaustion, emit `NotReadyTimeout`.
- `MigrationOpts::retry_not_ready` plumbed through `start_migration_with`.
- Integration: two-node, target runtime in `Registering`, source issues migration, target calls `start()` after 2 s, migration completes.

### Stage 5 — Per-SDK lifecycle + retry surface (~0.5 d per SDK, ×4)

- Rust SDK: `DaemonRuntime::new(mesh)` / `start().await` / `is_ready()` / `MigrationOpts.retry_not_ready`.
- NAPI + TS: `rt.start(): Promise<void>`, `rt.isReady(): boolean`, `opts.retryNotReadyMs`.
- PyO3 + Python: `await rt.start()`, `rt.is_ready()`, `retry_not_ready_s=30`.
- Go: `rt.Start(ctx) error`, `rt.IsReady() bool`, `MigrationOpts.RetryNotReady time.Duration`.
- Per-binding test: construct → register → start order assertions + NotReady retry smoke.

### Stage 6 — Documentation sweep (~0.5 d)

- Update `SDK_COMPUTE_SURFACE_PLAN.md` § *Factory registration semantics* to point at this plan.
- README section on "Daemon lifecycle": explain Registering vs Ready, the `start()` fence, and why it exists.
- Migration troubleshooting: "`NotReady` means the target is still booting; `FactoryNotFound` means the target doesn't host this daemon kind."

## Test plan

Integration tests in `tests/daemon_runtime_readiness.rs`:

- `runtime_registering_rejects_spawn_with_not_ready` — caller tries to spawn without calling `start()`, gets `DaemonError::NotReady`.
- `migration_retries_while_target_is_booting` — target stays in `Registering` for 2 s after source fires `start_migration`; source retries twice with backoff; third attempt lands after target.start(); migration completes.
- `migration_gives_up_after_retry_budget` — target never starts; source exhausts 30 s budget; phase stream emits `Failed { NotReadyTimeout { attempts: 5 } }`.
- `factory_not_found_is_terminal_no_retry` — target.start()ed, `kind="echo"` not registered; source gets `FactoryNotFound { kind: "echo" }` immediately; no retry attempts.
- `compute_not_supported_from_bare_mesh` — peer node is a plain `Mesh`, no runtime; source migration targets it; response is `ComputeNotSupported` on the first attempt; no retry.
- `register_factory_after_start_is_effective` — start()ed, then register a new kind, migrate that kind; completes.
- `shutdown_mid_migration_rejects_new_inbound` — shutdown called mid-way; subsequent migration attempts get `NotReady`, in-flight migration completes.

## Critical files

```
net/crates/net/src/adapter/net/compute/migration.rs       +MigrationFailureReason enum
net/crates/net/src/adapter/net/compute/orchestrator.rs    wire encode/decode swap,
                                                           +retry state machine,
                                                           +backoff scheduler
net/crates/net/src/adapter/net/compute/registry.rs        +AtomicU8 state,
                                                           +is_ready accessor
net/crates/net/src/adapter/net/mesh.rs                    default migration handler
                                                           that responds
                                                           ComputeNotSupported
net/crates/net/sdk/src/compute.rs                         DaemonRuntime::start / shutdown
                                                           / is_ready;
                                                           MigrationOpts.retry_not_ready
net/crates/net/tests/daemon_runtime_readiness.rs          new integration file
```

No new subprotocol IDs. One incompatible bump to the existing migration subprotocol's wire format. One additive field on `MigrationOpts`. One new SDK lifecycle method (`start`) across all four SDKs.

## Risks

- **Wire bump on `MigrationFailed`.** Pre-fix and post-fix nodes can't interoperate on the failure path — a pre-fix sender encodes `reason: String`, a post-fix receiver expects `code + payload`. In-flight migrations at deploy time will see corrupted reason codes if the deploy isn't synchronous. Mitigation: migration is opt-in today (explicit `start_migration` call) and the SDK is pre-1.0; ship the bump with a version note. If cross-version tolerance becomes a hard requirement later, add a subprotocol version byte at the start of every `MigrationMessage` and negotiate during session open.
- **Silent-drop path if the default handler forgets `ComputeNotSupported`.** If `MeshNode` ever ships without the fallback handler (e.g. someone removes it thinking it's dead code), the source retries against a target that's just dropping messages — looks identical to a network partition, burns the whole retry budget on a guaranteed-to-fail target. Tests explicitly exercise the bare-mesh-no-runtime case to keep this wired.
- **Retry storm under pathological config.** If a user sets `retry_not_ready: Some(Duration::MAX)`, a target stuck in `Registering` forever leaks orchestrator state on the source. Cap the budget internally at 5 min; reject `Some(d)` with `d > 5 min` at `MigrationOpts` validation.
- **`register_factory` after Ready is allowed, but a migration that arrived between Ready and the registration would have seen `FactoryNotFound`.** That's the user's bug (they said "ready" before they were actually ready for every kind). Not a race we can close without re-introducing the original problem. Document: "mark ready after registering every kind you plan to host at boot; runtime registration is for kinds you genuinely don't know about at startup."
- **State-machine race between `start` and `shutdown`.** Two coincident callers: one calls `start`, another calls `shutdown`. `AtomicU8::compare_exchange` per transition; losing side gets `Err(AlreadyTransitioning)`.

## Sizing

| Stage | Effort |
|---|---|
| 1. `MigrationFailureReason` + wire encoding | 2 d |
| 2. `DaemonRuntime` state machine | 2 d |
| 3. Handler wired to state | 1 d |
| 4. Source-side retry | 2 d |
| 5. SDK lifecycle surface (×4) | 2 d |
| 6. Docs | 0.5 d |

Total: ~9.5 d core + bindings.

## Dependencies

- Independent from [`DAEMON_CHANNEL_REBIND_PLAN.md`](DAEMON_CHANNEL_REBIND_PLAN.md) and [`DAEMON_IDENTITY_MIGRATION_PLAN.md`](DAEMON_IDENTITY_MIGRATION_PLAN.md). Can land before, after, or in parallel with either.
- Land Stage 1 (wire bump) before Stages 5/6 of the compute-surface plan — the bindings should surface the typed reason from day one rather than being patched later.
- Depends on `SDK_COMPUTE_SURFACE_PLAN.md` Stages 1–2 (daemon runtime + migration baseline) — there's nothing to gate without them.

## Explicit follow-ups (not in this plan)

- **Persistent factory tables.** A daemon host that survives restart without user re-registration. Requires either a plugin model (dylib loading) or a compile-time registry. Out of scope until we have a user case.
- **Auto-boot of registered daemons on start.** Today `register_factory` just lists what's constructable; `spawn` is always explicit. A "register + auto-boot" API would reduce boot sequencing further but conflates the factory registry with a spawn schedule.
- **Cross-node factory directory.** "Who hosts `kind=X`?" — maps onto the capability index if daemons announce `kind:X` as a tag, then `start_migration_auto` already finds them. This plan does not gate that; it's orthogonal.
- **Gradual migration throttling.** If a node boots and hundreds of pending migrations resolve at once, the runtime could shed load by staying in `Registering` longer. Punt until we have a user hitting this.

## Open questions for review

- **`Registering + factory present → NotReady` vs `→ proceed`.** The plan chooses NotReady (conservative) — even if the factory exists in Registering, we don't start migrating until `start()` is called. The alternative is "as soon as the factory exists, a specific migration can proceed." Going with conservative because it makes the `start()` fence do what its name says.
- **Default retry budget of 30 s.** Long enough for a realistic cold-start, short enough that the user sees failures promptly in a misconfigured environment. Revisit after Stage 4 smoke tests — a slow-starting Python daemon with large Python imports may need more.
- **`retry_not_ready: None` vs `Some(0)` to disable retry.** Using `Option<Duration>` is idiomatic but asymmetric (`None` means no retry; `Some(d)` means budgeted retry). `Duration::ZERO` as "no retry" is error-prone. Staying with `Option`.
- **Handler-state read cost on the hot path.** Every inbound migration message does an `AtomicU8::load(Acquire)`. Single uncontended load is ~1 ns; negligible. Not a concern at migration rates (single-digit per hour at most).
- **Should `start` return a handle that can be `.await`ed for full readiness?** Today it's `async fn start(&self) -> Result`; returns when the mesh is up and the handler is wired. Alternative: return a `ReadinessHandle` the caller can pass elsewhere. Keeping it simple — `is_ready()` is enough.
