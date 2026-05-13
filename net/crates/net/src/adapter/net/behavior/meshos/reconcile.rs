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
use super::event::DaemonIntent;
use super::state::{DaemonLifecycle, DaemonStatus, DesiredState, MeshOsState};

/// Default grace window granted to a `StopDaemon` action. The
/// supervisor sends `MeshOsControl::Shutdown { deadline = now +
/// STOP_GRACE_PERIOD }`; past the deadline the supervisor force-
/// terminates. Mirror of the plan's "graceful shutdown" section.
pub const STOP_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// Pure-sync diff over `(actual, desired)`. Returns the minimal
/// action list that would close the gap. Phase B handles
/// daemon supervision; Phases C–G layer in their action
/// families.
pub fn reconcile(actual: &MeshOsState, desired: &DesiredState) -> Vec<MeshOsAction> {
    let mut actions = Vec::new();
    // Phase B: daemon supervision diff. The reconcile pass is a
    // sync sample; we use the actual-state `last_tick` (set by
    // the loop on Tick) as the time anchor so consecutive
    // reconcile passes on the same fold produce identical
    // output. Falls back to `Instant::now()` for tests that call
    // reconcile without driving Ticks.
    let now = actual.last_tick.unwrap_or_else(Instant::now);
    diff_daemons(actual, desired, now, &mut actions);
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
    use super::super::event::{ChainId, DaemonRef};
    use super::super::state::{AvoidEntry, BlobObservation, DaemonStatus};
    use super::super::supervision::RestartState;

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
        assert!(reconcile(&actual, &desired).is_empty());
    }

    #[test]
    fn reconcile_is_idempotent_under_repeated_calls() {
        // Load-bearing contract: action executor relies on
        // replay-with-no-side-effect. Pin it explicitly.
        let actual = MeshOsState::default();
        let desired = DesiredState::default();
        let first = reconcile(&actual, &desired);
        let second = reconcile(&actual, &desired);
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
        assert!(reconcile(&actual, &desired).is_empty());
    }

    #[test]
    fn desired_run_with_stopped_actual_emits_start_daemon() {
        let mut actual = MeshOsState::default();
        let d = daemon("telemetry", 1);
        actual.daemons.insert(d.clone(), DaemonStatus::default()); // default lifecycle = Stopped
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(&actual, &desired);
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
        let actions = reconcile(&actual, &desired);
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
        assert!(reconcile(&actual, &desired).is_empty());
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

        let actions = reconcile(&actual, &desired);
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
        assert!(reconcile(&actual, &desired).is_empty());
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

        let actions = reconcile(&actual, &desired);
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

        let actions = reconcile(&actual, &desired);
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

        let actions = reconcile(&actual, &desired);
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
        let a = reconcile(&actual, &desired);
        let b = reconcile(&actual, &desired);
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
    }
}
