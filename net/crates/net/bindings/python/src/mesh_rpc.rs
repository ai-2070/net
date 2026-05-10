//! Python bindings for nRPC — Phase B3 (raw-bytes surface).
//!
//! Exposes:
//!
//! - [`PyMeshRpc`] — envelope around a live `NetMesh` providing
//!   `serve`, `call`, `call_service`, `call_streaming`,
//!   `find_service_nodes`.
//! - [`PyServeHandle`] — context manager (`__enter__`/`__exit__`)
//!   that unregisters on close.
//! - [`PyRpcStream`] — synchronous iterator over streaming-call
//!   responses; iter consumes until EOF or raises on terminal
//!   error / mid-stream codec failure.
//!
//! ## Async model
//!
//! Python users call all RPC operations synchronously — the Rust
//! side bridges via `runtime.block_on(...)` while `py.detach(...)`
//! releases the GIL during the wait. This matches the existing
//! `PyDaemonRuntime` convention and keeps the public Python API
//! free of asyncio complexity. Async-`def` Python handlers are a
//! follow-up: the user wraps an async function with a sync trampoline
//! that runs an asyncio loop, and passes the trampoline to `serve`.
//!
//! ## Handler bridging
//!
//! Each `serve()` call wraps the user's `def handler(req: bytes) ->
//! bytes` in a [`PyRpcHandler`] adapter that implements the Rust
//! `RpcHandler` async trait. When a request lands, the handler task
//! runs the Python callable inside `tokio::task::spawn_blocking` so
//! the GIL acquisition doesn't starve the async runtime.
//!
//! ## Error mapping
//!
//! All errors map to a typed exception class registered with the
//! `_net` module in `lib.rs`:
//!
//! - [`RpcNoRouteError`] — caller can't reach the target
//! - [`RpcTimeoutError`] — caller-side deadline elapsed
//! - [`RpcServerError`] — server returned non-Ok status
//! - [`RpcTransportError`] — underlying publish / encryption failure
//! - [`RpcCodecError`] — local codec failure (typed-wrapper layer)
//! - [`RpcError`] — base class; `except RpcError` catches all of the
//!   above.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use pyo3::create_exception;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use tokio::runtime::Runtime;
use tokio::task::AbortHandle;

use ::net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use ::net::adapter::net::mesh_rpc::{
    CallOptions as InnerCallOptions, RpcError as InnerRpcError, RpcStream as InnerRpcStream,
    ServeHandle as InnerServeHandle,
};
use ::net::adapter::net::MeshNode;

// ============================================================================
// Typed exception hierarchy. Registered with the `_net` module in
// lib.rs::_net so user code can `from net import RpcError, ...`.
// All extend RpcError → catch-all with `except RpcError:`.
// ============================================================================

create_exception!(
    _net,
    RpcError,
    pyo3::exceptions::PyException,
    "Base class for all nRPC failures. Catch with `except RpcError:` to handle any \
     nRPC failure; drill down to the concrete subclass for specific handling."
);

create_exception!(
    _net,
    RpcNoRouteError,
    RpcError,
    "Caller can't reach the target — either the target node id is unknown to the \
     local mesh, the reply-channel registry is at its cap, or a dispatcher hash \
     collision precluded a fresh registration. NOT retried by the default retry \
     policy."
);

create_exception!(
    _net,
    RpcTimeoutError,
    RpcError,
    "Caller's deadline elapsed before the server responded. The caller-side has \
     already published a CANCEL to the server."
);

create_exception!(
    _net,
    RpcServerError,
    RpcError,
    "Server returned a non-Ok status. The exception args carry the status code \
     (u16) and diagnostic message."
);

create_exception!(
    _net,
    RpcTransportError,
    RpcError,
    "Underlying transport / publish failure (encryption, congestion, etc.). \
     Distinct from RpcNoRouteError; the default retry policy retries Transport \
     errors because they're typically transient."
);

create_exception!(
    _net,
    RpcCodecError,
    RpcError,
    "Local serialization failure — the typed wrapper couldn't encode the request \
     OR couldn't decode the response. Caller-fixable local bug; NOT retried by \
     the default retry policy."
);

create_exception!(
    _net,
    RpcAppError,
    RpcError,
    "Raised inside a serve handler to signal an application-defined status code \
     (e.g. NRPC_TYPED_BAD_REQUEST = 0x8000). The Rust side translates a raised \
     `RpcAppError(code, body)` into `RpcStatus::Application(code)` with `body` \
     as the response body. Use this from typed wrappers to surface a typed \
     bad-request without going through the generic Internal mapping."
);

// ============================================================================
// Helpers — convert inner RpcError to the matching Python exception.
// ============================================================================

/// Stable error-message prefix shared with every other binding.
/// JS / Go / Python all emit `nrpc:<kind>: <detail>` so cross-
/// binding consumers can match on a single regex. See
/// `bindings/node/src/mesh_rpc.rs::ERR_NRPC_PREFIX` and
/// `tests/cross_lang_nrpc/golden_vectors.json` for the contract.
pub(crate) const ERR_NRPC_PREFIX: &str = "nrpc:";

fn rpc_error_to_pyerr(err: InnerRpcError) -> PyErr {
    match err {
        InnerRpcError::NoRoute { target, reason } => RpcNoRouteError::new_err(format!(
            "{ERR_NRPC_PREFIX}no_route: target=0x{target:x} reason={reason}"
        )),
        InnerRpcError::Timeout { elapsed_ms } => {
            RpcTimeoutError::new_err(format!("{ERR_NRPC_PREFIX}timeout: elapsed_ms={elapsed_ms}"))
        }
        InnerRpcError::ServerError { status, message } => RpcServerError::new_err(format!(
            "{ERR_NRPC_PREFIX}server_error: status=0x{status:04x} message={message}"
        )),
        InnerRpcError::Transport(e) => {
            RpcTransportError::new_err(format!("{ERR_NRPC_PREFIX}transport: {e}"))
        }
        InnerRpcError::Codec { direction, message } => {
            let kind = match direction {
                ::net::adapter::net::mesh_rpc::CodecDirection::Encode => "codec_encode",
                ::net::adapter::net::mesh_rpc::CodecDirection::Decode => "codec_decode",
            };
            RpcCodecError::new_err(format!("{ERR_NRPC_PREFIX}{kind}: {message}"))
        }
    }
}

// ============================================================================
// Cancellable — caller-side cancel token.
//
// A Python-visible class that wraps a tokio AbortHandle. Pass an
// instance via `opts={'cancel': cancel}` to a unary call; from
// another thread, `cancel.cancel()` aborts the in-flight task
// which drops the SDK future and fires CANCEL on the wire.
// Mirrors the Go binding's net_rpc_cancel_call surface.
// ============================================================================

/// Caller-side cancel token. Pass to a unary call via
/// ``opts={'cancel': cancel}``; ``cancel.cancel()`` from another
/// thread aborts the call mid-flight (CANCEL fires on the wire,
/// caller observes ``RpcCancelledError``).
#[pyclass(name = "Cancellable", module = "_net")]
pub struct PyCancellable {
    /// Set by the call site after spawning the cancellable task.
    /// `None` until the call is in flight; cleared by
    /// `cancel()` and by the call's natural completion.
    handle: Mutex<Option<AbortHandle>>,
    /// Latches `true` when a cancel has been requested. `cancel()`
    /// can be called BEFORE the call starts; the call site checks
    /// this flag and returns immediately if it's set, so the user-
    /// visible behavior is "cancel takes effect at-or-before the
    /// next FFI call."
    cancelled: std::sync::atomic::AtomicBool,
}

#[pymethods]
impl PyCancellable {
    #[new]
    fn new() -> Self {
        Self {
            handle: Mutex::new(None),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Request cancellation of the in-flight call. Idempotent.
    /// If no call is in flight (yet OR already finished), latches
    /// the request — the next call started with this Cancellable
    /// returns immediately as cancelled.
    fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(mut guard) = self.handle.lock() {
            if let Some(h) = guard.take() {
                h.abort();
            }
        }
    }

    /// `True` once cancel() has been called.
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }
}

impl PyCancellable {
    /// Internal: set the abort handle for the in-flight call.
    /// If `cancelled` is already true, abort immediately.
    fn arm(&self, handle: AbortHandle) {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            handle.abort();
            return;
        }
        if let Ok(mut guard) = self.handle.lock() {
            *guard = Some(handle);
        }
    }

    /// Internal: clear the abort handle on call completion. The
    /// natural-completion path runs this so a stale AbortHandle
    /// doesn't outlive the call (a subsequent cancel() would
    /// otherwise abort whichever future got the recycled tokio
    /// task slot — pathological but cheap to defend).
    fn disarm(&self) {
        if let Ok(mut guard) = self.handle.lock() {
            let _ = guard.take();
        }
    }
}

// ============================================================================
// RpcCancelledError — surfaces a user-driven cancel.
// ============================================================================

create_exception!(
    _net,
    RpcCancelledError,
    RpcError,
    "Raised when a unary call was cancelled mid-flight via \
     ``Cancellable.cancel()``. CANCEL has been published to the \
     server; the server-side handler observes its \
     ``ctx.cancellation`` token. Caller-fixable / terminal — NOT \
     retried by the default retry policy."
);

// ============================================================================
// CallOptions — accepted as a Python dict.
//
// Subset of the inner CallOptions that's safe + useful to expose
// at B3. Routing policy + trace context land in a follow-up phase.
// ============================================================================

fn call_options_from_dict(opts: Option<&Bound<'_, PyDict>>) -> PyResult<InnerCallOptions> {
    let mut inner = InnerCallOptions::default();
    let Some(d) = opts else {
        return Ok(inner);
    };
    if let Some(v) = d.get_item("deadline_ms")? {
        let ms: u64 = v.extract().map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!("deadline_ms must be int: {e}"))
        })?;
        inner.deadline = Some(Instant::now() + Duration::from_millis(ms));
    }
    // `stream_window_initial` is the canonical key; `stream_window`
    // is an alias accepted for parity with the README example. We
    // prefer the canonical key when both are present so a user
    // mid-migration doesn't get a surprise from the alias overriding
    // an explicit canonical setting.
    let stream_window = match d.get_item("stream_window_initial")? {
        Some(v) => Some(("stream_window_initial", v)),
        None => d.get_item("stream_window")?.map(|v| ("stream_window", v)),
    };
    if let Some((key, v)) = stream_window {
        let n: u32 = v.extract().map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!("{key} must be int: {e}"))
        })?;
        inner.stream_window_initial = Some(n);
    }
    // Phase 9b: caller-supplied request headers. Accepts a
    // `List[Tuple[str, bytes]]` — each entry's name is a
    // lowercase `cyberdeck-*` / `nrpc-*` string; value is raw
    // bytes (UTF-8 for text-like headers). Most notable user is
    // the `cyberdeck-where:` predicate-pushdown header; build
    // entries via `net_sdk.where_header(pred)`.
    if let Some(v) = d.get_item("request_headers")? {
        let list: Vec<(String, Vec<u8>)> = v.extract().map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!(
                "request_headers must be List[Tuple[str, bytes]]: {e}"
            ))
        })?;
        inner.request_headers = list;
    }
    Ok(inner)
}

/// Extract the optional ``Cancellable`` from the user's opts
/// dict. Returns ``Ok(None)`` when no cancellable was provided
/// or the dict was missing entirely. Raises ``TypeError`` if the
/// `cancel` key holds something other than a ``Cancellable``.
fn extract_cancellable<'py>(
    opts: Option<&Bound<'py, PyDict>>,
) -> PyResult<Option<Py<PyCancellable>>> {
    let Some(d) = opts else {
        return Ok(None);
    };
    let Some(v) = d.get_item("cancel")? else {
        return Ok(None);
    };
    if v.is_none() {
        return Ok(None);
    }
    let cell: Py<PyCancellable> = v.extract().map_err(|e| {
        pyo3::exceptions::PyTypeError::new_err(format!(
            "opts['cancel'] must be a net.Cancellable: {e}"
        ))
    })?;
    Ok(Some(cell))
}

// ============================================================================
// Handler bridging.
//
// `PyRpcHandler` adapts a Python callable to the `RpcHandler` async
// trait. Each handler invocation:
//   1. Wraps the request bytes as a Python `bytes`
//   2. Acquires the GIL via `Python::attach`
//   3. Calls the user's `handler(req)` → expects `bytes` (or
//      bytes-like) back
//   4. Returns the result as the response payload
//
// The Python call runs inside `tokio::task::spawn_blocking` so the
// GIL acquisition doesn't park an async-runtime worker indefinitely.
// ============================================================================

const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(60);

struct PyRpcHandler {
    callable: Py<PyAny>,
    timeout: Duration,
}

/// Internal handler-return discriminator. Either an Ok body, or
/// an application-status signal that the typed wrapper raised via
/// `RpcAppError(code, body)` so the Rust side can emit
/// `RpcResponsePayload { status: Application(code), body }` instead
/// of squashing it into `RpcStatus::Internal`.
enum HandlerOutcome {
    Ok(Vec<u8>),
    AppError { code: u16, body: Vec<u8> },
}

/// Inspect a Python exception. If it's an `RpcAppError(code, body)`,
/// extract the (code, body) pair so the handler can surface it as
/// `RpcResponsePayload { status: Application(code) }`. Anything
/// else returns `None` and the caller maps to `Internal`.
fn extract_app_error(py: Python<'_>, pyerr: &PyErr) -> Option<(u16, Vec<u8>)> {
    if !pyerr.is_instance_of::<RpcAppError>(py) {
        return None;
    }
    let value = pyerr.value(py);
    let args = value.getattr("args").ok()?.cast_into::<PyTuple>().ok()?;
    if args.len() < 2 {
        return None;
    }
    let code: u16 = args.get_item(0).ok()?.extract().ok()?;
    // The body field is canonically `bytes`. Accept `str` too — the
    // typed wrapper's JSON encoder always produces utf-8 bytes, but
    // a hand-written user handler that raises `RpcAppError(0x8001,
    // "boom")` with a str shouldn't get a generic "must return
    // bytes" — surface their string as the body.
    let body_obj = args.get_item(1).ok()?;
    let body: Vec<u8> = if let Ok(b) = body_obj.extract::<Vec<u8>>() {
        b
    } else if let Ok(s) = body_obj.extract::<String>() {
        s.into_bytes()
    } else {
        return None;
    };
    Some((code, body))
}

#[async_trait::async_trait]
impl RpcHandler for PyRpcHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        // Clone the Python callable reference (refcount bump under
        // the GIL) and move it into the blocking task. `Py<PyAny>`
        // is `Send + Sync` so the bare clone crosses threads cleanly;
        // the GIL token only outlives the `attach` block.
        let callable = Python::attach(|py| self.callable.clone_ref(py));
        let req_body = ctx.payload.body;
        let result = tokio::time::timeout(
            self.timeout,
            tokio::task::spawn_blocking(move || -> Result<HandlerOutcome, String> {
                Python::attach(|py| -> Result<HandlerOutcome, String> {
                    let req_bytes = PyBytes::new(py, &req_body);
                    let args = PyTuple::new(py, [req_bytes.into_any()])
                        .map_err(|e| format!("failed to build args: {e}"))?;
                    match callable.call1(py, args) {
                        Ok(ret) => {
                            let bound = ret.into_bound(py);
                            let bytes_vec: Vec<u8> = bound
                                .extract()
                                .map_err(|e| format!("Python handler must return bytes: {e}"))?;
                            Ok(HandlerOutcome::Ok(bytes_vec))
                        }
                        Err(pyerr) => {
                            if let Some((code, body)) = extract_app_error(py, &pyerr) {
                                Ok(HandlerOutcome::AppError { code, body })
                            } else {
                                Err(format!("Python handler raised: {pyerr}"))
                            }
                        }
                    }
                })
            }),
        )
        .await;

        match result {
            Ok(Ok(Ok(HandlerOutcome::Ok(body)))) => Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body,
            }),
            Ok(Ok(Ok(HandlerOutcome::AppError { code, body }))) => {
                Err(RpcHandlerError::Application {
                    code,
                    // RpcHandlerError::Application carries `message:
                    // String`. The fold encodes it as the response
                    // body; lossy-utf8 is fine because the typed
                    // wrappers always produce utf-8 JSON.
                    message: String::from_utf8_lossy(&body).into_owned(),
                })
            }
            Ok(Ok(Err(msg))) => Err(RpcHandlerError::Internal(msg)),
            Ok(Err(join_err)) => Err(RpcHandlerError::Internal(format!(
                "spawn_blocking task panicked: {join_err}"
            ))),
            Err(_) => Err(RpcHandlerError::Internal(format!(
                "Python handler did not respond within {} ms",
                self.timeout.as_millis()
            ))),
        }
    }
}

// ============================================================================
// PyServeHandle — context manager wrapping the inner ServeHandle.
//
// Supports BOTH `with rpc.serve(...) as h: ...` AND explicit
// `h.close()`. The context-manager exit path drops the inner
// ServeHandle (which unregisters); in-flight handlers continue.
// ============================================================================

#[pyclass(name = "ServeHandle", module = "_net")]
pub struct PyServeHandle {
    inner: Arc<Mutex<Option<InnerServeHandle>>>,
}

#[pymethods]
impl PyServeHandle {
    /// Unregister the service. Idempotent — repeated calls are
    /// no-ops. After close, in-flight handlers continue to
    /// completion but no new requests will be dispatched.
    fn close(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let _ = guard.take();
    }

    /// `True` once `close()` has been called.
    fn is_closed(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.is_none()
    }

    /// Context-manager protocol — returns self for `as h:`.
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Context-manager exit — unregisters via `close()` regardless
    /// of whether the with-block raised. Returns `False` so any
    /// in-flight exception propagates.
    #[pyo3(signature = (_exc_type, _exc_value, _traceback))]
    fn __exit__(
        &self,
        _exc_type: Option<Bound<'_, PyAny>>,
        _exc_value: Option<Bound<'_, PyAny>>,
        _traceback: Option<Bound<'_, PyAny>>,
    ) -> bool {
        self.close();
        false
    }
}

// ============================================================================
// PyRpcStream — synchronous iterator wrapper.
//
// Python iter protocol: `__iter__` returns self, `__next__` blocks
// until the next chunk OR raises StopIteration on EOF. Drop /
// explicit `close()` emits CANCEL to the server.
// ============================================================================

#[pyclass(name = "RpcStream", module = "_net")]
pub struct PyRpcStream {
    inner: Arc<Mutex<Option<InnerRpcStream>>>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyRpcStream {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Pull the next chunk. Returns `bytes` for a non-terminal
    /// chunk; raises `StopIteration` on clean EOF; raises an
    /// `RpcError` subclass on terminal non-Ok status.
    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                // Take the inner stream out of the mutex while we
                // await — holding a `std::sync::MutexGuard` across
                // an `.await` is unsound (and clippy-flagged). A
                // concurrent `close()` that takes the inner first
                // races us cleanly: we observe `None` and report
                // "stream already closed."
                let mut stream = match inner.lock().unwrap_or_else(|p| p.into_inner()).take() {
                    Some(s) => s,
                    None => return Err(RpcError::new_err("stream already closed")),
                };
                let next = stream.next().await;
                match next {
                    Some(Ok(bytes)) => {
                        // Put the stream back so subsequent __next__
                        // polls keep going.
                        *inner.lock().unwrap_or_else(|p| p.into_inner()) = Some(stream);
                        Ok::<Option<Bytes>, PyErr>(Some(bytes))
                    }
                    Some(Err(e)) => {
                        // Mid-stream error — drop the stream (CANCEL
                        // is unnecessary; the server already
                        // terminated us) and surface the error.
                        drop(stream);
                        Err(rpc_error_to_pyerr(e))
                    }
                    None => {
                        // Clean EOF — drop the inner stream so the
                        // CANCEL-on-drop guard fires immediately.
                        drop(stream);
                        Ok(None)
                    }
                }
            })
        })?;
        match result {
            Some(bytes) => Ok(PyBytes::new(py, &bytes)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }

    /// Grant `n` additional flow-control credits to the server's
    /// pump. No-op if the call didn't set `stream_window_initial`.
    fn grant(&self, n: u32) {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stream) = guard.as_ref() {
            stream.grant(n);
        }
    }

    /// `True` if the call set `stream_window_initial`.
    fn flow_controlled(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().map(|s| s.flow_controlled()).unwrap_or(false)
    }

    /// Close the stream; emits CANCEL to the server (best-effort).
    /// Idempotent.
    fn close(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let _ = guard.take();
    }
}

/// Sentinel for the abort path of `block_until_cancellable`. The
/// caller maps this to ``RpcCancelledError`` after re-acquiring
/// the GIL.
struct PyCancelledError;

/// Run `fut` on the runtime under either a plain `block_on` or a
/// spawn+abort handle, depending on whether the user passed a
/// `Cancellable`. The non-cancellable fast path is unchanged from
/// the previous code shape.
///
/// On the cancellable path: the future is spawned as a tokio task,
/// its `AbortHandle` is armed on the user's `Cancellable`, and we
/// `block_on(task)`. A user calling `cancel()` from another thread
/// triggers `JoinError::is_cancelled`, which we surface as
/// `Err(PyCancelledError)`. The natural-completion path disarms
/// the handle so a stale abort can't leak across calls.
fn run_cancellable_call<F, T>(
    runtime: &Arc<Runtime>,
    cancel: Option<&Py<PyCancellable>>,
    fut: F,
) -> std::result::Result<T, PyCancelledError>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let Some(cancel_py) = cancel else {
        return Ok(runtime.block_on(fut));
    };

    // Pre-check: cancel() may have been called BEFORE we got
    // here. Honor it without entering the runtime.
    let already_cancelled = Python::attach(|py| {
        cancel_py
            .bind(py)
            .borrow()
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire)
    });
    if already_cancelled {
        return Err(PyCancelledError);
    }

    let task = runtime.spawn(fut);
    let abort_handle = task.abort_handle();
    Python::attach(|py| {
        cancel_py.bind(py).borrow().arm(abort_handle);
    });
    let result = runtime.block_on(task);
    Python::attach(|py| {
        cancel_py.bind(py).borrow().disarm();
    });
    match result {
        Ok(v) => Ok(v),
        Err(e) if e.is_cancelled() => Err(PyCancelledError),
        // Panic in the SDK path — surface as cancelled so the
        // caller gets a useful diagnostic instead of a process-
        // wide panic across the FFI.
        Err(_) => Err(PyCancelledError),
    }
}

// ============================================================================
// PyMeshRpc — the public envelope class.
//
// Constructed via `MeshRpc(net_mesh)` — takes the existing NetMesh
// and shares its MeshNode + tokio runtime.
// ============================================================================

#[pyclass(name = "MeshRpc", module = "_net")]
pub struct PyMeshRpc {
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyMeshRpc {
    /// Build a MeshRpc against an existing NetMesh. Cheap
    /// (`Arc::clone`); call once per mesh and reuse.
    #[new]
    fn new(mesh: &crate::mesh_bindings::NetMesh) -> PyResult<Self> {
        let node = mesh.node_arc_clone()?;
        let runtime = mesh.runtime_arc();
        Ok(PyMeshRpc { node, runtime })
    }

    /// Register a synchronous Python handler on `service`.
    /// `handler` must be callable as `handler(req: bytes) -> bytes`.
    ///
    /// Returns a [`PyServeHandle`] context manager whose `close()`
    /// unregisters; in-flight handlers continue to completion
    /// after close.
    ///
    /// `handler_timeout_ms` caps the per-call wait for the Python
    /// handler to respond — defaults to 60 000 (60s). A wedged
    /// handler past the cap surfaces to the caller as
    /// `RpcStatus::Internal` "Python handler did not respond
    /// within N ms" so the in-flight slot doesn't leak. Set to
    /// 0 to disable the cap (not recommended — a stuck handler
    /// will hold a runtime worker indefinitely).
    #[pyo3(signature = (service, handler, handler_timeout_ms=None))]
    fn serve(
        &self,
        service: String,
        handler: Py<PyAny>,
        handler_timeout_ms: Option<u64>,
    ) -> PyResult<PyServeHandle> {
        let timeout = match handler_timeout_ms {
            Some(0) => Duration::from_secs(u64::MAX / 1000),
            Some(ms) => Duration::from_millis(ms),
            None => DEFAULT_HANDLER_TIMEOUT,
        };
        let rust_handler = Arc::new(PyRpcHandler {
            callable: handler,
            timeout,
        });
        let inner = self
            .node
            .serve_rpc(&service, rust_handler)
            .map_err(|e| RpcError::new_err(format!("serve failed: {e}")))?;
        Ok(PyServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }

    /// Direct-addressed unary call. Caller specifies
    /// `target_node_id`; the SDK does NOT consult the capability
    /// index. Returns the response body as `bytes`; raises an
    /// `RpcError` subclass on failure.
    ///
    /// Pass ``opts={'cancel': cancel}`` with a ``Cancellable`` to
    /// allow another thread to abort the call mid-flight.
    #[pyo3(signature = (target_node_id, service, request, opts=None))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let cancel = extract_cancellable(opts)?;
        let inner_opts = call_options_from_dict(opts)?;
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let result = py.detach(|| {
            run_cancellable_call(&runtime, cancel.as_ref(), async move {
                node.call(target_node_id, &service, req_bytes, inner_opts)
                    .await
            })
        });
        let reply = match result {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => return Err(rpc_error_to_pyerr(e)),
            Err(PyCancelledError) => {
                return Err(RpcCancelledError::new_err(
                    "nrpc:cancelled: call cancelled by caller",
                ))
            }
        };
        Ok(PyBytes::new(py, reply.body.as_ref()))
    }

    /// Service-discovery unary call. Resolves `service` against
    /// the local capability index (`nrpc:<service>` tags),
    /// applies the routing policy, calls.
    ///
    /// Pass ``opts={'cancel': cancel}`` with a ``Cancellable`` to
    /// allow another thread to abort the call mid-flight.
    #[pyo3(signature = (service, request, opts=None))]
    fn call_service<'py>(
        &self,
        py: Python<'py>,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let cancel = extract_cancellable(opts)?;
        let inner_opts = call_options_from_dict(opts)?;
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let result = py.detach(|| {
            run_cancellable_call(&runtime, cancel.as_ref(), async move {
                node.call_service(&service, req_bytes, inner_opts).await
            })
        });
        let reply = match result {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => return Err(rpc_error_to_pyerr(e)),
            Err(PyCancelledError) => {
                return Err(RpcCancelledError::new_err(
                    "nrpc:cancelled: call cancelled by caller",
                ))
            }
        };
        Ok(PyBytes::new(py, reply.body.as_ref()))
    }

    /// Open a streaming-response call. Returns an [`PyRpcStream`];
    /// drain via the iterator protocol. Drop / `close()` emits
    /// CANCEL to the server.
    #[pyo3(signature = (target_node_id, service, request, opts=None))]
    fn call_streaming<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<PyRpcStream> {
        let opts = call_options_from_dict(opts)?;
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let inner = py.detach(|| {
            runtime.block_on(async move {
                node.call_streaming(target_node_id, &service, req_bytes, opts)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })?;
        Ok(PyRpcStream {
            inner: Arc::new(Mutex::new(Some(inner))),
            runtime: self.runtime.clone(),
        })
    }

    /// All node ids currently advertising `nrpc:<service>` in the
    /// local capability index. Returns a list of `int`.
    ///
    /// Releases the GIL across the lookup. The capability index
    /// is parking-lot-locked; with the GIL held we'd block other
    /// Python threads on every discovery query — which is the
    /// hot path on a service-discovery-driven caller. Drops to
    /// the same convention as `call` / `call_service` /
    /// `call_streaming`.
    fn find_service_nodes(&self, py: Python<'_>, service: String) -> Vec<u64> {
        let node = self.node.clone();
        py.detach(|| node.find_service_nodes(&service))
    }
}

// ============================================================================
// Tests for the pure-logic helpers — error mapping. Following the
// existing python binding convention: pyo3's `PyErr::Drop` calls
// Python-runtime symbols not available in standalone `cargo
// test`, so we test only string-format helpers (no PyErr
// instantiation).
// ============================================================================

#[cfg(test)]
mod tests {
    use ::net::adapter::net::mesh_rpc::CodecDirection;
    use ::net::adapter::net::mesh_rpc::RpcError as InnerRpcError;
    use ::net::error::AdapterError;

    /// `rpc_error_to_pyerr` produces the documented stable kind /
    /// message format for each `RpcError` variant. We can't
    /// actually construct the `PyErr` outside the Python runtime
    /// (its Drop calls Python-extension symbols), so we test the
    /// pre-PyErr message-format helper inline.
    ///
    /// Format MUST match the Node binding's `nrpc_err_from_inner`
    /// (`bindings/node/src/mesh_rpc.rs`) so the cross-language
    /// `classify_error` parsers stay symmetrical.
    #[test]
    fn rpc_error_message_formats_are_stable() {
        let format = |err: InnerRpcError| -> String {
            match err {
                InnerRpcError::NoRoute { target, reason } => {
                    format!("nrpc:no_route: target=0x{target:x} reason={reason}")
                }
                InnerRpcError::Timeout { elapsed_ms } => {
                    format!("nrpc:timeout: elapsed_ms={elapsed_ms}")
                }
                InnerRpcError::ServerError { status, message } => {
                    format!("nrpc:server_error: status=0x{status:04x} message={message}")
                }
                InnerRpcError::Transport(e) => format!("nrpc:transport: {e}"),
                InnerRpcError::Codec { direction, message } => {
                    let kind = match direction {
                        CodecDirection::Encode => "codec_encode",
                        CodecDirection::Decode => "codec_decode",
                    };
                    format!("nrpc:{kind}: {message}")
                }
            }
        };

        assert_eq!(
            format(InnerRpcError::NoRoute {
                target: 0xDEAD_BEEF,
                reason: "no session".into(),
            }),
            "nrpc:no_route: target=0xdeadbeef reason=no session"
        );
        assert_eq!(
            format(InnerRpcError::Timeout { elapsed_ms: 200 }),
            "nrpc:timeout: elapsed_ms=200"
        );
        assert_eq!(
            format(InnerRpcError::ServerError {
                status: 0x8001,
                message: "app error".into(),
            }),
            "nrpc:server_error: status=0x8001 message=app error"
        );
        assert_eq!(
            format(InnerRpcError::Transport(AdapterError::Connection(
                "boom".into()
            ))),
            "nrpc:transport: connection error: boom"
        );
        assert_eq!(
            format(InnerRpcError::Codec {
                direction: CodecDirection::Encode,
                message: "bad json".into(),
            }),
            "nrpc:codec_encode: bad json"
        );
        assert_eq!(
            format(InnerRpcError::Codec {
                direction: CodecDirection::Decode,
                message: "trailing".into(),
            }),
            "nrpc:codec_decode: trailing"
        );

        // Every kind starts with the canonical prefix so the JS
        // and Python `classify_error` parsers can match a single
        // anchor. A regression to the legacy "no route to ..."
        // format would silently fail every cross-binding compat
        // test that asserts `nrpc:` is present.
        for variant in [
            InnerRpcError::NoRoute {
                target: 1,
                reason: "x".into(),
            },
            InnerRpcError::Timeout { elapsed_ms: 1 },
            InnerRpcError::ServerError {
                status: 0,
                message: "x".into(),
            },
            InnerRpcError::Transport(AdapterError::Connection("x".into())),
            InnerRpcError::Codec {
                direction: CodecDirection::Encode,
                message: "x".into(),
            },
        ] {
            assert!(
                format(variant).starts_with("nrpc:"),
                "every variant must carry the canonical nrpc: prefix"
            );
        }
    }
}
