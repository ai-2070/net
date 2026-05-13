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
//! - [`reconcile`] — `reconcile(actual, desired) -> Vec<MeshOsAction>`
//!   pure sync function. Phase A returns `vec![]`.
//! - [`event_loop`] — `MeshOsLoop` + `MeshOsHandle`. The loop
//!   body. `MeshOsHandle::publish` is the source-side fan-in API.

pub mod action;
pub mod config;
pub mod control;
pub mod event;
pub mod event_loop;
pub mod reconcile;
pub mod state;
pub mod supervision;

pub use action::{ActionId, AllocateActionId, MaintenanceTransition, MeshOsAction, PendingAction};
pub use config::{BackpressureConfig, MeshOsConfig};
pub use control::MeshOsControl;
pub use event::{
    AdminEvent, BlobAnnouncement, ChainId, DaemonHealth, DaemonIntent, DaemonIntentUpdate,
    DaemonLifecycleSignal, DaemonRef, MeshOsEvent, NodeHealth, NodeId, PlacementIntent,
    ReplicaUpdate,
};
pub use event_loop::{MeshOsHandle, MeshOsHandleError, MeshOsLoop};
pub use reconcile::{reconcile, STOP_GRACE_PERIOD};
pub use state::{
    AvoidEntry, BlobObservation, DaemonLifecycle, DaemonStatus, DesiredState, MaintenanceMirror,
    MeshOsState,
};
pub use supervision::{BackoffConfig, BackoffTracker, RestartState};
