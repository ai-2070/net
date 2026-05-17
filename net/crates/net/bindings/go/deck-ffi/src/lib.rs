//! C ABI for the Deck SDK — operator-side bindings.
//!
//! Consumed by the Go binding at `bindings/go/net/deck.go` and by
//! the C SDK header at `include/net_deck.h` (Phase 7).
//!
//! # Scope (slice 1)
//!
//! - Client lifecycle: `client_new` (constructs a private supervisor
//!   runtime internally) / `client_free`.
//! - `AdminCommands` × 9: `drain`, `enter_maintenance`,
//!   `exit_maintenance`, `cordon`, `uncordon`, `drop_replicas`,
//!   `invalidate_placement`, `restart_all_daemons`,
//!   `clear_avoid_list`.
//! - One-shot reads: `status` (JSON), `status_summary` (typed
//!   struct).
//! - Streams: `subscribe_snapshots` + `subscribe_status_summaries`
//!   with `_next` / `_close`.
//! - Last-error trio: `last_error_message` / `last_error_kind` /
//!   `clear_last_error`, matching the substrate's
//!   `<<deck-sdk-kind:KIND>>MSG` envelope.
//!
//! # Out of scope until slice 2
//!
//! - Audit query builder + audit/log/failure streams. The Rust
//!   SDK has them today; the FFI wraps them in slice 2.
//! - ICE surface (`force_*`, `simulate`/`commit` typestate). Slice 3.
//!
//! # Operator-only mode
//!
//! Slice 1 takes a single-process model: the cdylib owns a private
//! `MeshOsDaemonSdk` constructed inside `client_new`. The caller
//! supplies only the operator identity + supervisor config; the
//! deck client wraps the substrate's runtime end-to-end. Composing
//! against an external `NetMeshOsSdk` handle (from
//! `bindings/go/meshos-ffi`) requires cross-cdylib symbol sharing
//! and lands in slice 2 when an operator workflow demands it.
//!
//! # Handle model + error model
//!
//! Identical to `meshos-ffi`: opaque `*mut T` pointers freed via
//! matching `_free`, thread-local last-error pair on every
//! non-OK status, `ffi_guard!` `catch_unwind` at every entry
//! point.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_uint, CStr, CString};
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use futures::StreamExt;
use tokio::runtime::Runtime;

use net::adapter::net::behavior::deck::{
    AdminCommands as CoreAdminCommands, ChainCommit as CoreChainCommit, DeckClient as CoreClient,
    DeckClientConfig as CoreConfig, DeckError, OperatorIdentity as CoreIdentity,
    SnapshotStream as CoreSnapshotStream, StatusSummary, StatusSummaryStream as CoreStatusStream,
};
use net::adapter::net::behavior::meshos::{
    LoggingDispatcher, MeshOsConfig, MeshOsDaemonSdk as CoreSdk,
};
use net::adapter::net::EntityKeypair;

// =========================================================================
// Status codes
// =========================================================================

pub const NET_DECK_OK: c_int = 0;
pub const NET_DECK_ERR_NULL: c_int = -1;
pub const NET_DECK_ERR_CALL_FAILED: c_int = -2;
pub const NET_DECK_ERR_INVALID_ARG: c_int = -3;
pub const NET_DECK_ERR_ALREADY_SHUTDOWN: c_int = -4;
pub const NET_DECK_ERR_END_OF_STREAM: c_int = -5;

// =========================================================================
// Status summary wire form
// =========================================================================

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NetDeckPeerCounts {
    pub healthy: c_uint,
    pub degraded: c_uint,
    pub unreachable: c_uint,
    pub unknown: c_uint,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NetDeckDaemonCounts {
    pub running: c_uint,
    pub starting: c_uint,
    pub stopping: c_uint,
    pub stopped: c_uint,
    pub backing_off: c_uint,
    pub crash_looping: c_uint,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NetDeckStatusSummary {
    pub peers: NetDeckPeerCounts,
    pub daemons: NetDeckDaemonCounts,
    pub replica_chains: c_uint,
    pub avoid_list_entries: c_uint,
    pub recently_emitted_count: c_uint,
    pub recent_failure_count: c_uint,
    pub admin_audit_ring_depth: c_uint,
    /// `freeze_remaining_ms` is `Option<u64>` on the substrate;
    /// `freeze_remaining_present` discriminates None (0) from a
    /// valid `freeze_remaining_ms` (1).
    pub freeze_remaining_present: c_int,
    pub freeze_remaining_ms: u64,
    /// `1` iff this node's local maintenance state is not `Active`.
    pub local_maintenance_active: c_int,
}

impl NetDeckStatusSummary {
    fn from_core(s: &StatusSummary) -> Self {
        let (present, ms) = match s.freeze_remaining_ms {
            Some(ms) => (1, ms),
            None => (0, 0),
        };
        Self {
            peers: NetDeckPeerCounts {
                healthy: s.peers.healthy as c_uint,
                degraded: s.peers.degraded as c_uint,
                unreachable: s.peers.unreachable as c_uint,
                unknown: s.peers.unknown as c_uint,
            },
            daemons: NetDeckDaemonCounts {
                running: s.daemons.running as c_uint,
                starting: s.daemons.starting as c_uint,
                stopping: s.daemons.stopping as c_uint,
                stopped: s.daemons.stopped as c_uint,
                backing_off: s.daemons.backing_off as c_uint,
                crash_looping: s.daemons.crash_looping as c_uint,
            },
            replica_chains: s.replica_chains as c_uint,
            avoid_list_entries: s.avoid_list_entries as c_uint,
            recently_emitted_count: s.recently_emitted_count as c_uint,
            recent_failure_count: s.recent_failure_count as c_uint,
            admin_audit_ring_depth: s.admin_audit_ring_depth as c_uint,
            freeze_remaining_present: present,
            freeze_remaining_ms: ms,
            local_maintenance_active: if s.local_maintenance_active { 1 } else { 0 },
        }
    }
}

// =========================================================================
// ChainCommit wire form
// =========================================================================

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NetDeckChainCommit {
    pub commit_id: u64,
    pub operator_id: u64,
    /// `event_kind` is a substrate-static string; we emit it as
    /// an enum tag for the C side. Use `net_deck_event_kind_str`
    /// to recover the string form if needed.
    pub event_kind: c_int,
    pub committed_at_ms: u64,
}

pub const NET_DECK_EVENT_KIND_UNKNOWN: c_int = 0;
pub const NET_DECK_EVENT_KIND_DRAIN: c_int = 1;
pub const NET_DECK_EVENT_KIND_ENTER_MAINTENANCE: c_int = 2;
pub const NET_DECK_EVENT_KIND_EXIT_MAINTENANCE: c_int = 3;
pub const NET_DECK_EVENT_KIND_CORDON: c_int = 4;
pub const NET_DECK_EVENT_KIND_UNCORDON: c_int = 5;
pub const NET_DECK_EVENT_KIND_DROP_REPLICAS: c_int = 6;
pub const NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT: c_int = 7;
pub const NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS: c_int = 8;
pub const NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST: c_int = 9;

fn event_kind_to_c(kind: &str) -> c_int {
    match kind {
        "drain" => NET_DECK_EVENT_KIND_DRAIN,
        "enter_maintenance" => NET_DECK_EVENT_KIND_ENTER_MAINTENANCE,
        "exit_maintenance" => NET_DECK_EVENT_KIND_EXIT_MAINTENANCE,
        "cordon" => NET_DECK_EVENT_KIND_CORDON,
        "uncordon" => NET_DECK_EVENT_KIND_UNCORDON,
        "drop_replicas" => NET_DECK_EVENT_KIND_DROP_REPLICAS,
        "invalidate_placement" => NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT,
        "restart_all_daemons" => NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS,
        "clear_avoid_list" => NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST,
        _ => NET_DECK_EVENT_KIND_UNKNOWN,
    }
}

fn chain_commit_to_c(commit: &CoreChainCommit) -> NetDeckChainCommit {
    let committed_at_ms = commit
        .committed_at()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    NetDeckChainCommit {
        commit_id: commit.commit_id(),
        operator_id: commit.operator_id(),
        event_kind: event_kind_to_c(commit.event_kind()),
        committed_at_ms,
    }
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

fn set_last_error_from_sdk(err: &DeckError) {
    set_last_error(err.kind, &err.message);
}

fn clear_last_error_inner() {
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = None);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = None);
}

#[no_mangle]
pub extern "C" fn net_deck_last_error_message() -> *const c_char {
    LAST_ERROR_MESSAGE.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

#[no_mangle]
pub extern "C" fn net_deck_last_error_kind() -> *const c_char {
    LAST_ERROR_KIND.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

#[no_mangle]
pub extern "C" fn net_deck_clear_last_error() {
    clear_last_error_inner();
}

// =========================================================================
// FFI guard
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
                .thread_name("net-deck-ffi")
                .build()
                .expect("failed to construct deck-ffi tokio runtime"),
        )
    })
}

// =========================================================================
// Handle types
// =========================================================================

pub struct NetDeckClient {
    /// `Option` so `client_free` can drop the inner SDK + runtime
    /// state cleanly. The supervisor runtime is held inside
    /// `_sdk` for its lifetime.
    client: Option<CoreClient>,
    _sdk: Option<CoreSdk>,
}

pub struct NetDeckSnapshotStream {
    inner: Option<CoreSnapshotStream>,
}

pub struct NetDeckStatusSummaryStream {
    inner: Option<CoreStatusStream>,
}

// =========================================================================
// String handling — Rust → C
// =========================================================================

/// Free a heap-allocated C string returned by this crate (e.g.
/// from `net_deck_status` or `net_deck_snapshot_stream_next`).
/// Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(s);
    }
}

/// Reference `CStr` so the import doesn't get flagged as unused.
const _: fn() = || {
    let _ = CStr::from_bytes_with_nul(b"\0");
};

// =========================================================================
// Client lifecycle
// =========================================================================

/// Construct a deck client owning a private MeshOS supervisor
/// runtime. The caller passes the supervisor config (this_node /
/// tick interval / queue capacities — pass 0 for substrate
/// defaults) plus a 32-byte ed25519 seed for the operator identity.
///
/// Returns `NET_DECK_OK` on success and writes the heap-allocated
/// handle to `*out`. On failure populates the thread-local
/// last-error pair and returns a non-OK status. The handle MUST
/// be freed via `net_deck_client_free`.
#[no_mangle]
pub extern "C" fn net_deck_client_new(
    this_node: u64,
    tick_interval_ms: u64,
    event_queue_capacity: usize,
    action_queue_capacity: usize,
    snapshot_poll_interval_ms: u64,
    ice_signature_threshold: usize,
    operator_seed_ptr: *const u8,
    out: *mut *mut NetDeckClient,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if operator_seed_ptr.is_null() || out.is_null() {
            set_last_error("invalid_argument", "operator_seed / out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        clear_last_error_inner();
        let seed: [u8; 32] = unsafe { std::slice::from_raw_parts(operator_seed_ptr, 32) }
            .try_into()
            .expect("slice has len 32");
        let keypair = EntityKeypair::from_bytes(seed);
        let identity = CoreIdentity::from_keypair(keypair);

        let mut sdk_cfg = MeshOsConfig::default();
        sdk_cfg.this_node = this_node;
        if tick_interval_ms > 0 {
            sdk_cfg.tick_interval = Duration::from_millis(tick_interval_ms);
        }
        if event_queue_capacity > 0 {
            sdk_cfg.event_queue_capacity = event_queue_capacity;
        }
        if action_queue_capacity > 0 {
            sdk_cfg.action_queue_capacity = action_queue_capacity;
        }
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = {
            let _enter = runtime().enter();
            CoreSdk::start(sdk_cfg, dispatcher)
        };

        let mut deck_cfg = CoreConfig::default();
        if snapshot_poll_interval_ms > 0 {
            deck_cfg.snapshot_poll_interval = Duration::from_millis(snapshot_poll_interval_ms);
        }
        if ice_signature_threshold > 0 {
            deck_cfg.ice_signature_threshold = ice_signature_threshold;
        }

        let client = CoreClient::new(
            sdk.runtime().handle_clone(),
            sdk.runtime().snapshot_reader().clone(),
            identity,
            deck_cfg,
        );

        let handle = Box::into_raw(Box::new(NetDeckClient {
            client: Some(client),
            _sdk: Some(sdk),
        }));
        unsafe { *out = handle };
        NET_DECK_OK
    })
}

/// Free a deck client. Tears down the private supervisor runtime.
/// Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_client_free(client: *mut NetDeckClient) {
    if client.is_null() {
        return;
    }
    unsafe {
        let mut boxed = Box::from_raw(client);
        boxed.client.take();
        if let Some(sdk) = boxed._sdk.take() {
            // Drive a clean shutdown of the wrapped runtime.
            let _ = runtime().block_on(sdk.shutdown());
        }
    }
}

/// Operator identifier bound to this client. Returns `0` on NULL.
#[no_mangle]
pub extern "C" fn net_deck_client_operator_id(client: *const NetDeckClient) -> u64 {
    let Some(c) = (unsafe { client.as_ref() }) else {
        return 0;
    };
    match c.client.as_ref() {
        Some(cl) => cl.identity().operator_id(),
        None => 0,
    }
}

// =========================================================================
// Helpers — get the inner CoreClient
// =========================================================================

fn with_client<F, R>(client: *const NetDeckClient, default: R, f: F) -> R
where
    F: FnOnce(&CoreClient) -> R,
{
    let Some(c) = (unsafe { client.as_ref() }) else {
        set_last_error("invalid_argument", "client pointer is NULL");
        return default;
    };
    let Some(inner) = c.client.as_ref() else {
        set_last_error("already_shutdown", "DeckClient was already freed");
        return default;
    };
    f(inner)
}

// =========================================================================
// status / status_summary
// =========================================================================

/// One-shot read of the latest `MeshOsSnapshot` as a heap-
/// allocated JSON string. Caller frees via `net_deck_free_string`.
/// Returns NULL on error (last-error populated).
#[no_mangle]
pub extern "C" fn net_deck_status(client: *const NetDeckClient) -> *mut c_char {
    ffi_guard!(ptr::null_mut(), {
        let result = with_client(client, None, |cl| Some(cl.status()));
        let Some(snap) = result else {
            return ptr::null_mut();
        };
        match serde_json::to_string(&snap) {
            Ok(s) => match CString::new(s) {
                Ok(c) => c.into_raw(),
                Err(_) => {
                    set_last_error("snapshot_serialize_failed", "JSON contained NUL byte");
                    ptr::null_mut()
                }
            },
            Err(e) => {
                set_last_error("snapshot_serialize_failed", &e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// One-shot read of the rolled-up `StatusSummary`. Writes a
/// stable C struct to `*out`. Returns `NET_DECK_OK` on success,
/// or a non-OK status with the last-error pair populated.
#[no_mangle]
pub extern "C" fn net_deck_status_summary(
    client: *const NetDeckClient,
    out: *mut NetDeckStatusSummary,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let summary = with_client(client, None, |cl| Some(cl.status_summary()));
        match summary {
            Some(s) => {
                unsafe { *out = NetDeckStatusSummary::from_core(&s) };
                NET_DECK_OK
            }
            None => NET_DECK_ERR_NULL,
        }
    })
}

// =========================================================================
// AdminCommands × 9 — each commits and writes a ChainCommit
// =========================================================================

fn admin_commit<F>(
    client: *const NetDeckClient,
    out: *mut NetDeckChainCommit,
    op: F,
) -> c_int
where
    F: for<'a> FnOnce(
        CoreAdminCommands<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CoreChainCommit, DeckError>> + 'a>,
    >,
{
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        clear_last_error_inner();
        match runtime().block_on(op(inner.admin())) {
            Ok(commit) => {
                unsafe { *out = chain_commit_to_c(&commit) };
                NET_DECK_OK
            }
            Err(e) => {
                set_last_error_from_sdk(&e);
                NET_DECK_ERR_CALL_FAILED
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_drain(
    client: *const NetDeckClient,
    node: u64,
    drain_for_ms: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.drain(node, Duration::from_millis(drain_for_ms)).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_enter_maintenance(
    client: *const NetDeckClient,
    node: u64,
    drain_for_ms: u64,
    has_drain_for: c_int,
    out: *mut NetDeckChainCommit,
) -> c_int {
    let drain_for = if has_drain_for != 0 {
        Some(Duration::from_millis(drain_for_ms))
    } else {
        None
    };
    admin_commit(client, out, move |admin| {
        Box::pin(async move { admin.enter_maintenance(node, drain_for).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_exit_maintenance(
    client: *const NetDeckClient,
    node: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.exit_maintenance(node).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_cordon(
    client: *const NetDeckClient,
    node: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.cordon(node).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_uncordon(
    client: *const NetDeckClient,
    node: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.uncordon(node).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_drop_replicas(
    client: *const NetDeckClient,
    node: u64,
    chains_ptr: *const u64,
    chains_len: usize,
    out: *mut NetDeckChainCommit,
) -> c_int {
    if chains_len > 0 && chains_ptr.is_null() {
        set_last_error("invalid_argument", "chains pointer is NULL but chains_len > 0");
        return NET_DECK_ERR_INVALID_ARG;
    }
    let chains: Vec<u64> = if chains_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(chains_ptr, chains_len) }.to_vec()
    };
    admin_commit(client, out, move |admin| {
        Box::pin(async move { admin.drop_replicas(node, chains).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_invalidate_placement(
    client: *const NetDeckClient,
    node: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.invalidate_placement(node).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_restart_all_daemons(
    client: *const NetDeckClient,
    node: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.restart_all_daemons(node).await })
    })
}

#[no_mangle]
pub extern "C" fn net_deck_admin_clear_avoid_list(
    client: *const NetDeckClient,
    node: u64,
    out: *mut NetDeckChainCommit,
) -> c_int {
    admin_commit(client, out, |admin| {
        Box::pin(async move { admin.clear_avoid_list(node).await })
    })
}

// =========================================================================
// Snapshot stream
// =========================================================================

/// Subscribe to the live `MeshOsSnapshot` stream. Returns a
/// heap-allocated handle the caller polls via
/// `net_deck_snapshot_stream_next`. Free via
/// `net_deck_snapshot_stream_free` (or `_close` which is an
/// alias).
#[no_mangle]
pub extern "C" fn net_deck_subscribe_snapshots(
    client: *const NetDeckClient,
    out: *mut *mut NetDeckSnapshotStream,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let stream = {
            let Some(c) = (unsafe { client.as_ref() }) else {
                set_last_error("invalid_argument", "client pointer is NULL");
                return NET_DECK_ERR_NULL;
            };
            let Some(inner) = c.client.as_ref() else {
                set_last_error("already_shutdown", "DeckClient was already freed");
                return NET_DECK_ERR_ALREADY_SHUTDOWN;
            };
            let _enter = runtime().enter();
            inner.snapshots()
        };
        clear_last_error_inner();
        let boxed = Box::into_raw(Box::new(NetDeckSnapshotStream {
            inner: Some(stream),
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

/// Block until the next snapshot arrives or `timeout_ms` elapses.
/// On success writes a heap-allocated JSON string to `*out`
/// (caller frees via `net_deck_free_string`) and returns
/// `NET_DECK_OK`. On timeout returns `NET_DECK_OK` with `*out =
/// NULL`. On stream end returns `NET_DECK_ERR_END_OF_STREAM`.
/// Pass `0` for an unbounded wait.
#[no_mangle]
pub extern "C" fn net_deck_snapshot_stream_next(
    stream: *mut NetDeckSnapshotStream,
    timeout_ms: u64,
    out: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(s) = (unsafe { stream.as_mut() }) else {
            set_last_error("invalid_argument", "stream pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let inner = match s.inner.as_mut() {
            Some(i) => i,
            None => {
                unsafe { *out = ptr::null_mut() };
                return NET_DECK_ERR_END_OF_STREAM;
            }
        };
        clear_last_error_inner();
        let snap = runtime().block_on(async {
            if timeout_ms == 0 {
                inner.next().await
            } else {
                match tokio::time::timeout(Duration::from_millis(timeout_ms), inner.next()).await {
                    Ok(s) => s,
                    Err(_) => None,
                }
            }
        });
        match snap {
            Some(Ok(snap)) => {
                let json = match serde_json::to_string(&snap) {
                    Ok(j) => j,
                    Err(e) => {
                        set_last_error("snapshot_serialize_failed", &e.to_string());
                        return NET_DECK_ERR_CALL_FAILED;
                    }
                };
                let c = match CString::new(json) {
                    Ok(c) => c,
                    Err(_) => {
                        set_last_error("snapshot_serialize_failed", "JSON contained NUL byte");
                        return NET_DECK_ERR_CALL_FAILED;
                    }
                };
                unsafe { *out = c.into_raw() };
                NET_DECK_OK
            }
            Some(Err(e)) => {
                set_last_error_from_sdk(&e);
                NET_DECK_ERR_CALL_FAILED
            }
            None if timeout_ms == 0 => {
                // Stream ended naturally (substrate runtime shut down).
                unsafe { *out = ptr::null_mut() };
                NET_DECK_ERR_END_OF_STREAM
            }
            None => {
                // Timeout elapsed without an item.
                unsafe { *out = ptr::null_mut() };
                NET_DECK_OK
            }
        }
    })
}

/// Close + free a snapshot stream. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_snapshot_stream_free(stream: *mut NetDeckSnapshotStream) {
    if stream.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(stream);
    }
}

// =========================================================================
// Status summary stream
// =========================================================================

#[no_mangle]
pub extern "C" fn net_deck_subscribe_status_summaries(
    client: *const NetDeckClient,
    out: *mut *mut NetDeckStatusSummaryStream,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let stream = {
            let Some(c) = (unsafe { client.as_ref() }) else {
                set_last_error("invalid_argument", "client pointer is NULL");
                return NET_DECK_ERR_NULL;
            };
            let Some(inner) = c.client.as_ref() else {
                set_last_error("already_shutdown", "DeckClient was already freed");
                return NET_DECK_ERR_ALREADY_SHUTDOWN;
            };
            let _enter = runtime().enter();
            inner.status_summary_stream()
        };
        clear_last_error_inner();
        let boxed = Box::into_raw(Box::new(NetDeckStatusSummaryStream {
            inner: Some(stream),
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

#[no_mangle]
pub extern "C" fn net_deck_status_summary_stream_next(
    stream: *mut NetDeckStatusSummaryStream,
    timeout_ms: u64,
    out: *mut NetDeckStatusSummary,
    has_item_out: *mut c_int,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() || has_item_out.is_null() {
            set_last_error("invalid_argument", "out / has_item_out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(s) = (unsafe { stream.as_mut() }) else {
            set_last_error("invalid_argument", "stream pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let inner = match s.inner.as_mut() {
            Some(i) => i,
            None => {
                unsafe { *has_item_out = 0 };
                return NET_DECK_ERR_END_OF_STREAM;
            }
        };
        clear_last_error_inner();
        let item = runtime().block_on(async {
            if timeout_ms == 0 {
                inner.next().await
            } else {
                match tokio::time::timeout(Duration::from_millis(timeout_ms), inner.next()).await {
                    Ok(s) => s,
                    Err(_) => None,
                }
            }
        });
        match item {
            Some(Ok(summary)) => {
                unsafe {
                    *out = NetDeckStatusSummary::from_core(&summary);
                    *has_item_out = 1;
                };
                NET_DECK_OK
            }
            Some(Err(e)) => {
                set_last_error_from_sdk(&e);
                NET_DECK_ERR_CALL_FAILED
            }
            None if timeout_ms == 0 => {
                unsafe { *has_item_out = 0 };
                NET_DECK_ERR_END_OF_STREAM
            }
            None => {
                unsafe { *has_item_out = 0 };
                NET_DECK_OK
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn net_deck_status_summary_stream_free(stream: *mut NetDeckStatusSummaryStream) {
    if stream.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(stream);
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_client(seed_byte: u8) -> *mut NetDeckClient {
        let seed = [seed_byte; 32];
        let mut client: *mut NetDeckClient = ptr::null_mut();
        let status = net_deck_client_new(0, 0, 0, 0, 0, 0, seed.as_ptr(), &mut client);
        assert_eq!(status, NET_DECK_OK);
        assert!(!client.is_null());
        client
    }

    #[test]
    fn client_lifecycle() {
        let client = make_client(0x42);
        let op_id = net_deck_client_operator_id(client);
        let expected = EntityKeypair::from_bytes([0x42; 32]).origin_hash();
        assert_eq!(op_id, expected);
        net_deck_client_free(client);
    }

    #[test]
    fn status_returns_parseable_json_caller_frees() {
        let client = make_client(0x10);
        let json_ptr = net_deck_status(client);
        assert!(!json_ptr.is_null());
        let s = unsafe { CStr::from_ptr(json_ptr).to_string_lossy().into_owned() };
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert!(parsed.is_object());
        net_deck_free_string(json_ptr);
        net_deck_client_free(client);
    }

    #[test]
    fn status_summary_writes_typed_struct() {
        let client = make_client(0x11);
        let mut summary = NetDeckStatusSummary::default();
        let status = net_deck_status_summary(client, &mut summary);
        assert_eq!(status, NET_DECK_OK);
        // Single-node fixture — no peers, no daemons.
        assert_eq!(summary.peers.healthy, 0);
        assert_eq!(summary.daemons.running, 0);
        net_deck_client_free(client);
    }

    #[test]
    fn admin_drain_commits_and_returns_chain_commit() {
        let client = make_client(0x12);
        let mut commit = NetDeckChainCommit::default();
        let status = net_deck_admin_drain(client, 0xABCD, 60_000, &mut commit);
        assert_eq!(status, NET_DECK_OK);
        assert!(commit.commit_id > 0);
        assert_eq!(commit.event_kind, NET_DECK_EVENT_KIND_DRAIN);
        let expected_op = EntityKeypair::from_bytes([0x12; 32]).origin_hash();
        assert_eq!(commit.operator_id, expected_op);
        net_deck_client_free(client);
    }

    #[test]
    fn every_admin_method_commits_with_expected_event_kind() {
        let client = make_client(0x13);
        let node = 0xCAFE;
        let mut c = NetDeckChainCommit::default();

        assert_eq!(
            net_deck_admin_drain(client, node, 1_000, &mut c),
            NET_DECK_OK
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_DRAIN);

        assert_eq!(
            net_deck_admin_enter_maintenance(client, node, 0, 0, &mut c),
            NET_DECK_OK
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_ENTER_MAINTENANCE);

        assert_eq!(
            net_deck_admin_exit_maintenance(client, node, &mut c),
            NET_DECK_OK
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_EXIT_MAINTENANCE);

        assert_eq!(net_deck_admin_cordon(client, node, &mut c), NET_DECK_OK);
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_CORDON);

        assert_eq!(net_deck_admin_uncordon(client, node, &mut c), NET_DECK_OK);
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_UNCORDON);

        let chains: [u64; 2] = [0xDEAD, 0xBEEF];
        assert_eq!(
            net_deck_admin_drop_replicas(client, node, chains.as_ptr(), 2, &mut c),
            NET_DECK_OK,
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_DROP_REPLICAS);

        assert_eq!(
            net_deck_admin_invalidate_placement(client, node, &mut c),
            NET_DECK_OK
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT);

        assert_eq!(
            net_deck_admin_restart_all_daemons(client, node, &mut c),
            NET_DECK_OK
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS);

        assert_eq!(
            net_deck_admin_clear_avoid_list(client, node, &mut c),
            NET_DECK_OK
        );
        assert_eq!(c.event_kind, NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST);

        net_deck_client_free(client);
    }

    #[test]
    fn snapshot_stream_subscribe_next_with_timeout_close() {
        let client = make_client(0x14);
        let mut stream: *mut NetDeckSnapshotStream = ptr::null_mut();
        assert_eq!(
            net_deck_subscribe_snapshots(client, &mut stream),
            NET_DECK_OK
        );
        assert!(!stream.is_null());

        // First call may return immediately or wait one
        // poll-interval (100ms default). Use a 500ms timeout.
        let mut json_ptr: *mut c_char = ptr::null_mut();
        let status = net_deck_snapshot_stream_next(stream, 500, &mut json_ptr);
        assert_eq!(status, NET_DECK_OK);
        // We should get at least one snapshot within 500ms.
        assert!(!json_ptr.is_null(), "expected a snapshot within 500ms");
        net_deck_free_string(json_ptr);

        net_deck_snapshot_stream_free(stream);
        net_deck_client_free(client);
    }

    #[test]
    fn status_summary_stream_subscribe_next_close() {
        let client = make_client(0x15);
        let mut stream: *mut NetDeckStatusSummaryStream = ptr::null_mut();
        assert_eq!(
            net_deck_subscribe_status_summaries(client, &mut stream),
            NET_DECK_OK
        );
        let mut summary = NetDeckStatusSummary::default();
        let mut has_item: c_int = 0;
        let status =
            net_deck_status_summary_stream_next(stream, 500, &mut summary, &mut has_item);
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(has_item, 1);
        net_deck_status_summary_stream_free(stream);
        net_deck_client_free(client);
    }

    #[test]
    fn null_client_returns_invalid_arg() {
        let client: *mut NetDeckClient = ptr::null_mut();
        let mut summary = NetDeckStatusSummary::default();
        assert_eq!(
            net_deck_status_summary(client, &mut summary),
            NET_DECK_ERR_NULL
        );

        let kind_ptr = net_deck_last_error_kind();
        assert!(!kind_ptr.is_null());
        let kind = unsafe { CStr::from_ptr(kind_ptr).to_string_lossy().into_owned() };
        assert_eq!(kind, "invalid_argument");
        net_deck_clear_last_error();
    }
}
