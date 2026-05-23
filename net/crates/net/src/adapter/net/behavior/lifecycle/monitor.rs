//! [`HealthMonitor`] — background driver that polls a
//! [`LifecycleGroup`]'s per-replica health and respawns
//! unhealthy replicas via an operator-supplied factory.
//!
//! Direction B / step 4b of
//! `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.
//! Built as a sibling to `LifecycleGroup` rather than baked
//! into the group itself so:
//!
//! - Groups that don't want auto-respawn (single-process tests,
//!   purely-snapshot CLI inspection) pay no overhead.
//! - The monitor's factory + state lives outside the group, so
//!   the group's lifetime + the monitor's lifetime are
//!   independent (the monitor can be stopped while the group
//!   stays running, or vice versa).
//!
//! # Threading
//!
//! The monitor takes an `Arc<tokio::sync::Mutex<LifecycleGroup<L>>>`
//! — async-mutex is required because the monitor's poll +
//! replace path holds the lock across `.await` points. Operators
//! who never auto-respawn keep their `LifecycleGroup` un-wrapped;
//! switching to managed mode is a one-line wrap.
//!
//! # What's NOT in this slice
//!
//! - **Registry integration.** The `AggregatorRegistry` stores
//!   replicas + handles separately (not as a `LifecycleGroup`),
//!   so wiring auto-respawn into registry-managed groups needs
//!   a small registry refactor — tracked as a follow-up.
//! - **Backoff on repeated failure.** If a replica keeps going
//!   unhealthy, the monitor keeps replacing it on every tick.
//!   Operators can read `HealthMonitorStats::replacements_failed`
//!   to detect persistent failures and shut the monitor down.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as ParkingMutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use super::daemon::LifecycleDaemon;
use super::group::LifecycleGroup;

/// Runtime counters surfaced to operator tooling. Atomic
/// fields so reads from CLI / Deck panels are wait-free.
#[derive(Debug, Default)]
pub struct HealthMonitorStats {
    /// Number of poll ticks the monitor has completed since
    /// `spawn`. Increments after each pass over the group's
    /// replicas, regardless of how many were unhealthy.
    pub ticks: AtomicU64,
    /// Number of `LifecycleGroup::replace` calls initiated.
    pub replacements_initiated: AtomicU64,
    /// Number of replace attempts that failed (factory
    /// returned a daemon whose `on_start` errored, or the slot
    /// index went out of bounds mid-respawn). Operators
    /// detecting persistent failures should consult this.
    pub replacements_failed: AtomicU64,
    /// `Instant` of the most recent poll tick, recorded after
    /// each pass. `None` until the first tick lands.
    pub last_tick_at: ParkingMutex<Option<std::time::Instant>>,
}

/// Background driver for [`LifecycleGroup`] auto-respawn.
/// Construct via [`HealthMonitor::spawn`]; stop via
/// [`HealthMonitor::stop`].
pub struct HealthMonitor<L: LifecycleDaemon> {
    stats: Arc<HealthMonitorStats>,
    shutdown: Arc<AtomicBool>,
    task: AsyncMutex<Option<JoinHandle<()>>>,
    /// Held for type inference + the `_marker` field that lets
    /// the monitor outlive the spawning function without
    /// dangling. The actual group reference is captured by the
    /// spawned task.
    _marker: std::marker::PhantomData<L>,
}

/// Internal: the monitor accepts either a plain
/// `Arc<AsyncMutex<LifecycleGroup<L>>>` (callers that own the
/// group exclusively) or an Option-wrapped variant (registry
/// path where unregister `take`s the group out). The variants
/// share the `run_poll_pass` body.
enum MonitorGroupRef<L: LifecycleDaemon> {
    Plain(Arc<AsyncMutex<LifecycleGroup<L>>>),
    Optional(Arc<AsyncMutex<Option<LifecycleGroup<L>>>>),
}

/// One health-poll + respawn pass against a live group.
/// Returns true when at least one replacement was attempted
/// (currently used only for tracing — the return is consumed
/// to silence an unused warning).
async fn run_poll_pass<L, F>(
    group: &mut LifecycleGroup<L>,
    factory: &mut F,
    stats: &Arc<HealthMonitorStats>,
) -> bool
where
    L: LifecycleDaemon,
    F: FnMut(u8) -> Arc<L> + Send + 'static,
{
    let snapshot = group.health().await;
    let mut any_work = false;
    for (idx, h) in snapshot.iter().enumerate() {
        if h.healthy {
            continue;
        }
        let new_daemon = factory(u8::try_from(idx).unwrap_or(u8::MAX));
        stats.replacements_initiated.fetch_add(1, Ordering::AcqRel);
        any_work = true;
        if let Err(e) = group.replace(idx, new_daemon).await {
            stats.replacements_failed.fetch_add(1, Ordering::AcqRel);
            tracing::warn!(
                error = %e,
                replica_index = idx,
                "HealthMonitor: replace failed; continuing"
            );
        }
    }
    any_work
}

impl<L: LifecycleDaemon> HealthMonitor<L> {
    /// Spawn a background task that polls `group.health()` every
    /// `interval` and calls `group.replace(index, factory(index))`
    /// for each replica reporting unhealthy. The factory is
    /// invoked with the failing replica's index so it can build
    /// an identical replacement (same config + deterministic
    /// identity via `LifecycleGroup::replica_keypair`).
    ///
    /// Convenience wrapper around [`Self::spawn_with_option`]
    /// for callers that don't need the `take`-on-unregister
    /// pattern (e.g. tests that own their group directly).
    pub fn spawn<F>(
        group: Arc<AsyncMutex<LifecycleGroup<L>>>,
        factory: F,
        interval: Duration,
    ) -> Self
    where
        F: FnMut(u8) -> Arc<L> + Send + 'static,
    {
        // Wrap the non-option group in an Option-wrapped mutex
        // by transferring the LifecycleGroup. We can't move out
        // of the existing Arc<AsyncMutex<LifecycleGroup>>, so
        // the easiest path is: produce a separate
        // Arc<AsyncMutex<Option<...>>> that the monitor sees,
        // and arrange for the original arc to share the same
        // underlying group via a relay future. The simpler
        // alternative: just spawn directly against an
        // owned-by-the-monitor mutex by cloning at the API
        // boundary. Tests that hold the original `group`
        // continue to work because they also `lock().await`
        // against the same `Arc<AsyncMutex<LifecycleGroup>>`.
        //
        // Concretely: spawn returns a monitor whose internal
        // task uses the **same** mutex. We can't change the
        // inner type without taking ownership of the group.
        // So `spawn` keeps its original behavior — operates
        // directly against `Arc<AsyncMutex<LifecycleGroup<L>>>`.
        Self::spawn_inner_loop(MonitorGroupRef::Plain(group), factory, interval)
    }

    /// Spawn a monitor against an `Option`-wrapped group. When
    /// the option is `None` (e.g. after a registry
    /// `unregister` consumed the group), the monitor's poll
    /// pass becomes a no-op — the loop continues ticking but
    /// performs no health checks or replacements until the
    /// option is repopulated (which the current registry
    /// doesn't do) or `stop()` is called.
    ///
    /// This is the variant `AggregatorRegistry::register_with_monitor`
    /// uses so unregister can take the group out without
    /// disturbing the monitor's existence; the monitor's
    /// `stop()` is awaited as part of unregister.
    pub fn spawn_with_option<F>(
        group: Arc<AsyncMutex<Option<LifecycleGroup<L>>>>,
        factory: F,
        interval: Duration,
    ) -> Self
    where
        F: FnMut(u8) -> Arc<L> + Send + 'static,
    {
        Self::spawn_inner_loop(MonitorGroupRef::Optional(group), factory, interval)
    }

    fn spawn_inner_loop<F>(
        group_ref: MonitorGroupRef<L>,
        mut factory: F,
        interval: Duration,
    ) -> Self
    where
        F: FnMut(u8) -> Arc<L> + Send + 'static,
    {
        let stats = Arc::new(HealthMonitorStats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats_for_task = stats.clone();
        let shutdown_for_task = shutdown.clone();

        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the immediate first tick so the monitor's
            // first poll happens after one full interval —
            // gives daemons a chance to settle in.
            ticker.tick().await;
            loop {
                if shutdown_for_task.load(Ordering::Acquire) {
                    return;
                }
                ticker.tick().await;
                if shutdown_for_task.load(Ordering::Acquire) {
                    return;
                }

                // Hold the lock for the entire health-poll +
                // respawn pass. Snapshot health first, then
                // replace any unhealthy slots. The `Option`
                // variant short-circuits if the group's been
                // taken (registry unregister path).
                let did_work = match &group_ref {
                    MonitorGroupRef::Plain(g) => {
                        let mut guard = g.lock().await;
                        run_poll_pass(&mut *guard, &mut factory, &stats_for_task).await
                    }
                    MonitorGroupRef::Optional(g) => {
                        let mut guard = g.lock().await;
                        match guard.as_mut() {
                            Some(lg) => run_poll_pass(lg, &mut factory, &stats_for_task).await,
                            None => false,
                        }
                    }
                };
                let _ = did_work;
                stats_for_task.ticks.fetch_add(1, Ordering::AcqRel);
                *stats_for_task.last_tick_at.lock() = Some(std::time::Instant::now());
            }
        });

        Self {
            stats,
            shutdown,
            task: AsyncMutex::new(Some(task)),
            _marker: std::marker::PhantomData,
        }
    }

    /// Borrow the runtime counters for operator tooling.
    pub fn stats(&self) -> &Arc<HealthMonitorStats> {
        &self.stats
    }

    /// Signal the monitor loop to stop and await its teardown.
    /// Idempotent — calling twice is a no-op the second time.
    pub async fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        let task = self.task.lock().await.take();
        if let Some(t) = task {
            let _ = t.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::daemon::{LifecycleError, ReplicaHealth};
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicBool as StdAtomicBool;

    struct ToggleHealthDaemon {
        unhealthy: StdAtomicBool,
        starts: AtomicU64,
        stops: AtomicU64,
    }

    impl ToggleHealthDaemon {
        fn new(start_unhealthy: bool) -> Self {
            Self {
                unhealthy: StdAtomicBool::new(start_unhealthy),
                starts: AtomicU64::new(0),
                stops: AtomicU64::new(0),
            }
        }
    }

    #[async_trait]
    impl LifecycleDaemon for ToggleHealthDaemon {
        fn name(&self) -> &str {
            "toggle"
        }
        async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        async fn on_stop(&self) {
            self.stops.fetch_add(1, Ordering::AcqRel);
        }
        async fn health(&self) -> ReplicaHealth {
            if self.unhealthy.load(Ordering::Acquire) {
                ReplicaHealth::unhealthy("toggle-set-unhealthy")
            } else {
                ReplicaHealth::healthy()
            }
        }
    }

    #[tokio::test]
    async fn monitor_replaces_an_unhealthy_replica_after_one_poll() {
        // Build a 2-replica group where replica 1 reports
        // unhealthy. Spawn a monitor with a factory that
        // returns fresh healthy daemons. After one poll
        // interval, replica 1 must be a fresh daemon.
        let original_replicas: Arc<parking_lot::Mutex<Vec<Arc<ToggleHealthDaemon>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let original_clone = original_replicas.clone();
        let group = LifecycleGroup::<ToggleHealthDaemon>::spawn(2, [0u8; 32], move |idx| {
            // Replica 1 reports unhealthy from the start.
            let d = Arc::new(ToggleHealthDaemon::new(idx == 1));
            original_clone.lock().push(d.clone());
            d
        })
        .await
        .expect("spawn group");
        let original_at_1 = original_replicas.lock()[1].clone();

        let group = Arc::new(AsyncMutex::new(group));
        let factory_calls: Arc<parking_lot::Mutex<Vec<u8>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let factory_calls_clone = factory_calls.clone();
        let monitor = HealthMonitor::spawn(
            group.clone(),
            move |idx| {
                factory_calls_clone.lock().push(idx);
                // Replacement is healthy — so the next poll
                // doesn't try to replace it again.
                Arc::new(ToggleHealthDaemon::new(false))
            },
            Duration::from_millis(50),
        );

        // First tick lands at ~50ms; sleep enough for one or
        // two ticks but not so many that we'd see a runaway
        // replace loop.
        tokio::time::sleep(Duration::from_millis(140)).await;

        // The original unhealthy replica must have been stopped
        // and replaced. Factory was called at index 1.
        assert!(
            !factory_calls.lock().is_empty(),
            "factory should have been called at least once"
        );
        assert!(
            factory_calls.lock().contains(&1),
            "factory must have been called for the unhealthy index 1"
        );
        assert!(
            original_at_1.stops.load(Ordering::Acquire) >= 1,
            "original index-1 daemon must have been stopped during replace"
        );

        // The replaced daemon at index 1 in the group must be a
        // different Arc than the original.
        {
            let g = group.lock().await;
            let now_at_1 = g.replica(1).expect("replica 1");
            assert!(
                !Arc::ptr_eq(&now_at_1, &original_at_1),
                "replica 1 should be the replacement, not the original"
            );
        }

        // Stats are populated.
        assert!(
            monitor
                .stats()
                .replacements_initiated
                .load(Ordering::Acquire)
                >= 1
        );
        assert!(monitor.stats().ticks.load(Ordering::Acquire) >= 1);

        monitor.stop().await;
        // The Mutex still holds a live group; release it cleanly.
        let g = Arc::try_unwrap(group)
            .map_err(|_| "still referenced")
            .expect("only ref")
            .into_inner();
        g.stop().await;
    }

    #[tokio::test]
    async fn monitor_skips_replace_when_all_healthy() {
        // Healthy 2-replica group; monitor should never call
        // the factory.
        let group = LifecycleGroup::<ToggleHealthDaemon>::spawn(2, [0u8; 32], |_idx| {
            Arc::new(ToggleHealthDaemon::new(false))
        })
        .await
        .expect("spawn");
        let group = Arc::new(AsyncMutex::new(group));
        let factory_calls: Arc<parking_lot::Mutex<u32>> = Arc::new(parking_lot::Mutex::new(0));
        let factory_calls_clone = factory_calls.clone();
        let monitor = HealthMonitor::spawn(
            group.clone(),
            move |_idx| {
                *factory_calls_clone.lock() += 1;
                Arc::new(ToggleHealthDaemon::new(false))
            },
            Duration::from_millis(30),
        );

        // Run for several poll intervals.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*factory_calls.lock(), 0, "factory must not be called");
        assert_eq!(
            monitor
                .stats()
                .replacements_initiated
                .load(Ordering::Acquire),
            0
        );
        assert!(monitor.stats().ticks.load(Ordering::Acquire) >= 1);

        monitor.stop().await;
        let g = Arc::try_unwrap(group)
            .map_err(|_| "still referenced")
            .expect("only ref")
            .into_inner();
        g.stop().await;
    }
}
