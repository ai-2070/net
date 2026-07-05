//! Consent + pin-store bindings (`MCP_BRIDGE_SDK_PLAN.md` P1).
//!
//! Thin wrappers over `net_sdk::consent` / `net_sdk::pins` — the surfaces
//! that graduated out of the MCP bridge in P0. Doctrine #1 applies with
//! force here: **no logic in bindings**. Identity canonicalization, the
//! consent decision, and above all the pin store's atomic-save +
//! cross-process lock protocol live in the one Rust implementation; this
//! module marshals arguments and results.
//!
//! Two shapes follow from that:
//!
//! - [`PyPinStore`] is a *path-scoped handle*, not an open snapshot. Reads
//!   load a fresh snapshot; every mutation runs a full locked
//!   `PinStore::mutate` transaction — so Python can never do an unlocked
//!   read-modify-write, hold a stale snapshot across a save, or open the
//!   store file directly. The same file the `net mcp pin` CLI and a running
//!   `net mcp serve` shim use is honored bidirectionally.
//! - Decisions and states cross the boundary as the structured enums'
//!   stable string forms (`"pending"`/`"approved"`,
//!   `"allowed"`/`"requires_approval"`) — computed in Rust, never
//!   re-derived in Python.
//!
//! The approval split is preserved verbatim: `request` only ever writes a
//! *pending* record (the model-callable verb), while `approve`/`reject` are
//! the operator verbs an embedding agent runtime must keep out of its model
//! loop.
//!
//! Errors: store I/O / corruption raises [`PinsError`] with a `pins: `
//! prefix; a malformed capability id raises `ValueError` with a
//! `consent: ` prefix.

use pyo3::exceptions::{PyException, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;

use net_sdk::consent::{CapabilityId, ConsentPolicy as CoreConsentPolicy, CredentialStatus};
use net_sdk::pins::{PinState, PinStore as CorePinStore, PinStoreError};

pyo3::create_exception!(
    _net,
    PinsError,
    PyException,
    "Raised when the pin store fails: an I/O error reading/writing the \
     store file, or a store file that exists but does not parse (corrupt \
     stores error rather than silently dropping consent decisions)."
);

/// Map a core pin-store failure onto the Python exception, with the
/// module's `pins: ` message prefix.
fn pins_err(e: PinStoreError) -> PyErr {
    PyErr::new::<PinsError, _>(format!("pins: {e}"))
}

/// The shared tokio runtime handle (the async bridge boots it in module
/// init, before any of these classes can be constructed).
fn runtime() -> PyResult<tokio::runtime::Handle> {
    crate::async_bridge::runtime()
        .ok_or_else(|| PyRuntimeError::new_err("pins: async bridge runtime not initialized"))
}

/// A capability id argument: a `CapabilityId` instance or its
/// `provider/capability` display string (parsed — and therefore
/// canonicalized — by the core, so `0x2a/echo` and `42/echo` key the same
/// consent records).
fn extract_cap_id(obj: &Bound<'_, PyAny>) -> PyResult<CapabilityId> {
    if let Ok(id) = obj.extract::<PyRef<'_, PyCapabilityId>>() {
        return Ok(id.inner.clone());
    }
    let s: String = obj
        .extract()
        .map_err(|_| PyTypeError::new_err("consent: cap_id must be a str or a CapabilityId"))?;
    CapabilityId::parse(&s).map_err(|e| PyValueError::new_err(format!("consent: {e}")))
}

/// The stable string form of a pin state.
fn pin_state_str(state: PinState) -> &'static str {
    match state {
        PinState::Pending => "pending",
        PinState::Approved => "approved",
    }
}

/// A capability's canonical identity: `provider/capability`. Construction
/// and parsing canonicalize the provider (whitespace, `0x`-hex node ids),
/// so a pin or consent record keyed through this type can never miss its
/// twin spelled differently.
#[pyclass(name = "CapabilityId", frozen, eq, hash, from_py_object)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyCapabilityId {
    pub(crate) inner: CapabilityId,
}

#[pymethods]
impl PyCapabilityId {
    /// Build from parts. The provider is canonicalized.
    #[new]
    fn new(provider: &str, capability: &str) -> Self {
        Self {
            inner: CapabilityId::new(provider, capability),
        }
    }

    /// Parse the `provider/capability` display form (splits on the FIRST
    /// `/`; the capability half may itself contain `/`). Raises
    /// `ValueError` on a missing or empty half.
    #[staticmethod]
    fn parse(s: &str) -> PyResult<Self> {
        CapabilityId::parse(s)
            .map(|inner| Self { inner })
            .map_err(|e| PyValueError::new_err(format!("consent: {e}")))
    }

    /// The provider node qualifier (canonical spelling).
    #[getter]
    fn provider(&self) -> String {
        self.inner.provider.clone()
    }

    /// The capability / tool name.
    #[getter]
    fn capability(&self) -> String {
        self.inner.capability.clone()
    }

    /// The `provider/capability` display / wire form.
    fn display(&self) -> String {
        self.inner.display()
    }

    fn __str__(&self) -> String {
        self.inner.display()
    }

    fn __repr__(&self) -> String {
        format!(
            "CapabilityId(provider='{}', capability='{}')",
            self.inner.provider, self.inner.capability
        )
    }
}

/// Does a wire-declared credential status require local consent before the
/// capability may be invoked? Implements the core trust boundary: a wire
/// `"none"` is NOT trusted (it gates like `"unknown"`), so even `"none"`
/// (and any unrecognised value, or no wire value at all) returns `True` — a
/// discovered capability can only ever over-gate, never bypass consent.
#[pyfunction]
pub fn credential_requires_consent(status: &str) -> bool {
    CredentialStatus::from_wire(status).requires_consent()
}

/// The consumer-side consent gate: a config allowlist plus a set of pinned
/// capabilities, deciding per capability + wire credential status. The
/// decision logic is the SDK's — this class only carries state.
#[pyclass(name = "ConsentPolicy")]
pub struct PyConsentPolicy {
    inner: CoreConsentPolicy,
}

#[pymethods]
impl PyConsentPolicy {
    /// An empty policy: with no entries, EVERY discovered capability
    /// requires approval (a wire credential status — including `"none"` —
    /// is never trusted).
    #[new]
    fn new() -> Self {
        Self {
            inner: CoreConsentPolicy::new(),
        }
    }

    /// Allowlist a capability (operator config) — a standing pre-approval.
    fn allow(&mut self, cap_id: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.allow(extract_cap_id(cap_id)?);
        Ok(())
    }

    /// Record an approved pin.
    fn pin(&mut self, cap_id: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.pin(extract_cap_id(cap_id)?);
        Ok(())
    }

    /// Remove a pin.
    fn unpin(&mut self, cap_id: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.unpin(&extract_cap_id(cap_id)?);
        Ok(())
    }

    /// Is the capability pinned?
    fn is_pinned(&self, cap_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        Ok(self.inner.is_pinned(&extract_cap_id(cap_id)?))
    }

    /// The pinned capabilities' display ids, sorted.
    fn pinned(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.pinned().map(|id| id.display()).collect();
        ids.sort();
        ids
    }

    /// Decide whether the capability, with the given wire credential
    /// status, may be invoked: `"allowed"` or `"requires_approval"`. The
    /// decision is the SDK enum's stable string form — never re-derive it.
    fn decide(&self, cap_id: &Bound<'_, PyAny>, credential_status: &str) -> PyResult<&'static str> {
        let id = extract_cap_id(cap_id)?;
        Ok(
            if self
                .inner
                .decide(&id, credential_status)
                .requires_approval()
            {
                "requires_approval"
            } else {
                "allowed"
            },
        )
    }

    /// Convenience: does invoking the capability require approval the
    /// operator has not granted?
    fn requires_approval(
        &self,
        cap_id: &Bound<'_, PyAny>,
        credential_status: &str,
    ) -> PyResult<bool> {
        let id = extract_cap_id(cap_id)?;
        Ok(self.inner.requires_approval(&id, credential_status))
    }

    fn __repr__(&self) -> String {
        format!("ConsentPolicy(pinned={})", self.inner.pinned().count())
    }
}

/// The persistent, machine-shared pin store — a *path-scoped handle*.
///
/// Reads load a fresh snapshot of the file; every mutation runs a full
/// load→apply→save transaction under the SDK's cross-process advisory
/// lock, so a concurrent `net mcp pin` CLI invocation, a running
/// `net mcp serve` shim, or another Python thread/process can never be
/// clobbered by a stale snapshot. The GIL is released for the duration of
/// every store operation.
///
/// `request` is the model-callable verb (only ever writes a *pending*
/// record); `approve` / `reject` are operator verbs — keep them out of any
/// model loop.
#[pyclass(name = "PinStore")]
pub struct PyPinStore {
    path: std::path::PathBuf,
}

impl PyPinStore {
    /// Run a locked mutate transaction on the blocking side of the shared
    /// runtime, GIL released.
    fn mutate_blocking<R, F>(&self, py: Python<'_>, f: F) -> PyResult<R>
    where
        F: FnOnce(&mut CorePinStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        let path = self.path.clone();
        let handle = runtime()?;
        py.detach(move || handle.block_on(CorePinStore::mutate(path, f)))
            .map_err(pins_err)
    }

    /// Load a read snapshot, GIL released.
    fn load_blocking(&self, py: Python<'_>) -> PyResult<CorePinStore> {
        let path = self.path.clone();
        let handle = runtime()?;
        py.detach(move || handle.block_on(CorePinStore::load(path)))
            .map_err(pins_err)
    }
}

#[pymethods]
impl PyPinStore {
    /// A handle on the pin store file at `path`. The file need not exist
    /// yet — a missing store reads as empty and is created on the first
    /// mutation.
    #[new]
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    /// The store's file path.
    #[getter]
    fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// Record a pin **request** (the model-callable verb). Writes a
    /// `"pending"` record if none exists; an existing record — pending or
    /// approved — is left untouched (a request never upgrades a pin).
    /// Returns the resulting state.
    fn request(&self, py: Python<'_>, cap_id: &Bound<'_, PyAny>) -> PyResult<&'static str> {
        let id = extract_cap_id(cap_id)?;
        self.mutate_blocking(py, move |s| pin_state_str(s.request(&id)))
    }

    /// **Approve** a pin (operator verb; creates the record if absent).
    /// Returns whether this changed the stored state.
    fn approve(&self, py: Python<'_>, cap_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        let id = extract_cap_id(cap_id)?;
        self.mutate_blocking(py, move |s| s.approve(&id))
    }

    /// **Reject / remove** a pin entirely (operator verb). Returns whether
    /// a record was removed.
    fn reject(&self, py: Python<'_>, cap_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        let id = extract_cap_id(cap_id)?;
        self.mutate_blocking(py, move |s| s.remove(&id))
    }

    /// Is the capability approved (fresh snapshot)?
    fn is_approved(&self, py: Python<'_>, cap_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        let id = extract_cap_id(cap_id)?;
        Ok(self.load_blocking(py)?.is_approved(&id))
    }

    /// The capability's state — `"pending"`, `"approved"`, or `None`.
    fn state(&self, py: Python<'_>, cap_id: &Bound<'_, PyAny>) -> PyResult<Option<&'static str>> {
        let id = extract_cap_id(cap_id)?;
        Ok(self.load_blocking(py)?.state(&id).map(pin_state_str))
    }

    /// Every approved capability's display id, sorted.
    fn approved(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let store = self.load_blocking(py)?;
        let mut ids: Vec<String> = store.approved().iter().map(|id| id.display()).collect();
        ids.sort();
        Ok(ids)
    }

    /// Every pending capability's display id, sorted.
    fn pending(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let store = self.load_blocking(py)?;
        let mut ids: Vec<String> = store.pending().iter().map(|id| id.display()).collect();
        ids.sort();
        Ok(ids)
    }

    /// All records as `(cap_id, state)` tuples, sorted by cap_id.
    fn list(&self, py: Python<'_>) -> PyResult<Vec<(String, String)>> {
        let store = self.load_blocking(py)?;
        let mut rows: Vec<(String, String)> = store
            .list()
            .into_iter()
            .map(|(id, state)| (id.display(), pin_state_str(state).to_string()))
            .collect();
        rows.sort();
        Ok(rows)
    }

    fn __repr__(&self) -> String {
        format!("PinStore(path='{}')", self.path.display())
    }
}

/// Async dual of :class:`PinStore` — the same path-scoped, lock-protected
/// operations as awaitables on the shared bridge runtime. Construct with
/// the store path; use from `asyncio` code that must not block the loop on
/// the cross-process lock.
#[pyclass(name = "AsyncPinStore")]
pub struct PyAsyncPinStore {
    path: std::path::PathBuf,
}

impl PyAsyncPinStore {
    fn mutate_future<'py, R, F>(&self, py: Python<'py>, f: F) -> PyResult<Bound<'py, PyAny>>
    where
        F: FnOnce(&mut CorePinStore) -> R + Send + 'static,
        R: for<'p> IntoPyObject<'p> + Send + 'static,
    {
        let path = self.path.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            CorePinStore::mutate(path, f).await.map_err(pins_err)
        })
    }
}

#[pymethods]
impl PyAsyncPinStore {
    /// An async handle on the pin store file at `path`.
    #[new]
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    /// The store's file path.
    #[getter]
    fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// Awaitable :meth:`PinStore.request`.
    fn request<'py>(
        &self,
        py: Python<'py>,
        cap_id: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let id = extract_cap_id(cap_id)?;
        self.mutate_future(py, move |s| pin_state_str(s.request(&id)))
    }

    /// Awaitable :meth:`PinStore.approve`.
    fn approve<'py>(
        &self,
        py: Python<'py>,
        cap_id: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let id = extract_cap_id(cap_id)?;
        self.mutate_future(py, move |s| s.approve(&id))
    }

    /// Awaitable :meth:`PinStore.reject`.
    fn reject<'py>(
        &self,
        py: Python<'py>,
        cap_id: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let id = extract_cap_id(cap_id)?;
        self.mutate_future(py, move |s| s.remove(&id))
    }

    /// Awaitable :meth:`PinStore.is_approved`.
    fn is_approved<'py>(
        &self,
        py: Python<'py>,
        cap_id: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let id = extract_cap_id(cap_id)?;
        let path = self.path.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok::<_, PyErr>(
                CorePinStore::load(path)
                    .await
                    .map_err(pins_err)?
                    .is_approved(&id),
            )
        })
    }

    /// Awaitable :meth:`PinStore.list`.
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let path = self.path.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let store = CorePinStore::load(path).await.map_err(pins_err)?;
            let mut rows: Vec<(String, String)> = store
                .list()
                .into_iter()
                .map(|(id, state)| (id.display(), pin_state_str(state).to_string()))
                .collect();
            rows.sort();
            Ok::<_, PyErr>(rows)
        })
    }

    fn __repr__(&self) -> String {
        format!("AsyncPinStore(path='{}')", self.path.display())
    }
}
