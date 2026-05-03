//! C FFI bindings for CortEX + RedEX, behind the `cortex` feature.
//!
//! Surface targeted at the Go SDK: NetDb, TasksAdapter, MemoriesAdapter,
//! and raw RedexFile access. Everything crosses the boundary as:
//!
//! - Opaque handles (`*mut T`) freed via dedicated `_free` functions.
//! - Scalar IDs / timestamps as `u64`.
//! - Everything else as JSON strings allocated with `CString::into_raw`,
//!   freed by the caller via [`crate::ffi::net_free_string`].
//!
//! Watch / tail iterators use a cursor pattern:
//! `net_*_next(cursor, timeout_ms, out_json, out_len) -> c_int` returns
//! `0 = event delivered`, `1 = timeout`, `2 = stream ended cleanly`,
//! or a negative `NetError`. The Go layer wraps the cursor in a
//! goroutine that pumps into a channel, calling `close` on `ctx.Done()`.

use std::ffi::{c_char, c_int, CStr, CString};
use std::os::raw::c_void;
use std::ptr;
use std::sync::Arc;

use futures::stream::BoxStream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::Mutex as TokioMutex;

use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::cortex::memories::{
    MemoriesAdapter as InnerMemoriesAdapter, MemoriesFilter, MemoriesWatcher, Memory,
    OrderBy as MemoriesOrderBy,
};
use crate::adapter::net::cortex::tasks::{
    OrderBy as TasksOrderBy, Task, TaskStatus, TasksAdapter as InnerTasksAdapter, TasksFilter,
    TasksWatcher,
};
use crate::adapter::net::redex::{
    FsyncPolicy, Redex as InnerRedex, RedexError, RedexEvent, RedexFile as InnerRedexFile,
    RedexFileConfig,
};

use super::NetError;

// =========================================================================
// Extended error codes for the CortEX surface. Keep numbering below -99
// (NetError::Unknown) so they never collide with the base surface.
// =========================================================================

pub(crate) const NET_ERR_CORTEX_CLOSED: c_int = -100;
pub(crate) const NET_ERR_CORTEX_FOLD: c_int = -101;
// Exported via the Go header (`net.h` / `ErrNetDb`) for forward
// compatibility with future NetDb-layer errors; no current FFI site
// returns it, hence the allow.
#[allow(dead_code)]
pub(crate) const NET_ERR_NETDB: c_int = -102;
pub(crate) const NET_ERR_REDEX: c_int = -103;
pub(crate) const NET_ERR_TIMEOUT: c_int = 1;
pub(crate) const NET_ERR_STREAM_ENDED: c_int = 2;

// =========================================================================
// Shared utilities
// =========================================================================

/// One tokio runtime, lazily initialized, used by every CortEX /
/// RedEX FFI call. The watch / tail cursors rely on a single runtime
/// so the spawned forwarding tasks survive across cursor calls.
/// Uses `eprintln! + std::process::abort()` on builder failure
/// instead of `expect`-panic. See `ffi/mesh.rs::runtime()` for the
/// full rationale.
fn runtime() -> &'static Arc<Runtime> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => Arc::new(rt),
            Err(e) => {
                eprintln!(
                    "FATAL: cortex FFI tokio runtime build failure ({e:?}); aborting to avoid panic across the FFI boundary"
                );
                std::process::abort();
            }
        }
    })
}

/// `block_on(...)` wrapper that aborts on runtime-in-runtime
/// rather than panicking across the FFI boundary. See
/// `ffi/mesh.rs::block_on` for the full rationale; the check is the
/// same `Handle::try_current()` test, the abort message names the
/// cortex surface so the post-mortem is unambiguous.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "FATAL: cortex FFI called from inside a tokio runtime context; \
             aborting to avoid runtime-in-runtime panic across the FFI boundary"
        );
        std::process::abort();
    }
    runtime().block_on(future)
}

/// Copy a C string into an owned `String`. Returns `None` on null or
/// non-UTF-8 input.
///
/// Returns `String` (not `&str`) by design: a helper that returned a
/// borrow would need a free-choice lifetime like `Option<&'a str>`,
/// which would let callers pick `'static` and silently produce a
/// dangling reference once the caller's `*const c_char` goes out of
/// scope. Owning the copy eliminates the footgun at a small allocation
/// cost per FFI call (these paths already allocate for JSON parsing).
unsafe fn c_str_to_owned(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(|s| s.to_owned())
}

/// Serialize `value` as JSON into a C-owned string + length. On
/// success writes the pointer to `*out_ptr` and the length to
/// `*out_len` (excluding the null terminator) and returns `0`.
/// On non-success the out params are zeroed (`null`, `0`) so a
/// caller that reads them before checking the return code sees
/// "no output" rather than stale stack data. The caller must
/// free the string with `net_free_string` on success.
///
/// Null-checks `out_ptr` and `out_len` before writing through
/// them. Returns `NetError::NullPointer` so the FFI caller can
/// distinguish "I forgot output pointers" from "the operation
/// failed."
fn write_json_out<T: Serialize>(
    value: &T,
    out_ptr: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if out_ptr.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let Ok(s) = serde_json::to_string(value) else {
        // Pre-zero so the caller can rely on the contract
        // "non-zero return ⇒ out_ptr is null and out_len is 0"
        // rather than reading stale data from before the call.
        unsafe {
            *out_ptr = ptr::null_mut();
            *out_len = 0;
        }
        return NetError::Unknown.into();
    };
    let len = s.len();
    let Ok(cs) = CString::new(s) else {
        unsafe {
            *out_ptr = ptr::null_mut();
            *out_len = 0;
        }
        return NetError::Unknown.into();
    };
    unsafe {
        *out_ptr = cs.into_raw();
        *out_len = len;
    }
    0
}

/// Helper: pre-zero `*out_ptr` and `*out_len` after a null-check.
/// Call at the top of every FFI function that takes
/// `(out_json, out_len)` so subsequent error returns leave the
/// out params as `(null, 0)` rather than stale stack data. The
/// audit (#136) calls this contract "pre-zero" — every error
/// return must satisfy "out_json is null AND out_len is 0,"
/// distinct from the success contract "out_json is heap-allocated
/// and out_len is its length." Pre-fix several functions
/// returned errors without touching the out params, so callers
/// that didn't strictly check the return code dereferenced
/// stale data.
fn zero_out_json(out_ptr: *mut *mut c_char, out_len: *mut usize) {
    if !out_ptr.is_null() {
        unsafe {
            *out_ptr = ptr::null_mut();
        }
    }
    if !out_len.is_null() {
        unsafe {
            *out_len = 0;
        }
    }
}

// =========================================================================
// Compile-time Send + Sync assertions for FFI handle inner types.
//
// These handles are returned to C as `*mut HandleType` and routinely
// shared across goroutines / Python threads — the docstrings on
// every "open" / "watch" function advertise this pattern. Soundness
// rests entirely on the inner type's `Send + Sync` impl; the FFI
// layer doesn't typecheck `Send + Sync` itself, so a future refactor
// that adds a `Cell` / `RefCell` / `Rc` / `*mut` field to one of
// these types would compile cleanly while silently introducing a
// data race that any threaded caller would trigger.
//
// The `const _: fn() = ...` idiom is a compile-time trait check
// without pulling in `static_assertions` as a dep. If any inner
// type loses `Send + Sync`, this block fails to compile.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InnerRedex>();
    assert_send_sync::<InnerRedexFile>();
    assert_send_sync::<InnerTasksAdapter>();
    assert_send_sync::<InnerMemoriesAdapter>();
    assert_send_sync::<
        TokioMutex<Option<BoxStream<'static, std::result::Result<RedexEvent, RedexError>>>>,
    >();
    assert_send_sync::<TokioMutex<Option<BoxStream<'static, Vec<Task>>>>>();
    assert_send_sync::<TokioMutex<Option<BoxStream<'static, Vec<Memory>>>>>();
};

// =========================================================================
// Redex manager
// =========================================================================

pub struct RedexHandle {
    inner: Arc<InnerRedex>,
}

/// Create a new Redex manager. `persistent_dir` may be NULL for
/// heap-only. Returns a heap-allocated handle the caller must free
/// with `net_redex_free`.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_new(persistent_dir: *const c_char) -> *mut RedexHandle {
    let dir = if persistent_dir.is_null() {
        None
    } else {
        unsafe { c_str_to_owned(persistent_dir) }
    };
    let inner = match dir {
        Some(d) => InnerRedex::new().with_persistent_dir(d),
        None => InnerRedex::new(),
    };
    Box::into_raw(Box::new(RedexHandle {
        inner: Arc::new(inner),
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_free(handle: *mut RedexHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

// =========================================================================
// RedexFile
// =========================================================================

#[derive(Deserialize, Default)]
struct RedexFileConfigJson {
    #[serde(default)]
    persistent: bool,
    fsync_every_n: Option<u64>,
    fsync_interval_ms: Option<u64>,
    retention_max_events: Option<u64>,
    retention_max_bytes: Option<u64>,
    retention_max_age_ms: Option<u64>,
}

pub struct RedexFileHandle {
    inner: Arc<InnerRedexFile>,
}

/// Open (or get) a RedEX file for raw append / tail / read-range.
/// `config_json` may be NULL for defaults. Writes the file handle to
/// `*out_handle` on success. Caller frees with `net_redex_file_free`.
#[unsafe(no_mangle)]
// Field-by-field reassignment after `default()` is clearer here than
// a struct literal because several fields need conditional logic
// (fsync policy validation) that inlines awkwardly.
#[allow(clippy::field_reassign_with_default)]
pub extern "C" fn net_redex_open_file(
    redex: *mut RedexHandle,
    name: *const c_char,
    config_json: *const c_char,
    out_handle: *mut *mut RedexFileHandle,
) -> c_int {
    if redex.is_null() || name.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let redex = unsafe { &*redex };
    let Some(name_str) = (unsafe { c_str_to_owned(name) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Ok(channel) = ChannelName::new(&name_str) else {
        return NET_ERR_REDEX;
    };
    let cfg_json: RedexFileConfigJson = if config_json.is_null() {
        RedexFileConfigJson::default()
    } else {
        let Some(s) = (unsafe { c_str_to_owned(config_json) }) else {
            return NetError::InvalidUtf8.into();
        };
        match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(_) => return NetError::InvalidJson.into(),
        }
    };
    let mut cfg = RedexFileConfig::default();
    cfg.persistent = cfg_json.persistent;
    match (cfg_json.fsync_every_n, cfg_json.fsync_interval_ms) {
        (Some(_), Some(_)) | (Some(0), _) | (_, Some(0)) => return NET_ERR_REDEX,
        (Some(n), None) => cfg.fsync_policy = FsyncPolicy::EveryN(n),
        (None, Some(ms)) => {
            cfg.fsync_policy = FsyncPolicy::Interval(std::time::Duration::from_millis(ms))
        }
        _ => {}
    }
    // Reject `Some(0)` for every retention dimension at the same
    // gate that rejects fsync zeros above. Setting
    // `retention_max_events = 0` (or _bytes / _age_ms) means
    // "evict everything immediately on first append" — almost
    // certainly a config mistake intended as "no limit", which in
    // every JSON schema this crate accepts is expressed as `null`
    // / omission. Pre-fix `Some(0)` was propagated unchecked,
    // turning a config typo into silent total data loss on every
    // write.
    if matches!(cfg_json.retention_max_events, Some(0))
        || matches!(cfg_json.retention_max_bytes, Some(0))
        || matches!(cfg_json.retention_max_age_ms, Some(0))
    {
        return NET_ERR_REDEX;
    }
    cfg.retention_max_events = cfg_json.retention_max_events;
    cfg.retention_max_bytes = cfg_json.retention_max_bytes;
    if let Some(ms) = cfg_json.retention_max_age_ms {
        cfg.retention_max_age_ns = Some(ms.saturating_mul(1_000_000));
    }
    match redex.inner.open_file(&channel, cfg) {
        Ok(file) => {
            let handle = Box::new(RedexFileHandle {
                inner: Arc::new(file),
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_REDEX,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_free(handle: *mut RedexFileHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Append one payload. Writes the assigned seq to `*out_seq`.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_append(
    handle: *mut RedexFileHandle,
    payload: *const u8,
    len: usize,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || payload.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    let slice = unsafe { std::slice::from_raw_parts(payload, len) };
    match file.inner.append(slice) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_REDEX,
    }
}

#[derive(Serialize)]
struct RedexEventJson {
    seq: u64,
    /// Hex-encoded payload so JSON transport is safe for binary data.
    payload_hex: String,
    checksum: u32,
    is_inline: bool,
}

impl From<RedexEvent> for RedexEventJson {
    fn from(ev: RedexEvent) -> Self {
        RedexEventJson {
            seq: ev.entry.seq,
            payload_hex: hex_encode(&ev.payload),
            checksum: ev.entry.checksum(),
            is_inline: ev.entry.is_inline(),
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_len(handle: *mut RedexFileHandle) -> u64 {
    if handle.is_null() {
        return 0;
    }
    let file = unsafe { &*handle };
    file.inner.len() as u64
}

/// Read the half-open range `[start, end)` into a JSON array.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_read_range(
    handle: *mut RedexFileHandle,
    start: u64,
    end: u64,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    let events: Vec<RedexEventJson> = file
        .inner
        .read_range(start, end)
        .into_iter()
        .map(RedexEventJson::from)
        .collect();
    write_json_out(&events, out_json, out_len)
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_sync(handle: *mut RedexFileHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    match file.inner.sync() {
        Ok(()) => 0,
        Err(_) => NET_ERR_REDEX,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_close(handle: *mut RedexFileHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    match file.inner.close() {
        Ok(()) => 0,
        Err(_) => NET_ERR_REDEX,
    }
}

// RedEX tail cursor

pub struct RedexTailHandle {
    stream: TokioMutex<Option<BoxStream<'static, std::result::Result<RedexEvent, RedexError>>>>,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_tail(
    handle: *mut RedexFileHandle,
    from_seq: u64,
    out_cursor: *mut *mut RedexTailHandle,
) -> c_int {
    if handle.is_null() || out_cursor.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    let stream = file.inner.tail(from_seq);
    let boxed: BoxStream<'static, std::result::Result<RedexEvent, RedexError>> = stream.boxed();
    let cursor = Box::new(RedexTailHandle {
        stream: TokioMutex::new(Some(boxed)),
    });
    unsafe {
        *out_cursor = Box::into_raw(cursor);
    }
    0
}

/// Pull the next tail event. `timeout_ms == 0` blocks indefinitely.
/// Returns:
/// * `0`  — event delivered; JSON written to `*out_json` (caller frees
///   via `net_free_string`).
/// * `1`  — timeout (no event available within `timeout_ms`).
/// * `2`  — stream ended (file closed or dropped).
/// * negative — error.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_tail_next(
    cursor: *mut RedexTailHandle,
    timeout_ms: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if cursor.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    // Pre-zero out params so timeout / stream-end / error
    // returns leave the caller with `(null, 0)` rather than
    // stale stack data. The doc-comment establishes this
    // contract ("non-zero return ⇒ no JSON written"), but pre-
    // fix the function returned NET_ERR_TIMEOUT and
    // NET_ERR_STREAM_ENDED without touching the out params.
    zero_out_json(out_json, out_len);
    let cursor = unsafe { &*cursor };
    block_on(async move {
        let mut guard = cursor.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return NET_ERR_STREAM_ENDED;
        };
        let next_fut = stream.next();
        let outcome = if timeout_ms == 0 {
            next_fut.await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms as u64),
                next_fut,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => return NET_ERR_TIMEOUT,
            }
        };
        match outcome {
            Some(Ok(ev)) => {
                // Drop the cursor guard BEFORE the JSON
                // serialization so concurrent callers on the
                // same cursor don't stall waiting for our
                // write_json_out to finish. Pre-fix the
                // serialization ran inside the TokioMutex
                // critical section, so a fast event arrival on
                // a shared cursor under contention serialized
                // calls behind whichever caller was building
                // the JSON. The event is owned at this point;
                // the mutex was only protecting the stream
                // poll, not the event itself.
                drop(guard);
                let js = RedexEventJson::from(ev);
                write_json_out(&js, out_json, out_len)
            }
            Some(Err(RedexError::Closed)) | None => {
                *guard = None;
                NET_ERR_STREAM_ENDED
            }
            Some(Err(_)) => {
                *guard = None;
                NET_ERR_REDEX
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_tail_free(cursor: *mut RedexTailHandle) {
    if cursor.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(cursor));
    }
}

// =========================================================================
// Tasks adapter — standalone open. Go-side `NetDb` struct composes
// Redex + Tasks + Memories without a dedicated FFI handle.
// =========================================================================

pub struct TasksAdapterHandle {
    inner: Arc<InnerTasksAdapter>,
}

/// Open a tasks adapter against a Redex. `persistent != 0` routes
/// writes through the Redex's persistent directory (requires the
/// Redex to have been created with a `persistent_dir`).
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_adapter_open(
    redex: *mut RedexHandle,
    origin_hash: u32,
    persistent: c_int,
    out_handle: *mut *mut TasksAdapterHandle,
) -> c_int {
    if redex.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let redex = unsafe { &*redex };
    let cfg = if persistent != 0 {
        RedexFileConfig::default().with_persistent(true)
    } else {
        RedexFileConfig::default()
    };
    // `open_with_config` spawns the fold task via `tokio::spawn` and
    // needs a live reactor; run under our runtime.
    let redex_inner = redex.inner.clone();
    let result = block_on(async move {
        InnerTasksAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
    });
    match result {
        Ok(adapter) => {
            let handle = Box::new(TasksAdapterHandle {
                inner: Arc::new(adapter),
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_adapter_close(handle: *mut TasksAdapterHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    match tasks.inner.close() {
        Ok(()) => 0,
        Err(_) => NET_ERR_CORTEX_CLOSED,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_adapter_free(handle: *mut TasksAdapterHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

#[derive(Serialize)]
struct TaskJson {
    id: u64,
    title: String,
    status: &'static str,
    created_ns: u64,
    updated_ns: u64,
}

impl From<Task> for TaskJson {
    fn from(t: Task) -> Self {
        TaskJson {
            id: t.id,
            title: t.title,
            status: match t.status {
                TaskStatus::Pending => "pending",
                TaskStatus::Completed => "completed",
            },
            created_ns: t.created_ns,
            updated_ns: t.updated_ns,
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_create(
    handle: *mut TasksAdapterHandle,
    id: u64,
    title: *const c_char,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || title.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let Some(title) = (unsafe { c_str_to_owned(title) }) else {
        return NetError::InvalidUtf8.into();
    };
    match tasks.inner.create(id, title, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_rename(
    handle: *mut TasksAdapterHandle,
    id: u64,
    new_title: *const c_char,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || new_title.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let Some(nt) = (unsafe { c_str_to_owned(new_title) }) else {
        return NetError::InvalidUtf8.into();
    };
    match tasks.inner.rename(id, nt, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_complete(
    handle: *mut TasksAdapterHandle,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    match tasks.inner.complete(id, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_delete(
    handle: *mut TasksAdapterHandle,
    id: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    match tasks.inner.delete(id) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

/// Block until fold has applied every event up through `seq`. Pass
/// `timeout_ms == 0` to wait indefinitely. Returns `0` on success,
/// `1` on timeout, or negative on error.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_wait_for_seq(
    handle: *mut TasksAdapterHandle,
    seq: u64,
    timeout_ms: u32,
) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let adapter = tasks.inner.clone();
    block_on(async move {
        let fut = adapter.wait_for_seq(seq);
        if timeout_ms == 0 {
            fut.await;
            0
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms as u64), fut)
                .await
            {
                Ok(_) => 0,
                Err(_) => NET_ERR_TIMEOUT,
            }
        }
    })
}

#[derive(Deserialize, Default)]
struct TasksFilterJson {
    status: Option<String>,
    title_contains: Option<String>,
    created_after_ns: Option<u64>,
    created_before_ns: Option<u64>,
    updated_after_ns: Option<u64>,
    updated_before_ns: Option<u64>,
    order_by: Option<String>,
    limit: Option<u32>,
}

fn build_tasks_watcher(
    adapter: &InnerTasksAdapter,
    filter_json: *const c_char,
) -> Result<TasksWatcher, c_int> {
    let mut w = adapter.watch();
    if filter_json.is_null() {
        return Ok(w);
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: TasksFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    w = match f.status.as_deref() {
        Some("pending") => w.where_status(TaskStatus::Pending),
        Some("completed") => w.where_status(TaskStatus::Completed),
        Some(_) => return Err(NetError::InvalidJson.into()),
        None => w,
    };
    if let Some(s) = f.title_contains {
        w = w.title_contains(s);
    }
    if let Some(ns) = f.created_after_ns {
        w = w.created_after(ns);
    }
    if let Some(ns) = f.created_before_ns {
        w = w.created_before(ns);
    }
    if let Some(ns) = f.updated_after_ns {
        w = w.updated_after(ns);
    }
    if let Some(ns) = f.updated_before_ns {
        w = w.updated_before(ns);
    }
    if let Some(o) = f.order_by.as_deref() {
        w = match o {
            "id_asc" => w.order_by(TasksOrderBy::IdAsc),
            "id_desc" => w.order_by(TasksOrderBy::IdDesc),
            "created_asc" => w.order_by(TasksOrderBy::CreatedAsc),
            "created_desc" => w.order_by(TasksOrderBy::CreatedDesc),
            "updated_asc" => w.order_by(TasksOrderBy::UpdatedAsc),
            "updated_desc" => w.order_by(TasksOrderBy::UpdatedDesc),
            _ => return Err(NetError::InvalidJson.into()),
        };
    }
    if let Some(l) = f.limit {
        w = w.limit(l as usize);
    }
    Ok(w)
}

/// Apply JSON filter to a query-side filter (used by `list_tasks`).
#[allow(clippy::field_reassign_with_default)]
fn build_tasks_list_filter(filter_json: *const c_char) -> Result<TasksFilter, c_int> {
    if filter_json.is_null() {
        return Ok(TasksFilter::default());
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: TasksFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    let mut out = TasksFilter::default();
    out.status = match f.status.as_deref() {
        Some("pending") => Some(TaskStatus::Pending),
        Some("completed") => Some(TaskStatus::Completed),
        Some(_) => return Err(NetError::InvalidJson.into()),
        None => None,
    };
    out.title_contains = f.title_contains;
    out.created_after_ns = f.created_after_ns;
    out.created_before_ns = f.created_before_ns;
    out.updated_after_ns = f.updated_after_ns;
    out.updated_before_ns = f.updated_before_ns;
    out.order_by = match f.order_by.as_deref() {
        None => None,
        Some("id_asc") => Some(TasksOrderBy::IdAsc),
        Some("id_desc") => Some(TasksOrderBy::IdDesc),
        Some("created_asc") => Some(TasksOrderBy::CreatedAsc),
        Some("created_desc") => Some(TasksOrderBy::CreatedDesc),
        Some("updated_asc") => Some(TasksOrderBy::UpdatedAsc),
        Some("updated_desc") => Some(TasksOrderBy::UpdatedDesc),
        // Reject unknown order_by instead of silently falling back —
        // a misspelling ("createdasc") would otherwise return a
        // successful but misordered result.
        Some(_) => return Err(NetError::InvalidJson.into()),
    };
    out.limit = f.limit.map(|l| l as usize);
    Ok(out)
}

fn run_tasks_list(tasks: &InnerTasksAdapter, filter: &TasksFilter) -> Vec<Task> {
    let state = tasks.state();
    let guard = state.read();
    let mut q = guard.query();
    if let Some(s) = filter.status {
        q = q.where_status(s);
    }
    if let Some(s) = &filter.title_contains {
        q = q.title_contains(s.clone());
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
    if let Some(o) = filter.order_by {
        q = q.order_by(o);
    }
    if let Some(l) = filter.limit {
        q = q.limit(l);
    }
    q.collect()
}

/// List tasks matching `filter_json` (may be NULL). Writes a JSON
/// array of tasks to `*out_json`; caller frees via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_list(
    handle: *mut TasksAdapterHandle,
    filter_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    // Pre-zero so a filter-build error return leaves the out
    // params at (null, 0) rather than stale stack data — matches
    // the contract documented on `write_json_out`.
    zero_out_json(out_json, out_len);
    let tasks = unsafe { &*handle };
    let filter = match build_tasks_list_filter(filter_json) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let items: Vec<TaskJson> = run_tasks_list(&tasks.inner, &filter)
        .into_iter()
        .map(TaskJson::from)
        .collect();
    write_json_out(&items, out_json, out_len)
}

pub struct TasksWatchHandle {
    stream: TokioMutex<Option<BoxStream<'static, Vec<Task>>>>,
}

/// Atomic snapshot + watch. Writes:
/// * `*out_snapshot` — JSON array of tasks in the current filter result.
/// * `*out_cursor` — watch cursor; iterate via `net_tasks_watch_next`
///   and free via `net_tasks_watch_free`.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_snapshot_and_watch(
    handle: *mut TasksAdapterHandle,
    filter_json: *const c_char,
    out_snapshot: *mut *mut c_char,
    out_snapshot_len: *mut usize,
    out_cursor: *mut *mut TasksWatchHandle,
) -> c_int {
    if handle.is_null()
        || out_snapshot.is_null()
        || out_snapshot_len.is_null()
        || out_cursor.is_null()
    {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let watcher = match build_tasks_watcher(&tasks.inner, filter_json) {
        Ok(w) => w,
        Err(code) => return code,
    };
    // `watcher.stream()` spawns a forwarding task — needs a live
    // reactor.
    let adapter = tasks.inner.clone();
    let (snapshot, stream) = block_on(async move { adapter.snapshot_and_watch(watcher) });
    let snapshot_json: Vec<TaskJson> = snapshot.into_iter().map(TaskJson::from).collect();
    let code = write_json_out(&snapshot_json, out_snapshot, out_snapshot_len);
    if code != 0 {
        return code;
    }
    let handle = Box::new(TasksWatchHandle {
        stream: TokioMutex::new(Some(stream)),
    });
    unsafe {
        *out_cursor = Box::into_raw(handle);
    }
    0
}

/// Pull the next tasks-watch batch. Semantics match
/// [`net_redex_tail_next`] — `0` on event (JSON array written),
/// `1` on timeout, `2` on stream end, negative on error.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_watch_next(
    cursor: *mut TasksWatchHandle,
    timeout_ms: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if cursor.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let cursor = unsafe { &*cursor };
    block_on(async move {
        let mut guard = cursor.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return NET_ERR_STREAM_ENDED;
        };
        let next_fut = stream.next();
        let outcome = if timeout_ms == 0 {
            next_fut.await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms as u64),
                next_fut,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => return NET_ERR_TIMEOUT,
            }
        };
        match outcome {
            Some(batch) => {
                let js: Vec<TaskJson> = batch.into_iter().map(TaskJson::from).collect();
                write_json_out(&js, out_json, out_len)
            }
            None => {
                *guard = None;
                NET_ERR_STREAM_ENDED
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_watch_free(cursor: *mut TasksWatchHandle) {
    if cursor.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(cursor));
    }
}

// =========================================================================
// Memories adapter (same shape as tasks)
// =========================================================================

pub struct MemoriesAdapterHandle {
    inner: Arc<InnerMemoriesAdapter>,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_adapter_open(
    redex: *mut RedexHandle,
    origin_hash: u32,
    persistent: c_int,
    out_handle: *mut *mut MemoriesAdapterHandle,
) -> c_int {
    if redex.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let redex = unsafe { &*redex };
    let cfg = if persistent != 0 {
        RedexFileConfig::default().with_persistent(true)
    } else {
        RedexFileConfig::default()
    };
    let redex_inner = redex.inner.clone();
    let result = block_on(async move {
        InnerMemoriesAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
    });
    match result {
        Ok(adapter) => {
            let handle = Box::new(MemoriesAdapterHandle {
                inner: Arc::new(adapter),
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_adapter_close(handle: *mut MemoriesAdapterHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    match mem.inner.close() {
        Ok(()) => 0,
        Err(_) => NET_ERR_CORTEX_CLOSED,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_adapter_free(handle: *mut MemoriesAdapterHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

#[derive(Serialize)]
struct MemoryJson {
    id: u64,
    content: String,
    tags: Vec<String>,
    source: String,
    created_ns: u64,
    updated_ns: u64,
    pinned: bool,
}

impl From<Memory> for MemoryJson {
    fn from(m: Memory) -> Self {
        MemoryJson {
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

#[derive(Deserialize)]
struct MemoryStoreInput {
    id: u64,
    content: String,
    tags: Vec<String>,
    source: String,
    now_ns: u64,
}

/// Store a memory. Input is a JSON object
/// `{id, content, tags, source, now_ns}`.
#[unsafe(no_mangle)]
pub extern "C" fn net_memories_store(
    handle: *mut MemoriesAdapterHandle,
    input_json: *const c_char,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || input_json.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_owned(input_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let input: MemoryStoreInput = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    match mem.inner.store(
        input.id,
        input.content,
        input.tags,
        input.source,
        input.now_ns,
    ) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[derive(Deserialize)]
struct MemoryRetagInput {
    id: u64,
    tags: Vec<String>,
    now_ns: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_retag(
    handle: *mut MemoriesAdapterHandle,
    input_json: *const c_char,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || input_json.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_owned(input_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let input: MemoryRetagInput = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    match mem.inner.retag(input.id, input.tags, input.now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_pin(
    handle: *mut MemoriesAdapterHandle,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    match mem.inner.pin(id, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_unpin(
    handle: *mut MemoriesAdapterHandle,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    match mem.inner.unpin(id, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_delete(
    handle: *mut MemoriesAdapterHandle,
    id: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    match mem.inner.delete(id) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_wait_for_seq(
    handle: *mut MemoriesAdapterHandle,
    seq: u64,
    timeout_ms: u32,
) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let adapter = mem.inner.clone();
    block_on(async move {
        let fut = adapter.wait_for_seq(seq);
        if timeout_ms == 0 {
            fut.await;
            0
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms as u64), fut)
                .await
            {
                Ok(_) => 0,
                Err(_) => NET_ERR_TIMEOUT,
            }
        }
    })
}

#[derive(Deserialize, Default)]
struct MemoriesFilterJson {
    source: Option<String>,
    content_contains: Option<String>,
    tag: Option<String>,
    any_tag: Option<Vec<String>>,
    all_tags: Option<Vec<String>>,
    pinned: Option<bool>,
    created_after_ns: Option<u64>,
    created_before_ns: Option<u64>,
    updated_after_ns: Option<u64>,
    updated_before_ns: Option<u64>,
    order_by: Option<String>,
    limit: Option<u32>,
}

fn parse_memories_order_by(s: &str) -> Option<MemoriesOrderBy> {
    match s {
        "id_asc" => Some(MemoriesOrderBy::IdAsc),
        "id_desc" => Some(MemoriesOrderBy::IdDesc),
        "created_asc" => Some(MemoriesOrderBy::CreatedAsc),
        "created_desc" => Some(MemoriesOrderBy::CreatedDesc),
        "updated_asc" => Some(MemoriesOrderBy::UpdatedAsc),
        "updated_desc" => Some(MemoriesOrderBy::UpdatedDesc),
        _ => None,
    }
}

fn build_memories_watcher(
    adapter: &InnerMemoriesAdapter,
    filter_json: *const c_char,
) -> Result<MemoriesWatcher, c_int> {
    let mut w = adapter.watch();
    if filter_json.is_null() {
        return Ok(w);
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: MemoriesFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    if let Some(s) = f.source {
        w = w.where_source(s);
    }
    if let Some(s) = f.content_contains {
        w = w.content_contains(s);
    }
    if let Some(t) = f.tag {
        w = w.where_tag(t);
    }
    if let Some(tags) = f.any_tag {
        w = w.where_any_tag(tags);
    }
    if let Some(tags) = f.all_tags {
        w = w.where_all_tags(tags);
    }
    if let Some(p) = f.pinned {
        w = w.where_pinned(p);
    }
    if let Some(ns) = f.created_after_ns {
        w = w.created_after(ns);
    }
    if let Some(ns) = f.created_before_ns {
        w = w.created_before(ns);
    }
    if let Some(ns) = f.updated_after_ns {
        w = w.updated_after(ns);
    }
    if let Some(ns) = f.updated_before_ns {
        w = w.updated_before(ns);
    }
    if let Some(o) = f.order_by.as_deref() {
        if let Some(ob) = parse_memories_order_by(o) {
            w = w.order_by(ob);
        } else {
            return Err(NetError::InvalidJson.into());
        }
    }
    if let Some(l) = f.limit {
        w = w.limit(l as usize);
    }
    Ok(w)
}

#[allow(clippy::field_reassign_with_default)]
fn build_memories_list_filter(filter_json: *const c_char) -> Result<MemoriesFilter, c_int> {
    if filter_json.is_null() {
        return Ok(MemoriesFilter::default());
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: MemoriesFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    let mut out = MemoriesFilter::default();
    out.source = f.source;
    out.content_contains = f.content_contains;
    out.tag = f.tag;
    out.any_tag = f.any_tag;
    out.all_tags = f.all_tags;
    out.pinned = f.pinned;
    out.created_after_ns = f.created_after_ns;
    out.created_before_ns = f.created_before_ns;
    out.updated_after_ns = f.updated_after_ns;
    out.updated_before_ns = f.updated_before_ns;
    // Reject unknown order_by instead of silently falling back —
    // keep parity with build_memories_watcher above.
    out.order_by = match f.order_by.as_deref() {
        None => None,
        Some(o) => match parse_memories_order_by(o) {
            Some(ob) => Some(ob),
            None => return Err(NetError::InvalidJson.into()),
        },
    };
    out.limit = f.limit.map(|l| l as usize);
    Ok(out)
}

fn run_memories_list(mem: &InnerMemoriesAdapter, filter: &MemoriesFilter) -> Vec<Memory> {
    let state = mem.state();
    let guard = state.read();
    let mut q = guard.query();
    if let Some(s) = &filter.source {
        q = q.where_source(s.clone());
    }
    if let Some(s) = &filter.content_contains {
        q = q.content_contains(s.clone());
    }
    if let Some(t) = &filter.tag {
        q = q.where_tag(t.clone());
    }
    if let Some(tags) = &filter.any_tag {
        q = q.where_any_tag(tags.clone());
    }
    if let Some(tags) = &filter.all_tags {
        q = q.where_all_tags(tags.clone());
    }
    if let Some(p) = filter.pinned {
        q = q.where_pinned(p);
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
    if let Some(o) = filter.order_by {
        q = q.order_by(o);
    }
    if let Some(l) = filter.limit {
        q = q.limit(l);
    }
    q.collect()
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_list(
    handle: *mut MemoriesAdapterHandle,
    filter_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let filter = match build_memories_list_filter(filter_json) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let items: Vec<MemoryJson> = run_memories_list(&mem.inner, &filter)
        .into_iter()
        .map(MemoryJson::from)
        .collect();
    write_json_out(&items, out_json, out_len)
}

pub struct MemoriesWatchHandle {
    stream: TokioMutex<Option<BoxStream<'static, Vec<Memory>>>>,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_snapshot_and_watch(
    handle: *mut MemoriesAdapterHandle,
    filter_json: *const c_char,
    out_snapshot: *mut *mut c_char,
    out_snapshot_len: *mut usize,
    out_cursor: *mut *mut MemoriesWatchHandle,
) -> c_int {
    if handle.is_null()
        || out_snapshot.is_null()
        || out_snapshot_len.is_null()
        || out_cursor.is_null()
    {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let watcher = match build_memories_watcher(&mem.inner, filter_json) {
        Ok(w) => w,
        Err(code) => return code,
    };
    let adapter = mem.inner.clone();
    let (snapshot, stream) = block_on(async move { adapter.snapshot_and_watch(watcher) });
    let snapshot_json: Vec<MemoryJson> = snapshot.into_iter().map(MemoryJson::from).collect();
    let code = write_json_out(&snapshot_json, out_snapshot, out_snapshot_len);
    if code != 0 {
        return code;
    }
    let handle = Box::new(MemoriesWatchHandle {
        stream: TokioMutex::new(Some(stream)),
    });
    unsafe {
        *out_cursor = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_watch_next(
    cursor: *mut MemoriesWatchHandle,
    timeout_ms: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if cursor.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let cursor = unsafe { &*cursor };
    block_on(async move {
        let mut guard = cursor.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return NET_ERR_STREAM_ENDED;
        };
        let next_fut = stream.next();
        let outcome = if timeout_ms == 0 {
            next_fut.await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms as u64),
                next_fut,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => return NET_ERR_TIMEOUT,
            }
        };
        match outcome {
            Some(batch) => {
                let js: Vec<MemoryJson> = batch.into_iter().map(MemoryJson::from).collect();
                write_json_out(&js, out_json, out_len)
            }
            None => {
                *guard = None;
                NET_ERR_STREAM_ENDED
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_watch_free(cursor: *mut MemoriesWatchHandle) {
    if cursor.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(cursor));
    }
}

// ABI-visible no-op to force the linker to keep `c_void` happy on
// some older linkers; harmless otherwise.
#[doc(hidden)]
pub fn _ffi_cortex_keep_alive() -> *mut c_void {
    ptr::null_mut()
}

#[cfg(test)]
mod tests {
    //! Direct Rust-side coverage for the C FFI shims. The Go / Node
    //! / Python binding tests cover happy-path round-trips; these
    //! pin the corner cases that those tests don't exercise:
    //! invalid config rejection, watch-cursor lifetime, and the
    //! shared-runtime contract.

    use super::*;
    use std::ffi::CString;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;

    fn redex() -> *mut RedexHandle {
        net_redex_new(ptr::null())
    }

    fn open_file(redex: *mut RedexHandle, name: &str, cfg_json: Option<&str>) -> c_int {
        let name_c = CString::new(name).unwrap();
        let cfg_c = cfg_json.map(|s| CString::new(s).unwrap());
        let cfg_ptr = cfg_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null());
        let mut handle: *mut RedexFileHandle = ptr::null_mut();
        let rc = net_redex_open_file(redex, name_c.as_ptr(), cfg_ptr, &mut handle);
        if rc == 0 && !handle.is_null() {
            net_redex_file_free(handle);
        }
        rc
    }

    /// Conflicting `fsync_every_n` AND `fsync_interval_ms`, as well
    /// as either set to 0, must be rejected with `NET_ERR_REDEX`.
    /// Go-side configs come straight from JSON without further
    /// validation; if these slip past the FFI, the file opens with
    /// silently-default fsync behavior and durability claims become
    /// untrue.
    #[test]
    fn redex_open_file_rejects_conflicting_or_zero_fsync_config() {
        let r = redex();
        // Pre-checks: defaults and each individual setting succeed.
        assert_eq!(open_file(r, "ok-default", None), 0);
        assert_eq!(open_file(r, "ok-everyn", Some(r#"{"fsync_every_n":4}"#)), 0);
        assert_eq!(
            open_file(r, "ok-interval", Some(r#"{"fsync_interval_ms":50}"#),),
            0
        );

        // Rejected combinations. Each row tests one invalid config.
        let invalid = [
            ("both-set", r#"{"fsync_every_n":4,"fsync_interval_ms":50}"#),
            ("zero-everyn", r#"{"fsync_every_n":0}"#),
            ("zero-interval", r#"{"fsync_interval_ms":0}"#),
            ("both-zero", r#"{"fsync_every_n":0,"fsync_interval_ms":0}"#),
            (
                "everyn-set-interval-zero",
                r#"{"fsync_every_n":4,"fsync_interval_ms":0}"#,
            ),
        ];
        for (name, cfg) in invalid {
            let rc = open_file(r, name, Some(cfg));
            assert_eq!(
                rc, NET_ERR_REDEX,
                "config {name:?} ({cfg}) should be rejected with NET_ERR_REDEX (got {rc})"
            );
        }

        net_redex_free(r);
    }

    /// Pin: `net_redex_open_file` rejects `Some(0)` for any
    /// retention dimension at the same gate that rejects fsync
    /// zeros. Pre-fix the retention triple was propagated
    /// unchecked, so a config typo
    /// (`{"retention_max_events": 0}` instead of `null`) silently
    /// configured "evict everything immediately" and lost every
    /// write to the file.
    #[test]
    fn redex_open_file_rejects_zero_retention() {
        let r = redex();
        let invalid = [
            ("zero-events", r#"{"retention_max_events":0}"#),
            ("zero-bytes", r#"{"retention_max_bytes":0}"#),
            ("zero-age", r#"{"retention_max_age_ms":0}"#),
            (
                "any-zero-among-many",
                r#"{"retention_max_events":1000,"retention_max_bytes":0}"#,
            ),
        ];
        for (name, cfg) in invalid {
            let rc = open_file(r, name, Some(cfg));
            assert_eq!(
                rc, NET_ERR_REDEX,
                "config {name:?} ({cfg}) must be rejected with NET_ERR_REDEX (got {rc})"
            );
        }

        // Non-zero retention still parses.
        let valid = [
            ("non-zero-events", r#"{"retention_max_events":10000}"#),
            ("non-zero-bytes", r#"{"retention_max_bytes":1048576}"#),
            ("non-zero-age", r#"{"retention_max_age_ms":60000}"#),
            ("null-retention", r#"{"retention_max_events":null}"#),
        ];
        for (name, cfg) in valid {
            let rc = open_file(r, name, Some(cfg));
            assert_eq!(
                rc, 0,
                "valid config {name:?} ({cfg}) should succeed (got {rc})"
            );
        }

        net_redex_free(r);
    }

    /// `net_redex_open_file` with non-JSON config must return
    /// `InvalidJson`, not silently default. Pinned because the Go
    /// SDK relies on this distinction to surface a useful error.
    #[test]
    fn redex_open_file_rejects_non_json_config() {
        let r = redex();
        let name = CString::new("bad-json").unwrap();
        let cfg = CString::new("not-json {").unwrap();
        let mut handle: *mut RedexFileHandle = ptr::null_mut();
        let rc = net_redex_open_file(r, name.as_ptr(), cfg.as_ptr(), &mut handle);
        assert_eq!(rc, NetError::InvalidJson as c_int);
        assert!(handle.is_null());
        net_redex_free(r);
    }

    /// Once the underlying RedexFile is closed, an outstanding tail
    /// cursor's next `tail_next` call must observe `STREAM_ENDED`
    /// cleanly. This is the load-bearing lifetime contract for any
    /// language binding that pumps the cursor into a goroutine /
    /// task — without it, the consumer would block on a closed
    /// stream forever.
    #[test]
    fn redex_tail_cursor_observes_close_with_stream_ended() {
        let r = redex();
        let name = CString::new("tail-close").unwrap();
        let mut file: *mut RedexFileHandle = ptr::null_mut();
        assert_eq!(
            net_redex_open_file(r, name.as_ptr(), ptr::null(), &mut file),
            0
        );

        let mut cursor: *mut RedexTailHandle = ptr::null_mut();
        assert_eq!(net_redex_file_tail(file, 0, &mut cursor), 0);

        // Close the file while the cursor is live.
        assert_eq!(net_redex_file_close(file), 0);

        // Next call on the cursor must return STREAM_ENDED, not
        // block, not panic, not return an error code.
        let mut out_json: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_redex_tail_next(cursor, 1_000, &mut out_json, &mut out_len);
        assert_eq!(
            rc, NET_ERR_STREAM_ENDED,
            "expected STREAM_ENDED after file close (got {rc})"
        );
        assert!(out_json.is_null(), "no event payload should be written");

        net_redex_tail_free(cursor);
        net_redex_file_free(file);
        net_redex_free(r);
    }

    /// `runtime()` is a process-wide `OnceLock<Arc<Runtime>>`. Many
    /// FFI entry points call it on first use. We assert that
    /// concurrent first-callers from N threads all observe the
    /// same runtime instance — i.e. that `OnceLock` initialization
    /// is correctly atomic and no thread sees a half-built
    /// runtime. (`OnceLock` guarantees this; the test pins the
    /// guarantee against an accidental refactor to a non-atomic
    /// alternative.)
    #[test]
    fn runtime_first_call_returns_same_instance_under_concurrency() {
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let b = barrier.clone();
            handles.push(thread::spawn(move || {
                b.wait();
                let rt = runtime();
                Arc::as_ptr(rt) as usize
            }));
        }
        let mut ptrs: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ptrs.sort();
        ptrs.dedup();
        assert_eq!(
            ptrs.len(),
            1,
            "concurrent first-callers observed {} distinct runtimes (must be exactly 1)",
            ptrs.len()
        );
    }
}
