//! Live agent-to-agent task handoff over the mesh — the networked half of
//! Hermes V2 Phase 3.
//!
//! The wire is **direct-addressed nRPC**, reusing the mesh's own handshake and
//! request/response path (the same idiom as `mesh_enroll`):
//!
//! * **Executor** — [`Mesh::serve_a2a`] registers three services backed by a
//!   [`TaskRegistry`] + a host [`TaskExecutor`]: [`A2A_TASK_SERVICE`] accepts a
//!   [`TaskBrief`] and spawns the executor (returning a [`TaskAck`]),
//!   [`A2A_STATUS_SERVICE`] answers a task's [`TaskRecord`], and
//!   [`A2A_CANCEL_SERVICE`] trips its cancel token. Its running dispatch loop
//!   answers routed handshakes from any in-root peer — zero pairing ceremony.
//! * **Requester** — [`Mesh::submit_task`] / [`Mesh::task_status`] /
//!   [`Mesh::cancel_task`] `call` those services by the executor's node id. The
//!   requester submits, keeps working, polls, and cancels — and the executor
//!   **demonstrably stops** (cooperative cancellation, `a2a`).
//!
//! In-root reachability is assumed (both agents are enrolled on the same mesh);
//! the caller connects to the executor node the usual way (`connect_via` for a
//! routed handshake) before submitting. A2A is for **parallelism** — briefs
//! carry Datafort context refs and results come back as artifact refs, because
//! the executor doesn't share the requester's memory.

use std::sync::Arc;

use crate::a2a::{TaskAck, TaskBrief, TaskExecutor, TaskRecord, TaskRegistry};
use crate::mesh::Mesh;
use crate::mesh_rpc::{CallOptionsTyped, Codec, ServeError, ServeHandle};

/// The nRPC service an executor serves to accept a [`TaskBrief`] (submit).
pub const A2A_TASK_SERVICE: &str = "net.a2a.task";
/// The nRPC service an executor serves to answer a task's [`TaskRecord`].
pub const A2A_STATUS_SERVICE: &str = "net.a2a.status";
/// The nRPC service an executor serves to cancel a task.
pub const A2A_CANCEL_SERVICE: &str = "net.a2a.cancel";

/// Errors from the requester-side A2A flow.
#[derive(Debug, thiserror::Error)]
pub enum A2aFlowError {
    /// Dialing the executor or calling a service failed.
    #[error("a2a transport failed: {0}")]
    Transport(String),
    /// A response could not be decoded.
    #[error("a2a decode error: {0}")]
    Decode(String),
}

/// Encode a task id as a request body (a JSON string). One place so the status
/// and cancel services agree with their callers.
fn task_ref_bytes(task_id: &str) -> Vec<u8> {
    serde_json::to_vec(task_id).unwrap_or_default()
}

impl Mesh {
    /// **Executor side.** Serve the three A2A services backed by `registry` +
    /// `executor`: accept briefs (spawning the executor), answer status, and
    /// cancel. Returns the [`ServeHandle`]s — hold them for as long as this
    /// agent should accept tasks; dropping them unregisters the services. This
    /// node must be `start()`ed.
    ///
    /// Rollback is automatic: if serving a later service fails, the already-
    /// registered handles drop (unregistering) as the error returns.
    pub fn serve_a2a(
        &self,
        registry: TaskRegistry,
        executor: Arc<dyn TaskExecutor>,
    ) -> Result<Vec<ServeHandle>, ServeError> {
        let submit = {
            let registry = registry.clone();
            self.serve_rpc_typed(A2A_TASK_SERVICE, Codec::Json, move |req: Vec<u8>| {
                let registry = registry.clone();
                let executor = Arc::clone(&executor);
                async move {
                    // Never fails out of band: a malformed brief answers a
                    // `TaskAck { accepted: false }` the requester reads.
                    let ack = match TaskBrief::decode(&req) {
                        Ok(brief) => {
                            let task_id = registry.submit(brief, executor);
                            TaskAck {
                                task_id,
                                accepted: true,
                                reason: None,
                            }
                        }
                        Err(e) => TaskAck {
                            task_id: String::new(),
                            accepted: false,
                            reason: Some(e.to_string()),
                        },
                    };
                    Ok::<Vec<u8>, String>(ack.encode())
                }
            })?
        };

        let status = {
            let registry = registry.clone();
            self.serve_rpc_typed(A2A_STATUS_SERVICE, Codec::Json, move |req: Vec<u8>| {
                let registry = registry.clone();
                async move {
                    let task_id: String = serde_json::from_slice(&req).unwrap_or_default();
                    // `Option<TaskRecord>` → JSON (null when unknown).
                    let record: Option<TaskRecord> = registry.record(&task_id);
                    Ok::<Vec<u8>, String>(serde_json::to_vec(&record).unwrap_or_default())
                }
            })?
        };

        let cancel =
            self.serve_rpc_typed(A2A_CANCEL_SERVICE, Codec::Json, move |req: Vec<u8>| {
                let registry = registry.clone();
                async move {
                    let task_id: String = serde_json::from_slice(&req).unwrap_or_default();
                    let cancelled = registry.cancel(&task_id);
                    Ok::<Vec<u8>, String>(serde_json::to_vec(&cancelled).unwrap_or_default())
                }
            })?;

        Ok(vec![submit, status, cancel])
    }

    /// **Requester side.** Hand `brief` to the executor at `target_node_id` and
    /// return its [`TaskAck`]. Non-blocking on the executor: the task runs
    /// async on the far side while this agent keeps working. The caller must
    /// already be connected to `target_node_id` (an in-root peer — dial it with
    /// `connect_via` first if needed).
    pub async fn submit_task(
        &self,
        target_node_id: u64,
        brief: &TaskBrief,
    ) -> Result<TaskAck, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_TASK_SERVICE,
                &brief.encode(),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        TaskAck::decode(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** The executor's current [`TaskRecord`] for `task_id`
    /// (state + brief + last-update time), or `None` if the executor doesn't
    /// know it.
    pub async fn task_status(
        &self,
        target_node_id: u64,
        task_id: &str,
    ) -> Result<Option<TaskRecord>, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_STATUS_SERVICE,
                &task_ref_bytes(task_id),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        serde_json::from_slice(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }

    /// **Requester side.** Cancel `task_id` on the executor. Returns whether the
    /// executor had it in flight (a terminal / unknown task returns `false`).
    /// The executor's cooperative cancellation stops the work; poll
    /// [`task_status`](Self::task_status) to observe the `Cancelled` state.
    pub async fn cancel_task(
        &self,
        target_node_id: u64,
        task_id: &str,
    ) -> Result<bool, A2aFlowError> {
        let response: Vec<u8> = self
            .call_typed(
                target_node_id,
                A2A_CANCEL_SERVICE,
                &task_ref_bytes(task_id),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| A2aFlowError::Transport(format!("call: {e}")))?;
        serde_json::from_slice(&response).map_err(|e| A2aFlowError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::{CancelToken, TaskState};
    use crate::mesh::MeshBuilder;
    use crate::Identity;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// A test executor: either completes with a fixed ref, or waits for cancel.
    struct TestExecutor {
        result: String,
        wait_for_cancel: bool,
        saw_cancel: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TaskExecutor for TestExecutor {
        async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
            if self.wait_for_cancel {
                cancel.cancelled().await;
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Err("stopped".to_string());
            }
            Ok(self.result.clone())
        }
    }

    async fn build_started(psk: &[u8; 32]) -> Mesh {
        let mesh = MeshBuilder::new("127.0.0.1:0", psk)
            .unwrap()
            .identity(Identity::generate())
            .build()
            .await
            .unwrap();
        mesh.start();
        mesh
    }

    /// Poll the executor's status for `task_id` until it satisfies `pred`.
    async fn wait_status(
        requester: &Mesh,
        executor_id: u64,
        task_id: &str,
        pred: impl Fn(&TaskState) -> bool,
    ) -> TaskState {
        for _ in 0..100 {
            if let Ok(Some(rec)) = requester.task_status(executor_id, task_id).await {
                if pred(&rec.state) {
                    return rec.state;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("task {task_id} never satisfied the status predicate");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_requester_submits_a_task_and_cancels_it_mid_run() {
        let psk = [0x61u8; 32];
        let executor_mesh = build_started(&psk).await;
        let saw = Arc::new(AtomicBool::new(false));
        let _handles = executor_mesh
            .serve_a2a(
                TaskRegistry::new(),
                Arc::new(TestExecutor {
                    result: String::new(),
                    wait_for_cancel: true,
                    saw_cancel: Arc::clone(&saw),
                }),
            )
            .expect("serve a2a");

        let requester = build_started(&psk).await;
        requester
            .connect_via(
                &executor_mesh.local_addr().to_string(),
                executor_mesh.public_key(),
                executor_mesh.node_id(),
            )
            .await
            .expect("connect to the executor");
        let exec_id = executor_mesh.node_id();

        // Hand off a long job — context rides as a Datafort ref.
        let brief = TaskBrief::new("grind a long job").with_context_refs(vec!["blob://ctx".into()]);
        let ack = requester
            .submit_task(exec_id, &brief)
            .await
            .expect("submit");
        assert!(ack.accepted);
        assert_eq!(ack.task_id, brief.task_id);

        // It reaches Running (the requester keeps working meanwhile).
        wait_status(&requester, exec_id, &brief.task_id, |s| {
            matches!(s, TaskState::Running)
        })
        .await;

        // Cancel mid-run → the executor demonstrably stops.
        assert!(requester
            .cancel_task(exec_id, &brief.task_id)
            .await
            .expect("cancel"));
        let state = wait_status(&requester, exec_id, &brief.task_id, |s| s.is_terminal()).await;
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            saw.load(Ordering::SeqCst),
            "the remote executor observed the cancel"
        );

        requester.shutdown().await.ok();
        executor_mesh.shutdown().await.ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_task_completes_with_an_artifact_ref_over_the_wire() {
        let psk = [0x62u8; 32];
        let executor_mesh = build_started(&psk).await;
        let _handles = executor_mesh
            .serve_a2a(
                TaskRegistry::new(),
                Arc::new(TestExecutor {
                    result: "blob://summary-42".to_string(),
                    wait_for_cancel: false,
                    saw_cancel: Arc::new(AtomicBool::new(false)),
                }),
            )
            .expect("serve a2a");

        let requester = build_started(&psk).await;
        requester
            .connect_via(
                &executor_mesh.local_addr().to_string(),
                executor_mesh.public_key(),
                executor_mesh.node_id(),
            )
            .await
            .expect("connect");
        let exec_id = executor_mesh.node_id();

        let brief = TaskBrief::new("summarize");
        requester
            .submit_task(exec_id, &brief)
            .await
            .expect("submit");

        // The result lands as an artifact ref.
        let state = wait_status(&requester, exec_id, &brief.task_id, |s| s.is_terminal()).await;
        assert_eq!(
            state,
            TaskState::Completed {
                result_ref: "blob://summary-42".to_string()
            }
        );

        // Cancelling a finished task is a no-op; an unknown task is None.
        assert!(!requester
            .cancel_task(exec_id, &brief.task_id)
            .await
            .unwrap());
        assert!(requester
            .task_status(exec_id, "nope")
            .await
            .unwrap()
            .is_none());

        requester.shutdown().await.ok();
        executor_mesh.shutdown().await.ok();
    }
}
