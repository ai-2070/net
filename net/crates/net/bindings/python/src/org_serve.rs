//! OSDK-L Workstream P — the `serve_org` provider verb for Python.
//!
//! Split from `org.rs` only for length; it is the same module surface. The
//! handler bridge follows `PyRpcHandler` exactly: the Python callable runs
//! inside `spawn_blocking` under `Python::attach`, with a bounded timeout and a
//! "must return bytes" contract.

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};

use super::org::org_serve_error;

/// Application status for a handler that raised — the same value the typed nRPC
/// layer uses. A handler cannot counterfeit an admission denial (0x0009).
const ORG_HANDLER_ERROR: u16 = 0x8001;

/// Build the `caller` dict handed to the Python handler: the five verified
/// fields plus `is_same_org`, all ids as `bytes`.
fn caller_dict<'py>(
    py: Python<'py>,
    caller: &net_sdk::org::OrgCaller,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("entity", PyBytes::new(py, caller.entity.as_bytes()))?;
    d.set_item("acting_org", PyBytes::new(py, caller.acting_org.as_bytes()))?;
    d.set_item(
        "provider_org",
        PyBytes::new(py, caller.provider_org.as_bytes()),
    )?;
    d.set_item("provider", PyBytes::new(py, caller.provider.as_bytes()))?;
    d.set_item("capability", PyBytes::new(py, caller.capability.as_bytes()))?;
    d.set_item("is_same_org", caller.is_same_org())?;
    Ok(d)
}

/// Handle for a served organization service. `close()` unregisters.
#[pyclass(name = "OrgServeHandle", module = "_net")]
pub struct PyOrgServeHandle {
    inner: parking_lot::Mutex<Option<net_sdk::mesh_rpc::ServeHandle>>,
}

#[pymethods]
impl PyOrgServeHandle {
    /// Unregister the service. Idempotent. In-flight handlers run to
    /// completion.
    fn close(&self) {
        let _ = self.inner.lock().take();
    }

    #[getter]
    fn is_closed(&self) -> bool {
        self.inner.lock().is_none()
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<Bound<'_, PyAny>>,
        _exc_val: Option<Bound<'_, PyAny>>,
        _exc_tb: Option<Bound<'_, PyAny>>,
    ) -> bool {
        self.close();
        false
    }
}

/// Serve a protected, privately-discoverable service.
///
/// `access` is `"same_org"` or `"granted"` and selects both who may call AND
/// how the service is announced — both ship only inside an encrypted audience.
/// The handler is `handler(caller: dict, request: bytes) -> bytes`; `caller`
/// carries the five verified fields plus `is_same_org`. Raising surfaces as an
/// application error, never as an admission denial.
///
/// Requires an installed node authority.
#[pyfunction]
#[pyo3(signature = (mesh, service, access, handler, handler_timeout_ms=None))]
pub fn serve_org(
    mesh: &crate::mesh_bindings::NetMesh,
    service: String,
    access: &str,
    handler: Py<PyAny>,
    handler_timeout_ms: Option<u64>,
) -> PyResult<PyOrgServeHandle> {
    let node = mesh.node_arc_clone()?;
    let access = super::org::access_from_str(access)?;
    let timeout = match handler_timeout_ms {
        Some(0) => Duration::from_secs(u64::from(u32::MAX)),
        Some(ms) => Duration::from_millis(ms),
        None => Duration::from_secs(60),
    };
    let callable = Arc::new(handler);

    // `serve_org_bytes_node` -> `serve_rpc_*` spawns an inbound-event bridge
    // with a bare `tokio::spawn`, which needs an ambient runtime. This is a sync
    // `#[pyfunction]` called on the Python thread with no runtime — enter the
    // mesh's for the registration (the a2a binding does the same for the same
    // reason). Without it, the first live serve panics "there is no reactor
    // running"; the refusal-only tests never reached it.
    let runtime = mesh.runtime_arc();
    let handle = {
        let _guard = runtime.enter();
        net_sdk::org::serve_org_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller, body: bytes::Bytes| {
                let callable = callable.clone();
                async move { run_py_org_handler(callable, caller, body, timeout).await }
            },
        )
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(org_serve_error(&e)))?
    };

    Ok(PyOrgServeHandle {
        inner: parking_lot::Mutex::new(Some(handle)),
    })
}

/// Invoke the Python handler with the verified caller and request bytes.
///
/// A raised exception maps to the application band (the handler said no); a
/// marshaling failure maps to internal. Neither is ever an admission denial.
async fn run_py_org_handler(
    callable: Arc<Py<PyAny>>,
    caller: net_sdk::org::OrgCaller,
    body: bytes::Bytes,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, net_sdk::org::OrgHandlerError> {
    let callable = Python::attach(|py| callable.clone_ref(py));
    let result = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, (bool, String)> {
            Python::attach(|py| -> Result<Vec<u8>, (bool, String)> {
                let caller_obj = caller_dict(py, &caller)
                    .map_err(|e| (false, format!("failed to build caller: {e}")))?;
                let req_bytes = PyBytes::new(py, &body);
                let args = PyTuple::new(py, [caller_obj.into_any(), req_bytes.into_any()])
                    .map_err(|e| (false, format!("failed to build args: {e}")))?;
                match callable.call1(py, args) {
                    Ok(ret) => ret
                        .into_bound(py)
                        .extract::<Vec<u8>>()
                        .map_err(|e| (false, format!("org handler must return bytes: {e}"))),
                    // `true` = application error (handler raised); `false` =
                    // internal marshaling failure.
                    Err(pyerr) => Err((true, format!("org handler raised: {pyerr}"))),
                }
            })
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(out))) => Ok(bytes::Bytes::from(out)),
        Ok(Ok(Err((true, msg)))) => Err(net_sdk::org::OrgHandlerError::Application {
            code: ORG_HANDLER_ERROR,
            message: msg,
        }),
        Ok(Ok(Err((false, msg)))) => Err(net_sdk::org::OrgHandlerError::Internal(msg)),
        Ok(Err(join_err)) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "org handler task panicked: {join_err}"
        ))),
        Err(_) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "org handler did not respond within {} ms",
            timeout.as_millis()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Provisioning — the operator/startup steps that make the surface usable
// ---------------------------------------------------------------------------

/// Install an adopted node authority from ``authority_dir`` — the directory
/// ``net node adopt`` wrote.
///
/// REQUIRED before ``OrgClient.bind`` can succeed or a ``"granted"`` service
/// can serve. This is node STARTUP (loading already-adopted files), not
/// adoption (the ceremony that mints them — that stays in the CLI). The node's
/// identity must be the one the membership names, so the mesh must have been
/// created with the matching ``identity_seed``.
#[pyfunction]
pub fn install_org_authority(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    authority_dir: String,
) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    // Blocking file I/O (opens + validates the adopted authority dir) — release
    // the GIL so other Python threads keep running, matching the crate's
    // convention for blocking work.
    py.detach(|| {
        net_sdk::org::install_org_authority_node(&node, std::path::Path::new(&authority_dir))
    })
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

/// Install a provider grant audience so a ``"granted"`` service can seal
/// envelopes: the grant this node's org issued (wire ``bytes``) plus its
/// out-of-band secret (a ``str`` PATH — the raw key never enters Python).
///
/// A ``"same_org"`` provider does NOT need this.
#[pyfunction]
pub fn install_provider_grant_audience(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    grant: &[u8],
    audience_secret_path: String,
) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    // Copy the borrowed grant bytes so the blocking loader (opens + validates
    // the secret file) can run with the GIL RELEASED without touching
    // Python-owned memory off-thread.
    let grant = grant.to_vec();
    py.detach(|| {
        net_sdk::org::install_provider_grant_audience_node(
            &node,
            &grant,
            std::path::Path::new(&audience_secret_path),
        )
    })
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}
