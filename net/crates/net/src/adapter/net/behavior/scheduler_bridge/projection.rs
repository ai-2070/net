//! Projection 1 — workflow task state → desired daemon intents.

use crate::adapter::net::behavior::fold::IslandId;
use crate::adapter::net::behavior::meshos::{DaemonIntent, DaemonIntentUpdate, DaemonRef, NodeId};
use crate::adapter::net::cortex::workflow::{TaskStatus, WorkflowState};

use super::claim_registry::ClaimRegistry;
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

/// Project the held exclusive claims into daemon→host node pins
/// (integration plan Projection 2 — the forced-placement seam). For each
/// daemon holding an `ActiveClaim`, resolve the claim's island to its host
/// node and emit a `(daemon, host)` pin, so the daemon runs on exactly the
/// claimed node and the drift scorer never places it (plan LD 1).
///
/// `resolve_host` maps an island id to its current host — the wiring
/// supplies a closure over the `IslandTopology` fold's
/// `IslandQuery::Get`. Keeping it a closure leaves this projection pure
/// and unit-testable without dragging the fold in.
///
/// If an island no longer resolves (its host died and the island aged
/// out of the topology — see Projection 4), the claim is stale: emit
/// nothing for it. The task loses its claim at TTL and re-claims
/// elsewhere; pinning a daemon to a vanished node would only produce an
/// unschedulable intent.
///
/// Returns only the **pins**, not full `DaemonIntentUpdate`s: a claim
/// constrains *where* a daemon runs, never *whether* it runs — that is
/// Projection 1's call. [`super::desired_daemon_intents`] overlays each pin
/// onto Projection 1's intent by setting *only* its `node`, so a claim held
/// against a non-`Running` task still merges to `Stop` (pinned), never a
/// spurious `Run`. (An earlier form returned `Run`-pinned updates and
/// relied on `apply_daemon_intent`'s last-write-wins to overlay them; that
/// clobbered Projection 1's `Stop` for a non-`Running` held claim, so the
/// pin is now intent-free by construction.) Output is sorted by daemon id
/// for a stable order.
pub fn project_forced_placements<F>(
    claims: &ClaimRegistry,
    resolve_host: F,
) -> Vec<(DaemonRef, NodeId)>
where
    F: Fn(IslandId) -> Option<NodeId>,
{
    let mut out: Vec<(DaemonRef, NodeId)> = claims
        .iter()
        .filter_map(|(daemon, claim)| resolve_host(claim.island).map(|host| (daemon.clone(), host)))
        .collect();
    out.sort_by_key(|(daemon, _)| daemon.id);
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
        assert_eq!(
            by_name["task/2"].intent,
            DaemonIntent::Stop,
            "terminal → Stop"
        );
        assert_eq!(
            by_name["task/3"].intent,
            DaemonIntent::Stop,
            "Waiting → Stop"
        );
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

    #[test]
    fn forced_placements_pin_claim_bearing_daemons_to_their_resolved_host() {
        use crate::adapter::net::cortex::workflow::ActiveClaim;

        let mut claims = ClaimRegistry::new();
        claims.insert(daemon_ref(1), ActiveClaim { island: 0xA0 });
        claims.insert(daemon_ref(2), ActiveClaim { island: 0xB0 });
        // island 0xA0 lives on node 7; 0xB0 on node 9.
        let resolve = |island| match island {
            0xA0 => Some(7),
            0xB0 => Some(9),
            _ => None,
        };

        let by_name: std::collections::HashMap<String, NodeId> =
            project_forced_placements(&claims, resolve)
                .into_iter()
                .map(|(daemon, host)| (daemon.name, host))
                .collect();
        assert_eq!(by_name["task/1"], 7, "pinned to its island's host");
        assert_eq!(by_name["task/2"], 9);
    }

    #[test]
    fn forced_placement_skips_a_claim_whose_island_vanished() {
        use crate::adapter::net::cortex::workflow::ActiveClaim;

        let mut claims = ClaimRegistry::new();
        claims.insert(daemon_ref(1), ActiveClaim { island: 0xDEAD });
        // The host died and the island aged out of the topology.
        let placements = project_forced_placements(&claims, |_| None);
        assert!(
            placements.is_empty(),
            "a stale claim with no resolvable host produces no intent",
        );
    }

    /// End-to-end Phase A composition: Projection 1 marks every task
    /// `node: None`; Projection 2 overlays `node: Some(host)` for the
    /// claim-holder; `apply_daemon_intent` (last-write-wins) leaves
    /// exactly the claim-bearing daemon pinned and the rest run-anywhere.
    #[tokio::test]
    async fn projection1_then_projection2_overlay_pins_only_the_claim_holder() {
        use super::super::desired_daemon_intents;
        use crate::adapter::net::behavior::meshos::state::DesiredState;
        use crate::adapter::net::cortex::workflow::ActiveClaim;

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB03).await.unwrap();
        wf.submit(1).unwrap();
        wf.submit(2).unwrap();
        wf.start(1).unwrap(); // task 1 Running, holds a claim
        let seq = wf.start(2).unwrap(); // task 2 Running, no claim
        wf.wait_for_seq(seq).await.unwrap();

        let mut claims = ClaimRegistry::new();
        claims.insert(daemon_ref(1), ActiveClaim { island: 0xA0 });
        let resolve = |island| if island == 0xA0 { Some(7) } else { None };

        // The canonical composition: the in-process merge applied to a
        // DesiredState (Projection 1 baseline + Projection 2 pin overlay).
        let mut desired = DesiredState::default();
        for intent in desired_daemon_intents(&wf.state().read(), &claims, resolve) {
            desired.apply_daemon_intent(&intent);
        }

        // Task 1's daemon is pinned to node 7; task 2's stays run-anywhere.
        assert_eq!(desired.desired_daemon_nodes.get(&daemon_ref(1)), Some(&7));
        assert!(
            !desired.desired_daemon_nodes.contains_key(&daemon_ref(2)),
            "the non-claim task is never pinned",
        );
    }

    /// Phase A acceptance (the plan's "Done when", start half): a
    /// `Running(ActiveClaim)` task, projected through both projections
    /// into `DesiredState`, makes reconcile emit `StartDaemon` on the
    /// claim's node and on NO other node — entirely through
    /// `DesiredState`, no workflow→dispatcher call in the path.
    #[tokio::test]
    async fn running_claim_starts_its_daemon_on_the_claim_node_only() {
        use super::super::desired_daemon_intents;
        use crate::adapter::net::behavior::meshos::state::{DesiredState, MeshOsState};
        use crate::adapter::net::behavior::meshos::{
            reconcile, LocalityConfig, MaintenanceConfig, MeshOsAction, SchedulerConfig,
        };
        use crate::adapter::net::cortex::workflow::{ActiveClaim, WorkflowAdapter};

        const CLAIM_HOST: NodeId = 7;
        const OTHER_NODE: NodeId = 99;

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB04).await.unwrap();
        wf.submit(1).unwrap();
        let seq = wf.start(1).unwrap(); // task 1 → Running
        wf.wait_for_seq(seq).await.unwrap();

        let mut claims = ClaimRegistry::new();
        claims.insert(daemon_ref(1), ActiveClaim { island: 0xA0 });

        // Merge both projections into one DesiredState (the canonical path).
        let mut desired = DesiredState::default();
        {
            let state = wf.state();
            let guard = state.read();
            for intent in desired_daemon_intents(&guard, &claims, |island| {
                (island == 0xA0).then_some(CLAIM_HOST)
            }) {
                desired.apply_daemon_intent(&intent);
            }
        }

        // Daemon absent in actual state → a Run intent yields StartDaemon.
        let actual = MeshOsState::default();
        let (loc, maint, sched) = (
            LocalityConfig::default(),
            MaintenanceConfig::default(),
            SchedulerConfig::default(),
        );

        let on_claim_host = reconcile(&actual, &desired, CLAIM_HOST, &loc, &maint, &sched, None);
        assert!(
            on_claim_host.iter().any(
                |a| matches!(a, MeshOsAction::StartDaemon { daemon } if *daemon == daemon_ref(1)),
            ),
            "the claim's node starts the pinned daemon; got {on_claim_host:?}",
        );

        let elsewhere = reconcile(&actual, &desired, OTHER_NODE, &loc, &maint, &sched, None);
        assert!(
            !elsewhere
                .iter()
                .any(|a| matches!(a, MeshOsAction::StartDaemon { .. })),
            "a non-claim node never starts the pinned daemon; got {elsewhere:?}",
        );
    }

    /// A claim still held against a task that is NOT `Running` (e.g. the
    /// task failed before `release_step` cleared the registry) must merge
    /// to a `Stop` pinned to the host — never a spurious `Run`. The pin
    /// sets only the node; Projection 1 owns the Run/Stop call. (Regression
    /// for the old `Run`-pinned overlay, which let a stale claim resurrect
    /// a terminal task's daemon.)
    #[tokio::test]
    async fn held_claim_on_a_non_running_task_merges_to_stop_pinned() {
        use super::super::desired_daemon_intents;
        use crate::adapter::net::cortex::workflow::ActiveClaim;

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB08).await.unwrap();
        wf.submit(1).unwrap();
        wf.start(1).unwrap(); // 1 → Running
        let seq = wf.complete(1).unwrap(); // 1 → Done (terminal, not Running)
        wf.wait_for_seq(seq).await.unwrap();

        // The registry still holds task 1's claim (release lagged).
        let mut claims = ClaimRegistry::new();
        claims.insert(daemon_ref(1), ActiveClaim { island: 0xA0 });
        let resolve = |island| (island == 0xA0).then_some(7);

        let intents = desired_daemon_intents(&wf.state().read(), &claims, resolve);
        let one = intents
            .iter()
            .find(|u| u.daemon == daemon_ref(1))
            .expect("task 1 has an intent");
        assert_eq!(
            one.intent,
            DaemonIntent::Stop,
            "a non-Running held claim still projects Stop",
        );
        assert_eq!(one.node, Some(7), "but is still pinned to the claim host");
    }
}
