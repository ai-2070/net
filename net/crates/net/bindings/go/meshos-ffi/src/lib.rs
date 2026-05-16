//! C ABI for the MeshOS daemon-author SDK.
//!
//! Consumed by the Go binding at `bindings/go/net/meshos.go` and
//! by the C SDK header at `include/net_meshos.h` (Phase 5).
//!
//! # Scope (slice 1a)
//!
//! - SDK lifecycle: `sdk_start` / `sdk_shutdown` / `sdk_free`.
//! - Handle lifecycle: `register_daemon` (with an internal no-op
//!   daemon — the Go callback bridge lands in slice 1b) /
//!   `handle_free` / `graceful_shutdown`.
//! - Control event RX: `next_control` / `try_next_control`.
//! - Log emission: `publish_log`.
//! - Diagnostics: `dropped_control_events`.
//! - Last-error trio: `last_error_message` / `last_error_kind` /
//!   `clear_last_error`, matching the substrate's
//!   `<<meshos-sdk-kind:KIND>>MSG` envelope.
//!
//! # Out of scope until slice 1b
//!
//! - User-supplied daemon callbacks (`process` / `snapshot` /
//!   `restore` / `on_control` / `health` / `saturation`). The
//!   registered daemon today is a no-op `MeshDaemon` impl that
//!   returns `Healthy`, `0.0`, and empty process outputs. This
//!   exercises the supervisor lifecycle end-to-end without the
//!   cgo callback complexity.
//!
//! # Handle model
//!
//! Every Rust object that crosses the FFI is wrapped in a
//! heap-allocated box and handed to the caller as `*mut T`. The
//! consumer owns the pointer and MUST call the matching `_free`
//! function exactly once (or `graceful_shutdown` for the handle,
//! which also frees).
//!
//! # Error model
//!
//! Functions return `c_int` status codes (0 = OK, < 0 = error).
//! On error, the substrate's `<<meshos-sdk-kind:KIND>>MSG`
//! envelope is parsed and stored in a thread-local last-error
//! pair (`message` + `kind`); both are readable via
//! `net_meshos_last_error_*`. The pointers remain valid until
//! the next FFI call on the same thread.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::os::raw::c_float;
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use tokio::runtime::Runtime;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::behavior::meshos::logs::LogLevel as CoreLogLevel;
use net::adapter::net::behavior::meshos::{
    LoggingDispatcher, MeshOsConfig, MeshOsDaemonHandle as CoreHandle, MeshOsDaemonSdk as CoreSdk,
    SdkError, DEFAULT_GRACEFUL_SHUTDOWN,
};
use net::adapter::net::compute::{
    DaemonControl as CoreDaemonControl, DaemonError as CoreDaemonError, MeshDaemon,
};
use net::adapter::net::state::causal::CausalEvent;
use net::adapter::net::EntityKeypair;

// =========================================================================
// Status codes
// =========================================================================

pub const NET_MESHOS_OK: c_int = 0;
pub const NET_MESHOS_ERR_NULL: c_int = -1;
pub const NET_MESHOS_ERR_CALL_FAILED: c_int = -2;
pub const NET_MESHOS_ERR_INVALID_ARG: c_int = -3;
pub const NET_MESHOS_ERR_ALREADY_SHUTDOWN: c_int = -4;

// =========================================================================
// DaemonControl wire form — pass-by-out-param scalars
// =========================================================================

/// Discriminator constants for `NetMeshOsDaemonControl::kind`.
/// Bindings stay forward-compatible with new substrate variants by
/// passing `KIND_UNKNOWN` through unchanged.
pub const NET_MESHOS_CONTROL_NONE: c_int = 0;
pub const NET_MESHOS_CONTROL_SHUTDOWN: c_int = 1;
pub const NET_MESHOS_CONTROL_DRAIN_START: c_int = 2;
pub const NET_MESHOS_CONTROL_DRAIN_FINISH: c_int = 3;
pub const NET_MESHOS_CONTROL_BACKPRESSURE_ON: c_int = 4;
pub const NET_MESHOS_CONTROL_BACKPRESSURE_OFF: c_int = 5;
pub const NET_MESHOS_CONTROL_UNKNOWN: c_int = 99;

/// C-form control event. `kind` is one of the `NET_MESHOS_CONTROL_*`
/// constants; payload fields are populated only for variants that
/// carry them. The cross-binding wire form — Python / Node emit
/// the equivalent dict / object shape.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NetMeshOsDaemonControl {
    pub kind: c_int,
    /// Valid for `SHUTDOWN` and `DRAIN_START`.
    pub grace_period_ms: u64,
    /// Valid for `BACKPRESSURE_ON`. Range `[0.0, 1.0]`.
    pub level: c_float,
}

impl NetMeshOsDaemonControl {
    fn from_core(ev: CoreDaemonControl) -> Self {
        let mut out = Self::default();
        match ev {
            CoreDaemonControl::Shutdown { grace_period_ms } => {
                out.kind = NET_MESHOS_CONTROL_SHUTDOWN;
                out.grace_period_ms = grace_period_ms;
            }
            CoreDaemonControl::DrainStart { grace_period_ms } => {
                out.kind = NET_MESHOS_CONTROL_DRAIN_START;
                out.grace_period_ms = grace_period_ms;
            }
            CoreDaemonControl::DrainFinish => {
                out.kind = NET_MESHOS_CONTROL_DRAIN_FINISH;
            }
            CoreDaemonControl::BackpressureOn { level } => {
                out.kind = NET_MESHOS_CONTROL_BACKPRESSURE_ON;
                out.level = level;
            }
            CoreDaemonControl::BackpressureOff => {
                out.kind = NET_MESHOS_CONTROL_BACKPRESSURE_OFF;
            }
            _ => {
                out.kind = NET_MESHOS_CONTROL_UNKNOWN;
            }
        }
        out
    }
}

// =========================================================================
// LogLevel — C-form parser
// =========================================================================

pub const NET_MESHOS_LOG_TRACE: c_int = 0;
pub const NET_MESHOS_LOG_DEBUG: c_int = 1;
pub const NET_MESHOS_LOG_INFO: c_int = 2;
pub const NET_MESHOS_LOG_WARN: c_int = 3;
pub const NET_MESHOS_LOG_ERROR: c_int = 4;

fn parse_log_level(level: c_int) -> Option<CoreLogLevel> {
    Some(match level {
        NET_MESHOS_LOG_TRACE => CoreLogLevel::Trace,
        NET_MESHOS_LOG_DEBUG => CoreLogLevel::Debug,
        NET_MESHOS_LOG_INFO => CoreLogLevel::Info,
        NET_MESHOS_LOG_WARN => CoreLogLevel::Warn,
        NET_MESHOS_LOG_ERROR => CoreLogLevel::Error,
        _ => return None,
    })
}

// =========================================================================
// Thread-local last-error trio
// =========================================================================

thread_local! {
    static LAST_ERROR_MESSAGE: RefCell<Option<CString>> = const { RefCell::new(None) };
    static LAST_ERROR_KIND: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(kind: &str, message: &str) {
    let msg = CString::new(message).ok();
    let kind = CString::new(kind).ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn set_last_error_from_sdk(err: &SdkError) {
    set_last_error(err.kind, &err.message);
}

fn clear_last_error_inner() {
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = None);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = None);
}

/// Most recent error message on this thread, or NULL. The pointer
/// is valid until the next FFI call on the same thread that
/// touches the thread-local. Callers must NOT free.
#[no_mangle]
pub extern "C" fn net_meshos_last_error_message() -> *const c_char {
    LAST_ERROR_MESSAGE.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Most recent error kind on this thread, or NULL. The kind is
/// the substrate's discriminator (e.g. `"register_failed"`,
/// `"queue_full"`, `"already_shutdown"`).
#[no_mangle]
pub extern "C" fn net_meshos_last_error_kind() -> *const c_char {
    LAST_ERROR_KIND.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Clear the thread-local last-error pair.
#[no_mangle]
pub extern "C" fn net_meshos_clear_last_error() {
    clear_last_error_inner();
}

// =========================================================================
// FFI guard — wraps every entry point in `catch_unwind`
// =========================================================================

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
                set_last_error("runtime_panic", &detail);
                $default
            }
        }
    }};
}

// =========================================================================
// Shared tokio runtime
// =========================================================================

fn runtime() -> &'static Arc<Runtime> {
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("net-meshos-ffi")
                .build()
                .expect("failed to construct meshos-ffi tokio runtime"),
        )
    })
}

// =========================================================================
// NoopDaemon — slice 1a placeholder
// =========================================================================

/// Internal no-op `MeshDaemon` impl. Slice 1a registers this for
/// every `register_daemon` call so the supervisor lifecycle is
/// exercisable from Go without the cgo callback bridge. Slice 1b
/// replaces this with a `CgoDaemonBridge` that delegates to user
/// callbacks via `//export` trampolines.
struct NoopDaemon {
    name: String,
}

impl MeshDaemon for NoopDaemon {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::default()
    }

    fn optional_capabilities(&self) -> CapabilitySet {
        CapabilitySet::default()
    }

    fn process(
        &mut self,
        _event: &CausalEvent,
    ) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        Ok(Vec::new())
    }
}

// =========================================================================
// Handle types
// =========================================================================

pub struct NetMeshOsSdk {
    inner: parking_lot_or_std::Mutex<Option<CoreSdk>>,
}

pub struct NetMeshOsHandle {
    inner: parking_lot_or_std::Mutex<Option<CoreHandle>>,
    daemon_id: u64,
    daemon_name: CString,
}

// std::sync::Mutex is fine — these are coarse-grained handles
// touched by only one consumer thread in the common case.
mod parking_lot_or_std {
    pub use std::sync::Mutex;
}

// =========================================================================
// SDK lifecycle
// =========================================================================

/// Start the MeshOS SDK. Accepts an optional config (pass NULL for
/// substrate defaults). On success, writes a heap-allocated handle
/// to `*out` and returns `NET_MESHOS_OK`; on failure, populates
/// the thread-local last-error pair and returns a non-OK status.
///
/// `this_node` is the substrate's node id (0 for defaults).
/// `tick_interval_ms` is the supervisor reconcile cadence (0 →
/// substrate default 500ms). `event_queue_capacity` and
/// `action_queue_capacity` size the internal mpsc channels (0 →
/// substrate default 1024 each). `control_capacity` is the
/// per-daemon control-channel capacity (0 → substrate default 8).
#[no_mangle]
pub extern "C" fn net_meshos_sdk_start(
    this_node: u64,
    tick_interval_ms: u64,
    event_queue_capacity: usize,
    action_queue_capacity: usize,
    control_capacity: usize,
    out: *mut *mut NetMeshOsSdk,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_MESHOS_ERR_INVALID_ARG;
        }
        clear_last_error_inner();
        let mut cfg = MeshOsConfig::default();
        cfg.this_node = this_node;
        if tick_interval_ms > 0 {
            cfg.tick_interval = Duration::from_millis(tick_interval_ms);
        }
        if event_queue_capacity > 0 {
            cfg.event_queue_capacity = event_queue_capacity;
        }
        if action_queue_capacity > 0 {
            cfg.action_queue_capacity = action_queue_capacity;
        }
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let mut sdk = {
            let _enter = runtime().enter();
            CoreSdk::start(cfg, dispatcher)
        };
        if control_capacity > 0 {
            sdk = sdk.with_control_capacity(control_capacity);
        }
        let handle = Box::into_raw(Box::new(NetMeshOsSdk {
            inner: parking_lot_or_std::Mutex::new(Some(sdk)),
        }));
        unsafe { *out = handle };
        NET_MESHOS_OK
    })
}

/// Free an SDK handle without graceful shutdown. The wrapped
/// runtime stays alive on its tokio tasks until they finish
/// naturally; for orderly teardown call `net_meshos_sdk_shutdown`
/// first. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_meshos_sdk_free(sdk: *mut NetMeshOsSdk) {
    if sdk.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(sdk);
    }
}

/// Drive a clean shutdown of the wrapped runtime. Consumes the
/// inner SDK by value — subsequent calls on the handle return
/// `NET_MESHOS_ERR_ALREADY_SHUTDOWN`. Caller still must
/// `net_meshos_sdk_free` to release the outer handle.
#[no_mangle]
pub extern "C" fn net_meshos_sdk_shutdown(sdk: *mut NetMeshOsSdk) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(sdk_ref) = (unsafe { sdk.as_ref() }) else {
            set_last_error("invalid_argument", "sdk pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        clear_last_error_inner();
        let inner = match sdk_ref.inner.lock().unwrap().take() {
            Some(s) => s,
            None => {
                set_last_error("already_shutdown", "MeshOsDaemonSdk was already shut down");
                return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
            }
        };
        match runtime().block_on(inner.shutdown()) {
            Ok(_stats) => NET_MESHOS_OK,
            Err(e) => {
                set_last_error("shutdown_failed", &format!("{e:?}"));
                NET_MESHOS_ERR_CALL_FAILED
            }
        }
    })
}

/// Diagnostic counter — total control events the router dropped
/// across every registered daemon because a daemon's channel was
/// full. Returns `u64::MAX` on NULL / already-shutdown.
#[no_mangle]
pub extern "C" fn net_meshos_sdk_dropped_control_events(sdk: *mut NetMeshOsSdk) -> u64 {
    ffi_guard!(u64::MAX, {
        let Some(sdk_ref) = (unsafe { sdk.as_ref() }) else {
            return u64::MAX;
        };
        let guard = sdk_ref.inner.lock().unwrap();
        match guard.as_ref() {
            Some(s) => s.dropped_control_events(),
            None => u64::MAX,
        }
    })
}

// =========================================================================
// Daemon registration (slice 1a — internal NoopDaemon)
// =========================================================================

/// Register a daemon under the supplied identity. Slice 1a wires
/// a no-op `MeshDaemon` impl; the Go-callback bridge lands in
/// slice 1b. `name_ptr` / `name_len` is the daemon's name (UTF-8,
/// not NUL-terminated). `seed_ptr` is a 32-byte ed25519 seed for
/// the daemon's `EntityKeypair`.
///
/// On success, writes a heap-allocated handle to `*out` and
/// returns `NET_MESHOS_OK`. The handle holds the daemon's substrate
/// identity for the lifetime of the registration.
///
/// # Safety
///
/// `name_ptr` must point to `name_len` bytes of valid UTF-8.
/// `seed_ptr` must point to exactly 32 bytes.
#[no_mangle]
pub extern "C" fn net_meshos_register_daemon(
    sdk: *mut NetMeshOsSdk,
    name_ptr: *const c_char,
    name_len: usize,
    seed_ptr: *const u8,
    out: *mut *mut NetMeshOsHandle,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(sdk_ref) = (unsafe { sdk.as_ref() }) else {
            set_last_error("invalid_argument", "sdk pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        if name_ptr.is_null() || seed_ptr.is_null() || out.is_null() {
            set_last_error("invalid_argument", "name / seed / out pointer is NULL");
            return NET_MESHOS_ERR_INVALID_ARG;
        }
        let name = match cstr_to_string(name_ptr, name_len) {
            Some(n) => n,
            None => {
                set_last_error("invalid_argument", "daemon name is not valid UTF-8");
                return NET_MESHOS_ERR_INVALID_ARG;
            }
        };
        let seed: [u8; 32] = unsafe { std::slice::from_raw_parts(seed_ptr, 32) }
            .try_into()
            .expect("slice was constructed with len 32");
        clear_last_error_inner();
        let keypair = EntityKeypair::from_bytes(seed);
        let daemon = Box::new(NoopDaemon { name: name.clone() });
        let guard = sdk_ref.inner.lock().unwrap();
        let sdk_inner = match guard.as_ref() {
            Some(s) => s,
            None => {
                set_last_error("already_shutdown", "MeshOsDaemonSdk was already shut down");
                return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
            }
        };
        match sdk_inner.register_daemon(daemon, keypair) {
            Ok(handle) => {
                let daemon_id = handle.daemon_id();
                let daemon_name = match CString::new(handle.daemon_name()) {
                    Ok(s) => s,
                    Err(_) => CString::new(name).expect("name has no NUL"),
                };
                let boxed = Box::into_raw(Box::new(NetMeshOsHandle {
                    inner: parking_lot_or_std::Mutex::new(Some(handle)),
                    daemon_id,
                    daemon_name,
                }));
                unsafe { *out = boxed };
                NET_MESHOS_OK
            }
            Err(e) => {
                set_last_error_from_sdk(&e);
                NET_MESHOS_ERR_CALL_FAILED
            }
        }
    })
}

/// Free a daemon handle. If the substrate-side handle is still
/// present (i.e. `graceful_shutdown` wasn't called), the Rust-side
/// `Drop` impl still cleans up the registry slot. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_meshos_handle_free(handle: *mut NetMeshOsHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(handle);
    }
}

/// Substrate identifier (origin hash). Stable across the handle's
/// lifetime. Returns `0` on NULL.
#[no_mangle]
pub extern "C" fn net_meshos_handle_daemon_id(handle: *const NetMeshOsHandle) -> u64 {
    match unsafe { handle.as_ref() } {
        Some(h) => h.daemon_id,
        None => 0,
    }
}

/// Daemon name (NUL-terminated). Pointer valid for the handle's
/// lifetime. Returns NULL on NULL handle.
#[no_mangle]
pub extern "C" fn net_meshos_handle_daemon_name(handle: *const NetMeshOsHandle) -> *const c_char {
    match unsafe { handle.as_ref() } {
        Some(h) => h.daemon_name.as_ptr(),
        None => ptr::null(),
    }
}

// =========================================================================
// Control event RX
// =========================================================================

/// Non-blocking control-event receive. Writes the next event to
/// `*out` and returns `NET_MESHOS_OK`. If the channel is empty,
/// writes `kind = NET_MESHOS_CONTROL_NONE` and still returns
/// `NET_MESHOS_OK` — callers branch on `out.kind`.
#[no_mangle]
pub extern "C" fn net_meshos_try_next_control(
    handle: *mut NetMeshOsHandle,
    out: *mut NetMeshOsDaemonControl,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_MESHOS_ERR_INVALID_ARG;
        }
        clear_last_error_inner();
        let mut guard = h_ref.inner.lock().unwrap();
        let h = match guard.as_mut() {
            Some(h) => h,
            None => {
                set_last_error("already_shutdown", "daemon handle was already consumed");
                return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
            }
        };
        let ev = h.try_next_control();
        unsafe {
            *out = match ev {
                Some(e) => NetMeshOsDaemonControl::from_core(e),
                None => NetMeshOsDaemonControl::default(),
            };
        }
        NET_MESHOS_OK
    })
}

/// Block until the next control event arrives, the runtime shuts
/// down, or `timeout_ms` elapses. On timeout or shutdown writes
/// `kind = NET_MESHOS_CONTROL_NONE`. Pass `0` for an unbounded
/// wait (matching the substrate's `next_control` semantics).
#[no_mangle]
pub extern "C" fn net_meshos_next_control(
    handle: *mut NetMeshOsHandle,
    timeout_ms: u64,
    out: *mut NetMeshOsDaemonControl,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_MESHOS_ERR_INVALID_ARG;
        }
        clear_last_error_inner();
        let ev = {
            let mut guard = h_ref.inner.lock().unwrap();
            let h = match guard.as_mut() {
                Some(h) => h,
                None => {
                    set_last_error("already_shutdown", "daemon handle was already consumed");
                    return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
                }
            };
            runtime().block_on(async {
                if timeout_ms == 0 {
                    h.next_control().await
                } else {
                    match tokio::time::timeout(Duration::from_millis(timeout_ms), h.next_control())
                        .await
                    {
                        Ok(ev) => ev,
                        Err(_) => None,
                    }
                }
            })
        };
        unsafe {
            *out = match ev {
                Some(e) => NetMeshOsDaemonControl::from_core(e),
                None => NetMeshOsDaemonControl::default(),
            };
        }
        NET_MESHOS_OK
    })
}

// =========================================================================
// Log emission
// =========================================================================

/// Publish a log line tagged with this daemon's id. Non-blocking;
/// fills the last-error pair with `kind = "queue_full"` or
/// `"loop_closed"` when the substrate's log ring is saturated.
#[no_mangle]
pub extern "C" fn net_meshos_publish_log(
    handle: *mut NetMeshOsHandle,
    level: c_int,
    message_ptr: *const c_char,
    message_len: usize,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        let Some(lvl) = parse_log_level(level) else {
            set_last_error(
                "invalid_log_level",
                "log level must be 0|1|2|3|4 (trace|debug|info|warn|error)",
            );
            return NET_MESHOS_ERR_INVALID_ARG;
        };
        if message_ptr.is_null() {
            set_last_error("invalid_argument", "message pointer is NULL");
            return NET_MESHOS_ERR_INVALID_ARG;
        }
        let message = match cstr_to_string(message_ptr, message_len) {
            Some(m) => m,
            None => {
                set_last_error("invalid_argument", "message is not valid UTF-8");
                return NET_MESHOS_ERR_INVALID_ARG;
            }
        };
        clear_last_error_inner();
        let guard = h_ref.inner.lock().unwrap();
        let h = match guard.as_ref() {
            Some(h) => h,
            None => {
                set_last_error("already_shutdown", "daemon handle was already consumed");
                return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
            }
        };
        match h.publish_log(lvl, message) {
            Ok(()) => NET_MESHOS_OK,
            Err(e) => {
                set_last_error_from_sdk(&e);
                NET_MESHOS_ERR_CALL_FAILED
            }
        }
    })
}

// =========================================================================
// Graceful shutdown
// =========================================================================

/// Drive a graceful shutdown on the handle. Sends
/// `Shutdown { grace_period_ms }` on the daemon's control channel,
/// parks for `grace_ms`, then unregisters. Consumes the inner
/// handle — subsequent operations return
/// `NET_MESHOS_ERR_ALREADY_SHUTDOWN`. Caller still must
/// `net_meshos_handle_free` to release the outer handle.
///
/// Pass `0` for `grace_ms` to use the substrate's default
/// (`DEFAULT_GRACEFUL_SHUTDOWN`, 5 s).
#[no_mangle]
pub extern "C" fn net_meshos_graceful_shutdown(
    handle: *mut NetMeshOsHandle,
    grace_ms: u64,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        let inner = match h_ref.inner.lock().unwrap().take() {
            Some(h) => h,
            None => {
                set_last_error("already_shutdown", "daemon handle was already consumed");
                return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
            }
        };
        clear_last_error_inner();
        let grace = if grace_ms == 0 {
            DEFAULT_GRACEFUL_SHUTDOWN
        } else {
            Duration::from_millis(grace_ms)
        };
        match runtime().block_on(inner.graceful_shutdown(grace)) {
            Ok(()) => NET_MESHOS_OK,
            Err(e) => {
                set_last_error_from_sdk(&e);
                NET_MESHOS_ERR_CALL_FAILED
            }
        }
    })
}

// =========================================================================
// Helpers
// =========================================================================

/// Parse `(*const c_char, len)` into a Rust `String`. Returns
/// `None` on invalid UTF-8.
fn cstr_to_string(ptr: *const c_char, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // `len == 0`: caller passed an empty string. Accept rather
    // than treating as invalid.
    if len == 0 {
        return Some(String::new());
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    std::str::from_utf8(slice).ok().map(|s| s.to_string())
}

/// Silence the `unused` warning for `CStr` (kept available for
/// future slices that need NUL-terminated strings).
const _: fn() = || {
    let _ = CStr::from_bytes_with_nul(b"\0");
};

// =========================================================================
// Tests — exercise the C ABI end-to-end from Rust. Since the Go
// binding pattern is reference-only (no go.mod), these tests are
// the canonical end-to-end verification.
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr_ptr(s: &str) -> (*const c_char, usize) {
        (s.as_ptr() as *const c_char, s.len())
    }

    #[test]
    fn sdk_lifecycle_start_shutdown_free() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        let status = net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk);
        assert_eq!(status, NET_MESHOS_OK);
        assert!(!sdk.is_null());

        assert_eq!(net_meshos_sdk_shutdown(sdk), NET_MESHOS_OK);

        // Double shutdown surfaces ALREADY_SHUTDOWN.
        assert_eq!(
            net_meshos_sdk_shutdown(sdk),
            NET_MESHOS_ERR_ALREADY_SHUTDOWN
        );
        let kind_ptr = net_meshos_last_error_kind();
        assert!(!kind_ptr.is_null());
        let kind = unsafe { CStr::from_ptr(kind_ptr).to_string_lossy().into_owned() };
        assert_eq!(kind, "already_shutdown");

        net_meshos_sdk_free(sdk);
    }

    #[test]
    fn register_daemon_publish_log_graceful_shutdown() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);

        let name = "echo";
        let (name_ptr, name_len) = cstr_ptr(name);
        let seed = [7u8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        let status =
            net_meshos_register_daemon(sdk, name_ptr, name_len, seed.as_ptr(), &mut handle);
        assert_eq!(status, NET_MESHOS_OK);
        assert!(!handle.is_null());

        // Daemon id from the seed = EntityKeypair::from_bytes(seed).origin_hash().
        let expected_id = EntityKeypair::from_bytes(seed).origin_hash();
        assert_eq!(net_meshos_handle_daemon_id(handle), expected_id);

        let name_back = unsafe { CStr::from_ptr(net_meshos_handle_daemon_name(handle)) };
        assert_eq!(name_back.to_str().unwrap(), "echo");

        // publish_log at every level.
        let msg = "hello";
        let (msg_ptr, msg_len) = cstr_ptr(msg);
        for lvl in [
            NET_MESHOS_LOG_TRACE,
            NET_MESHOS_LOG_DEBUG,
            NET_MESHOS_LOG_INFO,
            NET_MESHOS_LOG_WARN,
            NET_MESHOS_LOG_ERROR,
        ] {
            assert_eq!(
                net_meshos_publish_log(handle, lvl, msg_ptr, msg_len),
                NET_MESHOS_OK,
            );
        }

        // Invalid log level surfaces the typed kind.
        assert_eq!(
            net_meshos_publish_log(handle, 99, msg_ptr, msg_len),
            NET_MESHOS_ERR_INVALID_ARG,
        );
        let kind = unsafe {
            CStr::from_ptr(net_meshos_last_error_kind())
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(kind, "invalid_log_level");

        // try_next_control on a quiet channel returns NONE.
        let mut ev = NetMeshOsDaemonControl::default();
        assert_eq!(net_meshos_try_next_control(handle, &mut ev), NET_MESHOS_OK);
        assert_eq!(ev.kind, NET_MESHOS_CONTROL_NONE);

        // graceful_shutdown consumes the handle.
        assert_eq!(net_meshos_graceful_shutdown(handle, 10), NET_MESHOS_OK);
        assert_eq!(
            net_meshos_graceful_shutdown(handle, 10),
            NET_MESHOS_ERR_ALREADY_SHUTDOWN,
        );

        net_meshos_handle_free(handle);
        assert_eq!(net_meshos_sdk_shutdown(sdk), NET_MESHOS_OK);
        net_meshos_sdk_free(sdk);
    }

    #[test]
    fn next_control_with_timeout_returns_none_on_quiet_channel() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);

        let name = "echo";
        let (name_ptr, name_len) = cstr_ptr(name);
        let seed = [11u8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        assert_eq!(
            net_meshos_register_daemon(sdk, name_ptr, name_len, seed.as_ptr(), &mut handle),
            NET_MESHOS_OK,
        );

        let mut ev = NetMeshOsDaemonControl::default();
        let status = net_meshos_next_control(handle, 100, &mut ev);
        assert_eq!(status, NET_MESHOS_OK);
        assert_eq!(ev.kind, NET_MESHOS_CONTROL_NONE);

        assert_eq!(net_meshos_graceful_shutdown(handle, 10), NET_MESHOS_OK);
        net_meshos_handle_free(handle);
        assert_eq!(net_meshos_sdk_shutdown(sdk), NET_MESHOS_OK);
        net_meshos_sdk_free(sdk);
    }

    #[test]
    fn null_sdk_pointer_returns_invalid_arg() {
        let sdk: *mut NetMeshOsSdk = ptr::null_mut();
        let mut h: *mut NetMeshOsHandle = ptr::null_mut();
        // null SDK pointer
        let status = net_meshos_register_daemon(sdk, ptr::null(), 0, ptr::null(), &mut h);
        assert_eq!(status, NET_MESHOS_ERR_NULL);
        let _ = unsafe { CStr::from_ptr(net_meshos_last_error_kind()) };

        // null out pointer on sdk_start
        let status = net_meshos_sdk_start(0, 0, 0, 0, 0, ptr::null_mut());
        assert_eq!(status, NET_MESHOS_ERR_INVALID_ARG);

        // Cleanup the leak — there's no SDK to free since `sdk` is
        // still null. Just clear the last-error pair for hygiene.
        net_meshos_clear_last_error();
    }
}
