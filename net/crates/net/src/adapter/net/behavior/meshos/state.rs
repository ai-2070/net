//! [`MeshOsState`] — folded **actual** state observed from
//! substrate events. [`DesiredState`] — folded **desired** state
//! pulled from Dataforts placement intent. Reconcile reads both
//! and emits the action diff.
//!
//! Phase A ships the shape with empty bodies; the loop folds
//! arriving events into [`MeshOsState`] via [`MeshOsState::apply`]
//! so the reconcile pass has somewhere to read from even though
//! it currently returns no actions. Later phases attach real
//! data (per-daemon backoff trackers, replica sets, avoid lists,
//! maintenance state, etc.).

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use super::event::{
    AdminEvent, BlobAnnouncement, ChainId, DaemonHealth, DaemonIntent, DaemonIntentUpdate,
    DaemonLifecycleSignal, DaemonRef, LocalReplicaIntent, LocalReplicaIntentUpdate, MeshOsEvent,
    NodeHealth, NodeId, PlacementIntent, ReplicaUpdate,
};
use super::maintenance::MaintenanceState;
use super::supervision::BackoffTracker;

/// Folded view of what's happening on the cluster *right now*,
/// from this node's vantage. Updated by `apply` as events
/// arrive; read by the reconcile pass.
#[derive(Clone, Debug, Default)]
pub struct MeshOsState {
    /// Per-daemon observed status (Phase B fills the body).
    pub(crate) daemons: HashMap<DaemonRef, DaemonStatus>,
    /// Replicas this node observes — keyed by chain id, valued
    /// by the set of holders the substrate knows about.
    /// `BTreeSet` keeps holder iteration deterministically
    /// sorted, which the Phase C lex-smallest victim selection
    /// reads as `.iter().next()`, and the per-chain holder set
    /// stays unique without explicit `contains` guards on add.
    pub(crate) replicas: HashMap<ChainId, BTreeSet<NodeId>>,
    /// Current leader for each chain (per
    /// `replication_election`). Reconcile reads this to decide
    /// whether `Request*` actions are admissible on this node.
    pub(crate) replica_leader: HashMap<ChainId, NodeId>,
    /// Per-peer RTT samples (latest only; Phase D adds the rolling window).
    pub(crate) rtt: HashMap<NodeId, Duration>,
    /// Per-peer health (Phase D fills the body).
    pub(crate) node_health: HashMap<NodeId, NodeHealth>,
    /// Maintenance state for each peer (Phase E owns the
    /// transitions; here we only mirror what the admin chain
    /// committed).
    pub(crate) maintenance: HashMap<NodeId, MaintenanceMirror>,
    /// Phase E — this node's own maintenance state machine.
    /// Driven by admin events (operator commands) and observed
    /// transition confirmations from the chain. Reconcile reads
    /// it to decide whether to emit forward-transition actions.
    pub(crate) local_maintenance: MaintenanceState,
    /// Blobs this node knows about, keyed by blob id.
    pub(crate) blobs: HashMap<u64, BlobObservation>,
    /// Peers currently on the local avoid list, with their TTL.
    pub(crate) avoid_list: HashMap<NodeId, AvoidEntry>,
    /// Phase D-1 — last time the scheduler emitted a
    /// rebalance for this chain. Subsequent evaluations within
    /// `SchedulerConfig::cooldown` skip the chain to avoid
    /// flap.
    pub(crate) last_rebalance: HashMap<ChainId, Instant>,
    /// Phase B — the most recent `until` an `ApplyBackoff`
    /// action emitted for the daemon. The reconcile pass
    /// suppresses re-emission when the supervisor's
    /// `release_at()` hasn't moved past the value the loop
    /// last committed, so a daemon parked in `BackingOff`
    /// doesn't generate a fresh action every tick.
    pub(crate) applied_backoffs: HashMap<DaemonRef, Instant>,
    /// Last `Tick` we processed — used by tests / diagnostics.
    pub(crate) last_tick: Option<Instant>,
    /// Cluster-wide reconcile-emission freeze. Some(instant)
    /// means "freeze in effect until this instant"; reconcile
    /// returns an empty action vector while `now < until`. The
    /// tick GC clears the value once it expires. Driven by the
    /// ICE [`AdminEvent::FreezeCluster`] / [`AdminEvent::ThawCluster`]
    /// admin events.
    pub(crate) freeze_until: Option<Instant>,
}

/// Per-daemon observed status. Phase B fleshes out the fields
/// reconcile reads to decide start / stop / backoff actions.
#[derive(Clone, Debug, Default)]
pub struct DaemonStatus {
    /// Latest self-reported health, if any.
    pub health: Option<DaemonHealth>,
    /// Latest self-reported saturation in `[0.0, 1.0]`.
    pub saturation: f32,
    /// Monotonic timestamp of the most recent `Started` signal.
    pub last_started: Option<Instant>,
    /// Monotonic timestamp of the most recent `ExitedCleanly` signal.
    pub last_exit: Option<Instant>,
    /// Monotonic timestamp of the most recent `Crashed` signal.
    pub last_crash: Option<Instant>,
    /// Lifecycle phase the supervisor believes the daemon is in.
    /// Default `Stopped`. Updated by the `apply_daemon` fold.
    pub lifecycle: DaemonLifecycle,
    /// Per-daemon backoff tracker. Reconcile reads
    /// `backoff.state()` to gate `StartDaemon` emission.
    pub backoff: BackoffTracker,
}

/// Lifecycle phase the supervisor tracks per daemon. Used by
/// reconcile to decide start / stop emissions.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum DaemonLifecycle {
    /// Daemon is not currently running on this node.
    #[default]
    Stopped,
    /// `StartDaemon` action emitted; waiting for the supervisor
    /// to confirm via `DaemonLifecycleSignal::Started`.
    Starting,
    /// Daemon is running. Default reconcile target when desired
    /// intent is `Run`.
    Running,
    /// `StopDaemon` action emitted; waiting for clean exit or
    /// forced termination.
    Stopping,
}

/// Mirrored maintenance state, copied from chain metadata. The
/// authoritative source is the admin chain commit; this is the
/// per-node fold for cheap reads inside reconcile.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaintenanceMirror {
    /// Normal participation.
    Active,
    /// Entering maintenance — replicas migrating, daemons draining.
    EnteringMaintenance,
    /// Steady-state isolated — replicas migrated + daemons stopped.
    Maintenance,
    /// Exiting maintenance — health revalidation + capability refresh.
    ExitingMaintenance,
    /// Drain failed — operator warning state.
    DrainFailed,
    /// Recovery — ramp-up window after exit.
    Recovery,
}

/// Observed blob — size + holder set + whether it's been seen
/// alive in this loop's window.
#[derive(Clone, Debug, Default)]
pub struct BlobObservation {
    /// Blob size in bytes (latest announcement).
    pub size_bytes: u64,
    /// Peers that have announced this blob.
    pub holders: Vec<NodeId>,
}

/// Avoid-list entry. TTL is observed by reconcile; expiry is
/// cleaned up by the next tick after `until <= now`.
#[derive(Clone, Debug)]
pub struct AvoidEntry {
    /// Operator-readable reason (audit + Deck render).
    pub reason: String,
    /// Instant past which the entry is GC'd.
    pub until: Instant,
}

impl MeshOsState {
    /// Fold an event into the actual-state view. Called by the
    /// event loop after popping each event off the receiver.
    /// `this_node` is the loop's identity — needed by the
    /// Phase E fold to decide whether an `AdminEvent` /
    /// `MaintenanceTransitionObserved` mutates this node's own
    /// `local_maintenance` or just the per-peer mirror.
    pub fn apply(&mut self, event: &MeshOsEvent, this_node: NodeId) {
        match event {
            MeshOsEvent::Tick => {
                let now = Instant::now();
                self.last_tick = Some(now);
                self.gc_avoid_list(now);
                self.release_elapsed_backoffs(now);
                self.gc_freeze(now);
            }
            MeshOsEvent::ReplicaUpdate(update) => self.apply_replica(update),
            MeshOsEvent::DaemonLifecycle { daemon, signal } => {
                self.apply_daemon(daemon, signal);
            }
            MeshOsEvent::DaemonIntentUpdate(_) => {
                // Desired-state input; routed by the loop into
                // `DesiredState`, no actual-state side effect.
            }
            MeshOsEvent::LocalReplicaIntent(_) => {
                // Desired-state input; routed by the loop.
            }
            MeshOsEvent::ReplicaLeaderUpdate { chain, leader } => {
                if let Some(leader) = leader {
                    self.replica_leader.insert(*chain, *leader);
                } else {
                    self.replica_leader.remove(chain);
                }
            }
            MeshOsEvent::ReplicaLeaderLostAndRemoved { chain, holder } => {
                // Atomic pair: clear the leader AND remove the
                // holder. Two separate events could fragment
                // under backpressure; bundling them in one fold
                // call keeps the snapshot coherent.
                self.replica_leader.remove(chain);
                if let Some(entry) = self.replicas.get_mut(chain) {
                    entry.remove(holder);
                }
            }
            MeshOsEvent::RttSample { peer, rtt } => {
                self.rtt.insert(*peer, *rtt);
            }
            MeshOsEvent::NodeHealth { peer, health } => {
                self.node_health.insert(*peer, *health);
            }
            MeshOsEvent::AdminEvent(admin) => self.apply_admin(admin, this_node),
            MeshOsEvent::BlobAnnouncement(blob) => self.apply_blob(blob),
            MeshOsEvent::PlacementIntent(_) => {
                // Placement intent is desired-state input; the
                // loop routes it into `DesiredState`, not here.
            }
            MeshOsEvent::Shutdown => {
                // Shutdown is loop-control; no fold side effect.
            }
            MeshOsEvent::MaintenanceTransitionObserved { node, state } => {
                self.apply_maintenance_transition(*node, state.clone(), this_node);
            }
        }
    }

    fn apply_maintenance_transition(
        &mut self,
        node: NodeId,
        state: MaintenanceState,
        this_node: NodeId,
    ) {
        // Mirror the transition into the per-peer map (Deck +
        // reconcile reads). Convert the rich state to the
        // simple mirror enum.
        let mirror = match &state {
            MaintenanceState::Active => MaintenanceMirror::Active,
            MaintenanceState::EnteringMaintenance { .. } => MaintenanceMirror::EnteringMaintenance,
            MaintenanceState::Maintenance { .. } => MaintenanceMirror::Maintenance,
            MaintenanceState::ExitingMaintenance { .. } => MaintenanceMirror::ExitingMaintenance,
            MaintenanceState::DrainFailed { .. } => MaintenanceMirror::DrainFailed,
            MaintenanceState::Recovery { .. } => MaintenanceMirror::Recovery,
        };
        self.maintenance.insert(node, mirror);
        if node == this_node {
            // Forward-only transitions. A late-arriving observed
            // event for an older state (e.g. a replay anomaly
            // pushing Recovery back to Maintenance) is dropped.
            if !self.local_maintenance.is_valid_successor(&state) {
                tracing::warn!(
                    target: "meshos",
                    current = ?self.local_maintenance,
                    rejected = ?state,
                    "MaintenanceTransitionObserved rejected — not a forward arc",
                );
                return;
            }
            self.local_maintenance = state;
        }
    }

    fn apply_replica(&mut self, update: &ReplicaUpdate) {
        match update {
            ReplicaUpdate::Added { chain, holder } | ReplicaUpdate::Repaired { chain, holder } => {
                self.replicas.entry(*chain).or_default().insert(*holder);
            }
            ReplicaUpdate::Removed { chain, holder } | ReplicaUpdate::Lost { chain, holder } => {
                if let Some(entry) = self.replicas.get_mut(chain) {
                    entry.remove(holder);
                }
            }
        }
    }

    fn apply_daemon(&mut self, daemon: &DaemonRef, signal: &DaemonLifecycleSignal) {
        let status = self.daemons.entry(daemon.clone()).or_default();
        match signal {
            DaemonLifecycleSignal::Started { at } => {
                status.last_started = Some(*at);
                status.lifecycle = DaemonLifecycle::Running;
                status.backoff.observe_start(*at);
            }
            DaemonLifecycleSignal::ExitedCleanly { at } => {
                status.last_exit = Some(*at);
                status.lifecycle = DaemonLifecycle::Stopped;
                status.backoff.observe_clean_exit(*at);
            }
            DaemonLifecycleSignal::Crashed { at, .. } => {
                status.last_crash = Some(*at);
                status.lifecycle = DaemonLifecycle::Stopped;
                status.backoff.observe_crash(*at);
            }
            DaemonLifecycleSignal::HealthChanged { health, .. } => {
                status.health = Some(health.clone());
            }
            DaemonLifecycleSignal::SaturationChanged { saturation, .. } => {
                status.saturation = *saturation;
            }
        }
    }

    fn apply_admin(&mut self, admin: &AdminEvent, this_node: NodeId) {
        // Phase A mirrored the maintenance-state arm; Phase D
        // adds the avoid-list / cordon / drain handlers that
        // don't need to ride DesiredState. The desired-state-
        // side admin consequences (`DropReplicas`,
        // `RestartAllDaemons`) are projected into DesiredState
        // by the loop's `apply` path, not here.
        //
        // Phase E: the EnterMaintenance / ExitMaintenance
        // commands also flip `local_maintenance` when they
        // target this node — the operator-driven transition
        // entry points into the state machine.
        // Anchor admin-driven transitions on the most recent
        // tick rather than wall-now so two replays of the same
        // event sequence converge on the same `since` instants.
        // Falls back to `Instant::now()` only on bootstrap
        // before the first tick has fired.
        let anchor = self.last_tick.unwrap_or_else(Instant::now);
        match admin {
            AdminEvent::EnterMaintenance { node, deadline } => {
                self.maintenance
                    .insert(*node, MaintenanceMirror::EnteringMaintenance);
                if *node == this_node {
                    self.local_maintenance = MaintenanceState::EnteringMaintenance {
                        since: anchor,
                        deadline: *deadline,
                    };
                }
            }
            AdminEvent::ExitMaintenance { node } => {
                self.maintenance
                    .insert(*node, MaintenanceMirror::ExitingMaintenance);
                if *node == this_node
                    && matches!(
                        self.local_maintenance,
                        MaintenanceState::Maintenance { .. } | MaintenanceState::DrainFailed { .. }
                    )
                {
                    self.local_maintenance = MaintenanceState::ExitingMaintenance { since: anchor };
                }
            }
            AdminEvent::ClearAvoidList { node: _ } => {
                // The clear is unconditional on this node's
                // fold — the admin chain commit applies to the
                // target node, and every other node simply
                // observes the chain entry. The desired-state-
                // side reset is handled by reconcile's
                // idempotent re-emission of `MarkAvoid` if the
                // RTT is still bad.
                self.avoid_list.clear();
            }
            AdminEvent::FreezeCluster { ttl } => {
                // Cluster-wide signal — every node observes the
                // same admin event and folds it identically.
                self.freeze_until = Some(anchor + *ttl);
            }
            AdminEvent::ThawCluster => {
                self.freeze_until = None;
            }
            _ => {}
        }
    }

    fn apply_blob(&mut self, blob: &BlobAnnouncement) {
        if blob.added {
            let entry = self.blobs.entry(blob.blob).or_default();
            entry.size_bytes = blob.size_bytes;
            if !entry.holders.contains(&blob.holder) {
                entry.holders.push(blob.holder);
            }
        } else if let Some(entry) = self.blobs.get_mut(&blob.blob) {
            entry.holders.retain(|h| h != &blob.holder);
        }
    }

    fn gc_avoid_list(&mut self, now: Instant) {
        self.avoid_list.retain(|_, entry| entry.until > now);
    }

    fn release_elapsed_backoffs(&mut self, now: Instant) {
        for status in self.daemons.values_mut() {
            status.backoff.maybe_release(now);
        }
    }

    /// Clear an expired cluster-wide freeze. Idempotent — does
    /// nothing if no freeze is in effect or the freeze still has
    /// time remaining.
    fn gc_freeze(&mut self, now: Instant) {
        if let Some(until) = self.freeze_until {
            if now >= until {
                self.freeze_until = None;
            }
        }
    }

    /// `true` iff a cluster-wide reconcile freeze is in effect.
    /// Reconcile reads this each tick to decide whether to emit
    /// actions.
    pub(crate) fn is_frozen(&self, now: Instant) -> bool {
        self.freeze_until.map(|until| now < until).unwrap_or(false)
    }
}

/// Folded desired-state view — what Dataforts says the cluster
/// *should* look like. Reconcile reads both sides and computes
/// the diff.
#[derive(Clone, Debug, Default)]
pub struct DesiredState {
    /// Desired replica count per chain (cluster-wide). Reconcile
    /// reads this on the leader node to emit `RequestPlacement`
    /// (count short) / `RequestEviction` (count over) actions.
    pub(crate) desired_replicas: HashMap<ChainId, u32>,
    /// Per-chain "should this node hold a replica?" intent.
    /// Source: the leader's `Request*` actions, projected via
    /// the Dataforts fold. Reconcile reads this to emit
    /// `PullReplica` / `DropReplica` actions.
    pub(crate) desired_local_replicas: HashMap<ChainId, LocalReplicaIntent>,
    /// Per-daemon intent. Reconcile reads this against the
    /// actual `MeshOsState::daemons[*].lifecycle` to emit
    /// `StartDaemon` / `StopDaemon`.
    pub(crate) desired_daemons: HashMap<DaemonRef, DaemonIntent>,
}

impl DesiredState {
    /// Fold a placement-intent input event (cluster-wide count).
    pub fn apply(&mut self, intent: &PlacementIntent) {
        self.desired_replicas
            .insert(intent.chain, intent.desired_replicas);
    }

    /// Fold a daemon-intent input event.
    pub fn apply_daemon_intent(&mut self, update: &DaemonIntentUpdate) {
        self.desired_daemons
            .insert(update.daemon.clone(), update.intent);
    }

    /// Fold a per-node replica intent input event.
    pub fn apply_local_replica_intent(&mut self, update: &LocalReplicaIntentUpdate) {
        self.desired_local_replicas
            .insert(update.chain, update.intent);
    }

    /// Project an admin chain commit into desired-state changes.
    /// Phase D handles `DropReplicas` (forces `LocalReplicaIntent::Drop`
    /// on the named chains for the named node). Other admin
    /// variants either ride [`MeshOsState`] (maintenance,
    /// avoid-list clear) or park for Phase E (cordon, drain).
    pub fn apply_admin(&mut self, admin: &AdminEvent, this_node: NodeId) {
        if let AdminEvent::DropReplicas { node, chains } = admin {
            if *node != this_node {
                return;
            }
            for chain in chains {
                self.desired_local_replicas
                    .insert(*chain, LocalReplicaIntent::Drop);
            }
        }
    }
}

/// Re-exported here so the test module + reconcile can refer to
/// `state::RestartState` without crossing the supervision module.
pub use super::supervision::RestartState as DaemonRestartState;

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn add(chain: ChainId, holder: NodeId) -> MeshOsEvent {
        MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added { chain, holder })
    }
    fn rm(chain: ChainId, holder: NodeId) -> MeshOsEvent {
        MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Removed { chain, holder })
    }

    #[test]
    fn replica_fold_preserves_event_order_add_add_remove() {
        // Property: Add(11), Add(12), Remove(11) -> [12].
        // Re-orderings produce different end states, so a pass
        // here is direct evidence the apply path honors arrival
        // order.
        let chain: ChainId = 0xC0FFEE;
        let mut state = MeshOsState::default();
        state.apply(&add(chain, 11), 0);
        state.apply(&add(chain, 12), 0);
        state.apply(&rm(chain, 11), 0);
        assert_eq!(
            state.replicas.get(&chain),
            Some(&::std::collections::BTreeSet::from([12]))
        );
    }

    #[test]
    fn replica_fold_is_idempotent_under_duplicate_add() {
        let chain: ChainId = 1;
        let mut state = MeshOsState::default();
        state.apply(&add(chain, 7), 0);
        state.apply(&add(chain, 7), 0);
        assert_eq!(
            state.replicas.get(&chain),
            Some(&::std::collections::BTreeSet::from([7]))
        );
    }

    #[test]
    fn avoid_list_gc_drops_expired_entries_on_tick() {
        let mut state = MeshOsState::default();
        state.avoid_list.insert(
            1,
            AvoidEntry {
                reason: "expired".into(),
                until: Instant::now() - Duration::from_secs(1),
            },
        );
        state.avoid_list.insert(
            2,
            AvoidEntry {
                reason: "fresh".into(),
                until: Instant::now() + Duration::from_secs(60),
            },
        );
        state.apply(&MeshOsEvent::Tick, 0);
        assert!(!state.avoid_list.contains_key(&1), "expired entry not gc'd");
        assert!(state.avoid_list.contains_key(&2), "fresh entry dropped");
    }

    #[test]
    fn placement_intent_routes_into_desired_not_actual() {
        // The loop routes PlacementIntent into DesiredState, not
        // MeshOsState. Pin that contract here so a future
        // refactor can't silently change it.
        let mut actual = MeshOsState::default();
        let intent = PlacementIntent {
            chain: 42,
            desired_replicas: 3,
        };
        actual.apply(&MeshOsEvent::PlacementIntent(intent.clone()), 0);
        // PlacementIntent has no effect on actual state.
        assert!(actual.replicas.is_empty());
        let mut desired = DesiredState::default();
        desired.apply(&intent);
        assert_eq!(desired.desired_replicas.get(&42), Some(&3));
    }

    #[test]
    fn admin_event_clear_avoid_list_drops_all_entries() {
        let mut state = MeshOsState::default();
        state.avoid_list.insert(
            1,
            AvoidEntry {
                reason: "rtt".into(),
                until: Instant::now() + Duration::from_secs(60),
            },
        );
        state.avoid_list.insert(
            2,
            AvoidEntry {
                reason: "manual".into(),
                until: Instant::now() + Duration::from_secs(60),
            },
        );
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ClearAvoidList { node: 7 }),
            0,
        );
        assert!(state.avoid_list.is_empty());
    }

    #[test]
    fn admin_event_enter_then_exit_mirrors_into_maintenance_map() {
        let node: NodeId = 99;
        let mut state = MeshOsState::default();
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node,
                deadline: None,
            }),
            0,
        );
        assert_eq!(
            state.maintenance.get(&node).copied(),
            Some(MaintenanceMirror::EnteringMaintenance),
        );
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ExitMaintenance { node }),
            0,
        );
        assert_eq!(
            state.maintenance.get(&node).copied(),
            Some(MaintenanceMirror::ExitingMaintenance),
        );
    }

    #[test]
    fn enter_maintenance_targeting_this_node_flips_local_state() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        assert!(matches!(state.local_maintenance, MaintenanceState::Active));
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: THIS_NODE,
                deadline: None,
            }),
            THIS_NODE,
        );
        assert!(matches!(
            state.local_maintenance,
            MaintenanceState::EnteringMaintenance { .. }
        ));
    }

    #[test]
    fn enter_maintenance_since_is_anchored_on_last_tick_for_replay_determinism() {
        // Regression for I3: admin transitions used to sample
        // `Instant::now()` inside the fold, so two replays of
        // the same event sequence converged on different
        // `since` values. Now they read the tick anchor.
        const THIS_NODE: NodeId = 42;
        let anchor = Instant::now();
        let mut state = MeshOsState {
            last_tick: Some(anchor),
            ..MeshOsState::default()
        };
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: THIS_NODE,
                deadline: None,
            }),
            THIS_NODE,
        );
        match state.local_maintenance {
            MaintenanceState::EnteringMaintenance { since, .. } => {
                assert_eq!(
                    since, anchor,
                    "since must equal the tick anchor, not a fresh Instant::now",
                );
            }
            other => panic!("expected EnteringMaintenance, got {other:?}"),
        }
    }

    #[test]
    fn enter_maintenance_targeting_other_node_only_updates_mirror() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
                node: 99,
                deadline: None,
            }),
            THIS_NODE,
        );
        assert!(matches!(state.local_maintenance, MaintenanceState::Active));
        assert_eq!(
            state.maintenance.get(&99).copied(),
            Some(MaintenanceMirror::EnteringMaintenance),
        );
    }

    #[test]
    fn maintenance_transition_observed_for_this_node_advances_local_state() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        let base = Instant::now();
        state.apply(
            &MeshOsEvent::MaintenanceTransitionObserved {
                node: THIS_NODE,
                state: MaintenanceState::Maintenance { since: base },
            },
            THIS_NODE,
        );
        match state.local_maintenance {
            MaintenanceState::Maintenance { since } => assert_eq!(since, base),
            other => panic!("expected Maintenance, got {other:?}"),
        }
        // Mirror also updated.
        assert_eq!(
            state.maintenance.get(&THIS_NODE).copied(),
            Some(MaintenanceMirror::Maintenance),
        );
    }

    #[test]
    fn exit_maintenance_only_transitions_from_maintenance_or_drain_failed() {
        const THIS_NODE: NodeId = 42;
        // From Active — exit is silent (no-op for local state).
        let mut state = MeshOsState::default();
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ExitMaintenance { node: THIS_NODE }),
            THIS_NODE,
        );
        assert!(matches!(state.local_maintenance, MaintenanceState::Active));
        // From Maintenance — flips to ExitingMaintenance.
        state.local_maintenance = MaintenanceState::Maintenance {
            since: Instant::now(),
        };
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ExitMaintenance { node: THIS_NODE }),
            THIS_NODE,
        );
        assert!(matches!(
            state.local_maintenance,
            MaintenanceState::ExitingMaintenance { .. }
        ));
    }

    #[test]
    fn maintenance_transition_observed_rejects_backward_arc() {
        // A late-arriving observed event for an older state must
        // not push the local machine backward. Land in Recovery
        // first, then publish an observed Maintenance — the
        // local state stays in Recovery.
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        let base = Instant::now();
        state.local_maintenance = MaintenanceState::Recovery { since: base };
        // Backward arc — should be rejected.
        state.apply(
            &MeshOsEvent::MaintenanceTransitionObserved {
                node: THIS_NODE,
                state: MaintenanceState::Maintenance { since: base },
            },
            THIS_NODE,
        );
        match &state.local_maintenance {
            MaintenanceState::Recovery { since } => assert_eq!(*since, base),
            other => panic!("expected Recovery to be preserved, got {other:?}"),
        }
        // Forward arc — should be accepted.
        state.apply(
            &MeshOsEvent::MaintenanceTransitionObserved {
                node: THIS_NODE,
                state: MaintenanceState::Active,
            },
            THIS_NODE,
        );
        assert!(matches!(state.local_maintenance, MaintenanceState::Active));
    }

    #[test]
    fn freeze_cluster_admin_event_sets_freeze_until_to_anchor_plus_ttl() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        let anchor = Instant::now();
        state.last_tick = Some(anchor);
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FreezeCluster {
                ttl: Duration::from_secs(30),
            }),
            THIS_NODE,
        );
        let until = state.freeze_until.expect("freeze should be set");
        let delta = until.saturating_duration_since(anchor);
        assert!(
            (Duration::from_secs(29)..=Duration::from_secs(31)).contains(&delta),
            "freeze TTL should anchor on last_tick, got {delta:?}",
        );
    }

    #[test]
    fn thaw_cluster_admin_event_clears_freeze() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        state.last_tick = Some(Instant::now());
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FreezeCluster {
                ttl: Duration::from_secs(30),
            }),
            THIS_NODE,
        );
        assert!(state.freeze_until.is_some());
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ThawCluster),
            THIS_NODE,
        );
        assert!(state.freeze_until.is_none());
    }

    #[test]
    fn freeze_gc_clears_expired_freeze_on_tick() {
        let mut state = MeshOsState::default();
        let past = Instant::now() - Duration::from_secs(1);
        state.freeze_until = Some(past);
        // Tick after the freeze expired.
        state.apply(&MeshOsEvent::Tick, 0);
        assert!(state.freeze_until.is_none());
    }

    #[test]
    fn freeze_gc_preserves_freeze_that_has_time_remaining() {
        let mut state = MeshOsState::default();
        let future = Instant::now() + Duration::from_secs(30);
        state.freeze_until = Some(future);
        state.apply(&MeshOsEvent::Tick, 0);
        assert!(state.freeze_until.is_some());
    }

    #[test]
    fn is_frozen_returns_true_only_while_freeze_in_effect() {
        let mut state = MeshOsState::default();
        let now = Instant::now();
        assert!(!state.is_frozen(now));
        state.freeze_until = Some(now + Duration::from_secs(30));
        assert!(state.is_frozen(now));
        // After expiration the helper reports false even before
        // the next tick's GC runs.
        assert!(!state.is_frozen(now + Duration::from_secs(31)));
    }

    #[test]
    fn thaw_is_idempotent_when_no_freeze_in_effect() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ThawCluster),
            THIS_NODE,
        );
        assert!(state.freeze_until.is_none());
    }
}
