//! `MeshOsAction` — the union of outputs the reconcile pass
//! emits. One action per substrate-side change MeshOS wants to
//! see happen; the action executor drains the queue under the
//! backpressure layer (Phase G) and dispatches each action to
//! the subsystem that owns its mechanics.
//!
//! Named `MeshOsAction` (not bare `Action`) to avoid colliding
//! with [`crate::adapter::net::behavior::rules::Action`], which
//! the rules engine already publishes at the behavior plane
//! root.
//!
//! Phase A ships the enum + an `ActionId` allocator + the
//! `#[non_exhaustive]` attribute so later phases add variants
//! without breaking downstream matches. The reconcile pass
//! returns an empty `Vec<MeshOsAction>` until Phase B.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::event::{ChainId, DaemonRef, NodeId};

/// Monotonic per-process action id. Allocated by
/// [`AllocateActionId::next`] when reconcile produces an action;
/// used as the snapshot-fold key so Deck can correlate
/// "in-flight" / "pending" / "completed" across the same id.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ActionId(
    /// Raw monotonic value. Process-local; not stable across
    /// node restarts.
    pub u64,
);

/// Process-global action-id allocator.
#[derive(Debug, Default)]
pub struct AllocateActionId {
    counter: AtomicU64,
}

impl AllocateActionId {
    /// Construct an allocator whose first handed-out id is `1`.
    /// Zero is reserved as a "no action" sentinel for downstream
    /// snapshots.
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
        }
    }

    /// Allocate the next id. Threadsafe, lock-free.
    pub fn next(&self) -> ActionId {
        ActionId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

/// The action payload. Variants reflect the seven action
/// families the plan calls out (`start_daemon` / `stop_daemon` /
/// `migrate_blob` / `pull_replica` / `reduce_heat` /
/// `mark_avoid` / `apply_backoff`) plus the maintenance-state
/// transitions Phase E drives.
///
/// **`Instant`-typed deadlines are process-local.** `StopDaemon`
/// and `ApplyBackoff` carry `std::time::Instant`, which is
/// monotonic-per-process and not portable across nodes. These
/// actions are produced and consumed inside one MeshOS process;
/// the chain-recorded form (`ActionChainRecord`) flattens
/// deadlines to Unix-epoch ms for cross-node replay. Don't move
/// an in-process `MeshOsAction` over the wire — go through the
/// chain record.
///
/// `#[non_exhaustive]` so phases B–G land their handlers without
/// breaking downstream matches.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeshOsAction {
    /// Phase B. Start a daemon that should be running on this
    /// node but isn't.
    StartDaemon {
        /// Daemon the supervisor should bring up.
        daemon: DaemonRef,
    },

    /// Phase B. Stop a daemon that's running locally but
    /// shouldn't be. `reason` rides along into the failure /
    /// audit log.
    StopDaemon {
        /// Daemon to shut down.
        daemon: DaemonRef,
        /// Operator-readable reason recorded to the audit log.
        reason: String,
        /// Deadline by which `MeshOsControl::Shutdown` must
        /// produce a clean exit; afterward the supervisor force-
        /// terminates.
        deadline: Instant,
    },

    /// Phase C. Pull a replica this node should hold but
    /// doesn't.
    PullReplica {
        /// Chain whose replica to materialize.
        chain: ChainId,
        /// Peer to pull the bytes from.
        source: NodeId,
    },

    /// Phase C. Drop a stale replica.
    DropReplica {
        /// Chain whose local replica to evict.
        chain: ChainId,
    },

    /// Phase C — leader-only. Ask another node to host a
    /// replica. Emitted by the leader for a chain whose desired
    /// replica count is short.
    RequestPlacement {
        /// Chain whose replica count is short.
        chain: ChainId,
        /// Peers that already hold the chain (or are otherwise
        /// not candidates).
        exclude: Vec<NodeId>,
    },

    /// Phase C — leader-only. Ask a peer to drop a replica it
    /// holds (chain over-replicated, or its score has dropped).
    RequestEviction {
        /// Chain whose replica to evict.
        chain: ChainId,
        /// Peer that should drop its copy.
        victim: NodeId,
    },

    /// Phase E. Migrate a blob between holders (Dataforts
    /// movement layer is the muscle; MeshOS owns the trigger).
    MigrateBlob {
        /// Blob id (Dataforts-native).
        blob: u64,
        /// Current holder.
        from: NodeId,
        /// Target holder.
        to: NodeId,
    },

    /// Phase E. Reduce a blob's heat (gravity-driven movement
    /// follows the heat down).
    ReduceHeat {
        /// Blob whose heat counter to decrement.
        blob: u64,
        /// Amount to subtract.
        by: u32,
    },

    /// Phase D. Add a peer to the avoid list for the configured
    /// TTL. RTT-degradation or operator command.
    MarkAvoid {
        /// Peer to deprioritize.
        peer: NodeId,
        /// Why we're avoiding (audit + Deck rendering).
        reason: String,
        /// How long the avoid entry persists before GC.
        ttl: Duration,
    },

    /// Phase G. Apply backoff to a daemon's restart loop after
    /// it crashed. The backoff window is computed by the
    /// supervisor; MeshOS just commits the decision.
    ApplyBackoff {
        /// Daemon whose restart loop to gate.
        daemon: DaemonRef,
        /// Earliest instant a restart may be admitted.
        until: Instant,
    },

    /// Phase E. Commit a maintenance-state transition for a
    /// node. The state machine lives in chain metadata
    /// (`metadata.maintenance_state`); the action is the chain
    /// commit, the metadata write is the effect.
    CommitMaintenanceTransition {
        /// Node whose maintenance state is changing.
        node: NodeId,
        /// Target state.
        target: MaintenanceTransition,
    },
}

/// Phase E. The set of maintenance-state transitions the
/// reconcile pass can commit. Each transition is idempotent —
/// applying it twice is a no-op.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaintenanceTransition {
    /// Active → EnteringMaintenance. Triggers replica freeze +
    /// daemon drain.
    EnteringMaintenance,
    /// EnteringMaintenance → Maintenance. Replicas migrated,
    /// daemons drained; node is steady-state isolated.
    Maintenance,
    /// Maintenance → ExitingMaintenance. Operator requested
    /// resume; node restarts daemons + emits fresh capabilities.
    ExitingMaintenance,
    /// EnteringMaintenance → DrainFailed. Deadline elapsed with
    /// replicas / daemons unable to drain; needs operator action.
    /// `reason` carries the deadline / observed-condition message
    /// so the chain commit can record it; the
    /// `MaintenanceTransitionObserved` fold consumes it into
    /// `MaintenanceState::DrainFailed { reason }`.
    DrainFailed {
        /// Operator-readable reason recorded into the chain
        /// commit and the local mirror.
        reason: String,
    },
    /// ExitingMaintenance → Recovery. Ramp-up window active;
    /// node is on the avoid list for new placements.
    Recovery,
    /// Recovery → Active. Ramp-up window complete; node is
    /// fully back in service.
    Active,
}

/// A queued action ready for the action executor to admit. Pairs
/// the action with the id the snapshot fold keys off.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingAction {
    /// Allocated id; stable for the action's full lifetime
    /// (pending → in-flight → completed/failed).
    pub id: ActionId,
    /// The action payload.
    pub action: MeshOsAction,
    /// When reconcile emitted it. Used by Deck to render
    /// queue latency.
    pub emitted_at: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_hands_out_monotonic_unique_ids() {
        let alloc = AllocateActionId::new();
        let a = alloc.next();
        let b = alloc.next();
        let c = alloc.next();
        assert_eq!(a, ActionId(1));
        assert_eq!(b, ActionId(2));
        assert_eq!(c, ActionId(3));
        assert!(a != b && b != c);
    }
}
