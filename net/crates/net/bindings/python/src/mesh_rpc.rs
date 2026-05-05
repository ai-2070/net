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

// ============================================================================
// Helpers — convert inner RpcError to the matching Python exception.
// ============================================================================

fn rpc_error_to_pyerr(err: InnerRpcError) -> PyErr {
    match err {
        InnerRpcError::NoRoute { target, reason } => {
            RpcNoRouteError::new_err(format!("no route to target 0x{target:x}: {reason}"))
        }
        InnerRpcError::Timeout { elapsed_ms } => {
            RpcTimeoutError::new_err(format!("rpc timeout after {elapsed_ms}ms"))
        }
        InnerRpcError::ServerError { status, message } => {
            RpcServerError::new_err(format!("server returned status 0x{status:04x}: {message}"))
        }
        InnerRpcError::Transport(e) => RpcTransportError::new_err(format!("transport: {e}")),
        InnerRpcError::Codec { direction, message } => {
            let dir = match direction {
                ::net::adapter::net::mesh_rpc::CodecDirection::Encode => "encode",
                ::net::adapter::net::mesh_rpc::CodecDirection::Decode => "decode",
            };
            RpcCodecError::new_err(format!("codec {dir}: {message}"))
        }
    }
}

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
    if let Some(v) = d.get_item("stream_window_initial")? {
        let n: u32 = v.extract().map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!(
                "stream_window_initial must be int: {e}"
            ))
        })?;
        inner.stream_window_initial = Some(n);
    }
    Ok(inner)
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
            tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                Python::attach(|py| -> Result<Vec<u8>, String> {
                    let req_bytes = PyBytes::new(py, &req_body);
                    let args = PyTuple::new(py, [req_bytes.into_any()])
                        .map_err(|e| format!("failed to build args: {e}"))?;
                    let ret = callable
                        .call1(py, args)
                        .map_err(|e| format!("Python handler raised: {e}"))?;
                    let bound = ret.into_bound(py);
                    let bytes_vec: Vec<u8> = bound
                        .extract()
                        .map_err(|e| format!("Python handler must return bytes: {e}"))?;
                    Ok(bytes_vec)
                })
            }),
        )
        .await;

        let body = match result {
            Ok(Ok(Ok(body))) => body,
            Ok(Ok(Err(msg))) => return Err(RpcHandlerError::Internal(msg)),
            Ok(Err(join_err)) => {
                return Err(RpcHandlerError::Internal(format!(
                    "spawn_blocking task panicked: {join_err}"
                )))
            }
            Err(_) => {
                return Err(RpcHandlerError::Internal(format!(
                    "Python handler did not respond within {} ms",
                    self.timeout.as_millis()
                )))
            }
        };

        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
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
    fn serve(&self, service: String, handler: Py<PyAny>) -> PyResult<PyServeHandle> {
        let rust_handler = Arc::new(PyRpcHandler {
            callable: handler,
            timeout: DEFAULT_HANDLER_TIMEOUT,
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
    #[pyo3(signature = (target_node_id, service, request, opts=None))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let opts = call_options_from_dict(opts)?;
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let reply = py.detach(|| {
            runtime.block_on(async move {
                node.call(target_node_id, &service, req_bytes, opts)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })?;
        Ok(PyBytes::new(py, reply.body.as_ref()))
    }

    /// Service-discovery unary call. Resolves `service` against
    /// the local capability index (`nrpc:<service>` tags),
    /// applies the routing policy, calls.
    #[pyo3(signature = (service, request, opts=None))]
    fn call_service<'py>(
        &self,
        py: Python<'py>,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let opts = call_options_from_dict(opts)?;
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let runtime = self.runtime.clone();
        let node = self.node.clone();
        let reply = py.detach(|| {
            runtime.block_on(async move {
                node.call_service(&service, req_bytes, opts)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })?;
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
    fn find_service_nodes(&self, service: String) -> Vec<u64> {
        self.node.find_service_nodes(&service)
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
    #[test]
    fn rpc_error_message_formats_are_stable() {
        // Re-derive the per-variant format strings the same way
        // `rpc_error_to_pyerr` does. Pinned because user code's
        // `str(rpc_err)` reads these directly.
        let format = |err: InnerRpcError| -> String {
            match err {
                InnerRpcError::NoRoute { target, reason } => {
                    format!("no route to target 0x{target:x}: {reason}")
                }
                InnerRpcError::Timeout { elapsed_ms } => {
                    format!("rpc timeout after {elapsed_ms}ms")
                }
                InnerRpcError::ServerError { status, message } => {
                    format!("server returned status 0x{status:04x}: {message}")
                }
                InnerRpcError::Transport(e) => format!("transport: {e}"),
                InnerRpcError::Codec { direction, message } => {
                    let dir = match direction {
                        CodecDirection::Encode => "encode",
                        CodecDirection::Decode => "decode",
                    };
                    format!("codec {dir}: {message}")
                }
            }
        };

        assert_eq!(
            format(InnerRpcError::NoRoute {
                target: 0xDEAD_BEEF,
                reason: "no session".into(),
            }),
            "no route to target 0xdeadbeef: no session"
        );
        assert_eq!(
            format(InnerRpcError::Timeout { elapsed_ms: 200 }),
            "rpc timeout after 200ms"
        );
        assert_eq!(
            format(InnerRpcError::ServerError {
                status: 0x8001,
                message: "app error".into(),
            }),
            "server returned status 0x8001: app error"
        );
        assert_eq!(
            format(InnerRpcError::Transport(AdapterError::Connection(
                "boom".into()
            ))),
            "transport: connection error: boom"
        );
        assert_eq!(
            format(InnerRpcError::Codec {
                direction: CodecDirection::Encode,
                message: "bad json".into(),
            }),
            "codec encode: bad json"
        );
        assert_eq!(
            format(InnerRpcError::Codec {
                direction: CodecDirection::Decode,
                message: "trailing".into(),
            }),
            "codec decode: trailing"
        );
    }
}
