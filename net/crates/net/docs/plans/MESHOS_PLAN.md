## MeshOS — implementation plan

> The cluster-behavior engine. One canonical event loop per node that reconciles **desired** state (from Dataforts) against **actual** state (from RedEX folds), supervises daemons, enforces replica placement, applies admin intent (drain, cordon, maintenance), emits backpressure under churn, and folds the result into a behavior snapshot for Deck. Composes — not duplicates — the substrate primitives that already ship: `PlacementFilter`, `CapabilityIndex`, `RedexFold`, the `MeshDaemon` trait, the migration orchestrator, replication election. Companion to [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) (whose continuous-rebalance loop becomes Phase D-1 here) and [`MESHDB_PLAN.md`](MESHDB_PLAN.md) (which Deck queries to render the behavior snapshot). **Atomic Playboys release** per [`RELEASE_ROADMAP.md`](RELEASE_ROADMAP.md); follows MeshDB.

## Status

Design only. The features overview in [`MESHOS_FEATURES.md`](MESHOS_FEATURES.md) is the product brief; this doc is the implementation plan that turns it into shippable phases.

Activation gate: a workload that actually exercises the reconciliation loop end-to-end — Dataforts placing replicas continuously, drain operations driving real evacuations, Deck consuming the behavior snapshot to render the cluster jungle. MeshOS without those consumers is a state machine looking for events to process; with them it is the cluster's nervous system.

**Substrate prereqs** (all in code today):

- **`MeshDaemon` trait + `DaemonRegistry`** at `src/adapter/net/compute/{daemon.rs, registry.rs}`. Sync, WASM-compatible: `name()`, `requirements()`, `process(event)`, `snapshot()`, `restore()`. Health-check / saturation reporting are **absent today** — MeshOS extends the trait in Phase B.
- **Migration orchestrator** at `src/adapter/net/compute/orchestrator.rs`. Six-phase handoff (`TakeSnapshot → SnapshotReady → RestoreComplete → ReplayComplete → CutoverNotify → CleanupComplete`). MeshOS delegates the *mechanics* to the orchestrator; it owns only the *decisions*.
- **Replication election + heartbeat** at `src/adapter/net/redex/{replication_election.rs, replication_heartbeat.rs}`. Leader election + `HeartbeatTracker` for replica consensus. MeshOS consumes the leader signal to decide whose local reconciler acts on which replica.
- **`PlacementFilter` + `Artifact`** at `src/adapter/net/behavior/placement.rs`. 5-axis scoring (scope × proximity × intent × colocation × resource). MeshOS scores existing placements continuously; `PlacementFilter` was designed for one-shot placement, MeshOS recomputes it.
- **`CapabilityIndex`** at `src/adapter/net/behavior/capability.rs`. `find_nodes_matching(filter)`, `all_nodes()`, tag walks. MeshOS reads it to discover candidates + watches its `mutation_version` for state drift.
- **`RedexFold<State>`** at `src/adapter/net/redex/fold.rs`. The reconciler reads desired state via folds; emits action chains; folds the outcome back into the behavior snapshot. MeshOS does not invent a new state layer.
- **`CortexAdapter`** at `src/adapter/net/cortex/adapter.rs`. `watch` / `snapshot_and_watch` / `changes_with_lag()` — already the canonical Net → folded-state bridge. Phase F's Deck snapshot is one more `RedexFold` over MeshOS's own action stream.
- **MeshDB** at `src/adapter/net/behavior/meshdb/`. Deck queries the behavior snapshot via MeshDB's federated executor. MeshOS does not duplicate the query layer.

**Substrate gaps** that MeshOS introduces:

- **No per-node canonical event loop today.** Every behavior subsystem (`ReplicationCoordinator`, `CortexAdapter`, `FederatedMeshQueryExecutor`) spawns its own `tokio` tasks. Phase A consolidates these into one `MeshOsLoop` per node — one stream → one reconcile → consistent actions. Existing subsystems become event *sources* into the loop, not independent reactors.
- **No daemon health / saturation surface.** Phase B extends `MeshDaemon` with optional `health()` + `saturation()` methods (default impls return `Healthy` / `0.0`) and adds a `MeshOsControl` enum that supervised daemons receive (graceful shutdown, drain start, drain finish).
- **No drain / cordon / maintenance metadata fields.** Phase D + E add `metadata.draining`, `metadata.drain_deadline`, `metadata.cordoned`, `metadata.maintenance_state` to the chain metadata surface. Drain signals propagate through RedEX, not RPC, so all nodes converge on identical interpretation.
- **No `MESH_SCHEDULER_PLAN.md` implementation.** That plan's `LocalScheduler` design becomes the body of Phase D-1 here — same per-daemon score tracker, same hysteresis / cooldown / drain integration, but driven by the MeshOS event loop rather than its own task tree.

## Frame

The substrate ships independent reactors. RedEX folds run wherever a `CortexAdapter` consumer asks for them. The replication coordinator spawns per-channel heartbeat tasks. The federated query executor spawns per-call response pumps. Each reactor is correct in isolation; nothing wires them into a single coherent observation of *what the node is doing right now*.

**MeshOS is that single observation point.** One event loop per node consumes:

- replica updates (from `ReplicationCoordinator`)
- daemon lifecycle signals (from `DaemonRegistry`)
- RTT samples (from the proximity graph)
- node health (from heartbeat)
- admin actions (from the admin chain — drain, cordon, maintenance)
- blob announcements (from Dataforts)
- placement intent (from Dataforts)

It folds the union of these into a `MeshOsState` snapshot, compares against the desired-state fold (also driven by RedEX events from Dataforts), and emits a minimal action list per tick: `start_daemon` / `stop_daemon` / `migrate_blob` / `pull_replica` / `reduce_heat` / `mark_avoid` / `apply_backoff`. Actions delegate to existing subsystems (orchestrator handles migrations; the daemon registry handles supervision; Dataforts owns blob movement); MeshOS owns only the *deciding when and what*.

**Why one loop matters.** Multiple reactors compounding their own retry / backoff / placement decisions race each other. Two subsystems each independently deciding "this replica is unhealthy; pull it" produce double-pulls. Two subsystems each independently deciding "this node is draining; migrate everything off" produce thundering herds. The single-loop posture lets MeshOS apply *one* coherent backpressure layer over *all* outbound actions — global pull cooldown, crash-loop gating, replica stabilization windows — without each subsystem needing to re-implement the same throttle.

**The architectural posture.** Decentralized — no central coordinator. Every node runs its own `MeshOsLoop`. The chain-driven admin events (drain, cordon, maintenance) replicate via RedEX, so all nodes converge on the same interpretation. The single coordinator that *does* exist for replica decisions (the leader elected by `replication_election`) feeds in as one event source among many; MeshOS does not race the election.

## Why this exists

Three reasons this needs a written plan, not just "we'll layer it on when Dataforts is producing real placement decisions":

1. **The event ordering surface is correctness-load-bearing.** A replica announcement arriving before its capability tag, a daemon health drop arriving before its restart-cooldown timer, an admin drain event arriving before the chain commit confirms — these interleavings determine whether MeshOS does the right thing. Designing them in isolation produces bugs you only find under partition. Designing them together produces a canonical ordering the reconciler can rely on.

2. **Maintenance nodes are operationally critical.** Cluster upgrades, daemon upgrades, node replacement, key rotation — every operator workflow that touches a running cluster needs maintenance mode to be safe. Implementing it as an afterthought (drain a node by hand-tagging it, hope nothing else races the migration) produces incident reports. Implementing it as a first-class state machine — Active → EnteringMaintenance → Maintenance → ExitingMaintenance → Recovery, all transitions chain-driven, all idempotent — produces a substrate operators can trust.

3. **Deck depends on the snapshot shape.** The cluster-jungle visualization Deck renders is exactly the fold MeshOS emits. Locking the snapshot shape (current actions, pending actions, recent failures, drift, heat, locality, daemon status, placement stability) before Deck integration starts is much cheaper than negotiating it after both sides are coded. The snapshot is part of the public surface, not an implementation detail.

## What ships

Seven interlocking pieces, in dependency order:

1. **The `MeshOsLoop` — one event loop per node.** Single-stream reactor consuming the union of event sources. Owns the canonical ordering. Existing subsystems become sources, not reactors.
2. **Daemon supervision.** Extends `MeshDaemon` with optional health / saturation reporting. Adds `MeshOsControl` events delivered to daemons (graceful shutdown, drain-start, drain-finish). Implements exponential-backoff restart + crash-loop gating.
3. **Replica enforcement.** Continuous replica-count compliance against the desired-state fold from Dataforts. Greedy pulls, anti-entropy, stale-replica de-duplication, safe-rejoin handling.
4. **Locality awareness + admin event handling.** RTT samples flow into the placement override path; admin events (drain, cordon, uncordon, clear-avoid, invalidate-placement) flow through RedEX as first-class chain commits. Replica scheduler — the body of `MESH_SCHEDULER_PLAN.md` — lands here as Phase D-1.
5. **Maintenance nodes.** The Active → EnteringMaintenance → Maintenance → ExitingMaintenance → DrainFailed → Recovery state machine. All transitions idempotent + chain-driven. Replica freeze, daemon drain, blob safety, admin surface unlocked, controlled exit with avoid-list timeout.
6. **Behavior snapshot fold for Deck.** A `RedexFold<MeshOsSnapshot>` over the MeshOS action chain. Queryable via MeshDB. Snapshot shape: current actions, pending actions, recent failures, drift indicators, heat levels, locality map, daemon health, placement stability.
7. **Safety & backpressure.** Global pull cooldown, crash-loop gating, replica stabilization windows, drain rate-limiting (N migrations/sec/zone). Applied as one layer over the loop's outbound actions — every action path passes through the same backpressure check.

What this doc does NOT ship (deferred even from MeshOS):

- **User-job scheduling.** MeshOS supervises *cluster-resident* daemons (replication coordinators, capability emitters, blob movers). User jobs — submit-a-binary, run-this-WASM-module, schedule-this-task — are a different problem with different ergonomics. The features doc names this as a non-goal; the plan honors it.
- **Remote execution / RPC fan-out.** MeshOS does not invoke daemons over the wire. Daemon process events are local; cross-node coordination rides RedEX and the capability index.
- **Workflow orchestration.** No DAG runner, no step-dependency graph, no retry-with-conditional-branching. Out of scope; downstream tools can build it on top.
- **Cross-language SDK at v1.** The features doc specifies Rust-only for the daemon SDK. Python / Node / Go bindings follow once the Rust surface stabilizes (same pattern as MeshDB).
- **Predictive scheduling / ML placement.** Same posture as `MESH_SCHEDULER_PLAN.md` — reactive only. Reactive loop tuned correctly outperforms a predictive layer trained on insufficient signal.

---

## Design

### 1. `MeshOsLoop` — the canonical event loop

Lives in `src/adapter/net/behavior/meshos/loop.rs` (new module — `src/adapter/net/behavior/meshos/`).

```rust
pub struct MeshOsLoop {
    /// Per-node identity + config.
    node_id: NodeId,
    config: Arc<MeshOsConfig>,

    /// Event sources — multiplex into one stream.
    events: tokio::sync::mpsc::Receiver<MeshOsEvent>,

    /// Folded actual state.
    actual: MeshOsState,

    /// Folded desired state (from Dataforts).
    desired: DesiredState,

    /// Outbound action queue — drained by the action executor.
    actions: VecDeque<Action>,

    /// Backpressure tracker.
    backpressure: BackpressureState,

    /// Subsystems MeshOS dispatches actions into.
    daemons: Arc<DaemonRegistry>,
    orchestrator: Arc<MigrationOrchestrator>,
    capability_index: Arc<CapabilityIndex>,
    mesh: Weak<MeshNode>,
}

pub enum MeshOsEvent {
    /// RedEX-side replica announcement / removal.
    ReplicaUpdate(ReplicaUpdate),
    /// Daemon lifecycle from `DaemonRegistry`.
    DaemonLifecycle(DaemonId, DaemonLifecycleSignal),
    /// New RTT sample from the proximity graph.
    RttSample { peer: NodeId, rtt: Duration },
    /// Heartbeat-derived node health change.
    NodeHealth { peer: NodeId, health: NodeHealth },
    /// Admin chain event (drain / cordon / maintenance / etc).
    AdminEvent(AdminEvent),
    /// Dataforts blob announcement.
    BlobAnnouncement(BlobAnnouncement),
    /// Dataforts placement intent update.
    PlacementIntent(PlacementIntent),
    /// Periodic tick — reconcile pass driven from a single timer.
    Tick,
}
```

**One stream, one ordering.** Events arrive on the single `mpsc::Receiver`; sources fan in via dedicated converters that translate subsystem-native signals into `MeshOsEvent`. The loop pops one event at a time, updates state, runs reconcile, and emits any actions the diff produces. **Tick events** are the only timer-driven input — the reconcile pass runs at most once per tick, even if many events arrive between ticks. This is the global rate-limiter.

**Tick cadence.** Heartbeat-aligned 500 ms by default (matches `MeshConfig::heartbeat_interval`). Configurable per node via `MeshOsConfig::tick_interval`. A reconcile pass at higher cadence would race the heartbeat tracker's own state updates; lower would lag drift detection.

**Reconcile shape.** Pure function: `reconcile(actual, desired) -> Vec<Action>`. The diff is the same shape as MeshDB's planner: walk the desired-state tree, compare against the actual-state tree, emit the minimal action list that closes the gap. Idempotent — replaying the same `(actual, desired)` produces the same actions.

**Action emission, not action execution.** The reconcile pass emits `Action` variants into `self.actions`; a separate action-executor task drains the queue at backpressure-controlled rate. This separation is load-bearing for backpressure (Phase G) and for testability — reconcile is a sync pure function easily unit-tested without async machinery.

### 2. Daemon supervision

Lives in `src/adapter/net/behavior/meshos/supervision.rs`.

**Extending `MeshDaemon`.** The trait gains optional methods with default impls:

```rust
pub trait MeshDaemon {
    /* existing required: name / requirements / process / snapshot / restore */

    /// Current health — default `Healthy`. Polled by MeshOS each tick.
    fn health(&self) -> DaemonHealth { DaemonHealth::Healthy }

    /// Saturation in `[0.0, 1.0]` — default 0.0. 1.0 == fully saturated.
    fn saturation(&self) -> f32 { 0.0 }

    /// Receive a MeshOS control event. Default: ignore.
    fn on_control(&mut self, _event: MeshOsControl) {}
}

pub enum DaemonHealth { Healthy, Degraded { reason: String }, Unhealthy }

pub enum MeshOsControl {
    /// Graceful shutdown — finish in-flight work, then exit.
    Shutdown { deadline: Instant },
    /// Drain start — stop accepting new work; in-flight work continues.
    DrainStart { deadline: Instant },
    /// Drain finished — fully evacuate now.
    DrainFinish,
    /// Cluster-wide backpressure — reduce optional work.
    BackpressureOn { level: f32 },
    BackpressureOff,
}
```

Adding optional methods with default impls preserves source compatibility for every existing daemon. WASM compatibility is preserved — the new methods are sync.

**Restart policy.** Crash-loop detection via a per-daemon `BackoffTracker`:

```rust
pub struct BackoffTracker {
    /// Restart timestamps, last N (N = 16 by default).
    restarts: VecDeque<Instant>,
    /// Current backoff window.
    backoff: Duration,
    /// Crash-loop threshold — N restarts within window triggers gating.
    crash_loop_threshold: u32,
    crash_loop_window: Duration,
}
```

Default: exponential backoff starts at 500 ms, doubles on each restart, caps at 60 s. Crash-loop gating: 5 restarts within 60 s flips the daemon to `MeshOsState::Daemons::CrashLooping`, which **stops further restart attempts** until either (a) operator intervention via admin event, or (b) the cooldown window elapses (default 5 minutes).

**Graceful shutdown.** When the reconcile loop emits `Action::StopDaemon { id, reason }`, the supervisor sends `MeshOsControl::Shutdown { deadline = now + grace_period }` and waits. If the daemon exits cleanly before the deadline, fine. If not, it's terminated and the failure is recorded in `RecentFailures` for Deck.

### 3. Replica enforcement

Lives in `src/adapter/net/behavior/meshos/replicas.rs`.

The reconcile pass compares desired replica count (from Dataforts placement fold) against actual replica count (from `ReplicationCoordinator` state). The diff produces:

```rust
pub enum ReplicaAction {
    /// This node should pull a replica it doesn't have.
    PullReplica { chain: ChainId, source: NodeId },
    /// This node holds a stale replica; drop it.
    DropReplica { chain: ChainId },
    /// Replica count is below desired — request another node host it.
    RequestPlacement { chain: ChainId, exclude: Vec<NodeId> },
    /// Replica count is above desired — pick the lowest-scored holder and ask it to drop.
    RequestEviction { chain: ChainId, victim: NodeId },
}
```

**Only the leader acts on `Request*` actions.** Per-replica, the leader (from `replication_election`) is the authority for placement decisions. Non-leader nodes that score the same artifact may *propose* but do not *act*. This mirrors the coordination posture in `MESH_SCHEDULER_PLAN.md` and avoids the multi-node race.

**Anti-entropy.** A periodic background sweep (every `replica_sweep_interval`, default 5 minutes) compares the local replica set against the capability index's view. Replicas the index claims this node holds but doesn't surface as a `RepairReplica` action; replicas this node holds that the index doesn't know about surface as `WithdrawReplica`.

**De-duplication on rejoin.** When a node rejoins after partition, the heartbeat path identifies replicas held by multiple nodes that the leader didn't sanction. The duplicate (the one with the staler tip) is evicted. The fresher copy stays.

### 4. Locality awareness + admin event handling

Lives in `src/adapter/net/behavior/meshos/{locality.rs, admin.rs}`. Phase D-1 (the continuous-rebalance loop) lives in `src/adapter/net/behavior/meshos/scheduler.rs` — the implementation body of `MESH_SCHEDULER_PLAN.md`, plumbed into the MeshOS loop instead of running as a standalone subsystem.

**Locality flow.** RTT samples from the proximity graph become `MeshOsEvent::RttSample`. The loop folds them into the actual-state `LocalityMap`; reconcile reads from it when scoring candidates. A degrading RTT to a holder of a hot replica produces a `MarkAvoid { peer, reason, ttl }` action that adds the peer to the local avoid list for the placement scorer. Avoid entries time out after `avoid_ttl` (default 5 minutes); permanent avoids are admin-event-driven.

**Admin event surface.** Admin events ride RedEX as commits on a per-cluster admin chain. The chain shape:

```rust
pub enum AdminEvent {
    EnterMaintenance { node: NodeId, deadline: Option<Instant>, by: OperatorId },
    ExitMaintenance { node: NodeId, by: OperatorId },
    Drain { node: NodeId, deadline: Instant, by: OperatorId },
    Uncordon { node: NodeId, by: OperatorId },
    Cordon { node: NodeId, by: OperatorId },
    RestartAllDaemons { node: NodeId, by: OperatorId },
    ClearAvoidList { node: NodeId, by: OperatorId },
    DropReplicas { node: NodeId, chains: Vec<ChainId>, by: OperatorId },
    InvalidatePlacement { node: NodeId, by: OperatorId },
}
```

The admin chain is **signed by an operator identity** that the channel-auth layer recognizes as authorized — this re-uses the existing channel-auth guards (`CHANNEL_AUTH_GUARD_PLAN.md`); MeshOS does not invent its own authorization. Unauthorized commits are rejected at the chain-commit layer, never reaching the reconcile pass.

**Convergence.** Because admin events ride RedEX, every node observes them in the same order. Two operators racing each other to `EnterMaintenance(node_x)` and `ExitMaintenance(node_x)` will have one event commit first; all nodes apply them in the committed order; the second is a no-op. No coordination required.

### 5. Maintenance nodes — the state machine

Lives in `src/adapter/net/behavior/meshos/maintenance.rs`.

State machine:

```rust
pub enum MaintenanceState {
    Active,
    EnteringMaintenance { since: Instant, deadline: Option<Instant> },
    Maintenance { since: Instant },
    ExitingMaintenance { since: Instant },
    DrainFailed { since: Instant, reason: String },
    Recovery { since: Instant },
}
```

Stored in chain metadata: `metadata.maintenance_state` on the node's capability set. Every node's MeshOS loop watches the metadata via the capability index; transitions are observed identically everywhere.

**Active → EnteringMaintenance.** Triggered by `AdminEvent::EnterMaintenance`. MeshOS immediately:

1. **Replica freeze.** No new replica placement targets this node — Dataforts respects `maintenance_state != Active` as "absent in scoring".
2. **Schedule existing replicas for migration.** Each replica produces a `PullReplica { source: this_node, target: best_candidate }` action on the *target's* loop (via admin chain), not this one. The target acts; this node observes.
3. **Daemon drain start.** Non-essential daemons receive `MeshOsControl::DrainStart { deadline }`.
4. **Health no longer fed into placement.** Even if heartbeat says healthy, scoring treats this node as absent.

**EnteringMaintenance → Maintenance.** Transition triggered when all replicas have migrated AND all non-essential daemons have exited. Loop checks both conditions each tick; commits the state transition when both true. If the deadline elapses with conditions unmet, transitions to `DrainFailed` instead.

**Maintenance.** Steady state. Operator commands run unimpeded — key rotation, identity changes, indexing fixes, storage repairs, config reloads. The node holds no replicas (or only essential ones — the chain metadata flags which classes are "essential" and survive maintenance). Blob cleanup runs once all replicas have migrated.

**Maintenance → ExitingMaintenance.** Triggered by `AdminEvent::ExitMaintenance`. The exit is **gated** — the node must:

1. Restart its daemons and have them report `DaemonHealth::Healthy`.
2. Emit a fresh `CapabilitySet::from_fork` so peers observe the updated metadata.
3. Wait for RTT stabilization (proximity graph re-converges with the rejoined node — typically 2-3 heartbeat intervals).

When all three complete, transitions to `Recovery`.

**Recovery.** The node is back in the proximity graph but **not yet eligible for hot placement**. A ramp-up window (default 5 minutes via `recovery_ramp_window`) keeps the node on the avoid list for new replica placement. After the window, transitions to `Active`. The ramp prevents thrash where a freshly rejoined node immediately gets hammered with replicas that then move again when the next ranking shift happens.

**DrainFailed.** Operator warning state. Replicas didn't migrate; daemons didn't drain; deadline elapsed. The node stays in `DrainFailed` until either operator intervention (`AdminEvent::ExitMaintenance { force = true }`) or the underlying condition resolves. Surfaces as a red banner on Deck.

### 6. Behavior snapshot fold for Deck

Lives in `src/adapter/net/behavior/meshos/snapshot.rs`.

A `RedexFold<MeshOsSnapshot>` over the MeshOS action chain:

```rust
pub struct MeshOsSnapshot {
    /// Currently-executing actions, keyed by action id.
    pub in_flight: HashMap<ActionId, InFlightAction>,
    /// Actions queued but not yet started (backpressure-gated).
    pub pending: VecDeque<PendingAction>,
    /// Recent failures with timestamps + reasons.
    pub recent_failures: VecDeque<FailureRecord>,
    /// Drift indicators — desired vs actual deltas the reconcile loop has identified.
    pub drift: HashMap<DriftKey, DriftValue>,
    /// Heat levels per blob, derived from access frequency in the Dataforts fold.
    pub heat: HashMap<BlobId, HeatLevel>,
    /// Locality map — RTT to each peer.
    pub locality: HashMap<NodeId, LocalityEntry>,
    /// Per-daemon health + saturation snapshot.
    pub daemons: HashMap<DaemonId, DaemonSnapshot>,
    /// Placement stability metric per chain.
    pub placement_stability: HashMap<ChainId, StabilityScore>,
    /// Each peer's maintenance state.
    pub maintenance: HashMap<NodeId, MaintenanceState>,
}
```

**Bounded sizes.** `recent_failures` is a ring buffer (default 256 entries). `drift` is cleared when the gap closes. `pending` is the action-executor queue, naturally bounded by backpressure. The snapshot is fixed-overhead under steady-state churn — no unbounded growth.

**Deck consumes via MeshDB.** Deck issues a `MeshQuery::Latest { origin: meshos_snapshot_chain }` against MeshDB; the federated executor routes to a node holding the fold; the row is the postcard-encoded snapshot. Deck deserializes, renders. No new wire protocol — re-uses `SUBPROTOCOL_MESHDB`.

**Per-node vs cluster-wide.** Each node folds its *own* MeshOS state. Deck assembles a cluster view by querying each node's snapshot in parallel via MeshDB's federated executor. The aggregation is at the query layer; MeshOS itself stays node-local.

### 7. Safety & backpressure

Lives in `src/adapter/net/behavior/meshos/backpressure.rs`.

Single backpressure layer over the action executor. Every action passes through `BackpressureState::admit(action) -> AdmissionResult` before execution:

```rust
pub enum AdmissionResult {
    /// Execute now.
    Admit,
    /// Defer — re-evaluate after `retry_after`.
    Defer { retry_after: Duration },
    /// Gate — daemon is in a crash-loop; do not retry until cooldown.
    Gate { cooldown_until: Instant },
}
```

**Throttles applied:**

- **Global pull cooldown.** No replica pull is admitted within `pull_cooldown` of the previous one (default 250 ms). Prevents stampede when many replicas drift simultaneously.
- **Drain rate-limit.** Migrations triggered by drain signals are capped at `drain_rate_per_zone` per second (default 10). Prevents melting the source zone.
- **Crash-loop gating.** A daemon in `MeshOsState::Daemons::CrashLooping` does not get restart actions admitted until its cooldown elapses.
- **Replica stabilization window.** After a replica migration completes, the migrated chain is excluded from further migration decisions for `replica_stabilization_window` (default 60 s). Avoids A→B→A bouncing.
- **Cluster-wide backpressure flag.** When the action queue depth exceeds `cluster_backpressure_threshold` (default 1000), MeshOS broadcasts `MeshOsControl::BackpressureOn { level }` to supervised daemons. They reduce optional work (cache warmup, background indexing, etc.). Cleared when the queue drains below `cluster_backpressure_release` (default 200).

**Why one layer.** Each throttle in isolation is easy to get right; the interaction surface is where bugs live. By funneling every action through one admit check, MeshOS guarantees the throttles compose — a drain-triggered migration doesn't bypass the pull cooldown; a crash-looping daemon doesn't dodge the gating just because its restart is admin-driven.

### 8. Rust SDK surface

Lives in `src/adapter/net/behavior/meshos/sdk.rs`. Re-exported through `meshos::*`.

```rust
// Daemon-side surface.
pub trait MeshDaemon { /* see Phase B */ }

pub struct DaemonHandle {
    pub fn register(reg: &DaemonRegistry, daemon: Arc<dyn MeshDaemon>) -> Self;
    pub fn report_health(&self, h: DaemonHealth);
    pub fn report_saturation(&self, s: f32);
    pub fn publish_capabilities(&self, caps: CapabilitySet);
    pub async fn receive_control(&self) -> MeshOsControl;
    pub async fn graceful_shutdown(self, deadline: Instant) -> Result<(), ShutdownError>;
}

// Admin-side surface (separate file, op-only).
pub mod admin {
    pub async fn enter_maintenance(mesh: &MeshNode, node: NodeId) -> Result<()>;
    pub async fn exit_maintenance(mesh: &MeshNode, node: NodeId) -> Result<()>;
    pub async fn drain(mesh: &MeshNode, node: NodeId, deadline: Instant) -> Result<()>;
    pub async fn uncordon(mesh: &MeshNode, node: NodeId) -> Result<()>;
    pub async fn cordon(mesh: &MeshNode, node: NodeId) -> Result<()>;
}
```

**Rust-only at v1.** The features doc names this explicitly. Cross-language bindings follow once the trait is stable. The bridge pattern is identical to MeshDB's pyo3 / napi-rs / cgo path.

**`DaemonHandle::receive_control` is async.** The daemon's `process()` method stays sync (WASM compatibility), but the side-channel for control events is async — the supervisor pushes events to a per-daemon mpsc, the SDK exposes the async receive.

---

## Locked decisions

Lock these in the plan so Phase implementations don't relitigate them:

1. **One event loop per node, not per subsystem.** Existing reactors become event sources; their internal task trees collapse into the single `MeshOsLoop`. Pre-MeshOS independent reactors stay where they are until each is wrapped — incremental migration is fine, but the end state is one loop.

2. **Admin events ride RedEX, not RPC.** Chain-driven, signed by operator identities, ordered globally. No carve-out for "fast-path drain" or anything similar.

3. **Reconcile is a pure sync function.** Async-free, testable as `fn(actual, desired) -> Vec<Action>`. All async sits in event sources + action executor.

4. **Action emission ≠ action execution.** Reconcile emits; the action executor drains the queue under backpressure. This separation is non-negotiable for the safety properties.

5. **Maintenance state lives in chain metadata, not in-memory.** Every node converges on identical state via RedEX. No in-memory "this node is in maintenance" flag that could drift.

6. **The leader is the placement authority.** Per-replica, only the elected leader acts on `RequestPlacement` / `RequestEviction`. Other nodes may observe and propose, never act.

7. **Tick cadence = heartbeat cadence.** Default 500 ms, configurable. Reconcile passes never exceed tick frequency.

8. **`MeshDaemon` extension is additive only.** New methods get default impls. Existing daemons compile unchanged. WASM compatibility preserved.

9. **Behavior snapshot rides MeshDB, not a new wire protocol.** Deck queries via `MeshQuery::Latest` against the snapshot chain. Re-uses `SUBPROTOCOL_MESHDB`.

10. **One backpressure layer over all outbound actions.** Pull cooldown, crash-loop gating, replica stabilization, drain rate-limit all funnel through one `admit()` check.

---

## Phases

Activation order, dependency-driven:

- **Phase A — `MeshOsLoop` skeleton.** One event-source converter per existing subsystem; reconcile as a no-op pure function returning `vec![]`; action executor drains an empty queue. Wires the plumbing without changing behavior. Validates the event-ordering contract under load.

- **Phase B — Daemon supervision.** Extend `MeshDaemon` with health / saturation / `on_control`. Implement `BackoffTracker` + crash-loop gating. Reconcile starts emitting `StartDaemon` / `StopDaemon` based on desired-state-from-Dataforts deltas. Replaces the per-daemon supervisor pattern that exists today.

- **Phase C — Replica enforcement.** Pull / drop / request-placement / request-eviction. Leader-only action emission for `Request*` variants. Anti-entropy sweep. Rejoin de-duplication.

- **Phase D — Locality + admin events.** D-1 ports `MESH_SCHEDULER_PLAN.md` into the loop as the continuous-rebalance step. D-2 adds RTT-driven avoid-list + admin chain commits + per-event handlers.

- **Phase E — Maintenance state machine.** Active → EnteringMaintenance → Maintenance → ExitingMaintenance → Recovery (+ DrainFailed). Metadata fields land in `behavior::metadata`. Chain-driven transitions; idempotent under replay.

- **Phase F — Behavior snapshot fold.** `RedexFold<MeshOsSnapshot>` over the action chain. Deck integration via MeshDB. Per-node fold, federated aggregation at query time.

- **Phase G — Backpressure & safety.** One `admit()` layer over the action executor. Pull cooldown, drain rate-limit, crash-loop gating, replica stabilization windows, cluster backpressure broadcast.

**Phases B-F are independently shippable** once Phase A's loop is in place. Phase G can ship alongside any of B-F or independently — its only dependency is the action-executor split from Phase A.

---

## Non-goals

Per the features doc, MeshOS is not:

- A scheduler for user jobs.
- A remote execution system.
- A workflow orchestrator.
- A data warehouse.
- A compute framework.

It is the behavior layer of the cluster — the logic that keeps everything coherent and alive.

---

## Interaction surfaces

MeshOS interacts with four substrate systems:

- **RedEX** for event streams and state commitments.
- **Capability System** for node attributes + daemon metadata + admin-event authorization.
- **Dataforts** for desired-state inputs (placement intent, replica counts, heat levels).
- **MeshDB** for serving the behavior snapshot to Deck.

MeshOS does not duplicate their logic — it composes them. Every existing primitive that ships in The Warriors / Rebel Yell / Atomic Playboys substrate flows into MeshOS as an input; MeshOS emits a coherent action plan; existing subsystems execute the actions. **The brain, not the muscle.**

---

## Test surface

Following the MeshDB pattern — every Phase ships with substrate-level unit tests + integration tests behind a `meshos` Cargo feature flag.

- **Unit tests per module.** `reconcile` is a pure sync function — exhaustive table-driven tests over (actual, desired) → expected-actions. `BackoffTracker`, `BackpressureState::admit`, the maintenance state machine all live in `mod tests`.
- **Integration tests.** `tests/meshos_*.rs` exercises end-to-end flows: a 3-node in-process cluster, a fake Dataforts emitting placement intent, observed convergence to the desired-state shape under controlled event timing.
- **Property tests.** Reconcile idempotence — replaying `(actual, desired)` produces an empty action list after the first pass converges. Maintenance state machine — every reachable sequence of admin events ends in a valid terminal state (`Active` or `DrainFailed`).
- **Behavior snapshot regression.** Postcard round-trip of the `MeshOsSnapshot` shape across every supported variant. The shape is part of the public API once Deck integrates, so the round-trip is the stability hatch.

---

*Atomic Playboys release. Follows MeshDB. Gates on a real Dataforts placement workload + a Deck consumer surface — without those, MeshOS is a reconciler with nothing to reconcile.*
