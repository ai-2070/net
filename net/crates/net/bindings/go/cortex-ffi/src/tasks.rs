//! Tasks adapter FFI surface — CRUD + filter snapshot.
//!
//! Watcher exports live in this file too once T53 lands; the
//! CRUD half is sufficient to compose against from Go for any
//! workload that doesn't need live observation.

use std::ffi::{c_char, c_int};
use std::ptr;
use std::sync::Arc;

use net::adapter::net::cortex::tasks::TasksAdapter as InnerTasksAdapter;

use super::json::{TaskFilterJson, TaskJson};
use super::{
    c_str_to_str, clear_last_error_inner, into_c_string_json, parse_task_status,
    parse_tasks_order_by, runtime, set_last_error_from_adapter, set_last_error_static,
    NetCortexRedex, NetCortexTasksAdapter, NET_CORTEX_ERR_CALL_FAILED, NET_CORTEX_ERR_INVALID_ARG,
    NET_CORTEX_ERR_NULL, NET_CORTEX_OK,
};

// =====================================================================
// open / close
// =====================================================================

/// Open a tasks adapter against `redex`. Writes the new handle
/// to `*out` and returns [`NET_CORTEX_OK`]. On failure leaves
/// `*out = NULL`, populates the thread-local last-error pair,
/// and returns a typed status code.
///
/// # Safety
/// `redex` must be a pointer returned by `net_cortex_redex_new`,
/// or NULL (returns `NET_CORTEX_ERR_NULL`). `out` must point to
/// a writeable `*mut NetCortexTasksAdapter` slot, or NULL
/// (returns `NET_CORTEX_ERR_INVALID_ARG`).
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_open(
    redex: *const NetCortexRedex,
    origin_hash: u64,
    out: *mut *mut NetCortexTasksAdapter,
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
        let opened = runtime().block_on(InnerTasksAdapter::open(&redex_arc, origin_hash));
        match opened {
            Ok(adapter) => {
                clear_last_error_inner();
                let boxed = Box::into_raw(Box::new(NetCortexTasksAdapter {
                    inner: Arc::new(adapter),
                }));
                *out = boxed;
                NET_CORTEX_OK
            }
            Err(e) => {
                set_last_error_from_adapter(&e);
                NET_CORTEX_ERR_CALL_FAILED
            }
        }
    })
}

/// Rehydrate a tasks adapter from a snapshot previously captured
/// via `net_cortex_tasks_snapshot`. `state_bytes` is the opaque
/// state blob; `last_seq` is the applied-seq scalar, or `u64::MAX`
/// to indicate "no prior events" (matches `Option::None` on the
/// Rust side).
///
/// # Safety
/// `state_bytes` must be either NULL with `state_len == 0`, or a
/// valid pointer to `state_len` readable bytes the caller owns.
/// The byte buffer is copied before the call returns.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_open_from_snapshot(
    redex: *const NetCortexRedex,
    origin_hash: u64,
    state_bytes: *const u8,
    state_len: usize,
    last_seq: u64,
    out: *mut *mut NetCortexTasksAdapter,
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
        let opened = runtime().block_on(InnerTasksAdapter::open_from_snapshot(
            &redex_arc,
            origin_hash,
            &state_vec,
            last,
        ));
        match opened {
            Ok(adapter) => {
                clear_last_error_inner();
                *out = Box::into_raw(Box::new(NetCortexTasksAdapter {
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

/// Free a tasks adapter handle. Active watcher streams built
/// from this handle keep their own `Arc<TasksAdapter>` clone
/// and continue to operate until their own `_close` /
/// `_free`. No-op on NULL.
///
/// # Safety
/// `adapter` must be a pointer returned by `net_cortex_tasks_open`
/// (or `_open_from_snapshot`), or NULL. Must not have been freed
/// already.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_free(adapter: *mut NetCortexTasksAdapter) {
    if adapter.is_null() {
        return;
    }
    drop(Box::from_raw(adapter));
}

// =====================================================================
// CRUD
// =====================================================================

/// Create a new task. Writes the RedEX sequence of the append
/// to `*out_seq` and returns `NET_CORTEX_OK`.
///
/// # Safety
/// `adapter` must be valid; `title` must be a valid NUL-terminated
/// C string; `out_seq` must point to a writeable `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_create(
    adapter: *const NetCortexTasksAdapter,
    id: u64,
    title: *const c_char,
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
        let Some(title_str) = c_str_to_str(title) else {
            set_last_error_static("title is NULL or not UTF-8", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        };
        match a.inner.create(id, title_str.to_string(), now_ns) {
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

/// Rename an existing task. No-op at fold time if `id` is
/// unknown — the RedEX append still happens and `*out_seq`
/// is populated.
///
/// # Safety
/// Same as [`net_cortex_tasks_create`].
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_rename(
    adapter: *const NetCortexTasksAdapter,
    id: u64,
    new_title: *const c_char,
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
        let Some(title_str) = c_str_to_str(new_title) else {
            set_last_error_static("new_title is NULL or not UTF-8", "invalid_argument");
            return NET_CORTEX_ERR_INVALID_ARG;
        };
        match a.inner.rename(id, title_str.to_string(), now_ns) {
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

/// Mark a task completed.
///
/// # Safety
/// `adapter` must be valid; `out_seq` must point to a writeable
/// `u64`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_complete(
    adapter: *const NetCortexTasksAdapter,
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
        match a.inner.complete(id, now_ns) {
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

/// Delete a task.
///
/// # Safety
/// Same as [`net_cortex_tasks_complete`].
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_delete(
    adapter: *const NetCortexTasksAdapter,
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

/// Materialized-state cardinality. Cheap; acquires the state
/// read lock briefly.
///
/// # Safety
/// `adapter` must be valid; `out_count` must point to a writeable
/// `usize`.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_count(
    adapter: *const NetCortexTasksAdapter,
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

/// Block until every event up through `seq` has been folded
/// into state. Use as a read-after-write barrier (`seq` is the
/// value returned by a prior `_create` / `_rename` / etc).
///
/// # Safety
/// `adapter` must be valid.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_wait_for_seq(
    adapter: *const NetCortexTasksAdapter,
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
/// `net_cortex_tasks_open_from_snapshot`. Writes the state-blob
/// pointer to `*out_state` (caller frees via
/// `net_cortex_free_bytes`), the byte count to `*out_state_len`,
/// and the applied seq to `*out_last_seq` (`u64::MAX` if the
/// adapter has never observed an event).
///
/// # Safety
/// `adapter` must be valid; the three `out_*` pointers must each
/// point to a writeable slot of their respective type.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_tasks_snapshot(
    adapter: *const NetCortexTasksAdapter,
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

/// Free a buffer returned by `net_cortex_tasks_snapshot` /
/// `net_cortex_memories_snapshot`. No-op on NULL.
///
/// # Safety
/// `ptr` + `len` must be the values previously written by a
/// snapshot export. Calling with mismatched values, or freeing
/// the same buffer twice, is undefined behaviour.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
}

/// Run a filter against the current state. Writes a JSON array
/// of task objects to `*out_json` (caller frees via
/// `net_cortex_free_string`). Pass a NULL or empty `filter_json`
/// for an unfiltered listing.
///
/// Filter shape (all fields optional):
///
/// ```json
/// {
///   "status": "pending" | "completed",
///   "title_contains": "...",
///   "created_after_ns": 12345,
///   "created_before_ns": 67890,
///   "updated_after_ns": 12345,
///   "updated_before_ns": 67890,
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
pub unsafe extern "C" fn net_cortex_tasks_list(
    adapter: *const NetCortexTasksAdapter,
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
        let filter: TaskFilterJson = if filter_json.is_null() {
            TaskFilterJson::default()
        } else {
            let Some(s) = c_str_to_str(filter_json) else {
                set_last_error_static("filter_json is not UTF-8", "invalid_argument");
                return NET_CORTEX_ERR_INVALID_ARG;
            };
            if s.is_empty() {
                TaskFilterJson::default()
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
        if let Some(s) = filter.status.as_deref() {
            let Some(status) = parse_task_status(s) else {
                set_last_error_static(
                    &format!("unknown task status: {s}"),
                    "invalid_argument",
                );
                return NET_CORTEX_ERR_INVALID_ARG;
            };
            q = q.where_status(status);
        }
        if let Some(needle) = filter.title_contains {
            q = q.title_contains(needle);
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
        if let Some(o) = filter.order_by.as_deref() {
            let Some(order) = parse_tasks_order_by(o) else {
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
        let rows: Vec<TaskJson> = q.collect().into_iter().map(TaskJson::from).collect();
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
