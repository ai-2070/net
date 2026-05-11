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

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tokio::runtime::Runtime;

use ::net::adapter::net::dataforts::{
    global_blob_adapter_registry, publish_blob, resolve_payload, BlobAdapter,
    BlobError as InnerBlobError, BlobRef as InnerBlobRef, FileSystemAdapter,
};

use crate::cortex::CortexError;

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
            inner: InnerBlobRef::new(uri, arr, size),
        })
    }

    #[getter]
    fn version(&self) -> u8 {
        self.inner.version
    }

    #[getter]
    fn uri(&self) -> &str {
        &self.inner.uri
    }

    /// 32-byte BLAKE3 hash of the content.
    #[getter]
    fn hash<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.hash)
    }

    #[getter]
    fn size(&self) -> u64 {
        self.inner.size
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
        format!(
            "BlobRef(uri={:?}, size={}, hash={})",
            self.inner.uri,
            self.inner.size,
            hex32(&self.inner.hash)
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
    let adapter: Arc<dyn BlobAdapter> =
        Arc::new(FileSystemAdapter::new(adapter_id.clone(), PathBuf::from(root)));
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
    global_blob_adapter_registry().unregister(adapter_id).is_some()
}

/// True if `adapter_id` resolves to a registered adapter.
#[pyfunction]
pub fn blob_adapter_registered(adapter_id: &str) -> bool {
    global_blob_adapter_registry().get(adapter_id).is_some()
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
        .ok_or_else(|| PyKeyError::new_err(format!("blob adapter {:?} not registered", adapter_id)))?;
    let rt = shared_runtime()?;
    let data = data.to_vec();
    let bytes = py
        .detach(|| rt.block_on(async move { publish_blob(adapter.as_ref(), uri, &data).await }))
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
        .ok_or_else(|| PyKeyError::new_err(format!("blob adapter {:?} not registered", adapter_id)))?;
    let rt = shared_runtime()?;
    let payload = payload.to_vec();
    let bytes = py
        .detach(|| rt.block_on(async move { resolve_payload(&payload, adapter.as_ref()).await }))
        .map_err(map_blob_err)?;
    Ok(PyBytes::new(py, &bytes))
}
