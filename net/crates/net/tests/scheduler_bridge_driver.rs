//! Integration: the `SchedulerBridgeDriver` end-to-end over live handles.
//!
//! Proves the driver's two glue paths the unit tests can't reach:
//!   - `tick()` computes the merged desired intents and publishes them
//!     into the MeshOS loop's event channel;
//!   - the lifecycle observer applies Projection 3 — a daemon crash fails
//!     the task and releases its claim.

#![cfg(all(feature = "cortex", feature = "meshos"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::meshos::event_loop::{MeshOsLoop, MeshOsLoopParts};
use net::adapter::net::behavior::meshos::MeshOsConfig;
use net::adapter::net::behavior::scheduler_bridge::{daemon_ref, SchedulerBridgeDriver};
use net::adapter::net::compute::DaemonLifecycleEvent;
use net::adapter::net::cortex::workflow::{ActiveClaim, TaskStatus, WorkflowAdapter};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x42u8; 32];

async fn build_mesh() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = MeshNodeConfig::new(addr, PSK);
    let kp = EntityKeypair::generate();
    Arc::new(MeshNode::new(kp, cfg).await.expect("MeshNode::new"))
}

#[tokio::test]
async fn driver_tick_publishes_and_a_crash_fails_the_task() {
    // MeshOS loop pieces — we don't run the loop; `try_publish` lands in
    // the buffered channel (the loop owns the receiver) and the initial
    // snapshot has no peers, which is all `tick()` needs here.
    let MeshOsLoopParts {
        mesh_loop: _mesh_loop,
        handle,
        actions_rx: _actions_rx,
        reader,
    } = MeshOsLoop::new(MeshOsConfig::default());

    let mesh = build_mesh().await;
    let redex = Redex::new();
    let wf = Arc::new(WorkflowAdapter::open(&redex, 0x00D2_1442).await.unwrap());

    // Task 1 runs, holding a claim.
    wf.submit(1).unwrap();
    let seq = wf.start(1).unwrap();
    wf.wait_for_seq(seq).await.unwrap();

    let driver = SchedulerBridgeDriver::new(wf.clone(), mesh.clone(), handle.clone(), reader);
    driver.on_running(1, ActiveClaim { island: 0xA0 });

    // tick(): one live task → one published intent; no peers → none down.
    let report = driver.tick();
    assert_eq!(
        report.published, 1,
        "the one Running task's intent is published"
    );
    assert_eq!(report.down, 0, "an empty snapshot marks no host down");

    // A daemon crash for task 1's daemon fails the step and releases the
    // claim (the observed-up path, Projection 3).
    let observer = driver.lifecycle_observer();
    let d = daemon_ref(1);
    observer.observe(DaemonLifecycleEvent::Crashed {
        id: d.id,
        name: d.name.clone(),
        at: Instant::now(),
        reason: "oom".into(),
    });

    // `wf.fail` folds asynchronously — poll until the task is terminal.
    let mut status = None;
    for _ in 0..100 {
        if let Some(st) = wf.get(1) {
            if st.status.is_terminal() {
                status = Some(st.status);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        status,
        Some(TaskStatus::Failed),
        "the crash drove the task Failed"
    );
    assert!(
        driver.on_released(1).is_none(),
        "the crash already released the claim",
    );
}

#[tokio::test]
async fn spawn_loop_runs_until_shutdown_then_stops_cleanly() {
    let MeshOsLoopParts {
        mesh_loop: _mesh_loop,
        handle,
        actions_rx: _actions_rx,
        reader,
    } = MeshOsLoop::new(MeshOsConfig::default());

    let mesh = build_mesh().await;
    let redex = Redex::new();
    let wf = Arc::new(WorkflowAdapter::open(&redex, 0x00D2_1443).await.unwrap());
    wf.submit(1).unwrap();
    let seq = wf.start(1).unwrap();
    wf.wait_for_seq(seq).await.unwrap();

    let driver = Arc::new(SchedulerBridgeDriver::new(wf.clone(), mesh, handle, reader));
    driver.on_running(1, ActiveClaim { island: 0xA0 });

    // The loop keeps ticking on its own clock until shutdown.
    let loop_handle = driver.clone().spawn(Duration::from_millis(5));
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !loop_handle.is_finished(),
        "the loop runs on its own until shutdown",
    );

    // Shutdown → the loop stops promptly and without panicking.
    driver.shutdown();
    tokio::time::timeout(Duration::from_secs(2), loop_handle)
        .await
        .expect("the spawned loop stops within 2s of shutdown")
        .expect("the loop task did not panic");
}
