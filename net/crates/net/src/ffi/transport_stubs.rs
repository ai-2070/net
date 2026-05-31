//! Feature-OFF stubs for the transport FFI symbols.
//!
//! `ffi::transport` is gated on the full
//! `net + dataforts + netdb + redex-disk` quad (see `ffi::mod`), so a
//! `libnet` cdylib built without all four is missing every
//! `net_serve_blob_transfer` / `net_fetch_blob*` / `net_store_dir` /
//! `net_fetch_dir` / `net_dir_manifest_read` / `net_transport_free_buffer`
//! symbol. cgo / dlsym consumers — notably the Go binding's
//! `transport.go`, which links these symbols unconditionally — then fail
//! at program load with `undefined symbol`.
//!
//! This module lives outside the transport gate and emits stub
//! definitions for those symbols. Each stub returns
//! `NET_ERR_FEATURE_NOT_BUILT` (or null / no-op for the pointer-return
//! and free functions) so callers route to a clean typed error rather
//! than a load-time crash. Mirrors `ffi::blob_stubs`.
//!
//! Active when the cortex surface (`netdb + redex-disk`, which provides
//! `NET_ERR_FEATURE_NOT_BUILT` and the redex / mesh symbols Go links
//! against) is compiled in but the transport quad is NOT fully
//! satisfied — i.e. `net` or `dataforts` is off. Builds without
//! `netdb + redex-disk` expose no cortex surface at all, so a Go
//! consumer in that shape can't link any redex / mesh / blob entry
//! point and this stub set is moot.

#![cfg(all(
    feature = "netdb",
    feature = "redex-disk",
    not(all(feature = "net", feature = "dataforts"))
))]

use std::ffi::{c_char, c_int, c_void};

use super::cortex::NET_ERR_FEATURE_NOT_BUILT;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_serve_blob_transfer(
    _node: *const c_void,
    _adapter: *const c_void,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fetch_blob(
    _node: *const c_void,
    _holder_id: u64,
    _hash: *const u8,
    _out_bytes: *mut *mut u8,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fetch_blob_discovered(
    _node: *const c_void,
    _hash: *const u8,
    _out_bytes: *mut *mut u8,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_store_dir(
    _adapter: *const c_void,
    _root_path: *const c_char,
    _out_manifest_ref: *mut *mut u8,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_fetch_dir(
    _node: *const c_void,
    _source_id: u64,
    _manifest_ref: *const u8,
    _manifest_ref_len: usize,
    _dest_path: *const c_char,
    _out_files: *mut u64,
    _out_bytes: *mut u64,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_dir_manifest_read(
    _node: *const c_void,
    _source_id: u64,
    _manifest_ref: *const u8,
    _manifest_ref_len: usize,
    _out_json: *mut *mut c_char,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_FEATURE_NOT_BUILT
}

/// Free a transport byte buffer. The transport functions in this build
/// never hand one out (every stub errors before allocating), so this is
/// a no-op on every realistic call path; the symbol exists only so the
/// Go binding's unconditional `net_transport_free_buffer` link resolves.
///
/// # Safety
/// `ptr` may be null; if non-null it must originate from a matching
/// transport call (which on this build never happens).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_transport_free_buffer(_ptr: *mut u8, _len: usize) {}

#[cfg(test)]
mod tests {
    //! Contract checks on the stub bodies — every `c_int` return is
    //! `NET_ERR_FEATURE_NOT_BUILT`. Cheap defense against drift.
    use super::*;

    #[test]
    fn stubs_return_feature_not_built_when_transport_off() {
        let null = std::ptr::null::<c_void>();
        // SAFETY: every stub is a pure constant/no-op return — it never
        // dereferences its pointer args, so NULL is fine.
        unsafe {
            assert_eq!(
                net_serve_blob_transfer(null, null),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_fetch_blob(
                    null,
                    0,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_fetch_blob_discovered(
                    null,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_store_dir(null, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut()),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_fetch_dir(
                    null,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            assert_eq!(
                net_dir_manifest_read(
                    null,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NET_ERR_FEATURE_NOT_BUILT
            );
            // Free stub is a no-op; just exercise it for coverage.
            net_transport_free_buffer(std::ptr::null_mut(), 0);
        }
    }
}
