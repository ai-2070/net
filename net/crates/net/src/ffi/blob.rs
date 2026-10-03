//! C FFI for Dataforts Phase 3 blob storage.
//!
//! Exposes:
//!
//! - `net_blob_register_fs_adapter` / `net_blob_unregister_adapter` —
//!   registry lifecycle for a Rust-backed FileSystemAdapter.
//! - `net_blob_adapter_registered` — probe.
//! - `net_blob_publish` — content → encoded BlobRef bytes (caller
//!   frees).
//! - `net_blob_resolve` — payload bytes → resolved content (caller
//!   frees).
//!
//! Returned buffers are heap-owned by Rust and MUST be freed via
//! `net_blob_free_buffer`. Errors use the same `c_int` discipline
//! as the rest of the FFI surface; the blob-specific extended
//! codes are in the `-110..` range to stay below the cortex
//! surface's `-100..-109` band.
//!
//! # Safety
//!
//! Every entry point is `unsafe extern "C"` and inherits the same
//! caller-side contract as the rest of the FFI surface (see
//! `ffi/mod.rs` and `include/net.h`): valid + aligned pointers,
//! opaque handles produced by this crate's matching constructor
//! (`Box::into_raw` inside the FFI surface — foreign-allocated
//! pointers will UB when consumed by `Box::from_raw`),
//! NUL-terminated UTF-8 strings, accurate buffer/length pairs,
//! out-parameter pointers writable for the call's lifetime, and
//! Rust-allocated buffers freed via `net_blob_free_buffer`.
#![allow(clippy::missing_safety_doc)]
#![expect(
    clippy::undocumented_unsafe_blocks,
    reason = "module-wide FFI safety contract documented in the # Safety preamble above"
)]
#![expect(
    clippy::multiple_unsafe_ops_per_block,
    reason = "FFI entry points routinely deref + write to multiple out-parameter fields under the same caller contract"
)]

use std::ffi::{c_char, c_int, CStr};
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;

use tokio::runtime::Runtime;

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
use crate::adapter::net::behavior::TopologyScope;
use crate::adapter::net::dataforts::{
    global_blob_adapter_registry, publish_blob, resolve_payload, BlobAdapter,
    BlobError as InnerBlobError, FileSystemAdapter,
};
// `InnerBlobRef` is only decoded inside the `MeshBlobAdapter`
// store/fetch/exists entry points, which themselves require the
// `dataforts + netdb + redex-disk` triple. Without the triple,
// the import is unused and `-D warnings` fails CI.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
use crate::adapter::net::dataforts::{
    BlobRef as InnerBlobRef, MeshBlobAdapter as InnerMeshBlobAdapter,
    OverflowConfig as InnerOverflowConfig,
};
// The mint. `net_mesh_blob_adapter_store` needs a *pre-encoded* ref, so
// without this a C or Go producer can fetch a blob it cannot create.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
use crate::adapter::net::dataforts::publish_blob_ref;

use super::NetError;

/// BlobRef decode failed (truncated / unsupported version).
pub const NET_ERR_BLOB_DECODE: c_int = -110;
/// Adapter registry: adapter id already registered.
pub const NET_ERR_BLOB_DUPLICATE_ID: c_int = -111;
/// Adapter registry: adapter id not found.
pub const NET_ERR_BLOB_NOT_REGISTERED: c_int = -112;
/// Adapter returned `NotFound` for the requested URI.
pub const NET_ERR_BLOB_NOT_FOUND: c_int = -113;
/// Substrate-side hash verification rejected the fetched bytes.
pub const NET_ERR_BLOB_HASH_MISMATCH: c_int = -114;
/// Adapter returned a non-classifiable backend error.
pub const NET_ERR_BLOB_BACKEND: c_int = -115;
/// `BlobRef::UnsupportedScheme` — used for both "unknown URI scheme"
/// and "channel pointing at an unregistered adapter id".
pub const NET_ERR_BLOB_UNSUPPORTED_SCHEME: c_int = -116;
/// Channel has no `blob_adapter_id` configured.
pub const NET_ERR_BLOB_ADAPTER_NOT_CONFIGURED: c_int = -118;
/// Configured `blob_adapter_id` is not in the registry.
pub const NET_ERR_BLOB_ADAPTER_NOT_REGISTERED: c_int = -119;
/// Panic surfaced from inside a user-installed adapter callback
/// (or anywhere on the FFI body). The substrate catches it with
/// `catch_unwind` and reports this code rather than unwinding
/// across the FFI boundary (which is undefined behaviour for the
/// C / cgo / Python callers).
pub const NET_ERR_BLOB_PANIC: c_int = -117;
/// Auth gate rejected the blob op: AuthGuard ACL miss, or no
/// guard configured for an op that requires one. Distinct from
/// `NET_ERR_BLOB_BACKEND` so bindings can route 401-style hits
/// without parsing the error string.
pub const NET_ERR_BLOB_UNAUTHORIZED: c_int = -120;
/// An argument the v0.3 tree / range / repair entry points refuse before
/// touching the adapter: a reversed, over-cap or out-of-extent range, an
/// unknown encoding kind, Reed-Solomon parameters out of range, or a cache
/// capacity this target cannot address. Numbered outside the blob band
/// (`-121..=-128` and `-130..` are other modules' codes, and `-120` is
/// already shared with `NET_ERR_IDENTITY`) from an inventory of every
/// `src/ffi` code, so it collides with nothing.
pub const NET_ERR_BLOB_INVALID_ARGUMENT: c_int = -150;

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
                eprintln!("FATAL: blob FFI tokio runtime build failure ({e:?}); aborting");
                std::process::abort();
            }
        }
    })
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!("FATAL: blob FFI called from inside a tokio runtime context; aborting");
        std::process::abort();
    }
    runtime().block_on(future)
}

unsafe fn c_str_to_owned(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(|s| s.to_owned())
}

fn err_to_code(e: &InnerBlobError) -> c_int {
    match e {
        InnerBlobError::HashMismatch { .. } => NET_ERR_BLOB_HASH_MISMATCH,
        InnerBlobError::NotFound(_) => NET_ERR_BLOB_NOT_FOUND,
        InnerBlobError::Backend(_) => NET_ERR_BLOB_BACKEND,
        InnerBlobError::Cancelled => NET_ERR_BLOB_BACKEND,
        InnerBlobError::UnsupportedScheme(_) => NET_ERR_BLOB_UNSUPPORTED_SCHEME,
        InnerBlobError::UnsupportedVersion(_) => NET_ERR_BLOB_DECODE,
        InnerBlobError::Decode(_) => NET_ERR_BLOB_DECODE,
        InnerBlobError::AdapterNotConfigured => NET_ERR_BLOB_ADAPTER_NOT_CONFIGURED,
        InnerBlobError::AdapterNotRegistered(_) => NET_ERR_BLOB_ADAPTER_NOT_REGISTERED,
        InnerBlobError::Unauthorized(_) => NET_ERR_BLOB_UNAUTHORIZED,
        // `ShortChunk` is a size disagreement (backend truncated
        // the chunk); route through `NET_ERR_BLOB_BACKEND` rather
        // than `NET_ERR_BLOB_HASH_MISMATCH` so retry logic that
        // distinguishes truncation from content divergence keeps
        // the existing classifier intact. A dedicated code can be
        // added later when a binding consumer needs to fork on the
        // distinction at the FFI surface.
        InnerBlobError::ShortChunk { .. } => NET_ERR_BLOB_BACKEND,
    }
}

/// Register a filesystem-backed BlobAdapter under `adapter_id`.
/// Both `adapter_id` and `root` are null-terminated UTF-8 strings.
/// Returns `0` on success, `NET_ERR_BLOB_DUPLICATE_ID` if the id
/// already exists, or `NetError::InvalidUtf8` / `NullPointer` for
/// malformed input.
///
/// # Safety
/// `adapter_id` and `root` must each point to a valid null-terminated
/// UTF-8 byte sequence and remain valid for the duration of this
/// call. Either may be null, in which case the function returns
/// `NetError::InvalidUtf8`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_register_fs_adapter(
    adapter_id: *const c_char,
    root: *const c_char,
) -> c_int {
    let id = match c_str_to_owned(adapter_id) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let root = match c_str_to_owned(root) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let adapter: Arc<dyn BlobAdapter> =
        Arc::new(FileSystemAdapter::new(id.clone(), PathBuf::from(root)));
    match global_blob_adapter_registry().register(adapter) {
        Ok(()) => 0,
        Err(_) => NET_ERR_BLOB_DUPLICATE_ID,
    }
}

/// Remove an adapter registration. Returns `1` if an adapter was
/// removed, `0` if no adapter was registered under that id.
///
/// # Safety
/// `adapter_id` must point to a valid null-terminated UTF-8 byte
/// sequence and remain valid for the call. Null returns
/// `NetError::InvalidUtf8`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_unregister_adapter(adapter_id: *const c_char) -> c_int {
    let id = match c_str_to_owned(adapter_id) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    if global_blob_adapter_registry().unregister(&id).is_some() {
        1
    } else {
        0
    }
}

/// Returns `1` if `adapter_id` resolves to a registered adapter,
/// `0` otherwise.
///
/// # Safety
/// `adapter_id` must point to a valid null-terminated UTF-8 byte
/// sequence and remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_adapter_registered(adapter_id: *const c_char) -> c_int {
    let id = match c_str_to_owned(adapter_id) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    if global_blob_adapter_registry().get(&id).is_some() {
        1
    } else {
        0
    }
}

/// Publish `data` (len `data_len` bytes) to the adapter registered
/// under `adapter_id`. On success returns `0` and writes a freshly-
/// allocated Rust-owned buffer pointer into `*out_payload` /
/// `*out_payload_len` containing the wire-encoded BlobRef. Caller
/// MUST free via [`net_blob_free_buffer`].
///
/// On error returns a negative code and leaves the out-params at
/// `(null, 0)`.
///
/// # Safety
/// - `adapter_id` and `uri` must each point to a valid null-
///   terminated UTF-8 byte sequence.
/// - `data` must point to a readable region of at least `data_len`
///   bytes (or be null when `data_len == 0`).
/// - `out_payload` and `out_payload_len` must each point to writable
///   `*mut u8` / `usize` storage; the function writes through both.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_publish(
    adapter_id: *const c_char,
    uri: *const c_char,
    data: *const u8,
    data_len: usize,
    out_payload: *mut *mut u8,
    out_payload_len: *mut usize,
) -> c_int {
    if out_payload.is_null() || out_payload_len.is_null() {
        return NetError::NullPointer.into();
    }
    *out_payload = ptr::null_mut();
    *out_payload_len = 0;

    let id = match c_str_to_owned(adapter_id) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let uri = match c_str_to_owned(uri) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    if data.is_null() && data_len > 0 {
        return NetError::NullPointer.into();
    }
    // `slice::from_raw_parts` requires `len <= isize::MAX`.
    if data_len > isize::MAX as usize {
        return NetError::InvalidJson.into();
    }
    let data_slice = if data_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(data, data_len)
    };

    let adapter = match global_blob_adapter_registry().get(&id) {
        Some(a) => a,
        None => return NET_ERR_BLOB_NOT_REGISTERED,
    };
    // Wrap the body in catch_unwind so a panic in a user-
    // installed adapter callback (or anywhere downstream) cannot
    // unwind across the FFI boundary into the C / cgo / Python
    // caller — that's undefined behaviour.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(async move { publish_blob(adapter.as_ref(), uri, data_slice).await })
    }));
    let bytes = match result {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return err_to_code(&e),
        Err(_) => return NET_ERR_BLOB_PANIC,
    };

    write_bytes_out(&bytes, out_payload, out_payload_len)
}

/// Resolve a payload to its content bytes. Inline payloads round-
/// trip; encoded-BlobRef payloads fetch + verify through the
/// adapter registered under `adapter_id`.
///
/// Returns `0` and writes a freshly-allocated Rust-owned buffer
/// into `*out_content` / `*out_content_len`. Caller MUST free via
/// [`net_blob_free_buffer`]. On error returns a negative code and
/// leaves the out-params at `(null, 0)`.
///
/// # Safety
/// - `adapter_id` must point to a valid null-terminated UTF-8 byte
///   sequence.
/// - `payload` must point to a readable region of at least
///   `payload_len` bytes (or be null when `payload_len == 0`).
/// - `out_content` and `out_content_len` must each point to writable
///   `*mut u8` / `usize` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_resolve(
    adapter_id: *const c_char,
    payload: *const u8,
    payload_len: usize,
    out_content: *mut *mut u8,
    out_content_len: *mut usize,
) -> c_int {
    if out_content.is_null() || out_content_len.is_null() {
        return NetError::NullPointer.into();
    }
    *out_content = ptr::null_mut();
    *out_content_len = 0;

    let id = match c_str_to_owned(adapter_id) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    if payload.is_null() && payload_len > 0 {
        return NetError::NullPointer.into();
    }
    // `slice::from_raw_parts` requires `len <= isize::MAX`.
    if payload_len > isize::MAX as usize {
        return NetError::InvalidJson.into();
    }
    let payload_slice = if payload_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(payload, payload_len)
    };

    let adapter = match global_blob_adapter_registry().get(&id) {
        Some(a) => a,
        None => return NET_ERR_BLOB_NOT_REGISTERED,
    };
    // Same catch_unwind protection as net_blob_publish.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(async move { resolve_payload(payload_slice, adapter.as_ref()).await })
    }));
    let bytes = match result {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return err_to_code(&e),
        Err(_) => return NET_ERR_BLOB_PANIC,
    };

    write_bytes_out(&bytes, out_content, out_content_len)
}

/// Allocate a Rust-owned buffer with an explicit `Layout::array::<u8>(len)`,
/// copy `src` into it, and write `(ptr, len)` to the caller's out-pointers.
/// Pairs with [`net_blob_free_buffer`], which deallocates with the matching
/// layout. Pre-fix this path went `Vec → into_boxed_slice → Box::into_raw`,
/// freed via `Box::from_raw(slice_from_raw_parts_mut(ptr, len))`. That
/// worked because `into_boxed_slice` happens to shrink-to-fit today, but
/// relied on a `Vec` / `Box<[u8]>` allocator-internals coincidence. A
/// future refactor to `Vec::leak` (which does NOT shrink) would have
/// silently mismatched the dealloc layout. Using an explicit
/// `Layout::array::<u8>` on both sides makes the contract self-evident.
///
/// # Safety
/// `out_ptr` and `out_len` must be writable.
unsafe fn write_bytes_out(src: &[u8], out_ptr: *mut *mut u8, out_len: *mut usize) -> c_int {
    let len = src.len();
    if len == 0 {
        unsafe {
            *out_ptr = ptr::null_mut();
            *out_len = 0;
        }
        return 0;
    }
    let layout = match std::alloc::Layout::array::<u8>(len) {
        Ok(l) => l,
        // `Layout::array::<u8>` only fails when `len > isize::MAX`.
        // The publish/resolve paths have already rejected that range
        // (slice::from_raw_parts shares the same cap), so this is
        // unreachable from the existing call sites — but defending it
        // here keeps `write_bytes_out` safe to reuse from any future
        // caller. Returning a typed code beats panicking across the
        // surrounding `extern "C"` frame.
        Err(_) => return NetError::InvalidJson.into(),
    };
    let alloc_ptr = unsafe { std::alloc::alloc(layout) };
    if alloc_ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), alloc_ptr, len);
        *out_ptr = alloc_ptr;
        *out_len = len;
    }
    0
}

/// Free a buffer returned by [`net_blob_publish`] or
/// [`net_blob_resolve`]. Calling with `(null, _)` or `(_, 0)` is a no-op.
///
/// # Safety
/// `ptr` MUST be a buffer that the substrate previously returned
/// from `net_blob_publish` or `net_blob_resolve` (or null), and
/// `len` MUST match the corresponding `*out_*_len` value from
/// that call. Calling with any other `(ptr, len)` is undefined
/// behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_free_buffer(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    // Match the `Layout::array::<u8>(len)` used by `write_bytes_out`.
    // Any `len > isize::MAX` could not have come from us — the
    // allocating side would have rejected the same layout — so the
    // safest response is to abandon the free rather than unwind
    // across the FFI boundary.
    let layout = match std::alloc::Layout::array::<u8>(len) {
        Ok(l) => l,
        Err(_) => return,
    };
    std::alloc::dealloc(ptr, layout);
}

// Ensure the unused-import lint stays quiet under feature gates that
// drop one of these surfaces — currently all callable.
#[allow(dead_code)]
fn _force_use() -> *mut c_void {
    ptr::null_mut()
}

// =========================================================================
// C-side callback adapter — register a function-pointer-table from
// a cgo / native caller and let the substrate dispatch BlobAdapter
// calls into it. The substrate wraps the table as a `dyn BlobAdapter`
// and stores it in the global registry under the supplied id.
// =========================================================================

use std::ops::Range;

use async_trait::async_trait;
use bytes::Bytes;

/// `store` function pointer. Caller-allocates nothing; returns
/// `0` on success or a negative `c_int` on failure.
pub type NetBlobAdapterStoreFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    uri: *const c_char,
    hash: *const u8, // exactly 32 bytes
    size: u64,
    data: *const u8,
    data_len: usize,
) -> c_int;

/// `fetch` / `fetch_range` function pointer. Caller-allocates the
/// return buffer and writes the pointer + length into the
/// out-params. The substrate releases it via the vtable's
/// `free_buffer` after consuming the bytes.
pub type NetBlobAdapterFetchFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    uri: *const c_char,
    hash: *const u8,
    size: u64,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> c_int;

/// `fetch_range` function pointer.
pub type NetBlobAdapterFetchRangeFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    uri: *const c_char,
    hash: *const u8,
    size: u64,
    range_start: u64,
    range_end: u64,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> c_int;

/// `exists` function pointer. Writes a `0` / `1` boolean into
/// `out_exists` on success.
pub type NetBlobAdapterExistsFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    uri: *const c_char,
    hash: *const u8,
    size: u64,
    out_exists: *mut c_int,
) -> c_int;

/// Frees a buffer that the caller's `fetch` / `fetch_range`
/// allocated. The substrate calls this after consuming the
/// returned bytes.
pub type NetBlobAdapterFreeFn = unsafe extern "C" fn(ctx: *mut c_void, data: *mut u8, len: usize);

/// Releases the caller's `ctx` once the substrate holds no reference to
/// it. Passed to [`net_blob_register_callback_adapter_owned`]; called
/// exactly once, from the drop of the last shared reference to the
/// context, which every in-flight vtable call holds until it returns
/// (after `free_buffer`, for fetches).
pub type NetBlobAdapterReleaseFn = unsafe extern "C" fn(ctx: *mut c_void);

/// Function-pointer-table the C-side caller passes to
/// [`net_blob_register_callback_adapter`]. The struct is `#[repr(C)]`
/// for cross-ABI stability.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NetBlobAdapterVtable {
    /// `store(ctx, uri, hash, size, data, data_len) -> c_int`
    pub store: NetBlobAdapterStoreFn,
    /// `fetch(ctx, uri, hash, size, &out_data, &out_len) -> c_int`
    pub fetch: NetBlobAdapterFetchFn,
    /// `fetch_range(ctx, uri, hash, size, start, end, &out_data, &out_len)`
    pub fetch_range: NetBlobAdapterFetchRangeFn,
    /// `exists(ctx, uri, hash, size, &out_exists) -> c_int`
    pub exists: NetBlobAdapterExistsFn,
    /// `free_buffer(ctx, data, len)` — substrate calls this after
    /// consuming a buffer the caller returned via `fetch` /
    /// `fetch_range`.
    pub free_buffer: NetBlobAdapterFreeFn,
}

/// Opaque caller-context pointer.
///
/// # Concurrency contract (caller MUST uphold)
///
/// The substrate dispatches every vtable call from a
/// `tokio::task::spawn_blocking` worker, which means the same
/// `ctx` pointer is observed from **multiple OS threads over the
/// lifetime of the registration** and may be observed
/// **concurrently** if two events for the same adapter are
/// in-flight. `Send + Sync` are asserted unconditionally because
/// the substrate has no visibility into what the pointer
/// references — the C-side registrant is the trust boundary.
///
/// In practical terms, this means a registrant **MUST** pass a
/// `ctx` whose pointee is:
///
/// - **`Send` across threads**: any per-thread state (e.g. a
///   thread-local OS handle, a goroutine-local pointer, a
///   Python `PyObject*` held without the GIL) is unsafe.
/// - **`Sync` for concurrent dispatch**: any state mutated
///   inside vtable callbacks must be protected against
///   data races by the registrant (lock, atomic, etc.).
///
/// Wrappers that cannot meet the `Sync` requirement (e.g. a
/// Python adapter that uses the GIL) MUST serialize their own
/// dispatch behind a `Mutex` before passing control to the
/// language runtime.
struct OpaqueCtx {
    ptr: *mut c_void,
    /// Owned-context registrations only: called once from `Drop`, and
    /// only once `armed` (set after the registry accepted the adapter, so
    /// a refused registration leaves `ctx` owned by the caller).
    release: Option<NetBlobAdapterReleaseFn>,
    armed: std::sync::atomic::AtomicBool,
}

// SAFETY: opaque-pointer transport — see `OpaqueCtx` doc above.
// Cross-thread coherence of the pointee is the C-side caller's
// responsibility; the substrate only reads and forwards the
// same address verbatim.
unsafe impl Send for OpaqueCtx {}
unsafe impl Sync for OpaqueCtx {}

impl OpaqueCtx {
    fn new(ptr: *mut c_void) -> Self {
        Self {
            ptr,
            release: None,
            armed: std::sync::atomic::AtomicBool::new(false),
        }
    }
    fn owned(ptr: *mut c_void, release: NetBlobAdapterReleaseFn) -> Self {
        Self {
            ptr,
            release: Some(release),
            armed: std::sync::atomic::AtomicBool::new(false),
        }
    }
    fn arm(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::Release);
    }
    fn get(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for OpaqueCtx {
    fn drop(&mut self) {
        if !self.armed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if let Some(release) = self.release {
            // SAFETY: `release` came from the registrant, who promised it
            // stays callable until it has run; `Drop` runs once.
            unsafe { release(self.ptr) };
        }
    }
}

/// Test barriers (`fixtures` only): hold a vtable dispatch at a stage so a
/// test can act while a call is in flight. Stage 1 is "about to invoke the
/// callback"; stage 2 is "about to call `free_buffer`". An arm targets one
/// context (or any, with `0`), so concurrent tests on other adapters never
/// trip it. Compiled for unit tests and `fixtures`; no-ops otherwise.
#[cfg(any(test, feature = "fixtures"))]
mod callback_barrier {
    use parking_lot::{Condvar, Mutex};

    struct State {
        /// `Some(target ctx address)`, `Some(0)` for any context.
        armed: [Option<usize>; 3],
        held: [bool; 3],
        released: [bool; 3],
    }

    static STATE: Mutex<State> = Mutex::new(State {
        armed: [None; 3],
        held: [false; 3],
        released: [false; 3],
    });
    static CV: Condvar = Condvar::new();

    pub(super) fn checkpoint(stage: usize, ctx: usize) {
        let mut st = STATE.lock();
        match st.armed[stage] {
            Some(target) if target == 0 || target == ctx => {}
            _ => return,
        }
        st.armed[stage] = None; // single-shot
        st.held[stage] = true;
        CV.notify_all();
        while !st.released[stage] {
            CV.wait(&mut st);
        }
        st.held[stage] = false;
        st.released[stage] = false;
    }

    pub(super) fn arm(stage: usize, target: usize) {
        let mut st = STATE.lock();
        st.armed[stage] = Some(target);
        st.released[stage] = false;
    }

    pub(super) fn wait_held(stage: usize, timeout: std::time::Duration) -> bool {
        let mut st = STATE.lock();
        CV.wait_while_for(&mut st, |s| !s.held[stage], timeout);
        st.held[stage]
    }

    pub(super) fn release(stage: usize) {
        let mut st = STATE.lock();
        st.released[stage] = true;
        CV.notify_all();
    }
}

#[inline]
fn callback_checkpoint(_stage: usize, _ctx: *mut c_void) {
    #[cfg(any(test, feature = "fixtures"))]
    callback_barrier::checkpoint(_stage, _ctx as usize);
}

/// `BlobAdapter` impl that calls into a vtable of C function
/// pointers. Each trait method translates the args into
/// `*const c_char` / `*const u8` shapes, dispatches inside
/// `tokio::task::spawn_blocking` so the tokio worker isn't
/// blocked on synchronous C-side I/O, and maps the return code
/// back into a `Result<_, BlobError>`.
struct CallbackBlobAdapter {
    id: String,
    vtable: NetBlobAdapterVtable,
    ctx: Arc<OpaqueCtx>,
}

unsafe impl Send for CallbackBlobAdapter {}
unsafe impl Sync for CallbackBlobAdapter {}

fn code_to_err(code: c_int, label: &str) -> InnerBlobError {
    match code {
        NET_ERR_BLOB_NOT_FOUND => InnerBlobError::NotFound(label.into()),
        NET_ERR_BLOB_HASH_MISMATCH => InnerBlobError::Backend(format!(
            "{}: substrate hash mismatch (caller returned wrong bytes)",
            label
        )),
        NET_ERR_BLOB_UNSUPPORTED_SCHEME => InnerBlobError::UnsupportedScheme(label.into()),
        NET_ERR_BLOB_DECODE => InnerBlobError::Decode(label.into()),
        _ => InnerBlobError::Backend(format!("{}: code {}", label, code)),
    }
}

/// Extract `(uri, hash, size)` from a [`BlobRef::Small`] for an FFI
/// vtable call. The C vtable signature only supports single-hash
/// blobs; chunked dispatch happens at the substrate's
/// `MeshBlobAdapter` layer above this FFI shim. A
/// [`BlobRef::Manifest`] passed here is a layering bug; surface
/// `InnerBlobError::Backend` rather than silently truncating to the
/// first chunk.
fn expect_small_for_ffi(
    blob_ref: &crate::adapter::net::dataforts::BlobRef,
) -> std::result::Result<(String, [u8; 32], u64), InnerBlobError> {
    match blob_ref {
        crate::adapter::net::dataforts::BlobRef::Small {
            uri, hash, size, ..
        } => Ok((uri.clone(), *hash, *size)),
        crate::adapter::net::dataforts::BlobRef::Manifest { .. }
        | crate::adapter::net::dataforts::BlobRef::Tree { .. } => Err(InnerBlobError::Backend(
            "CallbackBlobAdapter operates on Small blobs only; \
                 chunked blobs are dispatched at the substrate above"
                .to_owned(),
        )),
    }
}

#[async_trait]
impl BlobAdapter for CallbackBlobAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    async fn store(
        &self,
        blob_ref: &crate::adapter::net::dataforts::BlobRef,
        bytes: &[u8],
    ) -> std::result::Result<(), InnerBlobError> {
        let vtable = self.vtable;
        let ctx = self.ctx.clone();
        let (uri_str, hash, size) = expect_small_for_ffi(blob_ref)?;
        let uri = match std::ffi::CString::new(uri_str) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        let data = bytes.to_vec();
        tokio::task::spawn_blocking(move || -> std::result::Result<(), InnerBlobError> {
            let code = unsafe {
                (vtable.store)(
                    ctx.get(),
                    uri.as_ptr(),
                    hash.as_ptr(),
                    size,
                    data.as_ptr(),
                    data.len(),
                )
            };
            if code == 0 {
                Ok(())
            } else {
                Err(code_to_err(code, "store"))
            }
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn fetch(
        &self,
        blob_ref: &crate::adapter::net::dataforts::BlobRef,
    ) -> std::result::Result<Bytes, InnerBlobError> {
        let vtable = self.vtable;
        let ctx = self.ctx.clone();
        let (uri_str, hash, size) = expect_small_for_ffi(blob_ref)?;
        let uri = match std::ffi::CString::new(uri_str) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        tokio::task::spawn_blocking(move || -> std::result::Result<Bytes, InnerBlobError> {
            let mut out_data: *mut u8 = ptr::null_mut();
            let mut out_len: usize = 0;
            callback_checkpoint(1, ctx.get());
            let code = unsafe {
                (vtable.fetch)(
                    ctx.get(),
                    uri.as_ptr(),
                    hash.as_ptr(),
                    size,
                    &mut out_data,
                    &mut out_len,
                )
            };
            if code != 0 {
                return Err(code_to_err(code, "fetch"));
            }
            if out_data.is_null() {
                if out_len == 0 {
                    return Ok(Bytes::new());
                }
                return Err(InnerBlobError::Backend(
                    "fetch: caller returned null pointer with non-zero len".into(),
                ));
            }
            // Copy out before freeing — the FFI caller owns the
            // buffer and frees it via free_buffer. We can't hand
            // the FFI-owned pointer to `Bytes` because rust would
            // assume Vec-style allocator ownership, so the copy
            // is unavoidable here (per dataforts perf #184 — the
            // savings the Bytes signature unlocks are inside the
            // mesh/fs/noop adapters; FFI callbacks pay the copy
            // at the boundary in either direction).
            let buf = unsafe { std::slice::from_raw_parts(out_data, out_len).to_vec() };
            callback_checkpoint(2, ctx.get());
            unsafe { (vtable.free_buffer)(ctx.get(), out_data, out_len) };
            Ok(Bytes::from(buf))
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn fetch_range(
        &self,
        blob_ref: &crate::adapter::net::dataforts::BlobRef,
        range: Range<u64>,
    ) -> std::result::Result<Bytes, InnerBlobError> {
        let vtable = self.vtable;
        let ctx = self.ctx.clone();
        let (uri_str, hash, size) = expect_small_for_ffi(blob_ref)?;
        let uri = match std::ffi::CString::new(uri_str) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        let start = range.start;
        let end = range.end;
        tokio::task::spawn_blocking(move || -> std::result::Result<Bytes, InnerBlobError> {
            let mut out_data: *mut u8 = ptr::null_mut();
            let mut out_len: usize = 0;
            callback_checkpoint(1, ctx.get());
            let code = unsafe {
                (vtable.fetch_range)(
                    ctx.get(),
                    uri.as_ptr(),
                    hash.as_ptr(),
                    size,
                    start,
                    end,
                    &mut out_data,
                    &mut out_len,
                )
            };
            if code != 0 {
                return Err(code_to_err(code, "fetch_range"));
            }
            if out_data.is_null() {
                if out_len == 0 {
                    return Ok(Bytes::new());
                }
                return Err(InnerBlobError::Backend(
                    "fetch_range: caller returned null pointer with non-zero len".into(),
                ));
            }
            let buf = unsafe { std::slice::from_raw_parts(out_data, out_len).to_vec() };
            callback_checkpoint(2, ctx.get());
            unsafe { (vtable.free_buffer)(ctx.get(), out_data, out_len) };
            Ok(Bytes::from(buf))
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn exists(
        &self,
        blob_ref: &crate::adapter::net::dataforts::BlobRef,
    ) -> std::result::Result<bool, InnerBlobError> {
        let vtable = self.vtable;
        let ctx = self.ctx.clone();
        let (uri_str, hash, size) = expect_small_for_ffi(blob_ref)?;
        let uri = match std::ffi::CString::new(uri_str) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        tokio::task::spawn_blocking(move || -> std::result::Result<bool, InnerBlobError> {
            let mut out_exists: c_int = 0;
            let code = unsafe {
                (vtable.exists)(
                    ctx.get(),
                    uri.as_ptr(),
                    hash.as_ptr(),
                    size,
                    &mut out_exists,
                )
            };
            if code != 0 {
                return Err(code_to_err(code, "exists"));
            }
            Ok(out_exists != 0)
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }
}

/// Register a C-side BlobAdapter implementation. The vtable is
/// copied into the adapter; `ctx` is shuttled across every call as
/// an opaque pointer (caller is responsible for thread-safety).
///
/// Returns `0` on success, `NET_ERR_BLOB_DUPLICATE_ID` if `id` is
/// already registered, or `NetError::InvalidUtf8` / `NullPointer`
/// for malformed input.
///
/// # Safety
/// - `adapter_id` must point to a valid null-terminated UTF-8 byte
///   sequence.
/// - `vtable` must point to a fully-initialised `NetBlobAdapterVtable`
///   whose function pointers remain valid for the lifetime of the
///   registration (i.e. until `net_blob_unregister_adapter` returns
///   AND any in-flight calls have completed).
/// - `ctx` is an opaque pointer the substrate passes through unchanged
///   to every vtable call; the caller is responsible for keeping the
///   pointee alive for the same lifetime as `vtable`.
///
/// # Concurrency contract (caller MUST uphold)
///
/// The substrate dispatches every vtable call from a
/// `tokio::task::spawn_blocking` worker. The same `ctx` will be
/// observed from **multiple OS threads** over the lifetime of the
/// registration and may be observed **concurrently** when two
/// in-flight calls are dispatched to the same adapter.
///
/// The pointee of `ctx` therefore MUST be:
/// - safely transferable across threads (`Send`-equivalent in the
///   caller's runtime); and
/// - safely accessed concurrently (`Sync`-equivalent), or guarded
///   inside the vtable callbacks by a caller-owned lock.
///
/// Passing a thread-local pointer (an OS thread handle, a Go
/// goroutine-local pointer, a Python `PyObject*` held outside the
/// GIL, etc.) is **undefined behaviour**. Wrappers whose runtime
/// cannot meet the `Sync` requirement MUST serialize vtable
/// dispatch inside the callback before crossing into the
/// language runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_register_callback_adapter(
    adapter_id: *const c_char,
    vtable: *const NetBlobAdapterVtable,
    ctx: *mut c_void,
) -> c_int {
    if vtable.is_null() {
        return NetError::NullPointer.into();
    }
    let id = match c_str_to_owned(adapter_id) {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    // Validate every fn-ptr field is non-null BEFORE materialising
    // the vtable as a value-typed `NetBlobAdapterVtable` — Rust's
    // `unsafe extern "C" fn` type is non-nullable, so loading a
    // struct whose C-side caller left any field NULL is immediate
    // UB. Cast each field through a `*const ()` to read the raw
    // bits without constructing a non-null fn-pointer value.
    {
        let raw = vtable as *const c_void as *const *const c_void;
        // Five fn-ptr fields (store / fetch / fetch_range /
        // exists / free_buffer). Reading them as *const c_void
        // gives the raw address without invoking the fn-ptr type's
        // non-null invariant.
        for i in 0..5 {
            let field = unsafe { *raw.add(i) };
            if field.is_null() {
                return NET_ERR_BLOB_BACKEND;
            }
        }
    }
    let vtable = unsafe { *vtable };
    let adapter: Arc<dyn BlobAdapter> = Arc::new(CallbackBlobAdapter {
        id: id.clone(),
        vtable,
        ctx: Arc::new(OpaqueCtx::new(ctx)),
    });
    match global_blob_adapter_registry().register(adapter) {
        Ok(()) => 0,
        Err(_) => NET_ERR_BLOB_DUPLICATE_ID,
    }
}

/// Like [`net_blob_register_callback_adapter`], but the substrate takes
/// ownership of `ctx` and tells the caller when it is done with it:
/// `release_fn(ctx)` runs **exactly once**, after the adapter is
/// unregistered (or replaced) **and** the last in-flight vtable call
/// holding the context has returned — including its `free_buffer`. That
/// is the point at which the caller may reclaim whatever `ctx` names (a
/// cgo handle, a refcount). Additive: the existing registration function
/// and the vtable layout are unchanged.
///
/// Ownership on failure: if this returns non-zero (NULL vtable or
/// `release_fn`, a NULL vtable entry, bad UTF-8, a duplicate id),
/// `release_fn` is **never** called and `ctx` stays the caller's.
///
/// # Safety
/// As [`net_blob_register_callback_adapter`]; additionally `release_fn`
/// must stay callable until it has run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_register_callback_adapter_owned(
    adapter_id: *const c_char,
    vtable: *const NetBlobAdapterVtable,
    ctx: *mut c_void,
    release_fn: Option<NetBlobAdapterReleaseFn>,
) -> c_int {
    let Some(release_fn) = release_fn else {
        return NetError::NullPointer.into();
    };
    if vtable.is_null() {
        return NetError::NullPointer.into();
    }
    let id = match unsafe { c_str_to_owned(adapter_id) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    {
        let raw = vtable as *const c_void as *const *const c_void;
        for i in 0..5 {
            if unsafe { *raw.add(i) }.is_null() {
                return NET_ERR_BLOB_BACKEND;
            }
        }
    }
    let vtable = unsafe { *vtable };
    // Not armed yet: if the registry refuses the adapter, dropping it
    // drops this context without calling release_fn.
    let ctx = Arc::new(OpaqueCtx::owned(ctx, release_fn));
    let adapter: Arc<dyn BlobAdapter> = Arc::new(CallbackBlobAdapter {
        id,
        vtable,
        ctx: Arc::clone(&ctx),
    });
    match global_blob_adapter_registry().register(adapter) {
        Ok(()) => {
            ctx.arm();
            0
        }
        Err(_) => NET_ERR_BLOB_DUPLICATE_ID,
    }
}

/// TEST SEAM (`fixtures` only): arm a single-shot hold at `stage` (1 =
/// before the next vtable fetch callback, 2 = before its `free_buffer`)
/// for the adapter registered with `ctx` (NULL: any adapter).
#[cfg(feature = "fixtures")]
#[unsafe(no_mangle)]
pub extern "C" fn net_blob_test_barrier_arm(stage: c_int, ctx: *mut c_void) {
    if (1..=2).contains(&stage) {
        callback_barrier::arm(stage as usize, ctx as usize);
    }
}

/// TEST SEAM (`fixtures` only): wait up to `timeout_ms` for a dispatch to
/// be held at `stage`. `1` held, `0` not.
#[cfg(feature = "fixtures")]
#[unsafe(no_mangle)]
pub extern "C" fn net_blob_test_barrier_wait_held(stage: c_int, timeout_ms: u32) -> c_int {
    if !(1..=2).contains(&stage) {
        return 0;
    }
    c_int::from(callback_barrier::wait_held(
        stage as usize,
        std::time::Duration::from_millis(u64::from(timeout_ms)),
    ))
}

/// TEST SEAM (`fixtures` only): let the dispatch held at `stage` continue.
#[cfg(feature = "fixtures")]
#[unsafe(no_mangle)]
pub extern "C" fn net_blob_test_barrier_release(stage: c_int) {
    if (1..=2).contains(&stage) {
        callback_barrier::release(stage as usize);
    }
}

// =========================================================================
// MeshBlobAdapter — v0.2 substrate-owned blob CAS + v0.3 active overflow
// =========================================================================
//
// Mirrors the Node + Python `MeshBlobAdapter` surface for the
// Go binding via cgo. JSON-encoded configs at the FFI boundary
// (matches the existing `net_redex_enable_greedy_dataforts` and
// peers); the Go wrapper marshals from `struct{...}` into the
// JSON shape before calling these.

/// Opaque handle to a `MeshBlobAdapter`. The Box owns an
/// `Arc<InnerMeshBlobAdapter>` so multiple handles can share
/// the adapter — but the FFI surface only ever hands out one
/// handle per `_new` call; the operator clones at the Go layer
/// if they want fan-out. Free with [`net_mesh_blob_adapter_free`].
///
/// Carries a [`HandleGuard`] inline so a concurrent `_free` racing an
/// in-flight op cannot deallocate the inner out from under it. Same
/// quiescing recipe as the cortex / mesh / redis handles: every op
/// gates on `guard.try_enter()`; `_free` drives `guard.begin_free()`
/// and leaks the box (dropping only the inner). See
/// [`super::handle_guard`] for the soundness argument.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
pub struct MeshBlobAdapterHandle {
    inner: ManuallyDrop<Arc<InnerMeshBlobAdapter>>,
    guard: HandleGuard,
}

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
use std::mem::ManuallyDrop;

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
use super::handle_guard::{HandleGuard, FFI_HANDLE_FREE_DEADLINE};

/// Run a blob-adapter FFI body under `catch_unwind`. With
/// `panic = "unwind"`, a panic escaping an `extern "C"` function is UB
/// across the cgo / N-API / cffi boundary. The shim catches the
/// unwind, logs, and returns the caller-supplied fallback — matching
/// the protection `net_blob_publish` / `net_blob_resolve` already
/// carry, which the metrics / config accessors previously lacked.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[inline]
fn adapter_guard<R>(name: &'static str, fallback: R, f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            tracing::error!(
                ffi_function = name,
                "panic caught in mesh blob adapter FFI; returning fallback to avoid \
                 UB across the C boundary",
            );
            fallback
        }
    }
}

/// JSON shape for the `overflow` config option passed to
/// [`net_mesh_blob_adapter_new`] + [`net_mesh_blob_adapter_set_overflow_config`].
/// Mirrors the typed `OverflowConfig` from the Rust crate;
/// `scope` is one of `"node" | "zone" | "region" | "mesh"`.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[derive(serde::Deserialize, serde::Serialize)]
struct OverflowConfigJson {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    high_water_ratio: Option<f64>,
    #[serde(default)]
    low_water_ratio: Option<f64>,
    #[serde(default)]
    max_pushes_per_tick: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    tick_interval_ms: Option<u64>,
}

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
fn parse_overflow_json(s: &str) -> Result<InnerOverflowConfig, c_int> {
    if s.is_empty() {
        return Ok(InnerOverflowConfig::default());
    }
    let raw: OverflowConfigJson =
        serde_json::from_str(s).map_err(|_| -> c_int { NetError::InvalidJson.into() })?;
    overflow_from_raw(raw)
}

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
fn overflow_from_raw(raw: OverflowConfigJson) -> Result<InnerOverflowConfig, c_int> {
    let mut cfg = InnerOverflowConfig {
        enabled: raw.enabled,
        ..InnerOverflowConfig::default()
    };
    if let Some(v) = raw.high_water_ratio {
        cfg.high_water_ratio = v;
    }
    if let Some(v) = raw.low_water_ratio {
        cfg.low_water_ratio = v;
    }
    if let Some(v) = raw.max_pushes_per_tick {
        cfg.max_pushes_per_tick = v as usize;
    }
    if let Some(s) = raw.scope {
        cfg.scope = match s.to_ascii_lowercase().as_str() {
            "node" => TopologyScope::Node,
            "zone" => TopologyScope::Zone,
            "region" => TopologyScope::Region,
            "mesh" => TopologyScope::Mesh,
            _ => {
                let code: c_int = NetError::InvalidJson.into();
                return Err(code);
            }
        };
    }
    if let Some(v) = raw.tick_interval_ms {
        cfg.tick_interval_ms = v;
    }
    Ok(cfg)
}

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
fn overflow_to_json(cfg: InnerOverflowConfig) -> String {
    let scope = match cfg.scope {
        TopologyScope::Node => "node",
        TopologyScope::Zone => "zone",
        TopologyScope::Region => "region",
        TopologyScope::Mesh => "mesh",
    };
    let raw = OverflowConfigJson {
        enabled: cfg.enabled,
        high_water_ratio: Some(cfg.high_water_ratio),
        low_water_ratio: Some(cfg.low_water_ratio),
        max_pushes_per_tick: Some(cfg.max_pushes_per_tick as u64),
        scope: Some(scope.to_string()),
        tick_interval_ms: Some(cfg.tick_interval_ms),
    };
    serde_json::to_string(&raw).unwrap_or_else(|_| "{}".to_string())
}

/// Construct a `MeshBlobAdapter` against `redex`.
///
/// - `redex` — pointer to a `RedexHandle` from `net_redex_new`. The
///   adapter clones the inner `Arc<Redex>`; the redex handle stays
///   valid after this call.
/// - `adapter_id` — null-terminated UTF-8 identity tag.
/// - `persistent` — `0` = in-memory chunks; `1` = disk-backed
///   (requires the redex to have been opened with a `persistent_dir`).
/// - `overflow_json` — null OR null-terminated JSON for the v0.3
///   overflow config. Empty string / null = overflow off (the
///   v0.2 default).
///
/// Returns a non-null handle on success. On error returns null and
/// sets no errno-equivalent — operators check for null + retry with
/// a well-formed JSON config. Free with `net_mesh_blob_adapter_free`.
///
/// # Safety
/// `redex` must be a valid `RedexHandle*` returned from `net_redex_new`
/// and not yet freed. `adapter_id` must be a valid null-terminated
/// UTF-8 string. `overflow_json` may be null or a valid
/// null-terminated UTF-8 JSON string.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_new(
    redex: *mut super::cortex::RedexHandle,
    adapter_id: *const c_char,
    persistent: c_int,
    overflow_json: *const c_char,
) -> *mut MeshBlobAdapterHandle {
    if redex.is_null() {
        return ptr::null_mut();
    }
    let id = match unsafe { c_str_to_owned(adapter_id) } {
        Some(s) => s,
        None => return ptr::null_mut(),
    };
    let overflow_str = if overflow_json.is_null() {
        String::new()
    } else {
        match unsafe { c_str_to_owned(overflow_json) } {
            Some(s) => s,
            None => return ptr::null_mut(),
        }
    };
    let overflow_cfg = match parse_overflow_json(&overflow_str) {
        Ok(c) => c,
        Err(_) => return ptr::null_mut(),
    };
    // Gated clone of the redex inner — `None` means the redex handle
    // is being freed concurrently; surface a null handle rather than
    // racing the inner out of `ManuallyDrop`.
    let Some(redex_inner) = (unsafe { (*redex).redex_arc() }) else {
        return ptr::null_mut();
    };
    let mut builder = InnerMeshBlobAdapter::new(id, redex_inner).with_persistent(persistent != 0);
    if !overflow_str.is_empty() {
        builder = builder.with_overflow(overflow_cfg);
    }
    Box::into_raw(Box::new(MeshBlobAdapterHandle {
        inner: ManuallyDrop::new(Arc::new(builder)),
        guard: HandleGuard::new(),
    }))
}

/// Free a handle from [`net_mesh_blob_adapter_new`]. Idempotent
/// against a null pointer.
///
/// # Safety
/// `handle` must be a pointer returned by `net_mesh_blob_adapter_new`
/// + not yet freed, or null.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_free(handle: *mut MeshBlobAdapterHandle) {
    if handle.is_null() {
        return;
    }
    // Quiesce in-flight ops before dropping the inner; the box stays
    // leaked (never `Box::from_raw`) so a concurrent op's `try_enter`
    // fetch_add still lands on valid memory. See `super::handle_guard`.
    let h: &MeshBlobAdapterHandle = unsafe { &*handle };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        // SAFETY: drained; sole writable reference. Single-winner
        // contract on `begin_free` makes this `take` happen at most once.
        unsafe {
            let inner = ManuallyDrop::take(&mut (*handle).inner);
            drop(inner);
        }
    } else {
        tracing::warn!(
            "net_mesh_blob_adapter_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

/// Clone the `Arc<MeshBlobAdapter>` backing this handle under the
/// handle guard, for the sibling transport FFI (`ffi::transport`). The
/// `try_enter` op is held across the `Arc::clone` so a concurrent
/// `net_mesh_blob_adapter_free` cannot take the inner out of
/// `ManuallyDrop` mid-clone; the bumped refcount then keeps the adapter
/// alive independently. Returns `None` once `_free` has begun. Mirrors
/// [`super::mesh::mesh_node_arc`].
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
pub(super) fn blob_adapter_arc(h: &MeshBlobAdapterHandle) -> Option<Arc<InnerMeshBlobAdapter>> {
    let _op = h.guard.try_enter()?;
    Some(Arc::clone(&h.inner))
}

/// Store `data` of `data_len` bytes under the content address
/// declared by `blob_ref_bytes` (a previously-encoded `BlobRef`
/// wire blob from `net_blob_publish` or constructed externally).
///
/// Returns `0` on success, `NET_ERR_BLOB_*` on adapter-side error,
/// or `NetError::NullPointer` / `InvalidUtf8` for input validation.
/// The substrate BLAKE3-verifies the bytes against the BlobRef
/// hash before persisting.
///
/// # Safety
/// `handle` is a valid `MeshBlobAdapterHandle*`. `blob_ref_bytes`
/// + `data` point to readable buffers of the supplied lengths.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_store(
    handle: *const MeshBlobAdapterHandle,
    blob_ref_bytes: *const u8,
    blob_ref_len: usize,
    data: *const u8,
    data_len: usize,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_store", null_rc, || {
        if handle.is_null() || blob_ref_bytes.is_null() {
            return NetError::NullPointer.into();
        }
        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        if blob_ref_len > isize::MAX as usize || data_len > isize::MAX as usize {
            return NetError::InvalidJson.into();
        }
        let h = unsafe { &*handle };
        // Bail (same shape as null handle) if `_free` has begun.
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let blob_slice = unsafe { std::slice::from_raw_parts(blob_ref_bytes, blob_ref_len) };
        let blob_ref = match InnerBlobRef::decode(blob_slice) {
            Ok(Some(b)) => b,
            _ => return NET_ERR_BLOB_DECODE,
        };
        let data_slice = if data.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(data, data_len) }
        };
        let adapter = Arc::clone(&h.inner);
        let data_owned = data_slice.to_vec();
        let result = block_on(async move { (*adapter).store(&blob_ref, &data_owned).await });
        match result {
            Ok(()) => 0,
            Err(e) => err_to_code(&e),
        }
    })
}

/// Mint a content address for `data` and store it: compute the
/// BLAKE3 hash of the bytes, build the `BlobRef`, persist through
/// the adapter, and write the *encoded* ref to `*out_ref` /
/// `*out_ref_len` for the caller.
///
/// This is the producer half that [`net_mesh_blob_adapter_store`]
/// cannot provide: `store` requires an already-encoded ref, so
/// without this a C or Go producer has no way to create one.
/// `store` remains the path for writing under a ref minted
/// elsewhere.
///
/// Returns `0` on success (caller frees `*out_ref` via
/// [`net_blob_free_buffer`]), `NET_ERR_BLOB_*` on adapter-side
/// error, `NET_ERR_BLOB_UNSUPPORTED_SCHEME` when the URI's scheme
/// is not one the adapter accepts, or `NetError::NullPointer` /
/// `InvalidUtf8` for input validation.
///
/// # Safety
/// `handle` is a valid `MeshBlobAdapterHandle*`. `uri_ptr` points
/// to `uri_len` readable bytes; `data` points to `data_len`
/// readable bytes (or is null when `data_len` is 0). `out_ref` and
/// `out_ref_len` are non-null and writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_publish(
    handle: *const MeshBlobAdapterHandle,
    uri_ptr: *const u8,
    uri_len: usize,
    data: *const u8,
    data_len: usize,
    out_ref: *mut *mut u8,
    out_ref_len: *mut usize,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_publish", null_rc, || {
        if handle.is_null() || uri_ptr.is_null() || out_ref.is_null() || out_ref_len.is_null() {
            return NetError::NullPointer.into();
        }
        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        if uri_len > isize::MAX as usize || data_len > isize::MAX as usize {
            return NetError::InvalidJson.into();
        }
        let h = unsafe { &*handle };
        // Bail (same shape as null handle) if `_free` has begun.
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let uri_bytes = unsafe { std::slice::from_raw_parts(uri_ptr, uri_len) };
        let uri = match std::str::from_utf8(uri_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => return NetError::InvalidUtf8.into(),
        };
        let data_slice = if data.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(data, data_len) }
        };
        let adapter = Arc::clone(&h.inner);
        let data_owned = data_slice.to_vec();
        let result = block_on(async move {
            // `Arc::clone` on the `ManuallyDrop<Arc<..>>` field, never
            // `.clone()`: the latter clones the WRAPPER, so the bumped
            // strong count is never released and every call leaks one
            // reference to the adapter. One deref then reaches the
            // adapter itself, which is what implements `BlobAdapter`.
            publish_blob_ref(&*adapter, uri, &data_owned).await
        });
        match result {
            Ok(blob_ref) => {
                let encoded = blob_ref.encode();
                unsafe { write_bytes_out(&encoded, out_ref, out_ref_len) }
            }
            Err(e) => err_to_code(&e),
        }
    })
}

/// Copy the 32-byte BLAKE3 hash out of an encoded `BlobRef`.
///
/// The fetch side of the transport ABI (`net_fetch_blob`) addresses a
/// blob by its raw hash, while a published ref crosses as the encoded
/// wire form — so a producer that only holds the encoded ref cannot
/// name what it just stored. This is the join between the two.
///
/// Writes exactly 32 bytes to `out_hash` (caller-allocated) and
/// returns `0`; returns `NET_ERR_BLOB_DECODE` for a malformed or
/// non-small ref (a manifest/tree ref has no single content hash —
/// use its root hash accessor instead).
///
/// # Safety
/// `encoded` points to `encoded_len` readable bytes; `out_hash`
/// points to at least 32 writable bytes.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_ref_hash(
    encoded: *const u8,
    encoded_len: usize,
    out_hash: *mut u8,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_blob_ref_hash", null_rc, || {
        if encoded.is_null() || out_hash.is_null() {
            return NetError::NullPointer.into();
        }
        if encoded_len > isize::MAX as usize {
            return NetError::InvalidJson.into();
        }
        let slice = unsafe { std::slice::from_raw_parts(encoded, encoded_len) };
        let blob_ref = match InnerBlobRef::decode(slice) {
            Ok(Some(b)) => b,
            _ => return NET_ERR_BLOB_DECODE,
        };
        match blob_ref.small_hash() {
            Some(hash) => {
                unsafe { std::ptr::copy_nonoverlapping(hash.as_ptr(), out_hash, hash.len()) };
                0
            }
            None => NET_ERR_BLOB_DECODE,
        }
    })
}

/// Fetch the content for `blob_ref_bytes`. On success writes a
/// heap-allocated buffer pointer to `*out_data` + length to
/// `*out_len` and returns `0`. The caller MUST free via
/// [`net_blob_free_buffer`].
///
/// # Safety
/// `handle`, `blob_ref_bytes`, `out_data`, `out_len` must all be
/// non-null and point to valid memory of the appropriate type.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_fetch(
    handle: *const MeshBlobAdapterHandle,
    blob_ref_bytes: *const u8,
    blob_ref_len: usize,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_fetch", null_rc, || {
        if handle.is_null() || blob_ref_bytes.is_null() || out_data.is_null() || out_len.is_null() {
            return NetError::NullPointer.into();
        }
        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        if blob_ref_len > isize::MAX as usize {
            return NetError::InvalidJson.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let blob_slice = unsafe { std::slice::from_raw_parts(blob_ref_bytes, blob_ref_len) };
        let blob_ref = match InnerBlobRef::decode(blob_slice) {
            Ok(Some(b)) => b,
            _ => return NET_ERR_BLOB_DECODE,
        };
        let adapter = Arc::clone(&h.inner);
        let result = block_on(async move { (*adapter).fetch(&blob_ref).await });
        match result {
            // Allocate with the same explicit `Layout::array::<u8>(len)`
            // path that `net_blob_free_buffer` deallocates with, so the
            // pair is layout-symmetric regardless of any future
            // `Vec::leak` / `into_boxed_slice` refactor inside the
            // adapter.
            Ok(bytes) => unsafe { write_bytes_out(&bytes, out_data, out_len) },
            Err(e) => err_to_code(&e),
        }
    })
}

/// Probe local presence — writes `1` to `*out_exists` if the chunk
/// is locally reachable, `0` otherwise. Returns `0` on success or
/// a `NET_ERR_*` code on failure.
///
/// # Safety
/// `handle`, `blob_ref_bytes`, `out_exists` must all be non-null.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_exists(
    handle: *const MeshBlobAdapterHandle,
    blob_ref_bytes: *const u8,
    blob_ref_len: usize,
    out_exists: *mut c_int,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_exists", null_rc, || {
        if handle.is_null() || blob_ref_bytes.is_null() || out_exists.is_null() {
            return NetError::NullPointer.into();
        }
        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        if blob_ref_len > isize::MAX as usize {
            return NetError::InvalidJson.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let blob_slice = unsafe { std::slice::from_raw_parts(blob_ref_bytes, blob_ref_len) };
        let blob_ref = match InnerBlobRef::decode(blob_slice) {
            Ok(Some(b)) => b,
            _ => return NET_ERR_BLOB_DECODE,
        };
        let adapter = Arc::clone(&h.inner);
        let result = block_on(async move { (*adapter).exists(&blob_ref).await });
        match result {
            Ok(present) => {
                unsafe { *out_exists = if present { 1 } else { 0 } };
                0
            }
            Err(e) => err_to_code(&e),
        }
    })
}

/// Render the adapter's Prometheus text body. Returns a
/// `CString::into_raw`-allocated `*mut c_char` that the caller
/// MUST free via [`crate::ffi::net_free_string`]. Returns null on
/// allocation failure (rare).
///
/// # Safety
/// `handle` must be a valid `MeshBlobAdapterHandle*`.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_prometheus_text(
    handle: *const MeshBlobAdapterHandle,
) -> *mut c_char {
    adapter_guard(
        "net_mesh_blob_adapter_prometheus_text",
        ptr::null_mut(),
        || {
            if handle.is_null() {
                return ptr::null_mut();
            }
            let h = unsafe { &*handle };
            let _op = match h.guard.try_enter() {
                Some(op) => op,
                None => return ptr::null_mut(),
            };
            let adapter = Arc::clone(&h.inner);
            let body = (*adapter).prometheus_text();
            match std::ffi::CString::new(body) {
                Ok(s) => s.into_raw(),
                Err(_) => ptr::null_mut(),
            }
        },
    )
}

// ---- v0.3 active-overflow surface ----

/// True / false for `overflow_enabled` on the adapter. Returns
/// `1` / `0`; returns negative `NET_ERR_*` on null handle.
///
/// # Safety
/// `handle` must be a valid `MeshBlobAdapterHandle*`.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_overflow_enabled(
    handle: *const MeshBlobAdapterHandle,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_overflow_enabled", null_rc, || {
        if handle.is_null() {
            return NetError::NullPointer.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let adapter = Arc::clone(&h.inner);
        if (*adapter).overflow_enabled() {
            1
        } else {
            0
        }
    })
}

/// True / false for `overflow_active` (the hysteresis runtime
/// state). Same return shape as `_overflow_enabled`.
///
/// # Safety
/// `handle` must be a valid `MeshBlobAdapterHandle*`.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_overflow_active(
    handle: *const MeshBlobAdapterHandle,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_overflow_active", null_rc, || {
        if handle.is_null() {
            return NetError::NullPointer.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let adapter = Arc::clone(&h.inner);
        if (*adapter).overflow_active() {
            1
        } else {
            0
        }
    })
}

/// Snapshot the current overflow configuration as a JSON
/// string. Returns a `CString::into_raw`-allocated `*mut c_char`
/// the caller MUST free via [`crate::ffi::net_free_string`].
///
/// # Safety
/// `handle` must be a valid `MeshBlobAdapterHandle*`.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_overflow_config(
    handle: *const MeshBlobAdapterHandle,
) -> *mut c_char {
    adapter_guard(
        "net_mesh_blob_adapter_overflow_config",
        ptr::null_mut(),
        || {
            if handle.is_null() {
                return ptr::null_mut();
            }
            let h = unsafe { &*handle };
            let _op = match h.guard.try_enter() {
                Some(op) => op,
                None => return ptr::null_mut(),
            };
            let adapter = Arc::clone(&h.inner);
            let cfg = (*adapter).overflow_config();
            let json = overflow_to_json(cfg);
            match std::ffi::CString::new(json) {
                Ok(s) => s.into_raw(),
                Err(_) => ptr::null_mut(),
            }
        },
    )
}

/// Flip the overflow master switch. Returns `0` on success,
/// `NET_ERR_*` on null handle.
///
/// # Safety
/// `handle` must be a valid `MeshBlobAdapterHandle*`.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_set_overflow_enabled(
    handle: *const MeshBlobAdapterHandle,
    enabled: c_int,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard(
        "net_mesh_blob_adapter_set_overflow_enabled",
        null_rc,
        || {
            if handle.is_null() {
                return NetError::NullPointer.into();
            }
            let h = unsafe { &*handle };
            let _op = match h.guard.try_enter() {
                Some(op) => op,
                None => return NetError::NullPointer.into(),
            };
            let adapter = Arc::clone(&h.inner);
            (*adapter).set_overflow_enabled(enabled != 0);
            0
        },
    )
}

/// Replace the entire overflow configuration with the JSON
/// shape `config_json`. Returns `0` on success,
/// `NetError::InvalidJson` on malformed input.
///
/// # Safety
/// `handle` + `config_json` must be valid. `config_json` must be a
/// null-terminated UTF-8 JSON string.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_set_overflow_config(
    handle: *const MeshBlobAdapterHandle,
    config_json: *const c_char,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_set_overflow_config", null_rc, || {
        if handle.is_null() || config_json.is_null() {
            return NetError::NullPointer.into();
        }
        let s = match unsafe { c_str_to_owned(config_json) } {
            Some(s) => s,
            None => return NetError::InvalidUtf8.into(),
        };
        let cfg = match parse_overflow_json(&s) {
            Ok(c) => c,
            Err(code) => return code,
        };
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let adapter = Arc::clone(&h.inner);
        (*adapter).set_overflow_config(cfg);
        0
    })
}

// =========================================================================
// v0.3 tree / erasure / range / repair surface (C-ABI parity with the
// Node and Python bindings). Contract:
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, "New C
// ABI contract (S6)". In short: required out-pointers are null-checked
// before anything is written, then every out slot is initialised before
// any later failure; refusals of a well-formed call are
// NET_ERR_BLOB_INVALID_ARGUMENT; structured results are JSON freed with
// net_free_string; byte results are freed with net_blob_free_buffer.
// =========================================================================

/// Write `json` to `*out` as a heap C string (freed with `net_free_string`).
///
/// # Safety
/// `out` must be writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
unsafe fn write_json_out(json: String, out: *mut *mut c_char) -> c_int {
    match std::ffi::CString::new(json) {
        Ok(c) => {
            unsafe { *out = c.into_raw() };
            0
        }
        Err(_) => NetError::InvalidJson.into(),
    }
}

#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
fn hex32(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Decode an encoded ref from a caller buffer. `Err` carries the code.
///
/// # Safety
/// `ptr` points to `len` readable bytes (checked non-null by the caller).
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
unsafe fn decode_ref_arg(ptr: *const u8, len: usize) -> Result<InnerBlobRef, c_int> {
    if len > isize::MAX as usize {
        return Err(NET_ERR_BLOB_INVALID_ARGUMENT);
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    match InnerBlobRef::decode(slice) {
        Ok(Some(b)) => Ok(b),
        _ => Err(NET_ERR_BLOB_DECODE),
    }
}

/// Options object for [`net_mesh_blob_adapter_new_v2`]. Unknown keys are
/// refused, so a misspelled option fails loudly instead of being ignored.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MeshBlobAdapterOptionsJson {
    /// The legacy overflow object, parsed exactly as
    /// [`net_mesh_blob_adapter_new`] parses its `overflow_json`.
    #[serde(default)]
    overflow: Option<OverflowConfigJson>,
    /// Tree-node cache capacity in bytes. Absent: no cache. `0`: a cache
    /// of capacity zero (every lookup misses), which is a distinct state.
    #[serde(default)]
    tree_node_cache_bytes: Option<u64>,
}

/// Construct a `MeshBlobAdapter` with the full option set. Additive: the
/// legacy [`net_mesh_blob_adapter_new`] is unchanged.
///
/// `options_json` may be NULL or empty for the defaults, or a JSON object
/// with `overflow` (the legacy overflow object) and `tree_node_cache_bytes`.
/// On success writes the handle to `*out_handle` and returns `0`; free it
/// with `net_mesh_blob_adapter_free`. Errors: `NetError::NullPointer` (NULL
/// `out_handle` or `redex`), `NetError::InvalidUtf8`,
/// `NetError::InvalidJson` (malformed or unknown option, bad overflow
/// object), `NET_ERR_BLOB_INVALID_ARGUMENT` (a cache capacity this target
/// cannot address), `NetError::ShuttingDown` (redex being freed). On any
/// error `*out_handle` is NULL.
///
/// # Safety
/// `redex` is a live `RedexHandle*`; `adapter_id` and (when non-NULL)
/// `options_json` are NUL-terminated UTF-8; `out_handle` is writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_new_v2(
    redex: *mut super::cortex::RedexHandle,
    adapter_id: *const c_char,
    persistent: c_int,
    options_json: *const c_char,
    out_handle: *mut *mut MeshBlobAdapterHandle,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_new_v2", null_rc, || {
        if out_handle.is_null() {
            return NetError::NullPointer.into();
        }
        unsafe { *out_handle = ptr::null_mut() };
        if redex.is_null() {
            return NetError::NullPointer.into();
        }
        let Some(id) = (unsafe { c_str_to_owned(adapter_id) }) else {
            return NetError::InvalidUtf8.into();
        };
        let opts = if options_json.is_null() {
            MeshBlobAdapterOptionsJson::default()
        } else {
            let Some(text) = (unsafe { c_str_to_owned(options_json) }) else {
                return NetError::InvalidUtf8.into();
            };
            if text.is_empty() {
                MeshBlobAdapterOptionsJson::default()
            } else {
                match serde_json::from_str(&text) {
                    Ok(o) => o,
                    Err(_) => return NetError::InvalidJson.into(),
                }
            }
        };
        let overflow = match opts.overflow.map(overflow_from_raw).transpose() {
            Ok(o) => o,
            Err(code) => return code,
        };
        let cache = match opts.tree_node_cache_bytes.map(usize::try_from).transpose() {
            Ok(c) => c,
            Err(_) => return NET_ERR_BLOB_INVALID_ARGUMENT,
        };
        let Some(redex_inner) = (unsafe { (*redex).redex_arc() }) else {
            return NetError::ShuttingDown.into();
        };
        let mut builder =
            InnerMeshBlobAdapter::new(id, redex_inner).with_persistent(persistent != 0);
        if let Some(cfg) = overflow {
            builder = builder.with_overflow(cfg);
        }
        if let Some(cap) = cache {
            builder = builder.with_tree_node_cache(cap);
        }
        unsafe {
            *out_handle = Box::into_raw(Box::new(MeshBlobAdapterHandle {
                inner: ManuallyDrop::new(Arc::new(builder)),
                guard: HandleGuard::new(),
            }));
        }
        0
    })
}

/// Fetch the half-open byte range `[start, end)` of the blob named by the
/// encoded ref. Works for every ref shape, including tree refs that
/// [`net_mesh_blob_adapter_fetch`] refuses; partial ranges are not
/// verified against the whole-content hash (as in the Node and Python
/// bindings).
///
/// Checks run in core's order and accept or refuse exactly as core does:
/// `start > end` is refused; `start == end` succeeds with `(NULL, 0)`, even
/// beyond the blob's size; a non-empty range longer than
/// `MAX_FETCH_RANGE_BYTES` (1 GiB) or ending past the blob's size is
/// refused. Where core reports those refusals as a generic backend error,
/// this returns `NET_ERR_BLOB_INVALID_ARGUMENT`.
///
/// # Safety
/// `handle` is a live handle; `blob_ref_bytes` points to `blob_ref_len`
/// readable bytes; `out_data` / `out_len` are writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_fetch_range(
    handle: *const MeshBlobAdapterHandle,
    blob_ref_bytes: *const u8,
    blob_ref_len: usize,
    start: u64,
    end: u64,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_fetch_range", null_rc, || {
        if out_data.is_null() || out_len.is_null() {
            return NetError::NullPointer.into();
        }
        unsafe {
            *out_data = ptr::null_mut();
            *out_len = 0;
        }
        if handle.is_null() || blob_ref_bytes.is_null() {
            return NetError::NullPointer.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let blob_ref = match unsafe { decode_ref_arg(blob_ref_bytes, blob_ref_len) } {
            Ok(b) => b,
            Err(code) => return code,
        };
        if start > end {
            return NET_ERR_BLOB_INVALID_ARGUMENT;
        }
        if start == end {
            return 0;
        }
        if end - start > crate::adapter::net::dataforts::blob::mesh::MAX_FETCH_RANGE_BYTES
            || end > blob_ref.size()
        {
            return NET_ERR_BLOB_INVALID_ARGUMENT;
        }
        let adapter = Arc::clone(&h.inner);
        match block_on(async move { (*adapter).fetch_range(&blob_ref, start..end).await }) {
            Ok(bytes) => unsafe { write_bytes_out(&bytes, out_data, out_len) },
            Err(e) => err_to_code(&e),
        }
    })
}

/// Store `data` as a tree blob with default chunking and write the encoded
/// tree ref. `encoding_kind`: `0` Replicated (then `rs_k` and `rs_m` must
/// both be 0), `1` Reed-Solomon (`rs_k == rs_m == 0` selects the core
/// defaults; otherwise both are at least 1 and `rs_k + rs_m <= 255`).
/// Anything else is `NET_ERR_BLOB_INVALID_ARGUMENT`. Reed-Solomon closes a
/// stripe only at `k` full chunks; a short trailing stripe is stored
/// Replicated.
///
/// # Safety
/// `handle` is a live handle; `data` points to `data_len` readable bytes
/// (or is NULL with `data_len == 0`); `out_ref` / `out_ref_len` are
/// writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_store_tree(
    handle: *const MeshBlobAdapterHandle,
    data: *const u8,
    data_len: usize,
    encoding_kind: u8,
    rs_k: u8,
    rs_m: u8,
    out_ref: *mut *mut u8,
    out_ref_len: *mut usize,
) -> c_int {
    use crate::adapter::net::dataforts::blob::blob_tree::ChunkingStrategy;
    use crate::adapter::net::dataforts::blob::erasure::{DEFAULT_RS_K, DEFAULT_RS_M};
    use crate::adapter::net::dataforts::Encoding as InnerEncoding;

    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_store_tree", null_rc, || {
        if out_ref.is_null() || out_ref_len.is_null() {
            return NetError::NullPointer.into();
        }
        unsafe {
            *out_ref = ptr::null_mut();
            *out_ref_len = 0;
        }
        if handle.is_null() || (data.is_null() && data_len > 0) {
            return NetError::NullPointer.into();
        }
        if data_len > isize::MAX as usize {
            return NET_ERR_BLOB_INVALID_ARGUMENT;
        }
        let encoding = match (encoding_kind, rs_k, rs_m) {
            (0, 0, 0) => InnerEncoding::Replicated,
            (1, 0, 0) => InnerEncoding::ReedSolomon {
                k: DEFAULT_RS_K,
                m: DEFAULT_RS_M,
            },
            (1, k, m) if k >= 1 && m >= 1 && (u16::from(k) + u16::from(m)) <= 255 => {
                InnerEncoding::ReedSolomon { k, m }
            }
            _ => return NET_ERR_BLOB_INVALID_ARGUMENT,
        };
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let owned = if data_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(data, data_len) }.to_vec()
        };
        let adapter = Arc::clone(&h.inner);
        let result = block_on(async move {
            let stream =
                futures::stream::once(async move { Ok::<_, InnerBlobError>(Bytes::from(owned)) });
            (*adapter)
                .store_stream_tree(Box::pin(stream), encoding, ChunkingStrategy::default())
                .await
        });
        match result {
            Ok(blob_ref) => unsafe { write_bytes_out(&blob_ref.encode(), out_ref, out_ref_len) },
            Err(e) => err_to_code(&e),
        }
    })
}

/// Repair a Reed-Solomon tree blob in place: rebuild missing data chunks
/// from parity and re-store them. Writes the report as JSON (all fields
/// `u64`): `stripes_walked`, `stripes_already_healthy`, `stripes_repaired`,
/// `chunks_restored`, `stripes_unrecoverable`,
/// `replicated_stripes_skipped`, `replicated_leaves_skipped`. A stripe that
/// cannot be rebuilt is counted, not an error: success does not mean the
/// blob is whole.
///
/// # Safety
/// As [`net_mesh_blob_adapter_fetch_range`]; `out_json` is writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_repair_blob(
    handle: *const MeshBlobAdapterHandle,
    blob_ref_bytes: *const u8,
    blob_ref_len: usize,
    out_json: *mut *mut c_char,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_repair_blob", null_rc, || {
        if out_json.is_null() {
            return NetError::NullPointer.into();
        }
        unsafe { *out_json = ptr::null_mut() };
        if handle.is_null() || blob_ref_bytes.is_null() {
            return NetError::NullPointer.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let blob_ref = match unsafe { decode_ref_arg(blob_ref_bytes, blob_ref_len) } {
            Ok(b) => b,
            Err(code) => return code,
        };
        let adapter = Arc::clone(&h.inner);
        match block_on(async move { adapter.repair_blob(&blob_ref).await }) {
            Ok(r) => {
                let json = serde_json::json!({
                    "stripes_walked": r.stripes_walked,
                    "stripes_already_healthy": r.stripes_already_healthy,
                    "stripes_repaired": r.stripes_repaired,
                    "chunks_restored": r.chunks_restored,
                    "stripes_unrecoverable": r.stripes_unrecoverable,
                    "replicated_stripes_skipped": r.replicated_stripes_skipped,
                    "replicated_leaves_skipped": r.replicated_leaves_skipped,
                });
                unsafe { write_json_out(json.to_string(), out_json) }
            }
            Err(e) => err_to_code(&e),
        }
    })
}

/// Tree-node cache statistics as JSON: `{"hits","misses","bytes","entries"}`
/// (all `u64`), or the JSON literal `null` when the adapter was built
/// without a cache. Either way returns `0`.
///
/// # Safety
/// `handle` is a live handle; `out_json` is writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_tree_node_cache_stats(
    handle: *const MeshBlobAdapterHandle,
    out_json: *mut *mut c_char,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard(
        "net_mesh_blob_adapter_tree_node_cache_stats",
        null_rc,
        || {
            if out_json.is_null() {
                return NetError::NullPointer.into();
            }
            unsafe { *out_json = ptr::null_mut() };
            if handle.is_null() {
                return NetError::NullPointer.into();
            }
            let h = unsafe { &*handle };
            let _op = match h.guard.try_enter() {
                Some(op) => op,
                None => return NetError::NullPointer.into(),
            };
            let json = match h.inner.tree_node_cache_stats() {
                Some((hits, misses, bytes, entries)) => serde_json::json!({
                    "hits": hits,
                    "misses": misses,
                    "bytes": bytes as u64,
                    "entries": entries as u64,
                }),
                None => serde_json::Value::Null,
            };
            unsafe { write_json_out(json.to_string(), out_json) }
        },
    )
}

/// Describe an encoded ref as JSON. Always present: `version` (u8), `uri`,
/// `size` (u64), `is_tree`, `is_chunked`. Present only where the shape has
/// them (absent, never zero-filled, otherwise): `hash` (lowercase hex, small
/// refs), `tree_root_hash` (hex) and `tree_depth` (u8) for tree refs, and
/// `encoding` (`{"kind":"replicated"}` or
/// `{"kind":"reed_solomon","k":..,"m":..}`) for chunked refs.
///
/// # Safety
/// `encoded` points to `encoded_len` readable bytes; `out_json` is
/// writable.
#[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_ref_describe(
    encoded: *const u8,
    encoded_len: usize,
    out_json: *mut *mut c_char,
) -> c_int {
    use crate::adapter::net::dataforts::Encoding as InnerEncoding;

    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_blob_ref_describe", null_rc, || {
        if out_json.is_null() {
            return NetError::NullPointer.into();
        }
        unsafe { *out_json = ptr::null_mut() };
        if encoded.is_null() {
            return NetError::NullPointer.into();
        }
        let blob_ref = match unsafe { decode_ref_arg(encoded, encoded_len) } {
            Ok(b) => b,
            Err(code) => return code,
        };
        let mut obj = serde_json::Map::new();
        obj.insert("version".into(), blob_ref.version().into());
        obj.insert("uri".into(), blob_ref.uri().into());
        obj.insert("size".into(), blob_ref.size().into());
        obj.insert("is_tree".into(), blob_ref.is_tree().into());
        obj.insert("is_chunked".into(), blob_ref.is_chunked().into());
        if let Some(h) = blob_ref.small_hash() {
            obj.insert("hash".into(), hex32(h).into());
        }
        if let Some(h) = blob_ref.tree_root_hash() {
            obj.insert("tree_root_hash".into(), hex32(h).into());
        }
        if let Some(d) = blob_ref.tree_depth() {
            obj.insert("tree_depth".into(), d.into());
        }
        if let Some(enc) = blob_ref.encoding() {
            let v = match enc {
                InnerEncoding::Replicated => serde_json::json!({ "kind": "replicated" }),
                InnerEncoding::ReedSolomon { k, m } => {
                    serde_json::json!({ "kind": "reed_solomon", "k": k, "m": m })
                }
            };
            obj.insert("encoding".into(), v);
        }
        unsafe { write_json_out(serde_json::Value::Object(obj).to_string(), out_json) }
    })
}

/// TEST SEAM (`fixtures` only, absent from production builds): make one
/// data shard of a Reed-Solomon tree blob unavailable, for the repair
/// witness. Walks to the first erasure leaf, picks data chunk `data_index`
/// of stripe `stripe_index`, deletes it through the adapter's own deletion
/// path (which also drops its cached tree-node and chunk-file entries),
/// then confirms it no longer fetches. Writes its 32-byte hash to
/// `out_hash`. `NET_ERR_BLOB_INVALID_ARGUMENT` for a non-tree ref, a
/// non-erasure tree, or an index out of range; `NET_ERR_BLOB_BACKEND` if
/// the chunk still fetches after deletion.
///
/// # Safety
/// As [`net_mesh_blob_adapter_fetch_range`]; `out_hash` has 32 writable
/// bytes.
#[cfg(all(
    feature = "dataforts",
    feature = "netdb",
    feature = "redex-disk",
    feature = "fixtures"
))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_test_drop_data_chunk(
    handle: *const MeshBlobAdapterHandle,
    blob_ref_bytes: *const u8,
    blob_ref_len: usize,
    stripe_index: u32,
    data_index: u32,
    out_hash: *mut u8,
) -> c_int {
    use crate::adapter::net::dataforts::blob::blob_tree::TreeNode;

    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard(
        "net_mesh_blob_adapter_test_drop_data_chunk",
        null_rc,
        || {
            if handle.is_null() || blob_ref_bytes.is_null() || out_hash.is_null() {
                return NetError::NullPointer.into();
            }
            let h = unsafe { &*handle };
            let _op = match h.guard.try_enter() {
                Some(op) => op,
                None => return NetError::NullPointer.into(),
            };
            let blob_ref = match unsafe { decode_ref_arg(blob_ref_bytes, blob_ref_len) } {
                Ok(b) => b,
                Err(code) => return code,
            };
            let Some(root) = blob_ref.tree_root_hash().copied() else {
                return NET_ERR_BLOB_INVALID_ARGUMENT;
            };
            let adapter = Arc::clone(&h.inner);
            let outcome: Result<[u8; 32], c_int> = block_on(async move {
                let code = |e: InnerBlobError| err_to_code(&e);
                let root_bytes = adapter.fetch_chunk(&root).await.map_err(code)?;
                let mut node = TreeNode::decode(&root_bytes).map_err(code)?;
                let stripes = loop {
                    match node {
                        TreeNode::ErasureLeaf { stripes } => break stripes,
                        TreeNode::Internal { children } => {
                            let Some((child, _)) = children.first() else {
                                return Err(NET_ERR_BLOB_INVALID_ARGUMENT);
                            };
                            let bytes = adapter.fetch_chunk(child).await.map_err(code)?;
                            node = TreeNode::decode(&bytes).map_err(code)?;
                        }
                        _ => return Err(NET_ERR_BLOB_INVALID_ARGUMENT),
                    }
                };
                let stripe = stripes
                    .get(stripe_index as usize)
                    .ok_or(NET_ERR_BLOB_INVALID_ARGUMENT)?;
                let hash = stripe
                    .chunks
                    .iter()
                    .filter(|c| c.is_data())
                    .nth(data_index as usize)
                    .map(|c| c.hash)
                    .ok_or(NET_ERR_BLOB_INVALID_ARGUMENT)?;
                adapter.delete_chunk(&hash).await.map_err(code)?;
                if adapter.fetch_chunk(&hash).await.is_ok() {
                    return Err(NET_ERR_BLOB_BACKEND);
                }
                Ok(hash)
            });
            match outcome {
                Ok(hash) => {
                    unsafe { ptr::copy_nonoverlapping(hash.as_ptr(), out_hash, 32) };
                    0
                }
                Err(code) => code,
            }
        },
    )
}

/// TEST SEAM (`fixtures` only): `1` if the chunk with this 32-byte hash
/// fetches from the adapter, `0` if not, negative on error.
///
/// # Safety
/// `handle` is a live handle; `hash` points to 32 readable bytes.
#[cfg(all(
    feature = "dataforts",
    feature = "netdb",
    feature = "redex-disk",
    feature = "fixtures"
))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_test_chunk_present(
    handle: *const MeshBlobAdapterHandle,
    hash: *const u8,
) -> c_int {
    let null_rc: c_int = NetError::NullPointer.into();
    adapter_guard("net_mesh_blob_adapter_test_chunk_present", null_rc, || {
        if handle.is_null() || hash.is_null() {
            return NetError::NullPointer.into();
        }
        let h = unsafe { &*handle };
        let _op = match h.guard.try_enter() {
            Some(op) => op,
            None => return NetError::NullPointer.into(),
        };
        let mut key = [0u8; 32];
        unsafe { ptr::copy_nonoverlapping(hash, key.as_mut_ptr(), 32) };
        let adapter = Arc::clone(&h.inner);
        c_int::from(block_on(
            async move { adapter.fetch_chunk(&key).await.is_ok() },
        ))
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "test code legitimately uses std::sync::{Mutex,RwLock} for SUT setup; tests have no real poison concern"
    )]
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_id(prefix: &str) -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        format!("{}-{}-{}", prefix, std::process::id(), n)
    }

    /// End-to-end: register FS adapter, publish, resolve, free.
    /// Pins the contract on the symbols Go / C consumers will use.
    #[test]
    fn ffi_publish_resolve_round_trip() {
        let id = unique_id("ffi-blob");
        let root = std::env::temp_dir().join(format!("net-ffi-blob-{}", id));
        let id_c = CString::new(id.clone()).unwrap();
        let root_c = CString::new(root.to_string_lossy().as_ref()).unwrap();
        let uri_c = CString::new("file:///ffi-round-trip").unwrap();

        unsafe {
            assert_eq!(
                net_blob_register_fs_adapter(id_c.as_ptr(), root_c.as_ptr()),
                0
            );
            assert_eq!(net_blob_adapter_registered(id_c.as_ptr()), 1);

            let payload = b"end-to-end ffi blob round trip";
            let mut out_buf: *mut u8 = std::ptr::null_mut();
            let mut out_len: usize = 0;
            let rc = net_blob_publish(
                id_c.as_ptr(),
                uri_c.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                &mut out_buf,
                &mut out_len,
            );
            assert_eq!(rc, 0);
            assert!(!out_buf.is_null());
            // First bytes are the BlobRef magic.
            let encoded = std::slice::from_raw_parts(out_buf, out_len);
            assert_eq!(
                &encoded[..4],
                &crate::adapter::net::dataforts::BLOB_REF_MAGIC,
            );

            // Resolve back through the same adapter.
            let mut content_buf: *mut u8 = std::ptr::null_mut();
            let mut content_len: usize = 0;
            let rc = net_blob_resolve(
                id_c.as_ptr(),
                out_buf,
                out_len,
                &mut content_buf,
                &mut content_len,
            );
            assert_eq!(rc, 0);
            let resolved = std::slice::from_raw_parts(content_buf, content_len);
            assert_eq!(resolved, payload);

            net_blob_free_buffer(out_buf, out_len);
            net_blob_free_buffer(content_buf, content_len);
            assert_eq!(net_blob_unregister_adapter(id_c.as_ptr()), 1);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ffi_resolve_returns_not_registered_for_unknown_adapter() {
        let id_c = CString::new("never-registered").unwrap();
        let payload = b"any";
        let mut out_buf: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = unsafe {
            net_blob_resolve(
                id_c.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                &mut out_buf,
                &mut out_len,
            )
        };
        assert_eq!(rc, NET_ERR_BLOB_NOT_REGISTERED);
        assert!(out_buf.is_null());
        assert_eq!(out_len, 0);
    }

    /// Round-trip an `net_blob_register_callback_adapter`-registered
    /// adapter: publish bytes through the vtable, then resolve them
    /// back. The vtable's `fetch` returns bytes from a static map
    /// indexed by the BLAKE3 hash; the substrate-side hash check
    /// validates the round trip.
    mod callback_adapter_round_trip {
        use super::*;
        use std::collections::HashMap;
        use std::sync::Mutex;

        #[repr(C)]
        struct CallbackCtx {
            store: Mutex<HashMap<[u8; 32], Vec<u8>>>,
        }

        unsafe extern "C" fn cb_store(
            ctx: *mut c_void,
            _uri: *const c_char,
            hash: *const u8,
            _size: u64,
            data: *const u8,
            data_len: usize,
        ) -> c_int {
            let ctx = &*(ctx as *const CallbackCtx);
            let mut h = [0u8; 32];
            h.copy_from_slice(std::slice::from_raw_parts(hash, 32));
            let buf = if data_len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data, data_len).to_vec()
            };
            ctx.store.lock().unwrap().insert(h, buf);
            0
        }

        unsafe extern "C" fn cb_fetch(
            ctx: *mut c_void,
            _uri: *const c_char,
            hash: *const u8,
            _size: u64,
            out_data: *mut *mut u8,
            out_len: *mut usize,
        ) -> c_int {
            let ctx = &*(ctx as *const CallbackCtx);
            let mut h = [0u8; 32];
            h.copy_from_slice(std::slice::from_raw_parts(hash, 32));
            let store = ctx.store.lock().unwrap();
            match store.get(&h) {
                Some(bytes) => {
                    let boxed = bytes.clone().into_boxed_slice();
                    let len = boxed.len();
                    let ptr = Box::into_raw(boxed) as *mut u8;
                    *out_data = ptr;
                    *out_len = len;
                    0
                }
                None => NET_ERR_BLOB_NOT_FOUND,
            }
        }

        unsafe extern "C" fn cb_fetch_range(
            ctx: *mut c_void,
            _uri: *const c_char,
            hash: *const u8,
            _size: u64,
            range_start: u64,
            range_end: u64,
            out_data: *mut *mut u8,
            out_len: *mut usize,
        ) -> c_int {
            let ctx = &*(ctx as *const CallbackCtx);
            let mut h = [0u8; 32];
            h.copy_from_slice(std::slice::from_raw_parts(hash, 32));
            let store = ctx.store.lock().unwrap();
            match store.get(&h) {
                Some(bytes) => {
                    let s = range_start as usize;
                    let e = range_end as usize;
                    if s > e || e > bytes.len() {
                        return NET_ERR_BLOB_BACKEND;
                    }
                    let slice = bytes[s..e].to_vec().into_boxed_slice();
                    let len = slice.len();
                    *out_data = Box::into_raw(slice) as *mut u8;
                    *out_len = len;
                    0
                }
                None => NET_ERR_BLOB_NOT_FOUND,
            }
        }

        unsafe extern "C" fn cb_exists(
            ctx: *mut c_void,
            _uri: *const c_char,
            hash: *const u8,
            _size: u64,
            out_exists: *mut c_int,
        ) -> c_int {
            let ctx = &*(ctx as *const CallbackCtx);
            let mut h = [0u8; 32];
            h.copy_from_slice(std::slice::from_raw_parts(hash, 32));
            *out_exists = if ctx.store.lock().unwrap().contains_key(&h) {
                1
            } else {
                0
            };
            0
        }

        unsafe extern "C" fn cb_free(_ctx: *mut c_void, data: *mut u8, len: usize) {
            if data.is_null() {
                return;
            }
            let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(data, len));
        }

        #[test]
        fn callback_adapter_publish_resolve_round_trip() {
            let ctx = Box::new(CallbackCtx {
                store: Mutex::new(HashMap::new()),
            });
            let ctx_ptr = Box::into_raw(ctx) as *mut c_void;
            let vtable = NetBlobAdapterVtable {
                store: cb_store,
                fetch: cb_fetch,
                fetch_range: cb_fetch_range,
                exists: cb_exists,
                free_buffer: cb_free,
            };

            let id_c = std::ffi::CString::new("ffi-cb-roundtrip").unwrap();
            let uri_c = std::ffi::CString::new("cb://round-trip").unwrap();
            unsafe {
                assert_eq!(
                    net_blob_register_callback_adapter(id_c.as_ptr(), &vtable, ctx_ptr),
                    0
                );

                let payload = b"vtable round-trip payload";
                let mut out_buf: *mut u8 = std::ptr::null_mut();
                let mut out_len: usize = 0;
                let rc = net_blob_publish(
                    id_c.as_ptr(),
                    uri_c.as_ptr(),
                    payload.as_ptr(),
                    payload.len(),
                    &mut out_buf,
                    &mut out_len,
                );
                assert_eq!(rc, 0);

                let mut content_buf: *mut u8 = std::ptr::null_mut();
                let mut content_len: usize = 0;
                let rc = net_blob_resolve(
                    id_c.as_ptr(),
                    out_buf,
                    out_len,
                    &mut content_buf,
                    &mut content_len,
                );
                assert_eq!(rc, 0);
                let resolved = std::slice::from_raw_parts(content_buf, content_len);
                assert_eq!(resolved, payload);

                net_blob_free_buffer(out_buf, out_len);
                net_blob_free_buffer(content_buf, content_len);
                assert_eq!(net_blob_unregister_adapter(id_c.as_ptr()), 1);

                // Reclaim the leaked ctx box.
                drop(Box::from_raw(ctx_ptr as *mut CallbackCtx));
            }
        }

        // ---- owned-context registration (plan gap G-B / S5b) ----------

        /// Counts release_fn calls for one registration's context.
        #[repr(C)]
        struct OwnedCtx {
            inner: CallbackCtx,
            released: std::sync::atomic::AtomicUsize,
        }

        unsafe extern "C" fn owned_release(ctx: *mut c_void) {
            let ctx = &*(ctx as *const OwnedCtx);
            ctx.released
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        // The vtable entries reinterpret ctx as CallbackCtx; OwnedCtx is
        // repr(C) with CallbackCtx first, so the same pointer serves both.
        fn owned_ctx() -> *mut OwnedCtx {
            Box::into_raw(Box::new(OwnedCtx {
                inner: CallbackCtx {
                    store: Mutex::new(HashMap::new()),
                },
                released: std::sync::atomic::AtomicUsize::new(0),
            }))
        }

        fn released(ctx: *mut OwnedCtx) -> usize {
            unsafe { &*ctx }
                .released
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn vtable() -> NetBlobAdapterVtable {
            NetBlobAdapterVtable {
                store: cb_store,
                fetch: cb_fetch,
                fetch_range: cb_fetch_range,
                exists: cb_exists,
                free_buffer: cb_free,
            }
        }

        /// Publish `payload` through adapter `id`, returning the encoded ref.
        fn publish(id: &std::ffi::CStr, payload: &[u8]) -> Vec<u8> {
            let uri = std::ffi::CString::new("cb://owned").unwrap();
            let mut out: *mut u8 = std::ptr::null_mut();
            let mut len = 0usize;
            let rc = unsafe {
                net_blob_publish(
                    id.as_ptr(),
                    uri.as_ptr(),
                    payload.as_ptr(),
                    payload.len(),
                    &mut out,
                    &mut len,
                )
            };
            assert_eq!(rc, 0);
            let v = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
            unsafe { net_blob_free_buffer(out, len) };
            v
        }

        fn resolve(id: &std::ffi::CStr, encoded: &[u8]) -> c_int {
            let mut out: *mut u8 = std::ptr::null_mut();
            let mut len = 0usize;
            let rc = unsafe {
                net_blob_resolve(
                    id.as_ptr(),
                    encoded.as_ptr(),
                    encoded.len(),
                    &mut out,
                    &mut len,
                )
            };
            if rc == 0 {
                unsafe { net_blob_free_buffer(out, len) };
            }
            rc
        }

        /// Barrier tests share process-global state; serialize them (CI
        /// runs lib tests with `cargo test`, i.e. threads in one process).
        static BARRIER_TESTS: Mutex<()> = Mutex::new(());

        #[test]
        fn owned_ctx_releases_once_on_unregister_and_never_on_refusal() {
            let id = std::ffi::CString::new("ffi-cb-owned-basic").unwrap();
            let ctx = owned_ctx();
            let vt = vtable();
            unsafe {
                assert_eq!(
                    net_blob_register_callback_adapter_owned(
                        id.as_ptr(),
                        &vt,
                        ctx as *mut c_void,
                        Some(owned_release)
                    ),
                    0
                );
            }
            // A second registration under the same id is refused: its
            // context stays the caller's and is never released.
            let dup = owned_ctx();
            unsafe {
                assert_eq!(
                    net_blob_register_callback_adapter_owned(
                        id.as_ptr(),
                        &vt,
                        dup as *mut c_void,
                        Some(owned_release)
                    ),
                    NET_ERR_BLOB_DUPLICATE_ID
                );
            }
            assert_eq!(
                released(dup),
                0,
                "a refused registration must not release ctx"
            );
            // A NULL release_fn is refused too.
            unsafe {
                assert_eq!(
                    net_blob_register_callback_adapter_owned(
                        id.as_ptr(),
                        &vt,
                        dup as *mut c_void,
                        None
                    ),
                    c_int::from(NetError::NullPointer)
                );
            }

            let encoded = publish(&id, b"owned-basic");
            assert_eq!(resolve(&id, &encoded), 0);
            assert_eq!(released(ctx), 0, "registered and idle: not released");
            assert_eq!(unsafe { net_blob_unregister_adapter(id.as_ptr()) }, 1);
            assert_eq!(released(ctx), 1, "released exactly once after unregister");
            assert_eq!(released(dup), 0);
            unsafe {
                drop(Box::from_raw(ctx));
                drop(Box::from_raw(dup));
            }
        }

        /// Unregister while a fetch is held inside the callback: release
        /// waits for the in-flight call, then fires exactly once.
        #[test]
        fn owned_ctx_release_waits_for_a_held_fetch() {
            let _serial = BARRIER_TESTS.lock().unwrap_or_else(|e| e.into_inner());
            for stage in [1usize, 2] {
                let id = std::ffi::CString::new(format!("ffi-cb-owned-held-{stage}")).unwrap();
                let ctx = owned_ctx();
                let vt = vtable();
                unsafe {
                    assert_eq!(
                        net_blob_register_callback_adapter_owned(
                            id.as_ptr(),
                            &vt,
                            ctx as *mut c_void,
                            Some(owned_release)
                        ),
                        0
                    );
                }
                let encoded = publish(&id, format!("held at {stage}").as_bytes());

                crate::ffi::blob::callback_barrier::arm(stage, ctx as usize);
                let id2 = id.clone();
                let enc2 = encoded.clone();
                let worker = std::thread::spawn(move || resolve(&id2, &enc2));
                assert!(
                    crate::ffi::blob::callback_barrier::wait_held(
                        stage,
                        std::time::Duration::from_secs(5)
                    ),
                    "stage {stage}: never held"
                );

                assert_eq!(unsafe { net_blob_unregister_adapter(id.as_ptr()) }, 1);
                assert_eq!(
                    released(ctx),
                    0,
                    "stage {stage}: released while a call still held ctx"
                );

                crate::ffi::blob::callback_barrier::release(stage);
                assert_eq!(
                    worker.join().unwrap(),
                    0,
                    "stage {stage}: the held fetch must complete"
                );
                // The worker's resolve returned after the blocking task
                // dropped its reference, so release has run.
                assert_eq!(
                    released(ctx),
                    1,
                    "stage {stage}: released exactly once after the call"
                );
                unsafe { drop(Box::from_raw(ctx)) };
            }
        }

        /// The awaiting future is dropped while the blocking callback is
        /// held; release still runs exactly once, after the callback.
        #[test]
        fn owned_ctx_release_once_when_the_future_is_cancelled() {
            let _serial = BARRIER_TESTS.lock().unwrap_or_else(|e| e.into_inner());
            let ctx = owned_ctx();
            let payload = b"cancelled mid-callback";
            let hash = *blake3::hash(payload).as_bytes();
            unsafe { &*ctx }
                .inner
                .store
                .lock()
                .unwrap()
                .insert(hash, payload.to_vec());
            let octx = Arc::new(OpaqueCtx::owned(ctx as *mut c_void, owned_release));
            octx.arm();
            let adapter = CallbackBlobAdapter {
                id: "ffi-cb-owned-cancel".into(),
                vtable: vtable(),
                ctx: Arc::clone(&octx),
            };
            drop(octx);
            let blob = crate::adapter::net::dataforts::BlobRef::small(
                "cb://cancel",
                hash,
                payload.len() as u64,
            );

            let rt = tokio::runtime::Runtime::new().unwrap();
            crate::ffi::blob::callback_barrier::arm(1, ctx as usize);
            let task = rt.spawn(async move {
                let _ = adapter.fetch(&blob).await;
            });
            assert!(crate::ffi::blob::callback_barrier::wait_held(
                1,
                std::time::Duration::from_secs(5)
            ));
            task.abort(); // cancel the awaiting future; drops the adapter
            rt.block_on(async {
                let _ = task.await;
            });
            assert_eq!(released(ctx), 0, "the blocking callback still holds ctx");
            crate::ffi::blob::callback_barrier::release(1);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while released(ctx) == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                released(ctx),
                1,
                "released exactly once after the cancelled call finished"
            );
            drop(rt);
            assert_eq!(released(ctx), 1);
            unsafe { drop(Box::from_raw(ctx)) };
        }
    }

    #[test]
    fn ffi_duplicate_registration_rejected() {
        let id = unique_id("ffi-dup");
        let root = std::env::temp_dir().join(format!("net-ffi-blob-{}", id));
        let id_c = CString::new(id.clone()).unwrap();
        let root_c = CString::new(root.to_string_lossy().as_ref()).unwrap();
        unsafe {
            assert_eq!(
                net_blob_register_fs_adapter(id_c.as_ptr(), root_c.as_ptr()),
                0
            );
            assert_eq!(
                net_blob_register_fs_adapter(id_c.as_ptr(), root_c.as_ptr()),
                NET_ERR_BLOB_DUPLICATE_ID
            );
            assert_eq!(net_blob_unregister_adapter(id_c.as_ptr()), 1);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression for the `MeshBlobAdapterHandle` use-after-free /
    /// double-free (security audit H1). Pre-fix the handle had no
    /// `HandleGuard`; `_free` did an unconditional `Box::from_raw`,
    /// so an op racing `_free` read freed memory and a second `_free`
    /// was a double-free.
    ///
    /// Post-fix the box is leaked on `_free` (only the inner is
    /// dropped) and every op gates on `guard.try_enter()`. This makes
    /// two properties observable + deterministic:
    ///   1. An op on a freed handle bails with the null-pointer code
    ///      (reading the still-valid leaked guard) instead of UB.
    ///   2. A second `_free` is a no-op (single-winner `begin_free`),
    ///      not a double-free.
    #[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
    #[test]
    fn blob_adapter_ops_after_free_bail_and_double_free_is_safe() {
        use crate::ffi::cortex::{net_redex_free, net_redex_new};

        let null_rc: c_int = NetError::NullPointer.into();
        let id_c = CString::new(unique_id("ffi-blob-adapter-uaf")).unwrap();

        unsafe {
            // In-memory redex (NULL persistent_dir) → no disk needed.
            let redex = net_redex_new(std::ptr::null());
            assert!(!redex.is_null());

            // persistent = 0 (in-memory chunks), overflow_json = NULL.
            let adapter = net_mesh_blob_adapter_new(redex, id_c.as_ptr(), 0, std::ptr::null());
            assert!(!adapter.is_null(), "adapter must construct");

            // While live, the metrics accessors return valid results.
            let live = net_mesh_blob_adapter_overflow_enabled(adapter);
            assert!(live == 0 || live == 1, "live overflow_enabled in {{0,1}}");

            // Free once.
            net_mesh_blob_adapter_free(adapter);

            // Ops on the freed handle must bail (guard.freeing == true →
            // try_enter == None), NOT UAF. The leaked box keeps the
            // guard readable.
            assert_eq!(
                net_mesh_blob_adapter_overflow_enabled(adapter),
                null_rc,
                "op on freed handle must return the null-pointer bail code",
            );
            assert_eq!(net_mesh_blob_adapter_overflow_active(adapter), null_rc);
            assert!(
                net_mesh_blob_adapter_prometheus_text(adapter).is_null(),
                "ptr-returning op on freed handle must return null",
            );
            let blob_ref = [0u8; 4];
            assert_eq!(
                net_mesh_blob_adapter_store(
                    adapter,
                    blob_ref.as_ptr(),
                    blob_ref.len(),
                    std::ptr::null(),
                    0,
                ),
                null_rc,
                "store on freed handle must bail, not run against freed inner",
            );

            // Double free must be safe (single-winner begin_free).
            net_mesh_blob_adapter_free(adapter);

            // NULL handle is a no-op for every entry point.
            net_mesh_blob_adapter_free(std::ptr::null_mut());
            assert_eq!(
                net_mesh_blob_adapter_overflow_enabled(std::ptr::null()),
                null_rc,
            );

            net_redex_free(redex);
        }
    }

    /// Contract witnesses for the v0.3 entry points: every row of the S6
    /// table in GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md ("New C ABI
    /// contract (S6)").
    #[cfg(all(feature = "dataforts", feature = "netdb", feature = "redex-disk"))]
    mod v3_contract {
        use super::super::*;
        use crate::ffi::cortex::{net_redex_free, net_redex_new, RedexHandle};
        use crate::ffi::net_free_string;
        use std::ffi::{CStr, CString};

        const NULL_RC: c_int = -1;
        const INVALID_JSON: c_int = -3;

        struct Fixture {
            redex: *mut RedexHandle,
            adapter: *mut MeshBlobAdapterHandle,
        }

        impl Fixture {
            fn new(options: Option<&str>) -> Self {
                let id = CString::new(super::unique_id("ffi-v3")).unwrap();
                let opts = options.map(|o| CString::new(o).unwrap());
                unsafe {
                    let redex = net_redex_new(std::ptr::null());
                    assert!(!redex.is_null());
                    let mut adapter = std::ptr::null_mut();
                    let rc = net_mesh_blob_adapter_new_v2(
                        redex,
                        id.as_ptr(),
                        0,
                        opts.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                        &mut adapter,
                    );
                    assert_eq!(rc, 0, "new_v2({options:?})");
                    assert!(!adapter.is_null());
                    Fixture { redex, adapter }
                }
            }

            /// Store `data` as a tree and return the encoded ref.
            fn store_tree(&self, data: &[u8], kind: u8, k: u8, m: u8) -> Result<Vec<u8>, c_int> {
                let mut out = std::ptr::null_mut();
                let mut len = 0usize;
                let rc = unsafe {
                    net_mesh_blob_adapter_store_tree(
                        self.adapter,
                        data.as_ptr(),
                        data.len(),
                        kind,
                        k,
                        m,
                        &mut out,
                        &mut len,
                    )
                };
                if rc != 0 {
                    assert!(
                        out.is_null() && len == 0,
                        "failed store_tree left its outputs set"
                    );
                    return Err(rc);
                }
                let v = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
                unsafe { net_blob_free_buffer(out, len) };
                Ok(v)
            }

            fn range(&self, r: &[u8], start: u64, end: u64) -> Result<Vec<u8>, c_int> {
                let mut out = std::ptr::dangling_mut::<u8>(); // poison: must be reset
                let mut len = 7usize;
                let rc = unsafe {
                    net_mesh_blob_adapter_fetch_range(
                        self.adapter,
                        r.as_ptr(),
                        r.len(),
                        start,
                        end,
                        &mut out,
                        &mut len,
                    )
                };
                if rc != 0 {
                    assert!(
                        out.is_null() && len == 0,
                        "failed fetch_range left its outputs set"
                    );
                    return Err(rc);
                }
                if len == 0 {
                    assert!(out.is_null(), "empty result must be (NULL, 0)");
                    return Ok(Vec::new());
                }
                let v = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
                unsafe { net_blob_free_buffer(out, len) };
                Ok(v)
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                unsafe {
                    net_mesh_blob_adapter_free(self.adapter);
                    net_redex_free(self.redex);
                }
            }
        }

        fn take_json(p: *mut c_char) -> serde_json::Value {
            assert!(!p.is_null());
            let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
            unsafe { net_free_string(p) };
            serde_json::from_str(&s).unwrap()
        }

        fn describe(r: &[u8]) -> Result<serde_json::Value, c_int> {
            let mut out = std::ptr::dangling_mut::<c_char>();
            let rc = unsafe { net_blob_ref_describe(r.as_ptr(), r.len(), &mut out) };
            if rc != 0 {
                assert!(out.is_null());
                return Err(rc);
            }
            Ok(take_json(out))
        }

        fn cache_stats(f: &Fixture) -> serde_json::Value {
            let mut out = std::ptr::null_mut();
            assert_eq!(
                unsafe { net_mesh_blob_adapter_tree_node_cache_stats(f.adapter, &mut out) },
                0
            );
            take_json(out)
        }

        #[test]
        fn new_v2_refusals_and_options() {
            let id = CString::new("ffi-v3-new").unwrap();
            unsafe {
                let redex = net_redex_new(std::ptr::null());
                // NULL out_handle: nothing written, -1.
                assert_eq!(
                    net_mesh_blob_adapter_new_v2(
                        redex,
                        id.as_ptr(),
                        0,
                        std::ptr::null(),
                        std::ptr::null_mut()
                    ),
                    NULL_RC
                );
                let mut out = std::ptr::dangling_mut::<MeshBlobAdapterHandle>();
                assert_eq!(
                    net_mesh_blob_adapter_new_v2(
                        std::ptr::null_mut(),
                        id.as_ptr(),
                        0,
                        std::ptr::null(),
                        &mut out
                    ),
                    NULL_RC
                );
                assert!(out.is_null(), "out_handle reset before a later failure");
                for bad in [
                    "{",
                    r#"{"tree_node_cache_byte": 1}"#,
                    r#"{"overflow": {"scope": "galaxy"}}"#,
                ] {
                    let c = CString::new(bad).unwrap();
                    let mut out = std::ptr::dangling_mut::<MeshBlobAdapterHandle>();
                    assert_eq!(
                        net_mesh_blob_adapter_new_v2(redex, id.as_ptr(), 0, c.as_ptr(), &mut out),
                        INVALID_JSON,
                        "{bad}"
                    );
                    assert!(out.is_null());
                }
                net_redex_free(redex);
            }
            // Absent cache, zero cache, and a real cache are distinct.
            assert!(cache_stats(&Fixture::new(None)).is_null());
            assert!(cache_stats(&Fixture::new(Some(""))).is_null());
            let zero = cache_stats(&Fixture::new(Some(r#"{"tree_node_cache_bytes": 0}"#)));
            assert_eq!(zero["bytes"], 0);
            assert!(
                !cache_stats(&Fixture::new(Some(r#"{"tree_node_cache_bytes": 1048576}"#)))
                    .is_null()
            );
            // The legacy overflow object rides along unchanged.
            let f = Fixture::new(Some(r#"{"overflow": {"enabled": true, "scope": "zone"}}"#));
            assert_eq!(
                unsafe { net_mesh_blob_adapter_overflow_enabled(f.adapter) },
                1
            );
        }

        #[test]
        fn store_tree_encoding_rows() {
            let f = Fixture::new(None);
            let data = b"encoding rows";
            for (kind, k, m) in [
                (2u8, 0u8, 0u8),
                (0, 1, 0),
                (0, 0, 1),
                (1, 0, 2),
                (1, 4, 0),
                (1, 200, 56),
            ] {
                assert_eq!(
                    f.store_tree(data, kind, k, m),
                    Err(NET_ERR_BLOB_INVALID_ARGUMENT),
                    "kind {kind} k {k} m {m}"
                );
            }
            for (kind, k, m) in [(0u8, 0u8, 0u8), (1, 0, 0), (1, 4, 2), (1, 200, 55)] {
                let r = f
                    .store_tree(data, kind, k, m)
                    .unwrap_or_else(|rc| panic!("kind {kind} k {k} m {m}: {rc}"));
                let d = describe(&r).unwrap();
                assert_eq!(d["is_tree"], true);
                let expect_kind = if kind == 0 {
                    "replicated"
                } else {
                    "reed_solomon"
                };
                assert_eq!(d["encoding"]["kind"], expect_kind);
                if kind == 1 && k != 0 {
                    assert_eq!(d["encoding"]["k"], k);
                    assert_eq!(d["encoding"]["m"], m);
                }
            }
            // (NULL, n > 0) input, and NULL out-pointers.
            let mut out = std::ptr::null_mut();
            let mut len = 0usize;
            unsafe {
                assert_eq!(
                    net_mesh_blob_adapter_store_tree(
                        f.adapter,
                        std::ptr::null(),
                        3,
                        0,
                        0,
                        0,
                        &mut out,
                        &mut len
                    ),
                    NULL_RC
                );
                assert_eq!(
                    net_mesh_blob_adapter_store_tree(
                        f.adapter,
                        data.as_ptr(),
                        data.len(),
                        0,
                        0,
                        0,
                        std::ptr::null_mut(),
                        &mut len
                    ),
                    NULL_RC
                );
            }
        }

        #[test]
        fn fetch_range_rows_in_core_order() {
            let f = Fixture::new(None);
            let data = b"0123456789abcdef";
            let r = f.store_tree(data, 0, 0, 0).unwrap();
            let size = data.len() as u64;
            assert_eq!(
                f.range(&r, 5, 4),
                Err(NET_ERR_BLOB_INVALID_ARGUMENT),
                "reversed"
            );
            assert_eq!(
                f.range(&r, size + 100, size + 100),
                Ok(Vec::new()),
                "empty beyond size"
            );
            assert_eq!(
                f.range(&r, 0, size + 1),
                Err(NET_ERR_BLOB_INVALID_ARGUMENT),
                "past the end"
            );
            assert_eq!(
                f.range(
                    &r,
                    0,
                    crate::adapter::net::dataforts::blob::mesh::MAX_FETCH_RANGE_BYTES + 1
                ),
                Err(NET_ERR_BLOB_INVALID_ARGUMENT),
                "over the cap"
            );
            assert_eq!(f.range(&r, 3, 9), Ok(data[3..9].to_vec()));
            assert_eq!(f.range(b"garbage", 0, 1), Err(NET_ERR_BLOB_DECODE));
            // Each out-pointer nulled in turn: -1, and the other untouched.
            let mut out = std::ptr::null_mut();
            let mut len = 7usize;
            unsafe {
                assert_eq!(
                    net_mesh_blob_adapter_fetch_range(
                        f.adapter,
                        r.as_ptr(),
                        r.len(),
                        0,
                        1,
                        std::ptr::null_mut(),
                        &mut len
                    ),
                    NULL_RC
                );
                assert_eq!(len, 7, "the non-NULL half of a NULL pair is not written");
                assert_eq!(
                    net_mesh_blob_adapter_fetch_range(
                        f.adapter,
                        r.as_ptr(),
                        r.len(),
                        0,
                        1,
                        &mut out,
                        std::ptr::null_mut()
                    ),
                    NULL_RC
                );
                // NULL handle with valid outputs: outputs reset, -1.
                let mut out = std::ptr::dangling_mut::<u8>();
                let mut len = 7usize;
                assert_eq!(
                    net_mesh_blob_adapter_fetch_range(
                        std::ptr::null(),
                        r.as_ptr(),
                        r.len(),
                        0,
                        1,
                        &mut out,
                        &mut len
                    ),
                    NULL_RC
                );
                assert!(out.is_null() && len == 0);
            }
        }

        #[test]
        fn describe_rows() {
            let f = Fixture::new(None);
            let tree = describe(&f.store_tree(b"tree", 0, 0, 0).unwrap()).unwrap();
            for key in [
                "version",
                "uri",
                "size",
                "is_tree",
                "is_chunked",
                "tree_root_hash",
                "tree_depth",
                "encoding",
            ] {
                assert!(tree.get(key).is_some(), "tree ref lacks {key}: {tree}");
            }
            assert!(
                tree.get("hash").is_none(),
                "tree ref must not carry a small hash: {tree}"
            );
            let small = InnerBlobRef::small("mesh://x", [7u8; 32], 3).encode();
            let d = describe(&small).unwrap();
            assert_eq!(d["hash"], "07".repeat(32));
            for key in ["tree_root_hash", "tree_depth", "encoding"] {
                assert!(d.get(key).is_none(), "small ref carries {key}: {d}");
            }
            assert_eq!(
                describe(&[0xB0, 0xB1, 0xB2, 0xB3, 1]),
                Err(NET_ERR_BLOB_DECODE)
            );
            unsafe {
                assert_eq!(
                    net_blob_ref_describe(small.as_ptr(), small.len(), std::ptr::null_mut()),
                    NULL_RC
                );
            }
        }

        #[test]
        fn repair_and_stats_null_and_decode_rows() {
            let f = Fixture::new(None);
            unsafe {
                assert_eq!(
                    net_mesh_blob_adapter_repair_blob(
                        f.adapter,
                        b"x".as_ptr(),
                        1,
                        std::ptr::null_mut()
                    ),
                    NULL_RC
                );
                let mut out = std::ptr::dangling_mut::<c_char>();
                assert_eq!(
                    net_mesh_blob_adapter_repair_blob(f.adapter, b"garbage".as_ptr(), 7, &mut out),
                    NET_ERR_BLOB_DECODE
                );
                assert!(out.is_null());
                assert_eq!(
                    net_mesh_blob_adapter_tree_node_cache_stats(f.adapter, std::ptr::null_mut()),
                    NULL_RC
                );
            }
            // A healthy replicated tree: walked, nothing to repair.
            let r = f.store_tree(b"healthy", 0, 0, 0).unwrap();
            let mut out = std::ptr::null_mut();
            assert_eq!(
                unsafe {
                    net_mesh_blob_adapter_repair_blob(f.adapter, r.as_ptr(), r.len(), &mut out)
                },
                0
            );
            let report = take_json(out);
            assert_eq!(report["chunks_restored"], 0);
            assert_eq!(report["stripes_unrecoverable"], 0);
        }
    }
}
