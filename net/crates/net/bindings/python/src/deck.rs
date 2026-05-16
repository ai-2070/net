//! PyO3 surface for the Deck SDK — operator-side bindings.
//!
//! Slice 1 of `DECK_SDK_PLAN.md` Phase 4: `DeckClient` +
//! `AdminCommands` (all 9 methods) + snapshot / status streams +
//! `OperatorIdentity`. Audit / log / failure streams + ICE land
//! in slice 2/3.
//!
//! # Phase 1 substrate constraint
//!
//! The substrate's `DeckClient` is **non-signing today** —
//! `AdminCommands` records the operator id on every commit but
//! doesn't yet route through channel-auth. The Python surface
//! exposes the same API so consumers benefit transparently when
//! the substrate cuts over.
//!
//! # Snapshot wire form
//!
//! `MeshOsSnapshot` is large; slice 1 emits it as a JSON string
//! that the Python wrapper at `sdk-py/src/net_sdk/deck.py`
//! auto-parses into a dict. Typed pyclass projections land
//! in slice 2 if a consumer asks. `StatusSummary` is small
//! enough to emit as a typed dict.
//!
//! # Error envelope
//!
//! Errors raise `DeckSdkError` carrying the substrate's
//! `<<deck-sdk-kind:KIND>>MSG` envelope verbatim, with `.kind`
//! and `.message` attached. Cross-binding parity with the MeshOS
//! SDK's error format.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::runtime::Runtime;

use net::adapter::net::behavior::deck::{
    AdminCommands as CoreAdminCommands, ChainCommit as CoreChainCommit, DeckClient as CoreClient,
    DeckClientConfig as CoreConfig, DeckError, OperatorIdentity as CoreIdentity,
    SnapshotStream as CoreSnapshotStream, StatusSummary, StatusSummaryStream as CoreStatusStream,
};
use net::adapter::net::behavior::meshos::{ChainId as CoreChainId, MeshOsSnapshot};

use futures::StreamExt;

use crate::meshos::PyMeshOsDaemonSdk;

// =========================================================================
// Exception class
// =========================================================================

pyo3::create_exception!(
    _net,
    DeckSdkError,
    PyException,
    "Deck SDK error. The message carries the substrate \
     `<<deck-sdk-kind:KIND>>MSG` envelope verbatim; programmatic \
     callers should read the `.kind` attribute rather than parsing \
     the message string."
);

fn deck_err(py: Python<'_>, kind: &str, message: &str) -> PyErr {
    let err = DeckSdkError::new_err(format!("<<deck-sdk-kind:{kind}>>{message}"));
    let _ = err.value(py).setattr("kind", kind);
    let _ = err.value(py).setattr("message", message);
    err
}

fn deck_err_from(py: Python<'_>, e: DeckError) -> PyErr {
    deck_err(py, e.kind, &e.message)
}

// =========================================================================
// PyDeckClientConfig — accept a Python dict
// =========================================================================

fn config_from_dict(py: Python<'_>, d: Option<&Bound<'_, PyDict>>) -> PyResult<CoreConfig> {
    let mut cfg = CoreConfig::default();
    let Some(d) = d else {
        return Ok(cfg);
    };
    if let Some(v) = d.get_item("snapshot_poll_interval_ms")? {
        let ms: u64 = v.extract().map_err(|e| {
            deck_err(
                py,
                "invalid_config",
                &format!("snapshot_poll_interval_ms must be int: {e}"),
            )
        })?;
        cfg.snapshot_poll_interval = Duration::from_millis(ms);
    }
    if let Some(v) = d.get_item("ice_signature_threshold")? {
        cfg.ice_signature_threshold = v.extract().map_err(|e| {
            deck_err(
                py,
                "invalid_config",
                &format!("ice_signature_threshold must be int: {e}"),
            )
        })?;
    }
    Ok(cfg)
}

// =========================================================================
// PyOperatorIdentity
// =========================================================================

/// Operator identity loaded from a maintenance node's identity
/// store. Construct via `generate()` (tests) or
/// `from_seed(bytes)` (production loads).
#[pyclass(name = "OperatorIdentity", module = "net._net", from_py_object)]
#[derive(Clone)]
pub struct PyOperatorIdentity {
    inner: CoreIdentity,
}

impl PyOperatorIdentity {
    pub(crate) fn inner(&self) -> &CoreIdentity {
        &self.inner
    }
}

#[pymethods]
impl PyOperatorIdentity {
    /// Generate a fresh operator identity. Tests + bootstrap.
    #[staticmethod]
    fn generate() -> Self {
        Self {
            inner: CoreIdentity::generate(),
        }
    }

    /// Load from a 32-byte ed25519 seed.
    #[staticmethod]
    fn from_seed(py: Python<'_>, seed: &[u8]) -> PyResult<Self> {
        if seed.len() != 32 {
            return Err(deck_err(
                py,
                "invalid_argument",
                &format!("seed must be 32 bytes, got {}", seed.len()),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(seed);
        let keypair = net::adapter::net::EntityKeypair::from_bytes(arr);
        Ok(Self {
            inner: CoreIdentity::from_keypair(keypair),
        })
    }

    /// Build from an existing `net.Identity`.
    #[staticmethod]
    fn from_identity(identity: &crate::identity::Identity) -> Self {
        Self {
            inner: CoreIdentity::from_keypair((*identity.keypair).clone()),
        }
    }

    /// 64-bit operator identifier (the keypair's origin hash).
    #[getter]
    fn operator_id(&self) -> u64 {
        self.inner.operator_id()
    }

    fn __repr__(&self) -> String {
        format!("OperatorIdentity(operator_id={:#x})", self.inner.operator_id())
    }
}

// =========================================================================
// PyChainCommit
// =========================================================================

fn chain_commit_to_dict<'py>(
    py: Python<'py>,
    commit: &CoreChainCommit,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("commit_id", commit.commit_id())?;
    d.set_item("operator_id", commit.operator_id())?;
    d.set_item("event_kind", commit.event_kind())?;
    let committed_at_ms = commit
        .committed_at()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    d.set_item("committed_at_ms", committed_at_ms)?;
    Ok(d)
}

// =========================================================================
// StatusSummary → dict
// =========================================================================

fn status_summary_to_dict<'py>(
    py: Python<'py>,
    s: &StatusSummary,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    let peers = PyDict::new(py);
    peers.set_item("healthy", s.peers.healthy)?;
    peers.set_item("degraded", s.peers.degraded)?;
    peers.set_item("unreachable", s.peers.unreachable)?;
    peers.set_item("unknown", s.peers.unknown)?;
    d.set_item("peers", peers)?;

    let daemons = PyDict::new(py);
    daemons.set_item("running", s.daemons.running)?;
    daemons.set_item("starting", s.daemons.starting)?;
    daemons.set_item("stopping", s.daemons.stopping)?;
    daemons.set_item("stopped", s.daemons.stopped)?;
    daemons.set_item("backing_off", s.daemons.backing_off)?;
    daemons.set_item("crash_looping", s.daemons.crash_looping)?;
    d.set_item("daemons", daemons)?;

    d.set_item("replica_chains", s.replica_chains)?;
    d.set_item("avoid_list_entries", s.avoid_list_entries)?;
    d.set_item("recently_emitted_count", s.recently_emitted_count)?;
    d.set_item("recent_failure_count", s.recent_failure_count)?;
    d.set_item("admin_audit_ring_depth", s.admin_audit_ring_depth)?;
    d.set_item("freeze_remaining_ms", s.freeze_remaining_ms)?;
    d.set_item("local_maintenance_active", s.local_maintenance_active)?;
    Ok(d)
}

// =========================================================================
// MeshOsSnapshot → JSON string
// =========================================================================

fn snapshot_to_json(py: Python<'_>, snap: &MeshOsSnapshot) -> PyResult<String> {
    serde_json::to_string(snap).map_err(|e| {
        deck_err(
            py,
            "snapshot_serialize_failed",
            &format!("MeshOsSnapshot JSON serialize: {e}"),
        )
    })
}

// =========================================================================
// PySnapshotStream — sync Python iterator
// =========================================================================

/// Live `MeshOsSnapshot` stream as a Python sync iterator. Each
/// `__next__` blocks until the next snapshot publishes (cadence =
/// `DeckClientConfig::snapshot_poll_interval`, default 100 ms).
/// `StopIteration` fires when the underlying stream's substrate
/// runtime shuts down.
///
/// Slice 1 returns JSON strings; the `sdk-py` wrapper parses them
/// into dicts automatically. Typed pyclass projections land in
/// slice 2 if a consumer asks.
#[pyclass(name = "SnapshotStream", module = "net._net")]
pub struct PySnapshotStream {
    inner: Option<CoreSnapshotStream>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PySnapshotStream {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<String> {
        let stream = self
            .inner
            .as_mut()
            .ok_or_else(|| deck_err(py, "stream_closed", "snapshot stream was closed"))?;
        let runtime = self.runtime.clone();
        let snap = py.detach(|| runtime.block_on(stream.next()));
        match snap {
            Some(Ok(snap)) => snapshot_to_json(py, &snap),
            Some(Err(e)) => Err(deck_err_from(py, e)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }

    /// Close the stream explicitly. Subsequent `__next__` calls
    /// raise `StopIteration`. Idempotent.
    fn close(&mut self) {
        self.inner = None;
    }
}

// =========================================================================
// PyStatusSummaryStream — sync Python iterator
// =========================================================================

#[pyclass(name = "StatusSummaryStream", module = "net._net")]
pub struct PyStatusSummaryStream {
    inner: Option<CoreStatusStream>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyStatusSummaryStream {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let stream = self
            .inner
            .as_mut()
            .ok_or_else(|| deck_err(py, "stream_closed", "status summary stream was closed"))?;
        let runtime = self.runtime.clone();
        let item = py.detach(|| runtime.block_on(stream.next()));
        match item {
            Some(Ok(s)) => status_summary_to_dict(py, &s),
            Some(Err(e)) => Err(deck_err_from(py, e)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }

    fn close(&mut self) {
        self.inner = None;
    }
}

// =========================================================================
// PyAdminCommands
// =========================================================================

/// Typed admin-event surface — one method per `AdminEvent`
/// variant. Each returns a `ChainCommit` dict for audit correlation.
/// Phase 1 substrate constraint: non-signing today (the substrate
/// records the operator id but doesn't yet route through
/// channel-auth).
#[pyclass(name = "AdminCommands", module = "net._net")]
pub struct PyAdminCommands {
    /// `Arc<CoreClient>` lets us produce an `AdminCommands<'a>`
    /// borrow on demand without holding it across the FFI.
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
}

impl PyAdminCommands {
    fn admin(&self) -> CoreAdminCommands<'_> {
        self.client.admin()
    }
}

#[pymethods]
impl PyAdminCommands {
    /// Drain a node — start draining workloads. `drain_for_ms` is
    /// the maximum drain duration in milliseconds (substrate
    /// honors this as a deadline).
    fn drain<'py>(
        &self,
        py: Python<'py>,
        node: u64,
        drain_for_ms: u64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| {
            runtime.block_on(self.admin().drain(node, Duration::from_millis(drain_for_ms)))
        });
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    /// Enter maintenance mode on a node. `drain_for_ms = None`
    /// uses substrate-default deadline; pass `int` ms for an
    /// explicit deadline.
    #[pyo3(signature = (node, drain_for_ms=None))]
    fn enter_maintenance<'py>(
        &self,
        py: Python<'py>,
        node: u64,
        drain_for_ms: Option<u64>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let drain_for = drain_for_ms.map(Duration::from_millis);
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().enter_maintenance(node, drain_for)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn exit_maintenance<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().exit_maintenance(node)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn cordon<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().cordon(node)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn uncordon<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().uncordon(node)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn drop_replicas<'py>(
        &self,
        py: Python<'py>,
        node: u64,
        chains: Vec<u64>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let chains: Vec<CoreChainId> = chains.into_iter().collect();
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().drop_replicas(node, chains)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn invalidate_placement<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().invalidate_placement(node)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn restart_all_daemons<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().restart_all_daemons(node)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn clear_avoid_list<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyDict>> {
        let runtime = self.runtime.clone();
        let commit = py.detach(|| runtime.block_on(self.admin().clear_avoid_list(node)));
        match commit {
            Ok(c) => chain_commit_to_dict(py, &c),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }
}

// =========================================================================
// PyDeckClient
// =========================================================================

/// Operator-facing handle to the cluster's admin / snapshot / log /
/// audit surfaces. Construct via `from_meshos(sdk, identity)`
/// against a running `MeshOsDaemonSdk`; the deck client borrows
/// the supervisor runtime.
#[pyclass(name = "DeckClient", module = "net._net")]
pub struct PyDeckClient {
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyDeckClient {
    /// Construct against a running `MeshOsDaemonSdk`. Reuses the
    /// SDK's tokio runtime so streams + admin commits run on the
    /// same supervisor scheduler.
    #[staticmethod]
    #[pyo3(signature = (sdk, identity, config=None))]
    fn from_meshos(
        py: Python<'_>,
        sdk: &PyMeshOsDaemonSdk,
        identity: PyOperatorIdentity,
        config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let cfg = config_from_dict(py, config)?;
        let runtime = sdk.runtime_clone().ok_or_else(|| {
            deck_err(
                py,
                "already_shutdown",
                "MeshOsDaemonSdk was already consumed by shutdown",
            )
        })?;
        let core_client = sdk
            .with_core(|core| {
                CoreClient::new(
                    core.runtime().handle_clone(),
                    core.runtime().snapshot_reader().clone(),
                    identity.inner.clone(),
                    cfg,
                )
            })
            .ok_or_else(|| {
                deck_err(
                    py,
                    "already_shutdown",
                    "MeshOsDaemonSdk was already consumed by shutdown",
                )
            })?;
        Ok(Self {
            client: Arc::new(core_client),
            runtime,
        })
    }

    /// Operator identity bound to this client.
    fn identity(&self) -> PyOperatorIdentity {
        PyOperatorIdentity {
            inner: self.client.identity().clone(),
        }
    }

    /// Typed admin-event surface. Each method commits an
    /// `AdminEvent` variant + returns a `ChainCommit` dict.
    #[getter]
    fn admin(&self) -> PyAdminCommands {
        PyAdminCommands {
            client: self.client.clone(),
            runtime: self.runtime.clone(),
        }
    }

    /// One-shot read of the latest `MeshOsSnapshot`. Returns a JSON
    /// string in slice 1; the `sdk-py` wrapper parses it to a dict.
    fn status(&self, py: Python<'_>) -> PyResult<String> {
        let snap = self.client.status();
        snapshot_to_json(py, &snap)
    }

    /// One-shot read of the rolled-up `StatusSummary`. Returns a
    /// typed dict (peers, daemons, replica_chains, …).
    fn status_summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let s = self.client.status_summary();
        status_summary_to_dict(py, &s)
    }

    /// Live snapshot stream — sync iterator over JSON-encoded
    /// `MeshOsSnapshot` strings. Stream construction creates a
    /// `tokio::time::Interval`; we enter the SDK's runtime context
    /// so the interval reactor is bound to the right scheduler.
    fn snapshots(&self) -> PySnapshotStream {
        let _enter = self.runtime.enter();
        PySnapshotStream {
            inner: Some(self.client.snapshots()),
            runtime: self.runtime.clone(),
        }
    }

    /// Live `StatusSummary` stream — sync iterator over typed dicts.
    /// Same runtime-context requirement as `snapshots()`.
    fn status_summary_stream(&self) -> PyStatusSummaryStream {
        let _enter = self.runtime.enter();
        PyStatusSummaryStream {
            inner: Some(self.client.status_summary_stream()),
            runtime: self.runtime.clone(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "DeckClient(operator_id={:#x})",
            self.client.identity().operator_id()
        )
    }
}
