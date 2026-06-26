//! Projection 1 — workflow task state → desired daemon intents.

use crate::adapter::net::behavior::meshos::{DaemonIntent, DaemonIntentUpdate};
use crate::adapter::net::cortex::workflow::{TaskStatus, WorkflowState};

use super::daemon_ref::daemon_ref;

/// Project every live task into a desired daemon intent (integration
/// plan Projection 1). A `Running` task wants its worker daemon up
/// (`Run`); every other state wants it down (`Stop`) — a task parked
/// `Waiting` (e.g. it lost its claim), `Blocked`, or terminal
/// (`Done` / `Failed`) must not keep a daemon running. Emitting `Stop`
/// for a never-started task is a harmless no-op at reconcile
/// (`Stop` + `Stopped`/absent → no action); emitting it for a task that
/// fell back from `Running` is the tear-down that matters.
///
/// All intents are **claim-agnostic** here: `node` is `None`
/// (run anywhere). Projection 2 overlays the claim → node pin for
/// claim-bearing tasks.
///
/// Each task — including each shard, which is itself a standalone
/// `TaskId` — is keyed by [`daemon_ref`] on its own id. That id has no
/// attempt component, so a step retry maps to the same ref and produces
/// no reconcile diff (plan RD 1).
///
/// Pure: reads only `WorkflowState`, returns a value, performs no I/O,
/// and never calls back into `meshos` (plan LD 5). Output is sorted by
/// daemon id so callers and tests see a stable order regardless of the
/// underlying map's iteration order.
///
/// Deletion note: a deleted task vanishes from `WorkflowState`, so this
/// projection emits nothing for it. Tearing down a deleted task's
/// daemon is the wiring layer's concern (re-derive `desired_daemons`,
/// or emit an explicit `Stop` on the delete edge) — out of scope for
/// the pure projection.
pub fn project_daemon_intents(workflow: &WorkflowState) -> Vec<DaemonIntentUpdate> {
    let mut out: Vec<DaemonIntentUpdate> = workflow
        .all()
        .map(|(id, state)| DaemonIntentUpdate {
            daemon: daemon_ref(id),
            intent: if state.status == TaskStatus::Running {
                DaemonIntent::Run
            } else {
                DaemonIntent::Stop
            },
            node: None,
        })
        .collect();
    out.sort_by_key(|update| update.daemon.id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::cortex::workflow::WorkflowAdapter;
    use crate::adapter::net::redex::Redex;

    /// Drive a few tasks through the real fold and assert the Run/Stop
    /// mapping plus the claim-agnostic `node: None`.
    #[tokio::test]
    async fn running_projects_run_everything_else_projects_stop() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB01).await.unwrap();
        // Four tasks: 1 → Running, 2 → Done (terminal), 3 → Waiting,
        // 4 → stays Submitted.
        for id in 1..=4u64 {
            wf.submit(id).unwrap();
        }
        wf.start(1).unwrap(); // 1 → Running
        wf.start(2).unwrap();
        wf.complete(2).unwrap(); // 2 → Done
        let seq = wf.wait(3).unwrap(); // 3 → Waiting (4 untouched: Submitted)
        wf.wait_for_seq(seq).await.unwrap();

        let state = wf.state();
        let guard = state.read();
        let intents = project_daemon_intents(&guard);

        let by_name: std::collections::HashMap<String, &DaemonIntentUpdate> =
            intents.iter().map(|u| (u.daemon.name.clone(), u)).collect();
        assert_eq!(by_name["task/1"].intent, DaemonIntent::Run);
        assert_eq!(by_name["task/2"].intent, DaemonIntent::Stop, "terminal → Stop");
        assert_eq!(by_name["task/3"].intent, DaemonIntent::Stop, "Waiting → Stop");
        assert_eq!(
            by_name["task/4"].intent,
            DaemonIntent::Stop,
            "Submitted (never ran) → Stop (a harmless no-op at reconcile)",
        );
        assert!(
            intents.iter().all(|u| u.node.is_none()),
            "Projection 1 is claim-agnostic: every intent runs anywhere",
        );
    }

    /// Output is sorted by daemon id and contains exactly one intent per
    /// live task.
    #[tokio::test]
    async fn output_is_one_intent_per_task_in_stable_order() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB02).await.unwrap();
        for id in [42u64, 7, 99, 1] {
            wf.submit(id).unwrap();
        }
        let seq = wf.start(7).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let state = wf.state();
        let guard = state.read();
        let intents = project_daemon_intents(&guard);

        assert_eq!(intents.len(), 4, "one intent per live task");
        let ids: Vec<u64> = intents.iter().map(|u| u.daemon.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "intents are emitted in stable daemon-id order");
    }
}
