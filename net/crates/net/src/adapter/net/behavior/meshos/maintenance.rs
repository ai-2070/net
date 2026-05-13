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
        /// Wall time the transition was entered.
        since: Instant,
        /// Optional deadline. Past this point the loop flips to
        /// [`MaintenanceState::DrainFailed`] if conditions
        /// haven't converged.
        deadline: Option<Instant>,
    },
    /// Steady-state isolation. Replicas migrated, daemons
    /// stopped. Operator commands run unimpeded.
    Maintenance {
        /// Wall time the steady state was entered.
        since: Instant,
    },
    /// Operator requested resume; this node is restarting
    /// daemons + re-emitting capabilities.
    ExitingMaintenance {
        /// Wall time the transition was entered.
        since: Instant,
    },
    /// Drain failed — replicas / daemons didn't converge before
    /// the deadline. Operator warning state until either the
    /// underlying condition resolves or an admin
    /// `ExitMaintenance { force = true }` lands.
    DrainFailed {
        /// Wall time the failure was recorded.
        since: Instant,
        /// Operator-readable reason.
        reason: String,
    },
    /// Recovery ramp-up window. Node is rejoined but on the
    /// avoid list for new placements until the window elapses.
    Recovery {
        /// Wall time the ramp-up started.
        since: Instant,
    },
}

impl MaintenanceState {
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
}
