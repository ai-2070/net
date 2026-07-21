//! OSDK-L Workstream P — the organization capability surface for Python.
//!
//! Two verbs, five concepts, and no way to put a discovery key in a Python
//! `bytes`. Marshaling only: every authority decision already happened in
//! `net_sdk::org`, and anything here that looks like a decision is a bug.
//!
//! # The credential asymmetry
//!
//! Public signed credentials — membership, dispatcher grant, capability grants
//! — cross as canonical wire `bytes`. The audience secret does **not**: it is
//! the raw discovery key, and handing it to Python would put it in a GC'd
//! object never zeroized, freely copied, visible in a heap dump. So Python
//! supplies a **path**; Rust opens and validates the file; the key's whole
//! lifetime stays in Rust. There is deliberately no bytes parameter.
//!
//! # Errors
//!
//! `OrgError` and its subclasses carry the `org:` wire vocabulary that
//! `tests/cross_lang_org/error_vectors.json` pins, encoded into the message the
//! way every other Python error domain does it (`ERR_NRPC_PREFIX`). The domain
//! says WHERE the refusal happened; `is_local` exposes it without re-parsing.
//!
//! # Lifecycle
//!
//! Explicit `close()` plus a context manager, matching every disposable in this
//! crate (there is no `__del__` anywhere). A live `OrgClient` holds an
//! `Arc<MeshNode>` and a consumer-audience lease, so an un-closed one keeps
//! ingest authority installed and blocks a clean `NetMesh.shutdown()`. Teardown
//! order: `org_client.close()` → `serve_handle.close()` → `mesh.shutdown()`.

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

pyo3::create_exception!(
    _net,
    OrgError,
    pyo3::exceptions::PyException,
    "Base for organization capability errors. The `domain` attribute says WHERE \
     the refusal happened; `is_local` is True iff nothing was sent. Subclasses: \
     OrgCredentialsError, OrgDiscoveryError, OrgAdmissionDeniedError."
);

pyo3::create_exception!(
    _net,
    OrgCredentialsError,
    OrgError,
    "Local: the credential set could not authorize this call. Nothing was sent."
);

pyo3::create_exception!(
    _net,
    OrgDiscoveryError,
    OrgError,
    "Local: no provider this credential set may call was found. Nothing was sent."
);

pyo3::create_exception!(
    _net,
    OrgAdmissionDeniedError,
    OrgError,
    "Remote: the provider's admission engine refused the call. The reason is one \
     of three coarse buckets by design — a precise remote reason would be a \
     credential oracle."
);

pyo3::create_exception!(
    _net,
    OrgUnclassifiedError,
    OrgError,
    "The `org:` vocabulary could not be parsed — this build and the SDK disagree \
     about the contract. An internal compatibility failure, NOT an admission \
     result: it deliberately does not impersonate one of the four domains."
);

/// Format a `ServeError` for the provider verb — serve failures are local
/// registration problems, not the four call-time domains.
pub(crate) fn org_serve_error(e: &net_sdk::mesh_rpc::ServeError) -> String {
    format!("org:serve_failed: {e}")
}

/// Map an `OrgSdkError` onto the right Python exception, carrying the `org:`
/// wire string so `classify_org_error` and the golden fixture agree.
fn org_err_to_py(e: net_sdk::org::OrgSdkError) -> PyErr {
    let wire = e.to_wire();
    match e.domain() {
        net_sdk::org::OrgErrorDomain::Credentials => OrgCredentialsError::new_err(wire),
        net_sdk::org::OrgErrorDomain::Discovery => OrgDiscoveryError::new_err(wire),
        net_sdk::org::OrgErrorDomain::AdmissionDenied => OrgAdmissionDeniedError::new_err(wire),
        // `org:rpc:` reuses the frozen nRPC vocabulary; surface it under the
        // base class rather than minting a second rpc exception here.
        net_sdk::org::OrgErrorDomain::Rpc => OrgError::new_err(wire),
        net_sdk::org::OrgErrorDomain::Unclassified => OrgUnclassifiedError::new_err(wire),
    }
}

/// Who may call a protected service, and how it is announced. Access implies
/// visibility — both variants ship only inside an encrypted audience.
pub(crate) fn access_from_str(access: &str) -> PyResult<net_sdk::org::OrgAccess> {
    match access {
        "same_org" | "SameOrg" => Ok(net_sdk::org::OrgAccess::SameOrg),
        "granted" | "Granted" => Ok(net_sdk::org::OrgAccess::Granted),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "access must be 'same_org' or 'granted', got {other:?}"
        ))),
    }
}

/// A validated organization credential set.
///
/// Consumed by [`OrgClient.bind`]: binding takes ownership, so a second bind
/// from the same instance fails. Construct a new one to bind again.
#[pyclass(name = "OrgCredentials", module = "_net")]
pub struct PyOrgCredentials {
    inner: parking_lot::Mutex<Option<net_sdk::org::OrgCredentials>>,
}

#[pymethods]
impl PyOrgCredentials {
    /// Validate and assemble a credential set from canonical wire bytes plus
    /// audience-secret file **paths**.
    ///
    /// Verifies every signature and structural relation, and loads each secret
    /// through the checked loader (validates the OPENED file: no symlink
    /// following, regular file, owner-only, exact size). Validity windows are
    /// NOT checked here — credentials are routinely assembled before use.
    #[new]
    #[pyo3(signature = (membership, dispatcher, grants, audience_secret_paths))]
    fn new(
        membership: &[u8],
        dispatcher: &[u8],
        grants: Vec<Vec<u8>>,
        audience_secret_paths: Vec<String>,
    ) -> PyResult<Self> {
        let paths: Vec<std::path::PathBuf> = audience_secret_paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        let inner =
            net_sdk::org::OrgCredentials::from_parts(membership, dispatcher, &grants, &paths)
                .map_err(|e| org_err_to_py(net_sdk::org::OrgSdkError::Credentials(e)))?;
        Ok(Self {
            inner: parking_lot::Mutex::new(Some(inner)),
        })
    }

    fn __repr__(&self) -> String {
        match &*self.inner.lock() {
            Some(_) => "OrgCredentials(unbound)".to_string(),
            None => "OrgCredentials(consumed)".to_string(),
        }
    }
}

impl PyOrgCredentials {
    fn take(&self) -> Option<net_sdk::org::OrgCredentials> {
        self.inner.lock().take()
    }
}

/// A credential set bound to a live mesh — the caller half of the facade.
///
/// Close it when done, or use it as a context manager. See the module docs on
/// teardown order.
#[pyclass(name = "OrgClient", module = "_net")]
pub struct PyOrgClient {
    /// `ArcSwapOption` so `close()` and an in-flight `call` cannot race into a
    /// half-torn state: a call snapshots the client first, and because clones
    /// share one audience lease and one node reference, a snapshot that wins
    /// keeps both alive to completion even if `close()` lands right after.
    inner: ArcSwapOption<net_sdk::org::OrgClient>,
    runtime: Arc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyOrgClient {
    /// Bind credentials to a mesh. Consumes `credentials`.
    ///
    /// Refuses unless the complete private-discovery identity relation holds:
    /// the node's identity was explicitly configured (an org membership names a
    /// durable entity, so a generated ephemeral keypair is refused), a node
    /// authority is installed, its owner org is the membership's org, and the
    /// membership vouches for this node's entity.
    #[staticmethod]
    fn bind(
        mesh: &crate::mesh_bindings::NetMesh,
        credentials: &PyOrgCredentials,
    ) -> PyResult<Self> {
        let node = mesh.node_arc_clone()?;
        let runtime = mesh.runtime_arc();
        let creds = credentials.take().ok_or_else(|| {
            OrgCredentialsError::new_err(
                "org:credentials:already_consumed: these OrgCredentials were already bound; \
                 construct a new set to bind again",
            )
        })?;
        let client = net_sdk::org::OrgClient::bind_node(node, creds).map_err(org_err_to_py)?;
        Ok(Self {
            inner: ArcSwapOption::from_pointee(client),
            runtime,
        })
    }

    /// Call a protected service — bytes in, bytes out.
    ///
    /// Discovers privately, selects one authorized provider, and issues ONE
    /// exact-target call. Never retries: a signed proof is bound to one call id.
    /// Releases the GIL for the duration of the call.
    #[pyo3(signature = (service, request))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        service: String,
        request: &[u8],
    ) -> PyResult<Bound<'py, PyBytes>> {
        // Snapshot first: a concurrent close() cannot pull the lease/node out
        // from under a call that already started.
        let client = self
            .inner
            .load_full()
            .ok_or_else(|| OrgError::new_err("org:closed: this OrgClient has been closed"))?;
        let runtime = self.runtime.clone();
        let body = bytes::Bytes::copy_from_slice(request);
        let reply = py.detach(move || {
            runtime.block_on(async move { client.call_bytes(&service, body).await })
        });
        let reply = reply.map_err(org_err_to_py)?;
        Ok(PyBytes::new(py, &reply))
    }

    /// The organization this client acts for, as 32 raw bytes.
    #[getter]
    fn acting_org<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let client = self
            .inner
            .load_full()
            .ok_or_else(|| OrgError::new_err("org:closed: this OrgClient has been closed"))?;
        Ok(PyBytes::new(py, client.acting_org().as_bytes()))
    }

    /// The entity this client calls as, as 32 raw bytes.
    #[getter]
    fn caller<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let client = self
            .inner
            .load_full()
            .ok_or_else(|| OrgError::new_err("org:closed: this OrgClient has been closed"))?;
        Ok(PyBytes::new(py, client.caller().as_bytes()))
    }

    /// Release the client: drops its audience lease and node reference.
    /// Idempotent. Call before `mesh.shutdown()`.
    fn close(&self) {
        let _ = self.inner.swap(None);
    }

    /// Whether `close` has been called.
    #[getter]
    fn is_closed(&self) -> bool {
        self.inner.load().is_none()
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
