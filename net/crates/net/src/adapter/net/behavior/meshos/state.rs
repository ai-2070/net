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

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::event::{
    AdminEvent, BlobAnnouncement, ChainId, DaemonHealth, DaemonLifecycleSignal, DaemonRef,
    MeshOsEvent, NodeHealth, NodeId, PlacementIntent, ReplicaUpdate,
};

/// Folded view of what's happening on the cluster *right now*,
/// from this node's vantage. Updated by `apply` as events
/// arrive; read by the reconcile pass.
#[derive(Clone, Debug, Default)]
pub struct MeshOsState {
    /// Per-daemon observed status (Phase B fills the body).
    pub daemons: HashMap<DaemonRef, DaemonStatus>,
    /// Replicas this node observes — keyed by chain id, valued
    /// by the set of holders the substrate knows about.
    pub replicas: HashMap<ChainId, Vec<NodeId>>,
    /// Per-peer RTT samples (latest only; Phase D adds the rolling window).
    pub rtt: HashMap<NodeId, Duration>,
    /// Per-peer health (Phase D fills the body).
    pub node_health: HashMap<NodeId, NodeHealth>,
    /// Maintenance state for each peer (Phase E owns the
    /// transitions; here we only mirror what the admin chain
    /// committed).
    pub maintenance: HashMap<NodeId, MaintenanceMirror>,
    /// Blobs this node knows about, keyed by blob id.
    pub blobs: HashMap<u64, BlobObservation>,
    /// Peers currently on the local avoid list, with their TTL.
    pub avoid_list: HashMap<NodeId, AvoidEntry>,
    /// Last `Tick` we processed — used by tests / diagnostics.
    pub last_tick: Option<Instant>,
}

/// Per-daemon observed status. Phase A keeps it minimal; Phase B
/// extends with backoff state, crash-loop flag, last-restart
/// timestamps.
#[derive(Clone, Debug, Default)]
pub struct DaemonStatus {
    /// Latest self-reported health, if any.
    pub health: Option<DaemonHealth>,
    /// Latest self-reported saturation in `[0.0, 1.0]`.
    pub saturation: f32,
    /// Wall time of the most recent `Started` signal.
    pub last_started: Option<Instant>,
    /// Wall time of the most recent `ExitedCleanly` signal.
    pub last_exit: Option<Instant>,
    /// Wall time of the most recent `Crashed` signal.
    pub last_crash: Option<Instant>,
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
    pub fn apply(&mut self, event: &MeshOsEvent) {
        match event {
            MeshOsEvent::Tick => {
                self.last_tick = Some(Instant::now());
                self.gc_avoid_list();
            }
            MeshOsEvent::ReplicaUpdate(update) => self.apply_replica(update),
            MeshOsEvent::DaemonLifecycle { daemon, signal } => {
                self.apply_daemon(daemon, signal);
            }
            MeshOsEvent::RttSample { peer, rtt } => {
                self.rtt.insert(*peer, *rtt);
            }
            MeshOsEvent::NodeHealth { peer, health } => {
                self.node_health.insert(*peer, *health);
            }
            MeshOsEvent::AdminEvent(admin) => self.apply_admin(admin),
            MeshOsEvent::BlobAnnouncement(blob) => self.apply_blob(blob),
            MeshOsEvent::PlacementIntent(_) => {
                // Placement intent is desired-state input; the
                // loop routes it into `DesiredState`, not here.
            }
            MeshOsEvent::Shutdown => {
                // Shutdown is loop-control; no fold side effect.
            }
        }
    }

    fn apply_replica(&mut self, update: &ReplicaUpdate) {
        match update {
            ReplicaUpdate::Added { chain, holder } | ReplicaUpdate::Repaired { chain, holder } => {
                let entry = self.replicas.entry(*chain).or_default();
                if !entry.contains(holder) {
                    entry.push(*holder);
                }
            }
            ReplicaUpdate::Removed { chain, holder } | ReplicaUpdate::Lost { chain, holder } => {
                if let Some(entry) = self.replicas.get_mut(chain) {
                    entry.retain(|h| h != holder);
                }
            }
        }
    }

    fn apply_daemon(&mut self, daemon: &DaemonRef, signal: &DaemonLifecycleSignal) {
        let status = self.daemons.entry(daemon.clone()).or_default();
        match signal {
            DaemonLifecycleSignal::Started { at } => status.last_started = Some(*at),
            DaemonLifecycleSignal::ExitedCleanly { at } => status.last_exit = Some(*at),
            DaemonLifecycleSignal::Crashed { at, .. } => status.last_crash = Some(*at),
            DaemonLifecycleSignal::HealthChanged { health, .. } => {
                status.health = Some(health.clone());
            }
            DaemonLifecycleSignal::SaturationChanged { saturation, .. } => {
                status.saturation = *saturation;
            }
        }
    }

    fn apply_admin(&mut self, admin: &AdminEvent) {
        // Phase A only mirrors the maintenance-state piece of
        // the admin surface; the rest of the AdminEvent variants
        // get handlers in Phase D / E.
        match admin {
            AdminEvent::EnterMaintenance { node, .. } => {
                self.maintenance
                    .insert(*node, MaintenanceMirror::EnteringMaintenance);
            }
            AdminEvent::ExitMaintenance { node } => {
                self.maintenance
                    .insert(*node, MaintenanceMirror::ExitingMaintenance);
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

    fn gc_avoid_list(&mut self) {
        let now = Instant::now();
        self.avoid_list.retain(|_, entry| entry.until > now);
    }
}

/// Folded desired-state view — what Dataforts says the cluster
/// *should* look like. Reconcile reads both sides and computes
/// the diff.
#[derive(Clone, Debug, Default)]
pub struct DesiredState {
    /// Desired replica count per chain. The reconcile pass
    /// compares this against `MeshOsState::replicas` to emit
    /// `PullReplica` / `DropReplica` / `Request*` actions.
    pub desired_replicas: HashMap<ChainId, u32>,
}

impl DesiredState {
    /// Fold a desired-state input event. Today only
    /// `PlacementIntent` lands here; Phase B+ adds the
    /// daemon-intent shape so the supervisor knows which daemons
    /// should be running where.
    pub fn apply(&mut self, intent: &PlacementIntent) {
        self.desired_replicas
            .insert(intent.chain, intent.desired_replicas);
    }
}

#[cfg(test)]
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
        state.apply(&add(chain, 11));
        state.apply(&add(chain, 12));
        state.apply(&rm(chain, 11));
        assert_eq!(state.replicas.get(&chain), Some(&vec![12]));
    }

    #[test]
    fn replica_fold_is_idempotent_under_duplicate_add() {
        let chain: ChainId = 1;
        let mut state = MeshOsState::default();
        state.apply(&add(chain, 7));
        state.apply(&add(chain, 7));
        assert_eq!(state.replicas.get(&chain), Some(&vec![7]));
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
        state.apply(&MeshOsEvent::Tick);
        assert!(state.avoid_list.get(&1).is_none(), "expired entry not gc'd");
        assert!(state.avoid_list.get(&2).is_some(), "fresh entry dropped");
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
        actual.apply(&MeshOsEvent::PlacementIntent(intent.clone()));
        // PlacementIntent has no effect on actual state.
        assert!(actual.replicas.is_empty());
        let mut desired = DesiredState::default();
        desired.apply(&intent);
        assert_eq!(desired.desired_replicas.get(&42), Some(&3));
    }

    #[test]
    fn admin_event_enter_then_exit_mirrors_into_maintenance_map() {
        let node: NodeId = 99;
        let mut state = MeshOsState::default();
        state.apply(&MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
            node,
            deadline: None,
        }));
        assert_eq!(
            state.maintenance.get(&node).copied(),
            Some(MaintenanceMirror::EnteringMaintenance),
        );
        state.apply(&MeshOsEvent::AdminEvent(AdminEvent::ExitMaintenance { node }));
        assert_eq!(
            state.maintenance.get(&node).copied(),
            Some(MaintenanceMirror::ExitingMaintenance),
        );
    }
}
