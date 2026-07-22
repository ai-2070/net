//! C ABI organization-capability-auth surface for the Go binding of Net
//! (OSDK-L Workstream C). Wraps `net_sdk::org` — the verb facade over the
//! closed OA substrate — so the Go binding (and any C consumer) reaches the
//! same two verbs, five concepts, and four error domains as the Rust, Node,
//! and Python SDKs.
//!
//! # Handle model (mirrors `rpc-ffi` / `compute-ffi`)
//!
//! Every Rust object that crosses the FFI boundary is a heap-allocated `Box`
//! handed to the caller as `*mut T`. Go owns the pointer and MUST call the
//! matching `_free` exactly once. The frees here take a **double pointer**
//! (`T**`) and NULL the caller's slot, so a Go finalizer racing an explicit
//! `Close()` cannot double-free — the honest contract §D7 chose over a free
//! that falsely claims idempotence.
//!
//! # The secret asymmetry (locked decision #1)
//!
//! Public signed credentials (membership, dispatcher, grants) cross as wire
//! **bytes**. The audience secret — the raw 32-byte discovery key — crosses
//! ONLY as a file **path**. There is deliberately no bytes variant: a key must
//! never enter a GC'd runtime's heap. `net_org_credentials_new` loads each
//! secret through the SDK's checked loader (`load_grant_audience_secret`),
//! which validates the opened object and zeroizes on drop, entirely in Rust.
//!
//! # Error codes
//!
//! `c_int` returns: `0` (`NET_ORG_OK`) on success, a negative
//! `NET_ORG_ERR_*` otherwise. The four CALL domains map to distinct codes
//! (`CREDENTIALS`/`DISCOVERY`/`ADMISSION_DENIED`/`RPC`) so a Go `errors.Is`
//! works without parsing; the full `org:<domain>:<kind>` wire string is written
//! to the out-param `char** out_err` for rich reconstruction. Provisioning
//! failures are their own code (`PROVISION`) — a node either starts or it does
//! not, so they are NOT one of the call domains (§D9).
//!
//! # Tokio runtime & handler bridging
//!
//! A lazy `OnceLock<Arc<Runtime>>` bridges the blocking FFI boundary into the
//! async SDK. Go registers one process-wide handler dispatcher
//! (`net_org_set_handler_dispatcher`, first-call-wins); the Rust serve path
//! invokes it with a `handler_id`, the verified `NetOrgCaller`, and the request
//! bytes, receiving a Go-`malloc`'d response the Rust side copies and frees via
//! `libc::free` — the same contract the sibling crates use.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::runtime::Runtime;

use net::adapter::net::mesh_rpc::{ServeError, ServeHandle};
use net::adapter::net::MeshNode;
use net_sdk::org::{
    install_org_authority_node, install_provider_grant_audience_node, OrgAccess, OrgCaller,
    OrgClient, OrgCredentials, OrgErrorDomain, OrgHandlerError, OrgSdkError,
};

// =========================================================================
// FFI guard — wraps every entry point in `catch_unwind`
// =========================================================================

/// Wrap an `extern "C"` body in `catch_unwind` so a panic can never unwind
/// across the C ABI boundary (which is undefined behavior). On panic the
/// diagnostic is logged via `eprintln!` (this crate has no thread-local
/// last-error channel) and `$default` is returned. Mirrors the `ffi_guard!`
/// macro in the sibling `rpc-ffi` / `compute-ffi` crates.
///
/// Defined ahead of every entry point because `macro_rules!` is textually
/// scoped — a use before the definition would not resolve.
macro_rules! ffi_guard {
    ($default:expr, $body:block) => {{
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| $body));
        match result {
            Ok(v) => v,
            Err(payload) => {
                let detail = if let Some(s) = payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "panic across FFI boundary".to_string()
                };
                eprintln!("net-org-ffi: caught panic across FFI boundary: {detail}");
                $default
            }
        }
    }};
}

// =========================================================================
// Error codes
// =========================================================================

/// Operation succeeded.
pub const NET_ORG_OK: c_int = 0;
/// Null or invalid pointer passed where a live handle / buffer was expected.
pub const NET_ORG_ERR_NULL: c_int = -1;
/// A `(ptr, len)` string argument was non-UTF-8 (e.g. service name, path).
pub const NET_ORG_ERR_INVALID_UTF8: c_int = -2;
/// CALL domain: the local credential set could not authorize the call —
/// assembly, binding, matching, or a validity window. Nothing was sent.
pub const NET_ORG_ERR_CREDENTIALS: c_int = -3;
/// CALL domain: no authorized, directly-reachable provider was found.
/// Nothing was sent.
pub const NET_ORG_ERR_DISCOVERY: c_int = -4;
/// CALL domain: the provider's admission engine refused the call. The
/// `out_err` wire carries only the coarse bucket (a precise reason would be a
/// credential oracle).
pub const NET_ORG_ERR_ADMISSION_DENIED: c_int = -5;
/// CALL domain: transport, or a non-admission server error.
pub const NET_ORG_ERR_RPC: c_int = -6;
/// The client handle is closed. Reserved for the Go wrapper's own sentinel —
/// the Rust side never returns it (a closed handle reaches here as NULL).
pub const NET_ORG_ERR_CLOSED: c_int = -7;
/// Parser / ABI fallback (§D5a). Reserved for the Go wrapper — the Rust side
/// never returns it, since it always emits a canonical domain.
pub const NET_ORG_ERR_UNCLASSIFIED: c_int = -8;
/// `net_org_set_handler_dispatcher` was never called before `net_org_serve`.
pub const NET_ORG_ERR_NO_DISPATCHER: c_int = -9;
/// `serve` rejected: a handler is already registered for this service on this
/// node.
pub const NET_ORG_ERR_ALREADY_SERVING: c_int = -10;
/// `serve` failed for a reason other than already-serving (bad service name,
/// missing node authority, …).
pub const NET_ORG_ERR_SERVE: c_int = -11;
/// A provisioning step (§D9) failed — installing the node authority or a
/// provider grant audience. NOT a call-domain result: a node either starts
/// correctly or it does not.
pub const NET_ORG_ERR_PROVISION: c_int = -12;

/// Map a canonical `OrgSdkError` domain onto its coarse `c_int` code. The full
/// `org:<domain>:<kind>` string still crosses via `out_err`; this exists so a
/// Go `errors.Is` on a sentinel works without parsing the wire.
fn org_error_code(e: &OrgSdkError) -> c_int {
    match e.domain() {
        OrgErrorDomain::Credentials => NET_ORG_ERR_CREDENTIALS,
        OrgErrorDomain::Discovery => NET_ORG_ERR_DISCOVERY,
        OrgErrorDomain::AdmissionDenied => NET_ORG_ERR_ADMISSION_DENIED,
        OrgErrorDomain::Rpc => NET_ORG_ERR_RPC,
        // Rust never produces `Unclassified`; it exists only for a binding
        // whose vocabulary disagrees with this build. Fold to RPC defensively
        // — it will never be reached.
        OrgErrorDomain::Unclassified => NET_ORG_ERR_UNCLASSIFIED,
    }
}

// =========================================================================
// ABI version stamp — independent of net_rpc's (locked decision #10).
// =========================================================================

/// ABI version of this cdylib. Starts at `0x0001` — a fresh stamp for the org
/// surface, versioned independently of `net_rpc`'s ABI. Bump on any
/// signature/layout change to a `net_org_*` symbol or `net_org_caller_t`.
pub const NET_ORG_ABI_VERSION: u32 = 0x0001;

/// Return the ABI version this cdylib was built with.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_abi_version() -> u32 {
    ffi_guard!(0, { NET_ORG_ABI_VERSION })
}

/// Returns `NET_ORG_OK` iff the loaded library is at least `expected`
/// (forward-compatible: a newer lib satisfies an older header). A consumer
/// pins the version its header was generated against at init and hard-fails on
/// mismatch. Returns `NET_ORG_ERR_NULL` if the loaded lib is older.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_check_abi_version(expected: u32) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        if NET_ORG_ABI_VERSION >= expected {
            NET_ORG_OK
        } else {
            NET_ORG_ERR_NULL
        }
    })
}

// =========================================================================
// Tokio runtime bridge.
// =========================================================================

/// Lazy process-wide runtime for blocking into the async SDK across the FFI
/// boundary. The mesh's own operations run on their own runtime; this one just
/// drives the boundary future.
fn runtime() -> &'static Arc<Runtime> {
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("net-org-ffi")
                .build()
                .expect("failed to construct org-ffi tokio runtime"),
        )
    })
}

/// `block_on(...)` wrapper that aborts on runtime-in-runtime rather than
/// panicking across the C ABI boundary. Calling `Runtime::block_on` from inside
/// a tokio context panics; that unwind would cross into Go — UB. The check
/// costs one TLS lookup per call.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "FATAL: org FFI called from inside a tokio runtime context; \
             aborting to avoid runtime-in-runtime panic across the FFI boundary"
        );
        std::process::abort();
    }
    runtime().block_on(future)
}

// =========================================================================
// Helpers.
// =========================================================================

/// Convert a `(ptr, len)` C buffer to a Rust `String`. `None` on null pointer
/// or non-UTF-8 bytes. Rejects `len > isize::MAX` (a `from_raw_parts`
/// precondition; `(size_t)-1` from C would be immediate UB).
fn cstr_to_string(ptr: *const c_char, len: usize) -> Option<String> {
    if ptr.is_null() || len > isize::MAX as usize {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

/// Copy a REQUIRED `(ptr, len)` byte buffer into an owned `Vec`. `None` on a
/// null pointer (the argument is mandatory) or `len > isize::MAX`.
///
/// # Safety
///
/// `ptr` must point to at least `len` readable bytes when non-NULL.
unsafe fn copy_bytes_required(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
    if ptr.is_null() || len > isize::MAX as usize {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

/// Copy an OPTIONAL `(ptr, len)` request body into `Bytes`, treating a NULL
/// pointer as the empty body. `None` on `len > isize::MAX`.
///
/// # Safety
///
/// `ptr` must point to at least `len` readable bytes when non-NULL.
unsafe fn copy_body(ptr: *const u8, len: usize) -> Option<Bytes> {
    if ptr.is_null() {
        return Some(Bytes::new());
    }
    if len > isize::MAX as usize {
        return None;
    }
    Some(Bytes::copy_from_slice(unsafe {
        std::slice::from_raw_parts(ptr, len)
    }))
}

/// Set `*out_err` to a heap-allocated CString containing `message`. Caller
/// frees via [`net_org_free_cstring`]. No-op if `out_err` is NULL. Never
/// carries credential material — the callers pass either an `OrgSdkError`
/// wire string (which renders only ids / a coarse bucket) or a plain message.
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

/// Hand a response body out to C via `Box<[u8]>::into_raw` — layout `(ptr, len)`
/// with `cap == len`, so [`net_org_response_free`] reconstructs it exactly. No
/// `shrink_to_fit` best-effort hazard.
fn write_response(body: Vec<u8>, out_ptr: *mut *mut u8, out_len: *mut usize) {
    if out_ptr.is_null() || out_len.is_null() {
        return;
    }
    let boxed: Box<[u8]> = body.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    unsafe {
        *out_ptr = ptr;
        *out_len = len;
    }
}

/// Free a CString returned out-of-band by this crate (an `out_err` message).
/// Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_free_cstring(s: *mut c_char) {
    ffi_guard!((), {
        if s.is_null() {
            return;
        }
        unsafe {
            let _ = CString::from_raw(s);
        }
    })
}

/// Free a response-body buffer returned via `out_resp_ptr` (from
/// [`net_org_call`]). Idempotent on NULL or zero length. Pass the SAME `len`
/// received. Same `Box<[u8]>` layout discipline as `write_response`.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_response_free(ptr: *mut u8, len: usize) {
    ffi_guard!((), {
        if ptr.is_null() || len == 0 {
            return;
        }
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
        }
    })
}

// =========================================================================
// net_org_caller_t — the verified admission facts (R4).
// =========================================================================

/// `#[repr(C)]` projection of the SDK's `OrgCaller` — five 32-byte ids, in the
/// canonical order. The FFI copies each id out through its public byte
/// accessor; the Rust `OrgCaller` type is unchanged (its memory layout is not
/// an ABI concern — locked decision #6d). Declared in the C header as
/// `net_org_caller_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NetOrgCaller {
    /// The acting entity — the caller S.
    pub caller: [u8; 32],
    /// The organization S acted for.
    pub acting_org: [u8; 32],
    /// This provider's owner organization.
    pub provider_org: [u8; 32],
    /// This exact provider node.
    pub provider: [u8; 32],
    /// The capability that was invoked.
    pub capability: [u8; 32],
}

impl From<&OrgCaller> for NetOrgCaller {
    fn from(c: &OrgCaller) -> Self {
        Self {
            caller: *c.entity.as_bytes(),
            acting_org: *c.acting_org.as_bytes(),
            provider_org: *c.provider_org.as_bytes(),
            provider: *c.provider.as_bytes(),
            capability: *c.capability.as_bytes(),
        }
    }
}

// =========================================================================
// Handler dispatch (serve).
// =========================================================================

/// The Go-side handler dispatcher. Rust calls this when an admitted request
/// lands: it receives the reserved `handler_id`, the verified caller, and the
/// request bytes, and returns a Go-`malloc`'d `(out_resp_ptr, out_resp_len)`
/// on success or writes `out_err` and returns non-zero on failure.
pub type OrgHandlerFn = unsafe extern "C" fn(
    handler_id: u64,
    caller: *const NetOrgCaller,
    req_ptr: *const u8,
    req_len: usize,
    out_resp_ptr: *mut *mut u8,
    out_resp_len: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int;

/// Process-wide handler dispatcher, first-call-wins (mirrors
/// `net_rpc_set_handler_dispatcher`).
static ORG_DISPATCHER: OnceLock<OrgHandlerFn> = OnceLock::new();

/// Monotonic handler-id counter. Starts at 1 so `0` is the "unreserved"
/// sentinel. IDs are never reused; an unused reservation is harmless.
static NEXT_ORG_HANDLER_ID: AtomicU64 = AtomicU64::new(1);

/// Default per-handler wait cap. A wedged Go callback would otherwise hold the
/// in-flight slot indefinitely.
const DEFAULT_ORG_HANDLER_TIMEOUT: Duration = Duration::from_secs(60);

/// The stable `nrpc:app_error:0x<code>:<body>` prefix — shared with the nRPC
/// typed layer, so a Go org handler returning `AppError(code, body)` surfaces
/// as an `OrgHandlerError::Application`, never a flattened internal error.
const GO_APP_ERROR_PREFIX: &str = "nrpc:app_error:";

/// Parse `nrpc:app_error:0x<code>:<body>` into `(code, body)`. `None` if the
/// prefix is absent or the format is malformed.
fn parse_go_app_error(message: &str) -> Option<(u16, String)> {
    let rest = message.strip_prefix(GO_APP_ERROR_PREFIX)?;
    let (code_str, body) = rest.split_once(':')?;
    let code_hex = code_str
        .strip_prefix("0x")
        .or(code_str.strip_prefix("0X"))?;
    let code = u16::from_str_radix(code_hex, 16).ok()?;
    Some((code, body.to_string()))
}

/// Convert a Go-side handler error string into a typed `Application` (when it
/// carries the app-error prefix) or a generic `Internal` `OrgHandlerError`.
fn org_handler_error_from_msg(msg: String) -> OrgHandlerError {
    if let Some((code, message)) = parse_go_app_error(&msg) {
        return OrgHandlerError::Application { code, message };
    }
    OrgHandlerError::Internal(msg)
}

/// Register the process-wide org handler dispatcher. Idempotent — only the
/// first call takes effect.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_set_handler_dispatcher(dispatcher: OrgHandlerFn) {
    ffi_guard!((), {
        let _ = ORG_DISPATCHER.set(dispatcher);
    })
}

/// Reserve the next handler id without registering anything. The Go side
/// reserves an id, stores its callable under that id, THEN calls
/// [`net_org_serve`] — closing the request-arrives-before-store race.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_reserve_handler_id() -> u64 {
    ffi_guard!(0, { NEXT_ORG_HANDLER_ID.fetch_add(1, Ordering::Relaxed) })
}

/// Drive one admitted request through the Go dispatcher on a blocking thread,
/// bounded by `timeout`. This is the closure body `serve_org_bytes_node`
/// registers — the single place org admission facts are projected to C and a
/// handler failure is classified.
async fn org_dispatch(
    handler_id: u64,
    caller: NetOrgCaller,
    body: Bytes,
    timeout: Duration,
) -> Result<Bytes, OrgHandlerError> {
    let dispatcher = match ORG_DISPATCHER.get() {
        Some(d) => *d,
        None => {
            return Err(OrgHandlerError::Internal(
                "net_org_set_handler_dispatcher never called".into(),
            ));
        }
    };

    let join = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let mut resp_ptr: *mut u8 = std::ptr::null_mut();
            let mut resp_len: usize = 0;
            let mut err_ptr: *mut c_char = std::ptr::null_mut();
            let code = unsafe {
                dispatcher(
                    handler_id,
                    &caller as *const NetOrgCaller,
                    body.as_ptr(),
                    body.len(),
                    &mut resp_ptr,
                    &mut resp_len,
                    &mut err_ptr,
                )
            };
            if code == NET_ORG_OK {
                if resp_ptr.is_null() {
                    return Ok(Vec::new());
                }
                if resp_len > isize::MAX as usize {
                    unsafe { libc::free(resp_ptr as *mut libc::c_void) };
                    return Err("Go org handler response length exceeds isize::MAX".to_string());
                }
                // Copy the Go-`malloc`'d bytes into a Rust-owned Vec, then free
                // the Go buffer (Go allocates via C.malloc; we release via the
                // matching free).
                let bytes = unsafe { std::slice::from_raw_parts(resp_ptr, resp_len).to_vec() };
                unsafe { libc::free(resp_ptr as *mut libc::c_void) };
                Ok(bytes)
            } else {
                let msg = if err_ptr.is_null() {
                    format!("Go org handler returned code {code} with no error message")
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

    match join {
        Ok(Ok(Ok(body))) => Ok(Bytes::from(body)),
        Ok(Ok(Err(msg))) => Err(org_handler_error_from_msg(msg)),
        Ok(Err(join_err)) => Err(OrgHandlerError::Internal(format!(
            "Go org handler blocking task panicked: {join_err}"
        ))),
        Err(_) => Err(OrgHandlerError::Internal(format!(
            "Go org handler did not respond within {} ms",
            timeout.as_millis()
        ))),
    }
}

// =========================================================================
// OrgCredentials — public bytes + audience-secret PATHS.
// =========================================================================

/// Opaque wrapper around the SDK's validated `OrgCredentials`.
pub struct NetOrgCredentials {
    inner: OrgCredentials,
}

/// Build a validated credential set from public credential BYTES plus
/// audience-secret file PATHS (§D2). There is deliberately no bytes variant
/// for the secret — the raw discovery key never crosses as a buffer.
///
/// On success `*out_creds` receives an owned handle (free with
/// [`net_org_credentials_free`], or consume via [`net_org_bind`]). On failure
/// returns `NET_ORG_ERR_CREDENTIALS` (or `NET_ORG_ERR_NULL` /
/// `NET_ORG_ERR_INVALID_UTF8` for malformed input) and writes the
/// `org:credentials:<kind>` wire to `out_err`.
///
/// Signature verification, binding checks, and grant/secret pairing all run
/// here (in Rust) — the C consumer never sees a key.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_org_credentials_new(
    membership_ptr: *const u8,
    membership_len: usize,
    dispatcher_ptr: *const u8,
    dispatcher_len: usize,
    grant_ptrs: *const *const u8,
    grant_lens: *const usize,
    grant_count: usize,
    audience_secret_paths: *const *const c_char,
    audience_secret_count: usize,
    out_creds: *mut *mut NetOrgCredentials,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        if out_creds.is_null() {
            return NET_ORG_ERR_NULL;
        }
        let Some(membership) = (unsafe { copy_bytes_required(membership_ptr, membership_len) })
        else {
            write_err(out_err, "membership bytes are NULL or oversized".into());
            return NET_ORG_ERR_NULL;
        };
        let Some(dispatcher) = (unsafe { copy_bytes_required(dispatcher_ptr, dispatcher_len) })
        else {
            write_err(
                out_err,
                "dispatcher-grant bytes are NULL or oversized".into(),
            );
            return NET_ORG_ERR_NULL;
        };

        // Read the grant byte arrays.
        let mut grants: Vec<Vec<u8>> = Vec::with_capacity(grant_count);
        if grant_count > 0 {
            if grant_ptrs.is_null() || grant_lens.is_null() {
                write_err(out_err, "grant array pointers are NULL".into());
                return NET_ORG_ERR_NULL;
            }
            // `from_raw_parts` requires `count * size_of::<elem>() <= isize::MAX`;
            // the elements here are pointer-sized. Go bounds this via `len(..)`,
            // but a raw-C caller must not overflow it.
            if grant_count > isize::MAX as usize / std::mem::size_of::<*const u8>() {
                write_err(out_err, "grant array count exceeds isize::MAX".into());
                return NET_ORG_ERR_NULL;
            }
            let ptrs = unsafe { std::slice::from_raw_parts(grant_ptrs, grant_count) };
            let lens = unsafe { std::slice::from_raw_parts(grant_lens, grant_count) };
            for (p, l) in ptrs.iter().zip(lens.iter()) {
                let Some(bytes) = (unsafe { copy_bytes_required(*p, *l) }) else {
                    write_err(out_err, "a grant byte pointer is NULL or oversized".into());
                    return NET_ORG_ERR_NULL;
                };
                grants.push(bytes);
            }
        }

        // Read the audience-secret PATHS (never bytes).
        let mut paths: Vec<PathBuf> = Vec::with_capacity(audience_secret_count);
        if audience_secret_count > 0 {
            if audience_secret_paths.is_null() {
                write_err(out_err, "audience-secret path array is NULL".into());
                return NET_ORG_ERR_NULL;
            }
            // See the grant-array bound above — pointer-sized elements.
            if audience_secret_count > isize::MAX as usize / std::mem::size_of::<*const c_char>() {
                write_err(out_err, "audience-secret path count exceeds isize::MAX".into());
                return NET_ORG_ERR_NULL;
            }
            let path_ptrs =
                unsafe { std::slice::from_raw_parts(audience_secret_paths, audience_secret_count) };
            for p in path_ptrs {
                if p.is_null() {
                    write_err(out_err, "an audience-secret path is NULL".into());
                    return NET_ORG_ERR_NULL;
                }
                let cstr = unsafe { std::ffi::CStr::from_ptr(*p) };
                let Ok(s) = cstr.to_str() else {
                    write_err(out_err, "an audience-secret path is not UTF-8".into());
                    return NET_ORG_ERR_INVALID_UTF8;
                };
                paths.push(PathBuf::from(s));
            }
        }

        match OrgCredentials::from_parts(&membership, &dispatcher, &grants, &paths) {
            Ok(creds) => {
                unsafe {
                    *out_creds = Box::into_raw(Box::new(NetOrgCredentials { inner: creds }));
                }
                NET_ORG_OK
            }
            Err(e) => {
                // Render through the canonical wire so the domain + kind cross
                // intact (`org:credentials:<kind>`).
                write_err(out_err, OrgSdkError::from(e).to_wire());
                NET_ORG_ERR_CREDENTIALS
            }
        }
    })
}

/// Free an unconsumed credential handle. Double pointer: NULLs `*credentials`.
/// Idempotent on NULL or a pointer to NULL. A handle consumed by
/// [`net_org_bind`] is already NULL, so this is a no-op after a bind.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_credentials_free(credentials: *mut *mut NetOrgCredentials) {
    ffi_guard!((), {
        if credentials.is_null() {
            return;
        }
        let p = unsafe { *credentials };
        if p.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(p));
            *credentials = std::ptr::null_mut();
        }
    })
}

// =========================================================================
// OrgClient — bind, call, close.
// =========================================================================

/// Opaque wrapper around the SDK's `OrgClient`. Dropping it releases the
/// consumer-audience lease (via `OrgClient`'s `Drop`), which is the withdrawal
/// step: while a client is un-closed the node keeps ingest authority for its
/// grants.
pub struct NetOrgClient {
    inner: OrgClient,
}

/// Bind a credential set to a mesh node, producing a client (§1).
///
/// `mesh_arc` MUST come from `net_mesh_arc_clone`; it is **consumed** here (the
/// same ownership transfer `net_rpc_new` makes) — the caller mints a fresh
/// clone per bind and MUST NOT free it. The node itself lives on via the Go
/// `MeshNode`'s own Arc; a failed bind simply drops this clone.
///
/// `credentials` is **consumed unconditionally**: `OrgClient::bind_node` takes
/// them by value, so there is no non-consuming failure path. On return
/// `*credentials` is set to NULL on BOTH success and failure — a Go finalizer
/// can therefore never double-free a consumed handle. (A failed bind means the
/// credentials do not match this node's identity/authority; retrying with the
/// same set would not succeed, so consuming them costs nothing.)
///
/// On success `*out_client` receives an owned handle (free with
/// [`net_org_client_free`]). On failure returns the credential-domain code and
/// writes the `org:credentials:<kind>` wire to `out_err`.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_bind(
    mesh_arc: *mut Arc<MeshNode>,
    credentials: *mut *mut NetOrgCredentials,
    out_client: *mut *mut NetOrgClient,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        if mesh_arc.is_null() || credentials.is_null() || out_client.is_null() {
            return NET_ORG_ERR_NULL;
        }
        // Take ownership of the mesh arc IMMEDIATELY (Go does not free it), so
        // any validation early-return below drops the node rather than leaking
        // the whole handle. Mirrors `net_rpc_new`.
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let creds_ptr = unsafe { *credentials };
        if creds_ptr.is_null() {
            write_err(out_err, "credentials handle is NULL".into());
            return NET_ORG_ERR_NULL;
        }
        // Consume the credentials box now and NULL the caller's slot — the bind
        // takes them by value regardless of outcome.
        let creds_box: Box<NetOrgCredentials> = unsafe { Box::from_raw(creds_ptr) };
        unsafe {
            *credentials = std::ptr::null_mut();
        }

        match OrgClient::bind_node(node, creds_box.inner) {
            Ok(client) => {
                unsafe {
                    *out_client = Box::into_raw(Box::new(NetOrgClient { inner: client }));
                }
                NET_ORG_OK
            }
            Err(e) => {
                let code = org_error_code(&e);
                write_err(out_err, e.to_wire());
                code
            }
        }
    })
}

/// Close the client — releases the consumer-audience lease, frees the handle,
/// and NULLs `*client`. Double pointer so a finalizer racing an explicit close
/// cannot double-free. Idempotent on NULL or a pointer to NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_client_free(client: *mut *mut NetOrgClient) {
    ffi_guard!((), {
        if client.is_null() {
            return;
        }
        let p = unsafe { *client };
        if p.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(p));
            *client = std::ptr::null_mut();
        }
    })
}

/// Call a protected service (§2). Bytes in, bytes out — the caller marshals
/// (JSON is the Go typed layer's codec).
///
/// `deadline_ms == 0` means the facade default; a positive value is a hard
/// deadline. `cancel_token == 0` means uncancellable; a non-zero token
/// (reserved via [`net_org_reserve_cancel_token`]) lets a concurrent
/// [`net_org_cancel_call`] drop the one in-flight future. Neither is an
/// authorization input.
///
/// On success writes `(out_resp_ptr, out_resp_len)` (free with
/// [`net_org_response_free`]) and returns `NET_ORG_OK`. On failure returns the
/// domain code and writes the `org:<domain>:<kind>` wire to `out_err`.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_org_call(
    client: *mut NetOrgClient,
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
    ffi_guard!(NET_ORG_ERR_NULL, {
        let Some(h) = (unsafe { client.as_ref() }) else {
            return NET_ORG_ERR_NULL;
        };
        let Some(service) = cstr_to_string(service_ptr, service_len) else {
            write_err(out_err, "service name is NULL or non-UTF-8".into());
            return NET_ORG_ERR_INVALID_UTF8;
        };
        let Some(req) = (unsafe { copy_body(req_ptr, req_len) }) else {
            write_err(out_err, "request body length exceeds isize::MAX".into());
            return NET_ORG_ERR_NULL;
        };

        let result = block_on(async {
            h.inner
                .call_bytes_deadline(&service, req, deadline_ms, cancel_token)
                .await
        });

        match result {
            Ok(body) => {
                write_response(body.to_vec(), out_resp_ptr, out_resp_len);
                NET_ORG_OK
            }
            Err(e) => {
                let code = org_error_code(&e);
                write_err(out_err, e.to_wire());
                code
            }
        }
    })
}

/// Reserve a cancel token scoped to this client's node, for a subsequent
/// cancellable [`net_org_call`]. Reserve BEFORE the call so a cancel that races
/// registration is still delivered. Returns `0` if `client` is NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_reserve_cancel_token(client: *mut NetOrgClient) -> u64 {
    ffi_guard!(0, {
        let Some(h) = (unsafe { client.as_ref() }) else {
            return 0;
        };
        h.inner.reserve_cancel_token()
    })
}

/// Drop the ONE in-flight call bound to `token`. Idempotent; no-op on `0`, a
/// NULL client, or an unknown token. Never launches a second attempt — a signed
/// proof is never resent (the facade's no-retry rule).
#[unsafe(no_mangle)]
pub extern "C" fn net_org_cancel_call(client: *mut NetOrgClient, cancel_token: u64) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        let Some(h) = (unsafe { client.as_ref() }) else {
            return NET_ORG_ERR_NULL;
        };
        h.inner.cancel(cancel_token);
        NET_ORG_OK
    })
}

// =========================================================================
// serve — register a protected handler.
// =========================================================================

/// `OrgAccess::SameOrg` — members of THIS node's own organization.
pub const NET_ORG_ACCESS_SAME_ORG: c_int = 0;
/// `OrgAccess::Granted` — members of another org holding a capability grant.
pub const NET_ORG_ACCESS_GRANTED: c_int = 1;

/// Opaque ServeHandle. Wraps the SDK `ServeHandle` in `Arc<Mutex<Option<..>>>`
/// so `close()` drops deterministically and a later `_free` is a no-op when
/// already closed.
pub struct NetOrgServeHandle {
    inner: Arc<Mutex<Option<ServeHandle>>>,
    handler_id: u64,
}

/// Register a protected service (§4). `mesh_arc` is **consumed** (a fresh clone
/// per call, as `net_rpc_new`; Go must NOT free it — the node lives on via the
/// Go `MeshNode`). `access` is `NET_ORG_ACCESS_SAME_ORG` / `_GRANTED`.
/// `handler_id` MUST already be reserved via [`net_org_reserve_handler_id`] AND
/// stored in the Go registry before this call — pre-registration is the
/// load-bearing invariant.
///
/// On success `*out_handle` receives an owned handle (free with
/// [`net_org_serve_handle_free`]). On failure returns `NET_ORG_ERR_SERVE`
/// (or `NET_ORG_ERR_ALREADY_SERVING` / `NET_ORG_ERR_NO_DISPATCHER`) and writes
/// a message to `out_err`. Requires an installed node authority (§D9).
#[unsafe(no_mangle)]
pub extern "C" fn net_org_serve(
    mesh_arc: *mut Arc<MeshNode>,
    service_ptr: *const c_char,
    service_len: usize,
    access: c_int,
    handler_id: u64,
    out_handle: *mut *mut NetOrgServeHandle,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        if mesh_arc.is_null() || out_handle.is_null() {
            return NET_ORG_ERR_NULL;
        }
        // Own the mesh arc immediately (Go does not free it) so the validation
        // early-returns below drop the node rather than leaking it.
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let Some(service) = cstr_to_string(service_ptr, service_len) else {
            write_err(out_err, "service name is NULL or non-UTF-8".into());
            return NET_ORG_ERR_INVALID_UTF8;
        };
        if ORG_DISPATCHER.get().is_none() {
            write_err(
                out_err,
                "net_org_set_handler_dispatcher must be called before net_org_serve".into(),
            );
            return NET_ORG_ERR_NO_DISPATCHER;
        }
        if handler_id == 0 {
            write_err(
                out_err,
                "handler_id must be non-zero (reserve via net_org_reserve_handler_id)".into(),
            );
            return NET_ORG_ERR_NULL;
        }
        let access = match access {
            NET_ORG_ACCESS_SAME_ORG => OrgAccess::SameOrg,
            NET_ORG_ACCESS_GRANTED => OrgAccess::Granted,
            other => {
                write_err(out_err, format!("invalid access mode {other}"));
                return NET_ORG_ERR_NULL;
            }
        };

        let timeout = DEFAULT_ORG_HANDLER_TIMEOUT;
        let handler = move |caller: OrgCaller, body: Bytes| {
            let caller_c = NetOrgCaller::from(&caller);
            async move { org_dispatch(handler_id, caller_c, body, timeout).await }
        };

        // `serve_org_bytes_node` -> `serve_rpc_*` spawns an inbound-event bridge
        // task with a bare `tokio::spawn`, which needs an ambient runtime. This
        // FFI entry point is called on a Go-owned C thread with no runtime, so
        // enter ours for the registration (the bridge task then runs on it). The
        // SDK tests never hit this because they serve inside `#[tokio::test]`.
        let _rt_guard = runtime().enter();
        match net_sdk::org::serve_org_bytes_node(node, &service, access, handler) {
            Ok(inner) => {
                unsafe {
                    *out_handle = Box::into_raw(Box::new(NetOrgServeHandle {
                        inner: Arc::new(Mutex::new(Some(inner))),
                        handler_id,
                    }));
                }
                NET_ORG_OK
            }
            Err(e) => {
                let code = match e {
                    ServeError::AlreadyServing(_) => NET_ORG_ERR_ALREADY_SERVING,
                    _ => NET_ORG_ERR_SERVE,
                };
                write_err(out_err, format!("serve failed: {e}"));
                code
            }
        }
    })
}

/// Diagnostic accessor: the handler_id of this ServeHandle. `0` on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_serve_handle_id(handle: *const NetOrgServeHandle) -> u64 {
    ffi_guard!(0, {
        let Some(h) = (unsafe { handle.as_ref() }) else {
            return 0;
        };
        h.handler_id
    })
}

/// Unregister the service. Idempotent — in-flight handlers continue but no new
/// requests are dispatched. No-op on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_serve_handle_close(handle: *mut NetOrgServeHandle) {
    ffi_guard!((), {
        let Some(h) = (unsafe { handle.as_ref() }) else {
            return;
        };
        let _ = h.inner.lock().take();
    })
}

/// Free the ServeHandle (implicitly closing if needed). Double pointer: NULLs
/// `*handle`. Idempotent on NULL or a pointer to NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_serve_handle_free(handle: *mut *mut NetOrgServeHandle) {
    ffi_guard!((), {
        if handle.is_null() {
            return;
        }
        let p = unsafe { *handle };
        if p.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(p));
            *handle = std::ptr::null_mut();
        }
    })
}

// =========================================================================
// Provisioning (§D9) — install the node authority + provider grant audiences.
// =========================================================================

/// Install an adopted node authority from the directory `net node adopt` wrote
/// (§D9). Required before `net_org_bind` can succeed or a granted service can
/// serve. `mesh_arc` is **consumed** (a fresh clone per call; Go must NOT free
/// it). The install mutates the node's shared interior state, so it is visible
/// through the Go `MeshNode` after this clone drops. `dir` is a `(ptr, len)`
/// path.
///
/// This is node **startup** — distinct from adoption (mints files; CLI-only)
/// and issuance (mints certs; CLI-only). A provisioning failure returns
/// `NET_ORG_ERR_PROVISION` with a plain message on `out_err`; it is NOT a
/// call-domain result.
#[unsafe(no_mangle)]
pub extern "C" fn net_org_install_authority(
    mesh_arc: *mut Arc<MeshNode>,
    dir_ptr: *const c_char,
    dir_len: usize,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        if mesh_arc.is_null() {
            return NET_ORG_ERR_NULL;
        }
        // Own the mesh arc immediately (Go does not free it) so a non-UTF-8 dir
        // early-return drops the node rather than leaking it.
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let Some(dir) = cstr_to_string(dir_ptr, dir_len) else {
            write_err(out_err, "authority dir is NULL or non-UTF-8".into());
            return NET_ORG_ERR_INVALID_UTF8;
        };
        match install_org_authority_node(&node, std::path::Path::new(&dir)) {
            Ok(()) => NET_ORG_OK,
            Err(e) => {
                write_err(out_err, e.to_string());
                NET_ORG_ERR_PROVISION
            }
        }
    })
}

/// Install a PROVIDER grant audience so a granted service can seal envelopes
/// (§D9). The grant crosses as wire **bytes**; its secret crosses as a **path**
/// — the same asymmetry credentials use, for the same reason. `mesh_arc` is
/// **consumed** (a fresh clone per call; Go must NOT free it). Failure returns
/// `NET_ORG_ERR_PROVISION` with a plain message.
///
/// A `SameOrg` provider does NOT need this (it seals under the owner audience
/// the authority carries); only a `Granted` provider does.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn net_org_install_provider_grant_audience(
    mesh_arc: *mut Arc<MeshNode>,
    grant_ptr: *const u8,
    grant_len: usize,
    secret_path_ptr: *const c_char,
    secret_path_len: usize,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_ORG_ERR_NULL, {
        if mesh_arc.is_null() {
            return NET_ORG_ERR_NULL;
        }
        // Own the mesh arc immediately (Go does not free it) so the grant/secret
        // validation early-returns below drop the node rather than leaking it.
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let Some(grant) = (unsafe { copy_bytes_required(grant_ptr, grant_len) }) else {
            write_err(out_err, "grant bytes are NULL or oversized".into());
            return NET_ORG_ERR_NULL;
        };
        let Some(secret_path) = cstr_to_string(secret_path_ptr, secret_path_len) else {
            write_err(out_err, "secret path is NULL or non-UTF-8".into());
            return NET_ORG_ERR_INVALID_UTF8;
        };
        match install_provider_grant_audience_node(
            &node,
            &grant,
            std::path::Path::new(&secret_path),
        ) {
            Ok(()) => NET_ORG_OK,
            Err(e) => {
                write_err(out_err, e.to_string());
                NET_ORG_ERR_PROVISION
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `net_org_caller_t` is exactly five 32-byte ids in canonical order — the
    /// C ABI's layout. A field reordered / added / retyped shifts these and Go
    /// would read admission facts at the wrong offsets with no compile error.
    #[test]
    fn net_org_caller_layout_is_five_32_byte_ids() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<NetOrgCaller>(), 160, "5 x 32 bytes, no padding");
        assert_eq!(align_of::<NetOrgCaller>(), 1, "byte arrays — no alignment");
        assert_eq!(offset_of!(NetOrgCaller, caller), 0);
        assert_eq!(offset_of!(NetOrgCaller, acting_org), 32);
        assert_eq!(offset_of!(NetOrgCaller, provider_org), 64);
        assert_eq!(offset_of!(NetOrgCaller, provider), 96);
        assert_eq!(offset_of!(NetOrgCaller, capability), 128);
    }

    /// The ABI stamp is the org surface's own, starting at `0x0001`, and the
    /// checker is forward-compatible (a newer lib satisfies an older header).
    #[test]
    fn abi_version_is_independent_and_forward_compatible() {
        assert_eq!(net_org_abi_version(), 0x0001);
        assert_eq!(net_org_check_abi_version(0x0001), NET_ORG_OK);
        assert_eq!(net_org_check_abi_version(0x0002), NET_ORG_ERR_NULL);
    }

    /// Malformed credential bytes are refused through the real entrypoint, and
    /// the refusal crosses as the canonical `org:credentials:signature_invalid`
    /// wire — proving signature verification runs across the boundary (156/185
    /// are the exact wire lengths, so this is not a length check).
    #[test]
    fn credentials_new_refuses_unsigned_bytes_with_org_wire() {
        let membership = [0u8; 156];
        let dispatcher = [0u8; 185];
        let mut out: *mut NetOrgCredentials = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        let code = net_org_credentials_new(
            membership.as_ptr(),
            membership.len(),
            dispatcher.as_ptr(),
            dispatcher.len(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            &mut out,
            &mut err,
        );
        assert_eq!(code, NET_ORG_ERR_CREDENTIALS);
        assert!(out.is_null(), "no handle on failure");
        assert!(!err.is_null(), "an error wire was written");
        let wire = unsafe { std::ffi::CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned();
        assert!(
            wire.starts_with("org:credentials:signature_invalid"),
            "expected canonical credentials wire, got {wire:?}"
        );
        net_org_free_cstring(err);
    }

    /// A NULL secret PATH pointer is refused, and there is no bytes sibling —
    /// the audience secret can never cross as a buffer (locked decision #1).
    #[test]
    fn credentials_new_rejects_a_null_secret_path() {
        let membership = [0u8; 156];
        let dispatcher = [0u8; 185];
        let null_path: *const c_char = std::ptr::null();
        let mut out: *mut NetOrgCredentials = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        let code = net_org_credentials_new(
            membership.as_ptr(),
            membership.len(),
            dispatcher.as_ptr(),
            dispatcher.len(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &null_path as *const *const c_char,
            1,
            &mut out,
            &mut err,
        );
        assert_eq!(code, NET_ORG_ERR_NULL);
        assert!(out.is_null());
        if !err.is_null() {
            net_org_free_cstring(err);
        }
    }

    /// The domain → code mapping is total and distinct across the four call
    /// domains, so a Go `errors.Is` on a sentinel is unambiguous.
    #[test]
    fn error_codes_are_distinct_per_domain() {
        let codes = [
            NET_ORG_ERR_CREDENTIALS,
            NET_ORG_ERR_DISCOVERY,
            NET_ORG_ERR_ADMISSION_DENIED,
            NET_ORG_ERR_RPC,
        ];
        let distinct: std::collections::HashSet<c_int> = codes.into_iter().collect();
        assert_eq!(distinct.len(), 4, "call-domain codes must be distinct");
    }

    /// The hand-written `include/net_org.h` numeric contract (error codes,
    /// access modes, ABI stamp) must match the Rust `pub const`s. This is the
    /// standalone-header drift guard — `net_org.h` is not part of
    /// `header_parity_test.go` (it is its own cdylib header, like `net_rpc.h`),
    /// so this mirror test is what keeps the two in sync. The
    /// `tests/transport_error_codes.rs` precedent, applied to org.
    #[test]
    fn header_numeric_contract_matches_rust() {
        fn parse_value(tok: &str) -> Option<i64> {
            if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).ok()
            } else {
                tok.parse::<i64>().ok()
            }
        }

        let header = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../include/net_org.h"
        ))
        .expect("read include/net_org.h");

        let mut defines = std::collections::HashMap::new();
        for line in header.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("#define NET_ORG_") else {
                continue;
            };
            let mut toks = rest.split_whitespace();
            let (Some(name), Some(val)) = (toks.next(), toks.next()) else {
                continue;
            };
            if let Some(v) = parse_value(val) {
                defines.insert(format!("NET_ORG_{name}"), v);
            }
        }

        let want: &[(&str, i64)] = &[
            ("NET_ORG_ABI_VERSION", NET_ORG_ABI_VERSION as i64),
            ("NET_ORG_OK", NET_ORG_OK as i64),
            ("NET_ORG_ERR_NULL", NET_ORG_ERR_NULL as i64),
            ("NET_ORG_ERR_INVALID_UTF8", NET_ORG_ERR_INVALID_UTF8 as i64),
            ("NET_ORG_ERR_CREDENTIALS", NET_ORG_ERR_CREDENTIALS as i64),
            ("NET_ORG_ERR_DISCOVERY", NET_ORG_ERR_DISCOVERY as i64),
            (
                "NET_ORG_ERR_ADMISSION_DENIED",
                NET_ORG_ERR_ADMISSION_DENIED as i64,
            ),
            ("NET_ORG_ERR_RPC", NET_ORG_ERR_RPC as i64),
            ("NET_ORG_ERR_CLOSED", NET_ORG_ERR_CLOSED as i64),
            ("NET_ORG_ERR_UNCLASSIFIED", NET_ORG_ERR_UNCLASSIFIED as i64),
            (
                "NET_ORG_ERR_NO_DISPATCHER",
                NET_ORG_ERR_NO_DISPATCHER as i64,
            ),
            (
                "NET_ORG_ERR_ALREADY_SERVING",
                NET_ORG_ERR_ALREADY_SERVING as i64,
            ),
            ("NET_ORG_ERR_SERVE", NET_ORG_ERR_SERVE as i64),
            ("NET_ORG_ERR_PROVISION", NET_ORG_ERR_PROVISION as i64),
            ("NET_ORG_ACCESS_SAME_ORG", NET_ORG_ACCESS_SAME_ORG as i64),
            ("NET_ORG_ACCESS_GRANTED", NET_ORG_ACCESS_GRANTED as i64),
        ];
        for (name, val) in want {
            let got = defines
                .get(*name)
                .unwrap_or_else(|| panic!("include/net_org.h is missing #define {name}"));
            assert_eq!(
                got, val,
                "drift between net_org.h and Rust on {name}: header {got}, rust {val}"
            );
        }
    }

    /// A Go handler's `nrpc:app_error:0x<code>:<body>` maps to a typed
    /// `Application`; anything else is `Internal`. Same wire as the nRPC typed
    /// layer, so Go's existing `AppError` helper works for org handlers.
    #[test]
    fn app_error_prefix_maps_to_application() {
        match org_handler_error_from_msg("nrpc:app_error:0x8001:nope".to_string()) {
            OrgHandlerError::Application { code, message } => {
                assert_eq!(code, 0x8001);
                assert_eq!(message, "nope");
            }
            other => panic!("expected Application, got {other:?}"),
        }
        match org_handler_error_from_msg("boom".to_string()) {
            OrgHandlerError::Internal(m) => assert_eq!(m, "boom"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// §1 regression — the `mesh_arc` handed to a provisioning/serve entry point
    /// is documented as **consumed**, so a validation early-return MUST drop the
    /// owned `Arc<MeshNode>` rather than strand it. We hand a fresh clone (exactly
    /// what `net_mesh_arc_clone` produces) to `net_org_install_provider_grant_audience`
    /// with NULL grant bytes — the earliest refusal reachable from ordinary Go
    /// (`InstallProviderGrantAudience(node, nil, path)`) — and prove the node's
    /// strong count returns to baseline. Before the fix (consume AFTER the
    /// validation returns), this leaked one whole node per bad call.
    #[test]
    fn provisioning_entry_does_not_leak_the_mesh_arc_on_bad_input() {
        let identity = net_sdk::identity::Identity::generate();
        let cfg =
            net::adapter::net::MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0u8; 32]);
        let node = Arc::new(
            runtime()
                .block_on(MeshNode::new((**identity.keypair()).clone(), cfg))
                .expect("MeshNode::new"),
        );
        let baseline = Arc::strong_count(&node);

        // A fresh clone, boxed the way `net_mesh_arc_clone` hands it over.
        let arc_box: *mut Arc<MeshNode> = Box::into_raw(Box::new(node.clone()));
        assert_eq!(Arc::strong_count(&node), baseline + 1);

        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_org_install_provider_grant_audience(
            arc_box,
            std::ptr::null(), // NULL grant bytes → refused before the consume line
            0,
            std::ptr::null(),
            0,
            &mut err,
        );
        assert_eq!(rc, NET_ORG_ERR_NULL, "NULL grant bytes must be refused");
        if !err.is_null() {
            net_org_free_cstring(err);
        }

        // The consumed clone must have been dropped on the early return.
        assert_eq!(
            Arc::strong_count(&node),
            baseline,
            "mesh_arc leaked on the input-validation error path"
        );
    }
}
