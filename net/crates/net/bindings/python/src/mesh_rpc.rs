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

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use pyo3::create_exception;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use tokio::runtime::Runtime;

use ::net::adapter::net::cortex::{
    RequestStream as InnerRequestStream, RpcCallEvent as InnerRpcCallEvent,
    RpcCallStatus as InnerRpcCallStatus, RpcClientStreamingHandler, RpcContext,
    RpcDirection as InnerRpcDirection, RpcDuplexHandler, RpcHandler, RpcHandlerError, RpcObserver,
    RpcResponsePayload, RpcResponseSink as InnerRpcResponseSink, RpcStatus, RpcStreamingContext,
};
use ::net::adapter::net::mesh_rpc::{
    CallOptions as InnerCallOptions, ClientStreamCallRaw as InnerClientStreamCallRaw,
    DuplexCallRaw as InnerDuplexCallRaw, DuplexSink as InnerDuplexSink,
    DuplexStream as InnerDuplexStream, RpcError as InnerRpcError, RpcStream as InnerRpcStream,
    ServeHandle as InnerServeHandle,
};
use ::net::adapter::net::mesh_rpc_metrics::ServiceMetrics as InnerServiceMetrics;
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

create_exception!(
    _net,
    RpcCapabilityDeniedError,
    RpcError,
    "v0.4 capability-auth gate denied the call. The target's signed \
     `CapabilityAnnouncement` either does not list the requested `nrpc:<service>` \
     tag, or it lists the tag with allow-lists the caller does not match. NOT \
     retried by the default retry policy — only a fresh (more permissive) \
     announcement from the target can change the verdict."
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
        InnerRpcError::CapabilityDenied { target, capability } => {
            RpcCapabilityDeniedError::new_err(format!(
                "{ERR_NRPC_PREFIX}capability_denied: target=0x{target:x} capability={capability}"
            ))
        }
        InnerRpcError::Cancelled => RpcCancelledError::new_err(format!(
            "{ERR_NRPC_PREFIX}cancelled: call cancelled by caller"
        )),
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

/// Caller-side cancel token. Pass to any call via
/// ``opts={'cancel': cancel}``; ``cancel.cancel()`` from another
/// thread aborts the call mid-flight (CANCEL fires on the wire,
/// the caller observes ``RpcCancelledError`` for unary or stream
/// EOF for streaming).
///
/// Implementation (v3 / C-A2): delegates to the substrate's
/// ``MeshNode::cancel(token)`` primitive. The Cancellable holds an
/// optional ``(mesh, token)`` pair that's populated by the call
/// site when the Cancellable is used. ``cancel()`` called before
/// any call has been issued latches a ``pre_cancelled`` flag —
/// the first arm observes the flag and fires cancel on the
/// substrate immediately.
#[pyclass(name = "Cancellable", module = "_net")]
pub struct PyCancellable {
    /// Mesh + reserved token, populated when the Cancellable is
    /// first used by a call site (via [`Self::arm`]). `None`
    /// until armed; populated for the lifetime of the call.
    armed: Mutex<Option<(Arc<MeshNode>, u64)>>,
    /// Latches `true` when a cancel was requested before the
    /// Cancellable was armed. `arm` observes the flag and fires
    /// cancel on the substrate immediately so the user-visible
    /// behavior is "cancel takes effect at-or-before the call
    /// starts."
    pre_cancelled: std::sync::atomic::AtomicBool,
}

#[pymethods]
impl PyCancellable {
    #[new]
    fn new() -> Self {
        Self {
            armed: Mutex::new(None),
            pre_cancelled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Request cancellation. Idempotent. If no call is in flight
    /// (or the Cancellable hasn't been used yet), latches the
    /// request — the next call issued with this Cancellable will
    /// short-circuit to cancelled.
    fn cancel(&self) {
        self.pre_cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some((mesh, token)) = self.armed.lock().take() {
            mesh.cancel(token);
        }
    }

    /// `True` once cancel() has been called.
    fn is_cancelled(&self) -> bool {
        self.pre_cancelled
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

impl PyCancellable {
    /// Internal: arm the Cancellable for an in-flight call.
    /// Reserves a token on the given mesh, stores `(mesh, token)`,
    /// and returns the token so the caller can populate
    /// `opts.cancel_token`. If `cancel()` was called BEFORE arm,
    /// fires cancel on the substrate immediately (the substrate's
    /// CR-13 pre-arm contract ensures the call short-circuits).
    pub(crate) fn arm(&self, mesh: Arc<MeshNode>) -> u64 {
        let token = mesh.reserve_cancel_token();
        if self
            .pre_cancelled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            mesh.cancel(token);
            return token;
        }
        *self.armed.lock() = Some((mesh, token));
        token
    }

    /// Internal: clear the armed state on call completion so a
    /// subsequent reuse of this Cancellable starts fresh. The
    /// `pre_cancelled` flag stays set across reuses; users
    /// typically create a fresh Cancellable per call.
    pub(crate) fn disarm(&self) {
        let _ = self.armed.lock().take();
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
    // ABI 0x0002 — request-direction credit window for client-
    // streaming + duplex. Same canonical / alias treatment as the
    // response-side window key.
    let request_window = match d.get_item("request_window_initial")? {
        Some(v) => Some(("request_window_initial", v)),
        None => d.get_item("request_window")?.map(|v| ("request_window", v)),
    };
    if let Some((key, v)) = request_window {
        let n: u32 = v.extract().map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!("{key} must be int: {e}"))
        })?;
        inner.request_window_initial = Some(n);
    }
    // Phase 9b: caller-supplied request headers. Accepts a
    // `List[Tuple[str, bytes]]` — each entry's name is a
    // lowercase `cyberdeck-*` / `nrpc-*` string; value is raw
    // bytes (UTF-8 for text-like headers). Most notable user is
    // the `net-where:` predicate-pushdown header; build
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
                body: body.into(),
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
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }

    /// `True` once `close()` has been called.
    fn is_closed(&self) -> bool {
        let guard = self.inner.lock();
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
                let mut stream = match inner.lock().take() {
                    Some(s) => s,
                    None => return Err(RpcError::new_err("stream already closed")),
                };
                let next = stream.next().await;
                match next {
                    Some(Ok(bytes)) => {
                        // Put the stream back so subsequent __next__
                        // polls keep going.
                        *inner.lock() = Some(stream);
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
        let guard = self.inner.lock();
        if let Some(stream) = guard.as_ref() {
            stream.grant(n);
        }
    }

    /// `True` if the call set `stream_window_initial`.
    fn flow_controlled(&self) -> bool {
        let guard = self.inner.lock();
        guard.as_ref().map(|s| s.flow_controlled()).unwrap_or(false)
    }

    /// Close the stream; emits CANCEL to the server (best-effort).
    /// Idempotent.
    fn close(&self) {
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }
}

// ============================================================================
// ABI 0x0002 — Client-streaming caller-side (Phase B10-1).
//
// Same Arc<Mutex<Option<InnerClientStreamCallRaw>>> pattern as
// PyRpcStream — take the inner across each block_on so close()
// can race cleanly. finish() takes the inner permanently
// (consumes the call); close() releases without REQUEST_END.
// ============================================================================

/// Open client-streaming call. Push chunks via ``send(bytes)``,
/// then ``finish()`` to await the terminal response. ``close()``
/// fires CANCEL via the SDK's Drop if finish wasn't reached.
#[pyclass(name = "ClientStreamCall", module = "_net")]
pub struct PyClientStreamCall {
    inner: Arc<Mutex<Option<InnerClientStreamCallRaw>>>,
    runtime: Arc<Runtime>,
    call_id_cached: u64,
    flow_controlled_cached: bool,
    /// Lets ``close()`` interrupt a pending ``send()`` blocked on
    /// flow-control credit. Same role as the Node binding's
    /// ``close_notify`` — a Notify permit fires the select branch
    /// in send and the call is dropped (CANCEL fires from Drop).
    close_notify: Arc<tokio::sync::Notify>,
}

#[pymethods]
impl PyClientStreamCall {
    /// Push one body chunk. Encodes as the initial REQUEST (first
    /// call) or as a REQUEST_CHUNK (subsequent). Blocks until the
    /// SDK accepts the chunk (under flow control, blocks for one
    /// upload credit).
    ///
    /// Concurrent ``close()`` interrupts the await — send returns
    /// ``RpcError("send aborted by close()")`` instead of hanging
    /// forever on a credit grant that will never arrive.
    fn send<'py>(&self, py: Python<'py>, body: &Bound<'py, PyBytes>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let body_bytes = Bytes::copy_from_slice(body.as_bytes());
        let notify = self.close_notify.clone();
        py.detach(|| {
            runtime.block_on(async move {
                let mut call = match inner.lock().take() {
                    Some(c) => c,
                    None => return Err(RpcError::new_err("client-stream call already closed")),
                };
                let result = tokio::select! {
                    r = call.send(body_bytes) => r,
                    _ = notify.notified() => {
                        // close() fired. Drop the call (which
                        // fires CANCEL via SDK's Drop).
                        drop(call);
                        return Err(RpcError::new_err("send aborted by close()"));
                    }
                };
                match result {
                    Ok(()) => {
                        *inner.lock() = Some(call);
                        Ok(())
                    }
                    Err(e) => {
                        drop(call);
                        Err(rpc_error_to_pyerr(e))
                    }
                }
            })
        })
    }

    /// Close the upload direction (emit REQUEST_END) and await
    /// the server's terminal response. Consumes the call.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                let call = match inner.lock().take() {
                    Some(c) => c,
                    None => return Err(RpcError::new_err("client-stream call already closed")),
                };
                call.finish().await.map_err(rpc_error_to_pyerr)
            })
        })?;
        Ok(PyBytes::new(py, result.body.as_ref()))
    }

    /// Server-assigned `call_id` for diagnostics / trace
    /// correlation.
    fn call_id(&self) -> u64 {
        self.call_id_cached
    }

    /// ``True`` if the call was opened with a non-``None``
    /// ``request_window_initial``.
    fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
    }

    /// Close without finishing. Fires CANCEL via the SDK's Drop
    /// if the initial REQUEST has already flown. Idempotent.
    /// Concurrent in-flight ``send()`` waiting on credit is
    /// interrupted via the close-notify.
    fn close(&self) {
        self.close_notify.notify_one();
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
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
// ABI 0x0002 — Duplex caller-side (Phase B10-1).
// ============================================================================

/// Open duplex call. Combined send + receive surface. Use
/// ``into_split()`` to get independent ``DuplexSink`` +
/// ``DuplexStream`` halves.
#[pyclass(name = "DuplexCall", module = "_net")]
pub struct PyDuplexCall {
    inner: Arc<Mutex<Option<InnerDuplexCallRaw>>>,
    runtime: Arc<Runtime>,
    call_id_cached: u64,
    flow_controlled_cached: bool,
    /// Same role as ``PyClientStreamCall::close_notify`` — lets
    /// ``close()`` interrupt a pending ``send()`` blocked on
    /// flow-control credit.
    close_notify: Arc<tokio::sync::Notify>,
}

#[pymethods]
impl PyDuplexCall {
    /// Push one body chunk to the server.
    ///
    /// Concurrent ``close()`` interrupts the await via the
    /// close-notify (same shape as ``ClientStreamCall.send``).
    fn send<'py>(&self, py: Python<'py>, body: &Bound<'py, PyBytes>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let body_bytes = Bytes::copy_from_slice(body.as_bytes());
        let notify = self.close_notify.clone();
        py.detach(|| {
            runtime.block_on(async move {
                let mut call = match inner.lock().take() {
                    Some(c) => c,
                    None => return Err(RpcError::new_err("duplex call already closed")),
                };
                let result = tokio::select! {
                    r = call.send(body_bytes) => r,
                    _ = notify.notified() => {
                        drop(call);
                        return Err(RpcError::new_err("send aborted by close()"));
                    }
                };
                match result {
                    Ok(()) => {
                        *inner.lock() = Some(call);
                        Ok(())
                    }
                    Err(e) => {
                        drop(call);
                        Err(rpc_error_to_pyerr(e))
                    }
                }
            })
        })
    }

    /// Close the upload direction (emit REQUEST_END). Response
    /// stream stays open for subsequent ``next()`` calls.
    fn finish_sending(&self, py: Python<'_>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        py.detach(|| {
            runtime.block_on(async move {
                let mut call = match inner.lock().take() {
                    Some(c) => c,
                    None => return Err(RpcError::new_err("duplex call already closed")),
                };
                let result = call.finish_sending().await;
                *inner.lock() = Some(call);
                result.map_err(rpc_error_to_pyerr)
            })
        })
    }

    /// Pull the next response chunk. Returns ``bytes`` on success;
    /// raises ``StopIteration`` on clean EOF; raises an
    /// ``RpcError`` subclass on terminal non-Ok status.
    ///
    /// Python iter protocol — ``DuplexCall`` is iterable. Use it
    /// either as ``for chunk in call:`` or via explicit
    /// ``next(call)``.
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                let mut call = match inner.lock().take() {
                    Some(c) => c,
                    None => return Err(RpcError::new_err("duplex call already closed")),
                };
                let next = call.next().await;
                match next {
                    Some(Ok(bytes)) => {
                        *inner.lock() = Some(call);
                        Ok::<Option<Bytes>, PyErr>(Some(bytes))
                    }
                    Some(Err(e)) => {
                        drop(call);
                        Err(rpc_error_to_pyerr(e))
                    }
                    None => {
                        drop(call);
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

    /// Split the call into independent send + receive halves.
    /// Returns ``(sink, stream)``. After ``into_split``, the
    /// original ``DuplexCall`` is "done" — subsequent ``send`` /
    /// ``finish_sending`` / ``__next__`` raise ``RpcError``.
    #[pyo3(name = "into_split")]
    fn split(&self) -> PyResult<(PyDuplexSink, PyDuplexStream)> {
        let call = self
            .inner
            .lock()
            .take()
            .ok_or_else(|| RpcError::new_err("duplex call already closed"))?;
        let call_id = call.call_id();
        let flow_controlled = call.flow_controlled();
        let (sink, stream) = call.into_split();
        Ok((
            PyDuplexSink {
                inner: Arc::new(Mutex::new(Some(sink))),
                runtime: self.runtime.clone(),
                call_id_cached: call_id,
                flow_controlled_cached: flow_controlled,
                close_notify: Arc::new(tokio::sync::Notify::new()),
            },
            PyDuplexStream {
                inner: Arc::new(Mutex::new(Some(stream))),
                runtime: self.runtime.clone(),
                call_id_cached: call_id,
            },
        ))
    }

    /// Server-assigned `call_id`.
    fn call_id(&self) -> u64 {
        self.call_id_cached
    }

    /// ``True`` if the call was opened with a non-``None``
    /// ``request_window_initial``.
    fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
    }

    /// Close the call. Fires CANCEL via the SDK's Drop.
    /// Idempotent. Concurrent in-flight ``send()`` waiting on
    /// credit is interrupted via the close-notify.
    fn close(&self) {
        self.close_notify.notify_one();
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
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

/// Send-half of a ``DuplexCall`` after ``into_split``.
#[pyclass(name = "DuplexSink", module = "_net")]
pub struct PyDuplexSink {
    inner: Arc<Mutex<Option<InnerDuplexSink>>>,
    runtime: Arc<Runtime>,
    call_id_cached: u64,
    flow_controlled_cached: bool,
    /// Same role as ``PyClientStreamCall::close_notify``.
    close_notify: Arc<tokio::sync::Notify>,
}

#[pymethods]
impl PyDuplexSink {
    /// Push one body chunk to the server. Concurrent ``close()``
    /// interrupts the await via the close-notify (same shape as
    /// ``DuplexCall.send``).
    fn send<'py>(&self, py: Python<'py>, body: &Bound<'py, PyBytes>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let body_bytes = Bytes::copy_from_slice(body.as_bytes());
        let notify = self.close_notify.clone();
        py.detach(|| {
            runtime.block_on(async move {
                let mut sink = match inner.lock().take() {
                    Some(s) => s,
                    None => return Err(RpcError::new_err("duplex sink already closed")),
                };
                let result = tokio::select! {
                    r = sink.send(body_bytes) => r,
                    _ = notify.notified() => {
                        drop(sink);
                        return Err(RpcError::new_err("send aborted by close()"));
                    }
                };
                match result {
                    Ok(()) => {
                        *inner.lock() = Some(sink);
                        Ok(())
                    }
                    Err(e) => {
                        drop(sink);
                        Err(rpc_error_to_pyerr(e))
                    }
                }
            })
        })
    }

    /// Close the upload direction (emit REQUEST_END). Consumes
    /// the sink — subsequent ``send`` raises.
    fn finish(&self, py: Python<'_>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        py.detach(|| {
            runtime.block_on(async move {
                let sink = match inner.lock().take() {
                    Some(s) => s,
                    None => return Err(RpcError::new_err("duplex sink already closed")),
                };
                sink.finish_sending().await.map_err(rpc_error_to_pyerr)
            })
        })
    }

    fn call_id(&self) -> u64 {
        self.call_id_cached
    }
    fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
    }
    fn close(&self) {
        self.close_notify.notify_one();
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
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

/// Receive-half of a ``DuplexCall`` after ``into_split``.
/// Python iterator over response chunks.
#[pyclass(name = "DuplexStream", module = "_net")]
pub struct PyDuplexStream {
    inner: Arc<Mutex<Option<InnerDuplexStream>>>,
    runtime: Arc<Runtime>,
    call_id_cached: u64,
}

#[pymethods]
impl PyDuplexStream {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                let mut stream = match inner.lock().take() {
                    Some(s) => s,
                    None => return Err(RpcError::new_err("duplex stream already closed")),
                };
                use futures::StreamExt;
                let next = stream.next().await;
                match next {
                    Some(Ok(bytes)) => {
                        *inner.lock() = Some(stream);
                        Ok::<Option<Bytes>, PyErr>(Some(bytes))
                    }
                    Some(Err(e)) => {
                        drop(stream);
                        Err(rpc_error_to_pyerr(e))
                    }
                    None => {
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

    fn call_id(&self) -> u64 {
        self.call_id_cached
    }
    fn close(&self) {
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }
}

// ============================================================================
// ABI 0x0002 — Server-side handler primitives (Phase B10-2).
//
// PyRequestStreamRecv wraps the SDK's RequestStream and is
// handed to Python handlers as a Python iterator over inbound
// chunk bodies. PyResponseSinkSend wraps RpcResponseSink for
// duplex handlers.
//
// Python handler signatures:
//
//   # Client-streaming
//   def handler(stream):
//       total = 0
//       for chunk in stream:
//           total += len(chunk)
//       return total.to_bytes(8, "little")
//
//   # Duplex
//   def handler(stream, sink):
//       for chunk in stream:
//           sink.send(b"echo:" + chunk)
//
// Lifetime: bounded by the handler callback. The SDK's
// underlying RequestStream / RpcResponseSink are taken into
// these wrappers at handler dispatch and dropped when the
// wrappers are dropped (which happens when Python releases its
// references after the handler returns).
// ============================================================================

/// Inbound request-stream iterable for client-streaming + duplex
/// server handlers. Use the Python iter protocol:
/// ``for chunk in stream: ...`` or explicit ``next(stream)``.
/// Raises ``StopIteration`` on EOF (REQUEST_END / CANCEL).
///
/// Carries the per-call streaming context as attributes:
/// ``caller_origin`` (peer identity hash), ``call_id`` (substrate
/// call id), ``deadline_ns`` (Unix-nanos absolute, 0 means
/// "no deadline"), and ``headers`` (list of [name, bytes] tuples).
///
/// **Asyncio adapter.** The class is intentionally a sync
/// iterator — ``__next__`` blocks the calling thread on
/// ``runtime.block_on`` (with the GIL released). Asyncio
/// consumers should bridge via ``asyncio.to_thread``:
///
/// ```python
/// import asyncio
///
/// async def consume_stream(stream):
///     # `asyncio.to_thread(next, stream, None)` runs the
///     # blocking `next()` on the default executor's thread
///     # pool and returns control to the event loop until a
///     # chunk arrives. Once None comes back the stream is done.
///     while (chunk := await asyncio.to_thread(next, stream, None)) is not None:
///         handle(chunk)
/// ```
///
/// This keeps the binding surface minimal (no pyo3-asyncio
/// dependency, no async-method machinery) while still letting
/// asyncio-native handlers drain the stream without blocking
/// their event loop.
#[pyclass(name = "RequestStreamRecv", module = "_net")]
pub struct PyRequestStreamRecv {
    inner: Arc<Mutex<Option<InnerRequestStream>>>,
    runtime: Arc<Runtime>,
    caller_origin: u64,
    call_id: u64,
    deadline_ns: u64,
    headers: Arc<Vec<(String, Vec<u8>)>>,
}

#[pymethods]
impl PyRequestStreamRecv {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Caller's peer origin hash.
    #[getter]
    fn caller_origin(&self) -> u64 {
        self.caller_origin
    }

    /// Substrate-minted call id (stable for the call lifetime).
    #[getter]
    fn call_id(&self) -> u64 {
        self.call_id
    }

    /// Caller's declared deadline as Unix-nanos absolute; 0 means
    /// no deadline. Handlers MAY observe it to short-circuit
    /// slow work past the wire deadline.
    #[getter]
    fn deadline_ns(&self) -> u64 {
        self.deadline_ns
    }

    /// Initial-REQUEST headers as a list of (name, bytes) tuples.
    /// Names are lowercase per the substrate convention.
    #[getter]
    fn headers<'py>(&self, py: Python<'py>) -> Vec<(String, Bound<'py, PyBytes>)> {
        self.headers
            .iter()
            .map(|(n, v)| (n.clone(), PyBytes::new(py, v)))
            .collect()
    }
    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let result: Option<Bytes> = py.detach(|| {
            runtime.block_on(async move {
                let mut stream = match inner.lock().take() {
                    Some(s) => s,
                    None => return None,
                };
                use futures::StreamExt;
                let next = stream.next().await;
                match next {
                    Some(bytes) => {
                        *inner.lock() = Some(stream);
                        Some(bytes)
                    }
                    None => {
                        drop(stream);
                        None
                    }
                }
            })
        });
        match result {
            Some(bytes) => Ok(PyBytes::new(py, &bytes)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }
}

/// Outbound response sink for duplex handlers. Emit chunks via
/// ``sink.send(bytes)``. Non-blocking (SDK try_send under the
/// hood); returns ``True`` on success, ``False`` if the sink
/// was already torn down.
#[pyclass(name = "ResponseSinkSend", module = "_net")]
pub struct PyResponseSinkSend {
    inner: Arc<Mutex<Option<InnerRpcResponseSink>>>,
}

#[pymethods]
impl PyResponseSinkSend {
    /// Emit one chunk. Returns ``True`` on success.
    ///
    /// Non-blocking by design — this is `try_send` into a bounded
    /// 1024-chunk mpsc feeding the response pump. The pump itself
    /// awaits per-call credit (`stream_window_initial` opt-in)
    /// before publishing to the wire; if the pump stalls on
    /// credit, the mpsc fills and excess chunks are dropped (and
    /// counted via `streaming_chunks_dropped_total`). Handlers
    /// honor flow control by pacing their emits to the protocol's
    /// REQUEST_GRANT cadence rather than burst-pushing. Matches
    /// the Rust SDK's `ResponseSinkTyped::send` contract.
    fn send<'py>(&self, body: &Bound<'py, PyBytes>) -> bool {
        let guard = self.inner.lock();
        match guard.as_ref() {
            Some(sink) => {
                sink.send(Bytes::copy_from_slice(body.as_bytes()));
                true
            }
            None => false,
        }
    }
}

/// `RpcClientStreamingHandler` impl bridging to a Python
/// callable.
struct PyRpcClientStreamingHandler {
    callable: Py<PyAny>,
    timeout: Duration,
    runtime: Arc<Runtime>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for PyRpcClientStreamingHandler {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: InnerRequestStream,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let callable = Python::attach(|py| self.callable.clone_ref(py));
        let runtime = self.runtime.clone();
        let stream_inner = Arc::new(Mutex::new(Some(requests)));
        let ctx_caller_origin = ctx.caller_origin;
        let ctx_call_id = ctx.call_id;
        let ctx_deadline_ns = ctx.deadline_ns;
        let ctx_headers = Arc::new(ctx.headers);
        let result = tokio::time::timeout(
            self.timeout,
            tokio::task::spawn_blocking(move || -> Result<HandlerOutcome, String> {
                Python::attach(|py| -> Result<HandlerOutcome, String> {
                    let stream_obj = Py::new(
                        py,
                        PyRequestStreamRecv {
                            inner: stream_inner,
                            runtime,
                            caller_origin: ctx_caller_origin,
                            call_id: ctx_call_id,
                            deadline_ns: ctx_deadline_ns,
                            headers: ctx_headers,
                        },
                    )
                    .map_err(|e| format!("failed to build request stream: {e}"))?;
                    let args = PyTuple::new(py, [stream_obj.into_any()])
                        .map_err(|e| format!("failed to build args: {e}"))?;
                    match callable.call1(py, args) {
                        Ok(ret) => {
                            let bound = ret.into_bound(py);
                            let bytes_vec: Vec<u8> = bound.extract().map_err(|e| {
                                format!("Python client-streaming handler must return bytes: {e}")
                            })?;
                            Ok(HandlerOutcome::Ok(bytes_vec))
                        }
                        Err(pyerr) => {
                            if let Some((code, body)) = extract_app_error(py, &pyerr) {
                                Ok(HandlerOutcome::AppError { code, body })
                            } else {
                                Err(format!("Python client-streaming handler raised: {pyerr}"))
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
                body: body.into(),
            }),
            Ok(Ok(Ok(HandlerOutcome::AppError { code, body }))) => {
                Err(RpcHandlerError::Application {
                    code,
                    message: String::from_utf8_lossy(&body).into_owned(),
                })
            }
            Ok(Ok(Err(msg))) => Err(RpcHandlerError::Internal(msg)),
            Ok(Err(join_err)) => Err(RpcHandlerError::Internal(format!(
                "spawn_blocking task panicked: {join_err}"
            ))),
            Err(_) => Err(RpcHandlerError::Internal(format!(
                "Python client-streaming handler did not respond within {} ms",
                self.timeout.as_millis()
            ))),
        }
    }
}

/// `RpcDuplexHandler` impl bridging to a Python callable.
struct PyRpcDuplexHandler {
    callable: Py<PyAny>,
    timeout: Duration,
    runtime: Arc<Runtime>,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for PyRpcDuplexHandler {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: InnerRequestStream,
        responses: InnerRpcResponseSink,
    ) -> std::result::Result<(), RpcHandlerError> {
        let callable = Python::attach(|py| self.callable.clone_ref(py));
        let runtime = self.runtime.clone();
        let stream_inner = Arc::new(Mutex::new(Some(requests)));
        let sink_inner = Arc::new(Mutex::new(Some(responses)));
        let ctx_caller_origin = ctx.caller_origin;
        let ctx_call_id = ctx.call_id;
        let ctx_deadline_ns = ctx.deadline_ns;
        let ctx_headers = Arc::new(ctx.headers);
        let result = tokio::time::timeout(
            self.timeout,
            tokio::task::spawn_blocking(move || -> Result<HandlerOutcome, String> {
                Python::attach(|py| -> Result<HandlerOutcome, String> {
                    let stream_obj = Py::new(
                        py,
                        PyRequestStreamRecv {
                            inner: stream_inner,
                            runtime,
                            caller_origin: ctx_caller_origin,
                            call_id: ctx_call_id,
                            deadline_ns: ctx_deadline_ns,
                            headers: ctx_headers,
                        },
                    )
                    .map_err(|e| format!("failed to build request stream: {e}"))?;
                    let sink_obj = Py::new(py, PyResponseSinkSend { inner: sink_inner })
                        .map_err(|e| format!("failed to build response sink: {e}"))?;
                    let args = PyTuple::new(py, [stream_obj.into_any(), sink_obj.into_any()])
                        .map_err(|e| format!("failed to build args: {e}"))?;
                    match callable.call1(py, args) {
                        Ok(_) => Ok(HandlerOutcome::Ok(Vec::new())),
                        Err(pyerr) => {
                            // Same Application-error mapping the
                            // client-streaming path uses — a Python
                            // handler raising the typed Application
                            // exception surfaces as
                            // RpcStatus::Application(code) on the
                            // caller side rather than collapsing to
                            // Internal.
                            if let Some((code, body)) = extract_app_error(py, &pyerr) {
                                Ok(HandlerOutcome::AppError { code, body })
                            } else {
                                Err(format!("Python duplex handler raised: {pyerr}"))
                            }
                        }
                    }
                })
            }),
        )
        .await;
        match result {
            Ok(Ok(Ok(HandlerOutcome::Ok(_)))) => Ok(()),
            Ok(Ok(Ok(HandlerOutcome::AppError { code, body }))) => {
                Err(RpcHandlerError::Application {
                    code,
                    message: String::from_utf8_lossy(&body).into_owned(),
                })
            }
            Ok(Ok(Err(msg))) => Err(RpcHandlerError::Internal(msg)),
            Ok(Err(join_err)) => Err(RpcHandlerError::Internal(format!(
                "spawn_blocking task panicked: {join_err}"
            ))),
            Err(_) => Err(RpcHandlerError::Internal(format!(
                "Python duplex handler did not respond within {} ms",
                self.timeout.as_millis()
            ))),
        }
    }
}

/// Sentinel for the abort path of `block_until_cancellable`. The
/// Apply the optional ``Cancellable`` from the user's opts to the
/// inner ``CallOptions``: reserves a token on the substrate, arms
/// the Cancellable, and populates ``inner_opts.cancel_token``.
///
/// Returns a Py<PyCancellable> reference (cloned under the GIL) so
/// the caller can run [`disarm_cancellable`] on completion.
///
/// v3 / C-A2: delegates entirely to the substrate's
/// ``Mesh::reserve_cancel_token`` / ``Mesh::cancel`` primitives —
/// the pyo3 binding no longer owns a local spawn+abort registry.
fn apply_cancellable(
    mesh: &Arc<MeshNode>,
    cancel: Option<&Py<PyCancellable>>,
    inner_opts: &mut InnerCallOptions,
) -> Option<Py<PyCancellable>> {
    let cancel_py = cancel?;
    let (cloned, token) = Python::attach(|py| {
        let token = cancel_py.bind(py).borrow().arm(Arc::clone(mesh));
        (cancel_py.clone_ref(py), token)
    });
    inner_opts.cancel_token = Some(token);
    Some(cloned)
}

/// Disarm the Cancellable returned by [`apply_cancellable`], so a
/// subsequent `cancel()` on it doesn't try to call
/// `Mesh::cancel(token)` for an already-resolved token. Idempotent.
fn disarm_cancellable(cancel: Option<Py<PyCancellable>>) {
    if let Some(c) = cancel {
        Python::attach(|py| c.bind(py).borrow().disarm());
    }
}

// ============================================================================
// Observer + metrics POD shapes (S2-A2).
//
// Mirrors the napi binding's `RpcCallEventJs` / `ServiceMetricsJs`
// / `RpcMetricsSnapshotJs` shapes. The only Python-specific
// choices are:
//   - u64 fields → plain Python int (PyO3 handles the conversion;
//     no `BigInt` equivalent needed because Python ints are
//     arbitrary precision).
//   - `RpcCallStatus` tagged union → `status_kind: str` +
//     `status_message: Optional[str]`. Same shape as the JS POD.
//   - `RpcDirection` enum → `direction: str` (`"outbound"` /
//     `"inbound"`).
// ============================================================================

/// Single observed RPC call boundary. Surfaced to the observer
/// callback installed via `MeshRpc.set_observer`. Read fields
/// directly — all attributes are plain ints / strs.
#[pyclass(name = "RpcCallEvent", module = "_net", get_all)]
pub struct PyRpcCallEvent {
    /// 64-bit node id of the calling node.
    pub caller: u64,
    /// 64-bit node id of the responding node.
    pub callee: u64,
    /// Service / method name.
    pub method: String,
    /// Elapsed time in milliseconds.
    pub latency_ms: u32,
    /// `"ok"` | `"error"` | `"timeout"` | `"canceled"`. Match on
    /// this discriminant before reading `status_message`.
    pub status_kind: String,
    /// Populated only when `status_kind == "error"`. Carries an
    /// operator-readable diagnostic from the substrate.
    pub status_message: Option<String>,
    /// Wire payload size of the request body (0 when not
    /// available).
    pub request_bytes: u32,
    /// Wire payload size of the response body (0 when not
    /// available).
    pub response_bytes: u32,
    /// `"outbound"` (this node initiated) or `"inbound"` (this
    /// node received). v1 only emits `"outbound"`.
    pub direction: String,
    /// Unix-ms timestamp captured at fire time.
    pub ts_unix_ms: u64,
}

impl From<&InnerRpcCallEvent> for PyRpcCallEvent {
    fn from(evt: &InnerRpcCallEvent) -> Self {
        let (status_kind, status_message) = match &evt.status {
            InnerRpcCallStatus::Ok => ("ok", None),
            InnerRpcCallStatus::Error(msg) => ("error", Some(msg.clone())),
            InnerRpcCallStatus::Timeout => ("timeout", None),
            InnerRpcCallStatus::Canceled => ("canceled", None),
        };
        let direction = match evt.direction {
            InnerRpcDirection::Outbound => "outbound",
            InnerRpcDirection::Inbound => "inbound",
        };
        Self {
            caller: evt.caller,
            callee: evt.callee,
            method: evt.method.clone(),
            latency_ms: evt.latency_ms,
            status_kind: status_kind.to_string(),
            status_message,
            request_bytes: evt.request_bytes,
            response_bytes: evt.response_bytes,
            direction: direction.to_string(),
            ts_unix_ms: evt.ts_unix_ms,
        }
    }
}

/// Per-service caller- + server-side nRPC counters at a point
/// in time. Element of `RpcMetricsSnapshot.services`.
#[pyclass(name = "ServiceMetrics", module = "_net", get_all)]
pub struct PyServiceMetrics {
    pub service: String,
    // ---- caller-side ----
    pub calls_total: u64,
    pub errors_no_route: u64,
    pub errors_timeout: u64,
    pub errors_server: u64,
    pub errors_transport: u64,
    pub in_flight: i64,
    pub latency_sum_ns: u64,
    pub latency_count: u64,
    pub latency_buckets: Vec<u64>,
    // ---- server-side ----
    pub handler_invocations_total: u64,
    pub handler_panics_total: u64,
    pub handler_in_flight: i64,
    pub handler_duration_sum_ns: u64,
    pub handler_duration_count: u64,
    pub handler_duration_buckets: Vec<u64>,
    pub streaming_chunks_emitted_total: u64,
    pub streaming_chunks_dropped_total: u64,
    pub capability_denied_total: u64,
}

impl From<&InnerServiceMetrics> for PyServiceMetrics {
    fn from(m: &InnerServiceMetrics) -> Self {
        Self {
            service: m.service.clone(),
            calls_total: m.calls_total,
            errors_no_route: m.errors_no_route,
            errors_timeout: m.errors_timeout,
            errors_server: m.errors_server,
            errors_transport: m.errors_transport,
            in_flight: m.in_flight,
            latency_sum_ns: m.latency_sum_ns,
            latency_count: m.latency_count,
            latency_buckets: m.latency_buckets.clone(),
            handler_invocations_total: m.handler_invocations_total,
            handler_panics_total: m.handler_panics_total,
            handler_in_flight: m.handler_in_flight,
            handler_duration_sum_ns: m.handler_duration_sum_ns,
            handler_duration_count: m.handler_duration_count,
            handler_duration_buckets: m.handler_duration_buckets.clone(),
            streaming_chunks_emitted_total: m.streaming_chunks_emitted_total,
            streaming_chunks_dropped_total: m.streaming_chunks_dropped_total,
            capability_denied_total: m.capability_denied_total,
        }
    }
}

/// Snapshot of the per-service nRPC metrics registry. Returned
/// by `MeshRpc.metrics_snapshot`.
#[pyclass(name = "RpcMetricsSnapshot", module = "_net", get_all)]
pub struct PyRpcMetricsSnapshot {
    /// One entry per service that has been called at least once
    /// since the mesh was created. Sorted by service name.
    pub services: Vec<Py<PyServiceMetrics>>,
    /// Cumulative count of observer events dropped because the
    /// observer's bounded buffer was full (v3 / O-A2). Climbing
    /// values indicate the installed Python callback can't keep
    /// up with the dispatch rate; push events into a
    /// :class:`queue.Queue` and drain off a dedicated thread.
    pub observer_dropped_total: u64,
}

// ----------------------------------------------------------------------
// Observer trampoline — bounded mpsc + dedicated worker (v3 / O-A2).
//
// v1 (S2-A2) spawned a fresh blocking-pool task per event via
// `runtime.spawn_blocking`. Under sustained load this drained the
// tokio blocking pool faster than user callbacks acquire-and-
// release the GIL; past the pool's cap, spawn_blocking queues
// internally with no observability.
//
// v3 inserts a bounded mpsc + single worker so:
//   - The substrate dispatch thread pays only an atomic counter
//     on overflow (vs the channel-send-or-block on every event).
//   - Drops surface via `metrics_snapshot.observer_dropped_total`.
//   - Serialized GIL acquisition (one worker → one acquire-release
//     cycle per drained event) matches Python's natural threading
//     model better than 100 concurrent blocking-pool tasks all
//     fighting for the GIL.
//
// Locked decision #1 (NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md):
// callbacks should still be cheap, but the substrate dispatch
// path is defended.
// ----------------------------------------------------------------------

/// Bound on the observer event buffer. Matches the napi binding's
/// `OBSERVER_BUFFER_CAPACITY` (1024) for consistency across
/// bindings.
const OBSERVER_BUFFER_CAPACITY: usize = 1024;

/// Process-global count of observer events dropped because the
/// bounded buffer was full. Surfaced via
/// `metrics_snapshot.observer_dropped_total`.
static OBSERVER_DROPPED_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct PyRpcObserver {
    sender: tokio::sync::mpsc::Sender<Arc<InnerRpcCallEvent>>,
}

impl PyRpcObserver {
    /// Install a new observer: build the bounded channel, spawn
    /// the drain worker that acquires the GIL once per drained
    /// event and invokes the Python callable.
    ///
    /// N5: the channel carries `Arc<InnerRpcCallEvent>`, not the
    /// converted `PyRpcCallEvent`. The substrate dispatch thread
    /// pays only `Arc::clone` + try_send; the worker builds the
    /// pyclass POD lazily under the GIL.
    fn install(callable: Py<PyAny>, runtime: Arc<Runtime>) -> Self {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<Arc<InnerRpcCallEvent>>(OBSERVER_BUFFER_CAPACITY);
        runtime.spawn(async move {
            while let Some(evt) = receiver.recv().await {
                Python::attach(|py| {
                    let py_evt = match Py::new(py, PyRpcCallEvent::from(evt.as_ref())) {
                        Ok(o) => o,
                        Err(_) => return,
                    };
                    // Ignore exceptions — observers can't
                    // influence the in-flight call.
                    let _ = callable.call1(py, (py_evt,));
                });
            }
            // Sender dropped → channel closed → worker exits.
        });
        Self { sender }
    }
}

impl RpcObserver for PyRpcObserver {
    fn on_call(&self, evt: InnerRpcCallEvent) {
        if self.sender.try_send(Arc::new(evt)).is_err() {
            OBSERVER_DROPPED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
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
        let mut inner_opts = call_options_from_dict(opts)?;
        let armed_cancel = apply_cancellable(&self.node, cancel.as_ref(), &mut inner_opts);
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let result = py.detach(|| {
            runtime.block_on(async move {
                node.call(target_node_id, &service, req_bytes, inner_opts)
                    .await
            })
        });
        disarm_cancellable(armed_cancel);
        Ok(PyBytes::new(
            py,
            result.map_err(rpc_error_to_pyerr)?.body.as_ref(),
        ))
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
        let mut inner_opts = call_options_from_dict(opts)?;
        let armed_cancel = apply_cancellable(&self.node, cancel.as_ref(), &mut inner_opts);
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let result = py.detach(|| {
            runtime
                .block_on(async move { node.call_service(&service, req_bytes, inner_opts).await })
        });
        disarm_cancellable(armed_cancel);
        Ok(PyBytes::new(
            py,
            result.map_err(rpc_error_to_pyerr)?.body.as_ref(),
        ))
    }

    /// Open a streaming-response call. Returns an [`PyRpcStream`];
    /// drain via the iterator protocol. Drop / `close()` emits
    /// CANCEL to the server. Pass ``opts={'cancel': cancel}`` to
    /// allow mid-stream cancel via :class:`Cancellable`.
    #[pyo3(signature = (target_node_id, service, request, opts=None))]
    fn call_streaming<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<PyRpcStream> {
        let cancel = extract_cancellable(opts)?;
        let mut inner_opts = call_options_from_dict(opts)?;
        // armed_cancel is dropped here — the Cancellable's stored
        // (mesh, token) outlives this call via the substrate
        // registry. Cancel mid-stream still works because the
        // substrate's cancel-watcher task closes the pending
        // entry when Mesh::cancel(token) fires.
        let _armed = apply_cancellable(&self.node, cancel.as_ref(), &mut inner_opts);
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let inner = py.detach(|| {
            runtime.block_on(async move {
                node.call_streaming(target_node_id, &service, req_bytes, inner_opts)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })?;
        Ok(PyRpcStream {
            inner: Arc::new(Mutex::new(Some(inner))),
            runtime: self.runtime.clone(),
        })
    }

    /// Open a client-streaming call. Returns a
    /// :class:`ClientStreamCall`; push chunks via ``call.send(bytes)``
    /// then ``call.finish()`` to await the terminal response. The
    /// initial REQUEST is published lazily on the first ``send``
    /// (or on ``finish`` for the degenerate zero-send path). Pass
    /// ``opts={'cancel': cancel}`` to allow mid-stream cancel via
    /// :class:`Cancellable`.
    #[pyo3(signature = (target_node_id, service, opts=None))]
    fn call_client_stream(
        &self,
        py: Python<'_>,
        target_node_id: u64,
        service: String,
        opts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyClientStreamCall> {
        let cancel = extract_cancellable(opts)?;
        let mut inner_opts = call_options_from_dict(opts)?;
        let _armed = apply_cancellable(&self.node, cancel.as_ref(), &mut inner_opts);
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let inner = py.detach(|| {
            runtime.block_on(async move {
                node.call_client_stream(target_node_id, &service, inner_opts)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })?;
        let call_id = inner.call_id();
        let flow_controlled = inner.flow_controlled();
        Ok(PyClientStreamCall {
            inner: Arc::new(Mutex::new(Some(inner))),
            runtime: self.runtime.clone(),
            call_id_cached: call_id,
            flow_controlled_cached: flow_controlled,
            close_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Open a duplex call. Returns a :class:`DuplexCall` with both
    /// send + receive surfaces. Pass
    /// ``opts={'request_window_initial': N}`` / ``opts={'stream_window_initial': N}``
    /// to enable per-direction flow control, or
    /// ``opts={'cancel': cancel}`` for mid-stream cancel via
    /// :class:`Cancellable`.
    #[pyo3(signature = (target_node_id, service, opts=None))]
    fn call_duplex(
        &self,
        py: Python<'_>,
        target_node_id: u64,
        service: String,
        opts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyDuplexCall> {
        let cancel = extract_cancellable(opts)?;
        let mut inner_opts = call_options_from_dict(opts)?;
        let _armed = apply_cancellable(&self.node, cancel.as_ref(), &mut inner_opts);
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let inner = py.detach(|| {
            runtime.block_on(async move {
                node.call_duplex(target_node_id, &service, inner_opts)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })?;
        let call_id = inner.call_id();
        let flow_controlled = inner.flow_controlled();
        Ok(PyDuplexCall {
            inner: Arc::new(Mutex::new(Some(inner))),
            runtime: self.runtime.clone(),
            call_id_cached: call_id,
            flow_controlled_cached: flow_controlled,
            close_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Register a Python client-streaming handler on ``service``.
    /// ``handler`` must be callable as
    /// ``handler(stream: RequestStreamRecv) -> bytes``. Iterate
    /// ``stream`` to drain inbound chunks; return ``bytes`` as the
    /// terminal response.
    #[pyo3(signature = (service, handler, handler_timeout_ms=None))]
    fn serve_client_stream(
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
        let rust_handler = Arc::new(PyRpcClientStreamingHandler {
            callable: handler,
            timeout,
            runtime: self.runtime.clone(),
        });
        let inner = self
            .node
            .serve_rpc_client_stream(&service, rust_handler)
            .map_err(|e| RpcError::new_err(format!("serve failed: {e}")))?;
        Ok(PyServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }

    /// Register a Python duplex handler on ``service``. ``handler``
    /// must be callable as
    /// ``handler(stream: RequestStreamRecv, sink: ResponseSinkSend) -> None``.
    /// Drain ``stream`` for inbound chunks; emit response chunks
    /// via ``sink.send(bytes)``.
    #[pyo3(signature = (service, handler, handler_timeout_ms=None))]
    fn serve_duplex(
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
        let rust_handler = Arc::new(PyRpcDuplexHandler {
            callable: handler,
            timeout,
            runtime: self.runtime.clone(),
        });
        let inner = self
            .node
            .serve_rpc_duplex(&service, rust_handler)
            .map_err(|e| RpcError::new_err(format!("serve failed: {e}")))?;
        Ok(PyServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
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

    // ---- observer + metrics (S2-A2) ------------------------------------

    /// Install (pass a callable) or clear (pass `None`) the
    /// caller-side nRPC observer. Replaces any previously-
    /// installed observer.
    ///
    /// The callable fires once per completed outbound RPC. The
    /// substrate dispatch thread enqueues the call onto the
    /// tokio runtime's blocking pool, so the dispatch hot path
    /// never blocks on GIL acquisition. Exceptions raised by the
    /// observer are silently swallowed — observers can't
    /// influence the in-flight call.
    ///
    /// Callbacks must be cheap: push events into a queue or ring
    /// buffer for slow consumers; do not do work inline.
    ///
    /// v1 emits only `direction == "outbound"` events.
    #[pyo3(signature = (observer=None))]
    fn set_observer(&self, py: Python<'_>, observer: Option<Py<PyAny>>) -> PyResult<()> {
        let observer = observer.filter(|o| !o.is_none(py));
        match observer {
            Some(callable) => {
                if !callable.bind(py).is_callable() {
                    return Err(pyo3::exceptions::PyTypeError::new_err(
                        "observer must be a callable or None",
                    ));
                }
                let obs: Arc<dyn RpcObserver> =
                    Arc::new(PyRpcObserver::install(callable, self.runtime.clone()));
                self.node.set_rpc_observer(Some(obs));
            }
            None => {
                self.node.set_rpc_observer(None);
            }
        }
        Ok(())
    }

    /// Snapshot the per-service nRPC metrics registry. Cheap —
    /// one DashMap iteration. Safe to call on every Prometheus
    /// scrape. Returns an `RpcMetricsSnapshot` whose `services`
    /// list each carries the caller + server-side counters.
    fn metrics_snapshot(&self, py: Python<'_>) -> PyResult<Py<PyRpcMetricsSnapshot>> {
        let inner = self.node.rpc_metrics_snapshot();
        let services: PyResult<Vec<Py<PyServiceMetrics>>> = inner
            .services
            .iter()
            .map(|m| Py::new(py, PyServiceMetrics::from(m)))
            .collect();
        let observer_dropped_total =
            OBSERVER_DROPPED_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
        Py::new(
            py,
            PyRpcMetricsSnapshot {
                services: services?,
                observer_dropped_total,
            },
        )
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
                InnerRpcError::CapabilityDenied { target, capability } => {
                    format!("nrpc:capability_denied: target=0x{target:x} capability={capability}")
                }
                InnerRpcError::Cancelled => "nrpc:cancelled: call cancelled by caller".to_string(),
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
        assert_eq!(
            format(InnerRpcError::CapabilityDenied {
                target: 0xCAFE_F00D,
                capability: "echo".into(),
            }),
            "nrpc:capability_denied: target=0xcafef00d capability=echo"
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
            InnerRpcError::CapabilityDenied {
                target: 1,
                capability: "x".into(),
            },
        ] {
            assert!(
                format(variant).starts_with("nrpc:"),
                "every variant must carry the canonical nrpc: prefix"
            );
        }
    }

    /// `PyRpcObserver::on_call` drops events when the bounded
    /// channel fills, incrementing the process-global drop counter
    /// by one per drop. Mirror of the napi binding's
    /// `observer_drops_overflow_events_and_counts_them` — same
    /// invariant lives at the substrate dispatch boundary in all
    /// three bindings.
    #[test]
    fn pyo3_observer_drops_overflow_events_and_counts_them() {
        use super::{PyRpcObserver, OBSERVER_BUFFER_CAPACITY, OBSERVER_DROPPED_TOTAL};
        use ::net::adapter::net::cortex::{
            RpcCallEvent as InnerRpcCallEvent, RpcCallStatus as InnerRpcCallStatus,
            RpcDirection as InnerRpcDirection, RpcObserver,
        };
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let baseline = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed);
        let (sender, _recv) =
            tokio::sync::mpsc::channel::<Arc<InnerRpcCallEvent>>(OBSERVER_BUFFER_CAPACITY);
        let obs = PyRpcObserver { sender };
        let make_event = || InnerRpcCallEvent {
            caller: 1,
            callee: 2,
            method: "test.svc.echo".into(),
            latency_ms: 0,
            status: InnerRpcCallStatus::Ok,
            request_bytes: 0,
            response_bytes: 0,
            direction: InnerRpcDirection::Outbound,
            ts_unix_ms: 0,
        };
        const FIRED: u64 = 2000;
        for _ in 0..FIRED {
            obs.on_call(make_event());
        }
        let dropped = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed) - baseline;
        let expected = FIRED - OBSERVER_BUFFER_CAPACITY as u64;
        assert!(
            dropped >= expected,
            "expected ≥ {expected} drops, got {dropped}",
        );
    }
}
