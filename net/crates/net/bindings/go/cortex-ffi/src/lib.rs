//! C FFI for the CortEX (NetDB) tasks + memories adapters.
//!
//! # Scope
//!
//! Mirrors the surface the Node (`bindings/node/src/cortex.rs`) +
//! Python (`bindings/python/src/cortex.rs`) bindings expose:
//!
//! - [`Redex`](::net::adapter::net::redex::Redex) handle factory.
//! - `TasksAdapter` + `MemoriesAdapter` CRUD + filter snapshot.
//! - Live watchers (`watch` + `snapshot_and_watch`) returning a
//!   pollable stream handle.
//!
//! # Symbol naming
//!
//! `net_cortex_<noun>_<verb>` mirroring the existing `ffi::cortex`
//! convention. Symbols are unconditionally exported when the
//! crate is built with the substrate's `cortex` feature (the
//! Cargo.toml enables it as a default dep feature).
//!
//! # Error codes
//!
//! Functions return `i32` status codes — see the
//! `NET_CORTEX_OK` / `NET_CORTEX_ERR_*` constants below for the
//! table. The detail message + stable kind discriminator are
//! fetched via `net_cortex_last_error_message()` +
//! `net_cortex_last_error_kind()` on the calling thread; both
//! pointers stay valid until the next FFI call on the same
//! thread touches the thread-local.
//!
//! # Stream semantics
//!
//! Watcher streams emit one JSON-encoded row batch per call to
//! `_stream_next`. The first emission is the current filter
//! result; subsequent emissions arrive when a fold tick produces
//! a different filter result (deduplicated by `Vec` equality).
//! Pass `timeout_ms == 0` for an unbounded wait; otherwise the
//! call returns `NET_CORTEX_OK` with a NULL `*out` when the
//! timeout elapses without an item — distinguishing "no data
//! yet" from "stream ended" (`NET_CORTEX_ERR_END_OF_STREAM`).
//!
//! # Handle ownership
//!
//! Every `*mut`-returning factory transfers ownership to the
//! caller. Pair each handle with its matching `_free` /
//! `_close`. `_free` on NULL is a no-op; calling `_free` twice
//! is undefined behaviour (use-after-free).

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;
use std::sync::{Arc, OnceLock};

use futures::stream::BoxStream;
use tokio::runtime::Runtime;
use tokio::sync::Mutex as TokioMutex;

use net::adapter::net::cortex::memories::{
    MemoriesAdapter as InnerMemoriesAdapter, Memory as InnerMemory,
    OrderBy as InnerMemoriesOrderBy,
};
use net::adapter::net::cortex::tasks::{
    OrderBy as InnerTasksOrderBy, Task as InnerTask, TaskStatus as InnerTaskStatus,
    TasksAdapter as InnerTasksAdapter,
};
use net::adapter::net::cortex::CortexAdapterError;
use net::adapter::net::redex::Redex as InnerRedex;

// =====================================================================
// Status codes
// =====================================================================

/// Status code: function returned successfully.
pub const NET_CORTEX_OK: c_int = 0;
/// Status code: caller passed a NULL pointer.
pub const NET_CORTEX_ERR_NULL: c_int = -1;
/// Status code: caller passed an otherwise invalid argument
/// (out-of-range, malformed JSON, NUL byte in a C string).
pub const NET_CORTEX_ERR_INVALID_ARG: c_int = -2;
/// Status code: substrate call failed (CortEX adapter error,
/// JSON serialization failure, panic across the boundary).
pub const NET_CORTEX_ERR_CALL_FAILED: c_int = -3;
/// Status code: caller used a handle whose lifecycle has already
/// completed (adapter closed, stream freed).
pub const NET_CORTEX_ERR_ALREADY_SHUTDOWN: c_int = -4;
/// Status code: stream has ended cleanly; no further `_next`
/// calls will produce items.
pub const NET_CORTEX_ERR_END_OF_STREAM: c_int = -5;

// =====================================================================
// Thread-local last-error reporting
// =====================================================================
//
// FFI errors flow through a per-thread "last error" pair (message
// + kind). Callers retrieve both via the `_last_error_*` getters;
// pointers stay valid until the next FFI call on the same thread
// touches the thread-local. Both panics from the async closure and
// `CortexAdapterError`s from the adapter populate this.

thread_local! {
    static LAST_ERROR_MESSAGE: RefCell<Option<CString>> = const { RefCell::new(None) };
    static LAST_ERROR_KIND: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error_static(message: &str, kind: &str) {
    let msg = CString::new(message).ok();
    let kind = CString::new(kind).ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn set_last_error_from_adapter(err: &CortexAdapterError) {
    let msg = CString::new(err.to_string()).ok();
    let kind = CString::new(cortex_error_kind(err)).ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn set_last_error_from_panic(payload: &(dyn std::any::Any + Send)) {
    let detail = if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic across FFI boundary".to_string()
    };
    let msg = CString::new(format!("runtime panic: {detail}")).ok();
    let kind = CString::new("runtime_panic").ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn clear_last_error_inner() {
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = None);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = None);
}

/// Map a [`CortexAdapterError`] to a stable string discriminator.
/// Kept narrow on purpose: the adapter's error type has a handful
/// of variants and we don't want the kind string to leak inner
/// transport-layer detail.
fn cortex_error_kind(err: &CortexAdapterError) -> &'static str {
    match err {
        CortexAdapterError::Redex(_) => "redex_error",
        CortexAdapterError::Closed => "closed",
        CortexAdapterError::FoldStopped { .. } => "fold_stopped",
        CortexAdapterError::InvalidStartPosition(_) => "invalid_start_position",
    }
}

/// Return the most recent error message recorded on this thread,
/// or NULL if there is none. The pointer is valid until the next
/// FFI call on the same thread touches the thread-local. Callers
/// must not free the returned pointer.
#[no_mangle]
pub extern "C" fn net_cortex_last_error_message() -> *const c_char {
    LAST_ERROR_MESSAGE.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Return the most recent error kind recorded on this thread, or
/// NULL if there is none. Same lifetime rules as
/// `net_cortex_last_error_message`.
#[no_mangle]
pub extern "C" fn net_cortex_last_error_kind() -> *const c_char {
    LAST_ERROR_KIND.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Clear the thread-local last-error state.
#[no_mangle]
pub extern "C" fn net_cortex_clear_last_error() {
    clear_last_error_inner();
}

/// Free a heap-allocated `*mut c_char` returned from any
/// `_next` / `_snapshot_json` / list export. No-op on NULL.
///
/// # Safety
/// `ptr` must be a pointer previously returned by this crate (a
/// CString-into-raw'd buffer), or NULL.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    drop(CString::from_raw(ptr));
}

// =====================================================================
// FFI guard + helper macros
// =====================================================================
//
// `ffi_guard!` traps panics that escape an `extern "C"` body, records
// the panic message as the thread-local last-error pair with kind
// `"runtime_panic"`, and returns `$default`. Unwinding across the C
// boundary is undefined behaviour — every entry point wraps its body
// in this macro.

macro_rules! ffi_guard {
    ($default:expr, $body:block) => {{
        match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| $body)) {
            Ok(v) => v,
            Err(payload) => {
                $crate::set_last_error_from_panic(&*payload);
                $default
            }
        }
    }};
}

// =====================================================================
// Tokio runtime singleton
// =====================================================================
//
// CortEX adapter `open` calls are async; watcher streams' `next`
// awaits a `BoxStream`. A single multi-thread runtime serves every
// FFI call. Built lazily on first use so the cdylib's
// `_init_array` / DllMain doesn't pay the cost when an embedder
// only needs the type definitions.

fn runtime() -> &'static Arc<Runtime> {
    static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Arc::new(
            Runtime::new().expect("net-cortex-ffi: failed to start tokio multi-thread runtime"),
        )
    })
}

// =====================================================================
// Redex handle
// =====================================================================
//
// Both adapters share one `Arc<Redex>`. Exposed as an opaque
// handle so a single Redex can back multiple adapter handles
// (the common shape for any program that wants both tasks +
// memories on one local mesh node).

/// Opaque Redex manager handle. Wraps `Arc<Redex>` so adapters
/// built from this handle keep the manager alive even if the
/// caller frees their pointer first.
pub struct NetCortexRedex {
    inner: Arc<InnerRedex>,
}

/// Allocate a fresh in-memory Redex manager (no auth, no
/// persistent directory). Free with `net_cortex_redex_free`.
///
/// # Safety
/// Allocates a heap object; caller owns the returned pointer.
#[no_mangle]
pub extern "C" fn net_cortex_redex_new() -> *mut NetCortexRedex {
    ffi_guard!(ptr::null_mut(), {
        clear_last_error_inner();
        Box::into_raw(Box::new(NetCortexRedex {
            inner: Arc::new(InnerRedex::new()),
        }))
    })
}

/// Free a Redex handle. Adapters built from this handle keep
/// their own `Arc<Redex>` clone and stay usable. No-op on NULL.
///
/// # Safety
/// `handle` must be a pointer returned by `net_cortex_redex_new`,
/// or NULL. Must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn net_cortex_redex_free(handle: *mut NetCortexRedex) {
    if handle.is_null() {
        return;
    }
    drop(Box::from_raw(handle));
}

// =====================================================================
// Stream + adapter handle types
// =====================================================================
//
// Adapter handles wrap an `Arc<Inner...Adapter>` so close/drop on
// one thread doesn't pull state out from under a watcher stream
// holding a clone. Stream handles wrap the boxed stream behind
// a `TokioMutex<Option<...>>` so `_close` can drop the inner
// stream without racing a `_next` on another thread.

/// Opaque CortEX tasks adapter handle.
pub struct NetCortexTasksAdapter {
    inner: Arc<InnerTasksAdapter>,
}

/// Opaque CortEX memories adapter handle.
pub struct NetCortexMemoriesAdapter {
    inner: Arc<InnerMemoriesAdapter>,
}

/// Opaque tasks-watcher stream handle. The inner stream is
/// option-wrapped so `_close` can drop it without racing a
/// `_next` running on another thread (the `TokioMutex` is
/// only awaited inside `_next`).
pub struct NetCortexTasksStream {
    #[allow(dead_code)] // populated by T53 (watcher exports)
    pub(crate) stream: TokioMutex<Option<BoxStream<'static, Vec<InnerTask>>>>,
}

/// Opaque memories-watcher stream handle. See [`NetCortexTasksStream`].
pub struct NetCortexMemoriesStream {
    #[allow(dead_code)] // populated by T53 (watcher exports)
    pub(crate) stream: TokioMutex<Option<BoxStream<'static, Vec<InnerMemory>>>>,
}

// =====================================================================
// JSON wire shapes
// =====================================================================
//
// The FFI surfaces three JSON payload shapes:
// - `TaskJson` / `MemoryJson` — one row.
// - `TaskFilterJson` / `MemoryFilterJson` — `list` filter spec
//   (one-shot, infrequent).
// - The stream `_next` output is a JSON array of row objects.
//
// Watcher filter setters use struct-of-primitives FFI fns
// instead of JSON because they're called many times per
// observation and serde at every step would add useless
// overhead. The shapes match the Node binding's `TaskFilter` /
// `MemoryFilter` so SDK code translates one-to-one across
// languages.

mod json {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub(super) struct TaskJson {
        pub id: u64,
        pub title: String,
        pub status: &'static str,
        pub created_ns: u64,
        pub updated_ns: u64,
    }

    impl From<InnerTask> for TaskJson {
        fn from(t: InnerTask) -> Self {
            Self {
                id: t.id,
                title: t.title,
                status: task_status_to_str(t.status),
                created_ns: t.created_ns,
                updated_ns: t.updated_ns,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct MemoryJson {
        pub id: u64,
        pub content: String,
        pub tags: Vec<String>,
        pub source: String,
        pub created_ns: u64,
        pub updated_ns: u64,
        pub pinned: bool,
    }

    impl From<InnerMemory> for MemoryJson {
        fn from(m: InnerMemory) -> Self {
            Self {
                id: m.id,
                content: m.content,
                tags: m.tags,
                source: m.source,
                created_ns: m.created_ns,
                updated_ns: m.updated_ns,
                pinned: m.pinned,
            }
        }
    }

    /// `list_tasks` filter spec. All fields optional — omitted
    /// fields don't constrain the result.
    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub(super) struct TaskFilterJson {
        pub status: Option<String>,
        pub title_contains: Option<String>,
        pub created_after_ns: Option<u64>,
        pub created_before_ns: Option<u64>,
        pub updated_after_ns: Option<u64>,
        pub updated_before_ns: Option<u64>,
        pub order_by: Option<String>,
        pub limit: Option<u32>,
    }

    /// `list_memories` filter spec. Mirrors the Node binding's
    /// shape (tag predicates: `tag`, `any_tag`, `all_tags`).
    #[derive(Deserialize, Default)]
    #[serde(default)]
    pub(super) struct MemoryFilterJson {
        pub source: Option<String>,
        pub content_contains: Option<String>,
        pub tag: Option<String>,
        pub any_tag: Option<Vec<String>>,
        pub all_tags: Option<Vec<String>>,
        pub created_after_ns: Option<u64>,
        pub created_before_ns: Option<u64>,
        pub updated_after_ns: Option<u64>,
        pub updated_before_ns: Option<u64>,
        pub pinned: Option<bool>,
        pub order_by: Option<String>,
        pub limit: Option<u32>,
    }

    fn task_status_to_str(s: InnerTaskStatus) -> &'static str {
        match s {
            InnerTaskStatus::Pending => "pending",
            InnerTaskStatus::Completed => "completed",
        }
    }
}

// =====================================================================
// Enum parsing helpers
// =====================================================================
//
// Used by both the JSON filter parser and the watcher set_* fns.
// Bad strings populate the last-error pair and surface as
// `NET_CORTEX_ERR_INVALID_ARG` at the call site.

fn parse_task_status(s: &str) -> Option<InnerTaskStatus> {
    match s {
        "pending" => Some(InnerTaskStatus::Pending),
        "completed" => Some(InnerTaskStatus::Completed),
        _ => None,
    }
}

fn parse_tasks_order_by(s: &str) -> Option<InnerTasksOrderBy> {
    match s {
        "id_asc" => Some(InnerTasksOrderBy::IdAsc),
        "id_desc" => Some(InnerTasksOrderBy::IdDesc),
        "created_asc" => Some(InnerTasksOrderBy::CreatedAsc),
        "created_desc" => Some(InnerTasksOrderBy::CreatedDesc),
        "updated_asc" => Some(InnerTasksOrderBy::UpdatedAsc),
        "updated_desc" => Some(InnerTasksOrderBy::UpdatedDesc),
        _ => None,
    }
}

fn parse_memories_order_by(s: &str) -> Option<InnerMemoriesOrderBy> {
    match s {
        "id_asc" => Some(InnerMemoriesOrderBy::IdAsc),
        "id_desc" => Some(InnerMemoriesOrderBy::IdDesc),
        "created_asc" => Some(InnerMemoriesOrderBy::CreatedAsc),
        "created_desc" => Some(InnerMemoriesOrderBy::CreatedDesc),
        "updated_asc" => Some(InnerMemoriesOrderBy::UpdatedAsc),
        "updated_desc" => Some(InnerMemoriesOrderBy::UpdatedDesc),
        _ => None,
    }
}

/// Convert a raw C string into a borrowed `&str`. Returns
/// `None` on NULL or non-UTF-8 input. The returned reference is
/// only valid for the lifetime of the caller's pointer.
///
/// # Safety
/// `ptr` must either be NULL or point to a NUL-terminated C
/// string the caller owns.
unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok()
}

/// Serialize an arbitrary `Serialize` value to a heap-allocated
/// C string. The caller owns the returned pointer and must free
/// with `net_cortex_free_string`. Returns NULL + populates the
/// last-error pair on serialization failure or NUL-byte content.
fn into_c_string_json<T: serde::Serialize>(value: &T) -> *mut c_char {
    match serde_json::to_string(value) {
        Ok(s) => match CString::new(s) {
            Ok(c) => c.into_raw(),
            Err(_) => {
                set_last_error_static(
                    "serialized JSON contained a NUL byte",
                    "json_serialize_failed",
                );
                ptr::null_mut()
            }
        },
        Err(e) => {
            set_last_error_static(
                &format!("json serialize failed: {e}"),
                "json_serialize_failed",
            );
            ptr::null_mut()
        }
    }
}

// Submodules carry the per-adapter surfaces. Both are unconditionally
// compiled — the Cargo.toml's substrate `cortex` feature is the gate.
mod memories;
mod tasks;

// Re-export every `#[no_mangle] extern "C" fn` so they're visible
// at the cdylib's symbol root.
pub use memories::*;
pub use tasks::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_are_distinct() {
        // Stable consumer contract — bind these values into
        // your Go / C constants and rely on them.
        assert_eq!(NET_CORTEX_OK, 0);
        assert_eq!(NET_CORTEX_ERR_NULL, -1);
        assert_eq!(NET_CORTEX_ERR_INVALID_ARG, -2);
        assert_eq!(NET_CORTEX_ERR_CALL_FAILED, -3);
        assert_eq!(NET_CORTEX_ERR_ALREADY_SHUTDOWN, -4);
        assert_eq!(NET_CORTEX_ERR_END_OF_STREAM, -5);
    }

    #[test]
    fn redex_handle_new_then_free() {
        let handle = net_cortex_redex_new();
        assert!(!handle.is_null());
        unsafe { net_cortex_redex_free(handle) };
    }

    #[test]
    fn redex_free_on_null_is_noop() {
        unsafe { net_cortex_redex_free(ptr::null_mut()) };
    }

    #[test]
    fn enum_parsers_round_trip() {
        assert!(matches!(
            parse_task_status("pending"),
            Some(InnerTaskStatus::Pending)
        ));
        assert!(matches!(
            parse_task_status("completed"),
            Some(InnerTaskStatus::Completed)
        ));
        assert!(parse_task_status("bogus").is_none());
        assert!(parse_tasks_order_by("id_asc").is_some());
        assert!(parse_tasks_order_by("bogus").is_none());
        assert!(parse_memories_order_by("created_desc").is_some());
    }

    #[test]
    fn last_error_round_trip() {
        clear_last_error_inner();
        set_last_error_static("hello", "test_kind");
        let msg = net_cortex_last_error_message();
        assert!(!msg.is_null());
        let kind = net_cortex_last_error_kind();
        assert!(!kind.is_null());
        // The pointers stay valid until the next call mutates
        // the thread-local; reading them back here is safe.
        let msg_str = unsafe { CStr::from_ptr(msg) }.to_str().unwrap();
        let kind_str = unsafe { CStr::from_ptr(kind) }.to_str().unwrap();
        assert_eq!(msg_str, "hello");
        assert_eq!(kind_str, "test_kind");
        net_cortex_clear_last_error();
        assert!(net_cortex_last_error_message().is_null());
    }
}
