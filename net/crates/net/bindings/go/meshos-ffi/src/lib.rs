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
//! # Slice 1b — vtable-based daemon callbacks
//!
//! `net_meshos_register_daemon_with_vtable` accepts a
//! `NetMeshOsDaemonVtable` struct of C function pointers
//! (`process` / `snapshot` / `restore` / `on_control` / `health`
//! / `saturation`) plus an opaque `user_ctx`. The bridge wraps
//! these into a `MeshDaemon` impl driven by the supervisor.
//!
//! The slice-1a `net_meshos_register_daemon` (which registers an
//! internal no-op daemon) is kept for lifecycle-only consumers
//! that want to drive control / log / shutdown surfaces without
//! plugging in process logic.
//!
//! Process and snapshot callbacks emit zero or more output
//! buffers via the `net_meshos_process_emit` / `_snapshot_emit`
//! helpers (the bridge passes an opaque emit-context handle on
//! each invocation; the consumer calls the helper to hand
//! payloads back to Rust). This avoids cross-allocator buffer
//! ownership issues.
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
    LoggingDispatcher, MaintenanceMirrorSnapshot, MaintenanceStateView, MeshOsConfig,
    MeshOsDaemonHandle as CoreHandle, MeshOsDaemonSdk as CoreSdk, MetadataView, PeerHealthSnapshot,
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

/// Internal no-op `MeshDaemon` impl. The slice-1a
/// `net_meshos_register_daemon` entry point registers this for
/// every call — lifecycle-only consumers that want to drive
/// control / log / shutdown surfaces without plugging in process
/// logic use this path. Real daemons use
/// `net_meshos_register_daemon_with_vtable`.
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
// Slice 1b — vtable-based daemon bridge
// =========================================================================

/// Health discriminator for the vtable's `health` callback.
pub const NET_MESHOS_HEALTH_HEALTHY: c_int = 0;
pub const NET_MESHOS_HEALTH_DEGRADED: c_int = 1;
pub const NET_MESHOS_HEALTH_UNHEALTHY: c_int = 2;

/// Vtable of C function pointers a consumer fills in to implement
/// a daemon. All fields except `process` are optional — set to
/// NULL to take the substrate default. The bridge ignores fields
/// it doesn't recognize.
///
/// Each callback receives the consumer's `user_ctx` (set at
/// `register_daemon_with_vtable` time). Callbacks fire from
/// tokio worker threads — consumers MUST treat `user_ctx` access
/// as concurrent and protect any shared state.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NetMeshOsDaemonVtable {
    /// Required. Returns 0 on success; non-zero is treated as a
    /// substrate-side `ProcessFailed`. Emit zero or more output
    /// buffers via `net_meshos_process_emit(emit_ctx, ptr, len)`.
    pub process: Option<
        unsafe extern "C" fn(
            user_ctx: *mut std::ffi::c_void,
            emit_ctx: *mut NetMeshOsProcessEmitCtx,
            origin_hash: u64,
            sequence: u64,
            payload_ptr: *const u8,
            payload_len: usize,
        ) -> c_int,
    >,
    /// Optional. Stateless daemons set NULL (or return 0 from the
    /// callback). Emit at most one snapshot buffer via
    /// `net_meshos_snapshot_emit(emit_ctx, ptr, len)`; subsequent
    /// emit calls in the same callback are ignored.
    pub snapshot: Option<
        unsafe extern "C" fn(
            user_ctx: *mut std::ffi::c_void,
            emit_ctx: *mut NetMeshOsSnapshotEmitCtx,
        ),
    >,
    /// Optional. Returns 0 on success; non-zero is treated as
    /// substrate `RestoreFailed`.
    pub restore: Option<
        unsafe extern "C" fn(
            user_ctx: *mut std::ffi::c_void,
            payload_ptr: *const u8,
            payload_len: usize,
        ) -> c_int,
    >,
    /// Optional. Fires from the supervisor's reconcile pass when
    /// a daemon-targeted action dispatches. Same wire form as
    /// `net_meshos_next_control` (`kind` discriminator + payload
    /// fields).
    pub on_control: Option<
        unsafe extern "C" fn(
            user_ctx: *mut std::ffi::c_void,
            kind: c_int,
            grace_period_ms: u64,
            level: c_float,
        ),
    >,
    /// Optional. Returns one of `NET_MESHOS_HEALTH_*`. NULL = always
    /// `Healthy`.
    pub health: Option<unsafe extern "C" fn(user_ctx: *mut std::ffi::c_void) -> c_int>,
    /// Optional. Returns a value in `[0.0, 1.0]`. NULL = `0.0`.
    pub saturation: Option<unsafe extern "C" fn(user_ctx: *mut std::ffi::c_void) -> c_float>,
}

/// Opaque emit context handed to `process` callbacks. Consumers
/// call `net_meshos_process_emit(emit_ctx, ptr, len)` for each
/// output buffer they want to publish.
pub struct NetMeshOsProcessEmitCtx {
    outputs: Vec<Bytes>,
}

/// Opaque emit context handed to `snapshot` callbacks. Consumers
/// call `net_meshos_snapshot_emit(emit_ctx, ptr, len)` to publish
/// at most one snapshot buffer.
pub struct NetMeshOsSnapshotEmitCtx {
    payload: Option<Bytes>,
}

/// Emit a process-output buffer. The bridge copies the bytes into
/// a Rust-owned `Bytes` immediately; the caller may free the
/// source buffer as soon as this returns. Safe to call multiple
/// times per `process` invocation (each call adds one output to
/// the substrate's emitted-events list).
///
/// # Safety
///
/// `ctx` must be the value handed to a vtable `process` callback;
/// `payload_ptr` must point to `payload_len` bytes of readable
/// memory.
#[no_mangle]
pub unsafe extern "C" fn net_meshos_process_emit(
    ctx: *mut NetMeshOsProcessEmitCtx,
    payload_ptr: *const u8,
    payload_len: usize,
) {
    let Some(ctx) = ctx.as_mut() else {
        return;
    };
    if payload_ptr.is_null() && payload_len > 0 {
        return;
    }
    let slice = if payload_len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) }
    };
    ctx.outputs.push(Bytes::copy_from_slice(slice));
}

/// Emit the daemon's serialized snapshot buffer. Calling more than
/// once per snapshot callback is a no-op for subsequent calls —
/// the first emission wins. Bytes are copied into a Rust-owned
/// `Bytes` immediately.
///
/// # Safety
///
/// `ctx` must be the value handed to a vtable `snapshot` callback;
/// `payload_ptr` must point to `payload_len` bytes of readable
/// memory.
#[no_mangle]
pub unsafe extern "C" fn net_meshos_snapshot_emit(
    ctx: *mut NetMeshOsSnapshotEmitCtx,
    payload_ptr: *const u8,
    payload_len: usize,
) {
    let Some(ctx) = ctx.as_mut() else {
        return;
    };
    if ctx.payload.is_some() {
        return;
    }
    if payload_ptr.is_null() && payload_len > 0 {
        return;
    }
    let slice = if payload_len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) }
    };
    ctx.payload = Some(Bytes::copy_from_slice(slice));
}

/// Wrapper around `*mut c_void` so the bridge can be `Send + Sync`.
/// The consumer is responsible for the thread-safety of their
/// `user_ctx` — vtable callbacks fire from tokio worker threads.
struct UserCtx(*mut std::ffi::c_void);

// SAFETY: thread-safety of `user_ctx` access is the consumer's
// responsibility. Documented at the vtable level.
unsafe impl Send for UserCtx {}
unsafe impl Sync for UserCtx {}

/// `MeshDaemon` impl wrapping the consumer's vtable. Each trait
/// method invokes the matching vtable callback; missing callbacks
/// fall back to the substrate default.
struct CDaemonBridge {
    name: String,
    vtable: NetMeshOsDaemonVtable,
    user_ctx: UserCtx,
}

impl MeshDaemon for CDaemonBridge {
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

    fn process(&mut self, event: &CausalEvent) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        let Some(process_fn) = self.vtable.process else {
            return Err(CoreDaemonError::ProcessFailed(
                "vtable has no `process` callback".into(),
            ));
        };
        let mut emit_ctx = NetMeshOsProcessEmitCtx {
            outputs: Vec::new(),
        };
        let payload = &event.payload;
        let rc = unsafe {
            process_fn(
                self.user_ctx.0,
                &mut emit_ctx,
                event.link.origin_hash,
                event.link.sequence,
                if payload.is_empty() {
                    std::ptr::null()
                } else {
                    payload.as_ptr()
                },
                payload.len(),
            )
        };
        if rc != 0 {
            return Err(CoreDaemonError::ProcessFailed(format!(
                "vtable `process` returned non-zero status: {rc}"
            )));
        }
        Ok(emit_ctx.outputs)
    }

    fn snapshot(&self) -> Option<Bytes> {
        let snapshot_fn = self.vtable.snapshot?;
        let mut emit_ctx = NetMeshOsSnapshotEmitCtx { payload: None };
        unsafe {
            snapshot_fn(self.user_ctx.0, &mut emit_ctx);
        }
        emit_ctx.payload
    }

    fn restore(&mut self, state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        let Some(restore_fn) = self.vtable.restore else {
            return Ok(());
        };
        let rc = unsafe {
            restore_fn(
                self.user_ctx.0,
                if state.is_empty() {
                    std::ptr::null()
                } else {
                    state.as_ptr()
                },
                state.len(),
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(CoreDaemonError::RestoreFailed(format!(
                "vtable `restore` returned non-zero status: {rc}"
            )))
        }
    }

    fn health(&self) -> net::adapter::net::compute::DaemonHealth {
        use net::adapter::net::compute::DaemonHealth as H;
        let Some(health_fn) = self.vtable.health else {
            return H::Healthy;
        };
        let kind = unsafe { health_fn(self.user_ctx.0) };
        match kind {
            NET_MESHOS_HEALTH_HEALTHY => H::Healthy,
            NET_MESHOS_HEALTH_DEGRADED => H::Degraded {
                reason: String::new(),
            },
            NET_MESHOS_HEALTH_UNHEALTHY => H::Unhealthy,
            _ => H::Healthy,
        }
    }

    fn saturation(&self) -> f32 {
        let Some(saturation_fn) = self.vtable.saturation else {
            return 0.0;
        };
        let v = unsafe { saturation_fn(self.user_ctx.0) };
        v.clamp(0.0, 1.0)
    }

    fn on_control(&mut self, event: CoreDaemonControl) {
        let Some(on_control_fn) = self.vtable.on_control else {
            return;
        };
        let c = NetMeshOsDaemonControl::from_core(event);
        unsafe {
            on_control_fn(self.user_ctx.0, c.kind, c.grace_period_ms, c.level);
        }
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
        let inner = match sdk_ref.inner.lock().unwrap_or_else(|e| e.into_inner()).take() {
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
        let guard = sdk_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        let guard = sdk_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
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

/// Register a daemon with consumer-supplied callbacks. Same shape
/// as `net_meshos_register_daemon` but takes a vtable +
/// `user_ctx`. The vtable's `process` field is required; every
/// other field may be NULL to take the substrate default.
///
/// `user_ctx` is passed verbatim to every callback. The consumer
/// owns `user_ctx`'s lifetime — it MUST remain valid until the
/// returned handle is freed (either via `net_meshos_handle_free`
/// or `net_meshos_graceful_shutdown`). Concurrent access from
/// multiple tokio worker threads is the consumer's
/// responsibility.
///
/// The vtable struct is copied; the consumer may free or
/// invalidate it as soon as the call returns.
///
/// # Safety
///
/// Standard FFI-pointer caveats. `name_ptr` must point to
/// `name_len` bytes of valid UTF-8; `seed_ptr` to exactly 32
/// bytes; `vtable_ptr` to a single `NetMeshOsDaemonVtable`. Each
/// function pointer in the vtable (when non-NULL) must remain
/// valid until the handle is freed.
#[no_mangle]
pub extern "C" fn net_meshos_register_daemon_with_vtable(
    sdk: *mut NetMeshOsSdk,
    name_ptr: *const c_char,
    name_len: usize,
    seed_ptr: *const u8,
    vtable_ptr: *const NetMeshOsDaemonVtable,
    user_ctx: *mut std::ffi::c_void,
    out: *mut *mut NetMeshOsHandle,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(sdk_ref) = (unsafe { sdk.as_ref() }) else {
            set_last_error("invalid_argument", "sdk pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        if name_ptr.is_null() || seed_ptr.is_null() || vtable_ptr.is_null() || out.is_null() {
            set_last_error(
                "invalid_argument",
                "name / seed / vtable / out pointer is NULL",
            );
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
        let vtable = unsafe { *vtable_ptr };
        if vtable.process.is_none() {
            set_last_error(
                "invalid_argument",
                "vtable `process` callback is required (was NULL)",
            );
            return NET_MESHOS_ERR_INVALID_ARG;
        }
        clear_last_error_inner();
        let keypair = EntityKeypair::from_bytes(seed);
        let daemon = Box::new(CDaemonBridge {
            name: name.clone(),
            vtable,
            user_ctx: UserCtx(user_ctx),
        });
        let guard = sdk_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut guard = h_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut guard = h_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        let guard = h_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        let inner = match h_ref.inner.lock().unwrap_or_else(|e| e.into_inner()).take() {
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
// Metadata + capability advertisement (slice 2)
// =========================================================================

/// JSON-render a [`MetadataView`] for cross-FFI transport.
/// Mirrors the Python binding's `metadata_view_to_dict` shape so
/// every binding observes the same field names + value
/// projections.
fn metadata_view_to_json(view: &MetadataView) -> String {
    use serde_json::{json, Value};
    let peers: Vec<Value> = view
        .peers
        .iter()
        .map(|(id, snap)| {
            json!({
                "node_id": id,
                "rtt_ms": snap.rtt_ms,
                "health": snap.health.map(peer_health_str),
                "maintenance": snap.maintenance.map(maintenance_mirror_str),
                "cpu_load_1m": snap.cpu_load_1m,
                "mem_used_bytes": snap.mem_used_bytes,
                "mem_total_bytes": snap.mem_total_bytes,
                "disk_used_bytes": snap.disk_used_bytes,
                "disk_total_bytes": snap.disk_total_bytes,
                "saturation_trend": snap.saturation_trend,
                "capability_set": snap.capability_set.iter().collect::<Vec<_>>(),
                "software_version": snap.software_version,
                "forked_from": snap.forked_from,
            })
        })
        .collect();
    let value = json!({
        "node_id": view.node_id,
        "daemon_id": view.daemon_id,
        "daemon_name": view.daemon_name,
        "maintenance_state": maintenance_state_to_json(&view.maintenance_state),
        "peers": peers,
    });
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".into())
}

fn maintenance_state_to_json(state: &MaintenanceStateView) -> serde_json::Value {
    use serde_json::json;
    match state {
        MaintenanceStateView::Active => json!({"kind": "active"}),
        MaintenanceStateView::EnteringMaintenance {
            since_ms,
            deadline_remaining_ms,
        } => json!({
            "kind": "entering_maintenance",
            "since_ms": since_ms,
            "deadline_remaining_ms": deadline_remaining_ms,
        }),
        MaintenanceStateView::Maintenance { since_ms } => json!({
            "kind": "maintenance",
            "since_ms": since_ms,
        }),
        MaintenanceStateView::ExitingMaintenance { since_ms } => json!({
            "kind": "exiting_maintenance",
            "since_ms": since_ms,
        }),
        MaintenanceStateView::DrainFailed { since_ms, reason } => json!({
            "kind": "drain_failed",
            "since_ms": since_ms,
            "reason": reason,
        }),
        MaintenanceStateView::Recovery { since_ms } => json!({
            "kind": "recovery",
            "since_ms": since_ms,
        }),
        // `MaintenanceStateView` is `#[non_exhaustive]`. Future
        // variants emit a JSON object with `kind: "unknown"` so
        // older bindings degrade gracefully instead of failing
        // to parse.
        _ => json!({"kind": "unknown"}),
    }
}

fn peer_health_str(h: PeerHealthSnapshot) -> &'static str {
    match h {
        PeerHealthSnapshot::Healthy => "Healthy",
        PeerHealthSnapshot::Degraded => "Degraded",
        PeerHealthSnapshot::Unreachable => "Unreachable",
        _ => "Unknown",
    }
}

fn maintenance_mirror_str(m: MaintenanceMirrorSnapshot) -> &'static str {
    match m {
        MaintenanceMirrorSnapshot::Active => "Active",
        MaintenanceMirrorSnapshot::EnteringMaintenance => "EnteringMaintenance",
        MaintenanceMirrorSnapshot::Maintenance => "Maintenance",
        MaintenanceMirrorSnapshot::ExitingMaintenance => "ExitingMaintenance",
        MaintenanceMirrorSnapshot::DrainFailed => "DrainFailed",
        MaintenanceMirrorSnapshot::Recovery => "Recovery",
        _ => "Unknown",
    }
}

/// Return a JSON-encoded `MetadataView` snapshot. Caller MUST
/// release via `net_meshos_free_string`. Returns NULL on NULL
/// handle or after `graceful_shutdown` (the inner SDK handle is
/// consumed); last-error populated.
#[no_mangle]
pub extern "C" fn net_meshos_metadata(handle: *const NetMeshOsHandle) -> *mut c_char {
    ffi_guard!(ptr::null_mut(), {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return ptr::null_mut();
        };
        let guard = h_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.as_ref() else {
            set_last_error("already_shutdown", "daemon handle was already consumed");
            return ptr::null_mut();
        };
        let json = metadata_view_to_json(inner.metadata());
        match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => {
                set_last_error(
                    "metadata_serialize_failed",
                    "metadata JSON contained an unexpected NUL byte",
                );
                ptr::null_mut()
            }
        }
    })
}

/// Refresh the metadata cache from the runtime's latest snapshot
/// + re-render. Caller MUST release via `net_meshos_free_string`.
#[no_mangle]
pub extern "C" fn net_meshos_refresh_metadata(handle: *mut NetMeshOsHandle) -> *mut c_char {
    ffi_guard!(ptr::null_mut(), {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return ptr::null_mut();
        };
        let mut guard = h_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.as_mut() else {
            set_last_error("already_shutdown", "daemon handle was already consumed");
            return ptr::null_mut();
        };
        let json = metadata_view_to_json(inner.refresh_metadata());
        match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => {
                set_last_error(
                    "metadata_serialize_failed",
                    "metadata JSON contained an unexpected NUL byte",
                );
                ptr::null_mut()
            }
        }
    })
}

/// Free a heap-allocated C string returned by this crate (e.g.
/// from `net_meshos_metadata`). Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_meshos_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(s);
    }
}

/// Publish a capability advertisement update for this daemon.
/// `tags_json_ptr` / `tags_json_len` carry a UTF-8 JSON array of
/// tag strings (`["hardware.gpu", "software.model.foo=…", …]`).
///
/// **Stub today.** The substrate's
/// `MeshOsDaemonHandle::publish_capabilities` returns `Ok(())`
/// without committing to the capability chain — every binding
/// surfaces the same stub semantics so consumers don't write
/// code against a contract the substrate doesn't yet honor.
/// Cuts over transparently when the substrate's chain commit
/// lands.
#[no_mangle]
pub extern "C" fn net_meshos_publish_capabilities(
    handle: *mut NetMeshOsHandle,
    tags_json_ptr: *const c_char,
    tags_json_len: usize,
) -> c_int {
    ffi_guard!(NET_MESHOS_ERR_CALL_FAILED, {
        let Some(h_ref) = (unsafe { handle.as_ref() }) else {
            set_last_error("invalid_argument", "handle pointer is NULL");
            return NET_MESHOS_ERR_NULL;
        };
        let guard = h_ref.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.as_ref() else {
            set_last_error("already_shutdown", "daemon handle was already consumed");
            return NET_MESHOS_ERR_ALREADY_SHUTDOWN;
        };
        let mut set = CapabilitySet::new();
        if tags_json_len > 0 {
            let Some(s) = cstr_to_string(tags_json_ptr, tags_json_len) else {
                set_last_error("invalid_argument", "tags_json is not valid UTF-8");
                return NET_MESHOS_ERR_INVALID_ARG;
            };
            let tags: Vec<String> = match serde_json::from_str(&s) {
                Ok(t) => t,
                Err(e) => {
                    set_last_error(
                        "invalid_argument",
                        &format!("tags_json must be a JSON array of strings: {e}"),
                    );
                    return NET_MESHOS_ERR_INVALID_ARG;
                }
            };
            for tag in tags {
                set = set.add_tag(tag);
            }
        }
        clear_last_error_inner();
        match inner.publish_capabilities(set) {
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

    // =========================================================================
    // Slice 1b — vtable bridge tests
    // =========================================================================

    /// Test fixture state the vtable callbacks dispatch into. Held
    /// alive by the test stack for the duration of the daemon
    /// registration; the `user_ctx` raw pointer borrows from this.
    struct TestState {
        process_count: std::sync::atomic::AtomicUsize,
        snapshot_count: std::sync::atomic::AtomicUsize,
        health_calls: std::sync::atomic::AtomicUsize,
        saturation_calls: std::sync::atomic::AtomicUsize,
        last_payload: parking_lot_or_std::Mutex<Vec<u8>>,
    }

    impl TestState {
        fn new() -> Self {
            Self {
                process_count: std::sync::atomic::AtomicUsize::new(0),
                snapshot_count: std::sync::atomic::AtomicUsize::new(0),
                health_calls: std::sync::atomic::AtomicUsize::new(0),
                saturation_calls: std::sync::atomic::AtomicUsize::new(0),
                last_payload: parking_lot_or_std::Mutex::new(Vec::new()),
            }
        }
    }

    unsafe extern "C" fn vt_process(
        user_ctx: *mut std::ffi::c_void,
        emit_ctx: *mut NetMeshOsProcessEmitCtx,
        _origin_hash: u64,
        _sequence: u64,
        payload_ptr: *const u8,
        payload_len: usize,
    ) -> c_int {
        let state = unsafe { &*(user_ctx as *const TestState) };
        state
            .process_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if payload_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) };
            *state.last_payload.lock().unwrap_or_else(|e| e.into_inner()) = slice.to_vec();
        }
        // Emit two echo outputs so we can verify multi-emit works.
        let out1 = b"out1";
        let out2 = b"out2";
        unsafe { net_meshos_process_emit(emit_ctx, out1.as_ptr(), out1.len()) };
        unsafe { net_meshos_process_emit(emit_ctx, out2.as_ptr(), out2.len()) };
        0
    }

    unsafe extern "C" fn vt_snapshot(
        user_ctx: *mut std::ffi::c_void,
        emit_ctx: *mut NetMeshOsSnapshotEmitCtx,
    ) {
        let state = unsafe { &*(user_ctx as *const TestState) };
        state
            .snapshot_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let payload = b"snapshot-v1";
        unsafe { net_meshos_snapshot_emit(emit_ctx, payload.as_ptr(), payload.len()) };
    }

    unsafe extern "C" fn vt_health(user_ctx: *mut std::ffi::c_void) -> c_int {
        let state = unsafe { &*(user_ctx as *const TestState) };
        state
            .health_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        NET_MESHOS_HEALTH_DEGRADED
    }

    unsafe extern "C" fn vt_saturation(user_ctx: *mut std::ffi::c_void) -> c_float {
        let state = unsafe { &*(user_ctx as *const TestState) };
        state
            .saturation_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        0.7
    }

    #[test]
    fn vtable_register_daemon_succeeds_and_bridge_calls_into_consumer() {
        // SDK + vtable plumbing happens via raw pointers. The
        // TestState backs `user_ctx`; we keep it alive on the
        // test stack so the raw pointer stays valid throughout.
        let state = TestState::new();
        let user_ctx = &state as *const TestState as *mut std::ffi::c_void;

        let vtable = NetMeshOsDaemonVtable {
            process: Some(vt_process),
            snapshot: Some(vt_snapshot),
            restore: None,
            on_control: None,
            health: Some(vt_health),
            saturation: Some(vt_saturation),
        };

        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);

        let name = "vt-echo";
        let (name_ptr, name_len) = (name.as_ptr() as *const c_char, name.len());
        let seed = [0x77u8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        let status = net_meshos_register_daemon_with_vtable(
            sdk,
            name_ptr,
            name_len,
            seed.as_ptr(),
            &vtable,
            user_ctx,
            &mut handle,
        );
        assert_eq!(status, NET_MESHOS_OK);
        assert!(!handle.is_null());

        // Drive the bridge directly. The CDaemonBridge isn't
        // exposed via the FFI, but we can synthesize a CausalEvent
        // and invoke the bridge through the substrate's
        // `MeshDaemon` trait — but that requires reaching into the
        // SDK's internal registry. Instead, exercise the bridge
        // surface end-to-end by constructing a CDaemonBridge
        // directly and calling its trait methods.
        let mut bridge = CDaemonBridge {
            name: name.to_string(),
            vtable,
            user_ctx: UserCtx(user_ctx),
        };
        let event = CausalEvent {
            link: net::adapter::net::state::causal::CausalLink {
                origin_hash: 0xdead_beef,
                horizon_encoded: 0,
                sequence: 1,
                parent_hash: 0,
            },
            payload: bytes::Bytes::from_static(b"hello"),
            received_at: 0,
        };
        let outputs = MeshDaemon::process(&mut bridge, &event).expect("process succeeds");
        assert_eq!(outputs.len(), 2, "expected 2 emitted outputs");
        assert_eq!(outputs[0].as_ref(), b"out1");
        assert_eq!(outputs[1].as_ref(), b"out2");
        assert_eq!(
            state
                .process_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(*state.last_payload.lock().unwrap_or_else(|e| e.into_inner()), b"hello");

        let snap = MeshDaemon::snapshot(&bridge).expect("snapshot returned");
        assert_eq!(snap.as_ref(), b"snapshot-v1");
        assert_eq!(
            state
                .snapshot_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );

        // health + saturation default-overrides
        let h = MeshDaemon::health(&bridge);
        assert!(matches!(
            h,
            net::adapter::net::compute::DaemonHealth::Degraded { .. }
        ));
        let sat = MeshDaemon::saturation(&bridge);
        assert!((sat - 0.7).abs() < 1e-6);
        assert_eq!(
            state
                .health_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            state
                .saturation_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );

        assert_eq!(net_meshos_graceful_shutdown(handle, 10), NET_MESHOS_OK);
        net_meshos_handle_free(handle);
        assert_eq!(net_meshos_sdk_shutdown(sdk), NET_MESHOS_OK);
        net_meshos_sdk_free(sdk);
    }

    #[test]
    fn vtable_register_rejects_null_process_callback() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);
        let vtable = NetMeshOsDaemonVtable {
            process: None, // intentionally invalid
            snapshot: None,
            restore: None,
            on_control: None,
            health: None,
            saturation: None,
        };
        let name = "no-process";
        let (name_ptr, name_len) = (name.as_ptr() as *const c_char, name.len());
        let seed = [0x88u8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        let status = net_meshos_register_daemon_with_vtable(
            sdk,
            name_ptr,
            name_len,
            seed.as_ptr(),
            &vtable,
            ptr::null_mut(),
            &mut handle,
        );
        assert_eq!(status, NET_MESHOS_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_meshos_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");
        net_meshos_clear_last_error();
        net_meshos_sdk_shutdown(sdk);
        net_meshos_sdk_free(sdk);
    }

    // ---- Slice 2: metadata + publish_capabilities ----

    #[test]
    fn metadata_returns_json_with_node_id_and_daemon_fields() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);

        let name = "metadata-test";
        let (name_ptr, name_len) = cstr_ptr(name);
        let seed = [0x55u8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        assert_eq!(
            net_meshos_register_daemon(sdk, name_ptr, name_len, seed.as_ptr(), &mut handle),
            NET_MESHOS_OK,
        );

        let json_ptr = net_meshos_metadata(handle);
        assert!(!json_ptr.is_null());
        let json_str = unsafe { CStr::from_ptr(json_ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        net_meshos_free_string(json_ptr);

        // Parse + inspect.
        let v: serde_json::Value = serde_json::from_str(&json_str).expect("metadata JSON parses");
        assert!(v.get("node_id").is_some(), "metadata missing node_id");
        let expected_id = EntityKeypair::from_bytes(seed).origin_hash();
        assert_eq!(v["daemon_id"].as_u64().unwrap(), expected_id);
        assert_eq!(v["daemon_name"].as_str().unwrap(), "metadata-test");
        // `maintenance_state` is a tagged object.
        let kind = v["maintenance_state"]["kind"].as_str().unwrap();
        assert!(
            [
                "active",
                "entering_maintenance",
                "maintenance",
                "exiting_maintenance",
                "drain_failed",
                "recovery",
                "unknown"
            ]
            .contains(&kind),
            "unexpected maintenance_state kind: {kind}",
        );
        // `peers` is always an array (possibly empty in this fixture).
        assert!(v["peers"].is_array());

        net_meshos_graceful_shutdown(handle, 10);
        net_meshos_handle_free(handle);
        net_meshos_sdk_shutdown(sdk);
        net_meshos_sdk_free(sdk);
    }

    #[test]
    fn metadata_after_shutdown_returns_null_with_kind() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);
        let name = "shutdown-meta";
        let (name_ptr, name_len) = cstr_ptr(name);
        let seed = [0x77u8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        net_meshos_register_daemon(sdk, name_ptr, name_len, seed.as_ptr(), &mut handle);
        net_meshos_graceful_shutdown(handle, 10);
        let json_ptr = net_meshos_metadata(handle);
        assert!(json_ptr.is_null());
        let kind = unsafe { CStr::from_ptr(net_meshos_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "already_shutdown");
        net_meshos_clear_last_error();
        net_meshos_handle_free(handle);
        net_meshos_sdk_shutdown(sdk);
        net_meshos_sdk_free(sdk);
    }

    #[test]
    fn publish_capabilities_stub_returns_ok() {
        let mut sdk: *mut NetMeshOsSdk = ptr::null_mut();
        assert_eq!(net_meshos_sdk_start(0, 0, 0, 0, 0, &mut sdk), NET_MESHOS_OK);
        let name = "cap-test";
        let (name_ptr, name_len) = cstr_ptr(name);
        let seed = [0xAAu8; 32];
        let mut handle: *mut NetMeshOsHandle = ptr::null_mut();
        net_meshos_register_daemon(sdk, name_ptr, name_len, seed.as_ptr(), &mut handle);

        let tags = r#"["hardware.gpu", "software.model.foo=llama-3.1-70b"]"#;
        let (tags_ptr, tags_len) = cstr_ptr(tags);
        let status = net_meshos_publish_capabilities(handle, tags_ptr, tags_len);
        assert_eq!(status, NET_MESHOS_OK);

        // Empty / NULL tags also valid → clears advertisement.
        let status = net_meshos_publish_capabilities(handle, ptr::null(), 0);
        assert_eq!(status, NET_MESHOS_OK);

        // Malformed JSON → invalid_argument.
        let bad = r#"not an array"#;
        let (bad_ptr, bad_len) = cstr_ptr(bad);
        let status = net_meshos_publish_capabilities(handle, bad_ptr, bad_len);
        assert_eq!(status, NET_MESHOS_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_meshos_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");
        net_meshos_clear_last_error();

        net_meshos_graceful_shutdown(handle, 10);
        net_meshos_handle_free(handle);
        net_meshos_sdk_shutdown(sdk);
        net_meshos_sdk_free(sdk);
    }
}
