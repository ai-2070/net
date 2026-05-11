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

fn runtime() -> &'static Arc<Runtime> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => Arc::new(rt),
        Err(e) => {
            eprintln!(
                "FATAL: blob FFI tokio runtime build failure ({e:?}); aborting"
            );
            std::process::abort();
        }
    })
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "FATAL: blob FFI called from inside a tokio runtime context; aborting"
        );
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
    let bytes = match block_on(async move {
        publish_blob(adapter.as_ref(), uri, data_slice).await
    }) {
        Ok(b) => b,
        Err(e) => return err_to_code(&e),
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
    let bytes = match block_on(async move {
        resolve_payload(payload_slice, adapter.as_ref()).await
    }) {
        Ok(b) => b,
        Err(e) => return err_to_code(&e),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_free_buffer(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let _ = Box::from_raw(std::slice::from_raw_parts_mut(ptr, len) as *mut [u8]);
}

// Ensure the unused-import lint stays quiet under feature gates that
// drop one of these surfaces — currently all callable.
#[allow(dead_code)]
fn _force_use() -> *mut c_void {
    ptr::null_mut()
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
