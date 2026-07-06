//! PyO3 surface for device enrollment (`HERMES_INTEGRATION_PLAN_V2.md`
//! Phase 1).
//!
//! Thin wrappers over `net_sdk::enrollment` / `net_sdk::operator` /
//! `net_sdk::devices`: the invite → join → approve handshake and the
//! operator-side device-lifecycle facade, exposed so a Python operator can
//! mint invites, approve requests, and manage devices, and a Python device can
//! build a signed [`PyJoinRequest`] and verify the [`PyJoinOutcome`] it gets
//! back.
//!
//! **H8 (no key material, ever).** [`PyJoinRequest::create`] and
//! [`PyOperatorEnrollment`] take opaque `Identity` handles; the private ed25519
//! seed is read inside Rust (via `to_sdk`) and never surfaces to Python.
//! Everything crossing the boundary is a *public* entity-id, an invite string,
//! or signed chain bytes. The one implementation of the handshake lives in the
//! Rust SDK (bridge doctrine H2) — this file forwards.
//!
//! **Scope.** This is the transport-independent surface. The *live* device
//! `join` and operator `serve_enrollment` (which drive the mesh nRPC wire) are
//! a follow-up on the SDK `Mesh` handle.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tokio::runtime::Runtime;

use net::adapter::net::channel::ChannelConfigRegistry;
use net::adapter::net::identity::TokenCache;
use net::adapter::net::MeshNode;
use net_sdk::delegation::DEFAULT_DELEGATION_DEPTH;
use net_sdk::devices::DeviceRecord;
use net_sdk::enrollment::{
    fingerprint as sdk_fingerprint, DeviceEnrollment, InviteToken, JoinOutcome, JoinRequest,
};
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::operator::OperatorEnrollment;
use net_sdk::Identity as SdkIdentity;

use crate::delegation::{entity_id_from_bytes, to_sdk, PyDelegationChain, PyRevocationRegistry};
use crate::identity::Identity;

/// Rebuild a Python `Identity` handle from an SDK identity — the keypair `Arc`
/// is shared; a fresh token cache is attached. The private seed stays in Rust.
fn to_py_identity(sdk: &SdkIdentity) -> Identity {
    Identity {
        keypair: sdk.keypair().clone(),
        cache: Arc::new(TokenCache::new()),
    }
}

fn enroll_err(msg: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(msg.to_string())
}

/// A short, human-comparable fingerprint of an entity-id (the 32-byte ed25519
/// public key), shown on both sides of a join so a human can confirm the mesh
/// identity matches — `A1B2-C3D4-E5F6-0789`.
#[pyfunction]
pub fn fingerprint(entity: &[u8]) -> PyResult<String> {
    Ok(sdk_fingerprint(&entity_id_from_bytes(entity)?))
}

/// A pre-authorization to *ask* to join a mesh — not a key. Carries the mesh
/// `root`, a `rendezvous` locator, a single-use nonce, and a short TTL.
#[pyclass(name = "InviteToken", skip_from_py_object)]
#[derive(Clone)]
pub struct PyInviteToken {
    pub(crate) inner: InviteToken,
}

#[pymethods]
impl PyInviteToken {
    /// Parse an invite string (`net-invite:<base64url>`). Raises on a missing
    /// prefix, bad base64, or malformed bytes.
    #[staticmethod]
    fn decode(s: &str) -> PyResult<Self> {
        Ok(Self {
            inner: InviteToken::decode(s).map_err(enroll_err)?,
        })
    }

    /// The copy-paste / QR invite string.
    fn encode(&self) -> String {
        self.inner.encode()
    }

    /// The mesh root entity-id this invite admits into (32 bytes).
    #[getter]
    fn root<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.root.as_bytes())
    }

    /// The rendezvous locator the device dials (opaque transport string).
    #[getter]
    fn rendezvous(&self) -> String {
        self.inner.rendezvous.clone()
    }

    /// Unix-seconds expiry.
    #[getter]
    fn expires_at(&self) -> u64 {
        self.inner.expires_at
    }

    /// The displayed fingerprint of the mesh root — show it to the joiner.
    fn root_fingerprint(&self) -> String {
        self.inner.root_fingerprint()
    }

    /// Whether the invite has expired at `now` (unix secs).
    fn is_expired(&self, now: u64) -> bool {
        self.inner.is_expired(now)
    }

    /// Canonical wire bytes.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.to_bytes())
    }

    /// Parse canonical wire bytes.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        Ok(Self {
            inner: InviteToken::from_bytes(data).map_err(enroll_err)?,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "InviteToken(root_fingerprint={}, rendezvous={:?}, expires_at={})",
            self.inner.root_fingerprint(),
            self.inner.rendezvous,
            self.inner.expires_at
        )
    }
}

/// A device's request to join, signed by the device's own key.
#[pyclass(name = "JoinRequest", skip_from_py_object)]
#[derive(Clone)]
pub struct PyJoinRequest {
    pub(crate) inner: JoinRequest,
}

#[pymethods]
impl PyJoinRequest {
    /// Build + sign a request against `invite`. `device` is the opaque
    /// `Identity` handle whose key is being enrolled (H8: seed stays in Rust).
    #[staticmethod]
    fn create(device: &Identity, name: &str, tags: Vec<String>, invite: &PyInviteToken) -> Self {
        Self {
            inner: JoinRequest::create(&to_sdk(device), name, tags, &invite.inner),
        }
    }

    /// `True` if the device's self-signature verifies (it holds its key).
    fn verify_self_signature(&self) -> bool {
        self.inner.verify_self_signature().is_ok()
    }

    /// The device entity-id (32 bytes).
    #[getter]
    fn device<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.device.as_bytes())
    }

    /// The device-chosen name.
    #[getter]
    fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// The device-chosen tags.
    #[getter]
    fn tags(&self) -> Vec<String> {
        self.inner.tags.clone()
    }

    /// Canonical wire bytes.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.to_bytes())
    }

    /// Parse canonical wire bytes (does not verify the signature).
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        Ok(Self {
            inner: JoinRequest::from_bytes(data).map_err(enroll_err)?,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "JoinRequest(device=0x{}, name={:?})",
            hex::encode(self.inner.device.as_bytes()),
            self.inner.name
        )
    }
}

/// The operator's response to a join request — the payload the enrollment RPC
/// returns to the device.
#[pyclass(name = "JoinOutcome", skip_from_py_object)]
#[derive(Clone)]
pub struct PyJoinOutcome {
    inner: JoinOutcome,
}

#[pymethods]
impl PyJoinOutcome {
    /// Parse canonical wire bytes.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        Ok(Self {
            inner: JoinOutcome::from_bytes(data).map_err(enroll_err)?,
        })
    }

    /// Canonical wire bytes.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.to_bytes())
    }

    /// `True` if the device was admitted.
    #[getter]
    fn is_admitted(&self) -> bool {
        matches!(self.inner, JoinOutcome::Admitted { .. })
    }

    /// The stable reject code (`1..=7`) if rejected, else `None`.
    #[getter]
    fn reject_code(&self) -> Option<u16> {
        match &self.inner {
            JoinOutcome::Rejected { code, .. } => Some(*code),
            JoinOutcome::Admitted { .. } => None,
        }
    }

    /// The human reject message if rejected, else `None`.
    #[getter]
    fn reject_message(&self) -> Option<String> {
        match &self.inner {
            JoinOutcome::Rejected { message, .. } => Some(message.clone()),
            JoinOutcome::Admitted { .. } => None,
        }
    }

    /// Device-side: verify the admitted grant anchors at the invited mesh root
    /// (`invite_root`) and binds to this `device`, returning the
    /// `DelegationChain`. Raises if the outcome was a rejection, or the grant is
    /// untrusted (wrong root / wrong device) — defending the joiner against a
    /// rogue operator.
    fn into_chain(&self, device: &[u8], invite_root: &[u8]) -> PyResult<PyDelegationChain> {
        let device_id = entity_id_from_bytes(device)?;
        let root_id = entity_id_from_bytes(invite_root)?;
        let chain = self
            .inner
            .clone()
            .into_chain(&device_id, &root_id)
            .map_err(enroll_err)?;
        Ok(PyDelegationChain::from_inner(chain))
    }

    fn __repr__(&self) -> String {
        match &self.inner {
            JoinOutcome::Admitted { .. } => "JoinOutcome(admitted)".to_string(),
            JoinOutcome::Rejected { code, message } => {
                format!("JoinOutcome(rejected, code={code}, message={message:?})")
            }
        }
    }
}

/// One enrolled device in the operator's inventory.
#[pyclass(name = "DeviceRecord", skip_from_py_object)]
#[derive(Clone)]
pub struct PyDeviceRecord {
    inner: DeviceRecord,
}

#[pymethods]
impl PyDeviceRecord {
    /// The device entity-id (32 bytes).
    #[getter]
    fn device<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.device.as_bytes())
    }

    #[getter]
    fn name(&self) -> String {
        self.inner.name.clone()
    }

    #[getter]
    fn tags(&self) -> Vec<String> {
        self.inner.tags.clone()
    }

    #[getter]
    fn enrolled_at(&self) -> u64 {
        self.inner.enrolled_at
    }

    /// Unix-seconds the device was revoked, or `None` while active.
    #[getter]
    fn revoked_at(&self) -> Option<u64> {
        self.inner.revoked_at
    }

    #[getter]
    fn is_revoked(&self) -> bool {
        self.inner.is_revoked()
    }

    fn __repr__(&self) -> String {
        format!(
            "DeviceRecord(name={:?}, device=0x{}, revoked={})",
            self.inner.name,
            hex::encode(self.inner.device.as_bytes()),
            self.inner.is_revoked()
        )
    }
}

/// The operator side: mint invites, approve join requests into `root → device`
/// delegations, and manage the device inventory — composing the enrollment
/// authority + device registry + revocation store for one mesh root.
#[pyclass(name = "OperatorEnrollment", skip_from_py_object)]
pub struct PyOperatorEnrollment {
    inner: Arc<OperatorEnrollment>,
}

impl PyOperatorEnrollment {
    /// Shared handle to the underlying facade — used by the live
    /// `NetMesh.serve_enrollment_auto` bridge to hand the coordinator to the
    /// nRPC handler.
    pub(crate) fn arc(&self) -> Arc<OperatorEnrollment> {
        self.inner.clone()
    }
}

#[pymethods]
impl PyOperatorEnrollment {
    /// Build a coordinator for the `root` `Identity` handle, with explicit
    /// device-registry and revocation-store paths.
    #[new]
    fn new(root: &Identity, registry_path: &str, revocation_path: &str) -> Self {
        Self {
            inner: Arc::new(OperatorEnrollment::new(
                to_sdk(root),
                PathBuf::from(registry_path),
                PathBuf::from(revocation_path),
            )),
        }
    }

    /// Build using the per-user default store paths (the same machine-shared
    /// files the CLI and a `net wrap` provider converge on). Raises if neither
    /// path resolves.
    #[staticmethod]
    fn with_default_paths(root: &Identity) -> PyResult<Self> {
        OperatorEnrollment::with_default_paths(to_sdk(root))
            .map(|inner| Self {
                inner: Arc::new(inner),
            })
            .ok_or_else(|| enroll_err("no default store paths could be resolved"))
    }

    /// The mesh root entity-id (32 bytes).
    #[getter]
    fn root_id<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.root_id().as_bytes())
    }

    /// The displayed fingerprint of the mesh root.
    fn root_fingerprint(&self) -> String {
        self.inner.root_fingerprint()
    }

    /// Mint an invite for this mesh valid for `ttl_seconds`, tracking it so a
    /// later `approve` can match a request to it. `rendezvous` is the transport
    /// locator devices dial (e.g. `Mesh.rendezvous_string()`).
    fn invite(&self, rendezvous: &str, ttl_seconds: u64) -> PyInviteToken {
        PyInviteToken {
            inner: self
                .inner
                .invite(rendezvous.to_string(), Duration::from_secs(ttl_seconds)),
        }
    }

    /// Approve an arriving request (auto — invite-as-authorization), reading
    /// the system clock: run the fail-closed checks, record the device, retire
    /// the single-use invite, and return the `root → device` `DelegationChain`.
    /// Raises on any rejection (unknown/expired/wrong invite, bad signature).
    #[pyo3(signature = (request, grant_ttl_seconds, max_depth=None))]
    fn approve(
        &self,
        py: Python<'_>,
        request: &PyJoinRequest,
        grant_ttl_seconds: u64,
        max_depth: Option<u8>,
    ) -> PyResult<PyDelegationChain> {
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let ttl = Duration::from_secs(grant_ttl_seconds);
        let enrollment = py
            .detach(|| self.inner.approve(&request.inner, ttl, depth))
            .map_err(enroll_err)?;
        Ok(PyDelegationChain::from_inner(enrollment.chain))
    }

    /// The **server-side** handler: turn serialized `JoinRequest` bytes into
    /// serialized `JoinOutcome` bytes (auto — invite-as-authorization). This is
    /// what the enrollment RPC moves; a Python plugin can serve enrollment by
    /// feeding it received request bytes and returning the outcome bytes. Never
    /// raises — a malformed request or a rejection is a coded `JoinOutcome`.
    #[pyo3(signature = (request_bytes, grant_ttl_seconds, max_depth=None))]
    fn handle_join_request<'py>(
        &self,
        py: Python<'py>,
        request_bytes: &[u8],
        grant_ttl_seconds: u64,
        max_depth: Option<u8>,
    ) -> Bound<'py, PyBytes> {
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let ttl = Duration::from_secs(grant_ttl_seconds);
        let req = request_bytes.to_vec();
        let out = py.detach(|| self.inner.handle_join_request(&req, ttl, depth));
        PyBytes::new(py, &out)
    }

    /// Revoke a device: raise its revocation floor (kills all current
    /// delegations) and stamp the inventory. Reads the system clock.
    fn revoke(&self, py: Python<'_>, device: &[u8]) -> PyResult<()> {
        let id = entity_id_from_bytes(device)?;
        py.detach(|| self.inner.revoke(&id)).map_err(enroll_err)
    }

    /// The enrolled devices in the inventory.
    fn devices(&self, py: Python<'_>) -> PyResult<Vec<PyDeviceRecord>> {
        let records = py.detach(|| self.inner.devices()).map_err(enroll_err)?;
        Ok(records
            .into_iter()
            .map(|inner| PyDeviceRecord { inner })
            .collect())
    }

    /// Prune a device from the inventory entirely (orthogonal to revoking its
    /// floor). Returns whether a record existed.
    fn forget(&self, py: Python<'_>, device: &[u8]) -> PyResult<bool> {
        let id = entity_id_from_bytes(device)?;
        py.detach(|| self.inner.forget(&id)).map_err(enroll_err)
    }

    /// Outstanding (minted, unredeemed, unexpired at `now`) invites.
    fn pending_invites(&self, now: u64) -> Vec<PyInviteToken> {
        self.inner
            .pending_invites(now)
            .into_iter()
            .map(|inner| PyInviteToken { inner })
            .collect()
    }
}

/// A device's **persisted** enrollment — its own key + the `root → device`
/// grant it received — so it survives restarts without re-pairing. The device
/// seed stays in Rust (H8); [`Self::device`] hands back an opaque `Identity`.
#[pyclass(name = "DeviceEnrollment", skip_from_py_object)]
#[derive(Clone)]
pub struct PyDeviceEnrollment {
    inner: DeviceEnrollment,
}

#[pymethods]
impl PyDeviceEnrollment {
    /// Bundle a device `Identity` handle with the `root → device` chain it
    /// received from `join`, plus the unix-seconds it enrolled.
    #[new]
    fn new(device: &Identity, chain: &PyDelegationChain, enrolled_at: u64) -> Self {
        Self {
            inner: DeviceEnrollment::new(to_sdk(device), chain.inner_chain(), enrolled_at),
        }
    }

    /// Load a persisted enrollment from `path`. `None` if none is saved yet;
    /// raises on a corrupt file.
    #[staticmethod]
    fn load(py: Python<'_>, path: &str) -> PyResult<Option<Self>> {
        let owned = path.to_string();
        let loaded = py
            .detach(|| DeviceEnrollment::load(&owned))
            .map_err(enroll_err)?;
        Ok(loaded.map(|inner| Self { inner }))
    }

    /// Persist to `path` (`0600`, atomic). Overwrites — e.g. after a renewal.
    fn save(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let owned = path.to_string();
        py.detach(|| self.inner.save(&owned)).map_err(enroll_err)
    }

    /// The device's opaque `Identity` handle (its private seed stays in Rust) —
    /// use it to extend the grant to a gateway.
    #[getter]
    fn device(&self) -> Identity {
        to_py_identity(self.inner.device())
    }

    /// The `root → device` delegation chain.
    #[getter]
    fn chain(&self) -> PyDelegationChain {
        PyDelegationChain::from_inner(self.inner.chain().clone())
    }

    /// The mesh root the grant anchors at (32 bytes).
    #[getter]
    fn root<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.root().as_bytes())
    }

    #[getter]
    fn enrolled_at(&self) -> u64 {
        self.inner.enrolled_at()
    }

    /// Unix-seconds the grant expires.
    #[getter]
    fn expires_at(&self) -> u64 {
        self.inner.expires_at()
    }

    /// Whether the grant still verifies + is unexpired. Pass a
    /// `RevocationRegistry` (an empty one is fine device-side — the provider
    /// enforces revocation on invoke).
    #[pyo3(signature = (revocation, skew_seconds=0))]
    fn is_valid(&self, revocation: &PyRevocationRegistry, skew_seconds: u64) -> bool {
        self.inner.is_valid(&revocation.inner, skew_seconds)
    }

    /// Whether the grant is within `window_seconds` of expiry at `now` (unix
    /// secs) — the trigger for silent renewal.
    fn needs_renewal(&self, window_seconds: u64, now: u64) -> bool {
        self.inner.needs_renewal(window_seconds, now)
    }

    fn __repr__(&self) -> String {
        format!(
            "DeviceEnrollment(device=0x{}, expires_at={})",
            hex::encode(self.inner.device().entity_id().as_bytes()),
            self.inner.expires_at()
        )
    }
}

// -----------------------------------------------------------------------------
// Live mesh bridge — the pieces the `NetMesh` PyO3 methods call to drive the
// SDK `Mesh` over the raw `MeshNode` the Python binding holds. The wire
// orchestration lives once in the SDK (`net_sdk::mesh_enroll`); this just wraps
// the node in a `Mesh` and forwards (bridge doctrine H2).
// -----------------------------------------------------------------------------

/// Wrap a raw node in an SDK `Mesh` sharing the same live node. A fresh channel
/// registry is fine — nRPC dispatch lives on the node; the registry is
/// auxiliary bookkeeping the served handle keeps alive.
fn mesh_over(node: Arc<MeshNode>, identity: Option<SdkIdentity>) -> Mesh {
    Mesh::from_node_arc(node, Arc::new(ChannelConfigRegistry::new()), identity)
}

/// The invite `rendezvous` locator for `node` (addr + Noise pubkey + node id).
pub(crate) fn mesh_rendezvous_string(node: Arc<MeshNode>) -> String {
    mesh_over(node, None).rendezvous_string()
}

/// Device-side: enroll `device`'s key into the mesh named by `invite` over the
/// live `node`, returning the verified `root -> device` chain. Releases the GIL
/// for the network round-trip.
pub(crate) fn mesh_join(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: &Runtime,
    device: &Identity,
    invite: String,
    name: String,
    tags: Vec<String>,
) -> PyResult<PyDelegationChain> {
    let mesh = mesh_over(node, Some(to_sdk(device)));
    let chain = py
        .detach(move || runtime.block_on(mesh.join(&invite, name, tags)))
        .map_err(enroll_err)?;
    Ok(PyDelegationChain::from_inner(chain))
}

/// Operator-side: serve enrollment on the live `node` (auto — the invite is the
/// authorization). Returns a handle that must be held to keep the service open.
pub(crate) fn mesh_serve_enrollment_auto(
    node: Arc<MeshNode>,
    runtime: &Runtime,
    operator: Arc<OperatorEnrollment>,
    grant_ttl: Duration,
    max_depth: u8,
) -> PyResult<PyEnrollmentServeHandle> {
    let mesh = mesh_over(node, None);
    // `serve_rpc` spawns a bridge task, so it needs a runtime context.
    let _guard = runtime.enter();
    let handle = mesh
        .serve_enrollment_auto(operator, grant_ttl, max_depth)
        .map_err(enroll_err)?;
    Ok(PyEnrollmentServeHandle {
        inner: Some((mesh, handle)),
    })
}

/// Keeps a served enrollment service alive. Dropping it (or calling
/// [`Self::stop`]) unregisters the service.
#[pyclass(name = "EnrollmentServeHandle", skip_from_py_object)]
pub struct PyEnrollmentServeHandle {
    // The `Mesh` holds the channel registry the service registered against and
    // the `ServeHandle` holds the dispatcher registration — both must outlive
    // the service.
    inner: Option<(Mesh, ServeHandle)>,
}

#[pymethods]
impl PyEnrollmentServeHandle {
    /// Stop serving enrollment (unregister the service).
    fn stop(&mut self) {
        self.inner = None;
    }

    /// Whether the service is still registered.
    #[getter]
    fn serving(&self) -> bool {
        self.inner.is_some()
    }

    fn __repr__(&self) -> String {
        format!("EnrollmentServeHandle(serving={})", self.inner.is_some())
    }
}
