//! Phase E — local-node maintenance state machine.
//!
//! Every node carries one [`MaintenanceState`] for itself,
//! advanced by chain-driven [`super::event::AdminEvent`] commits
//! (operator-triggered transitions) + reconcile-emitted
//! [`super::action::MeshOsAction::CommitMaintenanceTransition`]
//! actions (loop-driven transitions). The state machine has six
//! states; every transition is idempotent under replay:
//!
//! ```text
//!   Active
//!     │  AdminEvent::EnterMaintenance
//!     ▼
//!   EnteringMaintenance ─────► DrainFailed   (deadline elapsed)
//!     │
//!     │  replicas drained + daemons stopped
//!     ▼
//!   Maintenance
//!     │  AdminEvent::ExitMaintenance
//!     ▼
//!   ExitingMaintenance
//!     │  daemons restarted + healthy
//!     ▼
//!   Recovery
//!     │  ramp-up window elapsed
//!     ▼
//!   Active
//! ```
//!
//! Per the plan's locked decision #5, **the source of truth is
//! the admin chain**, not the in-memory enum here. This struct
//! is the per-loop mirror that reconcile reads against. It's
//! advanced by:
//!
//! - admin-chain commits (operator-triggered Enter / Exit)
//! - [`super::event::MeshOsEvent::MaintenanceTransitionObserved`]
//!   when the action executor's commit makes it back through
//!   the chain
//!
//! Reconcile reads `local_maintenance` and emits
//! `CommitMaintenanceTransition` actions when the conditions
//! for a forward transition hold.

use std::time::Instant;

/// Per-node maintenance state. Carries `since` (for Deck
/// render), the deadline for `EnteringMaintenance` (so
/// `DrainFailed` can fire on elapse), and the reason for
/// `DrainFailed` (operator surfacing).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaintenanceState {
    /// Normal participation. Default state on construction.
    #[default]
    Active,
    /// Operator requested maintenance; this node is preparing
    /// for isolation. Replicas are migrating; non-essential
    /// daemons are stopping.
    EnteringMaintenance {
        /// Monotonic timestamp when the transition was entered.
        since: Instant,
        /// Optional deadline. Past this point the loop flips to
        /// [`MaintenanceState::DrainFailed`] if conditions
        /// haven't converged.
        deadline: Option<Instant>,
    },
    /// Steady-state isolation. Replicas migrated, daemons
    /// stopped. Operator commands run unimpeded.
    Maintenance {
        /// Monotonic timestamp when the steady state was entered.
        since: Instant,
    },
    /// Operator requested resume; this node is restarting
    /// daemons + re-emitting capabilities.
    ExitingMaintenance {
        /// Monotonic timestamp when the transition was entered.
        since: Instant,
    },
    /// Drain failed — replicas / daemons didn't converge before
    /// the deadline. Operator warning state until either the
    /// underlying condition resolves or an admin
    /// `ExitMaintenance { force = true }` lands.
    DrainFailed {
        /// Monotonic timestamp when the failure was recorded.
        since: Instant,
        /// Operator-readable reason.
        reason: String,
    },
    /// Recovery ramp-up window. Node is rejoined but on the
    /// avoid list for new placements until the window elapses.
    Recovery {
        /// Monotonic timestamp when the ramp-up started.
        since: Instant,
    },
}

impl MaintenanceState {
    /// Position in the state machine. Larger = further along.
    /// `Active` is `0`, `Recovery` is `5`. A transition is valid
    /// when the new position is `>=` the current position, OR
    /// when the new state is `Active` (the `Recovery → Active`
    /// terminal arc wraps to `0`). Same-position transitions
    /// stay idempotent.
    #[allow(dead_code)]
    fn rank(&self) -> u8 {
        match self {
            MaintenanceState::Active => 0,
            MaintenanceState::EnteringMaintenance { .. } => 1,
            MaintenanceState::Maintenance { .. } => 2,
            MaintenanceState::ExitingMaintenance { .. } => 3,
            MaintenanceState::DrainFailed { .. } => 3,
            MaintenanceState::Recovery { .. } => 4,
        }
    }

    /// `true` when `new` is a permissible successor to `self`.
    /// Forward arcs in the diagram + the `Recovery → Active`
    /// terminal arc are valid; backward arcs (e.g., a late-
    /// arriving `Maintenance` observed event after `Recovery`)
    /// are not. Same-rank transitions are valid (idempotent
    /// replay of the same observed state).
    ///
    /// The maintenance graph isn't totally ordered: `ExitingMaintenance`
    /// and `DrainFailed` share the same `rank()` (both come out
    /// of `Maintenance`), so a pure rank-ladder admitted the
    /// `ExitingMaintenance → DrainFailed` regression: an operator
    /// `ExitMaintenance { force=true }` could be silently undone
    /// by a delayed `MaintenanceTransitionObserved(DrainFailed)`
    /// from chain replay. The explicit match-table below rejects
    /// that specific cross-rank arc while still allowing every
    /// legitimate forward arc.
    pub fn is_valid_successor(&self, new: &MaintenanceState) -> bool {
        use MaintenanceState::*;
        // Idempotent same-variant transitions are always allowed.
        // This is REPLAY TOLERANCE, not strict field equality: an
        // `EnteringMaintenance { since: a, deadline: X }` →
        // `EnteringMaintenance { since: b, deadline: Y }` returns
        // true even though the inner fields differ. Chain replays
        // and operator re-publishes legitimately produce same-
        // variant observations with refreshed timestamps; the fold
        // downstream takes the most-recent value and does NOT key
        // ordering on `is_valid_successor`'s yes/no answer — so
        // accepting inner-field drift here is the correct behavior.
        // A future fold that depended on strict same-variant
        // equality would need to compare fields itself; this
        // function answers a different question ("is this
        // transition legal at the rank graph level?").
        if std::mem::discriminant(self) == std::mem::discriminant(new) {
            return true;
        }
        match (self, new) {
            // Forward path.
            (Active, EnteringMaintenance { .. }) => true,
            (EnteringMaintenance { .. }, Maintenance { .. }) => true,
            // Drain can fail during the entering window OR once
            // steady-state maintenance is established.
            (EnteringMaintenance { .. }, DrainFailed { .. }) => true,
            (Maintenance { .. }, ExitingMaintenance { .. }) => true,
            (Maintenance { .. }, DrainFailed { .. }) => true,
            (ExitingMaintenance { .. }, Recovery { .. }) => true,
            // Recovery from a stuck drain — operator can force
            // exit out of DrainFailed back into ExitingMaintenance,
            // and a subsequent retry can reach Recovery from there.
            (DrainFailed { .. }, ExitingMaintenance { .. }) => true,
            // Terminal arc.
            (Recovery { .. }, Active) => true,
            // Everything else (including the documented regression
            // ExitingMaintenance → DrainFailed) is rejected. Two
            // states at the same rank are no longer indistinguishable.
            _ => false,
        }
    }

    /// `true` when the node is in any state other than
    /// `Active`. Used by reconcile to short-circuit replica /
    /// daemon emission while a maintenance window is in flight.
    pub fn is_non_active(&self) -> bool {
        !matches!(self, MaintenanceState::Active)
    }

    /// `true` when the node is fully isolated (steady-state
    /// maintenance). Operator commands run only here.
    pub fn is_steady_maintenance(&self) -> bool {
        matches!(self, MaintenanceState::Maintenance { .. })
    }

    /// The instant the current state was entered, if any.
    /// `Active` has no `since` (it's the default).
    pub fn since(&self) -> Option<Instant> {
        match self {
            MaintenanceState::Active => None,
            MaintenanceState::EnteringMaintenance { since, .. }
            | MaintenanceState::Maintenance { since }
            | MaintenanceState::ExitingMaintenance { since }
            | MaintenanceState::DrainFailed { since, .. }
            | MaintenanceState::Recovery { since } => Some(*since),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn default_is_active() {
        let s = MaintenanceState::default();
        assert!(matches!(s, MaintenanceState::Active));
        assert!(!s.is_non_active());
        assert!(!s.is_steady_maintenance());
        assert_eq!(s.since(), None);
    }

    #[test]
    fn predicates_reflect_state_shape() {
        let base = Instant::now();
        let entering = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: Some(base + Duration::from_secs(60)),
        };
        assert!(entering.is_non_active());
        assert!(!entering.is_steady_maintenance());
        assert_eq!(entering.since(), Some(base));

        let steady = MaintenanceState::Maintenance { since: base };
        assert!(steady.is_non_active());
        assert!(steady.is_steady_maintenance());

        let recovery = MaintenanceState::Recovery { since: base };
        assert!(recovery.is_non_active());
        assert!(!recovery.is_steady_maintenance());
    }

    #[test]
    fn is_valid_successor_accepts_forward_arcs_only() {
        let base = Instant::now();
        let active = MaintenanceState::Active;
        let entering = MaintenanceState::EnteringMaintenance {
            since: base,
            deadline: None,
        };
        let maintenance = MaintenanceState::Maintenance { since: base };
        let exiting = MaintenanceState::ExitingMaintenance { since: base };
        let drain_failed = MaintenanceState::DrainFailed {
            since: base,
            reason: "deadline".into(),
        };
        let recovery = MaintenanceState::Recovery { since: base };

        // Forward arcs (allowed).
        assert!(active.is_valid_successor(&entering));
        assert!(entering.is_valid_successor(&maintenance));
        assert!(entering.is_valid_successor(&drain_failed));
        assert!(maintenance.is_valid_successor(&exiting));
        assert!(drain_failed.is_valid_successor(&exiting));
        assert!(exiting.is_valid_successor(&recovery));
        assert!(recovery.is_valid_successor(&active));

        // Same-state (idempotent) — allowed.
        assert!(entering.is_valid_successor(&entering));
        assert!(recovery.is_valid_successor(&recovery));

        // Backward arcs (rejected).
        assert!(!entering.is_valid_successor(&active));
        assert!(!maintenance.is_valid_successor(&entering));
        assert!(!recovery.is_valid_successor(&maintenance));
        assert!(!exiting.is_valid_successor(&entering));
        assert!(!drain_failed.is_valid_successor(&entering));
        // Active is only reachable from Recovery (or Active).
        assert!(!maintenance.is_valid_successor(&active));
        assert!(!entering.is_valid_successor(&active));
    }

    /// `ExitingMaintenance` and `DrainFailed` share the same
    /// `rank()`, so the pre-fix rank ladder admitted the
    /// backward arc `ExitingMaintenance → DrainFailed`. A
    /// delayed `MaintenanceTransitionObserved(DrainFailed)`
    /// arriving from chain replay would silently undo an
    /// operator's `ExitMaintenance { force=true }`. The
    /// match-table rejects that specific cross-rank arc.
    #[test]
    fn is_valid_successor_rejects_exiting_to_drain_failed() {
        let base = Instant::now();
        let exiting = MaintenanceState::ExitingMaintenance { since: base };
        let drain_failed = MaintenanceState::DrainFailed {
            since: base,
            reason: "late replay".into(),
        };
        assert!(
            !exiting.is_valid_successor(&drain_failed),
            "operator force-exit must not be regressed by a late DrainFailed observation",
        );
    }
}
