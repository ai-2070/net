//! C ABI for the compute (MeshDaemon + migration) surface — Stage 6 of
//! `SDK_COMPUTE_SURFACE_PLAN.md`. Consumed by the Go binding at
//! `bindings/go/net/compute.go`.
//!
//! **Sub-step 1** (this file): lifecycle skeleton. A Go caller can
//! build a `DaemonRuntime` bound to an existing `MeshNodeHandle`
//! (from `net::ffi::mesh`), transition it to ready, register a
//! placeholder kind, and shut it down. Event delivery, spawn,
//! migration, and snapshot/restore land in sub-steps 2-4.
//!
//! # Handle model
//!
//! Every Rust object that crosses the FFI boundary is wrapped in a
//! heap-allocated box and handed to the caller as `*mut T`. Go owns
//! the pointer (runtime-finalizer pattern in `compute.go`) and MUST
//! call the matching `_free` function exactly once.
//!
//! # Error codes
//!
//! `c_int` return values (0 = success, < 0 = error) follow the
//! convention established by `net::ffi::mesh` +
//! `net::ffi::cortex`. Structured error information is surfaced via
//! an out-param `*mut *mut c_char` that the caller frees with
//! [`net_compute_free_cstring`].
//!
//! # Tokio runtime
//!
//! This crate owns a lazy `OnceLock<Arc<Runtime>>` for the async
//! SDK calls, mirroring the pattern used by `net::ffi::mesh`. The
//! mesh's internal operations run on their own global runtime; the
//! compute runtime needs its own tokio context for `block_on`
//! because we cross the FFI boundary inside it.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use dashmap::DashMap;
use net::adapter::net::behavior::capability::CapabilityFilter;
use net::adapter::net::channel::ChannelConfigRegistry;
use net::adapter::net::compute::{DaemonError as CoreDaemonError, DaemonHostConfig, MeshDaemon};
use net::adapter::net::state::causal::CausalEvent;
use net::adapter::net::MeshNode;
use net_sdk::compute::{
    DaemonError as SdkDaemonError, DaemonHandle as SdkDaemonHandle,
    DaemonRuntime as SdkDaemonRuntime, MigrationError as SdkMigrationError, MigrationFailureReason,
    MigrationHandle as SdkMigrationHandle, MigrationOpts, MigrationPhase as CoreMigrationPhase,
    StateSnapshot,
};
use net_sdk::mesh::Mesh as SdkMesh;
use net_sdk::Identity as SdkIdentity;
use tokio::runtime::Runtime;

// =========================================================================
// Error codes
// =========================================================================

/// Operation succeeded.
pub const NET_COMPUTE_OK: c_int = 0;
/// Null or invalid pointer passed where a live handle was expected.
pub const NET_COMPUTE_ERR_NULL: c_int = -1;
/// Generic catch-all for errors whose detail is returned via the
/// out-param `*mut *mut c_char` on the same call.
pub const NET_COMPUTE_ERR_CALL_FAILED: c_int = -2;
/// Kind already registered on `net_compute_register_factory`.
pub const NET_COMPUTE_ERR_DUPLICATE_KIND: c_int = -3;

// =========================================================================
// Structured error formatter (mirrors NAPI / PyO3)
// =========================================================================

/// Format an SDK `DaemonError` into a stable machine-readable
/// string for the Go side to parse. Migration failures get the
/// `migration: <kind>[: detail]` prefix so `MigrationErrorKind`
/// on the Go side can dispatch on the kind without regex parsing.
/// Everything else falls through to the SDK's Display.
///
/// Matches the format used by `format_migration_failure_reason` /
/// `format_migration_error` in the NAPI / Python bindings.
fn format_sdk_error(e: &SdkDaemonError) -> String {
    match e {
        SdkDaemonError::MigrationFailed(reason) => {
            format!("migration: {}", format_migration_failure_reason(reason))
        }
        SdkDaemonError::Migration(mig_err) => {
            format!("migration: {}", format_migration_error(mig_err))
        }
        other => other.to_string(),
    }
}

fn format_migration_failure_reason(reason: &MigrationFailureReason) -> String {
    match reason {
        MigrationFailureReason::NotReady => "not-ready".to_string(),
        MigrationFailureReason::FactoryNotFound => "factory-not-found".to_string(),
        MigrationFailureReason::ComputeNotSupported => "compute-not-supported".to_string(),
        MigrationFailureReason::StateFailed(msg) => format!("state-failed: {msg}"),
        MigrationFailureReason::AlreadyMigrating => "already-migrating".to_string(),
        MigrationFailureReason::IdentityTransportFailed(msg) => {
            format!("identity-transport-failed: {msg}")
        }
        MigrationFailureReason::NotReadyTimeout { attempts } => {
            format!("not-ready-timeout: {attempts}")
        }
    }
}

fn format_migration_error(err: &SdkMigrationError) -> String {
    match err {
        SdkMigrationError::DaemonNotFound(origin) => format!("daemon-not-found: {origin:#x}"),
        SdkMigrationError::TargetUnavailable(node) => format!("target-unavailable: {node:#x}"),
        SdkMigrationError::NoTargetAvailable => "no-target-available".to_string(),
        SdkMigrationError::StateFailed(msg) => format!("state-failed: {msg}"),
        SdkMigrationError::AlreadyMigrating(origin) => format!("already-migrating: {origin:#x}"),
        SdkMigrationError::WrongPhase { expected, got } => {
            format!("wrong-phase: {expected:?}: {got:?}")
        }
        SdkMigrationError::SnapshotTooLarge { size, max } => {
            format!("snapshot-too-large: {size}: {max}")
        }
    }
}

fn migration_phase_str(phase: CoreMigrationPhase) -> &'static str {
    match phase {
        CoreMigrationPhase::Snapshot => "snapshot",
        CoreMigrationPhase::Transfer => "transfer",
        CoreMigrationPhase::Restore => "restore",
        CoreMigrationPhase::Replay => "replay",
        CoreMigrationPhase::Cutover => "cutover",
        CoreMigrationPhase::Complete => "complete",
    }
}

// =========================================================================
// Shared tokio runtime
// =========================================================================

/// Lazy global runtime. Matches the pattern in `net::ffi::mesh` /
/// `net::ffi::cortex`. Panics on initial construction failure —
/// the process is unusable without a runtime, so failing fast is
/// preferable to silently returning errors on every subsequent
/// call.
fn runtime() -> &'static Arc<Runtime> {
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("net-compute-ffi")
                .build()
                .expect("failed to construct compute-ffi tokio runtime"),
        )
    })
}

// =========================================================================
// Handle types
// =========================================================================

/// Opaque pointer exposed to Go. Wraps the SDK's `DaemonRuntime`
/// plus the shared `factories` set the compute-ffi crate uses to
/// dedupe `register_factory` calls on this side of the FFI.
pub struct DaemonRuntimeHandle {
    inner: Arc<SdkDaemonRuntime>,
    /// Tracks registered kinds (same role as the `factories`
    /// DashMap on NAPI / PyO3). Sub-step 2 will swap the value
    /// type to a Go-callback trampoline table.
    factories: Arc<DashMap<String, ()>>,
    /// Monotonic, process-unique identifier used to scope the
    /// Go-side factory map to *this* runtime. Without it, two
    /// `DaemonRuntime`s in the same process that register the
    /// same `kind` would collide on the process-global Go map
    /// and the second registration would overwrite the first —
    /// a migrated-in daemon could then reconstruct using the
    /// wrong implementation. We pass `runtime_id` through the
    /// factory trampoline so Go looks up `(runtime_id, kind)`
    /// instead of `kind` alone.
    runtime_id: u64,
}

/// Monotonic counter for `DaemonRuntimeHandle::runtime_id`. Starts
/// at 1 so `0` is reserved as a sentinel for "no runtime" (defensive
/// against uninitialized trampoline calls).
static NEXT_RUNTIME_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

// =========================================================================
// Free helpers — misc out-of-band allocations
// =========================================================================

/// Free a CString previously returned out-of-band by this crate
/// (e.g., structured error detail). Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_compute_free_cstring(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(s);
    }
}

// =========================================================================
// DaemonRuntime lifecycle
// =========================================================================

/// Build a new `DaemonRuntime` from Arc-cloned handles to the
/// source `MeshNode` and its shared `ChannelConfigRegistry`. The
/// pointers MUST come from `net_mesh_arc_clone` /
/// `net_mesh_channel_configs_arc_clone` (defined in
/// `net::ffi::mesh`).
///
/// Ownership semantics:
/// - `node_arc` and `channel_configs_arc` are CONSUMED by this
///   call — the compute runtime takes their `Arc` content via
///   `Box::from_raw` and re-boxes it into its own fields. Callers
///   MUST NOT free them after a successful call.
/// - On failure (either input NULL), the pointers are left intact
///   so the caller's deferred `_free` stays correct.
///
/// Returns a boxed `DaemonRuntimeHandle` — caller owns it and
/// frees with [`net_compute_runtime_free`]. Returns NULL if any
/// input is NULL.
#[no_mangle]
pub extern "C" fn net_compute_runtime_new(
    node_arc: *mut Arc<MeshNode>,
    channel_configs_arc: *mut Arc<ChannelConfigRegistry>,
) -> *mut DaemonRuntimeHandle {
    if node_arc.is_null() || channel_configs_arc.is_null() {
        return std::ptr::null_mut();
    }
    // Take ownership of the boxed Arcs — the caller's LDFLAGS
    // expectation (paired `_free`) is handled by the fact that we
    // document consumption in the docstring. Go callers release
    // these pointers from their finalizer list after the call
    // succeeds.
    let node = unsafe { *Box::from_raw(node_arc) };
    let cc = unsafe { *Box::from_raw(channel_configs_arc) };

    let sdk_mesh = SdkMesh::from_node_arc(node, cc, None);
    let sdk_rt = SdkDaemonRuntime::new(Arc::new(sdk_mesh));

    let runtime_id = NEXT_RUNTIME_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Box::into_raw(Box::new(DaemonRuntimeHandle {
        inner: Arc::new(sdk_rt),
        factories: Arc::new(DashMap::new()),
        runtime_id,
    }))
}

/// Return the monotonic runtime identifier assigned at
/// `net_compute_runtime_new`. Go uses this to scope its factory
/// map to this runtime so two runtimes in the same process can
/// register the same kind with different factories without
/// collision.
///
/// Returns `0` on NULL input.
#[no_mangle]
pub extern "C" fn net_compute_runtime_id(handle: *const DaemonRuntimeHandle) -> u64 {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    h.runtime_id
}

/// Free a runtime handle. The underlying `MeshNode` stays alive
/// so long as the caller holds another `Arc` to it (typically via
/// its own `MeshNodeHandle`). Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_compute_runtime_free(handle: *mut DaemonRuntimeHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(handle);
    }
}

/// Transition the runtime to `Ready`. Installs the migration
/// subprotocol handler on the underlying mesh. Idempotent on an
/// already-ready runtime.
///
/// On failure, writes a heap-allocated `char*` error detail to
/// `*err_out` (caller frees with [`net_compute_free_cstring`]).
/// Returns [`NET_COMPUTE_OK`] / [`NET_COMPUTE_ERR_*`].
#[no_mangle]
pub extern "C" fn net_compute_runtime_start(
    handle: *mut DaemonRuntimeHandle,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let inner = h.inner.clone();
    let rt = runtime();
    let res = rt.block_on(async move { inner.start().await });
    match res {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Tear down the runtime. Drains daemons + factory registrations +
/// uninstalls the migration handler. The underlying `MeshNode` is
/// untouched.
#[no_mangle]
pub extern "C" fn net_compute_runtime_shutdown(
    handle: *mut DaemonRuntimeHandle,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let inner = h.inner.clone();
    let factories = h.factories.clone();
    let rt = runtime();
    let res = rt.block_on(async move { inner.shutdown().await });
    match res {
        Ok(()) => {
            factories.clear();
            NET_COMPUTE_OK
        }
        Err(e) => {
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Return `1` if the runtime has transitioned to `Ready` and not
/// yet begun shutting down, else `0`. Returns [`NET_COMPUTE_ERR_NULL`]
/// on a NULL handle.
#[no_mangle]
pub extern "C" fn net_compute_runtime_is_ready(handle: *mut DaemonRuntimeHandle) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if h.inner.is_ready() {
        1
    } else {
        0
    }
}

/// Return the number of daemons currently registered on this
/// runtime. Returns `-1` on NULL handle (cast of
/// [`NET_COMPUTE_ERR_NULL`]).
#[no_mangle]
pub extern "C" fn net_compute_runtime_daemon_count(handle: *mut DaemonRuntimeHandle) -> i64 {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL as i64;
    };
    h.inner.daemon_count() as i64
}

/// Register a factory kind on the runtime. Enables `spawn` and
/// migration-target setup (`expect_migration` /
/// `register_migration_target_identity`) for that kind.
///
/// **Migration-target reconstruction caveat:** the SDK-side
/// factory closure mirrored here returns a
/// `ReconstructionErrorBridge` fallback — a placeholder
/// `MeshDaemon` whose `restore` / `process` return a typed error
/// identifying the root cause (e.g. "register via
/// RegisterFactoryFunc to enable reconstruction"). A daemon
/// migrated INTO a Go target today therefore fails visibly on
/// first event rather than silently accepting and dropping them.
/// Use [`net_compute_register_factory_with_func`] instead for the
/// full migration-capable path.
///
/// Returns [`NET_COMPUTE_OK`] on success,
/// [`NET_COMPUTE_ERR_DUPLICATE_KIND`] if the kind was already
/// registered, or [`NET_COMPUTE_ERR_NULL`] for invalid arguments.
///
/// # Safety
///
/// `kind_ptr` must point to `kind_len` bytes of valid UTF-8. NUL
/// termination is NOT required — `kind_len` is the exact byte
/// count.
#[no_mangle]
pub extern "C" fn net_compute_register_factory(
    handle: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if kind_ptr.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let kind = match cstr_to_string(kind_ptr, kind_len) {
        Some(s) => s,
        None => return NET_COMPUTE_ERR_NULL,
    };
    use dashmap::mapref::entry::Entry;
    match h.factories.entry(kind.clone()) {
        Entry::Occupied(_) => return NET_COMPUTE_ERR_DUPLICATE_KIND,
        Entry::Vacant(slot) => {
            slot.insert(());
        }
    }

    // Mirror into the SDK factory registry with a loud-failure
    // fallback bridge. Without this, `expect_migration` /
    // `register_migration_target_identity` reject with
    // `FactoryNotFound`. The bridge fails on first `restore` /
    // `process` with a typed error so a migration that lands on
    // a kind registered via `RegisterFactory` (without Func) is
    // visibly rejected rather than silently accepting events.
    let kind_for_bridge = kind.clone();
    if let Err(e) = h.inner.register_factory(&kind, move || {
        Box::new(ReconstructionErrorBridge::new(
            kind_for_bridge.clone(),
            "kind registered via RegisterFactory (without Func); use RegisterFactoryFunc to enable migration-target reconstruction",
        )) as Box<dyn MeshDaemon>
    }) {
        // SDK rejected the mirror — roll back our kind-set entry
        // so the two registries stay in sync.
        h.factories.remove(&kind);
        // Discriminate between the two realistic failure modes.
        // A concurrent `Shutdown()` is just as likely as a
        // duplicate-kind collision (the mirror+FFI-side entry
        // would have been caught already by the `Entry::Occupied`
        // arm above), so `ShuttingDown` must not masquerade as
        // a duplicate — callers need to tell "already registered"
        // apart from "runtime is gone."
        return match e {
            SdkDaemonError::FactoryAlreadyRegistered(_) => NET_COMPUTE_ERR_DUPLICATE_KIND,
            SdkDaemonError::ShuttingDown | SdkDaemonError::NotReady => NET_COMPUTE_ERR_CALL_FAILED,
            _ => NET_COMPUTE_ERR_CALL_FAILED,
        };
    }
    NET_COMPUTE_OK
}

/// Register a factory kind with a real Go-side factory function
/// so migration-target reconstruction builds a fresh user daemon
/// instead of falling back to `ReconstructionErrorBridge`. The Go
/// caller supplied the factory via
/// [`RegisterFactoryFunc`](../../../net/net/migration.go);
/// we look it up via the dispatcher's `factory` trampoline on
/// every reconstruction. The trampoline receives the runtime id
/// (from `net_compute_runtime_id`) so the Go side can scope its
/// factory map per-runtime — two runtimes in the same process
/// can register the same `kind` without colliding.
///
/// Use this instead of [`net_compute_register_factory`] when you
/// want migrated-in daemons on this target to actually run user
/// code. Same duplicate-kind semantics as the plain variant.
#[no_mangle]
pub extern "C" fn net_compute_register_factory_with_func(
    handle: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if kind_ptr.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let kind = match cstr_to_string(kind_ptr, kind_len) {
        Some(s) => s,
        None => return NET_COMPUTE_ERR_NULL,
    };
    use dashmap::mapref::entry::Entry;
    match h.factories.entry(kind.clone()) {
        Entry::Occupied(_) => return NET_COMPUTE_ERR_DUPLICATE_KIND,
        Entry::Vacant(slot) => {
            slot.insert(());
        }
    }

    // SDK closure reaches back into Go via the factory trampoline
    // to build a fresh daemon per invocation. Captures the runtime
    // id so Go can scope its factory map to this runtime and avoid
    // the process-global overwrite bug (two runtimes registering
    // the same kind would otherwise collide).
    let kind_for_closure = kind.clone();
    let runtime_id = h.runtime_id;
    let closure = move || -> Box<dyn MeshDaemon> {
        let Some(d) = DISPATCHER.get() else {
            // Failing loudly beats silent-noop: a reconstructed
            // daemon that processes zero events is harder to spot
            // than a typed error. See `ReconstructionErrorBridge`.
            let reason = "Go dispatcher not registered (net_compute_set_dispatcher never called)";
            eprintln!("net-compute-ffi: kind '{kind_for_closure}': {reason}");
            return Box::new(ReconstructionErrorBridge::new(
                kind_for_closure.clone(),
                reason,
            ));
        };
        let mut daemon_id: u64 = 0;
        let code = unsafe {
            (d.factory)(
                runtime_id,
                kind_for_closure.as_ptr() as *const c_char,
                kind_for_closure.len(),
                &mut daemon_id,
            )
        };
        if code != NET_COMPUTE_OK {
            let reason = format!("Go factory returned error code {code}");
            eprintln!("net-compute-ffi: kind '{kind_for_closure}': {reason}");
            return Box::new(ReconstructionErrorBridge::new(
                kind_for_closure.clone(),
                reason,
            ));
        }
        Box::new(GoBridge {
            name: kind_for_closure.clone(),
            daemon_id,
        })
    };

    if let Err(e) = h.inner.register_factory(&kind, closure) {
        h.factories.remove(&kind);
        // Same discrimination as `net_compute_register_factory`
        // above — `ShuttingDown` / `NotReady` must not masquerade
        // as duplicate-kind so the Go layer can surface
        // `ErrRuntimeShutDown` to the caller.
        return match e {
            SdkDaemonError::FactoryAlreadyRegistered(_) => NET_COMPUTE_ERR_DUPLICATE_KIND,
            SdkDaemonError::ShuttingDown | SdkDaemonError::NotReady => NET_COMPUTE_ERR_CALL_FAILED,
            _ => NET_COMPUTE_ERR_CALL_FAILED,
        };
    }
    NET_COMPUTE_OK
}

// =========================================================================
// Callback dispatcher — Go side registers these once
// =========================================================================

/// C-ABI type: invoke Go's `Process` for the daemon identified by
/// `daemon_id`. Outputs push into the `OutputsVec` handed in;
/// return code `0` on success, non-zero if the Go side failed.
pub type ProcessFn = unsafe extern "C" fn(
    daemon_id: u64,
    origin_hash: u32,
    sequence: u64,
    payload_ptr: *const u8,
    payload_len: usize,
    outputs: *mut OutputsVec,
) -> c_int;

/// C-ABI type: invoke Go's `Snapshot` for `daemon_id`. On success,
/// writes either `(NULL, 0)` (stateless) or a heap-allocated
/// `(ptr, len)` pair that the Rust side frees with
/// [`net_compute_snapshot_bytes_free`]. Return `0` on success;
/// non-zero if Snapshot threw.
pub type SnapshotFn =
    unsafe extern "C" fn(daemon_id: u64, out_ptr: *mut *mut u8, out_len: *mut usize) -> c_int;

/// C-ABI type: invoke Go's `Restore` for `daemon_id`. Returns `0`
/// on success, non-zero if Restore threw.
pub type RestoreFn =
    unsafe extern "C" fn(daemon_id: u64, state_ptr: *const u8, state_len: usize) -> c_int;

/// C-ABI type: Go releases its registry entry for `daemon_id`.
/// Called when the Rust side drops the last reference to the
/// bridge (e.g., daemon stopped, migration source cutover).
pub type FreeFn = unsafe extern "C" fn(daemon_id: u64);

/// C-ABI type: migration-target factory trampoline. Rust invokes
/// it on every migration-target reconstruction via the factory
/// closure mirrored into the SDK by
/// [`net_compute_register_factory_with_func`]; the Go side looks
/// up the registered factory by `(runtime_id, kind)`, invokes it
/// to build a fresh `MeshDaemon`, inserts it into the daemon
/// registry, and writes the new `daemon_id` into `*out_daemon_id`.
/// The `runtime_id` argument scopes the lookup so two runtimes in
/// the same process that registered the same kind don't collide
/// (see `net_compute_runtime_id`).
///
/// Returns `NET_COMPUTE_OK` on success; non-zero means the Go
/// side couldn't build a daemon (no factory registered, factory
/// panicked, etc.). The Rust bridge installs a
/// `ReconstructionErrorBridge` on failure — the migration's next
/// `restore` / `process` returns a typed error carrying the
/// failure reason.
pub type FactoryFn = unsafe extern "C" fn(
    runtime_id: u64,
    kind_ptr: *const c_char,
    kind_len: usize,
    out_daemon_id: *mut u64,
) -> c_int;

/// The five trampolines registered once from Go's `init()`. Stored
/// in a `OnceLock` so invocation is lock-free.
struct DispatcherFns {
    process: ProcessFn,
    snapshot: SnapshotFn,
    restore: RestoreFn,
    free: FreeFn,
    factory: FactoryFn,
}

static DISPATCHER: OnceLock<DispatcherFns> = OnceLock::new();

/// Register the Go-side dispatcher trampolines. MUST be called
/// exactly once before any `net_compute_spawn` or
/// `net_compute_deliver`. A second call is ignored (the first
/// registration wins — `OnceLock` semantics).
///
/// # Safety
///
/// The five function pointers MUST have C linkage, must be valid
/// for the remaining lifetime of the process, and must follow the
/// contracts of [`ProcessFn`] / [`SnapshotFn`] / [`RestoreFn`] /
/// [`FreeFn`] / [`FactoryFn`]. Passing NULL is a hard error.
#[no_mangle]
pub extern "C" fn net_compute_set_dispatcher(
    process: Option<ProcessFn>,
    snapshot: Option<SnapshotFn>,
    restore: Option<RestoreFn>,
    free: Option<FreeFn>,
    factory: Option<FactoryFn>,
) -> c_int {
    let Some(process) = process else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(snapshot) = snapshot else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(restore) = restore else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(free) = free else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(factory) = factory else {
        return NET_COMPUTE_ERR_NULL;
    };
    let _ = DISPATCHER.set(DispatcherFns {
        process,
        snapshot,
        restore,
        free,
        factory,
    });
    NET_COMPUTE_OK
}

// =========================================================================
// OutputsVec — growable container for `Vec<Bytes>` populated by Go
// =========================================================================

/// Opaque buffer the Rust bridge hands to Go during a `process`
/// callback. Go calls [`net_compute_outputs_push`] once per output
/// payload.
///
/// Exposed as `*mut OutputsVec` to C for pass-through only — fields
/// stay private so the Rust side controls the memory lifecycle.
#[repr(C)]
pub struct OutputsVec {
    inner: Vec<Bytes>,
}

/// Push a single output payload into the vec. Copies `len` bytes
/// from `data` into a freshly-allocated `Bytes`. Returns
/// [`NET_COMPUTE_OK`] on success or [`NET_COMPUTE_ERR_NULL`] on bad
/// input.
///
/// # Safety
///
/// `vec` must point to an `OutputsVec` owned by the Rust bridge
/// (lifetime bound to the caller's `net_compute_deliver` /
/// `process` callback frame). `data` must be a valid read-only
/// pointer to at least `len` bytes.
#[no_mangle]
pub extern "C" fn net_compute_outputs_push(
    vec: *mut OutputsVec,
    data: *const u8,
    len: usize,
) -> c_int {
    if vec.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    if len > 0 && data.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let v = unsafe { &mut *vec };
    let slice = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    v.inner.push(Bytes::copy_from_slice(slice));
    NET_COMPUTE_OK
}

/// Free a `(ptr, len)` snapshot payload heap-allocated by Go
/// inside [`SnapshotFn`]. Called by the Rust bridge once the
/// `Bytes` copy is taken.
///
/// # Safety
///
/// Pairs with Go-side `C.malloc`-equivalent allocations. Go
/// snapshot trampolines must allocate via `C.malloc` and hand us
/// the pointer; we free via `libc::free`. NULL is a no-op.
#[no_mangle]
pub extern "C" fn net_compute_snapshot_bytes_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    // The Go side allocated via `C.malloc`; `libc::free` matches.
    unsafe {
        libc::free(ptr as *mut std::ffi::c_void);
    }
}

// =========================================================================
// GoBridge — `MeshDaemon` impl driven by the registered dispatcher
// =========================================================================

/// Daemon bridge holding a `daemon_id` (the Go-side registry key)
/// and the kind name for debug. `MeshDaemon` impls invoke the
/// registered dispatcher trampolines with this ID.
///
/// Drop triggers [`FreeFn`] so the Go side can release its entry.
struct GoBridge {
    name: String,
    daemon_id: u64,
}

impl Drop for GoBridge {
    fn drop(&mut self) {
        if let Some(d) = DISPATCHER.get() {
            unsafe { (d.free)(self.daemon_id) };
        }
    }
}

impl MeshDaemon for GoBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    fn process(&mut self, event: &CausalEvent) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        let Some(d) = DISPATCHER.get() else {
            return Err(CoreDaemonError::ProcessFailed(
                "Go dispatcher not registered — call net_compute_set_dispatcher at init"
                    .to_string(),
            ));
        };
        let mut outputs = OutputsVec { inner: Vec::new() };
        let code = unsafe {
            (d.process)(
                self.daemon_id,
                event.link.origin_hash,
                event.link.sequence,
                event.payload.as_ptr(),
                event.payload.len(),
                &mut outputs,
            )
        };
        if code != NET_COMPUTE_OK {
            return Err(CoreDaemonError::ProcessFailed(format!(
                "Go process callback returned {code}"
            )));
        }
        Ok(outputs.inner)
    }

    fn snapshot(&self) -> Option<Bytes> {
        let d = DISPATCHER.get()?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: usize = 0;
        let code = unsafe { (d.snapshot)(self.daemon_id, &mut ptr, &mut len) };
        if code != NET_COMPUTE_OK {
            eprintln!("GoBridge::snapshot: dispatcher returned {code}; treating as None");
            return None;
        }
        if ptr.is_null() || len == 0 {
            return None;
        }
        // Copy the Go-allocated buffer into a Rust `Bytes` and
        // free the original so Go's malloc pool stays tidy.
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        let out = Bytes::copy_from_slice(slice);
        unsafe { libc::free(ptr as *mut std::ffi::c_void) };
        Some(out)
    }

    fn restore(&mut self, state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        let Some(d) = DISPATCHER.get() else {
            return Err(CoreDaemonError::RestoreFailed(
                "Go dispatcher not registered".to_string(),
            ));
        };
        let code = unsafe { (d.restore)(self.daemon_id, state.as_ptr(), state.len()) };
        if code != NET_COMPUTE_OK {
            return Err(CoreDaemonError::RestoreFailed(format!(
                "Go restore callback returned {code}"
            )));
        }
        Ok(())
    }
}

// =========================================================================
// DaemonHandle — thin C-ABI wrapper around the SDK handle
// =========================================================================

/// Opaque handle returned by `net_compute_spawn`. Holds the SDK
/// handle plus the `origin_hash` / `entity_id` accessors the Go
/// side surfaces as methods.
pub struct DaemonHandleC {
    origin_hash: u32,
    entity_id: [u8; 32],
    #[allow(dead_code)]
    inner: SdkDaemonHandle,
}

/// Read the daemon's `origin_hash`. Returns 0 on NULL handle
/// (callers can't hit this normally — a spawned daemon always
/// has a non-zero origin_hash).
#[no_mangle]
pub extern "C" fn net_compute_daemon_handle_origin_hash(h: *const DaemonHandleC) -> u32 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.origin_hash
}

/// Copy the 32-byte `entity_id` into `out[0..32]`. Returns
/// [`NET_COMPUTE_OK`] / [`NET_COMPUTE_ERR_NULL`].
#[no_mangle]
pub extern "C" fn net_compute_daemon_handle_entity_id(
    h: *const DaemonHandleC,
    out: *mut u8,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(h.entity_id.as_ptr(), out, 32);
    }
    NET_COMPUTE_OK
}

/// Free the handle. The daemon itself keeps running — call
/// [`net_compute_runtime_stop`] first to tear it down.
#[no_mangle]
pub extern "C" fn net_compute_daemon_handle_free(h: *mut DaemonHandleC) {
    if h.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(h);
    }
}

// =========================================================================
// Spawn / stop / deliver
// =========================================================================

/// Spawn a Go daemon. `daemon_id` is the Go-side registry key that
/// the dispatcher trampolines will use to look up the daemon
/// instance on every `process` / `snapshot` / `restore` callback.
///
/// `identity_seed` must point to 32 bytes of ed25519 seed.
/// `kind_ptr` + `kind_len`: UTF-8 name the caller registered via
/// [`net_compute_register_factory`] (sub-step 2 doesn't actually
/// require prior registration; the lookup happens on the SDK side
/// via `spawn_with_daemon`, which takes the bridge directly).
///
/// On success, writes the `DaemonHandleC*` to `*out_handle` and
/// returns [`NET_COMPUTE_OK`]. On failure, leaves `*out_handle =
/// NULL` and populates `*err_out` with a structured detail.
///
/// # Safety
///
/// See field docs. `daemon_id` must match a live Go-side entry;
/// we don't validate but the dispatcher trampolines will treat an
/// unknown ID as a daemon-side error.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_spawn(
    handle: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    identity_seed: *const u8,
    daemon_id: u64,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    out_handle: *mut *mut DaemonHandleC,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if kind_ptr.is_null() || identity_seed.is_null() || out_handle.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };
    // Copy the 32-byte seed — the SDK `Identity::from_seed` takes
    // `[u8; 32]` by value and we don't want to hold onto Go's
    // backing memory past the call.
    let mut seed = [0u8; 32];
    unsafe {
        std::ptr::copy_nonoverlapping(identity_seed, seed.as_mut_ptr(), 32);
    }
    let sdk_identity = SdkIdentity::from_seed(seed);

    let mut cfg = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        cfg.max_log_entries = max_log_entries;
    }

    let bridge = Box::new(GoBridge {
        name: kind.clone(),
        daemon_id,
    });
    // kind_factory for migration-target reconstruction. Uses a
    // loud-failure bridge: if a migration lands here before the
    // caller has also installed `RegisterFactoryFunc` for this
    // kind, the migration fails visibly on first `restore` /
    // `process` rather than silently accepting events into a
    // noop.
    let kind_for_fallback = kind.clone();
    let kind_factory = move || -> Box<dyn MeshDaemon> {
        Box::new(ReconstructionErrorBridge::new(
            kind_for_fallback.clone(),
            "spawn-side kind registration does not route to a Go factory; call DaemonRuntime.RegisterFactoryFunc before the migration lands",
        ))
    };

    let inner = h.inner.clone();
    let rt = runtime();
    let result = rt.block_on(async move {
        inner
            .spawn_with_daemon(sdk_identity, cfg, bridge, kind_factory)
            .await
    });
    match result {
        Ok(sdk_handle) => {
            let origin_hash = sdk_handle.origin_hash;
            let entity_id = *sdk_handle.entity_id.as_bytes();
            let boxed = Box::new(DaemonHandleC {
                origin_hash,
                entity_id,
                inner: sdk_handle,
            });
            unsafe {
                *out_handle = Box::into_raw(boxed);
            }
            NET_COMPUTE_OK
        }
        Err(e) => {
            unsafe {
                *out_handle = std::ptr::null_mut();
            }
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Stop a daemon by `origin_hash`. Idempotent during shutdown.
/// Returns [`NET_COMPUTE_OK`] / [`NET_COMPUTE_ERR_*`].
#[no_mangle]
pub extern "C" fn net_compute_runtime_stop(
    handle: *mut DaemonRuntimeHandle,
    origin_hash: u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let inner = h.inner.clone();
    let rt = runtime();
    match rt.block_on(async move { inner.stop(origin_hash).await }) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Take a snapshot of the daemon identified by `origin_hash`.
/// Returns the daemon's serialized state bytes in a one-element
/// `OutputsVec`, or an empty `OutputsVec` if the daemon is
/// stateless (no `snapshot` method, or snapshot returned nil).
///
/// The wire format is the core's `StateSnapshot::to_bytes`
/// encoding — round-trip via [`net_compute_spawn_from_snapshot`].
///
/// Caller reads via `net_compute_outputs_len` / `_at` and frees
/// via `net_compute_outputs_free`, same as `deliver`.
#[no_mangle]
pub extern "C" fn net_compute_runtime_snapshot(
    handle: *mut DaemonRuntimeHandle,
    origin_hash: u32,
    out_outputs: *mut *mut OutputsVec,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_outputs.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let inner = h.inner.clone();
    let rt = runtime();
    let result = rt.block_on(async move { inner.snapshot(origin_hash).await });
    match result {
        Ok(opt) => {
            let vec = match opt {
                Some(snap) => OutputsVec {
                    inner: vec![Bytes::from(snap.to_bytes())],
                },
                None => OutputsVec { inner: Vec::new() },
            };
            unsafe {
                *out_outputs = Box::into_raw(Box::new(vec));
            }
            NET_COMPUTE_OK
        }
        Err(e) => {
            unsafe {
                *out_outputs = std::ptr::null_mut();
            }
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Spawn a daemon from a previously-taken snapshot. Parallels
/// [`net_compute_spawn`] but seeds the daemon's initial state
/// from `snapshot_bytes` by calling its `restore` method before
/// any events land.
///
/// `snapshot_bytes` MUST be the exact buffer returned by a prior
/// [`net_compute_runtime_snapshot`] call; corrupted bytes surface
/// as `snapshot decode failed`.
///
/// Identity check: the snapshot's `entity_id` must match the
/// caller's identity — mismatch surfaces as
/// `daemon: snapshot identity mismatch`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_spawn_from_snapshot(
    handle: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    identity_seed: *const u8,
    snapshot_ptr: *const u8,
    snapshot_len: usize,
    daemon_id: u64,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    out_handle: *mut *mut DaemonHandleC,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if kind_ptr.is_null() || identity_seed.is_null() || out_handle.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    if snapshot_len > 0 && snapshot_ptr.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };

    // Decode the snapshot up front so a corrupted buffer surfaces
    // with a clean message before we build a bridge / register a
    // daemon_id.
    let snap_bytes = if snapshot_len == 0 {
        &[] as &[u8]
    } else {
        unsafe { std::slice::from_raw_parts(snapshot_ptr, snapshot_len) }
    };
    let Some(snapshot_decoded) = StateSnapshot::from_bytes(snap_bytes) else {
        write_err(err_out, "snapshot decode failed");
        return NET_COMPUTE_ERR_CALL_FAILED;
    };

    let mut seed = [0u8; 32];
    unsafe {
        std::ptr::copy_nonoverlapping(identity_seed, seed.as_mut_ptr(), 32);
    }
    let sdk_identity = SdkIdentity::from_seed(seed);

    let mut cfg = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        cfg.max_log_entries = max_log_entries;
    }

    let bridge = Box::new(GoBridge {
        name: kind.clone(),
        daemon_id,
    });
    // Same loud-failure fallback rationale as the plain-spawn
    // path above — see the comment there.
    let kind_for_fallback = kind.clone();
    let kind_factory = move || -> Box<dyn MeshDaemon> {
        Box::new(ReconstructionErrorBridge::new(
            kind_for_fallback.clone(),
            "spawn-from-snapshot kind registration does not route to a Go factory; call DaemonRuntime.RegisterFactoryFunc before the migration lands",
        ))
    };

    let inner = h.inner.clone();
    let rt = runtime();
    let result = rt.block_on(async move {
        inner
            .spawn_from_snapshot_with_daemon(
                sdk_identity,
                snapshot_decoded,
                cfg,
                bridge,
                kind_factory,
            )
            .await
    });
    match result {
        Ok(sdk_handle) => {
            let origin_hash = sdk_handle.origin_hash;
            let entity_id = *sdk_handle.entity_id.as_bytes();
            let boxed = Box::new(DaemonHandleC {
                origin_hash,
                entity_id,
                inner: sdk_handle,
            });
            unsafe {
                *out_handle = Box::into_raw(boxed);
            }
            NET_COMPUTE_OK
        }
        Err(e) => {
            unsafe {
                *out_handle = std::ptr::null_mut();
            }
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

// =========================================================================
// Migration — start / expect / register_target_identity / phase
// =========================================================================

/// Opaque handle returned by `net_compute_start_migration*`. Wraps
/// the SDK `MigrationHandle` plus cached `origin_hash` /
/// `source_node` / `target_node` for zero-cost accessors from Go.
pub struct MigrationHandleC {
    origin_hash: u32,
    source_node: u64,
    target_node: u64,
    inner: SdkMigrationHandle,
}

/// Free a migration handle. Dropping the Go-side handle does NOT
/// cancel the migration — the orchestrator keeps driving it in
/// the background. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_compute_migration_handle_free(h: *mut MigrationHandleC) {
    if h.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(h);
    }
}

#[no_mangle]
pub extern "C" fn net_compute_migration_handle_origin_hash(h: *const MigrationHandleC) -> u32 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.origin_hash
}

#[no_mangle]
pub extern "C" fn net_compute_migration_handle_source_node(h: *const MigrationHandleC) -> u64 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.source_node
}

#[no_mangle]
pub extern "C" fn net_compute_migration_handle_target_node(h: *const MigrationHandleC) -> u64 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.target_node
}

/// Return the current migration phase as a CString, or NULL if
/// the migration has left the orchestrator's records (terminal
/// success or abort). Caller frees via
/// [`net_compute_free_cstring`].
#[no_mangle]
pub extern "C" fn net_compute_migration_handle_phase(h: *const MigrationHandleC) -> *mut c_char {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return std::ptr::null_mut();
    };
    match h.inner.phase() {
        Some(p) => CString::new(migration_phase_str(p))
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// Block until the migration reaches a terminal state. Returns
/// [`NET_COMPUTE_OK`] on `Complete`; on abort/failure writes a
/// structured `migration: <kind>[: detail]` body to `*err_out`.
#[no_mangle]
pub extern "C" fn net_compute_migration_handle_wait(
    h: *mut MigrationHandleC,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let handle = h.inner.clone();
    let rt = runtime();
    match rt.block_on(async move { handle.wait().await }) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &format_sdk_error(&e));
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Like `wait` with a caller-controlled timeout in milliseconds.
#[no_mangle]
pub extern "C" fn net_compute_migration_handle_wait_with_timeout(
    h: *mut MigrationHandleC,
    timeout_ms: u64,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let handle = h.inner.clone();
    let rt = runtime();
    match rt.block_on(async move {
        handle
            .wait_with_timeout(std::time::Duration::from_millis(timeout_ms))
            .await
    }) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &format_sdk_error(&e));
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Request cancellation. Best-effort; past `cutover` the routing
/// flip cannot be cleanly undone.
#[no_mangle]
pub extern "C" fn net_compute_migration_handle_cancel(
    h: *mut MigrationHandleC,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let handle = &h.inner;
    let rt = runtime();
    match rt.block_on(async move { handle.cancel().await }) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &format_sdk_error(&e));
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Start a migration with default options. Returns a boxed
/// `MigrationHandleC*` via `*out_handle` on success. On failure,
/// `*out_handle` is NULL and `*err_out` carries the structured
/// `migration: <kind>[: detail]` body.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_start_migration(
    handle: *mut DaemonRuntimeHandle,
    origin_hash: u32,
    source_node: u64,
    target_node: u64,
    transport_identity: u8,  // 0 = false, non-zero = true
    retry_not_ready_ms: u64, // 0 = disabled
    out_handle: *mut *mut MigrationHandleC,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_handle.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let opts = MigrationOpts {
        transport_identity: transport_identity != 0,
        retry_not_ready: if retry_not_ready_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(retry_not_ready_ms))
        },
    };

    let inner = h.inner.clone();
    let rt = runtime();
    let result = rt.block_on(async move {
        inner
            .start_migration_with(origin_hash, source_node, target_node, opts)
            .await
    });
    match result {
        Ok(mig) => {
            let boxed = Box::new(MigrationHandleC {
                origin_hash: mig.origin_hash,
                source_node: mig.source_node,
                target_node: mig.target_node,
                inner: mig,
            });
            unsafe {
                *out_handle = Box::into_raw(boxed);
            }
            NET_COMPUTE_OK
        }
        Err(e) => {
            unsafe {
                *out_handle = std::ptr::null_mut();
            }
            write_err(err_out, &format_sdk_error(&e));
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Declare on the target side that a migration will land here for
/// `origin_hash` of `kind`. Registers a placeholder factory —
/// identity comes from the migration snapshot's envelope.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_expect_migration(
    handle: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    origin_hash: u32,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let mut cfg = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        cfg.max_log_entries = max_log_entries;
    }
    match h.inner.expect_migration(&kind, origin_hash, cfg) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &format_sdk_error(&e));
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Pre-register a target-side identity for a migration that will
/// NOT carry an identity envelope (source used
/// `transport_identity=false`). `identity_seed` = 32 bytes.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_register_migration_target_identity(
    handle: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    identity_seed: *const u8,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if identity_seed.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let mut seed = [0u8; 32];
    unsafe {
        std::ptr::copy_nonoverlapping(identity_seed, seed.as_mut_ptr(), 32);
    }
    let sdk_identity = SdkIdentity::from_seed(seed);
    let mut cfg = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        cfg.max_log_entries = max_log_entries;
    }
    match h
        .inner
        .register_migration_target_identity(&kind, sdk_identity, cfg)
    {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => {
            write_err(err_out, &format_sdk_error(&e));
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Query the orchestrator's migration phase for `origin_hash`.
/// Returns NULL if no migration is in flight for that origin
/// (either never started or already cleaned up). Caller frees via
/// [`net_compute_free_cstring`] on a non-NULL return.
#[no_mangle]
pub extern "C" fn net_compute_migration_phase(
    handle: *mut DaemonRuntimeHandle,
    origin_hash: u32,
) -> *mut c_char {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    match h.inner.migration_phase(origin_hash) {
        Some(p) => CString::new(migration_phase_str(p))
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// Deliver one event to the daemon at `origin_hash`. The Go
/// dispatcher's `process` callback fires with the same event
/// fields; the daemon's outputs push into a fresh `OutputsVec`
/// and the caller receives them via `outputs` (must be non-NULL).
///
/// The caller is responsible for reading the outputs before the
/// next deliver — [`net_compute_outputs_take`] drains them.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_runtime_deliver(
    handle: *mut DaemonRuntimeHandle,
    origin_hash: u32,
    event_origin_hash: u32,
    event_sequence: u64,
    event_payload: *const u8,
    event_payload_len: usize,
    out_outputs: *mut *mut OutputsVec,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { handle.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_outputs.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    if event_payload_len > 0 && event_payload.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let payload = if event_payload_len == 0 {
        Bytes::new()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(event_payload, event_payload_len) };
        Bytes::copy_from_slice(slice)
    };
    let event = CausalEvent {
        link: net::adapter::net::state::causal::CausalLink {
            origin_hash: event_origin_hash,
            horizon_encoded: 0,
            sequence: event_sequence,
            parent_hash: 0,
        },
        payload,
        received_at: 0,
    };

    let inner = h.inner.clone();
    let rt = runtime();
    let result = rt.block_on(async move { inner.deliver(origin_hash, &event) });
    match result {
        Ok(outputs) => {
            let vec = OutputsVec {
                inner: outputs.into_iter().map(|ev| ev.payload.clone()).collect(),
            };
            unsafe {
                *out_outputs = Box::into_raw(Box::new(vec));
            }
            NET_COMPUTE_OK
        }
        Err(e) => {
            unsafe {
                *out_outputs = std::ptr::null_mut();
            }
            write_err(err_out, &e.to_string());
            NET_COMPUTE_ERR_CALL_FAILED
        }
    }
}

/// Return the number of outputs stored in `vec`. Returns `0` on
/// NULL.
#[no_mangle]
pub extern "C" fn net_compute_outputs_len(vec: *const OutputsVec) -> usize {
    let Some(v) = (unsafe { vec.as_ref() }) else {
        return 0;
    };
    v.inner.len()
}

/// Copy the `idx`-th output payload's `(ptr, len)` into
/// `*out_ptr` / `*out_len`. The pointer is borrowed from the vec —
/// caller must copy before the next [`net_compute_outputs_free`].
#[no_mangle]
pub extern "C" fn net_compute_outputs_at(
    vec: *const OutputsVec,
    idx: usize,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> c_int {
    let Some(v) = (unsafe { vec.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_ptr.is_null() || out_len.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(b) = v.inner.get(idx) else {
        return NET_COMPUTE_ERR_NULL;
    };
    unsafe {
        *out_ptr = b.as_ptr();
        *out_len = b.len();
    }
    NET_COMPUTE_OK
}

/// Free the outputs vec produced by
/// [`net_compute_runtime_deliver`]. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_compute_outputs_free(vec: *mut OutputsVec) {
    if vec.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(vec);
    }
}

// =========================================================================
// ReconstructionErrorBridge — loud-failure fallback for factory errors
// =========================================================================

/// Fallback `MeshDaemon` used when a migration-target factory
/// reconstruction fails (Go dispatcher missing, Go factory returned
/// non-OK, register-factory-without-func placeholder, etc.).
///
/// **Why loud, not silent.** The previous implementation
/// (`NoopBridge`) returned `Ok(vec![])` from `process`, which made
/// migrations appear to succeed on the target even though every
/// event the daemon received was silently dropped. Operators saw
/// "migration completed" alongside vanishing event throughput and
/// no diagnostic. This bridge instead:
///
/// - Returns `CoreDaemonError::RestoreFailed` from `restore`, so
///   the migration's restore phase fails visibly with a typed
///   error carrying the underlying reason.
/// - Returns `CoreDaemonError::ProcessFailed` from every `process`
///   call, so stateless migrations (where `restore` isn't
///   exercised) still surface a failure on the first event.
///
/// Mirrors the same fix applied to the NAPI and PyO3 bindings —
/// see `bindings/node/src/compute.rs` and
/// `bindings/python/src/compute.rs` for the peer implementations.
struct ReconstructionErrorBridge {
    name: String,
    reason: String,
}

impl ReconstructionErrorBridge {
    fn new(name: String, reason: impl Into<String>) -> Self {
        Self {
            name,
            reason: reason.into(),
        }
    }

    fn err_msg(&self, op: &str) -> String {
        format!(
            "reconstruction failed for daemon kind '{}' on {}: {}",
            self.name, op, self.reason,
        )
    }
}

impl MeshDaemon for ReconstructionErrorBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    fn process(
        &mut self,
        _event: &CausalEvent,
    ) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        Err(CoreDaemonError::ProcessFailed(self.err_msg("process")))
    }

    fn restore(&mut self, _state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        Err(CoreDaemonError::RestoreFailed(self.err_msg("restore")))
    }
}

// =========================================================================
// Helpers
// =========================================================================

fn write_err(out: *mut *mut c_char, msg: &str) {
    if out.is_null() {
        return;
    }
    match CString::new(msg) {
        Ok(c) => unsafe {
            *out = c.into_raw();
        },
        Err(_) => unsafe {
            // Message contained an interior NUL — shouldn't happen
            // with our own errors; null-out rather than panic.
            *out = std::ptr::null_mut();
        },
    }
}

/// # Safety
///
/// Caller guarantees `ptr` is a valid read-only pointer to a UTF-8
/// byte sequence of `len` bytes (no trailing NUL required).
fn cstr_to_string(ptr: *const c_char, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

// =========================================================================
// Test-only helpers — compiled only under `--features test-helpers`.
//
// These symbols exist for the `go test` suite's group-placement
// fixtures. Production builds of the cdylib ship without this
// feature, so `libnet_compute.{dylib,so}` does not export the
// helpers and the Go production wrapper (which elides its
// `TestInjectSyntheticPeer` under the same build tag) never
// references them. A consumer bypassing the Go wrapper with
// `dlsym` would find the symbol missing in a shipped binary.
// =========================================================================

/// **Test-only** helper for `groups_test.go`. Injects a synthetic
/// capability announcement directly into the caller-provided
/// mesh's capability index so `place_with_spread` has enough
/// candidates for `ReplicaGroup` / `ForkGroup` / `StandbyGroup`
/// tests without a real handshake.
///
/// Production code should NOT use this — the mesh's normal
/// `announce_capabilities` is what peers broadcast through at
/// runtime. Gated behind the `test-helpers` feature so the symbol
/// is absent from release binaries.
///
/// # Safety
///
/// `mesh_arc` must be a pointer returned by `net_mesh_arc_clone`
/// (from `net::ffi::mesh`); it is NOT consumed by this call.
#[cfg(feature = "test-helpers")]
#[no_mangle]
pub extern "C" fn net_compute_test_inject_synthetic_peer(
    mesh_arc: *mut std::sync::Arc<MeshNode>,
    node_id: u64,
) {
    use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
    use net::adapter::net::identity::EntityId;
    if mesh_arc.is_null() {
        return;
    }
    let arc = unsafe { &*mesh_arc };
    let index = arc.capability_index().clone();
    let eid = EntityId::from_bytes([0u8; 32]);
    index.index(CapabilityAnnouncement::new(
        node_id,
        eid,
        1,
        CapabilitySet::new(),
    ));
}

// =========================================================================
// Groups — Stage 4 of SDK_GROUPS_SURFACE_PLAN.md
// =========================================================================

use net_sdk::groups::{
    ForkGroup as SdkForkGroup, ForkGroupConfig as SdkForkGroupConfig, ForkRecord as SdkForkRecord,
    GroupError as SdkGroupError, GroupHealth as SdkGroupHealth, MemberInfo as SdkMemberInfo,
    MemberRole as SdkMemberRole, ReplicaGroup as SdkReplicaGroup,
    ReplicaGroupConfig as SdkReplicaGroupConfig, RequestContext as SdkRequestContext,
    StandbyGroup as SdkStandbyGroup, StandbyGroupConfig as SdkStandbyGroupConfig,
};

/// Format an SDK `GroupError` into the stable
/// `group: <kind>[: detail]` body the Go side parses via
/// `migrationErr`-style helpers.
fn format_group_error(e: &SdkGroupError) -> String {
    match e {
        SdkGroupError::NotReady => "group: not-ready".to_string(),
        SdkGroupError::FactoryNotFound(kind) => {
            format!("group: factory-not-found: {kind}")
        }
        SdkGroupError::Daemon(d) => format!("group: daemon: {d}"),
        SdkGroupError::Core(core) => format_core_group_error(core),
    }
}

fn format_core_group_error(e: &net::adapter::net::compute::GroupError) -> String {
    use net::adapter::net::compute::GroupError as C;
    match e {
        C::NoHealthyMember => "group: no-healthy-member".to_string(),
        C::PlacementFailed(msg) => format!("group: placement-failed: {msg}"),
        C::RegistryFailed(msg) => format!("group: registry-failed: {msg}"),
        C::InvalidConfig(msg) => format!("group: invalid-config: {msg}"),
    }
}

fn group_err_out(err_out: *mut *mut c_char, e: &SdkGroupError) -> c_int {
    write_err(err_out, &format_group_error(e));
    NET_COMPUTE_ERR_CALL_FAILED
}

/// Same wire format as [`group_err_out`] but takes a raw reason
/// string — used on the FFI-side validation paths (e.g. u32→u8
/// overflow) that detect the error before it reaches the SDK.
/// The reason is expected to already include the `group: <kind>`
/// prefix (e.g. `"invalid-config: replica count 300 exceeds 255"`).
fn group_err_out_reason(err_out: *mut *mut c_char, reason: String) -> c_int {
    write_err(err_out, &format!("group: {reason}"));
    NET_COMPUTE_ERR_CALL_FAILED
}

fn parse_seed(ptr: *const u8) -> Option<[u8; 32]> {
    if ptr.is_null() {
        return None;
    }
    let mut seed = [0u8; 32];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, seed.as_mut_ptr(), 32);
    }
    Some(seed)
}

fn parse_strategy(
    ptr: *const c_char,
    len: usize,
) -> Option<net::adapter::net::behavior::loadbalance::Strategy> {
    use net::adapter::net::behavior::loadbalance::Strategy;
    match cstr_to_string(ptr, len)?.as_str() {
        "round-robin" => Some(Strategy::RoundRobin),
        "consistent-hash" => Some(Strategy::ConsistentHash),
        "least-load" => Some(Strategy::LeastLoad),
        "least-connections" => Some(Strategy::LeastConnections),
        "random" => Some(Strategy::Random),
        _ => None,
    }
}

fn health_status(h: SdkGroupHealth) -> (c_int, u32, u32) {
    // Encode GroupHealth as three ints:
    //   status: 0 = healthy, 1 = degraded, 2 = dead
    //   healthy: number of healthy members (0 if status != degraded)
    //   total: total member count (0 if status != degraded)
    match h {
        SdkGroupHealth::Healthy => (0, 0, 0),
        SdkGroupHealth::Degraded { healthy, total } => (1, healthy as u32, total as u32),
        SdkGroupHealth::Dead => (2, 0, 0),
    }
}

/// Simple JSON encoder for member arrays. Avoids pulling `serde`
/// into `compute-ffi` just for one function.
fn members_to_json(members: &[SdkMemberInfo]) -> String {
    let mut out = String::from("[");
    for (i, m) in members.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"index":{},"origin_hash":{},"node_id":{},"entity_id":"{}","healthy":{}}}"#,
            m.index,
            m.origin_hash,
            m.node_id,
            hex_encode(&m.entity_id_bytes),
            m.healthy,
        ));
    }
    out.push(']');
    out
}

fn fork_records_to_json(records: &[SdkForkRecord]) -> String {
    let mut out = String::from("[");
    for (i, r) in records.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let snap = match r.from_snapshot_seq {
            Some(s) => format!("{s}"),
            None => "null".to_string(),
        };
        out.push_str(&format!(
            r#"{{"original_origin":{},"forked_origin":{},"fork_seq":{},"from_snapshot_seq":{}}}"#,
            r.original_origin, r.forked_origin, r.fork_seq, snap,
        ));
    }
    out.push(']');
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ---------------------------------------------------------------
// ReplicaGroup
// ---------------------------------------------------------------

pub struct ReplicaGroupHandle {
    inner: std::sync::Arc<SdkReplicaGroup>,
}

/// Spawn a replica group bound to an existing `DaemonRuntime`.
/// `kind_ptr` + `kind_len` name the factory (must be registered
/// via `net_compute_register_factory_with_func`).
///
/// On success, writes the handle to `*out_handle` and returns
/// `NET_COMPUTE_OK`. On failure, writes a
/// `group: <kind>[: detail]` body to `*err_out` and returns
/// `NET_COMPUTE_ERR_CALL_FAILED`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_replica_group_spawn(
    runtime: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    replica_count: u32,
    group_seed: *const u8,
    lb_strategy_ptr: *const c_char,
    lb_strategy_len: usize,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    out_handle: *mut *mut ReplicaGroupHandle,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(rt) = (unsafe { runtime.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_handle.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(seed) = parse_seed(group_seed) else {
        write_err(
            err_out,
            "group: invalid-config: group_seed must be 32 bytes",
        );
        return NET_COMPUTE_ERR_CALL_FAILED;
    };
    let Some(lb) = parse_strategy(lb_strategy_ptr, lb_strategy_len) else {
        write_err(err_out, "group: invalid-config: unknown lb strategy");
        return NET_COMPUTE_ERR_CALL_FAILED;
    };

    let mut host = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        host.max_log_entries = max_log_entries;
    }
    let cfg = SdkReplicaGroupConfig {
        replica_count: replica_count as u8,
        group_seed: seed,
        lb_strategy: lb,
        host_config: host,
    };

    match SdkReplicaGroup::spawn(&rt.inner, &kind, cfg) {
        Ok(g) => {
            let h = ReplicaGroupHandle {
                inner: std::sync::Arc::new(g),
            };
            unsafe { *out_handle = Box::into_raw(Box::new(h)) };
            NET_COMPUTE_OK
        }
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_replica_group_free(h: *mut ReplicaGroupHandle) {
    if h.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(h);
    }
}

#[no_mangle]
pub extern "C" fn net_compute_replica_group_replica_count(h: *const ReplicaGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.replica_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_replica_group_healthy_count(h: *const ReplicaGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.healthy_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_replica_group_group_id(h: *const ReplicaGroupHandle) -> u32 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.inner.group_id()
}

/// Fill `(out_status, out_healthy, out_total)` with the group's
/// current health. `out_status`: 0 = healthy, 1 = degraded,
/// 2 = dead. On non-degraded `healthy` and `total` are 0.
#[no_mangle]
pub extern "C" fn net_compute_replica_group_health(
    h: *const ReplicaGroupHandle,
    out_status: *mut c_int,
    out_healthy: *mut u32,
    out_total: *mut u32,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_status.is_null() || out_healthy.is_null() || out_total.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let (s, hc, t) = health_status(h.inner.health());
    unsafe {
        *out_status = s;
        *out_healthy = hc;
        *out_total = t;
    }
    NET_COMPUTE_OK
}

/// Route a request to the best healthy replica. `routing_key_ptr`
/// (may be NULL if len=0) is consistent-hashed for sticky routing.
#[no_mangle]
pub extern "C" fn net_compute_replica_group_route_event(
    h: *const ReplicaGroupHandle,
    routing_key_ptr: *const c_char,
    routing_key_len: usize,
    out_origin: *mut u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_origin.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let mut rc = SdkRequestContext::new();
    if routing_key_len > 0 {
        if let Some(k) = cstr_to_string(routing_key_ptr, routing_key_len) {
            rc = rc.with_routing_key(k);
        }
    }
    match h.inner.route_event(&rc) {
        Ok(origin) => {
            unsafe { *out_origin = origin };
            NET_COMPUTE_OK
        }
        Err(e) => group_err_out(err_out, &e),
    }
}

/// Resize the group to `n` members. Reuses the kind the group was
/// spawned with.
#[no_mangle]
pub extern "C" fn net_compute_replica_group_scale_to(
    h: *const ReplicaGroupHandle,
    n: u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let n_u8 = match u8::try_from(n) {
        Ok(v) => v,
        Err(_) => {
            return group_err_out_reason(
                err_out,
                format!("invalid-config: replica count {n} exceeds {}", u8::MAX),
            );
        }
    };
    match h.inner.scale_to(n_u8) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_replica_group_on_node_recovery(
    h: *const ReplicaGroupHandle,
    node_id: u64,
) {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return;
    };
    h.inner.on_node_recovery(node_id);
}

/// Return the members roster as a heap-allocated JSON string.
/// Caller frees via `net_compute_free_cstring`.
#[no_mangle]
pub extern "C" fn net_compute_replica_group_members_json(
    h: *const ReplicaGroupHandle,
) -> *mut c_char {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let json = members_to_json(&h.inner.replicas());
    CString::new(json)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------
// ForkGroup
// ---------------------------------------------------------------

pub struct ForkGroupHandle {
    inner: std::sync::Arc<SdkForkGroup>,
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_fork_group_spawn(
    runtime: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    parent_origin: u32,
    fork_seq: u64,
    fork_count: u32,
    lb_strategy_ptr: *const c_char,
    lb_strategy_len: usize,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    out_handle: *mut *mut ForkGroupHandle,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(rt) = (unsafe { runtime.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_handle.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(lb) = parse_strategy(lb_strategy_ptr, lb_strategy_len) else {
        write_err(err_out, "group: invalid-config: unknown lb strategy");
        return NET_COMPUTE_ERR_CALL_FAILED;
    };
    let mut host = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        host.max_log_entries = max_log_entries;
    }
    let cfg = SdkForkGroupConfig {
        fork_count: fork_count as u8,
        lb_strategy: lb,
        host_config: host,
    };
    match SdkForkGroup::fork(&rt.inner, &kind, parent_origin, fork_seq, cfg) {
        Ok(g) => {
            let h = ForkGroupHandle {
                inner: std::sync::Arc::new(g),
            };
            unsafe { *out_handle = Box::into_raw(Box::new(h)) };
            NET_COMPUTE_OK
        }
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_free(h: *mut ForkGroupHandle) {
    if h.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(h);
    }
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_fork_count(h: *const ForkGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.fork_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_healthy_count(h: *const ForkGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.healthy_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_parent_origin(h: *const ForkGroupHandle) -> u32 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.inner.parent_origin()
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_fork_seq(h: *const ForkGroupHandle) -> u64 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.inner.fork_seq()
}

/// Verify every fork's lineage record. Returns 1 on verified, 0
/// otherwise. Caller sees NULL handle as 0.
#[no_mangle]
pub extern "C" fn net_compute_fork_group_verify_lineage(h: *const ForkGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    if h.inner.verify_lineage() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_scale_to(
    h: *const ForkGroupHandle,
    n: u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let n_u8 = match u8::try_from(n) {
        Ok(v) => v,
        Err(_) => {
            return group_err_out_reason(
                err_out,
                format!("invalid-config: fork count {n} exceeds {}", u8::MAX),
            );
        }
    };
    match h.inner.scale_to(n_u8) {
        Ok(()) => NET_COMPUTE_OK,
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_on_node_recovery(h: *const ForkGroupHandle, node_id: u64) {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return;
    };
    h.inner.on_node_recovery(node_id);
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_members_json(h: *const ForkGroupHandle) -> *mut c_char {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let json = members_to_json(&h.inner.members());
    CString::new(json)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn net_compute_fork_group_fork_records_json(
    h: *const ForkGroupHandle,
) -> *mut c_char {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let json = fork_records_to_json(&h.inner.fork_records());
    CString::new(json)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------
// StandbyGroup
// ---------------------------------------------------------------

pub struct StandbyGroupHandle {
    inner: std::sync::Arc<SdkStandbyGroup>,
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_compute_standby_group_spawn(
    runtime: *mut DaemonRuntimeHandle,
    kind_ptr: *const c_char,
    kind_len: usize,
    member_count: u32,
    group_seed: *const u8,
    auto_snapshot_interval: u64,
    max_log_entries: u32,
    out_handle: *mut *mut StandbyGroupHandle,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(rt) = (unsafe { runtime.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_handle.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    let Some(kind) = cstr_to_string(kind_ptr, kind_len) else {
        return NET_COMPUTE_ERR_NULL;
    };
    let Some(seed) = parse_seed(group_seed) else {
        write_err(
            err_out,
            "group: invalid-config: group_seed must be 32 bytes",
        );
        return NET_COMPUTE_ERR_CALL_FAILED;
    };
    let mut host = DaemonHostConfig {
        auto_snapshot_interval,
        ..DaemonHostConfig::default()
    };
    if max_log_entries > 0 {
        host.max_log_entries = max_log_entries;
    }
    let cfg = SdkStandbyGroupConfig {
        member_count: member_count as u8,
        group_seed: seed,
        host_config: host,
    };
    match SdkStandbyGroup::spawn(&rt.inner, &kind, cfg) {
        Ok(g) => {
            let h = StandbyGroupHandle {
                inner: std::sync::Arc::new(g),
            };
            unsafe { *out_handle = Box::into_raw(Box::new(h)) };
            NET_COMPUTE_OK
        }
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_free(h: *mut StandbyGroupHandle) {
    if h.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(h);
    }
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_member_count(h: *const StandbyGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.member_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_standby_count(h: *const StandbyGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.standby_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_active_index(h: *const StandbyGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.active_index() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_active_origin(h: *const StandbyGroupHandle) -> u32 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.inner.active_origin()
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_active_healthy(h: *const StandbyGroupHandle) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    if h.inner.active_healthy() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_group_id(h: *const StandbyGroupHandle) -> u32 {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return 0;
    };
    h.inner.group_id()
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_buffered_event_count(
    h: *const StandbyGroupHandle,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    h.inner.buffered_event_count() as c_int
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_sync_standbys(
    h: *const StandbyGroupHandle,
    out_through: *mut u64,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_through.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    match h.inner.sync_standbys() {
        Ok(seq) => {
            unsafe { *out_through = seq };
            NET_COMPUTE_OK
        }
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_promote(
    h: *const StandbyGroupHandle,
    out_origin: *mut u32,
    err_out: *mut *mut c_char,
) -> c_int {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return NET_COMPUTE_ERR_NULL;
    };
    if out_origin.is_null() {
        return NET_COMPUTE_ERR_NULL;
    }
    match h.inner.promote() {
        Ok(origin) => {
            unsafe { *out_origin = origin };
            NET_COMPUTE_OK
        }
        Err(e) => group_err_out(err_out, &e),
    }
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_on_node_recovery(
    h: *const StandbyGroupHandle,
    node_id: u64,
) {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return;
    };
    h.inner.on_node_recovery(node_id);
}

#[no_mangle]
pub extern "C" fn net_compute_standby_group_members_json(
    h: *const StandbyGroupHandle,
) -> *mut c_char {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let json = members_to_json(&h.inner.members());
    CString::new(json)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Returns `"active"` / `"standby"` (caller frees via
/// `net_compute_free_cstring`) or NULL for an out-of-range index.
#[no_mangle]
pub extern "C" fn net_compute_standby_group_member_role(
    h: *const StandbyGroupHandle,
    index: u32,
) -> *mut c_char {
    let Some(h) = (unsafe { h.as_ref() }) else {
        return std::ptr::null_mut();
    };
    match h.inner.member_role(index as u8) {
        Some(SdkMemberRole::Active) => CString::new("active")
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Some(SdkMemberRole::Standby) => CString::new("standby")
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: migration-target reconstruction fallback must
    // fail loudly, never silently. Before this fix the compute-ffi
    // layer installed a `NoopBridge` whose `process` returned
    // `Ok(vec![])`, so a migration could "succeed" on a Go target
    // where the dispatcher wasn't installed / the Go factory
    // returned an error — the daemon would then silently swallow
    // every event with no diagnostic. The replacement
    // `ReconstructionErrorBridge` returns typed `RestoreFailed`
    // from `restore` and typed `ProcessFailed` from `process`
    // carrying the underlying reason, mirroring the same fix in
    // the NAPI and PyO3 bindings.

    #[test]
    fn reconstruction_error_bridge_process_returns_typed_error() {
        let mut bridge = ReconstructionErrorBridge::new(
            "counter".to_string(),
            "Go factory returned error code 2",
        );
        let event = CausalEvent {
            link: ::net::adapter::net::state::causal::CausalLink {
                origin_hash: 0xdead_beef,
                horizon_encoded: 0,
                sequence: 1,
                parent_hash: 0,
            },
            payload: bytes::Bytes::from_static(b"x"),
            received_at: 0,
        };

        let result = bridge.process(&event);
        match result {
            Err(CoreDaemonError::ProcessFailed(msg)) => {
                assert!(msg.contains("counter"), "missing kind: {msg}");
                assert!(
                    msg.contains("Go factory returned error code 2"),
                    "missing underlying reason: {msg}",
                );
                assert!(msg.contains("process"), "missing op label: {msg}");
            }
            Err(other) => panic!("expected ProcessFailed, got {other:?}"),
            Ok(outputs) => panic!(
                "silent-noop regression: ReconstructionErrorBridge must never return Ok; got {} outputs",
                outputs.len(),
            ),
        }
    }

    #[test]
    fn reconstruction_error_bridge_restore_returns_typed_error() {
        let mut bridge =
            ReconstructionErrorBridge::new("echo".to_string(), "Go dispatcher not registered");
        let state = bytes::Bytes::from_static(&[0u8; 16]);

        let result = bridge.restore(state);
        match result {
            Err(CoreDaemonError::RestoreFailed(msg)) => {
                assert!(msg.contains("echo"), "missing kind: {msg}");
                assert!(
                    msg.contains("dispatcher not registered"),
                    "missing underlying reason: {msg}",
                );
                assert!(msg.contains("restore"), "missing op label: {msg}");
            }
            Err(other) => panic!("expected RestoreFailed, got {other:?}"),
            Ok(()) => panic!(
                "silent-noop regression: ReconstructionErrorBridge::restore must never return Ok",
            ),
        }
    }
}
