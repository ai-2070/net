//! End-to-end integration tests for the MeshOS pipeline:
//! event source → [`MeshOsLoop`] → reconcile diff →
//! [`ActionExecutor`] → admit gate → [`ActionDispatcher`].
//!
//! Each phase has unit tests of its own; this file pins the
//! *contract between phases*. A regression where reconcile
//! emits the right action but admit drops it (or admit admits
//! it but the dispatcher never receives it) doesn't surface
//! in any single-phase test — only here.
//!
//! Run: `cargo test --features meshos --test meshos_pipeline`

#![cfg(feature = "meshos")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use net::adapter::net::behavior::meshos::{
    attach_to_daemon_registry, ActionExecutor, AdminEvent, ChainId, DaemonIntent,
    DaemonIntentUpdate, DaemonLifecycleSignal, DaemonRef, LocalReplicaIntent,
    LocalReplicaIntentUpdate, LoggingDispatcher, MaintenanceTransition, MeshOsAction,
    MeshOsConfig, MeshOsEvent, MeshOsLoop, NodeId,
};
use net::adapter::net::behavior::capability::CapabilityFilter;
use net::adapter::net::compute::{
    DaemonError, DaemonHostConfig, DaemonRegistry, MeshDaemon,
};
use net::adapter::net::compute::DaemonHost;
use net::adapter::net::state::causal::CausalEvent;
use net::adapter::net::EntityKeypair;

const THIS_NODE: NodeId = 100;

fn fast_config() -> MeshOsConfig {
    MeshOsConfig {
        this_node: THIS_NODE,
        tick_interval: Duration::from_millis(20),
        event_queue_capacity: 64,
        action_queue_capacity: 64,
        backpressure: Default::default(),
        locality: Default::default(),
        maintenance: Default::default(),
    }
}

fn daemon(name: &str, id: u64) -> DaemonRef {
    DaemonRef {
        id,
        name: name.into(),
    }
}

/// Bring up the full pipeline and return the handle to publish
/// events, plus the dispatcher to inspect what actions made it
/// through, plus the loop's join handle (so the test can
/// `Shutdown` and assert clean exit).
fn spawn_pipeline(
    cfg: MeshOsConfig,
) -> (
    net::adapter::net::behavior::meshos::MeshOsHandle,
    Arc<LoggingDispatcher>,
    tokio::task::JoinHandle<u64>,
    tokio::task::JoinHandle<Arc<net::adapter::net::behavior::meshos::ExecutorStats>>,
) {
    let (mesh_loop, handle, actions_rx) = MeshOsLoop::new(cfg.clone());
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let exec = ActionExecutor::new(actions_rx, Arc::new(cfg), Arc::clone(&dispatcher));
    let loop_task = tokio::spawn(mesh_loop.run());
    let exec_task = tokio::spawn(exec.run());
    (handle, dispatcher, loop_task, exec_task)
}

/// Tear down the pipeline cleanly + return the dispatcher log.
async fn drain_pipeline(
    handle: net::adapter::net::behavior::meshos::MeshOsHandle,
    dispatcher: Arc<LoggingDispatcher>,
    loop_task: tokio::task::JoinHandle<u64>,
    exec_task: tokio::task::JoinHandle<Arc<net::adapter::net::behavior::meshos::ExecutorStats>>,
    settle: Duration,
) -> Vec<MeshOsAction> {
    // Let pending ticks fire + actions flow through admit + dispatcher.
    tokio::time::sleep(settle).await;
    handle.publish(MeshOsEvent::Shutdown).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_task)
        .await
        .expect("loop did not exit");
    // Drop the handle so the executor's mpsc receiver returns
    // None and exits.
    drop(handle);
    let _ = tokio::time::timeout(Duration::from_secs(2), exec_task)
        .await
        .expect("executor did not exit");
    dispatcher.log()
}

#[tokio::test]
async fn daemon_intent_run_flows_through_to_start_daemon_dispatch() {
    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    let d = daemon("telemetry", 1);
    handle
        .publish(MeshOsEvent::DaemonIntentUpdate(DaemonIntentUpdate {
            daemon: d.clone(),
            intent: DaemonIntent::Run,
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(150),
    )
    .await;

    let started: Vec<&DaemonRef> = log
        .iter()
        .filter_map(|a| match a {
            MeshOsAction::StartDaemon { daemon } => Some(daemon),
            _ => None,
        })
        .collect();
    assert!(
        started.iter().any(|d2| **d2 == d),
        "expected StartDaemon({d:?}) in dispatcher log; got {log:?}",
    );
}

#[tokio::test]
async fn daemon_intent_stop_flows_through_only_after_a_start_was_seen() {
    // Seed: tell the loop the daemon is Running (via a
    // DaemonLifecycle::Started signal). Then intent=Stop. The
    // diff should emit StopDaemon.
    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    let d = daemon("telemetry", 2);
    handle
        .publish(MeshOsEvent::DaemonLifecycle {
            daemon: d.clone(),
            signal: DaemonLifecycleSignal::Started { at: Instant::now() },
        })
        .await
        .unwrap();
    handle
        .publish(MeshOsEvent::DaemonIntentUpdate(DaemonIntentUpdate {
            daemon: d.clone(),
            intent: DaemonIntent::Stop,
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(150),
    )
    .await;

    assert!(
        log.iter().any(|a| matches!(
            a,
            MeshOsAction::StopDaemon { daemon, .. } if *daemon == d
        )),
        "expected StopDaemon({d:?}) in dispatcher log; got {log:?}",
    );
}

#[tokio::test]
async fn local_replica_intent_hold_with_known_holder_emits_pull_replica() {
    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    let chain: ChainId = 0xCAFE;
    // Seed: some peer holds the chain.
    handle
        .publish(MeshOsEvent::ReplicaUpdate(
            net::adapter::net::behavior::meshos::ReplicaUpdate::Added { chain, holder: 7 },
        ))
        .await
        .unwrap();
    handle
        .publish(MeshOsEvent::LocalReplicaIntent(LocalReplicaIntentUpdate {
            chain,
            intent: LocalReplicaIntent::Hold,
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(150),
    )
    .await;

    assert!(
        log.iter().any(|a| matches!(
            a,
            MeshOsAction::PullReplica { chain: c, source: 7 } if *c == chain
        )),
        "expected PullReplica(chain={chain:#x}, source=7) in log; got {log:?}",
    );
}

#[tokio::test]
async fn admin_drop_replicas_translates_to_drop_replica_dispatch() {
    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    let chain: ChainId = 0xBEEF;
    // Seed: this node holds the chain.
    handle
        .publish(MeshOsEvent::ReplicaUpdate(
            net::adapter::net::behavior::meshos::ReplicaUpdate::Added {
                chain,
                holder: THIS_NODE,
            },
        ))
        .await
        .unwrap();
    // Admin commits: drop these chains.
    handle
        .publish(MeshOsEvent::AdminEvent(AdminEvent::DropReplicas {
            node: THIS_NODE,
            chains: vec![chain],
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(150),
    )
    .await;

    assert!(
        log.iter().any(|a| matches!(
            a,
            MeshOsAction::DropReplica { chain: c } if *c == chain
        )),
        "expected DropReplica(chain={chain:#x}) in log; got {log:?}",
    );
}

#[tokio::test]
async fn maintenance_enter_with_empty_workload_walks_to_steady_state() {
    // Enter maintenance on a fresh loop (no replicas, no daemons).
    // The diff should immediately emit CommitMaintenanceTransition
    // → Maintenance (conditions met — nothing to drain).
    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    handle
        .publish(MeshOsEvent::AdminEvent(AdminEvent::EnterMaintenance {
            node: THIS_NODE,
            deadline: None,
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(150),
    )
    .await;

    assert!(
        log.iter().any(|a| matches!(
            a,
            MeshOsAction::CommitMaintenanceTransition {
                node, target: MaintenanceTransition::Maintenance,
            } if *node == THIS_NODE
        )),
        "expected CommitMaintenanceTransition(target=Maintenance) in log; got {log:?}",
    );
}

/// Minimal daemon for source-converter integration test —
/// stateless, no-op `process`, named for `MeshDaemon::name`.
struct NoopDaemon {
    name: String,
}

impl MeshDaemon for NoopDaemon {
    fn name(&self) -> &str {
        &self.name
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn registry_lifecycle_observer_flows_to_meshos_dispatcher() {
    // End-to-end: install the MeshOS lifecycle sink on a real
    // DaemonRegistry, register a daemon, then publish a
    // matching DaemonIntentUpdate(Run). The registry fires
    // `Registered`, the sink translates to
    // DaemonLifecycleSignal::Started, the loop folds it into
    // `lifecycle = Running`, and reconcile's daemon diff
    // emits nothing (already in desired state).
    //
    // The interesting bit: without the sink the loop would
    // see intent=Run + actual=Stopped (the default) and emit
    // StartDaemon — which would be wrong, since the daemon is
    // already up. The sink's job is to keep the loop's actual
    // state in sync with reality.

    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    let registry = DaemonRegistry::new();
    let _prior = attach_to_daemon_registry(&registry, handle.clone());

    let kp = EntityKeypair::generate();
    let daemon_id = kp.origin_hash();
    let host = DaemonHost::new(
        Box::new(NoopDaemon {
            name: "watcher".into(),
        }),
        kp,
        DaemonHostConfig::default(),
    );
    registry.register(host).unwrap();

    // Tell MeshOS this daemon should be running. The id +
    // name must match what the registry's `Registered` event
    // carried.
    handle
        .publish(MeshOsEvent::DaemonIntentUpdate(DaemonIntentUpdate {
            daemon: DaemonRef {
                id: daemon_id,
                name: "watcher".into(),
            },
            intent: DaemonIntent::Run,
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(200),
    )
    .await;

    // No StartDaemon should appear — the daemon's lifecycle
    // already reads Running thanks to the sink.
    let leaked: Vec<&DaemonRef> = log
        .iter()
        .filter_map(|a| match a {
            MeshOsAction::StartDaemon { daemon } => Some(daemon),
            _ => None,
        })
        .filter(|d| d.id == daemon_id)
        .collect();
    assert!(
        leaked.is_empty(),
        "sink failed to surface the lifecycle to the loop; got StartDaemon leaks: {leaked:?}",
    );
}

#[tokio::test]
async fn unregister_flows_through_to_actual_stopped_lifecycle() {
    // Now flip the test: register the daemon AND publish
    // intent=Run, then unregister. The actual lifecycle
    // transitions Running → Stopped via the sink; intent is
    // still Run; reconcile emits StartDaemon. End-to-end
    // observation that the unregister side of the sink fires.
    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);

    let registry = DaemonRegistry::new();
    let _prior = attach_to_daemon_registry(&registry, handle.clone());

    let kp = EntityKeypair::generate();
    let daemon_id = kp.origin_hash();
    let host = DaemonHost::new(
        Box::new(NoopDaemon {
            name: "watcher".into(),
        }),
        kp,
        DaemonHostConfig::default(),
    );
    registry.register(host).unwrap();
    handle
        .publish(MeshOsEvent::DaemonIntentUpdate(DaemonIntentUpdate {
            daemon: DaemonRef {
                id: daemon_id,
                name: "watcher".into(),
            },
            intent: DaemonIntent::Run,
        }))
        .await
        .unwrap();
    // Let the Registered event flow + the loop process it.
    tokio::time::sleep(Duration::from_millis(80)).await;
    // Now unregister — the sink fires ExitedCleanly →
    // lifecycle Stopped → reconcile sees Run + Stopped →
    // StartDaemon.
    registry.unregister(daemon_id).unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(200),
    )
    .await;

    assert!(
        log.iter().any(|a| matches!(
            a,
            MeshOsAction::StartDaemon { daemon } if daemon.id == daemon_id
        )),
        "expected StartDaemon for id={daemon_id:#x} after unregister; got {log:?}",
    );
}

#[tokio::test]
async fn admit_gates_held_daemon_so_dispatcher_never_sees_a_start() {
    // Reconcile WILL emit StartDaemon for an intent=Run daemon
    // whose state is Stopped + backoff is Idle. To exercise the
    // gate end-to-end we need to crash-loop the daemon first so
    // its BackoffTracker advances. Then intent=Run + admit gates.

    let cfg = fast_config();
    let (handle, dispatcher, loop_task, exec_task) = spawn_pipeline(cfg);
    let d = daemon("flap", 3);

    // Crash the daemon 5 times within the rolling window → CrashLooping gate.
    let now = Instant::now();
    for i in 0..5 {
        handle
            .publish(MeshOsEvent::DaemonLifecycle {
                daemon: d.clone(),
                signal: DaemonLifecycleSignal::Crashed {
                    at: now + Duration::from_millis(i * 10),
                    reason: "test".into(),
                },
            })
            .await
            .unwrap();
    }
    // Intent=Run with the gate held. Reconcile emits ApplyBackoff
    // (not StartDaemon — the supervision gate observed inside
    // reconcile catches it). admit also gates if the
    // backpressure.record_daemon_gate were set, but the loop
    // doesn't propagate that automatically. Either way, no
    // StartDaemon should appear.
    handle
        .publish(MeshOsEvent::DaemonIntentUpdate(DaemonIntentUpdate {
            daemon: d.clone(),
            intent: DaemonIntent::Run,
        }))
        .await
        .unwrap();

    let log = drain_pipeline(
        handle,
        dispatcher,
        loop_task,
        exec_task,
        Duration::from_millis(150),
    )
    .await;

    assert!(
        !log.iter().any(|a| matches!(
            a,
            MeshOsAction::StartDaemon { daemon: d2 } if *d2 == d
        )),
        "StartDaemon leaked through despite crash-loop gate; got {log:?}",
    );
}
