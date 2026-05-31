//! C FFI for the transport surface — blob transfer + directory
//! transfer over the fairscheduler stream transport (Transport SDK
//! plan, T-C). cgo / dlsym consumers (Go now; any C-ABI language
//! later) target these symbols; the contract is documented in
//! `include/net_transport.h`.
//!
//! # Handle model
//!
//! Transfer is node-driven in the substrate, so these functions take
//! the existing [`MeshNodeHandle`](super::mesh::MeshNodeHandle) from
//! the mesh surface (`net_mesh_*`) plus, for the store / serve side,
//! the [`MeshBlobAdapterHandle`](super::blob::MeshBlobAdapterHandle)
//! from the blob surface (`net_mesh_blob_adapter_*`). No new handle
//! type is introduced. Both are cloned under their owning handle's
//! guard for the duration of an op (see `mesh_node_arc` /
//! `blob_adapter_arc`), so a concurrent `_free` cannot deallocate the
//! inner mid-call.
//!
//! # Serving is required to fetch
//!
//! A node must install the transfer engine via
//! [`net_serve_blob_transfer`] before it can serve chunks to peers OR
//! issue its own fetches — an un-installed node returns
//! `NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED`.
//!
//! # Memory + errors
//!
//! Owned byte buffers (`out_bytes` / `out_len`) are freed with
//! [`net_transport_free_buffer`]; JSON strings (`net_dir_manifest_read`)
//! with [`net_free_string`](super::net_free_string). Errors are negative
//! `c_int` codes in the `NET_ERR_TRANSFER_*` / `NET_ERR_DIR_*` band; the
//! per-code meaning is in `include/net_transport.h`.
//!
//! # Async
//!
//! The handle-based async trio (`net_fetch_blob_async` / `_await` /
//! `_cancel`) from the plan is a follow-up; this slice ships the
//! synchronous surface (`block_on`-backed), which covers the blocking
//! call shape the bindings need first.

#![allow(clippy::missing_safety_doc)]

use std::os::raw::{c_char, c_int};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::ptr;
use std::sync::Arc;

use super::blob::{blob_adapter_arc, MeshBlobAdapterHandle};
use super::mesh::{block_on, mesh_node_arc, write_string_out, MeshNodeHandle};
use crate::adapter::net::dataforts::blob::{BlobError, BlobRef, MeshBlobAdapter};
use crate::adapter::net::dataforts::{
    fetch_dir as substrate_fetch_dir, store_dir as substrate_store_dir, DirError, DirManifest,
};
use crate::adapter::net::MeshNode;

// ── Error codes (mirror `include/net_transport.h`) ──────────────────
// Fresh band below the blob (-110..-120) / NAT (-130..-137) ranges so
// transfer codes never collide with another surface's. Kept in sync
// with the header by `tests/transport_error_codes.rs`.

/// Success.
pub const NET_TRANSPORT_OK: c_int = 0;
/// A holder did not have the requested content.
pub const NET_ERR_TRANSFER_NOT_FOUND: c_int = -200;
/// Fetched bytes did not hash to the expected content address.
pub const NET_ERR_TRANSFER_HASH_MISMATCH: c_int = -201;
/// Holder discovery exhausted every connected peer without a hit.
pub const NET_ERR_TRANSFER_ALL_PEERS_FAILED: c_int = -202;
/// The fetch was cancelled.
pub const NET_ERR_TRANSFER_CANCELLED: c_int = -203;
/// A required pointer argument was NULL.
pub const NET_ERR_TRANSFER_NULL_POINTER: c_int = -204;
/// The handle is shutting down (its `_free` has begun).
pub const NET_ERR_TRANSFER_SHUTTING_DOWN: c_int = -205;
/// The node has no transfer engine installed (call
/// `net_serve_blob_transfer` first).
pub const NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED: c_int = -206;
/// Any other substrate-side transfer failure (transport error, etc.).
pub const NET_ERR_TRANSFER_BACKEND: c_int = -207;
/// A panic was caught crossing the FFI boundary.
pub const NET_ERR_TRANSFER_PANIC: c_int = -208;
/// An argument was malformed (bad UTF-8 path, oversize length, etc.).
pub const NET_ERR_TRANSFER_INVALID_ARGUMENT: c_int = -209;
/// A directory manifest failed to decode / had an unsupported version.
pub const NET_ERR_DIR_INVALID_MANIFEST: c_int = -210;
/// A manifest entry path escaped the destination root.
pub const NET_ERR_DIR_PATH_INVALID: c_int = -211;
/// Filesystem I/O failed during directory reconstruction.
pub const NET_ERR_DIR_IO: c_int = -213;

/// Map a substrate [`BlobError`] to a transport error code.
fn blob_err_code(e: &BlobError) -> c_int {
    match e {
        BlobError::NotFound(_) => NET_ERR_TRANSFER_NOT_FOUND,
        BlobError::HashMismatch { .. } => NET_ERR_TRANSFER_HASH_MISMATCH,
        BlobError::Cancelled => NET_ERR_TRANSFER_CANCELLED,
        // `transfer_fetch_chunk` surfaces a missing engine as a Backend
        // string; give the common setup mistake its own code.
        BlobError::Backend(m) if m.contains("engine not installed") => {
            NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED
        }
        _ => NET_ERR_TRANSFER_BACKEND,
    }
}

/// Map a substrate [`DirError`] to a transport error code.
fn dir_err_code(e: &DirError) -> c_int {
    match e {
        DirError::Blob(b) => blob_err_code(b),
        DirError::UnsafePath(_) => NET_ERR_DIR_PATH_INVALID,
        DirError::Manifest(_) => NET_ERR_DIR_INVALID_MANIFEST,
        DirError::Io(_) => NET_ERR_DIR_IO,
    }
}

/// Copy `src` into a freshly-allocated C buffer, writing the pointer +
/// length to the out-params. Caller frees via [`net_transport_free_buffer`].
/// An empty `src` yields `(NULL, 0)`. The allocation uses an explicit
/// `Layout::array::<u8>` so the free path can deallocate with the
/// symmetric layout (mirrors `net_blob_free_buffer`).
unsafe fn write_bytes_out(src: &[u8], out_ptr: *mut *mut u8, out_len: *mut usize) -> c_int {
    let len = src.len();
    if len == 0 {
        unsafe {
            *out_ptr = ptr::null_mut();
            *out_len = 0;
        }
        return NET_TRANSPORT_OK;
    }
    let layout = match std::alloc::Layout::array::<u8>(len) {
        Ok(l) => l,
        Err(_) => return NET_ERR_TRANSFER_BACKEND,
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
    NET_TRANSPORT_OK
}

/// Read a 32-byte content hash from a caller buffer. The caller
/// guarantees `p` points to at least 32 readable bytes.
unsafe fn read_hash(p: *const u8) -> [u8; 32] {
    let mut h = [0u8; 32];
    unsafe { std::ptr::copy_nonoverlapping(p, h.as_mut_ptr(), 32) };
    h
}

/// Decode a caller-supplied encoded [`BlobRef`] buffer.
unsafe fn read_blob_ref(ptr: *const u8, len: usize) -> Result<BlobRef, c_int> {
    if len > isize::MAX as usize {
        return Err(NET_ERR_TRANSFER_INVALID_ARGUMENT);
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match BlobRef::decode(bytes) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(NET_ERR_TRANSFER_INVALID_ARGUMENT),
        Err(e) => Err(blob_err_code(&e)),
    }
}

/// Convert a C path string to a `Path`. Returns `Err` on NULL / bad UTF-8.
unsafe fn read_path<'a>(p: *const c_char) -> Result<&'a Path, c_int> {
    if p.is_null() {
        return Err(NET_ERR_TRANSFER_NULL_POINTER);
    }
    match unsafe { std::ffi::CStr::from_ptr(p) }.to_str() {
        Ok(s) => Ok(Path::new(s)),
        Err(_) => Err(NET_ERR_TRANSFER_INVALID_ARGUMENT),
    }
}

/// Reassemble a whole blob (`Small` = one chunk, `Manifest` = ordered
/// chunk list) from a known `source` over the transfer transport.
async fn fetch_blob_bytes(
    node: &Arc<MeshNode>,
    source: u64,
    blob_ref: &BlobRef,
) -> Result<Vec<u8>, BlobError> {
    match blob_ref {
        BlobRef::Small { hash, .. } => Ok(node.transfer_fetch_chunk(source, *hash).await?.to_vec()),
        BlobRef::Manifest {
            chunks,
            total_size,
            ..
        } => {
            let mut buf = Vec::with_capacity(*total_size as usize);
            for chunk in chunks {
                buf.extend_from_slice(&node.transfer_fetch_chunk(source, chunk.hash).await?);
            }
            Ok(buf)
        }
        BlobRef::Tree { .. } => Err(BlobError::Backend(
            "transfer: BlobRef::Tree not supported by the transport FFI".into(),
        )),
    }
}

// ── Public FFI ──────────────────────────────────────────────────────

/// Install the blob-transfer engine on `node` over `adapter`. Required
/// before the node can serve chunks OR fetch. Idempotent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_serve_blob_transfer(
    node: *const MeshNodeHandle,
    adapter: *const MeshBlobAdapterHandle,
) -> c_int {
    if node.is_null() || adapter.is_null() {
        return NET_ERR_TRANSFER_NULL_POINTER;
    }
    let node_arc = match mesh_node_arc(unsafe { &*node }) {
        Some(n) => n,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    let adapter_arc: Arc<MeshBlobAdapter> = match blob_adapter_arc(unsafe { &*adapter }) {
        Some(a) => a,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        node_arc.serve_blob_transfer(adapter_arc);
    })) {
        Ok(()) => NET_TRANSPORT_OK,
        Err(_) => NET_ERR_TRANSFER_PANIC,
    }
}

/// Fetch the blob addressed by the 32-byte `hash` from the known holder
/// `holder_id`. On success writes a freshly-allocated buffer to
/// `out_bytes` / `out_len` (free with [`net_transport_free_buffer`]).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fetch_blob(
    node: *const MeshNodeHandle,
    holder_id: u64,
    hash: *const u8,
    out_bytes: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    if node.is_null() || hash.is_null() || out_bytes.is_null() || out_len.is_null() {
        return NET_ERR_TRANSFER_NULL_POINTER;
    }
    unsafe {
        *out_bytes = ptr::null_mut();
        *out_len = 0;
    }
    let node_arc = match mesh_node_arc(unsafe { &*node }) {
        Some(n) => n,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    let hash = unsafe { read_hash(hash) };
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        block_on(async move { node_arc.transfer_fetch_chunk(holder_id, hash).await })
    }));
    match outcome {
        Err(_) => NET_ERR_TRANSFER_PANIC,
        Ok(Ok(bytes)) => unsafe { write_bytes_out(&bytes, out_bytes, out_len) },
        Ok(Err(e)) => blob_err_code(&e),
    }
}

/// Like [`net_fetch_blob`] but discovers the holder among connected
/// peers. Returns `NET_ERR_TRANSFER_ALL_PEERS_FAILED` if no peer has it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fetch_blob_discovered(
    node: *const MeshNodeHandle,
    hash: *const u8,
    out_bytes: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    if node.is_null() || hash.is_null() || out_bytes.is_null() || out_len.is_null() {
        return NET_ERR_TRANSFER_NULL_POINTER;
    }
    unsafe {
        *out_bytes = ptr::null_mut();
        *out_len = 0;
    }
    let node_arc = match mesh_node_arc(unsafe { &*node }) {
        Some(n) => n,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    let hash = unsafe { read_hash(hash) };
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        block_on(async move { node_arc.transfer_fetch_chunk_discovered(hash).await })
    }));
    match outcome {
        Err(_) => NET_ERR_TRANSFER_PANIC,
        Ok(Ok(bytes)) => unsafe { write_bytes_out(&bytes, out_bytes, out_len) },
        // Discovery reports "nobody served it" as NotFound — re-tag so
        // the caller can distinguish it from a named-holder miss.
        Ok(Err(BlobError::NotFound(_))) => NET_ERR_TRANSFER_ALL_PEERS_FAILED,
        Ok(Err(e)) => blob_err_code(&e),
    }
}

/// Store the local directory at `root_path` as content-addressed blobs
/// in `adapter`, writing the encoded manifest [`BlobRef`] to
/// `out_manifest_ref` / `out_len` (free with [`net_transport_free_buffer`]).
/// That buffer is what `net_fetch_dir` on the receiver consumes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_store_dir(
    adapter: *const MeshBlobAdapterHandle,
    root_path: *const c_char,
    out_manifest_ref: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    if adapter.is_null() || root_path.is_null() || out_manifest_ref.is_null() || out_len.is_null() {
        return NET_ERR_TRANSFER_NULL_POINTER;
    }
    unsafe {
        *out_manifest_ref = ptr::null_mut();
        *out_len = 0;
    }
    let adapter_arc: Arc<MeshBlobAdapter> = match blob_adapter_arc(unsafe { &*adapter }) {
        Some(a) => a,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    let root = match unsafe { read_path(root_path) } {
        Ok(p) => p.to_path_buf(),
        Err(code) => return code,
    };
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        block_on(async move { substrate_store_dir(&adapter_arc, &root).await })
    }));
    match outcome {
        Err(_) => NET_ERR_TRANSFER_PANIC,
        Ok(Ok(blob_ref)) => unsafe {
            write_bytes_out(&blob_ref.encode(), out_manifest_ref, out_len)
        },
        Ok(Err(e)) => dir_err_code(&e),
    }
}

/// Fetch the directory whose encoded manifest [`BlobRef`] is
/// `manifest_ref` (`manifest_ref_len` bytes) from `source_id` and
/// reconstruct it under `dest_path`. Writes the count of files written
/// to `out_files` and total bytes to `out_bytes` (either may be NULL to
/// ignore). Uses the default fetch concurrency.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fetch_dir(
    node: *const MeshNodeHandle,
    source_id: u64,
    manifest_ref: *const u8,
    manifest_ref_len: usize,
    dest_path: *const c_char,
    out_files: *mut u64,
    out_bytes: *mut u64,
) -> c_int {
    if node.is_null() || manifest_ref.is_null() || dest_path.is_null() {
        return NET_ERR_TRANSFER_NULL_POINTER;
    }
    let node_arc = match mesh_node_arc(unsafe { &*node }) {
        Some(n) => n,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    let blob_ref = match unsafe { read_blob_ref(manifest_ref, manifest_ref_len) } {
        Ok(r) => r,
        Err(code) => return code,
    };
    let dest = match unsafe { read_path(dest_path) } {
        Ok(p) => p.to_path_buf(),
        Err(code) => return code,
    };
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        block_on(async move { substrate_fetch_dir(&node_arc, source_id, &blob_ref, &dest, 0).await })
    }));
    match outcome {
        Err(_) => NET_ERR_TRANSFER_PANIC,
        Ok(Ok(stats)) => {
            if !out_files.is_null() {
                unsafe { *out_files = stats.files as u64 };
            }
            if !out_bytes.is_null() {
                unsafe { *out_bytes = stats.bytes };
            }
            NET_TRANSPORT_OK
        }
        Ok(Err(e)) => dir_err_code(&e),
    }
}

/// Fetch + decode the directory manifest at `manifest_ref` from
/// `source_id` WITHOUT reconstructing the tree, writing it as a JSON
/// string to `out_json` / `out_len` for introspection. Free the string
/// with [`net_free_string`](super::net_free_string).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_dir_manifest_read(
    node: *const MeshNodeHandle,
    source_id: u64,
    manifest_ref: *const u8,
    manifest_ref_len: usize,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if node.is_null() || manifest_ref.is_null() || out_json.is_null() || out_len.is_null() {
        return NET_ERR_TRANSFER_NULL_POINTER;
    }
    let node_arc = match mesh_node_arc(unsafe { &*node }) {
        Some(n) => n,
        None => return NET_ERR_TRANSFER_SHUTTING_DOWN,
    };
    let blob_ref = match unsafe { read_blob_ref(manifest_ref, manifest_ref_len) } {
        Ok(r) => r,
        Err(code) => return code,
    };
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        block_on(async move { fetch_blob_bytes(&node_arc, source_id, &blob_ref).await })
    }));
    let bytes = match outcome {
        Err(_) => return NET_ERR_TRANSFER_PANIC,
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return blob_err_code(&e),
    };
    let manifest: DirManifest = match postcard::from_bytes(&bytes) {
        Ok(m) => m,
        Err(_) => return NET_ERR_DIR_INVALID_MANIFEST,
    };
    let json = match serde_json::to_string(&manifest) {
        Ok(s) => s,
        Err(_) => return NET_ERR_DIR_INVALID_MANIFEST,
    };
    write_string_out(json, out_json, out_len)
}

/// Free a byte buffer returned by `net_fetch_blob*` / `net_store_dir`.
/// NULL / zero-length is a no-op. Idempotent only if the caller nulls
/// its pointer after the call (double-free is UB, as with any C free).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_transport_free_buffer(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let layout = match std::alloc::Layout::array::<u8>(len) {
        Ok(l) => l,
        Err(_) => return,
    };
    unsafe { std::alloc::dealloc(ptr, layout) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_distinct_and_negative() {
        let codes = [
            NET_ERR_TRANSFER_NOT_FOUND,
            NET_ERR_TRANSFER_HASH_MISMATCH,
            NET_ERR_TRANSFER_ALL_PEERS_FAILED,
            NET_ERR_TRANSFER_CANCELLED,
            NET_ERR_TRANSFER_NULL_POINTER,
            NET_ERR_TRANSFER_SHUTTING_DOWN,
            NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED,
            NET_ERR_TRANSFER_BACKEND,
            NET_ERR_TRANSFER_PANIC,
            NET_ERR_TRANSFER_INVALID_ARGUMENT,
            NET_ERR_DIR_INVALID_MANIFEST,
            NET_ERR_DIR_PATH_INVALID,
            NET_ERR_DIR_IO,
        ];
        for (i, a) in codes.iter().enumerate() {
            assert!(*a < 0, "code {a} must be negative");
            for b in &codes[i + 1..] {
                assert_ne!(a, b, "duplicate transport error code {a}");
            }
        }
    }

    #[test]
    fn blob_errors_map_to_transport_codes() {
        assert_eq!(
            blob_err_code(&BlobError::NotFound("x".into())),
            NET_ERR_TRANSFER_NOT_FOUND
        );
        assert_eq!(
            blob_err_code(&BlobError::HashMismatch {
                expected: [0u8; 32],
                actual: [1u8; 32],
            }),
            NET_ERR_TRANSFER_HASH_MISMATCH
        );
        assert_eq!(
            blob_err_code(&BlobError::Backend("blob transfer: engine not installed".into())),
            NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED
        );
        assert_eq!(
            blob_err_code(&BlobError::Backend("some other failure".into())),
            NET_ERR_TRANSFER_BACKEND
        );
    }

    #[test]
    fn null_pointers_are_rejected_without_deref() {
        // No handle / out-params → NULL pointer code, no UB.
        let rc = unsafe {
            net_fetch_blob(
                ptr::null(),
                0,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        assert_eq!(rc, NET_ERR_TRANSFER_NULL_POINTER);
    }
}
