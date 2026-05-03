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
pub use daemon::{DaemonError, DaemonHostConfig, DaemonStats, MeshDaemon};
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
    chunk_snapshot, BufferOutcome, MigrationMessage, MigrationOrchestrator, SnapshotReassembler,
    MAX_SNAPSHOT_CHUNK_SIZE, MAX_SNAPSHOT_SIZE,
};
pub use registry::DaemonRegistry;
pub use replica_group::{ReplicaGroup, ReplicaGroupConfig, SUBPROTOCOL_REPLICA_GROUP};
pub use scheduler::{PlacementDecision, PlacementReason, Scheduler, SchedulerError};
pub use standby_group::{MemberRole, StandbyGroup, StandbyGroupConfig};
