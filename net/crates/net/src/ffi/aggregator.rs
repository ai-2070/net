//! C FFI bindings for the aggregator-registry RPC client +
//! channel-visibility setter.
//!
//! Stage 5 of `docs/plans/SDK_AGGREGATOR_SUBNET_PLAN.md`. Surface
//! targeted at the Go SDK (via the future `aggregator-ffi`
//! cdylib) + any raw C consumer.
//!
//! # Surface
//!
//! - `net_registry_client_new` / `_free` / `_set_deadline` —
//!   construct + tear down a [`RegistryClient`].
//! - `net_registry_client_list` — enumerate groups; returns a
//!   JSON array string.
//! - `net_registry_client_spawn` — deploy a new group; returns
//!   the spawned group's JSON.
//! - `net_registry_client_unregister` — tear down a group;
//!   returns `true`/`false`.
//! - `net_registry_last_error_detail` — operator-facing detail
//!   string for the last failed call on this handle.
//! - `net_register_channel` — channel-config setter with the
//!   `net_visibility_t` enum.
//!
//! Everything crosses the boundary the same way as
//! `ffi::mesh`: opaque handles freed via dedicated `_free`,
//! scalar ids as `u64`, JSON strings via `CString::into_raw`
//! freed by the caller via `net_free_string`.
//!
//! # Safety
//!
//! Same caller-side contract as `ffi::mesh` and `ffi::cortex` —
//! see those module preambles. `clippy::missing_safety_doc`
//! and per-block `// SAFETY:` comments are suppressed at the
//! module level for the same rationale.
#![allow(clippy::missing_safety_doc)]
#![expect(
    clippy::undocumented_unsafe_blocks,
    reason = "module-wide FFI safety contract documented in ffi::mod.rs preamble"
)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as ParkingMutex;

use crate::adapter::net::behavior::aggregator::{
    FoldQueryClient, FoldQueryClientError, FoldQueryError, RegistryClient, RegistryClientError,
    RegistryGroupSummary, RegistryRpcError, SummaryAnnouncement,
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
}

// ─── Handle ───

/// FFI handle for a [`RegistryClient`] + a slot for the last
/// error's operator-facing detail string.
pub struct RegistryClientHandle {
    client: RegistryClient,
    /// Held alongside `client` so `set_deadline` can rebuild
    /// the client (RegistryClient is `with_deadline(self) -> Self`).
    mesh: Arc<crate::adapter::net::MeshNode>,
    /// Diagnostic detail for the most recent op that returned
    /// a non-OK status. `parking_lot::Mutex` because the FFI
    /// surface is `Sync` (entry points are called from multiple
    /// threads in async runtimes).
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
    let mesh_arc: Arc<crate::adapter::net::MeshNode> =
        unsafe { super::mesh::mesh_node_arc(&*mesh_handle) };
    let client = RegistryClient::new(mesh_arc.clone());
    let boxed = Box::new(RegistryClientHandle {
        client,
        mesh: mesh_arc,
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

/// Override the per-call deadline in milliseconds.
/// `millis == 0` resets to the substrate default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_registry_client_set_deadline(
    handle: *mut RegistryClientHandle,
    millis: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &mut RegistryClientHandle = unsafe { &mut *handle };
    let new_client = if millis == 0 {
        RegistryClient::new(h.mesh.clone())
    } else {
        RegistryClient::new(h.mesh.clone()).with_deadline(Duration::from_millis(millis))
    };
    h.client = new_client;
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
    if handle.is_null() || out_error_kind.is_null() {
        if !out_error_kind.is_null() {
            unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
        }
        return std::ptr::null_mut();
    }
    let h: &RegistryClientHandle = unsafe { &*handle };
    let result = block_on(h.client.list(target_node_id));
    match result {
        Ok(groups) => {
            let json = groups_to_json(&groups);
            unsafe { *out_error_kind = NET_REGISTRY_OK };
            match CString::new(json) {
                Ok(s) => s.into_raw(),
                Err(_) => {
                    unsafe { *out_error_kind = NET_REGISTRY_ERR_CODEC };
                    std::ptr::null_mut()
                }
            }
        }
        Err(e) => {
            let (kind, detail) = classify(&e);
            store_error_detail(h, detail);
            unsafe { *out_error_kind = kind };
            std::ptr::null_mut()
        }
    }
}

/// Spawn a new group by referencing a daemon-side template.
/// Returns a JSON-encoded `RegistryGroupSummaryJson` for the
/// spawned group; caller frees via `net_free_string`.
///
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
    if handle.is_null() || template_name.is_null() || group_name.is_null() {
        if !out_error_kind.is_null() {
            unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
        }
        return std::ptr::null_mut();
    }
    let h: &RegistryClientHandle = unsafe { &*handle };
    let template = match unsafe { CStr::from_ptr(template_name).to_str() } {
        Ok(s) => s.to_owned(),
        Err(_) => {
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
            }
            return std::ptr::null_mut();
        }
    };
    let group = match unsafe { CStr::from_ptr(group_name).to_str() } {
        Ok(s) => s.to_owned(),
        Err(_) => {
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
            }
            return std::ptr::null_mut();
        }
    };
    let result = block_on(
        h.client
            .spawn(target_node_id, template, group, replica_count),
    );
    match result {
        Ok(summary) => {
            let json = group_to_json(&summary);
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = NET_REGISTRY_OK };
            }
            match CString::new(json) {
                Ok(s) => s.into_raw(),
                Err(_) => {
                    if !out_error_kind.is_null() {
                        unsafe { *out_error_kind = NET_REGISTRY_ERR_CODEC };
                    }
                    std::ptr::null_mut()
                }
            }
        }
        Err(e) => {
            let (kind, detail) = classify(&e);
            store_error_detail(h, detail);
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = kind };
            }
            std::ptr::null_mut()
        }
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
    if handle.is_null() || group_name.is_null() {
        if !out_error_kind.is_null() {
            unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
        }
        return -1;
    }
    let h: &RegistryClientHandle = unsafe { &*handle };
    let group = match unsafe { CStr::from_ptr(group_name).to_str() } {
        Ok(s) => s.to_owned(),
        Err(_) => {
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
            }
            return -1;
        }
    };
    match block_on(h.client.unregister(target_node_id, group)) {
        Ok(existed) => {
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = NET_REGISTRY_OK };
            }
            if existed {
                1
            } else {
                0
            }
        }
        Err(e) => {
            let (kind, detail) = classify(&e);
            store_error_detail(h, detail);
            if !out_error_kind.is_null() {
                unsafe { *out_error_kind = kind };
            }
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
    let mesh_arc: Arc<crate::adapter::net::MeshNode> =
        unsafe { super::mesh::mesh_node_arc(&*mesh_handle) };
    let Some(configs) = mesh_arc.channel_configs() else {
        return NET_REGISTRY_ERR_INVALID_ARGS;
    };
    let cfg = ChannelConfig::new(ChannelId::new(channel)).with_visibility(vis);
    configs.insert(cfg);
    NET_REGISTRY_OK
}

// ─── FoldQueryClient handle ───

/// FFI handle for a [`FoldQueryClient`] + a slot for the last
/// error's operator-facing detail string.
pub struct FoldQueryClientHandle {
    client: FoldQueryClient,
    /// Held alongside `client` so `set_ttl` / `set_deadline` can
    /// rebuild the client (builders are `with_*(self) -> Self`).
    mesh: Arc<crate::adapter::net::MeshNode>,
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
    let mesh_arc: Arc<crate::adapter::net::MeshNode> =
        unsafe { super::mesh::mesh_node_arc(&*mesh_handle) };
    let client = FoldQueryClient::new(mesh_arc.clone());
    let boxed = Box::new(FoldQueryClientHandle {
        client,
        mesh: mesh_arc,
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
/// the cache entirely.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_set_ttl(
    handle: *mut FoldQueryClientHandle,
    millis: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &mut FoldQueryClientHandle = unsafe { &mut *handle };
    h.client = FoldQueryClient::new(h.mesh.clone()).with_ttl(Duration::from_millis(millis));
}

/// Override the per-call deadline in milliseconds. `millis == 0`
/// resets to the substrate default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_set_deadline(
    handle: *mut FoldQueryClientHandle,
    millis: u64,
) {
    if handle.is_null() {
        return;
    }
    let h: &mut FoldQueryClientHandle = unsafe { &mut *handle };
    h.client = if millis == 0 {
        FoldQueryClient::new(h.mesh.clone())
    } else {
        FoldQueryClient::new(h.mesh.clone()).with_deadline(Duration::from_millis(millis))
    };
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
    if handle.is_null() || out_error_kind.is_null() {
        if !out_error_kind.is_null() {
            unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
        }
        return std::ptr::null_mut();
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    let result = block_on(h.client.query_latest(target_node_id, kind));
    finish_summaries(h, result, out_error_kind)
}

/// Force a fresh `SummarizeNow` query — never cached. Returns
/// the same JSON shape as `_query_latest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fold_query_client_query_summarize_now(
    handle: *mut FoldQueryClientHandle,
    target_node_id: u64,
    kind: u16,
    out_error_kind: *mut c_int,
) -> *mut c_char {
    if handle.is_null() || out_error_kind.is_null() {
        if !out_error_kind.is_null() {
            unsafe { *out_error_kind = NET_REGISTRY_ERR_INVALID_ARGS };
        }
        return std::ptr::null_mut();
    }
    let h: &FoldQueryClientHandle = unsafe { &*handle };
    let result = block_on(h.client.query_summarize_now(target_node_id, kind));
    finish_summaries(h, result, out_error_kind)
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
    h.client.invalidate_cache();
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
    h.client.invalidate_target(target_node_id);
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

fn finish_summaries(
    h: &FoldQueryClientHandle,
    result: Result<Vec<SummaryAnnouncement>, FoldQueryClientError>,
    out_error_kind: *mut c_int,
) -> *mut c_char {
    match result {
        Ok(summaries) => {
            let json = summaries_to_json(&summaries);
            unsafe { *out_error_kind = NET_REGISTRY_OK };
            match CString::new(json) {
                Ok(s) => s.into_raw(),
                Err(_) => {
                    unsafe { *out_error_kind = NET_REGISTRY_ERR_CODEC };
                    std::ptr::null_mut()
                }
            }
        }
        Err(e) => {
            let (kind, detail) = classify_fold_query(&e);
            store_fold_query_error_detail(h, detail);
            unsafe { *out_error_kind = kind };
            std::ptr::null_mut()
        }
    }
}

fn classify_fold_query(err: &FoldQueryClientError) -> (i32, String) {
    match err {
        FoldQueryClientError::Transport(e) => (NET_REGISTRY_ERR_TRANSPORT, format!("{e}")),
        FoldQueryClientError::Codec(c) => (NET_REGISTRY_ERR_CODEC, c.clone()),
        FoldQueryClientError::Server(srv) => match srv {
            FoldQueryError::UnknownKind { kind } => (
                NET_REGISTRY_ERR_UNKNOWN_KIND,
                format!("unknown fold kind: 0x{kind:04x}"),
            ),
            FoldQueryError::DecodeFailed(s) => {
                (NET_REGISTRY_ERR_CODEC, format!("server decode: {s}"))
            }
        },
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
    let mut out = String::from("[");
    for (i, s) in summaries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&summary_to_json(s));
    }
    out.push(']');
    out
}

fn summary_to_json(s: &SummaryAnnouncement) -> String {
    let mut out = format!(
        "{{\"fold_kind\":{},\"source_subnet\":{},\"generation\":{},\"buckets\":[",
        s.fold_kind,
        json_string(&format!("{}", s.source_subnet)),
        s.generation,
    );
    for (i, (name, count)) in s.buckets.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"count\":{}}}",
            json_string(name),
            count
        ));
    }
    out.push_str("]}");
    out
}

/// Map a `RegistryClientError` to `(error_kind, detail_string)`.
fn classify(err: &RegistryClientError) -> (i32, String) {
    match err {
        RegistryClientError::Transport(e) => (NET_REGISTRY_ERR_TRANSPORT, format!("{e}")),
        RegistryClientError::Codec(c) => (NET_REGISTRY_ERR_CODEC, c.clone()),
        RegistryClientError::Server(rpc) => match rpc {
            RegistryRpcError::DecodeFailed(s) => {
                (NET_REGISTRY_ERR_CODEC, format!("server decode: {s}"))
            }
            RegistryRpcError::UnknownTemplate(t) => (
                NET_REGISTRY_ERR_UNKNOWN_TEMPLATE,
                format!("unknown template: {t}"),
            ),
            RegistryRpcError::DuplicateGroupName(n) => (
                NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME,
                format!("duplicate group name: {n}"),
            ),
            RegistryRpcError::SpawnRejected(d) => (
                NET_REGISTRY_ERR_SPAWN_REJECTED,
                format!("spawn rejected: {d}"),
            ),
            RegistryRpcError::SpawnNotSupported => (
                NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED,
                "daemon is read-only (no spawn handler installed)".to_string(),
            ),
        },
    }
}

fn store_error_detail(h: &RegistryClientHandle, detail: String) {
    let c = match CString::new(detail) {
        Ok(c) => c,
        Err(_) => CString::new("invalid utf-8 in error detail").unwrap_or_default(),
    };
    *h.last_error_detail.lock() = Some(c);
}

/// Encode a `[RegistryGroupSummary]` as JSON without pulling
/// in `serde_json` at this layer. The shape is documented in
/// the SDK plan's cross-language wire-contract table — we hand-
/// encode here to avoid adding a serde dep to the FFI's `net`
/// feature surface (the substrate doesn't pull serde_json
/// directly through `net`).
fn groups_to_json(groups: &[RegistryGroupSummary]) -> String {
    let mut out = String::from("[");
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&group_to_json(g));
    }
    out.push(']');
    out
}

fn group_to_json(g: &RegistryGroupSummary) -> String {
    let seed_hex: String = g.group_seed.iter().map(|b| format!("{b:02x}")).collect();
    let mut out = format!(
        "{{\"name\":{},\"group_seed_hex\":\"{seed_hex}\",\"replicas\":[",
        json_string(&g.name),
    );
    for (i, r) in g.replicas.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"generation\":{},\"healthy\":{},\"diagnostic\":{},\"placement_node_id\":{}}}",
            r.generation,
            r.healthy,
            r.diagnostic
                .as_ref()
                .map(|s| json_string(s))
                .unwrap_or_else(|| "null".to_string()),
            r.placement_node_id
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".to_string()),
        ));
    }
    out.push_str("]}");
    out
}

/// JSON-quote a string. Escapes `\` and `"`; replaces ASCII
/// control characters with `\uXXXX`. Sufficient for our
/// operator-supplied group/template-name strings + diagnostics —
/// none of which contain Unicode escapes that require full JSON
/// escaping (our usage is ASCII-dominant).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    fn json_string_escapes_control_characters() {
        assert_eq!(json_string("plain"), r#""plain""#);
        assert_eq!(json_string("with \"quote\""), r#""with \"quote\"""#);
        assert_eq!(json_string("back\\slash"), r#""back\\slash""#);
        assert_eq!(json_string("new\nline"), r#""new\nline""#);
        // ASCII bell (0x07) → .
        assert_eq!(json_string("\u{0007}"), "\"\\u0007\"");
    }

    #[test]
    fn group_to_json_includes_every_documented_field() {
        let g = RegistryGroupSummary {
            name: "alpha".into(),
            group_seed: [0xABu8; 32],
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
