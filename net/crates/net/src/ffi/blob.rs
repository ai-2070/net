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

use std::ffi::{c_char, c_int, CStr};
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;

use tokio::runtime::Runtime;

use crate::adapter::net::dataforts::{
    global_blob_adapter_registry, publish_blob, resolve_payload, BlobAdapter,
    BlobError as InnerBlobError, FileSystemAdapter,
};

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
/// Panic surfaced from inside a user-installed adapter callback
/// (or anywhere on the FFI body). The substrate catches it with
/// `catch_unwind` and reports this code rather than unwinding
/// across the FFI boundary (which is undefined behaviour for the
/// C / cgo / Python callers).
pub const NET_ERR_BLOB_PANIC: c_int = -117;

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

    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    *out_payload = ptr;
    *out_payload_len = len;
    0
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

    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    *out_content = ptr;
    *out_content_len = len;
    0
}

/// Free a buffer returned by [`net_blob_publish`] or
/// [`net_blob_resolve`]. Calling with `(null, 0)` is a no-op.
///
/// # Safety
/// `ptr` MUST be a buffer that the substrate previously returned
/// from `net_blob_publish` or `net_blob_resolve` (or null), and
/// `len` MUST match the corresponding `*out_*_len` value from
/// that call. Calling with any other `(ptr, len)` is undefined
/// behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_free_buffer(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len));
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
use std::sync::atomic::{AtomicPtr, Ordering as AtomicOrdering};

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

/// Opaque caller-context pointer. The caller is responsible for
/// thread-safety; the substrate just shuttles it across calls.
/// `AtomicPtr` provides `Send + Sync` without requiring the
/// pointee to be either.
struct OpaqueCtx(AtomicPtr<c_void>);

impl OpaqueCtx {
    fn new(ptr: *mut c_void) -> Self {
        Self(AtomicPtr::new(ptr))
    }
    fn get(&self) -> *mut c_void {
        self.0.load(AtomicOrdering::Acquire)
    }
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
        let uri = match std::ffi::CString::new(blob_ref.uri.clone()) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        let hash = blob_ref.hash;
        let size = blob_ref.size;
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
    ) -> std::result::Result<Vec<u8>, InnerBlobError> {
        let vtable = self.vtable;
        let ctx = self.ctx.clone();
        let uri = match std::ffi::CString::new(blob_ref.uri.clone()) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        let hash = blob_ref.hash;
        let size = blob_ref.size;
        tokio::task::spawn_blocking(move || -> std::result::Result<Vec<u8>, InnerBlobError> {
            let mut out_data: *mut u8 = ptr::null_mut();
            let mut out_len: usize = 0;
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
                    return Ok(Vec::new());
                }
                return Err(InnerBlobError::Backend(
                    "fetch: caller returned null pointer with non-zero len".into(),
                ));
            }
            // Copy out before freeing — the caller owns the buffer
            // and frees it via free_buffer.
            let buf = unsafe { std::slice::from_raw_parts(out_data, out_len).to_vec() };
            unsafe { (vtable.free_buffer)(ctx.get(), out_data, out_len) };
            Ok(buf)
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn fetch_range(
        &self,
        blob_ref: &crate::adapter::net::dataforts::BlobRef,
        range: Range<u64>,
    ) -> std::result::Result<Vec<u8>, InnerBlobError> {
        let vtable = self.vtable;
        let ctx = self.ctx.clone();
        let uri = match std::ffi::CString::new(blob_ref.uri.clone()) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        let hash = blob_ref.hash;
        let size = blob_ref.size;
        let start = range.start;
        let end = range.end;
        tokio::task::spawn_blocking(move || -> std::result::Result<Vec<u8>, InnerBlobError> {
            let mut out_data: *mut u8 = ptr::null_mut();
            let mut out_len: usize = 0;
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
                    return Ok(Vec::new());
                }
                return Err(InnerBlobError::Backend(
                    "fetch_range: caller returned null pointer with non-zero len".into(),
                ));
            }
            let buf = unsafe { std::slice::from_raw_parts(out_data, out_len).to_vec() };
            unsafe { (vtable.free_buffer)(ctx.get(), out_data, out_len) };
            Ok(buf)
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
        let uri = match std::ffi::CString::new(blob_ref.uri.clone()) {
            Ok(c) => c,
            Err(e) => return Err(InnerBlobError::Backend(format!("uri NUL: {}", e))),
        };
        let hash = blob_ref.hash;
        let size = blob_ref.size;
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
///   pointee alive and thread-safe for the same lifetime as `vtable`.
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

#[cfg(test)]
mod tests {
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
        let uri_c = CString::new("ffi://round-trip").unwrap();

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
            // First byte is the discriminator.
            let encoded = std::slice::from_raw_parts(out_buf, out_len);
            assert_eq!(
                encoded[0],
                crate::adapter::net::dataforts::BLOB_REF_DISCRIMINATOR
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
}
