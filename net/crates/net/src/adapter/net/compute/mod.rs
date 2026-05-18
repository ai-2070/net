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
    /// observe total recovery work this tick. Held under a single
    /// lock for the duration — handlers are expected to be cheap;
    /// heavy work should be deferred to the action executor.
    ///
    /// A handler that panics is caught with `catch_unwind` and its
    /// slot is dropped from the registry, mirroring how
    /// `poll_probes` handles third-party-installed probe panics.
    pub fn try_run_all(&self) -> Vec<u8> {
        let mut guard = self.handlers.lock();
        let mut all = Vec::new();
        guard.retain_mut(|h| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h()));
            match result {
                Ok(recovered) => {
                    all.extend(recovered);
                    true
                }
                Err(_) => {
                    tracing::warn!(
                        target: "meshos",
                        "RecoveryRegistry: handler panicked; evicting"
                    );
                    false
                }
            }
        });
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
}
