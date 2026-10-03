//! Feature-OFF stubs for the dataforts blob FFI symbols.
//!
//! `ffi::blob` is gated on `feature = "dataforts"` at the
//! module level (see `ffi::mod`), so when a `libnet` cdylib is
//! built without the `dataforts + netdb + redex-disk` feature
//! triple every `net_blob_*` / `net_mesh_blob_adapter_*` symbol
//! is absent. cgo / dlsym consumers (notably the Go binding's
//! `blob.go`, which links these symbols unconditionally) then
//! fail at program load with `undefined symbol`.
//!
//! This module lives outside the dataforts gate and emits stub
//! definitions for the symbols Go and other cgo consumers rely
//! on. Each stub returns `NET_ERR_FEATURE_NOT_BUILT` (or null
//! for pointer-typed returns) so callers route to a clean typed
//! error rather than a load-time crash.
//!
//! Active only when the cortex surface (`netdb + redex-disk`,
//! which provides `RedexHandle` and `NET_ERR_FEATURE_NOT_BUILT`)
//! is compiled in but `dataforts` is off — that's the
//! configuration where a libnet cdylib exposes redex / cortex
//! symbols to Go, the Go `blob.go` links the blob symbols
//! unconditionally, but the dataforts feature wasn't selected.
//! Builds without `netdb + redex-disk` have no cortex surface
//! either; Go consumers in that shape can't link any of the
//! redex / mesh / blob entry points and the stub set is moot.
//!
//! Mirrors the convention `ffi::cortex` already uses for the
//! `net_redex_enable_greedy_dataforts` / `_gravity_*` symbols.

#![cfg(all(feature = "netdb", feature = "redex-disk", not(feature = "dataforts")))]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use super::cortex::NET_ERR_FEATURE_NOT_BUILT;

/// Opaque handle. Never constructed in this build — the `_new`
/// stub returns null. Exists so the per-symbol stubs have a
/// matching pointer type for the C ABI.
pub struct MeshBlobAdapterHandle {
    _private: (),
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_new(
    _redex: *mut super::cortex::RedexHandle,
    _adapter_id: *const c_char,
    _persistent: c_int,
    _overflow_json: *const c_char,
) -> *mut MeshBlobAdapterHandle {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_free(_handle: *mut MeshBlobAdapterHandle) {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_store(
    _handle: *const MeshBlobAdapterHandle,
    _blob_ref_bytes: *const u8,
    _blob_ref_len: usize,
    _data: *const u8,
    _data_len: usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_publish(
    _handle: *const MeshBlobAdapterHandle,
    _uri_ptr: *const u8,
    _uri_len: usize,
    _data: *const u8,
    _data_len: usize,
    _out_ref: *mut *mut u8,
    _out_ref_len: *mut usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_ref_hash(
    _encoded: *const u8,
    _encoded_len: usize,
    _out_hash: *mut u8,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_fetch(
    _handle: *const MeshBlobAdapterHandle,
    _blob_ref_bytes: *const u8,
    _blob_ref_len: usize,
    _out_data: *mut *mut u8,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_exists(
    _handle: *const MeshBlobAdapterHandle,
    _blob_ref_bytes: *const u8,
    _blob_ref_len: usize,
    _out_exists: *mut c_int,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_prometheus_text(
    _handle: *const MeshBlobAdapterHandle,
) -> *mut c_char {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_overflow_enabled(
    _handle: *const MeshBlobAdapterHandle,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_overflow_active(
    _handle: *const MeshBlobAdapterHandle,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_overflow_config(
    _handle: *const MeshBlobAdapterHandle,
) -> *mut c_char {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_set_overflow_enabled(
    _handle: *const MeshBlobAdapterHandle,
    _enabled: c_int,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_set_overflow_config(
    _handle: *const MeshBlobAdapterHandle,
    _config_json: *const c_char,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

/// `net_blob_free_buffer` lives in `ffi::blob` (gated on
/// `dataforts`); cgo consumers call it on every `_fetch` reply
/// regardless of feature build. Provide an always-on stub so the
/// symbol resolves. The fetch stub never hands out a buffer, so
/// this is a no-op on every realistic call path; the
/// belt-and-suspenders null check defends against a caller that
/// stashed a non-null pointer from a prior dataforts-on build.
///
/// # Safety
/// `ptr` may be null; if non-null, it must originate from a
/// matching `_fetch` call (which on this build never happens —
/// the function then deliberately does nothing).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_free_buffer(_ptr: *mut u8, _len: usize) {}

// ---- v0.3 tree / range / repair surface (net_mesh_blob_adapter_new_v2 and
// friends). Same posture as the stubs above, plus the S6 contract: any
// non-NULL out-pointer is initialised before returning, so a caller that
// ignores the code still reads (NULL, 0).

/// # Safety
/// `out_handle` may be null; if non-null it must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_new_v2(
    _redex: *mut super::cortex::RedexHandle,
    _adapter_id: *const c_char,
    _persistent: c_int,
    _options_json: *const c_char,
    out_handle: *mut *mut MeshBlobAdapterHandle,
) -> c_int {
    if !out_handle.is_null() {
        unsafe { *out_handle = ptr::null_mut() };
    }
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_data` / `out_len` may be null; if non-null they must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_fetch_range(
    _handle: *const MeshBlobAdapterHandle,
    _blob_ref_bytes: *const u8,
    _blob_ref_len: usize,
    _start: u64,
    _end: u64,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    if !out_data.is_null() && !out_len.is_null() {
        unsafe {
            *out_data = ptr::null_mut();
            *out_len = 0;
        }
    }
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_ref` / `out_ref_len` may be null; if non-null they must be writable.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_store_tree(
    _handle: *const MeshBlobAdapterHandle,
    _data: *const u8,
    _data_len: usize,
    _encoding_kind: u8,
    _rs_k: u8,
    _rs_m: u8,
    out_ref: *mut *mut u8,
    out_ref_len: *mut usize,
) -> c_int {
    if !out_ref.is_null() && !out_ref_len.is_null() {
        unsafe {
            *out_ref = ptr::null_mut();
            *out_ref_len = 0;
        }
    }
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_json` may be null; if non-null it must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_repair_blob(
    _handle: *const MeshBlobAdapterHandle,
    _blob_ref_bytes: *const u8,
    _blob_ref_len: usize,
    out_json: *mut *mut c_char,
) -> c_int {
    if !out_json.is_null() {
        unsafe { *out_json = ptr::null_mut() };
    }
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_json` may be null; if non-null it must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_tree_node_cache_stats(
    _handle: *const MeshBlobAdapterHandle,
    out_json: *mut *mut c_char,
) -> c_int {
    if !out_json.is_null() {
        unsafe { *out_json = ptr::null_mut() };
    }
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_json` may be null; if non-null it must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_ref_describe(
    _encoded: *const u8,
    _encoded_len: usize,
    out_json: *mut *mut c_char,
) -> c_int {
    if !out_json.is_null() {
        unsafe { *out_json = ptr::null_mut() };
    }
    NET_ERR_FEATURE_NOT_BUILT
}

/// Test seam stub (`fixtures` only), so a `test_helpers` Go binary still
/// links against a dataforts-off `libnet`.
///
/// # Safety
/// Never dereferences its arguments.
#[cfg(feature = "fixtures")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_test_drop_data_chunk(
    _handle: *const MeshBlobAdapterHandle,
    _blob_ref_bytes: *const u8,
    _blob_ref_len: usize,
    _stripe_index: u32,
    _data_index: u32,
    _out_hash: *mut u8,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

/// Test seam stub (`fixtures` only).
///
/// # Safety
/// Never dereferences its arguments.
#[cfg(feature = "fixtures")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_mesh_blob_adapter_test_chunk_present(
    _handle: *const MeshBlobAdapterHandle,
    _hash: *const u8,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

// ---- Process-wide adapter registry (S5, S5b). `go/blob_registry.go` and
// `go/blob_adapter.go` link these unconditionally too. Same posture: the
// code, every non-NULL out slot reset, and an owned registration that never
// calls its release_fn (the context stays the caller's, as on any refusal).

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_register_fs_adapter(
    _adapter_id: *const c_char,
    _root: *const c_char,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_unregister_adapter(_adapter_id: *const c_char) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_adapter_registered(_adapter_id: *const c_char) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_payload` / `out_payload_len` may be null; if non-null they must be
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_publish(
    _adapter_id: *const c_char,
    _uri: *const c_char,
    _data: *const u8,
    _data_len: usize,
    out_payload: *mut *mut u8,
    out_payload_len: *mut usize,
) -> c_int {
    unsafe { reset_buf(out_payload, out_payload_len) };
    NET_ERR_FEATURE_NOT_BUILT
}

/// # Safety
/// `out_content` / `out_content_len` may be null; if non-null they must be
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_resolve(
    _adapter_id: *const c_char,
    _payload: *const u8,
    _payload_len: usize,
    out_content: *mut *mut u8,
    out_content_len: *mut usize,
) -> c_int {
    unsafe { reset_buf(out_content, out_content_len) };
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_register_callback_adapter(
    _adapter_id: *const c_char,
    _vtable: *const c_void,
    _ctx: *mut c_void,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_blob_register_callback_adapter_owned(
    _adapter_id: *const c_char,
    _vtable: *const c_void,
    _ctx: *mut c_void,
    _release_fn: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

/// Reset a (pointer, length) out pair, skipping NULL slots.
///
/// # Safety
/// Each non-null argument must be writable.
unsafe fn reset_buf(data: *mut *mut u8, len: *mut usize) {
    if !data.is_null() {
        unsafe { *data = ptr::null_mut() };
    }
    if !len.is_null() {
        unsafe { *len = 0 };
    }
}

#[cfg(test)]
mod tests {
    //! Contract checks on the stub bodies — every `c_int`
    //! return is the documented constant, every pointer
    //! return is null. Cheap defense against accidental
    //! drift if a follow-up refactors the constants.
    use super::*;

    #[test]
    fn stubs_return_feature_not_built_when_dataforts_off() {
        let null_handle = std::ptr::null::<MeshBlobAdapterHandle>();
        // SAFETY: every stub is a pure constant/null return — it never
        // dereferences its pointer args, so NULL is fine.
        unsafe {
            assert_eq!(
                net_mesh_blob_adapter_store(null_handle, std::ptr::null(), 0, std::ptr::null(), 0),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_mesh_blob_adapter_fetch(
                    null_handle,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_mesh_blob_adapter_exists(
                    null_handle,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_mesh_blob_adapter_overflow_enabled(null_handle),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_mesh_blob_adapter_overflow_active(null_handle),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_mesh_blob_adapter_set_overflow_enabled(null_handle, 1),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_mesh_blob_adapter_set_overflow_config(null_handle, std::ptr::null()),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(net_mesh_blob_adapter_new(
                std::ptr::null_mut(),
                std::ptr::null(),
                0,
                std::ptr::null()
            )
            .is_null());
            // v0.3 surface: the code, and every non-NULL out slot reset.
            let mut out_handle = std::ptr::dangling_mut::<MeshBlobAdapterHandle>();
            assert_eq!(
                net_mesh_blob_adapter_new_v2(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut out_handle
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out_handle.is_null());
            let mut out_data = std::ptr::dangling_mut::<u8>();
            let mut out_len = 7usize;
            assert_eq!(
                net_mesh_blob_adapter_fetch_range(
                    null_handle,
                    std::ptr::null(),
                    0,
                    0,
                    1,
                    &mut out_data,
                    &mut out_len
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out_data.is_null() && out_len == 0);
            let mut out_ref = std::ptr::dangling_mut::<u8>();
            let mut out_ref_len = 7usize;
            assert_eq!(
                net_mesh_blob_adapter_store_tree(
                    null_handle,
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    &mut out_ref,
                    &mut out_ref_len
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out_ref.is_null() && out_ref_len == 0);
            let mut out_json = std::ptr::dangling_mut::<c_char>();
            assert_eq!(
                net_mesh_blob_adapter_repair_blob(null_handle, std::ptr::null(), 0, &mut out_json),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out_json.is_null());
            let mut out_json = std::ptr::dangling_mut::<c_char>();
            assert_eq!(
                net_mesh_blob_adapter_tree_node_cache_stats(null_handle, &mut out_json),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out_json.is_null());
            let mut out_json = std::ptr::dangling_mut::<c_char>();
            assert_eq!(
                net_blob_ref_describe(std::ptr::null(), 0, &mut out_json),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out_json.is_null());
            assert!(net_mesh_blob_adapter_prometheus_text(null_handle).is_null());
            assert!(net_mesh_blob_adapter_overflow_config(null_handle).is_null());
            net_mesh_blob_adapter_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn registry_stubs_refuse_and_never_release() {
        static RELEASED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        unsafe extern "C" fn release(_ctx: *mut c_void) {
            RELEASED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let id = c"stub-id";
        // SAFETY: the stubs never dereference their inputs; the out slots
        // are live locals.
        unsafe {
            assert_eq!(
                net_blob_register_fs_adapter(id.as_ptr(), c"/tmp".as_ptr()),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_blob_unregister_adapter(id.as_ptr()),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_blob_adapter_registered(id.as_ptr()),
                NET_ERR_FEATURE_NOT_BUILT
            );
            let mut out = std::ptr::dangling_mut::<u8>();
            let mut out_len = 7usize;
            assert_eq!(
                net_blob_publish(
                    id.as_ptr(),
                    c"file:///x".as_ptr(),
                    std::ptr::null(),
                    0,
                    &mut out,
                    &mut out_len
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out.is_null() && out_len == 0);
            let mut out = std::ptr::dangling_mut::<u8>();
            let mut out_len = 7usize;
            assert_eq!(
                net_blob_resolve(id.as_ptr(), std::ptr::null(), 0, &mut out, &mut out_len),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert!(out.is_null() && out_len == 0);
            assert_eq!(
                net_blob_resolve(
                    id.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_blob_register_callback_adapter(
                    id.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_blob_register_callback_adapter_owned(
                    id.as_ptr(),
                    std::ptr::null(),
                    std::ptr::dangling_mut(),
                    Some(release)
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
        }
        assert_eq!(
            RELEASED.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a refusal keeps ctx with the caller"
        );
    }
}
