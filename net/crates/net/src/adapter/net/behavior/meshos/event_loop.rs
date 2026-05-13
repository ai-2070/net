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
use super::event::MeshOsEvent;
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

    /// Reconcile-pass counter — used by tests / diagnostics to
    /// confirm reconcile fired exactly once per Tick.
    reconcile_count: u64,

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

    /// Atomic snapshot of installed probe counts —
    /// `(locality, health)`. One read lock covers both lists,
    /// so the pair is consistent even if a concurrent
    /// installer fires between them.
    pub fn probe_counts(&self) -> (usize, usize) {
        let guard = self.inner.lists.read();
        (guard.locality.len(), guard.health.len())
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
        let snapshot = Arc::new(ArcSwap::from_pointee(MeshOsSnapshot::default()));
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
            reconcile_count: 0,
            dropped_actions: Arc::new(AtomicU64::new(0)),
            executor_failures: None,
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

        loop {
            tokio::select! {
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
                _ = tick.tick() => {
                    // The Tick event drives reconcile; we route
                    // it through the same `apply` path so the
                    // `last_tick` fold field updates uniformly.
                    self.apply(&MeshOsEvent::Tick);
                    // Pull-via-tick probes — folded BEFORE
                    // reconcile so the reconcile pass sees the
                    // latest samples in this tick window.
                    self.poll_probes();
                    self.run_reconcile().await;
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
        // Clone both lists out under a single read lock so a
        // concurrent install between the locality and health
        // passes doesn't see one half of an inconsistent pair.
        let (locality, health) = {
            let guard = self.probes.lists.read();
            (guard.locality.clone(), guard.health.clone())
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
    }

    fn apply(&mut self, event: &MeshOsEvent) {
        match event {
            MeshOsEvent::PlacementIntent(intent) => self.desired.apply(intent),
            MeshOsEvent::DaemonIntentUpdate(update) => self.desired.apply_daemon_intent(update),
            MeshOsEvent::LocalReplicaIntent(update) => {
                self.desired.apply_local_replica_intent(update)
            }
            MeshOsEvent::AdminEvent(admin) => {
                self.desired.apply_admin(admin, self.config.this_node);
            }
            _ => {}
        }
        self.actual.apply(event, self.config.this_node);
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
            // observable.
            self.recent_emissions.push(pending.clone());
            if let Err(mpsc::error::TrySendError::Full(rejected)) =
                self.actions_tx.try_send(pending)
            {
                dropped_this_tick += 1;
                if first_dropped_kind.is_none() {
                    first_dropped_kind = Some(super::snapshot::action_kind_str(&rejected.action));
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
        let snap = MeshOsSnapshot::from_state(
            &self.actual,
            &self.desired,
            &self.recent_emissions,
            &failures,
        );
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
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(cfg);
        let task = tokio::spawn(loop_.run());
        // Drive an EnterMaintenance — reconcile emits a
        // CommitMaintenanceTransition that lands in the
        // snapshot's `pending` mirror.
        handle
            .publish(MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: 1,
                deadline: None,
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
            !snap.pending.is_empty(),
            "expected at least one pending action; saw none",
        );
        // The latest tick anchors the snapshot's now. Actions
        // emitted in that tick render with age_ms == 0 only when
        // reconcile uses last_tick for emitted_at.
        let zero_age_count = snap.pending.iter().filter(|p| p.age_ms == 0).count();
        assert!(
            zero_age_count >= 1,
            "expected at least one action emitted in the snapshot's tick to render \
             age_ms == 0 (anchored on last_tick); pending = {:?}",
            snap.pending,
        );
    }
}
