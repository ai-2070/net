//! The scheduler-bridge runtime facade.
//!
//! [`SchedulerBridge`] owns the one piece of cross-tick mutable state the
//! projections share — the [`ClaimRegistry`] — and composes all five
//! projections behind a single surface the driver loop drives:
//!   - **claim lifecycle:** [`on_running`](SchedulerBridge::on_running)
//!     (`StepGate::Running`) / [`on_released`](SchedulerBridge::on_released)
//!     (`release_step`) maintain the registry;
//!   - **desired-down:** [`desired_intents`](SchedulerBridge::desired_intents)
//!     merges Projection 1 + 2 into the `DaemonIntentUpdate`s to publish;
//!   - **observed-up:**
//!     [`lifecycle_transition`](SchedulerBridge::lifecycle_transition)
//!     (Projection 3) maps a daemon signal to the workflow transition to
//!     apply;
//!   - **veto:** [`check_migration`](SchedulerBridge::check_migration)
//!     (Projection 5) gates a migration on the registry.
//!
//! It holds **no live handles** (no `MeshOsHandle` / `WorkflowAdapter` /
//! `MeshNode`): it takes the read state per call and returns values. The
//! driver loop owns the handles and does the I/O — publish the intents,
//! call `MeshNode::set_liveness_down`, apply the transitions via
//! `WorkflowAdapter`. That keeps this facade pure and testable, and keeps
//! the bridge from importing the runtime (plan LD 5).

use std::collections::HashMap;

use crate::adapter::net::behavior::fold::{IslandId, NodeId};
use crate::adapter::net::behavior::meshos::{DaemonIntentUpdate, DaemonLifecycleSignal, DaemonRef};
use crate::adapter::net::cortex::workflow::{ActiveClaim, TaskId, WorkflowState};

use super::claim_registry::ClaimRegistry;
use super::daemon_ref::daemon_ref;
use super::lifecycle::{
    apply_lifecycle, build_daemon_task_map, signal_implies_transition, LifecycleTransition,
};
use super::migration::{ClaimHeld, MigrationEligible};
use super::projection::{project_daemon_intents, project_forced_placements};

/// Compose Projection 1 (task → intent, `node: None`) and Projection 2
/// (claim → node-pin) into the single set of `DaemonIntentUpdate`s to
/// publish this tick — one intent per daemon, with claim-bearing daemons
/// pinned to their resolved host. Merging in-process (rather than
/// publishing both projections' outputs and relying on
/// `apply_daemon_intent`'s last-write-wins) emits one event per daemon,
/// not two. A claim whose daemon has no live task intent (stale) is
/// ignored; an island that no longer resolves leaves its daemon
/// run-anywhere (Projection 2 emits nothing for it). Output sorted by
/// daemon id.
pub fn desired_daemon_intents<F>(
    workflow: &WorkflowState,
    claims: &ClaimRegistry,
    resolve_host: F,
) -> Vec<DaemonIntentUpdate>
where
    F: Fn(IslandId) -> Option<NodeId>,
{
    // Projection 1 baseline, keyed by daemon for the overlay.
    let mut by_daemon: HashMap<DaemonRef, DaemonIntentUpdate> = project_daemon_intents(workflow)
        .into_iter()
        .map(|u| (u.daemon.clone(), u))
        .collect();
    // Projection 2 overlay: pin each claim-bearing daemon to its host by
    // setting only the `node` — Projection 1's Run/Stop is preserved, so a
    // claim held against a non-Running task stays Stop (pinned).
    for (daemon, host) in project_forced_placements(claims, resolve_host) {
        if let Some(intent) = by_daemon.get_mut(&daemon) {
            intent.node = Some(host);
        }
    }
    let mut out: Vec<DaemonIntentUpdate> = by_daemon.into_values().collect();
    out.sort_by_key(|u| u.daemon.id);
    out
}

/// The scheduler-bridge runtime facade — owns the [`ClaimRegistry`] and
/// composes the five projections. The driver loop holds one of these.
#[derive(Debug, Default)]
pub struct SchedulerBridge {
    claims: ClaimRegistry,
}

impl SchedulerBridge {
    /// A bridge with no claims held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `task`'s step is now `Running` holding `claim` — call
    /// when `drive_capability_step` returns `StepGate::Running(claim)`.
    pub fn on_running(&mut self, task: TaskId, claim: ActiveClaim) {
        self.claims.insert(daemon_ref(task), claim);
    }

    /// Clear `task`'s claim — call on `release_step`. Returns the
    /// released claim, if any. Idempotent.
    pub fn on_released(&mut self, task: TaskId) -> Option<ActiveClaim> {
        self.claims.remove(&daemon_ref(task))
    }

    /// The `DaemonIntentUpdate`s to publish this tick (Projection 1 + 2
    /// merged). `resolve_host` maps a claim's island to its host node
    /// (the driver supplies a closure over the `IslandTopology` fold).
    pub fn desired_intents<F>(
        &self,
        workflow: &WorkflowState,
        resolve_host: F,
    ) -> Vec<DaemonIntentUpdate>
    where
        F: Fn(IslandId) -> Option<NodeId>,
    {
        desired_daemon_intents(workflow, &self.claims, resolve_host)
    }

    /// Map a daemon lifecycle signal to the workflow transition to apply
    /// (Projection 3), or `None` for a system daemon / no-op signal. A
    /// signal that can't imply a transition (health / saturation reports,
    /// graceful exit) returns *before* the daemon→task map is built, so the
    /// common high-frequency signals cost nothing. For a transition-bearing
    /// signal the map is rebuilt from `workflow`; a driver processing a
    /// burst can instead call [`build_daemon_task_map`] once and use
    /// [`apply_lifecycle`] directly.
    pub fn lifecycle_transition(
        &self,
        signal: &DaemonLifecycleSignal,
        daemon: &DaemonRef,
        workflow: &WorkflowState,
    ) -> Option<LifecycleTransition> {
        if !signal_implies_transition(signal) {
            return None;
        }
        apply_lifecycle(signal, daemon, &build_daemon_task_map(workflow))
    }

    /// Gate a migration on the claim registry (Projection 5). `Ok` is the
    /// proof token that `daemon` holds no exclusive claim and may move;
    /// `Err(ClaimHeld)` is the veto.
    pub fn check_migration(&self, daemon: DaemonRef) -> Result<MigrationEligible, ClaimHeld> {
        MigrationEligible::check(daemon, &self.claims)
    }

    /// Borrow the claim registry read-only (diagnostics / tests).
    pub fn claims(&self) -> &ClaimRegistry {
        &self.claims
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::cortex::workflow::WorkflowAdapter;
    use crate::adapter::net::redex::Redex;

    #[test]
    fn claim_lifecycle_drives_the_migration_veto() {
        let mut bridge = SchedulerBridge::new();
        let d1 = daemon_ref(1);
        // Free daemon → migratable.
        assert!(bridge.check_migration(d1.clone()).is_ok());

        // Running with a claim → vetoed.
        bridge.on_running(1, ActiveClaim { island: 0xA0 });
        assert!(bridge.check_migration(d1.clone()).is_err());
        assert_eq!(bridge.claims().get(&d1).map(|c| c.island), Some(0xA0));

        // Released → migratable again.
        assert_eq!(bridge.on_released(1).map(|c| c.island), Some(0xA0));
        assert!(bridge.check_migration(d1).is_ok());
    }

    #[tokio::test]
    async fn desired_intents_pins_only_the_claim_holder() {
        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB06).await.unwrap();
        wf.submit(1).unwrap();
        wf.submit(2).unwrap();
        wf.start(1).unwrap();
        let seq = wf.start(2).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let mut bridge = SchedulerBridge::new();
        bridge.on_running(1, ActiveClaim { island: 0xA0 });
        let resolve = |island| (island == 0xA0).then_some(7);

        let intents = bridge.desired_intents(&wf.state().read(), resolve);
        let by_name: HashMap<String, &DaemonIntentUpdate> =
            intents.iter().map(|u| (u.daemon.name.clone(), u)).collect();
        // One intent per task; the claim-holder pinned, the other free.
        assert_eq!(intents.len(), 2);
        assert_eq!(by_name["task/1"].node, Some(7));
        assert!(by_name["task/2"].node.is_none());
    }

    #[tokio::test]
    async fn lifecycle_transition_maps_a_crash_to_failstep() {
        use std::time::Instant;

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x5C4E_DB07).await.unwrap();
        let seq = wf.submit(1).unwrap();
        wf.wait_for_seq(seq).await.unwrap();

        let bridge = SchedulerBridge::new();
        let crash = DaemonLifecycleSignal::Crashed {
            at: Instant::now(),
            reason: "oom".into(),
        };
        assert_eq!(
            bridge.lifecycle_transition(&crash, &daemon_ref(1), &wf.state().read()),
            Some(LifecycleTransition::FailStep(1)),
        );
    }
}
