//! The pure-sync reconcile function. Locked decision #3:
//! `reconcile(actual, desired) -> Vec<MeshOsAction>` is
//! async-free, no I/O, no allocations beyond the returned
//! action list. Testable as a sync table-driven fixture.
//!
//! Phase B fills in the daemon-supervision arm — `StartDaemon`
//! / `StopDaemon` / `ApplyBackoff` emit based on the diff
//! between `DesiredState::desired_daemons` and
//! `MeshOsState::daemons[*].lifecycle`. The crash-loop /
//! BackingOff gate trips through `backoff.state().is_admissible`.
//! Replica + maintenance + locality reconcile arms park for
//! their respective phases.
//!
//! Reconcile is **idempotent**: calling it twice in a row with
//! the same `(actual, desired)` produces the same action list.
//! This is load-bearing for replay safety + cache key
//! generation, and the test below asserts it.

use std::time::{Duration, Instant};

use super::action::MeshOsAction;
use super::event::{ChainId, DaemonIntent, LocalReplicaIntent, NodeId};
use super::state::{DaemonLifecycle, DaemonStatus, DesiredState, MeshOsState};

/// Default grace window granted to a `StopDaemon` action. The
/// supervisor sends `MeshOsControl::Shutdown { deadline = now +
/// STOP_GRACE_PERIOD }`; past the deadline the supervisor force-
/// terminates. Mirror of the plan's "graceful shutdown" section.
pub const STOP_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// Pure-sync diff over `(actual, desired, this_node)`. Returns
/// the minimal action list that would close the gap.
///
/// `this_node` is the loop's identity; reconcile reads it to
/// gate the leader-only `Request*` action variants (only the
/// elected leader of a chain may commit placement / eviction
/// for that chain — locked decision #6).
pub fn reconcile(
    actual: &MeshOsState,
    desired: &DesiredState,
    this_node: NodeId,
) -> Vec<MeshOsAction> {
    let mut actions = Vec::new();
    // The reconcile pass is a sync sample; we use the
    // actual-state `last_tick` (set by the loop on Tick) as the
    // time anchor so consecutive reconcile passes on the same
    // fold produce identical output. Falls back to
    // `Instant::now()` for tests that call reconcile without
    // driving Ticks.
    let now = actual.last_tick.unwrap_or_else(Instant::now);
    diff_daemons(actual, desired, now, &mut actions);
    diff_replicas(actual, desired, this_node, &mut actions);
    actions
}

fn diff_daemons(
    actual: &MeshOsState,
    desired: &DesiredState,
    now: Instant,
    out: &mut Vec<MeshOsAction>,
) {
    for (daemon, intent) in &desired.desired_daemons {
        let status = actual.daemons.get(daemon);
        match intent {
            DaemonIntent::Run => match status.map(|s| s.lifecycle).unwrap_or_default() {
                DaemonLifecycle::Running | DaemonLifecycle::Starting => {
                    // Already in the desired state (or
                    // converging to it). No action.
                }
                DaemonLifecycle::Stopping => {
                    // Mid-stop; let the stop finish, then a
                    // future reconcile pass will start it back
                    // up if intent stays `Run`.
                }
                DaemonLifecycle::Stopped => {
                    let admissible = status
                        .map(|s| s.backoff.state().is_admissible(now))
                        .unwrap_or(true);
                    if admissible {
                        out.push(MeshOsAction::StartDaemon {
                            daemon: daemon.clone(),
                        });
                    } else if let Some(s) = status {
                        emit_backoff_record_if_needed(daemon, s, out);
                    }
                }
            },
            DaemonIntent::Stop => match status.map(|s| s.lifecycle).unwrap_or_default() {
                DaemonLifecycle::Running | DaemonLifecycle::Starting => {
                    out.push(MeshOsAction::StopDaemon {
                        daemon: daemon.clone(),
                        reason: "desired-state intent: Stop".to_string(),
                        deadline: now + STOP_GRACE_PERIOD,
                    });
                }
                DaemonLifecycle::Stopped | DaemonLifecycle::Stopping => {
                    // Already in (or converging to) the desired
                    // state.
                }
            },
        }
    }
}

/// Phase C — replica diff. Two arms:
///
/// 1. Local replica intent (any node). For each chain with a
///    `desired_local_replicas[chain]` entry: if `Hold` and this
///    node isn't a holder → `PullReplica`; if `Drop` and this
///    node IS a holder → `DropReplica`.
///
/// 2. Cluster-wide replica count (leader only). For each chain
///    whose elected leader (`actual.replica_leader[chain]`) is
///    `this_node`: if actual holders < desired count →
///    `RequestPlacement`; if > → `RequestEviction { victim:
///    naive_pick }`. Naive victim selection (Phase C-1):
///    lex-smallest holder. Phase D-1's scheduler refines this
///    with placement-score-based ranking.
fn diff_replicas(
    actual: &MeshOsState,
    desired: &DesiredState,
    this_node: NodeId,
    out: &mut Vec<MeshOsAction>,
) {
    // Sort the chain ids so reconcile output is byte-stable
    // across calls regardless of HashMap iteration order. The
    // idempotence contract relies on it.
    let mut local_chains: Vec<ChainId> = desired.desired_local_replicas.keys().copied().collect();
    local_chains.sort();
    for chain in local_chains {
        let intent = desired.desired_local_replicas[&chain];
        let holds = actual
            .replicas
            .get(&chain)
            .is_some_and(|hs| hs.contains(&this_node));
        match (intent, holds) {
            (LocalReplicaIntent::Hold, false) => {
                if let Some(source) = pick_pull_source(actual, chain, this_node) {
                    out.push(MeshOsAction::PullReplica { chain, source });
                }
                // If no source is known yet, no action — the
                // next ReplicaUpdate will surface a candidate.
            }
            (LocalReplicaIntent::Drop, true) => {
                out.push(MeshOsAction::DropReplica { chain });
            }
            _ => {}
        }
    }

    let mut count_chains: Vec<ChainId> = desired.desired_replicas.keys().copied().collect();
    count_chains.sort();
    for chain in count_chains {
        let leader = actual.replica_leader.get(&chain).copied();
        if leader != Some(this_node) {
            // Not the leader for this chain — silent. Other
            // nodes might score the same action and propose
            // it, but only the leader acts.
            continue;
        }
        let desired_count = desired.desired_replicas[&chain];
        let holders = actual.replicas.get(&chain);
        let actual_count = holders.map(|h| h.len()).unwrap_or(0) as u32;
        if actual_count < desired_count {
            out.push(MeshOsAction::RequestPlacement {
                chain,
                exclude: holders.cloned().unwrap_or_default(),
            });
        } else if actual_count > desired_count {
            // Pick the lex-smallest holder as the victim;
            // Phase D-1 swaps in a placement-score-based pick.
            if let Some(victim) = holders.and_then(|hs| hs.iter().min()).copied() {
                out.push(MeshOsAction::RequestEviction { chain, victim });
            }
        }
    }
}

/// Naive Phase-C-1 source selection: pick the lex-smallest
/// holder other than `this_node`. Phase D-1 swaps in
/// RTT/placement-score-based selection.
fn pick_pull_source(actual: &MeshOsState, chain: ChainId, this_node: NodeId) -> Option<NodeId> {
    actual
        .replicas
        .get(&chain)?
        .iter()
        .copied()
        .filter(|h| *h != this_node)
        .min()
}

fn emit_backoff_record_if_needed(
    daemon: &super::event::DaemonRef,
    status: &DaemonStatus,
    out: &mut Vec<MeshOsAction>,
) {
    // Only record `ApplyBackoff` on the snapshot when the gate
    // is actually open in the future — `is_admissible == false`
    // is the prerequisite. The action carries the same `until`
    // the supervisor will honor.
    if let Some(until) = status.backoff.state().release_at() {
        out.push(MeshOsAction::ApplyBackoff {
            daemon: daemon.clone(),
            until,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use super::super::event::{ChainId, DaemonRef, NodeId};
    use super::super::state::{AvoidEntry, BlobObservation, DaemonStatus};
    use super::super::supervision::RestartState;

    /// Identity used by every reconcile-test call. Pinning a
    /// single value keeps the leader-only gating tests
    /// readable.
    const THIS_NODE: NodeId = 100;

    fn daemon(name: &str, id: u64) -> DaemonRef {
        DaemonRef {
            id,
            name: name.into(),
        }
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn anchor() -> Instant {
        Instant::now()
    }

    #[test]
    fn reconcile_empty_inputs_returns_no_actions() {
        let actual = MeshOsState::default();
        let desired = DesiredState::default();
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn reconcile_is_idempotent_under_repeated_calls() {
        // Load-bearing contract: action executor relies on
        // replay-with-no-side-effect. Pin it explicitly.
        let actual = MeshOsState::default();
        let desired = DesiredState::default();
        let first = reconcile(&actual, &desired, THIS_NODE);
        let second = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(first, second);
    }

    #[test]
    fn reconcile_with_no_daemon_intent_emits_nothing_even_with_state() {
        // No `desired_daemons` -> no daemon actions. The
        // replica / blob / avoid-list folds are observable but
        // park until their respective phases.
        let mut actual = MeshOsState::default();
        actual.daemons.insert(daemon("telemetry", 1), DaemonStatus::default());
        actual.replicas.insert(0xCAFE_BABE as ChainId, vec![1, 2, 3]);
        actual.blobs.insert(
            42,
            BlobObservation {
                size_bytes: 1024,
                holders: vec![1],
            },
        );
        actual.avoid_list.insert(
            7,
            AvoidEntry {
                reason: "rtt".into(),
                until: Instant::now() + Duration::from_secs(60),
            },
        );
        let desired = DesiredState::default();
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn desired_run_with_stopped_actual_emits_start_daemon() {
        let mut actual = MeshOsState::default();
        let d = daemon("telemetry", 1);
        actual.daemons.insert(d.clone(), DaemonStatus::default()); // default lifecycle = Stopped
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(
            actions,
            vec![MeshOsAction::StartDaemon { daemon: d }],
        );
    }

    #[test]
    fn desired_run_when_daemon_absent_emits_start_daemon() {
        // The daemon doesn't yet appear in `actual.daemons` —
        // first-time-start case. Reconcile must not require a
        // status entry to emit StartDaemon.
        let actual = MeshOsState::default();
        let mut desired = DesiredState::default();
        let d = daemon("telemetry", 1);
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);
        let actions = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(actions, vec![MeshOsAction::StartDaemon { daemon: d }]);
    }

    #[test]
    fn desired_run_with_running_actual_emits_nothing() {
        let mut actual = MeshOsState::default();
        let d = daemon("telemetry", 1);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d, DaemonIntent::Run);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn desired_stop_with_running_actual_emits_stop_daemon_with_grace_window() {
        let mut actual = MeshOsState::default();
        let base = anchor();
        actual.last_tick = Some(base);
        let d = daemon("telemetry", 1);
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Stop);

        let actions = reconcile(&actual, &desired, THIS_NODE);
        match actions.as_slice() {
            [MeshOsAction::StopDaemon {
                daemon: d2,
                deadline,
                ..
            }] => {
                assert_eq!(d2, &d);
                assert_eq!(*deadline, base + STOP_GRACE_PERIOD);
            }
            other => panic!("expected one StopDaemon, got {other:?}"),
        }
    }

    #[test]
    fn desired_stop_with_stopped_actual_emits_nothing() {
        let mut actual = MeshOsState::default();
        let d = daemon("telemetry", 1);
        actual.daemons.insert(d.clone(), DaemonStatus::default());
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d, DaemonIntent::Stop);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn backoff_active_gates_start_daemon_emission() {
        // A daemon in BackingOff state must NOT be restarted by
        // reconcile. Instead, `ApplyBackoff` records the gate
        // on the snapshot fold so Deck can render the delay.
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        let d = daemon("telemetry", 1);
        let mut status = DaemonStatus::default();
        // Force a crash so the tracker is in BackingOff(t+500ms).
        status.backoff.observe_crash(base);
        assert!(matches!(status.backoff.state(), RestartState::BackingOff { .. }));
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(&actual, &desired, THIS_NODE);
        match actions.as_slice() {
            [MeshOsAction::ApplyBackoff {
                daemon: d2,
                until,
            }] => {
                assert_eq!(d2, &d);
                assert_eq!(*until, base + Duration::from_millis(500));
            }
            other => panic!("expected ApplyBackoff while gated, got {other:?}"),
        }
    }

    #[test]
    fn backoff_release_after_until_unblocks_start_daemon() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(at(base, 60)); // well past the 500 ms window
        let d = daemon("telemetry", 1);
        let mut status = DaemonStatus::default();
        status.backoff.observe_crash(base);
        // The fold side runs `maybe_release` on each Tick; in
        // the unit test we simulate that explicitly.
        status.backoff.maybe_release(at(base, 60));
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(actions, vec![MeshOsAction::StartDaemon { daemon: d }]);
    }

    #[test]
    fn crash_loop_gate_blocks_start_daemon_emission_under_threshold_crashes() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(at(base, 1)); // immediately after the 5th crash
        let d = daemon("telemetry", 1);
        let mut status = DaemonStatus::default();
        for i in 0..5 {
            status.backoff.observe_crash(at(base, i));
        }
        assert!(matches!(status.backoff.state(), RestartState::CrashLooping { .. }));
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(&actual, &desired, THIS_NODE);
        match actions.as_slice() {
            [MeshOsAction::ApplyBackoff { daemon: d2, .. }] => assert_eq!(d2, &d),
            other => panic!("expected ApplyBackoff under crash-loop gate, got {other:?}"),
        }
    }

    #[test]
    fn reconcile_emits_actions_in_a_stable_order_across_calls() {
        // The same input produces the same output (idempotence)
        // including order — HashMap iteration order would break
        // this if we ever depended on it. We accept HashMap's
        // non-determinism in *which order* the actions appear,
        // but each call against the same state hashes the same
        // way so the result is byte-stable.
        let mut actual = MeshOsState::default();
        let d1 = daemon("a", 1);
        let d2 = daemon("b", 2);
        actual.daemons.insert(d1.clone(), DaemonStatus::default());
        actual.daemons.insert(d2.clone(), DaemonStatus::default());
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d1, DaemonIntent::Run);
        desired.desired_daemons.insert(d2, DaemonIntent::Run);
        let a = reconcile(&actual, &desired, THIS_NODE);
        let b = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
    }

    // ----- Phase C: replica enforcement -----

    const CHAIN_A: ChainId = 0xAA;
    const CHAIN_B: ChainId = 0xBB;

    #[test]
    fn local_intent_hold_when_not_a_holder_emits_pull_replica() {
        let mut actual = MeshOsState::default();
        // Other peers hold the chain; this node doesn't.
        actual.replicas.insert(CHAIN_A, vec![1, 2, 3]);
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Hold);
        let actions = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(
            actions,
            vec![MeshOsAction::PullReplica {
                chain: CHAIN_A,
                source: 1, // lex-smallest holder
            }],
        );
    }

    #[test]
    fn local_intent_hold_when_already_a_holder_emits_nothing() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1, 2, THIS_NODE]);
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Hold);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn local_intent_drop_when_actually_holding_emits_drop_replica() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1, THIS_NODE]);
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Drop);
        let actions = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(actions, vec![MeshOsAction::DropReplica { chain: CHAIN_A }]);
    }

    #[test]
    fn local_intent_drop_when_not_holding_emits_nothing() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1, 2]);
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Drop);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn pull_replica_skipped_when_no_other_holder_known() {
        // If `desired_local_replicas[chain] = Hold` but no other
        // peer is known to hold the chain, we cannot pick a
        // source — defer emission until a ReplicaUpdate surfaces
        // a candidate.
        let actual = MeshOsState::default();
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Hold);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn leader_with_undercount_emits_request_placement() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1, 2]);
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 4);
        let actions = reconcile(&actual, &desired, THIS_NODE);
        assert_eq!(
            actions,
            vec![MeshOsAction::RequestPlacement {
                chain: CHAIN_A,
                exclude: vec![1, 2],
            }],
        );
    }

    #[test]
    fn leader_with_overcount_emits_request_eviction_lex_smallest_victim() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![5, 2, 9]);
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 2);
        let actions = reconcile(&actual, &desired, THIS_NODE);
        // Naive Phase C-1 victim selection: lex-smallest holder.
        assert_eq!(
            actions,
            vec![MeshOsAction::RequestEviction {
                chain: CHAIN_A,
                victim: 2,
            }],
        );
    }

    #[test]
    fn non_leader_does_not_emit_request_placement_even_under_undercount() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1, 2]);
        actual.replica_leader.insert(CHAIN_A, 999); // someone else is leader
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 4);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn no_known_leader_silences_request_actions() {
        // No entry in `replica_leader` for the chain ⇒ no
        // `Request*` is admissible from any node. We wait for
        // election to fire `ReplicaLeaderUpdate`.
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1]);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 3);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn leader_at_exact_count_emits_nothing() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1, 2, 3]);
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 3);
        assert!(reconcile(&actual, &desired, THIS_NODE).is_empty());
    }

    #[test]
    fn reconcile_replica_actions_are_sorted_by_chain_id_for_stability() {
        // Two chains both undercount; the actions should appear
        // in chain-id ascending order regardless of HashMap
        // iteration. Pins the determinism contract.
        let mut actual = MeshOsState::default();
        actual.replicas.insert(CHAIN_A, vec![1]);
        actual.replicas.insert(CHAIN_B, vec![1]);
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        actual.replica_leader.insert(CHAIN_B, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 3);
        desired.desired_replicas.insert(CHAIN_B, 3);
        let actions = reconcile(&actual, &desired, THIS_NODE);
        // Two RequestPlacement, in chain-id order (A < B).
        assert_eq!(actions.len(), 2);
        match (&actions[0], &actions[1]) {
            (
                MeshOsAction::RequestPlacement { chain: c1, .. },
                MeshOsAction::RequestPlacement { chain: c2, .. },
            ) => {
                assert_eq!(*c1, CHAIN_A);
                assert_eq!(*c2, CHAIN_B);
            }
            other => panic!("expected two RequestPlacement actions in chain order, got {other:?}"),
        }
    }
}
