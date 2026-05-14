//! Phase G — the single `admit()` layer that gates every
//! outbound [`super::action::MeshOsAction`] before the action
//! executor dispatches it. Locked decision #10: one
//! backpressure layer over all outbound actions, so a
//! drain-triggered migration cannot dodge the pull cooldown,
//! and a crash-looping daemon cannot dodge the gate just because
//! its restart was admin-driven.
//!
//! Tunables live on [`super::config::BackpressureConfig`].
//! Reads `last_tick` from the actual fold so consecutive admits
//! against the same time anchor are deterministic — the same
//! property reconcile relies on for idempotence.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::action::{ActionId, MeshOsAction, PendingAction};
use super::config::BackpressureConfig;
use super::event::{ChainId, DaemonRef};

/// Per-call decision the executor uses to dispatch or defer.
/// Locked decision #10: all admit() paths route through the
/// same enum so backpressure is observable + testable as a sync
/// pure-function.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AdmissionResult {
    /// Dispatch the action immediately.
    Admit,
    /// Defer — re-evaluate after `retry_after`. The executor
    /// puts the action back on the queue with a retry deadline.
    Defer {
        /// How long to wait before retrying.
        retry_after: Duration,
    },
    /// Hard gate — the action is dropped, not re-queued. Used
    /// for crash-loop gating where re-emission would spin.
    Gate {
        /// Earliest instant a retry could be admitted (the
        /// gate's release time).
        cooldown_until: Instant,
        /// Operator-readable reason — rides to the recent-
        /// failures ring buffer + audit log.
        reason: &'static str,
    },
}

/// State the backpressure layer maintains across admit calls.
/// Owned by the action executor (one per loop). Updated on
/// every `admit()` that returns `Admit`, so the next call sees
/// the cooldown / stabilization windows the prior admit
/// established.
#[derive(Debug, Default)]
pub struct BackpressureState {
    /// Last instant a `PullReplica` was admitted (any chain).
    /// Used for the global pull cooldown.
    last_pull_admitted: Option<Instant>,
    /// Per-chain stabilization deadlines. After a replica
    /// action lands for a chain, the chain is excluded from
    /// further `PullReplica` / `DropReplica` admits until the
    /// window elapses.
    chain_stabilization: HashMap<ChainId, Instant>,
    /// Per-daemon gate-release timestamps. Mirrors the
    /// per-daemon `BackoffTracker` state.release_at()` so the
    /// executor doesn't need to consult the actual fold on
    /// every admit.
    daemon_gates: HashMap<DaemonRef, Instant>,
    /// Sliding window of drain-triggered migrations within the
    /// current second. Bounded by `drain_rate_per_zone_per_sec`.
    /// Each entry is tagged with the admitting `ActionId` so
    /// `release_failed_admit` removes by id (not by `Instant`
    /// equality, which races when two admits land at the same
    /// time anchor).
    drain_window: Vec<(ActionId, Instant)>,
    /// Action id of the most recent `PullReplica` that took the
    /// `last_pull_admitted` slot. Used by `release_failed_admit`
    /// to undo the reservation without clobbering a sibling's
    /// admit that happens to share the same `Instant`.
    last_pull_admitted_by: Option<ActionId>,
    /// Current action-queue depth observed by the executor. The
    /// executor pushes this in before calling `admit()` (the
    /// executor knows the depth; the admit layer reads it).
    pub(crate) queue_depth: usize,
    /// Whether the cluster-wide backpressure flag is currently
    /// asserted. Updated by `update_cluster_backpressure` per
    /// tick.
    pub(crate) cluster_backpressure: bool,
}

impl BackpressureState {
    /// Build a fresh state. All windows empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide whether `action` may be dispatched now. Sync; no
    /// I/O; no allocations beyond the drain-window vec resize.
    /// `now` is the time anchor (passed in for testability +
    /// deterministic admit-vs-reconcile sequencing); `action_id`
    /// is the emitting `PendingAction`'s id, used to tag the
    /// reservation so a failed dispatch can release the exact
    /// slot it took (not a sibling's slot stamped at the same
    /// `Instant`).
    pub fn admit(
        &mut self,
        action_id: ActionId,
        action: &MeshOsAction,
        now: Instant,
        config: &BackpressureConfig,
    ) -> AdmissionResult {
        // Apply all relevant throttles in priority order. Each
        // throttle either short-circuits with Gate/Defer or
        // falls through to the next; a final Admit records the
        // action so subsequent throttles can read its effect.

        match action {
            MeshOsAction::PullReplica { chain, .. } => {
                if let Some(deadline) = self.chain_stabilization.get(chain) {
                    if *deadline > now {
                        return AdmissionResult::Defer {
                            retry_after: deadline.saturating_duration_since(now),
                        };
                    }
                }
                if let Some(last) = self.last_pull_admitted {
                    let next_admit = last.checked_add(config.pull_cooldown).unwrap_or(last);
                    if next_admit > now {
                        return AdmissionResult::Defer {
                            retry_after: next_admit.saturating_duration_since(now),
                        };
                    }
                }
                self.last_pull_admitted = Some(now);
                self.last_pull_admitted_by = Some(action_id);
                self.chain_stabilization.insert(
                    *chain,
                    now.checked_add(config.replica_stabilization_window)
                        .unwrap_or(now),
                );
                AdmissionResult::Admit
            }
            MeshOsAction::DropReplica { chain } => {
                if let Some(deadline) = self.chain_stabilization.get(chain) {
                    if *deadline > now {
                        return AdmissionResult::Defer {
                            retry_after: deadline.saturating_duration_since(now),
                        };
                    }
                }
                self.chain_stabilization.insert(
                    *chain,
                    now.checked_add(config.replica_stabilization_window)
                        .unwrap_or(now),
                );
                AdmissionResult::Admit
            }
            MeshOsAction::StartDaemon { daemon }
            | MeshOsAction::StopDaemon { daemon, .. }
            | MeshOsAction::ApplyBackoff { daemon, .. } => {
                if let Some(until) = self.daemon_gates.get(daemon) {
                    if *until > now {
                        return AdmissionResult::Gate {
                            cooldown_until: *until,
                            reason: "daemon-backoff",
                        };
                    }
                }
                AdmissionResult::Admit
            }
            MeshOsAction::MigrateBlob { .. } => {
                // Treat blob migrations under the drain rate
                // limit (locked decision #10 paragraph 2):
                // drain-triggered movement runs through this
                // same throttle.
                self.gc_drain_window(now);
                if (self.drain_window.len() as u32) >= config.drain_rate_per_zone_per_sec {
                    return AdmissionResult::Defer {
                        retry_after: Duration::from_millis(100),
                    };
                }
                self.drain_window.push((action_id, now));
                AdmissionResult::Admit
            }
            // RequestPlacement / RequestEviction / MarkAvoid /
            // ReduceHeat / CommitMaintenanceTransition: no
            // throttle today. They cost essentially nothing to
            // commit (one chain commit each); cluster-wide
            // backpressure is the only mechanism if they ever
            // need rate-limiting.
            _ => AdmissionResult::Admit,
        }
    }

    /// Tell the backpressure layer that a daemon is now under a
    /// `BackoffTracker` gate. The executor reads
    /// [`super::supervision::RestartState::release_at`] off the
    /// state fold and pushes it here so admit() doesn't have to
    /// crack the fold open every call.
    pub fn record_daemon_gate(&mut self, daemon: DaemonRef, until: Instant) {
        self.daemon_gates.insert(daemon, until);
    }

    /// Roll back the reservations [`admit`] made for the
    /// `(action_id, action)` pair. Call after a dispatch failure:
    /// the cooldowns admit installed reflect a side effect that
    /// never happened, so leaving them in place would gate
    /// unrelated future actions for nothing. The `action_id` tag
    /// scopes the rollback to the matching admit so a sibling
    /// admit at the same `Instant` doesn't lose its reservation.
    pub fn release_failed_admit(&mut self, action_id: ActionId, action: &MeshOsAction) {
        match action {
            MeshOsAction::PullReplica { chain, .. } => {
                if self.last_pull_admitted_by == Some(action_id) {
                    self.last_pull_admitted = None;
                    self.last_pull_admitted_by = None;
                }
                self.chain_stabilization.remove(chain);
            }
            MeshOsAction::DropReplica { chain } => {
                self.chain_stabilization.remove(chain);
            }
            MeshOsAction::MigrateBlob { .. } => {
                if let Some(pos) = self
                    .drain_window
                    .iter()
                    .position(|(id, _)| *id == action_id)
                {
                    self.drain_window.remove(pos);
                }
            }
            _ => {}
        }
    }

    /// Periodic-tick maintenance. Drops daemon gates that have
    /// elapsed; drops chain stabilization windows that have
    /// elapsed.
    pub fn tick(&mut self, now: Instant) {
        self.daemon_gates.retain(|_, until| *until > now);
        self.chain_stabilization.retain(|_, until| *until > now);
        self.gc_drain_window(now);
    }

    /// Compute cluster-wide backpressure state from the current
    /// queue depth + hysteresis thresholds. Returns the
    /// transition direction, if any.
    pub fn update_cluster_backpressure(
        &mut self,
        queue_depth: usize,
        config: &BackpressureConfig,
    ) -> ClusterBackpressureChange {
        self.queue_depth = queue_depth;
        if self.cluster_backpressure {
            // Currently asserted — release only below the
            // hysteresis low-water mark.
            if queue_depth <= config.cluster_backpressure_release {
                self.cluster_backpressure = false;
                ClusterBackpressureChange::Released
            } else {
                ClusterBackpressureChange::Steady
            }
        } else {
            // Not asserted — trigger above the high-water mark.
            if queue_depth >= config.cluster_backpressure_threshold {
                self.cluster_backpressure = true;
                ClusterBackpressureChange::Asserted
            } else {
                ClusterBackpressureChange::Steady
            }
        }
    }

    fn gc_drain_window(&mut self, now: Instant) {
        let cutoff = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
        self.drain_window.retain(|(_, t)| *t > cutoff);
    }
}

/// Transition direction returned by
/// [`BackpressureState::update_cluster_backpressure`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ClusterBackpressureChange {
    /// No change — queue depth between the thresholds.
    Steady,
    /// Newly asserted — queue crossed the high-water mark.
    /// Executor broadcasts `MeshOsControl::BackpressureOn` to
    /// every supervised daemon.
    Asserted,
    /// Newly released — queue dropped below the low-water mark.
    /// Executor broadcasts `MeshOsControl::BackpressureOff`.
    Released,
}

/// Helper for tests + callers: wrap a [`PendingAction`] into an
/// `admit` call.
pub fn admit(
    state: &mut BackpressureState,
    pending: &PendingAction,
    now: Instant,
    config: &BackpressureConfig,
) -> AdmissionResult {
    state.admit(pending.id, &pending.action, now, config)
}

#[cfg(test)]
mod tests {
    use super::super::action::{ActionId, MaintenanceTransition};
    use super::*;

    fn dref(name: &str, id: u64) -> DaemonRef {
        DaemonRef {
            id,
            name: name.into(),
        }
    }

    fn t(secs: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_secs(secs)
    }

    fn t_ms(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
    }

    fn aid(n: u64) -> ActionId {
        ActionId(n)
    }

    fn admit_pull(state: &mut BackpressureState, chain: ChainId, now: Instant) -> AdmissionResult {
        state.admit(
            aid(chain),
            &MeshOsAction::PullReplica { chain, source: 1 },
            now,
            &BackpressureConfig::default(),
        )
    }

    #[test]
    fn first_pull_replica_is_admitted() {
        let mut state = BackpressureState::new();
        assert_eq!(admit_pull(&mut state, 1, t(0)), AdmissionResult::Admit);
    }

    #[test]
    fn second_pull_replica_within_cooldown_defers() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        // First pull at t=0 succeeds.
        let _ = admit_pull(&mut state, 1, t_ms(0));
        // Second pull at t=100ms (within the default 250 ms cooldown).
        match state.admit(
            aid(2),
            &MeshOsAction::PullReplica {
                chain: 2,
                source: 1,
            },
            t_ms(100),
            &cfg,
        ) {
            AdmissionResult::Defer { retry_after } => {
                // Should defer ~150ms (the remaining cooldown).
                assert!(retry_after > Duration::from_millis(140));
                assert!(retry_after <= Duration::from_millis(160));
            }
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn pull_after_cooldown_is_admitted() {
        let mut state = BackpressureState::new();
        let _ = admit_pull(&mut state, 1, t_ms(0));
        // After 300 ms (past the 250 ms cooldown), with a
        // different chain (so the stabilization window doesn't
        // gate us).
        assert_eq!(admit_pull(&mut state, 2, t_ms(300)), AdmissionResult::Admit);
    }

    #[test]
    fn replica_stabilization_blocks_subsequent_actions_on_same_chain() {
        let mut state = BackpressureState::new();
        let _ = admit_pull(&mut state, 1, t(0));
        // Even 60 s later, the same chain is still gated by
        // the 60 s stabilization window.
        match admit_pull(&mut state, 1, t(30)) {
            AdmissionResult::Defer { retry_after } => {
                assert!(retry_after > Duration::from_secs(29));
            }
            other => panic!("expected Defer for chain still stabilizing, got {other:?}"),
        }
    }

    #[test]
    fn replica_stabilization_releases_after_window() {
        let mut state = BackpressureState::new();
        let _ = admit_pull(&mut state, 1, t(0));
        // 61 s later — stabilization window is 60 s — pull
        // would succeed on this chain. Need to also satisfy
        // the pull cooldown (250 ms since the last); 61 s
        // qualifies.
        assert_eq!(admit_pull(&mut state, 1, t(61)), AdmissionResult::Admit);
    }

    #[test]
    fn drop_replica_observes_chain_stabilization_window() {
        let mut state = BackpressureState::new();
        let _ = admit_pull(&mut state, 1, t(0));
        match state.admit(
            aid(2),
            &MeshOsAction::DropReplica { chain: 1 },
            t(10),
            &BackpressureConfig::default(),
        ) {
            AdmissionResult::Defer { .. } => {}
            other => panic!("expected Defer for stabilizing chain, got {other:?}"),
        }
    }

    #[test]
    fn daemon_gate_blocks_start_daemon_with_gate_admission_result() {
        let mut state = BackpressureState::new();
        let d = dref("telemetry", 1);
        state.record_daemon_gate(d.clone(), t(60));
        match state.admit(
            aid(1),
            &MeshOsAction::StartDaemon { daemon: d.clone() },
            t(0),
            &BackpressureConfig::default(),
        ) {
            AdmissionResult::Gate {
                cooldown_until,
                reason,
            } => {
                assert_eq!(cooldown_until, t(60));
                assert_eq!(reason, "daemon-backoff");
            }
            other => panic!("expected Gate, got {other:?}"),
        }
    }

    #[test]
    fn daemon_gate_releases_after_until_elapses() {
        let mut state = BackpressureState::new();
        let d = dref("telemetry", 1);
        state.record_daemon_gate(d.clone(), t(60));
        // Past the gate — admit.
        assert_eq!(
            state.admit(
                aid(1),
                &MeshOsAction::StartDaemon { daemon: d.clone() },
                t(61),
                &BackpressureConfig::default(),
            ),
            AdmissionResult::Admit,
        );
    }

    #[test]
    fn tick_garbage_collects_elapsed_daemon_gates_and_stabilization_windows() {
        let mut state = BackpressureState::new();
        let d = dref("telemetry", 1);
        state.record_daemon_gate(d.clone(), t(10));
        state.chain_stabilization.insert(1, t(10));
        state.tick(t(20));
        // Both entries are past their until and should be gone.
        assert!(!state.daemon_gates.contains_key(&d));
        assert!(!state.chain_stabilization.contains_key(&1));
    }

    #[test]
    fn migrate_blob_throttled_by_drain_rate_limit() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        // Default drain_rate_per_zone_per_sec = 10. Admit 10 in a row.
        for i in 0..10 {
            assert_eq!(
                state.admit(
                    aid(100 + i),
                    &MeshOsAction::MigrateBlob {
                        blob: i,
                        from: 1,
                        to: 2,
                    },
                    t(0),
                    &cfg,
                ),
                AdmissionResult::Admit,
            );
        }
        // 11th in the same second → Defer.
        match state.admit(
            aid(111),
            &MeshOsAction::MigrateBlob {
                blob: 11,
                from: 1,
                to: 2,
            },
            t(0),
            &cfg,
        ) {
            AdmissionResult::Defer { .. } => {}
            other => panic!("expected Defer for over-rate-limit migration, got {other:?}"),
        }
    }

    #[test]
    fn migrate_blob_rate_limit_window_slides_with_time() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        for i in 0..10 {
            let _ = state.admit(
                aid(200 + i),
                &MeshOsAction::MigrateBlob {
                    blob: i,
                    from: 1,
                    to: 2,
                },
                t(0),
                &cfg,
            );
        }
        // A second later, the window has slid forward; new
        // migrations admit again.
        assert_eq!(
            state.admit(
                aid(299),
                &MeshOsAction::MigrateBlob {
                    blob: 99,
                    from: 1,
                    to: 2,
                },
                t(2),
                &cfg,
            ),
            AdmissionResult::Admit,
        );
    }

    #[test]
    fn cluster_backpressure_asserts_above_high_water_mark() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        assert_eq!(
            state.update_cluster_backpressure(1000, &cfg),
            ClusterBackpressureChange::Asserted,
        );
        assert!(state.cluster_backpressure);
    }

    #[test]
    fn cluster_backpressure_releases_below_low_water_mark() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let _ = state.update_cluster_backpressure(1000, &cfg);
        // Drop below release threshold (default 200).
        assert_eq!(
            state.update_cluster_backpressure(150, &cfg),
            ClusterBackpressureChange::Released,
        );
        assert!(!state.cluster_backpressure);
    }

    #[test]
    fn cluster_backpressure_hysteresis_holds_between_thresholds() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        // Cross high-water mark.
        let _ = state.update_cluster_backpressure(1000, &cfg);
        // Now drop to a value above the release threshold but
        // below the trigger threshold (e.g., 500). Should stay
        // asserted.
        assert_eq!(
            state.update_cluster_backpressure(500, &cfg),
            ClusterBackpressureChange::Steady,
        );
        assert!(state.cluster_backpressure);
        // Below low-water mark — released.
        assert_eq!(
            state.update_cluster_backpressure(100, &cfg),
            ClusterBackpressureChange::Released,
        );
    }

    #[test]
    fn unthrottled_action_kinds_pass_straight_through() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let cases = [
            MeshOsAction::RequestPlacement {
                chain: 1,
                exclude: vec![],
                target: None,
            },
            MeshOsAction::RequestEviction {
                chain: 1,
                victim: 2,
            },
            MeshOsAction::MarkAvoid {
                peer: 1,
                reason: "x".into(),
                ttl: Duration::from_secs(60),
            },
            MeshOsAction::ReduceHeat { blob: 1, by: 1 },
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Maintenance,
            },
        ];
        for (idx, action) in cases.iter().enumerate() {
            assert_eq!(
                state.admit(aid(idx as u64 + 1), action, t(0), &cfg),
                AdmissionResult::Admit,
            );
        }
    }

    #[test]
    fn release_failed_admit_rolls_back_pull_cooldown_and_stabilization() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let now = t_ms(0);
        // First pull admits — cooldown + stabilization set.
        assert_eq!(
            state.admit(
                aid(1),
                &MeshOsAction::PullReplica {
                    chain: 1,
                    source: 2
                },
                now,
                &cfg,
            ),
            AdmissionResult::Admit,
        );
        assert_eq!(state.last_pull_admitted, Some(now));
        assert!(state.chain_stabilization.contains_key(&1));
        // Dispatch failed — release.
        state.release_failed_admit(
            aid(1),
            &MeshOsAction::PullReplica {
                chain: 1,
                source: 2,
            },
        );
        assert!(state.last_pull_admitted.is_none());
        assert!(!state.chain_stabilization.contains_key(&1));
        // A pull on a different chain immediately after the
        // release admits — the leaked cooldown would have
        // forced Defer.
        assert_eq!(
            state.admit(
                aid(2),
                &MeshOsAction::PullReplica {
                    chain: 2,
                    source: 3
                },
                now,
                &cfg,
            ),
            AdmissionResult::Admit,
        );
    }

    #[test]
    fn release_failed_admit_pops_drain_window_entry_for_migrate_blob() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let now = t_ms(0);
        let migrate = MeshOsAction::MigrateBlob {
            blob: 1,
            from: 1,
            to: 2,
        };
        assert_eq!(
            state.admit(aid(1), &migrate, now, &cfg),
            AdmissionResult::Admit,
        );
        assert_eq!(state.drain_window.len(), 1);
        state.release_failed_admit(aid(1), &migrate);
        assert_eq!(state.drain_window.len(), 0);
    }

    #[test]
    fn release_failed_admit_preserves_prior_admits_after_window() {
        // PullReplica admits at t=0 (cooldown set), then a
        // different chain admits at t=300ms (past the 250ms
        // cooldown) and its dispatch fails. Release rolls back
        // the second admit only; the first admit's pull (which
        // succeeded) is conceptually unaffected, but since
        // `last_pull_admitted` only tracks one anchor and the
        // first cooldown has elapsed, the rollback leaves the
        // state cleanly drained.
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let _ = state.admit(
            aid(1),
            &MeshOsAction::PullReplica {
                chain: 1,
                source: 2,
            },
            t_ms(0),
            &cfg,
        );
        let later = t_ms(300);
        let _ = state.admit(
            aid(2),
            &MeshOsAction::PullReplica {
                chain: 2,
                source: 2,
            },
            later,
            &cfg,
        );
        assert_eq!(state.last_pull_admitted, Some(later));
        state.release_failed_admit(
            aid(2),
            &MeshOsAction::PullReplica {
                chain: 2,
                source: 2,
            },
        );
        // Anchor cleared; chain 2 stabilization removed.
        assert!(state.last_pull_admitted.is_none());
        assert!(!state.chain_stabilization.contains_key(&2));
        // Chain 1's stabilization (installed by the first admit)
        // is untouched.
        assert!(state.chain_stabilization.contains_key(&1));
    }

    #[test]
    fn release_failed_admit_removes_only_the_failing_drain_window_entry() {
        // Two MigrateBlob admits at the same Instant. The second
        // fails. release_failed_admit should remove the second's
        // slot, not the first's, even though both have identical
        // `Instant` timestamps. The drain-rate budget is
        // preserved for the still-in-flight first migration.
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let now = t_ms(0);
        let first = MeshOsAction::MigrateBlob {
            blob: 1,
            from: 1,
            to: 2,
        };
        let second = MeshOsAction::MigrateBlob {
            blob: 2,
            from: 1,
            to: 2,
        };
        assert_eq!(
            state.admit(aid(101), &first, now, &cfg),
            AdmissionResult::Admit,
        );
        assert_eq!(
            state.admit(aid(102), &second, now, &cfg),
            AdmissionResult::Admit,
        );
        assert_eq!(state.drain_window.len(), 2);
        // Second admit's dispatch fails — release by its id.
        state.release_failed_admit(aid(102), &second);
        assert_eq!(state.drain_window.len(), 1);
        // The remaining slot belongs to the first admit
        // (aid 101), preserving the budget already committed
        // against the in-flight first migration.
        assert_eq!(state.drain_window[0].0, aid(101));
    }

    #[test]
    fn admit_helper_wraps_a_pending_action() {
        let mut state = BackpressureState::new();
        let cfg = BackpressureConfig::default();
        let pending = PendingAction {
            id: ActionId(1),
            action: MeshOsAction::PullReplica {
                chain: 1,
                source: 2,
            },
            emitted_at: t(0),
        };
        assert_eq!(
            admit(&mut state, &pending, t(0), &cfg),
            AdmissionResult::Admit
        );
    }
}
