//! Pure-function coordinator step — converts the substrate's
//! state pieces ([`HeartbeatTracker`], [`BandwidthBudget`],
//! current [`ReplicaRole`], local `tail_seq`) into a deterministic
//! list of [`OutboundMessage`]s the runtime layer ships through the
//! mesh, plus an optional [`PendingTransition`] the coordinator
//! should apply.
//!
//! Slot-fills the gap between the existing pure-logic pieces and
//! the runtime layer that doesn't exist yet. The eventual tokio
//! interval-driven loop calls [`tick`] each tick:
//!
//! ```text
//! loop {
//!     let outcome = replication_step::tick(TickInputs { ... });
//!     for msg in outcome.outbound {
//!         mesh.dispatch(msg).await;
//!     }
//!     if let Some(pending) = outcome.transition {
//!         coordinator.transition_to(pending.target, pending.signal).await;
//!     }
//!     interval.tick().await;
//! }
//! ```
//!
//! Inbound events (peer heartbeats, sync requests, sync responses,
//! nacks) drive separate event handlers that update the tracker /
//! budget state synchronously; the next [`tick`] picks up the
//! observation.
//!
//! `tick` is pure over `(state, now)` — no I/O, no async, no
//! mutation of anything other than the references it's handed.
//! Unit-testable without tokio / mesh / live RedexFile.

use std::time::Instant;

use super::replication::{ChannelId, ReplicaRole, SyncHeartbeat, SyncRequest};

/// Default `chunk_max` carried on every replica-issued
/// [`SyncRequest`] — 256 KiB. Sized so a healthy replica stays
/// caught up on a single request per heartbeat cycle for a busy
/// channel (typical event sizes 100B – 4KiB → 60 – 2500 events per
/// chunk), while keeping the leader's response latency bounded.
/// The leader's `handle_sync_request` enforces a 64 MiB hard
/// ceiling regardless; this default sits comfortably below.
pub const SYNC_REQUEST_CHUNK_MAX_DEFAULT: u32 = 256 * 1024;
use super::replication_election::{elect, ElectionOutcome};
use super::replication_heartbeat::HeartbeatTracker;
use super::replication_state::TransitionSignal;
use crate::adapter::net::behavior::placement::NodeId;

/// Outbound wire message the runtime layer ships. The runtime
/// routes by `target` — every variant identifies the destination
/// node id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundMessage {
    /// Periodic liveness + tail-seq heartbeat. Emitted to every
    /// replica in the channel's replica set on every tick.
    Heartbeat {
        /// Destination node id.
        target: NodeId,
        /// Wire-format heartbeat payload.
        msg: SyncHeartbeat,
    },
    /// Replica → leader pull request. Emitted by `tick` when the
    /// believed leader's last-observed `tail_seq` exceeds the local
    /// `tail_seq` (i.e., we're behind). One request per lagging
    /// tick; the leader's `SyncResponse` advances the local tail
    /// and the next tick re-evaluates lag. Bounded by
    /// `chunk_max_bytes` from [`TickInputs`].
    SyncRequest {
        /// Destination leader node id.
        target: NodeId,
        /// Wire-format request payload.
        msg: SyncRequest,
    },
}

/// Transition the coordinator should apply after a tick.
/// Routed through [`super::ReplicationCoordinator::transition_to`]
/// so the state-machine validator + tag-lifecycle + metrics run.
///
/// Returned by [`tick`] rather than applied inline because the
/// coordinator's `transition_to` is async (the tag sink is
/// async); `tick` itself stays a sync pure function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingTransition {
    /// Target [`ReplicaRole`] the coordinator should transition to.
    pub target: ReplicaRole,
    /// Signal class driving the transition. Pinned per
    /// `replication_state::TransitionSignal` so the validator
    /// distinguishes "wrong signal" from "wrong pair."
    pub signal: TransitionSignal,
}

/// References the [`tick`] function reads + (selectively) mutates
/// to compute one step. The runtime layer passes references to
/// the live coordinator state.
pub struct TickInputs<'a> {
    /// This node's id — used to identify "self" in the replica
    /// set + to seed the heartbeat-tracker's self-RTT in the
    /// election function. Note: `tick` doesn't run an election
    /// itself; the coordinator's `Candidate → Leader / Replica`
    /// transition does. `tick` decides *when* to enter
    /// `Candidate`.
    pub self_node_id: NodeId,
    /// Current coordinator role. `tick` reads this to decide
    /// what to emit: leaders + replicas emit heartbeats; Idle
    /// and Candidate stay silent on the wire.
    pub current_role: ReplicaRole,
    /// 32-byte BLAKE2s channel id for the wire-format
    /// `SyncHeartbeat::channel_id` field.
    pub channel_id: ChannelId,
    /// Local `tail_seq` — emitted in every heartbeat.
    pub tail_seq: u64,
    /// Replica set membership (the canonical N replicas chosen
    /// at placement time). `tick` emits heartbeats to every
    /// member other than self.
    pub replica_set: &'a [NodeId],
    /// Heartbeat tracker. `tick` consults it for
    /// `is_leader_silent` to decide whether to request a
    /// `Replica → Candidate` transition. The post-tick caller
    /// records inbound heartbeats; this method only reads.
    pub tracker: &'a HeartbeatTracker,
    /// Wall-clock millis snapshot for the outbound
    /// `SyncHeartbeat::wall_clock_ms` field. Operator-facing
    /// drift detection only; not consumed for ordering.
    pub wall_clock_ms: u64,
    /// Maximum bytes the replica's `SyncRequest` asks for in the
    /// leader's matching response. Carried on every replica-side
    /// catch-up emission; `0` lets the leader pick (subject to
    /// the catch-up helper's 64 MiB hard ceiling).
    pub chunk_max_bytes: u32,
    /// Current instant — passed to [`HeartbeatTracker`] for
    /// silence checks.
    pub now: Instant,
}

/// What [`tick`] returns.
#[derive(Debug, Default)]
pub struct StepOutcome {
    /// Wire messages to ship. Ordered: peers iterated in
    /// `replica_set` order so emission is deterministic per
    /// tick.
    pub outbound: Vec<OutboundMessage>,
    /// Optional transition the coordinator should run via
    /// `transition_to(target, signal)`. `None` when no
    /// transition is warranted this tick.
    pub transition: Option<PendingTransition>,
}

/// Compute one tick's outbound actions + optional transition.
///
/// Behavior by role:
/// - `Leader` — emit one heartbeat to every other replica in the
///   set with `role = Leader`. No transition requested (a leader
///   doesn't preemptively elect itself out).
/// - `Replica` — emit one heartbeat to every other replica with
///   `role = Replica`. If [`HeartbeatTracker::is_leader_silent`]
///   returns true, request `transition_to(Candidate,
///   MissedHeartbeats)`.
/// - `Candidate` — no heartbeats (the role is transient,
///   microseconds-scale; emitting from this state would broadcast
///   ambiguous role information). No transition requested by
///   `tick`; the coordinator drives the `Candidate → Leader /
///   Replica` transition synchronously after the election
///   function runs.
/// - `Idle` — no heartbeats, no transition. The node carries the
///   channel's storage but has no replica role; it's not
///   participating in coordination.
///
/// `tick` itself does NOT mutate the tracker — inbound events
/// (received heartbeats) drive [`HeartbeatTracker::record_heartbeat`]
/// separately. `tick` is purely an emission + detection step.
pub fn tick(inputs: TickInputs<'_>) -> StepOutcome {
    let mut outcome = StepOutcome::default();

    if !inputs.current_role.emits_heartbeats() {
        return outcome;
    }

    // Emit one heartbeat per replica-set member other than self.
    // Skip self even if it's listed in replica_set — emitting a
    // self-heartbeat over the wire would be wasted bytes + a
    // broadcast-loop hazard. The local lag-from-self is always
    // zero by definition.
    for &peer in inputs.replica_set {
        if peer == inputs.self_node_id {
            continue;
        }
        outcome.outbound.push(OutboundMessage::Heartbeat {
            target: peer,
            msg: SyncHeartbeat {
                channel_id: inputs.channel_id,
                tail_seq: inputs.tail_seq,
                role: inputs.current_role,
                wall_clock_ms: inputs.wall_clock_ms,
            },
        });
    }

    // Leader-silence detection — only meaningful when we're a
    // Replica (we expect leader heartbeats). A Leader watching
    // its own peers' silence is a different concern (the
    // leader-side lag metric) and doesn't trigger a transition.
    if inputs.current_role == ReplicaRole::Replica && inputs.tracker.is_leader_silent(inputs.now) {
        outcome.transition = Some(PendingTransition {
            target: ReplicaRole::Candidate,
            signal: TransitionSignal::MissedHeartbeats,
        });
        // Skip the lag-driven SyncRequest emission below — once
        // we've decided to elect, the believed leader is stale
        // and the request would race the role transition. The
        // next tick (post-election) will re-evaluate against
        // the new leader.
        return outcome;
    }

    // Lag detection — when the believed leader's last-observed
    // tail_seq exceeds our local tail, issue a SyncRequest so the
    // catch-up helper can advance us. One request per lagging
    // tick; if a response is already in flight the replica's
    // `apply_sync_response` will advance the tail and the next
    // tick re-evaluates. Repeated requests within a flight window
    // are bounded by the heartbeat cadence — typically 500 ms —
    // and the leader's bandwidth budget rejects with
    // `Backpressure` if it can't keep up.
    if inputs.current_role == ReplicaRole::Replica {
        // R-15: filter out a misrouted self-as-leader heartbeat
        // before emitting a SyncRequest. The tracker normally
        // doesn't see self heartbeats but a defensive filter
        // keeps `tick()` from ever emitting a wire frame
        // addressed to the local node id.
        if let Some(leader) = inputs
            .tracker
            .believed_leader()
            .filter(|&l| l != inputs.self_node_id)
        {
            if let Some(peer) = inputs.tracker.peer_state(leader) {
                if peer.tail_seq > inputs.tail_seq {
                    outcome.outbound.push(OutboundMessage::SyncRequest {
                        target: leader,
                        msg: SyncRequest {
                            channel_id: inputs.channel_id,
                            since_seq: inputs.tail_seq,
                            chunk_max: inputs.chunk_max_bytes,
                        },
                    });
                }
            }
        }
    }

    outcome
}

/// Compute the deterministic election winner from the runtime's
/// current view + return the right [`PendingTransition`] for the
/// coordinator. Called by the runtime when the coordinator is in
/// the `Candidate` state — `tick`'s "request Candidate" plus this
/// helper's "request Leader/Replica" together cover the
/// `Replica → Candidate → Leader|Replica` cycle.
///
/// Behavior:
/// - `ElectionOutcome::SelfWins` → `PendingTransition { Leader,
///   ElectionWon }`.
/// - `ElectionOutcome::PeerWins(_)` → `PendingTransition { Replica,
///   ElectionLost }`.
/// - `ElectionOutcome::NoEligibleReplica` → `None` (no transition;
///   the coordinator stays in `Candidate` until the next round
///   when `tracker.healthy_peers` repopulates).
///
/// `rtt_to` and `health_of` follow the
/// [`super::elect`] signature — same predicates the coordinator
/// would pass directly. `tracker` supplies `health_of` via
/// `healthy_peers(now).contains(node)`; the runtime layer wraps
/// it.
pub fn election_outcome<R, H>(
    self_node_id: NodeId,
    replica_set: &[NodeId],
    rtt_to: R,
    health_of: H,
) -> Option<PendingTransition>
where
    R: Fn(NodeId) -> Option<std::time::Duration>,
    H: Fn(NodeId) -> bool,
{
    match elect(replica_set, self_node_id, rtt_to, health_of) {
        ElectionOutcome::SelfWins => Some(PendingTransition {
            target: ReplicaRole::Leader,
            signal: TransitionSignal::ElectionWon,
        }),
        ElectionOutcome::PeerWins(_) => Some(PendingTransition {
            target: ReplicaRole::Replica,
            signal: TransitionSignal::ElectionLost,
        }),
        ElectionOutcome::NoEligibleReplica => None,
    }
}

/// Extension: which replica roles emit periodic heartbeats?
/// Leader + Replica do; Candidate (transient) and Idle (not
/// participating) stay silent.
trait ReplicaRoleExt {
    fn emits_heartbeats(self) -> bool;
}

impl ReplicaRoleExt for ReplicaRole {
    fn emits_heartbeats(self) -> bool {
        matches!(self, ReplicaRole::Leader | ReplicaRole::Replica)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::channel::ChannelName;
    use std::time::Duration;

    fn channel_id_for(name: &str) -> ChannelId {
        let cn = ChannelName::new(name).unwrap();
        ChannelId::from_name(&cn)
    }

    fn t0() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn empty_tracker() -> HeartbeatTracker {
        HeartbeatTracker::new(500)
    }

    fn tracker_with_silent_leader(leader: NodeId, miss_seconds: u64) -> HeartbeatTracker {
        let mut t = HeartbeatTracker::new(500); // 500ms cadence, 3x miss = 1500ms
        let base = t0();
        t.record_heartbeat(leader, ReplicaRole::Leader, 0, base);
        // We construct the test scenario where the silence
        // threshold is met by calling `is_leader_silent(now)`
        // with `now = base + miss_seconds * 1000ms`.
        let _ = miss_seconds; // wired through `now` in the caller
        t
    }

    // ────────────────────────────────────────────────────────────────
    // Heartbeat emission by role
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn idle_emits_no_heartbeats() {
        let tracker = empty_tracker();
        let inputs = TickInputs {
            self_node_id: 0x1,
            current_role: ReplicaRole::Idle,
            channel_id: channel_id_for("test/idle"),
            tail_seq: 0,
            replica_set: &[0x1, 0x2, 0x3],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        assert!(outcome.outbound.is_empty());
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn candidate_emits_no_heartbeats() {
        let tracker = empty_tracker();
        let inputs = TickInputs {
            self_node_id: 0x1,
            current_role: ReplicaRole::Candidate,
            channel_id: channel_id_for("test/candidate"),
            tail_seq: 0,
            replica_set: &[0x1, 0x2, 0x3],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        assert!(outcome.outbound.is_empty());
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn leader_emits_to_every_other_replica() {
        let tracker = empty_tracker();
        let cid = channel_id_for("test/leader");
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Leader,
            channel_id: cid,
            tail_seq: 42,
            replica_set: &[0x10, 0x20, 0x30, 0x40],
            tracker: &tracker,
            wall_clock_ms: 1_700_000_000_000,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        assert_eq!(outcome.outbound.len(), 3);
        for (i, msg) in outcome.outbound.iter().enumerate() {
            let OutboundMessage::Heartbeat { target, msg } = msg else {
                panic!("expected Heartbeat, got {msg:?}");
            };
            assert_eq!(*target, [0x20, 0x30, 0x40][i]);
            assert_eq!(msg.channel_id, cid);
            assert_eq!(msg.tail_seq, 42);
            assert_eq!(msg.role, ReplicaRole::Leader);
            assert_eq!(msg.wall_clock_ms, 1_700_000_000_000);
        }
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn replica_emits_to_every_other_replica() {
        let tracker = empty_tracker();
        let cid = channel_id_for("test/replica");
        let inputs = TickInputs {
            self_node_id: 0x20,
            current_role: ReplicaRole::Replica,
            channel_id: cid,
            tail_seq: 99,
            replica_set: &[0x10, 0x20, 0x30],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        assert_eq!(outcome.outbound.len(), 2);
        let targets: Vec<NodeId> = outcome
            .outbound
            .iter()
            .map(|m| match m {
                OutboundMessage::Heartbeat { target, .. } => *target,
                OutboundMessage::SyncRequest { .. } => {
                    panic!("expected only heartbeats; got SyncRequest")
                }
            })
            .collect();
        assert_eq!(targets, vec![0x10, 0x30]);
        // No silent leader observed (tracker is empty) → no
        // transition request.
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn solo_node_in_replica_set_emits_no_heartbeats() {
        let tracker = empty_tracker();
        let inputs = TickInputs {
            self_node_id: 0x1,
            current_role: ReplicaRole::Leader,
            channel_id: channel_id_for("test/solo"),
            tail_seq: 0,
            replica_set: &[0x1],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        // Self is the only member; no peer to send to.
        assert!(outcome.outbound.is_empty());
    }

    #[test]
    fn self_skipped_in_emission() {
        // Self appears multiple times in replica_set (degenerate
        // but allowed input shape); every self-mention is
        // skipped.
        let tracker = empty_tracker();
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Leader,
            channel_id: channel_id_for("test/self_skip"),
            tail_seq: 0,
            replica_set: &[0x10, 0x10, 0x20, 0x10],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        assert_eq!(outcome.outbound.len(), 1);
    }

    // ────────────────────────────────────────────────────────────────
    // Leader-silence detection
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn replica_with_silent_leader_requests_candidate_transition() {
        let leader_id = 0x42;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        // Leader heartbeat is observed at base.
        tracker.record_heartbeat(leader_id, ReplicaRole::Leader, 0, base);
        // Past the 3 × 500ms silence threshold.
        let now = at(base, 2_000);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/silent_leader"),
            tail_seq: 0,
            replica_set: &[0x10, leader_id],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now,
        };
        let outcome = tick(inputs);
        // Heartbeat emitted to the (now-silent) leader.
        assert_eq!(outcome.outbound.len(), 1);
        // Transition request: Candidate via MissedHeartbeats.
        assert_eq!(
            outcome.transition,
            Some(PendingTransition {
                target: ReplicaRole::Candidate,
                signal: TransitionSignal::MissedHeartbeats,
            }),
        );
    }

    #[test]
    fn replica_with_fresh_leader_does_not_request_transition() {
        let leader_id = 0x42;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        tracker.record_heartbeat(leader_id, ReplicaRole::Leader, 0, base);
        // 100ms after the heartbeat — well within window.
        let now = at(base, 100);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/fresh"),
            tail_seq: 0,
            replica_set: &[0x10, leader_id],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now,
        };
        let outcome = tick(inputs);
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn leader_with_silent_peers_does_not_request_self_transition() {
        // A leader observing its peers' silence doesn't elect
        // itself out — the silence-detection is a Replica-side
        // signal. The leader's lag-from-peer is a different
        // metric path.
        let _ = tracker_with_silent_leader; // exercise the helper for warnings
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        // We're the leader; a stale peer heartbeat (not the
        // leader's own) is in the tracker but it doesn't matter
        // because the tracker doesn't believe us to be a leader.
        tracker.record_heartbeat(0x20, ReplicaRole::Replica, 0, base);
        let now = at(base, 60_000);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Leader,
            channel_id: channel_id_for("test/leader_silent_peers"),
            tail_seq: 0,
            replica_set: &[0x10, 0x20],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now,
        };
        let outcome = tick(inputs);
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn candidate_with_silent_leader_does_not_request_double_transition() {
        // The Candidate state is the result of having already
        // run a `transition_to(Candidate, MissedHeartbeats)` once.
        // `tick` from Candidate doesn't re-trigger; the
        // coordinator drives the next hop synchronously via the
        // election function.
        let leader_id = 0x42;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        tracker.record_heartbeat(leader_id, ReplicaRole::Leader, 0, base);
        let now = at(base, 60_000);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Candidate,
            channel_id: channel_id_for("test/candidate_silent"),
            tail_seq: 0,
            replica_set: &[0x10, leader_id],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now,
        };
        let outcome = tick(inputs);
        assert!(outcome.transition.is_none());
    }

    // ────────────────────────────────────────────────────────────────
    // Election outcome routing
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn election_self_wins_yields_leader_transition() {
        let pt = election_outcome(
            0x10,
            &[0x10, 0x20],
            |_| Some(Duration::from_millis(100)),
            |_| true,
        );
        assert_eq!(
            pt,
            Some(PendingTransition {
                target: ReplicaRole::Leader,
                signal: TransitionSignal::ElectionWon,
            }),
        );
    }

    #[test]
    fn election_peer_wins_yields_replica_transition() {
        // The `elect()` function hardcodes self's RTT to zero
        // (the proximity graph doesn't store self-edges), so
        // self always wins on RTT when included as healthy. To
        // make a peer win we mark self unhealthy in
        // `health_of` — the standard shape for "this candidate
        // node is recovering / has bad observation."
        let pt = election_outcome(
            0x99,
            &[0x10, 0x99],
            |_| Some(Duration::from_millis(5)),
            |node| node != 0x99, // self unhealthy
        );
        assert_eq!(
            pt,
            Some(PendingTransition {
                target: ReplicaRole::Replica,
                signal: TransitionSignal::ElectionLost,
            }),
        );
    }

    #[test]
    fn election_no_eligible_yields_no_transition() {
        // Every peer marked unhealthy → no winner → coordinator
        // stays in Candidate until next round.
        let pt = election_outcome(0x10, &[0x10, 0x20, 0x30], |_| None, |_| false);
        assert!(pt.is_none());
    }

    #[test]
    fn emission_is_deterministic_across_calls() {
        // Two ticks against the same (state, now) produce
        // byte-identical outbound. Sanity check on the pure-
        // function contract.
        let tracker = empty_tracker();
        let cid = channel_id_for("test/deterministic");
        let mk_inputs = || TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Leader,
            channel_id: cid,
            tail_seq: 7,
            replica_set: &[0x10, 0x20, 0x30],
            tracker: &tracker,
            wall_clock_ms: 1234,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let a = tick(mk_inputs());
        let b = tick(mk_inputs());
        assert_eq!(a.outbound, b.outbound);
        assert_eq!(a.transition, b.transition);
    }

    #[test]
    fn heartbeat_carries_current_tail_seq_value() {
        // Pin: the tail_seq emitted in heartbeats is exactly
        // the value supplied at tick time (not a snapshot from
        // somewhere stale).
        let tracker = empty_tracker();
        let inputs = TickInputs {
            self_node_id: 0x1,
            current_role: ReplicaRole::Leader,
            channel_id: channel_id_for("test/tail"),
            tail_seq: u64::MAX - 1,
            replica_set: &[0x1, 0x2],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 0,
            now: t0(),
        };
        let outcome = tick(inputs);
        let OutboundMessage::Heartbeat { msg, .. } = &outcome.outbound[0] else {
            panic!("expected Heartbeat at index 0");
        };
        assert_eq!(msg.tail_seq, u64::MAX - 1);
    }

    // ────────────────────────────────────────────────────────────────
    // Lag-driven SyncRequest emission (replica catch-up)
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn replica_behind_leader_emits_sync_request() {
        // Replica's local tail is 10; leader's last-observed
        // heartbeat carried tail_seq 100. Expect one SyncRequest
        // to the leader with since_seq=10 and chunk_max=CHUNK_MAX.
        let leader_id = 0x42;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        tracker.record_heartbeat(leader_id, ReplicaRole::Leader, 100, base);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/lag"),
            tail_seq: 10,
            replica_set: &[0x10, leader_id],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 256 * 1024,
            now: at(base, 100),
        };
        let outcome = tick(inputs);
        // One heartbeat to the leader + one SyncRequest to the
        // leader. Order: heartbeats first (emitted in replica_set
        // iteration), then SyncRequest.
        assert_eq!(outcome.outbound.len(), 2);
        let sync_req = outcome.outbound.iter().find_map(|m| match m {
            OutboundMessage::SyncRequest { target, msg } => Some((*target, msg.clone())),
            _ => None,
        });
        let (target, msg) = sync_req.expect("expected one SyncRequest");
        assert_eq!(target, leader_id);
        assert_eq!(msg.since_seq, 10);
        assert_eq!(msg.chunk_max, 256 * 1024);
        // Verify channel_id matches.
        assert_eq!(msg.channel_id, channel_id_for("test/lag"));
        // No transition — fresh leader, no silence.
        assert!(outcome.transition.is_none());
    }

    #[test]
    fn replica_caught_up_emits_no_sync_request() {
        let leader_id = 0x42;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        tracker.record_heartbeat(leader_id, ReplicaRole::Leader, 100, base);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/caught_up"),
            tail_seq: 100, // exactly the leader's tail
            replica_set: &[0x10, leader_id],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 256 * 1024,
            now: at(base, 100),
        };
        let outcome = tick(inputs);
        // Only the heartbeat — no SyncRequest, no transition.
        assert_eq!(outcome.outbound.len(), 1);
        assert!(matches!(
            outcome.outbound[0],
            OutboundMessage::Heartbeat { .. }
        ));
    }

    #[test]
    fn replica_with_no_believed_leader_emits_no_sync_request() {
        // No leader heartbeat ever seen → believed_leader is None.
        let tracker = empty_tracker();
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/no_leader"),
            tail_seq: 0,
            replica_set: &[0x10, 0x42],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 256 * 1024,
            now: t0(),
        };
        let outcome = tick(inputs);
        // Heartbeat to peer 0x42, but no SyncRequest (no leader
        // to ask).
        assert!(outcome
            .outbound
            .iter()
            .all(|m| matches!(m, OutboundMessage::Heartbeat { .. })));
    }

    #[test]
    fn leader_does_not_emit_sync_request_even_if_peer_advertises_higher_tail() {
        // Defensive: a leader that observes a peer (would-be-
        // replica) advertising a higher tail than its own does
        // NOT issue a SyncRequest. The leader is the single
        // writer; "behind" doesn't apply to it.
        let peer = 0x20;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        // A peer announces it has a tail_seq above the leader's.
        // Pathological but representable on the wire.
        tracker.record_heartbeat(peer, ReplicaRole::Replica, 999, base);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Leader,
            channel_id: channel_id_for("test/leader_no_request"),
            tail_seq: 0,
            replica_set: &[0x10, peer],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 256 * 1024,
            now: at(base, 100),
        };
        let outcome = tick(inputs);
        assert!(outcome
            .outbound
            .iter()
            .all(|m| matches!(m, OutboundMessage::Heartbeat { .. })));
    }

    #[test]
    fn replica_with_silent_leader_skips_sync_request_when_electing() {
        // Coverage of the "skip lag emission during election"
        // path: when tick decides to enter Candidate due to
        // missed heartbeats, the same tick does NOT emit a
        // SyncRequest. The believed leader is stale — issuing
        // would race the election transition.
        let leader_id = 0x42;
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        tracker.record_heartbeat(leader_id, ReplicaRole::Leader, 100, base);
        // Advance past the 3 × 500ms silence threshold.
        let now = at(base, 2_000);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/skip_during_election"),
            tail_seq: 10, // significantly behind
            replica_set: &[0x10, leader_id],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 256 * 1024,
            now,
        };
        let outcome = tick(inputs);
        // Heartbeat is still emitted (Replica heartbeats regardless),
        // but no SyncRequest.
        assert!(outcome
            .outbound
            .iter()
            .all(|m| matches!(m, OutboundMessage::Heartbeat { .. })));
        assert_eq!(
            outcome.transition,
            Some(PendingTransition {
                target: ReplicaRole::Candidate,
                signal: TransitionSignal::MissedHeartbeats,
            }),
        );
    }

    /// R-15: a misrouted self-as-leader heartbeat in the tracker
    /// must NOT produce a SyncRequest addressed to self. The
    /// production filter at the tick layer drops the case
    /// defensively even if the tracker is somehow polluted.
    #[test]
    fn replica_with_self_as_believed_leader_emits_no_sync_request() {
        let mut tracker = HeartbeatTracker::new(500);
        let base = t0();
        // Pathological setup: record a heartbeat as if self
        // were the leader (e.g. test loopback misroute).
        tracker.record_heartbeat(0x10, ReplicaRole::Leader, 99, base);
        let inputs = TickInputs {
            self_node_id: 0x10,
            current_role: ReplicaRole::Replica,
            channel_id: channel_id_for("test/self_loop"),
            tail_seq: 0,
            replica_set: &[0x10, 0x42],
            tracker: &tracker,
            wall_clock_ms: 0,
            chunk_max_bytes: 256 * 1024,
            now: at(base, 100),
        };
        let outcome = tick(inputs);
        // No SyncRequest addressed to anyone; the believed
        // leader filter rejected self.
        assert!(
            outcome
                .outbound
                .iter()
                .all(|m| !matches!(m, OutboundMessage::SyncRequest { .. })),
            "self-as-leader must not produce a SyncRequest"
        );
    }
}
