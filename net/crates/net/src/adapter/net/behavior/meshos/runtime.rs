//! [`MeshOsRuntime`] — the one-call entry point that bundles
//! the loop, the action executor, and the snapshot reader into
//! a single live object.
//!
//! Today's consumers wire the three pieces by hand:
//!
//! ```ignore
//! let parts = MeshOsLoop::new(config.clone());
//! let dispatcher = Arc::new(LoggingDispatcher::new());
//! let exec = ActionExecutor::new(parts.actions_rx, Arc::new(config), Arc::clone(&dispatcher));
//! let loop_task = tokio::spawn(parts.mesh_loop.run());
//! let exec_task = tokio::spawn(exec.run());
//! // Prefer publish_timeout over publish for long-lived sources
//! // so a wedged loop can't park the caller indefinitely.
//! parts.handle.publish_timeout(event, Duration::from_millis(50)).await?;
//! ```
//!
//! …which is fine for one or two integrations, awkward at
//! scale. `MeshOsRuntime::start(config, dispatcher)` collapses
//! it into one call and hands back a `Runtime` with
//! `handle()`, `snapshot_reader()`, `executor_stats()`, and
//! `shutdown()`. The two source-converter helpers
//! ([`super::sources::attach_to_daemon_registry`],
//! [`super::sources::attach_to_replication_coordinator`])
//! plug into the runtime's handle.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::config::MeshOsConfig;
use super::event::MeshOsEvent;
use super::event_loop::{MeshOsHandle, MeshOsLoop, MeshOsSnapshotReader, ProbeRegistry};
use super::executor::{ActionDispatcher, ActionExecutor, ExecutorStats, ExecutorStatsSnapshot};
use super::probes::{HealthProbe, LocalityProbe};
use super::scheduler::{PlacementScorer, SchedulerRegistry};
use super::snapshot::MeshOsSnapshot;
use super::sources::attach_to_daemon_registry;
use crate::adapter::net::compute::DaemonRegistry;

/// One-stop entry point. Spawns the loop + executor as tokio
/// tasks; exposes the publish handle, snapshot reader, and
/// executor stats; drives a clean shutdown via [`Self::shutdown`].
pub struct MeshOsRuntime {
    handle: MeshOsHandle,
    reader: MeshOsSnapshotReader,
    /// Loop + executor tasks. Wrapped in `Option` so
    /// [`shutdown_with_timeout`] can `take()` them through `&mut`
    /// while still letting the [`Drop`] impl abort whichever
    /// task is still running if the runtime is dropped without
    /// an explicit shutdown.
    loop_task: Option<JoinHandle<u64>>,
    exec_task: Option<JoinHandle<Arc<ExecutorStats>>>,
    stats: Arc<ExecutorStats>,
    /// Cloned [`ProbeRegistry`] retained so consumers can attach
    /// probes after `start`. The loop reads through the same
    /// shared cell, so additions take effect on the next Tick.
    probes: ProbeRegistry,
    /// Cloned [`SchedulerRegistry`] retained so consumers can
    /// install the placement scorer after `start`. Same
    /// shared-cell pattern as `probes`.
    scheduler: SchedulerRegistry,
    /// Shared counter the loop increments when an emitted action
    /// can't be enqueued (executor queue at
    /// `action_queue_capacity`). Sampled by the runtime for
    /// operator visibility into the silent-loss path.
    dropped_actions: Arc<AtomicU64>,
    /// Per-runtime daemon registry. Constructed on `start`; the
    /// lifecycle observer that fans daemon-lifecycle events into
    /// the loop is auto-attached. SDK consumers register daemons
    /// against this handle; substrate code can also pass a
    /// pre-built registry via [`Self::start_with_daemon_registry`]
    /// to share state with code already managing daemons.
    daemon_registry: Arc<DaemonRegistry>,
}

/// Plain-value rollup of the runtime's join statistics. Returned
/// by [`MeshOsRuntime::shutdown`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeStats {
    /// Total reconcile passes the loop ran before exiting.
    pub reconcile_passes: u64,
    /// Final executor counters.
    pub executor: ExecutorStatsSnapshot,
    /// Total actions reconcile emitted that the action-queue
    /// rejected because the executor was at
    /// `action_queue_capacity`.
    pub dropped_actions: u64,
}

impl MeshOsRuntime {
    /// Spawn the loop + executor and return a live runtime.
    /// The dispatcher is whatever wires the action variants to
    /// the substrate-side mechanics (`DaemonRegistry`,
    /// migration orchestrator, admin chain commits). Tests can
    /// pass an [`super::executor::LoggingDispatcher`] for the
    /// log-only path.
    pub fn start<D: ActionDispatcher>(config: MeshOsConfig, dispatcher: Arc<D>) -> Self {
        Self::start_with_probes(config, dispatcher, ProbeRegistry::new())
    }

    /// Like [`Self::start`], but accepts a pre-populated
    /// [`ProbeRegistry`]. The registry is cloned and retained
    /// — consumers can keep adding probes after `start_with_probes`
    /// via [`Self::add_locality_probe`] / [`Self::add_health_probe`]
    /// or by holding their own clone of the registry.
    pub fn start_with_probes<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
    ) -> Self {
        Self::start_full(config, dispatcher, probes, SchedulerRegistry::new())
    }

    /// Like [`Self::start`], but accepts both probe and
    /// scheduler registries.
    pub fn start_full<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
    ) -> Self {
        Self::start_with_daemon_registry(
            config,
            dispatcher,
            probes,
            scheduler,
            Arc::new(DaemonRegistry::new()),
        )
    }

    /// Most general entry point. Accepts probe + scheduler
    /// registries AND a pre-built [`DaemonRegistry`] the runtime
    /// will attach its lifecycle sink to. `start`, `start_with_probes`,
    /// and `start_full` build new registries internally; callers
    /// that need to share a registry with other subsystems pass
    /// theirs in here.
    pub fn start_with_daemon_registry<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
        daemon_registry: Arc<DaemonRegistry>,
    ) -> Self {
        Self::start_with_options(config, dispatcher, probes, scheduler, daemon_registry, None)
    }

    /// Most-general constructor with an optional [`super::control::ControlSink`]
    /// for fan-out of `MeshOsControl` events. The SDK uses this
    /// path; substrate code that doesn't care about control
    /// fan-out should call one of the simpler `start*`
    /// constructors.
    pub fn start_with_options<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
        daemon_registry: Arc<DaemonRegistry>,
        control_sink: Option<Arc<dyn super::control::ControlSink>>,
    ) -> Self {
        Self::start_with_all(
            config,
            dispatcher,
            probes,
            scheduler,
            daemon_registry,
            control_sink,
            None,
        )
    }

    /// Maximum-control constructor — accepts every optional
    /// extension the loop supports, including the ICE admin
    /// verifier that gates `MeshOsEvent::SignedIceCommit`
    /// folding on multi-operator signature verification.
    pub fn start_with_all<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
        daemon_registry: Arc<DaemonRegistry>,
        control_sink: Option<Arc<dyn super::control::ControlSink>>,
        admin_verifier: Option<Arc<super::ice::AdminVerifier>>,
    ) -> Self {
        Self::start_with_audit_chain(
            config,
            dispatcher,
            probes,
            scheduler,
            daemon_registry,
            control_sink,
            admin_verifier,
            None,
        )
    }

    /// Like [`Self::start_with_all`] but also accepts an
    /// optional [`super::audit_chain::AdminAuditChainAppender`].
    /// Production deployments wire a chain-backed appender
    /// here so the audit ring's bounded history extends to
    /// cluster-lifetime replay. Test + in-process callers
    /// leave it `None` and read the in-memory ring through
    /// the snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_audit_chain<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
        daemon_registry: Arc<DaemonRegistry>,
        control_sink: Option<Arc<dyn super::control::ControlSink>>,
        admin_verifier: Option<Arc<super::ice::AdminVerifier>>,
        admin_audit_appender: Option<Arc<dyn super::audit_chain::AdminAuditChainAppender>>,
    ) -> Self {
        Self::start_with_chains(
            config,
            dispatcher,
            probes,
            scheduler,
            daemon_registry,
            control_sink,
            admin_verifier,
            admin_audit_appender,
            None,
        )
    }

    /// Like [`Self::start_with_audit_chain`] but also accepts
    /// an optional [`super::log_chain::LogChainAppender`] for
    /// per-node log-chain history.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_chains<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
        daemon_registry: Arc<DaemonRegistry>,
        control_sink: Option<Arc<dyn super::control::ControlSink>>,
        admin_verifier: Option<Arc<super::ice::AdminVerifier>>,
        admin_audit_appender: Option<Arc<dyn super::audit_chain::AdminAuditChainAppender>>,
        log_appender: Option<Arc<dyn super::log_chain::LogChainAppender>>,
    ) -> Self {
        Self::start_with_all_chains(
            config,
            dispatcher,
            probes,
            scheduler,
            daemon_registry,
            control_sink,
            admin_verifier,
            admin_audit_appender,
            log_appender,
            None,
        )
    }

    /// Maximal-options constructor — accepts every chain
    /// seam the substrate exposes (admin audit, log,
    /// failure). Production deployments wiring all three
    /// `TypedRedexFile<*>` chains call this directly; the
    /// other `start_with_*` constructors forward with
    /// `None` defaults for the appenders they don't surface.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_all_chains<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
        daemon_registry: Arc<DaemonRegistry>,
        control_sink: Option<Arc<dyn super::control::ControlSink>>,
        admin_verifier: Option<Arc<super::ice::AdminVerifier>>,
        admin_audit_appender: Option<Arc<dyn super::audit_chain::AdminAuditChainAppender>>,
        log_appender: Option<Arc<dyn super::log_chain::LogChainAppender>>,
        failure_appender: Option<Arc<dyn super::failure_chain::FailureChainAppender>>,
    ) -> Self {
        let super::event_loop::MeshOsLoopParts {
            mesh_loop,
            handle,
            actions_rx,
            reader,
        } = MeshOsLoop::new(config.clone());
        // Wire the daemon-lifecycle source converter so registry
        // events fan into the loop's event stream. Replaces any
        // prior observer on the registry (one observer slot per
        // registry).
        let _prior = attach_to_daemon_registry(&daemon_registry, handle.clone());
        let cfg_arc = Arc::new(config);
        let mut exec = ActionExecutor::new(actions_rx, cfg_arc, dispatcher);
        if let Some(appender) = failure_appender {
            exec = exec.with_failure_appender(appender);
        }
        let stats = exec.stats_arc();
        // Share the executor's failures ring with the loop so
        // the snapshot publish path copies executor-side
        // failures into the snapshot's `recent_failures` field —
        // the chain-fold path is not the only surface.
        let mut mesh_loop = mesh_loop
            .with_probe_registry(probes.clone())
            .with_scheduler_registry(scheduler.clone())
            .with_executor_failures(exec.recent_failures_handle());
        if let Some(sink) = control_sink {
            mesh_loop = mesh_loop.with_control_sink(sink);
        }
        if let Some(verifier) = admin_verifier {
            mesh_loop = mesh_loop.with_admin_verifier(verifier);
        }
        if let Some(appender) = admin_audit_appender {
            mesh_loop = mesh_loop.with_admin_audit_appender(appender);
        }
        if let Some(appender) = log_appender {
            mesh_loop = mesh_loop.with_log_appender(appender);
        }
        let dropped_actions = mesh_loop.dropped_actions_counter();
        let loop_task = tokio::spawn(mesh_loop.run());
        let exec_task = tokio::spawn(exec.run());
        Self {
            handle,
            reader,
            loop_task: Some(loop_task),
            exec_task: Some(exec_task),
            stats,
            probes,
            scheduler,
            dropped_actions,
            daemon_registry,
        }
    }

    /// Sample the current count of reconcile-emitted actions
    /// the executor's mpsc rejected (queue full). A growing
    /// counter is the signal that reconcile is outpacing the
    /// dispatcher.
    pub fn dropped_actions(&self) -> u64 {
        self.dropped_actions.load(AtomicOrdering::Relaxed)
    }

    /// Install / replace the active placement scorer. Subsequent
    /// reconcile passes use the new scorer.
    pub fn install_placement_scorer(
        &self,
        scorer: Arc<dyn PlacementScorer>,
    ) -> Option<Arc<dyn PlacementScorer>> {
        self.scheduler.install(scorer)
    }

    /// Clone the scheduler registry.
    pub fn scheduler_registry(&self) -> SchedulerRegistry {
        self.scheduler.clone()
    }

    /// Install a [`LocalityProbe`] on the live loop. The probe
    /// is polled on the next Tick (and every Tick after).
    pub fn add_locality_probe(&self, probe: Arc<dyn LocalityProbe>) {
        self.probes.add_locality_probe(probe);
    }

    /// Install a [`HealthProbe`] on the live loop. Same cadence
    /// as locality probes.
    pub fn add_health_probe(&self, probe: Arc<dyn HealthProbe>) {
        self.probes.add_health_probe(probe);
    }

    /// Clone the probe registry. Used by tests + advanced
    /// callers that want to install probes outside the runtime's
    /// own lifetime.
    pub fn probe_registry(&self) -> ProbeRegistry {
        self.probes.clone()
    }

    /// Borrow the runtime's [`DaemonRegistry`]. The lifecycle
    /// sink is already attached, so any `register` /
    /// `unregister` call on the returned registry surfaces as a
    /// `DaemonLifecycleSignal` event in the loop's event stream.
    /// SDK consumers (Rust + future language bindings) register
    /// daemons through this handle.
    pub fn daemon_registry(&self) -> &Arc<DaemonRegistry> {
        &self.daemon_registry
    }

    /// Borrow the publish handle. Source converters
    /// (`attach_to_daemon_registry`, etc.) clone this to push
    /// events into the loop.
    pub fn handle(&self) -> &MeshOsHandle {
        &self.handle
    }

    /// Clone the publish handle. Cheap (one mpsc::Sender clone).
    pub fn handle_clone(&self) -> MeshOsHandle {
        self.handle.clone()
    }

    /// Borrow the snapshot reader. Phase F consumers
    /// (Deck integration, snapshot folds) clone this for
    /// out-of-loop reads.
    pub fn snapshot_reader(&self) -> &MeshOsSnapshotReader {
        &self.reader
    }

    /// Sample the most recent post-reconcile snapshot.
    pub fn snapshot(&self) -> MeshOsSnapshot {
        self.reader.read()
    }

    /// Sample the executor counters. Atomic loads — consistent
    /// per-counter but not as a single snapshot.
    pub fn executor_stats(&self) -> ExecutorStatsSnapshot {
        ExecutorStatsSnapshot {
            dispatched: self
                .stats
                .dispatched
                .load(std::sync::atomic::Ordering::Relaxed),
            failed: self.stats.failed.load(std::sync::atomic::Ordering::Relaxed),
            deferred: self
                .stats
                .deferred
                .load(std::sync::atomic::Ordering::Relaxed),
            gated: self.stats.gated.load(std::sync::atomic::Ordering::Relaxed),
            dispatch_retries: self
                .stats
                .dispatch_retries
                .load(std::sync::atomic::Ordering::Relaxed),
            cluster_backpressure_asserts: self
                .stats
                .cluster_backpressure_asserts
                .load(std::sync::atomic::Ordering::Relaxed),
            cluster_backpressure_releases: self
                .stats
                .cluster_backpressure_releases
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Drive a clean shutdown. Publishes
    /// [`MeshOsEvent::Shutdown`] to the loop, waits for the
    /// loop task to exit, drops the handle so the executor's
    /// receiver returns `None`, then waits for the executor to
    /// drain. Returns the final stats.
    ///
    /// `timeout` bounds each join — past it, the future
    /// returns `Err(RuntimeShutdownError::Timeout)` and the
    /// caller decides what to do with the tasks. Default is
    /// 2 s — generous for the test surface, tight enough for
    /// production.
    pub async fn shutdown(self) -> Result<RuntimeStats, RuntimeShutdownError> {
        self.shutdown_with_timeout(Duration::from_secs(2)).await
    }

    /// `shutdown` with an explicit timeout.
    pub async fn shutdown_with_timeout(
        mut self,
        timeout: Duration,
    ) -> Result<RuntimeStats, RuntimeShutdownError> {
        // Publish the shutdown event. If the loop already
        // exited (no handles left, or panic), `publish` returns
        // an error — we ignore it; the join below will surface
        // the loop's actual status.
        let _ = self.handle.publish(MeshOsEvent::Shutdown).await;
        let loop_task = self
            .loop_task
            .take()
            .expect("loop_task taken twice — shutdown is consume-self");
        let reconcile_passes = tokio::time::timeout(timeout, loop_task)
            .await
            .map_err(|_| RuntimeShutdownError::LoopTimeout)?
            .map_err(RuntimeShutdownError::LoopJoinError)?;
        // The loop just published `MeshOsEvent::Shutdown`, exited
        // its run body, and dropped its `actions_tx`. The executor's
        // `actions_rx.recv()` therefore returns `None` next pop,
        // and `ActionExecutor::run` returns its accumulated stats.
        let exec_task = self
            .exec_task
            .take()
            .expect("exec_task taken twice — shutdown is consume-self");
        let stats_arc = tokio::time::timeout(timeout, exec_task)
            .await
            .map_err(|_| RuntimeShutdownError::ExecutorTimeout)?
            .map_err(RuntimeShutdownError::ExecutorJoinError)?;
        let _ = stats_arc;
        Ok(RuntimeStats {
            reconcile_passes,
            executor: ExecutorStatsSnapshot {
                dispatched: self
                    .stats
                    .dispatched
                    .load(std::sync::atomic::Ordering::Relaxed),
                failed: self.stats.failed.load(std::sync::atomic::Ordering::Relaxed),
                deferred: self
                    .stats
                    .deferred
                    .load(std::sync::atomic::Ordering::Relaxed),
                gated: self.stats.gated.load(std::sync::atomic::Ordering::Relaxed),
                dispatch_retries: self
                    .stats
                    .dispatch_retries
                    .load(std::sync::atomic::Ordering::Relaxed),
                cluster_backpressure_asserts: self
                    .stats
                    .cluster_backpressure_asserts
                    .load(std::sync::atomic::Ordering::Relaxed),
                cluster_backpressure_releases: self
                    .stats
                    .cluster_backpressure_releases
                    .load(std::sync::atomic::Ordering::Relaxed),
            },
            dropped_actions: self.dropped_actions.load(AtomicOrdering::Relaxed),
        })
    }
}

impl Drop for MeshOsRuntime {
    /// If the runtime was dropped without an explicit
    /// [`shutdown`](Self::shutdown), abort whichever tasks are
    /// still in flight rather than detach them. Detaching would
    /// leak the loop + executor task tree along with the
    /// dispatcher `Arc` and the snapshot cell for the remainder
    /// of the process. After a clean `shutdown_with_timeout`
    /// the option fields are `None`, so this is a no-op.
    fn drop(&mut self) {
        let mut aborted = false;
        if let Some(loop_task) = self.loop_task.take() {
            if !loop_task.is_finished() {
                aborted = true;
                loop_task.abort();
            }
        }
        if let Some(exec_task) = self.exec_task.take() {
            if !exec_task.is_finished() {
                aborted = true;
                exec_task.abort();
            }
        }
        if aborted {
            tracing::warn!(
                target: "meshos",
                "MeshOsRuntime dropped without shutdown — aborted in-flight tasks",
            );
        }
    }
}

/// Errors from [`MeshOsRuntime::shutdown`].
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeShutdownError {
    /// The loop task didn't exit within the shutdown timeout.
    LoopTimeout,
    /// The loop task panicked or was cancelled.
    LoopJoinError(tokio::task::JoinError),
    /// The executor task didn't exit within the shutdown
    /// timeout (despite the source channel being dropped).
    ExecutorTimeout,
    /// The executor task panicked or was cancelled.
    ExecutorJoinError(tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::action::MaintenanceTransition;
    use super::super::action::MeshOsAction;
    use super::super::event::AdminEvent;
    use super::super::executor::LoggingDispatcher;
    use super::*;

    fn fast_cfg() -> MeshOsConfig {
        MeshOsConfig {
            this_node: 1,
            tick_interval: Duration::from_millis(10),
            event_queue_capacity: 64,
            action_queue_capacity: 64,
            backpressure: Default::default(),
            locality: Default::default(),
            maintenance: Default::default(),
            scheduler: Default::default(),
        }
    }

    #[tokio::test]
    async fn dropping_runtime_without_shutdown_aborts_tasks() {
        // Regression for I5: dropping the runtime without
        // calling `shutdown` used to detach both JoinHandles,
        // leaking the loop + executor task tree forever. The
        // `Drop` impl now aborts in-flight tasks.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        // Capture handles before drop so we can confirm
        // post-drop they're abort()ed.
        // The runtime owns the only JoinHandle, so the test
        // observes via the dispatcher's Arc strong count: when
        // the executor task is aborted, the dispatcher Arc held
        // inside drops, reducing the strong count.
        let initial_count = Arc::strong_count(&dispatcher);
        drop(rt);
        // Yield so the abort takes effect on the runtime tasks.
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if Arc::strong_count(&dispatcher) < initial_count {
                break;
            }
        }
        assert!(
            Arc::strong_count(&dispatcher) < initial_count,
            "executor task should have been aborted, releasing its dispatcher Arc",
        );
    }

    #[tokio::test]
    async fn runtime_start_and_clean_shutdown() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        // Let ticks fire.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stats = rt.shutdown().await.expect("clean shutdown");
        // At least a couple of reconcile passes ran.
        assert!(
            stats.reconcile_passes >= 1,
            "expected reconcile passes, got {}",
            stats.reconcile_passes,
        );
    }

    #[tokio::test]
    async fn runtime_handle_publishes_into_the_loop() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        // EnterMaintenance + empty workload → reconcile emits
        // CommitMaintenanceTransition(Maintenance).
        rt.handle()
            .publish(MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: 1,
                drain_for: None,
            }))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = rt.shutdown().await.expect("clean shutdown");
        let log = dispatcher.log();
        assert!(
            log.iter().any(|a| matches!(
                a,
                MeshOsAction::CommitMaintenanceTransition {
                    target: MaintenanceTransition::Maintenance,
                    ..
                }
            )),
            "expected maintenance transition in dispatcher log; got {log:?}",
        );
    }

    #[tokio::test]
    async fn runtime_snapshot_reader_reflects_post_reconcile_state() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        rt.handle()
            .publish(MeshOsEvent::ReplicaUpdate(
                super::super::event::ReplicaUpdate::Added {
                    chain: 0xC0FFEE,
                    holder: 7,
                },
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let snap = rt.snapshot();
        let entry = snap.replicas.get(&0xC0FFEE).expect("replica observed");
        assert_eq!(entry.holders, vec![7]);
        let _ = rt.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_executor_stats_increment_on_dispatch() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        rt.handle()
            .publish(MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: 1,
                drain_for: None,
            }))
            .await
            .unwrap();
        // Poll until at least one action dispatches, rather than
        // sleeping a fixed window. Bounded deadline so a wedged
        // executor surfaces as a clear timeout rather than a
        // silent stats==0 pass.
        let deadline = Instant::now() + Duration::from_secs(2);
        let stats = loop {
            let stats = rt.executor_stats();
            if stats.dispatched >= 1 {
                break stats;
            }
            if Instant::now() >= deadline {
                panic!("executor did not dispatch within 2s; final stats={stats:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            stats.dispatched >= 1,
            "expected at least one dispatch; got {stats:?}",
        );
        let _ = rt.shutdown().await;
    }

    #[tokio::test]
    async fn handle_clone_works_for_multiple_sources() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        let h1 = rt.handle_clone();
        let h2 = rt.handle_clone();
        h1.publish(MeshOsEvent::Tick).await.unwrap();
        h2.publish(MeshOsEvent::Tick).await.unwrap();
        let _ = rt.shutdown().await;
        // Just compile + run cleanly — the handle_clone path
        // is what source converters use to plug in.
        let _ = Instant::now();
    }

    #[tokio::test]
    async fn snapshot_recent_failures_surfaces_executor_dispatch_failures() {
        // Without the executor → loop failures ring wiring, every
        // consumer of `runtime.snapshot().recent_failures` saw an
        // empty deque — the executor maintained its own ring but
        // nothing published it. This test seeds a dispatch
        // failure via LoggingDispatcher::fail_next and asserts
        // the snapshot reflects it.
        use super::super::executor::DispatchError;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("synthetic failure"));
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        // Drive an EnterMaintenance — reconcile emits a
        // CommitMaintenanceTransition that the dispatcher's
        // queued `fail_next` will reject, recording one failure
        // on the executor's ring.
        rt.handle()
            .publish(MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: 1,
                drain_for: None,
            }))
            .await
            .unwrap();
        // Poll up to a couple of seconds for the executor to
        // record + the loop to publish.
        let deadline = Instant::now() + Duration::from_secs(2);
        let failures = loop {
            let snap = rt.snapshot();
            if !snap.recent_failures.is_empty() {
                break snap.recent_failures;
            }
            if Instant::now() >= deadline {
                panic!(
                    "expected at least one failure in snapshot; executor stats = {:?}",
                    rt.executor_stats()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            failures
                .iter()
                .any(|f| f.reason.contains("synthetic failure")),
            "expected the synthetic-failure record in {failures:?}",
        );
        let _ = rt.shutdown().await;
    }

    #[tokio::test]
    async fn daemon_registry_accessor_attaches_lifecycle_observer_on_start() {
        // The runtime owns a DaemonRegistry and auto-attaches the
        // daemon-lifecycle source converter during `start`. SDK
        // consumers reach the registry via `daemon_registry()` and
        // register daemons through it.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let rt = MeshOsRuntime::start(fast_cfg(), Arc::clone(&dispatcher));
        assert!(
            rt.daemon_registry().has_lifecycle_observer(),
            "runtime should auto-attach the daemon lifecycle observer",
        );
        // The accessor returns the same Arc each call — same
        // shared registry, not a new one.
        let a = Arc::as_ptr(rt.daemon_registry());
        let b = Arc::as_ptr(rt.daemon_registry());
        assert_eq!(a, b, "daemon_registry() must return the runtime-owned Arc");
        let _ = rt.shutdown().await;
    }

    #[tokio::test]
    async fn daemon_registry_can_be_pre_supplied_via_start_with_daemon_registry() {
        // Callers that need to share a registry with other
        // subsystems (the audit log, a metrics surface, etc.)
        // pass theirs in via `start_with_daemon_registry`.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let registry = Arc::new(DaemonRegistry::new());
        let rt = MeshOsRuntime::start_with_daemon_registry(
            fast_cfg(),
            Arc::clone(&dispatcher),
            ProbeRegistry::new(),
            SchedulerRegistry::new(),
            Arc::clone(&registry),
        );
        assert!(Arc::ptr_eq(rt.daemon_registry(), &registry));
        assert!(registry.has_lifecycle_observer());
        let _ = rt.shutdown().await;
    }
}
