//! A tokio runtime that can be dropped from anywhere.
//!
//! # The defect
//!
//! Dropping a `tokio::runtime::Runtime` blocks until its worker threads
//! wind down. Tokio refuses to do that from inside an async context and
//! panics instead:
//!
//! ```text
//! thread 'tokio-rt-worker' panicked at tokio/src/runtime/blocking/shutdown.rs:51:
//! Cannot drop a runtime in a context where blocking is not allowed.
//! This happens when a runtime is dropped from within an asynchronous context.
//! ```
//!
//! A Python extension hits this without doing anything unusual. Every
//! object here holds an `Arc<GuardedRuntime>`, and CPython can run a garbage
//! collection at any point where the GIL is held — including inside a
//! pyo3 callback executing on one of that runtime's own worker threads.
//! If the collected cycle holds the last `Arc`, the runtime's `Drop`
//! runs on a worker thread and tokio panics.
//!
//! Nothing in the binding schedules that; it depends on allocation
//! timing, which is why it surfaced as a test suite that died in a
//! different place each run.
//!
//! # Why it was invisible
//!
//! The workspace sets `panic = "abort"` for `[profile.release]`. CI
//! tests the binding via `maturin develop`, which builds **debug**, so
//! the panic unwinds, tokio's worker absorbs it, and 884 tests pass.
//! The published wheel is built `--release`, where the same panic calls
//! `abort()` — `__fastfail` on MSVC, exit code `0xC0000409` — and takes
//! the host process with it. A Jupyter kernel or a web server would
//! die with no traceback.
//!
//! So the configuration under test and the configuration that ships
//! disagreed about whether a panic is recoverable, and only the tested
//! one was ever exercised.
//!
//! # The fix
//!
//! [`GuardedRuntime`] checks, at drop time, whether it is inside a
//! runtime. Outside one, it drops normally — blocking until tasks
//! finish, which is the clean shutdown. Inside one, it calls
//! [`Runtime::shutdown_background`], which detaches the worker threads
//! and returns immediately; tokio provides it for exactly this case.
//!
//! The asymmetry is deliberate. `shutdown_background` does not wait for
//! tasks, so making it the unconditional path would turn every ordinary
//! shutdown into a detach and leak threads. It is used only where the
//! alternative is a panic.

use std::ops::Deref;

use tokio::runtime::{Handle, Runtime};

/// A [`Runtime`] whose `Drop` is safe on any thread, including its own
/// workers. See the module docs.
///
/// Derefs to the wrapped `Runtime`, so `block_on`, `spawn` and
/// `handle` work unchanged.
pub(crate) struct GuardedRuntime {
    /// `None` only between `Drop::drop` taking it and the value going
    /// away; every other access is `Some`.
    inner: Option<Runtime>,
}

impl GuardedRuntime {
    /// Wrap `rt`.
    pub(crate) fn new(rt: Runtime) -> Self {
        Self { inner: Some(rt) }
    }
}

impl Deref for GuardedRuntime {
    type Target = Runtime;

    #[expect(
        clippy::expect_used,
        reason = "`inner` is only taken by Drop, which consumes the value; no \
                  deref can observe None"
    )]
    fn deref(&self) -> &Runtime {
        self.inner.as_ref().expect("runtime taken only by Drop")
    }
}

impl Drop for GuardedRuntime {
    fn drop(&mut self) {
        let Some(rt) = self.inner.take() else {
            return;
        };
        if Handle::try_current().is_ok() {
            // We are on a thread owned by *some* runtime — very likely
            // this one, via a GC that ran inside a pyo3 callback.
            // Dropping here would block, which tokio refuses: it
            // panics, and under `panic = "abort"` that ends the host
            // process. Detach instead.
            rt.shutdown_background();
        } else {
            // The ordinary path: block until the workers are done, so
            // a caller that drops its last handle really has finished.
            drop(rt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outside a runtime, drop is the ordinary blocking shutdown.
    #[test]
    fn dropping_outside_a_runtime_is_clean() {
        let guarded = GuardedRuntime::new(Runtime::new().expect("runtime"));
        assert!(
            Handle::try_current().is_err(),
            "this test must run outside a runtime or it proves nothing"
        );
        drop(guarded);
    }

    /// The regression: dropping the last handle from inside a runtime
    /// must not panic.
    ///
    /// Before the guard this aborted the process in a release build and
    /// panicked on a worker thread in a debug one. `block_on` puts us
    /// in an async context, which is what a pyo3 callback running on a
    /// worker thread looks like to tokio.
    #[test]
    fn dropping_inside_a_runtime_does_not_panic() {
        let outer = Runtime::new().expect("outer runtime");
        let guarded = GuardedRuntime::new(Runtime::new().expect("inner runtime"));

        outer.block_on(async move {
            assert!(
                Handle::try_current().is_ok(),
                "block_on must put us in an async context"
            );
            drop(guarded);
        });
    }

    /// The same, through an `Arc` — the shape the binding actually
    /// uses. The last clone going away inside a task is precisely the
    /// GC case.
    #[test]
    fn dropping_the_last_arc_inside_a_task_does_not_panic() {
        let outer = Runtime::new().expect("outer runtime");
        let guarded =
            std::sync::Arc::new(GuardedRuntime::new(Runtime::new().expect("inner runtime")));
        let moved = std::sync::Arc::clone(&guarded);
        drop(guarded);

        outer.block_on(async move {
            // `moved` is the last reference; dropping it here runs
            // GuardedRuntime::drop on a runtime worker.
            drop(moved);
        });
    }

    /// The wrapper stays usable — a guard that broke `block_on` would
    /// pass the tests above and break the binding.
    #[test]
    fn the_wrapped_runtime_still_runs_work() {
        let guarded = GuardedRuntime::new(Runtime::new().expect("runtime"));
        let answer = guarded.block_on(async { 6 * 7 });
        assert_eq!(answer, 42);
    }
}
