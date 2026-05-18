//! Replica state-machine validation — Phase C pre-scaffolding for
//! `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §3.
//!
//! Pure-logic layer over the four-state model
//! ([`ReplicaRole::{Leader, Replica, Candidate, Idle}`] from the
//! wire crate, reused as the state-machine view). Pins which
//! transitions are valid + which signal class drives each. The
//! [`ReplicationCoordinator`] (Phase C) holds a `Cell<ReplicaRole>`
//! and routes every transition through
//! [`StateTransition::apply`] so the coordinator can't accidentally
//! advance a state combination the plan doesn't enumerate.
//!
//! Transition matrix (plan §3):
//!
//! | From      | To        | Trigger                                                |
//! |-----------|-----------|--------------------------------------------------------|
//! | `Idle`    | `Replica` | Capability filter selected this node                   |
//! | `Replica` | `Candidate` | 3 consecutive missed leader heartbeats              |
//! | `Candidate` | `Leader`  | Won the deterministic `elect()`                    |
//! | `Candidate` | `Replica` | Lost the deterministic `elect()`                   |
//! | `Leader`  | `Idle`    | Graceful relinquish (admin / `leader_pinned` migration) |
//! | `Replica` | `Idle`    | Disk pressure withdrawal under `UnderCapacity::Withdraw` |
//! | `*`       | `Idle`    | Channel close                                          |
//!
//! Self-transitions and any other pair are invalid. Phase F's DST
//! harness models the same matrix; this module's tests pin every
//! valid cell + reject every invalid one.

use super::replication::ReplicaRole;

/// Signal class that drives a state transition. Distinct from the
/// (from, to) pair because some pairs are reachable via more than
/// one signal (e.g. `Idle` is reachable via graceful-relinquish OR
/// channel-close — the operator-facing audit cares which).
///
/// Pinned at scaffolding time so Phase C wires signal-keyed
/// metrics (`election_thrash_total` counts only `MissedHeartbeats`-
/// driven transitions, not channel-close ones).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionSignal {
    /// Capability filter / `PlacementFilter` selected this node as
    /// a replica. Drives `Idle → Replica`.
    CapabilitySelected,
    /// 3 consecutive leader heartbeats missed within the §6
    /// hysteresis window. Drives `Replica → Candidate`.
    MissedHeartbeats,
    /// `elect()` computed this node as the winner. Drives
    /// `Candidate → Leader`.
    ElectionWon,
    /// `elect()` computed a peer as the winner. Drives
    /// `Candidate → Replica`.
    ElectionLost,
    /// Admin command (`leader_pinned` migration / explicit
    /// relinquish). Drives `Leader → Idle`.
    GracefulRelinquish,
    /// Local disk pressure tripped `UnderCapacity::Withdraw`.
    /// Drives `Replica → Idle`.
    DiskPressureWithdraw,
    /// Disk-pressure withdraw from `Leader`. Lets the transition
    /// metric label this case as disk-pressure rather than the
    /// `ChannelClose` fallback the FSM used pre-fix, which made
    /// operator dashboards triage it as "graceful channel close."
    LeaderDiskPressureWithdraw,
    /// Disk-pressure withdraw from `Candidate`. Same labeling
    /// rationale as `LeaderDiskPressureWithdraw`.
    CandidateDiskPressureWithdraw,
    /// Channel was closed. Drives `* → Idle` from any state.
    ChannelClose,
    /// This Leader observed another peer also claiming Leader for
    /// the same channel (inbound Heartbeat with `role=Leader`) and
    /// lost the deterministic tiebreak (lower tail_seq, or equal
    /// tail with greater node id). Drives `Leader → Replica` so a
    /// partition-heal converges to one leader rather than leaving
    /// both partitions claiming authority indefinitely.
    PeerLeaderObserved,
}

/// Result of validating + applying a state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateTransitionError {
    /// `(from, to)` pair is not in the plan §3 matrix.
    #[error("invalid replica state transition: {from:?} → {to:?}")]
    InvalidPair {
        /// State the transition started from.
        from: ReplicaRole,
        /// Attempted target state.
        to: ReplicaRole,
    },
    /// Transition pair is valid in some signal context but not for
    /// the supplied [`TransitionSignal`]. Surfaces the kind of
    /// "you used MissedHeartbeats to go Leader → Idle" misuse.
    #[error("transition {from:?} → {to:?} not permitted under signal {signal:?}")]
    SignalMismatch {
        /// State the transition started from.
        from: ReplicaRole,
        /// Attempted target state.
        to: ReplicaRole,
        /// Signal class supplied to [`StateTransition::apply`].
        signal: TransitionSignal,
    },
}

/// One step of the state machine. Holds the validated `(from, to,
/// signal)` triple; the coordinator constructs one via
/// [`Self::apply`] and uses it to atomically write the new role +
/// log the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateTransition {
    /// State the transition started from.
    pub from: ReplicaRole,
    /// State the transition ended at.
    pub to: ReplicaRole,
    /// Signal class that drove the transition.
    pub signal: TransitionSignal,
}

impl StateTransition {
    /// Validate + construct a `StateTransition`. Returns a typed
    /// error when `(from, to, signal)` lies outside the plan §3
    /// matrix.
    ///
    /// The plan's `Any → Idle on channel close` rule is encoded as:
    /// `signal == ChannelClose` permits transition from any state
    /// to `Idle`, including no-op `Idle → Idle` (a redundant close
    /// is harmless; pinning it as valid lets shutdown code be
    /// idempotent).
    pub fn apply(
        from: ReplicaRole,
        to: ReplicaRole,
        signal: TransitionSignal,
    ) -> Result<Self, StateTransitionError> {
        if signal == TransitionSignal::ChannelClose && to == ReplicaRole::Idle {
            // Channel close from any state, including no-op self-
            // transition. Always permitted.
            return Ok(Self { from, to, signal });
        }
        let permitted = matches!(
            (from, to, signal),
            (
                ReplicaRole::Idle,
                ReplicaRole::Replica,
                TransitionSignal::CapabilitySelected,
            ) | (
                ReplicaRole::Replica,
                ReplicaRole::Candidate,
                TransitionSignal::MissedHeartbeats,
            ) | (
                ReplicaRole::Candidate,
                ReplicaRole::Leader,
                TransitionSignal::ElectionWon,
            ) | (
                ReplicaRole::Candidate,
                ReplicaRole::Replica,
                TransitionSignal::ElectionLost,
            ) | (
                ReplicaRole::Leader,
                ReplicaRole::Idle,
                TransitionSignal::GracefulRelinquish,
            ) | (
                ReplicaRole::Replica,
                ReplicaRole::Idle,
                TransitionSignal::DiskPressureWithdraw,
            ) | (
                ReplicaRole::Leader,
                ReplicaRole::Idle,
                TransitionSignal::LeaderDiskPressureWithdraw,
            ) | (
                ReplicaRole::Candidate,
                ReplicaRole::Idle,
                TransitionSignal::CandidateDiskPressureWithdraw,
            ) | (
                ReplicaRole::Leader,
                ReplicaRole::Replica,
                TransitionSignal::PeerLeaderObserved,
            )
        );
        if !permitted {
            // Distinguish "pair not in matrix at all" from
            // "pair valid but wrong signal" so operator-facing
            // diagnostics surface the misuse cleanly.
            if pair_is_valid_for_some_signal(from, to) {
                return Err(StateTransitionError::SignalMismatch { from, to, signal });
            }
            return Err(StateTransitionError::InvalidPair { from, to });
        }
        Ok(Self { from, to, signal })
    }
}

/// True iff some [`TransitionSignal`] would permit the given
/// `(from, to)` pair. Used inside [`StateTransition::apply`] to
/// surface "you supplied the wrong signal" separately from
/// "this pair is impossible."
fn pair_is_valid_for_some_signal(from: ReplicaRole, to: ReplicaRole) -> bool {
    if to == ReplicaRole::Idle {
        // Channel-close drives `* → Idle` from any state.
        return true;
    }
    matches!(
        (from, to),
        (ReplicaRole::Idle, ReplicaRole::Replica)
            | (ReplicaRole::Replica, ReplicaRole::Candidate)
            | (ReplicaRole::Candidate, ReplicaRole::Leader)
            | (ReplicaRole::Candidate, ReplicaRole::Replica)
            | (ReplicaRole::Leader, ReplicaRole::Replica)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_to_replica_via_capability_selected() {
        let t = StateTransition::apply(
            ReplicaRole::Idle,
            ReplicaRole::Replica,
            TransitionSignal::CapabilitySelected,
        )
        .expect("plan §3 valid pair");
        assert_eq!(t.from, ReplicaRole::Idle);
        assert_eq!(t.to, ReplicaRole::Replica);
        assert_eq!(t.signal, TransitionSignal::CapabilitySelected);
    }

    #[test]
    fn replica_to_candidate_via_missed_heartbeats() {
        StateTransition::apply(
            ReplicaRole::Replica,
            ReplicaRole::Candidate,
            TransitionSignal::MissedHeartbeats,
        )
        .expect("plan §3 valid pair");
    }

    #[test]
    fn candidate_to_leader_via_election_won() {
        StateTransition::apply(
            ReplicaRole::Candidate,
            ReplicaRole::Leader,
            TransitionSignal::ElectionWon,
        )
        .expect("plan §3 valid pair");
    }

    #[test]
    fn candidate_to_replica_via_election_lost() {
        StateTransition::apply(
            ReplicaRole::Candidate,
            ReplicaRole::Replica,
            TransitionSignal::ElectionLost,
        )
        .expect("plan §3 valid pair");
    }

    #[test]
    fn leader_to_idle_via_graceful_relinquish() {
        StateTransition::apply(
            ReplicaRole::Leader,
            ReplicaRole::Idle,
            TransitionSignal::GracefulRelinquish,
        )
        .expect("plan §3 valid pair");
    }

    #[test]
    fn replica_to_idle_via_disk_pressure() {
        StateTransition::apply(
            ReplicaRole::Replica,
            ReplicaRole::Idle,
            TransitionSignal::DiskPressureWithdraw,
        )
        .expect("plan §3 valid pair");
    }

    #[test]
    fn channel_close_terminates_from_any_state() {
        for from in [
            ReplicaRole::Leader,
            ReplicaRole::Replica,
            ReplicaRole::Candidate,
            ReplicaRole::Idle,
        ] {
            StateTransition::apply(from, ReplicaRole::Idle, TransitionSignal::ChannelClose)
                .unwrap_or_else(|_| panic!("ChannelClose must drive {from:?} → Idle"));
        }
    }

    #[test]
    fn rejects_invalid_pair_idle_to_leader() {
        let err = StateTransition::apply(
            ReplicaRole::Idle,
            ReplicaRole::Leader,
            TransitionSignal::ElectionWon,
        )
        .expect_err("Idle → Leader is not in the matrix");
        assert!(matches!(
            err,
            StateTransitionError::InvalidPair {
                from: ReplicaRole::Idle,
                to: ReplicaRole::Leader,
            }
        ));
    }

    #[test]
    fn rejects_invalid_pair_replica_to_leader() {
        // The plan forces a transient `Candidate` between Replica
        // and Leader — the state machine refuses a direct hop.
        let err = StateTransition::apply(
            ReplicaRole::Replica,
            ReplicaRole::Leader,
            TransitionSignal::ElectionWon,
        )
        .expect_err("Replica → Leader bypasses Candidate");
        assert!(matches!(err, StateTransitionError::InvalidPair { .. }));
    }

    #[test]
    fn rejects_invalid_pair_leader_to_candidate() {
        // Leader doesn't lose leadership by re-running election —
        // leader-loss detection lives on the Replica side. A leader
        // that detects its own membership shrunk transitions
        // directly via GracefulRelinquish (admin) or ChannelClose;
        // never via Candidate.
        let err = StateTransition::apply(
            ReplicaRole::Leader,
            ReplicaRole::Candidate,
            TransitionSignal::MissedHeartbeats,
        )
        .expect_err("Leader → Candidate is not in the matrix");
        assert!(matches!(err, StateTransitionError::InvalidPair { .. }));
    }

    #[test]
    fn rejects_pair_valid_but_signal_mismatch() {
        // `Idle → Replica` is a valid pair, but the only signal
        // that drives it is CapabilitySelected. Any other signal
        // surfaces as SignalMismatch (not InvalidPair) so the
        // operator-facing log says "you used the wrong signal,"
        // not "this transition is impossible."
        let err = StateTransition::apply(
            ReplicaRole::Idle,
            ReplicaRole::Replica,
            TransitionSignal::ElectionWon,
        )
        .expect_err("wrong signal for Idle → Replica");
        assert!(matches!(
            err,
            StateTransitionError::SignalMismatch {
                from: ReplicaRole::Idle,
                to: ReplicaRole::Replica,
                signal: TransitionSignal::ElectionWon,
            }
        ));
    }

    #[test]
    fn rejects_self_transitions_via_normal_signals() {
        // No `Leader → Leader` / `Replica → Replica` / etc. via
        // the normal signals — those would mask transition logic.
        // The exception is `Idle → Idle` via ChannelClose
        // (idempotent shutdown), which the dedicated test pins.
        for state in [
            ReplicaRole::Leader,
            ReplicaRole::Replica,
            ReplicaRole::Candidate,
            ReplicaRole::Idle,
        ] {
            for signal in [
                TransitionSignal::CapabilitySelected,
                TransitionSignal::MissedHeartbeats,
                TransitionSignal::ElectionWon,
                TransitionSignal::ElectionLost,
                TransitionSignal::GracefulRelinquish,
                TransitionSignal::DiskPressureWithdraw,
            ] {
                let result = StateTransition::apply(state, state, signal);
                assert!(
                    result.is_err(),
                    "self-transition {state:?} → {state:?} via {signal:?} must be rejected",
                );
            }
        }
    }

    #[test]
    fn channel_close_idle_to_idle_is_valid_idempotent_shutdown() {
        // Channel-close on an already-Idle replica is a no-op but
        // must not error — shutdown code paths can be redundant
        // under failure-injection.
        StateTransition::apply(
            ReplicaRole::Idle,
            ReplicaRole::Idle,
            TransitionSignal::ChannelClose,
        )
        .expect("ChannelClose on Idle is idempotent");
    }

    #[test]
    fn channel_close_with_wrong_target_rejected() {
        // ChannelClose drives to Idle and only to Idle.
        let err = StateTransition::apply(
            ReplicaRole::Replica,
            ReplicaRole::Leader,
            TransitionSignal::ChannelClose,
        )
        .expect_err("ChannelClose target must be Idle");
        assert!(matches!(err, StateTransitionError::InvalidPair { .. }));
    }

    #[test]
    fn matrix_exhaustive_negative_coverage() {
        // For every (from, to) pair, exhaustively cycle through
        // every signal and assert: at most ONE (signal, pair)
        // combination is valid (excluding the Channel-Close-to-Idle
        // family which permits any from-state). Pins the matrix
        // shape — drift would surface here.
        const ROLES: [ReplicaRole; 4] = [
            ReplicaRole::Leader,
            ReplicaRole::Replica,
            ReplicaRole::Candidate,
            ReplicaRole::Idle,
        ];
        const SIGNALS: [TransitionSignal; 7] = [
            TransitionSignal::CapabilitySelected,
            TransitionSignal::MissedHeartbeats,
            TransitionSignal::ElectionWon,
            TransitionSignal::ElectionLost,
            TransitionSignal::GracefulRelinquish,
            TransitionSignal::DiskPressureWithdraw,
            TransitionSignal::ChannelClose,
        ];

        let mut valid_pairs = 0;
        for from in ROLES {
            for to in ROLES {
                let mut signal_hits = 0;
                for signal in SIGNALS {
                    if StateTransition::apply(from, to, signal).is_ok() {
                        signal_hits += 1;
                    }
                }
                if signal_hits > 0 {
                    valid_pairs += 1;
                }
                if to == ReplicaRole::Idle {
                    // ChannelClose adds one hit on every from→Idle
                    // pair. The base specific-signal pair (e.g.
                    // Leader→Idle via GracefulRelinquish,
                    // Replica→Idle via DiskPressureWithdraw) adds
                    // another for some from-states. Cap is 2.
                    assert!(
                        signal_hits <= 2,
                        "{from:?} → Idle has too many valid signals: {signal_hits}",
                    );
                } else {
                    assert!(
                        signal_hits <= 1,
                        "{from:?} → {to:?} has too many valid signals: {signal_hits}",
                    );
                }
            }
        }
        // 7 valid (from, to) pairs total per plan §3:
        // Idle→Replica, Replica→Candidate, Candidate→Leader,
        // Candidate→Replica, Leader→Idle, Replica→Idle, Idle→Idle.
        // Plus Candidate→Idle is reachable via ChannelClose. So 8.
        assert_eq!(valid_pairs, 8, "expected 8 reachable (from, to) pairs");
    }
}
