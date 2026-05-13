//! The pure-sync reconcile function. Locked decision #3:
//! `reconcile(actual, desired) -> Vec<MeshOsAction>` is
//! async-free, no I/O. Testable as a sync table-driven fixture.
//! The common no-op path allocates only the empty `Vec`; the
//! diff arms allocate the returned actions plus intermediate
//! `Vec<ChainId>`s used to sort iteration order
//! deterministically. Pre-allocated string constants for the
//! few reasons that ride into actions would shave the last
//! bits of allocation but aren't on any hot path today.
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

use super::action::{MaintenanceTransition, MeshOsAction};
use super::config::{LocalityConfig, MaintenanceConfig};
use super::event::{ChainId, DaemonIntent, LocalReplicaIntent, NodeId};
use super::maintenance::MaintenanceState;
use super::scheduler::{PlacementScorer, SchedulerConfig};
use super::state::{DaemonLifecycle, DaemonStatus, DesiredState, MeshOsState};

/// Default grace window granted to a `StopDaemon` action. The
/// supervisor sends `MeshOsControl::Shutdown { deadline = now +
/// STOP_GRACE_PERIOD }`; past the deadline the supervisor force-
/// terminates. Mirror of the plan's "graceful shutdown" section.
pub const STOP_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// Pure-sync diff over `(actual, desired, this_node, locality)`.
/// Returns the minimal action list that would close the gap.
///
/// `this_node` is the loop's identity; reconcile reads it to
/// gate the leader-only `Request*` action variants (only the
/// elected leader of a chain may commit placement / eviction
/// for that chain — locked decision #6). `locality` carries the
/// Phase D tunables (degraded-RTT threshold + `MarkAvoid` TTL).
pub fn reconcile(
    actual: &MeshOsState,
    desired: &DesiredState,
    this_node: NodeId,
    locality: &LocalityConfig,
    maintenance: &MaintenanceConfig,
    scheduler: &SchedulerConfig,
    scorer: Option<&dyn PlacementScorer>,
) -> Vec<MeshOsAction> {
    let mut actions = Vec::new();
    // The reconcile pass is a sync sample; we use the
    // actual-state `last_tick` (set by the loop on Tick) as the
    // time anchor so consecutive reconcile passes on the same
    // fold produce identical output. Falls back to
    // `Instant::now()` for tests that call reconcile without
    // driving Ticks.
    let now = actual.last_tick.unwrap_or_else(Instant::now);
    // Track which chains already saw a `RequestEviction` this
    // tick so the Phase C count-driven arm and the Phase D-1
    // scheduler arm don't both emit (possibly with different
    // victims) for the same chain in the same pass.
    let mut evicted_this_tick: std::collections::HashSet<ChainId> =
        std::collections::HashSet::new();
    diff_daemons(actual, desired, now, &mut actions);
    diff_replicas(
        actual,
        desired,
        this_node,
        &mut evicted_this_tick,
        &mut actions,
    );
    diff_locality(actual, now, locality, &mut actions);
    diff_maintenance(actual, this_node, now, maintenance, &mut actions);
    diff_scheduler(
        actual,
        this_node,
        scheduler,
        scorer,
        now,
        &evicted_this_tick,
        &mut actions,
    );
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
                        emit_backoff_record_if_needed(
                            daemon,
                            s,
                            actual.applied_backoffs.get(daemon).copied(),
                            out,
                        );
                    }
                }
            },
            DaemonIntent::Stop => match status.map(|s| s.lifecycle).unwrap_or_default() {
                DaemonLifecycle::Running | DaemonLifecycle::Starting => {
                    out.push(MeshOsAction::StopDaemon {
                        daemon: daemon.clone(),
                        reason: "desired-state intent: Stop".to_string(),
                        deadline: now.checked_add(STOP_GRACE_PERIOD).unwrap_or(now),
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
    evicted: &mut std::collections::HashSet<ChainId>,
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
                exclude: holders
                    .map(|hs| hs.iter().copied().collect())
                    .unwrap_or_default(),
            });
        } else if actual_count > desired_count {
            // Pick the lex-smallest holder as the victim;
            // BTreeSet's iter is sorted ascending, so `.next()`
            // is the min. Phase D-1 swaps in a placement-score-
            // based pick.
            if let Some(victim) = holders.and_then(|hs| hs.iter().next()).copied() {
                out.push(MeshOsAction::RequestEviction { chain, victim });
                evicted.insert(chain);
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

/// Phase D — locality diff. For each peer whose latest RTT
/// exceeds `degraded_rtt_threshold` AND who isn't already on
/// the avoid list, emit `MarkAvoid { peer, reason, ttl }`.
///
/// The "already on the avoid list" gate is what keeps reconcile
/// idempotent — a peer with a persistently bad RTT produces one
/// `MarkAvoid` action, not one per tick.
fn diff_locality(
    actual: &MeshOsState,
    now: Instant,
    locality: &LocalityConfig,
    out: &mut Vec<MeshOsAction>,
) {
    let _ = now;
    // Sort the peer list so action emission is byte-stable
    // across calls regardless of HashMap iteration order.
    let mut peers: Vec<(NodeId, Duration)> = actual
        .rtt
        .iter()
        .filter(|(_, rtt)| **rtt > locality.degraded_rtt_threshold)
        .map(|(peer, rtt)| (*peer, *rtt))
        .collect();
    peers.sort_by_key(|(peer, _)| *peer);
    for (peer, rtt) in peers {
        if actual.avoid_list.contains_key(&peer) {
            // Already avoided — reconcile is idempotent, no
            // duplicate emission.
            continue;
        }
        out.push(MeshOsAction::MarkAvoid {
            peer,
            reason: format!("rtt-degradation: {} ms", rtt.as_millis()),
            ttl: locality.avoid_ttl,
        });
    }
}

/// Phase E — maintenance state machine forward transitions.
/// Each branch is a sync condition check; when the condition
/// holds, emit a `CommitMaintenanceTransition` whose target is
/// the next state. The action executor commits to the admin
/// chain; the chain replay surfaces a
/// `MaintenanceTransitionObserved` that the fold consumes to
/// advance `local_maintenance`.
///
/// Idempotent: emitting the same transition twice is harmless
/// (the chain commit is idempotent), and reconcile re-evaluates
/// the condition each tick so a flapping condition produces one
/// pending transition, not many.
fn diff_maintenance(
    actual: &MeshOsState,
    this_node: NodeId,
    now: Instant,
    config: &MaintenanceConfig,
    out: &mut Vec<MeshOsAction>,
) {
    let target = match &actual.local_maintenance {
        MaintenanceState::Active => None,
        MaintenanceState::EnteringMaintenance { deadline, .. } => {
            if all_replicas_drained_locally(actual, this_node) && all_daemons_stopped(actual) {
                Some(MaintenanceTransition::Maintenance)
            } else if deadline.map(|d| now >= d).unwrap_or(false) {
                Some(MaintenanceTransition::DrainFailed)
            } else {
                None
            }
        }
        MaintenanceState::Maintenance { .. } => None,
        MaintenanceState::ExitingMaintenance { .. } => {
            if all_daemons_healthy(actual) {
                Some(MaintenanceTransition::Recovery)
            } else {
                None
            }
        }
        MaintenanceState::DrainFailed { .. } => None,
        MaintenanceState::Recovery { since } => {
            if now.saturating_duration_since(*since) >= config.recovery_ramp_window {
                Some(MaintenanceTransition::Active)
            } else {
                None
            }
        }
    };
    if let Some(target) = target {
        out.push(MeshOsAction::CommitMaintenanceTransition {
            node: this_node,
            target,
        });
    }
}

/// Phase D-1 — continuous-rebalance scoring arm.
///
/// For each chain where this node is the elected leader:
///
/// 1. Score every current holder via `scorer.score(chain, h)`.
/// 2. Find the worst holder (lowest score).
/// 3. If the worst score is below `score_floor` AND a better
///    alternative exists (alt_score - worst_score >
///    hysteresis_gap) AND the chain isn't on cooldown, emit
///    `RequestEviction { chain, victim: worst }`.
///
/// Phase C's existing diff observes the holder-count drop on
/// the next tick and emits `RequestPlacement` to refill — the
/// two-stage shape that keeps Phase D-1 from needing a new
/// action variant.
///
/// Idempotent: the cooldown map (`actual.last_rebalance`) is
/// recorded by the fold side; this function never mutates state.
/// A reconcile re-run with the same input produces the same
/// eviction emission (or none, if the cooldown blocks it).
fn diff_scheduler(
    actual: &MeshOsState,
    this_node: NodeId,
    config: &SchedulerConfig,
    scorer: Option<&dyn PlacementScorer>,
    now: Instant,
    evicted_this_tick: &std::collections::HashSet<ChainId>,
    out: &mut Vec<MeshOsAction>,
) {
    let Some(scorer) = scorer else {
        // No scorer installed — the scheduler arm is a no-op.
        return;
    };

    // Sort chain ids for byte-stable action ordering across
    // reconcile calls regardless of HashMap iteration.
    let mut chains: Vec<ChainId> = actual.replicas.keys().copied().collect();
    chains.sort();

    for chain in chains {
        // Skip chains the Phase C count-driven arm already
        // evicted from this tick — one eviction per chain per
        // tick is the safety budget.
        if evicted_this_tick.contains(&chain) {
            continue;
        }
        let leader = actual.replica_leader.get(&chain).copied();
        if leader != Some(this_node) {
            continue;
        }
        // Cooldown gate.
        if let Some(last) = actual.last_rebalance.get(&chain) {
            if now.saturating_duration_since(*last) < config.cooldown {
                continue;
            }
        }
        let holders: Vec<NodeId> = match actual.replicas.get(&chain) {
            Some(h) if !h.is_empty() => h.iter().copied().collect(),
            _ => continue,
        };
        // Pick the lowest-scoring holder. Use total_cmp on f32
        // so NaN doesn't surprise us; treat None as "no score
        // → skip this holder."
        let mut worst: Option<(NodeId, f32)> = None;
        for &h in &holders {
            let Some(score) = scorer.score(chain, h) else {
                continue;
            };
            worst = match worst {
                None => Some((h, score)),
                Some((_, ws)) if score < ws => Some((h, score)),
                _ => worst,
            };
        }
        let Some((victim, victim_score)) = worst else {
            continue;
        };
        if victim_score >= config.score_floor {
            // No holder is below the floor — leave the chain
            // alone.
            continue;
        }
        // Check there's a better alternative.
        let Some((_alt_node, alt_score)) = scorer.best_alternative(chain, &holders) else {
            continue;
        };
        if alt_score - victim_score <= config.hysteresis_gap {
            continue;
        }
        out.push(MeshOsAction::RequestEviction { chain, victim });
    }
}

fn all_replicas_drained_locally(actual: &MeshOsState, this_node: NodeId) -> bool {
    actual
        .replicas
        .values()
        .all(|holders| !holders.contains(&this_node))
}

fn all_daemons_stopped(actual: &MeshOsState) -> bool {
    actual
        .daemons
        .values()
        .all(|s| matches!(s.lifecycle, DaemonLifecycle::Stopped))
}

fn all_daemons_healthy(actual: &MeshOsState) -> bool {
    use super::event::DaemonHealth;
    actual.daemons.values().all(|s| {
        matches!(s.lifecycle, DaemonLifecycle::Running)
            && matches!(s.health, Some(DaemonHealth::Healthy) | None)
    })
}

fn emit_backoff_record_if_needed(
    daemon: &super::event::DaemonRef,
    status: &DaemonStatus,
    last_applied: Option<Instant>,
    out: &mut Vec<MeshOsAction>,
) {
    // Only record `ApplyBackoff` on the snapshot when the gate
    // is actually open in the future — `is_admissible == false`
    // is the prerequisite. The action carries the same `until`
    // the supervisor will honor. Suppress re-emission when the
    // loop has already committed the same `until`: a daemon
    // parked in `BackingOff` would otherwise generate a fresh
    // action every tick.
    if let Some(until) = status.backoff.state().release_at() {
        if last_applied == Some(until) {
            return;
        }
        out.push(MeshOsAction::ApplyBackoff {
            daemon: daemon.clone(),
            until,
        });
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use std::time::Duration;

    use super::super::event::{ChainId, DaemonRef, NodeId};
    use super::super::state::{AvoidEntry, BlobObservation, DaemonStatus};
    use super::super::supervision::RestartState;
    use super::*;

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
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn reconcile_is_idempotent_under_repeated_calls() {
        // Load-bearing contract: action executor relies on
        // replay-with-no-side-effect. Pin it explicitly.
        let actual = MeshOsState::default();
        let desired = DesiredState::default();
        let first = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        let second = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(first, second);
    }

    #[test]
    fn reconcile_with_no_daemon_intent_emits_nothing_even_with_state() {
        // No `desired_daemons` -> no daemon actions. The
        // replica / blob / avoid-list folds are observable but
        // park until their respective phases.
        let mut actual = MeshOsState::default();
        actual
            .daemons
            .insert(daemon("telemetry", 1), DaemonStatus::default());
        actual.replicas.insert(
            0xCAFE_BABE as ChainId,
            ::std::collections::BTreeSet::from([1, 2, 3]),
        );
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
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn desired_run_with_stopped_actual_emits_start_daemon() {
        let mut actual = MeshOsState::default();
        let d = daemon("telemetry", 1);
        actual.daemons.insert(d.clone(), DaemonStatus::default()); // default lifecycle = Stopped
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(actions, vec![MeshOsAction::StartDaemon { daemon: d }],);
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
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
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
        desired
            .desired_daemons
            .insert(d.clone(), DaemonIntent::Stop);

        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
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
        assert!(matches!(
            status.backoff.state(),
            RestartState::BackingOff { .. }
        ));
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        match actions.as_slice() {
            [MeshOsAction::ApplyBackoff { daemon: d2, until }] => {
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

        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        assert!(matches!(
            status.backoff.state(),
            RestartState::CrashLooping { .. }
        ));
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        match actions.as_slice() {
            [MeshOsAction::ApplyBackoff { daemon: d2, .. }] => assert_eq!(d2, &d),
            other => panic!("expected ApplyBackoff under crash-loop gate, got {other:?}"),
        }
    }

    #[test]
    fn apply_backoff_is_not_re_emitted_after_the_loop_records_it() {
        // Regression for I1: a daemon parked in `CrashLooping`
        // used to generate an `ApplyBackoff` action every tick.
        // The loop now writes `applied_backoffs[daemon] = until`
        // after consuming the action; reconcile suppresses the
        // re-emit while `until` hasn't moved.
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(at(base, 1));
        let d = daemon("telemetry", 1);
        let mut status = DaemonStatus::default();
        for i in 0..5 {
            status.backoff.observe_crash(at(base, i));
        }
        let until = status
            .backoff
            .state()
            .release_at()
            .expect("crash-looping state carries a release_at");
        actual.daemons.insert(d.clone(), status);
        let mut desired = DesiredState::default();
        desired.desired_daemons.insert(d.clone(), DaemonIntent::Run);

        // First reconcile pass: emits ApplyBackoff.
        let first = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(first.len(), 1, "first pass should emit ApplyBackoff");

        // Simulate the loop's writeback after consuming the
        // emitted action.
        actual.applied_backoffs.insert(d.clone(), until);

        // Second reconcile pass with the same `until` — no
        // re-emit.
        let second = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(
            second.is_empty(),
            "second pass within the same backoff window must not re-emit ApplyBackoff",
        );
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
        let a = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        let b = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1, 2, 3]));
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Hold);
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        actual.replicas.insert(
            CHAIN_A,
            ::std::collections::BTreeSet::from([1, 2, THIS_NODE]),
        );
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Hold);
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn local_intent_drop_when_actually_holding_emits_drop_replica() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1, THIS_NODE]));
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Drop);
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(actions, vec![MeshOsAction::DropReplica { chain: CHAIN_A }]);
    }

    #[test]
    fn local_intent_drop_when_not_holding_emits_nothing() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1, 2]));
        let mut desired = DesiredState::default();
        desired
            .desired_local_replicas
            .insert(CHAIN_A, LocalReplicaIntent::Drop);
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
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
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn leader_with_undercount_emits_request_placement() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1, 2]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 4);
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([5, 2, 9]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 2);
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1, 2]));
        actual.replica_leader.insert(CHAIN_A, 999); // someone else is leader
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 4);
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn no_known_leader_silences_request_actions() {
        // No entry in `replica_leader` for the chain ⇒ no
        // `Request*` is admissible from any node. We wait for
        // election to fire `ReplicaLeaderUpdate`.
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1]));
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 3);
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn leader_at_exact_count_emits_nothing() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1, 2, 3]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 3);
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    // ----- Phase D: locality + admin events -----

    #[test]
    fn rtt_above_threshold_emits_mark_avoid_once() {
        let mut actual = MeshOsState::default();
        actual.rtt.insert(42, Duration::from_millis(500));
        // Default LocalityConfig threshold is 250 ms; 500 ms
        // exceeds → MarkAvoid emitted.
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        match actions.as_slice() {
            [MeshOsAction::MarkAvoid { peer, ttl, .. }] => {
                assert_eq!(*peer, 42);
                assert_eq!(*ttl, LocalityConfig::default().avoid_ttl);
            }
            other => panic!("expected one MarkAvoid, got {other:?}"),
        }
    }

    #[test]
    fn rtt_below_threshold_emits_nothing() {
        let mut actual = MeshOsState::default();
        actual.rtt.insert(42, Duration::from_millis(100));
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn mark_avoid_is_idempotent_when_peer_already_on_avoid_list() {
        let mut actual = MeshOsState::default();
        actual.rtt.insert(42, Duration::from_millis(500));
        actual.avoid_list.insert(
            42,
            AvoidEntry {
                reason: "earlier".into(),
                until: Instant::now() + Duration::from_secs(60),
            },
        );
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(
            actions.is_empty(),
            "MarkAvoid duplicated for already-avoided peer: {actions:?}"
        );
    }

    #[test]
    fn mark_avoid_emission_is_sorted_by_peer_id_for_stability() {
        let mut actual = MeshOsState::default();
        actual.rtt.insert(7, Duration::from_millis(500));
        actual.rtt.insert(3, Duration::from_millis(500));
        actual.rtt.insert(11, Duration::from_millis(500));
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        let peers: Vec<NodeId> = actions
            .iter()
            .map(|a| match a {
                MeshOsAction::MarkAvoid { peer, .. } => *peer,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(peers, vec![3, 7, 11]);
    }

    #[test]
    fn drop_replicas_admin_event_projects_into_drop_intent() {
        // Apply the admin event via DesiredState::apply_admin
        // (mirrors what the loop does). Then reconcile should
        // emit DropReplica for each chain THIS_NODE currently
        // holds.
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE, 1]));
        actual
            .replicas
            .insert(CHAIN_B, ::std::collections::BTreeSet::from([THIS_NODE]));

        let mut desired = DesiredState::default();
        desired.apply_admin(
            &super::super::event::AdminEvent::DropReplicas {
                node: THIS_NODE,
                chains: vec![CHAIN_A, CHAIN_B],
            },
            THIS_NODE,
        );

        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        // Two DropReplica actions, sorted by chain id.
        assert_eq!(
            actions,
            vec![
                MeshOsAction::DropReplica { chain: CHAIN_A },
                MeshOsAction::DropReplica { chain: CHAIN_B },
            ],
        );
    }

    #[test]
    fn drop_replicas_admin_event_targeted_at_other_node_is_a_noop_locally() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE, 1]));
        let mut desired = DesiredState::default();
        desired.apply_admin(
            &super::super::event::AdminEvent::DropReplicas {
                node: 999, // not this node
                chains: vec![CHAIN_A],
            },
            THIS_NODE,
        );
        assert!(reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        )
        .is_empty());
    }

    #[test]
    fn custom_locality_threshold_overrides_default() {
        let mut actual = MeshOsState::default();
        actual.rtt.insert(42, Duration::from_millis(150));
        // Custom threshold of 100 ms — 150 ms now degrades.
        let locality = LocalityConfig {
            degraded_rtt_threshold: Duration::from_millis(100),
            avoid_ttl: Duration::from_secs(60),
        };
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &locality,
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        match actions.as_slice() {
            [MeshOsAction::MarkAvoid { peer, ttl, .. }] => {
                assert_eq!(*peer, 42);
                assert_eq!(*ttl, Duration::from_secs(60));
            }
            other => panic!("expected one MarkAvoid under tightened threshold, got {other:?}"),
        }
    }

    // ----- Phase E: maintenance state machine -----

    #[test]
    fn active_state_emits_no_maintenance_transition() {
        let actual = MeshOsState::default(); // local_maintenance = Active
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn entering_with_drained_replicas_and_stopped_daemons_emits_maintenance_transition() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual.local_maintenance = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: None,
        };
        // No replicas on this node; no running daemons. Transition admissible.
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(
            actions,
            vec![MeshOsAction::CommitMaintenanceTransition {
                node: THIS_NODE,
                target: MaintenanceTransition::Maintenance,
            }],
        );
    }

    #[test]
    fn entering_with_remaining_replicas_does_not_transition_to_maintenance() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual.local_maintenance = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: None,
        };
        // This node still holds a replica — block the transition.
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn entering_with_running_daemon_does_not_transition_to_maintenance() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual.local_maintenance = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: None,
        };
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        actual.daemons.insert(daemon("telemetry", 1), status);
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn entering_past_deadline_with_conditions_unmet_transitions_to_drain_failed() {
        let base = anchor();
        let deadline = base + Duration::from_secs(60);
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(deadline + Duration::from_secs(1)); // past deadline
        actual.local_maintenance = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: Some(deadline),
        };
        // Still holding a replica → drain unmet.
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(
            actions,
            vec![MeshOsAction::CommitMaintenanceTransition {
                node: THIS_NODE,
                target: MaintenanceTransition::DrainFailed,
            }],
        );
    }

    #[test]
    fn entering_past_deadline_with_conditions_met_prefers_maintenance_over_drain_failed() {
        // Both conditions met at the boundary instant — the
        // maintenance transition takes priority (it's the
        // success path).
        let base = anchor();
        let deadline = base + Duration::from_secs(60);
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(deadline + Duration::from_secs(1));
        actual.local_maintenance = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: Some(deadline),
        };
        // No replicas, no daemons → conditions met.
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(
            actions,
            vec![MeshOsAction::CommitMaintenanceTransition {
                node: THIS_NODE,
                target: MaintenanceTransition::Maintenance,
            }],
        );
    }

    #[test]
    fn maintenance_steady_state_emits_no_transition() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual.local_maintenance = MaintenanceState::Maintenance { since: base };
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn exiting_with_healthy_daemons_emits_recovery_transition() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual.local_maintenance = MaintenanceState::ExitingMaintenance { since: base };
        let mut status = DaemonStatus::default();
        status.lifecycle = DaemonLifecycle::Running;
        actual.daemons.insert(daemon("telemetry", 1), status);
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(
            actions,
            vec![MeshOsAction::CommitMaintenanceTransition {
                node: THIS_NODE,
                target: MaintenanceTransition::Recovery,
            }],
        );
    }

    #[test]
    fn recovery_before_ramp_window_elapses_emits_nothing() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base + Duration::from_secs(60)); // 1 min in
        actual.local_maintenance = MaintenanceState::Recovery { since: base };
        let desired = DesiredState::default();
        // Default ramp window is 5 min — we're only 1 min in.
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn recovery_after_ramp_window_elapses_emits_active_transition() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base + Duration::from_secs(6 * 60)); // 6 min in
        actual.local_maintenance = MaintenanceState::Recovery { since: base };
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert_eq!(
            actions,
            vec![MeshOsAction::CommitMaintenanceTransition {
                node: THIS_NODE,
                target: MaintenanceTransition::Active,
            }],
        );
    }

    #[test]
    fn drain_failed_emits_no_transition_until_operator_intervention() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual.local_maintenance = MaintenanceState::DrainFailed {
            since: base,
            reason: "deadline elapsed".into(),
        };
        let desired = DesiredState::default();
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
        assert!(actions.is_empty());
    }

    // ----- Phase D-1: scheduler arm -----

    /// Test-only scorer with a fixed score-table + alternative
    /// lookup. The reconcile tests build one per case so the
    /// emission contract is fully observable.
    struct FixedScorer {
        scores: std::collections::HashMap<(ChainId, NodeId), f32>,
        alternatives: std::collections::HashMap<ChainId, (NodeId, f32)>,
    }

    impl super::super::scheduler::PlacementScorer for FixedScorer {
        fn score(&self, chain: ChainId, node: NodeId) -> Option<f32> {
            self.scores.get(&(chain, node)).copied()
        }
        fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)> {
            let (n, s) = self.alternatives.get(&chain).copied()?;
            if exclude.contains(&n) {
                None
            } else {
                Some((n, s))
            }
        }
    }

    fn scheduler_call(
        actual: &MeshOsState,
        scorer: Option<&dyn super::super::scheduler::PlacementScorer>,
    ) -> Vec<MeshOsAction> {
        reconcile(
            actual,
            &DesiredState::default(),
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            scorer,
        )
    }

    #[test]
    fn no_scorer_yields_no_scheduler_actions() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let actions = scheduler_call(&actual, None);
        assert!(actions.is_empty());
    }

    #[test]
    fn high_score_holder_is_not_rebalanced() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.9)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.95))].into_iter().collect(),
        };
        let actions = scheduler_call(&actual, Some(&scorer));
        assert!(
            actions.is_empty(),
            "high-scoring holder should not rebalance"
        );
    }

    #[test]
    fn under_scoring_holder_emits_request_eviction_when_alternative_exceeds_hysteresis() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        let actions = scheduler_call(&actual, Some(&scorer));
        assert_eq!(
            actions,
            vec![MeshOsAction::RequestEviction {
                chain: CHAIN_A,
                victim: THIS_NODE,
            }],
        );
    }

    #[test]
    fn under_scoring_holder_with_no_alternative_does_not_emit() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: Default::default(), // no alternative
        };
        assert!(scheduler_call(&actual, Some(&scorer)).is_empty());
    }

    #[test]
    fn under_scoring_holder_with_small_improvement_is_blocked_by_hysteresis() {
        // Score = 0.3; alternative = 0.4; gap = 0.1. Default
        // hysteresis_gap = 0.2 — should not emit.
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.4))].into_iter().collect(),
        };
        assert!(scheduler_call(&actual, Some(&scorer)).is_empty());
    }

    #[test]
    fn non_leader_does_not_emit_eviction_even_for_under_scoring_chain() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, 999); // someone else leads
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        assert!(scheduler_call(&actual, Some(&scorer)).is_empty());
    }

    #[test]
    fn cooldown_blocks_rebalance_within_window() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base + Duration::from_secs(60)); // 1 min after rebalance
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        actual.last_rebalance.insert(CHAIN_A, base);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        // Default cooldown is 5 min; we're only 1 min past.
        assert!(scheduler_call(&actual, Some(&scorer)).is_empty());
    }

    #[test]
    fn cooldown_releases_after_window_elapses() {
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base + Duration::from_secs(6 * 60)); // 6 min in
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        actual.last_rebalance.insert(CHAIN_A, base);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        let actions = scheduler_call(&actual, Some(&scorer));
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn phase_c_overcount_eviction_suppresses_phase_d1_eviction_for_same_chain() {
        // Regression for I2. Two leader-only arms can both fire
        // a RequestEviction in one reconcile pass — Phase C
        // when actual_count > desired_count, Phase D-1 when the
        // worst score is below the floor. Without de-dup the
        // same chain emits two evictions (possibly with
        // different victims) in one tick. The pass now budgets
        // one eviction per chain per tick — Phase C runs first,
        // and the scheduler arm skips any chain it touched.
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        // Two holders for CHAIN_A; this_node is leader; desired
        // count is 1 — Phase C will emit eviction of lex-
        // smallest holder.
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE, 99]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 1);
        // Scheduler would otherwise fire too — THIS_NODE scores
        // 0.1, alternative 0.9.
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.1), ((CHAIN_A, 99), 0.1)]
                .into_iter()
                .collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            Some(&scorer),
        );
        let evictions: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, MeshOsAction::RequestEviction { .. }))
            .collect();
        assert_eq!(
            evictions.len(),
            1,
            "phase C and phase D-1 must not both evict the same chain in one tick",
        );
    }

    #[test]
    fn scheduler_eviction_is_idempotent_when_loop_writes_back_last_rebalance() {
        // Regression for the C3 review item. The pure reconcile
        // function reads `last_rebalance`; the canonical event
        // loop writes it after consuming the emitted action. A
        // re-run of reconcile within the cooldown window must
        // emit nothing.
        let base = anchor();
        let mut actual = MeshOsState::default();
        actual.last_tick = Some(base);
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3)].into_iter().collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        let first = scheduler_call(&actual, Some(&scorer));
        assert_eq!(
            first,
            vec![MeshOsAction::RequestEviction {
                chain: CHAIN_A,
                victim: THIS_NODE,
            }],
        );
        // Simulate the loop's writeback after consuming the
        // emitted action.
        actual.last_rebalance.insert(CHAIN_A, base);
        let second = scheduler_call(&actual, Some(&scorer));
        assert!(
            second.is_empty(),
            "second reconcile within cooldown must not re-emit",
        );
    }

    #[test]
    fn worst_holder_is_picked_as_victim() {
        let mut actual = MeshOsState::default();
        actual.replicas.insert(
            CHAIN_A,
            ::std::collections::BTreeSet::from([THIS_NODE, 11, 12]),
        );
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        let scorer = FixedScorer {
            scores: [
                ((CHAIN_A, THIS_NODE), 0.6),
                ((CHAIN_A, 11), 0.3), // lowest
                ((CHAIN_A, 12), 0.7),
            ]
            .into_iter()
            .collect(),
            alternatives: [(CHAIN_A, (5, 0.9))].into_iter().collect(),
        };
        match scheduler_call(&actual, Some(&scorer)).as_slice() {
            [MeshOsAction::RequestEviction { chain, victim }] => {
                assert_eq!(*chain, CHAIN_A);
                assert_eq!(*victim, 11);
            }
            other => panic!("expected one RequestEviction(11), got {other:?}"),
        }
    }

    #[test]
    fn scheduler_emits_actions_in_chain_id_sorted_order() {
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_B, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([THIS_NODE]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        actual.replica_leader.insert(CHAIN_B, THIS_NODE);
        let scorer = FixedScorer {
            scores: [((CHAIN_A, THIS_NODE), 0.3), ((CHAIN_B, THIS_NODE), 0.3)]
                .into_iter()
                .collect(),
            alternatives: [(CHAIN_A, (5, 0.9)), (CHAIN_B, (6, 0.9))]
                .into_iter()
                .collect(),
        };
        let actions = scheduler_call(&actual, Some(&scorer));
        let chains: Vec<ChainId> = actions
            .iter()
            .map(|a| match a {
                MeshOsAction::RequestEviction { chain, .. } => *chain,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(chains, vec![CHAIN_A, CHAIN_B]);
    }

    #[test]
    fn reconcile_replica_actions_are_sorted_by_chain_id_for_stability() {
        // Two chains both undercount; the actions should appear
        // in chain-id ascending order regardless of HashMap
        // iteration. Pins the determinism contract.
        let mut actual = MeshOsState::default();
        actual
            .replicas
            .insert(CHAIN_A, ::std::collections::BTreeSet::from([1]));
        actual
            .replicas
            .insert(CHAIN_B, ::std::collections::BTreeSet::from([1]));
        actual.replica_leader.insert(CHAIN_A, THIS_NODE);
        actual.replica_leader.insert(CHAIN_B, THIS_NODE);
        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(CHAIN_A, 3);
        desired.desired_replicas.insert(CHAIN_B, 3);
        let actions = reconcile(
            &actual,
            &desired,
            THIS_NODE,
            &LocalityConfig::default(),
            &MaintenanceConfig::default(),
            &SchedulerConfig::default(),
            None,
        );
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
