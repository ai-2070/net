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
#[cfg(feature = "net")]
use pyo3::types::PyBytes;

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
// Server-side dispatcher event loop.
//
// `pyo3_async_runtimes::tokio::into_future(coro)` calls
// `asyncio.ensure_future` via the bridge's *current* TaskLocals — the
// event loop that drives Rust→Python coroutine bridging. Locals are
// installed by `future_into_py` for the Python→Rust direction (a
// Python caller awaiting a Rust future), but server-handler dispatch
// runs in the reverse direction: a substrate tokio worker calls into
// Python and needs to drive an `async def` handler to completion.
// There's no Python loop running on the tokio worker — calling
// `into_future` from there raises `RuntimeError: no running event loop`.
//
// Fix: spin a single daemon Python thread that runs an asyncio event
// loop forever, and surface it as `TaskLocals` via
// `dispatcher_locals(py)`. `PyAsyncRpcHandler` then dispatches the
// handler coroutine through `into_future_with_locals(&locals, coro)`,
// which `call_soon_threadsafe`s onto the dispatcher loop. The
// dispatcher thread runs the coroutine; the Rust future resolves when
// the coroutine returns or raises.
//
// One dispatcher thread is enough because handler coroutines are
// cooperative — they `await` other I/O and yield back to the loop.
// A handler that blocks the loop (e.g. a long sync `time.sleep`)
// blocks every other in-flight handler, same as any asyncio
// deployment with one event loop.
// ============================================================================

#[cfg(feature = "net")]
static DISPATCHER_LOOP: OnceLock<pyo3::Py<pyo3::PyAny>> = OnceLock::new();

/// Lazily spawn the daemon dispatcher thread and return its
/// `TaskLocals`. Idempotent — subsequent calls return the same
/// loop. First call boots a Python `threading.Thread` whose target
/// runs `asyncio.run_forever()` on a fresh event loop.
#[cfg(feature = "net")]
#[allow(dead_code)] // Consumed by mesh_rpc.rs async-handler bridges.
pub fn dispatcher_locals(py: Python<'_>) -> PyResult<pyo3_async_runtimes::TaskLocals> {
    if let Some(loop_) = DISPATCHER_LOOP.get() {
        return Ok(pyo3_async_runtimes::TaskLocals::new(loop_.bind(py).clone()));
    }
    // First call — boot the dispatcher thread. The script creates
    // a fresh event loop, schedules `run_forever` on a daemon
    // thread, and exposes the loop on the script's globals so we
    // can fish it back out into Rust.
    let globals = pyo3::types::PyDict::new(py);
    py.run(
        c"\
import asyncio
import threading
_loop = asyncio.new_event_loop()
def _runner():
    asyncio.set_event_loop(_loop)
    _loop.run_forever()
threading.Thread(
    target=_runner,
    daemon=True,
    name='net-py-async-dispatcher',
).start()
",
        Some(&globals),
        None,
    )?;
    let loop_bound = globals.get_item("_loop")?.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err(
            "dispatcher_locals: _loop not captured by bootstrap script",
        )
    })?;
    let loop_obj: pyo3::Py<pyo3::PyAny> = loop_bound.clone().unbind();
    // OnceLock::set is fallible on race; if a sibling thread won,
    // discard ours (its loop is fine too — both run forever and
    // dispatch coroutines the same way).
    let _ = DISPATCHER_LOOP.set(loop_obj);
    let stored = DISPATCHER_LOOP.get().expect("just set");
    Ok(pyo3_async_runtimes::TaskLocals::new(
        stored.bind(py).clone(),
    ))
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

// ============================================================================
// await_substrate — marker wrapper for substrate calls that don't yet
// thread a cancel-token through.
//
// `mesh_rpc.rs` routes through `await_with_cancel` because
// `MeshNode::call(..., CallOptions { cancel_token, .. })` accepts the
// token. Other SDK surfaces (`SdkRegistryClient::list`,
// `FoldQueryClient::query_latest`, deck admin commits, blob
// store/fetch, etc.) don't currently accept a cancel-token parameter
// at the SDK boundary — adding one is a substrate-side surface
// change, out of scope for this binding.
//
// In the meantime, asyncio task-cancel still works: the wrapper
// tokio task is dropped, which drops the inner future. Rust's
// drop semantics cancel the in-flight `.await` cooperatively. The
// difference vs `await_with_cancel` is purely on the substrate
// side — no CANCEL frame fires to the server, so a server-side
// long-running handler keeps running until natural completion.
// Client-side observers see the call as cancelled either way.
//
// Routing through this named helper instead of raw
// `future_into_py` documents the intent at every call site and
// gives a single upgrade point once the SDK exposes
// cancel-token surfaces.
// ============================================================================

#[allow(dead_code)] // Consumed by aggregator / blob / cortex / deck / meshos.
pub fn await_substrate<'py, F, T, E>(py: Python<'py>, fut: F) -> PyResult<Bound<'py, PyAny>>
where
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    T: for<'p> pyo3::IntoPyObject<'p> + Send + 'static,
    E: Into<PyErr> + Send + 'static,
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move { fut.await.map_err(Into::into) })
}

// ============================================================================
// BytesReply — zero-extra-copy reply value for awaitables.
//
// Async substrate calls return `bytes::Bytes` payloads. The naive path
// `.map(|reply| reply.body.to_vec())` heap-allocates a `Vec<u8>`,
// memcpys into it, then `IntoPyObject` allocates a `PyBytes` and
// memcpys again — 2× the work of the sync path's single
// `PyBytes::new(py, body.as_ref())`.
//
// `BytesReply` wraps the substrate `Bytes` directly. Its
// `IntoPyObject` impl runs on the awaitable's resume step (GIL is
// held there) and produces a `PyBytes` with one memcpy from the
// `Bytes`'s underlying slice — same cost as the sync path.
// ============================================================================

#[cfg(feature = "net")]
#[allow(dead_code)] // Consumed by mesh_rpc.rs async call paths.
pub struct BytesReply(pub bytes::Bytes);

#[cfg(feature = "net")]
impl<'py> pyo3::IntoPyObject<'py> for BytesReply {
    type Target = PyBytes;
    type Output = Bound<'py, Self::Target>;
    type Error = PyErr;
    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        Ok(PyBytes::new(py, self.0.as_ref()))
    }
}

// ============================================================================
// await_with_notify — cancel-bridge for non-mesh subsystems.
//
// Mesh-substrate calls go through `await_with_cancel` /
// `await_with_existing_token`, which use the v3 `Mesh::cancel(token)`
// path. Subsystems that don't reach the mesh (raw `Redex` tail,
// `MemoriesAdapter` fold watch, etc.) still benefit from
// asyncio-task-cancel propagation — but they need a generic
// `Arc<Notify>` instead of a substrate cancel-token, because dropping
// the awaitable should trip a notify the inner future watches.
//
// Pattern: caller passes a `shutdown: Arc<Notify>` that the inner
// future selects against. Asyncio task-cancel → tokio task drop →
// our wrapper future drops → `NotifyGuard::drop` fires
// `shutdown.notify_waiters()`, letting the inner future exit cleanly.
// ============================================================================

#[allow(dead_code)] // Watch-iter async siblings consume this.
pub fn await_with_notify<'py, F, T, E>(
    py: Python<'py>,
    shutdown: std::sync::Arc<tokio::sync::Notify>,
    fut: F,
) -> PyResult<pyo3::Bound<'py, pyo3::PyAny>>
where
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    T: for<'p> pyo3::IntoPyObject<'p> + Send + 'static,
    E: Into<pyo3::PyErr> + Send + 'static,
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let mut guard = NotifyGuard {
            notify: shutdown,
            armed: true,
        };
        let result = fut.await;
        guard.armed = false;
        result.map_err(Into::into)
    })
}

/// RAII sibling of [`CancelGuard`] that trips a `tokio::sync::Notify`
/// on drop when armed. Awaitables that resolve normally disarm the
/// guard; asyncio-task-cancel drops the wrapper future with the
/// guard still armed, firing `notify_waiters()` so the inner
/// `select!` exits.
#[allow(dead_code)] // Constructed by await_with_notify.
struct NotifyGuard {
    notify: std::sync::Arc<tokio::sync::Notify>,
    armed: bool,
}

impl Drop for NotifyGuard {
    fn drop(&mut self) {
        if self.armed {
            self.notify.notify_waiters();
        }
    }
}
