//! C FFI bindings for the aggregator-registry RPC client +
//! fold-query client + channel-visibility setter
//! (`SDK_AGGREGATOR_SUBNET_PLAN.md` stages 5 + 4-fold-query).
//!
//! Boundary conventions mirror `ffi::mesh`: opaque handles
//! freed via dedicated `_free`, scalar ids as `u64`, JSON
//! strings via `CString::into_raw` freed by the caller via
//! `net_free_string`. Caller safety contract is identical to
//! `ffi::mesh` / `ffi::cortex`; `clippy::missing_safety_doc`
//! suppressed at the module level for the same rationale.
#![allow(clippy::missing_safety_doc)]
#![expect(
    clippy::undocumented_unsafe_blocks,
    reason = "module-wide FFI safety contract documented in ffi::mod.rs preamble"
)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::time::Duration;

use parking_lot::{Mutex as ParkingMutex, RwLock as ParkingRwLock};

use crate::adapter::net::behavior::aggregator::{
    FoldQueryClient, FoldQueryClientError, FoldQueryError, RegistryClient, RegistryClientError,
    RegistryGroupSummary, RegistryRpcError, SummaryAnnouncement, DEFAULT_QUERY_DEADLINE,
    DEFAULT_REGISTRY_DEADLINE,
};
use crate::adapter::net::{ChannelConfig, ChannelId, ChannelName, Visibility};

use super::mesh::MeshNodeHandle;

// ─── Error-kind discriminants (locked across SDKs) ───

/// Server handler rejected: no summarizer registered under the
/// requested fold kind. Only emitted by
/// `net_fold_query_client_*` ops.
pub const NET_REGISTRY_ERR_UNKNOWN_KIND: i32 = 7;

/// `net_registry_client_*` op succeeded.
pub const NET_REGISTRY_OK: i32 = 0;
/// Transport-level failure (no route, timeout, server returned
/// a non-Ok status before invoking the handler).
pub const NET_REGISTRY_ERR_TRANSPORT: i32 = 1;
/// Request serialization or response deserialization failed.
pub const NET_REGISTRY_ERR_CODEC: i32 = 2;
/// Server handler rejected: no template by that name.
pub const NET_REGISTRY_ERR_UNKNOWN_TEMPLATE: i32 = 3;
/// Server handler rejected: a group by that name is already
/// registered.
pub const NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME: i32 = 4;
/// Server handler rejected for a daemon-defined reason
/// (config validation, replica spawn failed, etc.).
pub const NET_REGISTRY_ERR_SPAWN_REJECTED: i32 = 5;
/// Server doesn't accept dynamic spawn (read-only daemon).
pub const NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED: i32 = 6;
/// Server handler rejected `Scale`: no group by that name is
/// registered on the target.
pub const NET_REGISTRY_ERR_UNKNOWN_GROUP: i32 = 8;
/// Server handler rejected `Scale` for a daemon-defined reason
/// (template mismatch, replica spawn/stop failure, etc.).
pub const NET_REGISTRY_ERR_SCALE_REJECTED: i32 = 9;
/// Server doesn't accept dynamic scale (no scale handler
/// installed).
pub const NET_REGISTRY_ERR_SCALE_NOT_SUPPORTED: i32 = 10;
/// Caller-side error: a string argument wasn't valid UTF-8 or
/// a pointer was null where one was required.
pub const NET_REGISTRY_ERR_INVALID_ARGS: i32 = 99;

// ─── Visibility discriminants ───

/// Wire-equivalent of [`Visibility`]. Values are
/// representation-stable across SDK releases — operator code
/// referring to them by literal value (not just by name) stays
/// correct. Mirrors every substrate variant 1-to-1; mirror order
/// is sorted by tier-broadness for operator readability.
#[repr(i32)]
#[derive(Copy, Clone)]
pub enum NetVisibility {
    /// Mirrors [`Visibility::Global`] — visible everywhere.
    Global = 0,
    /// Mirrors [`Visibility::ParentVisible`].
    ParentVisible = 1,
    /// Mirrors [`Visibility::Exported`] — explicit per-subnet export list.
    Exported = 2,
    /// Mirrors [`Visibility::SubnetLocal`] — packets never leave the subnet.
    SubnetLocal = 3,
}

impl NetVisibility {
    fn from_raw(raw: i32) -> Option<Visibility> {
        match raw {
            0 => Some(Visibility::Global),
            1 => Some(Visibility::ParentVisible),
            2 => Some(Visibility::Exported),
            3 => Some(Visibility::SubnetLocal),
            _ => None,
        }
    }

    /// Compile-time exhaustiveness check in the *opposite*
    /// direction — every substrate [`Visibility`] variant must
    /// have a wire-stable C ABI counterpart. If the substrate
    /// gains a variant, this `match` stops compiling, forcing
    /// the FFI maintainer to either add the discriminant + bump
    /// the wire contract or explicitly accept the omission with
    /// `_ => None`. Without this, [`from_raw`] would silently
    /// reject the new variant and operator code referring to it
    /// by literal value would see a NULL handle / ERR_INVALID
    /// instead of a typed wire error.
    #[allow(dead_code)] // existence is the check
    fn to_raw(v: Visibility) -> NetVisibility {
        match v {
            Visibility::Global => NetVisibility::Global,
            Visibility::ParentVisible => NetVisibility::ParentVisible,
            Visibility::Exported => NetVisibility::Exported,
            Visibility::SubnetLocal => NetVisibility::SubnetLocal,
        }
    }
}

// ─── Handle ───

/// FFI handle for a [`RegistryClient`].
///
/// The inner client is wrapped in a `RwLock` so concurrent ops
/// (entry points are called from many threads in async runtimes)
/// can share read access while a `set_deadline` writer
/// serializes. `last_error_detail` lives behind a separate
/// `parking_lot::Mutex`; the lifetime contract for pointers
/// returned by [`net_registry_last_error_detail`] is "valid
/// until the next op on this handle or until free".
pub struct RegistryClientHandle {
    client: ParkingRwLock<RegistryClient>,
    last_error_detail: ParkingMutex<Option<CString>>,
}

// ─── Constructor / free / builder ───

/// Construct a `RegistryClient` against an existing
/// [`MeshNodeHandle`]. Returns a handle the caller frees via
/// [`net_registry_client_free`]. Returns NULL on null input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_new(
    mesh_handle: *mut MeshNodeHandle,
) -> *mut RegistryClientHandle {
    if mesh_handle.is_null() {
        return std::ptr::null_mut();
    }
    // Gated clone of the mesh node — `None` means the mesh handle is
    // being freed concurrently; surface a null handle rather than
    // racing the inner out of `ManuallyDrop`.
    let Some(mesh_arc) = (unsafe { super::mesh::mesh_node_arc(&*mesh_handle) }) else {
        return std::ptr::null_mut();
    };
    let boxed = Box::new(RegistryClientHandle {
        client: ParkingRwLock::new(RegistryClient::new(mesh_arc)),
        last_error_detail: ParkingMutex::new(None),
    });
    Box::into_raw(boxed)
}

/// Free a `RegistryClient` handle produced by
/// [`net_registry_client_new`]. Idempotent on NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_free(handle: *mut RegistryClientHandle) {
    if handle.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(handle) });
}

/// Override the per-call deadline in milliseconds. `millis == 0`
/// resets to the substrate default. Safe to call concurrently
/// with in-flight ops; the writer takes the inner lock briefly
/// and any concurrent reader either observes the old or the new
/// deadline (no torn read).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_set_deadline(
    handle: *mut RegistryClientHandle,
    millis: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &RegistryClientHandle = unsafe { &*handle };
    let deadline = if millis == 0 {
        DEFAULT_REGISTRY_DEADLINE
    } else {
        Duration::from_millis(millis)
    };
    h.client.write().set_deadline_mut(deadline);
}

// ─── Op-handler internals ───
//
// Every public `net_registry_client_*` op shares the same six
// steps: null-check, parse CStr args, snapshot the client under
// the read lock, await the substrate call, classify+store-detail
// on error, write the out param. The `dispatch_*` + `write_*`
// helpers below capture each step once.

/// Set `*out` if non-null and return the JSON pointer + status.
/// Op handlers funnel every success / failure path through this
/// so the null-check on `out_error_kind` is centralized.
#[inline]
unsafe fn write_kind(out: *mut c_int, kind: c_int) {
    if !out.is_null() {
        unsafe { *out = kind };
    }
}

/// Read a NUL-terminated UTF-8 string argument and return an
/// owned `String`, or set the out-param to `INVALID_ARGS` +
/// return `None` if the pointer is null or the bytes aren't
/// valid UTF-8.
#[inline]
unsafe fn cstr_arg(ptr: *const c_char, out: *mut c_int) -> Option<String> {
    if ptr.is_null() {
        unsafe { write_kind(out, NET_REGISTRY_ERR_INVALID_ARGS) };
        return None;
    }
    match unsafe { CStr::from_ptr(ptr).to_str() } {
        Ok(s) => Some(s.to_owned()),
        Err(_) => {
            unsafe { write_kind(out, NET_REGISTRY_ERR_INVALID_ARGS) };
            None
        }
    }
}

/// Convert a JSON string into a heap-allocated `*mut c_char` the
/// caller frees with `net_free_string`. Returns NULL + sets the
/// out-param to `CODEC` if the string contains an embedded NUL.
#[inline]
unsafe fn json_to_raw(json: String, out: *mut c_int) -> *mut c_char {
    match CString::new(json) {
        Ok(s) => {
            unsafe { write_kind(out, NET_REGISTRY_OK) };
            s.into_raw()
        }
        Err(_) => {
            unsafe { write_kind(out, NET_REGISTRY_ERR_CODEC) };
            std::ptr::null_mut()
        }
    }
}

/// Funnel for any registry op that returns a JSON string.
/// Takes a closure that produces `Result<String, RegistryClientError>`
/// (the JSON-encoding step is the caller's responsibility because
/// the substrate type varies per op).
unsafe fn registry_op_json<F>(
    handle: *mut RegistryClientHandle,
    out_error_kind: *mut c_int,
    op: F,
) -> *mut c_char
where
    F: FnOnce(RegistryClient) -> Result<String, RegistryClientError>,
{
    if handle.is_null() {
        unsafe { write_kind(out_error_kind, NET_REGISTRY_ERR_INVALID_ARGS) };
        return std::ptr::null_mut();
    }
    let h: &RegistryClientHandle = unsafe { &*handle };
    let client = h.client.read().clone();
    match op(client) {
        Ok(json) => unsafe { json_to_raw(json, out_error_kind) },
        Err(e) => {
            let (kind, detail) = classify(&e);
            store_error_detail(h, detail);
            unsafe { write_kind(out_error_kind, kind) };
            std::ptr::null_mut()
        }
    }
}

// ─── Operations ───

/// Enumerate groups on `target_node_id`. Returns a JSON-encoded
/// `[RegistryGroupSummaryJson]` string the caller frees via
/// `net_free_string`. On error, writes the error kind to
/// `*out_error_kind` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_list(
    handle: *mut RegistryClientHandle,
    target_node_id: u64,
    out_error_kind: *mut c_int,
) -> *mut c_char {
    if out_error_kind.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        registry_op_json(handle, out_error_kind, |client| {
            block_on(client.list(target_node_id)).map(|groups| groups_to_json(&groups))
        })
    }
}

/// Spawn a new group by referencing a daemon-side template.
/// `template_name` + `group_name` are NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_spawn(
    handle: *mut RegistryClientHandle,
    target_node_id: u64,
    template_name: *const c_char,
    group_name: *const c_char,
    replica_count: u8,
    out_error_kind: *mut c_int,
) -> *mut c_char {
    let Some(template) = (unsafe { cstr_arg(template_name, out_error_kind) }) else {
        return std::ptr::null_mut();
    };
    let Some(group) = (unsafe { cstr_arg(group_name, out_error_kind) }) else {
        return std::ptr::null_mut();
    };
    unsafe {
        registry_op_json(handle, out_error_kind, |client| {
            block_on(client.spawn(target_node_id, template, group, replica_count))
                .map(|summary| group_to_json(&summary))
        })
    }
}

/// Tear down a registered group by name. Returns `1` when the
/// group existed and was stopped, `0` when no such group was
/// registered, `-1` on transport / codec / invalid-args
/// failure (consult `out_error_kind`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_unregister(
    handle: *mut RegistryClientHandle,
    target_node_id: u64,
    group_name: *const c_char,
    out_error_kind: *mut c_int,
) -> c_int {
    if handle.is_null() {
        unsafe { write_kind(out_error_kind, NET_REGISTRY_ERR_INVALID_ARGS) };
        return -1;
    }
    let Some(group) = (unsafe { cstr_arg(group_name, out_error_kind) }) else {
        return -1;
    };
    let h: &RegistryClientHandle = unsafe { &*handle };
    let client = h.client.read().clone();
    match block_on(client.unregister(target_node_id, group)) {
        Ok(existed) => {
            unsafe { write_kind(out_error_kind, NET_REGISTRY_OK) };
            if existed {
                1
            } else {
                0
            }
        }
        Err(e) => {
            let (kind, detail) = classify(&e);
            store_error_detail(h, detail);
            unsafe { write_kind(out_error_kind, kind) };
            -1
        }
    }
}

/// Get the operator-facing detail string for the most recent
/// non-OK op on this handle. Returns a NUL-terminated C string
/// owned by the handle — the pointer is valid until the next
/// op (which may overwrite it) or until the handle is freed.
/// Returns NULL when no error has been recorded.
///
/// Callers wanting to hold the string across other ops should
/// copy it before doing anything else with the handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_last_error_detail(
    handle: *mut RegistryClientHandle,
) -> *const c_char {
    if handle.is_null() {
        return std::ptr::null();
    }
    let h: &RegistryClientHandle = unsafe { &*handle };
    let guard = h.last_error_detail.lock();
    match guard.as_ref() {
        Some(c) => c.as_ptr(),
        None => std::ptr::null(),
    }
}

// ─── Visibility setter ───

/// Register a channel with a specific [`Visibility`] tier.
/// Mirrors `Mesh::register_channel` from the Rust SDK at the C
/// boundary. `visibility` is an [`i32`] matching the
/// [`NetVisibility`] discriminants.
///
/// Returns `NET_REGISTRY_OK` on success or a typed error code.
/// Operator-facing detail (e.g. "invalid channel name") is
/// written to a side-channel: the substrate logs via `tracing`
/// — no per-call detail string is allocated at this layer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_register_channel(
    mesh_handle: *mut MeshNodeHandle,
    name: *const c_char,
    visibility: c_int,
) -> c_int {
    if mesh_handle.is_null() || name.is_null() {
        return NET_REGISTRY_ERR_INVALID_ARGS;
    }
    let vis = match NetVisibility::from_raw(visibility) {
        Some(v) => v,
        None => return NET_REGISTRY_ERR_INVALID_ARGS,
    };
    let name_str = match unsafe { CStr::from_ptr(name).to_str() } {
        Ok(s) => s,
        Err(_) => return NET_REGISTRY_ERR_INVALID_ARGS,
    };
    let channel = match ChannelName::new(name_str) {
        Ok(c) => c,
        Err(_) => return NET_REGISTRY_ERR_INVALID_ARGS,
    };
    // Use the mesh's installed ChannelConfigRegistry. The
    // mesh-FFI's net_mesh_new always installs one, so this is
    // safe; if it ever changes, the registry being `None` is
    // surfaced as NET_REGISTRY_ERR_INVALID_ARGS.
    let Some(mesh_arc) = (unsafe { super::mesh::mesh_node_arc(&*mesh_handle) }) else {
        return NET_REGISTRY_ERR_INVALID_ARGS;
    };
    let Some(configs) = mesh_arc.channel_configs() else {
        return NET_REGISTRY_ERR_INVALID_ARGS;
    };
    let cfg = ChannelConfig::new(ChannelId::new(channel)).with_visibility(vis);
    configs.insert(cfg);
    NET_REGISTRY_OK
}

// ─── FoldQueryClient handle ───

/// FFI handle for a [`FoldQueryClient`]. Same sync model as
/// [`RegistryClientHandle`]: the inner client lives behind a
/// `RwLock` so `set_ttl` / `set_deadline` writers serialize with
/// in-flight ops, and the cache (held by the inner client's
/// `Arc<RwLock<HashMap<...>>>`) survives deadline / TTL changes.
pub struct FoldQueryClientHandle {
    client: ParkingRwLock<FoldQueryClient>,
    last_error_detail: ParkingMutex<Option<CString>>,
}

/// Construct a `FoldQueryClient` against an existing
/// [`MeshNodeHandle`]. Returns a handle the caller frees via
/// [`net_fold_query_client_free`]. Returns NULL on null input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_new(
    mesh_handle: *mut MeshNodeHandle,
) -> *mut FoldQueryClientHandle {
    if mesh_handle.is_null() {
        return std::ptr::null_mut();
    }
    let Some(mesh_arc) = (unsafe { super::mesh::mesh_node_arc(&*mesh_handle) }) else {
        return std::ptr::null_mut();
    };
    let boxed = Box::new(FoldQueryClientHandle {
        client: ParkingRwLock::new(FoldQueryClient::new(mesh_arc)),
        last_error_detail: ParkingMutex::new(None),
    });
    Box::into_raw(boxed)
}

/// Free a `FoldQueryClient` handle. Idempotent on NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_free(handle: *mut FoldQueryClientHandle) {
    if handle.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(handle) });
}

/// Override the cache TTL in milliseconds. `millis == 0` disables
/// the cache entirely. Mutates in place — the warmed cache
/// survives the adjustment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_set_ttl(
    handle: *mut FoldQueryClientHandle,
    millis: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    h.client.write().set_ttl_mut(Duration::from_millis(millis));
}

/// Override the per-call deadline in milliseconds. `millis == 0`
/// resets to the substrate default. Mutates in place.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_set_deadline(
    handle: *mut FoldQueryClientHandle,
    millis: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    let deadline = if millis == 0 {
        DEFAULT_QUERY_DEADLINE
    } else {
        Duration::from_millis(millis)
    };
    h.client.write().set_deadline_mut(deadline);
}

/// Query the aggregator's latest cached summaries. Cache hit
/// returns immediately; miss issues a wire RPC, caches the
/// response, and returns. Returns a JSON-encoded
/// `[SummaryAnnouncementJson]` string the caller frees via
/// `net_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_query_latest(
    handle: *mut FoldQueryClientHandle,
    target_node_id: u64,
    kind: u16,
    out_error_kind: *mut c_int,
) -> *mut c_char {
    if out_error_kind.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        fold_query_op_json(handle, out_error_kind, |client| {
            block_on(client.query_latest(target_node_id, kind))
                .map(|summaries| summaries_to_json(&summaries))
        })
    }
}

/// Force a fresh `SummarizeNow` query — never cached.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_query_summarize_now(
    handle: *mut FoldQueryClientHandle,
    target_node_id: u64,
    kind: u16,
    out_error_kind: *mut c_int,
) -> *mut c_char {
    if out_error_kind.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        fold_query_op_json(handle, out_error_kind, |client| {
            block_on(client.query_summarize_now(target_node_id, kind))
                .map(|summaries| summaries_to_json(&summaries))
        })
    }
}

/// Drop every cached entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_invalidate_cache(
    handle: *mut FoldQueryClientHandle,
) {
    if handle.is_null() {
        return;
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    h.client.read().invalidate_cache();
}

/// Drop only cache entries matching `target_node_id`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_invalidate_target(
    handle: *mut FoldQueryClientHandle,
    target_node_id: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    h.client.read().invalidate_target(target_node_id);
}

/// Operator-facing detail string for the most recent non-OK
/// fold-query op. Same valid-until contract as
/// [`net_registry_last_error_detail`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_last_error_detail(
    handle: *mut FoldQueryClientHandle,
) -> *const c_char {
    if handle.is_null() {
        return std::ptr::null();
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    let guard = h.last_error_detail.lock();
    match guard.as_ref() {
        Some(c) => c.as_ptr(),
        None => std::ptr::null(),
    }
}

// ─── Internals ───

/// Run a future to completion on the shared mesh-FFI tokio
/// runtime. Same as `ffi::mesh::block_on` — re-uses that
/// runtime so we don't fragment scheduling.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    super::mesh::block_on(future)
}

/// Funnel for any fold-query op that returns a JSON string.
/// Mirror of [`registry_op_json`].
unsafe fn fold_query_op_json<F>(
    handle: *mut FoldQueryClientHandle,
    out_error_kind: *mut c_int,
    op: F,
) -> *mut c_char
where
    F: FnOnce(FoldQueryClient) -> Result<String, FoldQueryClientError>,
{
    if handle.is_null() {
        unsafe { write_kind(out_error_kind, NET_REGISTRY_ERR_INVALID_ARGS) };
        return std::ptr::null_mut();
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    let client = h.client.read().clone();
    match op(client) {
        Ok(json) => unsafe { json_to_raw(json, out_error_kind) },
        Err(e) => {
            let (kind, detail) = classify_fold_query(&e);
            store_fold_query_error_detail(h, detail);
            unsafe { write_kind(out_error_kind, kind) };
            std::ptr::null_mut()
        }
    }
}

fn classify_fold_query(err: &FoldQueryClientError) -> (i32, String) {
    match err {
        FoldQueryClientError::Transport(e) => (NET_REGISTRY_ERR_TRANSPORT, format!("{e}")),
        FoldQueryClientError::Codec(c) => (NET_REGISTRY_ERR_CODEC, c.clone()),
        FoldQueryClientError::Server(FoldQueryError::UnknownKind { kind }) => (
            NET_REGISTRY_ERR_UNKNOWN_KIND,
            format!("unknown fold kind: 0x{kind:04x}"),
        ),
        FoldQueryClientError::Server(FoldQueryError::DecodeFailed(s)) => {
            (NET_REGISTRY_ERR_CODEC, format!("server decode: {s}"))
        }
    }
}

fn store_fold_query_error_detail(h: &FoldQueryClientHandle, detail: String) {
    let c = match CString::new(detail) {
        Ok(c) => c,
        Err(_) => CString::new("invalid utf-8 in error detail").unwrap_or_default(),
    };
    *h.last_error_detail.lock() = Some(c);
}

fn summaries_to_json(summaries: &[SummaryAnnouncement]) -> String {
    let wire: Vec<SummaryWire<'_>> = summaries.iter().map(SummaryWire::from).collect();
    // `to_string` only fails on serializer-side issues — none of
    // our wire types have non-string map keys or Float NaN — so
    // the unwrap is unreachable. Defensive `to_string`-on-error
    // keeps the FFI surface infallible.
    serde_json::to_string(&wire).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
fn summary_to_json(s: &SummaryAnnouncement) -> String {
    serde_json::to_string(&SummaryWire::from(s)).unwrap_or_else(|_| "{}".to_string())
}

#[derive(serde::Serialize)]
struct SummaryWire<'a> {
    fold_kind: u16,
    source_subnet: String,
    generation: u64,
    buckets: Vec<BucketWire<'a>>,
}

#[derive(serde::Serialize)]
struct BucketWire<'a> {
    name: &'a str,
    count: u64,
}

impl<'a> From<&'a SummaryAnnouncement> for SummaryWire<'a> {
    fn from(s: &'a SummaryAnnouncement) -> Self {
        Self {
            fold_kind: s.fold_kind,
            source_subnet: format!("{}", s.source_subnet),
            generation: s.generation,
            buckets: s
                .buckets
                .iter()
                .map(|(n, c)| BucketWire {
                    name: n.as_str(),
                    count: *c,
                })
                .collect(),
        }
    }
}

/// Map a `RegistryClientError` to `(error_kind, detail_string)`.
fn classify(err: &RegistryClientError) -> (i32, String) {
    match err {
        RegistryClientError::Transport(e) => (NET_REGISTRY_ERR_TRANSPORT, format!("{e}")),
        RegistryClientError::Codec(c) => (NET_REGISTRY_ERR_CODEC, c.clone()),
        RegistryClientError::Server(RegistryRpcError::DecodeFailed(s)) => {
            (NET_REGISTRY_ERR_CODEC, format!("server decode: {s}"))
        }
        RegistryClientError::Server(RegistryRpcError::UnknownTemplate(t)) => (
            NET_REGISTRY_ERR_UNKNOWN_TEMPLATE,
            format!("unknown template: {t}"),
        ),
        RegistryClientError::Server(RegistryRpcError::DuplicateGroupName(n)) => (
            NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME,
            format!("duplicate group name: {n}"),
        ),
        RegistryClientError::Server(RegistryRpcError::SpawnRejected(d)) => (
            NET_REGISTRY_ERR_SPAWN_REJECTED,
            format!("spawn rejected: {d}"),
        ),
        RegistryClientError::Server(RegistryRpcError::SpawnNotSupported) => (
            NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED,
            "daemon is read-only (no spawn handler installed)".to_string(),
        ),
        RegistryClientError::Server(RegistryRpcError::UnknownGroup(g)) => (
            NET_REGISTRY_ERR_UNKNOWN_GROUP,
            format!("unknown group: {g}"),
        ),
        RegistryClientError::Server(RegistryRpcError::ScaleRejected(d)) => (
            NET_REGISTRY_ERR_SCALE_REJECTED,
            format!("scale rejected: {d}"),
        ),
        RegistryClientError::Server(RegistryRpcError::ScaleNotSupported) => (
            NET_REGISTRY_ERR_SCALE_NOT_SUPPORTED,
            "daemon doesn't accept dynamic scale (no scaler installed)".to_string(),
        ),
    }
}

fn store_error_detail(h: &RegistryClientHandle, detail: String) {
    let c = match CString::new(detail) {
        Ok(c) => c,
        Err(_) => CString::new("invalid utf-8 in error detail").unwrap_or_default(),
    };
    *h.last_error_detail.lock() = Some(c);
}

/// Encode the wire-contract JSON for a slice of registry-group
/// summaries via `serde_json`. The substrate type
/// `RegistryGroupSummary` derives `Serialize` but its
/// `group_seed: [u8; 32]` field serializes as an array of u8 —
/// the wire contract calls for `group_seed_hex: "abab…"` (64
/// lowercase hex chars). The proxy wire-types below handle the
/// rename + hex encoding.
fn groups_to_json(groups: &[RegistryGroupSummary]) -> String {
    let wire: Vec<GroupWire<'_>> = groups.iter().map(GroupWire::from).collect();
    serde_json::to_string(&wire).unwrap_or_else(|_| "[]".to_string())
}

fn group_to_json(g: &RegistryGroupSummary) -> String {
    serde_json::to_string(&GroupWire::from(g)).unwrap_or_else(|_| "{}".to_string())
}

#[derive(serde::Serialize)]
struct GroupWire<'a> {
    name: &'a str,
    group_seed_hex: String,
    replicas: Vec<ReplicaWire<'a>>,
}

#[derive(serde::Serialize)]
struct ReplicaWire<'a> {
    generation: u64,
    healthy: bool,
    diagnostic: Option<&'a str>,
    placement_node_id: Option<u64>,
}

impl<'a> From<&'a RegistryGroupSummary> for GroupWire<'a> {
    fn from(g: &'a RegistryGroupSummary) -> Self {
        Self {
            name: g.name.as_str(),
            group_seed_hex: hex::encode(g.group_seed),
            replicas: g
                .replicas
                .iter()
                .map(|r| ReplicaWire {
                    generation: r.generation,
                    healthy: r.healthy,
                    diagnostic: r.diagnostic.as_deref(),
                    placement_node_id: r.placement_node_id,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_round_trips_through_raw() {
        for (raw, expected) in [
            (0, Visibility::Global),
            (1, Visibility::ParentVisible),
            (2, Visibility::Exported),
            (3, Visibility::SubnetLocal),
        ] {
            let back = NetVisibility::from_raw(raw).expect("known discriminant");
            assert_eq!(format!("{back:?}"), format!("{expected:?}"));
        }
        assert!(NetVisibility::from_raw(99).is_none());
        assert!(NetVisibility::from_raw(-1).is_none());
    }

    #[test]
    fn group_to_json_includes_every_documented_field() {
        let g = RegistryGroupSummary {
            name: "alpha".into(),
            group_seed: [0xABu8; 32],
            source_subnet: crate::adapter::net::subnet::SubnetId::GLOBAL,
            fold_kinds: vec![0x0001],
            replicas: vec![
                crate::adapter::net::behavior::aggregator::RegistryReplicaSummary {
                    generation: 42,
                    healthy: true,
                    diagnostic: None,
                    placement_node_id: Some(0xBEEF),
                },
                crate::adapter::net::behavior::aggregator::RegistryReplicaSummary {
                    generation: 0,
                    healthy: false,
                    diagnostic: Some("stuck".into()),
                    placement_node_id: None,
                },
            ],
        };
        let json = group_to_json(&g);
        assert!(json.contains("\"name\":\"alpha\""));
        // Each byte 0xAB → "ab"; 32 of them = 64 hex chars
        // alternating "ab".
        assert!(json.contains("\"group_seed_hex\":\"abababababababababababababababababababababababababababababababab\""));
        assert!(json.contains("\"generation\":42"));
        assert!(json.contains("\"healthy\":true"));
        assert!(json.contains("\"diagnostic\":null"));
        assert!(json.contains("\"placement_node_id\":48879"));
        assert!(json.contains("\"healthy\":false"));
        assert!(json.contains("\"diagnostic\":\"stuck\""));
        assert!(json.contains("\"placement_node_id\":null"));
    }

    #[test]
    fn summary_to_json_includes_every_documented_field() {
        let s = SummaryAnnouncement {
            fold_kind: 0x42,
            source_subnet: crate::adapter::net::subnet::SubnetId::GLOBAL,
            generation: 7,
            buckets: vec![("alpha".into(), 1), ("beta".into(), 2)],
        };
        let json = summary_to_json(&s);
        assert!(json.contains("\"fold_kind\":66"));
        assert!(json.contains("\"source_subnet\":\"global\""));
        assert!(json.contains("\"generation\":7"));
        assert!(json.contains("\"name\":\"alpha\""));
        assert!(json.contains("\"count\":1"));
        assert!(json.contains("\"name\":\"beta\""));
        assert!(json.contains("\"count\":2"));
    }

    #[test]
    fn classify_fold_query_maps_every_variant() {
        use crate::adapter::net::mesh_rpc::RpcError;
        // Transport — anything carrying an RpcError lands on
        // NET_REGISTRY_ERR_TRANSPORT regardless of the inner kind.
        let transport = FoldQueryClientError::Transport(RpcError::NoRoute {
            target: 0,
            reason: String::new(),
        });
        assert_eq!(
            classify_fold_query(&transport).0,
            NET_REGISTRY_ERR_TRANSPORT
        );

        let codec = FoldQueryClientError::Codec("bad".into());
        assert_eq!(classify_fold_query(&codec).0, NET_REGISTRY_ERR_CODEC);

        let unknown_kind = FoldQueryClientError::Server(FoldQueryError::UnknownKind { kind: 0x42 });
        let (kind_code, detail) = classify_fold_query(&unknown_kind);
        assert_eq!(kind_code, NET_REGISTRY_ERR_UNKNOWN_KIND);
        assert!(detail.contains("0x0042"));

        let decode_failed =
            FoldQueryClientError::Server(FoldQueryError::DecodeFailed("boom".into()));
        assert_eq!(
            classify_fold_query(&decode_failed).0,
            NET_REGISTRY_ERR_CODEC,
        );
    }
}
