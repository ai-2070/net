//! Python binding for Dataforts Phase 3 blob storage.
//!
//! Exposes:
//!
//! - [`PyBlobRef`] — typed handle to out-of-band content.
//! - Module functions for registering Rust-implemented adapters
//!   (only `FileSystemAdapter` in this slice) and routing
//!   `publish_blob` / `resolve_payload` through the registered
//!   adapter.
//!
//! A follow-up slice will add a `PyBlobAdapter` wrapper that lets
//! Python classes implement the trait; that needs a
//! `spawn_blocking` GIL dance per call which this slice keeps out
//! of scope. The current binding lets Python apps use blob storage
//! today through a Rust-backed adapter.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tokio::runtime::Runtime;

use ::net::adapter::net::dataforts::{
    global_blob_adapter_registry, publish_blob, resolve_payload, BlobAdapter,
    BlobError as InnerBlobError, BlobRef as InnerBlobRef, FileSystemAdapter,
    MeshBlobAdapter as InnerMeshBlobAdapter,
};

use crate::cortex::{CortexError, PyRedex};

pyo3::create_exception!(
    _net,
    BlobError,
    pyo3::exceptions::PyException,
    "Raised on dataforts blob operations: hash mismatch, missing \
     content, unsupported URI scheme, adapter / network failures, \
     and BlobRef decode errors. Catch with `except BlobError:`."
);

fn map_blob_err(e: InnerBlobError) -> PyErr {
    BlobError::new_err(format!("{}", e))
}

/// Typed handle to a single content-addressed blob. Round-trips
/// through every binding as a typed value; the `encode()` method
/// produces the wire form (a discriminator-prefixed byte string)
/// suitable for use as an event payload.
#[pyclass(name = "BlobRef", frozen, eq, hash, from_py_object)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyBlobRef {
    inner: InnerBlobRef,
}

#[pymethods]
impl PyBlobRef {
    /// Construct a BlobRef. `hash` must be exactly 32 bytes
    /// (BLAKE3-256 digest of the content the URI resolves to).
    #[new]
    fn new(uri: String, hash: Vec<u8>, size: u64) -> PyResult<Self> {
        if hash.len() != 32 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "BlobRef hash must be 32 bytes, got {}",
                hash.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash);
        Ok(Self {
            inner: InnerBlobRef::small(uri, arr, size),
        })
    }

    #[getter]
    fn version(&self) -> u8 {
        self.inner.version()
    }

    #[getter]
    fn uri(&self) -> &str {
        self.inner.uri()
    }

    /// 32-byte BLAKE3 hash of the content. For Small (the only
    /// variant the Python constructor produces today); v0.2 will
    /// surface chunked manifests via a separate accessor.
    #[getter]
    fn hash<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let hash = self
            .inner
            .small_hash()
            .expect("PyBlobRef constructor only produces Small variants");
        PyBytes::new(py, hash)
    }

    #[getter]
    fn size(&self) -> u64 {
        self.inner.size()
    }

    /// Emit the wire-encoded form (discriminator + version + hash +
    /// size + uri bytes). The result is suitable as an event
    /// payload — pass it to `RedexFile.append` or `Mesh.publish`.
    fn encode<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.encode())
    }

    /// Parse a wire-encoded BlobRef back into a typed handle.
    /// Raises `BlobError` if the bytes are malformed or carry an
    /// unsupported version; returns `None` (Python None) when the
    /// bytes do not start with the discriminator (i.e. an inline
    /// payload, not a blob ref).
    #[staticmethod]
    fn from_encoded(bytes: &[u8]) -> PyResult<Option<Self>> {
        match InnerBlobRef::decode(bytes).map_err(map_blob_err)? {
            Some(inner) => Ok(Some(Self { inner })),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        let hash = self.inner.small_hash().copied().unwrap_or([0; 32]);
        format!(
            "BlobRef(uri={:?}, size={}, hash={})",
            self.inner.uri(),
            self.inner.size(),
            hex32(&hash)
        )
    }
}

impl PyBlobRef {
    /// Internal accessor — future bindings code (e.g. surfacing
    /// `RedexFile::resolve_one` to Python) needs a direct handle
    /// on the inner ref. Pre-allowed via `#[allow(dead_code)]` so
    /// the binding can compile while the consumer of this accessor
    /// is being written.
    #[allow(dead_code)]
    pub(crate) fn as_inner(&self) -> &InnerBlobRef {
        &self.inner
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Register a filesystem-backed BlobAdapter under `adapter_id`.
/// `root` is the on-disk directory the adapter content-addresses
/// blobs under. Raises `BlobError` if `adapter_id` is already in
/// use.
#[pyfunction]
pub fn register_filesystem_blob_adapter(adapter_id: String, root: String) -> PyResult<()> {
    let adapter: Arc<dyn BlobAdapter> = Arc::new(FileSystemAdapter::new(
        adapter_id.clone(),
        PathBuf::from(root),
    ));
    global_blob_adapter_registry()
        .register(adapter)
        .map_err(|e| BlobError::new_err(format!("{}", e)))
}

/// Remove an adapter registration. Returns `True` if an adapter
/// was removed, `False` if no adapter was registered under that id.
/// In-flight fetches through the removed adapter still complete
/// because the registry holds Arc references.
#[pyfunction]
pub fn unregister_blob_adapter(adapter_id: &str) -> bool {
    global_blob_adapter_registry()
        .unregister(adapter_id)
        .is_some()
}

/// True if `adapter_id` resolves to a registered adapter.
#[pyfunction]
pub fn blob_adapter_registered(adapter_id: &str) -> bool {
    global_blob_adapter_registry().get(adapter_id).is_some()
}

/// Drop every adapter registration. Called from the binding's
/// `atexit` hook so any `PyBlobAdapter` holding a `Py<PyAny>` is
/// dropped while the interpreter is still alive and the GIL is
/// still acquirable. Without this, a `Py<PyAny>` cleanup after
/// interpreter finalization aborts the process via PyO3's safety
/// guard. Idempotent.
///
/// Counts drained / missing entries and emits the summary to
/// stderr only when `NET_PY_TRACE_ATEXIT` is set in the
/// environment — quiet by default so production processes don't
/// add noise to their shutdown logs, observable when operators
/// need to debug a shutdown race. The Python binding doesn't link
/// `tracing` directly so this is the lowest-dep diagnostic that
/// works in any install.
#[pyfunction]
pub fn _drain_blob_adapters() {
    let registry = global_blob_adapter_registry();
    let ids = registry.ids();
    let total = ids.len();
    let mut drained = 0usize;
    let mut missing = 0usize;
    for id in ids {
        if registry.unregister(&id).is_some() {
            drained += 1;
        } else {
            // Race: another caller unregistered between `ids()`
            // and our `unregister`. Not a failure; just rare.
            missing += 1;
        }
    }
    if std::env::var_os("NET_PY_TRACE_ATEXIT").is_some() {
        eprintln!(
            "net.py: atexit blob-adapter drain — total={} drained={} missing={}",
            total, drained, missing,
        );
    }
}

/// Snapshot of currently-registered adapter ids.
#[pyfunction]
pub fn blob_adapter_ids() -> Vec<String> {
    global_blob_adapter_registry().ids()
}

fn shared_runtime() -> PyResult<Arc<Runtime>> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    if let Some(rt) = RT.get() {
        return Ok(rt.clone());
    }
    let rt = Runtime::new()
        .map(Arc::new)
        .map_err(|e| CortexError::new_err(format!("tokio runtime build: {}", e)))?;
    Ok(RT.get_or_init(|| rt).clone())
}

/// Write `data` to the adapter registered under `adapter_id` and
/// return the encoded BlobRef bytes ready to use as an event
/// payload. The substrate computes the BLAKE3 hash and verifies
/// the round-trip on the read side, so the returned bytes are
/// safe to publish.
#[pyfunction]
pub fn blob_publish<'py>(
    py: Python<'py>,
    adapter_id: &str,
    uri: String,
    data: &[u8],
) -> PyResult<Bound<'py, PyBytes>> {
    let adapter = global_blob_adapter_registry()
        .get(adapter_id)
        .ok_or_else(|| {
            PyKeyError::new_err(format!("blob adapter {:?} not registered", adapter_id))
        })?;
    let rt = shared_runtime()?;
    // Copy bytes WHILE the GIL is still held. PyO3's `&[u8]`
    // extractor accepts both `bytes` (immutable) and `bytearray`
    // (mutable + resizable). A mutable bytearray's buffer can
    // move or reallocate if another Python thread mutates it
    // while the GIL is released; the previous shape captured a
    // raw pointer + length and copied inside `py.detach()`, which
    // is unsound for the bytearray case. The copy here is
    // bounded by the input size — typical blob payloads are
    // ~70 bytes (wire-encoded BlobRef), so the GIL-hold window
    // is negligible. For multi-MB stores route through
    // `MeshBlobAdapter::store` on a PyBytes argument instead.
    let data_owned = data.to_vec();
    let bytes = py
        .detach(|| -> Result<Vec<u8>, InnerBlobError> {
            rt.block_on(async move { publish_blob(adapter.as_ref(), uri, &data_owned).await })
        })
        .map_err(map_blob_err)?;
    Ok(PyBytes::new(py, &bytes))
}

/// Resolve `payload` to its content bytes. Inline payloads come
/// back as-is; encoded-BlobRef payloads route through the adapter
/// registered under `adapter_id`, fetch + verify, and return the
/// resolved bytes.
#[pyfunction]
pub fn blob_resolve<'py>(
    py: Python<'py>,
    adapter_id: &str,
    payload: &[u8],
) -> PyResult<Bound<'py, PyBytes>> {
    let adapter = global_blob_adapter_registry()
        .get(adapter_id)
        .ok_or_else(|| {
            PyKeyError::new_err(format!("blob adapter {:?} not registered", adapter_id))
        })?;
    let rt = shared_runtime()?;
    // Copy bytes under the GIL — see the rationale in
    // `blob_publish`. PyO3's `&[u8]` accepts mutable `bytearray`,
    // and a raw-pointer capture across `py.detach()` is unsound
    // when the caller's buffer can be mutated by another thread.
    let payload_owned = payload.to_vec();
    let bytes = py
        .detach(|| -> Result<Vec<u8>, InnerBlobError> {
            rt.block_on(async move { resolve_payload(&payload_owned, adapter.as_ref()).await })
        })
        .map_err(map_blob_err)?;
    Ok(PyBytes::new(py, &bytes))
}

// =========================================================================
// Python-implemented BlobAdapter wrapper
// =========================================================================

/// `BlobAdapter` impl that bridges to a Python object holding
/// `store` / `fetch` / `fetch_range` / `exists` methods. Each call
/// crosses the FFI via `spawn_blocking` + `Python::attach` so the
/// tokio worker thread isn't pinned during Python execution.
///
/// **Both sync and `async def` Python methods are supported.** The
/// bridge inspects the return value via `inspect.iscoroutine`:
/// - non-coroutine return → use as-is.
/// - coroutine return → drive to completion via `asyncio.run`.
///
/// This lets adapters that do real I/O (e.g. boto3 / aiobotocore,
/// httpx) live as `async def` methods, while in-memory mock
/// adapters can stay plain sync.
///
/// The Python adapter MUST implement (sync OR async — pick one
/// shape per method, mixing is fine across methods):
/// ```python
/// class MyAdapter:
///     async def store(self, blob_ref: BlobRef, data: bytes) -> None: ...
///     async def fetch(self, blob_ref: BlobRef) -> bytes: ...
///     async def fetch_range(self, blob_ref: BlobRef, start: int, end: int) -> bytes: ...
///     def exists(self, blob_ref: BlobRef) -> bool: ...
/// ```
///
/// Python exceptions raised inside any method bubble up as
/// `BlobError::Backend(str(exc))`. Cleanly distinguishing
/// `NotFound` from other backend errors at the bridge layer
/// requires the Python adapter to expose a dedicated marker —
/// future slice; for now everything collapses to Backend.
pub struct PyBlobAdapter {
    id: String,
    py_obj: Arc<Py<PyAny>>,
}

impl PyBlobAdapter {
    pub fn new(id: String, py_obj: Py<PyAny>) -> Self {
        Self {
            id,
            py_obj: Arc::new(py_obj),
        }
    }
}

fn pyerr_to_backend(label: &str, err: PyErr) -> InnerBlobError {
    InnerBlobError::Backend(format!("{}: {}", label, err))
}

/// A binding-owned asyncio loop, created lazily on the first
/// `async def` adapter call and kept alive on a dedicated
/// background thread. Coroutines from `PyBlobAdapter` are
/// submitted to this loop via `asyncio.run_coroutine_threadsafe`,
/// so every adapter coroutine sees the same loop across calls.
///
/// This avoids the `asyncio.run` per-call footgun: `asyncio.run`
/// builds a fresh loop each invocation and any state the user's
/// adapter shares with another loop (an open `aiohttp` session, a
/// SQLAlchemy async engine, an `aiobotocore` client) explodes with
/// "attached to a different loop" or hangs.
///
/// Users who want their adapter to run on their *application*
/// event loop (e.g. to share state across the rest of their app)
/// must submit it themselves via `run_coroutine_threadsafe`
/// against their own loop. The binding-owned loop is the
/// "no-config" fallback for adapters that don't share state.
struct BindingAsyncLoop {
    loop_obj: Py<PyAny>,
}

impl BindingAsyncLoop {
    fn get_or_init(py: Python<'_>) -> std::result::Result<&'static Self, InnerBlobError> {
        use std::sync::OnceLock;
        static LOOP: OnceLock<BindingAsyncLoop> = OnceLock::new();
        if let Some(l) = LOOP.get() {
            return Ok(l);
        }
        let initialized = Self::build(py)?;
        Ok(LOOP.get_or_init(|| initialized))
    }

    fn build(py: Python<'_>) -> std::result::Result<Self, InnerBlobError> {
        let asyncio = py
            .import("asyncio")
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        let loop_obj = asyncio
            .call_method0("new_event_loop")
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        // Spawn a dedicated thread that pins the loop and runs it
        // forever. The thread holds the GIL only while inside
        // `loop.run_forever()` — Python yields the GIL on every
        // tick, so other Rust threads can re-enter Python freely.
        let threading = py
            .import("threading")
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        let run_forever = loop_obj
            .getattr("run_forever")
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs
            .set_item("target", run_forever)
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        kwargs
            .set_item("daemon", true)
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        let thread = threading
            .call_method("Thread", (), Some(&kwargs))
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        thread
            .call_method0("start")
            .map_err(|e| pyerr_to_backend("async-loop init", e))?;
        Ok(Self {
            loop_obj: loop_obj.unbind(),
        })
    }

    fn loop_for<'py>(&'_ self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.loop_obj.bind(py).clone()
    }
}

/// If `value` is an awaitable / coroutine, drive it on the
/// binding-owned loop via `asyncio.run_coroutine_threadsafe` and
/// return the resolved value; otherwise return it unchanged.
/// Caller holds the GIL.
fn drive_if_coroutine<'py>(
    py: Python<'py>,
    value: Bound<'py, PyAny>,
    label: &str,
) -> std::result::Result<Bound<'py, PyAny>, InnerBlobError> {
    let inspect = py
        .import("inspect")
        .map_err(|e| pyerr_to_backend(label, e))?;
    let is_coro: bool = inspect
        .call_method1("iscoroutine", (&value,))
        .and_then(|r| r.extract::<bool>())
        .map_err(|e| pyerr_to_backend(label, e))?;
    if !is_coro {
        return Ok(value);
    }
    let asyncio = py
        .import("asyncio")
        .map_err(|e| pyerr_to_backend(label, e))?;
    let binding_loop = BindingAsyncLoop::get_or_init(py)?;
    let future = asyncio
        .call_method1(
            "run_coroutine_threadsafe",
            (value, binding_loop.loop_for(py)),
        )
        .map_err(|e| pyerr_to_backend(label, e))?;
    // `concurrent.futures.Future.result()` blocks until done; it
    // releases the GIL internally while waiting on the underlying
    // condition variable, so the binding-owned loop thread can
    // run unhindered.
    future
        .call_method0("result")
        .map_err(|e| pyerr_to_backend(label, e))
}

#[async_trait]
impl BlobAdapter for PyBlobAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    async fn store(&self, blob_ref: &InnerBlobRef, bytes: &[u8]) -> Result<(), InnerBlobError> {
        let obj = self.py_obj.clone();
        let blob = blob_ref.clone();
        let data = bytes.to_vec();
        tokio::task::spawn_blocking(move || -> Result<(), InnerBlobError> {
            Python::attach(|py| {
                let py_blob = PyBlobRef { inner: blob };
                let py_data = PyBytes::new(py, &data);
                let ret = obj
                    .bind(py)
                    .call_method1("store", (py_blob, py_data))
                    .map_err(|e| pyerr_to_backend("store", e))?;
                let _ = drive_if_coroutine(py, ret, "store")?;
                Ok(())
            })
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn fetch(&self, blob_ref: &InnerBlobRef) -> Result<Vec<u8>, InnerBlobError> {
        let obj = self.py_obj.clone();
        let blob = blob_ref.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, InnerBlobError> {
            Python::attach(|py| {
                let py_blob = PyBlobRef { inner: blob };
                let ret = obj
                    .bind(py)
                    .call_method1("fetch", (py_blob,))
                    .map_err(|e| pyerr_to_backend("fetch", e))?;
                let resolved = drive_if_coroutine(py, ret, "fetch")?;
                let bytes: Vec<u8> = resolved
                    .extract()
                    .map_err(|e| pyerr_to_backend("fetch return", e))?;
                Ok(bytes)
            })
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn fetch_range(
        &self,
        blob_ref: &InnerBlobRef,
        range: Range<u64>,
    ) -> Result<Vec<u8>, InnerBlobError> {
        let obj = self.py_obj.clone();
        let blob = blob_ref.clone();
        let start = range.start;
        let end = range.end;
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, InnerBlobError> {
            Python::attach(|py| {
                let py_blob = PyBlobRef { inner: blob };
                let ret = obj
                    .bind(py)
                    .call_method1("fetch_range", (py_blob, start, end))
                    .map_err(|e| pyerr_to_backend("fetch_range", e))?;
                let resolved = drive_if_coroutine(py, ret, "fetch_range")?;
                let bytes: Vec<u8> = resolved
                    .extract()
                    .map_err(|e| pyerr_to_backend("fetch_range return", e))?;
                Ok(bytes)
            })
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }

    async fn exists(&self, blob_ref: &InnerBlobRef) -> Result<bool, InnerBlobError> {
        let obj = self.py_obj.clone();
        let blob = blob_ref.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, InnerBlobError> {
            Python::attach(|py| {
                let py_blob = PyBlobRef { inner: blob };
                let ret = obj
                    .bind(py)
                    .call_method1("exists", (py_blob,))
                    .map_err(|e| pyerr_to_backend("exists", e))?;
                let resolved = drive_if_coroutine(py, ret, "exists")?;
                let flag: bool = resolved
                    .extract()
                    .map_err(|e| pyerr_to_backend("exists return", e))?;
                Ok(flag)
            })
        })
        .await
        .map_err(|e| InnerBlobError::Backend(format!("spawn_blocking join: {}", e)))?
    }
}

/// Register a Python-implemented BlobAdapter. `instance` must be a
/// Python object with `store(blob_ref, data) -> None`,
/// `fetch(blob_ref) -> bytes`,
/// `fetch_range(blob_ref, start, end) -> bytes`, and
/// `exists(blob_ref) -> bool` methods. Each call is dispatched to
/// the Python instance via `spawn_blocking` so the substrate's
/// tokio runtime isn't blocked during Python execution.
///
/// Methods may be `async def`. When they are, the returned
/// coroutine is driven on a binding-owned asyncio loop that runs
/// on a dedicated background thread for the lifetime of the
/// process. All adapter coroutines share that loop, so adapter
/// state can be reused across calls (open `aiohttp` sessions,
/// connection pools, etc.). Adapter code that needs to share
/// state with the *application's* own asyncio loop must submit
/// it through `asyncio.run_coroutine_threadsafe(coro, app_loop)`
/// from inside the adapter method — the binding does not (and
/// cannot) know about the application's loop.
#[pyfunction]
pub fn register_blob_adapter(adapter_id: String, instance: Py<PyAny>) -> PyResult<()> {
    let adapter: Arc<dyn BlobAdapter> = Arc::new(PyBlobAdapter::new(adapter_id.clone(), instance));
    global_blob_adapter_registry()
        .register(adapter)
        .map_err(|e| BlobError::new_err(format!("{}", e)))
}

// =========================================================================
// MeshBlobAdapter — Dataforts v0.2 substrate-owned blob CAS
// =========================================================================

/// Python handle to a [`MeshBlobAdapter`](::net::adapter::net::
/// dataforts::MeshBlobAdapter): the v0.2 substrate-owned blob
/// content-addressable store. Each blob chunk lives as a
/// content-addressed `RedexFile` at `dataforts/blob/<hex32>` on
/// the wrapped `Redex` handle.
///
/// Construct from a `Redex` instance:
///
/// ```python
/// from net import Redex, MeshBlobAdapter, BlobRef
/// redex = Redex(persistent_dir="/var/lib/net/redex")
/// adapter = MeshBlobAdapter(redex, "mesh-app")
/// payload = b"the substrate carries the bytes"
/// blob_ref = BlobRef("mesh://demo",
///                    hashlib.blake3(payload).digest(),
///                    len(payload))
/// adapter.store(blob_ref, payload)
/// back = adapter.fetch(blob_ref)
/// assert back == payload
/// ```
///
/// All adapter methods are blocking from Python's perspective —
/// the binding pumps the substrate's tokio runtime under the
/// hood, releasing the GIL for the duration of each call so
/// concurrent Python threads aren't blocked.
#[pyclass(name = "MeshBlobAdapter")]
pub struct PyMeshBlobAdapter {
    inner: Arc<InnerMeshBlobAdapter>,
    id: String,
}

#[pymethods]
impl PyMeshBlobAdapter {
    /// Construct a substrate-owned blob adapter against `redex`.
    /// `adapter_id` is the operator-facing identity surfaced in
    /// Prometheus + log lines; it doubles as the registry key
    /// when the adapter is also registered via
    /// [`register_blob_adapter`].
    ///
    /// `persistent=True` flips the per-chunk `RedexFile`s into the
    /// on-disk persistent path — requires the underlying `Redex`
    /// to have been constructed with `persistent_dir=...`.
    #[new]
    #[pyo3(signature = (redex, adapter_id, *, persistent = false))]
    fn new(redex: &PyRedex, adapter_id: String, persistent: bool) -> Self {
        let inner = Arc::new(
            InnerMeshBlobAdapter::new(adapter_id.clone(), redex.inner_arc())
                .with_persistent(persistent),
        );
        Self {
            inner,
            id: adapter_id,
        }
    }

    /// Adapter identity (the `adapter_id` passed at construction).
    #[getter]
    fn adapter_id(&self) -> &str {
        &self.id
    }

    fn __repr__(&self) -> String {
        format!("MeshBlobAdapter(id={:?})", self.id)
    }

    /// Store `data` under the content-address declared by
    /// `blob_ref`. The substrate verifies that `blake3(data)`
    /// matches `blob_ref.hash` before persisting; mismatches
    /// raise `BlobError`. Idempotent — repeated stores of
    /// identical bytes against the same hash are a no-op.
    ///
    /// Releases the GIL during the actual store.
    pub fn store(&self, py: Python<'_>, blob_ref: &PyBlobRef, data: &[u8]) -> PyResult<()> {
        let rt = shared_runtime()?;
        let adapter = self.inner.clone();
        let blob = blob_ref.as_inner().clone();
        // Copy bytes WHILE the GIL is held. `&[u8]` accepts
        // mutable `bytearray`, whose backing buffer can move or
        // reallocate if another Python thread mutates it after
        // the GIL is released. Capturing a raw pointer across
        // `py.detach()` and reading inside the closure was
        // unsound for that case.
        let data_owned = data.to_vec();
        py.detach(|| -> Result<(), InnerBlobError> {
            rt.block_on(async move { adapter.store(&blob, &data_owned).await })
        })
        .map_err(map_blob_err)
    }

    /// Fetch the content-addressed bytes for `blob_ref`. Verifies
    /// `blake3(returned) == blob_ref.hash`; raises `BlobError` on
    /// mismatch or missing content.
    pub fn fetch<'py>(
        &self,
        py: Python<'py>,
        blob_ref: &PyBlobRef,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let rt = shared_runtime()?;
        let adapter = self.inner.clone();
        let blob = blob_ref.as_inner().clone();
        let bytes = py
            .detach(|| -> Result<Vec<u8>, InnerBlobError> {
                rt.block_on(async move { adapter.fetch(&blob).await })
            })
            .map_err(map_blob_err)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Fetch a byte range of `blob_ref`'s content. The range is
    /// half-open `[start, end)` — `start` is inclusive, `end` is
    /// exclusive, matching Python slice semantics
    /// (`payload[start:end]`). The substrate does NOT verify
    /// partial fetches against the full-content hash — callers
    /// using range fetch accept that trade-off.
    pub fn fetch_range<'py>(
        &self,
        py: Python<'py>,
        blob_ref: &PyBlobRef,
        start: u64,
        end: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let rt = shared_runtime()?;
        let adapter = self.inner.clone();
        let blob = blob_ref.as_inner().clone();
        let bytes = py
            .detach(|| -> Result<Vec<u8>, InnerBlobError> {
                rt.block_on(async move { adapter.fetch_range(&blob, start..end).await })
            })
            .map_err(map_blob_err)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Probe local presence. Returns `True` when every chunk of
    /// `blob_ref` is reachable on the local node, `False`
    /// otherwise. Doesn't go over the wire.
    pub fn exists(&self, py: Python<'_>, blob_ref: &PyBlobRef) -> PyResult<bool> {
        let rt = shared_runtime()?;
        let adapter = self.inner.clone();
        let blob = blob_ref.as_inner().clone();
        py.detach(|| -> Result<bool, InnerBlobError> {
            rt.block_on(async move { adapter.exists(&blob).await })
        })
        .map_err(map_blob_err)
    }

    /// Render the adapter's Prometheus text body. Operators pipe
    /// the result into an HTTP scrape endpoint.
    pub fn prometheus_text(&self) -> String {
        self.inner.prometheus_text()
    }
}
