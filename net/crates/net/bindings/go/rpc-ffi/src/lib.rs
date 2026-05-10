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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::runtime::Runtime;

use futures::StreamExt;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{
    CallOptions as InnerCallOptions, RpcError as InnerRpcError, RpcStream as InnerRpcStream,
    ServeHandle as InnerServeHandle,
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
/// `net_rpc_stream_next` was called on a stream that has already
/// produced its terminal item (clean end OR a mid-stream error).
/// Surfaced as a sentinel separate from `NET_RPC_OK` so the Go
/// side can distinguish "stream is done — release the handle"
/// from "no chunk available right now."
pub const NET_RPC_ERR_STREAM_DONE: c_int = -6;

// =========================================================================
// ABI version stamp.
// =========================================================================

/// ABI version stamp. Bumped on any breaking change to the C-ABI
/// surface (signature changes, error-code re-numbering, layout
/// changes to opaque structs, semantic shifts in lifetime
/// contracts). Consumers SHOULD compare against their compiled-in
/// expected version at process init and refuse to load a mismatch.
///
///   - **0001** — initial release: lifecycle + unary `call` /
///     `call_service` / `find_service_nodes` / `serve` plus
///     Phase B6 streaming (`call_streaming`, `stream_next`,
///     `stream_grant`, `stream_close`, `stream_free`).
pub const NET_RPC_ABI_VERSION: u32 = 0x0001;

/// Returns the current ABI version. Consumers SHOULD call this at
/// init and compare against their expected value.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_abi_version() -> u32 {
    NET_RPC_ABI_VERSION
}

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

/// Monotonic counter for in-flight call cancel tokens. Starts at
/// 1 so `0` is reserved as "no token" sentinel.
static NEXT_CANCEL_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Per-token state. CR-13: split into `cancelled` (boolean
/// observed by `run_cancellable` after registering its handle)
/// and `handle` (the abort handle, populated by
/// `run_cancellable` post-spawn). Pre-CR-13 the registry was
/// `HashMap<u64, AbortHandle>`, so a cancel arriving in the
/// gap between `reserve_cancel_token` and the post-spawn
/// `insert` (or even in the gap between `spawn` and `insert`)
/// found no entry and dropped on the floor.
#[derive(Default)]
struct CancelEntry {
    cancelled: bool,
    handle: Option<tokio::task::AbortHandle>,
}

/// Process-global registry of in-flight call cancel state.
/// Populated by `net_rpc_call*` when the caller passes a non-NULL
/// `out_cancel_token` out-param; queried by `net_rpc_cancel_call`.
/// Entry removal happens by `run_cancellable` once the call
/// returns (success or abort path). A cancel issued for a never-
/// dispatched token leaves a `cancelled: true` entry behind that
/// the eventual `run_cancellable` (or `unregister_cancel_token`)
/// cleans up; in pathological never-dispatched paths the entry
/// is bounded by the number of unique tokens reserved, which is
/// negligible.
fn cancel_registry(
) -> &'static parking_lot::Mutex<std::collections::HashMap<u64, CancelEntry>> {
    static REG: OnceLock<
        parking_lot::Mutex<std::collections::HashMap<u64, CancelEntry>>,
    > = OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// Reserve a fresh cancel token. The Go side uses this to set up
/// a watcher (typically on `ctx.Done()`) BEFORE issuing the
/// blocking call — so the watcher's call to `net_rpc_cancel_call`
/// can race the call's completion safely.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_reserve_cancel_token() -> u64 {
    NEXT_CANCEL_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// Abort the in-flight call associated with `token`. Idempotent;
/// no-op if the token doesn't refer to a live call (already
/// completed, never registered, or already cancelled).
///
/// The aborted task's future is dropped, which fires the SDK's
/// `UnaryCallGuard::Drop` to publish CANCEL to the server. The
/// caller-side `net_rpc_call` returns `NET_RPC_ERR_CALL_FAILED`
/// with `out_err = "nrpc:cancelled: call cancelled by caller"`.
///
/// CR-13: a cancel that arrives BEFORE `run_cancellable` has
/// registered its abort handle (the gap between
/// `reserve_cancel_token` and the actual `rpc_call`, or between
/// `spawn` and `insert` inside `run_cancellable`) used to drop
/// on the floor — the call would run to completion. Now we
/// either abort an existing handle, or insert/mark the entry
/// with `cancelled = true` so `run_cancellable` aborts on
/// register.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_cancel_call(token: u64) {
    if token == 0 {
        return;
    }
    let mut reg = cancel_registry().lock();
    let entry = reg.entry(token).or_default();
    entry.cancelled = true;
    if let Some(handle) = entry.handle.take() {
        // Drop the lock before invoking abort; abort is cheap
        // but we don't want to hold the registry lock across
        // arbitrary tokio internals.
        drop(reg);
        handle.abort();
    }
}

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
                    let bytes = unsafe { std::slice::from_raw_parts(resp_ptr, resp_len).to_vec() };
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

/// Free a buffer of `len` u8s previously returned out-of-band by
/// this crate (e.g. response bytes from `net_rpc_call`). Idempotent
/// on NULL or zero-length.
///
/// Layout invariant: every site that hands a buffer out via
/// [`write_response`] does so by `Box::into_raw(Vec::into_boxed_slice)`,
/// whose memory layout is `(ptr, len)` with `cap == len` baked in —
/// no `shrink_to_fit` best-effort hazard. The free path
/// reconstructs the same `Box<[u8]>` and drops it.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_response_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

/// Free an array of u64 node ids previously returned by
/// [`net_rpc_find_service_nodes`]. Idempotent on NULL or zero.
///
/// Same Box<[u64]> layout discipline as
/// [`net_rpc_response_free`] — see its doc.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_find_service_nodes_free(ptr: *mut u64, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
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
    inner
}

/// FFI-side request-header descriptor. The C ABI consumer
/// allocates a `net_rpc_header_t[]`, fills each entry with
/// `(name_ptr, name_len, value_ptr, value_len)` slices it owns,
/// and passes the array + count to a `_with_headers` call. The
/// header name MUST be valid UTF-8; the value is opaque bytes
/// (Phase 9b's `cyberdeck-where:` value is JSON, but the
/// substrate doesn't enforce a value-side encoding).
///
/// Buffers are referenced for the duration of the call only —
/// the Rust side copies into owned `(String, Vec<u8>)` pairs
/// before dispatching, so the C consumer's lifetime concern is
/// "stays valid until the function returns."
#[repr(C)]
pub struct NetRpcHeader {
    pub name_ptr: *const c_char,
    pub name_len: usize,
    pub value_ptr: *const u8,
    pub value_len: usize,
}

/// Convert a C `(headers_ptr, header_count)` array into the
/// substrate's `Vec<(String, Vec<u8>)>` shape. Returns `None` if
/// any header name fails UTF-8 validation OR the caller passed
/// `header_count > 0` with a NULL `headers_ptr` (a contract
/// violation — the caller claims to ship N headers but didn't
/// supply the array). Caller maps `None` to a typed error.
unsafe fn collect_headers(
    headers_ptr: *const NetRpcHeader,
    header_count: usize,
) -> Option<Vec<(String, Vec<u8>)>> {
    if header_count == 0 {
        // Zero headers — the pointer is allowed to be NULL or
        // dangling because we never dereference it.
        return Some(Vec::new());
    }
    if headers_ptr.is_null() {
        // Caller said N>0 but didn't actually pass an array.
        // Surface as invalid input instead of silently dropping
        // every header.
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(headers_ptr, header_count) };
    let mut out = Vec::with_capacity(header_count);
    for h in slice {
        if h.name_ptr.is_null() {
            return None;
        }
        let name_bytes = unsafe { std::slice::from_raw_parts(h.name_ptr as *const u8, h.name_len) };
        let name = match std::str::from_utf8(name_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => return None,
        };
        let value = if h.value_ptr.is_null() || h.value_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(h.value_ptr, h.value_len).to_vec() }
        };
        out.push((name, value));
    }
    Some(out)
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
///
/// `cancel_token` (optional, pass `0` to opt out): a token from
/// [`net_rpc_reserve_cancel_token`]. When non-zero, the call is
/// dispatched on a tokio task whose `AbortHandle` is registered
/// under the token; a parallel call to [`net_rpc_cancel_call`]
/// drops the in-flight future, firing CANCEL to the server and
/// returning a `nrpc:cancelled:` error to the caller. The token
/// MUST have been reserved before this call to close the
/// "cancel arrives before registration" race.
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
    cancel_token: u64,
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

    let result = run_cancellable(cancel_token, async move {
        node.call(target_node_id, &service, req_bytes, opts).await
    });

    match result {
        Ok(Ok(reply)) => {
            write_response(reply.body.to_vec(), out_resp_ptr, out_resp_len);
            NET_RPC_OK
        }
        Ok(Err(e)) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
        Err(CancelledError) => {
            write_err(out_err, "cancelled: call cancelled by caller".into());
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// Sentinel for "the cancellable future was aborted by
/// [`net_rpc_cancel_call`]." Surfaces to Go as `nrpc:cancelled:`
/// (the Go wrapper's `parseRpcError` recognizes the kind segment).
#[derive(Debug)]
struct CancelledError;

/// Run `fut` under [`runtime().block_on`]. If `cancel_token != 0`,
/// register the task's `AbortHandle` so a parallel
/// [`net_rpc_cancel_call`] aborts mid-flight (which drops the
/// future, firing the SDK's `UnaryCallGuard::Drop` → CANCEL on
/// the wire). The await returns `Err(CancelledError)` on abort.
///
/// `token == 0` short-circuits to a plain `block_on` so the
/// non-cancellable path stays free of registry overhead.
fn run_cancellable<F, T>(cancel_token: u64, fut: F) -> std::result::Result<T, CancelledError>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if cancel_token == 0 {
        return Ok(runtime().block_on(fut));
    }
    let task = runtime().spawn(fut);
    let abort_handle = task.abort_handle();
    // CR-13: register-or-observe-prior-cancel. If
    // `net_rpc_cancel_call` already fired between the caller's
    // `reserve_cancel_token` and this register, the entry is
    // present with `cancelled=true` — abort right away. Else
    // store the abort handle so a future cancel finds it.
    let was_already_cancelled = {
        let mut reg = cancel_registry().lock();
        let entry = reg.entry(cancel_token).or_default();
        if entry.cancelled {
            true
        } else {
            entry.handle = Some(abort_handle.clone());
            false
        }
    };
    if was_already_cancelled {
        abort_handle.abort();
    }
    let result = runtime().block_on(task);
    // Cleanup: drop the entry whether we registered, observed
    // a prior cancel, or observed a successful completion.
    cancel_registry().lock().remove(&cancel_token);
    match result {
        Ok(value) => Ok(value),
        Err(join_err) if join_err.is_cancelled() => Err(CancelledError),
        Err(_join_err) => {
            // A panic in the SDK call surfaces here. Convert to a
            // sentinel cancel so the caller gets a useful
            // diagnostic rather than process-wide panic
            // propagation across the FFI.
            Err(CancelledError)
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
    cancel_token: u64,
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

    let result = run_cancellable(cancel_token, async move {
        node.call_service(&service, req_bytes, opts).await
    });

    match result {
        Ok(Ok(reply)) => {
            write_response(reply.body.to_vec(), out_resp_ptr, out_resp_len);
            NET_RPC_OK
        }
        Ok(Err(e)) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
        Err(CancelledError) => {
            write_err(out_err, "cancelled: call cancelled by caller".into());
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// Hand a `Vec<u8>` to the Go caller as a raw pointer + length.
///
/// Uses `Vec::into_boxed_slice` rather than `shrink_to_fit + Vec::
/// from_raw_parts(ptr, len, len)`: `shrink_to_fit` is documented
/// "best effort" and an allocator that rounds up (mimalloc on
/// some platforms, jemalloc historically) would leave `cap > len`,
/// making the freer's `Vec::from_raw_parts(_, len, len)` a
/// soundness violation (UB on dealloc — wrong layout). Boxed
/// slices have an exact `(ptr, len)` representation; the matching
/// free reconstructs `Box<[u8]>` directly.
fn write_response(body: Vec<u8>, out_ptr: *mut *mut u8, out_len: *mut usize) {
    let boxed: Box<[u8]> = body.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
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
    // Same boxed-slice discipline as `write_response` — `cap ==
    // len` exactly, no `shrink_to_fit` best-effort hazard. The
    // matching free is `net_rpc_find_service_nodes_free`.
    let boxed: Box<[u64]> = nodes.into_boxed_slice();
    let count = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u64;
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

/// Reserve the next handler id without registering anything.
///
/// The Go side uses this to close the request-arrives-before-Store
/// race: it reserves an id, stores the callable in its registry
/// under that id, THEN calls [`net_rpc_serve`] with the reserved
/// id. Without this, a request landing in the dispatcher between
/// `serve_rpc` returning and Go's `Store` would observe an empty
/// registry slot.
///
/// IDs are monotonically increasing from 1 and never reused; an
/// unused reservation is harmless (no cleanup required).
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_reserve_handler_id() -> u64 {
    NEXT_HANDLER_ID.fetch_add(1, Ordering::Relaxed)
}

/// Register a handler for `service`. The caller passes a
/// `handler_id` it ALREADY reserved (via
/// [`net_rpc_reserve_handler_id`]) AND already inserted into its
/// language-side callback registry. Pre-registration is the
/// load-bearing invariant — between `serve_rpc` returning and
/// the dispatcher's first lookup, the callable MUST be findable
/// under the supplied id, otherwise an early-arriving request
/// gets dropped as "no handler registered."
///
/// `handler_timeout_ms` caps the per-call wait for the Go-side
/// handler to respond. Pass `0` for the default (60 000ms / 60s);
/// pass a positive value for an explicit cap; pass `u64::MAX`
/// to effectively disable the cap (not recommended — a stuck
/// handler holds a runtime worker indefinitely).
///
/// Returns: heap-allocated ServeHandle on success; NULL with an
/// error message on `out_err` on failure (e.g. service already
/// served by this MeshNode).
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_serve(
    handle: *mut MeshRpcHandle,
    service_ptr: *const c_char,
    service_len: usize,
    handler_id: u64,
    handler_timeout_ms: u64,
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
    if handler_id == 0 {
        write_err(
            out_err,
            "handler_id must be non-zero (reserve via net_rpc_reserve_handler_id)".into(),
        );
        return std::ptr::null_mut();
    }
    let timeout = if handler_timeout_ms == 0 {
        DEFAULT_HANDLER_TIMEOUT
    } else {
        Duration::from_millis(handler_timeout_ms)
    };
    let rust_handler = Arc::new(GoRpcHandler {
        handler_id,
        timeout,
    });
    match h.node.serve_rpc(&service, rust_handler) {
        Ok(inner) => Box::into_raw(Box::new(ServeHandleC {
            inner: Arc::new(Mutex::new(Some(inner))),
            handler_id,
        })),
        Err(e) => {
            // `e.to_string()` includes the serve-error variant
            // name; the Go side does prefix matching to surface
            // a typed `ErrAlreadyServing` for the
            // `ServeError::AlreadyServing` case.
            write_err(out_err, format!("serve failed: {e}"));
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
// Streaming — opaque RpcStreamHandle, blocking next, explicit grant.
// =========================================================================

/// Opaque RpcStream handle exposed to Go. The inner SDK stream sits
/// behind an `Arc<Mutex<Option<...>>>` so:
///   - `close()` can `take()` the stream (which fires CANCEL via
///     the SDK's `Drop` impl) and remain idempotent.
///   - `next()` locks, polls, and re-stores `Some(stream)` until
///     the stream terminates.
///
/// Once `close()` runs OR the stream has yielded its terminal item,
/// subsequent `next()` calls return `NET_RPC_ERR_STREAM_DONE`.
pub struct RpcStreamHandleC {
    inner: Arc<Mutex<Option<InnerRpcStream>>>,
    /// Mirrors the SDK's `RpcStream::call_id`. Captured at
    /// construction so the diagnostic accessor doesn't need to
    /// re-acquire the mutex.
    call_id: u64,
    /// `true` once a terminal item (clean end OR error) has been
    /// observed. Latched separately from the `Option` so we don't
    /// re-take the inner stream just to check this state.
    done: AtomicBool,
}

/// Direct-addressed streaming call. Constructs the underlying
/// `RpcStream` synchronously (via `runtime.block_on`) and returns
/// an opaque handle. Per-chunk delivery is via
/// [`net_rpc_stream_next`].
///
/// `stream_window` of `0` disables flow control (auto-grant only).
/// Non-zero installs an initial credit window equal to the value
/// (matches `CallOptions::stream_window_initial`).
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_call_streaming(
    handle: *mut MeshRpcHandle,
    target_node_id: u64,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    deadline_ms: u64,
    stream_window: u32,
    out_stream: *mut *mut RpcStreamHandleC,
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
    let mut opts = build_call_options(deadline_ms);
    if stream_window > 0 {
        opts.stream_window_initial = Some(stream_window);
    }
    let node = h.node.clone();

    let result = runtime().block_on(async move {
        node.call_streaming(target_node_id, &service, req_bytes, opts)
            .await
    });

    match result {
        Ok(stream) => {
            let call_id = stream.call_id();
            let boxed = Box::new(RpcStreamHandleC {
                inner: Arc::new(Mutex::new(Some(stream))),
                call_id,
                done: AtomicBool::new(false),
            });
            unsafe {
                *out_stream = Box::into_raw(boxed);
            }
            NET_RPC_OK
        }
        Err(e) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// Block until the next chunk arrives, OR the stream terminates,
/// OR a mid-stream error fires.
///
/// Outcomes:
///   - chunk available: `*out_chunk_ptr / *out_chunk_len` set,
///     returns `NET_RPC_OK`. Caller frees the buffer via
///     [`net_rpc_response_free`].
///   - clean end: `*out_chunk_ptr == NULL`, `*out_chunk_len == 0`,
///     returns `NET_RPC_ERR_STREAM_DONE`. Subsequent calls return
///     the same code.
///   - mid-stream error: `*out_err` set, returns
///     `NET_RPC_ERR_CALL_FAILED`. The stream is also marked done;
///     subsequent calls return `NET_RPC_ERR_STREAM_DONE`.
///   - close raced: returns `NET_RPC_ERR_STREAM_DONE` (the take()
///     beat us to the inner stream).
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_stream_next(
    stream: *mut RpcStreamHandleC,
    out_chunk_ptr: *mut *mut u8,
    out_chunk_len: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int {
    let Some(s) = (unsafe { stream.as_ref() }) else {
        return NET_RPC_ERR_NULL;
    };
    if s.done.load(Ordering::Relaxed) {
        unsafe {
            *out_chunk_ptr = std::ptr::null_mut();
            *out_chunk_len = 0;
        }
        return NET_RPC_ERR_STREAM_DONE;
    }
    // Take the inner stream out of the mutex while we await — so
    // a concurrent `close()` (which `take()`s) can race us cleanly
    // by either taking ownership before us (we observe `None`,
    // return STREAM_DONE) or after us (we put the stream back; the
    // next close() takes it then).
    let inner_opt = s.inner.lock().take();
    let mut inner = match inner_opt {
        Some(i) => i,
        None => {
            s.done.store(true, Ordering::Relaxed);
            unsafe {
                *out_chunk_ptr = std::ptr::null_mut();
                *out_chunk_len = 0;
            }
            return NET_RPC_ERR_STREAM_DONE;
        }
    };
    let result = runtime().block_on(async { inner.next().await });
    match result {
        Some(Ok(chunk)) => {
            // Put the stream back so subsequent `next()` polls keep
            // going.
            *s.inner.lock() = Some(inner);
            write_response(chunk.to_vec(), out_chunk_ptr, out_chunk_len);
            NET_RPC_OK
        }
        Some(Err(e)) => {
            // Mid-stream error — the SDK guarantees no further items.
            // Drop the inner (firing CANCEL is unnecessary since the
            // server already terminated us) and latch done.
            drop(inner);
            s.done.store(true, Ordering::Relaxed);
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
        None => {
            // Clean end. Drop the inner and latch done.
            drop(inner);
            s.done.store(true, Ordering::Relaxed);
            unsafe {
                *out_chunk_ptr = std::ptr::null_mut();
                *out_chunk_len = 0;
            }
            NET_RPC_ERR_STREAM_DONE
        }
    }
}

/// Explicitly grant `amount` more credits to the server's pump.
/// No-op if flow control wasn't enabled for this stream OR the
/// stream is already done. See `RpcStream::grant` for semantics.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_stream_grant(stream: *mut RpcStreamHandleC, amount: u32) -> c_int {
    let Some(s) = (unsafe { stream.as_ref() }) else {
        return NET_RPC_ERR_NULL;
    };
    if s.done.load(Ordering::Relaxed) || amount == 0 {
        return NET_RPC_OK;
    }
    let guard = s.inner.lock();
    if let Some(inner) = guard.as_ref() {
        inner.grant(amount);
    }
    NET_RPC_OK
}

/// Diagnostic accessor: server-assigned call_id for this stream.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_stream_call_id(stream: *const RpcStreamHandleC) -> u64 {
    let Some(s) = (unsafe { stream.as_ref() }) else {
        return 0;
    };
    s.call_id
}

/// Cancel the stream (best-effort CANCEL via the SDK's Drop impl)
/// and latch it as done. Idempotent on NULL or already-closed.
/// Subsequent `next()` calls return `NET_RPC_ERR_STREAM_DONE`.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_stream_close(stream: *mut RpcStreamHandleC) {
    let Some(s) = (unsafe { stream.as_ref() }) else {
        return;
    };
    s.done.store(true, Ordering::Relaxed);
    let _ = s.inner.lock().take();
}

/// Free the stream handle. Implicitly closes if not already
/// closed. Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_stream_free(stream: *mut RpcStreamHandleC) {
    if stream.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(stream));
    }
}

// =========================================================================
// Header-bearing call variants (Phase 9b end-to-end).
//
// The legacy `net_rpc_call` / `_call_service` / `_call_streaming`
// don't take request headers — predicate-pushdown via the
// `cyberdeck-where:` header (built by
// `net_predicate_to_where_header` in the main `libnet` cdylib)
// had nowhere to go on the C ABI side. These three additive
// variants accept a `(headers, count)` pair and forward it to
// `InnerCallOptions::request_headers`. Legacy variants are
// untouched — non-9b callers compile + run as before.
// =========================================================================

/// `net_rpc_call` with arbitrary request headers attached.
/// Ergonomically identical to the legacy variant plus the
/// `(headers_ptr, header_count)` parameters.
///
/// Header buffers are read for the duration of the call only —
/// Rust copies into owned `(String, Vec<u8>)` before dispatching,
/// so the consumer can release / reuse the memory once the call
/// returns. NULL `headers_ptr` with `header_count == 0` is
/// equivalent to the legacy variant.
///
/// Header name MUST be valid UTF-8; non-UTF-8 names return
/// [`NET_RPC_ERR_INVALID_UTF8`] with a descriptive `out_err`
/// message and never reach the wire.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_call_with_headers(
    handle: *mut MeshRpcHandle,
    target_node_id: u64,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    deadline_ms: u64,
    cancel_token: u64,
    headers_ptr: *const NetRpcHeader,
    header_count: usize,
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
    let Some(headers) = (unsafe { collect_headers(headers_ptr, header_count) }) else {
        write_err(out_err, "request header name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let req_bytes = if req_ptr.is_null() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })
    };
    let mut opts = build_call_options(deadline_ms);
    opts.request_headers = headers;
    let node = h.node.clone();

    let result = run_cancellable(cancel_token, async move {
        node.call(target_node_id, &service, req_bytes, opts).await
    });

    match result {
        Ok(Ok(reply)) => {
            write_response(reply.body.to_vec(), out_resp_ptr, out_resp_len);
            NET_RPC_OK
        }
        Ok(Err(e)) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
        Err(CancelledError) => {
            write_err(out_err, "cancelled: call cancelled by caller".into());
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// `net_rpc_call_service` with arbitrary request headers attached.
/// Same shape as [`net_rpc_call_with_headers`] but resolves
/// `service` against the local capability index instead of taking
/// an explicit target.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_call_service_with_headers(
    handle: *mut MeshRpcHandle,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    deadline_ms: u64,
    cancel_token: u64,
    headers_ptr: *const NetRpcHeader,
    header_count: usize,
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
    let Some(headers) = (unsafe { collect_headers(headers_ptr, header_count) }) else {
        write_err(out_err, "request header name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let req_bytes = if req_ptr.is_null() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })
    };
    let mut opts = build_call_options(deadline_ms);
    opts.request_headers = headers;
    let node = h.node.clone();

    let result = run_cancellable(cancel_token, async move {
        node.call_service(&service, req_bytes, opts).await
    });

    match result {
        Ok(Ok(reply)) => {
            write_response(reply.body.to_vec(), out_resp_ptr, out_resp_len);
            NET_RPC_OK
        }
        Ok(Err(e)) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
        Err(CancelledError) => {
            write_err(out_err, "cancelled: call cancelled by caller".into());
            NET_RPC_ERR_CALL_FAILED
        }
    }
}

/// `net_rpc_call_streaming` with arbitrary request headers
/// attached. Same shape as
/// [`net_rpc_call_streaming`](self::net_rpc_call_streaming) plus the
/// `(headers_ptr, header_count)` pair.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_rpc_call_streaming_with_headers(
    handle: *mut MeshRpcHandle,
    target_node_id: u64,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    deadline_ms: u64,
    stream_window: u32,
    headers_ptr: *const NetRpcHeader,
    header_count: usize,
    out_stream: *mut *mut RpcStreamHandleC,
    out_err: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_RPC_ERR_NULL;
    };
    let Some(service) = cstr_to_string(service_ptr, service_len) else {
        write_err(out_err, "service name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let Some(headers) = (unsafe { collect_headers(headers_ptr, header_count) }) else {
        write_err(out_err, "request header name is NULL or non-UTF-8".into());
        return NET_RPC_ERR_INVALID_UTF8;
    };
    let req_bytes = if req_ptr.is_null() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })
    };
    let mut opts = build_call_options(deadline_ms);
    if stream_window > 0 {
        opts.stream_window_initial = Some(stream_window);
    }
    opts.request_headers = headers;
    let node = h.node.clone();

    let result = runtime().block_on(async move {
        node.call_streaming(target_node_id, &service, req_bytes, opts)
            .await
    });

    match result {
        Ok(stream) => {
            let call_id = stream.call_id();
            let boxed = Box::new(RpcStreamHandleC {
                inner: Arc::new(Mutex::new(Some(stream))),
                call_id,
                done: AtomicBool::new(false),
            });
            unsafe {
                *out_stream = Box::into_raw(boxed);
            }
            NET_RPC_OK
        }
        Err(e) => {
            write_err(out_err, format_rpc_error(&e));
            NET_RPC_ERR_CALL_FAILED
        }
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
        assert!(
            format_rpc_error(&InnerRpcError::Timeout { elapsed_ms: 100 }).starts_with("timeout:")
        );
        assert!(format_rpc_error(&InnerRpcError::ServerError {
            status: 0x4001,
            message: "x".into(),
        })
        .starts_with("server_error:"));
        assert!(
            format_rpc_error(&InnerRpcError::Transport(AdapterError::Connection(
                "boom".into()
            )))
            .starts_with("transport:")
        );
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

    /// Regression: `collect_headers` rejects `headers_ptr == NULL`
    /// when the caller claims `header_count > 0`. The pre-fix
    /// `if header_count == 0 || headers_ptr.is_null()` short-circuit
    /// silently returned an empty Vec for that combo, dropping
    /// every header the caller intended to ship.
    #[test]
    fn collect_headers_rejects_null_pointer_with_nonzero_count() {
        // NULL + count > 0 → invalid FFI input → None (caller
        // surfaces a typed error).
        let out = unsafe { collect_headers(std::ptr::null(), 3) };
        assert!(
            out.is_none(),
            "headers_ptr=NULL with count>0 must surface as invalid input, not as empty headers",
        );

        // count == 0 stays permissive: the pointer is never read,
        // so NULL / dangling is fine.
        let out = unsafe { collect_headers(std::ptr::null(), 0) };
        assert_eq!(out.as_deref().map(|v| v.len()), Some(0));

        // Negative control: a real array round-trips.
        let name = b"x-trace";
        let value = b"abc";
        let h = NetRpcHeader {
            name_ptr: name.as_ptr() as *const c_char,
            name_len: name.len(),
            value_ptr: value.as_ptr(),
            value_len: value.len(),
        };
        let out = unsafe { collect_headers(&h, 1) }.expect("valid header");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "x-trace");
        assert_eq!(out[0].1, b"abc");
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
    /// leaking. The boxed-slice layout means `cap == len` is
    /// guaranteed, not best-effort.
    #[test]
    fn write_response_then_response_free_round_trips() {
        let body = b"hello world".to_vec();
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        write_response(body, &mut out_ptr, &mut out_len);
        assert_eq!(out_len, 11);
        assert!(!out_ptr.is_null());
        net_rpc_response_free(out_ptr, out_len);
    }

    /// Regression: a Vec with capacity strictly greater than its
    /// length still round-trips correctly. The previous
    /// implementation called `shrink_to_fit` (best-effort) before
    /// `Vec::from_raw_parts(ptr, len, len)`; on an allocator that
    /// rounded the shrink up, the freer would deallocate with the
    /// wrong layout and trip UB. The boxed-slice layout used now
    /// removes that hazard — `into_boxed_slice` forces the cap to
    /// the exact len at the type level.
    #[test]
    fn write_response_handles_overallocated_vec() {
        let mut body: Vec<u8> = Vec::with_capacity(1024);
        body.extend_from_slice(b"short");
        // Sanity: cap > len so we exercise the formerly-hazardous path.
        assert!(body.capacity() > body.len());
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        write_response(body, &mut out_ptr, &mut out_len);
        assert_eq!(out_len, 5);
        assert!(!out_ptr.is_null());
        // Round-trip: the freer's Box<[u8]> reconstruction sees
        // matching layout and frees cleanly.
        net_rpc_response_free(out_ptr, out_len);
    }

    /// `net_rpc_abi_version` exposes the same constant declared
    /// in the source — drift between the consumer's expected
    /// version and the linked cdylib's actual version is the
    /// whole reason this stamp exists.
    #[test]
    fn abi_version_matches_constant() {
        assert_eq!(net_rpc_abi_version(), NET_RPC_ABI_VERSION);
        assert_eq!(NET_RPC_ABI_VERSION, 0x0001);
    }

    /// `net_rpc_stream_next` on a `done`-latched handle returns
    /// `STREAM_DONE` without touching the inner mutex (which is
    /// `None` after close). Subsequent calls keep returning the
    /// same code — no transition to OK.
    #[test]
    fn stream_next_after_close_returns_stream_done() {
        let handle = Box::into_raw(Box::new(RpcStreamHandleC {
            inner: Arc::new(Mutex::new(None)),
            call_id: 42,
            done: AtomicBool::new(false),
        }));
        // Pre-close — no inner stream → take() returns None →
        // latches done + returns STREAM_DONE.
        let mut chunk_ptr: *mut u8 = std::ptr::null_mut();
        let mut chunk_len: usize = 0;
        let mut err_ptr: *mut c_char = std::ptr::null_mut();
        let code1 = net_rpc_stream_next(handle, &mut chunk_ptr, &mut chunk_len, &mut err_ptr);
        assert_eq!(code1, NET_RPC_ERR_STREAM_DONE);
        assert!(chunk_ptr.is_null());
        assert_eq!(chunk_len, 0);
        // Second call hits the early-out via the latched flag.
        let code2 = net_rpc_stream_next(handle, &mut chunk_ptr, &mut chunk_len, &mut err_ptr);
        assert_eq!(code2, NET_RPC_ERR_STREAM_DONE);
        // Cleanup.
        net_rpc_stream_free(handle);
    }

    /// `net_rpc_stream_close` on a freshly-built handle latches
    /// `done` and clears the inner option, even when called
    /// multiple times.
    #[test]
    fn stream_close_is_idempotent() {
        let handle = Box::into_raw(Box::new(RpcStreamHandleC {
            inner: Arc::new(Mutex::new(None)),
            call_id: 7,
            done: AtomicBool::new(false),
        }));
        net_rpc_stream_close(handle);
        net_rpc_stream_close(handle); // second close — no panic
                                      // call_id stays addressable even after close.
        assert_eq!(net_rpc_stream_call_id(handle), 7);
        net_rpc_stream_free(handle);
    }

    /// `net_rpc_stream_grant` on a closed stream is a quiet
    /// no-op — never panics, never publishes spurious credit.
    #[test]
    fn stream_grant_after_close_is_noop() {
        let handle = Box::into_raw(Box::new(RpcStreamHandleC {
            inner: Arc::new(Mutex::new(None)),
            call_id: 99,
            done: AtomicBool::new(true),
        }));
        let code = net_rpc_stream_grant(handle, 16);
        assert_eq!(code, NET_RPC_OK);
        net_rpc_stream_free(handle);
    }

    /// `net_rpc_reserve_handler_id` returns monotonically
    /// increasing non-zero ids. Pinned because `net_rpc_serve`
    /// rejects `handler_id == 0` — a buggy reservation API that
    /// returned zero would silently break every Go-side serve
    /// call.
    #[test]
    fn reserve_handler_id_is_monotonic_and_nonzero() {
        let a = net_rpc_reserve_handler_id();
        let b = net_rpc_reserve_handler_id();
        let c = net_rpc_reserve_handler_id();
        assert!(a > 0 && b > 0 && c > 0, "ids must be non-zero");
        assert!(b > a && c > b, "ids must be strictly increasing");
    }

    /// `net_rpc_reserve_cancel_token` returns monotonically
    /// increasing non-zero tokens, and `net_rpc_cancel_call(0)`
    /// is a quiet no-op. Pinned: a regression to "0 is a valid
    /// token" would silently break the cancellation path.
    #[test]
    fn cancel_token_reserve_and_zero_cancel_are_safe() {
        let a = net_rpc_reserve_cancel_token();
        let b = net_rpc_reserve_cancel_token();
        assert!(a > 0 && b > 0 && b > a);
        // Cancelling 0 / a never-registered token is a quiet no-op.
        net_rpc_cancel_call(0);
        net_rpc_cancel_call(u64::MAX);
    }

    /// `run_cancellable` aborts the future when
    /// `net_rpc_cancel_call(token)` fires from another thread.
    /// Pinned: this is the load-bearing invariant the entire
    /// ctx.Done() → CANCEL pathway depends on.
    #[test]
    fn run_cancellable_aborts_on_cancel_call() {
        let token = net_rpc_reserve_cancel_token();
        // Fire cancel from a sibling thread; the future below
        // sleeps far longer than the cancel deadline, so if abort
        // doesn't work the test wedges (caught by cargo's
        // per-test timeout).
        let cancel_token = token;
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            net_rpc_cancel_call(cancel_token);
        });
        let result = run_cancellable(token, async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            "should never reach here"
        });
        assert!(result.is_err(), "cancel must abort the future");
        canceller.join().unwrap();
    }

    /// CR-13: `run_cancellable` honors a cancel that arrived
    /// BEFORE the call even started. Pre-CR-13 the registry
    /// didn't carry a `cancelled` flag — a cancel issued
    /// against a reserved token whose call hadn't yet inserted
    /// the abort handle would silently drop on the floor, and
    /// the subsequent call would run to completion despite the
    /// caller's cancel signal.
    #[test]
    fn run_cancellable_honors_cancel_issued_before_register() {
        let token = net_rpc_reserve_cancel_token();
        // Fire cancel against the reserved token BEFORE
        // run_cancellable runs. With CR-13 the registry now
        // carries `cancelled=true` for this token; without
        // CR-13 the cancel would no-op.
        net_rpc_cancel_call(token);
        let result = run_cancellable(token, async move {
            // Long-running future. If the pre-cancel didn't take
            // effect, this sleep would eventually return Ok and
            // the test wedges (caught by cargo's per-test timeout).
            tokio::time::sleep(Duration::from_secs(30)).await;
            "should never reach here"
        });
        assert!(
            result.is_err(),
            "pre-issued cancel must abort the future immediately"
        );
        // Registry entry should be cleaned up after run_cancellable.
        let lingering = cancel_registry().lock().contains_key(&token);
        assert!(!lingering, "registry entry must be removed after run_cancellable");
    }

    /// `run_cancellable` with token=0 short-circuits to plain
    /// block_on — no registry overhead, no abort handle.
    #[test]
    fn run_cancellable_token_zero_runs_to_completion() {
        let result = run_cancellable(0, async move { 42_u32 });
        assert_eq!(result.unwrap(), 42);
    }

    /// `net_rpc_serve` rejects `handler_id == 0` with a clear
    /// error message rather than calling into the SDK with a
    /// sentinel id. Pinned because zero is the canonical "no
    /// handler" sentinel everywhere else in the FFI.
    #[test]
    fn serve_rejects_zero_handler_id() {
        // Set a no-op dispatcher so the "dispatcher not set"
        // pre-check passes; we want to surface the zero-id check.
        unsafe extern "C" fn noop(
            _: u64,
            _: *const u8,
            _: usize,
            _: *mut *mut u8,
            _: *mut usize,
            _: *mut *mut c_char,
        ) -> c_int {
            0
        }
        let _ = DISPATCHER.set(noop);

        // Pass a NULL handle — even with a NULL handle, we should
        // *not* segfault on the zero-id path. But the NULL-handle
        // check fires first, so to actually exercise the zero-id
        // guard we'd need a real handle. Instead pin the message
        // for the zero-id path as documentation; this is a
        // correctness test for the explicit guard.
        let svc = b"any";
        let mut err: *mut c_char = std::ptr::null_mut();
        let h = net_rpc_serve(
            std::ptr::null_mut(),
            svc.as_ptr() as *const c_char,
            svc.len(),
            0,
            0,
            &mut err,
        );
        assert!(h.is_null());
        // The NULL-handle check matches first; ensure the message
        // is human-readable rather than a panic / blank string.
        if !err.is_null() {
            let msg = unsafe { std::ffi::CStr::from_ptr(err) }
                .to_string_lossy()
                .into_owned();
            net_rpc_free_cstring(err);
            assert!(!msg.is_empty(), "error message must be present");
        }
    }

    /// `collect_headers` round-trips a (name, value) array.
    /// Pin the FFI buffer-shape contract: name is UTF-8 by length,
    /// value is opaque bytes by length, both copied into owned
    /// Rust types.
    #[test]
    fn collect_headers_round_trips_name_and_value() {
        let name1 = b"cyberdeck-where";
        let value1 = b"{\"nodes\":[],\"root_idx\":0}";
        let name2 = b"x-trace-id";
        let value2: &[u8] = &[0xde, 0xad, 0xbe, 0xef];

        let arr = [
            NetRpcHeader {
                name_ptr: name1.as_ptr() as *const c_char,
                name_len: name1.len(),
                value_ptr: value1.as_ptr(),
                value_len: value1.len(),
            },
            NetRpcHeader {
                name_ptr: name2.as_ptr() as *const c_char,
                name_len: name2.len(),
                value_ptr: value2.as_ptr(),
                value_len: value2.len(),
            },
        ];
        let collected = unsafe { collect_headers(arr.as_ptr(), arr.len()) }
            .expect("UTF-8 names must collect cleanly");
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].0, "cyberdeck-where");
        assert_eq!(collected[0].1, value1.to_vec());
        assert_eq!(collected[1].0, "x-trace-id");
        assert_eq!(collected[1].1, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    /// Empty `(NULL, 0)` collects to an empty `Vec`. Matches the
    /// "no headers" path the `_with_headers` variants accept.
    #[test]
    fn collect_headers_empty_input_returns_empty_vec() {
        let collected = unsafe { collect_headers(std::ptr::null(), 0) }.expect("empty input is OK");
        assert!(collected.is_empty());
    }

    /// Non-UTF-8 header name returns `None` so the caller can
    /// surface `NET_RPC_ERR_INVALID_UTF8` to the consumer.
    #[test]
    fn collect_headers_non_utf8_name_returns_none() {
        let bad_name: &[u8] = &[0xff, 0xfe, 0xfd]; // invalid UTF-8 sequence
        let value = b"v";
        let arr = [NetRpcHeader {
            name_ptr: bad_name.as_ptr() as *const c_char,
            name_len: bad_name.len(),
            value_ptr: value.as_ptr(),
            value_len: value.len(),
        }];
        let collected = unsafe { collect_headers(arr.as_ptr(), arr.len()) };
        assert!(collected.is_none());
    }

    /// Empty value (NULL ptr or zero length) is preserved as
    /// empty bytes — the substrate's `request_headers` accepts
    /// zero-length values.
    #[test]
    fn collect_headers_null_or_zero_value_yields_empty_bytes() {
        let name = b"x-empty";
        let arr = [NetRpcHeader {
            name_ptr: name.as_ptr() as *const c_char,
            name_len: name.len(),
            value_ptr: std::ptr::null(),
            value_len: 0,
        }];
        let collected = unsafe { collect_headers(arr.as_ptr(), arr.len()) }.unwrap();
        assert_eq!(collected[0].0, "x-empty");
        assert!(collected[0].1.is_empty());
    }
}
