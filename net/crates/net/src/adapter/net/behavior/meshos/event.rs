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

    /// A multi-operator-signed ICE proposal arrived from the
    /// Deck SDK. The loop's optional admin verifier checks every
    /// signature in the bundle against the cluster's registered
    /// operator policy before folding the inner
    /// [`AdminEvent`]; if verification fails the event drops and
    /// the failure is recorded for operator visibility.
    SignedIceCommit {
        /// The proposal that the bundle signed over. The loop
        /// verifies each signature against
        /// [`super::ice::ice_proposal_signing_payload`].
        proposal: super::ice::IceActionProposal,
        /// Operator signatures collected for this proposal. Per
        /// the plan's locked decision #3 the substrate verifier
        /// requires the bundle to meet the cluster's configured
        /// `ice_signature_threshold`; the SDK-side gate also
        /// enforces this so under-threshold bundles fail before
        /// they reach the loop.
        signatures: Vec<super::ice::OperatorSignature>,
    },

    /// A single-operator-signed ordinary admin commit arrived
    /// from the Deck SDK. Like `SignedIceCommit` but for non-
    /// ICE admin events (drain, cordon, drop_replicas, …) —
    /// these are single-operator by design, so one signature
    /// per commit rather than a bundle. The loop's optional
    /// admin verifier checks the signature against the
    /// cluster's registered operator policy before folding the
    /// inner [`AdminEvent`]; failed verifications land on the
    /// audit ring with `VerificationOutcome::Rejected` and the
    /// inner event drops.
    SignedAdminCommit {
        /// The admin event the signature covers. The loop
        /// verifies via
        /// [`super::ice::admin_event_signing_payload`].
        event: AdminEvent,
        /// Issuing operator's signature over the event's
        /// signing payload.
        signature: super::ice::OperatorSignature,
    },

    /// A log line published by a daemon, source converter,
    /// or substrate-internal component. The loop stamps a
    /// monotonic seq + wall-clock timestamp + this node's id
    /// before pushing onto the per-node log ring. The Deck
    /// SDK's `subscribe_logs` reads the ring through the
    /// snapshot.
    LogLine(super::logs::LogLine),

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

    /// Bundled "leader stepped down AND node is no longer a
    /// holder" event. Source: the replication coordinator's
    /// `Leader → Idle` transition fires this as one event so a
    /// downstream sink cannot drop half of the
    /// (holder-removed, leader-cleared) pair under backpressure.
    /// The fold updates both `replicas[chain]` (removes
    /// `holder`) and `replica_leader[chain]` (clears) atomically.
    ReplicaLeaderLostAndRemoved {
        /// Chain whose holder + leader both changed.
        chain: ChainId,
        /// Node that lost both its holder slot and its leader role.
        holder: NodeId,
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
///
/// Implements `Serialize` / `Deserialize` so wire forms that
/// carry daemon references (e.g. ICE [`super::ice::BlastRadius`])
/// can round-trip postcard / JSON without a per-call projection.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
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
        /// Monotonic timestamp of the start.
        at: Instant,
    },
    /// Daemon exited without error (graceful shutdown).
    ExitedCleanly {
        /// Monotonic timestamp of the exit.
        at: Instant,
    },
    /// Daemon crashed; supervisor logged the reason.
    Crashed {
        /// Monotonic timestamp of the crash.
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

/// Stable identifier for a daemon-state-migration the
/// compute layer's `MigrationOrchestrator` runs. Wire form is
/// a `u64` so the SDK and substrate agree without exposing
/// the orchestrator's internal id space. The dispatcher
/// integration that maps this id to a running migration is
/// future substrate work.
pub type MigrationId = u64;

/// Admin chain event. Phase D defines the full enum + the
/// chain-driven signing contract; Phase A carries the smallest
/// surface that lets the loop accept events through.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum AdminEvent {
    /// Begin a maintenance window for `node`.
    EnterMaintenance {
        /// Node entering maintenance.
        node: NodeId,
        /// Optional drain-window duration measured from the
        /// loop's anchor instant (last tick); the fold computes
        /// the absolute deadline as `anchor + drain_for`. `None`
        /// defers to the cluster's configured default deadline.
        /// Wire form is a `Duration` (serde-friendly) rather
        /// than an `Instant` so the admin event signs cleanly
        /// and round-trips through postcard / the eventual
        /// admin chain.
        drain_for: Option<Duration>,
    },
    /// End a maintenance window for `node`.
    ExitMaintenance {
        /// Node leaving maintenance.
        node: NodeId,
    },
    /// Drain `node`'s workload by the configured deadline. Like
    /// a maintenance window but does not require an explicit
    /// Exit.
    Drain {
        /// Node to drain.
        node: NodeId,
        /// Drain-window duration measured from the loop's
        /// anchor instant (last tick); the fold computes the
        /// absolute deadline as `anchor + drain_for`. Wire form
        /// is a `Duration` (serde-friendly) so the admin event
        /// signs cleanly and round-trips through postcard.
        drain_for: Duration,
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
    /// Pause reconcile-driven action emission cluster-wide for
    /// `ttl`. Folds + chain commits keep running; only the
    /// reconcile output is suppressed. The freeze auto-expires
    /// at `now + ttl`; an earlier [`AdminEvent::ThawCluster`]
    /// clears the freeze immediately.
    ///
    /// ICE break-glass surface per `DECK_SDK_PLAN.md`. The
    /// substrate enforces the freeze; the operator-side
    /// signing + multi-operator gating lives in the Deck SDK
    /// once those slices land.
    FreezeCluster {
        /// How long the freeze should hold for.
        ttl: std::time::Duration,
    },
    /// Cancel an in-effect freeze early. No-op if no freeze is
    /// in effect; idempotent.
    ThawCluster,
    /// ICE break-glass: flush avoid-list entries cluster-wide
    /// under the given [`AvoidScope`]. The existing
    /// [`AdminEvent::ClearAvoidList`] is a global flush
    /// regardless of its `node` parameter; this variant gives
    /// the operator the three scoped flushes the plan calls out:
    /// per-this-node, per-targeted-peer, and full global.
    FlushAvoidLists {
        /// Which entries to flush — see [`AvoidScope`].
        scope: AvoidScope,
    },
    /// ICE break-glass: force-evict `victim` from `chain`,
    /// bypassing the scheduler's per-chain rebalance cooldown
    /// (`SchedulerConfig::cooldown`) and the count-driven
    /// hysteresis the non-force eviction path respects. Only
    /// the chain's elected leader actually emits the resulting
    /// `RequestEviction` action; non-leader observers fold the
    /// admin event but produce no action.
    ForceEvictReplica {
        /// Chain whose replica to evict.
        chain: ChainId,
        /// Node currently holding the replica that should be
        /// removed.
        victim: NodeId,
    },
    /// ICE break-glass: reset `daemon`'s backoff tracker so the
    /// supervisor's gate (BackingOff / CrashLooping) no longer
    /// suppresses `StartDaemon` emission. Use to give a crash-
    /// looping daemon an immediate retry after operator-side
    /// recovery. No-op for a daemon already in `Idle` state.
    ForceRestartDaemon {
        /// The daemon whose backoff should be cleared.
        daemon: DaemonRef,
    },
    /// ICE break-glass: force `chain` to be placed on `target`,
    /// bypassing the placement scorer. The chain's elected
    /// leader emits the resulting
    /// `RequestPlacement { target: Some(target), .. }` action
    /// (other nodes fold the admin event but don't emit). The
    /// dispatcher honors `target` directly; the count-driven
    /// arm will rebalance if the chain ends up over-replicated.
    /// No-op if `target` is already a holder.
    ForceCutover {
        /// Chain to pin.
        chain: ChainId,
        /// Node operator wants as a holder.
        target: NodeId,
    },
    /// ICE break-glass: abort an in-flight migration. Records
    /// the operator's intent on the audit ring and surfaces it
    /// to whatever dispatcher integrates with the compute
    /// layer's `MigrationOrchestrator`. The wire-form
    /// substrate plumbing lands here; the dispatcher hookup
    /// that finds the in-flight migration and tells it to
    /// stop is future substrate work — until that lands the
    /// commit is observable via the audit ring but doesn't
    /// itself stop the migration.
    KillMigration {
        /// The migration to abort.
        migration: MigrationId,
    },
}

/// Scope discriminator for [`AdminEvent::FlushAvoidLists`].
/// Each variant produces distinct fold behavior on the
/// observing node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum AvoidScope {
    /// Clear this node's entire avoid list, but only if
    /// `node` is this node. Other nodes are no-ops. Use when
    /// recovering a single node from a flapping-RTT episode.
    Local {
        /// Target node id. Other nodes ignore the event.
        node: NodeId,
    },
    /// Every node removes `peer` from its avoid list. Use to
    /// reverse a cluster-wide "avoid peer X" pattern after
    /// operator-side recovery (e.g. routing fix, peer
    /// restart). Idempotent — no-op for nodes that don't
    /// have `peer` in their avoid list.
    OnPeer {
        /// Peer to un-avoid cluster-wide.
        peer: NodeId,
    },
    /// Every node clears its entire avoid list. Use after a
    /// network event that produced spurious avoid entries
    /// across the whole cluster. Heaviest scope — reconcile
    /// will re-emit `MarkAvoid` on the next tick for any peer
    /// that still meets the degraded-RTT threshold.
    Global,
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

impl AdminEvent {
    /// `true` iff this admin event is an ICE break-glass
    /// variant (Force* / FreezeCluster / ThawCluster /
    /// FlushAvoidLists). Used by the Deck SDK's
    /// `AuditQuery::force_only` filter to project the audit
    /// ring to just the operator escalations security review
    /// cares about most.
    pub fn is_ice(&self) -> bool {
        matches!(
            self,
            AdminEvent::FreezeCluster { .. }
                | AdminEvent::ThawCluster
                | AdminEvent::FlushAvoidLists { .. }
                | AdminEvent::ForceEvictReplica { .. }
                | AdminEvent::ForceRestartDaemon { .. }
                | AdminEvent::ForceCutover { .. }
                | AdminEvent::KillMigration { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire-stability pin: every `AdminEvent` variant must
    /// round-trip through postcard. Locked once the admin chain
    /// commits to this form; the signed-admin-commit path
    /// signs over the postcard encoding so any future variant
    /// addition has to extend this test or break operators.
    #[test]
    fn admin_event_postcard_round_trips_every_variant() {
        let cases = [
            AdminEvent::EnterMaintenance {
                node: 42,
                drain_for: Some(Duration::from_secs(300)),
            },
            AdminEvent::EnterMaintenance {
                node: 42,
                drain_for: None,
            },
            AdminEvent::ExitMaintenance { node: 42 },
            AdminEvent::Drain {
                node: 42,
                drain_for: Duration::from_secs(600),
            },
            AdminEvent::Cordon { node: 42 },
            AdminEvent::Uncordon { node: 42 },
            AdminEvent::RestartAllDaemons { node: 42 },
            AdminEvent::ClearAvoidList { node: 42 },
            AdminEvent::DropReplicas {
                node: 42,
                chains: vec![1, 2, 3],
            },
            AdminEvent::InvalidatePlacement { node: 42 },
            AdminEvent::FreezeCluster {
                ttl: Duration::from_secs(60),
            },
            AdminEvent::ThawCluster,
            AdminEvent::FlushAvoidLists {
                scope: AvoidScope::Local { node: 42 },
            },
            AdminEvent::FlushAvoidLists {
                scope: AvoidScope::OnPeer { peer: 7 },
            },
            AdminEvent::FlushAvoidLists {
                scope: AvoidScope::Global,
            },
            AdminEvent::ForceEvictReplica {
                chain: 100,
                victim: 7,
            },
            AdminEvent::ForceRestartDaemon {
                daemon: DaemonRef {
                    id: 7,
                    name: "telemetry".into(),
                },
            },
            AdminEvent::ForceCutover {
                chain: 100,
                target: 42,
            },
            AdminEvent::KillMigration { migration: 999 },
        ];
        for ev in cases {
            let bytes = postcard::to_allocvec(&ev).expect("encode");
            let decoded: AdminEvent = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, ev);
        }
    }
}
