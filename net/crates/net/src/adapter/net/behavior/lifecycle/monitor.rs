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

/// Maximum number of ticks the backoff can skip between
/// replace attempts. With a 1 s monitor interval that's
/// ~256 s of cooldown after enough consecutive failures —
/// long enough that a persistently broken replica stops
/// churning the registry; short enough that recovery is
/// observable within minutes of the underlying fix landing.
const MAX_BACKOFF_SHIFT: u32 = 8;

/// Number of `consecutive_failures` slots beyond which we
/// stop growing the per-index Vec. Replica indices are u8,
/// so 256 caps the worst case.
const MAX_TRACKED_INDICES: usize = 256;

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
    /// Number of poll ticks where the monitor skipped a known-
    /// unhealthy replica because exponential backoff hadn't
    /// elapsed yet. Counts every (tick × skipped-replica)
    /// combination; operators reading the per-index backoff
    /// state should consult `consecutive_failures`.
    pub backoff_skips: AtomicU64,
    /// Per-replica-index consecutive-failure counter. Bumped
    /// each tick the replica is still unhealthy after a
    /// replace attempt; reset to 0 when the replica reports
    /// healthy. The `2^consecutive_failures` shift drives the
    /// next-retry-tick computation (capped at
    /// `MAX_BACKOFF_SHIFT`).
    pub consecutive_failures: ParkingMutex<Vec<u32>>,
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

/// One health-poll + respawn pass against a live group.
///
/// Honors the per-index exponential backoff stored in
/// `stats.consecutive_failures`. A replica that's been
/// unhealthy for N consecutive ticks gets retried at
/// `2^min(N, MAX_BACKOFF_SHIFT)` tick intervals — so a
/// persistently broken replica stops churning the registry
/// + the `LifecycleGroup::replace` lock at every tick.
///
/// Backoff state transitions:
/// - Replica reports healthy → reset to 0.
/// - Replica reports unhealthy + retry due → attempt replace,
///   then bump counter (the replacement is judged on the next
///   tick).
/// - Replica reports unhealthy + retry not due → record a
///   skip in `stats.backoff_skips` and continue.
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
    // Grow the per-index failure Vec to cover this group.
    {
        let mut failures = stats.consecutive_failures.lock();
        if failures.len() < snapshot.len() {
            failures.resize(snapshot.len().min(MAX_TRACKED_INDICES), 0);
        }
    }
    let current_tick = stats.ticks.load(Ordering::Acquire);
    for (idx, h) in snapshot.iter().enumerate() {
        if h.healthy {
            // Healthy → reset the failure counter; backoff
            // disappears immediately so a recovered replica
            // doesn't drag a stale skip-counter into the next
            // failure cycle.
            if idx < MAX_TRACKED_INDICES {
                let mut failures = stats.consecutive_failures.lock();
                if let Some(slot) = failures.get_mut(idx) {
                    *slot = 0;
                }
            }
            continue;
        }
        // Unhealthy. Decide whether the backoff lets us retry.
        let failures_before = if idx < MAX_TRACKED_INDICES {
            stats
                .consecutive_failures
                .lock()
                .get(idx)
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        if !should_retry_now(failures_before, current_tick) {
            stats.backoff_skips.fetch_add(1, Ordering::AcqRel);
            continue;
        }
        // Retry due — attempt the replace, then bump the
        // counter regardless of replace-error vs replace-ok
        // (the new daemon's health is judged next tick).
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
        if idx < MAX_TRACKED_INDICES {
            let mut failures = stats.consecutive_failures.lock();
            if let Some(slot) = failures.get_mut(idx) {
                *slot = slot.saturating_add(1);
            }
        }
    }
    any_work
}

/// Should we attempt a replace this tick given `failures`
/// prior failures + the `current_tick` counter?
///
/// - `failures == 0` → first sighting; always retry.
/// - `failures >= 1` → retry every `2^min(failures, MAX_BACKOFF_SHIFT)`
///   ticks. Concretely: `current_tick % step == 0` where
///   `step = 2^min(failures, MAX_BACKOFF_SHIFT)`.
fn should_retry_now(failures: u32, current_tick: u64) -> bool {
    if failures == 0 {
        return true;
    }
    let shift = failures.min(MAX_BACKOFF_SHIFT);
    let step: u64 = 1u64 << shift;
    current_tick.is_multiple_of(step)
}

impl<L: LifecycleDaemon> HealthMonitor<L> {
    /// Spawn a background task that polls `group.health()`
    /// every `interval` and calls
    /// `group.replace(index, factory(index))` for each replica
    /// reporting unhealthy.
    ///
    /// The group is an `Arc<AsyncMutex<Option<LifecycleGroup<L>>>>`
    /// so the registry's `unregister` path can `take` the group
    /// out without disturbing the monitor — the monitor's poll
    /// becomes a no-op once the `Option` is `None`. Pure-RAII
    /// callers wrap their group with `Some(...)` at construction.
    pub fn spawn<F>(
        group: Arc<AsyncMutex<Option<LifecycleGroup<L>>>>,
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
                // respawn pass. The `Option` short-circuits if
                // the group's been taken via unregister.
                {
                    let mut guard = group.lock().await;
                    if let Some(lg) = guard.as_mut() {
                        let _ = run_poll_pass(lg, &mut factory, &stats_for_task).await;
                    }
                }
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

        let group = Arc::new(AsyncMutex::new(Some(group)));
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
            let lg = g.as_ref().expect("group not taken");
            let now_at_1 = lg.replica(1).expect("replica 1");
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
        if let Some(lg) = g {
            lg.stop().await;
        }
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
        let group = Arc::new(AsyncMutex::new(Some(group)));
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
        if let Some(lg) = g {
            lg.stop().await;
        }
    }

    #[test]
    fn should_retry_now_first_failure_always_retries() {
        // No prior failures → retry every tick.
        for tick in 0..20u64 {
            assert!(should_retry_now(0, tick), "tick {tick} with 0 failures");
        }
    }

    #[test]
    fn should_retry_now_backoff_grows_exponentially() {
        // failures=1 → step=2, retries at every other tick.
        let retries_with_1: Vec<u64> = (0..16).filter(|t| should_retry_now(1, *t)).collect();
        assert_eq!(retries_with_1, vec![0, 2, 4, 6, 8, 10, 12, 14]);

        // failures=2 → step=4.
        let retries_with_2: Vec<u64> = (0..16).filter(|t| should_retry_now(2, *t)).collect();
        assert_eq!(retries_with_2, vec![0, 4, 8, 12]);

        // failures=3 → step=8.
        let retries_with_3: Vec<u64> = (0..16).filter(|t| should_retry_now(3, *t)).collect();
        assert_eq!(retries_with_3, vec![0, 8]);
    }

    #[test]
    fn should_retry_now_caps_at_max_backoff_shift() {
        // Very high failure counts cap at MAX_BACKOFF_SHIFT.
        // 2^MAX_BACKOFF_SHIFT ticks between retries.
        let max_step: u64 = 1u64 << MAX_BACKOFF_SHIFT;
        // 100 failures should NOT increase step beyond max.
        assert!(should_retry_now(100, max_step));
        assert!(!should_retry_now(100, max_step + 1));
        // Even u32::MAX failures cap at the same step.
        assert!(should_retry_now(u32::MAX, max_step));
    }

    /// Daemon that's perpetually unhealthy + counts how many
    /// `on_start` calls it sees. Lets the backoff test prove a
    /// persistent failure doesn't spawn N replacements at every
    /// monitor tick.
    struct PerpetuallyUnhealthyDaemon {
        starts: AtomicU64,
        stops: AtomicU64,
    }

    #[async_trait]
    impl LifecycleDaemon for PerpetuallyUnhealthyDaemon {
        fn name(&self) -> &str {
            "perpetually-unhealthy"
        }
        async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        async fn on_stop(&self) {
            self.stops.fetch_add(1, Ordering::AcqRel);
        }
        async fn health(&self) -> ReplicaHealth {
            ReplicaHealth::unhealthy("never-recovers")
        }
    }

    #[tokio::test]
    async fn monitor_backoff_throttles_replaces_after_consecutive_failures() {
        // A daemon that's always unhealthy gets replaced each
        // poll. With backoff, the replacement count grows
        // logarithmically with elapsed time — not linearly with
        // tick count. After enough consecutive failures the
        // monitor should be skipping most ticks.
        let group = LifecycleGroup::<PerpetuallyUnhealthyDaemon>::spawn(1, [0u8; 32], |_idx| {
            Arc::new(PerpetuallyUnhealthyDaemon {
                starts: AtomicU64::new(0),
                stops: AtomicU64::new(0),
            })
        })
        .await
        .expect("spawn");
        let group = Arc::new(AsyncMutex::new(Some(group)));
        let monitor = HealthMonitor::spawn(
            group.clone(),
            |_idx| {
                Arc::new(PerpetuallyUnhealthyDaemon {
                    starts: AtomicU64::new(0),
                    stops: AtomicU64::new(0),
                })
            },
            Duration::from_millis(15),
        );

        // Run for many monitor intervals — 300 ms / 15 ms = ~20
        // ticks worth. Without backoff, that'd be ~20 replace
        // attempts. With backoff (1, 2, 4, 8, 16-tick steps)
        // we expect ~6 replaces (one per backoff doubling).
        tokio::time::sleep(Duration::from_millis(300)).await;

        let initiated = monitor
            .stats()
            .replacements_initiated
            .load(Ordering::Acquire);
        let skips = monitor.stats().backoff_skips.load(Ordering::Acquire);
        // Backoff should have skipped at least a few ticks by
        // now. Exact counts are timing-sensitive in CI, so we
        // assert directional invariants:
        assert!(
            initiated <= 12,
            "without backoff this would be 20+; got {initiated}"
        );
        assert!(
            skips >= 3,
            "expected backoff to skip at least 3 ticks; got {skips}"
        );

        monitor.stop().await;
        let g = Arc::try_unwrap(group)
            .map_err(|_| "still referenced")
            .expect("only ref")
            .into_inner();
        if let Some(lg) = g {
            lg.stop().await;
        }
    }
}
