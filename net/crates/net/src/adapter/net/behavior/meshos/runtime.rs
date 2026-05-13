//! [`MeshOsRuntime`] — the one-call entry point that bundles
//! the loop, the action executor, and the snapshot reader into
//! a single live object.
//!
//! Today's consumers wire the three pieces by hand:
//!
//! ```ignore
//! let (mesh_loop, handle, actions_rx, reader) = MeshOsLoop::new(config.clone());
//! let dispatcher = Arc::new(LoggingDispatcher::new());
//! let exec = ActionExecutor::new(actions_rx, Arc::new(config), Arc::clone(&dispatcher));
//! let loop_task = tokio::spawn(mesh_loop.run());
//! let exec_task = tokio::spawn(exec.run());
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

/// One-stop entry point. Spawns the loop + executor as tokio
/// tasks; exposes the publish handle, snapshot reader, and
/// executor stats; drives a clean shutdown via [`Self::shutdown`].
pub struct MeshOsRuntime {
    handle: MeshOsHandle,
    reader: MeshOsSnapshotReader,
    loop_task: JoinHandle<u64>,
    exec_task: JoinHandle<Arc<ExecutorStats>>,
    stats: Arc<ExecutorStats>,
    /// Cloned [`ProbeRegistry`] retained so consumers can attach
    /// probes after `start`. The loop reads through the same
    /// shared cell, so additions take effect on the next Tick.
    probes: ProbeRegistry,
    /// Cloned [`SchedulerRegistry`] retained so consumers can
    /// install the placement scorer after `start`. Same
    /// shared-cell pattern as `probes`.
    scheduler: SchedulerRegistry,
}

/// Plain-value rollup of the runtime's join statistics. Returned
/// by [`MeshOsRuntime::shutdown`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeStats {
    /// Total reconcile passes the loop ran before exiting.
    pub reconcile_passes: u64,
    /// Final executor counters.
    pub executor: ExecutorStatsSnapshot,
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
    /// scheduler registries. The most general entry point —
    /// `start` and `start_with_probes` are conveniences over
    /// this.
    pub fn start_full<D: ActionDispatcher>(
        config: MeshOsConfig,
        dispatcher: Arc<D>,
        probes: ProbeRegistry,
        scheduler: SchedulerRegistry,
    ) -> Self {
        let (mesh_loop, handle, actions_rx, reader) = MeshOsLoop::new(config.clone());
        let mesh_loop = mesh_loop
            .with_probe_registry(probes.clone())
            .with_scheduler_registry(scheduler.clone());
        let cfg_arc = Arc::new(config);
        let exec = ActionExecutor::new(actions_rx, cfg_arc, dispatcher);
        let stats = exec.stats_arc();
        let loop_task = tokio::spawn(mesh_loop.run());
        let exec_task = tokio::spawn(exec.run());
        Self {
            handle,
            reader,
            loop_task,
            exec_task,
            stats,
            probes,
            scheduler,
        }
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
            failed: self
                .stats
                .failed
                .load(std::sync::atomic::Ordering::Relaxed),
            deferred: self
                .stats
                .deferred
                .load(std::sync::atomic::Ordering::Relaxed),
            gated: self
                .stats
                .gated
                .load(std::sync::atomic::Ordering::Relaxed),
            dispatch_retries: self
                .stats
                .dispatch_retries
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
        self,
        timeout: Duration,
    ) -> Result<RuntimeStats, RuntimeShutdownError> {
        // Publish the shutdown event. If the loop already
        // exited (no handles left, or panic), `publish` returns
        // an error — we ignore it; the join below will surface
        // the loop's actual status.
        let _ = self.handle.publish(MeshOsEvent::Shutdown).await;
        let reconcile_passes = tokio::time::timeout(timeout, self.loop_task)
            .await
            .map_err(|_| RuntimeShutdownError::LoopTimeout)?
            .map_err(RuntimeShutdownError::LoopJoinError)?;
        // Drop the handle so the executor's mpsc receiver sees
        // None and exits.
        drop(self.handle);
        let stats_arc = tokio::time::timeout(timeout, self.exec_task)
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
                failed: self
                    .stats
                    .failed
                    .load(std::sync::atomic::Ordering::Relaxed),
                deferred: self
                    .stats
                    .deferred
                    .load(std::sync::atomic::Ordering::Relaxed),
                gated: self
                    .stats
                    .gated
                    .load(std::sync::atomic::Ordering::Relaxed),
                dispatch_retries: self
                    .stats
                    .dispatch_retries
                    .load(std::sync::atomic::Ordering::Relaxed),
            },
        })
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

    use super::*;
    use super::super::action::MaintenanceTransition;
    use super::super::action::MeshOsAction;
    use super::super::event::AdminEvent;
    use super::super::executor::LoggingDispatcher;

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
                deadline: None,
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
                deadline: None,
            }))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let stats = rt.executor_stats();
        // At least one CommitMaintenanceTransition dispatched.
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
}
