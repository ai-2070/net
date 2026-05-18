//! [`MeshOsLoop`] — the canonical event loop. Locked decision #1:
//! one stream → one reconcile → consistent actions. Locked
//! decision #4: reconcile emits, the action executor drains.
//!
//! Phase A wires the plumbing. The loop owns an mpsc receiver
//! that consumes [`super::event::MeshOsEvent`]s from arbitrary
//! sources, folds each event into [`super::state::MeshOsState`]
//! (and routes desired-state input into
//! [`super::state::DesiredState`]), runs
//! [`super::reconcile::reconcile`] at most once per
//! [`super::event::MeshOsEvent::Tick`], and pushes any emitted
//! actions through an mpsc sender that the action executor will
//! drain. Phase A's reconcile is a no-op so the executor sees an
//! empty queue under steady state.
//!
//! Sources fan-in via converters owned by their subsystems —
//! none ship in Phase A. Tests drive events directly through the
//! source channel to exercise the ordering contract.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::time::{interval_at, Instant as TokioInstant, MissedTickBehavior};

use super::action::{AllocateActionId, PendingAction};
use super::config::MeshOsConfig;
use super::control::{ControlSink, MeshOsControl};
use super::event::MeshOsEvent;
use super::maintenance::MaintenanceState;
use super::probes::{HealthProbe, LocalityProbe};
use super::reconcile::reconcile;
use super::scheduler::SchedulerRegistry;
use super::snapshot::{FailureRecord, MeshOsSnapshot};
use super::state::{DesiredState, MeshOsState};

/// Per-node MeshOS instance. Owns the actual + desired state
/// folds, the event-source channel, and the action-executor
/// channel. Cloneable handles (`MeshOsHandle`) hand out
/// `mpsc::Sender<MeshOsEvent>` clones for sources to publish on;
/// `MeshOsLoop::run` is the long-lived task.
pub struct MeshOsLoop {
    config: Arc<MeshOsConfig>,

    /// Inbound event stream. The loop owns the receiver; sender
    /// clones live on every [`MeshOsHandle`]. When the last
    /// handle drops, `recv()` returns `None` and the loop exits.
    events_rx: mpsc::Receiver<MeshOsEvent>,

    /// Outbound action queue. The action executor task drains
    /// this; Phase A drains and drops (no real subsystem
    /// dispatch yet).
    actions_tx: mpsc::Sender<PendingAction>,

    /// Action id allocator.
    action_ids: AllocateActionId,

    /// Folded substrate state.
    actual: MeshOsState,
    /// Folded desired state (placement intent + future
    /// daemon-intent feeds).
    desired: DesiredState,

    /// Most recent post-reconcile snapshot. Updated on every
    /// Tick after the reconcile pass; readable through
    /// [`MeshOsSnapshotReader::read`] from any other task /
    /// thread. `ArcSwap` keeps the publish path one atomic
    /// pointer store and the read path one atomic load + Arc
    /// clone — no lock contention with reconcile.
    snapshot: Arc<ArcSwap<MeshOsSnapshot>>,

    /// Ring of the most-recently-emitted actions. The snapshot's
    /// `pending` field renders this as "what reconcile recently
    /// produced." Bounded by `action_queue_capacity`; older
    /// entries drop FIFO when the cap is exceeded. The executor
    /// does NOT signal completion back, so this is *not* a true
    /// in-flight list — drained / completed actions stay in the
    /// ring until they age out.
    recent_emissions: Vec<PendingAction>,

    /// Pull-via-tick probes the loop polls on each Tick. Shared
    /// with the [`ProbeRegistry`] so consumers can install
    /// probes after `MeshOsLoop::new` (the runtime in particular
    /// spawns the loop immediately and attaches probes via the
    /// registry).
    probes: ProbeRegistryInner,

    /// Phase D-1 scheduler registry — single-slot pluggable
    /// [`super::scheduler::PlacementScorer`]. Shared via Arc;
    /// install via [`SchedulerRegistry::install`] before or
    /// after `run()`.
    scheduler: SchedulerRegistry,

    /// X-13 recovery registry — pluggable per-tick recovery
    /// handlers for groups whose slots were marked unhealthy
    /// after a placement failure. The tick handler calls
    /// `try_run_all` after `poll_probes` so the very next
    /// reconcile pass sees the recovered slots. Empty by default;
    /// SDK consumers register handlers via
    /// `MeshOsRuntime::recovery_registry()`.
    recovery_registry: crate::adapter::net::compute::RecoveryRegistry,

    /// Reconcile-pass counter — used by tests / diagnostics to
    /// confirm reconcile fired exactly once per Tick.
    reconcile_count: u64,

    /// Monotonic counter the loop stamps onto every
    /// `AdminAuditRecord` it emits. Strictly increasing across
    /// the runtime's lifetime; the Deck SDK's audit-tail
    /// stream depends on this for dedup across snapshot polls.
    admin_audit_seq: u64,

    /// Monotonic counter the loop stamps onto every
    /// `LogRecord` it emits. Same pattern as
    /// `admin_audit_seq`; the Deck SDK's log-tail stream
    /// dedups against this.
    log_seq: u64,
    /// Boot-time identifier this loop stamps on every
    /// published snapshot via
    /// [`MeshOsSnapshot::runtime_epoch_id`]. SDK consumers
    /// dedup'ing via `seq` values pair each watermark with this
    /// value — when the snapshot's epoch flips, they reset.
    runtime_epoch_id: u64,
    /// Ring buffer of admin-commit outcomes — every admin
    /// commit the loop observes (signed ICE bundle or unsigned
    /// `MeshOsEvent::AdminEvent(...)`) lands here, regardless
    /// of whether a verifier accepted, rejected, or skipped
    /// it. Bounded to
    /// [`super::ice::DEFAULT_MAX_ADMIN_AUDIT_RECORDS`]; the
    /// loop drops the oldest entry FIFO when the cap is
    /// exceeded. Lives on the loop rather than `MeshOsState`
    /// because it's an append-only output buffer, not fold
    /// state — `state.rs` had explicit dead arms for
    /// `SignedIceCommit` / `SignedAdminCommit` precisely
    /// because they don't fold.
    admin_audit_ring: VecDeque<super::ice::AdminAuditRecord>,
    /// Ring buffer of log records — every
    /// `MeshOsEvent::LogLine` daemons or source converters
    /// publish lands here. Bounded to
    /// [`super::logs::DEFAULT_MAX_LOG_RING_RECORDS`]; older
    /// entries drop FIFO. Lives on the loop rather than
    /// `MeshOsState` because log lines don't fold.
    log_ring: VecDeque<super::logs::LogRecord>,

    /// Actions reconcile emitted that the action-queue
    /// `try_send` rejected because the executor was at
    /// `action_queue_capacity`. Cloneable counter — the runtime
    /// surfaces it through [`MeshOsRuntime::executor_stats`] for
    /// operator visibility.
    dropped_actions: Arc<AtomicU64>,
    /// Shared reference to the executor's recent-failures ring.
    /// The executor task writes; the loop reads on every
    /// `publish_snapshot` and copies the records into the
    /// snapshot's `recent_failures` field. Optional so a loop
    /// constructed without an executor (e.g. unit tests) still
    /// publishes (with an empty ring).
    executor_failures: Option<Arc<RwLock<VecDeque<FailureRecord>>>>,
    /// Shared failure-seq counter — same counter the executor
    /// uses. The loop stamps its own runtime-side failures
    /// (e.g. migration-abort dispatcher errors) with this so
    /// SDK consumers' dedup gate sees a single monotonic
    /// sequence across both writers.
    executor_failure_seq: Option<Arc<AtomicU64>>,
    /// Shared failure-chain appender — same instance the
    /// executor uses. The loop dual-writes runtime-side
    /// failures to the durable chain via this so chain
    /// history covers loop-side failures too.
    executor_failure_appender: Option<Arc<dyn super::failure_chain::FailureChainAppender>>,
    /// Optional control-event sink. When attached, the loop
    /// translates this-node `local_maintenance` transitions and
    /// future backpressure flips into [`MeshOsControl`] events
    /// and forwards them through the sink. The SDK installs one
    /// that fans events out to per-daemon channels via its
    /// router; substrate-internal uses can ignore.
    control_sink: Option<Arc<dyn ControlSink>>,
    /// Discriminant of the most recent `local_maintenance` value
    /// the loop observed. Updated on every `apply()` call so
    /// state transitions caused by either an admin event or a
    /// `MaintenanceTransitionObserved` (chain replay) surface
    /// uniformly.
    last_local_maintenance: MaintenanceDiscriminant,
    /// Optional ICE-commit verifier. When installed, every
    /// `MeshOsEvent::SignedIceCommit` is run through this gate
    /// before the inner `AdminEvent` folds into state. When
    /// absent, signed commits fold their inner event without
    /// verification — useful for in-process tests where the
    /// SDK side already gates. The substrate slice that ships
    /// the operator-policy chain wires a registry in by default.
    admin_verifier: Option<Arc<super::ice::AdminVerifier>>,

    /// Admin audit chain appender. Production deployments
    /// wire a `TypedRedexFile<AdminAuditRecord>` here so
    /// security review can replay every admin commit across
    /// the cluster's lifetime. Default is
    /// `NoOpAdminAuditChainAppender` — the in-memory ring on
    /// `MeshOsState.admin_audit` is the only readable surface
    /// when no chain is wired.
    admin_audit_appender: Arc<dyn super::audit_chain::AdminAuditChainAppender>,

    /// Log chain appender. Production deployments wire a
    /// `TypedRedexFile<LogRecord>` here so the per-daemon log
    /// view extends to cluster-lifetime replay. Default is
    /// `NoOpLogChainAppender` — only the in-memory ring on
    /// `MeshOsState.log_ring` is observable when no chain is
    /// wired.
    log_appender: Arc<dyn super::log_chain::LogChainAppender>,

    /// Migration-abort dispatcher. The loop calls this after
    /// folding a verified
    /// [`super::event::AdminEvent::KillMigration`]. Production
    /// deployments wire the
    /// [`super::migration_aborter::OrchestratorMigrationAborter`]
    /// adapter so the local `MigrationOrchestrator` actually
    /// aborts in-flight migrations; tests + bootstrap use the
    /// `NoOpMigrationAborter` default and the commit remains
    /// audit-only.
    migration_aborter: Arc<dyn super::migration_aborter::MigrationAborter>,

    /// Migration-snapshot source. The loop calls this on every
    /// snapshot publish and embeds the result in the
    /// snapshot's `in_flight_migrations` field, so the ICE
    /// simulator can enumerate which daemon a `KillMigration`
    /// target would affect. Production deployments wire the
    /// [`super::migration_snapshot_source::OrchestratorMigrationSnapshotSource`]
    /// adapter; bootstrap + tests use the no-op default and
    /// the snapshot's `in_flight_migrations` reads empty.
    migration_snapshot_source: Arc<dyn super::migration_snapshot_source::MigrationSnapshotSource>,
}

/// Cheap-to-compare snapshot of [`MaintenanceState`]'s variant
/// (no embedded `Instant` or `String`). Used by the loop to
/// detect transitions without holding the full state value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MaintenanceDiscriminant {
    #[default]
    Active,
    EnteringMaintenance,
    Maintenance,
    ExitingMaintenance,
    DrainFailed,
    Recovery,
}

impl MaintenanceDiscriminant {
    fn from_state(state: &MaintenanceState) -> Self {
        match state {
            MaintenanceState::Active => Self::Active,
            MaintenanceState::EnteringMaintenance { .. } => Self::EnteringMaintenance,
            MaintenanceState::Maintenance { .. } => Self::Maintenance,
            MaintenanceState::ExitingMaintenance { .. } => Self::ExitingMaintenance,
            MaintenanceState::DrainFailed { .. } => Self::DrainFailed,
            MaintenanceState::Recovery { .. } => Self::Recovery,
        }
    }
}

/// Inner shared cell — both probe lists behind one
/// `Arc<RwLock>` so the runtime + loop see the same set AND
/// [`ProbeRegistry::probe_counts`] is an atomic snapshot. The
/// install path is rare; the per-tick poll already clones the
/// lists out under the read lock, so the single-lock pattern
/// costs nothing on the hot path.
#[derive(Default)]
struct ProbeListsInner {
    locality: Vec<Arc<dyn LocalityProbe>>,
    health: Vec<Arc<dyn HealthProbe>>,
    inventory: Vec<Arc<dyn super::probes::InventoryProbe>>,
}

#[derive(Clone, Default)]
struct ProbeRegistryInner {
    lists: Arc<parking_lot::RwLock<ProbeListsInner>>,
}

/// External handle for attaching probes to the loop after
/// `MeshOsLoop::new` has consumed the loop (e.g. the runtime
/// spawned it as a tokio task). Clone-shared with the loop.
#[derive(Clone, Default)]
pub struct ProbeRegistry {
    inner: ProbeRegistryInner,
}

impl ProbeRegistry {
    /// Construct an empty registry. The loop reads through this
    /// instance after [`MeshOsLoop::with_probe_registry`]
    /// installs it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a [`LocalityProbe`]. Probes are polled by the
    /// loop in registration order, once per Tick.
    pub fn add_locality_probe(&self, probe: Arc<dyn LocalityProbe>) {
        self.inner.lists.write().locality.push(probe);
    }

    /// Install a [`HealthProbe`]. Probes are polled by the loop
    /// in registration order, once per Tick.
    pub fn add_health_probe(&self, probe: Arc<dyn HealthProbe>) {
        self.inner.lists.write().health.push(probe);
    }

    /// Install an [`super::probes::InventoryProbe`]. Probes are
    /// polled by the loop in registration order, once per Tick;
    /// partial samples merge into per-peer state (later probes
    /// overwrite earlier on the same peer + axis).
    pub fn add_inventory_probe(&self, probe: Arc<dyn super::probes::InventoryProbe>) {
        self.inner.lists.write().inventory.push(probe);
    }

    /// Atomic snapshot of installed probe counts —
    /// `(locality, health, inventory)`. One read lock covers
    /// all three lists, so the triple is consistent even if a
    /// concurrent installer fires between them.
    pub fn probe_counts(&self) -> (usize, usize, usize) {
        let guard = self.inner.lists.read();
        (
            guard.locality.len(),
            guard.health.len(),
            guard.inventory.len(),
        )
    }

    /// Drop every installed [`LocalityProbe`]. Long-running
    /// runtimes that swap probe sources (test harnesses,
    /// hot-config reloaders) would otherwise accumulate dead
    /// probes that keep firing every Tick; this lets a caller
    /// detach the old set before installing replacements.
    pub fn clear_locality_probes(&self) {
        self.inner.lists.write().locality.clear();
    }

    /// Drop every installed [`HealthProbe`]. Same rationale as
    /// [`Self::clear_locality_probes`].
    pub fn clear_health_probes(&self) {
        self.inner.lists.write().health.clear();
    }

    /// Drop every installed [`super::probes::InventoryProbe`].
    /// Same rationale as [`Self::clear_locality_probes`]. Last-
    /// writer-wins per peer means a stale probe left installed
    /// can stomp a live replacement's samples, so callers
    /// swapping inventory sources should clear first.
    pub fn clear_inventory_probes(&self) {
        self.inner.lists.write().inventory.clear();
    }
}

/// Read-only handle to the loop's most recent snapshot.
/// Construction returns one of these from
/// [`MeshOsLoop::new`]; Deck / Phase F integrations clone the
/// handle (cheap — one Arc clone) and call
/// [`MeshOsSnapshotReader::read`] to sample the current view
/// without entering the loop's event stream.
#[derive(Clone, Debug)]
pub struct MeshOsSnapshotReader {
    snapshot: Arc<ArcSwap<MeshOsSnapshot>>,
}

impl MeshOsSnapshotReader {
    /// Sample the most recent post-reconcile snapshot. One
    /// atomic load + one `Arc` clone — no lock acquisition, so
    /// reads cannot stall the loop's publish path.
    pub fn read(&self) -> MeshOsSnapshot {
        (**self.snapshot.load()).clone()
    }

    /// Borrow the latest snapshot through an `Arc`. Avoids the
    /// per-call deep clone when the caller only needs a few
    /// fields. The returned guard pins the snapshot until
    /// dropped — keep the borrow short.
    pub fn load(&self) -> arc_swap::Guard<Arc<MeshOsSnapshot>> {
        self.snapshot.load()
    }
}

/// Cloneable handle for publishing events into the loop. Cheap
/// to clone (just clones the `mpsc::Sender`). Drop the last
/// handle to signal end-of-events; the loop will exit after
/// draining its current backlog.
#[derive(Clone, Debug)]
pub struct MeshOsHandle {
    events: mpsc::Sender<MeshOsEvent>,
}

impl MeshOsHandle {
    /// Publish an event into the loop's stream. Async — backs
    /// pressure when the source channel is at
    /// `event_queue_capacity`. Sources that need a fire-and-
    /// forget path can `try_send` directly on the underlying
    /// sender via `into_sender`.
    ///
    /// **Note on wedge risk:** if the loop is stuck (probe
    /// holding a sync lock, dispatcher saturated, etc.) this
    /// future never resolves. Long-lived sources that must
    /// remain responsive should prefer
    /// [`Self::publish_timeout`] or [`Self::try_publish`].
    pub async fn publish(&self, event: MeshOsEvent) -> Result<(), MeshOsHandleError> {
        self.events
            .send(event)
            .await
            .map_err(|_| MeshOsHandleError::LoopClosed)
    }

    /// Publish an event with a bounded wait. Returns
    /// [`MeshOsHandleError::QueueFull`] if the source channel
    /// stayed at capacity for the entire `timeout` window.
    /// Sources that can't afford to park indefinitely on a
    /// wedged loop should call this instead of [`Self::publish`].
    pub async fn publish_timeout(
        &self,
        event: MeshOsEvent,
        timeout: std::time::Duration,
    ) -> Result<(), MeshOsHandleError> {
        match tokio::time::timeout(timeout, self.events.send(event)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(MeshOsHandleError::LoopClosed),
            Err(_) => Err(MeshOsHandleError::QueueFull),
        }
    }

    /// Try to publish without awaiting. Returns
    /// `MeshOsHandleError::QueueFull` when the source channel is
    /// at capacity.
    pub fn try_publish(&self, event: MeshOsEvent) -> Result<(), MeshOsHandleError> {
        self.events.try_send(event).map_err(|e| match e {
            mpsc::error::TrySendError::Closed(_) => MeshOsHandleError::LoopClosed,
            mpsc::error::TrySendError::Full(_) => MeshOsHandleError::QueueFull,
        })
    }

    /// Hand out the underlying sender for sources that need to
    /// manage their own backpressure / batching.
    pub fn into_sender(self) -> mpsc::Sender<MeshOsEvent> {
        self.events
    }
}

/// Surface-side errors from [`MeshOsHandle::publish`] /
/// [`MeshOsHandle::try_publish`]. The loop is conservative —
/// callers decide whether to retry, drop, or apply their own
/// backpressure.
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MeshOsHandleError {
    /// The loop has exited; further publishes will be dropped.
    LoopClosed,
    /// The source channel is at `event_queue_capacity`. The
    /// caller picks: back off + retry, drop the event, or apply
    /// source-side backpressure.
    QueueFull,
}

/// The result of [`MeshOsLoop::new`]. Held together so callers
/// destructure the pieces they need; future fields (metrics,
/// chain handles) can be added without breaking the
/// constructor signature.
pub struct MeshOsLoopParts {
    /// The loop itself — consume by spawning [`MeshOsLoop::run`].
    pub mesh_loop: MeshOsLoop,
    /// Publish handle; clone for each source converter.
    pub handle: MeshOsHandle,
    /// Action-queue receiver, fed into [`super::executor::ActionExecutor::new`].
    pub actions_rx: mpsc::Receiver<PendingAction>,
    /// Snapshot reader; clone for each Deck / consumer.
    pub reader: MeshOsSnapshotReader,
}

impl MeshOsLoop {
    /// Construct a loop bound to the given config. Returns the
    /// loop + its publish handle + the action-queue receiver +
    /// the snapshot reader, bundled in [`MeshOsLoopParts`] so
    /// future additions don't break the constructor signature.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: MeshOsConfig) -> MeshOsLoopParts {
        let config = Arc::new(config);
        let (events_tx, events_rx) = mpsc::channel(config.event_queue_capacity);
        let (actions_tx, actions_rx) = mpsc::channel(config.action_queue_capacity);
        let handle = MeshOsHandle { events: events_tx };
        // Stamp a unique runtime epoch id at construction.
        // 64-bit random per-runtime stamp. Pre-fix this was
        // `SystemTime::now().as_nanos() ^ static_counter.fetch_add(1)`,
        // but the static counter resets to 1 each process start —
        // two processes booting in the same nanosecond (CI parallel,
        // VM resume) XOR identical `(epoch, counter)` and produced
        // identical runtime_epoch_ids. The SDK's watermark-reset
        // gate (snapshot's `runtime_epoch_id` vs last-seen) was then
        // defeated: post-restart admin_audit_seq / log_seq /
        // failure_seq start back at 1 and pass the consumer's dedup
        // gate as "already seen," silently filtering valid post-
        // restart audit records. A `getrandom::fill` u64 has a
        // 2⁻⁶⁴ collision probability across all process restarts
        // in the fleet. The fallback path on a getrandom failure
        // preserves the prior (epoch ^ counter) shape so the SDK
        // gate still gets a non-zero stamp under the (extremely
        // rare) getrandom failure mode rather than panicking
        // through the substrate's loop construction.
        let runtime_epoch_id: u64 = {
            let mut buf = [0u8; 8];
            if getrandom::fill(&mut buf).is_ok() {
                u64::from_le_bytes(buf)
            } else {
                static RUNTIME_EPOCH_COUNTER: AtomicU64 = AtomicU64::new(1);
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0)
                    ^ RUNTIME_EPOCH_COUNTER.fetch_add(1, Ordering::SeqCst)
            }
        };
        let initial_snapshot = MeshOsSnapshot {
            runtime_epoch_id,
            ..Default::default()
        };
        let snapshot = Arc::new(ArcSwap::from_pointee(initial_snapshot));
        let reader = MeshOsSnapshotReader {
            snapshot: Arc::clone(&snapshot),
        };
        let me = Self {
            config,
            events_rx,
            actions_tx,
            action_ids: AllocateActionId::new(),
            actual: MeshOsState::default(),
            desired: DesiredState::default(),
            snapshot,
            recent_emissions: Vec::new(),
            probes: ProbeRegistryInner::default(),
            scheduler: SchedulerRegistry::new(),
            recovery_registry: crate::adapter::net::compute::RecoveryRegistry::new(),
            reconcile_count: 0,
            admin_audit_seq: 0,
            log_seq: 0,
            runtime_epoch_id,
            admin_audit_ring: VecDeque::with_capacity(super::ice::DEFAULT_MAX_ADMIN_AUDIT_RECORDS),
            log_ring: VecDeque::with_capacity(super::logs::DEFAULT_MAX_LOG_RING_RECORDS),
            dropped_actions: Arc::new(AtomicU64::new(0)),
            executor_failures: None,
            executor_failure_seq: None,
            executor_failure_appender: None,
            control_sink: None,
            last_local_maintenance: MaintenanceDiscriminant::Active,
            admin_verifier: None,
            admin_audit_appender: super::audit_chain::no_op_arc(),
            log_appender: super::log_chain::no_op_arc(),
            migration_aborter: super::migration_aborter::no_op_arc(),
            migration_snapshot_source: super::migration_snapshot_source::no_op_arc(),
        };
        MeshOsLoopParts {
            mesh_loop: me,
            handle,
            actions_rx,
            reader,
        }
    }

    /// Clone the dropped-action counter. The runtime uses this
    /// to surface the count through `ExecutorStatsSnapshot`;
    /// tests can also assert against it directly.
    pub fn dropped_actions_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped_actions)
    }

    /// Attach a probe registry. The loop polls each registered
    /// probe on every Tick, before reconcile. The registry is
    /// shareable + cloneable, so callers retain it to add probes
    /// after `MeshOsLoop::new` returns (the loop has been moved
    /// into the spawned task at that point).
    pub fn with_probe_registry(mut self, registry: ProbeRegistry) -> Self {
        self.probes = registry.inner;
        self
    }

    /// Attach a scheduler registry. The reconcile pass reads
    /// the registered scorer to drive Phase D-1 rebalancing.
    /// Cloneable + shareable like the probe registry.
    pub fn with_scheduler_registry(mut self, registry: SchedulerRegistry) -> Self {
        self.scheduler = registry;
        self
    }

    /// Attach a recovery registry. SDK consumers register one
    /// `RecoveryHandler` per group whose slots can be
    /// re-placed; the tick handler runs them all once per Tick
    /// (after `poll_probes`, before `run_reconcile`) so the
    /// reconcile pass sees the recovered slot states.
    pub fn with_recovery_registry(
        mut self,
        registry: crate::adapter::net::compute::RecoveryRegistry,
    ) -> Self {
        self.recovery_registry = registry;
        self
    }

    /// Borrow the recovery registry — SDK consumers `register`
    /// new handlers post-build. The `MeshOsRuntime` accessor
    /// surfaces this so callers don't have to retain a clone.
    pub fn recovery_registry(&self) -> &crate::adapter::net::compute::RecoveryRegistry {
        &self.recovery_registry
    }

    /// Attach the executor's recent-failures ring. The loop reads
    /// it on every `publish_snapshot` so the snapshot's
    /// `recent_failures` field reflects executor-side dispatch
    /// failures (the `MeshOsSnapshotFold` chain-record path is
    /// not the only failure surface). The runtime calls this
    /// after `ActionExecutor::new` so both halves of the pair
    /// share the same ring.
    pub fn with_executor_failures(
        mut self,
        failures: Arc<RwLock<VecDeque<FailureRecord>>>,
    ) -> Self {
        self.executor_failures = Some(failures);
        self
    }

    /// Attach the executor's failure-seq counter + chain
    /// appender so the loop can record its own runtime-side
    /// failures (e.g. migration-abort dispatcher errors) with
    /// the same monotonic sequence + durable chain dual-write
    /// the executor uses. Pair this with
    /// [`Self::with_executor_failures`]; together the trio
    /// makes the loop's internal `record_runtime_failure`
    /// helper a complete dual write into the snapshot ring +
    /// the chain.
    pub fn with_executor_failure_writer(
        mut self,
        seq: Arc<AtomicU64>,
        appender: Arc<dyn super::failure_chain::FailureChainAppender>,
    ) -> Self {
        self.executor_failure_seq = Some(seq);
        self.executor_failure_appender = Some(appender);
        self
    }

    /// Attach a [`ControlSink`]. When set, the loop translates
    /// this-node maintenance state transitions into
    /// [`MeshOsControl`] events and forwards them through the
    /// sink. The SDK installs a sink that routes events to
    /// per-daemon control channels via its router; substrate
    /// code that doesn't need the SDK surface can leave this
    /// unset.
    pub fn with_control_sink(mut self, sink: Arc<dyn ControlSink>) -> Self {
        self.control_sink = Some(sink);
        self
    }

    /// Attach an [`super::ice::AdminVerifier`]. When set, every
    /// [`MeshOsEvent::SignedIceCommit`] is gated on signature
    /// verification + the cluster's signature threshold before
    /// folding the inner [`super::event::AdminEvent`]. Verified
    /// commits fold normally; rejected commits drop + emit a
    /// failure record so operators see the rejection in the
    /// snapshot's `recent_failures` ring (substrate slice that
    /// wires the failure pipe lands alongside the SDK surface
    /// upgrade).
    pub fn with_admin_verifier(mut self, verifier: Arc<super::ice::AdminVerifier>) -> Self {
        self.admin_verifier = Some(verifier);
        self
    }

    /// Attach a [`super::audit_chain::AdminAuditChainAppender`].
    /// The loop's `record_admin_audit` path dual-writes every
    /// admin commit to both the in-memory ring (snapshot
    /// readable) and this appender (chain-backed history).
    /// Without an explicit appender the loop uses the no-op
    /// default; only the in-memory ring is observable.
    pub fn with_admin_audit_appender(
        mut self,
        appender: Arc<dyn super::audit_chain::AdminAuditChainAppender>,
    ) -> Self {
        self.admin_audit_appender = appender;
        self
    }

    /// Attach a [`super::log_chain::LogChainAppender`]. The
    /// loop's `record_log_line` path dual-writes every log
    /// line to both the in-memory ring (snapshot readable)
    /// and this appender (chain-backed history). Without an
    /// explicit appender the loop uses the no-op default.
    pub fn with_log_appender(
        mut self,
        appender: Arc<dyn super::log_chain::LogChainAppender>,
    ) -> Self {
        self.log_appender = appender;
        self
    }

    /// Attach a [`super::migration_aborter::MigrationAborter`].
    /// The loop calls this after folding a verified
    /// [`super::event::AdminEvent::KillMigration`]; production
    /// deployments wire the
    /// [`super::migration_aborter::OrchestratorMigrationAborter`]
    /// adapter so the cluster's local `MigrationOrchestrator`
    /// actually aborts in-flight migrations. Without an
    /// explicit aborter the commit lands on the audit chain
    /// but the migration runs to completion.
    pub fn with_migration_aborter(
        mut self,
        aborter: Arc<dyn super::migration_aborter::MigrationAborter>,
    ) -> Self {
        self.migration_aborter = aborter;
        self
    }

    /// Attach a
    /// [`super::migration_snapshot_source::MigrationSnapshotSource`].
    /// The loop reads this on every snapshot publish and
    /// embeds the result in the snapshot's
    /// `in_flight_migrations` field — the ICE simulator
    /// reads it to enumerate which daemon a `KillMigration`
    /// target would affect.
    pub fn with_migration_snapshot_source(
        mut self,
        source: Arc<dyn super::migration_snapshot_source::MigrationSnapshotSource>,
    ) -> Self {
        self.migration_snapshot_source = source;
        self
    }

    /// Drive the loop until either:
    /// 1. all `MeshOsHandle` clones drop and the source channel
    ///    empties (graceful end-of-events), or
    /// 2. a `MeshOsEvent::Shutdown` is dequeued.
    ///
    /// Returns the final `reconcile_count` — used by tests; in
    /// production it's diagnostic-only.
    pub async fn run(mut self) -> u64 {
        tracing::debug!(
            target: "meshos",
            this_node = self.config.this_node,
            tick_interval_ms = self.config.tick_interval.as_millis() as u64,
            event_queue_capacity = self.config.event_queue_capacity,
            action_queue_capacity = self.config.action_queue_capacity,
            "MeshOsLoop starting",
        );
        // Tick timer — fires every `tick_interval`. Configured
        // to skip missed ticks rather than burst, since the
        // reconcile pass is the slow-and-steady cadence the plan
        // locks in.
        let mut tick = interval_at(
            TokioInstant::now() + self.config.tick_interval,
            self.config.tick_interval,
        );
        // `Delay` over `Skip`: under load the reconcile cadence
        // drifts but no tick is silently dropped, so the loop
        // never falls behind on probe + reconcile passes that
        // would surface stale state. `Burst` would amplify a
        // backlog, which is what the locked-decision-1 "one
        // pass per tick" guarantee is designed to prevent.
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // Bound how many events get drained between ticks so a
        // heavy reconcile + 100 ms tick can't starve `events_rx`
        // under the `biased;` priority below. The pre-fix
        // `biased; tick.tick() ⇒ events_rx.recv()` ordering
        // correctly defends event-burst-starves-tick (the original
        // bug), but in the other direction a long `run_reconcile`
        // that yields mid-await re-enters the select and tick
        // wins again — events sit in the queue until the queue
        // saturates the sender or the system idles. Drain up to
        // `EVENTS_PER_TICK` events in a non-blocking pass after
        // each tick so both directions make bounded progress.
        const EVENTS_PER_TICK: usize = 32;
        let mut shutdown = false;
        loop {
            tokio::select! {
                // `biased` polls arms in source order, so a
                // saturated `events_rx` cannot starve the reconcile
                // tick. The previous (random) order let a sustained
                // event burst defer reconcile / poll_probes / gc_freeze
                // for the duration of the burst — `local_maintenance`
                // went stale, `applied_backoffs` stuck, and
                // `freeze_until` never GC'd because gc_freeze only
                // runs on Tick.
                biased;
                _ = tick.tick() => {
                    // The Tick event drives reconcile; we route
                    // it through the same `apply` path so the
                    // `last_tick` fold field updates uniformly.
                    self.apply(&MeshOsEvent::Tick);
                    // Pull-via-tick probes — folded BEFORE
                    // reconcile so the reconcile pass sees the
                    // latest samples in this tick window.
                    self.poll_probes();
                    // X-13: run the recovery handlers between
                    // probe samples and reconcile. Probes refresh
                    // the per-peer healthy view, then recovery
                    // re-places any unhealthy slots against the
                    // current healthy node pool, then reconcile
                    // sees the post-recovery state. Empty
                    // registry → no-op; handlers that report
                    // recovered slots are logged at debug for
                    // operator visibility.
                    let recovered = self.recovery_registry.try_run_all();
                    if !recovered.is_empty() {
                        tracing::debug!(
                            target: "meshos",
                            slots = ?recovered,
                            "X-13: recovery registry placed unhealthy slots"
                        );
                    }
                    self.run_reconcile().await;

                    // Drain a bounded batch of events from the
                    // queue before the next tick can re-fire. This
                    // is the inverse-direction starvation guard
                    // for the `biased;` above. `try_recv` is non-
                    // blocking, so an empty queue costs one Err
                    // each iteration and the loop falls through to
                    // the next select.
                    for _ in 0..EVENTS_PER_TICK {
                        match self.events_rx.try_recv() {
                            Ok(event) => {
                                if matches!(event, MeshOsEvent::Shutdown) {
                                    tracing::debug!(
                                        target: "meshos",
                                        reconcile_count = self.reconcile_count,
                                        "MeshOsLoop exiting — Shutdown drained post-tick",
                                    );
                                    shutdown = true;
                                    break;
                                }
                                self.apply(&event);
                            }
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                tracing::debug!(
                                    target: "meshos",
                                    reconcile_count = self.reconcile_count,
                                    "MeshOsLoop exiting — all handles dropped (drained post-tick)",
                                );
                                shutdown = true;
                                break;
                            }
                        }
                    }
                    if shutdown {
                        break;
                    }
                }
                event = self.events_rx.recv() => {
                    let Some(event) = event else {
                        tracing::debug!(
                            target: "meshos",
                            reconcile_count = self.reconcile_count,
                            "MeshOsLoop exiting — all handles dropped",
                        );
                        break;
                    };
                    if matches!(event, MeshOsEvent::Shutdown) {
                        tracing::debug!(
                            target: "meshos",
                            reconcile_count = self.reconcile_count,
                            "MeshOsLoop exiting — Shutdown event received",
                        );
                        break;
                    }
                    self.apply(&event);
                }
            }
        }

        self.reconcile_count
    }

    /// Poll every registered locality / health probe and fold
    /// the samples into the actual-state view. Idempotent — the
    /// folds overwrite per-peer entries, so a re-poll within
    /// the same tick produces the same state.
    ///
    /// Each probe call runs inside `catch_unwind` so a panicking
    /// probe doesn't unwind the loop task. Probes are
    /// third-party-installed by definition (the substrate hands
    /// out the `ProbeRegistry`), so trust-but-isolate is the
    /// right posture.
    fn poll_probes(&mut self) {
        // Clone all probe lists out under a single read lock so
        // a concurrent install between the locality / health /
        // inventory passes doesn't see a half-applied registry.
        let (locality, health, inventory) = {
            let guard = self.probes.lists.read();
            (
                guard.locality.clone(),
                guard.health.clone(),
                guard.inventory.clone(),
            )
        };
        for probe in &locality {
            let probe = Arc::clone(probe);
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe.rtt_samples()));
            match result {
                Ok(samples) => {
                    for (peer, rtt) in samples {
                        self.actual.rtt.insert(peer, rtt);
                    }
                }
                Err(_) => {
                    tracing::error!(
                        target: "meshos",
                        "locality probe panicked — sample skipped this tick",
                    );
                }
            }
        }
        for probe in &health {
            let probe = Arc::clone(probe);
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe.health_samples()));
            match result {
                Ok(samples) => {
                    for (peer, hc) in samples {
                        self.actual.node_health.insert(peer, hc);
                    }
                }
                Err(_) => {
                    tracing::error!(
                        target: "meshos",
                        "health probe panicked — sample skipped this tick",
                    );
                }
            }
        }
        // Track inventory samples seen this tick so the GC pass
        // below drops entries for peers no probe sees anymore.
        // The map is probe-exclusive (no `MeshOsEvent` populates
        // it), so it's safe to authoritatively prune from the
        // probe pass. The rtt + node_health maps are also fed by
        // `MeshOsEvent::{RttSample, NodeHealth}` event folds, so
        // their per-tick prune would erase event-driven samples;
        // those maps grow with proximity churn but the inventory
        // leak the review flagged was per-peer multi-field and
        // strictly worse.
        let mut peers_seen_inventory: std::collections::HashSet<super::event::NodeId> =
            std::collections::HashSet::new();
        let mut all_probes_succeeded = true;
        let mut any_probe_saw_samples = false;
        for probe in &inventory {
            let probe = Arc::clone(probe);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                probe.inventory_samples()
            }));
            match result {
                Ok(samples) => {
                    if !samples.is_empty() {
                        any_probe_saw_samples = true;
                    }
                    for (peer, inv) in samples {
                        peers_seen_inventory.insert(peer);
                        self.actual.inventory.insert(peer, inv);
                    }
                }
                Err(_) => {
                    all_probes_succeeded = false;
                    tracing::error!(
                        target: "meshos",
                        "inventory probe panicked — sample skipped this tick",
                    );
                }
            }
        }
        // Two-axis guard for the GC pass:
        //
        //   * `all_probes_succeeded` — a partial-panic pass
        //     can leave `peers_seen_inventory` short of peers
        //     that the panicking probe owned authoritatively
        //     (e.g. two inventory probes with disjoint
        //     coverage); pruning then drops the panicking
        //     probe's peers despite no probe knowing they
        //     departed. Wait for the next clean tick instead.
        //
        //   * `any_probe_saw_samples` — the trait contract
        //     allows a probe to return `Ok(vec![])` for a
        //     transient empty-this-tick (procfs unavailable,
        //     no peers known yet at startup); pruning on an
        //     all-empty pass would wipe every previously-seen
        //     peer's inventory. The first tick at startup
        //     legitimately has no peers, so this also
        //     prevents a cold-start wipe.
        if all_probes_succeeded && any_probe_saw_samples {
            self.actual
                .inventory
                .retain(|peer, _| peers_seen_inventory.contains(peer));
        }
    }

    fn apply(&mut self, event: &MeshOsEvent) {
        // ICE-signed commits get their own gate: verify the
        // bundle before folding the inner AdminEvent. The
        // outcome (accepted, rejected, unverified) lands on the
        // admin_audit ring regardless of verification result so
        // security review can replay every attempt.
        if let MeshOsEvent::SignedIceCommit {
            proposal,
            signatures,
            issued_at_ms,
            blast_hash,
        } = event
        {
            let outcome = match self.admin_verifier.as_ref() {
                Some(verifier) => {
                    let now_ms = super::ice::now_ms_since_unix_epoch();
                    match verifier.verify_commit(
                        proposal,
                        signatures,
                        *issued_at_ms,
                        blast_hash,
                        now_ms,
                    ) {
                        Ok(()) => super::ice::VerificationOutcome::Accepted,
                        Err(err) => super::ice::VerificationOutcome::Rejected {
                            kind: err.kind().to_string(),
                            message: err.to_string(),
                        },
                    }
                }
                None => super::ice::VerificationOutcome::Unverified,
            };
            let admin_event = proposal.to_admin_event();
            let operator_ids: Vec<u64> = signatures.iter().map(|s| s.operator_id).collect();
            self.record_admin_audit(&admin_event, operator_ids, outcome.clone());
            if let super::ice::VerificationOutcome::Rejected { kind, message } = &outcome {
                tracing::warn!(
                    target: "meshos",
                    kind = %kind,
                    error = %message,
                    "rejected SignedIceCommit — signature verification failed",
                );
                return;
            }
            // Verification passed (or no verifier installed —
            // tests / dev mode). Fold as if the inner AdminEvent
            // arrived directly. Dispatch fires AFTER actual.apply
            // so the dispatcher observes post-fold state — the
            // ordering invariant the loop preserves for every
            // chain-committed admin event.
            self.desired
                .apply_admin(&admin_event, self.config.this_node);
            let unwrapped = MeshOsEvent::AdminEvent(admin_event.clone());
            self.actual.apply(&unwrapped, self.config.this_node);
            self.dispatch_kill_migration_if_applicable(&admin_event);
            self.emit_maintenance_transitions();
            return;
        }
        // Single-signature signed-admin commits mirror the ICE
        // path: verify, audit, fold on success. The signature
        // covers `admin_event_signing_payload(event)` so the
        // SDK and substrate agree on the byte sequence.
        if let MeshOsEvent::SignedAdminCommit {
            event: admin_event,
            signature,
            issued_at_ms,
        } = event
        {
            // Freeze gate: ordinary admin commits that arrive
            // during an in-effect cluster freeze land on the
            // audit ring as Rejected with kind
            // "freeze_in_effect" and the inner event drops. ICE
            // commits (the multi-op SignedIceCommit path) bypass
            // by design — operators must be able to thaw the
            // cluster mid-freeze.
            let now = self
                .actual
                .last_tick
                .unwrap_or_else(std::time::Instant::now);
            if self.actual.is_frozen(now) && !admin_event.is_ice() {
                self.record_admin_audit(
                    admin_event,
                    vec![signature.operator_id],
                    super::ice::VerificationOutcome::Rejected {
                        kind: "freeze_in_effect".to_string(),
                        message: "ordinary admin commits are gated during a cluster freeze; \
                                  thaw via the ICE surface to unblock"
                            .to_string(),
                    },
                );
                tracing::warn!(
                    target: "meshos",
                    kind = "freeze_in_effect",
                    "rejected SignedAdminCommit — cluster is frozen and the event is non-ICE",
                );
                return;
            }
            let outcome = match self.admin_verifier.as_ref() {
                Some(verifier) => {
                    let now_ms = super::ice::now_ms_since_unix_epoch();
                    match verifier.verify_admin_commit(
                        admin_event,
                        signature,
                        *issued_at_ms,
                        now_ms,
                    ) {
                        Ok(()) => super::ice::VerificationOutcome::Accepted,
                        Err(err) => super::ice::VerificationOutcome::Rejected {
                            kind: err.kind().to_string(),
                            message: err.to_string(),
                        },
                    }
                }
                None => super::ice::VerificationOutcome::Unverified,
            };
            self.record_admin_audit(admin_event, vec![signature.operator_id], outcome.clone());
            if let super::ice::VerificationOutcome::Rejected { kind, message } = &outcome {
                tracing::warn!(
                    target: "meshos",
                    kind = %kind,
                    error = %message,
                    "rejected SignedAdminCommit — signature verification failed",
                );
                return;
            }
            self.desired.apply_admin(admin_event, self.config.this_node);
            let unwrapped = MeshOsEvent::AdminEvent(admin_event.clone());
            self.actual.apply(&unwrapped, self.config.this_node);
            self.dispatch_kill_migration_if_applicable(admin_event);
            self.emit_maintenance_transitions();
            return;
        }
        // Log lines bypass the actual/desired fold entirely;
        // the loop stamps + pushes onto the log ring directly.
        if let MeshOsEvent::LogLine(line) = event {
            self.record_log_line(line);
            return;
        }
        match event {
            MeshOsEvent::PlacementIntent(intent) => self.desired.apply(intent),
            MeshOsEvent::DaemonIntentUpdate(update) => self.desired.apply_daemon_intent(update),
            MeshOsEvent::LocalReplicaIntent(update) => {
                self.desired.apply_local_replica_intent(update)
            }
            MeshOsEvent::AdminEvent(admin) => {
                // Freeze gate, same as the SignedAdminCommit
                // path: ordinary admin events drop during a
                // cluster freeze. ICE events bypass.
                let now = self
                    .actual
                    .last_tick
                    .unwrap_or_else(std::time::Instant::now);
                if self.actual.is_frozen(now) && !admin.is_ice() {
                    self.record_admin_audit(
                        admin,
                        Vec::new(),
                        super::ice::VerificationOutcome::Rejected {
                            kind: "freeze_in_effect".to_string(),
                            message: "ordinary admin commits are gated during a cluster freeze; \
                                      thaw via the ICE surface to unblock"
                                .to_string(),
                        },
                    );
                    tracing::warn!(
                        target: "meshos",
                        kind = "freeze_in_effect",
                        "rejected unsigned AdminEvent — cluster is frozen and the event is non-ICE",
                    );
                    return;
                }
                // Unsigned admin commits also land on the audit
                // ring with Unverified outcome — so audit
                // consumers see every admin attempt the cluster
                // observed, not just signed ICE bundles.
                self.record_admin_audit(
                    admin,
                    Vec::new(),
                    super::ice::VerificationOutcome::Unverified,
                );
                self.desired.apply_admin(admin, self.config.this_node);
            }
            _ => {}
        }
        self.actual.apply(event, self.config.this_node);
        // KillMigration / future dispatchers fire AFTER
        // actual.apply so they observe post-fold state. Lifted
        // out of the AdminEvent arm above for that ordering.
        if let MeshOsEvent::AdminEvent(admin) = event {
            self.dispatch_kill_migration_if_applicable(admin);
        }
        self.emit_maintenance_transitions();
    }

    /// Record an admin-commit outcome on the state's audit
    /// ring. Bounded by
    /// [`super::ice::DEFAULT_MAX_ADMIN_AUDIT_RECORDS`]; drops
    /// oldest FIFO when the cap is exceeded.
    fn record_admin_audit(
        &mut self,
        event: &super::event::AdminEvent,
        operator_ids: Vec<u64>,
        outcome: super::ice::VerificationOutcome,
    ) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let committed_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // Stamp a monotonic per-runtime seq starting at 1 so a
        // `0` seq downstream reads as "unset." Saturating add so
        // a runaway wrap at u64::MAX preserves the monotonic
        // invariant the SDK dedup gate keys on; collision at
        // wrapped seq=1 would silently drop the new record.
        self.admin_audit_seq = self.admin_audit_seq.saturating_add(1);
        let record = super::ice::AdminAuditRecord {
            seq: self.admin_audit_seq,
            committed_at_ms,
            event: event.clone(),
            operator_ids,
            outcome,
            chain_pending: false,
        };
        // Ring-first then chain. The ring is the immediate
        // user-visible surface that the Deck SDK polls; the chain
        // is durability backup for cluster-lifetime replay. If the
        // chain append fails, mark the ring entry's chain_pending
        // flag so chain consumers (replaying after restart) can
        // distinguish "entry never landed" from "entry hasn't
        // reached me yet." Pre-fix the chain attempt ran BEFORE
        // the ring push and the ring entry never recorded the
        // chain-side outcome — chain consumers saw a gap with no
        // way to know it was permanent.
        self.admin_audit_ring.push_back(record.clone());
        while self.admin_audit_ring.len() > super::ice::DEFAULT_MAX_ADMIN_AUDIT_RECORDS {
            self.admin_audit_ring.pop_front();
        }
        if let Err(err) = self.admin_audit_appender.append(&record) {
            tracing::warn!(
                target: "meshos",
                seq = record.seq,
                error = %err,
                "admin-audit-chain append failed — ring entry marked chain_pending",
            );
            // Locate the just-pushed entry (back of the ring; cap
            // protects against an empty back() but we just pushed)
            // and flip its chain_pending flag so the ring surface
            // reports the gap.
            if let Some(last) = self.admin_audit_ring.back_mut() {
                last.chain_pending = true;
            }
        }
    }

    /// Stamp + push a `LogLine` onto the per-node log ring.
    /// Bounded by [`super::logs::DEFAULT_MAX_LOG_RING_RECORDS`];
    /// drops oldest FIFO when the cap is exceeded.
    fn record_log_line(&mut self, line: &super::logs::LogLine) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // Saturating add mirrors admin_audit_seq above; preserves
        // monotonicity past the (astronomical) u64::MAX boundary
        // so the SDK dedup key never collides via wrap.
        self.log_seq = self.log_seq.saturating_add(1);
        let record = super::logs::LogRecord {
            seq: self.log_seq,
            ts_ms,
            level: line.level,
            daemon_id: line.daemon_id,
            node_id: Some(self.config.this_node),
            message: line.message.clone(),
            chain_pending: false,
        };
        // Ring-first then chain. Same rationale as record_admin_audit:
        // the ring is the immediate user surface; the chain is
        // durability backup. Mark chain_pending on the just-pushed
        // ring entry when the chain append fails so chain consumers
        // can distinguish "gap" from "haven't replicated yet."
        self.log_ring.push_back(record.clone());
        while self.log_ring.len() > super::logs::DEFAULT_MAX_LOG_RING_RECORDS {
            self.log_ring.pop_front();
        }
        if let Err(err) = self.log_appender.append(&record) {
            tracing::warn!(
                target: "meshos",
                seq = record.seq,
                error = %err,
                "log-chain append failed — ring entry marked chain_pending",
            );
            if let Some(last) = self.log_ring.back_mut() {
                last.chain_pending = true;
            }
        }
    }

    /// Route a verified [`super::event::AdminEvent::KillMigration`]
    /// to the installed
    /// [`super::migration_aborter::MigrationAborter`]. No-op for
    /// every other admin variant — the match guards the lookup so
    /// callers can invoke this unconditionally after fold without
    /// re-pattern-matching. Errors are logged + swallowed; a
    /// dispatcher hiccup must never wedge the loop.
    fn dispatch_kill_migration_if_applicable(&self, admin: &super::event::AdminEvent) {
        let super::event::AdminEvent::KillMigration { migration } = admin else {
            return;
        };
        // No-op aborter installed with a verifier wired — this
        // is "production-partial" config: the chain commit
        // landed but the migration runs to completion because
        // the aborter is a no-op. Surface as a FailureRecord
        // so operators reading subscribe_failures see it.
        if self.migration_aborter.is_no_op() && self.admin_verifier.is_some() {
            self.record_runtime_failure(
                format!("kill-migration:{}", *migration),
                "no-op migration aborter installed while admin verifier is wired — \
                 chain commit landed but KillMigration is a no-op on this node"
                    .to_string(),
            );
            return;
        }
        if let Err(err) = self.migration_aborter.abort(*migration) {
            // Dispatch failure: log AND push to the failure
            // ring so the Deck SDK's subscribe_failures stream
            // surfaces it. A previous slice swallowed this
            // failure to a tracing::warn! only.
            tracing::warn!(
                target: "meshos",
                migration = migration,
                error = %err,
                "migration-abort dispatcher failed — chain commit landed but migration may continue",
            );
            self.record_runtime_failure(
                format!("kill-migration:{}", *migration),
                format!("migration-abort dispatcher error: {err}"),
            );
        }
    }

    /// Push a runtime-side failure onto the executor's failure
    /// ring + chain. Used by the loop for failures that don't
    /// originate from an action dispatch — e.g. migration-abort
    /// dispatcher errors after a `KillMigration` commit. Falls
    /// back to a `tracing::warn!` when the executor handles
    /// aren't wired (in-process tests that don't run the
    /// executor task).
    fn record_runtime_failure(&self, source: String, reason: String) {
        let recorded_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let seq = match self.executor_failure_seq.as_ref() {
            Some(s) => s.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1,
            None => {
                tracing::warn!(
                    target: "meshos",
                    source = %source,
                    reason = %reason,
                    "runtime-side failure surfaced but executor handles aren't wired",
                );
                return;
            }
        };
        let record = FailureRecord {
            seq,
            source,
            reason,
            recorded_at_ms,
        };
        if let Some(appender) = self.executor_failure_appender.as_ref() {
            if let Err(err) = appender.append(&record) {
                tracing::warn!(
                    target: "meshos",
                    seq = record.seq,
                    error = %err,
                    "failure-chain append failed — record kept on in-memory ring only",
                );
            }
        }
        if let Some(ring) = self.executor_failures.as_ref() {
            let mut g = ring.write();
            if g.len() >= super::snapshot::RECENT_FAILURES_CAPACITY {
                g.pop_front();
            }
            g.push_back(record);
        }
    }

    /// Detect `local_maintenance` discriminant transitions and
    /// fan-out the corresponding [`MeshOsControl`] through the
    /// installed sink. Idempotent: re-entering the same
    /// discriminant emits nothing. Forward arcs only — the fold
    /// rejects late-arriving backward arcs upstream.
    fn emit_maintenance_transitions(&mut self) {
        let Some(sink) = self.control_sink.as_ref() else {
            self.last_local_maintenance =
                MaintenanceDiscriminant::from_state(&self.actual.local_maintenance);
            return;
        };
        let current = MaintenanceDiscriminant::from_state(&self.actual.local_maintenance);
        if current == self.last_local_maintenance {
            return;
        }
        // Anchor the fall-back deadline on the actual state's
        // last_tick so loop-replay produces identical deadlines.
        // Falling back to wall-clock would break the
        // replay-convergence contract the fold side already
        // pins on `last_tick`.
        let anchor = self
            .actual
            .last_tick
            .unwrap_or_else(std::time::Instant::now);
        let event = match (self.last_local_maintenance, current) {
            (_, MaintenanceDiscriminant::EnteringMaintenance) => {
                // Operator opened a drain. Use the configured
                // deadline if the admin event carried one; else
                // fall back to the maintenance-config default.
                let deadline = match &self.actual.local_maintenance {
                    MaintenanceState::EnteringMaintenance {
                        deadline: Some(d), ..
                    } => *d,
                    _ => anchor + self.config.maintenance.default_drain_deadline,
                };
                Some(MeshOsControl::DrainStart { deadline })
            }
            (
                MaintenanceDiscriminant::EnteringMaintenance,
                MaintenanceDiscriminant::Maintenance | MaintenanceDiscriminant::DrainFailed,
            ) => Some(MeshOsControl::DrainFinish),
            _ => None,
        };
        self.last_local_maintenance = current;
        if let Some(event) = event {
            sink.emit(event);
        }
    }

    async fn run_reconcile(&mut self) {
        // Anchor every per-tick timestamp on `last_tick` (set by
        // `apply(Tick)` immediately before this call) so two
        // replays of the same event stream produce identical
        // `last_rebalance`, `applied_backoffs`, and
        // `PendingAction.emitted_at` values. Bootstrap fallback to
        // `Instant::now()` only when no Tick has fired yet.
        let now = self
            .actual
            .last_tick
            .unwrap_or_else(std::time::Instant::now);
        let scorer = self.scheduler.current();
        let actions = reconcile(
            &self.actual,
            &self.desired,
            self.config.this_node,
            &self.config.locality,
            &self.config.maintenance,
            &self.config.scheduler,
            scorer.as_deref(),
        );
        // Record cooldowns for any RequestEviction we emit so
        // the same chain doesn't flap on the next tick; track
        // `ApplyBackoff` emissions so reconcile suppresses
        // re-emit while the daemon stays in the same backoff
        // window.
        for action in &actions {
            match action {
                super::action::MeshOsAction::RequestEviction { chain, .. } => {
                    self.actual.last_rebalance.insert(*chain, now);
                }
                super::action::MeshOsAction::ApplyBackoff { daemon, until } => {
                    self.actual.applied_backoffs.insert(daemon.clone(), *until);
                }
                _ => {}
            }
        }
        // Drain queued force-evictions: reconcile consumed them
        // (whether or not this node is the leader for each
        // chain). Clearing here unconditionally keeps non-leader
        // observers from accumulating stale entries.
        if !self.actual.forced_evictions.is_empty() {
            self.actual.forced_evictions.clear();
        }
        // Same pattern for force-cutovers — every node drains
        // unconditionally so non-leader observers don't
        // accumulate stale entries.
        if !self.actual.forced_placements.is_empty() {
            self.actual.forced_placements.clear();
        }
        self.reconcile_count += 1;
        let mut dropped_this_tick: u64 = 0;
        let mut first_dropped_kind: Option<&'static str> = None;
        for action in actions {
            let pending = PendingAction {
                id: self.action_ids.next(),
                action,
                emitted_at: now,
            };
            // Drop on backpressure rather than block reconcile —
            // the executor's job is to apply admit(); reconcile
            // staying responsive is the higher-order property.
            // Count + log drops so the silent-loss path is
            // observable. recent_emissions only gets the action
            // on try_send success — pre-fix every action landed in
            // recent_emissions (and surfaced as `recently_emitted`
            // on the snapshot) even when the executor's queue was
            // full and the action would never run, double-counting
            // the drop in dropped_actions while painting a misleading
            // "emitted" picture in the snapshot.
            let pending_clone = pending.clone();
            match self.actions_tx.try_send(pending) {
                Ok(()) => self.recent_emissions.push(pending_clone),
                Err(mpsc::error::TrySendError::Full(rejected)) => {
                    dropped_this_tick += 1;
                    if first_dropped_kind.is_none() {
                        first_dropped_kind =
                            Some(super::snapshot::action_kind_str(&rejected.action));
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Executor task is gone — count as dropped and
                    // skip recent_emissions; subsequent reconciles
                    // will keep counting until the loop tears down.
                    dropped_this_tick += 1;
                }
            }
        }
        if dropped_this_tick > 0 {
            self.dropped_actions
                .fetch_add(dropped_this_tick, Ordering::Relaxed);
            tracing::warn!(
                target: "meshos",
                dropped = dropped_this_tick,
                first_kind = first_dropped_kind.unwrap_or("?"),
                queue_capacity = self.config.action_queue_capacity,
                "reconcile output dropped — action queue full",
            );
        }
        self.publish_snapshot();
        // Bound the in-loop pending mirror so a backed-up
        // executor doesn't let snapshot pending grow unbounded.
        // Action queue capacity is the natural bound.
        if self.recent_emissions.len() > self.config.action_queue_capacity {
            let overflow = self.recent_emissions.len() - self.config.action_queue_capacity;
            self.recent_emissions.drain(..overflow);
        }
    }

    fn publish_snapshot(&self) {
        // Read the executor's failures ring under a short read
        // lock and copy it into the snapshot. The lock is held
        // only across the clone to keep the executor's write
        // path (record_failure) responsive.
        let failures: Vec<FailureRecord> = match self.executor_failures.as_ref() {
            Some(ring) => ring.read().iter().cloned().collect(),
            None => Vec::new(),
        };
        let in_flight_migrations = self.migration_snapshot_source.list();
        let mut snap = MeshOsSnapshot::from_state(
            &self.actual,
            &self.desired,
            &self.recent_emissions,
            &failures,
            in_flight_migrations,
            &self.admin_audit_ring,
            &self.log_ring,
            self.config.this_node,
        );
        snap.runtime_epoch_id = self.runtime_epoch_id;
        self.snapshot.store(Arc::new(snap));
    }
}

/// Convenience: a config with a very short `tick_interval` for
/// tests, so the reconcile pass fires quickly. Not exported
/// outside the crate.
#[cfg(test)]
pub(crate) fn fast_test_config() -> MeshOsConfig {
    MeshOsConfig {
        this_node: 1,
        tick_interval: std::time::Duration::from_millis(10),
        event_queue_capacity: 64,
        action_queue_capacity: 64,
        backpressure: Default::default(),
        locality: Default::default(),
        maintenance: Default::default(),
        scheduler: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use super::super::event::{
        ChainId, LocalReplicaIntent, LocalReplicaIntentUpdate, MeshOsEvent, ReplicaUpdate,
    };
    use super::*;

    #[tokio::test]
    async fn loop_exits_cleanly_when_all_handles_drop() {
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            mut actions_rx,
            reader: _,
        } = MeshOsLoop::new(fast_test_config());
        let task = tokio::spawn(loop_.run());
        drop(handle);
        // Loop should drain quickly. `run` returns the
        // reconcile count.
        let count = tokio::time::timeout(StdDuration::from_secs(1), task)
            .await
            .expect("loop did not exit after all handles dropped")
            .expect("join");
        // Zero or more ticks may have fired before we dropped;
        // assert we got at least zero (compiles + ran).
        let _ = count;
        // The action queue must be empty under Phase A.
        assert!(actions_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn loop_exits_on_shutdown_event() {
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_test_config());
        let task = tokio::spawn(loop_.run());
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let count = tokio::time::timeout(StdDuration::from_secs(1), task)
            .await
            .expect("loop did not exit after Shutdown")
            .expect("join");
        let _ = count;
    }

    #[tokio::test]
    async fn publish_timeout_returns_queue_full_when_loop_is_wedged() {
        // Regression for I11: `publish` parks indefinitely on a
        // wedged loop. `publish_timeout` surfaces
        // QueueFull after the configured window so sources don't
        // stall.
        let cfg = MeshOsConfig {
            event_queue_capacity: 1,
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: _loop_,
            handle,
            actions_rx: _actions_rx,
            reader: _reader,
        } = MeshOsLoop::new(cfg);
        // Don't spawn the loop — handle is alive but the
        // receiver is parked inside `_loop_`. First publish
        // fills the capacity-1 channel; second blocks
        // indefinitely. publish_timeout surfaces QueueFull.
        handle
            .publish(MeshOsEvent::Tick)
            .await
            .expect("first send fits in the single-slot channel");
        let started = std::time::Instant::now();
        let err = handle
            .publish_timeout(MeshOsEvent::Tick, StdDuration::from_millis(50))
            .await
            .expect_err("second send should time out");
        assert!(matches!(err, MeshOsHandleError::QueueFull));
        assert!(
            started.elapsed() < StdDuration::from_millis(500),
            "publish_timeout must honor its budget",
        );
    }

    #[tokio::test]
    async fn panicking_probe_does_not_kill_the_loop() {
        // Regression for I6: a panicking probe used to unwind
        // the loop task. The catch_unwind wrapper in
        // `poll_probes` now logs + skips the bad sample so
        // reconcile keeps running.
        struct PanickyProbe;
        impl super::super::probes::LocalityProbe for PanickyProbe {
            fn rtt_samples(&self) -> Vec<(u64, std::time::Duration)> {
                panic!("boom from probe");
            }
        }
        let registry = ProbeRegistry::new();
        registry.add_locality_probe(Arc::new(PanickyProbe));
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(10),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _actions_rx,
            reader: _reader,
        } = MeshOsLoop::new(cfg);
        let loop_ = loop_.with_probe_registry(registry);
        let task = tokio::spawn(loop_.run());
        // Let several ticks fire — each one will invoke the
        // panicking probe.
        tokio::time::sleep(StdDuration::from_millis(60)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let count = tokio::time::timeout(StdDuration::from_secs(2), task)
            .await
            .expect("loop should still exit cleanly")
            .expect("loop task survived probe panics");
        assert!(
            count >= 1,
            "loop should have completed at least one reconcile pass despite probe panics",
        );
    }

    #[tokio::test]
    async fn inventory_gc_drops_peers_no_probe_samples_anymore() {
        // poll_probes used to insert per-peer inventory samples
        // and never remove them. A peer that departed from
        // proximity would leak into actual.inventory forever,
        // surfacing in every snapshot until the process
        // restarted. Pin the per-tick GC: an inventory probe
        // whose sample set shrinks across ticks should leave
        // actual.inventory pruned to match.
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use std::sync::Mutex;
        struct ShrinkingProbe {
            tick: AtomicUsize,
            // Mutex so the trait method can mutate freely
            // without &mut self.
            samples_per_tick: Mutex<Vec<Vec<(u64, super::super::probes::PeerInventory)>>>,
        }
        impl super::super::probes::InventoryProbe for ShrinkingProbe {
            fn inventory_samples(&self) -> Vec<(u64, super::super::probes::PeerInventory)> {
                let t = self.tick.fetch_add(1, AOrdering::Relaxed);
                let guard = self.samples_per_tick.lock().unwrap();
                // Past the end of the scripted set, stick on the
                // final entry so the steady state stays observed.
                let idx = t.min(guard.len().saturating_sub(1));
                guard.get(idx).cloned().unwrap_or_default()
            }
        }
        let inv_with_cpu = |cpu: f64| super::super::probes::PeerInventory {
            cpu_load_1m: Some(cpu),
            ..Default::default()
        };
        let probe = Arc::new(ShrinkingProbe {
            tick: AtomicUsize::new(0),
            samples_per_tick: Mutex::new(vec![
                vec![
                    (0xA, inv_with_cpu(0.1)),
                    (0xB, inv_with_cpu(0.2)),
                    (0xC, inv_with_cpu(0.3)),
                ],
                vec![(0xB, inv_with_cpu(0.2)), (0xC, inv_with_cpu(0.3))],
                vec![(0xB, inv_with_cpu(0.2)), (0xC, inv_with_cpu(0.3))],
            ]),
        });
        let registry = ProbeRegistry::new();
        registry.add_inventory_probe(probe);
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(10),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _actions_rx,
            reader,
        } = MeshOsLoop::new(cfg);
        let loop_ = loop_.with_probe_registry(registry);
        let task = tokio::spawn(loop_.run());
        // Wait long enough for at least two ticks to fire so
        // the second sample set (without peer 0xA) lands.
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        let snap = reader.read();
        let inv_peers: std::collections::BTreeSet<u64> = snap
            .peers
            .iter()
            .filter(|(_, p)| p.cpu_load_1m.is_some())
            .map(|(id, _)| *id)
            .collect();
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), task).await;
        assert!(
            !inv_peers.contains(&0xA),
            "peer 0xA stopped being sampled but still appears in the inventory: {inv_peers:?}",
        );
        assert!(inv_peers.contains(&0xB), "peer 0xB should still be sampled");
        assert!(inv_peers.contains(&0xC), "peer 0xC should still be sampled");
    }

    #[tokio::test]
    async fn inventory_gc_does_not_wipe_peers_when_one_of_two_probes_panics() {
        // Multi-probe partial-panic case: probe A reports peer
        // 0xA, probe B reports peer 0xB. On tick 2, probe B
        // panics; the GC pass must not drop 0xB just because A
        // didn't sample it. Without the `all_probes_succeeded`
        // guard, `peers_seen_inventory = {0xA}` and `retain`
        // would wipe 0xB even though no probe authoritatively
        // observed its departure.
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        struct SteadyProbe {
            peer: u64,
            cpu: f64,
        }
        impl super::super::probes::InventoryProbe for SteadyProbe {
            fn inventory_samples(&self) -> Vec<(u64, super::super::probes::PeerInventory)> {
                vec![(
                    self.peer,
                    super::super::probes::PeerInventory {
                        cpu_load_1m: Some(self.cpu),
                        ..Default::default()
                    },
                )]
            }
        }
        struct SometimesPanickingProbe {
            tick: AtomicUsize,
            peer: u64,
            cpu: f64,
        }
        impl super::super::probes::InventoryProbe for SometimesPanickingProbe {
            fn inventory_samples(&self) -> Vec<(u64, super::super::probes::PeerInventory)> {
                let t = self.tick.fetch_add(1, AOrdering::Relaxed);
                if t == 0 {
                    vec![(
                        self.peer,
                        super::super::probes::PeerInventory {
                            cpu_load_1m: Some(self.cpu),
                            ..Default::default()
                        },
                    )]
                } else {
                    panic!("probe B panicked on tick {t}");
                }
            }
        }
        let registry = ProbeRegistry::new();
        registry.add_inventory_probe(Arc::new(SteadyProbe {
            peer: 0xA,
            cpu: 0.1,
        }));
        registry.add_inventory_probe(Arc::new(SometimesPanickingProbe {
            tick: AtomicUsize::new(0),
            peer: 0xB,
            cpu: 0.2,
        }));
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(10),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _actions_rx,
            reader,
        } = MeshOsLoop::new(cfg);
        let loop_ = loop_.with_probe_registry(registry);
        let task = tokio::spawn(loop_.run());
        // Wait for the first tick (both probes succeed) and
        // several subsequent ticks (probe B panics).
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        let snap = reader.read();
        let inv_peers: std::collections::BTreeSet<u64> = snap
            .peers
            .iter()
            .filter(|(_, p)| p.cpu_load_1m.is_some())
            .map(|(id, _)| *id)
            .collect();
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), task).await;
        assert!(
            inv_peers.contains(&0xA),
            "probe A's peer should still be in inventory: {inv_peers:?}",
        );
        assert!(
            inv_peers.contains(&0xB),
            "probe B's peer must survive probe B's panic — partial-panic ticks are not authoritative for the panicking probe's peers: {inv_peers:?}",
        );
    }

    #[tokio::test]
    async fn inventory_gc_does_not_wipe_on_a_transient_empty_probe() {
        // The InventoryProbe trait permits an `Ok(vec![])`
        // return ("no peers this tick" — transient procfs
        // unavailability, cold start, etc.). Without the
        // `any_probe_saw_samples` guard the GC pass would
        // treat the empty return as "no peers authoritatively
        // exist anymore" and wipe every previously-seen peer's
        // inventory.
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        struct FlakyProbe {
            tick: AtomicUsize,
        }
        impl super::super::probes::InventoryProbe for FlakyProbe {
            fn inventory_samples(&self) -> Vec<(u64, super::super::probes::PeerInventory)> {
                let t = self.tick.fetch_add(1, AOrdering::Relaxed);
                // Tick 0: populate. Subsequent ticks: empty
                // (simulate the resource probe losing its
                // procfs handle for a few cycles).
                if t == 0 {
                    vec![
                        (
                            0xA,
                            super::super::probes::PeerInventory {
                                cpu_load_1m: Some(0.1),
                                ..Default::default()
                            },
                        ),
                        (
                            0xB,
                            super::super::probes::PeerInventory {
                                cpu_load_1m: Some(0.2),
                                ..Default::default()
                            },
                        ),
                    ]
                } else {
                    vec![]
                }
            }
        }
        let registry = ProbeRegistry::new();
        registry.add_inventory_probe(Arc::new(FlakyProbe {
            tick: AtomicUsize::new(0),
        }));
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(10),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _actions_rx,
            reader,
        } = MeshOsLoop::new(cfg);
        let loop_ = loop_.with_probe_registry(registry);
        let task = tokio::spawn(loop_.run());
        // Wait long enough for the populating tick + several
        // empty ticks.
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        let snap = reader.read();
        let inv_peers: std::collections::BTreeSet<u64> = snap
            .peers
            .iter()
            .filter(|(_, p)| p.cpu_load_1m.is_some())
            .map(|(id, _)| *id)
            .collect();
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), task).await;
        assert!(
            inv_peers.contains(&0xA),
            "transient empty probe return must not wipe inventory: {inv_peers:?}",
        );
        assert!(
            inv_peers.contains(&0xB),
            "transient empty probe return must not wipe inventory: {inv_peers:?}",
        );
    }

    #[tokio::test]
    async fn snapshot_reader_does_not_stall_under_concurrent_reads() {
        // With ArcSwap, publish is a pointer store and read is
        // a pointer load + Arc clone; concurrent readers cannot
        // stall the publisher. Smoke-test by spawning many
        // readers polling the snapshot in a tight loop while
        // the loop ticks repeatedly.
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(5),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _actions_rx,
            reader,
        } = MeshOsLoop::new(cfg);
        let task = tokio::spawn(loop_.run());

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..8 {
            let reader = reader.clone();
            let stop = Arc::clone(&stop);
            readers.push(tokio::spawn(async move {
                let mut count = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let _snap = reader.read();
                    count += 1;
                    tokio::task::yield_now().await;
                }
                count
            }));
        }
        // Let the loop fire ~10 ticks.
        tokio::time::sleep(StdDuration::from_millis(60)).await;
        stop.store(true, Ordering::Relaxed);
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _loop_count = task.await.expect("loop join");
        let mut total = 0u64;
        for r in readers {
            total += r.await.expect("reader join");
        }
        assert!(
            total > 0,
            "readers should have made progress while the loop published",
        );
    }

    #[tokio::test]
    async fn dropped_actions_counter_increments_when_action_queue_is_full() {
        // Regression for C4: reconcile output silently dropped
        // when the action mpsc is at capacity. Make the queue
        // tiny, hold the receiver without draining, project a
        // reconcile pass that emits multiple actions, assert the
        // counter caught the drop.
        let cfg = MeshOsConfig {
            action_queue_capacity: 1,
            tick_interval: StdDuration::from_millis(10),
            ..fast_test_config()
        };
        let this_node = cfg.this_node;
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _actions_rx,
            reader: _reader,
        } = MeshOsLoop::new(cfg);
        let counter = loop_.dropped_actions_counter();
        let task = tokio::spawn(loop_.run());
        // Put `this_node` as a holder of five chains.
        for chain in 1..=5u64 {
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                    chain: chain as ChainId,
                    holder: this_node,
                }))
                .await
                .unwrap();
        }
        // Project local Drop intent → reconcile emits one
        // DropReplica per chain on the next tick.
        for chain in 1..=5u64 {
            handle
                .publish(MeshOsEvent::LocalReplicaIntent(LocalReplicaIntentUpdate {
                    chain: chain as ChainId,
                    intent: LocalReplicaIntent::Drop,
                }))
                .await
                .unwrap();
        }
        // Let several ticks fire.
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        let dropped = counter.load(Ordering::Relaxed);
        assert!(
            dropped >= 1,
            "expected at least one dropped reconcile action with \
             action_queue_capacity = 1; got {dropped}",
        );
    }

    #[tokio::test]
    async fn ticks_drive_reconcile_passes() {
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(20),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(cfg);
        let task = tokio::spawn(loop_.run());
        // Let several ticks fire.
        tokio::time::sleep(StdDuration::from_millis(120)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let count = task.await.expect("join");
        // At a 20 ms tick over a 120 ms window we expect roughly
        // 4–7 reconcile passes; require at least 2 so a slow CI
        // host doesn't flake.
        assert!(
            count >= 2,
            "expected at least 2 reconcile passes, got {count}"
        );
    }

    #[tokio::test]
    async fn locality_probe_samples_fold_into_actual_rtt_via_snapshot() {
        // A LocalityProbe is polled on every Tick; its samples
        // land in MeshOsState::rtt; the snapshot's peers map
        // surfaces them.
        struct FixedProbe(Vec<(u64, std::time::Duration)>);
        impl super::super::probes::LocalityProbe for FixedProbe {
            fn rtt_samples(&self) -> Vec<(u64, std::time::Duration)> {
                self.0.clone()
            }
        }

        let registry = ProbeRegistry::new();
        registry.add_locality_probe(std::sync::Arc::new(FixedProbe(vec![
            (10, std::time::Duration::from_millis(33)),
            (11, std::time::Duration::from_millis(150)),
        ])));
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_probe_registry(registry);
        let task = tokio::spawn(loop_.run());

        tokio::time::sleep(StdDuration::from_millis(80)).await;
        let snap = reader.read();
        // Peer 10 surfaces with 33 ms.
        let p10 = snap.peers.get(&10).expect("peer 10 in snapshot");
        assert_eq!(p10.rtt_ms, Some(33));
        let p11 = snap.peers.get(&11).expect("peer 11 in snapshot");
        assert_eq!(p11.rtt_ms, Some(150));

        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
    }

    #[tokio::test]
    async fn health_probe_samples_fold_into_actual_health_via_snapshot() {
        struct FixedProbe(Vec<(u64, super::super::event::NodeHealth)>);
        impl super::super::probes::HealthProbe for FixedProbe {
            fn health_samples(&self) -> Vec<(u64, super::super::event::NodeHealth)> {
                self.0.clone()
            }
        }

        let registry = ProbeRegistry::new();
        registry.add_health_probe(std::sync::Arc::new(FixedProbe(vec![(
            5,
            super::super::event::NodeHealth::Unreachable,
        )])));
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_probe_registry(registry);
        let task = tokio::spawn(loop_.run());

        tokio::time::sleep(StdDuration::from_millis(80)).await;
        let snap = reader.read();
        let p5 = snap.peers.get(&5).expect("peer 5 in snapshot");
        // Wire form differs from the enum form but the
        // discriminator matches.
        assert!(matches!(
            p5.health,
            Some(super::super::snapshot::PeerHealthSnapshot::Unreachable)
        ));

        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
    }

    #[tokio::test]
    async fn probe_registry_attached_post_construction_is_polled_on_next_tick() {
        // The runtime / production pattern: construct the loop,
        // pass a shared registry through `with_probe_registry`,
        // THEN install probes on the registry. The next Tick
        // picks them up because both ends share the same Arc.
        struct FixedProbe;
        impl super::super::probes::LocalityProbe for FixedProbe {
            fn rtt_samples(&self) -> Vec<(u64, std::time::Duration)> {
                vec![(99, std::time::Duration::from_millis(7))]
            }
        }

        let registry = ProbeRegistry::new();
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_probe_registry(registry.clone());
        let task = tokio::spawn(loop_.run());

        // Add the probe AFTER spawning the loop — the shared
        // Arc means the loop sees it on the next Tick.
        registry.add_locality_probe(std::sync::Arc::new(FixedProbe));
        tokio::time::sleep(StdDuration::from_millis(80)).await;

        let snap = reader.read();
        let p99 = snap.peers.get(&99).expect("peer 99 in snapshot");
        assert_eq!(p99.rtt_ms, Some(7));

        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
    }

    #[tokio::test]
    async fn snapshot_reader_returns_updated_state_after_each_tick() {
        // The reader should reflect the most recent
        // post-reconcile snapshot. Fire some events that change
        // state, let ticks fire, sample the reader.
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let task = tokio::spawn(loop_.run());

        // Add a replica observation.
        handle
            .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                chain: 0xC0FFEE,
                holder: 7,
            }))
            .await
            .unwrap();
        // Give ticks time to fire + reconcile + publish.
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        let snap = reader.read();
        let entry = snap
            .replicas
            .get(&0xC0FFEE)
            .expect("snapshot should carry the replica");
        assert_eq!(entry.holders, vec![7]);

        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
    }

    #[tokio::test]
    async fn snapshot_reader_is_cloneable_and_sees_same_state() {
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: reader_a,
        } = MeshOsLoop::new(fast_test_config());
        let reader_b = reader_a.clone();
        let task = tokio::spawn(loop_.run());

        handle
            .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                chain: 1,
                holder: 1,
            }))
            .await
            .unwrap();
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        let snap_a = reader_a.read();
        let snap_b = reader_b.read();
        assert_eq!(snap_a, snap_b);

        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
    }

    #[tokio::test]
    async fn loop_drains_event_burst_without_panicking() {
        // Smoke test: the loop accepts a burst of arbitrary
        // events and exits cleanly when shutdown is published.
        // The fold-side ordering property is asserted directly
        // on `MeshOsState::apply` in `state::tests` — that
        // covers the substantive ordering guarantee without
        // having to crack open the consumed-loop's state.
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_test_config());

        let chain: ChainId = 0xC0FFEE;
        let probe = tokio::spawn(async move {
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                    chain,
                    holder: 11,
                }))
                .await
                .unwrap();
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                    chain,
                    holder: 12,
                }))
                .await
                .unwrap();
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Removed {
                    chain,
                    holder: 11,
                }))
                .await
                .unwrap();
            handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        });

        let task = tokio::spawn(loop_.run());
        probe.await.expect("publisher panicked");
        let _count = tokio::time::timeout(StdDuration::from_millis(200), task)
            .await
            .expect("loop did not exit")
            .expect("join");
    }

    #[test]
    fn loop_construction_returns_handle_and_actions_receiver() {
        // Compile + type-check: `new` returns the triple, the
        // handle is cloneable, the actions receiver is the
        // documented type.
        let MeshOsLoopParts {
            mesh_loop: _loop_,
            handle,
            actions_rx: _actions_rx,
            reader: _,
        } = MeshOsLoop::new(MeshOsConfig::default());
        let _clone = handle.clone();
    }

    #[tokio::test]
    async fn try_publish_surfaces_queue_full_under_saturation() {
        // Capacity-1 channel, loop not yet running — second
        // try_publish should hit QueueFull rather than block.
        let cfg = MeshOsConfig {
            event_queue_capacity: 1,
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(cfg);
        handle.try_publish(MeshOsEvent::Tick).unwrap();
        match handle.try_publish(MeshOsEvent::Tick) {
            Err(MeshOsHandleError::QueueFull) => {}
            other => panic!("expected QueueFull, got {other:?}"),
        }
        drop(handle);
        // Drain so the loop exits.
        let _ = loop_.run().await;
    }

    #[tokio::test]
    async fn shutdown_event_short_circuits_pending_events_after_it() {
        // Per the loop contract: Shutdown breaks the loop the
        // moment it's dequeued. Events queued behind Shutdown
        // must NOT mutate state — the post-Shutdown
        // ReplicaUpdate below should leave the snapshot's
        // replica fold empty. Use a long tick interval so the
        // reconcile arm doesn't race the assertion.
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_secs(60),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(cfg);
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        // Post-Shutdown event: enqueued behind Shutdown. The
        // loop must NOT apply it before exiting.
        handle
            .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                chain: 1,
                holder: 1,
            }))
            .await
            .unwrap();
        let count = tokio::time::timeout(StdDuration::from_secs(1), loop_.run())
            .await
            .expect("loop did not exit on Shutdown");
        // Reconcile never fired (Shutdown short-circuits the
        // loop before any tick).
        assert_eq!(count, 0, "Shutdown must break before reconcile fires");
        // The post-Shutdown ReplicaUpdate must NOT have applied —
        // the snapshot's replica fold stays empty.
        let snap = reader.read();
        assert!(
            snap.replicas.is_empty(),
            "post-Shutdown ReplicaUpdate must not enter the fold; saw {} entries",
            snap.replicas.len(),
        );
    }

    #[tokio::test]
    async fn reconcile_emitted_at_anchored_on_last_tick_for_replay_determinism() {
        // Reconcile must stamp every PendingAction.emitted_at on
        // the same Instant the Tick fold wrote into
        // actual.last_tick — not on a fresh Instant::now(). The
        // snapshot derives `age_ms` from
        // `now.saturating_duration_since(emitted_at)` against the
        // same anchor, so an action emitted in the latest tick
        // renders with age_ms == 0.
        //
        // We use a long tick interval and shut down right after
        // the first tick fires so the read sees actions whose
        // emitted_at matches the last_tick the snapshot is built
        // against.
        use super::super::event::AdminEvent;
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(80),
            ..fast_test_config()
        };
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            // Keep the receiver alive so the reconciler's try_send
            // succeeds and the action lands in recent_emissions —
            // pre-fix recent_emissions populated regardless of
            // try_send outcome (an actions_tx.Closed would still
            // mark the action as "emitted" in the snapshot).
            actions_rx: _actions_rx,
            reader,
        } = MeshOsLoop::new(cfg);
        let task = tokio::spawn(loop_.run());
        // Drive an EnterMaintenance — reconcile emits a
        // CommitMaintenanceTransition that lands in the
        // snapshot's `pending` mirror.
        handle
            .publish(MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: 1,
                drain_for: None,
            }))
            .await
            .unwrap();
        // Wait long enough for ONE tick to fire (80 ms), then
        // shutdown before a second tick.
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        let snap = reader.read();
        assert!(
            !snap.recently_emitted.is_empty(),
            "expected at least one recently-emitted action; saw none",
        );
        // The latest tick anchors the snapshot's now. Actions
        // emitted in that tick render with age_ms == 0 only when
        // reconcile uses last_tick for emitted_at.
        let zero_age_count = snap
            .recently_emitted
            .iter()
            .filter(|p| p.age_ms == 0)
            .count();
        assert!(
            zero_age_count >= 1,
            "expected at least one action emitted in the snapshot's tick to render \
             age_ms == 0 (anchored on last_tick); recently_emitted = {:?}",
            snap.recently_emitted,
        );
    }

    #[tokio::test]
    async fn log_chain_appender_receives_every_published_log_line() {
        use super::super::log_chain::BufferingLogChainAppender;
        use super::super::logs::{LogLevel, LogLine};
        let appender = Arc::new(BufferingLogChainAppender::default());
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_log_appender(
            appender.clone() as Arc<dyn super::super::log_chain::LogChainAppender>
        );
        let task = tokio::spawn(loop_.run());

        for (i, level) in [LogLevel::Info, LogLevel::Warn, LogLevel::Error]
            .into_iter()
            .enumerate()
        {
            handle
                .publish(MeshOsEvent::LogLine(LogLine {
                    level,
                    daemon_id: Some(7),
                    message: format!("msg {i}"),
                }))
                .await
                .unwrap();
        }
        tokio::time::sleep(StdDuration::from_millis(60)).await;

        let captured = appender.captured();
        assert_eq!(captured.len(), 3, "appender should see three records");
        let snap = reader.read();
        assert_eq!(snap.log_ring.len(), 3, "ring should hold three records");
        // Appender + ring see the SAME records (seq + content match
        // for each).
        for (i, captured_record) in captured.iter().enumerate().take(3) {
            assert_eq!(snap.log_ring[i].seq, captured_record.seq);
            assert_eq!(snap.log_ring[i].message, captured_record.message);
        }

        drop(handle);
        let _ = tokio::time::timeout(StdDuration::from_secs(1), task).await;
    }

    #[tokio::test]
    async fn admin_audit_chain_appender_receives_every_recorded_commit() {
        // Wire a buffering appender into the loop; drive an
        // admin event through; the appender + the snapshot
        // ring should each carry one record with the same seq.
        use super::super::audit_chain::BufferingAdminAuditChainAppender;
        use super::super::event::AdminEvent;
        let appender = Arc::new(BufferingAdminAuditChainAppender::default());
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_admin_audit_appender(
            appender.clone() as Arc<dyn super::super::audit_chain::AdminAuditChainAppender>
        );
        let task = tokio::spawn(loop_.run());

        // Publish an unsigned admin event — fold records it on
        // the ring with Unverified outcome AND fires the
        // appender.
        handle
            .publish(MeshOsEvent::AdminEvent(AdminEvent::Cordon { node: 42 }))
            .await
            .unwrap();
        tokio::time::sleep(StdDuration::from_millis(60)).await;

        let captured = appender.captured();
        assert_eq!(captured.len(), 1, "appender should see one record");
        assert_eq!(captured[0].event, AdminEvent::Cordon { node: 42 });

        let snap = reader.read();
        assert_eq!(snap.admin_audit.len(), 1, "ring should hold one record");
        // The appender + ring see the SAME record (seq + content match).
        assert_eq!(snap.admin_audit[0].seq, captured[0].seq);
        // Successful append → ring entry has chain_pending == false.
        assert!(
            !snap.admin_audit[0].chain_pending,
            "ring entry must NOT be marked chain_pending when the appender succeeded"
        );

        // Tidy.
        drop(handle);
        let _ = tokio::time::timeout(StdDuration::from_secs(1), task).await;
    }

    /// Pin that ring-first + mark-chain_pending kicks in when the
    /// audit chain appender returns an error: ring still records
    /// the entry but flags it so chain consumers can distinguish
    /// "permanent gap" from "haven't replicated yet."
    #[tokio::test]
    async fn audit_ring_flags_chain_pending_on_appender_failure() {
        use super::super::audit_chain::{AdminAuditAppendError, AdminAuditChainAppender};
        use super::super::event::AdminEvent;
        use super::super::ice::AdminAuditRecord;

        struct FailingAuditAppender;
        impl AdminAuditChainAppender for FailingAuditAppender {
            fn append(&self, _record: &AdminAuditRecord) -> Result<(), AdminAuditAppendError> {
                Err(AdminAuditAppendError {
                    reason: "test-injected appender failure".into(),
                })
            }
        }

        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_admin_audit_appender(Arc::new(FailingAuditAppender));
        let task = tokio::spawn(loop_.run());

        handle
            .publish(MeshOsEvent::AdminEvent(AdminEvent::Cordon { node: 99 }))
            .await
            .unwrap();
        tokio::time::sleep(StdDuration::from_millis(60)).await;

        let snap = reader.read();
        assert_eq!(snap.admin_audit.len(), 1, "ring must still record the entry");
        assert!(
            snap.admin_audit[0].chain_pending,
            "ring entry must be flagged chain_pending after the appender returned Err"
        );

        drop(handle);
        let _ = tokio::time::timeout(StdDuration::from_secs(1), task).await;
    }

    #[tokio::test]
    async fn kill_migration_dispatches_to_installed_aborter() {
        use super::super::event::AdminEvent;
        use super::super::migration_aborter::{BufferingMigrationAborter, MigrationAborter};
        let aborter = Arc::new(BufferingMigrationAborter::default());
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_migration_aborter(aborter.clone() as Arc<dyn MigrationAborter>);
        let task = tokio::spawn(loop_.run());

        handle
            .publish(MeshOsEvent::AdminEvent(AdminEvent::KillMigration {
                migration: 0xCAFE,
            }))
            .await
            .unwrap();
        tokio::time::sleep(StdDuration::from_millis(60)).await;

        assert_eq!(aborter.captured(), vec![0xCAFE]);

        drop(handle);
        let _ = tokio::time::timeout(StdDuration::from_secs(1), task).await;
    }

    #[tokio::test]
    async fn non_kill_admin_events_do_not_invoke_aborter() {
        use super::super::event::AdminEvent;
        use super::super::migration_aborter::{BufferingMigrationAborter, MigrationAborter};
        let aborter = Arc::new(BufferingMigrationAborter::default());
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_test_config());
        let loop_ = loop_.with_migration_aborter(aborter.clone() as Arc<dyn MigrationAborter>);
        let task = tokio::spawn(loop_.run());

        handle
            .publish(MeshOsEvent::AdminEvent(AdminEvent::Cordon { node: 42 }))
            .await
            .unwrap();
        tokio::time::sleep(StdDuration::from_millis(60)).await;

        assert!(aborter.captured().is_empty());

        drop(handle);
        let _ = tokio::time::timeout(StdDuration::from_secs(1), task).await;
    }

    /// `clear_inventory_probes` must drop every installed
    /// inventory probe (and only those) so callers swapping
    /// probe sources mid-flight can detach the previous set
    /// before installing replacements. Without this, a stale
    /// probe left in the append-only registry can stomp the
    /// new one's samples via last-writer-wins per peer.
    #[test]
    fn clear_inventory_probes_drops_installed_probes_only() {
        struct DummyInventoryProbe;
        impl super::super::probes::InventoryProbe for DummyInventoryProbe {
            fn inventory_samples(&self) -> Vec<(u64, super::super::probes::PeerInventory)> {
                vec![]
            }
        }
        struct DummyLocalityProbe;
        impl super::super::probes::LocalityProbe for DummyLocalityProbe {
            fn rtt_samples(&self) -> Vec<(u64, StdDuration)> {
                vec![]
            }
        }
        struct DummyHealthProbe;
        impl super::super::probes::HealthProbe for DummyHealthProbe {
            fn health_samples(&self) -> Vec<(u64, super::super::event::NodeHealth)> {
                vec![]
            }
        }

        let reg = ProbeRegistry::new();
        reg.add_locality_probe(Arc::new(DummyLocalityProbe));
        reg.add_health_probe(Arc::new(DummyHealthProbe));
        reg.add_inventory_probe(Arc::new(DummyInventoryProbe));
        reg.add_inventory_probe(Arc::new(DummyInventoryProbe));
        assert_eq!(reg.probe_counts(), (1, 1, 2));

        reg.clear_inventory_probes();
        assert_eq!(
            reg.probe_counts(),
            (1, 1, 0),
            "inventory cleared; locality + health untouched"
        );

        // The other clear paths are symmetric — verify they
        // also touch only their own list.
        reg.clear_locality_probes();
        assert_eq!(reg.probe_counts(), (0, 1, 0));
        reg.clear_health_probes();
        assert_eq!(reg.probe_counts(), (0, 0, 0));

        // Post-clear, re-install must succeed and count
        // independently — the underlying Vec wasn't replaced
        // with something half-broken.
        reg.add_inventory_probe(Arc::new(DummyInventoryProbe));
        assert_eq!(reg.probe_counts(), (0, 0, 1));
    }
}
