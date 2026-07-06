//! PyO3 surface for agent-to-agent (A2A) task handoff
//! (`HERMES_INTEGRATION_PLAN_V2.md` Phase 3).
//!
//! A Python agent serves the A2A task lifecycle backed by an **async task
//! executor callback** (its own agent loop), and a Python requester hands off a
//! job, polls, and cancels it by node id. The whole protocol + registry +
//! cancellation lives in `net_sdk::{a2a, mesh_a2a}` (H2); this file marshals.
//!
//! **Cancellation.** A `cancel_task` trips the Rust cancel token, which cancels
//! the Python executor's coroutine (an `asyncio.CancelledError` inside its
//! `await`) via [`crate::async_bridge::dispatch_handler_coro`]'s cancel-on-drop
//! — so a cancel demonstrably stops the remote work.
//!
//! **H8.** Only task briefs (prompt + Datafort context refs) and result refs
//! cross — never keys.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tokio::runtime::Runtime;

use net::adapter::net::channel::ChannelConfigRegistry;
use net::adapter::net::MeshNode;
use net_sdk::a2a::{CancelToken, TaskBrief, TaskExecutor, TaskRegistry};
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;

/// A [`TaskExecutor`] backed by a Python **async** callback
/// `async (task_id: str, prompt: str, context_refs: list[str], tags: list[str])
/// -> str` returning the result's artifact ref. A cancel drops the coroutine
/// future, cancelling the Python handler.
struct PyTaskExecutor {
    callback: Py<PyAny>,
}

#[async_trait::async_trait]
impl TaskExecutor for PyTaskExecutor {
    async fn run(&self, brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
        // GIL only to build + submit the coroutine; await it off the GIL.
        let fut = Python::attach(|py| -> PyResult<_> {
            let coro = self.callback.bind(py).call1((
                brief.task_id.as_str(),
                brief.prompt.as_str(),
                brief.context_refs.clone(),
                brief.tags.clone(),
            ))?;
            crate::async_bridge::dispatch_handler_coro(py, coro)
        })
        .map_err(|e| format!("a2a executor: calling the task handler failed: {e}"))?;

        // Cancellation: if the token trips, this branch wins and `fut` drops —
        // dispatch_handler_coro's guard cancels the Python coroutine. The
        // registry records `Cancelled` regardless of what we return here.
        tokio::select! {
            _ = cancel.cancelled() => Err("cancelled".to_string()),
            r = fut => match r {
                Ok(obj) => Python::attach(|py| {
                    obj.bind(py)
                        .extract::<String>()
                        .map_err(|e| format!("a2a task handler must return a str result ref: {e}"))
                }),
                Err(e) => Err(format!("a2a task handler raised: {e}")),
            },
        }
    }
}

/// Wrap a raw node in an SDK `Mesh` sharing the live node (fresh channel
/// registry). Mirrors `enrollment::mesh_over` / `publish::mesh_over`.
fn mesh_over(node: Arc<MeshNode>) -> Mesh {
    Mesh::from_node_arc(node, Arc::new(ChannelConfigRegistry::new()), None)
}

/// **Executor side.** Serve the A2A task lifecycle on the live `node`, backed by
/// a Python async task-executor `callback`, with a fresh [`TaskRegistry`].
/// Returns a handle that must be held to keep accepting tasks.
pub(crate) fn mesh_serve_a2a(
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
    callback: Py<PyAny>,
) -> PyResult<PyA2aServeHandle> {
    let mesh = mesh_over(node);
    let registry = TaskRegistry::new();
    let executor: Arc<dyn TaskExecutor> = Arc::new(PyTaskExecutor { callback });
    // `serve_rpc` spawns bridge tasks, so it needs a runtime context.
    let _guard = runtime.enter();
    let handles = mesh
        .serve_a2a(registry, executor)
        .map_err(|e| PyRuntimeError::new_err(format!("serve_a2a failed: {e}")))?;
    Ok(PyA2aServeHandle {
        inner: Some((mesh, handles)),
    })
}

/// **Requester side.** Hand `prompt` (+ Datafort `context_refs` + `tags`) to the
/// executor at `target_node_id`; return the accepted task id. Raises if the
/// executor rejected the brief. Releases the GIL for the round-trip.
pub(crate) fn mesh_submit_task(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
    target_node_id: u64,
    prompt: String,
    context_refs: Vec<String>,
    tags: Vec<String>,
) -> PyResult<String> {
    let mesh = mesh_over(node);
    let brief = TaskBrief::new(prompt)
        .with_context_refs(context_refs)
        .with_tags(tags);
    let ack = py
        .detach(move || runtime.block_on(mesh.submit_task(target_node_id, &brief)))
        .map_err(|e| PyRuntimeError::new_err(format!("submit_task: {e}")))?;
    if !ack.accepted {
        return Err(PyRuntimeError::new_err(format!(
            "executor rejected the task: {}",
            ack.reason.unwrap_or_else(|| "no reason given".to_string())
        )));
    }
    Ok(ack.task_id)
}

/// **Requester side.** The executor's status record for `task_id` as a JSON
/// string (`{brief, state, updated_at}`), or `None` if the executor doesn't
/// know it.
pub(crate) fn mesh_task_status(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
    target_node_id: u64,
    task_id: String,
) -> PyResult<Option<String>> {
    let mesh = mesh_over(node);
    let record = py
        .detach(move || runtime.block_on(mesh.task_status(target_node_id, &task_id)))
        .map_err(|e| PyRuntimeError::new_err(format!("task_status: {e}")))?;
    match record {
        Some(rec) => Ok(Some(
            String::from_utf8(rec.encode())
                .map_err(|e| PyRuntimeError::new_err(format!("encode record: {e}")))?,
        )),
        None => Ok(None),
    }
}

/// **Requester side.** Cancel `task_id` on the executor; returns whether it was
/// in flight. The executor's coroutine is cancelled — the work stops.
pub(crate) fn mesh_cancel_task(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
    target_node_id: u64,
    task_id: String,
) -> PyResult<bool> {
    let mesh = mesh_over(node);
    py.detach(move || runtime.block_on(mesh.cancel_task(target_node_id, &task_id)))
        .map_err(|e| PyRuntimeError::new_err(format!("cancel_task: {e}")))
}

/// Keeps the served A2A services alive (returned by `NetMesh.serve_a2a`).
/// Dropping it or calling [`stop`](Self::stop) unregisters them.
#[pyclass(name = "A2aServeHandle", module = "net._net", skip_from_py_object)]
pub struct PyA2aServeHandle {
    // The `Mesh` holds the channel registry the services registered against, and
    // each `ServeHandle` one dispatcher registration (task/status/cancel).
    inner: Option<(Mesh, Vec<ServeHandle>)>,
}

#[pymethods]
impl PyA2aServeHandle {
    /// Stop accepting A2A tasks (unregister the services).
    fn stop(&mut self) {
        self.inner = None;
    }

    /// Whether the services are still registered.
    #[getter]
    fn serving(&self) -> bool {
        self.inner.is_some()
    }

    fn __repr__(&self) -> String {
        format!("A2aServeHandle(serving={})", self.inner.is_some())
    }
}
