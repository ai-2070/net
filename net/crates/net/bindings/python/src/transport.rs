//! pyo3 bindings for the transport surface — blob transfer + directory
//! transfer over the fairscheduler stream transport (Transport SDK plan,
//! T-D). Mirrors the Rust SDK `net_sdk::transport` surface and the C ABI
//! in `include/net_transport.h`.
//!
//! Exposed to Python as classes/functions on the `_net` module and
//! re-exported through `net_sdk.transport`:
//!
//! - Wire types [`PyTransferControl`] / [`PyTransferHeader`] with
//!   `encode()` / `decode()` — the postcard wire form, byte-identical
//!   across every language tier (locked by the T-B golden vectors).
//! - Stream-id helpers `transfer_stream_id` / `is_transfer_stream_id` /
//!   `next_transfer_stream_id`.
//! - Node-driven ops `serve_blob_transfer`, `fetch_blob`,
//!   `fetch_blob_discovered`, `store_dir`, `fetch_dir` — they take the
//!   `_net.NetMesh` handle (transfer is node-driven) and, for the
//!   store/serve side, a `_net.MeshBlobAdapter`.
//! - [`TransferError`] — the exception raised on any transfer failure.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use net::adapter::net::dataforts::blob::transfer::{TransferControl, TransferHeader};
use net::adapter::net::dataforts::blob::{
    is_transfer_stream_id as core_is_transfer_stream_id,
    next_transfer_stream_id as core_next_transfer_stream_id, transfer_stream_id as core_transfer_stream_id,
    BlobError as InnerBlobError, BlobRef as InnerBlobRef,
};
use net::adapter::net::dataforts::{
    fetch_dir as core_fetch_dir, store_dir as core_store_dir, DirError as InnerDirError,
};
use net::adapter::net::MeshNode;

use crate::blob::{PyBlobRef, PyMeshBlobAdapter};
use crate::mesh_bindings::NetMesh;

pyo3::create_exception!(
    _net,
    TransferError,
    pyo3::exceptions::PyException,
    "Raised on transfer operations: content not found, holder discovery \
     failure, hash mismatch, engine-not-installed, manifest decode, unsafe \
     path, or transport failures. Catch with `except TransferError:`."
);

fn map_blob_err(e: InnerBlobError) -> PyErr {
    TransferError::new_err(format!("{e}"))
}

fn map_dir_err(e: InnerDirError) -> PyErr {
    TransferError::new_err(format!("{e}"))
}

// ── Wire types ──────────────────────────────────────────────────────

/// Transfer control frame (requester → holder): "send me the chunk
/// addressed by `hash`".
#[pyclass(name = "TransferControl", frozen, eq, skip_from_py_object)]
#[derive(Clone, PartialEq)]
pub struct PyTransferControl {
    inner: TransferControl,
}

#[pymethods]
impl PyTransferControl {
    /// Build a `Request` for the 32-byte BLAKE3 content `hash`.
    #[staticmethod]
    fn request(hash: Vec<u8>) -> PyResult<Self> {
        let hash: [u8; 32] = hash
            .try_into()
            .map_err(|_| PyValueError::new_err("hash must be exactly 32 bytes"))?;
        Ok(Self {
            inner: TransferControl::Request { hash },
        })
    }

    /// The 32-byte content hash this control requests.
    #[getter]
    fn hash<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let TransferControl::Request { hash } = &self.inner;
        PyBytes::new(py, hash)
    }

    /// Postcard wire bytes (byte-identical across language tiers).
    fn encode<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = postcard::to_allocvec(&self.inner)
            .map_err(|e| PyValueError::new_err(format!("encode failed: {e}")))?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Decode postcard wire bytes into a `TransferControl`.
    #[staticmethod]
    fn decode(bytes: &[u8]) -> PyResult<Self> {
        let inner = postcard::from_bytes(bytes)
            .map_err(|e| PyValueError::new_err(format!("decode failed: {e}")))?;
        Ok(Self { inner })
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.inner)
    }
}

/// Transfer header (holder → requester): the first data-plane frame,
/// declaring the total length (`Found`) or that the holder lacks the
/// chunk (`NotFound`).
#[pyclass(name = "TransferHeader", frozen, eq, skip_from_py_object)]
#[derive(Clone, PartialEq)]
pub struct PyTransferHeader {
    inner: TransferHeader,
}

#[pymethods]
impl PyTransferHeader {
    /// `Found` — `total_len` bytes of chunk data follow.
    #[staticmethod]
    fn found(total_len: u64) -> Self {
        Self {
            inner: TransferHeader::Found { total_len },
        }
    }

    /// `NotFound` — the holder does not have the chunk.
    #[staticmethod]
    fn not_found() -> Self {
        Self {
            inner: TransferHeader::NotFound,
        }
    }

    /// `True` if this is a `Found` header.
    #[getter]
    fn is_found(&self) -> bool {
        matches!(self.inner, TransferHeader::Found { .. })
    }

    /// The declared total length for a `Found` header, else `None`.
    #[getter]
    fn total_len(&self) -> Option<u64> {
        match self.inner {
            TransferHeader::Found { total_len } => Some(total_len),
            TransferHeader::NotFound => None,
        }
    }

    /// Postcard wire bytes (byte-identical across language tiers).
    fn encode<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = postcard::to_allocvec(&self.inner)
            .map_err(|e| PyValueError::new_err(format!("encode failed: {e}")))?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Decode postcard wire bytes into a `TransferHeader`.
    #[staticmethod]
    fn decode(bytes: &[u8]) -> PyResult<Self> {
        let inner = postcard::from_bytes(bytes)
            .map_err(|e| PyValueError::new_err(format!("decode failed: {e}")))?;
        Ok(Self { inner })
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.inner)
    }
}

// ── Stream-id helpers ───────────────────────────────────────────────

/// Construct a transfer stream id from a per-transfer `nonce`.
#[pyfunction]
fn transfer_stream_id(nonce: u64) -> u64 {
    core_transfer_stream_id(nonce)
}

/// True iff `stream_id` is a blob-transfer stream id.
#[pyfunction]
fn is_transfer_stream_id(stream_id: u64) -> bool {
    core_is_transfer_stream_id(stream_id)
}

/// Allocate a fresh, process-unique transfer stream id.
#[pyfunction]
fn next_transfer_stream_id() -> u64 {
    core_next_transfer_stream_id()
}

// ── Node-driven ops ─────────────────────────────────────────────────

/// Reassemble a whole blob (`Small` = one chunk, `Manifest` = ordered
/// chunk list) from a known `holder` over the transfer transport.
async fn fetch_blob_bytes(
    node: &Arc<MeshNode>,
    holder: u64,
    blob_ref: &InnerBlobRef,
) -> Result<Vec<u8>, InnerBlobError> {
    match blob_ref {
        InnerBlobRef::Small { hash, .. } => {
            Ok(node.transfer_fetch_chunk(holder, *hash).await?.to_vec())
        }
        InnerBlobRef::Manifest {
            chunks,
            total_size,
            ..
        } => {
            let mut buf = Vec::with_capacity(*total_size as usize);
            for chunk in chunks {
                buf.extend_from_slice(&node.transfer_fetch_chunk(holder, chunk.hash).await?);
            }
            Ok(buf)
        }
        InnerBlobRef::Tree { .. } => Err(InnerBlobError::Backend(
            "transfer: BlobRef::Tree not supported by the transport bindings".into(),
        )),
    }
}

/// Install the blob-transfer engine on `mesh` over `adapter`. Required
/// before the node can serve chunks OR fetch. Idempotent.
#[pyfunction]
fn serve_blob_transfer(mesh: &NetMesh, adapter: &PyMeshBlobAdapter) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    node.serve_blob_transfer(adapter.inner_arc());
    Ok(())
}

/// Fetch a whole blob from the known holder `holder_id`, returning the
/// reassembled, BLAKE3-verified bytes.
#[pyfunction]
fn fetch_blob<'py>(
    py: Python<'py>,
    mesh: &NetMesh,
    holder_id: u64,
    blob_ref: &PyBlobRef,
) -> PyResult<Bound<'py, PyBytes>> {
    let node = mesh.node_arc_clone()?;
    let rt = mesh.runtime_arc();
    let blob_ref = blob_ref.as_inner().clone();
    let bytes = py
        .detach(move || rt.block_on(fetch_blob_bytes(&node, holder_id, &blob_ref)))
        .map_err(map_blob_err)?;
    Ok(PyBytes::new(py, &bytes))
}

/// Like [`fetch_blob`] but discovers the holder among connected peers.
#[pyfunction]
fn fetch_blob_discovered<'py>(
    py: Python<'py>,
    mesh: &NetMesh,
    blob_ref: &PyBlobRef,
) -> PyResult<Bound<'py, PyBytes>> {
    let node = mesh.node_arc_clone()?;
    let rt = mesh.runtime_arc();
    let blob_ref = blob_ref.as_inner().clone();
    // Per-chunk discovery for Small/Manifest; Tree is unsupported here.
    let result: Result<Vec<u8>, InnerBlobError> = py.detach(move || {
        rt.block_on(async move {
            match &blob_ref {
                InnerBlobRef::Small { hash, .. } => {
                    Ok(node.transfer_fetch_chunk_discovered(*hash).await?.to_vec())
                }
                InnerBlobRef::Manifest {
                    chunks,
                    total_size,
                    ..
                } => {
                    let mut buf = Vec::with_capacity(*total_size as usize);
                    for chunk in chunks {
                        buf.extend_from_slice(
                            &node.transfer_fetch_chunk_discovered(chunk.hash).await?,
                        );
                    }
                    Ok(buf)
                }
                InnerBlobRef::Tree { .. } => Err(InnerBlobError::Backend(
                    "transfer: BlobRef::Tree not supported by the transport bindings".into(),
                )),
            }
        })
    });
    let bytes = result.map_err(map_blob_err)?;
    Ok(PyBytes::new(py, &bytes))
}

/// Store the local directory at `root` as content-addressed blobs in
/// `adapter`, returning the directory-manifest `BlobRef` (the token a
/// receiver passes to `fetch_dir` / `dir_manifest`).
#[pyfunction]
fn store_dir(
    py: Python<'_>,
    mesh: &NetMesh,
    adapter: &PyMeshBlobAdapter,
    root: String,
) -> PyResult<PyBlobRef> {
    let rt = mesh.runtime_arc();
    let adapter = adapter.inner_arc();
    let root = PathBuf::from(root);
    let blob_ref = py
        .detach(move || rt.block_on(core_store_dir(adapter.as_ref(), &root)))
        .map_err(map_dir_err)?;
    Ok(PyBlobRef::from_inner(blob_ref))
}

/// Fetch the directory whose manifest is `manifest_ref` from `source_id`
/// and reconstruct it under `dest`. Returns `(files_written, bytes_written)`.
#[pyfunction]
fn fetch_dir(
    py: Python<'_>,
    mesh: &NetMesh,
    source_id: u64,
    manifest_ref: &PyBlobRef,
    dest: String,
) -> PyResult<(u64, u64)> {
    let node = mesh.node_arc_clone()?;
    let rt = mesh.runtime_arc();
    let manifest_ref = manifest_ref.as_inner().clone();
    let dest = PathBuf::from(dest);
    let stats = py
        .detach(move || rt.block_on(core_fetch_dir(&node, source_id, &manifest_ref, &dest, 0)))
        .map_err(map_dir_err)?;
    Ok((stats.files as u64, stats.bytes))
}

/// Register the transport classes + functions on the `_net` module.
pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTransferControl>()?;
    m.add_class::<PyTransferHeader>()?;
    m.add("TransferError", m.py().get_type::<TransferError>())?;
    m.add_function(wrap_pyfunction!(transfer_stream_id, m)?)?;
    m.add_function(wrap_pyfunction!(is_transfer_stream_id, m)?)?;
    m.add_function(wrap_pyfunction!(next_transfer_stream_id, m)?)?;
    m.add_function(wrap_pyfunction!(serve_blob_transfer, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_blob, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_blob_discovered, m)?)?;
    m.add_function(wrap_pyfunction!(store_dir, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_dir, m)?)?;
    Ok(())
}
