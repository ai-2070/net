//! C ABI nRPC surface for the Go binding of Net — Phase B5
//! (lifecycle + raw `call` / `call_service` / `serve` /
//! `find_service_nodes`). Streaming + resilience helpers land in
//! Phase B6.
//!
//! # Handle model (mirrors `compute-ffi`)
//!
//! Every Rust object that crosses the FFI boundary is wrapped in
//! a heap-allocated `Box` and handed to the caller as `*mut T`.
//! Go owns the pointer (`runtime.SetFinalizer` pattern) and MUST
//! call the matching `_free` function exactly once.
//!
//! # Error codes
//!
//! `c_int` return values:
//!   - `0` (`NET_RPC_OK`) — success
//!   - negative — error (specific code per category)
//!
//! Structured detail (from `RpcError`) is surfaced via the out-
//! param `*mut *mut c_char` on the same call. Caller frees with
//! [`net_rpc_free_cstring`].
//!
//! # Tokio runtime
//!
//! This crate owns a lazy `OnceLock<Arc<Runtime>>` for blocking
//! into async SDK calls. The mesh's internal operations run on
//! their own runtime; this one just bridges the FFI boundary.
//!
//! # Handler bridging
//!
//! Go calls [`net_rpc_set_handler_dispatcher`] once at init,
//! supplying a function pointer the Rust side invokes when a
//! request lands. The dispatcher signature passes a
//! `handler_id: u64` (the Go side's lookup key for its handler
//! registry) plus `(req_ptr, req_len)`. The Go side returns
//! `(out_resp_ptr, out_resp_len)` heap-allocated via `C.malloc`;
//! the Rust side copies into a `Bytes` and frees the Go buffer
//! via `libc::free`.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::runtime::Runtime;

use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{
    CallOptions as InnerCallOptions, RoutingPolicy as InnerRoutingPolicy,
    RpcError as InnerRpcError, ServeHandle as InnerServeHandle,
};
use net::adapter::net::MeshNode;

// =========================================================================
// Error codes
// =========================================================================

/// Operation succeeded.
pub const NET_RPC_OK: c_int = 0;
/// Null or invalid pointer passed where a live handle was
/// expected.
pub const NET_RPC_ERR_NULL: c_int = -1;
/// Generic catch-all; structured detail is in the `out_err`
/// out-param on the same call.
pub const NET_RPC_ERR_CALL_FAILED: c_int = -2;
/// `serve` rejected: a handler is already registered for this
/// service on this MeshRpc.
pub const NET_RPC_ERR_ALREADY_SERVING: c_int = -3;
/// `set_handler_dispatcher` was never called — the binding can't
/// route incoming requests because there's no Go-side dispatcher
/// to invoke.
pub const NET_RPC_ERR_NO_DISPATCHER: c_int = -4;
/// Caller passed a UTF-8-invalid byte sequence where a string
/// was expected (e.g. service name).
pub const NET_RPC_ERR_INVALID_UTF8: c_int = -5;

// =========================================================================
// Runtime + counters.
// =========================================================================

/// Lazy multi-thread tokio runtime for the FFI's `block_on` calls.
fn runtime() -> &'static Arc<Runtime> {
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("net-rpc-ffi")
                .build()
                .expect("failed to construct rpc-ffi tokio runtime"),
        )
    })
}

/// Monotonic counter for `MeshRpcHandle::rpc_id`. Starts at 1 so
/// `0` is reserved as "no rpc" sentinel.
static NEXT_RPC_ID: AtomicU64 = AtomicU64::new(1);

/// Monotonic counter for handler registrations. Each `serve`
/// allocates a fresh `handler_id` that the Go side uses to look
/// up its callable in the Go-process-global handler registry.
static NEXT_HANDLER_ID: AtomicU64 = AtomicU64::new(1);

// =========================================================================
// Helpers.
// =========================================================================

/// Convert a `(ptr, len)` C buffer to a Rust `String`. Returns
/// `None` on null pointer or non-UTF-8 bytes.
fn cstr_to_string(ptr: *const c_char, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

/// Set `*out_err` to a heap-allocated CString containing
/// `message`. Caller frees via [`net_rpc_free_cstring`].
fn write_err(out_err: *mut *mut c_char, message: String) {
    if out_err.is_null() {
        return;
    }
    let cstr = match CString::new(message) {
        Ok(s) => s,
        Err(_) => CString::new("error message contained interior NUL").unwrap(),
    };
    unsafe {
        *out_err = cstr.into_raw();
    }
}

/// Format an inner [`InnerRpcError`] into the same stable string
/// shape the Node + Python bindings use: a colon-delimited
/// `<kind>: <detail>` so the Go side can parse the kind segment
/// for typed-error dispatch. Examples:
///
///   no_route: target=0xABCD reason=...
///   timeout: elapsed_ms=200
///   server_error: status=0x4001 message=...
///   transport: ...
///   codec_encode: ...
///   codec_decode: ...
fn format_rpc_error(err: &InnerRpcError) -> String {
    use net::adapter::net::mesh_rpc::CodecDirection;
    match err {
        InnerRpcError::NoRoute { target, reason } => {
            format!("no_route: target=0x{target:x} reason={reason}")
        }
        InnerRpcError::Timeout { elapsed_ms } => {
            format!("timeout: elapsed_ms={elapsed_ms}")
        }
        InnerRpcError::ServerError { status, message } => {
            format!("server_error: status=0x{status:04x} message={message}")
        }
        InnerRpcError::Transport(e) => format!("transport: {e}"),
        InnerRpcError::Codec { direction, message } => {
            let dir = match direction {
                CodecDirection::Encode => "codec_encode",
                CodecDirection::Decode => "codec_decode",
            };
            format!("{dir}: {message}")
        }
    }
}

// =========================================================================
// Handler dispatcher — Go registers once at init.
// =========================================================================

/// C-ABI signature: invoke Go's RPC handler for `handler_id`.
/// On success, sets `(*out_resp_ptr, *out_resp_len)` to a heap-
/// allocated buffer (allocated by the Go side via `C.malloc`)
/// and returns `0`. On failure, sets `*out_err` to a heap-
/// allocated UTF-8 message and returns non-zero. Rust copies the
/// response bytes into a `Bytes`, then releases the Go-allocated
/// buffer via `libc::free`.
pub type RpcHandlerFn = unsafe extern "C" fn(
    handler_id: u64,
    req_ptr: *const u8,
    req_len: usize,
    out_resp_ptr: *mut *mut u8,
    out_resp_len: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int;

/// Process-global Go-side handler dispatcher. Set once via
/// [`net_rpc_set_handler_dispatcher`]; subsequent calls are
/// silently ignored (first registration wins — `OnceLock`).
static DISPATCHER: OnceLock<RpcHandlerFn> = OnceLock::new();

/// Register the process-wide handler dispatcher. Idempotent —
/// only the first call takes effect; later calls return without
/// changing the dispatcher.
///
/// The Go binding calls this once during package init.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_set_handler_dispatcher(dispatcher: RpcHandlerFn) {
    let _ = DISPATCHER.set(dispatcher);
}

/// `RpcHandler` impl that bridges to the Go-side dispatcher.
struct GoRpcHandler {
    handler_id: u64,
    /// Cap on per-handler wait time. Without it, a wedged Go
    /// callback would hold the in-flight slot indefinitely.
    timeout: Duration,
}

#[async_trait::async_trait]
impl RpcHandler for GoRpcHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let dispatcher = match DISPATCHER.get() {
            Some(d) => *d,
            None => {
                return Err(RpcHandlerError::Internal(
                    "net_rpc_set_handler_dispatcher never called".into(),
                ));
            }
        };
        let handler_id = self.handler_id;
        let timeout = self.timeout;
        let req_body = ctx.payload.body;

        // Spawn the Go callback on a blocking thread so the cgo
        // call doesn't park an async-runtime worker.
        let join = tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                let mut resp_ptr: *mut u8 = std::ptr::null_mut();
                let mut resp_len: usize = 0;
                let mut err_ptr: *mut c_char = std::ptr::null_mut();
                let code = unsafe {
                    dispatcher(
                        handler_id,
                        req_body.as_ptr(),
                        req_body.len(),
                        &mut resp_ptr,
                        &mut resp_len,
                        &mut err_ptr,
                    )
                };
                if code == NET_RPC_OK {
                    if resp_ptr.is_null() {
                        return Ok(Vec::new());
                    }
                    // Copy the Go-allocated response bytes into a
                    // Rust-owned Vec so the lifetime is decoupled
                    // from the Go-malloc'd buffer.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(resp_ptr, resp_len).to_vec()
                    };
                    // Release the Go-allocated buffer.
                    unsafe { libc::free(resp_ptr as *mut libc::c_void) };
                    Ok(bytes)
                } else {
                    // Go reported an error; pull the message out
                    // and free its CString.
                    let msg = if err_ptr.is_null() {
                        format!("Go handler returned code {code} with no error message")
                    } else {
                        let s = unsafe { std::ffi::CStr::from_ptr(err_ptr) }
                            .to_string_lossy()
                            .into_owned();
                        unsafe { libc::free(err_ptr as *mut libc::c_void) };
                        s
                    };
                    Err(msg)
                }
            }),
        )
        .await;

        let body = match join {
            Ok(Ok(Ok(body))) => body,
            Ok(Ok(Err(msg))) => return Err(RpcHandlerError::Internal(msg)),
            Ok(Err(join_err)) => {
                return Err(RpcHandlerError::Internal(format!(
                    "Go-handler blocking task panicked: {join_err}"
                )));
            }
            Err(_) => {
                return Err(RpcHandlerError::Internal(format!(
                    "Go handler did not respond within {} ms",
                    timeout.as_millis()
                )));
            }
        };

        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}

const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(60);

// =========================================================================
// Free helpers.
// =========================================================================

/// Free a CString previously returned out-of-band by this crate
/// (e.g. structured error detail). Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_free_cstring(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(s);
    }
}

/// Free a Vec<u8> previously returned out-of-band by this crate
/// (e.g. response bytes from `net_rpc_call`). Idempotent on NULL
/// or zero-length.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_response_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        // Reconstruct the Vec from its raw parts. `cap == len` is
        // load-bearing — every site that hands a response buffer
        // out shrinks-to-fit before extracting raw parts so the
        // capacity matches the length.
        let _ = Vec::from_raw_parts(ptr, len, len);
    }
}

/// Free an array of u64 node ids previously returned by
/// [`net_rpc_find_service_nodes`]. Idempotent on NULL or zero.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_find_service_nodes_free(ptr: *mut u64, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        let _ = Vec::from_raw_parts(ptr, len, len);
    }
}

// =========================================================================
// MeshRpcHandle — opaque wrapper around an Arc<MeshNode>.
// =========================================================================

/// Opaque handle exposed to Go. Carries an `Arc<MeshNode>` and
/// the per-MeshRpc `rpc_id` (used by the SDK's internal
/// bookkeeping; surfaced for diagnostics).
pub struct MeshRpcHandle {
    node: Arc<MeshNode>,
    rpc_id: u64,
}

/// Build a new MeshRpc from an `Arc<MeshNode>` shared via
/// `net_mesh_arc_clone` (defined in `net::ffi::mesh`).
///
/// **Ownership semantics:**
/// - `node_arc` is CONSUMED by this call — the MeshRpc takes
///   the `Arc` content via `Box::from_raw` and stores it.
///   Caller MUST NOT free `node_arc` after a successful call.
/// - On failure (NULL input), the pointer is left intact.
///
/// Returns NULL on NULL input.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_new(node_arc: *mut Arc<MeshNode>) -> *mut MeshRpcHandle {
    if node_arc.is_null() {
        return std::ptr::null_mut();
    }
    let node = unsafe { *Box::from_raw(node_arc) };
    let rpc_id = NEXT_RPC_ID.fetch_add(1, Ordering::Relaxed);
    Box::into_raw(Box::new(MeshRpcHandle { node, rpc_id }))
}

/// Free a MeshRpc handle. The underlying MeshNode stays alive so
/// long as another `Arc` to it is held. Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_free(handle: *mut MeshRpcHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Diagnostic accessor: monotonic id of this MeshRpc.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_id(handle: *const MeshRpcHandle) -> u64 {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    h.rpc_id
}

// =========================================================================
// CallOptions — built from primitive args (no FFI struct).
// =========================================================================

/// Build inner CallOptions from primitive args. `deadline_ms == 0`
/// means "no deadline."
fn build_call_options(deadline_ms: u64) -> InnerCallOptions {
    let mut inner = InnerCallOptions::default();
    if deadline_ms > 0 {
        inner.deadline = Some(Instant::now() + Duration::from_millis(deadline_ms));
    }
    inner.routing_policy = InnerRoutingPolicy::default();
    inner
}

// =========================================================================
// Calls.
// =========================================================================

/// Direct-addressed unary call. Blocks the calling goroutine via
/// `runtime.block_on`; the Go side wraps in a goroutine for
/// concurrency.
///
/// `service_ptr / service_len` is a UTF-8 byte slice (no trailing
/// NUL required). `req_ptr / req_len` is the raw request body.
/// `deadline_ms == 0` means no deadline.
///
/// On success: writes `(out_resp_ptr, out_resp_len)` for the
/// response bytes (caller frees via `net_rpc_response_free`),
/// returns `NET_RPC_OK`.
///
/// On failure: writes a structured error message to `out_err`
/// (caller frees via `net_rpc_free_cstring`), returns the
/// matching error code.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_call(
    handle: *mut MeshRpcHandle,
    target_node_id: u64,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    deadline_ms: u64,
    out_resp_ptr: *mut *mut u8,
    out_resp_len: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_RPC_ERR_NULL;
    };
    let Some(service) = cstr_to_string(service_ptr, service_len) else {
        write_err(out_err, "service name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let req_bytes = if req_ptr.is_null() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })
    };
    let opts = build_call_options(deadline_ms);
    let node = h.node.clone();

    let result = runtime().block_on(async move {
        node.call(target_node_id, &service, req_bytes, opts).await
    });

    match result {
        Ok(reply) => {
            write_response(reply.body.to_vec(), out_resp_ptr, out_resp_len);
            NET_RPC_OK
        }
        Err(e) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// Service-discovery unary call. Same semantics as
/// [`net_rpc_call`] but resolves `service` against the local
/// capability index instead of taking an explicit target.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_call_service(
    handle: *mut MeshRpcHandle,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    deadline_ms: u64,
    out_resp_ptr: *mut *mut u8,
    out_resp_len: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_RPC_ERR_NULL;
    };
    let Some(service) = cstr_to_string(service_ptr, service_len) else {
        write_err(out_err, "service name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let req_bytes = if req_ptr.is_null() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })
    };
    let opts = build_call_options(deadline_ms);
    let node = h.node.clone();

    let result =
        runtime().block_on(async move { node.call_service(&service, req_bytes, opts).await });

    match result {
        Ok(reply) => {
            write_response(reply.body.to_vec(), out_resp_ptr, out_resp_len);
            NET_RPC_OK
        }
        Err(e) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// Hand a `Vec<u8>` to the Go caller as a raw pointer + length.
/// Shrinks to fit so `cap == len` — the matching
/// `net_rpc_response_free` reconstructs `Vec<u8>` from
/// `(ptr, len, len)`.
fn write_response(mut body: Vec<u8>, out_ptr: *mut *mut u8, out_len: *mut usize) {
    body.shrink_to_fit();
    let len = body.len();
    let ptr = body.as_mut_ptr();
    std::mem::forget(body);
    unsafe {
        *out_ptr = ptr;
        *out_len = len;
    }
}

// =========================================================================
// Service discovery.
// =========================================================================

/// All node ids advertising `nrpc:<service>` in the local
/// capability index. On success, writes `(out_ptr, out_count)`
/// for a heap-allocated `u64` array; caller frees via
/// [`net_rpc_find_service_nodes_free`]. Empty result → writes
/// `(NULL, 0)`. Returns `NET_RPC_OK` even when empty.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_find_service_nodes(
    handle: *mut MeshRpcHandle,
    service_ptr: *const c_char,
    service_len: usize,
    out_ptr: *mut *mut u64,
    out_count: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_RPC_ERR_NULL;
    };
    let Some(service) = cstr_to_string(service_ptr, service_len) else {
        write_err(out_err, "service name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let nodes = h.node.find_service_nodes(&service);
    if nodes.is_empty() {
        unsafe {
            *out_ptr = std::ptr::null_mut();
            *out_count = 0;
        }
        return NET_RPC_OK;
    }
    let mut buf = nodes;
    buf.shrink_to_fit();
    let count = buf.len();
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    unsafe {
        *out_ptr = ptr;
        *out_count = count;
    }
    NET_RPC_OK
}

// =========================================================================
// ServeHandle — register a handler.
// =========================================================================

/// Opaque ServeHandle exposed to Go. Wraps the SDK ServeHandle in
/// an `Arc<Mutex<Option<...>>>` so `close()` can drop deterministically
/// AND a subsequent `_free` (after the GC finalizer fires) is a
/// no-op when already closed.
pub struct ServeHandleC {
    inner: Arc<Mutex<Option<InnerServeHandle>>>,
    /// The handler_id allocated by `net_rpc_serve` — Go side
    /// uses this to look up its callable in its registry. Exposed
    /// via `net_rpc_serve_handle_id` so Go knows which entry to
    /// release on close.
    handler_id: u64,
}

/// Register a handler for `service`. Allocates a fresh
/// `handler_id` and returns a ServeHandle. The Go side adds an
/// entry to its callback registry keyed on the returned id
/// BEFORE invoking this function (so a request that lands
/// immediately after registration finds the callable).
///
/// On success: writes the handler_id to `*out_handler_id`,
/// returns a heap-allocated ServeHandle. On failure: returns
/// NULL and writes an error message to `out_err`.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_serve(
    handle: *mut MeshRpcHandle,
    service_ptr: *const c_char,
    service_len: usize,
    out_handler_id: *mut u64,
    out_err: *mut *mut c_char,
) -> *mut ServeHandleC {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        write_err(out_err, "MeshRpc handle is NULL".into());
        return std::ptr::null_mut();
    };
    let Some(service) = cstr_to_string(service_ptr, service_len) else {
        write_err(out_err, "service name is NULL or non-UTF-8".into());
        return std::ptr::null_mut();
    };
    if DISPATCHER.get().is_none() {
        write_err(
            out_err,
            "net_rpc_set_handler_dispatcher must be called before net_rpc_serve".into(),
        );
        return std::ptr::null_mut();
    }
    let handler_id = NEXT_HANDLER_ID.fetch_add(1, Ordering::Relaxed);
    let rust_handler = Arc::new(GoRpcHandler {
        handler_id,
        timeout: DEFAULT_HANDLER_TIMEOUT,
    });
    match h.node.serve_rpc(&service, rust_handler) {
        Ok(inner) => {
            unsafe {
                *out_handler_id = handler_id;
            }
            Box::into_raw(Box::new(ServeHandleC {
                inner: Arc::new(Mutex::new(Some(inner))),
                handler_id,
            }))
        }
        Err(e) => {
            write_err(out_err, format!("serve failed: {e}"));
            // Note: the use of e.to_string includes the
            // serve-error variant name, so the Go side can
            // detect "already serving" via prefix matching.
            std::ptr::null_mut()
        }
    }
}

/// Diagnostic accessor: handler_id of this ServeHandle.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_serve_handle_id(handle: *const ServeHandleC) -> u64 {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    h.handler_id
}

/// Unregister the service. Idempotent — repeated calls are
/// no-ops. After close, in-flight handlers continue but no new
/// requests will be dispatched.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_serve_handle_close(handle: *mut ServeHandleC) {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return;
    };
    let _ = h.inner.lock().take();
}

/// Free the ServeHandle. Implicitly closes if not already
/// closed. Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_serve_handle_free(handle: *mut ServeHandleC) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

// =========================================================================
// Tests for pure-logic helpers.
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::mesh_rpc::CodecDirection;
    use net::error::AdapterError;

    /// `format_rpc_error` produces the documented stable kind
    /// segment for each variant. The Go side parses the kind
    /// segment to dispatch to typed errors.
    #[test]
    fn format_rpc_error_kind_segments_are_stable() {
        assert!(format_rpc_error(&InnerRpcError::NoRoute {
            target: 0xABCD,
            reason: "x".into(),
        })
        .starts_with("no_route:"));
        assert!(format_rpc_error(&InnerRpcError::Timeout { elapsed_ms: 100 })
            .starts_with("timeout:"));
        assert!(format_rpc_error(&InnerRpcError::ServerError {
            status: 0x4001,
            message: "x".into(),
        })
        .starts_with("server_error:"));
        assert!(format_rpc_error(&InnerRpcError::Transport(
            AdapterError::Connection("boom".into())
        ))
        .starts_with("transport:"));
        assert!(format_rpc_error(&InnerRpcError::Codec {
            direction: CodecDirection::Encode,
            message: "x".into(),
        })
        .starts_with("codec_encode:"));
        assert!(format_rpc_error(&InnerRpcError::Codec {
            direction: CodecDirection::Decode,
            message: "x".into(),
        })
        .starts_with("codec_decode:"));
    }

    /// `cstr_to_string` rejects NULL and invalid UTF-8.
    #[test]
    fn cstr_to_string_handles_null_and_bad_utf8() {
        assert!(cstr_to_string(std::ptr::null(), 0).is_none());
        let bad: [u8; 3] = [0xff, 0xfe, 0xfd];
        assert!(cstr_to_string(bad.as_ptr() as *const c_char, 3).is_none());
        let good = b"hello";
        assert_eq!(
            cstr_to_string(good.as_ptr() as *const c_char, 5).as_deref(),
            Some("hello"),
        );
    }

    /// `build_call_options` honors deadline_ms == 0 as "no deadline".
    #[test]
    fn build_call_options_deadline_zero_means_no_deadline() {
        let opts = build_call_options(0);
        assert!(opts.deadline.is_none());
        let opts = build_call_options(500);
        assert!(opts.deadline.is_some());
    }

    /// `write_response` round-trips through the freer without
    /// leaking. Reconstructs the Vec via the matching
    /// `from_raw_parts(ptr, len, len)`.
    #[test]
    fn write_response_then_response_free_round_trips() {
        let body = b"hello world".to_vec();
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        write_response(body, &mut out_ptr, &mut out_len);
        assert_eq!(out_len, 11);
        assert!(!out_ptr.is_null());
        // Free should run without panicking — the matching
        // Vec::from_raw_parts is the load-bearing test.
        net_rpc_response_free(out_ptr, out_len);
    }
}
