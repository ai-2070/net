//! MeshOS — the cluster-behavior engine. One canonical event
//! loop per node that reconciles desired vs actual state,
//! supervises daemons, enforces replica placement, applies admin
//! intent, and folds the result into a behavior snapshot for
//! Deck.
//!
//! Phase A of [`MESHOS_PLAN.md`](../../../../../docs/plans/MESHOS_PLAN.md) —
//! the skeleton. Lands the module shape + the canonical types +
//! the loop body. Reconcile returns an empty action list under
//! every input; the action executor drains an empty queue under
//! steady state. Later phases fill in:
//!
//! - Phase B → daemon supervision (`MeshDaemon` extension,
//!   crash-loop gating, graceful shutdown).
//! - Phase C → replica enforcement (pull / drop / placement /
//!   eviction; leader-only `Request*` actions).
//! - Phase D → locality + admin event handling; the body of
//!   [`MESH_SCHEDULER_PLAN.md`](../../../../../docs/plans/MESH_SCHEDULER_PLAN.md)
//!   lands here as Phase D-1.
//! - Phase E → maintenance state machine (Active →
//!   EnteringMaintenance → Maintenance → ExitingMaintenance →
//!   Recovery + DrainFailed).
//! - Phase F → behavior snapshot fold (`RedexFold<MeshOsSnapshot>`).
//! - Phase G → safety + backpressure (`admit()` over the action
//!   executor).
//!
//! # Activation
//!
//! Gated behind the `meshos` Cargo feature. Disabled by default;
//! activation requires a concrete consumer workload (the Deck UI
//! is the named near-term consumer, plus Dataforts producing
//! enough placement intent to drive reconciliation end-to-end).
//!
//! # Surface map
//!
//! - [`event`] — `MeshOsEvent` + the supporting payloads. The
//!   single-stream input the loop consumes.
//! - [`action`] — `MeshOsAction` + `ActionId` + `PendingAction`.
//!   Reconcile's emitted-action surface. Disjoint from
//!   [`crate::adapter::net::behavior::rules::Action`] (rules
//!   engine).
//! - [`state`] — `MeshOsState` (actual, folded from events) +
//!   `DesiredState` (folded from placement intent).
//! - [`config`] — `MeshOsConfig` + `BackpressureConfig`. Defaults
//!   match the plan's locked decisions (tick = 500 ms heartbeat-
//!   aligned; queue capacities = 1024).
//! - [`mod@reconcile`] — `reconcile(actual, desired) -> Vec<MeshOsAction>`
//!   pure sync function. Phase A returns `vec![]`.
//! - [`event_loop`] — `MeshOsLoop` + `MeshOsHandle`. The loop
//!   body. `MeshOsHandle::publish` is the source-side fan-in API.

pub mod action;
pub mod audit_chain;
pub mod backpressure;
pub mod chain;
pub mod config;
pub mod control;
pub mod event;
pub mod event_loop;
pub mod executor;
pub mod failure_chain;
pub mod ice;
pub mod log_chain;
pub mod logs;
pub mod maintenance;
pub mod migration_aborter;
pub mod migration_snapshot_source;
pub mod probes;
pub mod reconcile;
pub mod redex_appenders;
pub mod runtime;
pub mod scheduler;
pub mod sdk;
pub mod snapshot;
pub mod sources;
pub mod state;
pub mod supervision;

pub use action::{ActionId, AllocateActionId, MaintenanceTransition, MeshOsAction, PendingAction};
pub use audit_chain::{
    AdminAuditAppendError, AdminAuditChainAppender, BufferingAdminAuditChainAppender,
    NoOpAdminAuditChainAppender, DEFAULT_AUDIT_BUFFERING_APPENDER_CAPACITY,
};
pub use backpressure::{admit, AdmissionResult, BackpressureState, ClusterBackpressureChange};
pub use chain::{
    append_dispatched, append_failed, append_gated, record_from, ActionChainAppender,
    ActionChainRecord, ActionDisposition, AppendError, BufferingActionChainAppender,
    MeshOsSnapshotFold, NoOpActionChainAppender,
};
pub use config::{BackpressureConfig, LocalityConfig, MaintenanceConfig, MeshOsConfig};
pub use control::{ControlSink, MeshOsControl};
pub use event::{
    AdminEvent, AvoidScope, BlobAnnouncement, ChainId, DaemonHealth, DaemonIntent,
    DaemonIntentUpdate, DaemonLifecycleSignal, DaemonRef, LocalReplicaIntent,
    LocalReplicaIntentUpdate, MeshOsEvent, MigrationId, NodeHealth, NodeId, PlacementIntent,
    ReplicaUpdate,
};
pub use event_loop::{
    MeshOsHandle, MeshOsHandleError, MeshOsLoop, MeshOsLoopParts, MeshOsSnapshotReader,
    ProbeRegistry,
};
pub use executor::{
    ActionDispatcher, ActionExecutor, DispatchError, ExecutorHandle, ExecutorStats,
    ExecutorStatsSnapshot, LoggingDispatcher,
};
pub use failure_chain::{
    BufferingFailureChainAppender, FailureAppendError, FailureChainAppender,
    NoOpFailureChainAppender, DEFAULT_FAILURE_BUFFERING_APPENDER_CAPACITY,
};
pub use ice::{
    admin_event_signing_payload, blast_radius_hash, ice_proposal_signing_payload,
    now_ms_since_unix_epoch, simulate as simulate_ice_proposal, AdminAuditRecord, AdminVerifier,
    BlastRadius, BlastRadiusHash, BlastWarning, IceActionProposal, OperatorRegistry,
    OperatorSignature, VerificationOutcome, VerifyError, ADMIN_SIGNING_DOMAIN,
    BLAST_RADIUS_HASH_LEN, DEFAULT_ICE_COOLDOWN_WINDOW, DEFAULT_MAX_ADMIN_AUDIT_RECORDS,
    DEFAULT_SIGNING_FRESHNESS_WINDOW, DEFAULT_SIGNING_FUTURE_SKEW, ICE_SIGNING_DOMAIN,
    SIMULATION_REQUIRED_SENTINEL,
};
pub use log_chain::{
    BufferingLogChainAppender, LogAppendError, LogChainAppender, NoOpLogChainAppender,
    DEFAULT_LOG_BUFFERING_APPENDER_CAPACITY,
};
pub use logs::{LogLevel, LogLine, LogRecord, DEFAULT_MAX_LOG_RING_RECORDS};
pub use maintenance::MaintenanceState;
pub use migration_aborter::{
    BufferingMigrationAborter, MigrationAbortError, MigrationAborter, NoOpMigrationAborter,
    OrchestratorMigrationAborter, DEFAULT_MIGRATION_ABORT_BUFFERING_CAPACITY,
};
pub use migration_snapshot_source::{
    MigrationSnapshotSource, NoOpMigrationSnapshotSource, OrchestratorMigrationSnapshotSource,
};
pub use probes::{
    HealthProbe, InventoryProbe, LocalityProbe, PeerInventory, ProximityGraphHealthProbe,
    ProximityGraphLocalityProbe,
};
pub use reconcile::{reconcile, STOP_GRACE_PERIOD};
pub use redex_appenders::{RedexAdminAuditAppender, RedexFailureAppender, RedexLogAppender};
pub use runtime::{MeshOsRuntime, MeshOsRuntimeBuilder, RuntimeShutdownError, RuntimeStats};
pub use scheduler::{
    LocalScheduler, PlacementScorer, ScoreHistory, ScoreSnapshot, SchedulerConfig,
    SchedulerRegistry, SnapshotScorer, Trend,
};
pub use sdk::{
    DaemonControlRouter, MaintenanceStateView, MeshOsDaemonHandle, MeshOsDaemonSdk, MetadataView,
    SdkError, SdkRoutingDispatcher, DEFAULT_CONTROL_CHANNEL_CAPACITY, DEFAULT_GRACEFUL_SHUTDOWN,
};
pub use snapshot::{
    action_kind_str, AvoidEntrySnapshot, DaemonHealthSnapshot, DaemonLifecycleSnapshot,
    DaemonSnapshot, FailureRecord, MaintenanceMirrorSnapshot, MaintenanceStateSnapshot,
    MeshOsSnapshot, MigrationPhaseSnapshot, MigrationSnapshot, PeerHealthSnapshot, PeerSnapshot,
    PendingActionSnapshot, ReplicaSnapshot, RestartStateSnapshot, RECENT_FAILURES_CAPACITY,
};
pub use sources::{
    attach_to_daemon_registry, attach_to_replication_coordinator, MeshOsDaemonLifecycleSink,
    MeshOsReplicaTransitionSink,
};
pub use state::{
    AvoidEntry, BlobObservation, DaemonLifecycle, DaemonStatus, DesiredState, MaintenanceMirror,
    MeshOsState,
};
pub use supervision::{BackoffConfig, BackoffTracker, RestartState};
