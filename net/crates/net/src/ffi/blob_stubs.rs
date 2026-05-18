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

use std::ffi::{c_char, c_int};
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
            net_mesh_blob_adapter_exists(null_handle, std::ptr::null(), 0, std::ptr::null_mut()),
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
        assert!(net_mesh_blob_adapter_prometheus_text(null_handle).is_null());
        assert!(net_mesh_blob_adapter_overflow_config(null_handle).is_null());
        net_mesh_blob_adapter_free(std::ptr::null_mut());
    }
}
