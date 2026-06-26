//! Projection 3 — daemon lifecycle signal → workflow step transition.
//!
//! A daemon's lifecycle is the observed-up edge of the integration: the
//! supervisor reports `Started` / `Crashed` / `ExitedCleanly` for a
//! daemon, and the task that daemon runs must react. `Started` confirms
//! the task is `Running`; an abnormal `Crashed` fails the task's step,
//! which — applied via `WorkflowAdapter::fail` — drives the task to
//! `Failed`, firing the already-landed `Trigger::AfterTerminal` so the
//! failure-policy code retries the shard / cancels siblings / fails the
//! parent, and the worker's `release_step` returns the held claim.
//!
//! Two pure pieces here, no I/O and no call back into `meshos` /
//! `workflow` (plan LD 5):
//!   - [`build_daemon_task_map`] — the daemon→task reverse map. The
//!     `daemon_ref` encoding is one-way (splitmix64), so a lifecycle
//!     signal (which carries only a `DaemonRef`) needs this to recover
//!     the `TaskId`.
//!   - [`apply_lifecycle`] — maps `(signal, daemon)` to the
//!     [`LifecycleTransition`] it implies. The runtime applies the
//!     transition (that is what fires the trigger machinery); a system
//!     (non-task) daemon, or a signal that implies no transition,
//!     yields `None`.

use std::collections::HashMap;

use crate::adapter::net::behavior::meshos::{DaemonLifecycleSignal, DaemonRef};
use crate::adapter::net::cortex::workflow::{TaskId, WorkflowState};

use super::daemon_ref::daemon_ref;

/// The workflow transition a daemon lifecycle signal implies. A pure
/// description — the runtime applies it through `WorkflowAdapter`, which
/// is what writes the task chain and fires the `AfterTerminal` trigger /
/// failure policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleTransition {
    /// The daemon started — confirm the task is `Running`.
    ConfirmRunning(TaskId),
    /// The daemon crashed / exited abnormally — fail the task's step.
    /// Applying this (`wf.fail`) drives the task to `Failed`, firing the
    /// `AfterTerminal` trigger the failure policy consumes; the worker's
    /// `release_step` returns the held claim as part of step cleanup.
    FailStep(TaskId),
}

/// Build the daemon→task reverse map from the live workflow state — one
/// pass over the tasks, `daemon_ref(task) → task`. Cheap to rebuild each
/// tick. A lifecycle signal carries only a `DaemonRef`, and the
/// `daemon_ref` encoding can't be inverted, so this map is how
/// [`apply_lifecycle`] recovers the task.
pub fn build_daemon_task_map(workflow: &WorkflowState) -> HashMap<DaemonRef, TaskId> {
    workflow.all().map(|(id, _)| (daemon_ref(id), id)).collect()
}

/// Project a daemon lifecycle signal into the workflow transition it
/// implies (plan Projection 3):
/// - `Started` → confirm the task `Running`;
/// - `Crashed` → fail the task's step (→ `AfterTerminal` → failure
///   policy → claim release);
/// - `ExitedCleanly` → the expected terminal/cancel shutdown — no-op;
/// - `HealthChanged` / `SaturationChanged` → not lifecycle transitions —
///   no-op.
///
/// Returns `None` when `daemon` isn't a known *task* daemon (a system
/// daemon's signal — its ref isn't in `daemon_task`) or the signal
/// implies no transition. Pure: resolves daemon→task via `daemon_task`,
/// returns a value, never touches `meshos` / the workflow (LD 5).
pub fn apply_lifecycle(
    signal: &DaemonLifecycleSignal,
    daemon: &DaemonRef,
    daemon_task: &HashMap<DaemonRef, TaskId>,
) -> Option<LifecycleTransition> {
    let task = *daemon_task.get(daemon)?;
    match signal {
        DaemonLifecycleSignal::Started { .. } => Some(LifecycleTransition::ConfirmRunning(task)),
        DaemonLifecycleSignal::Crashed { .. } => Some(LifecycleTransition::FailStep(task)),
        // Graceful exit is expected for a terminal/cancelled task; health
        // and saturation reports aren't lifecycle transitions. Exhaustive
        // on the `#[non_exhaustive]` enum on purpose (in-crate): a new
        // variant breaks the build and forces a deliberate choice.
        DaemonLifecycleSignal::ExitedCleanly { .. }
        | DaemonLifecycleSignal::HealthChanged { .. }
        | DaemonLifecycleSignal::SaturationChanged { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn map_of(ids: &[TaskId]) -> HashMap<DaemonRef, TaskId> {
        ids.iter().map(|&id| (daemon_ref(id), id)).collect()
    }

    #[test]
    fn started_confirms_running_crashed_fails_clean_exit_is_noop() {
        let map = map_of(&[1]);
        let d = daemon_ref(1);
        let at = Instant::now();

        assert_eq!(
            apply_lifecycle(&DaemonLifecycleSignal::Started { at }, &d, &map),
            Some(LifecycleTransition::ConfirmRunning(1)),
        );
        assert_eq!(
            apply_lifecycle(
                &DaemonLifecycleSignal::Crashed {
                    at,
                    reason: "segfault".into(),
                },
                &d,
                &map,
            ),
            Some(LifecycleTransition::FailStep(1)),
        );
        assert_eq!(
            apply_lifecycle(&DaemonLifecycleSignal::ExitedCleanly { at }, &d, &map),
            None,
            "graceful exit is the expected terminal/cancel shutdown",
        );
    }

    #[test]
    fn a_system_daemon_signal_yields_no_transition() {
        // The daemon isn't in the task map → not a task daemon → ignored.
        let map = map_of(&[1, 2]);
        let other = DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        assert_eq!(
            apply_lifecycle(
                &DaemonLifecycleSignal::Crashed {
                    at: Instant::now(),
                    reason: "x".into(),
                },
                &other,
                &map,
            ),
            None,
        );
    }

    #[test]
    fn resolves_each_daemon_back_to_its_own_task() {
        let map = map_of(&[10, 20, 30]);
        let at = Instant::now();
        for id in [10u64, 20, 30] {
            assert_eq!(
                apply_lifecycle(
                    &DaemonLifecycleSignal::Started { at },
                    &daemon_ref(id),
                    &map
                ),
                Some(LifecycleTransition::ConfirmRunning(id)),
            );
        }
    }

    #[tokio::test]
    async fn build_map_recovers_tasks_from_live_workflow_state() {
        use crate::adapter::net::cortex::workflow::WorkflowAdapter;
        use crate::adapter::net::redex::Redex;

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB05).await.unwrap();
        wf.submit(1).unwrap();
        let seq = wf.submit(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let map = build_daemon_task_map(&wf.state().read());
        assert_eq!(map.get(&daemon_ref(1)), Some(&1));
        assert_eq!(map.get(&daemon_ref(2)), Some(&2));
        assert_eq!(map.len(), 2);
    }
}
