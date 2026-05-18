//! FFI shutdown / in-flight Dekker handshake.
//!
//! `FfiOpGuard::try_enter` (`src/ffi/mod.rs:120-148`) and
//! `net_shutdown`'s spin-wait (`src/ffi/mod.rs:820-859`) form a
//! Dekker-style handshake across two SeqCst atomics
//! (`active_ops` + `shutting_down`). It is the single load-bearing
//! primitive preventing use-after-free across every language binding
//! when a caller races shutdown against in-flight ops.
//!
//! The handshake property: either an in-flight op observes
//! `shutting_down=true` and returns `ShuttingDown` (after un-bumping
//! `active_ops`), OR `net_shutdown` observes `active_ops > 0` and
//! spin-waits for the op to drop its guard. Never both
//! "op proceeds" AND "shutdown sees count == 0" — that combo
//! would let shutdown free the handle while an op is still using it.
//!
//! Under SeqCst the bad combo is impossible. Under weaker ordering
//! (a tempting "optimization") it is possible and surfaces as a
//! UAF segfault under load. This test runs the race at high
//! contention; if it crashes, the handshake is broken.
//!
//! Run: `cargo test --test ffi_shutdown_race --release`

use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use net::ffi::{
    net_free_poll_result, net_ingest_raw_ex, net_init, net_poll_ex, net_shutdown, NetEvent,
    NetHandle, NetPollResult, NetReceipt,
};
use rand::RngExt;

const NET_ERR_SUCCESS: i32 = 0;
const NET_ERR_SHUTTING_DOWN: i32 = -8;
const NET_ERR_INGESTION_FAILED: i32 = -5;

#[derive(Clone, Copy)]
struct HandlePtr(*mut NetHandle);
unsafe impl Send for HandlePtr {}

fn empty_poll_result() -> NetPollResult {
    NetPollResult {
        events: ptr::null_mut::<NetEvent>(),
        count: 0,
        next_id: ptr::null_mut::<c_char>(),
        has_more: 0,
    }
}

/// Acceptable ingest codes from a worker that is racing shutdown.
/// `IngestionFailed` is allowed because the bus may report
/// backpressure under high concurrent load — it is unrelated to the
/// handshake we are stressing.
fn is_acceptable_ingest_code(code: i32) -> bool {
    matches!(
        code,
        NET_ERR_SUCCESS | NET_ERR_SHUTTING_DOWN | NET_ERR_INGESTION_FAILED
    )
}

fn is_acceptable_poll_code(code: i32) -> bool {
    matches!(code, NET_ERR_SUCCESS | NET_ERR_SHUTTING_DOWN)
}

/// 8 worker threads slam `net_ingest_raw_ex` and `net_poll_ex`
/// against a single handle. The shutdown thread sleeps a randomized
/// 0–5 ms and then calls `net_shutdown`.
///
/// The handshake guarantees:
///   1. The process does not segfault (no UAF via dangling handle).
///   2. Every code the workers observe is in
///      `{Success, ShuttingDown, IngestionFailed}` for ingest and
///      `{Success, ShuttingDown}` for poll. A code outside that
///      set, or a code returned *after* the worker has observed
///      `ShuttingDown` once, would indicate the guard is leaking.
///   3. `net_shutdown` returns 0 — its spin-wait completed and the
///      handle was freed cleanly.
///
/// 20 outer iterations × 8 workers per iteration. Each worker stops
/// after observing `ShuttingDown` once.
#[test]
fn ffi_shutdown_dekker_handshake_holds_under_contention() {
    const ITERATIONS: usize = 20;
    const WORKERS: usize = 8;
    const MAX_OPS_PER_WORKER: usize = 50_000;

    let total_acceptable = Arc::new(AtomicU64::new(0));
    // Count workers that actually observed `ShuttingDown`. If
    // this stays at 0 across every iteration, we never raced
    // shutdown against a live worker — the test would pass
    // without exercising the thing it claims to cover
    // (cubic-flagged P2).
    let total_saw_shutdown = Arc::new(AtomicU64::new(0));

    for iter in 0..ITERATIONS {
        let handle = unsafe { net_init(ptr::null()) };
        assert!(!handle.is_null(), "net_init failed on iter {iter}");
        let h = HandlePtr(handle);

        let mut workers = Vec::with_capacity(WORKERS);
        for worker_id in 0..WORKERS {
            let counter = total_acceptable.clone();
            let worker_handle = h;
            workers.push(thread::spawn(move || {
                // Bind the Send-wrapped handle inside the closure
                // body so the captured upvar is the wrapper, not the
                // bare `*mut NetHandle` (which is not Send).
                let h = worker_handle;
                // Pre-format JSON outside the hot loop; FFI takes
                // (ptr, len) and len does not include the NUL.
                let json = format!("{{\"w\":{worker_id},\"i\":{iter}}}");
                let json_bytes = json.as_bytes();
                let mut local_acceptable: u64 = 0;
                let mut saw_shutdown = false;

                for _ in 0..MAX_OPS_PER_WORKER {
                    let mut receipt = NetReceipt {
                        shard_id: 0,
                        timestamp: 0,
                    };
                    let code = unsafe {
                        net_ingest_raw_ex(
                            h.0,
                            json_bytes.as_ptr() as *const c_char,
                            json_bytes.len(),
                            &mut receipt as *mut NetReceipt,
                        )
                    };
                    assert!(
                        is_acceptable_ingest_code(code),
                        "ingest worker {worker_id} got unexpected code {code} on iter {iter}"
                    );
                    if code == NET_ERR_SHUTTING_DOWN {
                        saw_shutdown = true;
                        break;
                    }
                    local_acceptable += 1;

                    let mut result = empty_poll_result();
                    let code = unsafe {
                        net_poll_ex(
                            h.0,
                            16,
                            ptr::null::<c_char>(),
                            &mut result as *mut NetPollResult,
                        )
                    };
                    assert!(
                        is_acceptable_poll_code(code),
                        "poll worker {worker_id} got unexpected code {code} on iter {iter}"
                    );
                    if code == NET_ERR_SUCCESS {
                        unsafe { net_free_poll_result(&mut result as *mut NetPollResult) };
                    }
                    if code == NET_ERR_SHUTTING_DOWN {
                        saw_shutdown = true;
                        break;
                    }
                    local_acceptable += 1;
                }

                counter.fetch_add(local_acceptable, Ordering::Relaxed);
                saw_shutdown
            }));
        }

        // Random 0–5 ms delay so shutdown lands in a different phase
        // of the workers' loop on each iteration. Without the
        // randomization, we'd always shutdown at the same moment
        // relative to worker start and miss interesting race
        // windows.
        let delay_ms = rand::rng().random_range(0..=5);
        thread::sleep(Duration::from_millis(delay_ms));

        let rc = unsafe { net_shutdown(h.0) };
        assert_eq!(rc, 0, "net_shutdown failed on iter {iter} (rc={rc})");

        // After net_shutdown returns, every worker must have either
        // exited via ShuttingDown OR exhausted MAX_OPS_PER_WORKER
        // before shutdown began. The latter is fine — it means the
        // worker raced ahead of shutdown entirely. Collect, but do
        // not fail on, the saw_shutdown flag: with very small
        // delays + very fast machines, some workers may finish
        // their entire op budget before shutdown is even called.
        let mut saw_shutdown_count = 0u64;
        for w in workers {
            if w.join().expect("worker thread panicked") {
                saw_shutdown_count += 1;
            }
        }
        total_saw_shutdown.fetch_add(saw_shutdown_count, Ordering::Relaxed);
    }

    // Across the whole test we should have racked up a sizeable
    // number of acceptable ops. This is just a smoke check that the
    // test is doing meaningful work; the real assertions are inside
    // the loop.
    let total = total_acceptable.load(Ordering::Relaxed);
    assert!(
        total >= (ITERATIONS as u64),
        "test did not exercise the FFI surface (only {total} successful ops)"
    );

    // Race-exercise sanity check: at least some workers, across
    // the 20 iterations × 8 workers = 160 worker-runs, must have
    // been mid-op when shutdown landed and returned
    // `ShuttingDown`. If every run finished ahead of shutdown we
    // never actually exercised the Dekker handshake this test
    // claims to cover — the randomized 0–5 ms delay is tuned so
    // most iterations overlap, and a stable zero here would
    // indicate either the delay tuning broke or the FFI started
    // silently buffering past shutdown.
    let shutdown_observers = total_saw_shutdown.load(Ordering::Relaxed);
    assert!(
        shutdown_observers > 0,
        "no worker across {ITERATIONS} iterations observed ShuttingDown — \
         test is not exercising the shutdown-vs-in-flight race",
    );
}

/// Regression: BUG_REPORT.md #1 — under the previous design,
/// `net_shutdown` `Box::from_raw`d the handle storage. The Dekker
/// handshake prevented an in-flight FFI op from *proceeding* past
/// shutdown but did not prevent its `fetch_add` from dereferencing
/// the freed atomic, producing a use-after-free.
///
/// The fix leaks the box on shutdown so the atomics remain valid
/// memory forever; concurrent / late FFI ops still observe
/// `shutting_down=true` and bail before touching `bus`/`runtime`.
///
/// This test calls FFI methods *after* `net_shutdown` returns —
/// technically a contract violation, but one whose consequence under
/// the old code would be UAF. Under the fix, every call must return
/// `ShuttingDown` and the process must remain stable.
#[test]
fn ffi_calls_after_shutdown_return_shutting_down_not_uaf() {
    let handle = unsafe { net_init(ptr::null()) };
    assert!(!handle.is_null());

    // Clean shutdown with no in-flight ops.
    let code = unsafe { net_shutdown(handle) };
    assert_eq!(code, NET_ERR_SUCCESS);

    // Now intentionally violate the "don't use after shutdown"
    // contract from multiple threads. The previous design would
    // segfault here (`fetch_add` on freed atomic). The fix keeps the
    // atomics alive via the leaked box, so every call cleanly returns
    // ShuttingDown.
    let worker_handle = HandlePtr(handle);
    let mut joins = Vec::new();
    for _ in 0..4 {
        joins.push(thread::spawn(move || {
            // Bind the Send-wrapped handle inside the closure body
            // so the captured upvar is the wrapper, not the bare
            // `*mut NetHandle` (which is not Send under Rust 2021
            // disjoint closure capture).
            let h = worker_handle;
            let json = b"{\"x\":1}";
            for _ in 0..1000 {
                let mut receipt = NetReceipt {
                    shard_id: 0,
                    timestamp: 0,
                };
                let code = unsafe {
                    net_ingest_raw_ex(
                        h.0,
                        json.as_ptr() as *const c_char,
                        json.len(),
                        &mut receipt as *mut NetReceipt,
                    )
                };
                assert_eq!(
                    code, NET_ERR_SHUTTING_DOWN,
                    "post-shutdown ingest must return ShuttingDown, got {code}"
                );
            }
        }));
    }
    for j in joins {
        j.join().expect("post-shutdown thread panicked");
    }

    // Calling shutdown again is also legal (idempotent) and must not
    // crash. It returns Unknown because the bus has already been
    // taken out, but that's fine — the contract violation is
    // calling FFI after shutdown, and the caller has been told.
    let code = unsafe { net_shutdown(handle) };
    // Either Success (if a no-op fast path) or Unknown is acceptable;
    // the only thing that must not happen is a crash.
    assert!(code == NET_ERR_SUCCESS || code == -99, "got {code}");
}
