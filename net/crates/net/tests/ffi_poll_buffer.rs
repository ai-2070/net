//! Regression coverage for `net_poll`'s buffer-size handling.
//!
//! Pre-fix `net_poll` always invoked `bus.poll(request)` first and
//! only checked the buffer size after the response was already
//! serialized; if the buffer was too small, the function returned
//! `BufferTooSmall` and dropped the response. A caller that
//! trusted the returned `next_id` from a previous call could
//! advance their cursor past unread events.
//!
//! Post-fix:
//! - Buffers smaller than `MIN_RESPONSE_BUFFER` (256 bytes) are
//!   rejected up front, before any adapter work happens.
//! - When a polled response is too large for the caller's buffer,
//!   a minimal fallback JSON is written that echoes the original
//!   cursor as `next_id` (so the caller's retry re-polls the same
//!   range against an idempotent adapter).
//!
//! The default FFI handle uses the noop adapter, which never
//! returns any events from `poll_shard`. That makes the post-poll
//! overflow path unreachable without a real adapter, so this test
//! pins the pre-poll minimum-buffer check (which is what catches
//! the degenerate "tiny buffer" misuse) and the empty-response
//! happy path.

use std::os::raw::c_char;
use std::ptr;

use net::ffi::{
    net_free_poll_result, net_init, net_poll, net_poll_ex, net_shutdown, NetEvent, NetPollResult,
};

const NET_ERR_BUFFER_TOO_SMALL: i32 = -7;

#[test]
fn net_poll_rejects_buffers_below_minimum_without_polling() {
    let handle = net_init(ptr::null());
    assert!(!handle.is_null(), "net_init failed");

    // Buffer of 100 bytes is below the 256-byte minimum and is
    // pre-emptively rejected. A pre-fix run polled the bus first
    // and dropped the response on this path; post-fix the rejection
    // happens before any cursor work.
    let mut buf = vec![0u8; 100];
    let code = net_poll(
        handle,
        ptr::null::<c_char>(),
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
    );
    assert_eq!(
        code, NET_ERR_BUFFER_TOO_SMALL,
        "100-byte buffer must be rejected with BufferTooSmall, got {}",
        code,
    );

    // Even tinier buffer — same rejection.
    let mut tiny = vec![0u8; 10];
    let code = net_poll(
        handle,
        ptr::null::<c_char>(),
        tiny.as_mut_ptr() as *mut c_char,
        tiny.len(),
    );
    assert_eq!(
        code, NET_ERR_BUFFER_TOO_SMALL,
        "10-byte buffer must be rejected with BufferTooSmall, got {}",
        code,
    );

    let _ = net_shutdown(handle);
}

/// Pin: `net_free_poll_result` must be idempotent. Pre-fix it
/// freed `events` and `next_id` but left the `NetPollResult`
/// fields holding the already-freed pointers; a second call —
/// from a defensive caller, a destructor wrapper, or a
/// double-free in a binding — would re-`Box::from_raw` the dead
/// pointers and crash. Post-fix the function nulls the fields
/// after free, so subsequent calls are no-ops.
#[test]
fn net_free_poll_result_is_idempotent() {
    let handle = net_init(ptr::null());
    assert!(!handle.is_null(), "net_init failed");

    let mut result = NetPollResult {
        events: ptr::null_mut::<NetEvent>(),
        count: 0,
        next_id: ptr::null_mut::<c_char>(),
        has_more: 0,
    };

    // First poll — populates the result. Default config + noop
    // adapter returns no events, so `events` and `next_id` may
    // both be null. The idempotency check still holds.
    let code = net_poll_ex(handle, 16, ptr::null::<c_char>(), &mut result as *mut _);
    assert_eq!(code, 0, "net_poll_ex returned {} (expected 0)", code);

    // First free — releases whatever was allocated and nulls
    // the fields.
    net_free_poll_result(&mut result as *mut _);
    assert!(result.events.is_null(), "events not nulled after free");
    assert_eq!(result.count, 0);
    assert!(result.next_id.is_null(), "next_id not nulled after free");
    assert_eq!(result.has_more, 0);

    // Second free — must be a no-op. Pre-fix this would
    // double-free the boxed slice and CString.
    net_free_poll_result(&mut result as *mut _);

    // And a third, just to be sure.
    net_free_poll_result(&mut result as *mut _);

    // Null pointer is also handled.
    net_free_poll_result(ptr::null_mut::<NetPollResult>());

    let _ = net_shutdown(handle);
}

#[test]
fn net_poll_accepts_buffers_at_or_above_minimum() {
    let handle = net_init(ptr::null());
    assert!(!handle.is_null(), "net_init failed");

    // 4 KB comfortably exceeds the minimum, and the noop adapter
    // returns an empty event list so the response easily fits.
    let mut buf = vec![0u8; 4096];
    let code = net_poll(
        handle,
        c"{\"limit\": 10}".as_ptr(),
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
    );
    assert!(
        code >= 0,
        "expected positive byte count from successful empty poll, got {}",
        code,
    );

    // The first `code` bytes of the buffer must be a valid JSON
    // response containing at least an `events` array.
    let written = &buf[..code as usize];
    let s = std::str::from_utf8(written).expect("response is not UTF-8");
    assert!(
        s.contains("\"events\""),
        "response missing events field: {}",
        s,
    );

    let _ = net_shutdown(handle);
}
