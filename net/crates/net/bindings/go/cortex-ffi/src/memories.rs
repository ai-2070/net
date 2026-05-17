//! Memories adapter FFI surface — CRUD + filter snapshot.
//!
//! Watcher exports land in T53. CRUD shape mirrors [`tasks`] —
//! see that module for the shared idioms around handle
//! ownership, last-error envelope, and JSON wire shapes.

use std::ffi::{c_char, c_int};
use std::ptr;
use std::sync::Arc;

use net::adapter::net::cortex::memories::MemoriesAdapter as InnerMemoriesAdapter;

use super::json::{MemoryFilterJson, MemoryJson};
use super::{
    c_str_to_str, clear_last_error_inner, into_c_string_json, parse_memories_order_by, runtime,
    set_last_error_from_adapter, set_last_error_static, NetCortexMemoriesAdapter, NetCortexRedex,
    NET_CORTEX_ERR_CALL_FAILED, NET_CORTEX_ERR_INVALID_ARG, NET_CORTEX_ERR_NULL, NET_CORTEX_OK,
};

// =====================================================================
// open / close
// =====================================================================

/// Open a memories adapter against `redex`. Shape matches
/// [`net_cortex_tasks_open`](super::net_cortex_tasks_open).
///
/// # Safety
/// See [`net_cortex_tasks_open`](super::net_cortex_tasks_open).
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_open(
    redex: *const NetCortexRedex,
    origin_hash: u64,
    out: *mut *mut NetCortexMemoriesAdapter,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error_static("out pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        *out = ptr::null_mut();
        let Some(r) = redex.as_ref() else {
            set_last_error_static("redex pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        let redex_arc = r.inner.clone();
        let opened = runtime().block_on(InnerMemoriesAdapter::open(&redex_arc, origin_hash));
        match opened {
            Ok(adapter) => {
                clear_last_error_inner();
                *out = Box::into_raw(Box::new(NetCortexMemoriesAdapter {
                    inner: Arc::new(adapter),
                }));
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Rehydrate a memories adapter from a snapshot previously
/// captured via `net_cortex_memories_snapshot`.
///
/// # Safety
/// See [`net_cortex_tasks_open_from_snapshot`](super::net_cortex_tasks_open_from_snapshot).
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_open_from_snapshot(
    redex: *const NetCortexRedex,
    origin_hash: u64,
    state_bytes: *const u8,
    state_len: usize,
    last_seq: u64,
    out: *mut *mut NetCortexMemoriesAdapter,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out.is_null() {
            set_last_error_static("out pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        *out = ptr::null_mut();
        let Some(r) = redex.as_ref() else {
            set_last_error_static("redex pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        let state_vec = if state_len == 0 {
            Vec::new()
        } else if state_bytes.is_null() {
            set_last_error_static(
                "state_bytes is NULL with non-zero state_len",
                "invalid_argument",
            );
            return NET_CORTEX_ERR_INVALID_ARG;
        } else {
            std::slice::from_raw_parts(state_bytes, state_len).to_vec()
        };
        let redex_arc = r.inner.clone();
        let last = if last_seq == u64::MAX { None } else { Some(last_seq) };
        let opened = runtime().block_on(InnerMemoriesAdapter::open_from_snapshot(
            &redex_arc,
            origin_hash,
            &state_vec,
            last,
        ));
        match opened {
            Ok(adapter) => {
                clear_last_error_inner();
                *out = Box::into_raw(Box::new(NetCortexMemoriesAdapter {
                    inner: Arc::new(adapter),
                }));
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Free a memories adapter handle. Active watcher streams keep
/// their own `Arc<MemoriesAdapter>` clone and survive. No-op on NULL.
///
/// # Safety
/// See [`net_cortex_tasks_free`](super::net_cortex_tasks_free).
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_free(adapter: *mut NetCortexMemoriesAdapter) {
    if adapter.is_null() {
        return;
    }
    drop(Box::from_raw(adapter));
}

// =====================================================================
// CRUD
// =====================================================================

/// Store a new memory. `tags_json` is a JSON array of strings —
/// pass `"[]"` or NULL for none.
///
/// # Safety
/// `adapter` must be valid; `content`, `source` must be valid
/// NUL-terminated C strings; `tags_json` must be NULL or a
/// NUL-terminated UTF-8 JSON array; `out_seq` must point to a
/// writeable `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_store(
    adapter: *const NetCortexMemoriesAdapter,
    id: u64,
    content: *const c_char,
    tags_json: *const c_char,
    source: *const c_char,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_seq.is_null() {
            set_last_error_static("out_seq is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        let Some(content_str) = c_str_to_str(content) else {
            set_last_error_static("content is NULL or not UTF-8", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        };
        let Some(source_str) = c_str_to_str(source) else {
            set_last_error_static("source is NULL or not UTF-8", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        };
        let tags: Vec<String> = if tags_json.is_null() {
            Vec::new()
        } else {
            let Some(s) = c_str_to_str(tags_json) else {
                set_last_error_static("tags_json is not UTF-8", "invalid_argument");
                return NET_CORTEX_ERR_INVALID_ARG;
            };
            if s.is_empty() {
                Vec::new()
            } else {
                match serde_json::from_str(s) {
                    Ok(v) => v,
                    Err(e) => {
                        set_last_error_static(
                            &format!("tags_json must be a JSON array of strings: {e}"),
                            "invalid_argument",
                        );
                        return NET_CORTEX_ERR_INVALID_ARG;
                    }
                }
            }
        };
        match a
            .inner
            .store(id, content_str.to_string(), tags, source_str.to_string(), now_ns)
        {
            Ok(seq) => {
                clear_last_error_inner();
                *out_seq = seq;
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Replace the tag set on an existing memory. `tags_json` shape
/// matches `net_cortex_memories_store`.
///
/// # Safety
/// `adapter` must be valid; `tags_json` must be NULL or a
/// NUL-terminated UTF-8 JSON array; `out_seq` must point to a
/// writeable `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_retag(
    adapter: *const NetCortexMemoriesAdapter,
    id: u64,
    tags_json: *const c_char,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_seq.is_null() {
            set_last_error_static("out_seq is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        let tags: Vec<String> = if tags_json.is_null() {
            Vec::new()
        } else {
            let Some(s) = c_str_to_str(tags_json) else {
                set_last_error_static("tags_json is not UTF-8", "invalid_argument");
                return NET_CORTEX_ERR_INVALID_ARG;
            };
            if s.is_empty() {
                Vec::new()
            } else {
                match serde_json::from_str(s) {
                    Ok(v) => v,
                    Err(e) => {
                        set_last_error_static(
                            &format!("tags_json must be a JSON array of strings: {e}"),
                            "invalid_argument",
                        );
                        return NET_CORTEX_ERR_INVALID_ARG;
                    }
                }
            }
        };
        match a.inner.retag(id, tags, now_ns) {
            Ok(seq) => {
                clear_last_error_inner();
                *out_seq = seq;
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Pin a memory.
///
/// # Safety
/// `adapter` must be valid; `out_seq` must point to a writeable `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_pin(
    adapter: *const NetCortexMemoriesAdapter,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_seq.is_null() {
            set_last_error_static("out_seq is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        match a.inner.pin(id, now_ns) {
            Ok(seq) => {
                clear_last_error_inner();
                *out_seq = seq;
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Unpin a memory.
///
/// # Safety
/// `adapter` must be valid; `out_seq` must point to a writeable `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_unpin(
    adapter: *const NetCortexMemoriesAdapter,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_seq.is_null() {
            set_last_error_static("out_seq is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        match a.inner.unpin(id, now_ns) {
            Ok(seq) => {
                clear_last_error_inner();
                *out_seq = seq;
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Delete a memory.
///
/// # Safety
/// `adapter` must be valid; `out_seq` must point to a writeable `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_delete(
    adapter: *const NetCortexMemoriesAdapter,
    id: u64,
    out_seq: *mut u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_seq.is_null() {
            set_last_error_static("out_seq is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        match a.inner.delete(id) {
            Ok(seq) => {
                clear_last_error_inner();
                *out_seq = seq;
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

// =====================================================================
// Read paths
// =====================================================================

/// Materialized-state cardinality.
///
/// # Safety
/// `adapter` must be valid; `out_count` must point to a writeable
/// `usize`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_count(
    adapter: *const NetCortexMemoriesAdapter,
    out_count: *mut usize,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_count.is_null() {
            set_last_error_static("out_count is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        clear_last_error_inner();
        *out_count = a.inner.count();
        NET_CORTEX_OK
    })
}

/// Block until every event up through `seq` has been folded.
///
/// # Safety
/// `adapter` must be valid.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_wait_for_seq(
    adapter: *const NetCortexMemoriesAdapter,
    seq: u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        let inner = a.inner.clone();
        runtime().block_on(async move { inner.wait_for_seq(seq).await });
        clear_last_error_inner();
        NET_CORTEX_OK
    })
}

/// Capture an opaque state snapshot for later rehydration via
/// `net_cortex_memories_open_from_snapshot`. Output shape matches
/// `net_cortex_tasks_snapshot` — caller frees the state buffer
/// with `net_cortex_free_bytes`.
///
/// # Safety
/// See [`net_cortex_tasks_snapshot`](super::net_cortex_tasks_snapshot).
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_snapshot(
    adapter: *const NetCortexMemoriesAdapter,
    out_state: *mut *mut u8,
    out_state_len: *mut usize,
    out_last_seq: *mut u64,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_state.is_null() || out_state_len.is_null() || out_last_seq.is_null() {
            set_last_error_static("out_* pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        *out_state = ptr::null_mut();
        *out_state_len = 0;
        *out_last_seq = u64::MAX;
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        match a.inner.snapshot() {
            Ok((bytes, last)) => {
                clear_last_error_inner();
                let mut boxed = bytes.into_boxed_slice();
                let ptr = boxed.as_mut_ptr();
                let len = boxed.len();
                std::mem::forget(boxed);
                *out_state = ptr;
                *out_state_len = len;
                *out_last_seq = last.unwrap_or(u64::MAX);
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Run a filter against the current state. Writes a JSON array
/// of memory objects to `*out_json` (caller frees via
/// `net_cortex_free_string`). Pass NULL or an empty string for
/// an unfiltered listing.
///
/// Filter shape (all fields optional):
///
/// ```json
/// {
///   "source": "...",
///   "content_contains": "...",
///   "tag": "single-tag",
///   "any_tag": ["a", "b"],
///   "all_tags": ["a", "b"],
///   "created_after_ns": 12345,
///   "created_before_ns": 67890,
///   "updated_after_ns": 12345,
///   "updated_before_ns": 67890,
///   "pinned": true,
///   "order_by": "id_asc" | "id_desc" | "created_asc" | "created_desc"
///             | "updated_asc" | "updated_desc",
///   "limit": 50
/// }
/// ```
///
/// # Safety
/// `adapter` must be valid; `filter_json`, if non-NULL, must be a
/// NUL-terminated UTF-8 string. `out_json` must point to a
/// writeable `*mut c_char` slot.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_memories_list(
    adapter: *const NetCortexMemoriesAdapter,
    filter_json: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_CORTEX_ERR_CALL_FAILED, {
        if out_json.is_null() {
            set_last_error_static("out_json is NULL", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        }
        *out_json = ptr::null_mut();
        let Some(a) = adapter.as_ref() else {
            set_last_error_static("adapter pointer is NULL", "invalid_argument");
            return NET_CORTEX_ERR_NULL;
        };
        let filter: MemoryFilterJson = if filter_json.is_null() {
            MemoryFilterJson::default()
        } else {
            let Some(s) = c_str_to_str(filter_json) else {
                set_last_error_static("filter_json is not UTF-8", "invalid_argument");
                return NET_CORTEX_ERR_INVALID_ARG;
            };
            if s.is_empty() {
                MemoryFilterJson::default()
            } else {
                match serde_json::from_str(s) {
                    Ok(f) => f,
                    Err(e) => {
                        set_last_error_static(
                            &format!("filter_json parse failed: {e}"),
                            "invalid_argument",
                        );
                        return NET_CORTEX_ERR_INVALID_ARG;
                    }
                }
            }
        };
        let state_arc = a.inner.state();
        let guard = state_arc.read();
        let mut q = guard.query();
        if let Some(src) = filter.source {
            q = q.where_source(src);
        }
        if let Some(needle) = filter.content_contains {
            q = q.content_contains(needle);
        }
        if let Some(t) = filter.tag {
            q = q.where_tag(t);
        }
        if let Some(ts) = filter.any_tag {
            q = q.where_any_tag(ts);
        }
        if let Some(ts) = filter.all_tags {
            q = q.where_all_tags(ts);
        }
        if let Some(ns) = filter.created_after_ns {
            q = q.created_after(ns);
        }
        if let Some(ns) = filter.created_before_ns {
            q = q.created_before(ns);
        }
        if let Some(ns) = filter.updated_after_ns {
            q = q.updated_after(ns);
        }
        if let Some(ns) = filter.updated_before_ns {
            q = q.updated_before(ns);
        }
        if let Some(p) = filter.pinned {
            q = q.where_pinned(p);
        }
        if let Some(o) = filter.order_by.as_deref() {
            let Some(order) = parse_memories_order_by(o) else {
                set_last_error_static(
                    &format!("unknown order_by: {o}"),
                    "invalid_argument",
                );
                return NET_CORTEX_ERR_INVALID_ARG;
            };
            q = q.order_by(order);
        }
        if let Some(n) = filter.limit {
            q = q.limit(n as usize);
        }
        let rows: Vec<MemoryJson> = q.collect().into_iter().map(MemoryJson::from).collect();
        drop(guard);
        let json_ptr = into_c_string_json(&rows);
        if json_ptr.is_null() {
            return NET_CORTEX_ERR_CALL_FAILED;
        }
        clear_last_error_inner();
        *out_json = json_ptr;
        NET_CORTEX_OK
    })
}
