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
    /// Ring buffer of the last N actions reconcile emitted,
    /// bounded by `action_queue_capacity`. Entries are NOT
    /// removed when the executor drains them — there's no
    /// completion signal back to the loop — so this is "what
    /// the loop recently asked for," not "what is currently
    /// in flight." Renamed from `pending` so neither the SDK
    /// surface nor downstream UIs (Deck status chip,
    /// dashboards) mislabel the count as live in-flight work.
    /// Wrapped in `Arc<[…]>` so consumers holding the
    /// snapshot past the `ArcSwap` guard can clone in O(1)
    /// instead of paying a per-element copy.
    pub recently_emitted: std::sync::Arc<[PendingActionSnapshot]>,
    /// Ring buffer of recent failures (daemon crashes, drain
    /// timeouts, etc.). Stays as a `VecDeque` because the
    /// chain-replay fold mutates this directly (`push_failure`
    /// in `chain.rs`); switching to `Arc<[…]>` would require
    /// the fold to thread a mutable working buffer separately.
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
    pub admin_audit: std::sync::Arc<[super::ice::AdminAuditRecord]>,
    /// Log ring — every `MeshOsEvent::LogLine` the loop
    /// observed, ordered oldest-first. Bounded by
    /// [`super::logs::DEFAULT_MAX_LOG_RING_RECORDS`]. The
    /// Deck SDK's `subscribe_logs(filter)` reads this ring;
    /// the future per-daemon RedEX-tail integration swaps the
    /// backing store without changing the snapshot shape.
    pub log_ring: std::sync::Arc<[super::logs::LogRecord]>,
    /// In-flight daemon migrations this node is hosting. Empty
    /// unless the runtime wired a
    /// [`super::migration_snapshot_source::MigrationSnapshotSource`]
    /// (the production
    /// [`super::migration_snapshot_source::OrchestratorMigrationSnapshotSource`]
    /// wraps a `MigrationOrchestrator`). The ICE
    /// `simulate_kill_migration` blast-radius preview reads
    /// this to enumerate the affected daemon when the
    /// operator targets a migration this node hosts.
    pub in_flight_migrations: std::sync::Arc<[MigrationSnapshot]>,
    /// Boot-time identifier the MeshOsLoop stamped when it
    /// started. Stays constant for the lifetime of the loop
    /// task and changes on every restart. SDK consumers
    /// dedup'ing via `seq` values (audit / log / failure
    /// rings) pair every watermark with this value — when
    /// the snapshot's `runtime_epoch_id` doesn't match the
    /// consumer's last-seen epoch, the seq counter reset and
    /// the consumer must reset its watermark to 0 rather than
    /// silently filter out post-restart records as
    /// "smaller than my last seq."
    pub runtime_epoch_id: u64,
}

/// Wire-form summary of one in-flight migration. Drawn from
/// `MigrationOrchestrator::list_migrations()` by the
/// [`super::migration_snapshot_source::MigrationSnapshotSource`]
/// adapter and embedded in the snapshot on every publish tick.
///
/// Fields after `elapsed_ms` carry `#[serde(default)]` so a
/// JSON consumer built against the pre-extension shape decodes
/// cleanly (postcard cross-binary compat still requires field
/// count + order agreement, same as `PeerSnapshot`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationSnapshot {
    /// `MigrationId` (== daemon origin hash). Matches
    /// [`super::event::MigrationId`] so the ICE simulator can
    /// look up by the same key the operator targets.
    pub daemon_origin: u64,
    /// Phase the orchestrator reports for this migration.
    pub phase: MigrationPhaseSnapshot,
    /// Milliseconds since the migration began on this node.
    pub elapsed_ms: u64,
    /// Node ID where the daemon is being migrated FROM.
    #[serde(default)]
    pub source_node: u64,
    /// Node ID where the daemon is being migrated TO.
    #[serde(default)]
    pub target_node: u64,
    /// Milliseconds since the current phase was entered.
    /// Distinct from `elapsed_ms` so operators can tell a
    /// migration stuck in Replay for 30 minutes apart from one
    /// merely 30 minutes old overall.
    #[serde(default)]
    pub age_in_phase_ms: u64,
    /// Snapshot payload size in bytes; `None` while the source
    /// hasn't produced one yet.
    #[serde(default)]
    pub snapshot_bytes: Option<u64>,
    /// Retry attempts accumulated by the orchestrator's
    /// retry-driver. `0` while no retry has been observed.
    #[serde(default)]
    pub retries: u32,
    /// Best-effort progress percentage. Today the orchestrator
    /// doesn't track byte-level transfer progress, so this is
    /// derived from the phase ordinal (Snapshot → ... → Complete)
    /// and reads as a coarse pipeline indicator rather than a
    /// fine-grained transfer gauge. `None` for phases the deck
    /// can't classify.
    #[serde(default)]
    pub progress_pct: Option<u8>,
    /// Events buffered awaiting replay. Bloats while the source
    /// runs Snapshot/Transfer/Restore and drains during Replay;
    /// a flat-high count is a "cutover overdue" signal.
    #[serde(default)]
    pub buffered_events: u32,
}

/// Wire form of [`crate::adapter::net::compute::MigrationPhase`].
/// `Default` is `Snapshot` (the first phase) purely as an
/// in-memory placeholder for parent structs that need
/// `Default`; the postcard decoder does **not** fall back to
/// `Default` for unknown variants — an older decoder hitting a
/// newer variant from across the wire errors at decode time.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationPhaseSnapshot {
    /// Source-side snapshot in progress.
    #[default]
    Snapshot,
    /// Transferring the snapshot to the target node.
    Transfer,
    /// Target-side restore + event buffering.
    Restore,
    /// Replaying buffered events on the target.
    Replay,
    /// Atomic routing cutover.
    Cutover,
    /// Cleanup on source.
    Complete,
}

impl From<crate::adapter::net::compute::MigrationPhase> for MigrationPhaseSnapshot {
    fn from(p: crate::adapter::net::compute::MigrationPhase) -> Self {
        use crate::adapter::net::compute::MigrationPhase;
        match p {
            MigrationPhase::Snapshot => Self::Snapshot,
            MigrationPhase::Transfer => Self::Transfer,
            MigrationPhase::Restore => Self::Restore,
            MigrationPhase::Replay => Self::Replay,
            MigrationPhase::Cutover => Self::Cutover,
            MigrationPhase::Complete => Self::Complete,
        }
    }
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
    /// Host node currently running this daemon. The substrate's
    /// fold path only inserts entries into `MeshOsState::daemons`
    /// in response to *local* lifecycle signals (the supervisor
    /// on this node hands them in), so for a snapshot built on
    /// node X this is unconditionally X. The field stays on the
    /// wire so Deck-side aggregations across multiple snapshots
    /// can still answer "which node hosts daemon 0xNN?" without
    /// the operator pivoting through a separate lookup. The
    /// `placement_matches_this_node_for_every_entry` regression
    /// test pins the invariant so a future refactor that
    /// piggy-backs remote daemon state on `actual.daemons` is
    /// caught at the snapshot boundary.
    ///
    /// `#[serde(default)]` so JSON consumers built against an
    /// older shape (no `placement` key) deserialize cleanly —
    /// the field reads back as `0` (`NodeId::default()`) and the
    /// caller can treat that as "unknown" rather than failing
    /// the decode outright.
    #[serde(default)]
    pub placement: NodeId,
    /// Milliseconds since the most recent lifecycle signal
    /// relevant to the current phase: time since `Started`
    /// while the daemon is running, time since the most recent
    /// exit / crash while the daemon is `Stopped`, and `0` for
    /// daemons that never reported any lifecycle signal at all
    /// (freshly registered before the supervisor confirmed).
    /// Phase-anchored so the Deck's per-daemon age column reads
    /// as "running for X" / "stopped X ago" without leaking
    /// wall-clock state.
    ///
    /// `#[serde(default)]` for the same forward-compat reason
    /// as `placement`.
    #[serde(default)]
    pub age_ms: u64,
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
///
/// Fields after `maintenance` are the Feature-11 inventory
/// axes — extended to give Deck a per-node resource view
/// (`NODE_INVENTORY` per `DECK_PLAN.md` § Deferred work). All
/// of them are `Option`-wrapped or default-able and carry
/// `#[serde(default)]` so a JSON consumer built against the
/// older shape (no inventory keys) deserializes cleanly —
/// missing fields read back at their default. For postcard,
/// the wire is positional + length-prefixed: cross-binary
/// postcard compatibility still requires both sides agree on
/// the field count, so this defense is JSON-only. In-process
/// (the substrate's `ArcSwap<MeshOsSnapshot>` path) the change
/// is unconditionally safe because both encoder and decoder
/// are the same binary.
///
/// Note: `Copy` and `Eq` are dropped from the derive set
/// because `capability_set: BTreeSet<String>` and
/// `software_version: Option<String>` are heap-owned, and
/// `cpu_load_1m: Option<f64>` doesn't implement `Eq`. Callers
/// that previously copied a `PeerSnapshot` by value now
/// borrow or clone; the snapshot is a read-only projection so
/// the change is benign for every downstream we ship today.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSnapshot {
    /// Latest RTT in milliseconds, if any.
    pub rtt_ms: Option<u64>,
    /// Latest health classification, if any.
    pub health: Option<PeerHealthSnapshot>,
    /// Maintenance state mirror.
    pub maintenance: Option<MaintenanceMirrorSnapshot>,
    /// Host CPU load average over the last minute. `None`
    /// when no resource probe is wired (lightweight containers
    /// without procfs / a node that opted out of host
    /// sampling).
    #[serde(default)]
    pub cpu_load_1m: Option<f64>,
    /// Host memory currently used, in bytes. `None` when no
    /// resource probe is wired.
    #[serde(default)]
    pub mem_used_bytes: Option<u64>,
    /// Host memory cap, in bytes. `None` when no resource
    /// probe is wired.
    #[serde(default)]
    pub mem_total_bytes: Option<u64>,
    /// Host disk currently used, in bytes. Distinct from the
    /// dataforts blob-adapter disk: this is the *host* disk,
    /// not the per-adapter dataforts cap.
    #[serde(default)]
    pub disk_used_bytes: Option<u64>,
    /// Host disk cap, in bytes.
    #[serde(default)]
    pub disk_total_bytes: Option<u64>,
    /// Rolling 0.0..=1.0 saturation score the substrate
    /// computes from existing health-probe signals. `None`
    /// when no probe drives it. Operators dashboard this to
    /// spot peers under sustained pressure before they tip
    /// into `Degraded` health.
    #[serde(default)]
    pub saturation_trend: Option<f32>,
    /// Capabilities the peer advertises. Empty when the peer
    /// hasn't published a capability set or when the local
    /// node's capability index hasn't indexed them yet. The
    /// capability strings match what the `capability_index`
    /// projection holds.
    #[serde(default)]
    pub capability_set: std::collections::BTreeSet<String>,
    /// Substrate software version the peer is running, as a
    /// semver string. `None` when the peer hasn't advertised
    /// its version (older substrates before this surface).
    #[serde(default)]
    pub software_version: Option<String>,
    /// For fork-group members, the origin node the fork
    /// descends from. `None` for non-fork peers.
    #[serde(default)]
    pub forked_from: Option<NodeId>,
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
    /// Monotonic per-runtime sequence number. Strictly
    /// increasing across the live executor's writes — the
    /// Deck SDK's failure-tail stream uses this for dedup
    /// across snapshot polls. Records constructed by the
    /// chain-replay path (`chain::push_failure`) carry `0`;
    /// only live executor-produced records have meaningful
    /// seqs.
    pub seq: u64,
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
    #[allow(clippy::too_many_arguments)]
    pub fn from_state(
        actual: &MeshOsState,
        desired: &DesiredState,
        pending: &[PendingAction],
        recent_failures: &[FailureRecord],
        in_flight_migrations: Vec<MigrationSnapshot>,
        admin_audit_ring: &std::collections::VecDeque<super::ice::AdminAuditRecord>,
        log_ring: &std::collections::VecDeque<super::logs::LogRecord>,
        this_node: NodeId,
    ) -> Self {
        let now = actual.last_tick.unwrap_or_else(std::time::Instant::now);

        let daemons = actual
            .daemons
            .iter()
            .map(|(d, status)| {
                // For a Running / Starting / Stopping daemon
                // `age_ms` is the time since the most recent
                // `Started` signal — that's the in-process uptime
                // the Deck wants. For a Stopped daemon, however,
                // `last_started` is the time the daemon *last*
                // started, which is unrelated to the present
                // (Stopped) state: an instance that crashed five
                // seconds ago after an hour of uptime would report
                // ~3.6M ms, which reads as "still running, hour
                // old" on the Deck. Anchor against the most
                // recent exit / crash for Stopped lifecycles so
                // the column reads as "stopped X ms ago"; fall
                // back to 0 for daemons that never reported any
                // lifecycle signal at all.
                let started_age = status
                    .last_started
                    .map(|t| now.saturating_duration_since(t).as_millis() as u64);
                let stopped_age = status
                    .last_exit
                    .into_iter()
                    .chain(status.last_crash)
                    .max()
                    .map(|t| now.saturating_duration_since(t).as_millis() as u64);
                let age_ms = match status.lifecycle {
                    DaemonLifecycle::Stopped => stopped_age.or(started_age).unwrap_or(0),
                    _ => started_age.unwrap_or(0),
                };
                let snapshot = DaemonSnapshot {
                    name: d.name.clone(),
                    lifecycle: status.lifecycle.into(),
                    placement: this_node,
                    age_ms,
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
            // Inventory axes — `InventoryProbe` samples land in
            // `actual.inventory`; copy each axis through to the
            // corresponding `PeerSnapshot` field. Unsampled
            // peers stay at the snapshot's defaults (None /
            // empty set).
            for (peer, inv) in &actual.inventory {
                let entry = peers.entry(*peer).or_default();
                entry.cpu_load_1m = inv.cpu_load_1m;
                entry.mem_used_bytes = inv.mem_used_bytes;
                entry.mem_total_bytes = inv.mem_total_bytes;
                entry.disk_used_bytes = inv.disk_used_bytes;
                entry.disk_total_bytes = inv.disk_total_bytes;
                entry.saturation_trend = inv.saturation_trend;
                entry.capability_set = inv.capability_set.clone();
                entry.software_version = inv.software_version.clone();
                entry.forked_from = inv.forked_from;
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

        let recently_emitted: std::sync::Arc<[PendingActionSnapshot]> = pending
            .iter()
            .map(|p| PendingActionSnapshot {
                id: p.id.0,
                kind: action_kind_str(&p.action).to_string(),
                age_ms: now.saturating_duration_since(p.emitted_at).as_millis() as u64,
            })
            .collect::<Vec<_>>()
            .into();

        let freeze_remaining_ms = actual
            .freeze_until
            .map(|until| until.saturating_duration_since(now).as_millis() as u64);
        let admin_audit: std::sync::Arc<[super::ice::AdminAuditRecord]> =
            admin_audit_ring.iter().cloned().collect::<Vec<_>>().into();
        let log_ring_arc: std::sync::Arc<[super::logs::LogRecord]> =
            log_ring.iter().cloned().collect::<Vec<_>>().into();

        let in_flight_migrations_arc: std::sync::Arc<[MigrationSnapshot]> =
            in_flight_migrations.into();
        Self {
            daemons,
            replicas,
            peers,
            avoid_list,
            local_maintenance,
            recently_emitted,
            recent_failures: recent_failures.iter().cloned().collect(),
            freeze_remaining_ms,
            admin_audit,
            log_ring: log_ring_arc,
            in_flight_migrations: in_flight_migrations_arc,
            // Set to `0` here; the loop's publish_snapshot
            // overwrites with the per-runtime epoch stamp
            // immediately after construction so chain-fold
            // callers don't have to care about the value.
            runtime_epoch_id: 0,
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
        seq: 0,
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
            seq: 1,
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
        let snap = MeshOsSnapshot::from_state(
            &actual,
            &desired,
            &[],
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
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
        let snap = MeshOsSnapshot::from_state(
            &actual,
            &desired,
            &[],
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
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
        let snap = MeshOsSnapshot::from_state(
            &actual,
            &desired,
            &[],
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
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

        let snap = MeshOsSnapshot::from_state(
            &actual,
            &desired,
            &pending,
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
        let bytes = postcard::to_allocvec(&snap).expect("encode");
        let back: MeshOsSnapshot = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(snap, back);

        let json = serde_json::to_string(&snap).expect("encode json");
        let back2: MeshOsSnapshot = serde_json::from_str(&json).expect("decode json");
        assert_eq!(snap, back2);
    }

    #[test]
    fn placement_matches_this_node_for_every_entry() {
        // The snapshot fold for `daemons` is local-only: every
        // entry's `placement` must equal the `this_node` the
        // caller passes, regardless of the daemon id or
        // lifecycle. Anchors against a future refactor that
        // accidentally piggy-backs remote daemon state onto
        // the local `actual.daemons` map — such a change would
        // silently mis-label remote daemons as locally hosted
        // unless the populator was updated alongside it.
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(Instant::now());
        for (name, id) in [("alpha", 1u64), ("beta", 2), ("gamma", 3)] {
            let mut status = DaemonStatus::default();
            status.lifecycle = DaemonLifecycle::Running;
            actual.daemons.insert(dref(name, id), status);
        }
        for this_node in [0u64, 1, 0x2A2A, NodeId::MAX] {
            let snap = MeshOsSnapshot::from_state(
                &actual,
                &DesiredState::default(),
                &[],
                &[],
                Vec::new(),
                &std::collections::VecDeque::new(),
                &std::collections::VecDeque::new(),
                this_node,
            );
            assert!(!snap.daemons.is_empty(), "fixture daemons were dropped");
            for (id, d) in &snap.daemons {
                assert_eq!(
                    d.placement, this_node,
                    "daemon {id} placement diverged from this_node {this_node:x}",
                );
            }
        }
    }

    #[test]
    fn daemon_age_anchors_on_last_exit_when_stopped() {
        // A daemon that ran for a long time and then crashed
        // should report `age_ms` against the exit, not the
        // last `Started`. Otherwise the Deck reads "running,
        // X hours old" for a daemon that has actually been
        // dead the whole time.
        //
        // We use small `checked_sub` offsets here (not real
        // hour-scale ones) because `Instant` is bounded by
        // system boot on Windows — subtracting an hour
        // panics if the system uptime is less than an hour
        // (common in fresh VMs / CI). The test's invariant
        // — that age_ms anchors on `last_exit`, not
        // `last_started` — holds at any timescale; only the
        // numbers in the assertion shift.
        let mut actual = MeshOsState::default();
        let now = Instant::now();
        actual.last_tick = Some(now);
        let d = dref("worker", 1);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Stopped;
        status.last_started = now.checked_sub(Duration::from_secs(2));
        status.last_exit = now.checked_sub(Duration::from_millis(500));
        // Both must construct cleanly on a sane test host.
        assert!(status.last_started.is_some());
        assert!(status.last_exit.is_some());
        actual.daemons.insert(d, status);
        let snap = MeshOsSnapshot::from_state(
            &actual,
            &DesiredState::default(),
            &[],
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
        let daemon = snap.daemons.get(&1).expect("daemon present");
        // ~500ms window. If age_ms anchored on `last_started`
        // it would be ~2000ms; if anchored on `last_exit` it
        // is ~500ms. Allow generous jitter on both sides — the
        // test pins the anchor, not the precise value.
        assert!(
            daemon.age_ms < 1500,
            "age_ms anchored wrong (looks like last_started): got {}",
            daemon.age_ms,
        );
        assert!(
            daemon.age_ms >= 400,
            "age_ms below last_exit floor: got {}",
            daemon.age_ms,
        );
    }

    #[test]
    fn daemon_age_anchors_on_last_started_when_running() {
        // The Running path should keep its original
        // semantics: time since the most recent `Started`.
        let mut actual = MeshOsState::default();
        let now = Instant::now();
        actual.last_tick = Some(now);
        let d = dref("worker", 2);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        status.last_started = Some(now - Duration::from_secs(30));
        status.last_exit = Some(now - Duration::from_secs(900));
        actual.daemons.insert(d, status);
        let snap = MeshOsSnapshot::from_state(
            &actual,
            &DesiredState::default(),
            &[],
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
        let daemon = snap.daemons.get(&2).expect("daemon present");
        assert!(daemon.age_ms >= 30_000, "got age_ms = {}", daemon.age_ms);
        assert!(daemon.age_ms < 60_000, "got age_ms = {}", daemon.age_ms);
    }

    #[test]
    fn daemon_age_uses_most_recent_of_exit_or_crash() {
        // Crash newer than exit → anchor on crash.
        let mut actual = MeshOsState::default();
        let now = Instant::now();
        actual.last_tick = Some(now);
        let d = dref("worker", 3);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Stopped;
        status.last_started = Some(now - Duration::from_secs(120));
        status.last_exit = Some(now - Duration::from_secs(90));
        status.last_crash = Some(now - Duration::from_secs(10));
        actual.daemons.insert(d, status);
        let snap = MeshOsSnapshot::from_state(
            &actual,
            &DesiredState::default(),
            &[],
            &[],
            Vec::new(),
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            0,
        );
        let daemon = snap.daemons.get(&3).expect("daemon present");
        assert!(daemon.age_ms >= 10_000, "got age_ms = {}", daemon.age_ms);
        assert!(daemon.age_ms < 60_000, "got age_ms = {}", daemon.age_ms);
    }

    #[test]
    fn daemon_snapshot_decodes_legacy_json_without_placement_or_age() {
        // Pre-`placement`/`age_ms` JSON shape. Reconstructed by
        // hand so the test pins the contract: a Deck-SDK client
        // built against the older schema can still decode a
        // newer snapshot's per-daemon entries, and a substrate
        // built today can still decode an older snapshot's
        // entries without the new fields.
        let legacy = r#"{
            "name": "compute-A",
            "lifecycle": "Running",
            "health": "Healthy",
            "saturation": 0.25,
            "restart_state": "Idle"
        }"#;
        let back: DaemonSnapshot = serde_json::from_str(legacy).expect("decode legacy");
        assert_eq!(back.name, "compute-A");
        assert_eq!(back.lifecycle, DaemonLifecycleSnapshot::Running);
        assert_eq!(back.placement, 0);
        assert_eq!(back.age_ms, 0);
    }

    #[test]
    fn peer_snapshot_decodes_legacy_json_without_inventory_fields() {
        // Pre-inventory JSON shape: only `rtt_ms`, `health`,
        // `maintenance`. The Feature-11 inventory axes (cpu /
        // mem / disk / saturation_trend / capability_set /
        // software_version / forked_from) must all default to
        // None / empty when absent so a Deck SDK consumer
        // bumping its substrate dep doesn't see decode failures.
        let legacy = r#"{
            "rtt_ms": 7,
            "health": "Healthy",
            "maintenance": "Active"
        }"#;
        let back: PeerSnapshot = serde_json::from_str(legacy).expect("decode legacy");
        assert_eq!(back.rtt_ms, Some(7));
        assert!(back.cpu_load_1m.is_none());
        assert!(back.mem_used_bytes.is_none());
        assert!(back.capability_set.is_empty());
        assert!(back.software_version.is_none());
        assert!(back.forked_from.is_none());
    }

    #[test]
    fn daemon_snapshot_postcard_wire_is_byte_stable() {
        // The first-pass review's `serde(default)` fix is
        // JSON-only — postcard cross-binary decode still
        // requires field count + order agreement between
        // encoder and decoder. Pin the on-wire bytes for a
        // representative `DaemonSnapshot` so any accidental
        // wire change (added field without a corresponding
        // bump, reordered fields, type substitution) trips
        // a clear regression here instead of silently rolling
        // out to consumers and surfacing as decode errors at
        // operator time.
        let s = DaemonSnapshot {
            name: "x".into(),
            lifecycle: DaemonLifecycleSnapshot::Running,
            health: None,
            saturation: 0.5,
            restart_state: RestartStateSnapshot::Idle,
            placement: 0xAA,
            age_ms: 1234,
        };
        let bytes = postcard::to_allocvec(&s).expect("encode");
        // Captured 2026-05-16 against this exact field shape.
        // To rotate after an intentional schema bump: drop a
        // `dbg!(&bytes);` here, re-run, paste the printout.
        let captured: &[u8] = &[
            0x01, 0x78, 0x02, 0x00, 0x00, 0x00, 0x00, 0x3F, 0x00, 0xAA, 0x01, 0xD2, 0x09,
        ];
        assert_eq!(
            bytes, captured,
            "DaemonSnapshot postcard wire drifted — got {bytes:?}",
        );
        let back: DaemonSnapshot = postcard::from_bytes(captured).expect("decode captured bytes");
        assert_eq!(back, s);
    }

    #[test]
    fn peer_snapshot_postcard_wire_is_byte_stable() {
        // Same forward-compat guard as the DaemonSnapshot test
        // above, against `PeerSnapshot` whose Feature-11
        // inventory axes were the most recent additions and
        // the most exposed via the Deck SDK surface.
        let mut p = PeerSnapshot {
            rtt_ms: Some(7),
            health: Some(PeerHealthSnapshot::Healthy),
            maintenance: Some(MaintenanceMirrorSnapshot::Active),
            cpu_load_1m: Some(0.25),
            mem_used_bytes: Some(1024),
            mem_total_bytes: Some(8192),
            disk_used_bytes: None,
            disk_total_bytes: None,
            saturation_trend: Some(0.4),
            capability_set: std::collections::BTreeSet::new(),
            software_version: Some("v1".into()),
            forked_from: None,
        };
        p.capability_set.insert("net.peer".into());
        let bytes = postcard::to_allocvec(&p).expect("encode");
        let captured: &[u8] = &[
            0x01, 0x07, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xD0,
            0x3F, 0x01, 0x80, 0x08, 0x01, 0x80, 0x40, 0x00, 0x00, 0x01, 0xCD, 0xCC, 0xCC, 0x3E,
            0x01, 0x08, 0x6E, 0x65, 0x74, 0x2E, 0x70, 0x65, 0x65, 0x72, 0x01, 0x02, 0x76, 0x31,
            0x00,
        ];
        assert_eq!(
            bytes, captured,
            "PeerSnapshot postcard wire drifted — got {bytes:?}",
        );
        let back: PeerSnapshot = postcard::from_bytes(captured).expect("decode captured bytes");
        assert_eq!(back, p);
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
