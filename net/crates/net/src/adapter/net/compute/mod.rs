//! Layer 5: Compute Runtime for Net.
//!
//! Defines the `MeshDaemon` trait for event processors, `DaemonHost` for
//! runtime management, `DaemonRegistry` for local daemon tracking,
//! `Scheduler` for capability-based placement, and `MigrationState`
//! for snapshot-based daemon migration.

pub mod bindings;
mod daemon;
pub mod daemon_factory;
pub mod fork_group;
pub mod group_coord;
mod host;
mod migration;
pub mod migration_source;
pub mod migration_target;
pub mod orchestrator;
mod registry;
pub mod replica_group;
mod scheduler;
pub mod standby_group;

pub use bindings::{DaemonBindings, SubscriptionBinding};
pub use daemon::{
    DaemonControl, DaemonError, DaemonHealth, DaemonHostConfig, DaemonLifecycleEvent,
    DaemonLifecycleObserver, DaemonStats, MeshDaemon,
};
pub use daemon_factory::{DaemonFactoryRegistry, FactoryEntry};
pub use fork_group::{ForkGroup, ForkGroupConfig, ForkInfo};
pub use group_coord::{GroupCoordinator, GroupError, GroupHealth, MemberInfo};
pub use host::DaemonHost;
pub use migration::{
    MigrationError, MigrationFailureReason, MigrationPhase, MigrationState, SUBPROTOCOL_MIGRATION,
};
pub use migration_source::MigrationSourceHandler;
pub use migration_target::MigrationTargetHandler;
pub use orchestrator::{
    chunk_snapshot, MigrationMessage, MigrationOrchestrator, SnapshotReassembler,
    MAX_SNAPSHOT_CHUNK_SIZE, MAX_SNAPSHOT_SIZE,
};
pub use registry::DaemonRegistry;
pub use replica_group::{ReplicaGroup, ReplicaGroupConfig, SUBPROTOCOL_REPLICA_GROUP};
pub use scheduler::{PlacementDecision, PlacementReason, Scheduler, SchedulerError};
pub use standby_group::{MemberRole, StandbyGroup, StandbyGroupConfig};

/// Recovery hook for replication / fork / standby groups whose
/// slots got marked unhealthy after a placement failure.
///
/// `on_node_failure*` paths in all three group types `mark_unhealthy`
/// the affected slot BEFORE attempting placement so traffic stops
/// routing to a dead node immediately. On a placement failure
/// (no healthy candidate at the moment) the slot stays unhealthy
/// with the dead node's `origin_hash` in the registry. The group's
/// per-node `on_node_recovery` only re-marks the slot healthy when
/// the recovered node id matches the FAILED node id — recovery of
/// a DIFFERENT spare node (which arrives later and could host the
/// slot) silently never retries placement.
///
/// Groups that opt in implement this trait and register themselves
/// with the meshos runtime's recovery registry; the loop's
/// reconcile tick walks every registered group, checks
/// `has_unhealthy_slots`, and calls `try_recover` with the live
/// scheduler. The cap per tick lets a pathological "every slot
/// unhealthy" state make progress without wedging the loop.
/// Type-erased per-tick recovery handler. Holds a closure that
/// captures everything `try_recover` needs (the group itself, a
/// scheduler clone, a daemon-registry clone, and the
/// daemon-factory closure). Returns the slot indices the closure
/// successfully recovered this tick.
///
/// `Box<dyn FnMut + Send>` rather than the trait directly so the
/// registry can store heterogeneous group types (StandbyGroup,
/// ForkGroup, ReplicaGroup, …) in one collection. Each caller
/// constructs a handler like:
///
/// ```ignore
/// let group = Arc::new(parking_lot::Mutex::new(my_standby_group));
/// let scheduler = scheduler.clone();
/// let registry = daemon_registry.clone();
/// runtime.recovery_registry().register(Box::new(move || {
///     group.lock().try_recover(
///         &scheduler,
///         &registry,
///         &|| Box::new(MyDaemon::new()),
///     )
/// }));
/// ```
pub type RecoveryHandler = Box<dyn FnMut() -> Vec<u8> + Send>;

/// Per-runtime collection of recovery handlers driven by the
/// meshos tick. Cheap to clone (Arc-wrapped); each
/// `MeshOsRuntime` exposes one via `recovery_registry()` and the
/// loop's tick handler calls `try_run_all` after `poll_probes`
/// each tick.
///
/// Dropped handlers (e.g. the underlying group was dropped) are
/// detected via panic in the closure and removed on the next
/// pass. Handlers MUST be cheap — they run inside the tick
/// handler's hot path; expensive recovery work should be
/// dispatched off-loop via the action queue.
#[derive(Clone, Default)]
pub struct RecoveryRegistry {
    handlers: std::sync::Arc<parking_lot::Mutex<Vec<RecoveryHandler>>>,
}

impl RecoveryRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a recovery handler. Returns the number of registered
    /// handlers after the insert. The handler runs once per tick
    /// from the meshos loop.
    pub fn register(&self, handler: RecoveryHandler) -> usize {
        let mut guard = self.handlers.lock();
        guard.push(handler);
        guard.len()
    }

    /// Number of registered handlers — used by tests + operator
    /// dashboards.
    pub fn len(&self) -> usize {
        self.handlers.lock().len()
    }

    /// `true` when no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.lock().is_empty()
    }

    /// Run every registered handler once. Concatenates the
    /// recovered slot indices each handler reports so callers can
    /// observe total recovery work this tick.
    ///
    /// Handlers run OUTSIDE the registry lock — the registry briefly
    /// takes the lock to swap the handler vector out, runs each
    /// handler in `catch_unwind` against the local vector, then
    /// re-takes the lock to merge survivors back. This prevents two
    /// hazards:
    ///   - **Reentrancy deadlock.** `parking_lot::Mutex` is non-
    ///     reentrant; a handler that calls back into `register` /
    ///     `len` / `is_empty` (directly or via the meshos tick path)
    ///     would self-deadlock if the registry lock were held across
    ///     the invocation.
    ///   - **Concurrent registration.** A new handler installed
    ///     mid-run lands on the now-empty registry vector and runs
    ///     on the next tick; pre-fix the register lock would have
    ///     blocked until the long handler chain finished.
    ///
    /// A handler that panics is caught with `catch_unwind` and its
    /// slot is dropped from the registry, mirroring how
    /// `poll_probes` handles third-party-installed probe panics.
    pub fn try_run_all(&self) -> Vec<u8> {
        // Swap the handler vec out under a brief lock so we can
        // invoke each handler with the registry lock released.
        let handlers_to_run = {
            let mut guard = self.handlers.lock();
            std::mem::take(&mut *guard)
        };
        let mut survivors: Vec<RecoveryHandler> = Vec::with_capacity(handlers_to_run.len());
        let mut all = Vec::new();
        for mut h in handlers_to_run {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut h));
            match result {
                Ok(recovered) => {
                    all.extend(recovered);
                    survivors.push(h);
                }
                Err(_) => {
                    tracing::warn!(
                        target: "meshos",
                        "RecoveryRegistry: handler panicked; evicting"
                    );
                    // h drops here; handler evicted.
                }
            }
        }
        // Merge survivors back. Any handler installed concurrently
        // landed on the now-non-empty vector; append survivors
        // before the new entries so registration order roughly
        // matches the pre-fix iteration order (new entries run on
        // the next tick, not this one).
        let mut guard = self.handlers.lock();
        if guard.is_empty() {
            *guard = survivors;
        } else {
            let mut combined = survivors;
            combined.extend(guard.drain(..));
            *guard = combined;
        }
        all
    }
}

impl std::fmt::Debug for RecoveryRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryRegistry")
            .field("handlers", &self.handlers.lock().len())
            .finish()
    }
}

/// Direct per-group recovery contract — implemented by every
/// group type that owns slots placed onto specific nodes. The
/// `RecoveryRegistry` above wraps `try_recover` calls in
/// closures so the meshos loop can run heterogeneous groups
/// uniformly; this trait remains the single source of truth for
/// per-group recovery semantics.
pub trait UnhealthySlotRecovery: Send + Sync {
    /// `true` when at least one slot is marked unhealthy and a
    /// `try_recover` call could attempt placement. Cheap probe so
    /// the recovery tick can skip groups with nothing to do.
    fn has_unhealthy_slots(&self) -> bool;

    /// Attempt to place every unhealthy slot against the current
    /// healthy node pool. Returns the slot indices that were
    /// successfully recovered. Implementations cap the recovery
    /// work per call (e.g. at most 4 slots) so a pathological
    /// "every slot unhealthy" state doesn't wedge the caller.
    ///
    /// `daemon_factory` produces a fresh boxed `MeshDaemon` per
    /// recovered slot — the group's existing daemon-keypair /
    /// chain plumbing rebuilds around it.
    fn try_recover(
        &mut self,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        daemon_factory: &dyn Fn() -> Box<dyn MeshDaemon>,
    ) -> Vec<u8>;
}

#[cfg(test)]
mod recovery_registry_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn try_run_all_collects_recovered_slots_and_drops_panicking_handlers() {
        let reg = RecoveryRegistry::new();
        assert!(reg.is_empty());

        // Handler A: reports two recovered slots each tick.
        let calls_a = Arc::new(AtomicU32::new(0));
        let counter_a = calls_a.clone();
        reg.register(Box::new(move || {
            counter_a.fetch_add(1, Ordering::Relaxed);
            vec![0, 1]
        }));

        // Handler B: panics on first call so the registry evicts
        // it. Subsequent ticks should observe only handler A.
        let calls_b = Arc::new(AtomicU32::new(0));
        let counter_b = calls_b.clone();
        reg.register(Box::new(move || {
            counter_b.fetch_add(1, Ordering::Relaxed);
            panic!("simulated handler panic");
        }));

        assert_eq!(reg.len(), 2);

        // First tick: A returns [0, 1]; B panics and is evicted.
        let recovered = reg.try_run_all();
        assert_eq!(recovered, vec![0, 1]);
        assert_eq!(reg.len(), 1, "panicking handler must be evicted");
        assert_eq!(calls_a.load(Ordering::Relaxed), 1);
        assert_eq!(calls_b.load(Ordering::Relaxed), 1);

        // Second tick: only A runs; B has been evicted.
        let recovered = reg.try_run_all();
        assert_eq!(recovered, vec![0, 1]);
        assert_eq!(reg.len(), 1);
        assert_eq!(calls_a.load(Ordering::Relaxed), 2);
        // B did NOT run again — evicted on first tick.
        assert_eq!(calls_b.load(Ordering::Relaxed), 1);
    }

    /// Regression: `try_run_all` must not hold the registry mutex
    /// across handler invocation, otherwise a handler that
    /// reentrantly touches the registry (calls `register` /
    /// `len` / `is_empty`) would self-deadlock —
    /// `parking_lot::Mutex` is non-reentrant. The fix swaps the
    /// handler vec out under a brief lock, runs handlers with
    /// the lock released, then merges survivors back.
    #[test]
    fn try_run_all_allows_handler_to_register_new_handler() {
        let reg = RecoveryRegistry::new();
        let reg_for_handler = reg.clone();
        let invoked = Arc::new(AtomicU32::new(0));
        let invoked_for_handler = invoked.clone();

        reg.register(Box::new(move || {
            // Reentrant call into the same registry. Pre-fix
            // this would deadlock because `try_run_all` held the
            // mutex across this invocation; `register` then
            // blocked on the same non-reentrant lock.
            reg_for_handler.register(Box::new(|| vec![99]));
            invoked_for_handler.fetch_add(1, Ordering::Relaxed);
            vec![7]
        }));
        assert_eq!(reg.len(), 1);

        let recovered = reg.try_run_all();
        assert_eq!(recovered, vec![7]);
        assert_eq!(invoked.load(Ordering::Relaxed), 1);
        // The handler installed a second handler during its run;
        // that one runs on the NEXT tick (not this one).
        assert_eq!(
            reg.len(),
            2,
            "concurrently-registered handler must land in the registry",
        );

        // Next tick: both handlers run. The original registers
        // ANOTHER handler each tick, so this grows by one per
        // tick — which is the user's design choice and proves
        // the no-deadlock contract holds across repeated runs.
        let recovered = reg.try_run_all();
        // Handler order between survivors and the freshly-installed
        // one is unspecified; sort to make the assertion robust.
        let mut got = recovered;
        got.sort();
        assert_eq!(got, vec![7, 99]);
    }

    /// Regression: handlers registered concurrently with `try_run_all`
    /// land in the registry without blocking on a long handler
    /// chain. With the swap-and-merge fix, `register` only briefly
    /// contends with the take + merge steps; in the prior code it
    /// blocked for the entire handler-run duration.
    #[test]
    fn register_during_try_run_all_does_not_block_indefinitely() {
        use std::sync::Barrier;
        use std::thread;
        use std::time::{Duration, Instant};

        let reg = Arc::new(RecoveryRegistry::new());
        let barrier_start = Arc::new(Barrier::new(2));
        let in_handler = Arc::new(AtomicU32::new(0));

        // Slow handler: bumps in_handler then sleeps. The thread
        // calling try_run_all spins inside this for the sleep
        // duration.
        let in_handler_for_h = in_handler.clone();
        let barrier_for_h = barrier_start.clone();
        reg.register(Box::new(move || {
            in_handler_for_h.fetch_add(1, Ordering::SeqCst);
            barrier_for_h.wait();
            std::thread::sleep(Duration::from_millis(200));
            vec![0]
        }));

        let reg_runner = reg.clone();
        let runner = thread::spawn(move || reg_runner.try_run_all());

        // Wait until the handler is mid-execution.
        barrier_start.wait();

        // Try to register a fresh handler. With the fix this
        // returns quickly (no contention on the handlers lock past
        // the brief swap). Pre-fix it would block for the full
        // 200 ms sleep before returning.
        let reg_writer = reg.clone();
        let started = Instant::now();
        reg_writer.register(Box::new(|| vec![42]));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "register must not block on a long-running handler — \
             elapsed = {elapsed:?}; pre-fix this would have been ~200 ms",
        );

        runner.join().unwrap();
        // Handler chain landed correctly.
        assert_eq!(in_handler.load(Ordering::SeqCst), 1);
    }
}
