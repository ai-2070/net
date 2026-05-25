//! Bridge between Rust async (tokio) and Python asyncio coroutines.
//!
//! Public surface:
//! - [`init`] sets up `pyo3_async_runtimes::tokio` with a
//!   process-static multi-thread runtime; called once from the
//!   `_net` pymodule init.
//! - [`runtime`] returns a handle to that runtime so other binding
//!   sites that need to spawn / `block_on` can share it instead of
//!   constructing their own.
//! - [`await_with_cancel`] wraps a substrate call's future in a
//!   Python awaitable whose asyncio cancellation propagates to
//!   `MeshNode::cancel(token)` via the v3 substrate primitive.
//!
//! T1+ slices ([`AsyncMeshRpc`](..)) build atop this with the
//! per-shape Async classes.

#[cfg(feature = "net")]
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

#[cfg(feature = "net")]
use ::net::adapter::net::MeshNode;
#[cfg(feature = "net")]
use pyo3::prelude::*;

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

// ============================================================================
// await_with_cancel — substrate-cancel-aware future_into_py
// ============================================================================

/// Wrap a substrate call's future in a Python awaitable whose
/// `asyncio.Task.cancel()` propagates to
/// [`MeshNode::cancel(token)`][cancel] via the v3 substrate
/// primitive. The closure receives the freshly-minted cancel
/// token; populate `opts.cancel_token = Some(token)` on the
/// call's `CallOptions` before kicking off the underlying
/// `node.call(...)` / `node.call_streaming(...)` etc.
///
/// Semantics:
/// - Future resolves normally → cancel guard disarms; the
///   substrate's internal `release(token)` already cleared the
///   registry entry, so the guard's drop is a no-op.
/// - Python `task.cancel()` mid-await → `pyo3_async_runtimes`
///   drops the spawned tokio task → our wrapper future is
///   dropped → [`CancelGuard::drop`] fires `node.cancel(token)`,
///   which triggers `RpcError::Cancelled` on the in-flight
///   substrate call via the cancel-registry `Notify` permit.
///
/// Returns a `Bound<'_, PyAny>` representing the Python
/// awaitable; the user awaits with `await rpc.call(...)`.
///
/// [cancel]: ::net::adapter::net::MeshNode::cancel
#[cfg(feature = "net")]
#[allow(dead_code)] // T1+ slices consume this.
pub fn await_with_cancel<'py, F, T, E, B>(
    py: Python<'py>,
    mesh: &Arc<MeshNode>,
    build_fut: B,
) -> PyResult<Bound<'py, PyAny>>
where
    B: FnOnce(u64) -> F,
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    T: for<'p> pyo3::IntoPyObject<'p> + Send + 'static,
    E: Into<PyErr> + Send + 'static,
{
    let token = mesh.reserve_cancel_token();
    let mesh_for_guard = Arc::clone(mesh);
    let fut = build_fut(token);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let mut guard = CancelGuard {
            mesh: mesh_for_guard,
            token,
            armed: true,
        };
        let result = fut.await;
        // Resolved normally — the substrate's per-shape release
        // already cleared the registry entry, so disarming here
        // just suppresses the redundant cancel call from Drop.
        guard.armed = false;
        result.map_err(Into::into)
    })
}

/// Variant of [`await_with_cancel`] that reuses an existing
/// cancel-token (already reserved by the call's construction
/// path) instead of minting a fresh one. Used by per-chunk pulls
/// on streaming iterators: every `__anext__` shares the
/// construction-time token, so a mid-stream
/// `asyncio.wait_for(..., timeout).cancel()` propagates to the
/// substrate's stream cancel-watcher and terminates the WHOLE
/// stream rather than just dropping one pull.
#[cfg(feature = "net")]
#[allow(dead_code)] // T1-A4+ streaming classes consume this.
pub fn await_with_existing_token<'py, F, T, E>(
    py: Python<'py>,
    mesh: &Arc<MeshNode>,
    token: u64,
    fut: F,
) -> PyResult<Bound<'py, PyAny>>
where
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    T: for<'p> pyo3::IntoPyObject<'p> + Send + 'static,
    E: Into<PyErr> + Send + 'static,
{
    let mesh_for_guard = Arc::clone(mesh);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let mut guard = CancelGuard {
            mesh: mesh_for_guard,
            token,
            armed: true,
        };
        let result = fut.await;
        guard.armed = false;
        result.map_err(Into::into)
    })
}

/// RAII guard whose Drop fires `mesh.cancel(token)` iff still
/// armed when the wrapper future is dropped (the asyncio
/// task-cancel path). Successful resolution disarms before Drop
/// runs, so cancel() is a no-op in the happy case.
#[cfg(feature = "net")]
#[allow(dead_code)] // Constructed by await_with_cancel — used by T1+ slices.
struct CancelGuard {
    mesh: Arc<MeshNode>,
    token: u64,
    armed: bool,
}

#[cfg(feature = "net")]
impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.mesh.cancel(self.token);
        }
    }
}
