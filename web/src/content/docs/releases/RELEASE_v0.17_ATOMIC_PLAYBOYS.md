# Net v0.17 — "Atomic Playboys"

*Named after Steve Stevens's 1989 solo album — same guitarist as v0.15's Rebel Yell, next chapter. v0.15 made the Dataforts data plane stand up. v0.16 stacked the MeshDB query plane on top. v0.17 stacks the MeshOS behavior plane on both: a per-node event loop + reconcile + admit + dispatch + scheduler + chain integration that composes against the capability index, proximity graph, replication election, daemon registry, migration orchestrator, and MeshDB snapshot fold the prior releases shipped.*

## MeshOS

MeshOS is the cluster-behavior engine that turns the Net substrate into a living distributed operating system, and v0.17 is where it lands. The substrate before MeshOS shipped every primitive a cluster needs — replicas placed by `PlacementFilter`, chains advertised through the capability index, blobs moved by Dataforts, queries answered by MeshDB, sessions cryptographically pinned by Net — but every primitive ran as its own independent reactor. The replication coordinator spawned per-channel heartbeat tasks. The CortEX adapter folded events wherever a consumer asked. The migration orchestrator handled handoffs but was wired by hand. Each reactor was correct in isolation; nothing wired them into a single coherent observation of *what the node is doing right now*.

MeshOS is that single observation point. One canonical event loop per node consumes the union of seven event types — replica updates, daemon lifecycle signals, RTT samples, node health flips, admin actions, blob announcements, placement intent — folds them into a `MeshOsState` view, compares against the `DesiredState` Dataforts continuously emits, and produces a minimal action list per tick: `StartDaemon` / `StopDaemon` / `PullReplica` / `DropReplica` / `RequestPlacement` / `RequestEviction` / `MigrateBlob` / `MarkAvoid` / `ApplyBackoff` / `CommitMaintenanceTransition`. Actions ride through a single admission gate (`admit()`) that funnels every outbound act through one coherent backpressure layer — global pull cooldown, drain rate-limit, per-daemon crash-loop gating, per-chain replica stabilization windows, cluster-wide hysteresis flag — and dispatch to a pluggable `ActionDispatcher` that bridges to the existing subsystems. The substrate stays unchanged; MeshOS composes against it.

The behavior layer is what makes the cluster move. Daemons that crash get exponential backoff, then a crash-loop gate at five failures per minute. Replicas that under-score relative to a `PlacementFilter`-driven `PlacementScorer` get evicted by the chain's elected leader and refilled on the next reconcile pass — same primitive, applied as a feedback loop. RTT samples cross a 250 ms degradation threshold and become avoid-list entries; the leader's continuous-rebalance scoring loop reads them. Admin events ride RedEX chain commits — `EnterMaintenance`, `Drain`, `Cordon`, `DropReplicas`, `ClearAvoidList`, `InvalidatePlacement` — so every node converges on the same operator-driven view without RPC coordination. The maintenance state machine (Active → EnteringMaintenance → Maintenance → ExitingMaintenance → Recovery, with DrainFailed as the deadline-elapsed sideways arc) is per-node and chain-driven; every transition is idempotent under replay.

The supervisor surface that daemons see is small and disciplined. `MeshDaemon` gains three optional methods with default impls — `health() -> DaemonHealth`, `saturation() -> f32`, `on_control(DaemonControl)` — so every existing daemon compiles unchanged while new daemons can participate in graceful shutdown, drain coordination, and cluster-wide backpressure. The control surface is the WASM-friendly `DaemonControl` enum carrying relative-millisecond deadlines (the loop-internal `MeshOsControl` keeps `Instant`-anchored deadlines for scheduling and bridges via `to_daemon_control(now)`). Variants cover the full operational range — `Shutdown { grace_period_ms }`, `DrainStart { grace_period_ms }`, `DrainFinish`, `BackpressureOn { level }`, `BackpressureOff` — and arrive through the canonical control channel the supervisor owns.

Observability folds back through the same model the rest of the substrate uses. The behavior snapshot — daemons (lifecycle, health, saturation, restart-state), replicas (holders, desired count, elected leader), peers (RTT, health, maintenance-mirror), the local avoid list, the node's own maintenance state, pending actions, and a bounded `recent_failures` ring buffer — is a `MeshOsSnapshot` published live behind an `ArcSwap<MeshOsSnapshot>` (lock-free reads from any thread) and committed durably through an `ActionChainAppender` whose records ride a RedEX chain. `MeshOsSnapshotFold` (`impl RedexFold<MeshOsSnapshot>`) consumes those records on every node, so Deck queries a per-node folded snapshot through MeshDB's `MeshQuery::Latest` against the snapshot chain — no new wire protocol, no separate observability stack. The federated query plane v0.16 shipped becomes the cluster-jungle render surface v0.17 promises.

There is no separate orchestration service to provision. There is no scheduler daemon to deploy. The reconciliation loop is on the mesh because the substrate is the cluster.

---

v0.17 lands **the full MeshOS substrate behind the `meshos` Cargo feature** — the canonical event loop, the desired-vs-actual reconcile, daemon supervision (`BackoffTracker` with crash-loop gating + the `MeshDaemon` trait extension + graceful-shutdown plumbing), replica enforcement (leader-only `Request*` emission + per-node `LocalReplicaIntent` projection of admin events), locality awareness (RTT-driven `MarkAvoid` emission + pull-via-tick proximity / health probes), admin events (chain-driven `EnterMaintenance` / `ExitMaintenance` / `Drain` / `Cordon` / `DropReplicas` / `ClearAvoidList` / `InvalidatePlacement`), the maintenance state machine (chain-driven transitions with per-state metadata), the behavior snapshot (`ArcSwap`-published per-tick build + `RedexFold<MeshOsSnapshot>` over the action chain), the single `admit()` backpressure layer (pull cooldown / replica stabilization / drain rate-limit / cluster hysteresis), the action executor (admit → dispatch → retry-through-admit → record-to-chain), the continuous-rebalance scoring loop (per-chain leader-driven eviction emission gated on `score_floor` + `hysteresis_gap` + `cooldown`), and the action chain integration (postcard-versioned `ActionChainRecord` + `ActionChainAppender` trait + `MeshOsSnapshotFold` that updates `recent_failures` from the chain replay). Two source-converter patterns ship in lockstep: push-via-observer for the `DaemonRegistry` and `ReplicationCoordinator` (low-latency lifecycle / replica-transition events) and pull-via-tick for the proximity graph (RTT samples + health classifications via `LocalityProbe` + `HealthProbe` traits, with the `[u8; 32] ↔ u64` id-bridge pinned to the substrate's `mesh::graph_id_to_node_id` convention). The full surface ships behind `MeshOsRuntime::start(config, dispatcher)` — one call replaces the hand-wired loop + executor + handle + reader + scheduler + probes wiring every consumer would otherwise re-implement.

The hardening posture from the Black Diamond / Rebel Yell / Eye of the Tiger line continues. **Two coordinated code-review passes** landed before the v0.17 branch cut, covering the async stitching layer, the pure-sync decision logic, the backpressure / dispatch / chain integration, and the SDK plan + README in one combined punch list — 8 Criticals, 23 Importants, 16 Nits across the two passes. Every item closed in-tree with per-item regression coverage where the shape made one possible. The list is real work: the dispatch retry path no longer bypasses admit (transient errors used to drift cooldown counters permanently); the cluster-backpressure broadcast is wired into the executor (`update_cluster_backpressure` was unit-tested but disconnected at runtime); the scheduler's eviction emission is idempotent under double-reconcile (a `pending_evictions: HashSet<ChainId>` written by the loop and cleared on observed holder-count drop); reconcile drops on a full action queue are counted + surfaced through `RuntimeStats` instead of silent `let _ = try_send(...)`; the snapshot publish path is genuinely lock-free (`ArcSwap<MeshOsSnapshot>` replacing the prior `parking_lot::RwLock<Arc<MeshOsSnapshot>>`); `ApplyBackoff` no longer re-emits every tick while a daemon is `BackingOff` (a `last_applied_backoff` sentinel on `MeshOsState`); Phase C and the scheduler arm can no longer double-evict the same chain on the same tick; the fold side now anchors every `Instant::now()` on the loop's `last_tick` for replay determinism; `MeshOsState::replicas` is a `BTreeSet<NodeId>` rather than `Vec<NodeId>` so reconcile is O(N log N) across many chains; `MeshOsRuntime` has a `Drop` impl that aborts both tasks (no more leak when a consumer forgets `shutdown()`); probe and dispatcher panics are caught with `std::panic::catch_unwind` and recorded as `FailureRecord`s rather than killing the loop or executor task; the defer heap enforces a `max_defer_count` (default 16) before dropping a poison-pill action; `MissedTickBehavior::Delay` replaces `Skip` so a slow tick doesn't silently lose reconcile passes; `BufferingActionChainAppender` is bounded with drop-oldest semantics; `ActionChainRecord` carries a one-byte wire-format version that the decoder checks before postcard dispatch; `MeshOsHandle::publish_timeout(event, Duration)` lands so source converters with a wedged loop don't park indefinitely; `FailureRecord.age_ms` derives from `emitted_at_ms` at snapshot-read time rather than the misleading constant zero; `tracing` instrumentation lands across every loop entry / shutdown / panic / dropped action / probe install; `Instant + Duration` arithmetic uses `checked_add` everywhere; `ReplicaTransitionEvent::LeaderLost` fires on `Leader → {Replica, Idle}` so `MeshOsState::replica_leader` clears properly; `MeshOsLoop::new` returns a `MeshOsLoopParts` struct rather than a 4-tuple so adding a probe registry or stats handle later isn't a breaking change; `probe_counts()` reads both lengths under a single guard; `MeshOsSnapshot::from_state` now populates `recent_failures` from `MeshOsState::recent_failures` (previously hard-coded empty); `MeshOsRuntime` exposes `register_daemon(...)` so the trait-implementor SDK path doesn't have to reach into the runtime's internals; the snapshot's `pending` field is correctly named after what it carries (recently-emitted-but-not-yet-acknowledged actions); `CommitMaintenanceTransition { target: DrainFailed }` now carries a `reason` field; public `Config` structs gain `#[non_exhaustive]`; the `BackpressureState::release_failed_admit` rollback fix replaces a wrong-entry pop-by-equality bug; the replica step-down path emits a single committed event rather than two events that could fragment under back-pressure; `run_reconcile` samples `Instant::now()` once per tick rather than three times. **172 meshos unit tests + 11 pipeline integration tests + 13 daemon-registry + 9 daemon-trait + 15 replication-coordinator tests** all pass. `cargo clippy --features meshos --lib --tests -- -D warnings` clean. `RUSTDOCFLAGS="-D warnings" cargo doc --features meshos --no-deps --lib` clean.

The MeshOS SDK plan covering Rust / Python / Node / Go / C ships alongside as a design document — `MESHOS_SDK_PLAN.md`. The Rust SDK is the canonical surface; Python / Node / Go / C land in dependency order per consumer demand, all gated on the daemon-side-only restriction (no placement APIs, no admin-event issuance, no MeshOS-control surfaces in any binding, ever). A new `sdk` workspace member at `crates/net/sdk/` opens the slot.

No new dependencies. No protocol changes. The crate version moves from `0.16.x` to `0.17.0` to reflect the new feature surface; the workspace gains the `sdk` member.

---

## The canonical event loop

The single per-node event loop that everything composes against. Lives in `src/adapter/net/behavior/meshos/event_loop.rs`.

```rust
pub struct MeshOsLoop { /* ... */ }
pub struct MeshOsLoopParts {
    pub loop_: MeshOsLoop,
    pub handle: MeshOsHandle,
    pub actions_rx: mpsc::Receiver<PendingAction>,
    pub snapshot_reader: MeshOsSnapshotReader,
}

impl MeshOsLoop {
    pub fn new(config: MeshOsConfig) -> MeshOsLoopParts { ... }
    pub fn with_probe_registry(self, registry: ProbeRegistry) -> Self { ... }
    pub fn with_scheduler_registry(self, registry: SchedulerRegistry) -> Self { ... }
    pub async fn run(self) -> u64 { ... }
}

pub enum MeshOsEvent {
    Tick,
    ReplicaUpdate(ReplicaUpdate),
    DaemonLifecycle { daemon: DaemonRef, signal: DaemonLifecycleSignal },
    RttSample { peer: NodeId, rtt: Duration },
    NodeHealth { peer: NodeId, health: NodeHealth },
    AdminEvent(AdminEvent),
    BlobAnnouncement(BlobAnnouncement),
    PlacementIntent(PlacementIntent),
    DaemonIntentUpdate(DaemonIntentUpdate),
    LocalReplicaIntent(LocalReplicaIntentUpdate),
    ReplicaLeaderUpdate { chain: ChainId, leader: Option<NodeId> },
    MaintenanceTransitionObserved { node: NodeId, state: MaintenanceState },
    Shutdown,
}
```

One mpsc receiver, one heartbeat-aligned tick timer (default 500 ms, `MissedTickBehavior::Delay`), one reconcile pass per tick. Every source converts to a `MeshOsEvent` and publishes through `MeshOsHandle`; reconcile runs `(actual, desired, this_node, locality, maintenance, scheduler, scorer) -> Vec<MeshOsAction>` as a pure-sync function, idempotent under replay. Actions land on `actions_tx` (drop-and-count on overflow, surfaced via `RuntimeStats.dropped_actions`); the action executor drains. The snapshot publishes through `ArcSwap<MeshOsSnapshot>` after every reconcile pass.

`MeshOsLoopParts` replaces the prior 4-tuple constructor — adding a probe registry or stats handle in a future slice no longer requires a breaking change to callers.

---

## Daemon supervision

The `MeshDaemon` trait gains three optional methods with default impls:

```rust
pub trait MeshDaemon: Send + Sync {
    /* existing required: name / requirements / process / snapshot / restore */

    fn health(&self) -> DaemonHealth { DaemonHealth::Healthy }
    fn saturation(&self) -> f32 { 0.0 }
    fn on_control(&mut self, _event: DaemonControl) {}
}

pub enum DaemonHealth { Healthy, Degraded { reason: String }, Unhealthy }

pub enum DaemonControl {
    Shutdown { grace_period_ms: u64 },
    DrainStart { grace_period_ms: u64 },
    DrainFinish,
    BackpressureOn { level: f32 },
    BackpressureOff,
}
```

Defaults preserve source compatibility for every existing daemon. `DaemonHealth` lives in `compute::daemon` as the canonical type; `MeshOS` re-exports it. `DaemonControl` carries WASM-friendly relative-millisecond deadlines so daemons running in any clock domain can react.

The supervisor side runs in `behavior::meshos::supervision`. Per-daemon `BackoffTracker` records crash timestamps in a rolling window, advances `RestartState` through `Idle → BackingOff { until } → BackingOff { until }` (window doubles per crash up to 60 s cap) → `CrashLooping { until }` after five crashes within 60 s. A "stable run" (longer than `stable_run_threshold`, default 60 s) resets the window back to initial. The gate state is observable as `RestartState::is_admissible(now)`; reconcile reads it to decide whether `StartDaemon` is admissible. `ApplyBackoff { daemon, until }` records the gate `until` on the snapshot fold when a desired-Run daemon is currently gated — and now only re-emits when the `until` actually changes, not every tick.

`StopDaemon` emits with a 30 s grace deadline (`STOP_GRACE_PERIOD`). The supervisor sends `MeshOsControl::Shutdown { deadline }` and waits; past the deadline the supervisor force-terminates. Both `StopDaemon` and `ApplyBackoff` carry relative-ms deadlines on the wire — `Instant`-anchored values stay loop-internal.

A new `DaemonLifecycleObserver` trait on `compute::daemon` lets the `DaemonRegistry`'s register / replace / unregister paths fire lifecycle events through `MeshOsDaemonLifecycleSink` into the loop. `attach_to_daemon_registry(registry, handle)` is the one-line wiring helper.

---

## Replica enforcement

Two arms per the canonical leader/follower split:

```rust
pub enum MeshOsAction {
    /* … */
    PullReplica { chain: ChainId, source: NodeId },
    DropReplica { chain: ChainId },
    RequestPlacement { chain: ChainId, exclude: Vec<NodeId> },
    RequestEviction { chain: ChainId, victim: NodeId },
    /* … */
}
```

**Per-node intent (any node).** `DesiredState::desired_local_replicas` carries a per-chain `Hold` / `Drop` projection from the leader's `RequestPlacement` / `RequestEviction` decisions. Reconcile emits `PullReplica { chain, source = lex-smallest other holder }` when the local intent is `Hold` and this node isn't already a holder; `DropReplica` when the local intent is `Drop` and this node currently holds.

**Cluster-wide count (leader-only).** Reconcile reads `MeshOsState::replica_leader[chain]` and emits `RequestPlacement` / `RequestEviction` only when this node is the elected leader. Naive victim selection picks the lex-smallest holder; the continuous-rebalance scheduler refines this with placement-score ranking. A `pending_evictions: HashSet<ChainId>` written by the loop on emission and cleared when the fold observes the holder-count drop gates the scheduler arm so double-reconcile within one cooldown window doesn't pile on duplicate evictions.

`MeshOsState::replicas` is a `BTreeSet<NodeId>` keyed by chain — the deterministic-iteration property the lex-smallest selection relies on is preserved while the set membership / iteration costs are O(N log N) instead of `Vec`'s O(N²).

A new `ReplicaTransitionObserver` trait on `redex::replication_coordinator` fires `BecameHolder` / `Idled` / `LeaderChanged` / `LeaderLost` events from the coordinator's `transition_to` success path. `MeshOsReplicaTransitionSink` translates each to the matching `MeshOsEvent` (with `LeaderLost` → `ReplicaLeaderUpdate { leader: None }` so `replica_leader` clears properly when the elected leader steps down). `attach_to_replication_coordinator(coord, handle, this_node)` wires per-channel.

---

## Locality + admin events

```rust
pub enum AdminEvent {
    EnterMaintenance { node: NodeId, deadline: Option<Instant> },
    ExitMaintenance { node: NodeId },
    Drain { node: NodeId, deadline: Instant },
    Cordon { node: NodeId },
    Uncordon { node: NodeId },
    RestartAllDaemons { node: NodeId },
    ClearAvoidList { node: NodeId },
    DropReplicas { node: NodeId, chains: Vec<ChainId> },
    InvalidatePlacement { node: NodeId },
}
```

RTT samples above `LocalityConfig::degraded_rtt_threshold` (default 250 ms, 2× heartbeat cadence) emit `MarkAvoid { peer, reason, ttl }`. Gated on whether the peer is already in `MeshOsState::avoid_list` so a persistently-bad peer produces one action, not one per tick. Emission sorts by peer id for byte-stable output. Avoid-list entries expire after `avoid_ttl` (default 5 min); the per-Tick fold GCs expired entries.

`DropReplicas { node, chains }` projects into `DesiredState::desired_local_replicas[chain] = Drop` for the named chains when `node == this_node`. The same `DropReplica` emission path the leader-driven scheduler uses handles the actual action — operator-commanded drops and scheduler-driven drops share one code path. `ClearAvoidList` empties `MeshOsState::avoid_list` in the fold; subsequent reconcile passes re-evaluate RTT and re-emit `MarkAvoid` if the underlying RTT is still bad.

Admin commits are signed via the existing channel-auth guards (`CHANNEL_AUTH_GUARD_PLAN.md`); unauthorized commits are rejected at the chain-commit layer and never reach the reconcile pass. The fold consumes them identically on every node, so two operators racing each other resolve at the chain-commit ordering rather than via RPC coordination.

---

## Pull-via-tick probes — proximity + heartbeat

Two pluggable probe traits:

```rust
pub trait LocalityProbe: Send + Sync + 'static {
    fn rtt_samples(&self) -> Vec<(NodeId, Duration)>;
}

pub trait HealthProbe: Send + Sync + 'static {
    fn health_samples(&self) -> Vec<(NodeId, NodeHealth)>;
}
```

Polled by the loop on every Tick, BEFORE reconcile, so the freshest fold drives the diff. The cadence-bound poll coalesces what would otherwise be a per-pingwave observer firing on the hot path — proximity-graph edge updates run many per second per peer, but reconcile only needs the latest sample per tick.

`ProximityGraphLocalityProbe` reads RTT from `ProximityGraph::all_nodes()`. `ProximityGraphHealthProbe` derives `Healthy` / `Degraded` / `Unreachable` from `ProximityNode::last_seen` against thresholds (defaults 1.5 s degraded, 5 s stale — 3× and 10× heartbeat). The `[u8; 32] ↔ u64` id-bridge follows the substrate's `mesh::graph_id_to_node_id` convention (first 8 bytes little-endian) — pinned at the SDK boundary so MeshOS's `u64` `NodeId` and the proximity graph's 32-byte form interoperate cleanly.

`ProbeRegistry` is a clone-shared cell (`Arc<RwLock<Vec<...>>>`) so consumers can install probes after `MeshOsRuntime::start` — the runtime retains its registry clone; the loop reads through; additions take effect on the next Tick. Each registered probe is wrapped in `std::panic::catch_unwind`; a panicking probe records a `FailureRecord` rather than killing the loop task.

---

## Maintenance state machine

Per-node state machine driven by chain-committed admin events and condition-driven forward transitions:

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

`AdminEvent::EnterMaintenance { node, deadline }` flips `local_maintenance` to `EnteringMaintenance` when `node == this_node`. Reconcile observes the conditions on every Tick: when all local replicas have migrated AND all daemons are stopped, emit `CommitMaintenanceTransition { target: Maintenance }`. When the deadline elapses with conditions unmet, emit `CommitMaintenanceTransition { target: DrainFailed { reason } }` — the reason rides on the wire so the operator surfacing on Deck carries the actual failure mode, not a generic flag. `AdminEvent::ExitMaintenance` flips from `Maintenance` (or `DrainFailed`) to `ExitingMaintenance`; reconcile observes daemon-restart health and emits `Recovery` once all daemons are running healthy. The `Recovery` ramp-up window (default 5 min via `MaintenanceConfig::recovery_ramp_window`) ends with a `CommitMaintenanceTransition { target: Active }`.

The transition round-trip through the chain lands via `MeshOsEvent::MaintenanceTransitionObserved { node, state }` — the action executor commits, the chain replay surfaces it, the fold gates the local state advance on whether the prior state was a valid predecessor. `since` is anchored on `last_tick` so two replays of the same admin-event sequence produce identical state.

`CommitMaintenanceTransition`'s `target` enum carries the `reason: String` field for `DrainFailed` directly — no out-of-band metadata required.

---

## Behavior snapshot fold for Deck

The serializable projection of every loop's view, published live behind `ArcSwap<MeshOsSnapshot>`:

```rust
pub struct MeshOsSnapshot {
    pub daemons: BTreeMap<u64, DaemonSnapshot>,
    pub replicas: BTreeMap<ChainId, ReplicaSnapshot>,
    pub peers: BTreeMap<NodeId, PeerSnapshot>,
    pub avoid_list: BTreeMap<NodeId, AvoidEntrySnapshot>,
    pub local_maintenance: MaintenanceStateSnapshot,
    pub recently_emitted: Vec<PendingActionSnapshot>,
    pub recent_failures: VecDeque<FailureRecord>,
}
```

All fields `Serialize + Deserialize`; `Instant` is flattened to milliseconds-relative-to-snapshot for wire portability. Tests pin postcard + JSON round-trip across every variant; the wire shape is part of the public API once Deck integrates. `FailureRecord.age_ms` derives at snapshot-build time from the record's `emitted_at_ms` (previously hard-coded zero — the field is now meaningful, not just stable).

`recently_emitted` is the ring buffer of actions reconcile has emitted but the executor hasn't acknowledged; bounded by `action_queue_capacity`. `recent_failures` collects entries from three sources: dispatcher errors (with `retry_after_ms` if any), admit-time gate trips (with `cooldown_ms` if any), and probe / dispatcher panic catches. `MeshOsSnapshot::from_state(actual, desired, recently_emitted)` builds the projection on demand; the loop publishes after every reconcile.

`MeshOsSnapshotReader::read()` clones the `Arc<MeshOsSnapshot>` under a single `ArcSwap::load_full` — no read lock, no contention with the publisher.

A `MeshOsSnapshotFold` (`impl RedexFold<MeshOsSnapshot>`) consumes `ActionChainRecord`s and updates a per-node snapshot on chain replay:

```rust
pub struct ActionChainRecord {
    pub id: u64,
    pub kind: String,
    pub emitted_at_ms: u64,
    pub disposition: ActionDisposition,
}

pub enum ActionDisposition {
    Dispatched,
    Failed { reason: String, retry_after_ms: Option<u64> },
    Gated { reason: String, cooldown_ms: Option<u64> },
}
```

`Dispatched` records leave the fold silent (the recently-emitted ring covers them); `Failed` and `Gated` records push `FailureRecord`s onto `recent_failures`, bounded by `RECENT_FAILURES_CAPACITY = 256`. The record carries a one-byte wire-format version that the decoder checks before postcard dispatch; an older / newer record surfaces as `DecodeError::UnsupportedVersion` rather than garbled deserialization. `BufferingActionChainAppender` for tests is bounded with drop-oldest; `NoOpActionChainAppender` is the bootstrap default.

Deck queries the snapshot via MeshDB's `MeshQuery::Latest` against the snapshot chain — the federated executor routes to a node holding the fold; the result row carries the postcard-encoded snapshot. No new wire protocol, no Deck-specific RPC. The v0.16 federated query plane becomes the v0.17 observability surface.

---

## Continuous-rebalance scheduler

The leader-driven scoring loop. For each chain where this node is the elected leader, score every holder via a pluggable `PlacementScorer`, pick the lowest, and emit `RequestEviction` when (worst score < `score_floor`) AND (best alternative > worst + `hysteresis_gap`) AND (cooldown elapsed):

```rust
pub trait PlacementScorer: Send + Sync + 'static {
    fn score(&self, chain: ChainId, node: NodeId) -> Option<f32>;
    fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)>;
}

pub struct SchedulerConfig {
    pub score_floor: f32,            // default 0.5
    pub hysteresis_gap: f32,         // default 0.2
    pub cooldown: Duration,          // default 5 min
}
```

The trait abstracts the substrate's `PlacementFilter` so production wires a `PlacementFilter`-backed impl and tests mock the score table. The eviction emission is idempotent across reconcile passes — a `pending_evictions: HashSet<ChainId>` written by the loop on each emission and cleared when the fold observes the holder count drop. Phase-C's existing diff observes the holder-count drop and refills via `RequestPlacement` on the next tick — two-stage rebalance with no new action variant. Per-chain `MeshOsState::last_rebalance` records the most recent eviction's `Instant` so the cooldown survives transient state and the same chain doesn't flap A→B→A within the window.

Scheduler emission sorts by chain id for byte-stable output across reconcile calls regardless of HashMap iteration. The scheduler arm short-circuits when Phase C's overcount diff already emitted an eviction for the same chain on the same tick — the two arms can no longer fragment the leader's view of the chain.

---

## Single admit() backpressure layer

One function gates every outbound action:

```rust
pub enum AdmissionResult {
    Admit,
    Defer { retry_after: Duration },
    Gate { cooldown_until: Instant, reason: &'static str },
}

impl BackpressureState {
    pub fn admit(
        &mut self,
        action: &MeshOsAction,
        now: Instant,
        config: &BackpressureConfig,
    ) -> AdmissionResult { ... }
}
```

Throttles applied: global pull cooldown (default 250 ms), per-chain replica stabilization (default 60 s), per-daemon gate driven by `BackoffTracker::release_at`, drain rate-limit (default 10/sec/zone), cluster-wide hysteresis flag (default 1000 high / 200 low). Same `admit()` for every action variant — a drain-triggered migration cannot dodge the pull cooldown; a crash-looping daemon cannot dodge the gate just because its restart was admin-driven.

The dispatch retry path now routes through `admit()` rather than directly re-pushing onto the defer heap. `BackpressureState::release_failed_admit(action, now)` rolls back the per-action reservations (`drain_window` push, `last_pull_admitted` stamp, `chain_stabilization` window) when a dispatch error fires after admit returned `Admit` — counters no longer drift permanently after transient errors. The defer heap enforces `max_defer_count` (default 16) before dropping a poison-pill action with a `FailureRecord`.

`ActionExecutor` runs `update_cluster_backpressure` once per `handle_one` with the current queue depth and surfaces the returned `ClusterBackpressureChange` through the dispatcher's `MeshOsControl::BackpressureOn { level }` / `BackpressureOff` broadcast — the plan's promise is no longer dead code. Per-tick `tick()` GCs elapsed daemon gates + chain stabilization windows so the state stays bounded under churn.

---

## Action executor

Drains the loop's `Receiver<PendingAction>`, runs each through `admit()`, dispatches via a pluggable `ActionDispatcher`, and records the outcome to the action chain:

```rust
pub trait ActionDispatcher: Send + Sync + 'static {
    fn dispatch<'a>(
        &'a self,
        action: MeshOsAction,
    ) -> BoxFuture<'a, Result<(), DispatchError>>;
}

pub struct DispatchError {
    pub reason: String,
    pub retry_after: Option<Duration>,
}
```

`LoggingDispatcher` ships for bootstrap and tests — records every dispatch in an internal `Mutex<Vec<MeshOsAction>>` and supports `fail_next(err)` for exercising the failure / retry paths. Production dispatchers wrap the existing subsystems (`DaemonRegistry` for start/stop, the migration orchestrator for pull/drop/migrate, the admin chain commit path for `CommitMaintenanceTransition`).

The executor's `with_chain_appender(...)` builder installs an `ActionChainAppender`; the dispatcher, gate, and retry paths all append records via `append_dispatched` / `append_failed` / `append_gated`. The chain replay drives the snapshot's `recent_failures` ring buffer.

Probe + dispatcher panics are caught via `std::panic::catch_unwind`; the panic message rides into a `FailureRecord` and the executor / loop continues. Stats are exposed live via `ExecutorHandle::stats()` and on shutdown via `RuntimeStats.executor`.

---

## `MeshOsRuntime` — one-call entry point

```rust
impl MeshOsRuntime {
    pub fn start<D: ActionDispatcher>(config: MeshOsConfig, dispatcher: Arc<D>) -> Self;
    pub fn start_with_probes<D: ActionDispatcher>(/* ... */) -> Self;
    pub fn start_full<D: ActionDispatcher>(/* ... */) -> Self;

    pub fn handle(&self) -> &MeshOsHandle;
    pub fn handle_clone(&self) -> MeshOsHandle;
    pub fn snapshot(&self) -> MeshOsSnapshot;
    pub fn snapshot_reader(&self) -> &MeshOsSnapshotReader;
    pub fn executor_stats(&self) -> ExecutorStatsSnapshot;
    pub fn add_locality_probe(&self, probe: Arc<dyn LocalityProbe>);
    pub fn add_health_probe(&self, probe: Arc<dyn HealthProbe>);
    pub fn install_placement_scorer(&self, scorer: Arc<dyn PlacementScorer>);
    pub fn register_daemon(&self, daemon: Box<dyn MeshDaemon>, keypair: EntityKeypair)
        -> Result<DaemonHandle, RuntimeError>;
    pub async fn shutdown(self) -> Result<RuntimeStats, RuntimeShutdownError>;
}

impl Drop for MeshOsRuntime {
    fn drop(&mut self) { /* aborts loop + executor tasks, warns if shutdown wasn't called */ }
}
```

`start(config, dispatcher)` spawns the loop + executor as tokio tasks; the returned struct exposes the publish handle, snapshot reader, probe / scheduler registries, executor stats, and a graceful shutdown path. Source-converter helpers (`attach_to_daemon_registry`, `attach_to_replication_coordinator`) plug into the runtime's handle.

`register_daemon(...)` is the daemon-side path — implementors of the extended `MeshDaemon` trait register through the runtime rather than reaching into the underlying `DaemonRegistry`. The runtime's `Drop` impl aborts both tasks (loop + executor) and emits a `tracing::warn` when shutdown wasn't called explicitly — no more leaked tasks on accidental drop.

`MeshOsHandle::publish_timeout(event, Duration)` complements `publish` and `try_publish` for source converters that need timeout semantics without blocking. The module-level example uses `try_publish` per the new doc-comment guidance.

---

## SDK plan

The MeshOS SDK plan covering Rust / Python / Node / Go / C ships as a design document at `docs/plans/MESHOS_SDK_PLAN.md`. The Rust SDK is the canonical surface — `MeshOsDaemonHandle` + `daemon_main!` macro + integration tests against `MeshOsRuntime` with `LoggingDispatcher`. Python (pyo3, sync-first), Node (napi-rs, AsyncIterable control events), Go (cgo + `context.Context`-aware control channels), and C (vtable + last-error surface mirroring MeshDB's FFI pattern) land in dependency order per consumer demand. A new `sdk` workspace member at `crates/net/sdk/` opens the slot.

The plan locks in ten decisions, most importantly the non-goals: no placement APIs in any binding, no admin-event issuance, no MeshOS-control surfaces. The SDK is **the daemon contract**, exposed in five languages. Operator tooling, federated interactions, and MeshDB queries belong to separate SDKs.

---

## Toolchain + dependency upgrades

No new dependencies. The `arc-swap = "1.7.1"` already in the workspace gets a new consumer (`MeshOsSnapshot` publish path). The `tracing = "0.1"` workspace dep gets a new consumer (every `meshos::*` module emits `debug!` / `warn!` / `error!` events at lifecycle and failure boundaries). The crate version moves from `0.16.x` to `0.17.0`; the workspace gains the `sdk` member at `crates/net/sdk/`.

The `meshos` Cargo feature gates the entire surface. It pulls in `cortex` (which pulls in `redex`); the substrate builds clean without `--features meshos` and the `meshos` cdylib path is purely additive.

---

## Test hygiene

- **Lib suite at 2715+ tests** (was 2645+ at v0.16 release). 200+ net new tests across the MeshOS surface + cross-cutting fixes; every numbered review item from both hardening passes ships with at least one regression where the shape made one possible. Notable additions:
  - **Reconcile + scheduler:** `reconcile::scheduler_eviction_is_idempotent_when_loop_writes_back_last_rebalance`, `reconcile::phase_c_overcount_eviction_suppresses_phase_d1_eviction_for_same_chain`, `reconcile::apply_backoff_is_not_re_emitted_after_the_loop_records_it`, the 13-test scheduler reconcile arm covering leader-only gating + hysteresis + cooldown + worst-victim selection + chain-id-sorted emission.
  - **Backpressure + executor:** `BackpressureState::release_failed_admit_*` (3 cases — pull cooldown, drain window, chain stabilization), `executor::cluster_backpressure_edges_surface_through_dispatcher_hook`, `executor::dispatch_failure_with_retry_releases_pull_cooldown`, `executor::dispatcher_panic_does_not_kill_executor`, `executor::dispatch_retry_drops_after_exceeding_max_defer_count`.
  - **Event loop:** `event_loop::snapshot_reader_does_not_stall_under_concurrent_reads`, `event_loop::dropped_actions_counter_increments_when_action_queue_is_full`, `event_loop::panicking_probe_does_not_kill_the_loop`, `event_loop::publish_timeout_returns_queue_full_when_loop_is_wedged`, `event_loop::shutdown_event_short_circuits_pending_events_after_it` (re-pinned with actual assertions).
  - **State + maintenance:** `state::enter_maintenance_since_is_anchored_on_last_tick_for_replay_determinism`, the `MaintenanceState` round-trip tests including `DrainFailed { reason }`, the `MaintenanceTransitionObserved` gated-state-advance tests.
  - **Runtime + chain:** `runtime::dropping_runtime_without_shutdown_aborts_tasks`, `runtime::register_daemon_round_trip_through_executor`, `chain::buffering_appender_drops_oldest_when_at_capacity`, `chain::decode_rejects_payload_with_unknown_wire_version`, `chain::decode_rejects_empty_payload`, `chain::encode_decode_round_trip_preserves_record`, the end-to-end executor → buffering appender → fold → snapshot test.
  - **Snapshot + sources:** `snapshot::failure_record_age_ms_derives_from_recorded_at_ms`, `sources::leader_lost_event_clears_replica_leader_via_none_update`.
- **`cargo clippy --features meshos --all-targets -D warnings` clean** across substrate + every binding crate.
- **`cargo doc --features meshos --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — every public item in the `meshos` surface carries a doc comment; intra-doc links resolve through the public re-exports.
- **172 meshos unit tests + 11 pipeline integration tests + 13 daemon-registry + 9 daemon-trait + 15 replication-coordinator tests** all pass.

---

## Breaking changes

### API — MeshOS surface is new

`MeshOsLoop` + `MeshOsRuntime` + `MeshOsHandle` + `MeshOsSnapshot` + `MeshOsSnapshotReader` + `MeshOsState` + `DesiredState` + `MeshOsConfig` + `MeshOsEvent` + `MeshOsAction` + `MeshOsControl` + `ProbeRegistry` + `SchedulerRegistry` + `ActionDispatcher` + `ActionExecutor` + `ActionChainAppender` + every operator family are all new in v0.17. Behind the `meshos` Cargo feature; non-meshos builds see the substrate path unchanged.

### `MeshDaemon` trait gains three optional methods

`health()` / `saturation()` / `on_control(DaemonControl)` land on the trait itself (not feature-gated) with default impls. Every existing daemon compiles unchanged. `DaemonHealth` and `DaemonControl` are new public types in `compute::daemon`; the latter is the WASM-friendly relative-ms form daemons receive.

### `DaemonRegistry` gains a lifecycle observer

`DaemonRegistry::set_lifecycle_observer(Option<Arc<dyn DaemonLifecycleObserver>>)` is new. The hot path is unaffected when no observer is installed (one `RwLock<Option<Arc>>` read + `is_none` check). The `unregister` path uses `try_lock` against the inner Mutex to avoid a deadlock when called from inside a `with_host` closure on the same id; observers see an empty name on that path and correlate by id with the prior `Registered` event.

### `ReplicationCoordinator` gains a transition observer

`ReplicationCoordinator::set_transition_observer(Option<Arc<dyn ReplicaTransitionObserver>>)` is new. `BecameHolder` / `Idled` / `LeaderChanged` / `LeaderLost` events fire from the successful path of `transition_to` after the chain-tag side effect lands.

### Workspace — new `sdk` member

`crates/net/sdk/` is a new workspace member. The slot opens for the Rust MeshOS SDK; the directory is empty in this release and populates once the SDK plan's Phase 1 lands.

### Behavioral fixes that may surface as test breakage

- **`MissedTickBehavior::Delay`** replaces `Skip` on the loop's heartbeat timer. Tests that asserted skipped ticks under load will see delayed ticks instead.
- **`MeshOsRuntime::drop` aborts both tasks.** Tests that relied on the loop / executor running past a dropped runtime will see the tasks aborted.
- **`MeshOsHandle::publish` is still async-blocking on a full queue;** tests that hung previously now have `publish_timeout(event, Duration)` available as a non-blocking-on-deadline alternative.
- **Probe panics no longer kill the loop.** Tests that asserted `JoinError::Panic` propagation through the loop task will see the probe's panic surface in `recent_failures` instead.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.17 line.** Recompile / rebuild the binding cdylib with the `meshos` Cargo feature on when you want the MeshOS surface; without it, the substrate is unchanged from v0.16.
2. **MeshOS opt-in.** Channels that want the cluster-behavior engine: build the substrate with `--features meshos` and call `MeshOsRuntime::start(MeshOsConfig::default(), dispatcher)` where `dispatcher` wires `MeshOsAction` variants to the existing subsystems (`DaemonRegistry` for `StartDaemon` / `StopDaemon`, the migration orchestrator for `PullReplica` / `DropReplica`, the admin-chain commit path for `CommitMaintenanceTransition`).
3. **Source converters.** Attach the daemon-registry sink via `attach_to_daemon_registry(&registry, runtime.handle_clone())`. Attach a per-coordinator replica sink via `attach_to_replication_coordinator(&coord, runtime.handle_clone(), this_node)`. Install proximity probes via `runtime.add_locality_probe(...)` and `runtime.add_health_probe(...)` against a shared `Arc<ProximityGraph>`.
4. **Placement scorer.** Install a `PlacementScorer` impl via `runtime.install_placement_scorer(scorer)`. The substrate ships the trait + scheduler arm; the impl wires to `PlacementFilter` per consumer.
5. **Action chain.** Install an `ActionChainAppender` on the executor (production: writes to a RedEX chain that the `MeshOsSnapshotFold` consumes on every node). The default `NoOpActionChainAppender` makes the chain optional; Deck integration drives the wiring.
6. **Daemon trait additions.** If you implement `MeshDaemon` and want supervision participation: override `health()` / `saturation()` / `on_control(DaemonControl)`. Defaults preserve compatibility; overrides opt into graceful shutdown, drain coordination, and cluster-wide backpressure.
7. **Shutdown.** Always call `runtime.shutdown().await` rather than dropping the runtime. The new `Drop` impl aborts the tasks and warns, but an explicit shutdown is the contract for clean lifecycle.
8. **Snapshot consumers.** Read the snapshot via `runtime.snapshot()` (cheap — one `ArcSwap::load_full`) or sample executor stats via `runtime.executor_stats()`. Deck queries arrive through MeshDB once the snapshot chain is wired.

---

Released 2026-05-14.

## License

See [LICENSE](../../LICENSE).
