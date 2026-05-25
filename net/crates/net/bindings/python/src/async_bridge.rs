//! Bridge between Rust async (tokio) and Python asyncio coroutines.
//!
//! v1 surface (F-1 of `PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md`):
//! - [`init`] sets up `pyo3_async_runtimes::tokio` with a
//!   process-static multi-thread runtime; called once from the
//!   `_net` pymodule init.
//! - [`runtime`] returns a handle to that runtime so other binding
//!   sites that need to spawn / `block_on` can share it instead of
//!   constructing their own.
//!
//! Later slices ([`F-2`](super), [`T1-A3`..]) build atop this with
//! the `await_with_cancel` adapter and the per-shape Async classes.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// Process-global tokio runtime shared between sync `block_on`
/// call sites (the existing pyo3 bindings) and async
/// `future_into_py` call sites (the new `Async*` classes landing
/// in waves T1+).
///
/// Held by value (not `Arc<Runtime>`) because
/// `pyo3_async_runtimes::tokio::init_with_runtime` takes a
/// `&Runtime` reference whose lifetime must outlive every
/// awaitable the bridge ever returns — process-static is the
/// simplest correct lifetime.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Initialize the async bridge. Called once from the `_net`
/// pymodule init; subsequent calls are no-ops (the `OnceLock`
/// guard short-circuits).
///
/// Builds a multi-thread tokio runtime with `net-py-async` worker
/// names and hands the reference to `pyo3_async_runtimes` so
/// `future_into_py(py, fut)` from any later site spawns onto this
/// runtime. The same runtime is also surfaced via [`runtime`] for
/// bindings that previously constructed their own per-instance
/// runtime; T1+ slices will migrate those to share this one.
pub fn init() -> Result<(), std::io::Error> {
    if RUNTIME.get().is_some() {
        return Ok(());
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("net-py-async")
        .build()?;
    // OnceLock::set is fallible on race; if a sibling thread won,
    // discard ours and use theirs. Either way the bridge sees a
    // valid runtime.
    let _ = RUNTIME.set(rt);
    let rt_ref = RUNTIME.get().expect("just set");
    pyo3_async_runtimes::tokio::init_with_runtime(rt_ref)
        .expect("init_with_runtime guarded by OnceLock");
    Ok(())
}

/// Handle to the process-static async runtime. `None` if [`init`]
/// has not been called — should never happen in practice because
/// the pymodule init calls [`init`] before any user code runs.
#[allow(dead_code)] // T1+ slices consume this.
pub fn runtime() -> Option<tokio::runtime::Handle> {
    RUNTIME.get().map(|rt| rt.handle().clone())
}
