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
