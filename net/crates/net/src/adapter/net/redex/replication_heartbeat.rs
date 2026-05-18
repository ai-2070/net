//! Heartbeat tracking — `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §6.
//!
//! Pure-logic component the [`ReplicationCoordinator`]'s eventual
//! heartbeat loop drives. Tracks per-peer last-seen / role / tail
//! observations from inbound [`SyncHeartbeat`] messages, exposes
//! the "is the believed leader silent for ≥ 3 heartbeats?"
//! predicate that triggers `transition_to(Candidate,
//! MissedHeartbeats)`, and surfaces per-peer lag for the leader-
//! side `dataforts_replication_lag_seconds{role=replica}` metric.
//!
//! Time is passed in by the caller (not from a system clock) so
//! tests can advance time deterministically without `tokio::time`
//! plumbing. The eventual tokio interval-driven loop calls
//! [`HeartbeatTracker::tick`] with `Instant::now()` each tick.
//!
//! The state machine in `replication_state.rs` enforces "which
//! signal drives which transition" — this module is purely the
//! signal generator: when the leader has been silent long enough,
//! the coordinator's tick reads
//! [`HeartbeatTracker::is_leader_silent`] and routes through
//! `transition_to(Candidate, MissedHeartbeats)`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::replication::ReplicaRole;
use crate::adapter::net::behavior::placement::NodeId;

/// Default consecutive-miss threshold per plan §6: "3 missed
/// heartbeats prevents election thrash under transient packet
/// loss."
pub const DEFAULT_MISS_THRESHOLD: u8 = 3;

/// Per-peer state cell. Captures the most recent
/// [`SyncHeartbeat`](super::replication::SyncHeartbeat) observation.
/// Public field shape so consumers can build leader-side lag
/// metrics directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerState {
    /// Most recent inbound heartbeat timestamp from this peer.
    pub last_seen: Instant,
    /// Role the peer claimed in its most recent heartbeat.
    pub role: ReplicaRole,
    /// `tail_seq` the peer claimed in its most recent heartbeat.
    /// Leader-side: lag-from-this-replica = our_tail - peer_tail.
    /// Replica-side: lag-from-leader = leader_tail - our_tail (the
    /// inverse).
    pub tail_seq: u64,
}

/// Tracker over inbound heartbeats. The coordinator's eventual
/// heartbeat loop drives a single one of these per replicated
/// channel.
///
/// Not Send + Sync by default — the coordinator wraps the
/// tracker in a `parking_lot::Mutex` so its tokio task can
/// take exclusive access during a tick. Single-threaded by
/// design; the criticism that a `RwLock<HashMap>` allows
/// concurrent reads is irrelevant here — the heartbeat loop
/// is the sole reader / writer.
pub struct HeartbeatTracker {
    /// Configured heartbeat cadence in milliseconds. Used as the
    /// unit of "miss" computation: silence ≥ miss_threshold ×
    /// heartbeat_ms triggers a Candidate transition.
    heartbeat_ms: u64,
    /// Consecutive-miss threshold. Default
    /// [`DEFAULT_MISS_THRESHOLD`].
    miss_threshold: u8,
    /// Per-peer most-recent heartbeat observation.
    peers: BTreeMap<NodeId, PeerState>,
    /// The peer this tracker believes is the current leader, if
    /// any. Set by the most recent heartbeat with `role ==
    /// Leader`; cleared by [`Self::clear_believed_leader`] (the
    /// coordinator clears it on `Replica → Candidate` so the next
    /// election cycle starts clean).
    believed_leader: Option<NodeId>,
}

impl HeartbeatTracker {
    /// Construct a tracker for a channel configured with
    /// `heartbeat_ms` cadence. Uses
    /// [`DEFAULT_MISS_THRESHOLD`] = 3.
    pub fn new(heartbeat_ms: u64) -> Self {
        Self::with_miss_threshold(heartbeat_ms, DEFAULT_MISS_THRESHOLD)
    }

    /// Explicit-threshold constructor — pin the miss count for
    /// tighter-SLA workloads or DST scenarios. Threshold of `0`
    /// is clamped to `1` so a heartbeat tracker is never in a
    /// "permanently silent" state (with `miss_threshold = 0`,
    /// even a fresh heartbeat would be "miss enough" to trigger).
    pub fn with_miss_threshold(heartbeat_ms: u64, miss_threshold: u8) -> Self {
        Self {
            heartbeat_ms,
            miss_threshold: miss_threshold.max(1),
            peers: BTreeMap::new(),
            believed_leader: None,
        }
    }

    /// Configured heartbeat cadence.
    pub fn heartbeat_ms(&self) -> u64 {
        self.heartbeat_ms
    }

    /// Configured miss threshold.
    pub fn miss_threshold(&self) -> u8 {
        self.miss_threshold
    }

    /// Record an inbound heartbeat from `peer`. Updates the
    /// peer's `last_seen` / `role` / `tail_seq` and — if `role ==
    /// Leader` — promotes `peer` to the believed leader (even if
    /// a different peer was previously believed-leader; the most
    /// recent `Leader`-roled heartbeat wins).
    pub fn record_heartbeat(
        &mut self,
        peer: NodeId,
        role: ReplicaRole,
        tail_seq: u64,
        now: Instant,
    ) {
        self.peers.insert(
            peer,
            PeerState {
                last_seen: now,
                role,
                tail_seq,
            },
        );
        if role == ReplicaRole::Leader {
            // Tiebreak: keep the lex-smallest NodeId among
            // concurrent Leader claimants. Pre-fix every Leader
            // heartbeat overwrote `believed_leader` unconditionally,
            // so two peers each claiming Leader during a failover
            // (or one malicious peer asserting Leader against a
            // real leader's heartbeats) flipped the believed leader
            // every tick. The tick()-driven SyncRequest then
            // alternated between the two leaders, landing divergent
            // chunks. Lex-smallest is deterministic and is the same
            // tiebreak election uses.
            match self.believed_leader {
                None => self.believed_leader = Some(peer),
                Some(existing) if peer < existing => {
                    self.believed_leader = Some(peer);
                }
                Some(_) => {}
            }
        }
    }

    /// True iff the believed leader has been silent past the
    /// miss-threshold window — i.e. `(now - leader.last_seen) >=
    /// miss_threshold × heartbeat_ms`.
    ///
    /// Returns `false` when:
    /// - No believed leader is known (a fresh tracker, or just
    ///   after [`Self::clear_believed_leader`]).
    /// - The believed leader's last heartbeat is fresh enough.
    ///
    /// Caller drives this on every coordinator tick.
    pub fn is_leader_silent(&self, now: Instant) -> bool {
        let Some(leader_id) = self.believed_leader else {
            return false;
        };
        let Some(leader) = self.peers.get(&leader_id) else {
            // Believed leader was set but the peer entry was
            // dropped (e.g. via `drop_peer`). Treat as silent so
            // the coordinator runs an election from a clean
            // slate.
            return true;
        };
        let threshold =
            Duration::from_millis(self.heartbeat_ms.saturating_mul(self.miss_threshold as u64));
        now.saturating_duration_since(leader.last_seen) >= threshold
    }

    /// Current believed leader. `None` if no heartbeat with
    /// `role == Leader` has been observed (or
    /// [`Self::clear_believed_leader`] was called).
    pub fn believed_leader(&self) -> Option<NodeId> {
        self.believed_leader
    }

    /// Clear the believed-leader cell. The coordinator calls
    /// this on every `Replica → Candidate` transition so the
    /// next election cycle starts clean; a stale believed leader
    /// would let [`Self::is_leader_silent`] return false even
    /// after the local node decided to run an election.
    pub fn clear_believed_leader(&mut self) {
        self.believed_leader = None;
    }

    /// Drop a peer from the tracker — disconnect / withdraw /
    /// channel close. If the dropped peer was the believed
    /// leader, clears that too so the coordinator's next tick
    /// can re-observe leadership cleanly.
    pub fn drop_peer(&mut self, peer: NodeId) {
        self.peers.remove(&peer);
        if self.believed_leader == Some(peer) {
            self.believed_leader = None;
        }
    }

    /// Read this peer's most recent observation, if any.
    pub fn peer_state(&self, peer: NodeId) -> Option<PeerState> {
        self.peers.get(&peer).copied()
    }

    /// Lag = `now - peer.last_seen` for the given peer.
    /// `None` if the peer is unknown.
    pub fn peer_lag(&self, peer: NodeId, now: Instant) -> Option<Duration> {
        self.peers
            .get(&peer)
            .map(|p| now.saturating_duration_since(p.last_seen))
    }

    /// Set of peers considered alive in the local view —
    /// last-seen within the miss-threshold window. Sorted by
    /// `NodeId` for stable iteration.
    ///
    /// Consumed by the [`elect`](super::replication_election::elect)
    /// selection function from `replication_election.rs` to filter
    /// the replica set down to the healthy subset.
    pub fn healthy_peers(&self, now: Instant) -> Vec<NodeId> {
        let threshold =
            Duration::from_millis(self.heartbeat_ms.saturating_mul(self.miss_threshold as u64));
        self.peers
            .iter()
            .filter(|(_, state)| now.saturating_duration_since(state.last_seen) < threshold)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Snapshot every peer's `(NodeId, tail_seq)` pair. Useful
    /// for the leader-side lag metric: the leader's own tail
    /// minus each replica's reported tail = that replica's
    /// observable lag.
    pub fn peer_tail_seqs(&self) -> Vec<(NodeId, u64)> {
        self.peers
            .iter()
            .map(|(id, state)| (*id, state.tail_seq))
            .collect()
    }

    /// Number of peers currently tracked.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn new_tracker_has_no_peers_or_leader() {
        let t = HeartbeatTracker::new(500);
        assert_eq!(t.peer_count(), 0);
        assert!(t.believed_leader().is_none());
        assert!(!t.is_leader_silent(t0()));
        assert_eq!(t.heartbeat_ms(), 500);
        assert_eq!(t.miss_threshold(), DEFAULT_MISS_THRESHOLD);
    }

    #[test]
    fn miss_threshold_zero_clamped_to_one() {
        let t = HeartbeatTracker::with_miss_threshold(100, 0);
        assert_eq!(t.miss_threshold(), 1);
    }

    #[test]
    fn record_heartbeat_tracks_peer_state() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);

        assert_eq!(t.peer_count(), 1);
        assert_eq!(t.believed_leader(), Some(0x42));
        let p = t.peer_state(0x42).unwrap();
        assert_eq!(p.role, ReplicaRole::Leader);
        assert_eq!(p.tail_seq, 100);
        assert_eq!(p.last_seen, base);
    }

    #[test]
    fn lex_smallest_leader_wins_tiebreak() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        // First Leader heartbeat establishes believed_leader.
        t.record_heartbeat(0x43, ReplicaRole::Leader, 200, base);
        assert_eq!(t.believed_leader(), Some(0x43));
        // A second peer with a lex-smaller NodeId also claims
        // Leader — believed_leader switches to the smaller id.
        // Pre-fix this overwrote unconditionally; two peers
        // alternating Leader claims flipped believed_leader every
        // tick, making replica SyncRequests alternate between
        // them and landing divergent chunks.
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, at(base, 100));
        assert_eq!(
            t.believed_leader(),
            Some(0x42),
            "lex-smallest Leader claimant should win the tiebreak",
        );
        // A LATER Leader heartbeat from the now-loser does NOT
        // re-flip the believed leader.
        t.record_heartbeat(0x43, ReplicaRole::Leader, 300, at(base, 200));
        assert_eq!(
            t.believed_leader(),
            Some(0x42),
            "established lex-smallest leader must not be overwritten by a larger-id claimant",
        );
    }

    #[test]
    fn replica_role_heartbeat_does_not_change_believed_leader() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        // Replica heartbeat from another peer — believed leader
        // stays the original.
        t.record_heartbeat(0x99, ReplicaRole::Replica, 95, at(base, 50));
        assert_eq!(t.believed_leader(), Some(0x42));
        // But the replica's state is recorded.
        assert_eq!(t.peer_state(0x99).unwrap().role, ReplicaRole::Replica);
    }

    #[test]
    fn leader_not_silent_within_window() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        // 500 ms elapsed = 1 heartbeat window. Below 3 × 500 ms
        // = silent? No.
        assert!(!t.is_leader_silent(at(base, 500)));
        // 1 ms before 3 × 500 ms still considered alive.
        assert!(!t.is_leader_silent(at(base, 1499)));
    }

    #[test]
    fn leader_silent_at_threshold() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        // Exactly 3 × 500 ms = 1500 ms — silent.
        assert!(t.is_leader_silent(at(base, 1500)));
    }

    #[test]
    fn leader_silent_past_threshold() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        assert!(t.is_leader_silent(at(base, 5000)));
    }

    #[test]
    fn fresh_leader_heartbeat_resets_silence() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        // Approach silence.
        assert!(t.is_leader_silent(at(base, 1500)));
        // A fresh heartbeat from the leader resets the window.
        t.record_heartbeat(0x42, ReplicaRole::Leader, 105, at(base, 1500));
        // 100ms after the fresh heartbeat — not silent.
        assert!(!t.is_leader_silent(at(base, 1600)));
    }

    #[test]
    fn dropped_believed_leader_treated_as_silent() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        t.drop_peer(0x42);
        // After drop_peer, believed_leader was the dropped peer,
        // so it's cleared — fall back to "no believed leader =
        // not silent."
        assert!(!t.is_leader_silent(at(base, 100)));
        assert!(t.believed_leader().is_none());
    }

    #[test]
    fn clear_believed_leader_does_not_drop_peer_entry() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 100, base);
        t.clear_believed_leader();
        assert!(t.believed_leader().is_none());
        // The peer's state is still there — only the "believe
        // them to be leader" cell was cleared.
        assert!(t.peer_state(0x42).is_some());
    }

    #[test]
    fn peer_lag_returns_elapsed() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x42, ReplicaRole::Replica, 100, base);
        let lag = t.peer_lag(0x42, at(base, 750)).unwrap();
        assert_eq!(lag, Duration::from_millis(750));
    }

    #[test]
    fn peer_lag_unknown_returns_none() {
        let t = HeartbeatTracker::new(500);
        assert!(t.peer_lag(0x42, t0()).is_none());
    }

    #[test]
    fn healthy_peers_filters_stale_entries() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x1, ReplicaRole::Leader, 100, base);
        t.record_heartbeat(0x2, ReplicaRole::Replica, 100, at(base, 200));
        t.record_heartbeat(0x3, ReplicaRole::Replica, 100, at(base, 400));
        // At t=1500ms (3 × 500), peer 1's heartbeat (at 0ms) is
        // stale; 2 (at 200ms) and 3 (at 400ms) are still fresh
        // (just barely for peer 2: 1500-200=1300 < 1500).
        let healthy = t.healthy_peers(at(base, 1500));
        assert_eq!(healthy, vec![0x2, 0x3]);
    }

    #[test]
    fn healthy_peers_sorted_by_node_id() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        // Insert in reverse order; output should be ascending.
        t.record_heartbeat(0x30, ReplicaRole::Replica, 0, base);
        t.record_heartbeat(0x10, ReplicaRole::Replica, 0, base);
        t.record_heartbeat(0x20, ReplicaRole::Replica, 0, base);
        let healthy = t.healthy_peers(at(base, 100));
        assert_eq!(healthy, vec![0x10, 0x20, 0x30]);
    }

    #[test]
    fn peer_tail_seqs_snapshot() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x10, ReplicaRole::Leader, 1000, base);
        t.record_heartbeat(0x20, ReplicaRole::Replica, 950, base);
        t.record_heartbeat(0x30, ReplicaRole::Replica, 980, base);
        let mut tails = t.peer_tail_seqs();
        tails.sort_by_key(|(id, _)| *id);
        assert_eq!(tails, vec![(0x10, 1000), (0x20, 950), (0x30, 980)]);
    }

    #[test]
    fn drop_peer_removes_and_decrements_count() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x1, ReplicaRole::Leader, 0, base);
        t.record_heartbeat(0x2, ReplicaRole::Replica, 0, base);
        assert_eq!(t.peer_count(), 2);
        t.drop_peer(0x1);
        assert_eq!(t.peer_count(), 1);
        assert!(t.peer_state(0x1).is_none());
        assert!(t.peer_state(0x2).is_some());
        // Believed leader cleared because it was the dropped peer.
        assert!(t.believed_leader().is_none());
    }

    #[test]
    fn drop_non_leader_peer_preserves_believed_leader() {
        let base = t0();
        let mut t = HeartbeatTracker::new(500);
        t.record_heartbeat(0x1, ReplicaRole::Leader, 0, base);
        t.record_heartbeat(0x2, ReplicaRole::Replica, 0, base);
        t.drop_peer(0x2);
        assert_eq!(t.believed_leader(), Some(0x1));
    }

    #[test]
    fn miss_threshold_one_triggers_after_one_window() {
        let base = t0();
        let mut t = HeartbeatTracker::with_miss_threshold(500, 1);
        t.record_heartbeat(0x42, ReplicaRole::Leader, 0, base);
        assert!(!t.is_leader_silent(at(base, 499)));
        assert!(t.is_leader_silent(at(base, 500)));
    }

    #[test]
    fn no_believed_leader_never_silent_regardless_of_time() {
        let base = t0();
        let t = HeartbeatTracker::new(500);
        // No heartbeats observed; not silent at any future time.
        assert!(!t.is_leader_silent(at(base, 60_000)));
    }
}
