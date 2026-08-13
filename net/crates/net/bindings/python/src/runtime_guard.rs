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
//! # Why it went unnoticed
//!
//! CI installs the binding with `maturin develop`, which builds
//! **debug**. There the panic unwinds, tokio's worker absorbs it, and
//! 884 tests pass with the failure showing up only as a line on stderr
//! that pytest captures. Nothing was watching for it.
//!
//! It is worth being careful about what this panic was observed to do,
//! because an earlier account of it was wrong.
//!
//! *Policy:* a pyo3 extension built with an effective
//! `panic = "abort"` terminates its host process on any internal Rust
//! panic — `__fastfail` on MSVC — taking a Jupyter kernel or a web
//! worker down with no traceback. That is why the wheel now ships the
//! `python-release` profile, which unwinds.
//!
//! *Evidence:* this particular panic was seen on a `--release` wheel
//! and the process **survived**, which an effective abort profile
//! cannot do. So we do not know that the shipped extension actually
//! used abort, and we have not established any link between this panic
//! and the separate `0xC0000409` termination that first drew attention
//! here. Both were real; the causal chain between them was assumed,
//! not shown.
//!
//! What is established, and is reason enough for the guard: dropping a
//! runtime inside an async context is a defect wherever it happens,
//! and it aborts under any build that does abort.
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

    /// Files whose runtime construction is deliberately not wrapped,
    /// with the reason. Keep this short and justified.
    const UNWRAPPED_BY_DESIGN: &[(&str, &str)] = &[(
        "async_bridge.rs",
        "process-static `OnceLock<Runtime>`: never dropped, so it cannot \
         reach the blocking-drop path. Held by value because \
         `init_with_runtime` needs a `&Runtime` outliving every awaitable.",
    )];

    /// Every runtime the binding builds must be wrapped **where it is
    /// built**, not where it is stored.
    ///
    /// The original sweep for this defect wrapped runtimes at their
    /// struct fields, which looks equivalent and is not. Two sites
    /// constructed a bare `Runtime` and only wrapped it several
    /// fallible steps later — `Net::new` had twenty early exits in
    /// that window, `PaymentHttpClient::new` one. On any of those error
    /// paths the bare runtime dropped unguarded, and a caller already
    /// inside a runtime got exactly the panic the guard exists to
    /// prevent.
    ///
    /// Testing the two known sites would pin the two known sites. This
    /// scans the crate so the next one fails here instead.
    #[test]
    fn every_runtime_is_guarded_at_its_construction_site() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0usize;
        let mut unguarded: Vec<String> = Vec::new();

        let mut files: Vec<_> = std::fs::read_dir(&src)
            .expect("read src/")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .collect();
        files.sort();

        for path in files {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == "runtime_guard.rs" {
                continue; // this file's own tests build runtimes on purpose
            }
            if UNWRAPPED_BY_DESIGN.iter().any(|(f, _)| *f == name) {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read source");
            let lines: Vec<&str> = body.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                // Prose that mentions `Runtime::new()` is not a
                // construction. Without this the scan flags the comments
                // explaining the guard, and — worse — inflates `checked`,
                // so the vacuity floor below could be satisfied entirely
                // by comments.
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                let builds = code.contains("Runtime::new()")
                    || code.contains("runtime::Builder::new_multi_thread()")
                    || code.contains("runtime::Builder::new_current_thread()");
                if !builds {
                    continue;
                }
                checked += 1;

                // The wrap may sit on the construction line, or open a
                // few lines above it for a multi-line builder chain.
                // Anything further away is a window where a bare
                // runtime is live across fallible code.
                let lo = i.saturating_sub(3);
                let hi = (i + 3).min(lines.len());
                if !lines[lo..hi]
                    .iter()
                    .any(|l| l.contains("GuardedRuntime::new"))
                {
                    unguarded.push(format!("{name}:{}: {}", i + 1, line.trim()));
                }
            }
        }

        assert!(
            unguarded.is_empty(),
            "runtime(s) built without a GuardedRuntime at the construction \
             site:\n  {}\n\nWrap at construction:\n\n    let rt = \
             Arc::new(GuardedRuntime::new(Runtime::new()?));\n\nWrapping only \
             where the runtime is finally stored leaves every fallible step \
             in between dropping a bare runtime, which panics if the caller \
             is inside a runtime. If a site genuinely cannot drop (a \
             never-dropped static), add it to UNWRAPPED_BY_DESIGN with the \
             reason.",
            unguarded.join("\n  ")
        );

        // Guard the guard: a rename or a move would make the loop above
        // find nothing and pass vacuously.
        assert!(
            checked >= 6,
            "expected at least 6 runtime construction sites, found {checked} — \
             did they move or get renamed? A vacuous pass here means this \
             invariant is unenforced."
        );
    }
}
