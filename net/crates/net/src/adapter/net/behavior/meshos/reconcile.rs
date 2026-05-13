//! The pure-sync reconcile function. Locked decision #3:
//! `reconcile(actual, desired) -> Vec<MeshOsAction>` is
//! async-free, no I/O, no allocations beyond the returned
//! action list. Testable as a sync table-driven fixture.
//!
//! Phase A returns an empty action list under every input. Each
//! subsequent phase fills in one family of actions:
//!
//! - Phase B → `StartDaemon` / `StopDaemon` / `ApplyBackoff`
//! - Phase C → `PullReplica` / `DropReplica` / `RequestPlacement`
//!             / `RequestEviction`
//! - Phase D → `MarkAvoid` + scheduler-driven `RequestPlacement`
//! - Phase E → `CommitMaintenanceTransition` /
//!             `MigrateBlob` / `ReduceHeat`
//!
//! Reconcile is **idempotent**: calling it twice in a row with
//! the same `(actual, desired)` produces the same action list.
//! This is load-bearing for replay safety + cache key
//! generation, and the Phase A test asserts it.

use super::action::MeshOsAction;
use super::state::{DesiredState, MeshOsState};

/// Pure-sync diff over `(actual, desired)`. Returns the minimal
/// action list that would close the gap. Phase A returns
/// `vec![]` — the skeleton.
pub fn reconcile(actual: &MeshOsState, desired: &DesiredState) -> Vec<MeshOsAction> {
    // Suppress unused-variable warnings until later phases read
    // the inputs; the signature is the public contract from day
    // one.
    let _ = (actual, desired);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_empty_inputs_returns_no_actions() {
        let actual = MeshOsState::default();
        let desired = DesiredState::default();
        assert!(reconcile(&actual, &desired).is_empty());
    }

    #[test]
    fn reconcile_is_idempotent_under_repeated_calls() {
        // Property test: with the same inputs, two consecutive
        // reconcile passes produce the same output. Load-bearing
        // contract — the action executor's safety relies on
        // replay-with-no-side-effect.
        let actual = MeshOsState::default();
        let desired = DesiredState::default();
        let first = reconcile(&actual, &desired);
        let second = reconcile(&actual, &desired);
        assert_eq!(first, second);
    }

    #[test]
    fn reconcile_phase_a_returns_empty_even_with_nonempty_inputs() {
        // Phase A guarantees: regardless of what actual/desired
        // hold, no actions emerge. Tests that pre-Phase-B
        // consumers see a stable empty result.
        use std::time::Instant;

        use super::super::event::{ChainId, DaemonRef};
        use super::super::state::{AvoidEntry, BlobObservation, DaemonStatus};

        let mut actual = MeshOsState::default();
        actual.daemons.insert(
            DaemonRef {
                id: 1,
                name: "telemetry".into(),
            },
            DaemonStatus::default(),
        );
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
                reason: "rtt-degradation".into(),
                until: Instant::now() + std::time::Duration::from_secs(60),
            },
        );

        let mut desired = DesiredState::default();
        desired.desired_replicas.insert(0xCAFE_BABE as ChainId, 5);

        assert!(reconcile(&actual, &desired).is_empty());
    }
}
