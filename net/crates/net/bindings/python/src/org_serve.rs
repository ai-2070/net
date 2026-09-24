//! OSDK-L Workstream P — the `serve_org` provider verb for Python.
//!
//! Split from `org.rs` only for length; it is the same module surface. The
//! handler bridge follows `PyRpcHandler` exactly: the Python callable runs
//! inside `spawn_blocking` under `Python::attach`, with a bounded timeout and a
//! "must return bytes" contract.

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};

use crate::mesh_rpc::{PyRequestStreamRecv, PyResponseSinkSend};
use crate::runtime_guard::GuardedRuntime;

use super::org::org_serve_error;

/// Application status for a handler that raised — the same value the typed nRPC
/// layer uses. A handler cannot counterfeit an admission denial (0x0009).
const ORG_HANDLER_ERROR: u16 = 0x8001;

/// Build the `caller` dict handed to the Python handler: the five verified
/// fields plus `is_same_org`, all ids as `bytes`.
///
/// `pub(crate)`: the subnet-exported serve (`subnet.rs`) hands the SAME
/// admitted `OrgCaller` shape to its handler, through the SAME bridge.
pub(crate) fn caller_dict<'py>(
    py: Python<'py>,
    caller: &net_sdk::org::OrgCaller,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("entity", PyBytes::new(py, caller.entity.as_bytes()))?;
    d.set_item("acting_org", PyBytes::new(py, caller.acting_org.as_bytes()))?;
    d.set_item(
        "provider_org",
        PyBytes::new(py, caller.provider_org.as_bytes()),
    )?;
    d.set_item("provider", PyBytes::new(py, caller.provider.as_bytes()))?;
    d.set_item("capability", PyBytes::new(py, caller.capability.as_bytes()))?;
    d.set_item("is_same_org", caller.is_same_org())?;
    Ok(d)
}

/// Handle for a served organization service. `close()` unregisters.
#[pyclass(name = "OrgServeHandle", module = "_net")]
pub struct PyOrgServeHandle {
    inner: parking_lot::Mutex<Option<net_sdk::mesh_rpc::ServeHandle>>,
}

impl PyOrgServeHandle {
    /// Wrap a registered serve handle — shared with the subnet-exported
    /// serve, which returns the same RAII shape.
    pub(crate) fn from_handle(handle: net_sdk::mesh_rpc::ServeHandle) -> Self {
        PyOrgServeHandle {
            inner: parking_lot::Mutex::new(Some(handle)),
        }
    }
}

#[pymethods]
impl PyOrgServeHandle {
    /// Unregister the service. Idempotent. In-flight handlers run to
    /// completion.
    fn close(&self) {
        let _ = self.inner.lock().take();
    }

    #[getter]
    fn is_closed(&self) -> bool {
        self.inner.lock().is_none()
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<Bound<'_, PyAny>>,
        _exc_val: Option<Bound<'_, PyAny>>,
        _exc_tb: Option<Bound<'_, PyAny>>,
    ) -> bool {
        self.close();
        false
    }
}

/// Serve a protected, privately-discoverable service.
///
/// `access` is `"same_org"` or `"granted"` and selects both who may call AND
/// how the service is announced — both ship only inside an encrypted audience.
/// The handler is `handler(caller: dict, request: bytes) -> bytes`; `caller`
/// carries the five verified fields plus `is_same_org`. Raising surfaces as an
/// application error, never as an admission denial. A non-callable `handler`
/// is refused here, at registration, rather than per request.
///
/// `handler_timeout_ms` bounds how long the provider WAITS for the handler's
/// reply (`0` = effectively infinite). The handler itself runs on a blocking
/// thread and is not interrupted when the wait elapses — a hung handler holds
/// its blocking-pool thread until it returns, the same behavior as the nRPC
/// handler.
///
/// Requires an installed node authority.
#[pyfunction]
#[pyo3(signature = (mesh, service, access, handler, handler_timeout_ms=None))]
pub fn serve_org(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    service: String,
    access: &str,
    handler: Py<PyAny>,
    handler_timeout_ms: Option<u64>,
) -> PyResult<PyOrgServeHandle> {
    // Fail fast: a non-callable handler is a caller bug, caught at registration
    // rather than surfacing as an application error on the first request.
    if !handler.bind(py).is_callable() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "serve_org handler must be callable: handler(caller: dict, request: bytes) -> bytes",
        ));
    }
    let node = mesh.node_arc_clone()?;
    let access = super::org::access_from_str(access)?;
    let timeout = match handler_timeout_ms {
        Some(0) => Duration::from_secs(u64::from(u32::MAX)),
        Some(ms) => Duration::from_millis(ms),
        None => Duration::from_secs(60),
    };
    let callable = Arc::new(handler);

    // `serve_org_bytes_node` -> `serve_rpc_*` spawns an inbound-event bridge
    // with a bare `tokio::spawn`, which needs an ambient runtime. This is a sync
    // `#[pyfunction]` called on the Python thread with no runtime — enter the
    // mesh's for the registration (the a2a binding does the same for the same
    // reason). Without it, the first live serve panics "there is no reactor
    // running"; the refusal-only tests never reached it.
    let runtime = mesh.runtime_arc();
    let handle = {
        let _guard = runtime.enter();
        net_sdk::org::serve_org_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller, body: bytes::Bytes| {
                let callable = callable.clone();
                async move { run_py_org_handler(callable, caller, body, timeout).await }
            },
        )
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(org_serve_error(&e)))?
    };

    Ok(PyOrgServeHandle {
        inner: parking_lot::Mutex::new(Some(handle)),
    })
}

/// Invoke the Python handler with the verified caller and request bytes.
///
/// A raised exception maps to the application band (the handler said no); a
/// marshaling failure maps to internal. Neither is ever an admission denial.
///
/// `pub(crate)`: the subnet-exported serve reuses this one bridge rather
/// than a second copy — the admitted facts and the return contract are
/// identical.
pub(crate) async fn run_py_org_handler(
    callable: Arc<Py<PyAny>>,
    caller: net_sdk::org::OrgCaller,
    body: bytes::Bytes,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, net_sdk::org::OrgHandlerError> {
    let callable = Python::attach(|py| callable.clone_ref(py));
    let result = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, (bool, String)> {
            Python::attach(|py| -> Result<Vec<u8>, (bool, String)> {
                let caller_obj = caller_dict(py, &caller)
                    .map_err(|e| (false, format!("failed to build caller: {e}")))?;
                let req_bytes = PyBytes::new(py, &body);
                let args = PyTuple::new(py, [caller_obj.into_any(), req_bytes.into_any()])
                    .map_err(|e| (false, format!("failed to build args: {e}")))?;
                match callable.call1(py, args) {
                    Ok(ret) => ret
                        .into_bound(py)
                        .extract::<Vec<u8>>()
                        .map_err(|e| (false, format!("org handler must return bytes: {e}"))),
                    // `true` = application error (handler raised); `false` =
                    // internal marshaling failure.
                    Err(pyerr) => Err((true, format!("org handler raised: {pyerr}"))),
                }
            })
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(out))) => Ok(bytes::Bytes::from(out)),
        Ok(Ok(Err((true, msg)))) => Err(net_sdk::org::OrgHandlerError::Application {
            code: ORG_HANDLER_ERROR,
            message: msg,
        }),
        Ok(Ok(Err((false, msg)))) => Err(net_sdk::org::OrgHandlerError::Internal(msg)),
        Ok(Err(join_err)) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "org handler task panicked: {join_err}"
        ))),
        Err(_) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "org handler did not respond within {} ms",
            timeout.as_millis()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Provisioning — the operator/startup steps that make the surface usable
// ---------------------------------------------------------------------------

/// Install an adopted node authority from ``authority_dir`` — the directory
/// ``net node adopt`` wrote.
///
/// REQUIRED before ``OrgClient.bind`` can succeed or a ``"granted"`` service
/// can serve. This is node STARTUP (loading already-adopted files), not
/// adoption (the ceremony that mints them — that stays in the CLI). The node's
/// identity must be the one the membership names, so the mesh must have been
/// created with the matching ``identity_seed``.
#[pyfunction]
pub fn install_org_authority(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    authority_dir: String,
) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    // Blocking file I/O (opens + validates the adopted authority dir) — release
    // the GIL so other Python threads keep running, matching the crate's
    // convention for blocking work.
    py.detach(|| {
        net_sdk::org::install_org_authority_node(&node, std::path::Path::new(&authority_dir))
    })
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

/// Install a provider grant audience so a ``"granted"`` service can seal
/// envelopes: the grant this node's org issued (wire ``bytes``) plus its
/// out-of-band secret (a ``str`` PATH — the raw key never enters Python).
///
/// A ``"same_org"`` provider does NOT need this.
#[pyfunction]
pub fn install_provider_grant_audience(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    grant: &[u8],
    audience_secret_path: String,
) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    // Copy the borrowed grant bytes so the blocking loader (opens + validates
    // the secret file) can run with the GIL RELEASED without touching
    // Python-owned memory off-thread.
    let grant = grant.to_vec();
    py.detach(|| {
        net_sdk::org::install_provider_grant_audience_node(
            &node,
            &grant,
            std::path::Path::new(&audience_secret_path),
        )
    })
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

// ===========================================================================
// §4.4 — the streaming provider verbs: `serve_org_streaming` /
// `serve_org_client_stream` / `serve_org_duplex`.
//
// Same registration contract as `serve_org` (access implies visibility, the
// trivial proof policy, registration before provisioning), the same handler
// bridge discipline (a `def` handler runs on a blocking thread under
// `Python::attach` with a bounded wait), and the same error mapping (a raised
// exception is the application band — the handler said no — a marshaling
// failure is internal; neither is ever an admission denial). The handlers
// receive the verified `caller` dict FIRST and the existing
// `ResponseSinkSend` / `RequestStreamRecv` primitives beside it (§4.4 —
// "passing `caller_dict` + `ResponseSinkSend`/`RequestStreamRecv`").
//
// # Handler-drop contract (Specification §2.2 — the F-S3.1-2 level)
//
// A protected call runs under a per-call retire supervisor. On retirement —
// caller CANCEL or the caller handle's `close()`/drop, the call deadline,
// revocation, session replacement, `serve_handle.close()` against an in-flight
// call, or node shutdown — the supervisor drops the handler future **without
// a final poll**. Handlers MUST NOT assume they are resumed, and MUST NOT
// assume cancellation arrives as a handler-side event:
//
// - Cancellation is observed ONLY through the retirement observables: the
//   request input (`RequestStreamRecv` iteration) fences to EOF and
//   library-controlled sinks stop admitting output. Everything else is
//   best-effort teardown machinery, never a contract.
// - A `def` handler runs on a detached blocking thread: dropping the Rust
//   future cannot interrupt it. It runs to whatever point it reaches; its
//   return value is discarded and its already-performed effects are NOT
//   recalled (§2.2 — arbitrary user code cannot be forcibly rolled back by
//   dropping a Rust future).
// - An `async def` handler's dispatched coroutine is cancelled on teardown,
//   so `asyncio.CancelledError` MAY surface at an `await` — but the supervisor
//   may drop the handler future without a final poll, and the coroutine may
//   never be resumed to observe anything. Do not rely on it for correctness;
//   treat the retirement observables as the only guaranteed signal.
//
// `handler_timeout_ms` bounds how long the provider WAITS for the handler
// (`0` = effectively infinite), exactly as `serve_org` documents: the wait
// elapsing does not interrupt the handler, it abandons it (the drop above
// applies) and surfaces an internal error to the caller.
// ===========================================================================

/// How a user handler is driven — resolved ONCE at registration
/// (`inspect.iscoroutinefunction`), never per request. The `Sync` arm is
/// `serve_org`'s bridge; the `Async` arm is `AsyncMeshRpc.serve*`'s coroutine
/// bridge (`dispatch_handler_coro`, whose drop cancels the dispatched task
/// best-effort — see the handler-drop contract above).
#[derive(Clone, Copy)]
enum HandlerDrive {
    Sync,
    Async,
}

/// A handler outcome before the org error mapping — `run_py_org_handler`'s
/// contract: `true` = the handler raised (application band), `false` = a
/// marshaling/timeout/panic failure (internal).
type HandlerResult<T> = std::result::Result<T, (bool, String)>;

/// `run_py_org_handler`'s outcome mapping, shared by the streaming bridges.
fn org_handler_outcome<T>(
    r: HandlerResult<T>,
) -> std::result::Result<T, net_sdk::org::OrgHandlerError> {
    match r {
        Ok(v) => Ok(v),
        Err((true, msg)) => Err(net_sdk::org::OrgHandlerError::Application {
            code: ORG_HANDLER_ERROR,
            message: msg,
        }),
        Err((false, msg)) => Err(net_sdk::org::OrgHandlerError::Internal(msg)),
    }
}

/// Await one handler invocation under the drive's mechanics, mapping panics
/// and the bounded wait onto the internal band. The handler's own raise (or a
/// return-contract violation) is classified by the per-shape `build`/`finish`
/// closures: `build` assembles the argument tuple (`caller` dict first), and
/// `finish` interprets a resolved return value (`bytes` for client-streaming,
/// ignored for the two sink-bearing shapes).
async fn drive_handler<T>(
    drive: HandlerDrive,
    callable: Arc<Py<PyAny>>,
    build: impl FnOnce(Python<'_>) -> HandlerResult<Py<pyo3::types::PyTuple>> + Send + 'static,
    finish: impl FnOnce(Python<'_>, Py<PyAny>) -> HandlerResult<T> + Send + 'static,
    timeout: Duration,
    label: &'static str,
) -> HandlerResult<T>
where
    T: Send + 'static,
{
    match drive {
        HandlerDrive::Sync => {
            // `serve_org`'s bridge: the callable runs on a blocking thread
            // under `Python::attach` so the GIL acquisition can't starve the
            // async runtime. The bounded wait abandoning the handler is the
            // handler-drop level documented above — the thread is NOT
            // interrupted.
            let callable = Python::attach(|py| callable.clone_ref(py));
            let joined = tokio::time::timeout(
                timeout,
                tokio::task::spawn_blocking(move || -> HandlerResult<T> {
                    Python::attach(|py| -> HandlerResult<T> {
                        let args = build(py)?;
                        match callable.call1(py, args) {
                            Ok(ret) => finish(py, ret),
                            Err(pyerr) => {
                                Err((true, format!("org {label} handler raised: {pyerr}")))
                            }
                        }
                    })
                }),
            )
            .await;
            match joined {
                Ok(Ok(r)) => r,
                Ok(Err(join_err)) => Err((false, format!("org handler task panicked: {join_err}"))),
                Err(_) => Err((
                    false,
                    format!(
                        "org handler did not respond within {} ms",
                        timeout.as_millis()
                    ),
                )),
            }
        }
        HandlerDrive::Async => {
            // `AsyncMeshRpc.serve*`'s coroutine bridge: the coroutine is
            // dispatched onto the async bridge's dispatcher loop; dropping the
            // dispatched future cancels its task (best-effort — see the
            // handler-drop contract above). There is no `ctx.cancellation` at
            // this seam, so retirement reaches the handler only through the
            // documented observables and the future drop.
            let dispatched = Python::attach(|py| -> Result<_, (bool, String)> {
                let args = build(py)?;
                let coro = callable
                    .call1(py, args)
                    .map_err(|pyerr| (true, format!("org {label} handler raised: {pyerr}")))?;
                crate::async_bridge::dispatch_handler_coro(py, coro.into_bound(py))
                    .map_err(|pyerr| (true, format!("org {label} handler raised: {pyerr}")))
            });
            let fut = match dispatched {
                Ok(f) => f,
                Err(e) => return Err(e),
            };
            match tokio::time::timeout(timeout, fut).await {
                Ok(Ok(value)) => Python::attach(|py| finish(py, value)),
                Ok(Err(pyerr)) => Err((true, format!("org {label} handler raised: {pyerr}"))),
                Err(_) => Err((
                    false,
                    format!(
                        "org handler did not respond within {} ms",
                        timeout.as_millis()
                    ),
                )),
            }
        }
    }
}

/// The server-streaming bridge: `handler(caller: dict, request: bytes, sink:
/// ResponseSinkSend) -> None`. Return value ignored — the substrate emits the
/// terminal frame at handler return (§2.2 "producer finished is not terminal":
/// the pump drains first).
async fn run_py_org_streaming_handler(
    callable: Arc<Py<PyAny>>,
    drive: HandlerDrive,
    caller: net_sdk::org::OrgCaller,
    body: bytes::Bytes,
    sink: ::net::adapter::net::cortex::RpcResponseSink,
    timeout: Duration,
) -> std::result::Result<(), net_sdk::org::OrgHandlerError> {
    let (sink_wrap, sink_liveness) = PyResponseSinkSend::from_org_response_sink(sink);
    let build = move |py: Python<'_>| -> HandlerResult<Py<pyo3::types::PyTuple>> {
        let caller_obj = caller_dict(py, &caller)
            .map_err(|e| (false, format!("failed to build caller: {e}")))?;
        let req = PyBytes::new(py, &body);
        let sink_obj = Py::new(py, sink_wrap)
            .map_err(|e| (false, format!("failed to build response sink: {e}")))?
            .into_bound(py)
            .into_any();
        pyo3::types::PyTuple::new(py, [caller_obj.into_any(), req.into_any(), sink_obj])
            .map(|t| t.unbind())
            .map_err(|e| (false, format!("failed to build args: {e}")))
    };
    let finish = |_py: Python<'_>, _ret: Py<PyAny>| -> HandlerResult<()> { Ok(()) };
    let outcome = drive_handler(drive, callable, build, finish, timeout, "streaming").await;
    // The response pump's sender must outlive the handler's completion
    // until this future resolves: dropped earlier (at Python's argument
    // teardown), the pump's exit races `handler_returned` to the terminal
    // and the client sees `0x0006: response pump failed` (R4COREFIX
    // finding 9 / F-S4PySdk-4).
    drop(sink_liveness);
    org_handler_outcome(outcome)
}

/// The client-streaming bridge: `handler(caller: dict, stream:
/// RequestStreamRecv) -> bytes`. Iterate `stream` to drain the upload (it
/// fences to EOF on retirement); the return value is the terminal response.
async fn run_py_org_client_stream_handler(
    callable: Arc<Py<PyAny>>,
    drive: HandlerDrive,
    caller: net_sdk::org::OrgCaller,
    requests: ::net::adapter::net::cortex::RequestStream,
    runtime: Arc<GuardedRuntime>,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, net_sdk::org::OrgHandlerError> {
    let build = move |py: Python<'_>| -> HandlerResult<Py<pyo3::types::PyTuple>> {
        let caller_obj = caller_dict(py, &caller)
            .map_err(|e| (false, format!("failed to build caller: {e}")))?;
        let stream_obj = Py::new(
            py,
            PyRequestStreamRecv::from_org_request_stream(requests, runtime),
        )
        .map_err(|e| (false, format!("failed to build request stream: {e}")))?
        .into_bound(py)
        .into_any();
        pyo3::types::PyTuple::new(py, [caller_obj.into_any(), stream_obj])
            .map(|t| t.unbind())
            .map_err(|e| (false, format!("failed to build args: {e}")))
    };
    let finish = |py: Python<'_>, ret: Py<PyAny>| -> HandlerResult<bytes::Bytes> {
        ret.into_bound(py)
            .extract::<Vec<u8>>()
            .map(bytes::Bytes::from)
            .map_err(|e| {
                (
                    false,
                    format!("org client-stream handler must return bytes: {e}"),
                )
            })
    };
    org_handler_outcome(
        drive_handler(drive, callable, build, finish, timeout, "client-streaming").await,
    )
}

/// The duplex bridge: `handler(caller: dict, stream: RequestStreamRecv, sink:
/// ResponseSinkSend) -> None`. Drain `stream`, emit through `sink`; the
/// substrate emits the terminal frame at handler return.
async fn run_py_org_duplex_handler(
    callable: Arc<Py<PyAny>>,
    drive: HandlerDrive,
    caller: net_sdk::org::OrgCaller,
    requests: ::net::adapter::net::cortex::RequestStream,
    sink: ::net::adapter::net::cortex::RpcResponseSink,
    runtime: Arc<GuardedRuntime>,
    timeout: Duration,
) -> std::result::Result<(), net_sdk::org::OrgHandlerError> {
    let (sink_wrap, sink_liveness) = PyResponseSinkSend::from_org_response_sink(sink);
    let build = move |py: Python<'_>| -> HandlerResult<Py<pyo3::types::PyTuple>> {
        let caller_obj = caller_dict(py, &caller)
            .map_err(|e| (false, format!("failed to build caller: {e}")))?;
        let stream_obj = Py::new(
            py,
            PyRequestStreamRecv::from_org_request_stream(requests, runtime),
        )
        .map_err(|e| (false, format!("failed to build request stream: {e}")))?
        .into_bound(py)
        .into_any();
        let sink_obj = Py::new(py, sink_wrap)
            .map_err(|e| (false, format!("failed to build response sink: {e}")))?
            .into_bound(py)
            .into_any();
        pyo3::types::PyTuple::new(py, [caller_obj.into_any(), stream_obj, sink_obj])
            .map(|t| t.unbind())
            .map_err(|e| (false, format!("failed to build args: {e}")))
    };
    let finish = |_py: Python<'_>, _ret: Py<PyAny>| -> HandlerResult<()> { Ok(()) };
    let outcome = drive_handler(drive, callable, build, finish, timeout, "duplex").await;
    // Same liveness ordering as the streaming bridge: the pump's sender
    // lives until this future resolves (R4COREFIX finding 9).
    drop(sink_liveness);
    org_handler_outcome(outcome)
}

/// Resolve the handler drive at registration — `def` → [`HandlerDrive::Sync`],
/// `async def` → [`HandlerDrive::Async`] — and refuse a non-callable up front
/// (`serve_org`'s registration contract).
fn org_handler_drive(py: Python<'_>, handler: &Py<PyAny>, verb: &str) -> PyResult<HandlerDrive> {
    if !handler.bind(py).is_callable() {
        return Err(pyo3::exceptions::PyTypeError::new_err(format!(
            "{verb} handler must be callable"
        )));
    }
    Ok(if crate::mesh_rpc::is_coroutine_function(py, handler) {
        HandlerDrive::Async
    } else {
        HandlerDrive::Sync
    })
}

/// `serve_org`'s bounded-wait resolution, verbatim: `0` = effectively
/// infinite, omitted = 60 s.
fn org_handler_timeout(handler_timeout_ms: Option<u64>) -> Duration {
    match handler_timeout_ms {
        Some(0) => Duration::from_secs(u64::from(u32::MAX)),
        Some(ms) => Duration::from_millis(ms),
        None => Duration::from_secs(60),
    }
}

/// Serve a protected, privately-discoverable service whose response is a
/// STREAM (OSDK §3; §4.4).
///
/// `access` is `"same_org"` or `"granted"` and selects both who may call AND
/// how the service is announced — both ship only inside an encrypted audience.
/// The handler is `handler(caller: dict, request: bytes, sink:
/// ResponseSinkSend) -> None` (a `def` or an `async def`): `caller` carries the
/// five verified fields plus `is_same_org` (attribution — none of it is
/// caller-claimed), `request` is the one request the signed opening binds, and
/// chunks go out through `sink.send(bytes)`. The substrate emits the terminal
/// frame at handler return. Raising surfaces as an application error, never as
/// an admission denial. A non-callable handler is refused here, at
/// registration.
///
/// `handler_timeout_ms` bounds how long the provider WAITS for the handler
/// (`0` = effectively infinite). The handler itself is not interrupted when
/// the wait elapses — see the handler-drop contract below.
///
/// **Handler-drop contract (Specification §2.2 — the F-S3.1-2 level).** A
/// protected call runs under a per-call retire supervisor. On retirement —
/// caller CANCEL or the caller handle's `close()`/drop, the call deadline,
/// revocation, session replacement, `serve_handle.close()` against an
/// in-flight call, or node shutdown — the supervisor drops the handler future
/// **without a final poll**. Cancellation is therefore observed ONLY through
/// the retirement observables (the request input fences to EOF where the
/// shape has one; library-controlled sinks stop admitting output) and NEVER as
/// a handler-side event: a `def` handler's blocking thread cannot be
/// interrupted and runs to whatever point it reaches (its return value is
/// discarded, its performed effects are not recalled); an `async def`
/// handler's coroutine MAY see `asyncio.CancelledError` at an `await` as
/// best-effort teardown machinery, but it may equally never be resumed to
/// observe anything — never rely on it.
///
/// **Diagnostics level.** The `RequestStreamRecv` an org handler receives
/// carries chunks + the retire signal only; its raw-transport diagnostic
/// getters (`caller_origin`, `call_id`, `deadline_ns`, `headers`) are
/// unpopulated (0 / empty) — the frozen `serve_org_*_bytes_node` seam surfaces
/// the verified `caller` dict instead. Use `caller` for attribution.
///
/// Requires an installed node authority.
#[pyfunction]
#[pyo3(signature = (mesh, service, access, handler, handler_timeout_ms=None))]
pub fn serve_org_streaming(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    service: String,
    access: &str,
    handler: Py<PyAny>,
    handler_timeout_ms: Option<u64>,
) -> PyResult<PyOrgServeHandle> {
    let drive = org_handler_drive(py, &handler, "serve_org_streaming")?;
    let node = mesh.node_arc_clone()?;
    let access = super::org::access_from_str(access)?;
    let timeout = org_handler_timeout(handler_timeout_ms);
    let callable = Arc::new(handler);
    let runtime = mesh.runtime_arc();

    // Same as `serve_org`: the registration spawns the inbound bridge, so
    // enter the mesh's runtime for it (without this the first live serve
    // panics "there is no reactor running").
    let handle = {
        let _guard = runtime.enter();
        net_sdk::org::serve_org_streaming_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller,
                  body: bytes::Bytes,
                  sink: ::net::adapter::net::cortex::RpcResponseSink| {
                let callable = callable.clone();
                async move {
                    run_py_org_streaming_handler(callable, drive, caller, body, sink, timeout).await
                }
            },
        )
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(org_serve_error(&e)))?
    };

    Ok(PyOrgServeHandle {
        inner: parking_lot::Mutex::new(Some(handle)),
    })
}

/// Serve a protected, privately-discoverable service with a STREAM OF
/// REQUESTS and one terminal response (OSDK §3; §4.4).
///
/// The handler is `handler(caller: dict, stream: RequestStreamRecv) -> bytes`
/// (a `def` or an `async def`): iterate `stream` to drain the upload — it
/// fences to EOF on retirement — and return the terminal response as
/// `bytes`. `caller` carries the five verified fields plus `is_same_org`.
/// Raising surfaces as an application error, never as an admission denial. A
/// non-callable handler is refused here, at registration.
///
/// `handler_timeout_ms` and the handler-drop contract are exactly
/// [`serve_org_streaming`]'s (including the diagnostics level of the
/// `RequestStreamRecv`): the retire supervisor may drop the handler future
/// without a final poll, cancellation is observed only through the retirement
/// observables, and a handler must never assume a handler-side cancel event.
///
/// Requires an installed node authority.
#[pyfunction]
#[pyo3(signature = (mesh, service, access, handler, handler_timeout_ms=None))]
pub fn serve_org_client_stream(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    service: String,
    access: &str,
    handler: Py<PyAny>,
    handler_timeout_ms: Option<u64>,
) -> PyResult<PyOrgServeHandle> {
    let drive = org_handler_drive(py, &handler, "serve_org_client_stream")?;
    let node = mesh.node_arc_clone()?;
    let access = super::org::access_from_str(access)?;
    let timeout = org_handler_timeout(handler_timeout_ms);
    let callable = Arc::new(handler);
    let runtime = mesh.runtime_arc();
    let runtime_for_handler = runtime.clone();

    let handle = {
        let _guard = runtime.enter();
        net_sdk::org::serve_org_client_stream_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller,
                  requests: ::net::adapter::net::cortex::RequestStream| {
                let callable = callable.clone();
                let runtime = runtime_for_handler.clone();
                async move {
                    run_py_org_client_stream_handler(
                        callable, drive, caller, requests, runtime, timeout,
                    )
                    .await
                }
            },
        )
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(org_serve_error(&e)))?
    };

    Ok(PyOrgServeHandle {
        inner: parking_lot::Mutex::new(Some(handle)),
    })
}

/// Serve a protected, privately-discoverable service BIDIRECTIONALLY (OSDK
/// §3; §4.4).
///
/// The handler is `handler(caller: dict, stream: RequestStreamRecv, sink:
/// ResponseSinkSend) -> None` (a `def` or an `async def`): drain `stream`,
/// emit response chunks through `sink.send(bytes)`; the substrate emits the
/// terminal frame at handler return. `caller` carries the five verified
/// fields plus `is_same_org`. Raising surfaces as an application error, never
/// as an admission denial. A non-callable handler is refused here, at
/// registration.
///
/// `handler_timeout_ms` and the handler-drop contract are exactly
/// [`serve_org_streaming`]'s (including the diagnostics level of the
/// `RequestStreamRecv`): the retire supervisor may drop the handler future
/// without a final poll, cancellation is observed only through the retirement
/// observables, and a handler must never assume a handler-side cancel event.
///
/// Requires an installed node authority.
#[pyfunction]
#[pyo3(signature = (mesh, service, access, handler, handler_timeout_ms=None))]
pub fn serve_org_duplex(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    service: String,
    access: &str,
    handler: Py<PyAny>,
    handler_timeout_ms: Option<u64>,
) -> PyResult<PyOrgServeHandle> {
    let drive = org_handler_drive(py, &handler, "serve_org_duplex")?;
    let node = mesh.node_arc_clone()?;
    let access = super::org::access_from_str(access)?;
    let timeout = org_handler_timeout(handler_timeout_ms);
    let callable = Arc::new(handler);
    let runtime = mesh.runtime_arc();
    let runtime_for_handler = runtime.clone();

    let handle = {
        let _guard = runtime.enter();
        net_sdk::org::serve_org_duplex_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller,
                  requests: ::net::adapter::net::cortex::RequestStream,
                  sink: ::net::adapter::net::cortex::RpcResponseSink| {
                let callable = callable.clone();
                let runtime = runtime_for_handler.clone();
                async move {
                    run_py_org_duplex_handler(
                        callable, drive, caller, requests, sink, runtime, timeout,
                    )
                    .await
                }
            },
        )
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(org_serve_error(&e)))?
    };

    Ok(PyOrgServeHandle {
        inner: parking_lot::Mutex::new(Some(handle)),
    })
}
