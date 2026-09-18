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

use crate::runtime_guard::GuardedRuntime;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use net::adapter::net::channel::ChannelConfigRegistry;
use net::adapter::net::MeshNode;
use net_sdk::a2a::{CancelToken, PreparedTask, TaskBrief, TaskExecutor, TaskRegistry};
use net_sdk::a2a_payment::TaskPaymentProof;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_a2a::A2aFlowError;
use net_sdk::mesh_rpc::ServeHandle;

pyo3::create_exception!(
    _net,
    PaymentRefused,
    pyo3::exceptions::PyException,
    "A paid A2A submission was refused on payment or admission grounds — \
     the provider answered the `net.payment` application error rather than \
     an ack. `args` is `(message, schematic_json | None)`: the human \
     refusal, and the machine-actionable `net.payment.failure@1` document \
     when the provider sent one (also on the `schematic` attribute). \
     Distinct from a transport failure (`RuntimeError`) because the \
     schematic says whether retrying, re-quoting, or neither is safe."
);

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

        // Cancellation: if the token trips, the cancel branch wins and `fut`
        // drops — dispatch_handler_coro's guard is armed at dispatch (not at
        // first poll), so dropping `fut` cancels the Python coroutine even if
        // the token was already tripped before this select ever polled it.
        // `biased` polls `fut` first so a result that's already in beats a
        // simultaneous cancel. The registry records `Cancelled` regardless of
        // what we return here.
        tokio::select! {
            biased;
            r = fut => match r {
                Ok(obj) => Python::attach(|py| {
                    obj.bind(py)
                        .extract::<String>()
                        .map_err(|e| format!("a2a task handler must return a str result ref: {e}"))
                }),
                Err(e) => Err(format!("a2a task handler raised: {e}")),
            },
            _ = cancel.cancelled() => Err("cancelled".to_string()),
        }
    }
}

/// The organization identity the requester verbs present.
///
/// A newtype rather than the client type directly, so the verb
/// signatures below do not fork per build: without the `org` feature
/// there is no `OrgClient` to carry and this is an empty struct. It is
/// deliberately not a bare `()` in that arm — strict clippy reads
/// passing a unit value to a function as a mistake, and it is right to.
#[derive(Clone, Default)]
pub(crate) struct A2aOrgCaller {
    #[cfg(feature = "org")]
    client: Option<Arc<net_sdk::org::OrgClient>>,
}

#[cfg(feature = "org")]
impl A2aOrgCaller {
    /// The identity installed on a mesh, if any.
    pub(crate) fn installed(client: Option<Arc<net_sdk::org::OrgClient>>) -> Self {
        Self { client }
    }
}

/// Wrap a raw node in an SDK `Mesh` sharing the live node (fresh channel
/// registry). Mirrors `enrollment::mesh_over` / `publish::mesh_over`.
pub(crate) fn mesh_over(node: Arc<MeshNode>) -> Mesh {
    Mesh::from_node_arc(node, Arc::new(ChannelConfigRegistry::new()), None)
}

/// [`mesh_over`] carrying the caller's A2A organization identity.
///
/// Each requester verb builds its own `Mesh` over the shared live node,
/// so the identity `NetMesh.set_a2a_org_caller` installed has to be
/// applied per call rather than once — which is also what keeps a later
/// `set_a2a_org_caller(None)` meaningful.
fn mesh_over_as(node: Arc<MeshNode>, org: A2aOrgCaller) -> Mesh {
    let mesh = mesh_over(node);
    #[cfg(feature = "org")]
    mesh.set_a2a_org_caller(org.client);
    #[cfg(not(feature = "org"))]
    let _ = org;
    mesh
}

/// **Executor side.** Serve the A2A task lifecycle on the live `node`, backed by
/// a Python async task-executor `callback`, with a fresh [`TaskRegistry`].
/// Returns a handle that must be held to keep accepting tasks.
pub(crate) fn mesh_serve_a2a(
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
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
    Ok(PyA2aServeHandle::new(mesh, Registered::Legacy(handles)))
}

/// **Requester side.** Hand `prompt` (+ Datafort `context_refs` + `tags`) to the
/// executor at `target_node_id`; return the accepted task id. Raises if the
/// executor rejected the brief. Releases the GIL for the round-trip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mesh_submit_task(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
    target_node_id: u64,
    prompt: String,
    context_refs: Vec<String>,
    tags: Vec<String>,
    task_id: Option<String>,
    service: Option<String>,
    revision: Option<String>,
    org: A2aOrgCaller,
) -> PyResult<String> {
    let mesh = mesh_over_as(node, org);
    let brief = build_brief(prompt, context_refs, tags, task_id, service, revision)?;
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

/// Assemble a [`TaskBrief`] from the binding kwargs.
///
/// `task_id = None` keeps [`TaskBrief::new`]'s random id; a caller-retained
/// id is what makes a paid purchase resumable, so it is the caller's choice
/// rather than ours. `service` and `revision` are both-or-neither: the
/// catalog-driven serving path resolves a brief by the pair, and a brief
/// naming one without the other could never match an offer.
fn build_brief(
    prompt: String,
    context_refs: Vec<String>,
    tags: Vec<String>,
    task_id: Option<String>,
    service: Option<String>,
    revision: Option<String>,
) -> PyResult<TaskBrief> {
    let mut brief = TaskBrief::new(prompt)
        .with_context_refs(context_refs)
        .with_tags(tags);
    if let Some(task_id) = task_id {
        if task_id.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "task_id must be a non-empty string (pass None for a random id)",
            ));
        }
        brief = brief.with_task_id(task_id);
    }
    match (service, revision) {
        (Some(service), Some(revision)) => brief = brief.with_service(service, revision),
        (None, None) => {}
        _ => {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "service and revision must be given together — a catalog-driven \
                 provider resolves a brief by the pair, so one without the other \
                 can never match an offer",
            ))
        }
    }
    Ok(brief)
}

/// **Requester side.** The executor's status record for `task_id` as a JSON
/// string (`{brief, state, updated_at}`), or `None` if the executor doesn't
/// know it.
pub(crate) fn mesh_task_status(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
    target_node_id: u64,
    task_id: String,
    org: A2aOrgCaller,
) -> PyResult<Option<String>> {
    let mesh = mesh_over_as(node, org);
    let record = py
        .detach(move || runtime.block_on(mesh.task_status(target_node_id, &task_id)))
        .map_err(|e| PyRuntimeError::new_err(format!("task_status: {e}")))?;
    match record {
        Some(rec) => {
            Ok(Some(String::from_utf8(rec.encode()).map_err(|e| {
                PyRuntimeError::new_err(format!("encode record: {e}"))
            })?))
        }
        None => Ok(None),
    }
}

/// **Requester side.** Cancel `task_id` on the executor; returns whether it was
/// in flight. The executor's coroutine is cancelled — the work stops.
pub(crate) fn mesh_cancel_task(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
    target_node_id: u64,
    task_id: String,
    org: A2aOrgCaller,
) -> PyResult<bool> {
    let mesh = mesh_over_as(node, org);
    py.detach(move || runtime.block_on(mesh.cancel_task(target_node_id, &task_id)))
        .map_err(|e| PyRuntimeError::new_err(format!("cancel_task: {e}")))
}

/// **Requester side.** What `target_node_id` serves: the JSON array of
/// `A2aOffer`s, one per configured service, with bounds, retention terms
/// and — for a paid service — its `net.pricing.terms@1`.
///
/// Uncharged. A node serving the legacy free path (`serve_a2a`) has no
/// describe service and raises a transport error: free-by-omission is not
/// an offer.
pub(crate) fn mesh_describe_a2a(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
    target_node_id: u64,
    org: A2aOrgCaller,
) -> PyResult<String> {
    let mesh = mesh_over_as(node, org);
    let offers = py
        .detach(move || runtime.block_on(mesh.describe_a2a(target_node_id)))
        .map_err(|e| PyRuntimeError::new_err(format!("describe_a2a: {e}")))?;
    serde_json::to_string(&offers)
        .map_err(|e| PyRuntimeError::new_err(format!("encode offers: {e}")))
}

/// **Requester side, raw.** Submit a prepared task with its payment proof:
/// the brief from `prepared`, the quote id + binding signature from `proof`,
/// to `prepared.provider_node`. Keeps no records — that is
/// `CapabilityGateway.submit_task`'s job.
///
/// A payment or admission refusal raises `PaymentRefused(message,
/// schematic_json)`; every other rejection is the executor's own reason on
/// a `RuntimeError`.
pub(crate) fn mesh_submit_task_paid(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
    prepared_json: &str,
    proof_json: &str,
    org: A2aOrgCaller,
) -> PyResult<String> {
    let mesh = mesh_over_as(node, org);
    let prepared: PreparedTask = serde_json::from_str(prepared_json).map_err(|e| {
        PyValueError::new_err(format!(
            "prepared_json is not a PreparedTask document (pass the `prepared` \
             field of prepare_task's result verbatim): {e}"
        ))
    })?;
    let proof: TaskPaymentProof = serde_json::from_str(proof_json).map_err(|e| {
        PyValueError::new_err(format!(
            "proof_json is not a TaskPaymentProof document (pass the `proof` \
             field of purchase_task's result verbatim): {e}"
        ))
    })?;
    let ack = py
        .detach(move || runtime.block_on(mesh.submit_task_paid(&prepared, &proof)))
        .map_err(flow_err)?;
    if !ack.accepted {
        return Err(PyRuntimeError::new_err(format!(
            "executor rejected the task: {}",
            ack.reason.unwrap_or_else(|| "no reason given".to_string())
        )));
    }
    Ok(ack.task_id)
}

/// Project an [`A2aFlowError`] onto a Python exception.
///
/// A payment or admission refusal is the one arm that is not a transport
/// problem, and it carries the provider's machine-actionable
/// `net.payment.failure@1` schematic beside the human message — so it gets
/// its own exception type with both on `args`, rather than a string a
/// caller would have to parse.
///
/// The two **local** refusals are `ValueError`, because they are caller
/// input this layer rejected before any packet: an undeliverable brief
/// and a proof whose shape cannot ride a request header. Rendering them
/// as `RuntimeError` beside genuine transport failures invited the retry
/// loop that is exactly wrong for them — neither a retry nor a fresh
/// quote can make an over-long proof presentable.
fn flow_err(e: A2aFlowError) -> PyErr {
    match e {
        A2aFlowError::PaymentRefused { message, schematic } => {
            let schematic_json = schematic.and_then(|s| serde_json::to_string(&*s).ok());
            let err = PaymentRefused::new_err((message, schematic_json.clone()));
            if let Some(json) = schematic_json {
                Python::attach(|py| {
                    // Best-effort: failing to attach the convenience
                    // attribute must not replace the refusal with an
                    // attribute error. `args` already carries it.
                    let _ = err.value(py).setattr("schematic", json);
                });
            }
            err
        }
        local @ (A2aFlowError::ProofUndeliverable(_) | A2aFlowError::BriefTooLarge { .. }) => {
            PyValueError::new_err(format!("submit_task_paid: {local}"))
        }
        other => PyRuntimeError::new_err(format!("submit_task_paid: {other}")),
    }
}

/// Keeps the served A2A services alive (returned by `NetMesh.serve_a2a` or
/// `PaymentProvider.serve_a2a_configured`).
/// Dropping it or calling [`stop`](Self::stop) unregisters them.
#[pyclass(name = "A2aServeHandle", module = "net._net", skip_from_py_object)]
pub struct PyA2aServeHandle {
    // The `Mesh` holds the channel registry the services registered against,
    // and the registrations themselves are whatever the serving path returned.
    inner: Option<(Mesh, Registered)>,
}

/// What a serve call registered — held opaquely, because dropping it is the
/// whole contract.
pub(crate) enum Registered {
    /// The legacy free path: one `ServeHandle` per service (task/status/
    /// cancel).
    Legacy(Vec<ServeHandle>),
    /// The configured path: five registrations plus the journal's
    /// exclusive-ownership handle, which `A2aServing` keeps alive for
    /// exactly as long as the handlers that can still write.
    #[cfg(feature = "payments")]
    Configured(net_sdk::mesh_a2a::A2aServing),
}

impl Registered {
    /// How many nRPC services this handle keeps registered: three on the
    /// free path (task/status/cancel), five on the configured one (plus
    /// prepare/describe).
    ///
    /// Read by the `services` getter, which is how a Python caller
    /// distinguishes the two serving paths — a configured provider that
    /// answered `describe` is a provider a requester can price.
    fn services(&self) -> usize {
        match self {
            Registered::Legacy(handles) => handles.len(),
            #[cfg(feature = "payments")]
            Registered::Configured(serving) => serving.handles.len(),
        }
    }
}

impl PyA2aServeHandle {
    /// Wrap what a serving call registered.
    pub(crate) fn new(mesh: Mesh, registered: Registered) -> Self {
        Self {
            inner: Some((mesh, registered)),
        }
    }
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

    /// How many nRPC services are registered: three on the free
    /// ``NetMesh.serve_a2a`` path, five on the configured
    /// ``PaymentProvider.serve_a2a_configured`` one (which also serves
    /// ``net.a2a.prepare`` and ``net.a2a.describe``). ``0`` once stopped.
    #[getter]
    fn services(&self) -> usize {
        self.inner.as_ref().map_or(0, |(_, r)| r.services())
    }

    fn __repr__(&self) -> String {
        format!(
            "A2aServeHandle(serving={}, services={})",
            self.inner.is_some(),
            self.services()
        )
    }
}
