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
    AdminEvent, AvoidScope, BlobAnnouncement, ChainId, DaemonHealth, DaemonIntent,
    DaemonIntentUpdate, DaemonLifecycleSignal, DaemonRef, LocalReplicaIntent,
    LocalReplicaIntentUpdate, MeshOsEvent, NodeHealth, NodeId, PlacementIntent, ReplicaUpdate,
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
    /// Per-peer inventory axes (CPU / mem / disk / saturation
    /// / capability set / software version / fork origin).
    /// Fed by `InventoryProbe` instances on every Tick; the
    /// snapshot fold projects this into `PeerSnapshot`'s
    /// inventory fields. `None` for any peer no probe samples.
    pub(crate) inventory: HashMap<NodeId, super::probes::PeerInventory>,
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
    /// Operator-issued force-evictions pending leader-side
    /// emission. The fold appends `(chain, victim)` on every
    /// `AdminEvent::ForceEvictReplica`; reconcile reads the
    /// list to emit `RequestEviction` actions bypassing
    /// scheduler cooldown; the loop drains the list after each
    /// reconcile pass so a single admin commit fires exactly
    /// one eviction.
    pub(crate) forced_evictions: Vec<(ChainId, NodeId)>,
    /// Operator-issued force-cutovers pending leader-side
    /// emission. Same pattern as `forced_evictions`: fold
    /// appends `(chain, target)` on every
    /// `AdminEvent::ForceCutover`; the leader's reconcile arm
    /// emits a `RequestPlacement { target: Some(target), .. }`
    /// bypassing the placement scorer; the loop drains the
    /// list after each pass.
    pub(crate) forced_placements: Vec<(ChainId, NodeId)>,
    // admin_audit and log_ring rings live on MeshOsLoop now,
    // not MeshOsState — they're append-only output buffers
    // that don't participate in fold convergence, and keeping
    // them off the state struct removes the need for the
    // dead `_ => {}` arms on SignedIceCommit / LogLine in the
    // apply path.
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
            MeshOsEvent::ReplicaBecameHolderAndLeader { chain, holder } => {
                // Atomic pair (symmetric to LeaderLostAndRemoved):
                // add the holder AND set the leader in one fold
                // call so a backpressured event channel can't
                // surface a phantom holder or phantom leader.
                self.replicas.entry(*chain).or_default().insert(*holder);
                self.replica_leader.insert(*chain, *holder);
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
            MeshOsEvent::SignedIceCommit { .. } | MeshOsEvent::SignedAdminCommit { .. } => {
                // The loop unwraps verified signed-commit events
                // into their inner `AdminEvent` and calls back
                // into this fold with the unwrapped form, so
                // these arms are dead in production. Kept
                // explicit so future MeshOsEvent additions
                // can't silently skip the fold.
            }
            MeshOsEvent::LogLine(_) => {
                // The loop's record_log_line path stamps the
                // record's seq + ts + node id and pushes onto
                // the log ring directly — this arm is dead in
                // production but kept explicit so future
                // MeshOsEvent additions don't silently skip
                // the fold.
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
            AdminEvent::EnterMaintenance { node, drain_for } => {
                self.maintenance
                    .insert(*node, MaintenanceMirror::EnteringMaintenance);
                if *node == this_node {
                    // The state-side deadline is an Instant
                    // computed from the loop's anchor + the
                    // wire-form Duration. Two replays produce
                    // identical Instants because `anchor` is
                    // pinned to `last_tick`.
                    self.local_maintenance = MaintenanceState::EnteringMaintenance {
                        since: anchor,
                        deadline: drain_for.map(|d| anchor + d),
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
            AdminEvent::FlushAvoidLists { scope } => {
                match scope {
                    AvoidScope::Local { node } => {
                        if *node == this_node {
                            self.avoid_list.clear();
                        }
                    }
                    AvoidScope::OnPeer { peer } => {
                        // Every node un-avoids the target peer.
                        self.avoid_list.remove(peer);
                    }
                    AvoidScope::Global => {
                        // Every node flushes its entire avoid
                        // list. Reconcile will re-emit
                        // `MarkAvoid` next tick for peers that
                        // still meet the degraded-RTT threshold.
                        self.avoid_list.clear();
                    }
                }
            }
            AdminEvent::ForceEvictReplica { chain, victim } => {
                // Cluster-wide signal: every node records the
                // pending eviction. The chain's elected leader
                // is the one that actually emits `RequestEviction`
                // on the next reconcile pass; other nodes drain
                // the entry without emitting, which is the same
                // shape the count-driven and scheduler arms use.
                self.forced_evictions.push((*chain, *victim));
            }
            AdminEvent::ForceRestartDaemon { daemon } => {
                // Clear the daemon's backoff gate so reconcile's
                // `StartDaemon` arm fires on the next tick. Also
                // drop any prior `ApplyBackoff` record — a future
                // crash should produce a fresh emission rather
                // than ride the previous cooldown's deadline.
                if let Some(status) = self.daemons.get_mut(daemon) {
                    status.backoff.force_release();
                }
                self.applied_backoffs.remove(daemon);
            }
            AdminEvent::ForceCutover { chain, target } => {
                // Cluster-wide signal: every node folds; only
                // the chain's elected leader emits the resulting
                // `RequestPlacement` action with target pinned.
                self.forced_placements.push((*chain, *target));
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
                drain_for: None,
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
                drain_for: None,
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
                drain_for: None,
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
                drain_for: None,
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
        // Active → EnteringMaintenance → Maintenance: the
        // is_valid_successor match-table forbids skipping
        // EnteringMaintenance, so drive both arcs in order.
        state.apply(
            &MeshOsEvent::MaintenanceTransitionObserved {
                node: THIS_NODE,
                state: MaintenanceState::EnteringMaintenance {
                    since: base,
                    deadline: None,
                },
            },
            THIS_NODE,
        );
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
        state.apply(&MeshOsEvent::AdminEvent(AdminEvent::ThawCluster), THIS_NODE);
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
        state.apply(&MeshOsEvent::AdminEvent(AdminEvent::ThawCluster), THIS_NODE);
        assert!(state.freeze_until.is_none());
    }

    fn seed_avoid_list(state: &mut MeshOsState, peers: &[NodeId]) {
        for peer in peers {
            state.avoid_list.insert(
                *peer,
                AvoidEntry {
                    reason: "test".into(),
                    until: Instant::now() + Duration::from_secs(60),
                },
            );
        }
    }

    #[test]
    fn flush_avoid_lists_local_clears_only_on_target_node() {
        const THIS_NODE: NodeId = 42;
        const OTHER_NODE: NodeId = 99;
        let mut state = MeshOsState::default();
        seed_avoid_list(&mut state, &[1, 2, 3]);

        // Fold on a node that ISN'T the target — should be no-op.
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FlushAvoidLists {
                scope: AvoidScope::Local { node: OTHER_NODE },
            }),
            THIS_NODE,
        );
        assert_eq!(
            state.avoid_list.len(),
            3,
            "Local{{other}} should not flush this node"
        );

        // Fold on the actual target — clears the list.
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FlushAvoidLists {
                scope: AvoidScope::Local { node: THIS_NODE },
            }),
            THIS_NODE,
        );
        assert!(state.avoid_list.is_empty());
    }

    #[test]
    fn flush_avoid_lists_on_peer_removes_only_that_peer_from_every_node() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        seed_avoid_list(&mut state, &[1, 2, 3]);
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FlushAvoidLists {
                scope: AvoidScope::OnPeer { peer: 2 },
            }),
            THIS_NODE,
        );
        // Only peer 2 should be removed.
        assert!(state.avoid_list.contains_key(&1));
        assert!(!state.avoid_list.contains_key(&2));
        assert!(state.avoid_list.contains_key(&3));
    }

    #[test]
    fn flush_avoid_lists_on_peer_is_idempotent_for_absent_peer() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        seed_avoid_list(&mut state, &[1, 2]);
        // Flush a peer that isn't on the list — no-op, no panic.
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FlushAvoidLists {
                scope: AvoidScope::OnPeer { peer: 99 },
            }),
            THIS_NODE,
        );
        assert_eq!(state.avoid_list.len(), 2);
    }

    #[test]
    fn flush_avoid_lists_global_clears_every_entry_on_every_node() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        seed_avoid_list(&mut state, &[1, 2, 3, 4, 5]);
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::FlushAvoidLists {
                scope: AvoidScope::Global,
            }),
            THIS_NODE,
        );
        assert!(state.avoid_list.is_empty());
    }

    #[test]
    fn force_evict_replica_admin_event_appends_to_forced_evictions() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ForceEvictReplica {
                chain: 100,
                victim: 7,
            }),
            THIS_NODE,
        );
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ForceEvictReplica {
                chain: 200,
                victim: 9,
            }),
            THIS_NODE,
        );
        assert_eq!(state.forced_evictions, vec![(100u64, 7u64), (200u64, 9u64)]);
    }

    #[test]
    fn force_restart_daemon_clears_backoff_gate_and_applied_record() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        let d = DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        let mut status = DaemonStatus::default();
        // Drive the tracker into BackingOff so the gate is set.
        let crash_at = Instant::now();
        status.backoff.observe_crash(crash_at);
        assert!(!matches!(
            status.backoff.state(),
            crate::adapter::net::behavior::meshos::supervision::RestartState::Idle
        ));
        state.daemons.insert(d.clone(), status);
        state.applied_backoffs.insert(d.clone(), crash_at);

        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ForceRestartDaemon { daemon: d.clone() }),
            THIS_NODE,
        );

        // Gate should be cleared.
        let after = state.daemons.get(&d).unwrap();
        assert!(matches!(
            after.backoff.state(),
            crate::adapter::net::behavior::meshos::supervision::RestartState::Idle
        ));
        // Applied-backoffs record should be cleared too.
        assert!(!state.applied_backoffs.contains_key(&d));
    }

    #[test]
    fn force_cutover_admin_event_appends_to_forced_placements() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ForceCutover {
                chain: 100,
                target: 7,
            }),
            THIS_NODE,
        );
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ForceCutover {
                chain: 200,
                target: 9,
            }),
            THIS_NODE,
        );
        assert_eq!(
            state.forced_placements,
            vec![(100u64, 7u64), (200u64, 9u64)]
        );
    }

    #[test]
    fn force_restart_daemon_is_noop_for_unknown_daemon() {
        const THIS_NODE: NodeId = 42;
        let mut state = MeshOsState::default();
        let d = DaemonRef {
            id: 999,
            name: "absent".into(),
        };
        state.apply(
            &MeshOsEvent::AdminEvent(AdminEvent::ForceRestartDaemon { daemon: d }),
            THIS_NODE,
        );
        // No panic, no state added.
        assert!(state.daemons.is_empty());
    }
}
