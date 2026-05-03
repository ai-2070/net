//! C FFI for the consumer-side Redis Streams dedup helper.
//!
//! Mirrors `net_sdk::RedisStreamDedup`. See that module's docs for
//! the dedup contract.
//!
//! # Surface
//!
//! ```c
//! /* Lifecycle. */
//! net_redis_dedup_t* net_redis_dedup_new(size_t capacity);
//! void net_redis_dedup_free(net_redis_dedup_t*);
//!
//! /* Test-and-insert. Returns:
//!  *   1 = duplicate (caller should skip the entry)
//!  *   0 = new       (caller should process AND we've now marked it seen)
//!  *  -1 = NULL pointer
//!  *  -2 = invalid UTF-8 in dedup_id
//!  */
//! int net_redis_dedup_is_duplicate(
//!     net_redis_dedup_t* handle,
//!     const char* dedup_id);
//!
//! /* Inspection. */
//! size_t net_redis_dedup_len(net_redis_dedup_t*);
//! size_t net_redis_dedup_capacity(net_redis_dedup_t*);
//! int    net_redis_dedup_is_empty(net_redis_dedup_t*);  /* 1 = empty, 0 = non-empty, negative on NULL */
//! void   net_redis_dedup_clear(net_redis_dedup_t*);
//! ```
//!
//! `capacity == 0` selects the helper default (4096) — matches
//! `RedisStreamDedup::new()`. Callers that need a tiny LRU should
//! pass `1` explicitly. NULL-handle behavior is operation-specific:
//! `is_duplicate` and `is_empty` return `-1`; `len` and `capacity`
//! return `0`; `clear` and `free` are no-ops.
//!
//! # Thread safety
//!
//! Each handle wraps a `Mutex<RedisStreamDedup>`. Concurrent calls
//! across threads on the same handle serialize through the mutex —
//! no UB, but no parallelism either. The expected usage shape is
//! one helper per consumer goroutine / thread (each with its own
//! LRU).

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

/// Opaque handle for the dedup helper. Crossed as `void*` from C.
pub struct RedisStreamDedupHandle {
    inner: Mutex<crate::adapter::RedisStreamDedup>,
}

/// Run an FFI body under `catch_unwind`. With `panic = "unwind"`
/// (Rust's default), any panic inside an `extern "C"` function would
/// be UB across the cgo / N-API / cffi boundary. The shim catches
/// the unwind, logs at error level, and returns a caller-supplied
/// fallback value.
///
/// The body is wrapped in `AssertUnwindSafe` because every entry
/// point here is FFI-style — the work is short, side-effect-only
/// against handles owned externally, and a panic mid-function leaves
/// no observable Rust state for the caller to misuse afterwards.
#[inline]
fn ffi_guard<R>(name: &'static str, fallback: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            tracing::error!(
                ffi_function = name,
                "panic caught in net_redis_dedup FFI; returning fallback to avoid \
                 UB across the C boundary",
            );
            fallback
        }
    }
}

/// Create a helper. `capacity == 0` means use the default (4096).
/// Returns a heap-allocated handle the caller must free with
/// `net_redis_dedup_free`. Never returns NULL — `Box::into_raw`
/// only fails on global allocator failure, which aborts the
/// process (same as every other crate-internal `Box::new` path).
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_new(capacity: usize) -> *mut RedisStreamDedupHandle {
    ffi_guard("net_redis_dedup_new", std::ptr::null_mut(), || {
        let inner = if capacity == 0 {
            crate::adapter::RedisStreamDedup::new()
        } else {
            crate::adapter::RedisStreamDedup::with_capacity(capacity)
        };
        Box::into_raw(Box::new(RedisStreamDedupHandle {
            inner: Mutex::new(inner),
        }))
    })
}

/// Free a helper handle. NULL is a no-op.
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_free(handle: *mut RedisStreamDedupHandle) {
    ffi_guard("net_redis_dedup_free", (), || {
        if handle.is_null() {
            return;
        }
        // Safety: caller upheld the handle-ownership contract documented
        // on `net_redis_dedup_new`.
        unsafe {
            drop(Box::from_raw(handle));
        }
    })
}

/// Test-and-insert. Returns 1 on duplicate, 0 on new, negative on
/// error (-1 NULL, -2 invalid UTF-8). See module docs for the
/// canonical consumer pattern.
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_is_duplicate(
    handle: *mut RedisStreamDedupHandle,
    dedup_id: *const c_char,
) -> c_int {
    ffi_guard("net_redis_dedup_is_duplicate", -1, || {
        if handle.is_null() || dedup_id.is_null() {
            return -1;
        }
        // Safety: caller-supplied null-terminated C string.
        let id = unsafe { CStr::from_ptr(dedup_id) };
        let Ok(id_str) = id.to_str() else {
            return -2;
        };
        // Safety: handle is non-NULL and points at a `Box`-allocated
        // `RedisStreamDedupHandle` per the constructor contract.
        let h = unsafe { &*handle };
        let mut guard = h
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_duplicate(id_str) {
            1
        } else {
            0
        }
    })
}

/// Number of distinct ids currently tracked. Returns 0 on NULL
/// handle (mirrors the "no ids" semantic).
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_len(handle: *mut RedisStreamDedupHandle) -> usize {
    ffi_guard("net_redis_dedup_len", 0, || {
        if handle.is_null() {
            return 0;
        }
        let h = unsafe { &*handle };
        let guard = h
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.len()
    })
}

/// Configured maximum capacity. Returns 0 on NULL handle.
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_capacity(handle: *mut RedisStreamDedupHandle) -> usize {
    ffi_guard("net_redis_dedup_capacity", 0, || {
        if handle.is_null() {
            return 0;
        }
        let h = unsafe { &*handle };
        let guard = h
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.capacity()
    })
}

/// Returns 1 if no ids are tracked, 0 if the helper has at least
/// one id, -1 on NULL handle.
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_is_empty(handle: *mut RedisStreamDedupHandle) -> c_int {
    ffi_guard("net_redis_dedup_is_empty", -1, || {
        if handle.is_null() {
            return -1;
        }
        let h = unsafe { &*handle };
        let guard = h
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_empty() {
            1
        } else {
            0
        }
    })
}

/// Clear all tracked ids. NULL is a no-op.
#[unsafe(no_mangle)]
pub extern "C" fn net_redis_dedup_clear(handle: *mut RedisStreamDedupHandle) {
    ffi_guard("net_redis_dedup_clear", (), || {
        if handle.is_null() {
            return;
        }
        let h = unsafe { &*handle };
        let mut guard = h
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clear();
    })
}

#[cfg(test)]
mod tests {
    //! C-FFI smoke tests. The Rust helper is exhaustively tested in
    //! `sdk/src/redis_dedup.rs`; here we just pin that the FFI
    //! shims agree with the Rust semantic on the canonical
    //! producer-retry scenario, plus error-shape tests for NULL
    //! and invalid-UTF-8 inputs.
    use super::*;
    use std::ffi::CString;
    use std::ptr;

    /// `ffi_guard` must catch panics rather than letting them
    /// unwind across the `extern "C"` boundary. With `panic =
    /// "unwind"` (the Rust default for `cdylib`), an uncaught
    /// panic crossing into cgo / N-API is undefined behavior.
    /// Pin the catch-and-fallback shape so a regression where the
    /// guard is removed (or `panic = "abort"` is the only line of
    /// defense) is surfaced by the test rather than discovered
    /// in a downstream binding's segfault.
    #[test]
    fn ffi_guard_catches_panic_and_returns_fallback() {
        // The `f` closure panics; the guard must catch and return
        // the supplied fallback (-42).
        let v = ffi_guard("test_guard", -42i32, || {
            panic!("intentional FFI panic");
        });
        assert_eq!(v, -42);

        // Sanity: a non-panicking body returns its own value.
        let v = ffi_guard("test_guard", 0i32, || 7);
        assert_eq!(v, 7);
    }

    #[test]
    fn null_handle_returns_negative() {
        let id = CString::new("anything").unwrap();
        assert_eq!(
            net_redis_dedup_is_duplicate(ptr::null_mut(), id.as_ptr()),
            -1,
        );
        assert_eq!(net_redis_dedup_len(ptr::null_mut()), 0);
        assert_eq!(net_redis_dedup_capacity(ptr::null_mut()), 0);
        assert_eq!(net_redis_dedup_is_empty(ptr::null_mut()), -1);
        // free + clear are no-ops; just verify they don't crash.
        net_redis_dedup_free(ptr::null_mut());
        net_redis_dedup_clear(ptr::null_mut());
    }

    #[test]
    fn null_dedup_id_returns_negative() {
        let h = net_redis_dedup_new(8);
        assert_eq!(net_redis_dedup_is_duplicate(h, ptr::null()), -1);
        net_redis_dedup_free(h);
    }

    #[test]
    fn lifecycle_round_trip_filters_duplicates() {
        let h = net_redis_dedup_new(0); // 0 → default 4096
        assert_eq!(net_redis_dedup_capacity(h), 4096);
        assert_eq!(net_redis_dedup_is_empty(h), 1);

        let id_a = CString::new("deadbeef:0:0:0").unwrap();
        let id_b = CString::new("deadbeef:0:0:1").unwrap();

        // First observation: 0 (not duplicate).
        assert_eq!(net_redis_dedup_is_duplicate(h, id_a.as_ptr()), 0);
        assert_eq!(net_redis_dedup_is_duplicate(h, id_b.as_ptr()), 0);
        assert_eq!(net_redis_dedup_len(h), 2);
        assert_eq!(net_redis_dedup_is_empty(h), 0);

        // Retry path: 1 (duplicate).
        assert_eq!(net_redis_dedup_is_duplicate(h, id_a.as_ptr()), 1);
        assert_eq!(net_redis_dedup_is_duplicate(h, id_b.as_ptr()), 1);

        net_redis_dedup_clear(h);
        assert_eq!(net_redis_dedup_len(h), 0);
        assert_eq!(net_redis_dedup_is_empty(h), 1);

        net_redis_dedup_free(h);
    }

    #[test]
    fn capacity_zero_is_clamped_to_default() {
        let h = net_redis_dedup_new(0);
        assert_eq!(net_redis_dedup_capacity(h), 4096);
        net_redis_dedup_free(h);
    }

    #[test]
    fn explicit_capacity_round_trips() {
        let h = net_redis_dedup_new(8192);
        assert_eq!(net_redis_dedup_capacity(h), 8192);
        net_redis_dedup_free(h);
    }

    /// Invalid UTF-8 in the dedup_id pointer surfaces as `-2`,
    /// distinct from `-1` (NULL). Pre-fix this would have either
    /// silently mis-decoded the bytes or panicked across the FFI
    /// boundary; the explicit error code lets callers branch on the
    /// actual failure mode.
    ///
    /// `dedup_id` strings produced by the Net Redis adapter are
    /// always ASCII (`{nonce:hex}:{shard}:{seq}:{i}`), so this
    /// only fires under stream-side corruption — but we want a
    /// clean error rather than UB when it does.
    #[test]
    fn invalid_utf8_dedup_id_returns_minus_two() {
        use std::ffi::CString;

        let h = net_redis_dedup_new(8);

        // `CString::new` rejects interior NULs but accepts arbitrary
        // bytes. Build a NUL-terminated buffer with a stray 0xC0
        // (invalid UTF-8 — start of a 2-byte sequence with no
        // continuation) and pass its pointer.
        //
        // `CString::from_vec_unchecked` is unsafe but the safety
        // contract is "no interior NULs," which holds for the bytes
        // below.
        let bad: CString = unsafe { CString::from_vec_unchecked(vec![0xC0, 0x41]) };
        let rc = net_redis_dedup_is_duplicate(h, bad.as_ptr());
        assert_eq!(rc, -2, "invalid UTF-8 dedup_id must return -2, got {rc}");

        // The bad input did NOT mutate the helper.
        assert_eq!(net_redis_dedup_len(h), 0);

        net_redis_dedup_free(h);
    }

    /// Pin that the C FFI's Mutex wrapping correctly serializes
    /// concurrent access from multiple threads on a single handle.
    /// The Rust helper is `!Sync`; the FFI wraps in
    /// `Mutex<RedisStreamDedup>` so concurrent calls are safe but
    /// serialize. A future refactor that drops the Mutex (e.g.
    /// "RedisStreamDedup is internally synchronized now") would
    /// make this test data-race UB under Miri / TSan.
    ///
    /// The shape: N threads each call `is_duplicate` over a
    /// disjoint id range. Every call must succeed (no panics, no
    /// returns ∉ {0, 1}). Final `len()` equals the union of all
    /// inserted ids — proving every call's mutation reached the
    /// helper.
    #[test]
    fn concurrent_threads_on_one_handle_serialize_safely() {
        use std::ffi::CString;
        use std::sync::Arc;
        use std::thread;

        const THREADS: usize = 8;
        const PER_THREAD: usize = 100;
        const TOTAL: usize = THREADS * PER_THREAD;

        let h = net_redis_dedup_new(TOTAL);
        // Wrap the raw pointer in something `Send` — the pointer
        // itself isn't `Send` due to its `*mut` shape, but the
        // documented C-side contract is "you may share this handle
        // across threads; concurrent calls serialize internally."
        // We trust that contract here.
        struct HandleSend(*mut RedisStreamDedupHandle);
        unsafe impl Send for HandleSend {}
        unsafe impl Sync for HandleSend {}
        let shared = Arc::new(HandleSend(h));

        let mut handles = Vec::with_capacity(THREADS);
        for tid in 0..THREADS {
            let shared = shared.clone();
            handles.push(thread::spawn(move || {
                for i in 0..PER_THREAD {
                    let id = CString::new(format!("t{tid}-id{i}")).unwrap();
                    let rc = net_redis_dedup_is_duplicate(shared.0, id.as_ptr());
                    assert!(
                        rc == 0 || rc == 1,
                        "thread {tid} id {i}: rc {rc} ∉ {{0, 1}} — \
                         concurrent FFI call returned an error code; \
                         Mutex serialization may be broken",
                    );
                    // Every id in this test is unique, so rc==1
                    // (duplicate) would be wrong.
                    assert_eq!(
                        rc, 0,
                        "thread {tid} id {i}: expected new (0), got duplicate (1)"
                    );
                }
            }));
        }
        for h in handles {
            h.join().expect("test thread panicked");
        }

        // Every insert reached the helper.
        assert_eq!(
            net_redis_dedup_len(h),
            TOTAL,
            "expected {TOTAL} ids tracked after concurrent inserts; \
             missing ids → concurrent calls dropped mutations",
        );

        net_redis_dedup_free(h);
    }
}
