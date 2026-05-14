//! Phase F — behavior snapshot for Deck.
//!
//! [`MeshOsSnapshot`] is the serializable projection of the
//! current loop state. Deck queries it via MeshDB (`MeshQuery::Latest`
//! against the snapshot chain); the federated executor routes
//! to a node holding the fold; the result row carries the
//! postcard-encoded snapshot.
//!
//! Phase F lands the shape + the build function + the
//! postcard / JSON round-trip stability hatch. The
//! `RedexFold<MeshOsSnapshot>` trait wiring (which makes the
//! snapshot a CortEX fold over the MeshOS action chain) follows
//! when the action-chain integration lands; for now the loop
//! can build a snapshot on demand via
//! [`MeshOsSnapshot::from_state`].
//!
//! All fields are `Serialize + Deserialize`. `Instant` doesn't
//! serialize, so the snapshot stores wall-clock-relative
//! milliseconds where the substrate state carries instants.
//! Tests pin the postcard + JSON round-trips so a future
//! refactor can't silently change the wire shape Deck depends
//! on.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::action::{MeshOsAction, PendingAction};
use super::event::{ChainId, DaemonRef, NodeId};
use super::maintenance::MaintenanceState;
use super::state::{DaemonLifecycle, DesiredState, MeshOsState};

/// Maximum recent-failures ring-buffer size kept on the loop.
/// Bounded so the snapshot stays fixed-overhead under churn.
pub const RECENT_FAILURES_CAPACITY: usize = 256;

/// Snapshot of one node's view of the cluster, projected for
/// Deck consumption. Serializable via postcard (wire) and
/// serde_json (debug / introspection).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MeshOsSnapshot {
    /// Snapshot of all daemons this node observes. Keyed by the
    /// daemon's registry id so Deck can render per-daemon
    /// cards.
    pub daemons: BTreeMap<u64, DaemonSnapshot>,
    /// Replicas observed, with their holder sets + the desired
    /// count + the elected leader.
    pub replicas: BTreeMap<ChainId, ReplicaSnapshot>,
    /// Per-peer locality / health summary.
    pub peers: BTreeMap<NodeId, PeerSnapshot>,
    /// Peers on the local avoid list with their TTLs.
    pub avoid_list: BTreeMap<NodeId, AvoidEntrySnapshot>,
    /// This node's own maintenance state.
    pub local_maintenance: MaintenanceStateSnapshot,
    /// Pending actions (reconcile emitted, executor hasn't
    /// drained yet).
    pub pending: Vec<PendingActionSnapshot>,
    /// Ring buffer of recent failures (daemon crashes, drain
    /// timeouts, etc.).
    pub recent_failures: VecDeque<FailureRecord>,
    /// Milliseconds remaining on the cluster-wide ICE freeze, or
    /// `None` if no freeze is in effect. Driven by the
    /// `AdminEvent::FreezeCluster` / `AdminEvent::ThawCluster`
    /// admin commits.
    pub freeze_remaining_ms: Option<u64>,
    /// Admin audit ring — every admin commit the loop observed
    /// (signed ICE bundles + unsigned admin events), ordered
    /// oldest-first. Bounded by
    /// [`super::ice::DEFAULT_MAX_ADMIN_AUDIT_RECORDS`]. The
    /// Deck SDK's `audit()` reads this ring;
    /// `audit().force_only()` filters to just the ICE-class
    /// entries (`AdminEvent::is_ice()`).
    pub admin_audit: Vec<super::ice::AdminAuditRecord>,
}

/// Per-daemon Deck-renderable summary.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DaemonSnapshot {
    /// Daemon name from `MeshDaemon::name()`.
    pub name: String,
    /// Lifecycle phase.
    pub lifecycle: DaemonLifecycleSnapshot,
    /// Latest self-reported health, if any.
    pub health: Option<DaemonHealthSnapshot>,
    /// Latest self-reported saturation, `[0.0, 1.0]`.
    pub saturation: f32,
    /// Crash-loop / backoff gate state.
    pub restart_state: RestartStateSnapshot,
}

/// Wire form of [`DaemonLifecycle`]. Defaults to `Stopped` for
/// round-trip stability when an older decoder meets a newer
/// encoder.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum DaemonLifecycleSnapshot {
    /// Daemon is not currently running.
    #[default]
    Stopped,
    /// Start requested; awaiting confirmation.
    Starting,
    /// Daemon is running.
    Running,
    /// Stop requested; awaiting confirmation.
    Stopping,
}

impl From<DaemonLifecycle> for DaemonLifecycleSnapshot {
    fn from(l: DaemonLifecycle) -> Self {
        match l {
            DaemonLifecycle::Stopped => Self::Stopped,
            DaemonLifecycle::Starting => Self::Starting,
            DaemonLifecycle::Running => Self::Running,
            DaemonLifecycle::Stopping => Self::Stopping,
        }
    }
}

/// Wire form of `DaemonHealth`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum DaemonHealthSnapshot {
    /// Daemon is fully operational.
    Healthy,
    /// Daemon is degraded — operator-readable reason.
    Degraded {
        /// Why the daemon is degraded.
        reason: String,
    },
    /// Daemon is non-functional.
    Unhealthy,
}

/// Wire form of the per-daemon restart gate state. `until_ms`
/// is the millisecond offset from "now" (the snapshot build
/// time) so older snapshots remain interpretable without an
/// instant reference.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum RestartStateSnapshot {
    /// No backoff in effect.
    #[default]
    Idle,
    /// Restart backoff active; admissible after `until_ms`.
    BackingOff {
        /// Milliseconds from snapshot time until the gate
        /// releases.
        until_ms: u64,
    },
    /// Crash-loop gate active; admissible after `until_ms`.
    CrashLooping {
        /// Milliseconds from snapshot time until the gate
        /// releases.
        until_ms: u64,
    },
}

/// Per-chain replica summary.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplicaSnapshot {
    /// Current holders.
    pub holders: Vec<NodeId>,
    /// Desired count (cluster-wide), if known.
    pub desired_count: Option<u32>,
    /// Elected leader, if any.
    pub leader: Option<NodeId>,
}

/// Per-peer Deck-renderable summary.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSnapshot {
    /// Latest RTT in milliseconds, if any.
    pub rtt_ms: Option<u64>,
    /// Latest health classification, if any.
    pub health: Option<PeerHealthSnapshot>,
    /// Maintenance state mirror.
    pub maintenance: Option<MaintenanceMirrorSnapshot>,
}

/// Wire form of `NodeHealth`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum PeerHealthSnapshot {
    /// Peer is responsive within the heartbeat window.
    Healthy,
    /// Peer is responsive but slow.
    Degraded,
    /// Peer is unreachable.
    Unreachable,
}

/// Wire form of `MaintenanceMirror`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceMirrorSnapshot {
    /// Active participation.
    Active,
    /// Entering maintenance.
    EnteringMaintenance,
    /// Steady-state isolated.
    Maintenance,
    /// Exiting maintenance.
    ExitingMaintenance,
    /// Drain failed.
    DrainFailed,
    /// Recovery ramp-up.
    Recovery,
}

/// Avoid-list snapshot entry. `ttl_ms` is the remaining TTL at
/// snapshot time.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvoidEntrySnapshot {
    /// Operator-readable reason.
    pub reason: String,
    /// Remaining TTL in ms at snapshot time.
    pub ttl_ms: u64,
}

/// Local-maintenance state snapshot. Mirrors
/// [`MaintenanceState`] with `Instant` fields converted to
/// milliseconds-since-entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceStateSnapshot {
    /// Active. Default.
    #[default]
    Active,
    /// Entering maintenance.
    EnteringMaintenance {
        /// Milliseconds since the transition was entered.
        since_ms: u64,
        /// Milliseconds remaining until the deadline elapses,
        /// or `None` for no deadline.
        deadline_remaining_ms: Option<u64>,
    },
    /// Steady-state maintenance.
    Maintenance {
        /// Milliseconds since the state was entered.
        since_ms: u64,
    },
    /// Exiting maintenance.
    ExitingMaintenance {
        /// Milliseconds since the state was entered.
        since_ms: u64,
    },
    /// Drain failed.
    DrainFailed {
        /// Milliseconds since the failure was recorded.
        since_ms: u64,
        /// Operator-readable reason.
        reason: String,
    },
    /// Recovery ramp-up.
    Recovery {
        /// Milliseconds since the ramp started.
        since_ms: u64,
    },
}

/// Snapshot of a pending action (reconcile emitted, action
/// executor hasn't drained yet).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingActionSnapshot {
    /// Action id (process-local; renders as the correlation key
    /// on Deck).
    pub id: u64,
    /// Discriminator name (`"start_daemon"`, `"pull_replica"`,
    /// …). Deck doesn't need to deserialize the full action
    /// payload — the kind is enough for queue-depth rendering.
    pub kind: String,
    /// Milliseconds since the action was emitted.
    pub age_ms: u64,
}

/// Recent failure record. Bounded ring buffer per
/// [`RECENT_FAILURES_CAPACITY`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureRecord {
    /// Source — `"daemon:foo"`, `"drain:node_x"`, etc.
    pub source: String,
    /// Operator-readable reason.
    pub reason: String,
    /// Wall-clock millis-since-Unix-epoch when the failure was
    /// recorded. Replay-stable (the same chain replayed on two
    /// nodes produces the same value). Consumers compute the
    /// relative "age" against their local clock at read time.
    pub recorded_at_ms: u64,
}

impl FailureRecord {
    /// Age in ms relative to `now_ms` (Unix-epoch ms). Returns 0
    /// for records dated in the future from `now_ms`'s
    /// perspective (clock skew between producer and consumer).
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.recorded_at_ms)
    }
}

impl MeshOsSnapshot {
    /// Build a snapshot from the loop's current actual + desired
    /// state + pending action queue. `pending` is whatever
    /// `MeshOsLoop` currently has emitted but not yet drained;
    /// callers pass it in (the loop has the queue, but the
    /// snapshot doesn't own it). `recent_failures` mirrors the
    /// executor's failure ring (the executor owns the writer
    /// side; the loop reads it on publish) — the snapshot copies
    /// it so consumers see executor-side dispatch failures even
    /// when the chain-fold path is not wired up.
    pub fn from_state(
        actual: &MeshOsState,
        desired: &DesiredState,
        pending: &[PendingAction],
        recent_failures: &[FailureRecord],
    ) -> Self {
        let now = actual.last_tick.unwrap_or_else(std::time::Instant::now);

        let daemons = actual
            .daemons
            .iter()
            .map(|(d, status)| {
                let snapshot = DaemonSnapshot {
                    name: d.name.clone(),
                    lifecycle: status.lifecycle.into(),
                    health: status.health.as_ref().map(|h| match h {
                        super::event::DaemonHealth::Healthy => DaemonHealthSnapshot::Healthy,
                        super::event::DaemonHealth::Degraded { reason } => {
                            DaemonHealthSnapshot::Degraded {
                                reason: reason.clone(),
                            }
                        }
                        super::event::DaemonHealth::Unhealthy => DaemonHealthSnapshot::Unhealthy,
                    }),
                    saturation: status.saturation,
                    restart_state: match status.backoff.state() {
                        super::supervision::RestartState::Idle => RestartStateSnapshot::Idle,
                        super::supervision::RestartState::BackingOff { until } => {
                            RestartStateSnapshot::BackingOff {
                                until_ms: until.saturating_duration_since(now).as_millis() as u64,
                            }
                        }
                        super::supervision::RestartState::CrashLooping { until } => {
                            RestartStateSnapshot::CrashLooping {
                                until_ms: until.saturating_duration_since(now).as_millis() as u64,
                            }
                        }
                    },
                };
                (d.id, snapshot)
            })
            .collect();

        let mut replicas: BTreeMap<ChainId, ReplicaSnapshot> = BTreeMap::new();
        for (chain, holders) in &actual.replicas {
            replicas.insert(
                *chain,
                ReplicaSnapshot {
                    holders: holders.iter().copied().collect(),
                    desired_count: desired.desired_replicas.get(chain).copied(),
                    leader: actual.replica_leader.get(chain).copied(),
                },
            );
        }
        // Chains that have a desired count but no observed
        // holders still surface so Deck can render "0/N".
        for (chain, count) in &desired.desired_replicas {
            replicas.entry(*chain).or_default().desired_count = Some(*count);
        }

        let peers: BTreeMap<NodeId, PeerSnapshot> = {
            let mut peers: BTreeMap<NodeId, PeerSnapshot> = BTreeMap::new();
            for (peer, rtt) in &actual.rtt {
                peers.entry(*peer).or_default().rtt_ms = Some(rtt.as_millis() as u64);
            }
            for (peer, health) in &actual.node_health {
                peers.entry(*peer).or_default().health = Some(match health {
                    super::event::NodeHealth::Healthy => PeerHealthSnapshot::Healthy,
                    super::event::NodeHealth::Degraded => PeerHealthSnapshot::Degraded,
                    super::event::NodeHealth::Unreachable => PeerHealthSnapshot::Unreachable,
                });
            }
            for (peer, mirror) in &actual.maintenance {
                peers.entry(*peer).or_default().maintenance = Some(match mirror {
                    super::state::MaintenanceMirror::Active => MaintenanceMirrorSnapshot::Active,
                    super::state::MaintenanceMirror::EnteringMaintenance => {
                        MaintenanceMirrorSnapshot::EnteringMaintenance
                    }
                    super::state::MaintenanceMirror::Maintenance => {
                        MaintenanceMirrorSnapshot::Maintenance
                    }
                    super::state::MaintenanceMirror::ExitingMaintenance => {
                        MaintenanceMirrorSnapshot::ExitingMaintenance
                    }
                    super::state::MaintenanceMirror::DrainFailed => {
                        MaintenanceMirrorSnapshot::DrainFailed
                    }
                    super::state::MaintenanceMirror::Recovery => {
                        MaintenanceMirrorSnapshot::Recovery
                    }
                });
            }
            peers
        };

        let avoid_list = actual
            .avoid_list
            .iter()
            .map(|(peer, entry)| {
                let ttl = entry.until.saturating_duration_since(now);
                (
                    *peer,
                    AvoidEntrySnapshot {
                        reason: entry.reason.clone(),
                        ttl_ms: ttl.as_millis() as u64,
                    },
                )
            })
            .collect();

        let local_maintenance = match &actual.local_maintenance {
            MaintenanceState::Active => MaintenanceStateSnapshot::Active,
            MaintenanceState::EnteringMaintenance { since, deadline } => {
                MaintenanceStateSnapshot::EnteringMaintenance {
                    since_ms: now.saturating_duration_since(*since).as_millis() as u64,
                    deadline_remaining_ms: deadline
                        .map(|d| d.saturating_duration_since(now).as_millis() as u64),
                }
            }
            MaintenanceState::Maintenance { since } => MaintenanceStateSnapshot::Maintenance {
                since_ms: now.saturating_duration_since(*since).as_millis() as u64,
            },
            MaintenanceState::ExitingMaintenance { since } => {
                MaintenanceStateSnapshot::ExitingMaintenance {
                    since_ms: now.saturating_duration_since(*since).as_millis() as u64,
                }
            }
            MaintenanceState::DrainFailed { since, reason } => {
                MaintenanceStateSnapshot::DrainFailed {
                    since_ms: now.saturating_duration_since(*since).as_millis() as u64,
                    reason: reason.clone(),
                }
            }
            MaintenanceState::Recovery { since } => MaintenanceStateSnapshot::Recovery {
                since_ms: now.saturating_duration_since(*since).as_millis() as u64,
            },
        };

        let pending = pending
            .iter()
            .map(|p| PendingActionSnapshot {
                id: p.id.0,
                kind: action_kind_str(&p.action).to_string(),
                age_ms: now.saturating_duration_since(p.emitted_at).as_millis() as u64,
            })
            .collect();

        let freeze_remaining_ms = actual
            .freeze_until
            .map(|until| until.saturating_duration_since(now).as_millis() as u64);
        let admin_audit: Vec<super::ice::AdminAuditRecord> =
            actual.admin_audit.iter().cloned().collect();

        Self {
            daemons,
            replicas,
            peers,
            avoid_list,
            local_maintenance,
            pending,
            recent_failures: recent_failures.iter().cloned().collect(),
            freeze_remaining_ms,
            admin_audit,
        }
    }
}

/// Stable lowercase kind discriminator for [`MeshOsAction`].
/// Phase F-stable; Deck branches on this without deserializing
/// the full action payload.
pub fn action_kind_str(action: &MeshOsAction) -> &'static str {
    match action {
        MeshOsAction::StartDaemon { .. } => "start_daemon",
        MeshOsAction::StopDaemon { .. } => "stop_daemon",
        MeshOsAction::PullReplica { .. } => "pull_replica",
        MeshOsAction::DropReplica { .. } => "drop_replica",
        MeshOsAction::RequestPlacement { .. } => "request_placement",
        MeshOsAction::RequestEviction { .. } => "request_eviction",
        MeshOsAction::MigrateBlob { .. } => "migrate_blob",
        MeshOsAction::ReduceHeat { .. } => "reduce_heat",
        MeshOsAction::MarkAvoid { .. } => "mark_avoid",
        MeshOsAction::ApplyBackoff { .. } => "apply_backoff",
        MeshOsAction::CommitMaintenanceTransition { .. } => "commit_maintenance_transition",
    }
}

/// Helper for tests + callers: dummy `now`-anchored
/// recent-failure record builder.
pub fn failure_record(
    source: impl Into<String>,
    reason: impl Into<String>,
    age: Duration,
) -> FailureRecord {
    // `age` is interpreted as "this many ms before now"; produce
    // a `recorded_at_ms` that, evaluated at the current
    // wall-clock, projects back to `age`.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let recorded_at_ms = now_ms.saturating_sub(age.as_millis() as u64);
    FailureRecord {
        source: source.into(),
        reason: reason.into(),
        recorded_at_ms,
    }
}

/// `DaemonRef` is `Hash + Eq` but the snapshot keyspace is just
/// the registry-local id (the name lives on the daemon-side
/// snapshot). This adapter is exported for callers that hold
/// `DaemonRef` and want the id-keyed snapshot.
pub fn daemon_id(d: &DaemonRef) -> u64 {
    d.id
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use std::time::Instant;

    use super::super::action::{ActionId, MaintenanceTransition};
    use super::super::event::{DaemonHealth, NodeHealth};
    use super::super::maintenance::MaintenanceState;
    use super::super::state::{AvoidEntry, DaemonStatus, MaintenanceMirror};
    use super::*;

    fn dref(name: &str, id: u64) -> DaemonRef {
        DaemonRef {
            id,
            name: name.into(),
        }
    }

    #[test]
    fn failure_record_age_ms_derives_from_recorded_at_ms() {
        // Regression for I12: the field used to be a constant
        // `age_ms = 0`. It's now a Unix-epoch timestamp;
        // consumers compute the relative age locally.
        let r = FailureRecord {
            source: "test".into(),
            reason: "boom".into(),
            recorded_at_ms: 1_000,
        };
        assert_eq!(r.age_ms(3_000), 2_000);
        // Clock skew where a peer reports a record dated after
        // the consumer's "now" produces 0 (saturating).
        assert_eq!(r.age_ms(500), 0);
    }

    #[test]
    fn empty_snapshot_round_trips_through_postcard() {
        let s = MeshOsSnapshot::default();
        let bytes = postcard::to_allocvec(&s).expect("encode");
        let back: MeshOsSnapshot = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(s, back);
    }

    #[test]
    fn empty_snapshot_round_trips_through_json() {
        let s = MeshOsSnapshot::default();
        let json = serde_json::to_string(&s).expect("encode");
        let back: MeshOsSnapshot = serde_json::from_str(&json).expect("decode");
        assert_eq!(s, back);
    }

    #[test]
    fn snapshot_captures_daemon_lifecycle_and_health() {
        let mut actual = MeshOsState::default();
        let base = Instant::now();
        actual.last_tick = Some(base);
        let d = dref("telemetry", 1);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        status.health = Some(DaemonHealth::Degraded {
            reason: "queue depth".into(),
        });
        status.saturation = 0.42;
        actual.daemons.insert(d.clone(), status);
        let desired = DesiredState::default();
        let snap = MeshOsSnapshot::from_state(&actual, &desired, &[], &[]);
        let daemon = snap.daemons.get(&1).expect("daemon present");
        assert_eq!(daemon.name, "telemetry");
        assert_eq!(daemon.lifecycle, DaemonLifecycleSnapshot::Running);
        assert!(matches!(
            daemon.health,
            Some(DaemonHealthSnapshot::Degraded { .. })
        ));
        assert!((daemon.saturation - 0.42).abs() < 1e-6);
    }

    #[test]
    fn snapshot_captures_replica_holders_and_leader_and_desired_count() {
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(Instant::now());
        actual
            .replicas
            .insert(0xAA, ::std::collections::BTreeSet::from([1, 2, 3]));
        actual.replica_leader.insert(0xAA, 1);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(0xAA, 5);
        let snap = MeshOsSnapshot::from_state(&actual, &desired, &[], &[]);
        let r = snap.replicas.get(&0xAA).expect("replica present");
        assert_eq!(r.holders, vec![1, 2, 3]);
        assert_eq!(r.desired_count, Some(5));
        assert_eq!(r.leader, Some(1));
    }

    #[test]
    fn snapshot_surfaces_chains_with_desired_count_but_no_holders_yet() {
        let actual = MeshOsState::default();
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(0xBB, 3);
        let snap = MeshOsSnapshot::from_state(&actual, &desired, &[], &[]);
        let r = snap
            .replicas
            .get(&0xBB)
            .expect("missing chain not surfaced");
        assert_eq!(r.holders, Vec::<NodeId>::new());
        assert_eq!(r.desired_count, Some(3));
    }

    #[test]
    fn snapshot_round_trips_a_realistic_state() {
        let mut actual = MeshOsState::default();
        let base = Instant::now();
        actual.last_tick = Some(base);

        let d = dref("watch", 7);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        status.health = Some(DaemonHealth::Healthy);
        actual.daemons.insert(d, status);

        actual
            .replicas
            .insert(0xC0FFEE, ::std::collections::BTreeSet::from([10, 11]));
        actual.replica_leader.insert(0xC0FFEE, 10);
        actual.rtt.insert(10, Duration::from_millis(45));
        actual.node_health.insert(10, NodeHealth::Healthy);
        actual
            .maintenance
            .insert(11, MaintenanceMirror::Maintenance);
        actual.avoid_list.insert(
            999,
            AvoidEntry {
                reason: "noisy".into(),
                until: base + Duration::from_secs(120),
            },
        );
        actual.local_maintenance = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: Some(base + Duration::from_secs(60)),
        };

        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(0xC0FFEE, 3);

        let pending = [PendingAction {
            id: ActionId(1),
            action: MeshOsAction::CommitMaintenanceTransition {
                node: 0,
                target: MaintenanceTransition::Maintenance,
            },
            emitted_at: base,
        }];

        let snap = MeshOsSnapshot::from_state(&actual, &desired, &pending, &[]);
        let bytes = postcard::to_allocvec(&snap).expect("encode");
        let back: MeshOsSnapshot = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(snap, back);

        let json = serde_json::to_string(&snap).expect("encode json");
        let back2: MeshOsSnapshot = serde_json::from_str(&json).expect("decode json");
        assert_eq!(snap, back2);
    }

    #[test]
    fn action_kind_str_covers_every_variant() {
        // Without `MeshOsAction`'s `#[non_exhaustive]` we'd get
        // a compile error if `action_kind_str` missed a variant.
        // The attribute means future variants surface as a
        // build-side warning, not a runtime breakage. Trip every
        // shipped variant so a refactor that drops one shows up
        // here.
        let cases: Vec<MeshOsAction> = vec![
            MeshOsAction::StartDaemon {
                daemon: dref("a", 1),
            },
            MeshOsAction::StopDaemon {
                daemon: dref("a", 1),
                reason: "x".into(),
                deadline: Instant::now(),
            },
            MeshOsAction::PullReplica {
                chain: 1,
                source: 2,
            },
            MeshOsAction::DropReplica { chain: 1 },
            MeshOsAction::RequestPlacement {
                chain: 1,
                exclude: vec![],
                target: None,
            },
            MeshOsAction::RequestEviction {
                chain: 1,
                victim: 2,
            },
            MeshOsAction::MigrateBlob {
                blob: 1,
                from: 2,
                to: 3,
            },
            MeshOsAction::ReduceHeat { blob: 1, by: 1 },
            MeshOsAction::MarkAvoid {
                peer: 1,
                reason: "x".into(),
                ttl: Duration::from_secs(60),
            },
            MeshOsAction::ApplyBackoff {
                daemon: dref("a", 1),
                until: Instant::now(),
            },
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Maintenance,
            },
        ];
        for a in cases {
            // Just exercise the discriminator; the test
            // succeeds when every shipped variant has a stable
            // kind string.
            let _ = action_kind_str(&a);
        }
    }
}
