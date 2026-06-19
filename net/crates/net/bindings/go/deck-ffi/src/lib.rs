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
#[cfg(test)]
use std::ffi::CStr;
use std::ffi::{c_char, c_int, c_uint, CString};
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use futures::StreamExt;
use tokio::runtime::Runtime;

use net::adapter::net::behavior::deck::{
    AdminCommands as CoreAdminCommands, AuditQuery as CoreAuditQuery,
    AuditStream as CoreAuditStream, ChainCommit as CoreChainCommit, DeckClient as CoreClient,
    DeckClientConfig as CoreConfig, DeckError, FailureStream as CoreFailureStream,
    IceProposal as CoreIceProposal, LogFilter as CoreLogFilter, LogStream as CoreLogStream,
    OperatorIdentity as CoreIdentity, SnapshotStream as CoreSnapshotStream, StatusSummary,
    StatusSummaryStream as CoreStatusStream,
};
use net::adapter::net::behavior::meshos::logs::LogLevel as CoreLogLevel;
use net::adapter::net::behavior::meshos::{
    blast_radius_hash, ice_proposal_signing_payload, AdminVerifier as CoreAdminVerifier,
    AvoidScope as CoreAvoidScope, ChainId as CoreChainId, DaemonRef as CoreDaemonRef,
    MigrationId as CoreMigrationId, OperatorRegistry as CoreOperatorRegistry,
    OperatorSignature as CoreOperatorSignature, VerifyError as CoreVerifyError,
};
use net::adapter::net::behavior::meshos::{
    LoggingDispatcher, MeshOsConfig, MeshOsDaemonSdk as CoreSdk,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::EntityKeypair;
use parking_lot::Mutex;

// =========================================================================
// Status codes
// =========================================================================

pub const NET_DECK_OK: c_int = 0;
pub const NET_DECK_ERR_NULL: c_int = -1;
pub const NET_DECK_ERR_CALL_FAILED: c_int = -2;
pub const NET_DECK_ERR_INVALID_ARG: c_int = -3;
pub const NET_DECK_ERR_ALREADY_SHUTDOWN: c_int = -4;
pub const NET_DECK_ERR_END_OF_STREAM: c_int = -5;

/// Poll one item from a stream's `.next()` future under an optional
/// timeout, returning `(item, timed_out)`.
///
/// This is the single decision point that keeps a genuine stream-end
/// (`Ok(None)` → `(None, false)`) distinguishable from an elapsed
/// timeout (`Err(Elapsed)` → `(None, true)`). The previous
/// `unwrap_or_default()` collapsed both into `None`, so every `_next`
/// fn reported a closed stream as a timeout (`NET_DECK_OK`, NULL out)
/// for any non-zero `timeout_ms`, making `NET_DECK_ERR_END_OF_STREAM`
/// reachable only with `timeout_ms == 0`. Callers must map
/// `(None, true) → OK/timeout` and `(None, false) → END_OF_STREAM`.
async fn next_with_timeout<F, T>(timeout_ms: u64, fut: F) -> (Option<T>, bool)
where
    F: std::future::Future<Output = Option<T>>,
{
    if timeout_ms == 0 {
        (fut.await, false)
    } else {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
            Ok(item) => (item, false),
            Err(_elapsed) => (None, true),
        }
    }
}

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
        // `Zeroizing` wipes the local stack copy on drop. The
        // caller's source buffer and the substrate's internal
        // keypair are independent copies; documenting the
        // threat-model disclaimer for those lives in net_deck.h.
        let seed = zeroize::Zeroizing::new(
            <[u8; 32]>::try_from(unsafe { std::slice::from_raw_parts(operator_seed_ptr, 32) })
                .expect("slice has len 32"),
        );
        let keypair = EntityKeypair::from_bytes(*seed);
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

fn admin_commit<F>(client: *const NetDeckClient, out: *mut NetDeckChainCommit, op: F) -> c_int
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
        set_last_error(
            "invalid_argument",
            "chains pointer is NULL but chains_len > 0",
        );
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
        let (snap, timed_out) = runtime().block_on(next_with_timeout(timeout_ms, inner.next()));
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
            None if timed_out => {
                // Timeout elapsed without an item.
                unsafe { *out = ptr::null_mut() };
                NET_DECK_OK
            }
            None => {
                // Stream ended naturally (substrate runtime shut down).
                unsafe { *out = ptr::null_mut() };
                NET_DECK_ERR_END_OF_STREAM
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
        let (item, timed_out) = runtime().block_on(next_with_timeout(timeout_ms, inner.next()));
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
            None if timed_out => {
                unsafe { *has_item_out = 0 };
                NET_DECK_OK
            }
            None => {
                unsafe { *has_item_out = 0 };
                NET_DECK_ERR_END_OF_STREAM
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
// Slice 2 — Log levels + LogFilter
// =========================================================================

pub const NET_DECK_LOG_TRACE: c_int = 0;
pub const NET_DECK_LOG_DEBUG: c_int = 1;
pub const NET_DECK_LOG_INFO: c_int = 2;
pub const NET_DECK_LOG_WARN: c_int = 3;
pub const NET_DECK_LOG_ERROR: c_int = 4;

fn log_level_from_c(level: c_int) -> Option<CoreLogLevel> {
    Some(match level {
        NET_DECK_LOG_TRACE => CoreLogLevel::Trace,
        NET_DECK_LOG_DEBUG => CoreLogLevel::Debug,
        NET_DECK_LOG_INFO => CoreLogLevel::Info,
        NET_DECK_LOG_WARN => CoreLogLevel::Warn,
        NET_DECK_LOG_ERROR => CoreLogLevel::Error,
        _ => return None,
    })
}

fn log_level_to_c(level: CoreLogLevel) -> c_int {
    match level {
        CoreLogLevel::Trace => NET_DECK_LOG_TRACE,
        CoreLogLevel::Debug => NET_DECK_LOG_DEBUG,
        CoreLogLevel::Info => NET_DECK_LOG_INFO,
        CoreLogLevel::Warn => NET_DECK_LOG_WARN,
        CoreLogLevel::Error => NET_DECK_LOG_ERROR,
        _ => NET_DECK_LOG_INFO,
    }
}

/// LogFilter — every field is optional; the `_present` bool
/// guards each scalar. Pass NULL to `net_deck_subscribe_logs` for
/// "match everything."
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NetDeckLogFilter {
    pub min_level_present: c_int,
    pub min_level: c_int,
    pub daemon_id_present: c_int,
    pub daemon_id: u64,
    pub node_id_present: c_int,
    pub node_id: u64,
    pub since_seq_present: c_int,
    pub since_seq: u64,
}

impl NetDeckLogFilter {
    fn into_core(self) -> Result<CoreLogFilter, &'static str> {
        let mut f = CoreLogFilter::default();
        if self.min_level_present != 0 {
            f.min_level = Some(log_level_from_c(self.min_level).ok_or("invalid_log_level")?);
        }
        if self.daemon_id_present != 0 {
            f.daemon_id = Some(self.daemon_id);
        }
        if self.node_id_present != 0 {
            f.node_id = Some(self.node_id);
        }
        if self.since_seq_present != 0 {
            f.since_seq = Some(self.since_seq);
        }
        Ok(f)
    }
}

// =========================================================================
// Slice 2 — Log + Failure record wire forms
// =========================================================================

/// LogRecord wire form. The `message` field is a heap-allocated
/// CString owned by the cdylib; caller MUST call
/// `net_deck_log_record_drop` to release it (idempotent on a
/// zero-initialized struct).
#[repr(C)]
pub struct NetDeckLogRecord {
    pub seq: u64,
    pub ts_ms: u64,
    pub level: c_int,
    pub daemon_id_present: c_int,
    pub daemon_id: u64,
    pub node_id_present: c_int,
    pub node_id: u64,
    /// Heap-allocated CString; caller frees via
    /// `net_deck_log_record_drop`.
    pub message: *mut c_char,
}

impl Default for NetDeckLogRecord {
    fn default() -> Self {
        Self {
            seq: 0,
            ts_ms: 0,
            level: NET_DECK_LOG_INFO,
            daemon_id_present: 0,
            daemon_id: 0,
            node_id_present: 0,
            node_id: 0,
            message: ptr::null_mut(),
        }
    }
}

fn log_record_to_c(record: &net::adapter::net::behavior::meshos::LogRecord) -> NetDeckLogRecord {
    let message = CString::new(record.message.clone())
        .unwrap_or_else(|_| CString::new("").expect("empty cstr"))
        .into_raw();
    NetDeckLogRecord {
        seq: record.seq,
        ts_ms: record.ts_ms,
        level: log_level_to_c(record.level),
        daemon_id_present: if record.daemon_id.is_some() { 1 } else { 0 },
        daemon_id: record.daemon_id.unwrap_or(0),
        node_id_present: if record.node_id.is_some() { 1 } else { 0 },
        node_id: record.node_id.unwrap_or(0),
        message,
    }
}

/// Drop a `NetDeckLogRecord`. Frees the heap-allocated `message`
/// pointer. Idempotent on a record whose `message` is NULL.
#[no_mangle]
pub extern "C" fn net_deck_log_record_drop(record: *mut NetDeckLogRecord) {
    let Some(record) = (unsafe { record.as_mut() }) else {
        return;
    };
    if !record.message.is_null() {
        unsafe {
            let _ = CString::from_raw(record.message);
        }
        record.message = ptr::null_mut();
    }
}

/// FailureRecord wire form. `source` and `reason` are heap-
/// allocated CStrings owned by the cdylib; caller MUST call
/// `net_deck_failure_record_drop`.
#[repr(C)]
pub struct NetDeckFailureRecord {
    pub seq: u64,
    pub source: *mut c_char,
    pub reason: *mut c_char,
    pub recorded_at_ms: u64,
}

impl Default for NetDeckFailureRecord {
    fn default() -> Self {
        Self {
            seq: 0,
            source: ptr::null_mut(),
            reason: ptr::null_mut(),
            recorded_at_ms: 0,
        }
    }
}

fn failure_record_to_c(
    record: &net::adapter::net::behavior::meshos::FailureRecord,
) -> NetDeckFailureRecord {
    let source = CString::new(record.source.clone())
        .unwrap_or_else(|_| CString::new("").expect("empty cstr"))
        .into_raw();
    let reason = CString::new(record.reason.clone())
        .unwrap_or_else(|_| CString::new("").expect("empty cstr"))
        .into_raw();
    NetDeckFailureRecord {
        seq: record.seq,
        source,
        reason,
        recorded_at_ms: record.recorded_at_ms,
    }
}

/// Drop a `NetDeckFailureRecord`. Frees the heap-allocated
/// `source` + `reason`. Idempotent on a record whose strings are
/// NULL.
#[no_mangle]
pub extern "C" fn net_deck_failure_record_drop(record: *mut NetDeckFailureRecord) {
    let Some(record) = (unsafe { record.as_mut() }) else {
        return;
    };
    if !record.source.is_null() {
        unsafe {
            let _ = CString::from_raw(record.source);
        }
        record.source = ptr::null_mut();
    }
    if !record.reason.is_null() {
        unsafe {
            let _ = CString::from_raw(record.reason);
        }
        record.reason = ptr::null_mut();
    }
}

// =========================================================================
// Slice 2 — Log + Failure streams
// =========================================================================

pub struct NetDeckLogStream {
    inner: Option<CoreLogStream>,
}

pub struct NetDeckFailureStream {
    inner: Option<CoreFailureStream>,
}

/// Subscribe to the runtime's log ring. `filter` may be NULL —
/// matches every record.
#[no_mangle]
pub extern "C" fn net_deck_subscribe_logs(
    client: *const NetDeckClient,
    filter: *const NetDeckLogFilter,
    out: *mut *mut NetDeckLogStream,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let core_filter = if filter.is_null() {
            CoreLogFilter::default()
        } else {
            match unsafe { *filter }.into_core() {
                Ok(f) => f,
                Err(kind) => {
                    set_last_error(kind, "log filter has an invalid field (likely min_level)");
                    return NET_DECK_ERR_INVALID_ARG;
                }
            }
        };
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
            inner.subscribe_logs(core_filter)
        };
        clear_last_error_inner();
        let boxed = Box::into_raw(Box::new(NetDeckLogStream {
            inner: Some(stream),
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

/// Block up to `timeout_ms` for the next log record. On success
/// writes the record to `*out` (caller frees via
/// `net_deck_log_record_drop`) and sets `*has_item_out = 1`. On
/// timeout sets `*has_item_out = 0` and returns OK. On stream end
/// returns `NET_DECK_ERR_END_OF_STREAM`. Pass `0` for an
/// unbounded wait.
#[no_mangle]
pub extern "C" fn net_deck_log_stream_next(
    stream: *mut NetDeckLogStream,
    timeout_ms: u64,
    out: *mut NetDeckLogRecord,
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
        let (item, timed_out) = runtime().block_on(next_with_timeout(timeout_ms, inner.next()));
        match item {
            Some(Ok(record)) => {
                unsafe {
                    *out = log_record_to_c(&record);
                    *has_item_out = 1;
                };
                NET_DECK_OK
            }
            Some(Err(e)) => {
                set_last_error_from_sdk(&e);
                NET_DECK_ERR_CALL_FAILED
            }
            None if timed_out => {
                unsafe { *has_item_out = 0 };
                NET_DECK_OK
            }
            None => {
                unsafe { *has_item_out = 0 };
                NET_DECK_ERR_END_OF_STREAM
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn net_deck_log_stream_free(stream: *mut NetDeckLogStream) {
    if stream.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(stream);
    }
}

#[no_mangle]
pub extern "C" fn net_deck_subscribe_failures(
    client: *const NetDeckClient,
    since_seq: u64,
    out: *mut *mut NetDeckFailureStream,
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
            inner.subscribe_failures(since_seq)
        };
        clear_last_error_inner();
        let boxed = Box::into_raw(Box::new(NetDeckFailureStream {
            inner: Some(stream),
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

#[no_mangle]
pub extern "C" fn net_deck_failure_stream_next(
    stream: *mut NetDeckFailureStream,
    timeout_ms: u64,
    out: *mut NetDeckFailureRecord,
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
        let (item, timed_out) = runtime().block_on(next_with_timeout(timeout_ms, inner.next()));
        match item {
            Some(Ok(record)) => {
                unsafe {
                    *out = failure_record_to_c(&record);
                    *has_item_out = 1;
                };
                NET_DECK_OK
            }
            Some(Err(e)) => {
                set_last_error_from_sdk(&e);
                NET_DECK_ERR_CALL_FAILED
            }
            None if timed_out => {
                unsafe { *has_item_out = 0 };
                NET_DECK_OK
            }
            None => {
                unsafe { *has_item_out = 0 };
                NET_DECK_ERR_END_OF_STREAM
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn net_deck_failure_stream_free(stream: *mut NetDeckFailureStream) {
    if stream.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(stream);
    }
}

// =========================================================================
// Slice 2 — AuditQuery (fluent builder) + AuditStream
// =========================================================================

/// Audit query builder. Holds only filter state; each builder
/// method takes the parent `NetDeckClient` pointer + the builder
/// pointer, so the builder doesn't need to borrow the client
/// across the FFI.
///
/// **Lifetime contract:** the builder is freestanding — it's safe
/// to free the parent client before / after the builder. The
/// `_collect` / `_stream` calls require a live parent client.
pub struct NetDeckAuditQuery {
    recent_limit: Option<usize>,
    by_operator: Option<u64>,
    between: Option<(u64, u64)>,
    force_only: bool,
    since: Option<u64>,
}

impl NetDeckAuditQuery {
    fn build<'a>(&self, client: &'a CoreClient) -> CoreAuditQuery<'a> {
        let mut q = client.audit();
        if let Some(n) = self.recent_limit {
            q = q.recent(n);
        }
        if let Some(op) = self.by_operator {
            q = q.by_operator(op);
        }
        if let Some((start, end)) = self.between {
            q = q.between(start, end);
        }
        if self.force_only {
            q = q.force_only();
        }
        if let Some(s) = self.since {
            q = q.since(s);
        }
        q
    }
}

pub struct NetDeckAuditStream {
    inner: Option<CoreAuditStream>,
}

/// Construct a new audit query. Holds only filter state; the
/// client pointer is supplied on `_collect` / `_stream`.
#[no_mangle]
pub extern "C" fn net_deck_audit_query_new(out: *mut *mut NetDeckAuditQuery) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        clear_last_error_inner();
        let boxed = Box::into_raw(Box::new(NetDeckAuditQuery {
            recent_limit: None,
            by_operator: None,
            between: None,
            force_only: false,
            since: None,
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

/// Free a freestanding audit query builder. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_audit_query_free(query: *mut NetDeckAuditQuery) {
    if query.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(query);
    }
}

// The audit-query setters clear the thread-local last-error pair on
// success so a successful setter doesn't leave a stale `kind` from
// an earlier unrelated failure visible to the caller's next
// `net_deck_last_error_kind()` read. They're not wrapped in
// `ffi_guard!` because the body is pure pointer-tag-and-assign with
// no panic surface.

#[no_mangle]
pub extern "C" fn net_deck_audit_query_recent(
    query: *mut NetDeckAuditQuery,
    limit: usize,
) -> c_int {
    let Some(q) = (unsafe { query.as_mut() }) else {
        set_last_error("invalid_argument", "query pointer is NULL");
        return NET_DECK_ERR_NULL;
    };
    q.recent_limit = Some(limit);
    clear_last_error_inner();
    NET_DECK_OK
}

#[no_mangle]
pub extern "C" fn net_deck_audit_query_by_operator(
    query: *mut NetDeckAuditQuery,
    operator_id: u64,
) -> c_int {
    let Some(q) = (unsafe { query.as_mut() }) else {
        set_last_error("invalid_argument", "query pointer is NULL");
        return NET_DECK_ERR_NULL;
    };
    q.by_operator = Some(operator_id);
    clear_last_error_inner();
    NET_DECK_OK
}

#[no_mangle]
pub extern "C" fn net_deck_audit_query_between(
    query: *mut NetDeckAuditQuery,
    start_ms: u64,
    end_ms: u64,
) -> c_int {
    let Some(q) = (unsafe { query.as_mut() }) else {
        set_last_error("invalid_argument", "query pointer is NULL");
        return NET_DECK_ERR_NULL;
    };
    q.between = Some((start_ms, end_ms));
    clear_last_error_inner();
    NET_DECK_OK
}

#[no_mangle]
pub extern "C" fn net_deck_audit_query_force_only(query: *mut NetDeckAuditQuery) -> c_int {
    let Some(q) = (unsafe { query.as_mut() }) else {
        set_last_error("invalid_argument", "query pointer is NULL");
        return NET_DECK_ERR_NULL;
    };
    q.force_only = true;
    clear_last_error_inner();
    NET_DECK_OK
}

#[no_mangle]
pub extern "C" fn net_deck_audit_query_since(query: *mut NetDeckAuditQuery, seq: u64) -> c_int {
    let Some(q) = (unsafe { query.as_mut() }) else {
        set_last_error("invalid_argument", "query pointer is NULL");
        return NET_DECK_ERR_NULL;
    };
    q.since = Some(seq);
    clear_last_error_inner();
    NET_DECK_OK
}

/// Collect audit records as an array of heap-allocated JSON
/// strings. On success writes the count to `*count_out` and a
/// heap-allocated `char**` to `*records_out`. Caller frees via
/// `net_deck_audit_records_free(records, count)`.
#[no_mangle]
pub extern "C" fn net_deck_audit_query_collect(
    query: *const NetDeckAuditQuery,
    client: *const NetDeckClient,
    records_out: *mut *mut *mut c_char,
    count_out: *mut usize,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if records_out.is_null() || count_out.is_null() {
            set_last_error("invalid_argument", "records_out / count_out is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(q) = (unsafe { query.as_ref() }) else {
            set_last_error("invalid_argument", "query pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        clear_last_error_inner();
        let records = q.build(inner).collect();
        let mut json_cstrings: Vec<*mut c_char> = Vec::with_capacity(records.len());
        for r in &records {
            let s = match serde_json::to_string(r) {
                Ok(s) => s,
                Err(e) => {
                    // Clean up anything we've already allocated.
                    for ptr in json_cstrings.drain(..) {
                        if !ptr.is_null() {
                            unsafe {
                                let _ = CString::from_raw(ptr);
                            }
                        }
                    }
                    set_last_error("audit_serialize_failed", &e.to_string());
                    return NET_DECK_ERR_CALL_FAILED;
                }
            };
            let c = match CString::new(s) {
                Ok(c) => c,
                Err(_) => {
                    for ptr in json_cstrings.drain(..) {
                        if !ptr.is_null() {
                            unsafe {
                                let _ = CString::from_raw(ptr);
                            }
                        }
                    }
                    set_last_error("audit_serialize_failed", "JSON contained NUL byte");
                    return NET_DECK_ERR_CALL_FAILED;
                }
            };
            json_cstrings.push(c.into_raw());
        }
        let count = json_cstrings.len();
        let boxed_array = json_cstrings.into_boxed_slice();
        let ptr = Box::into_raw(boxed_array) as *mut *mut c_char;
        unsafe {
            *records_out = ptr;
            *count_out = count;
        };
        NET_DECK_OK
    })
}

/// Free an array of audit records returned by `_collect`. Frees
/// each heap-allocated JSON CString + the outer array. Idempotent
/// on NULL.
#[no_mangle]
pub extern "C" fn net_deck_audit_records_free(records: *mut *mut c_char, count: usize) {
    if records.is_null() || count == 0 {
        if !records.is_null() {
            unsafe {
                let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(records, 0));
            }
        }
        return;
    }
    unsafe {
        let slice = std::slice::from_raw_parts_mut(records, count);
        for ptr in slice.iter() {
            if !ptr.is_null() {
                let _ = CString::from_raw(*ptr);
            }
        }
        let _ = Box::from_raw(slice as *mut [*mut c_char]);
    }
}

#[no_mangle]
pub extern "C" fn net_deck_audit_query_stream(
    query: *const NetDeckAuditQuery,
    client: *const NetDeckClient,
    out: *mut *mut NetDeckAuditStream,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(q) = (unsafe { query.as_ref() }) else {
            set_last_error("invalid_argument", "query pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        clear_last_error_inner();
        let stream = {
            let _enter = runtime().enter();
            q.build(inner).stream()
        };
        let boxed = Box::into_raw(Box::new(NetDeckAuditStream {
            inner: Some(stream),
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

/// Block up to `timeout_ms` for the next audit record. On success
/// writes a heap-allocated JSON CString to `*out` (caller frees
/// via `net_deck_free_string`) and returns NET_DECK_OK. On timeout
/// returns NET_DECK_OK with `*out = NULL`. On stream end returns
/// NET_DECK_ERR_END_OF_STREAM. Pass `0` for an unbounded wait.
#[no_mangle]
pub extern "C" fn net_deck_audit_stream_next(
    stream: *mut NetDeckAuditStream,
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
        let (item, timed_out) = runtime().block_on(next_with_timeout(timeout_ms, inner.next()));
        match item {
            Some(Ok(r)) => {
                let json = match serde_json::to_string(&r) {
                    Ok(s) => s,
                    Err(e) => {
                        set_last_error("audit_serialize_failed", &e.to_string());
                        return NET_DECK_ERR_CALL_FAILED;
                    }
                };
                let c = match CString::new(json) {
                    Ok(c) => c,
                    Err(_) => {
                        set_last_error("audit_serialize_failed", "JSON contained NUL byte");
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
            None if timed_out => {
                unsafe { *out = ptr::null_mut() };
                NET_DECK_OK
            }
            None => {
                unsafe { *out = ptr::null_mut() };
                NET_DECK_ERR_END_OF_STREAM
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn net_deck_audit_stream_free(stream: *mut NetDeckAuditStream) {
    if stream.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(stream);
    }
}

// =========================================================================
// Slice 3 — ICE break-glass surface
//
// Typestate enforced via two distinct opaque pointer types:
// `NetDeckIceProposal*` has no commit function; `commit` accepts
// only `NetDeckSimulatedIceProposal*`. The substrate's compile-
// time typestate translates to API-shape enforcement at the C
// boundary.
// =========================================================================

/// AvoidScope kind discriminator. Pass to `flush_avoid_lists`.
pub const NET_DECK_AVOID_SCOPE_GLOBAL: c_int = 0;
pub const NET_DECK_AVOID_SCOPE_LOCAL: c_int = 1;
pub const NET_DECK_AVOID_SCOPE_ON_PEER: c_int = 2;

fn avoid_scope_from_c(kind: c_int, node: u64, peer: u64) -> Result<CoreAvoidScope, &'static str> {
    Ok(match kind {
        NET_DECK_AVOID_SCOPE_GLOBAL => CoreAvoidScope::Global,
        NET_DECK_AVOID_SCOPE_LOCAL => CoreAvoidScope::Local { node },
        NET_DECK_AVOID_SCOPE_ON_PEER => CoreAvoidScope::OnPeer { peer },
        _ => return Err("invalid_avoid_scope"),
    })
}

/// `OperatorSignature` wire form. `signature_ptr` MUST point to
/// exactly 64 ed25519 signature bytes.
#[repr(C)]
#[derive(Debug)]
pub struct NetDeckOperatorSignature {
    pub operator_id: u64,
    pub signature_ptr: *const u8,
    pub signature_len: usize,
}

/// Internal owned struct for the substrate-side `OperatorSignature`.
struct OwnedOperatorSignature {
    operator_id: u64,
    signature: Vec<u8>,
}

impl OwnedOperatorSignature {
    fn to_core(&self) -> CoreOperatorSignature {
        CoreOperatorSignature {
            operator_id: self.operator_id,
            signature: self.signature.clone(),
        }
    }
}

unsafe fn signatures_from_c(
    sigs_ptr: *const NetDeckOperatorSignature,
    sigs_count: usize,
) -> Result<Vec<OwnedOperatorSignature>, &'static str> {
    if sigs_count == 0 {
        return Ok(Vec::new());
    }
    if sigs_ptr.is_null() {
        return Err("invalid_signature");
    }
    let slice = unsafe { std::slice::from_raw_parts(sigs_ptr, sigs_count) };
    let mut out = Vec::with_capacity(sigs_count);
    for sig in slice {
        if sig.signature_ptr.is_null() || sig.signature_len == 0 {
            return Err("invalid_signature");
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(sig.signature_ptr, sig.signature_len) }.to_vec();
        out.push(OwnedOperatorSignature {
            operator_id: sig.operator_id,
            signature: bytes,
        });
    }
    Ok(out)
}

#[cfg(test)]
thread_local! {
    /// Per-thread fault-injection switch. Set to `true` to force
    /// the next `build_core_proposal` call on this thread to
    /// return `Err`; the call resets it. Thread-local so parallel
    /// `cargo test` runs don't poison each other's expectations.
    static FAIL_NEXT_BUILD_CORE_PROPOSAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Build a substrate `IceProposal` from a saved action. The
/// substrate's factories pin a fresh `issued_at_ms` per call;
/// the simulator is pure over the latest snapshot.
///
/// `IceActionProposal` is `#[non_exhaustive]` — an unknown
/// variant returns `Err` rather than silently mapping to
/// `ThawCluster` (the most destructive action). Callers
/// translate the error into the standard last-error envelope
/// with kind `"unknown_action"`.
fn build_core_proposal<'a>(
    client: &'a CoreClient,
    action: net::adapter::net::behavior::meshos::IceActionProposal,
) -> Result<CoreIceProposal<'a>, String> {
    #[cfg(test)]
    if FAIL_NEXT_BUILD_CORE_PROPOSAL.with(|c| c.replace(false)) {
        return Err("fault injection: simulated unknown variant".to_string());
    }
    use net::adapter::net::behavior::meshos::IceActionProposal as A;
    match action {
        A::FreezeCluster { ttl } => Ok(client.ice().freeze_cluster(ttl)),
        A::FlushAvoidLists { scope } => Ok(client.ice().flush_avoid_lists(scope)),
        A::ForceEvictReplica { chain, victim } => {
            Ok(client.ice().force_evict_replica(chain, victim))
        }
        A::ForceRestartDaemon { daemon } => Ok(client.ice().force_restart_daemon(daemon)),
        A::ForceCutover { chain, target } => Ok(client.ice().force_cutover(chain, target)),
        A::KillMigration { migration } => Ok(client.ice().kill_migration(migration)),
        A::ThawCluster => Ok(client.ice().thaw_cluster()),
        other => Err(format!(
            "IceActionProposal carries an unknown variant ({other:?}); \
             rebuild the SDK binding against the current substrate"
        )),
    }
}

// =========================================================================
// IceProposal — pre-simulation handle
// =========================================================================

pub struct NetDeckIceProposal {
    action: net::adapter::net::behavior::meshos::IceActionProposal,
    issued_at_ms: u64,
    /// `true` once `net_deck_ice_proposal_simulate` consumed the
    /// proposal; subsequent simulate calls return `already_simulated`.
    /// Mirrors the `committed` flag on `NetDeckSimulatedIceProposal`.
    /// Replaces the previous `issued_at_ms = u64::MAX` sentinel,
    /// which leaked into `net_deck_ice_proposal_issued_at_ms` and
    /// made a consumed proposal report a wildly different stamp.
    consumed: bool,
}

fn make_ice_proposal_handle(
    client: &CoreClient,
    proposal: CoreIceProposal<'_>,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
    let _ = client;
    let action = proposal.action().clone();
    let issued_at_ms = proposal.issued_at_ms();
    let boxed = Box::into_raw(Box::new(NetDeckIceProposal {
        action,
        issued_at_ms,
        consumed: false,
    }));
    unsafe { *out = boxed };
    NET_DECK_OK
}

#[no_mangle]
pub extern "C" fn net_deck_ice_freeze_cluster(
    client: *const NetDeckClient,
    ttl_ms: u64,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
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
        let proposal = inner.ice().freeze_cluster(Duration::from_millis(ttl_ms));
        make_ice_proposal_handle(inner, proposal, out)
    })
}

#[no_mangle]
pub extern "C" fn net_deck_ice_flush_avoid_lists(
    client: *const NetDeckClient,
    scope_kind: c_int,
    scope_node: u64,
    scope_peer: u64,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let scope = match avoid_scope_from_c(scope_kind, scope_node, scope_peer) {
            Ok(s) => s,
            Err(kind) => {
                set_last_error(
                    kind,
                    "scope_kind must be NET_DECK_AVOID_SCOPE_{GLOBAL|LOCAL|ON_PEER}",
                );
                return NET_DECK_ERR_INVALID_ARG;
            }
        };
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        clear_last_error_inner();
        let proposal = inner.ice().flush_avoid_lists(scope);
        make_ice_proposal_handle(inner, proposal, out)
    })
}

#[no_mangle]
pub extern "C" fn net_deck_ice_force_evict_replica(
    client: *const NetDeckClient,
    chain: u64,
    victim: u64,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
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
        let proposal = inner
            .ice()
            .force_evict_replica(chain as CoreChainId, victim);
        make_ice_proposal_handle(inner, proposal, out)
    })
}

/// `name_ptr` / `name_len` is the daemon's `MeshDaemon::name()`
/// (UTF-8, NOT NUL-terminated).
#[no_mangle]
pub extern "C" fn net_deck_ice_force_restart_daemon(
    client: *const NetDeckClient,
    id: u64,
    name_ptr: *const c_char,
    name_len: usize,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        if name_ptr.is_null() {
            set_last_error("invalid_argument", "name pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let name = unsafe { std::slice::from_raw_parts(name_ptr as *const u8, name_len) };
        let name = match std::str::from_utf8(name) {
            Ok(s) => s.to_string(),
            Err(_) => {
                set_last_error("invalid_argument", "daemon name is not valid UTF-8");
                return NET_DECK_ERR_INVALID_ARG;
            }
        };
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        clear_last_error_inner();
        let daemon = CoreDaemonRef { id, name };
        let proposal = inner.ice().force_restart_daemon(daemon);
        make_ice_proposal_handle(inner, proposal, out)
    })
}

#[no_mangle]
pub extern "C" fn net_deck_ice_force_cutover(
    client: *const NetDeckClient,
    chain: u64,
    target: u64,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
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
        let proposal = inner.ice().force_cutover(chain as CoreChainId, target);
        make_ice_proposal_handle(inner, proposal, out)
    })
}

#[no_mangle]
pub extern "C" fn net_deck_ice_kill_migration(
    client: *const NetDeckClient,
    migration: u64,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
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
        let proposal = inner.ice().kill_migration(migration as CoreMigrationId);
        make_ice_proposal_handle(inner, proposal, out)
    })
}

#[no_mangle]
pub extern "C" fn net_deck_ice_thaw_cluster(
    client: *const NetDeckClient,
    out: *mut *mut NetDeckIceProposal,
) -> c_int {
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
        let proposal = inner.ice().thaw_cluster();
        make_ice_proposal_handle(inner, proposal, out)
    })
}

/// Read the proposal's pinned `issued_at_ms` stamp. Returns 0 on
/// NULL.
#[no_mangle]
pub extern "C" fn net_deck_ice_proposal_issued_at_ms(proposal: *const NetDeckIceProposal) -> u64 {
    match unsafe { proposal.as_ref() } {
        Some(p) => p.issued_at_ms,
        None => 0,
    }
}

/// Free a freestanding IceProposal. Idempotent on NULL.
/// Calling this after a successful `simulate` is fine — the
/// proposal's `consumed` flag is set so re-simulating returns
/// `already_simulated`; the underlying box still needs this
/// `_free` call to release.
#[no_mangle]
pub extern "C" fn net_deck_ice_proposal_free(proposal: *mut NetDeckIceProposal) {
    if proposal.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(proposal);
    }
}

// =========================================================================
// SimulatedIceProposal — only struct exposing commit
// =========================================================================

pub struct NetDeckSimulatedIceProposal {
    action: net::adapter::net::behavior::meshos::IceActionProposal,
    issued_at_ms: u64,
    blast: net::adapter::net::behavior::meshos::BlastRadius,
    committed: bool,
}

/// Consume the proposal and run the substrate simulator. On
/// success writes a `*NetDeckSimulatedIceProposal` to `*out`
/// (caller frees via `net_deck_simulated_free`) and returns
/// NET_DECK_OK. The proposal pointer becomes a husk — caller
/// still must `net_deck_ice_proposal_free` to release it.
///
/// Already-simulated proposals (where `simulate` already ran)
/// return NET_DECK_ERR_CALL_FAILED with kind `already_simulated`.
#[no_mangle]
pub extern "C" fn net_deck_ice_proposal_simulate(
    proposal: *mut NetDeckIceProposal,
    client: *const NetDeckClient,
    out: *mut *mut NetDeckSimulatedIceProposal,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(p) = (unsafe { proposal.as_mut() }) else {
            set_last_error("invalid_argument", "proposal pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        if p.consumed {
            set_last_error(
                "already_simulated",
                "IceProposal was already consumed by simulate()",
            );
            return NET_DECK_ERR_CALL_FAILED;
        }
        let action = p.action.clone();
        let issued_at_ms = p.issued_at_ms;
        clear_last_error_inner();
        // Validate the variant before flipping `consumed` so an
        // unknown-variant rejection leaves the husk retry-able
        // instead of stranding it as consumed-but-unused.
        let core_proposal = match build_core_proposal(inner, action.clone()) {
            Ok(p) => p,
            Err(msg) => {
                set_last_error("unknown_action", &msg);
                return NET_DECK_ERR_CALL_FAILED;
            }
        };
        // From here on the husk represents a real attempt. Mark
        // consumed; `issued_at_ms` stays valid so
        // `net_deck_ice_proposal_issued_at_ms` keeps returning
        // the pinned value.
        p.consumed = true;
        let blast = match runtime().block_on(core_proposal.simulate()) {
            Ok(sim) => sim.blast_radius().clone(),
            Err(e) => {
                set_last_error_from_sdk(&e);
                return NET_DECK_ERR_CALL_FAILED;
            }
        };
        let boxed = Box::into_raw(Box::new(NetDeckSimulatedIceProposal {
            action,
            issued_at_ms,
            blast,
            committed: false,
        }));
        unsafe { *out = boxed };
        NET_DECK_OK
    })
}

/// Read the pinned `issued_at_ms` stamp.
#[no_mangle]
pub extern "C" fn net_deck_simulated_issued_at_ms(
    simulated: *const NetDeckSimulatedIceProposal,
) -> u64 {
    match unsafe { simulated.as_ref() } {
        Some(s) => s.issued_at_ms,
        None => 0,
    }
}

/// Return the blast radius as a heap-allocated JSON CString.
/// Caller frees via `net_deck_free_string`. Returns NULL on
/// NULL handle (last-error populated).
#[no_mangle]
pub extern "C" fn net_deck_simulated_blast_radius(
    simulated: *const NetDeckSimulatedIceProposal,
) -> *mut c_char {
    ffi_guard!(ptr::null_mut(), {
        let Some(s) = (unsafe { simulated.as_ref() }) else {
            set_last_error("invalid_argument", "simulated pointer is NULL");
            return ptr::null_mut();
        };
        match serde_json::to_string(&s.blast) {
            Ok(j) => match CString::new(j) {
                Ok(c) => c.into_raw(),
                Err(_) => {
                    set_last_error("blast_serialize_failed", "JSON contained NUL byte");
                    ptr::null_mut()
                }
            },
            Err(e) => {
                set_last_error("blast_serialize_failed", &e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Write the 32-byte Blake3 digest of the blast radius into
/// `out_buf`. `out_buf` MUST point to at least 32 writable bytes.
#[no_mangle]
pub extern "C" fn net_deck_simulated_blast_hash(
    simulated: *const NetDeckSimulatedIceProposal,
    out_buf: *mut u8,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out_buf.is_null() {
            set_last_error("invalid_argument", "out_buf is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(s) = (unsafe { simulated.as_ref() }) else {
            set_last_error("invalid_argument", "simulated pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let hash = net::adapter::net::behavior::meshos::blast_radius_hash(&s.blast);
        let bytes = hash.as_ref();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, 32);
        }
        NET_DECK_OK
    })
}

/// Commit the simulated proposal. Consumes the inner state —
/// subsequent calls return `already_committed`. `sigs_ptr` may
/// be NULL when `sigs_count == 0`.
#[no_mangle]
pub extern "C" fn net_deck_simulated_commit(
    simulated: *mut NetDeckSimulatedIceProposal,
    client: *const NetDeckClient,
    sigs_ptr: *const NetDeckOperatorSignature,
    sigs_count: usize,
    out: *mut NetDeckChainCommit,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error("invalid_argument", "out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(s) = (unsafe { simulated.as_mut() }) else {
            set_last_error("invalid_argument", "simulated pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        if s.committed {
            set_last_error(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            );
            return NET_DECK_ERR_CALL_FAILED;
        }
        let Some(c) = (unsafe { client.as_ref() }) else {
            set_last_error("invalid_argument", "client pointer is NULL");
            return NET_DECK_ERR_NULL;
        };
        let Some(inner) = c.client.as_ref() else {
            set_last_error("already_shutdown", "DeckClient was already freed");
            return NET_DECK_ERR_ALREADY_SHUTDOWN;
        };
        let sigs = match unsafe { signatures_from_c(sigs_ptr, sigs_count) } {
            Ok(s) => s,
            Err(kind) => {
                set_last_error(
                    kind,
                    "signature array carries invalid entries (NULL pointer or zero length)",
                );
                return NET_DECK_ERR_INVALID_ARG;
            }
        };
        clear_last_error_inner();
        let action = s.action.clone();
        let core_sigs: Vec<CoreOperatorSignature> = sigs.iter().map(|x| x.to_core()).collect();
        // Validate the variant before flipping `committed` so an
        // unknown-variant rejection leaves the husk retry-able.
        let proposal = match build_core_proposal(inner, action) {
            Ok(p) => p,
            Err(msg) => {
                set_last_error("unknown_action", &msg);
                return NET_DECK_ERR_CALL_FAILED;
            }
        };
        // From here on the husk represents a real commit attempt;
        // refusing a retry is the correct behaviour even if the
        // substrate-side commit later errors.
        s.committed = true;
        let commit = runtime().block_on(async move {
            let simulated = proposal.simulate().await?;
            simulated.commit(&core_sigs).await
        });
        match commit {
            Ok(c) => {
                unsafe { *out = chain_commit_to_c(&c) };
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
pub extern "C" fn net_deck_simulated_free(simulated: *mut NetDeckSimulatedIceProposal) {
    if simulated.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(simulated);
    }
}

/// Return the deterministic ICE signing payload bytes (`ICE_SIGNING_DOMAIN
/// || issued_at_ms (le u64) || blast_hash (32) || postcard(action)`) as
/// a heap-allocated buffer. On success writes the buffer pointer to
/// `*out_ptr` and the byte count to `*out_len`. The buffer MUST be
/// released via `net_deck_signing_payload_free(ptr, len)`.
///
/// Returns `NET_DECK_ERR_CALL_FAILED` with kind `already_committed` if
/// the proposal has been consumed by `net_deck_simulated_commit`.
#[no_mangle]
pub extern "C" fn net_deck_simulated_signing_payload(
    simulated: *const NetDeckSimulatedIceProposal,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out_ptr.is_null() || out_len.is_null() {
            set_last_error("invalid_argument", "out_ptr / out_len is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(s) = (unsafe { simulated.as_ref() }) else {
            set_last_error("invalid_argument", "simulated pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        if s.committed {
            set_last_error(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            );
            return NET_DECK_ERR_CALL_FAILED;
        }
        let hash = blast_radius_hash(&s.blast);
        let payload = ice_proposal_signing_payload(&s.action, s.issued_at_ms, &hash);
        let len = payload.len();
        let mut boxed = payload.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);
        unsafe {
            *out_ptr = ptr;
            *out_len = len;
        }
        NET_DECK_OK
    })
}

/// Free a buffer returned by `net_deck_simulated_signing_payload`.
/// Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_signing_payload_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len));
    }
}

// =========================================================================
// OperatorIdentity opaque handle
//
// Decouples the operator identity from `net_deck_client_new` for
// the offline signing flow + operator-policy authoring. Existing
// `net_deck_client_new(... seed_ptr)` API still works; this is
// additive.
// =========================================================================

pub struct NetDeckOperatorIdentity {
    inner: CoreIdentity,
}

/// Generate a fresh ed25519 keypair + operator identity. Heap-
/// allocated; caller frees via `net_deck_operator_identity_free`.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_generate() -> *mut NetDeckOperatorIdentity {
    Box::into_raw(Box::new(NetDeckOperatorIdentity {
        inner: CoreIdentity::generate(),
    }))
}

/// Load an operator identity from a 32-byte ed25519 seed. Writes
/// the handle to `*out`. Returns `NET_DECK_ERR_INVALID_ARG` on
/// NULL pointers.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_from_seed(
    seed_ptr: *const u8,
    out: *mut *mut NetDeckOperatorIdentity,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if seed_ptr.is_null() || out.is_null() {
            set_last_error("invalid_argument", "seed_ptr / out pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        // `Zeroizing` wipes the local stack copy on drop.
        let seed = zeroize::Zeroizing::new(
            <[u8; 32]>::try_from(unsafe { std::slice::from_raw_parts(seed_ptr, 32) })
                .expect("slice has len 32"),
        );
        let identity = NetDeckOperatorIdentity {
            inner: CoreIdentity::from_keypair(EntityKeypair::from_bytes(*seed)),
        };
        unsafe { *out = Box::into_raw(Box::new(identity)) };
        NET_DECK_OK
    })
}

/// Return the operator id (the keypair's origin hash). Returns 0
/// on NULL handle.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_operator_id(
    identity: *const NetDeckOperatorIdentity,
) -> u64 {
    match unsafe { identity.as_ref() } {
        Some(i) => i.inner.operator_id(),
        None => 0,
    }
}

/// Write the 32-byte ed25519 public key into `out_buf`. `out_buf`
/// MUST point to at least 32 writable bytes.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_public_key(
    identity: *const NetDeckOperatorIdentity,
    out_buf: *mut u8,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out_buf.is_null() {
            set_last_error("invalid_argument", "out_buf is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(i) = (unsafe { identity.as_ref() }) else {
            set_last_error("invalid_argument", "identity pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let bytes = i.inner.keypair().entity_id().as_bytes();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, 32);
        }
        NET_DECK_OK
    })
}

/// Sign a simulated ICE proposal. On success writes the operator
/// id to `*out_operator_id` and the 64-byte ed25519 signature
/// into `out_signature`. `out_signature` MUST point to at least
/// 64 writable bytes.
///
/// Returns `already_committed` if the simulated proposal has
/// been consumed by `net_deck_simulated_commit`.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_sign_proposal(
    identity: *const NetDeckOperatorIdentity,
    simulated: *const NetDeckSimulatedIceProposal,
    out_operator_id: *mut u64,
    out_signature: *mut u8,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out_operator_id.is_null() || out_signature.is_null() {
            set_last_error(
                "invalid_argument",
                "out_operator_id / out_signature is NULL",
            );
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(i) = (unsafe { identity.as_ref() }) else {
            set_last_error("invalid_argument", "identity pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let Some(s) = (unsafe { simulated.as_ref() }) else {
            set_last_error("invalid_argument", "simulated pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        if s.committed {
            set_last_error(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            );
            return NET_DECK_ERR_CALL_FAILED;
        }
        let hash = blast_radius_hash(&s.blast);
        let sig = i.inner.sign_proposal(&s.action, s.issued_at_ms, &hash);
        unsafe {
            *out_operator_id = sig.operator_id;
            std::ptr::copy_nonoverlapping(sig.signature.as_ptr(), out_signature, 64);
        }
        NET_DECK_OK
    })
}

/// Sign raw payload bytes with this operator's ed25519 key.
/// Useful for offline / cross-deck signing flows where the
/// signing payload is exchanged out-of-band.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_sign_payload(
    identity: *const NetDeckOperatorIdentity,
    payload_ptr: *const u8,
    payload_len: usize,
    out_operator_id: *mut u64,
    out_signature: *mut u8,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if out_operator_id.is_null() || out_signature.is_null() {
            set_last_error(
                "invalid_argument",
                "out_operator_id / out_signature is NULL",
            );
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(i) = (unsafe { identity.as_ref() }) else {
            set_last_error("invalid_argument", "identity pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let payload: &[u8] = if payload_len == 0 {
            &[]
        } else if payload_ptr.is_null() {
            set_last_error(
                "invalid_argument",
                "payload_ptr is NULL but payload_len > 0",
            );
            return NET_DECK_ERR_INVALID_ARG;
        } else if payload_len > isize::MAX as usize {
            // `slice::from_raw_parts` requires `len <= isize::MAX`; a
            // sign-extended `-1` (or any oversized length) from cgo would
            // be immediate UB. Reject explicitly, matching core `ffi/mod.rs`.
            set_last_error("invalid_argument", "payload_len exceeds isize::MAX");
            return NET_DECK_ERR_INVALID_ARG;
        } else {
            unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) }
        };
        let sig = i.inner.keypair().sign(payload);
        unsafe {
            *out_operator_id = i.inner.operator_id();
            std::ptr::copy_nonoverlapping(sig.to_bytes().as_ptr(), out_signature, 64);
        }
        NET_DECK_OK
    })
}

/// Free an operator identity. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_operator_identity_free(identity: *mut NetDeckOperatorIdentity) {
    if identity.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(identity);
    }
}

// =========================================================================
// OperatorRegistry opaque handle
// =========================================================================

pub struct NetDeckOperatorRegistry {
    inner: Mutex<CoreOperatorRegistry>,
}

impl NetDeckOperatorRegistry {
    fn snapshot_arc(&self) -> Arc<CoreOperatorRegistry> {
        Arc::new(self.inner.lock().clone())
    }
}

/// Map a substrate `VerifyError` to the last-error envelope. Sets
/// the kind to the substrate's stable discriminator and the
/// message to `e.to_string()`.
fn set_verify_error(e: CoreVerifyError) {
    set_last_error(e.kind(), &e.to_string());
}

/// Create a new empty operator registry. Caller frees via
/// `net_deck_operator_registry_free`.
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_new() -> *mut NetDeckOperatorRegistry {
    Box::into_raw(Box::new(NetDeckOperatorRegistry {
        inner: Mutex::new(CoreOperatorRegistry::new()),
    }))
}

/// Insert an operator's 32-byte ed25519 public key under
/// `operator_id`. `public_key` MUST point to at least 32 bytes.
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_insert(
    registry: *mut NetDeckOperatorRegistry,
    operator_id: u64,
    public_key: *const u8,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        if public_key.is_null() {
            set_last_error("invalid_public_key", "public_key pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        }
        let Some(r) = (unsafe { registry.as_ref() }) else {
            set_last_error("invalid_argument", "registry pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let mut bytes = [0u8; 32];
        unsafe {
            std::ptr::copy_nonoverlapping(public_key, bytes.as_mut_ptr(), 32);
        }
        let entity_id = EntityId::from_bytes(bytes);
        r.inner.lock().insert(operator_id, entity_id);
        NET_DECK_OK
    })
}

/// Register an `OperatorIdentity`'s public key under its derived
/// operator id (the keypair's origin hash).
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_register(
    registry: *mut NetDeckOperatorRegistry,
    identity: *const NetDeckOperatorIdentity,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        let Some(r) = (unsafe { registry.as_ref() }) else {
            set_last_error("invalid_argument", "registry pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let Some(i) = (unsafe { identity.as_ref() }) else {
            set_last_error("invalid_argument", "identity pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        r.inner.lock().register(i.inner.keypair());
        NET_DECK_OK
    })
}

/// `1` iff `operator_id` is registered; `0` otherwise. Returns
/// `-1` on NULL pointer (no last-error set).
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_contains(
    registry: *const NetDeckOperatorRegistry,
    operator_id: u64,
) -> c_int {
    let Some(r) = (unsafe { registry.as_ref() }) else {
        return NET_DECK_ERR_NULL;
    };
    if r.inner.lock().contains(operator_id) {
        1
    } else {
        0
    }
}

/// Number of registered operators. Returns 0 on NULL.
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_len(
    registry: *const NetDeckOperatorRegistry,
) -> usize {
    match unsafe { registry.as_ref() } {
        Some(r) => r.inner.lock().len(),
        None => 0,
    }
}

/// Verify a single signature over `payload`. On success returns
/// `NET_DECK_OK`. On failure sets the last-error kind to the
/// substrate's stable discriminator (`not_authorized`,
/// `signature_invalid`) and returns `NET_DECK_ERR_CALL_FAILED`.
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_verify(
    registry: *const NetDeckOperatorRegistry,
    signature: *const NetDeckOperatorSignature,
    payload_ptr: *const u8,
    payload_len: usize,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        let Some(r) = (unsafe { registry.as_ref() }) else {
            set_last_error("invalid_argument", "registry pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let Some(sig_in) = (unsafe { signature.as_ref() }) else {
            set_last_error("invalid_signature", "signature pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        if sig_in.signature_ptr.is_null() || sig_in.signature_len == 0 {
            set_last_error(
                "invalid_signature",
                "signature buffer is NULL or zero-length",
            );
            return NET_DECK_ERR_INVALID_ARG;
        }
        let sig_bytes =
            unsafe { std::slice::from_raw_parts(sig_in.signature_ptr, sig_in.signature_len) }
                .to_vec();
        let core_sig = CoreOperatorSignature {
            operator_id: sig_in.operator_id,
            signature: sig_bytes,
        };
        let payload: &[u8] = if payload_len == 0 {
            &[]
        } else if payload_ptr.is_null() {
            set_last_error(
                "invalid_argument",
                "payload_ptr is NULL but payload_len > 0",
            );
            return NET_DECK_ERR_INVALID_ARG;
        } else {
            unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) }
        };
        match r.inner.lock().verify(&core_sig, payload) {
            Ok(()) => NET_DECK_OK,
            Err(e) => {
                set_verify_error(e);
                NET_DECK_ERR_CALL_FAILED
            }
        }
    })
}

/// Verify every signature in the bundle over `payload` and
/// confirm at least `threshold` distinct operator ids signed it.
/// On failure sets the appropriate kind and returns
/// `NET_DECK_ERR_CALL_FAILED`.
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_verify_bundle(
    registry: *const NetDeckOperatorRegistry,
    sigs_ptr: *const NetDeckOperatorSignature,
    sigs_count: usize,
    payload_ptr: *const u8,
    payload_len: usize,
    threshold: usize,
) -> c_int {
    ffi_guard!(NET_DECK_ERR_CALL_FAILED, {
        let Some(r) = (unsafe { registry.as_ref() }) else {
            set_last_error("invalid_argument", "registry pointer is NULL");
            return NET_DECK_ERR_INVALID_ARG;
        };
        let owned = match unsafe { signatures_from_c(sigs_ptr, sigs_count) } {
            Ok(o) => o,
            Err(kind) => {
                set_last_error(kind, "signature buffer is NULL or zero-length");
                return NET_DECK_ERR_INVALID_ARG;
            }
        };
        let core_sigs: Vec<CoreOperatorSignature> = owned
            .into_iter()
            .map(|s| CoreOperatorSignature {
                operator_id: s.operator_id,
                signature: s.signature,
            })
            .collect();
        let payload: &[u8] = if payload_len == 0 {
            &[]
        } else if payload_ptr.is_null() {
            set_last_error(
                "invalid_argument",
                "payload_ptr is NULL but payload_len > 0",
            );
            return NET_DECK_ERR_INVALID_ARG;
        } else {
            unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) }
        };
        match r.inner.lock().verify_bundle(&core_sigs, payload, threshold) {
            Ok(()) => NET_DECK_OK,
            Err(e) => {
                set_verify_error(e);
                NET_DECK_ERR_CALL_FAILED
            }
        }
    })
}

/// Free an operator registry. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_operator_registry_free(registry: *mut NetDeckOperatorRegistry) {
    if registry.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(registry);
    }
}

// =========================================================================
// AdminVerifier opaque handle
//
// Wraps a snapshotted registry with the cluster's policy knobs.
// Useful for offline unit testing of operator policy decisions.
// =========================================================================

pub struct NetDeckAdminVerifier {
    inner: CoreAdminVerifier,
}

/// Build a verifier with `threshold` minimum signatures and the
/// substrate defaults (300s freshness, 30s future-skew, 300s ICE
/// cooldown). `threshold = 0` is clamped to `1`.
#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_new(
    registry: *const NetDeckOperatorRegistry,
    threshold: usize,
) -> *mut NetDeckAdminVerifier {
    let Some(r) = (unsafe { registry.as_ref() }) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(NetDeckAdminVerifier {
        inner: CoreAdminVerifier::new(r.snapshot_arc(), threshold),
    }))
}

/// Build with explicit freshness + future-skew windows and the
/// default ICE cooldown.
#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_with_freshness(
    registry: *const NetDeckOperatorRegistry,
    threshold: usize,
    freshness_window_ms: u64,
    future_skew_ms: u64,
) -> *mut NetDeckAdminVerifier {
    let Some(r) = (unsafe { registry.as_ref() }) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(NetDeckAdminVerifier {
        inner: CoreAdminVerifier::with_freshness(
            r.snapshot_arc(),
            threshold,
            Duration::from_millis(freshness_window_ms),
            Duration::from_millis(future_skew_ms),
        ),
    }))
}

/// Build with every policy knob explicit.
#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_with_full_policy(
    registry: *const NetDeckOperatorRegistry,
    threshold: usize,
    freshness_window_ms: u64,
    future_skew_ms: u64,
    ice_cooldown_ms: u64,
) -> *mut NetDeckAdminVerifier {
    let Some(r) = (unsafe { registry.as_ref() }) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(NetDeckAdminVerifier {
        inner: CoreAdminVerifier::with_full_policy(
            r.snapshot_arc(),
            threshold,
            Duration::from_millis(freshness_window_ms),
            Duration::from_millis(future_skew_ms),
            Duration::from_millis(ice_cooldown_ms),
        ),
    }))
}

#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_threshold(
    verifier: *const NetDeckAdminVerifier,
) -> usize {
    match unsafe { verifier.as_ref() } {
        Some(v) => v.inner.threshold(),
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_freshness_window_ms(
    verifier: *const NetDeckAdminVerifier,
) -> u64 {
    match unsafe { verifier.as_ref() } {
        Some(v) => v.inner.freshness_window().as_millis() as u64,
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_future_skew_ms(
    verifier: *const NetDeckAdminVerifier,
) -> u64 {
    match unsafe { verifier.as_ref() } {
        Some(v) => v.inner.future_skew().as_millis() as u64,
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_ice_cooldown_ms(
    verifier: *const NetDeckAdminVerifier,
) -> u64 {
    match unsafe { verifier.as_ref() } {
        Some(v) => v.inner.ice_cooldown().as_millis() as u64,
        None => 0,
    }
}

/// Free an admin verifier. Idempotent on NULL.
#[no_mangle]
pub extern "C" fn net_deck_admin_verifier_free(verifier: *mut NetDeckAdminVerifier) {
    if verifier.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(verifier);
    }
}

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
        let status = net_deck_status_summary_stream_next(stream, 500, &mut summary, &mut has_item);
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

    // =========================================================================
    // Slice 2 tests
    // =========================================================================

    #[test]
    fn audit_query_lifecycle_collect_on_fresh_runtime() {
        let client = make_client(0x20);
        let mut q: *mut NetDeckAuditQuery = ptr::null_mut();
        assert_eq!(net_deck_audit_query_new(&mut q), NET_DECK_OK);
        assert!(!q.is_null());
        net_deck_audit_query_recent(q, 100);

        let mut records: *mut *mut c_char = ptr::null_mut();
        let mut count: usize = 0;
        let status = net_deck_audit_query_collect(q, client, &mut records, &mut count);
        assert_eq!(status, NET_DECK_OK);
        // Fresh runtime; may be 0 or more records (depending on
        // whether prior tests left state — admin ring is process-
        // scoped though we use a fresh `NetDeckClient` per test).
        net_deck_audit_records_free(records, count);
        net_deck_audit_query_free(q);
        net_deck_client_free(client);
    }

    #[test]
    fn audit_query_accepts_every_filter_method() {
        let client = make_client(0x21);
        let mut q: *mut NetDeckAuditQuery = ptr::null_mut();
        assert_eq!(net_deck_audit_query_new(&mut q), NET_DECK_OK);
        assert_eq!(net_deck_audit_query_recent(q, 10), NET_DECK_OK);
        assert_eq!(net_deck_audit_query_by_operator(q, 0x123), NET_DECK_OK);
        assert_eq!(
            net_deck_audit_query_between(q, 0, 2_000_000_000_000),
            NET_DECK_OK
        );
        assert_eq!(net_deck_audit_query_force_only(q), NET_DECK_OK);
        assert_eq!(net_deck_audit_query_since(q, 0), NET_DECK_OK);

        let mut records: *mut *mut c_char = ptr::null_mut();
        let mut count: usize = 0;
        let status = net_deck_audit_query_collect(q, client, &mut records, &mut count);
        assert_eq!(status, NET_DECK_OK);
        net_deck_audit_records_free(records, count);
        net_deck_audit_query_free(q);
        net_deck_client_free(client);
    }

    #[test]
    fn audit_ring_eventually_contains_record_after_admin_commit() {
        // Fast tick so the audit fold runs promptly.
        let seed = [0x22u8; 32];
        let mut client: *mut NetDeckClient = ptr::null_mut();
        let status = net_deck_client_new(0, 20, 0, 0, 0, 0, seed.as_ptr(), &mut client);
        assert_eq!(status, NET_DECK_OK);

        let mut commit = NetDeckChainCommit::default();
        assert_eq!(
            net_deck_admin_cordon(client, 0xCAFE, &mut commit),
            NET_DECK_OK
        );

        // Poll up to 2s.
        let mut q: *mut NetDeckAuditQuery = ptr::null_mut();
        net_deck_audit_query_new(&mut q);
        net_deck_audit_query_recent(q, 100);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            let mut records: *mut *mut c_char = ptr::null_mut();
            let mut count: usize = 0;
            let status = net_deck_audit_query_collect(q, client, &mut records, &mut count);
            assert_eq!(status, NET_DECK_OK);
            if count > 0 {
                // Parse the first record + assert key presence.
                let json_ptr = unsafe { *records };
                let s = unsafe { CStr::from_ptr(json_ptr).to_string_lossy().into_owned() };
                let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid json");
                let obj = parsed.as_object().expect("audit record is object");
                for key in ["seq", "committed_at_ms", "event", "operator_ids", "outcome"] {
                    assert!(obj.contains_key(key), "missing key {key}: {s}");
                }
                net_deck_audit_records_free(records, count);
                found = true;
                break;
            }
            net_deck_audit_records_free(records, count);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(found, "expected audit ring to populate within 2s");

        net_deck_audit_query_free(q);
        net_deck_client_free(client);
    }

    #[test]
    fn subscribe_logs_with_filter_and_close() {
        let client = make_client(0x23);
        let filter = NetDeckLogFilter {
            min_level_present: 1,
            min_level: NET_DECK_LOG_WARN,
            ..Default::default()
        };
        let mut stream: *mut NetDeckLogStream = ptr::null_mut();
        let status = net_deck_subscribe_logs(client, &filter, &mut stream);
        assert_eq!(status, NET_DECK_OK);
        assert!(!stream.is_null());
        net_deck_log_stream_free(stream);
        net_deck_client_free(client);
    }

    #[test]
    fn subscribe_logs_null_filter_matches_everything() {
        let client = make_client(0x24);
        let mut stream: *mut NetDeckLogStream = ptr::null_mut();
        let status = net_deck_subscribe_logs(client, ptr::null(), &mut stream);
        assert_eq!(status, NET_DECK_OK);
        net_deck_log_stream_free(stream);
        net_deck_client_free(client);
    }

    #[test]
    fn subscribe_logs_rejects_invalid_min_level() {
        let client = make_client(0x25);
        let filter = NetDeckLogFilter {
            min_level_present: 1,
            min_level: 99, // out of range
            ..Default::default()
        };
        let mut stream: *mut NetDeckLogStream = ptr::null_mut();
        let status = net_deck_subscribe_logs(client, &filter, &mut stream);
        assert_eq!(status, NET_DECK_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_log_level");
        net_deck_clear_last_error();
        net_deck_client_free(client);
    }

    #[test]
    fn log_stream_next_with_timeout_returns_no_item() {
        let client = make_client(0x26);
        let mut stream: *mut NetDeckLogStream = ptr::null_mut();
        net_deck_subscribe_logs(client, ptr::null(), &mut stream);
        let mut record = NetDeckLogRecord::default();
        let mut has_item: c_int = 0;
        let status = net_deck_log_stream_next(stream, 50, &mut record, &mut has_item);
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(has_item, 0, "expected no log record in 50ms");
        // Defensive: drop in case some platform writes startup
        // logs into the ring fast enough to land within 50ms.
        net_deck_log_record_drop(&mut record);
        net_deck_log_stream_free(stream);
        net_deck_client_free(client);
    }

    #[test]
    fn failure_stream_subscribe_next_close() {
        let client = make_client(0x27);
        let mut stream: *mut NetDeckFailureStream = ptr::null_mut();
        let status = net_deck_subscribe_failures(client, 0, &mut stream);
        assert_eq!(status, NET_DECK_OK);
        let mut record = NetDeckFailureRecord::default();
        let mut has_item: c_int = 0;
        let status = net_deck_failure_stream_next(stream, 50, &mut record, &mut has_item);
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(has_item, 0);
        net_deck_failure_record_drop(&mut record);
        net_deck_failure_stream_free(stream);
        net_deck_client_free(client);
    }

    #[test]
    fn audit_stream_subscribe_close_protocol() {
        let client = make_client(0x28);
        let mut q: *mut NetDeckAuditQuery = ptr::null_mut();
        net_deck_audit_query_new(&mut q);
        net_deck_audit_query_recent(q, 10);

        let mut stream: *mut NetDeckAuditStream = ptr::null_mut();
        let status = net_deck_audit_query_stream(q, client, &mut stream);
        assert_eq!(status, NET_DECK_OK);
        // Timeout on a quiet ring.
        let mut json_ptr: *mut c_char = ptr::null_mut();
        let status = net_deck_audit_stream_next(stream, 50, &mut json_ptr);
        assert_eq!(status, NET_DECK_OK);
        assert!(json_ptr.is_null(), "expected timeout — no audit item");
        net_deck_audit_stream_free(stream);
        net_deck_audit_query_free(q);
        net_deck_client_free(client);
    }

    // Finding #3: the EOF-vs-timeout decision. `next_with_timeout` is the
    // single source of truth all `_next` fns share; the `_next` match arms
    // map `(None, true) → OK/timeout` and `(None, false) → END_OF_STREAM`.
    #[test]
    fn next_with_timeout_distinguishes_eof_from_timeout() {
        // A genuinely-ended stream yields `Ok(None)` → (None, timed_out=false)
        // → the END_OF_STREAM arm. With the old `unwrap_or_default()` this was
        // indistinguishable from a timeout for any non-zero `timeout_ms`.
        let mut ended = futures::stream::empty::<u32>();
        let (item, timed_out) =
            runtime().block_on(next_with_timeout(100, futures::StreamExt::next(&mut ended)));
        assert!(item.is_none());
        assert!(!timed_out, "ended stream must NOT be flagged as a timeout");

        // A stream that never yields, with a short timeout, elapses →
        // (None, timed_out=true) → the OK/timeout arm.
        let mut never = futures::stream::pending::<u32>();
        let (item, timed_out) =
            runtime().block_on(next_with_timeout(10, futures::StreamExt::next(&mut never)));
        assert!(item.is_none());
        assert!(
            timed_out,
            "pending stream past the deadline must be a timeout"
        );

        // An available item is returned with `timed_out == false`.
        let mut ready = futures::stream::iter(vec![7u32]);
        let (item, timed_out) =
            runtime().block_on(next_with_timeout(100, futures::StreamExt::next(&mut ready)));
        assert_eq!(item, Some(7));
        assert!(!timed_out);

        // `timeout_ms == 0` (unbounded) never flags a timeout; an ended
        // stream still reports EOF.
        let mut ended0 = futures::stream::empty::<u32>();
        let (item, timed_out) =
            runtime().block_on(next_with_timeout(0, futures::StreamExt::next(&mut ended0)));
        assert!(item.is_none());
        assert!(!timed_out);
    }

    // Finding #31: `slice::from_raw_parts` requires `len <= isize::MAX`; an
    // oversized `payload_len` from cgo must be rejected before the deref
    // rather than triggering UB.
    #[test]
    fn sign_payload_rejects_oversized_len() {
        let identity = net_deck_operator_identity_generate();
        assert!(!identity.is_null());

        // A non-null pointer with `len > isize::MAX` must be rejected with
        // INVALID_ARG and must NOT dereference the pointer.
        let dangling: u8 = 0;
        let mut op_id: u64 = 0;
        let mut sig = [0u8; 64];
        let status = net_deck_operator_identity_sign_payload(
            identity,
            &dangling as *const u8,
            (isize::MAX as usize) + 1,
            &mut op_id,
            sig.as_mut_ptr(),
        );
        assert_eq!(status, NET_DECK_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");
        net_deck_clear_last_error();
        net_deck_operator_identity_free(identity);
    }

    // =========================================================================
    // Slice 3 — ICE tests
    // =========================================================================

    #[test]
    fn ice_freeze_cluster_factory_returns_proposal_with_issued_at_ms() {
        let client = make_client(0x30);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        let status = net_deck_ice_freeze_cluster(client, 60_000, &mut proposal);
        assert_eq!(status, NET_DECK_OK);
        assert!(!proposal.is_null());
        let issued_at_ms = net_deck_ice_proposal_issued_at_ms(proposal);
        assert!(issued_at_ms > 0);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    #[test]
    fn ice_all_factories_succeed() {
        let client = make_client(0x31);
        let mut p: *mut NetDeckIceProposal = ptr::null_mut();

        assert_eq!(
            net_deck_ice_freeze_cluster(client, 60_000, &mut p),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(
            net_deck_ice_flush_avoid_lists(client, NET_DECK_AVOID_SCOPE_GLOBAL, 0, 0, &mut p),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(
            net_deck_ice_flush_avoid_lists(client, NET_DECK_AVOID_SCOPE_LOCAL, 0xCAFE, 0, &mut p),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(
            net_deck_ice_flush_avoid_lists(client, NET_DECK_AVOID_SCOPE_ON_PEER, 0, 0xBEEF, &mut p),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(
            net_deck_ice_force_evict_replica(client, 1, 2, &mut p),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        let name = "echo";
        assert_eq!(
            net_deck_ice_force_restart_daemon(
                client,
                3,
                name.as_ptr() as *const c_char,
                name.len(),
                &mut p
            ),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(
            net_deck_ice_force_cutover(client, 4, 5, &mut p),
            NET_DECK_OK
        );
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(net_deck_ice_kill_migration(client, 6, &mut p), NET_DECK_OK);
        net_deck_ice_proposal_free(p);

        p = ptr::null_mut();
        assert_eq!(net_deck_ice_thaw_cluster(client, &mut p), NET_DECK_OK);
        net_deck_ice_proposal_free(p);

        net_deck_client_free(client);
    }

    #[test]
    fn ice_flush_avoid_lists_rejects_invalid_scope_kind() {
        let client = make_client(0x32);
        let mut p: *mut NetDeckIceProposal = ptr::null_mut();
        let status = net_deck_ice_flush_avoid_lists(client, 99, 0, 0, &mut p);
        assert_eq!(status, NET_DECK_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_avoid_scope");
        net_deck_clear_last_error();
        net_deck_client_free(client);
    }

    #[test]
    fn ice_simulate_advances_to_simulated_with_blast_radius_and_hash() {
        let client = make_client(0x33);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        assert_eq!(
            net_deck_ice_freeze_cluster(client, 60_000, &mut proposal),
            NET_DECK_OK
        );

        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        let status = net_deck_ice_proposal_simulate(proposal, client, &mut simulated);
        assert_eq!(status, NET_DECK_OK);
        assert!(!simulated.is_null());

        let issued_at_ms = net_deck_simulated_issued_at_ms(simulated);
        assert!(issued_at_ms > 0);

        let json_ptr = net_deck_simulated_blast_radius(simulated);
        assert!(!json_ptr.is_null());
        let json = unsafe { CStr::from_ptr(json_ptr).to_string_lossy().into_owned() };
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert!(parsed.is_object());
        net_deck_free_string(json_ptr);

        let mut hash = [0u8; 32];
        let status = net_deck_simulated_blast_hash(simulated, hash.as_mut_ptr());
        assert_eq!(status, NET_DECK_OK);
        // Blake3 hashes are not all zero unless the input was
        // a specific bytestring; even an empty blast radius
        // produces a non-trivial digest.
        assert_ne!(hash, [0u8; 32]);

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    #[test]
    fn ice_double_simulate_returns_already_simulated() {
        let client = make_client(0x34);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        assert_eq!(
            net_deck_ice_freeze_cluster(client, 60_000, &mut proposal),
            NET_DECK_OK
        );

        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        assert_eq!(
            net_deck_ice_proposal_simulate(proposal, client, &mut simulated),
            NET_DECK_OK
        );
        net_deck_simulated_free(simulated);

        // Second simulate on the same (consumed) proposal.
        simulated = ptr::null_mut();
        let status = net_deck_ice_proposal_simulate(proposal, client, &mut simulated);
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "already_simulated");
        net_deck_clear_last_error();

        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    /// Regression: `net_deck_ice_proposal_issued_at_ms` must keep
    /// returning the pinned stamp after the proposal is consumed
    /// by `_simulate`. Pre-fix the consumed-state marker was
    /// `issued_at_ms = u64::MAX`, so the accessor leaked the
    /// sentinel back to the caller — a consumer who pinned the
    /// stamp before simulating then re-read after would see a
    /// wildly different number with no error indication.
    #[test]
    fn ice_issued_at_ms_survives_consumption_by_simulate() {
        let client = make_client(0x4F);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        assert_eq!(
            net_deck_ice_freeze_cluster(client, 60_000, &mut proposal),
            NET_DECK_OK
        );

        let before = net_deck_ice_proposal_issued_at_ms(proposal);
        assert!(before > 0, "factory should pin a non-zero issued_at_ms");
        assert_ne!(
            before,
            u64::MAX,
            "factory should not return the consumed sentinel value"
        );

        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        assert_eq!(
            net_deck_ice_proposal_simulate(proposal, client, &mut simulated),
            NET_DECK_OK
        );

        let after = net_deck_ice_proposal_issued_at_ms(proposal);
        assert_eq!(
            after, before,
            "issued_at_ms must survive _simulate consumption (got {after}, was {before})"
        );

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    /// Regression: if `build_core_proposal` rejects the variant
    /// (a substrate enum has grown a new variant the binding
    /// doesn't recognize), the husk must stay retry-able rather
    /// than flip to `consumed`. Pre-fix the order was
    /// `consumed = true; build_core_proposal(...)?` so an
    /// unknown variant left an orphan husk and the next call
    /// surfaced `already_simulated` instead of the real cause.
    /// Regression: audit-query setters used to return
    /// `NET_DECK_ERR_NULL` on a NULL query pointer without
    /// touching the last-error envelope. A consumer that read
    /// `net_deck_last_error_*` after the NULL-arm return would
    /// see a stale unrelated kind/message from a prior call.
    #[test]
    fn audit_query_setters_set_last_error_on_null_query() {
        net_deck_clear_last_error();
        // Seed a stale envelope so we can prove the setters
        // overwrite it instead of leaving it leaking through.
        set_last_error("stale", "from a prior unrelated call");
        assert_eq!(
            net_deck_audit_query_recent(ptr::null_mut(), 10),
            NET_DECK_ERR_NULL
        );
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");

        set_last_error("stale", "from a prior unrelated call");
        assert_eq!(
            net_deck_audit_query_by_operator(ptr::null_mut(), 1),
            NET_DECK_ERR_NULL
        );
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");

        set_last_error("stale", "from a prior unrelated call");
        assert_eq!(
            net_deck_audit_query_between(ptr::null_mut(), 0, 0),
            NET_DECK_ERR_NULL
        );
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");

        set_last_error("stale", "from a prior unrelated call");
        assert_eq!(
            net_deck_audit_query_force_only(ptr::null_mut()),
            NET_DECK_ERR_NULL
        );
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");

        set_last_error("stale", "from a prior unrelated call");
        assert_eq!(
            net_deck_audit_query_since(ptr::null_mut(), 0),
            NET_DECK_ERR_NULL
        );
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_argument");

        net_deck_clear_last_error();
    }

    #[test]
    fn ice_simulate_unknown_variant_leaves_husk_retryable() {
        let client = make_client(0x55);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        assert_eq!(
            net_deck_ice_freeze_cluster(client, 60_000, &mut proposal),
            NET_DECK_OK
        );

        FAIL_NEXT_BUILD_CORE_PROPOSAL.with(|c| c.set(true));
        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        let status = net_deck_ice_proposal_simulate(proposal, client, &mut simulated);
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "unknown_action");
        assert!(simulated.is_null());
        net_deck_clear_last_error();

        // Husk should still be usable — retry without fault
        // injection must succeed, proving consumed never flipped.
        assert_eq!(
            net_deck_ice_proposal_simulate(proposal, client, &mut simulated),
            NET_DECK_OK
        );
        assert!(!simulated.is_null());

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    /// Same invariant on the commit path: an unknown variant must
    /// not leave the simulated husk in the `committed` state.
    /// The retry assertion is intentionally limited to checking
    /// the kind != "already_committed" via an empty-signatures
    /// retry — driving a real signed commit twice would invoke
    /// the substrate path with no policy setup, which is out of
    /// scope for this regression.
    #[test]
    fn ice_commit_unknown_variant_leaves_husk_retryable() {
        let client = make_client(0x56);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        net_deck_ice_freeze_cluster(client, 60_000, &mut proposal);
        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        net_deck_ice_proposal_simulate(proposal, client, &mut simulated);

        let sig_bytes = [0x00u8; 64];
        let sig = NetDeckOperatorSignature {
            operator_id: 1,
            signature_ptr: sig_bytes.as_ptr(),
            signature_len: 64,
        };

        FAIL_NEXT_BUILD_CORE_PROPOSAL.with(|c| c.set(true));
        let mut commit = NetDeckChainCommit::default();
        let status = net_deck_simulated_commit(simulated, client, &sig, 1, &mut commit);
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "unknown_action");
        net_deck_clear_last_error();

        // Retry with empty signatures — exercises the husk-state
        // check before the substrate path. Pre-fix this would
        // return "already_committed" (the husk had been
        // mistakenly flipped). Post-fix it returns
        // "insufficient_signatures" from the empty-sigs guard
        // because the husk is still uncommitted.
        let mut commit2 = NetDeckChainCommit::default();
        let status = net_deck_simulated_commit(simulated, client, ptr::null(), 0, &mut commit2);
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(
            kind, "insufficient_signatures",
            "husk should still be uncommitted after unknown_action error, got kind={kind}"
        );
        net_deck_clear_last_error();

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    #[test]
    fn ice_commit_with_empty_signatures_returns_insufficient() {
        let client = make_client(0x35);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        net_deck_ice_freeze_cluster(client, 60_000, &mut proposal);
        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        net_deck_ice_proposal_simulate(proposal, client, &mut simulated);

        let mut commit = NetDeckChainCommit::default();
        let status = net_deck_simulated_commit(simulated, client, ptr::null(), 0, &mut commit);
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "insufficient_signatures");
        net_deck_clear_last_error();

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    #[test]
    fn ice_commit_with_signature_succeeds_and_consumes_proposal() {
        let client = make_client(0x36);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        net_deck_ice_freeze_cluster(client, 60_000, &mut proposal);
        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        net_deck_ice_proposal_simulate(proposal, client, &mut simulated);

        let sig_bytes = [0x00u8; 64];
        let sig = NetDeckOperatorSignature {
            operator_id: 1,
            signature_ptr: sig_bytes.as_ptr(),
            signature_len: 64,
        };
        let mut commit = NetDeckChainCommit::default();
        let status = net_deck_simulated_commit(simulated, client, &sig, 1, &mut commit);
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(commit.event_kind, NET_DECK_EVENT_KIND_UNKNOWN);
        // FreezeCluster isn't in the admin-event kind discriminator
        // (it lives in ICE land); the deck SDK reports
        // `event_kind = "freeze_cluster"` which maps to UNKNOWN in
        // the C constant set.
        assert!(commit.commit_id > 0);

        // Second commit — proposal consumed.
        let mut commit2 = NetDeckChainCommit::default();
        let status = net_deck_simulated_commit(simulated, client, &sig, 1, &mut commit2);
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "already_committed");
        net_deck_clear_last_error();

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    #[test]
    fn ice_commit_rejects_malformed_signature_buffer() {
        let client = make_client(0x37);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        net_deck_ice_freeze_cluster(client, 60_000, &mut proposal);
        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        net_deck_ice_proposal_simulate(proposal, client, &mut simulated);

        // signature_len = 0 → invalid.
        let sig = NetDeckOperatorSignature {
            operator_id: 1,
            signature_ptr: ptr::null(),
            signature_len: 0,
        };
        let mut commit = NetDeckChainCommit::default();
        let status = net_deck_simulated_commit(simulated, client, &sig, 1, &mut commit);
        assert_eq!(status, NET_DECK_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_signature");
        net_deck_clear_last_error();

        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }

    // ---- Operator-policy verifier surface ----

    #[test]
    fn operator_identity_lifecycle() {
        let seed = [0x77u8; 32];
        let mut identity: *mut NetDeckOperatorIdentity = ptr::null_mut();
        let status = net_deck_operator_identity_from_seed(seed.as_ptr(), &mut identity);
        assert_eq!(status, NET_DECK_OK);
        assert!(!identity.is_null());

        let op_id = net_deck_operator_identity_operator_id(identity);
        let expected = EntityKeypair::from_bytes(seed).origin_hash();
        assert_eq!(op_id, expected);

        let mut pubkey = [0u8; 32];
        let status = net_deck_operator_identity_public_key(identity, pubkey.as_mut_ptr());
        assert_eq!(status, NET_DECK_OK);
        let expected_kp = EntityKeypair::from_bytes(seed);
        let expected_pk = expected_kp.entity_id().as_bytes();
        assert_eq!(&pubkey, expected_pk);

        net_deck_operator_identity_free(identity);
    }

    #[test]
    fn operator_registry_insert_and_contains() {
        let registry = net_deck_operator_registry_new();
        assert!(!registry.is_null());
        assert_eq!(net_deck_operator_registry_len(registry), 0);

        let pk = [0xAAu8; 32];
        let status = net_deck_operator_registry_insert(registry, 42, pk.as_ptr());
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(net_deck_operator_registry_len(registry), 1);
        assert_eq!(net_deck_operator_registry_contains(registry, 42), 1);
        assert_eq!(net_deck_operator_registry_contains(registry, 99), 0);

        net_deck_operator_registry_free(registry);
    }

    #[test]
    fn operator_registry_rejects_null_pubkey() {
        let registry = net_deck_operator_registry_new();
        let status = net_deck_operator_registry_insert(registry, 1, ptr::null());
        assert_eq!(status, NET_DECK_ERR_INVALID_ARG);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "invalid_public_key");
        net_deck_clear_last_error();
        net_deck_operator_registry_free(registry);
    }

    #[test]
    fn sign_payload_then_registry_verify_roundtrip() {
        let identity = net_deck_operator_identity_generate();
        let registry = net_deck_operator_registry_new();
        net_deck_operator_registry_register(registry, identity);

        let payload = b"hello-canary";
        let mut op_id: u64 = 0;
        let mut sig_bytes = [0u8; 64];
        let status = net_deck_operator_identity_sign_payload(
            identity,
            payload.as_ptr(),
            payload.len(),
            &mut op_id,
            sig_bytes.as_mut_ptr(),
        );
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(op_id, net_deck_operator_identity_operator_id(identity));

        let sig = NetDeckOperatorSignature {
            operator_id: op_id,
            signature_ptr: sig_bytes.as_ptr(),
            signature_len: 64,
        };
        let status =
            net_deck_operator_registry_verify(registry, &sig, payload.as_ptr(), payload.len());
        assert_eq!(status, NET_DECK_OK);

        // Tampered payload → signature_invalid.
        let bad = b"hello-canary!";
        let status = net_deck_operator_registry_verify(registry, &sig, bad.as_ptr(), bad.len());
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "signature_invalid");
        net_deck_clear_last_error();

        net_deck_operator_registry_free(registry);
        net_deck_operator_identity_free(identity);
    }

    #[test]
    fn registry_verify_rejects_unknown_operator() {
        let identity = net_deck_operator_identity_generate();
        let registry = net_deck_operator_registry_new();
        let payload = b"hello";
        let mut op_id: u64 = 0;
        let mut sig_bytes = [0u8; 64];
        net_deck_operator_identity_sign_payload(
            identity,
            payload.as_ptr(),
            payload.len(),
            &mut op_id,
            sig_bytes.as_mut_ptr(),
        );
        let sig = NetDeckOperatorSignature {
            operator_id: op_id,
            signature_ptr: sig_bytes.as_ptr(),
            signature_len: 64,
        };
        let status =
            net_deck_operator_registry_verify(registry, &sig, payload.as_ptr(), payload.len());
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "not_authorized");
        net_deck_clear_last_error();
        net_deck_operator_registry_free(registry);
        net_deck_operator_identity_free(identity);
    }

    #[test]
    fn verify_bundle_distinct_operator_dedup() {
        let a = net_deck_operator_identity_generate();
        let b = net_deck_operator_identity_generate();
        let registry = net_deck_operator_registry_new();
        net_deck_operator_registry_register(registry, a);
        net_deck_operator_registry_register(registry, b);

        let payload = b"bundle-canary";
        let mut sig_a_bytes = [0u8; 64];
        let mut sig_b_bytes = [0u8; 64];
        let mut a_id: u64 = 0;
        let mut b_id: u64 = 0;
        net_deck_operator_identity_sign_payload(
            a,
            payload.as_ptr(),
            payload.len(),
            &mut a_id,
            sig_a_bytes.as_mut_ptr(),
        );
        net_deck_operator_identity_sign_payload(
            b,
            payload.as_ptr(),
            payload.len(),
            &mut b_id,
            sig_b_bytes.as_mut_ptr(),
        );

        let sig_a = NetDeckOperatorSignature {
            operator_id: a_id,
            signature_ptr: sig_a_bytes.as_ptr(),
            signature_len: 64,
        };
        let sig_b = NetDeckOperatorSignature {
            operator_id: b_id,
            signature_ptr: sig_b_bytes.as_ptr(),
            signature_len: 64,
        };

        // Distinct operators → threshold 2 satisfied.
        let bundle = [sig_a, sig_b];
        let status = net_deck_operator_registry_verify_bundle(
            registry,
            bundle.as_ptr(),
            2,
            payload.as_ptr(),
            payload.len(),
            2,
        );
        assert_eq!(status, NET_DECK_OK);

        // Same operator twice → insufficient.
        let dup = [
            NetDeckOperatorSignature {
                operator_id: a_id,
                signature_ptr: sig_a_bytes.as_ptr(),
                signature_len: 64,
            },
            NetDeckOperatorSignature {
                operator_id: a_id,
                signature_ptr: sig_a_bytes.as_ptr(),
                signature_len: 64,
            },
        ];
        let status = net_deck_operator_registry_verify_bundle(
            registry,
            dup.as_ptr(),
            2,
            payload.as_ptr(),
            payload.len(),
            2,
        );
        assert_eq!(status, NET_DECK_ERR_CALL_FAILED);
        let kind = unsafe { CStr::from_ptr(net_deck_last_error_kind()).to_string_lossy() };
        assert_eq!(kind, "insufficient_signatures");
        net_deck_clear_last_error();

        net_deck_operator_registry_free(registry);
        net_deck_operator_identity_free(a);
        net_deck_operator_identity_free(b);
    }

    #[test]
    fn admin_verifier_policy_knobs() {
        let registry = net_deck_operator_registry_new();
        let v = net_deck_admin_verifier_new(registry, 3);
        assert!(!v.is_null());
        assert_eq!(net_deck_admin_verifier_threshold(v), 3);
        assert_eq!(net_deck_admin_verifier_freshness_window_ms(v), 300_000);
        assert_eq!(net_deck_admin_verifier_future_skew_ms(v), 30_000);
        assert_eq!(net_deck_admin_verifier_ice_cooldown_ms(v), 300_000);
        net_deck_admin_verifier_free(v);

        let v2 = net_deck_admin_verifier_with_full_policy(registry, 0, 1_000, 500, 250);
        assert_eq!(net_deck_admin_verifier_threshold(v2), 1); // clamp 0 → 1
        assert_eq!(net_deck_admin_verifier_freshness_window_ms(v2), 1_000);
        assert_eq!(net_deck_admin_verifier_future_skew_ms(v2), 500);
        assert_eq!(net_deck_admin_verifier_ice_cooldown_ms(v2), 250);
        net_deck_admin_verifier_free(v2);

        net_deck_operator_registry_free(registry);
    }

    #[test]
    fn signing_payload_matches_sign_proposal_payload() {
        let client = make_client(0x91);
        let mut proposal: *mut NetDeckIceProposal = ptr::null_mut();
        net_deck_ice_freeze_cluster(client, 60_000, &mut proposal);
        let mut simulated: *mut NetDeckSimulatedIceProposal = ptr::null_mut();
        net_deck_ice_proposal_simulate(proposal, client, &mut simulated);

        let mut payload_ptr: *mut u8 = ptr::null_mut();
        let mut payload_len: usize = 0;
        let status =
            net_deck_simulated_signing_payload(simulated, &mut payload_ptr, &mut payload_len);
        assert_eq!(status, NET_DECK_OK);
        assert!(!payload_ptr.is_null());
        assert!(payload_len > 0);

        // ICE_SIGNING_DOMAIN prefix.
        let payload = unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) };
        assert!(payload.starts_with(b"net.meshos.ice.v1\0"));

        let identity = net_deck_operator_identity_generate();
        let mut prop_op_id: u64 = 0;
        let mut prop_sig = [0u8; 64];
        let status = net_deck_operator_identity_sign_proposal(
            identity,
            simulated,
            &mut prop_op_id,
            prop_sig.as_mut_ptr(),
        );
        assert_eq!(status, NET_DECK_OK);

        let mut payload_op_id: u64 = 0;
        let mut payload_sig = [0u8; 64];
        let status = net_deck_operator_identity_sign_payload(
            identity,
            payload.as_ptr(),
            payload.len(),
            &mut payload_op_id,
            payload_sig.as_mut_ptr(),
        );
        assert_eq!(status, NET_DECK_OK);
        assert_eq!(prop_op_id, payload_op_id);
        assert_eq!(prop_sig, payload_sig);

        net_deck_signing_payload_free(payload_ptr, payload_len);
        net_deck_operator_identity_free(identity);
        net_deck_simulated_free(simulated);
        net_deck_ice_proposal_free(proposal);
        net_deck_client_free(client);
    }
}
