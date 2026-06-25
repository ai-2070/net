//! Rust SDK smoke test for the task-lifecycle (workflow) surface.
//!
//! Exercises the re-export chain in `sdk/src/cortex/workflow.rs`:
//! `Redex` → `WorkflowAdapter` → submit/start/advance/complete + reads.
//! If a public type or method disappears from the re-export, this test
//! stops compiling.

#![cfg(feature = "cortex")]

use net_sdk::cortex::workflow::{TaskStatus, WorkflowAdapter};
use net_sdk::cortex::Redex;

const ORIGIN: u64 = 0x0F10_5D01;

#[tokio::test]
async fn workflow_lifecycle_round_trip() {
    let redex = Redex::new();
    let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();

    wf.submit(1).unwrap();
    wf.start(1).unwrap();
    wf.advance(1).unwrap(); // step 0 → 1, attempts reset
    let seq = wf.complete(1).unwrap();
    wf.wait_for_seq(seq).await.unwrap();

    let st = wf.get(1).expect("task present");
    assert_eq!(st.status, TaskStatus::Done);
    assert!(st.status.is_terminal());
    assert_eq!(st.step, 1);

    assert_eq!(wf.status_counts().done, 1);
}

#[tokio::test]
async fn workflow_terminal_state_is_not_resurrected() {
    // The terminal-state guard is visible through the SDK: start/retry
    // after complete leave a `Done` task `Done`.
    let redex = Redex::new();
    let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();

    wf.submit(1).unwrap();
    wf.complete(1).unwrap();
    wf.start(1).unwrap();
    let seq = wf.retry(1).unwrap();
    wf.wait_for_seq(seq).await.unwrap();

    assert_eq!(wf.get(1).unwrap().status, TaskStatus::Done);
}

#[tokio::test]
async fn workflow_delete_reclaims_a_task() {
    let redex = Redex::new();
    let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();

    wf.submit(7).unwrap();
    let seq = wf.delete(7).unwrap();
    wf.wait_for_seq(seq).await.unwrap();
    assert!(wf.get(7).is_none());
}

// ---- Tier 2: shards + triggers --------------------------------------------

#[tokio::test]
async fn shards_fan_out_then_join_submits_the_reduce() {
    use net_sdk::cortex::workflow::{fan_out, try_join, Join, ShardGroup};

    let redex = Redex::new();
    let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();

    let group = ShardGroup::new(vec![10, 11, 12], 99); // reduce id = 99
    let seq = fan_out(&wf, &group).unwrap();
    wf.wait_for_seq(seq).await.unwrap();

    // Not all shards done yet → the reduce stays gated.
    assert_eq!(try_join(&wf, &group).unwrap(), Join::Pending);

    let mut last = 0;
    for s in [10, 11, 12] {
        last = wf.complete(s).unwrap();
    }
    wf.wait_for_seq(last).await.unwrap();

    // Every shard done → the reduce is submitted exactly once.
    match try_join(&wf, &group).unwrap() {
        Join::Submitted(s) => wf.wait_for_seq(s).await.unwrap(),
        other => panic!("expected Submitted, got {other:?}"),
    }
    assert!(wf.get(99).is_some(), "reduce task submitted");
    assert_eq!(try_join(&wf, &group).unwrap(), Join::AlreadySubmitted);
}

#[tokio::test]
async fn trigger_fires_dependent_when_predecessor_is_done() {
    use net_sdk::cortex::workflow::{Action, Trigger, TriggerEngine, TriggerWorld};

    let redex = Redex::new();
    let wf = WorkflowAdapter::open(&redex, ORIGIN).await.unwrap();
    let mut eng = TriggerEngine::new();
    eng.arm(Trigger::AfterTask(1), Action::Submit(2)); // B depends on A

    wf.submit(1).unwrap();
    wf.start(1).unwrap();
    let seq = wf.complete(1).unwrap();
    wf.wait_for_seq(seq).await.unwrap();

    let actions = {
        let state = wf.state();
        let guard = state.read();
        eng.on_task_change(1, &TriggerWorld::new(&guard, 0))
    };
    assert_eq!(actions, vec![Action::Submit(2)]);
    assert_eq!(eng.armed_count(), 0, "fired trigger is disarmed");
}
