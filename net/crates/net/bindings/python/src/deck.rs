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

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use tokio::runtime::Runtime;

use net::adapter::net::behavior::deck::{
    AdminCommands as CoreAdminCommands, AuditQuery as CoreAuditQuery,
    AuditStream as CoreAuditStream, ChainCommit as CoreChainCommit, DeckClient as CoreClient,
    DeckClientConfig as CoreConfig, DeckError, FailureStream as CoreFailureStream,
    IceProposal as CoreIceProposal, LogFilter as CoreLogFilter, LogStream as CoreLogStream,
    OperatorIdentity as CoreIdentity, SnapshotStream as CoreSnapshotStream, StatusSummary,
    StatusSummaryStream as CoreStatusStream,
};
use net::adapter::net::behavior::meshos::logs::LogLevel as CoreLogLevel;
use net::adapter::net::behavior::meshos::{
    blast_radius_hash, ice_proposal_signing_payload, AdminVerifier as CoreAdminVerifier,
    AvoidScope as CoreAvoidScope, ChainId as CoreChainId, DaemonRef as CoreDaemonRef,
    LoggingDispatcher, MeshOsDaemonSdk as CoreSdk, MeshOsSnapshot, MigrationId as CoreMigrationId,
    OperatorRegistry as CoreOperatorRegistry, OperatorSignature as CoreOperatorSignature,
    VerifyError as CoreVerifyError,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::EntityKeypair;

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

/// Map a substrate-side `VerifyError` to the Python `DeckSdkError`
/// envelope. The kind comes straight from the substrate's stable
/// discriminator (`not_authorized`, `signature_invalid`, etc.) so
/// cross-binding consumers branch on the same string.
fn verify_error_to_py(py: Python<'_>, e: CoreVerifyError) -> PyErr {
    deck_err(py, e.kind(), &e.to_string())
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
        // `Zeroizing` wipes the local stack copy on drop.
        let mut arr = zeroize::Zeroizing::new([0u8; 32]);
        arr.copy_from_slice(seed);
        let keypair = net::adapter::net::EntityKeypair::from_bytes(*arr);
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

    /// 32-byte ed25519 public key. Used by an offline tool that
    /// authors the cluster's `OperatorRegistry` from a set of
    /// known identities.
    fn public_key<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let bytes = self.inner.keypair().entity_id().as_bytes();
        PyBytes::new(py, bytes)
    }

    /// Sign a simulated ICE proposal. Returns a signature dict
    /// `{"operator_id": int, "signature": bytes}` directly
    /// consumable by `SimulatedIceProposal.commit([sig, ...])`.
    ///
    /// Wraps the substrate's `OperatorIdentity::sign_proposal` —
    /// covers `(ICE_SIGNING_DOMAIN || issued_at_ms ||
    /// blast_hash || postcard(action))` so the verifier rebuilds
    /// the same bytes locally.
    fn sign_proposal<'py>(
        &self,
        py: Python<'py>,
        simulated: &PySimulatedIceProposal,
    ) -> PyResult<Bound<'py, PyDict>> {
        let action = simulated.action.as_ref().ok_or_else(|| {
            deck_err(
                py,
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        let hash = blast_radius_hash(&simulated.blast);
        let sig = self
            .inner
            .sign_proposal(action, simulated.issued_at_ms, &hash);
        let d = PyDict::new(py);
        d.set_item("operator_id", sig.operator_id)?;
        d.set_item("signature", PyBytes::new(py, &sig.signature))?;
        Ok(d)
    }

    /// Sign raw payload bytes with this operator's ed25519 key.
    /// Returns `{"operator_id": int, "signature": bytes}`.
    ///
    /// Useful for offline / cross-deck signing flows where the
    /// `(action, issued_at_ms, blast_hash)` triple is exchanged
    /// out-of-band and the local deck reproduces
    /// `ice_proposal_signing_payload(...)` independently. Most
    /// consumers want `sign_proposal(simulated)` instead.
    fn sign_payload<'py>(&self, py: Python<'py>, payload: &[u8]) -> PyResult<Bound<'py, PyDict>> {
        let sig = self.inner.keypair().sign(payload);
        let d = PyDict::new(py);
        d.set_item("operator_id", self.inner.operator_id())?;
        d.set_item("signature", PyBytes::new(py, &sig.to_bytes()))?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "OperatorIdentity(operator_id={:#x})",
            self.inner.operator_id()
        )
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

fn status_summary_to_dict<'py>(py: Python<'py>, s: &StatusSummary) -> PyResult<Bound<'py, PyDict>> {
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
            runtime.block_on(
                self.admin()
                    .drain(node, Duration::from_millis(drain_for_ms)),
            )
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
        let commit =
            py.detach(|| runtime.block_on(self.admin().enter_maintenance(node, drain_for)));
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

    fn invalidate_placement<'py>(
        &self,
        py: Python<'py>,
        node: u64,
    ) -> PyResult<Bound<'py, PyDict>> {
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
/// audit surfaces. Construct via `DeckClient(...)` for the standalone
/// "operator-only" mode (the binding owns the supervisor), or via
/// `from_meshos(sdk, identity)` against an externally-managed
/// `MeshOsDaemonSdk`.
#[pyclass(name = "DeckClient", module = "net._net")]
pub struct PyDeckClient {
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
    /// `Some` only when the client owns its private supervisor
    /// (constructed via the `__new__` constructor). The SDK stays
    /// alive for the client's lifetime; the `Drop` impl below
    /// drains it on GC if the caller never invoked `close()`.
    /// `None` when constructed via `from_meshos` against an
    /// external SDK.
    owned_sdk: Option<CoreSdk>,
}

impl Drop for PyDeckClient {
    /// Drain the private supervisor on GC if `close()` wasn't
    /// called. Without this, a `from_seed`-built client that gets
    /// garbage-collected abandons its tokio workers rather than
    /// shutting them down — defeats the point of the `close()`
    /// fix. `owned_sdk` is `None` after a successful `close()` or
    /// for `from_meshos` builds, so the Drop is a no-op in both
    /// cases.
    ///
    /// Errors are silently discarded — Drop must not panic.
    /// `catch_unwind` wraps the `block_on` because a substrate
    /// panic during shutdown would otherwise propagate out of
    /// the Drop and abort the process.
    fn drop(&mut self) {
        let Some(sdk) = self.owned_sdk.take() else {
            return;
        };
        let runtime = self.runtime.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = runtime.block_on(sdk.shutdown());
        }));
    }
}

#[pymethods]
impl PyDeckClient {
    /// Construct a deck client owning a private supervisor runtime.
    /// Mirrors the cdylib's `net_deck_client_new` ("operator-only
    /// mode" per `net_deck.h`) for Python consumers who don't
    /// already have a `MeshOsDaemonSdk` to compose against.
    ///
    /// `operator_seed` must be exactly 32 bytes of ed25519 seed
    /// material — the operator id is derived as the keypair's
    /// origin hash. `meshos_config` / `deck_config` accept the
    /// same dict shapes as the standalone factories; pass `None`
    /// for substrate defaults.
    #[new]
    #[pyo3(signature = (operator_seed, meshos_config=None, deck_config=None))]
    fn new(
        py: Python<'_>,
        operator_seed: &Bound<'_, PyBytes>,
        meshos_config: Option<&Bound<'_, PyDict>>,
        deck_config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let seed_bytes = operator_seed.as_bytes();
        if seed_bytes.len() != 32 {
            return Err(deck_err(
                py,
                "invalid_argument",
                &format!(
                    "operator_seed must be exactly 32 bytes; got {}",
                    seed_bytes.len()
                ),
            ));
        }
        // `Zeroizing` wipes the local stack copy on drop.
        let mut seed = zeroize::Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(seed_bytes);
        let keypair = EntityKeypair::from_bytes(*seed);
        let identity = CoreIdentity::from_keypair(keypair);

        let sdk_cfg = crate::meshos::meshos_config_from_dict(py, meshos_config)?;
        let deck_cfg = config_from_dict(py, deck_config)?;

        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    deck_err(
                        py,
                        "runtime_start_failed",
                        &format!("failed to build tokio runtime: {e}"),
                    )
                })?,
        );
        let dispatcher = Arc::new(LoggingDispatcher::new());
        // Enter the runtime before `CoreSdk::start` so its internal
        // `tokio::spawn` lands on the runtime we own.
        let sdk = {
            let _enter = runtime.enter();
            CoreSdk::start(sdk_cfg, dispatcher)
        };
        let core_client = CoreClient::new(
            sdk.runtime().handle_clone(),
            sdk.runtime().snapshot_reader().clone(),
            identity,
            deck_cfg,
        );

        Ok(Self {
            client: Arc::new(core_client),
            runtime,
            owned_sdk: Some(sdk),
        })
    }

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
            owned_sdk: None,
        })
    }

    /// Operator identity bound to this client.
    fn identity(&self) -> PyOperatorIdentity {
        PyOperatorIdentity {
            inner: self.client.identity().clone(),
        }
    }

    /// Tear down the private supervisor runtime if this client
    /// owns one (constructed via `DeckClient(seed, ...)`). No-op
    /// for clients built via `from_meshos` against an externally-
    /// managed SDK — the caller is responsible for that SDK's
    /// own `shutdown()`. Idempotent: subsequent calls return
    /// without raising.
    ///
    /// Wired through the Python SDK wrapper's `close()` /
    /// `__enter__` / `__exit__` so `with DeckClient(seed) as
    /// deck:` drains the supervisor at scope exit.
    fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        let Some(sdk) = self.owned_sdk.take() else {
            // External SDK or already closed — no-op.
            return Ok(());
        };
        let runtime = self.runtime.clone();
        let result = py.detach(move || runtime.block_on(async { sdk.shutdown().await }));
        match result {
            Ok(_stats) => Ok(()),
            Err(e) => Err(deck_err(
                py,
                "shutdown_failed",
                &format!("runtime shutdown failed: {e:?}"),
            )),
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

    /// Break-glass surface. Returns `IceCommands` whose factories
    /// produce `IceProposal`s — each must be `simulate()`-d
    /// before commit per the typestate contract.
    #[getter]
    fn ice(&self) -> PyIceCommands {
        PyIceCommands {
            client: self.client.clone(),
            runtime: self.runtime.clone(),
        }
    }

    /// Audit query builder over the in-memory admin-audit ring.
    /// Chain `.recent(n)`, `.by_operator(op_id)`,
    /// `.between(start_ms, end_ms)`, `.force_only()`, `.since(seq)`
    /// before calling `.collect()` for a list or `.stream()` for a
    /// sync iterator.
    fn audit(&self) -> PyAuditQuery {
        PyAuditQuery {
            client: self.client.clone(),
            recent_limit: None,
            by_operator: None,
            between: None,
            force_only: false,
            since: None,
            runtime: self.runtime.clone(),
        }
    }

    /// Subscribe to per-daemon / per-node log lines. `filter` is
    /// an optional dict with keys `min_level` (str), `daemon_id`
    /// (int), `node_id` (int), `since_seq` (int). Missing keys
    /// match every record. Returns a sync iterator over
    /// `LogRecord` dicts.
    #[pyo3(signature = (filter=None))]
    fn subscribe_logs(
        &self,
        py: Python<'_>,
        filter: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyLogStream> {
        let core_filter = log_filter_from_dict(py, filter)?;
        let _enter = self.runtime.enter();
        Ok(PyLogStream {
            inner: Some(self.client.subscribe_logs(core_filter)),
            runtime: self.runtime.clone(),
        })
    }

    /// Subscribe to executor failure records starting from
    /// `since_seq + 1`. Returns a sync iterator over `FailureRecord`
    /// dicts.
    #[pyo3(signature = (since_seq=0))]
    fn subscribe_failures(&self, since_seq: u64) -> PyFailureStream {
        let _enter = self.runtime.enter();
        PyFailureStream {
            inner: Some(self.client.subscribe_failures(since_seq)),
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

// =========================================================================
// Slice 2 — LogFilter dict parsing
// =========================================================================

fn parse_log_level_str(py: Python<'_>, s: &str) -> PyResult<CoreLogLevel> {
    Ok(match s {
        "trace" | "TRACE" | "Trace" => CoreLogLevel::Trace,
        "debug" | "DEBUG" | "Debug" => CoreLogLevel::Debug,
        "info" | "INFO" | "Info" => CoreLogLevel::Info,
        "warn" | "WARN" | "Warn" | "warning" | "WARNING" => CoreLogLevel::Warn,
        "error" | "ERROR" | "Error" => CoreLogLevel::Error,
        other => {
            return Err(deck_err(
                py,
                "invalid_log_level",
                &format!("log level must be one of trace|debug|info|warn|error; got {other:?}"),
            ));
        }
    })
}

fn log_filter_from_dict(
    py: Python<'_>,
    filter: Option<&Bound<'_, PyDict>>,
) -> PyResult<CoreLogFilter> {
    let mut f = CoreLogFilter::default();
    let Some(d) = filter else {
        return Ok(f);
    };
    if let Some(v) = d.get_item("min_level")? {
        let s: String = v.extract().map_err(|e| {
            deck_err(
                py,
                "invalid_filter",
                &format!("min_level must be a string: {e}"),
            )
        })?;
        f.min_level = Some(parse_log_level_str(py, &s)?);
    }
    if let Some(v) = d.get_item("daemon_id")? {
        f.daemon_id =
            Some(v.extract().map_err(|e| {
                deck_err(py, "invalid_filter", &format!("daemon_id must be int: {e}"))
            })?);
    }
    if let Some(v) = d.get_item("node_id")? {
        f.node_id =
            Some(v.extract().map_err(|e| {
                deck_err(py, "invalid_filter", &format!("node_id must be int: {e}"))
            })?);
    }
    if let Some(v) = d.get_item("since_seq")? {
        f.since_seq =
            Some(v.extract().map_err(|e| {
                deck_err(py, "invalid_filter", &format!("since_seq must be int: {e}"))
            })?);
    }
    Ok(f)
}

// =========================================================================
// Slice 2 — Log + Failure records → typed dicts
// =========================================================================

fn log_level_to_str(lvl: CoreLogLevel) -> &'static str {
    match lvl {
        CoreLogLevel::Trace => "trace",
        CoreLogLevel::Debug => "debug",
        CoreLogLevel::Info => "info",
        CoreLogLevel::Warn => "warn",
        CoreLogLevel::Error => "error",
        _ => "unknown",
    }
}

fn log_record_to_dict<'py>(
    py: Python<'py>,
    record: &net::adapter::net::behavior::meshos::LogRecord,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("seq", record.seq)?;
    d.set_item("ts_ms", record.ts_ms)?;
    d.set_item("level", log_level_to_str(record.level))?;
    d.set_item("daemon_id", record.daemon_id)?;
    d.set_item("node_id", record.node_id)?;
    d.set_item("message", record.message.clone())?;
    Ok(d)
}

fn failure_record_to_dict<'py>(
    py: Python<'py>,
    record: &net::adapter::net::behavior::meshos::FailureRecord,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("seq", record.seq)?;
    d.set_item("source", record.source.clone())?;
    d.set_item("reason", record.reason.clone())?;
    d.set_item("recorded_at_ms", record.recorded_at_ms)?;
    Ok(d)
}

fn admin_audit_record_to_json(
    py: Python<'_>,
    record: &net::adapter::net::behavior::meshos::AdminAuditRecord,
) -> PyResult<String> {
    serde_json::to_string(record).map_err(|e| {
        deck_err(
            py,
            "audit_serialize_failed",
            &format!("AdminAuditRecord JSON serialize: {e}"),
        )
    })
}

// =========================================================================
// Slice 2 — PyLogStream
// =========================================================================

/// Live log stream as a sync Python iterator. Each `__next__`
/// blocks until the next record matching the filter publishes,
/// or raises `StopIteration` when the underlying stream's
/// substrate runtime shuts down.
#[pyclass(name = "LogStream", module = "net._net")]
pub struct PyLogStream {
    inner: Option<CoreLogStream>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyLogStream {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let stream = self
            .inner
            .as_mut()
            .ok_or_else(|| deck_err(py, "stream_closed", "log stream was closed"))?;
        let runtime = self.runtime.clone();
        let item = py.detach(|| runtime.block_on(stream.next()));
        match item {
            Some(Ok(record)) => log_record_to_dict(py, &record),
            Some(Err(e)) => Err(deck_err_from(py, e)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }

    fn close(&mut self) {
        self.inner = None;
    }
}

// =========================================================================
// Slice 2 — PyFailureStream
// =========================================================================

#[pyclass(name = "FailureStream", module = "net._net")]
pub struct PyFailureStream {
    inner: Option<CoreFailureStream>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyFailureStream {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let stream = self
            .inner
            .as_mut()
            .ok_or_else(|| deck_err(py, "stream_closed", "failure stream was closed"))?;
        let runtime = self.runtime.clone();
        let item = py.detach(|| runtime.block_on(stream.next()));
        match item {
            Some(Ok(record)) => failure_record_to_dict(py, &record),
            Some(Err(e)) => Err(deck_err_from(py, e)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }

    fn close(&mut self) {
        self.inner = None;
    }
}

// =========================================================================
// Slice 2 — PyAuditQuery (fluent builder) + PyAuditStream
// =========================================================================

/// Fluent admin-audit query builder. Chain the filter methods
/// before calling `.collect()` (eager list) or `.stream()` (sync
/// iterator). Mirrors the substrate's `AuditQuery` API.
///
/// Each filter method returns the (mutated) builder so consumers
/// can chain idiomatically in Python:
///
/// ```python
/// records = (client.audit()
///     .recent(100)
///     .by_operator(op_id)
///     .force_only()
///     .collect())
/// ```
#[pyclass(name = "AuditQuery", module = "net._net")]
pub struct PyAuditQuery {
    client: Arc<CoreClient>,
    recent_limit: Option<usize>,
    by_operator: Option<u64>,
    between: Option<(u64, u64)>,
    force_only: bool,
    since: Option<u64>,
    runtime: Arc<Runtime>,
}

impl PyAuditQuery {
    fn build<'a>(&self, client: &'a CoreClient) -> CoreAuditQuery<'a> {
        let mut q = client.audit();
        if let Some(n) = self.recent_limit {
            q = q.recent(n);
        }
        if let Some(op) = self.by_operator {
            q = q.by_operator(op);
        }
        if let Some((start, end)) = self.between {
            q = q.between(start, end);
        }
        if self.force_only {
            q = q.force_only();
        }
        if let Some(s) = self.since {
            q = q.since(s);
        }
        q
    }
}

#[pymethods]
impl PyAuditQuery {
    fn recent(mut slf: PyRefMut<Self>, limit: usize) -> PyRefMut<Self> {
        slf.recent_limit = Some(limit);
        slf
    }

    fn by_operator(mut slf: PyRefMut<Self>, operator_id: u64) -> PyRefMut<Self> {
        slf.by_operator = Some(operator_id);
        slf
    }

    fn between(mut slf: PyRefMut<Self>, start_ms: u64, end_ms: u64) -> PyRefMut<Self> {
        slf.between = Some((start_ms, end_ms));
        slf
    }

    fn force_only(mut slf: PyRefMut<Self>) -> PyRefMut<Self> {
        slf.force_only = true;
        slf
    }

    fn since(mut slf: PyRefMut<Self>, seq: u64) -> PyRefMut<Self> {
        slf.since = Some(seq);
        slf
    }

    /// Collect the audit records into a list of JSON strings. The
    /// `sdk-py` wrapper parses each entry into a native dict.
    fn collect(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let client = self.client.clone();
        let q = self.build(&client);
        // `collect` is sync on the substrate — reads off the
        // in-memory admin-audit ring synchronously.
        let records = q.collect();
        let mut out = Vec::with_capacity(records.len());
        for r in records {
            out.push(admin_audit_record_to_json(py, &r)?);
        }
        Ok(out)
    }

    /// Return a sync iterator over JSON-encoded audit records.
    fn stream(&self) -> PyAuditStream {
        let _enter = self.runtime.enter();
        PyAuditStream {
            inner: Some(self.build(&self.client).stream()),
            runtime: self.runtime.clone(),
        }
    }
}

#[pyclass(name = "AuditStream", module = "net._net")]
pub struct PyAuditStream {
    inner: Option<CoreAuditStream>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyAuditStream {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<String> {
        let stream = self
            .inner
            .as_mut()
            .ok_or_else(|| deck_err(py, "stream_closed", "audit stream was closed"))?;
        let runtime = self.runtime.clone();
        let item = py.detach(|| runtime.block_on(stream.next()));
        match item {
            Some(Ok(record)) => admin_audit_record_to_json(py, &record),
            Some(Err(e)) => Err(deck_err_from(py, e)),
            None => Err(pyo3::exceptions::PyStopIteration::new_err(())),
        }
    }

    fn close(&mut self) {
        self.inner = None;
    }
}

// =========================================================================
// Slice 3 — ICE break-glass surface
//
// Typestate: PyIceProposal has no `commit` — only PySimulatedIceProposal
// does. The compile-time check we get in Rust translates into the
// pyclass-level surface: `.simulate()` is the only path that produces
// a SimulatedIceProposal, and only that class exposes `.commit`.
// =========================================================================

/// Parse an `AvoidScope` from a Python dict. Three shapes:
///
/// - `{"kind": "global"}` — `AvoidScope::Global`
/// - `{"kind": "local", "node": int}` — `AvoidScope::Local { node }`
/// - `{"kind": "on_peer", "peer": int}` — `AvoidScope::OnPeer { peer }`
fn parse_avoid_scope(py: Python<'_>, d: &Bound<'_, PyDict>) -> PyResult<CoreAvoidScope> {
    let kind: String = match d.get_item("kind")? {
        Some(v) => v.extract().map_err(|e| {
            deck_err(
                py,
                "invalid_avoid_scope",
                &format!("scope.kind must be string: {e}"),
            )
        })?,
        None => {
            return Err(deck_err(
                py,
                "invalid_avoid_scope",
                "scope dict missing required key 'kind'",
            ));
        }
    };
    Ok(match kind.as_str() {
        "global" | "Global" => CoreAvoidScope::Global,
        "local" | "Local" => {
            let node: u64 = d
                .get_item("node")?
                .ok_or_else(|| {
                    deck_err(
                        py,
                        "invalid_avoid_scope",
                        "scope 'local' requires 'node' int",
                    )
                })?
                .extract()
                .map_err(|e| {
                    deck_err(py, "invalid_avoid_scope", &format!("node must be int: {e}"))
                })?;
            CoreAvoidScope::Local { node }
        }
        "on_peer" | "OnPeer" => {
            let peer: u64 = d
                .get_item("peer")?
                .ok_or_else(|| {
                    deck_err(
                        py,
                        "invalid_avoid_scope",
                        "scope 'on_peer' requires 'peer' int",
                    )
                })?
                .extract()
                .map_err(|e| {
                    deck_err(py, "invalid_avoid_scope", &format!("peer must be int: {e}"))
                })?;
            CoreAvoidScope::OnPeer { peer }
        }
        other => {
            return Err(deck_err(
                py,
                "invalid_avoid_scope",
                &format!("scope.kind must be 'global' | 'local' | 'on_peer'; got {other:?}"),
            ));
        }
    })
}

fn operator_signature_from_dict(
    py: Python<'_>,
    d: &Bound<'_, PyDict>,
) -> PyResult<CoreOperatorSignature> {
    let op_id: u64 = match d.get_item("operator_id")? {
        Some(v) => v.extract().map_err(|e| {
            deck_err(
                py,
                "invalid_signature",
                &format!("operator_id must be int: {e}"),
            )
        })?,
        None => {
            return Err(deck_err(
                py,
                "invalid_signature",
                "signature dict missing required key 'operator_id'",
            ));
        }
    };
    let sig_bytes: Vec<u8> = match d.get_item("signature")? {
        Some(v) => v.extract().map_err(|e| {
            deck_err(
                py,
                "invalid_signature",
                &format!("signature must be bytes: {e}"),
            )
        })?,
        None => {
            return Err(deck_err(
                py,
                "invalid_signature",
                "signature dict missing required key 'signature'",
            ));
        }
    };
    Ok(CoreOperatorSignature {
        operator_id: op_id,
        signature: sig_bytes,
    })
}

fn blast_radius_to_json(
    py: Python<'_>,
    blast: &net::adapter::net::behavior::meshos::BlastRadius,
) -> PyResult<String> {
    serde_json::to_string(blast).map_err(|e| {
        deck_err(
            py,
            "blast_serialize_failed",
            &format!("BlastRadius JSON serialize: {e}"),
        )
    })
}

/// Build a substrate-side `IceProposal` from a saved action.
/// The substrate's `IceProposal::new` pins a fresh
/// `issued_at_ms` per call; for the typestate's `simulate()`
/// path this is fine because the simulator is pure over the
/// current snapshot. The committed envelope re-binds
/// `issued_at_ms` to the value we held — same `(action,
/// issued_at_ms, blast_hash)` triple substrate-side verifier
/// expects.
///
/// `IceActionProposal` is `#[non_exhaustive]` — an unknown
/// variant returns a `DeckError { kind: "unknown_action" }`
/// rather than silently mapping to `ThawCluster` (the most
/// destructive action). Callers `?`-bubble through the
/// existing `DeckError → PyErr` adapter.
fn build_core_proposal<'a>(
    client: &'a CoreClient,
    action: net::adapter::net::behavior::meshos::IceActionProposal,
) -> Result<CoreIceProposal<'a>, DeckError> {
    use net::adapter::net::behavior::meshos::IceActionProposal as A;
    match action {
        A::FreezeCluster { ttl } => Ok(client.ice().freeze_cluster(ttl)),
        A::FlushAvoidLists { scope } => Ok(client.ice().flush_avoid_lists(scope)),
        A::ForceEvictReplica { chain, victim } => {
            Ok(client.ice().force_evict_replica(chain, victim))
        }
        A::ForceRestartDaemon { daemon } => Ok(client.ice().force_restart_daemon(daemon)),
        A::ForceCutover { chain, target } => Ok(client.ice().force_cutover(chain, target)),
        A::KillMigration { migration } => Ok(client.ice().kill_migration(migration)),
        A::ThawCluster => Ok(client.ice().thaw_cluster()),
        other => Err(DeckError {
            kind: "unknown_action",
            message: format!(
                "IceActionProposal carries an unknown variant ({other:?}); \
                 rebuild the SDK binding against the current substrate"
            ),
        }),
    }
}

// =========================================================================
// PyIceCommands — 7 factory methods
// =========================================================================

#[pyclass(name = "IceCommands", module = "net._net")]
pub struct PyIceCommands {
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl PyIceCommands {
    fn freeze_cluster(&self, ttl_ms: u64) -> PyIceProposal {
        let proposal = self
            .client
            .ice()
            .freeze_cluster(Duration::from_millis(ttl_ms));
        PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        )
    }

    fn flush_avoid_lists(
        &self,
        py: Python<'_>,
        scope: &Bound<'_, PyDict>,
    ) -> PyResult<PyIceProposal> {
        let scope = parse_avoid_scope(py, scope)?;
        let proposal = self.client.ice().flush_avoid_lists(scope);
        Ok(PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        ))
    }

    fn force_evict_replica(&self, chain: u64, victim: u64) -> PyIceProposal {
        let proposal = self
            .client
            .ice()
            .force_evict_replica(chain as CoreChainId, victim);
        PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        )
    }

    /// Propose force-restarting a daemon. `id` is the registry-
    /// local daemon id; `name` is `MeshDaemon::name()`.
    fn force_restart_daemon(&self, id: u64, name: String) -> PyIceProposal {
        let daemon = CoreDaemonRef { id, name };
        let proposal = self.client.ice().force_restart_daemon(daemon);
        PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        )
    }

    fn force_cutover(&self, chain: u64, target: u64) -> PyIceProposal {
        let proposal = self
            .client
            .ice()
            .force_cutover(chain as CoreChainId, target);
        PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        )
    }

    fn kill_migration(&self, migration: u64) -> PyIceProposal {
        let proposal = self
            .client
            .ice()
            .kill_migration(migration as CoreMigrationId);
        PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        )
    }

    fn thaw_cluster(&self) -> PyIceProposal {
        let proposal = self.client.ice().thaw_cluster();
        PyIceProposal::from_action(
            self.client.clone(),
            self.runtime.clone(),
            proposal.action().clone(),
            proposal.issued_at_ms(),
        )
    }
}

// =========================================================================
// PyIceProposal — pre-simulation typestate. No `commit` method.
// =========================================================================

#[pyclass(name = "IceProposal", module = "net._net")]
pub struct PyIceProposal {
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
    action: Option<net::adapter::net::behavior::meshos::IceActionProposal>,
    issued_at_ms: u64,
}

impl PyIceProposal {
    fn from_action(
        client: Arc<CoreClient>,
        runtime: Arc<Runtime>,
        action: net::adapter::net::behavior::meshos::IceActionProposal,
        issued_at_ms: u64,
    ) -> Self {
        Self {
            client,
            runtime,
            action: Some(action),
            issued_at_ms,
        }
    }
}

#[pymethods]
impl PyIceProposal {
    #[getter]
    fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Pre-execution preview. Consumes the proposal — subsequent
    /// calls raise `DeckSdkError(kind="already_simulated")`.
    fn simulate(&mut self, py: Python<'_>) -> PyResult<PySimulatedIceProposal> {
        let action = self
            .action
            .as_ref()
            .ok_or_else(|| {
                deck_err(
                    py,
                    "already_simulated",
                    "IceProposal was already consumed by simulate()",
                )
            })?
            .clone();
        let issued_at_ms = self.issued_at_ms;
        let runtime = self.runtime.clone();
        let client = self.client.clone();
        // Validate the variant up-front. The husk only flips to
        // consumed once we know the action is known to this
        // binding, so an unknown-variant rejection leaves the
        // proposal retry-able. Substrate-side simulate errors
        // still consume the husk (matching Go + Node).
        build_core_proposal(&client, action.clone())
            .map_err(|e| deck_err(py, e.kind, &e.message))?;
        self.action = None;
        let action_for_build = action.clone();
        let blast = py.detach(move || {
            runtime.block_on(async move {
                let proposal = build_core_proposal(&client, action_for_build)?;
                proposal.simulate().await.map(|s| s.blast_radius().clone())
            })
        });
        match blast {
            Ok(b) => Ok(PySimulatedIceProposal {
                client: self.client.clone(),
                runtime: self.runtime.clone(),
                action: Some(action),
                issued_at_ms,
                blast: b,
                committed: false,
            }),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "IceProposal(consumed={}, issued_at_ms={})",
            self.action.is_none(),
            self.issued_at_ms,
        )
    }
}

// =========================================================================
// PySimulatedIceProposal — only class exposing commit
// =========================================================================

#[pyclass(name = "SimulatedIceProposal", module = "net._net")]
pub struct PySimulatedIceProposal {
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
    action: Option<net::adapter::net::behavior::meshos::IceActionProposal>,
    issued_at_ms: u64,
    blast: net::adapter::net::behavior::meshos::BlastRadius,
    committed: bool,
}

#[pymethods]
impl PySimulatedIceProposal {
    /// Pre-execution blast-radius preview as a JSON string. The
    /// `sdk-py` wrapper parses to a native dict.
    fn blast_radius(&self, py: Python<'_>) -> PyResult<String> {
        blast_radius_to_json(py, &self.blast)
    }

    #[getter]
    fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Blake3 digest of the blast radius. Signers must cover
    /// this exact hash; substrate verifier rebuilds + compares.
    fn blast_hash<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let hash = blast_radius_hash(&self.blast);
        Ok(PyBytes::new(py, hash.as_ref()))
    }

    /// Deterministic signing payload bytes the verifier will
    /// reconstruct: `ICE_SIGNING_DOMAIN || issued_at_ms (le u64)
    /// || blast_hash (32) || postcard(action)`. Returned for the
    /// offline / cross-deck signing flow — pair with
    /// `OperatorIdentity.sign_payload(payload)` on a remote
    /// deck to produce a signature the local deck can pass into
    /// `commit([sig, ...])`. Raises `already_committed` once the
    /// proposal has been consumed by `commit()`.
    fn signing_payload<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let action = self.action.as_ref().ok_or_else(|| {
            deck_err(
                py,
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        let hash = blast_radius_hash(&self.blast);
        let payload = ice_proposal_signing_payload(action, self.issued_at_ms, &hash);
        Ok(PyBytes::new(py, &payload))
    }

    /// Commit the proposal with the supplied operator signatures.
    /// `signatures` is a list of dicts:
    /// `{"operator_id": int, "signature": bytes}`.
    fn commit<'py>(
        &mut self,
        py: Python<'py>,
        signatures: Vec<Bound<'_, PyDict>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        if self.committed {
            return Err(deck_err(
                py,
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            ));
        }
        let action = self
            .action
            .as_ref()
            .ok_or_else(|| {
                deck_err(
                    py,
                    "already_committed",
                    "SimulatedIceProposal was already consumed by commit()",
                )
            })?
            .clone();
        let mut sigs = Vec::with_capacity(signatures.len());
        for d in signatures {
            sigs.push(operator_signature_from_dict(py, &d)?);
        }
        let runtime = self.runtime.clone();
        let client = self.client.clone();
        // Validate the variant up-front so an unknown-variant
        // rejection leaves the husk retry-able. Substrate-side
        // simulate/commit errors still consume the husk (matching
        // Go + Node).
        build_core_proposal(&client, action.clone())
            .map_err(|e| deck_err(py, e.kind, &e.message))?;
        self.action = None;
        self.committed = true;
        let commit_result = py.detach(move || {
            runtime.block_on(async move {
                let proposal = build_core_proposal(&client, action)?;
                let simulated = proposal.simulate().await?;
                simulated.commit(&sigs).await
            })
        });
        match commit_result {
            Ok(commit) => chain_commit_to_dict(py, &commit),
            Err(e) => Err(deck_err_from(py, e)),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "SimulatedIceProposal(committed={}, issued_at_ms={}, affected_nodes={})",
            self.committed,
            self.issued_at_ms,
            self.blast.affected_nodes.len(),
        )
    }
}

// =========================================================================
// PyOperatorRegistry — operator-policy authoring + offline verify
// =========================================================================

/// Cluster operator-policy registry. Holds known operator public
/// keys keyed by 64-bit operator id; `verify` / `verify_bundle`
/// authenticate `OperatorSignature` dicts against the policy.
///
/// The substrate's loop has its own copy installed via
/// `AdminVerifier`; this Python class is the offline tool for
/// authoring the policy, pre-verifying bundles before commit,
/// and unit-testing operator workflows. Mutations are
/// thread-safe via an internal mutex (the cdylib runs callbacks
/// on tokio workers).
#[pyclass(name = "OperatorRegistry", module = "net._net", from_py_object)]
#[derive(Clone)]
pub struct PyOperatorRegistry {
    inner: Arc<Mutex<CoreOperatorRegistry>>,
}

impl PyOperatorRegistry {
    /// Snapshot the registry into an `Arc<CoreOperatorRegistry>`
    /// suitable for handing to `AdminVerifier::new`. The
    /// snapshot is detached — later mutations on the Python
    /// registry don't propagate.
    pub(crate) fn snapshot(&self) -> Arc<CoreOperatorRegistry> {
        Arc::new(self.inner.lock().clone())
    }
}

#[pymethods]
impl PyOperatorRegistry {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CoreOperatorRegistry::new())),
        }
    }

    /// Insert an operator's 32-byte ed25519 public key under
    /// `operator_id`. Subsequent `verify` calls for that
    /// operator id resolve against this entry.
    fn insert(&self, py: Python<'_>, operator_id: u64, public_key: &[u8]) -> PyResult<()> {
        if public_key.len() != 32 {
            return Err(deck_err(
                py,
                "invalid_public_key",
                &format!("public_key must be 32 bytes, got {}", public_key.len()),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(public_key);
        let entity_id = EntityId::from_bytes(arr);
        let _ = py;
        self.inner.lock().insert(operator_id, entity_id);
        Ok(())
    }

    /// Convenience — register `identity`'s public key under its
    /// derived operator id (the keypair's origin hash).
    fn register(&self, py: Python<'_>, identity: &PyOperatorIdentity) -> PyResult<()> {
        let _ = py;
        self.inner.lock().register(identity.inner.keypair());
        Ok(())
    }

    /// `True` iff `operator_id` is registered.
    fn contains(&self, py: Python<'_>, operator_id: u64) -> PyResult<bool> {
        let _ = py;
        Ok(self.inner.lock().contains(operator_id))
    }

    fn __contains__(&self, py: Python<'_>, operator_id: u64) -> PyResult<bool> {
        self.contains(py, operator_id)
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let _ = py;
        Ok(self.inner.lock().len())
    }

    fn is_empty(&self, py: Python<'_>) -> PyResult<bool> {
        let _ = py;
        Ok(self.inner.lock().is_empty())
    }

    /// Verify a single `OperatorSignature` dict over `payload`.
    /// Raises `DeckSdkError` with `kind = "not_authorized"` for
    /// an unknown operator id and `"signature_invalid"` for a
    /// malformed / tampered signature.
    fn verify(
        &self,
        py: Python<'_>,
        signature: &Bound<'_, PyDict>,
        payload: &[u8],
    ) -> PyResult<()> {
        let sig = operator_signature_from_dict(py, signature)?;
        self.inner
            .lock()
            .verify(&sig, payload)
            .map_err(|e| verify_error_to_py(py, e))
    }

    /// Verify every signature in the bundle over `payload` and
    /// confirm at least `threshold` *distinct* operator ids
    /// signed it. Distinct-operator dedup is the load-bearing
    /// M-of-N gate — a bundle of `[sig_A, sig_A]` against a
    /// `threshold = 2` raises `insufficient_signatures`.
    fn verify_bundle(
        &self,
        py: Python<'_>,
        signatures: Vec<Bound<'_, PyDict>>,
        payload: &[u8],
        threshold: usize,
    ) -> PyResult<()> {
        let mut sigs = Vec::with_capacity(signatures.len());
        for d in signatures {
            sigs.push(operator_signature_from_dict(py, &d)?);
        }
        self.inner
            .lock()
            .verify_bundle(&sigs, payload, threshold)
            .map_err(|e| verify_error_to_py(py, e))
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "OperatorRegistry(operators={})",
            self.inner.lock().len()
        ))
    }
}

// =========================================================================
// PyAdminVerifier — substrate verifier wrapper
// =========================================================================

/// Substrate-side admin commit verifier. Bundles an
/// `OperatorRegistry` snapshot with the cluster's signature
/// threshold + freshness/skew/ICE-cooldown windows. Useful for
/// offline unit testing of operator-policy decisions.
///
/// Constructors snapshot the registry at build time — later
/// mutations on the source `OperatorRegistry` are not reflected.
/// Rebuild the verifier after every policy change.
#[pyclass(name = "AdminVerifier", module = "net._net")]
pub struct PyAdminVerifier {
    inner: CoreAdminVerifier,
}

#[pymethods]
impl PyAdminVerifier {
    /// Build a verifier with `threshold` minimum signatures and
    /// the substrate's default freshness window (300s),
    /// future-skew tolerance (30s), and ICE cooldown (300s).
    /// `threshold = 0` is clamped to `1`.
    #[new]
    fn new(registry: &PyOperatorRegistry, threshold: usize) -> Self {
        Self {
            inner: CoreAdminVerifier::new(registry.snapshot(), threshold),
        }
    }

    /// Build with explicit freshness + future-skew windows and
    /// the default ICE cooldown.
    #[staticmethod]
    fn with_freshness(
        registry: &PyOperatorRegistry,
        threshold: usize,
        freshness_window_ms: u64,
        future_skew_ms: u64,
    ) -> Self {
        Self {
            inner: CoreAdminVerifier::with_freshness(
                registry.snapshot(),
                threshold,
                Duration::from_millis(freshness_window_ms),
                Duration::from_millis(future_skew_ms),
            ),
        }
    }

    /// Build with every policy knob explicit. Primarily for
    /// tests that need a short cooldown window.
    #[staticmethod]
    fn with_full_policy(
        registry: &PyOperatorRegistry,
        threshold: usize,
        freshness_window_ms: u64,
        future_skew_ms: u64,
        ice_cooldown_ms: u64,
    ) -> Self {
        Self {
            inner: CoreAdminVerifier::with_full_policy(
                registry.snapshot(),
                threshold,
                Duration::from_millis(freshness_window_ms),
                Duration::from_millis(future_skew_ms),
                Duration::from_millis(ice_cooldown_ms),
            ),
        }
    }

    #[getter]
    fn threshold(&self) -> usize {
        self.inner.threshold()
    }

    #[getter]
    fn freshness_window_ms(&self) -> u64 {
        self.inner.freshness_window().as_millis() as u64
    }

    #[getter]
    fn future_skew_ms(&self) -> u64 {
        self.inner.future_skew().as_millis() as u64
    }

    #[getter]
    fn ice_cooldown_ms(&self) -> u64 {
        self.inner.ice_cooldown().as_millis() as u64
    }

    fn __repr__(&self) -> String {
        format!(
            "AdminVerifier(threshold={}, freshness_ms={}, future_skew_ms={}, ice_cooldown_ms={})",
            self.inner.threshold(),
            self.inner.freshness_window().as_millis() as u64,
            self.inner.future_skew().as_millis() as u64,
            self.inner.ice_cooldown().as_millis() as u64,
        )
    }
}

// =========================================================================
// AsyncDeckClient + AsyncAdminCommands — T3-G1 + T3-G3.
//
// Async sibling of DeckClient wraps the same Arc<CoreClient>. close()
// becomes awaitable; getters (identity, status, status_summary) stay
// sync; .admin returns AsyncAdminCommands with awaitable
// drain / enter_maintenance / etc.; .snapshots and
// .status_summary_stream return async-iter siblings directly.
//
// AsyncIceCommands is deferred — the IceProposal typestate
// (proposal → simulate → commit) needs awaitable mirrors of
// PyIceProposal + PySimulatedIceProposal, which is more surface
// than fits this slice.
// =========================================================================

/// Newtype around a `ChainCommit` so an awaitable can resolve to
/// the same `PyDict` shape the sync admin methods return.
struct AsyncChainCommitWrap(CoreChainCommit);

impl<'py> pyo3::IntoPyObject<'py> for AsyncChainCommitWrap {
    type Target = pyo3::types::PyAny;
    type Output = Bound<'py, Self::Target>;
    type Error = PyErr;
    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        Ok(chain_commit_to_dict(py, &self.0)?.into_any())
    }
}

/// Async sibling of [`PyAdminCommands`]. Wraps the same
/// `Arc<CoreClient>`; each method commits an `AdminEvent` and
/// returns an awaitable resolving to the same chain-commit dict
/// shape as the sync sibling.
///
/// Sync equivalent: :class:`AdminCommands`.
#[pyclass(name = "AsyncAdminCommands", module = "net._net")]
pub struct PyAsyncAdminCommands {
    client: Arc<CoreClient>,
}

impl PyAsyncAdminCommands {
    fn admin(&self) -> CoreAdminCommands<'_> {
        self.client.admin()
    }
}

#[pymethods]
impl PyAsyncAdminCommands {
    fn drain<'py>(
        &self,
        py: Python<'py>,
        node: u64,
        drain_for_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .drain(node, Duration::from_millis(drain_for_ms))
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    #[pyo3(signature = (node, drain_for_ms=None))]
    fn enter_maintenance<'py>(
        &self,
        py: Python<'py>,
        node: u64,
        drain_for_ms: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let drain_for = drain_for_ms.map(Duration::from_millis);
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .enter_maintenance(node, drain_for)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn exit_maintenance<'py>(
        &self,
        py: Python<'py>,
        node: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .exit_maintenance(node)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn cordon<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .cordon(node)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn uncordon<'py>(&self, py: Python<'py>, node: u64) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .uncordon(node)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn drop_replicas<'py>(
        &self,
        py: Python<'py>,
        node: u64,
        chains: Vec<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let chains: Vec<CoreChainId> = chains.into_iter().collect();
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .drop_replicas(node, chains)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn invalidate_placement<'py>(
        &self,
        py: Python<'py>,
        node: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .invalidate_placement(node)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn restart_all_daemons<'py>(
        &self,
        py: Python<'py>,
        node: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .restart_all_daemons(node)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    fn clear_avoid_list<'py>(
        &self,
        py: Python<'py>,
        node: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let commit = client
                .admin()
                .clear_avoid_list(node)
                .await
                .map_err(|e| Python::attach(|py| deck_err_from(py, e)))?;
            Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit))
        })
    }

    // Suppress dead-code on the helper accessor — used implicitly
    // by the awaitable bodies above, which use `client.admin()`
    // directly. Kept for parity with PyAdminCommands::admin so the
    // two surfaces are visually aligned.
    #[allow(dead_code)]
    fn _admin_helper(&self) {
        let _ = self.admin();
    }
}

/// Async sibling of [`PyDeckClient`]. Wraps the same
/// `Arc<CoreClient>`; close becomes awaitable, getters stay sync,
/// and `.admin` returns `AsyncAdminCommands`.
///
/// Sync equivalent: :class:`DeckClient`.
#[pyclass(name = "AsyncDeckClient", module = "net._net")]
pub struct PyAsyncDeckClient {
    client: Arc<CoreClient>,
    runtime: Arc<Runtime>,
    /// Owned SDK is held jointly with the sync sibling — only one
    /// shape should call `close()`. If the sync sibling is built
    /// first and the async wraps it, the sync's `Drop` drains the
    /// SDK on GC. If only the async exists, it owns the SDK.
    owned_sdk: parking_lot::Mutex<Option<CoreSdk>>,
}

impl Drop for PyAsyncDeckClient {
    fn drop(&mut self) {
        let Some(sdk) = self.owned_sdk.lock().take() else {
            return;
        };
        let runtime = self.runtime.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = runtime.block_on(sdk.shutdown());
        }));
    }
}

#[pymethods]
impl PyAsyncDeckClient {
    /// Build against an existing sync `DeckClient`. Cheap
    /// (`Arc::clone`); the sync sibling retains ownership of the
    /// SDK (close on the sync sibling drains it).
    #[new]
    fn new(client: &PyDeckClient) -> Self {
        Self {
            client: client.client.clone(),
            runtime: client.runtime.clone(),
            owned_sdk: parking_lot::Mutex::new(None),
        }
    }

    /// Construct a standalone async deck client owning its own
    /// supervisor. Mirrors `DeckClient.__new__`.
    #[staticmethod]
    #[pyo3(signature = (operator_seed, meshos_config=None, deck_config=None))]
    fn from_seed(
        py: Python<'_>,
        operator_seed: &Bound<'_, PyBytes>,
        meshos_config: Option<&Bound<'_, PyDict>>,
        deck_config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let seed_bytes = operator_seed.as_bytes();
        if seed_bytes.len() != 32 {
            return Err(deck_err(
                py,
                "invalid_argument",
                &format!(
                    "operator_seed must be exactly 32 bytes; got {}",
                    seed_bytes.len()
                ),
            ));
        }
        let mut seed = zeroize::Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(seed_bytes);
        let keypair = EntityKeypair::from_bytes(*seed);
        let identity = CoreIdentity::from_keypair(keypair);

        let sdk_cfg = crate::meshos::meshos_config_from_dict(py, meshos_config)?;
        let deck_cfg = config_from_dict(py, deck_config)?;

        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    deck_err(
                        py,
                        "runtime_start_failed",
                        &format!("failed to build tokio runtime: {e}"),
                    )
                })?,
        );
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = {
            let _enter = runtime.enter();
            CoreSdk::start(sdk_cfg, dispatcher)
        };
        let core_client = CoreClient::new(
            sdk.runtime().handle_clone(),
            sdk.runtime().snapshot_reader().clone(),
            identity,
            deck_cfg,
        );
        Ok(Self {
            client: Arc::new(core_client),
            runtime,
            owned_sdk: parking_lot::Mutex::new(Some(sdk)),
        })
    }

    fn identity(&self) -> PyOperatorIdentity {
        PyOperatorIdentity {
            inner: self.client.identity().clone(),
        }
    }

    /// One-shot read of the latest `MeshOsSnapshot` (JSON string).
    /// Sync — local snapshot reader.
    fn status(&self, py: Python<'_>) -> PyResult<String> {
        let snap = self.client.status();
        snapshot_to_json(py, &snap)
    }

    /// One-shot read of the rolled-up `StatusSummary`. Sync.
    fn status_summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let s = self.client.status_summary();
        status_summary_to_dict(py, &s)
    }

    /// Live snapshot stream — returns an `AsyncSnapshotStream`
    /// ready for ``async for``.
    fn snapshots(&self) -> PyAsyncSnapshotStream {
        let _enter = self.runtime.enter();
        let stream = self.client.snapshots();
        PyAsyncSnapshotStream {
            inner: Arc::new(tokio::sync::Mutex::new(Some(stream))),
        }
    }

    /// Live `StatusSummary` async stream.
    fn status_summary_stream(&self) -> PyAsyncStatusSummaryStream {
        let _enter = self.runtime.enter();
        let stream = self.client.status_summary_stream();
        PyAsyncStatusSummaryStream {
            inner: Arc::new(tokio::sync::Mutex::new(Some(stream))),
        }
    }

    #[getter]
    fn admin(&self) -> PyAsyncAdminCommands {
        PyAsyncAdminCommands {
            client: self.client.clone(),
        }
    }

    /// Tear down the private supervisor runtime if this client
    /// owns one (constructed via `from_seed`). Returns an
    /// awaitable. No-op for clients built against an existing
    /// `DeckClient` (the sync sibling owns the SDK).
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let Some(sdk) = self.owned_sdk.lock().take() else {
            return pyo3_async_runtimes::tokio::future_into_py(py, async move {
                Ok::<(), PyErr>(())
            });
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            sdk.shutdown().await.map_err(|e| {
                Python::attach(|py| {
                    deck_err(
                        py,
                        "shutdown_failed",
                        &format!("runtime shutdown failed: {e:?}"),
                    )
                })
            })?;
            Ok::<(), PyErr>(())
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "AsyncDeckClient(operator_id={:#x})",
            self.client.identity().operator_id()
        )
    }
}

// =========================================================================
// AsyncSnapshotStream + AsyncStatusSummaryStream — T3-G2.
//
// PEP 525 async iterators over the existing CoreSnapshotStream /
// CoreStatusStream. Constructed via .from_sync(sync_stream) which
// consumes the sync stream's inner — the sync stream becomes
// closed afterward (calling __next__ raises StopIteration).
//
// Each async-iter yields the same per-tick payload shape as the
// sync sibling: SnapshotStream → JSON string, StatusSummaryStream
// → dict.
// =========================================================================

/// Newtype wrapping a `StatusSummary` so an awaitable can resolve
/// to a `PyDict` via `IntoPyObject` on the resume step (the
/// dict-builder needs a `Python<'py>` token).
struct AsyncStatusSummaryWrap(StatusSummary);

impl<'py> pyo3::IntoPyObject<'py> for AsyncStatusSummaryWrap {
    type Target = pyo3::types::PyAny;
    type Output = Bound<'py, Self::Target>;
    type Error = PyErr;
    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        Ok(status_summary_to_dict(py, &self.0)?.into_any())
    }
}

/// Async sibling of [`PySnapshotStream`]. PEP 525 async iterator
/// — ``async for snap_json in stream:`` yields JSON strings.
/// End-of-stream raises `StopAsyncIteration`.
///
/// Sync equivalent: :class:`SnapshotStream`.
#[pyclass(name = "AsyncSnapshotStream", module = "net._net")]
pub struct PyAsyncSnapshotStream {
    inner: Arc<tokio::sync::Mutex<Option<CoreSnapshotStream>>>,
}

#[pymethods]
impl PyAsyncSnapshotStream {
    /// Consume an existing sync `SnapshotStream`, taking ownership
    /// of its inner stream. The sync stream becomes closed —
    /// subsequent `__next__` calls raise `StopIteration`.
    #[staticmethod]
    fn from_sync(stream: &mut PySnapshotStream) -> PyResult<Self> {
        let inner = stream.inner.take().ok_or_else(|| {
            Python::attach(|py| deck_err(py, "stream_closed", "snapshot stream was closed"))
        })?;
        Ok(Self {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
        })
    }

    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = inner.lock().await;
            let stream = match guard.as_mut() {
                Some(s) => s,
                None => return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            };
            let next = stream.next().await;
            match next {
                Some(Ok(snap)) => serde_json::to_string(&snap).map_err(|e| {
                    Python::attach(|py| {
                        deck_err(
                            py,
                            "snapshot_serialize_failed",
                            &format!("MeshOsSnapshot JSON serialize: {e}"),
                        )
                    })
                }),
                Some(Err(e)) => Err(Python::attach(|py| deck_err_from(py, e))),
                None => {
                    *guard = None;
                    Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()))
                }
            }
        })
    }

    /// Stop the iterator. Idempotent.
    fn close(&self) {
        if let Ok(mut guard) = self.inner.try_lock() {
            *guard = None;
        }
    }

    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            *inner.lock().await = None;
            Ok::<(), PyErr>(())
        })
    }
}

/// Async sibling of [`PyStatusSummaryStream`]. PEP 525 async
/// iterator — ``async for summary in stream:`` yields the same
/// dict shape as :meth:`StatusSummaryStream.__next__`.
///
/// Sync equivalent: :class:`StatusSummaryStream`.
#[pyclass(name = "AsyncStatusSummaryStream", module = "net._net")]
pub struct PyAsyncStatusSummaryStream {
    inner: Arc<tokio::sync::Mutex<Option<CoreStatusStream>>>,
}

#[pymethods]
impl PyAsyncStatusSummaryStream {
    /// Consume an existing sync `StatusSummaryStream`.
    #[staticmethod]
    fn from_sync(stream: &mut PyStatusSummaryStream) -> PyResult<Self> {
        let inner = stream.inner.take().ok_or_else(|| {
            Python::attach(|py| {
                deck_err(py, "stream_closed", "status summary stream was closed")
            })
        })?;
        Ok(Self {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
        })
    }

    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = inner.lock().await;
            let stream = match guard.as_mut() {
                Some(s) => s,
                None => return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            };
            let next = stream.next().await;
            match next {
                Some(Ok(s)) => Ok::<AsyncStatusSummaryWrap, PyErr>(AsyncStatusSummaryWrap(s)),
                Some(Err(e)) => Err(Python::attach(|py| deck_err_from(py, e))),
                None => {
                    *guard = None;
                    Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()))
                }
            }
        })
    }

    fn close(&self) {
        if let Ok(mut guard) = self.inner.try_lock() {
            *guard = None;
        }
    }

    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            *inner.lock().await = None;
            Ok::<(), PyErr>(())
        })
    }
}
