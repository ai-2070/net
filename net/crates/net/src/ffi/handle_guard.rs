//! Per-FFI-handle quiescing protocol.
//!
//! Cortex and mesh FFI handles are typically `extern "C" fn(*mut Handle, ...)`.
//! Without explicit synchronization between in-flight ops and the
//! handle's `_free` entry point, a foreign caller (Go cgo / Python
//! threads / Node.js workers) racing a `_free` against an active op
//! produces:
//!
//! 1. **Use-after-free on the inner.** `_free` does
//!    `Box::from_raw(handle); drop(...)`; a concurrent op that already
//!    dereferenced `&*handle` keeps reading freed memory.
//!
//! 2. **Use-after-free on the handle box itself.** Even with the
//!    inner held alive via an `Arc<Inner>` clone (e.g.
//!    `MeshStreamHandle._node` keeps the node alive but not the
//!    handle box), a concurrent `_free` can deallocate the outer Box
//!    while the op is still doing pointer-equality / handle-matching
//!    checks via `&*handle`.
//!
//! [`crate::ffi::handle_guard::HandleGuard`] is the shared building
//! block. Each handle struct embeds one inline; every `extern "C"` op
//! gates on [`crate::ffi::handle_guard::HandleGuard::try_enter`];
//! every `_free` drives
//! [`crate::ffi::handle_guard::HandleGuard::begin_free`].
//!
//! ## Soundness: the box must outlive `try_enter`'s `fetch_add`
//!
//! The Dekker-style "set freeing, check active_ops" handshake orders
//! only the atomic operations — `Box::from_raw` is a non-atomic
//! deallocation and can interleave between an op's
//! `&*handle` and the op's `fetch_add(active_ops)`, producing UAF on
//! the freed atomic. The same hazard the parent
//! [`crate::ffi::NetHandle`] addresses by intentionally leaking its
//! box. We adopt the same rule: **never deallocate the handle box
//! once it has been handed to C.** `_free` instead takes the inner
//! out via [`std::mem::ManuallyDrop`] and drops it; the outer box
//! (carrying `HandleGuard`'s atomics + the now-empty
//! `ManuallyDrop`) is leaked permanently. Concurrent ops doing
//! `try_enter` after free safely fetch_add on still-valid memory,
//! observe `freeing=true`, decrement, and bail.
//!
//! The cost is `size_of::<Box<Handle>>()` per `_free` call. Handle
//! types are small (a few pointers + atomics), so total leak grows
//! with cumulative `open + free` cycles — acceptable for the
//! soundness gain.
//!
//! ## Adopting the guard on a NEW handle (checklist)
//!
//! The protocol is applied by convention at each call site rather
//! than via a wrapper type (the inline form is uniform across the
//! cortex / mesh / redis-dedup / aggregator handles). Miss any step
//! and the UAF this module exists to prevent comes back, so when you
//! add a handle:
//!
//! 1. Wrap every inner field in [`std::mem::ManuallyDrop`] and embed
//!    one [`crate::ffi::handle_guard::HandleGuard`] (not in
//!    `ManuallyDrop` — it must outlive the inner so post-free
//!    `try_enter` calls land on valid memory).
//! 2. In every `extern "C"` op, gate on
//!    [`crate::ffi::handle_guard::HandleGuard::try_enter`] *before*
//!    touching any inner field, and return the handle's
//!    "invalid/shutting down" sentinel (NULL / error code) on `None`.
//! 3. Don't hold the returned
//!    [`crate::ffi::handle_guard::HandleOp`] across a long blocking
//!    call — clone the (Arc-backed) inner out, drop the guard, then
//!    re-enter only to write back, so a concurrent `_free` never
//!    waits on a caller-set deadline (see `ffi::aggregator`).
//! 4. In `_free`, gate the `ManuallyDrop::drop` of each inner field
//!    on [`crate::ffi::handle_guard::HandleGuard::begin_free`]
//!    returning `true`; never `Box::from_raw` — leak the box.
//! 5. Any pointer handed back to C must be an owned copy, never a
//!    borrow into a `ManuallyDrop` field (which `_free` can drop).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long [`HandleGuard::begin_free`] will wait for in-flight ops
/// to drain before giving up. On timeout, the caller must NOT take
/// or drop the inner — concurrent ops may still be reading it. The
/// inner is leaked along with the box.
///
/// Five seconds matches the parent [`crate::ffi::NetHandle`]'s
/// `FFI_SHUTDOWN_DEADLINE`; well above any normal op latency
/// (ingest, append, snapshot etc. are all sub-second), large enough
/// to absorb a wedged adapter without reflexively leaking on a
/// transient stall.
pub const FFI_HANDLE_FREE_DEADLINE: Duration = Duration::from_secs(5);

/// Outcome of [`HandleGuard::begin_free_detailed`] — lets a `_free`
/// log accurately instead of treating every non-success the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginFree {
    /// This call won the free race AND in-flight ops drained: the
    /// caller owns the right to take/drop the inner exactly once.
    Drained,
    /// A prior `_free` already won for this handle (benign repeat
    /// free). The caller must NOT touch the inner — the winner has
    /// it — and nothing is leaked by this call, so don't warn.
    AlreadyFreeing,
    /// This call won the free race but in-flight ops did not drain
    /// within the deadline. The caller must leak the inner (a
    /// concurrent op may still be reading it) — worth a warning.
    TimedOut,
}

/// Per-handle quiescing core. Lives inline inside each handle
/// struct. `try_enter` returns a guard that prevents `_free` from
/// completing until dropped; `begin_free` quiesces in-flight ops
/// and prevents new ones.
pub struct HandleGuard {
    /// Set to `true` once `_free` has been called for this handle.
    /// All future `try_enter` calls observe this and bail. Stored
    /// as `AtomicBool` (not a generation counter) because we never
    /// re-use the handle after free — once flipped, never reset.
    freeing: AtomicBool,
    /// Number of in-flight ops currently inside `try_enter`'s guard.
    /// `_free` waits for this to reach zero (with a deadline) before
    /// taking the inner.
    active_ops: AtomicU32,
}

impl HandleGuard {
    /// Construct an empty guard. Use as a `const` initializer when
    /// possible.
    pub const fn new() -> Self {
        Self {
            freeing: AtomicBool::new(false),
            active_ops: AtomicU32::new(0),
        }
    }

    /// Try to enter an FFI operation against this handle.
    ///
    /// Increments `active_ops` first so a concurrent `begin_free`
    /// is forced to observe the increment OR to set `freeing` first
    /// (they synchronize via SeqCst). After the increment, we
    /// re-check `freeing`: if free is in progress, the op cannot
    /// proceed and we decrement back out. Otherwise we return a
    /// guard whose `Drop` decrements.
    ///
    /// Returns `None` if `_free` has already started — the caller
    /// must surface a typed "shutting down / freed" error code and
    /// MUST NOT touch any fields of the handle except this
    /// `HandleGuard` (which lives in still-valid leaked memory).
    pub fn try_enter(&self) -> Option<HandleOp<'_>> {
        // SeqCst: pairs with `begin_free`'s SeqCst freeing-store.
        // The total order ensures every (try_enter, begin_free)
        // pair agrees on which side won — either we observe
        // `freeing=true` (and bail), or `begin_free` observes
        // `active_ops > 0` (and waits).
        self.active_ops.fetch_add(1, Ordering::SeqCst);
        if self.freeing.load(Ordering::SeqCst) {
            self.active_ops.fetch_sub(1, Ordering::AcqRel);
            None
        } else {
            Some(HandleOp { core: self })
        }
    }

    /// Mark the handle as freeing and wait for in-flight ops to
    /// drain. Returns `true` if THIS call won the race to flip
    /// `freeing` AND in-flight ops drained within
    /// [`FFI_HANDLE_FREE_DEADLINE`]. Returns `false` on timeout
    /// OR if a prior caller already flipped `freeing`.
    ///
    /// **Single-winner contract.** Only ONE caller across the
    /// lifetime of this guard ever sees `true`. That winning
    /// caller is the one that owns the right to take the inner
    /// out of `ManuallyDrop` exactly once. Subsequent callers
    /// (whether concurrent or strictly after) see `false` and
    /// MUST NOT touch the inner — the winner has it (or had it,
    /// and dropped it).
    ///
    /// This is what makes `_free` idempotent: a second `_free`
    /// call gates the `ManuallyDrop::take` behind this method's
    /// `true` return, so it bails before the double-take that
    /// would UAF the inner allocation.
    ///
    /// On timeout (winner observed `freeing=false→true` but
    /// drain didn't complete), the caller must NOT take the
    /// inner — concurrent ops may still be holding it. Leak
    /// inner along with the box.
    ///
    /// Future `try_enter` calls will see `freeing=true` and bail,
    /// regardless of whether the winner's drain succeeded, timed
    /// out, or this caller is the loser. "No NEW ops will start"
    /// is set as soon as the winner flips the flag.
    pub fn begin_free(&self, deadline: Duration) -> bool {
        matches!(self.begin_free_detailed(deadline), BeginFree::Drained)
    }

    /// Same protocol as [`Self::begin_free`] but distinguishes the
    /// two `false` outcomes so a caller can log accurately: a benign
    /// repeat `_free` ([`BeginFree::AlreadyFreeing`]) leaks nothing
    /// and shouldn't warn, whereas a genuine drain timeout
    /// ([`BeginFree::TimedOut`]) deliberately leaks the inner and is
    /// worth a warning. The single-winner contract is unchanged:
    /// exactly one caller ever sees [`BeginFree::Drained`].
    pub fn begin_free_detailed(&self, deadline: Duration) -> BeginFree {
        // compare_exchange so only one caller wins the right to
        // flip false→true. Losers (whether racing concurrently
        // or strictly after) get an Err and bail without ever
        // entering the drain loop. SeqCst pairs with
        // `try_enter`'s SeqCst load and matches the rest of the
        // protocol's ordering. Pre-fix this was a `store(true)`
        // which made every caller "win" — the cortex / mesh
        // `_free` then double-took the inner via `ManuallyDrop::
        // take`, UAF on the second call.
        if self
            .freeing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return BeginFree::AlreadyFreeing;
        }
        let start = Instant::now();
        // Spin-with-sleep is appropriate: ops are sub-second; the
        // deadline catches pathological wedge cases. We don't have
        // an OS-level wait primitive on the atomic without
        // platform-specific atomic_wait (stable in Rust 1.89+ but
        // a larger refactor); the 1ms sleep keeps CPU low while
        // the deadline is large enough to absorb normal jitter.
        while self.active_ops.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= deadline {
                return BeginFree::TimedOut;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        BeginFree::Drained
    }

    /// True if `begin_free` has been called for this handle.
    /// Useful for assertions in tests; production paths should use
    /// `try_enter` (which already gates on this).
    #[cfg(test)]
    pub fn is_freeing(&self) -> bool {
        self.freeing.load(Ordering::SeqCst)
    }
}

impl Default for HandleGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard returned by [`HandleGuard::try_enter`]. While alive,
/// `begin_free` is forced to wait — the in-flight count seen by
/// `begin_free` includes this op.
///
/// Holds only a borrow of the [`HandleGuard`] (which lives in the
/// leaked handle box, so the borrow is sound for any duration the
/// op chooses). No public methods — drop is the only operation.
pub struct HandleOp<'a> {
    core: &'a HandleGuard,
}

impl Drop for HandleOp<'_> {
    fn drop(&mut self) {
        self.core.active_ops.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Pin: `try_enter` succeeds on a fresh guard; `Drop`
    /// decrements `active_ops` so a subsequent `begin_free`
    /// drains immediately.
    #[test]
    fn try_enter_succeeds_and_drop_decrements() {
        let g = HandleGuard::new();
        {
            let _op = g.try_enter().expect("fresh guard must accept ops");
            assert_eq!(g.active_ops.load(Ordering::SeqCst), 1);
        }
        assert_eq!(g.active_ops.load(Ordering::SeqCst), 0);
        assert!(g.begin_free(Duration::from_millis(50)));
    }

    /// Pin: `begin_free` flips `freeing` so subsequent `try_enter`
    /// calls bail with `None`.
    #[test]
    fn try_enter_after_free_returns_none() {
        let g = HandleGuard::new();
        assert!(g.begin_free(Duration::from_millis(50)));
        assert!(g.try_enter().is_none());
        // No-op leak: active_ops was already 0 + nothing increments
        // it on a None return path.
        assert_eq!(g.active_ops.load(Ordering::SeqCst), 0);
    }

    /// A `_free` racing an in-flight op must wait for the op to
    /// finish before returning success. Without the guard, `_free`
    /// would be an unconditional `Box::from_raw` and the op's
    /// subsequent dereference would UAF.
    #[test]
    fn begin_free_waits_for_inflight_op() {
        let g = Arc::new(HandleGuard::new());

        // Spawn a worker that holds an op for ~50ms.
        let g_op = g.clone();
        let started = Arc::new(AtomicBool::new(false));
        let started_op = started.clone();
        let worker = std::thread::spawn(move || {
            let op = g_op.try_enter().expect("op must enter before free");
            started_op.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            drop(op);
        });

        // Wait for the worker to enter the op so we don't race the
        // try_enter itself.
        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }

        // begin_free MUST block until the op drops. A pre-fix free
        // would return immediately with the op still running →
        // subsequent inner-drop UAFs the op.
        let t0 = Instant::now();
        let drained = g.begin_free(Duration::from_secs(2));
        let elapsed = t0.elapsed();
        assert!(drained, "begin_free must drain within deadline");
        assert!(
            elapsed >= Duration::from_millis(40),
            "begin_free returned in {:?} — must have waited for the in-flight op",
            elapsed,
        );
        worker.join().unwrap();
    }

    /// Pin: `begin_free` returns `false` on timeout when an op
    /// holds the guard past the deadline. Callers MUST leak the
    /// inner in this case rather than dropping it.
    #[test]
    fn begin_free_times_out_when_op_outlasts_deadline() {
        let g = Arc::new(HandleGuard::new());
        let g_op = g.clone();
        let release = Arc::new(AtomicBool::new(false));
        let release_op = release.clone();
        let worker = std::thread::spawn(move || {
            let op = g_op.try_enter().expect("op must enter");
            while !release_op.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(op);
        });

        // Brief sleep to let the worker enter; deadline is shorter
        // than the worker's hold time.
        std::thread::sleep(Duration::from_millis(20));
        let drained = g.begin_free(Duration::from_millis(50));
        assert!(!drained, "deadline expired with op still in flight");
        // freeing is still set even on timeout — future ops bail.
        assert!(g.is_freeing());
        assert!(g.try_enter().is_none());

        // Let the worker finish so the test thread can join.
        release.store(true, Ordering::SeqCst);
        worker.join().unwrap();
    }

    /// Pin: exactly ONE caller wins the `begin_free` race, even
    /// under concurrent invocation. The single-winner contract
    /// is what makes the per-handle `_free` (which gates
    /// `ManuallyDrop::take` on `begin_free` returning `true`)
    /// idempotent — a second caller that also returned `true`
    /// would double-take the inner and UAF.
    ///
    /// Pre-fix `begin_free` did a plain `store(true)` so every
    /// caller saw `true` and every `_free` re-took the inner.
    /// The post-fix `compare_exchange(false, true)` flips the
    /// flag exactly once and subsequent callers return `false`.
    #[test]
    fn begin_free_has_exactly_one_winner_under_concurrency() {
        const ROUNDS: usize = 32;
        for _ in 0..ROUNDS {
            let g = Arc::new(HandleGuard::new());
            let g1 = g.clone();
            let g2 = g.clone();
            let t1 = std::thread::spawn(move || g1.begin_free(Duration::from_millis(50)));
            let t2 = std::thread::spawn(move || g2.begin_free(Duration::from_millis(50)));
            let r1 = t1.join().unwrap();
            let r2 = t2.join().unwrap();
            assert!(
                r1 ^ r2,
                "exactly one caller must win begin_free; got r1={r1} r2={r2}",
            );
        }
    }

    /// Pin: a strictly-sequential second `begin_free` call after
    /// a successful first call returns `false`. This is the path
    /// every `_free` takes on a second invocation — the second
    /// caller must skip the `ManuallyDrop::take` branch.
    #[test]
    fn begin_free_returns_false_on_second_sequential_call() {
        let g = HandleGuard::new();
        assert!(g.begin_free(Duration::from_millis(50)));
        assert!(
            !g.begin_free(Duration::from_millis(50)),
            "second begin_free must bail — only the first caller \
             owns the right to take the inner",
        );
    }

    /// Pin: `begin_free_detailed` distinguishes the three outcomes,
    /// so `_free` can warn only on a genuine drain timeout and stay
    /// quiet on a benign repeat free.
    #[test]
    fn begin_free_detailed_distinguishes_outcomes() {
        // Fresh guard, no ops → Drained (the winner).
        let g = HandleGuard::new();
        assert_eq!(
            g.begin_free_detailed(Duration::from_millis(50)),
            BeginFree::Drained
        );
        // Second call → AlreadyFreeing (benign repeat, not a timeout).
        assert_eq!(
            g.begin_free_detailed(Duration::from_millis(50)),
            BeginFree::AlreadyFreeing
        );

        // Op held past the deadline → TimedOut for the winner.
        let g2 = Arc::new(HandleGuard::new());
        let g2_op = g2.clone();
        let release = Arc::new(AtomicBool::new(false));
        let release_op = release.clone();
        let worker = std::thread::spawn(move || {
            let op = g2_op.try_enter().expect("op must enter");
            while !release_op.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(op);
        });
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            g2.begin_free_detailed(Duration::from_millis(40)),
            BeginFree::TimedOut
        );
        // After a timed-out winner, a later call is still AlreadyFreeing.
        assert_eq!(
            g2.begin_free_detailed(Duration::from_millis(40)),
            BeginFree::AlreadyFreeing
        );
        release.store(true, Ordering::SeqCst);
        worker.join().unwrap();
    }

    /// Pin: a second `begin_free` after a TIMED-OUT first call
    /// also returns `false`. The first caller's
    /// `compare_exchange` already flipped `freeing=true`, so the
    /// second caller observes the flag and bails — the
    /// already-taken inner (or inner that the timed-out caller
    /// left in place to be leaked) must not be re-taken.
    #[test]
    fn begin_free_returns_false_after_timed_out_first_call() {
        let g = Arc::new(HandleGuard::new());
        let g_op = g.clone();
        let release = Arc::new(AtomicBool::new(false));
        let release_op = release.clone();
        let worker = std::thread::spawn(move || {
            let op = g_op.try_enter().expect("op must enter");
            while !release_op.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(op);
        });

        std::thread::sleep(Duration::from_millis(20));
        // First call times out (op still in flight) — returns false
        // but freeing is set.
        assert!(!g.begin_free(Duration::from_millis(40)));

        // Let the op drain.
        release.store(true, Ordering::SeqCst);
        worker.join().unwrap();

        // Second call must still bail — the first call won the
        // freeing flag even though it timed out, so no second
        // caller may claim the right to take the inner.
        assert!(
            !g.begin_free(Duration::from_millis(50)),
            "second begin_free after a timed-out first call must bail",
        );
    }
}
