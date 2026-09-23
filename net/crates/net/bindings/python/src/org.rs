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

use crate::runtime_guard::GuardedRuntime;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::mesh_rpc::{
    PyAsyncClientStreamCall, PyAsyncDuplexCall, PyAsyncRpcStream, PyClientStreamCall, PyDuplexCall,
    PyRpcStream,
};

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
     result: it deliberately does not impersonate one of the four domains. A \
     live in-version call never raises this; it is produced by \
     `net.org.classify_org_error(..)` when classifying a foreign or \
     version-skewed `org:` string."
);

/// Format a `ServeError` for the provider verb — serve failures are local
/// registration problems, NOT the four call-time domains, so they carry no
/// `org:` prefix (they surface as a plain `RuntimeError`, the same way the
/// `install_*` provisioning functions do). Prefixing them `org:` made
/// `parse_org_error` classify them as the `unknown` domain (`is_local=False`),
/// implying a request may have reached a provider.
pub(crate) fn org_serve_error(e: &net_sdk::mesh_rpc::ServeError) -> String {
    format!("org serve registration failed: {e}")
}

/// Map an `OrgSdkError` onto the right Python exception, carrying the `org:`
/// wire string so `classify_org_error` and the golden fixture agree.
///
/// `pub(crate)`: the streaming handles opened by this module's verbs classify
/// their midstream/terminal errors through here too (§4.4 — "midstream errors
/// through `org_err_to_py`").
pub(crate) fn org_err_to_py(e: net_sdk::org::OrgSdkError) -> PyErr {
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

/// The wire status a provider's admission denial carries (OA2-E2) — the same
/// constant the facade's `map_rpc_error` matches on (`sdk/src/org/call.rs`),
/// restated here because the `*_bytes_deadline` seam contract puts midstream
/// classification in the BINDING ("midstream errors stay `RpcError` on it and
/// the binding classifies them into its own org vocabulary").
const RPC_STATUS_ADMISSION_DENIED: u16 = 0x0009;

/// Classify one raw-handle `RpcError` into the facade's `org:` vocabulary.
///
/// Mirrors `net_sdk::org`'s private `map_rpc_error` / `admission_reason_of`
/// (`sdk/src/org/call.rs:1946-1972`): an admission denial (`0x0009`) decodes
/// to `AdmissionDenied(coarse)` — the coarse bucket and NOTHING else (a
/// precise remote reason would be a credential oracle) — and everything else
/// stays `Rpc`. Pure (no `PyErr`) so the classification is unit-witnessable.
fn org_sdk_error_from_rpc(e: ::net::adapter::net::mesh_rpc::RpcError) -> net_sdk::org::OrgSdkError {
    use ::net::adapter::net::mesh_rpc::RpcError;
    match &e {
        RpcError::ServerError {
            status, message, ..
        } if *status == RPC_STATUS_ADMISSION_DENIED => {
            // The coarse reason rides the denial's one-byte body, rendered
            // lossily into the message — recover it from the single char.
            let mut chars = message.chars();
            let coarse = match (chars.next(), chars.next()) {
                (Some(c), None) => u8::try_from(u32::from(c))
                    .ok()
                    .and_then(net_sdk::org::CoarseAdmissionReason::from_wire),
                _ => None,
            }
            .unwrap_or(net_sdk::org::CoarseAdmissionReason::Denied);
            net_sdk::org::OrgSdkError::AdmissionDenied(coarse)
        }
        _ => net_sdk::org::OrgSdkError::Rpc(e),
    }
}

/// Midstream/terminal error seam for handles the org verbs opened (§4.4):
/// classify into the org vocabulary, surface through [`org_err_to_py`]. A
/// midstream revocation arrives as `OrgAdmissionDeniedError`, deadline/cancel
/// retirement as the base `OrgError` over the frozen `org:rpc:` vocabulary.
pub(crate) fn org_stream_err_to_py(e: ::net::adapter::net::mesh_rpc::RpcError) -> PyErr {
    org_err_to_py(org_sdk_error_from_rpc(e))
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
        py: Python<'_>,
        membership: &[u8],
        dispatcher: &[u8],
        grants: Vec<Vec<u8>>,
        audience_secret_paths: Vec<String>,
    ) -> PyResult<Self> {
        // Copy the borrowed byte slices into owned buffers so the blocking
        // loader (opens + validates each secret file, verifies every signature)
        // can run with the GIL RELEASED without touching Python-owned memory
        // off-thread. `grants` and the paths are already owned.
        let membership = membership.to_vec();
        let dispatcher = dispatcher.to_vec();
        let paths: Vec<std::path::PathBuf> = audience_secret_paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        let inner = py
            .detach(|| {
                net_sdk::org::OrgCredentials::from_parts(&membership, &dispatcher, &grants, &paths)
            })
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
    runtime: Arc<GuardedRuntime>,
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
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let runtime = self.runtime.clone();
        let body = bytes::Bytes::copy_from_slice(request);
        let reply = py.detach(move || {
            runtime.block_on(async move { client.call_bytes(&service, body).await })
        });
        let reply = reply.map_err(org_err_to_py)?;
        Ok(PyBytes::new(py, &reply))
    }

    /// Call a subnet-exported service — bytes in, bytes out
    /// (`SUBNET_AUTH_SDK_PLAN.md` §3.6). Releases the GIL for the call.
    ///
    /// Discovers on the PUBLIC plane through the verified ownership
    /// projection, derives the same-org/granted relation from the
    /// VERIFIED owner, mints the same canonical proof as `call`, and
    /// sends exactly once — never a retry. Deliberately `call_exported`,
    /// not `call_subnet`: the caller names a service, not a subnet — it
    /// never joins the provider's subnet and receives no subnet context.
    #[pyo3(signature = (service, request))]
    fn call_exported<'py>(
        &self,
        py: Python<'py>,
        service: String,
        request: &[u8],
    ) -> PyResult<Bound<'py, PyBytes>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let runtime = self.runtime.clone();
        let body = bytes::Bytes::copy_from_slice(request);
        let reply = py.detach(move || {
            runtime.block_on(async move { client.call_exported_bytes(&service, body).await })
        });
        let reply = reply.map_err(org_err_to_py)?;
        Ok(PyBytes::new(py, &reply))
    }

    /// Call a protected service whose response is a STREAM (OSDK §3; §4.4) —
    /// one request in, byte chunks out. Returns the existing
    /// :class:`RpcStream` (iterate it with ``for chunk in stream:``).
    ///
    /// The provider is pinned for the whole stream — one plan, one signed
    /// opening, never re-resolved while the stream lives — and the call is
    /// NEVER retried (a signed proof is bound to one call id). Dropping or
    /// ``close()``-ing the stream emits exactly one CANCEL. Midstream errors
    /// arrive through the ``org:`` vocabulary (``OrgError`` family — e.g. a
    /// midstream revocation raises ``OrgAdmissionDeniedError``), NOT the
    /// ``RpcError`` family a public ``MeshRpc.call_streaming`` raises.
    ///
    /// ``deadline_ms == 0`` is the facade's default lifetime (300 s, Owner
    /// Q1) and NEVER "no deadline": a protected call's lifetime is finite by
    /// contract (D3). ``cancel_token == 0`` means uncancellable; reserve a
    /// token with :meth:`reserve_cancel_token` (BEFORE the call) and fire it
    /// with :meth:`cancel`.
    #[pyo3(signature = (service, request, deadline_ms=0, cancel_token=0))]
    fn call_streaming<'py>(
        &self,
        py: Python<'py>,
        service: String,
        request: &Bound<'py, PyBytes>,
        deadline_ms: u64,
        cancel_token: u64,
    ) -> PyResult<PyRpcStream> {
        // Snapshot first: a concurrent close() cannot pull the lease/node out
        // from under a call that already started.
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let runtime = self.runtime.clone();
        let body = bytes::Bytes::copy_from_slice(request.as_bytes());
        let stream = py.detach(move || {
            runtime.block_on(async move {
                client
                    .call_streaming_bytes_deadline(&service, body, deadline_ms, cancel_token)
                    .await
            })
        });
        let stream = stream.map_err(org_err_to_py)?;
        Ok(PyRpcStream::from_org_stream(stream, self.runtime.clone()))
    }

    /// Call a protected service with a STREAM OF REQUESTS and one terminal
    /// response (OSDK §3; §4.4). Returns the existing
    /// :class:`ClientStreamCall`: ``send(bytes)`` pushes items (the signed
    /// opening rides the FIRST one), ``finish()`` half-closes the upload and
    /// awaits the terminal response, ``close()`` fires CANCEL. The provider is
    /// pinned HERE — each ``send`` writes into the one pinned call and never
    /// re-resolves anything.
    ///
    /// Same deadline / cancel-token / ``org:`` error semantics as
    /// :meth:`call_streaming`.
    #[pyo3(signature = (service, deadline_ms=0, cancel_token=0))]
    fn call_client_stream(
        &self,
        py: Python<'_>,
        service: String,
        deadline_ms: u64,
        cancel_token: u64,
    ) -> PyResult<PyClientStreamCall> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let runtime = self.runtime.clone();
        let call = py.detach(move || {
            runtime.block_on(async move {
                client
                    .call_client_stream_bytes_deadline(&service, deadline_ms, cancel_token)
                    .await
            })
        });
        let call = call.map_err(org_err_to_py)?;
        Ok(PyClientStreamCall::from_org_stream(
            call,
            self.runtime.clone(),
        ))
    }

    /// Call a protected service BIDIRECTIONALLY (OSDK §3; §4.4). Returns the
    /// existing :class:`DuplexCall`: ``send(bytes)`` pushes request items,
    /// ``finish_sending()`` half-closes the upload, the handle itself iterates
    /// response chunks, and ``into_split()`` peels independent
    /// :class:`DuplexSink` + :class:`DuplexStream` halves. The provider is
    /// pinned at the verb.
    ///
    /// Same deadline / cancel-token / ``org:`` error semantics as
    /// :meth:`call_streaming`.
    #[pyo3(signature = (service, deadline_ms=0, cancel_token=0))]
    fn call_duplex(
        &self,
        py: Python<'_>,
        service: String,
        deadline_ms: u64,
        cancel_token: u64,
    ) -> PyResult<PyDuplexCall> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let runtime = self.runtime.clone();
        let call = py.detach(move || {
            runtime.block_on(async move {
                client
                    .call_duplex_bytes_deadline(&service, deadline_ms, cancel_token)
                    .await
            })
        });
        let call = call.map_err(org_err_to_py)?;
        Ok(PyDuplexCall::from_org_stream(call, self.runtime.clone()))
    }

    /// Reserve a cancel token for a subsequent streaming call (OSDK-L §D6a).
    /// Reserve BEFORE the call — a cancel that races the call's opening is
    /// still delivered — then pass the token as ``cancel_token=...`` and fire
    /// it from any thread with :meth:`cancel`.
    fn reserve_cancel_token(&self) -> PyResult<u64> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        Ok(client.reserve_cancel_token())
    }

    /// Cancel the one in-flight call bound to ``token`` (OSDK-L §D6a).
    /// Idempotent; a no-op for ``0`` or a token no call reserved. It never
    /// launches a second attempt — a signed proof is never resent.
    fn cancel(&self, token: u64) -> PyResult<()> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        client.cancel(token);
        Ok(())
    }

    /// The organization this client acts for, as 32 raw bytes.
    #[getter]
    fn acting_org<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        Ok(PyBytes::new(py, client.acting_org().as_bytes()))
    }

    /// The entity this client calls as, as 32 raw bytes.
    #[getter]
    fn caller<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
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

impl PyOrgClient {
    /// The live client, for installing as another surface's caller
    /// identity (`NetMesh.set_a2a_org_caller`,
    /// `CapabilityGateway.set_a2a_org_caller`). `None` once closed.
    ///
    /// A snapshot, like every other read here: the installed `Arc` keeps
    /// the audience lease and the node reference alive for as long as the
    /// A2A surface holds it, so a `close()` that lands afterwards cannot
    /// tear a call in flight.
    pub(crate) fn shared(&self) -> Option<Arc<net_sdk::org::OrgClient>> {
        self.inner.load_full()
    }
}

/// The async caller half of the facade (OSDK §4.4): the async forms of
/// [`OrgClient`]'s streaming verbs, returning awaitables that resolve to the
/// existing `Async*` handle classes.
///
/// Everything about the sync [`OrgClient`] carries over unchanged — one plan
/// per call with the provider pinned, the no-retry rule, the facade's finite
/// default lifetime when `deadline_ms == 0`, and midstream errors through the
/// `org:` vocabulary. What the async forms add is the asyncio cancellation
/// contract, stated at its exact levels: the construction await AND every
/// later `await` on the returned handle (`__anext__`, `send`, `finish`, …)
/// run through the binding's task-cancel bridge, so `task.cancel()` (or
/// `asyncio.wait_for` expiry) fires the substrate's cancel token — the
/// per-stream cancel watcher then tears the call down LOCALLY at once (the
/// response side EOFs while the handle is still alive), and the WIRE CANCEL
/// that retires the provider-side call rides the handle's `close()`/drop (the
/// substrate's per-shape Drop contract). Cancellation is never a handler-side
/// event — see the `serve_org_*` handler-drop contract (Specification §2.2).
/// There is deliberately no `cancel_token` parameter here: the bridge mints
/// and owns the token so task cancellation is the one cancellation story.
///
/// Lifecycle matches [`OrgClient`]: explicit `close()` plus a context manager;
/// teardown order `client.close()` → `serve_handle.close()` →
/// `mesh.shutdown()`.
#[pyclass(name = "AsyncOrgClient", module = "_net")]
pub struct PyAsyncOrgClient {
    inner: ArcSwapOption<net_sdk::org::OrgClient>,
    /// The mesh's node — the task-cancel bridge reserves and fires cancel
    /// tokens on it (`await_with_cancel`).
    node: Arc<::net::adapter::net::MeshNode>,
}

#[pymethods]
impl PyAsyncOrgClient {
    /// Bind credentials to a mesh. Consumes `credentials` (same
    /// private-discovery identity relation as [`OrgClient.bind`]).
    #[staticmethod]
    fn bind(
        mesh: &crate::mesh_bindings::NetMesh,
        credentials: &PyOrgCredentials,
    ) -> PyResult<Self> {
        let node = mesh.node_arc_clone()?;
        let creds = credentials.take().ok_or_else(|| {
            OrgCredentialsError::new_err(
                "org:credentials:already_consumed: these OrgCredentials were already bound; \
                 construct a new set to bind again",
            )
        })?;
        let client =
            net_sdk::org::OrgClient::bind_node(node.clone(), creds).map_err(org_err_to_py)?;
        Ok(Self {
            inner: ArcSwapOption::from_pointee(client),
            node,
        })
    }

    /// Open a protected streaming-response call (OSDK §4.4). Await it to get
    /// an :class:`AsyncRpcStream`; consume with ``async for chunk in stream:``.
    ///
    /// Cancellation: `task.cancel()` on the awaiting task fires the substrate
    /// cancel token mid-open or mid-stream — the per-stream cancel watcher
    /// tears the call down locally at once (the response side EOFs while the
    /// handle lives) — and the WIRE CANCEL that retires the provider-side
    /// call rides the handle's ``close()``/``aclose()``/drop (the per-shape
    /// Drop contract: dropping emits exactly one CANCEL). Midstream errors
    /// arrive through the ``org:`` vocabulary (:class:`OrgError` family).
    /// ``deadline_ms == 0`` is the facade's 300 s default, never "none".
    #[pyo3(signature = (service, request, deadline_ms=0))]
    fn call_streaming<'py>(
        &self,
        py: Python<'py>,
        service: String,
        request: &Bound<'py, PyBytes>,
        deadline_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let body = bytes::Bytes::copy_from_slice(request.as_bytes());
        let node = self.node.clone();
        crate::async_bridge::await_with_cancel(py, &self.node, move |token| async move {
            let stream = client
                .call_streaming_bytes_deadline(&service, body, deadline_ms, token)
                .await
                .map_err(org_err_to_py)?;
            Ok::<_, PyErr>(PyAsyncRpcStream::from_org_stream(stream, node, token))
        })
    }

    /// Open a protected client-streaming call (OSDK §4.4). Await it to get an
    /// :class:`AsyncClientStreamCall`; ``await call.send(body)`` pushes items
    /// (the signed opening rides the FIRST one), ``await call.finish()``
    /// half-closes and awaits the terminal response. The provider is pinned at
    /// the verb. Same cancellation / deadline / error semantics as
    /// :meth:`call_streaming`.
    #[pyo3(signature = (service, deadline_ms=0))]
    fn call_client_stream<'py>(
        &self,
        py: Python<'py>,
        service: String,
        deadline_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let node = self.node.clone();
        crate::async_bridge::await_with_cancel(py, &self.node, move |token| async move {
            let call = client
                .call_client_stream_bytes_deadline(&service, deadline_ms, token)
                .await
                .map_err(org_err_to_py)?;
            Ok::<_, PyErr>(PyAsyncClientStreamCall::from_org_stream(call, node, token))
        })
    }

    /// Open a protected duplex call (OSDK §4.4). Await it to get an
    /// :class:`AsyncDuplexCall`; ``await call.send(body)`` pushes request
    /// items, ``await call.finish_sending()`` half-closes the upload,
    /// ``async for`` consumes response chunks, and ``into_split()`` peels
    /// independent :class:`AsyncDuplexSink` + :class:`AsyncDuplexStream`
    /// halves. Same cancellation / deadline / error semantics as
    /// :meth:`call_streaming`.
    #[pyo3(signature = (service, deadline_ms=0))]
    fn call_duplex<'py>(
        &self,
        py: Python<'py>,
        service: String,
        deadline_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        let node = self.node.clone();
        crate::async_bridge::await_with_cancel(py, &self.node, move |token| async move {
            let call = client
                .call_duplex_bytes_deadline(&service, deadline_ms, token)
                .await
                .map_err(org_err_to_py)?;
            Ok::<_, PyErr>(PyAsyncDuplexCall::from_org_stream(call, node, token))
        })
    }

    /// The organization this client acts for, as 32 raw bytes.
    #[getter]
    fn acting_org<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
        Ok(PyBytes::new(py, client.acting_org().as_bytes()))
    }

    /// The entity this client calls as, as 32 raw bytes.
    #[getter]
    fn caller<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let client = self.inner.load_full().ok_or_else(|| {
            OrgCredentialsError::new_err("org:credentials:closed: this OrgClient has been closed")
        })?;
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
