//! `MeshOsEvent` — the union of inputs the canonical event loop
//! consumes. One enum, one ordering, one reconcile per tick.
//!
//! Per the plan's locked decision #1: existing substrate
//! subsystems (`ReplicationCoordinator`, `CortexAdapter`,
//! proximity graph, etc.) become *sources* that fan-in into the
//! single [`super::event_loop::MeshOsLoop`] receiver. Each
//! source pushes the subsystem-native signal into a converter
//! that emits a `MeshOsEvent`; the loop pops events in arrival
//! order.
//!
//! Phase A ships the enum + the supporting payload types. Later
//! phases attach real source converters to the existing
//! subsystems; Phase A's tests drive events directly through the
//! `mpsc::Sender` to exercise the ordering contract.

use std::time::{Duration, Instant};

/// Per-node identifier used by MeshOS. Aliased to `u64` for
/// consistency with the MeshDB surface (which is the federated
/// query layer MeshOS feeds the behavior snapshot to). The
/// older `behavior::metadata::NodeId = [u8; 32]` is the
/// substrate-wire form; the `u64` here is the
/// behavior-plane-internal form and matches the rest of MeshOS.
pub type NodeId = u64;

/// Per-event arrival order: enforced by the single-receiver
/// mpsc channel the loop owns. The [`super::event_loop::MeshOsLoop`]
/// pops one event at a time, updates state, and runs reconcile
/// at most once per [`MeshOsEvent::Tick`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeshOsEvent {
    /// Periodic timer tick. Drives the reconcile pass. Source: a
    /// dedicated timer task that fires every
    /// [`super::config::MeshOsConfig::tick_interval`]
    /// (default 500 ms — heartbeat-aligned).
    Tick,

    /// A replica was added / removed / lost / repaired on this
    /// node or one of its peers. Source (Phase C): the existing
    /// `ReplicationCoordinator`.
    ReplicaUpdate(ReplicaUpdate),

    /// A daemon's lifecycle changed (started / exited /
    /// crashed / reported health). Source (Phase B): the
    /// existing `DaemonRegistry`.
    DaemonLifecycle {
        /// Which daemon the signal belongs to.
        daemon: DaemonRef,
        /// What changed.
        signal: DaemonLifecycleSignal,
    },

    /// A fresh RTT sample arrived. Source: the proximity graph.
    RttSample {
        /// Peer the RTT was measured to.
        peer: NodeId,
        /// Measured round-trip time.
        rtt: Duration,
    },

    /// A peer's health flipped between Healthy / Degraded /
    /// Unhealthy. Source: the heartbeat tracker.
    NodeHealth {
        /// Peer whose health flipped.
        peer: NodeId,
        /// New health classification.
        health: NodeHealth,
    },

    /// An admin command was committed to the admin chain. Source
    /// (Phase D): the admin chain fold.
    AdminEvent(AdminEvent),

    /// A blob was announced / removed. Source (Phase E): the
    /// Dataforts capability fold.
    BlobAnnouncement(BlobAnnouncement),

    /// Desired-state placement intent updated. Source (Phase B+):
    /// the Dataforts placement fold.
    PlacementIntent(PlacementIntent),

    /// Desired-state daemon intent updated. Source (Phase B+):
    /// the Dataforts daemon-placement fold. Per-daemon "should
    /// be running here?" answer.
    DaemonIntentUpdate(DaemonIntentUpdate),

    /// Desired-state per-node replica intent. "Should this node
    /// hold a replica of `chain`?" Source (Phase C+): the
    /// leader's `RequestPlacement` / `RequestEviction` actions
    /// commit to the admin chain; each affected node's
    /// Dataforts fold projects them into a local intent.
    LocalReplicaIntent(LocalReplicaIntentUpdate),

    /// Replica leadership changed. Source (Phase C+):
    /// `replication_election`. The loop folds this into
    /// `MeshOsState::replica_leader`; reconcile reads it to gate
    /// `Request*` action emission.
    ReplicaLeaderUpdate {
        /// Chain whose leader changed.
        chain: ChainId,
        /// New leader, or `None` if leadership is currently
        /// vacant.
        leader: Option<NodeId>,
    },

    /// A maintenance-state transition was confirmed on the
    /// admin chain. Source (Phase E): the action executor's
    /// `CommitMaintenanceTransition` commit, re-observed via
    /// the chain. The fold uses this to advance
    /// `MeshOsState::local_maintenance` (when `node ==
    /// this_node`) and `MeshOsState::maintenance` (the per-peer
    /// mirror).
    MaintenanceTransitionObserved {
        /// Node whose state advanced.
        node: NodeId,
        /// New state.
        state: super::maintenance::MaintenanceState,
    },

    /// Cooperative loop shutdown. The loop drains pending events
    /// (no more reconcile passes) and exits.
    Shutdown,
}

/// Stable handle for a daemon — opaque pair of (registry-local
/// id, name). The id is the registry's choice (typically a
/// `u64`); name is the daemon's `MeshDaemon::name()`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct DaemonRef {
    /// Registry-local id.
    pub id: u64,
    /// Daemon name from `MeshDaemon::name()`.
    pub name: String,
}

/// What happened in the daemon's lifecycle. The state machine
/// the supervisor walks (Phase B) is `Stopped → Starting →
/// Running → Stopping → Stopped`, with `CrashLooping` as a
/// terminal failure state until cooldown.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DaemonLifecycleSignal {
    /// Daemon started successfully.
    Started {
        /// Wall time of the start.
        at: Instant,
    },
    /// Daemon exited without error (graceful shutdown).
    ExitedCleanly {
        /// Wall time of the exit.
        at: Instant,
    },
    /// Daemon crashed; supervisor logged the reason.
    Crashed {
        /// Wall time of the crash.
        at: Instant,
        /// Operator-readable reason.
        reason: String,
    },
    /// Daemon's `health()` self-report changed.
    HealthChanged {
        /// When the change was observed.
        at: Instant,
        /// New health classification.
        health: DaemonHealth,
    },
    /// Daemon's `saturation()` self-report changed.
    SaturationChanged {
        /// When the change was observed.
        at: Instant,
        /// New saturation value, `[0.0, 1.0]`.
        saturation: f32,
    },
}

/// Daemon-self-reported health. Re-exported from the trait
/// module ([`crate::adapter::net::compute::DaemonHealth`])
/// so MeshOS and the daemon trait stay in sync on one canonical
/// type — `MeshDaemon::health() -> DaemonHealth` is the same
/// `DaemonHealth` MeshOS folds.
pub use crate::adapter::net::compute::DaemonHealth;

/// Peer-level health, derived from heartbeat liveness.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NodeHealth {
    /// Peer is responsive within the heartbeat window.
    Healthy,
    /// Peer is responsive but slow / missing some heartbeats.
    Degraded,
    /// Peer hasn't responded inside the heartbeat window.
    Unreachable,
}

/// Replica-side event payload. Phase A keeps the shape minimal;
/// Phase C plumbs the rest of the `ReplicationCoordinator`
/// surface in.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ReplicaUpdate {
    /// A new replica was added — `holder` now hosts `chain`.
    Added {
        /// Chain whose replica count grew.
        chain: ChainId,
        /// Peer that just started hosting.
        holder: NodeId,
    },
    /// `holder` cleanly removed its `chain` replica.
    Removed {
        /// Chain whose replica count shrank.
        chain: ChainId,
        /// Peer that dropped the replica.
        holder: NodeId,
    },
    /// `holder` was hosting `chain` but is no longer reachable;
    /// the replica is presumed lost.
    Lost {
        /// Chain whose holder went unreachable.
        chain: ChainId,
        /// Peer presumed lost.
        holder: NodeId,
    },
    /// `holder` recovered after `Lost` and resumed hosting.
    Repaired {
        /// Chain whose replica recovered.
        chain: ChainId,
        /// Peer that came back online.
        holder: NodeId,
    },
}

/// Chain identifier — the substrate's 16-hex `u64` origin hash,
/// re-exported as a typed alias for the action / event surface.
pub type ChainId = u64;

/// Admin chain event. Phase D defines the full enum + the
/// chain-driven signing contract; Phase A carries the smallest
/// surface that lets the loop accept events through.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AdminEvent {
    /// Begin a maintenance window for `node`.
    EnterMaintenance {
        /// Node entering maintenance.
        node: NodeId,
        /// Optional deadline by which the EnteringMaintenance
        /// transition must complete (replicas drained, daemons
        /// stopped); past the deadline the node flips to
        /// DrainFailed.
        deadline: Option<Instant>,
    },
    /// End a maintenance window for `node`.
    ExitMaintenance {
        /// Node leaving maintenance.
        node: NodeId,
    },
    /// Drain `node`'s workload by `deadline`. Like a maintenance
    /// window but does not require an explicit Exit.
    Drain {
        /// Node to drain.
        node: NodeId,
        /// Deadline by which the drain must complete.
        deadline: Instant,
    },
    /// Mark `node` ineligible for new placements (existing
    /// workload stays).
    Cordon {
        /// Node to cordon.
        node: NodeId,
    },
    /// Remove a prior cordon.
    Uncordon {
        /// Node to un-cordon.
        node: NodeId,
    },
    /// Force-restart all daemons on `node` (operator command).
    RestartAllDaemons {
        /// Node whose daemons to bounce.
        node: NodeId,
    },
    /// Clear the local avoid list on `node` (operator command).
    ClearAvoidList {
        /// Node whose avoid list to clear.
        node: NodeId,
    },
    /// Drop the listed replicas from `node` (operator command).
    DropReplicas {
        /// Node whose replicas to drop.
        node: NodeId,
        /// Chains to evict.
        chains: Vec<ChainId>,
    },
    /// Force a placement recompute for `node` (operator command).
    InvalidatePlacement {
        /// Node whose placement to invalidate.
        node: NodeId,
    },
}

/// Blob announcement payload. Phase A skeleton — Phase E fleshes
/// out the fields the reconcile pass actually keys off of.
#[derive(Clone, Debug, PartialEq)]
pub struct BlobAnnouncement {
    /// Blob id (Dataforts-native u64).
    pub blob: u64,
    /// Peer publishing the announcement.
    pub holder: NodeId,
    /// Blob size in bytes.
    pub size_bytes: u64,
    /// `true` for add, `false` for remove.
    pub added: bool,
}

/// Desired-state placement intent. Source (Phase B+): the
/// Dataforts placement fold emits one of these per chain whose
/// desired replica count / placement preferences shifted.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementIntent {
    /// Chain whose intent changed.
    pub chain: ChainId,
    /// Desired replica count for the chain.
    pub desired_replicas: u32,
}

/// Per-daemon intent — should this daemon be running on this
/// node, or stopped? Source (Phase B+): the Dataforts
/// daemon-placement fold.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DaemonIntent {
    /// Daemon should be running locally.
    Run,
    /// Daemon should not be running locally.
    Stop,
}

/// Desired-state daemon intent update. Paired form of
/// [`DaemonIntent`] keyed by the [`DaemonRef`] it applies to.
#[derive(Clone, Debug, PartialEq)]
pub struct DaemonIntentUpdate {
    /// Daemon whose intent changed.
    pub daemon: DaemonRef,
    /// New intent.
    pub intent: DaemonIntent,
}

/// Per-node replica intent — should this node hold a replica of
/// the chain, or drop one it currently has? Phase C input
/// shape; the Dataforts placement fold projects
/// `RequestPlacement`/`RequestEviction` admin-chain commits
/// into one of these for each affected node.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LocalReplicaIntent {
    /// This node should hold a replica of the chain.
    Hold,
    /// This node should NOT hold a replica of the chain.
    Drop,
}

/// Update event for [`LocalReplicaIntent`].
#[derive(Clone, Debug, PartialEq)]
pub struct LocalReplicaIntentUpdate {
    /// Chain whose local intent changed.
    pub chain: ChainId,
    /// New intent.
    pub intent: LocalReplicaIntent,
}
