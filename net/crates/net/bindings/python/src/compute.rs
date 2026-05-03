//! PyO3 surface for the compute runtime — `MeshDaemon` + migration.
//!
//! Stage 5 of `SDK_COMPUTE_SURFACE_PLAN.md`.
//!
//! # Dispatcher pattern
//!
//! Unlike the NAPI side — where TSFN callbacks marshal to the Node
//! main thread via `call_with_return_value` + mpsc — PyO3 lets us
//! call into Python from any tokio worker by acquiring the GIL
//! with `Python::attach`. That yields a simpler bridge: hold
//! `Py<PyAny>` for each callback; every `process` / `snapshot` /
//! `restore` invocation does `Python::attach(|py| ...)` inline.
//! No cross-thread channel dance required.
//!
//! The GIL-acquisition latency *does* carry the same caveat as the
//! NAPI layer: Python-implemented daemons don't inherit the
//! microsecond-latency contract of `MeshDaemon::process`. Hot
//! loops should stay in Rust.
//!
//! # Error prefix
//!
//! Every `PyErr` produced here uses the `daemon:` prefix (same
//! convention as `identity:` / `cortex:` / `token:`).

use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

use net::adapter::net::behavior::capability::CapabilityFilter;
use net::adapter::net::compute::{DaemonError as CoreDaemonError, DaemonHostConfig, MeshDaemon};
use net::adapter::net::state::causal::{CausalEvent, CausalLink};
use net_sdk::compute::{
    DaemonError as SdkDaemonError, DaemonHandle as SdkDaemonHandle,
    DaemonRuntime as SdkDaemonRuntime, MigrationError as SdkMigrationError, MigrationFailureReason,
    MigrationHandle as SdkMigrationHandle, MigrationOpts, MigrationPhase as CoreMigrationPhase,
    StateSnapshot,
};
use net_sdk::mesh::Mesh as SdkMesh;
use tokio::runtime::Runtime;

// =========================================================================
// Error prefix — stable string convention
// =========================================================================

const ERR_DAEMON_PREFIX: &str = "daemon:";

// ---------------------------------------------------------------
// Exception classes
// ---------------------------------------------------------------
//
// `DaemonError` — base for anything the compute runtime throws.
// `MigrationError` — subclass of DaemonError; message carries a
//   structured `migration: <kind>[: detail]` payload so Python
//   callers can dispatch on the kind string without regex-parsing
//   free-form messages.
//
// Mirrors the TS `DaemonError` / `MigrationError extends DaemonError`
// shape; the kind vocabulary is the same set used by the NAPI
// layer's `format_migration_failure_reason` / `format_migration_error`.

pyo3::create_exception!(
    _net,
    DaemonError,
    PyException,
    "Base class for compute-runtime errors. Message is prefixed \
     with `daemon: `. Non-migration failures surface with a \
     free-form detail (`daemon: <detail>`); migration failures \
     use the `MigrationError` subclass with a `migration: <kind>` \
     body — see `MigrationError` for the kind vocabulary."
);

pyo3::create_exception!(
    _net,
    MigrationError,
    DaemonError,
    "Migration-layer failure. Message has the form \
     `daemon: migration: <kind>[: <detail>]`, where `<kind>` is \
     one of `not-ready` | `factory-not-found` | \
     `compute-not-supported` | `state-failed` | \
     `already-migrating` | `identity-transport-failed` | \
     `not-ready-timeout` | `daemon-not-found` | \
     `target-unavailable` | `wrong-phase` | `snapshot-too-large`. \
     Use the `net.migration_error_kind` helper to extract the \
     kind from a caught exception programmatically."
);

/// Build a `DaemonError` with the `daemon:` prefix. Matches the
/// identity / cortex / token conventions so Python-side
/// classifiers can route on the prefix cheaply.
pub(crate) fn daemon_err(msg: impl Into<String>) -> PyErr {
    PyErr::new::<DaemonError, _>(format!("{} {}", ERR_DAEMON_PREFIX, msg.into()))
}

/// Build a `MigrationError` with the `daemon: migration:` prefix.
fn migration_err(body: impl Into<String>) -> PyErr {
    PyErr::new::<MigrationError, _>(format!("{} migration: {}", ERR_DAEMON_PREFIX, body.into()))
}

/// Map an SDK `DaemonError` to the right Python-level exception
/// class. Migration failures get the structured
/// `migration: <kind>[: detail]` body on a `MigrationError`;
/// everything else falls through to `DaemonError` with the SDK's
/// Display-formatted message.
fn daemon_err_from_sdk(e: SdkDaemonError) -> PyErr {
    match e {
        SdkDaemonError::MigrationFailed(reason) => {
            migration_err(format_migration_failure_reason(&reason))
        }
        SdkDaemonError::Migration(mig_err) => migration_err(format_migration_error(&mig_err)),
        other => daemon_err(other.to_string()),
    }
}

fn format_migration_failure_reason(reason: &MigrationFailureReason) -> String {
    match reason {
        MigrationFailureReason::NotReady => "not-ready".to_string(),
        MigrationFailureReason::FactoryNotFound => "factory-not-found".to_string(),
        MigrationFailureReason::ComputeNotSupported => "compute-not-supported".to_string(),
        MigrationFailureReason::StateFailed(msg) => format!("state-failed: {msg}"),
        MigrationFailureReason::AlreadyMigrating => "already-migrating".to_string(),
        MigrationFailureReason::IdentityTransportFailed(msg) => {
            format!("identity-transport-failed: {msg}")
        }
        MigrationFailureReason::NotReadyTimeout { attempts } => {
            format!("not-ready-timeout: {attempts}")
        }
    }
}

fn format_migration_error(err: &SdkMigrationError) -> String {
    match err {
        SdkMigrationError::DaemonNotFound(origin) => format!("daemon-not-found: {origin:#x}"),
        SdkMigrationError::TargetUnavailable(node) => format!("target-unavailable: {node:#x}"),
        SdkMigrationError::NoTargetAvailable => "no-target-available".to_string(),
        SdkMigrationError::StateFailed(msg) => format!("state-failed: {msg}"),
        SdkMigrationError::AlreadyMigrating(origin) => format!("already-migrating: {origin:#x}"),
        SdkMigrationError::WrongPhase { expected, got } => {
            format!("wrong-phase: {expected:?}: {got:?}")
        }
        SdkMigrationError::SnapshotTooLarge { size, max } => {
            format!("snapshot-too-large: {size}: {max}")
        }
    }
}

// ---------------------------------------------------------------
// Migration phase helper
// ---------------------------------------------------------------

fn migration_phase_str(phase: CoreMigrationPhase) -> &'static str {
    match phase {
        CoreMigrationPhase::Snapshot => "snapshot",
        CoreMigrationPhase::Transfer => "transfer",
        CoreMigrationPhase::Restore => "restore",
        CoreMigrationPhase::Replay => "replay",
        CoreMigrationPhase::Cutover => "cutover",
        CoreMigrationPhase::Complete => "complete",
    }
}

// =========================================================================
// PyCausalEvent — the value delivered to a daemon's `process`
// =========================================================================

/// A causal event handed to a daemon's `process(event)` method.
///
/// Field shape matches `net::adapter::net::state::causal::CausalEvent`.
/// The 64-bit `sequence` is exposed as a Python `int` — Python
/// integers are unbounded so no precision concerns.
#[pyclass(name = "CausalEvent", module = "net._net", from_py_object)]
#[derive(Clone)]
pub struct PyCausalEvent {
    /// 32-bit hash of the emitting entity.
    #[pyo3(get)]
    pub origin_hash: u32,
    /// Sequence number in the emitter's causal chain.
    #[pyo3(get)]
    pub sequence: u64,
    /// Opaque payload bytes — identical to `event.payload` on the
    /// Rust side.
    pub payload: Vec<u8>,
}

#[pymethods]
impl PyCausalEvent {
    /// Construct manually — mainly used by tests that call
    /// `DaemonRuntime.deliver` directly.
    #[new]
    #[pyo3(signature = (origin_hash, sequence, payload))]
    fn new(origin_hash: u32, sequence: u64, payload: Vec<u8>) -> Self {
        Self {
            origin_hash,
            sequence,
            payload,
        }
    }

    /// Opaque payload bytes.
    #[getter]
    fn payload<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.payload)
    }

    fn __repr__(&self) -> String {
        format!(
            "CausalEvent(origin_hash={:#x}, sequence={}, payload_len={})",
            self.origin_hash,
            self.sequence,
            self.payload.len()
        )
    }
}

impl PyCausalEvent {
    fn from_core(event: &CausalEvent) -> Self {
        Self {
            origin_hash: event.link.origin_hash,
            sequence: event.link.sequence,
            payload: event.payload.as_ref().to_vec(),
        }
    }
}

// =========================================================================
// DaemonHostConfig — Python dict → core config
// =========================================================================

/// Parse an optional Python dict into a core `DaemonHostConfig`.
/// Keys: `auto_snapshot_interval` (int), `max_log_entries` (int).
/// Unknown keys are ignored; missing keys take core defaults.
fn daemon_host_config_from_dict(config: Option<&Bound<'_, PyDict>>) -> PyResult<DaemonHostConfig> {
    let mut cfg = DaemonHostConfig::default();
    let Some(d) = config else {
        return Ok(cfg);
    };
    if let Some(v) = d.get_item("auto_snapshot_interval")? {
        cfg.auto_snapshot_interval = v
            .extract::<u64>()
            .map_err(|e| daemon_err(format!("auto_snapshot_interval must be int: {e}")))?;
    }
    if let Some(v) = d.get_item("max_log_entries")? {
        cfg.max_log_entries = v
            .extract::<u32>()
            .map_err(|e| daemon_err(format!("max_log_entries must be int: {e}")))?;
    }
    Ok(cfg)
}

// =========================================================================
// PyDaemonHandle — returned by `spawn` / `spawn_from_snapshot`
// =========================================================================

/// Handle to a running daemon. Identifies a specific daemon by
/// its `origin_hash`; cloning the Python object shares the same
/// underlying daemon. Dropping the handle does NOT stop the
/// daemon — callers must call `stop(handle.origin_hash)`.
#[pyclass(name = "DaemonHandle", module = "net._net")]
pub struct PyDaemonHandle {
    origin_hash: u32,
    entity_id: [u8; 32],
    #[allow(dead_code)]
    inner: SdkDaemonHandle,
}

impl PyDaemonHandle {
    fn from_sdk(handle: SdkDaemonHandle) -> Self {
        let origin_hash = handle.origin_hash;
        let entity_id = *handle.entity_id.as_bytes();
        Self {
            origin_hash,
            entity_id,
            inner: handle,
        }
    }
}

#[pymethods]
impl PyDaemonHandle {
    /// 32-bit hash of the daemon's identity.
    #[getter]
    fn origin_hash(&self) -> u32 {
        self.origin_hash
    }

    /// Full 32-byte `EntityId` (ed25519 public key). Returned as
    /// `bytes` to match the convention used by `Identity.entity_id`.
    #[getter]
    fn entity_id<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.entity_id)
    }

    fn __repr__(&self) -> String {
        format!("DaemonHandle(origin_hash={:#x})", self.origin_hash)
    }
}

// =========================================================================
// PyDaemonRuntime — main surface
// =========================================================================

/// Python surface for the compute runtime. One instance per
/// `NetMesh`. Construct via `DaemonRuntime(mesh)`.
#[pyclass(name = "DaemonRuntime", module = "net._net")]
pub struct PyDaemonRuntime {
    inner: Arc<SdkDaemonRuntime>,
    runtime: Arc<Runtime>,
    /// Registered factory callables, keyed by `kind`. Values are
    /// `Py<PyAny>` holding the user's Python factory callable.
    /// The DashMap is the single source of truth; the SDK side
    /// mirrors a closure that reaches back into this map to call
    /// the Python factory on demand (on spawn, on migration-
    /// target reconstruction).
    factories: Arc<DashMap<String, Py<PyAny>>>,
}

impl PyDaemonRuntime {
    /// Shared access to the inner SDK runtime. Used by the
    /// `groups` module to pass a `&SdkDaemonRuntime` to the group
    /// constructors.
    #[cfg(feature = "groups")]
    pub(crate) fn sdk_runtime(&self) -> &SdkDaemonRuntime {
        &self.inner
    }
}

#[pymethods]
impl PyDaemonRuntime {
    /// Build a compute runtime against an existing `NetMesh`.
    #[new]
    fn new(mesh: &crate::mesh_bindings::NetMesh) -> PyResult<Self> {
        let node = mesh.node_arc_clone()?;
        let channel_configs = mesh.channel_configs_arc();
        let runtime = mesh.runtime_arc();
        let sdk_mesh = SdkMesh::from_node_arc(node, channel_configs, None);
        let sdk_rt = SdkDaemonRuntime::new(Arc::new(sdk_mesh));
        Ok(PyDaemonRuntime {
            inner: Arc::new(sdk_rt),
            runtime,
            factories: Arc::new(DashMap::new()),
        })
    }

    /// Transition to `Ready`. Installs the migration subprotocol
    /// handler. Idempotent — a second `Ready` call is a no-op; a
    /// call on a `ShuttingDown` runtime raises.
    fn start(&self, py: Python<'_>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        py.detach(|| {
            runtime
                .block_on(async move { inner.start().await })
                .map_err(|e| daemon_err(e.to_string()))
        })
    }

    /// Tear down the runtime. Drains daemons, clears factory
    /// registrations, uninstalls the migration handler. The
    /// underlying `NetMesh` is untouched.
    fn shutdown(&self, py: Python<'_>) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let factories = self.factories.clone();
        py.detach(move || {
            runtime
                .block_on(async move { inner.shutdown().await })
                .map_err(|e| daemon_err(e.to_string()))?;
            factories.clear();
            Ok(())
        })
    }

    /// `True` iff the runtime is `Ready`.
    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    /// Number of daemons currently registered.
    fn daemon_count(&self) -> u32 {
        self.inner.daemon_count() as u32
    }

    /// Register a factory callable under `kind`. The callable
    /// must return a `MeshDaemon`-shaped object with a `process`
    /// method and optional `snapshot` / `restore` methods.
    ///
    /// Second registration of the same `kind` raises
    /// `daemon: factory for kind '<kind>' is already registered`.
    fn register_factory(&self, kind: String, factory: Py<PyAny>) -> PyResult<()> {
        use dashmap::mapref::entry::Entry;
        match self.factories.entry(kind.clone()) {
            Entry::Occupied(_) => {
                return Err(daemon_err(format!(
                    "factory for kind '{kind}' is already registered"
                )));
            }
            Entry::Vacant(slot) => {
                slot.insert(factory);
            }
        }

        // Mirror into the SDK factory map. The SDK closure reaches
        // back into `self.factories` every time the core registry
        // needs a fresh daemon (spawn + migration-target
        // reconstruction). We clone `factories` (the Arc) into the
        // closure so the closure's Fn bound is satisfied without
        // borrowing `self`.
        //
        // On the failure path below (`ReconstructionErrorBridge`) we
        // log and return a bridge that raises a typed error on
        // `restore` / `process`, so a migration through a broken
        // factory fails visibly rather than silently swallowing
        // events — same approach as the NAPI layer.
        let factories_for_closure = self.factories.clone();
        let kind_for_closure = kind.clone();
        if let Err(e) = self.inner.register_factory(&kind, move || {
            build_bridge_from_factory(&factories_for_closure, &kind_for_closure)
        }) {
            self.factories.remove(&kind);
            return Err(daemon_err(e.to_string()));
        }
        Ok(())
    }

    /// Spawn a daemon of `kind` under the given identity.
    ///
    /// Invokes the registered factory callable (in the current
    /// thread's GIL context) to build the per-instance daemon
    /// object, then extracts its `process` / `snapshot` /
    /// `restore` methods and wraps them in a `PyDaemonBridge`
    /// that the core registry drives.
    ///
    /// `config` accepts an optional dict with keys
    /// `auto_snapshot_interval` (int) and `max_log_entries`
    /// (int); missing keys take runtime defaults.
    #[pyo3(signature = (kind, identity, config=None))]
    fn spawn(
        &self,
        py: Python<'_>,
        kind: String,
        identity: &crate::identity::Identity,
        config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyDaemonHandle> {
        if !self.factories.contains_key(&kind) {
            return Err(daemon_err(format!(
                "no factory registered for kind '{kind}'"
            )));
        }
        let cfg = daemon_host_config_from_dict(config)?;
        let sdk_identity = identity.to_sdk_identity();
        // Build the bridge by invoking the Python factory now.
        // This makes `spawn` fail fast if the factory throws
        // rather than surfacing the failure later during event
        // dispatch.
        let bridge = build_bridge_inline(py, &self.factories, &kind)?;

        // Kind-factory closure for migration-target
        // reconstruction — fresh bridge per invocation via the
        // same `factories` map.
        let factories_for_closure = self.factories.clone();
        let kind_for_closure = kind.clone();
        let kind_factory = move || -> Box<dyn MeshDaemon> {
            build_bridge_from_factory(&factories_for_closure, &kind_for_closure)
        };

        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let handle = py.detach(move || {
            runtime
                .block_on(async move {
                    inner
                        .spawn_with_daemon(sdk_identity, cfg, bridge, kind_factory)
                        .await
                })
                .map_err(|e| daemon_err(e.to_string()))
        })?;
        Ok(PyDaemonHandle::from_sdk(handle))
    }

    /// Stop a daemon, removing it from the runtime's registry.
    /// Idempotent during `ShuttingDown`; raises for other
    /// failures with the `daemon:` prefix.
    fn stop(&self, py: Python<'_>, origin_hash: u32) -> PyResult<()> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        py.detach(move || {
            runtime
                .block_on(async move { inner.stop(origin_hash).await })
                .map_err(|e| daemon_err(e.to_string()))
        })
    }

    /// Take a snapshot of a running daemon by `origin_hash`.
    ///
    /// Returns the daemon's serialized state as `bytes`, or
    /// `None` when the daemon is stateless (no `snapshot`
    /// method, or its `snapshot` returned `None`). The wire
    /// format is the core's `StateSnapshot::to_bytes` encoding —
    /// opaque to Python callers, but round-trippable via
    /// `spawn_from_snapshot`.
    fn snapshot<'py>(
        &self,
        py: Python<'py>,
        origin_hash: u32,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let snap = py.detach(move || {
            runtime
                .block_on(async move { inner.snapshot(origin_hash).await })
                .map_err(|e| daemon_err(e.to_string()))
        })?;
        Ok(snap.map(|s| PyBytes::new(py, &s.to_bytes())))
    }

    /// Spawn a daemon of `kind` from a previously-taken snapshot.
    /// Parallel to `spawn`, but the daemon's initial state is
    /// seeded from `snapshot_bytes` by calling its `restore`
    /// method before any events land.
    ///
    /// `snapshot_bytes` must be the exact `bytes` returned by a
    /// prior call to `snapshot`; mismatched or corrupted bytes
    /// surface as `daemon: snapshot decode failed`.
    ///
    /// Identity check: the snapshot's `entity_id` must match the
    /// caller's `identity` — mismatch raises `daemon: snapshot
    /// identity mismatch`.
    #[pyo3(signature = (kind, identity, snapshot_bytes, config=None))]
    fn spawn_from_snapshot(
        &self,
        py: Python<'_>,
        kind: String,
        identity: &crate::identity::Identity,
        snapshot_bytes: &[u8],
        config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyDaemonHandle> {
        if !self.factories.contains_key(&kind) {
            return Err(daemon_err(format!(
                "no factory registered for kind '{kind}'"
            )));
        }
        // Decode the snapshot synchronously — cheap, and a clean
        // `daemon: snapshot decode failed` is friendlier than the
        // downstream SDK error.
        let snapshot_decoded = StateSnapshot::from_bytes(snapshot_bytes)
            .ok_or_else(|| daemon_err("snapshot decode failed"))?;

        let cfg = daemon_host_config_from_dict(config)?;
        let sdk_identity = identity.to_sdk_identity();
        let bridge = build_bridge_inline(py, &self.factories, &kind)?;

        let factories_for_closure = self.factories.clone();
        let kind_for_closure = kind.clone();
        let kind_factory = move || -> Box<dyn MeshDaemon> {
            build_bridge_from_factory(&factories_for_closure, &kind_for_closure)
        };

        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let handle = py.detach(move || {
            runtime
                .block_on(async move {
                    inner
                        .spawn_from_snapshot_with_daemon(
                            sdk_identity,
                            snapshot_decoded,
                            cfg,
                            bridge,
                            kind_factory,
                        )
                        .await
                })
                .map_err(|e| daemon_err(e.to_string()))
        })?;
        Ok(PyDaemonHandle::from_sdk(handle))
    }

    /// Deliver one causal event to the daemon identified by
    /// `origin_hash`. Invokes the daemon's `process(event)`
    /// method (in this runtime's GIL context via the bridge)
    /// and returns the list of output `bytes` payloads.
    ///
    /// Direct ingress — Stage 1 convenience. Mesh-dispatched
    /// delivery lands in a later stage; this method stays as
    /// test sugar + a manual-trigger surface.
    fn deliver(
        &self,
        py: Python<'_>,
        origin_hash: u32,
        event: &PyCausalEvent,
    ) -> PyResult<Vec<Py<PyBytes>>> {
        let core_event = CausalEvent {
            link: CausalLink {
                origin_hash: event.origin_hash,
                horizon_encoded: 0,
                sequence: event.sequence,
                parent_hash: 0,
            },
            payload: Bytes::copy_from_slice(&event.payload),
            received_at: 0,
        };

        // Run the SDK deliver on a tokio worker. Keep the GIL
        // while doing so because `MeshDaemon::process` reaches
        // back into Python via `Python::attach` — if we held
        // it here it would deadlock. Detaching releases the
        // GIL; the worker acquires it via `attach` during
        // dispatch and releases it when process returns.
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let outputs = py.detach(move || {
            runtime.block_on(async move {
                inner
                    .deliver(origin_hash, &core_event)
                    .map_err(|e| daemon_err(e.to_string()))
            })
        })?;

        let out: Vec<Py<PyBytes>> = outputs
            .into_iter()
            .map(|ev| PyBytes::new(py, ev.payload.as_ref()).unbind())
            .collect();
        Ok(out)
    }

    /// Initiate a migration for the daemon identified by
    /// `origin_hash`, moving it from `source_node` to
    /// `target_node`. Returns a `MigrationHandle` whose `wait()`
    /// resolves when the migration reaches a terminal state
    /// (`complete` on success, `MigrationError` otherwise).
    ///
    /// Both node IDs are `u64` — Python's unbounded int handles
    /// the full range without precision concerns.
    fn start_migration(
        &self,
        py: Python<'_>,
        origin_hash: u32,
        source_node: u64,
        target_node: u64,
    ) -> PyResult<PyMigrationHandle> {
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let handle = py.detach(move || {
            runtime
                .block_on(async move {
                    inner
                        .start_migration(origin_hash, source_node, target_node)
                        .await
                })
                .map_err(daemon_err_from_sdk)
        })?;
        Ok(PyMigrationHandle::from_sdk(handle, self.runtime.clone()))
    }

    /// `start_migration` with caller-supplied options. Keys:
    /// `transport_identity` (bool), `retry_not_ready_ms` (int).
    #[pyo3(signature = (origin_hash, source_node, target_node, opts))]
    fn start_migration_with(
        &self,
        py: Python<'_>,
        origin_hash: u32,
        source_node: u64,
        target_node: u64,
        opts: &Bound<'_, PyDict>,
    ) -> PyResult<PyMigrationHandle> {
        let mut sdk_opts = MigrationOpts::default();
        if let Some(v) = opts.get_item("transport_identity")? {
            // Route the conversion error through `daemon_err` so an
            // invalid value (e.g. a string instead of a bool) raises
            // a `daemon:`-prefixed error that the SDK side classifies
            // as `DaemonError`, rather than a raw PyO3 TypeError that
            // bypasses the typed-error convention.
            sdk_opts.transport_identity = v
                .extract()
                .map_err(|e| daemon_err(format!("transport_identity must be bool: {e}")))?;
        }
        if let Some(v) = opts.get_item("retry_not_ready_ms")? {
            // Same prefix-preservation rationale as the
            // `transport_identity` branch above.
            let ms: u64 = v.extract().map_err(|e| {
                daemon_err(format!("retry_not_ready_ms must be non-negative int: {e}"))
            })?;
            sdk_opts.retry_not_ready = if ms == 0 {
                None
            } else {
                Some(std::time::Duration::from_millis(ms))
            };
        }
        let runtime = self.runtime.clone();
        let inner = self.inner.clone();
        let handle = py.detach(move || {
            runtime
                .block_on(async move {
                    inner
                        .start_migration_with(origin_hash, source_node, target_node, sdk_opts)
                        .await
                })
                .map_err(daemon_err_from_sdk)
        })?;
        Ok(PyMigrationHandle::from_sdk(handle, self.runtime.clone()))
    }

    /// Declare on the target node that a migration will land here
    /// for `origin_hash` of `kind`. Registers a placeholder
    /// factory — the migration snapshot's identity envelope
    /// supplies the real keypair at restore time.
    #[pyo3(signature = (kind, origin_hash, config=None))]
    fn expect_migration(
        &self,
        kind: String,
        origin_hash: u32,
        config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let cfg = daemon_host_config_from_dict(config)?;
        self.inner
            .expect_migration(&kind, origin_hash, cfg)
            .map_err(daemon_err_from_sdk)
    }

    /// Pre-register a target-side identity for a migration that
    /// will NOT carry an identity envelope (source used
    /// `transport_identity=False`). For the common envelope path,
    /// prefer `expect_migration`.
    #[pyo3(signature = (kind, identity, config=None))]
    fn register_migration_target_identity(
        &self,
        kind: String,
        identity: &crate::identity::Identity,
        config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let sdk_identity = identity.to_sdk_identity();
        let cfg = daemon_host_config_from_dict(config)?;
        self.inner
            .register_migration_target_identity(&kind, sdk_identity, cfg)
            .map_err(daemon_err_from_sdk)
    }

    /// Query the orchestrator's current migration phase for
    /// `origin_hash`, returned as a string
    /// (`snapshot` | `transfer` | `restore` | `replay` |
    /// `cutover` | `complete`) or `None` if no migration is in
    /// flight for that origin.
    fn migration_phase(&self, origin_hash: u32) -> Option<&'static str> {
        self.inner
            .migration_phase(origin_hash)
            .map(migration_phase_str)
    }

    fn __repr__(&self) -> String {
        format!(
            "DaemonRuntime(ready={}, daemons={})",
            self.inner.is_ready(),
            self.inner.daemon_count()
        )
    }
}

// =========================================================================
// PyMigrationHandle — observe and abort an in-flight migration
// =========================================================================

/// Handle to an in-flight migration. Returned by
/// `DaemonRuntime.start_migration` /
/// `DaemonRuntime.start_migration_with`.
///
/// Dropping the handle does NOT cancel the migration — the
/// orchestrator keeps driving it to completion in the background.
/// Keep the handle to observe phase transitions or request abort.
#[pyclass(name = "MigrationHandle", module = "net._net")]
pub struct PyMigrationHandle {
    origin_hash: u32,
    source_node: u64,
    target_node: u64,
    inner: SdkMigrationHandle,
    runtime: Arc<Runtime>,
}

impl PyMigrationHandle {
    fn from_sdk(handle: SdkMigrationHandle, runtime: Arc<Runtime>) -> Self {
        Self {
            origin_hash: handle.origin_hash,
            source_node: handle.source_node,
            target_node: handle.target_node,
            inner: handle,
            runtime,
        }
    }
}

#[pymethods]
impl PyMigrationHandle {
    /// 32-bit origin hash of the daemon being migrated.
    #[getter]
    fn origin_hash(&self) -> u32 {
        self.origin_hash
    }

    /// Source node ID (currently hosting the daemon).
    #[getter]
    fn source_node(&self) -> u64 {
        self.source_node
    }

    /// Target node ID (post-cutover).
    #[getter]
    fn target_node(&self) -> u64 {
        self.target_node
    }

    /// Current migration phase, or `None` once the migration has
    /// left the orchestrator's records (terminal success or
    /// abort). Callers distinguish success from abort by
    /// remembering the last non-None phase they observed.
    fn phase(&self) -> Option<&'static str> {
        self.inner.phase().map(migration_phase_str)
    }

    /// Block until the migration reaches a terminal state.
    /// Returns on `complete`; raises `MigrationError` on abort
    /// or structured failure. No wall-clock timeout — use
    /// `wait_with_timeout` for a bound.
    fn wait(&self, py: Python<'_>) -> PyResult<()> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(move || {
            runtime
                .block_on(async move { inner.wait().await })
                .map_err(daemon_err_from_sdk)
        })
    }

    /// Like `wait` with a caller-controlled timeout in
    /// milliseconds. On timeout, the orchestrator record is
    /// aborted and the call raises `MigrationError`.
    fn wait_with_timeout(&self, py: Python<'_>, timeout_ms: u64) -> PyResult<()> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(move || {
            runtime
                .block_on(async move {
                    inner
                        .wait_with_timeout(std::time::Duration::from_millis(timeout_ms))
                        .await
                })
                .map_err(daemon_err_from_sdk)
        })
    }

    /// Request cancellation of the migration. Best-effort: past
    /// `cutover`, the routing flip cannot be undone cleanly, and
    /// this call resolves without aborting.
    fn cancel(&self, py: Python<'_>) -> PyResult<()> {
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        py.detach(move || {
            runtime
                .block_on(async move { inner.cancel().await })
                .map_err(daemon_err_from_sdk)
        })
    }

    /// Return a sync iterator that yields each distinct migration
    /// phase as the orchestrator transitions through them, and
    /// terminates (via `StopIteration`) once the migration
    /// reaches a terminal state. 50 ms polling cadence — the
    /// same cadence used by the SDK's `wait()`.
    ///
    /// **Call-site ordering:** iterate as soon as the handle is
    /// returned. If you call `wait()` first and then iterate,
    /// the orchestrator record may already be cleared and the
    /// iterator yields nothing.
    fn phases(&self) -> PyMigrationPhasesIter {
        PyMigrationPhasesIter {
            inner: self.inner.clone(),
            last: None,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "MigrationHandle(origin_hash={:#x}, source={:#x}, target={:#x})",
            self.origin_hash, self.source_node, self.target_node,
        )
    }
}

// =========================================================================
// PyMigrationPhasesIter — sync iterator over migration phases
// =========================================================================

/// Sync iterator returned by `MigrationHandle.phases()`. Each
/// `__next__` polls the orchestrator and yields the current
/// phase when it differs from the previously yielded one.
/// Raises `StopIteration` when the orchestrator clears its
/// record (terminal success or abort).
#[pyclass(name = "MigrationPhasesIter", module = "net._net")]
pub struct PyMigrationPhasesIter {
    inner: SdkMigrationHandle,
    last: Option<&'static str>,
}

#[pymethods]
impl PyMigrationPhasesIter {
    fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<&'static str> {
        // Poll loop — block on 50 ms sleeps with the GIL
        // released, so other Python threads can run while we
        // wait. Exit when the orchestrator clears its record
        // (phase goes to None) or when a new distinct phase
        // shows up.
        loop {
            let current = self.inner.phase().map(migration_phase_str);
            match current {
                None => return Err(pyo3::exceptions::PyStopIteration::new_err(())),
                Some(phase) => {
                    if Some(phase) != self.last {
                        self.last = Some(phase);
                        return Ok(phase);
                    }
                    py.detach(|| {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    });
                }
            }
        }
    }
}

// =========================================================================
// PyDaemonBridge — MeshDaemon impl driven by Py<PyAny> callbacks
// =========================================================================

/// Daemon bridge wrapping three Python callables (`process`,
/// `snapshot?`, `restore?`) extracted from a factory invocation.
/// Every `MeshDaemon` method call does a single
/// `Python::attach` to reach the underlying Python callable.
struct PyDaemonBridge {
    name: String,
    process: Py<PyAny>,
    snapshot: Option<Py<PyAny>>,
    restore: Option<Py<PyAny>>,
}

impl MeshDaemon for PyDaemonBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    /// Dispatch the event to the Python `process` callable. The
    /// callable must return an iterable of `bytes`; anything else
    /// is surfaced as `CoreDaemonError::ProcessFailed`.
    fn process(&mut self, event: &CausalEvent) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        let py_event = PyCausalEvent::from_core(event);
        Python::attach(|py| -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
            let event_obj = Py::new(py, py_event).map_err(|e| {
                CoreDaemonError::ProcessFailed(format!("failed to wrap event: {e}"))
            })?;
            let args = PyTuple::new(py, [event_obj.into_any()]).map_err(|e| {
                CoreDaemonError::ProcessFailed(format!("failed to build args: {e}"))
            })?;
            let result = self
                .process
                .call1(py, args)
                .map_err(|e| CoreDaemonError::ProcessFailed(format!("process raised: {e}")))?;
            let list: Bound<'_, PyAny> = result.into_bound(py);
            parse_output_list(&list)
        })
    }

    /// Ask the Python `snapshot()` callable for the daemon's
    /// current state. Returns `None` if no snapshot callable
    /// was registered, or if the callable returned `None`.
    fn snapshot(&self) -> Option<Bytes> {
        let snapshot = self.snapshot.as_ref()?;
        Python::attach(|py| -> Option<Bytes> {
            match snapshot.call0(py) {
                Ok(ret) => {
                    let ret_any = ret.into_bound(py);
                    if ret_any.is_none() {
                        None
                    } else {
                        match ret_any.extract::<Vec<u8>>() {
                            Ok(v) => Some(Bytes::from(v)),
                            Err(e) => {
                                eprintln!(
                                    "PyDaemonBridge::snapshot: return value is not bytes: {e}; treating as None"
                                );
                                None
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("PyDaemonBridge::snapshot: callable raised: {e}; treating as None");
                    None
                }
            }
        })
    }

    /// Invoke the Python `restore(state)` callable. Errors
    /// propagate as `CoreDaemonError::RestoreFailed`.
    fn restore(&mut self, state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        let Some(restore) = self.restore.as_ref() else {
            return Ok(());
        };
        Python::attach(|py| -> std::result::Result<(), CoreDaemonError> {
            let state_bytes = PyBytes::new(py, state.as_ref());
            let args = PyTuple::new(py, [state_bytes.into_any()]).map_err(|e| {
                CoreDaemonError::RestoreFailed(format!("failed to build args: {e}"))
            })?;
            restore
                .call1(py, args)
                .map_err(|e| CoreDaemonError::RestoreFailed(format!("restore raised: {e}")))?;
            Ok(())
        })
    }
}

/// Parse the Python return value of `process` into `Vec<Bytes>`.
/// Accepts any iterable whose elements convert to `bytes`.
fn parse_output_list(obj: &Bound<'_, PyAny>) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
    // Common case: list of bytes.
    if let Ok(list) = obj.cast::<PyList>() {
        let mut out = Vec::with_capacity(list.len());
        for item in list.iter() {
            let v: Vec<u8> = item.extract().map_err(|e| {
                CoreDaemonError::ProcessFailed(format!("process output element is not bytes: {e}"))
            })?;
            out.push(Bytes::from(v));
        }
        return Ok(out);
    }
    // Fallback: any iterable of bytes.
    match obj.try_iter() {
        Ok(iter) => {
            let mut out = Vec::new();
            for item in iter {
                let item = item.map_err(|e| {
                    CoreDaemonError::ProcessFailed(format!("iterating process output: {e}"))
                })?;
                let v: Vec<u8> = item.extract().map_err(|e| {
                    CoreDaemonError::ProcessFailed(format!(
                        "process output element is not bytes: {e}"
                    ))
                })?;
                out.push(Bytes::from(v));
            }
            Ok(out)
        }
        Err(e) => Err(CoreDaemonError::ProcessFailed(format!(
            "process must return a list/iterable of bytes; got {e}"
        ))),
    }
}

// =========================================================================
// Bridge construction helpers
// =========================================================================

/// Build a `PyDaemonBridge` by invoking the Python factory for
/// `kind` under the current GIL context. Used by `spawn` —
/// propagates factory exceptions directly to the caller.
fn build_bridge_inline(
    py: Python<'_>,
    factories: &DashMap<String, Py<PyAny>>,
    kind: &str,
) -> PyResult<Box<dyn MeshDaemon>> {
    let Some(entry) = factories.get(kind) else {
        return Err(daemon_err(format!(
            "no factory registered for kind '{kind}'"
        )));
    };
    let factory_obj = entry.clone_ref(py);
    drop(entry); // release DashMap guard before doing Python work

    let instance = factory_obj
        .call0(py)
        .map_err(|e| daemon_err(format!("factory for kind '{kind}' raised: {e}")))?;

    let process = instance.getattr(py, "process").map_err(|e| {
        daemon_err(format!(
            "factory return for kind '{kind}' has no `process` method: {e}"
        ))
    })?;

    let snapshot = match instance.getattr(py, "snapshot") {
        Ok(v) => {
            if v.is_none(py) {
                None
            } else {
                Some(v)
            }
        }
        Err(_) => None,
    };
    let restore = match instance.getattr(py, "restore") {
        Ok(v) => {
            if v.is_none(py) {
                None
            } else {
                Some(v)
            }
        }
        Err(_) => None,
    };

    Ok(Box::new(PyDaemonBridge {
        name: kind.to_string(),
        process,
        snapshot,
        restore,
    }))
}

/// Build a `PyDaemonBridge` from a tokio worker (no GIL held on
/// entry). Used by the SDK kind-factory closure for
/// migration-target reconstruction. Acquires the GIL via
/// `Python::attach`, calls the Python factory, extracts methods,
/// and returns a bridge — or a `ReconstructionErrorBridge` on any
/// failure so the next `restore` / `process` raises a typed error
/// rather than silently swallowing events.
fn build_bridge_from_factory(
    factories: &DashMap<String, Py<PyAny>>,
    kind: &str,
) -> Box<dyn MeshDaemon> {
    let factory_obj = match factories.get(kind) {
        Some(entry) => Python::attach(|py| entry.clone_ref(py)),
        None => {
            let reason = "no factory registered for this kind".to_string();
            eprintln!("build_bridge_from_factory: kind '{kind}': {reason}");
            return Box::new(ReconstructionErrorBridge::new(kind.to_string(), reason));
        }
    };

    Python::attach(|py| -> Box<dyn MeshDaemon> {
        let instance = match factory_obj.call0(py) {
            Ok(i) => i,
            Err(e) => {
                let reason = format!("Python factory raised: {e}");
                eprintln!("build_bridge_from_factory: kind '{kind}': {reason}");
                return Box::new(ReconstructionErrorBridge::new(kind.to_string(), reason));
            }
        };
        let process = match instance.getattr(py, "process") {
            Ok(f) => f,
            Err(e) => {
                let reason = format!("factory instance has no `process` attribute: {e}");
                eprintln!("build_bridge_from_factory: kind '{kind}': {reason}");
                return Box::new(ReconstructionErrorBridge::new(kind.to_string(), reason));
            }
        };
        let snapshot = match instance.getattr(py, "snapshot") {
            Ok(v) if !v.is_none(py) => Some(v),
            _ => None,
        };
        let restore = match instance.getattr(py, "restore") {
            Ok(v) if !v.is_none(py) => Some(v),
            _ => None,
        };
        Box::new(PyDaemonBridge {
            name: kind.to_string(),
            process,
            snapshot,
            restore,
        })
    })
}

// =========================================================================
// ReconstructionErrorBridge — migration-target fallback when the
// Python factory can't be reached or its return value is malformed.
// =========================================================================

/// Fallback `MeshDaemon` used when `build_bridge_from_factory`
/// can't produce a real `PyDaemonBridge` (factory missing from the
/// registry, Python factory raised, instance lacks `process`, etc.).
///
/// **Why loud, not silent.** The previous implementation
/// (`NoopBridge`) returned `Ok(vec![])` from `process`, which made
/// migrations appear to succeed on the target even though every
/// event the daemon received was silently dropped. Operators saw
/// "migration completed" alongside vanishing event throughput and
/// no diagnostic. This bridge instead:
///
/// - Returns `CoreDaemonError::RestoreFailed` from `restore`, so
///   the migration's restore phase fails visibly with a typed
///   error carrying the underlying reason.
/// - Returns `CoreDaemonError::ProcessFailed` from every `process`
///   call, so stateless migrations (where `restore` isn't
///   exercised) still surface a failure on the first event.
///
/// Mirrors the same fix applied to the NAPI bindings — see
/// `bindings/node/src/compute.rs` for the Node-side version.
struct ReconstructionErrorBridge {
    name: String,
    reason: String,
}

impl ReconstructionErrorBridge {
    fn new(name: String, reason: impl Into<String>) -> Self {
        Self {
            name,
            reason: reason.into(),
        }
    }

    fn err_msg(&self, op: &str) -> String {
        format!(
            "reconstruction failed for daemon kind '{}' on {}: {}",
            self.name, op, self.reason,
        )
    }
}

impl MeshDaemon for ReconstructionErrorBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    fn process(
        &mut self,
        _event: &CausalEvent,
    ) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        Err(CoreDaemonError::ProcessFailed(self.err_msg("process")))
    }

    fn restore(&mut self, _state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        Err(CoreDaemonError::RestoreFailed(self.err_msg("restore")))
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: migration-target reconstruction fallback must fail
    // loudly, never silently. Before this fix the PyO3 layer installed
    // a `NoopBridge` whose `process` returned `Ok(vec![])`, so a
    // migration could "succeed" on a target where the Python factory
    // was missing / raised / lacked `process` — the daemon would then
    // silently swallow every event with no diagnostic. The replacement
    // `ReconstructionErrorBridge` returns typed `RestoreFailed` from
    // `restore` and `ProcessFailed` from `process`, each carrying the
    // underlying reason, so the migration fails visibly at either the
    // restore phase or the first event.
    //
    // Testing the bridge directly (without a real Python interpreter
    // spawn / migration run) proves the contract at the bridge layer.
    // The Python-side integration test in `tests/test_compute.py`
    // covers the end-to-end path via registered factories.

    #[test]
    fn reconstruction_error_bridge_process_returns_typed_error() {
        let mut bridge = ReconstructionErrorBridge::new(
            "echo".to_string(),
            "Python factory raised: AttributeError: 'NoneType' object has no attribute 'process'",
        );
        let event = CausalEvent {
            link: ::net::adapter::net::state::causal::CausalLink {
                origin_hash: 0xdead_beef,
                horizon_encoded: 0,
                sequence: 1,
                parent_hash: 0,
            },
            payload: bytes::Bytes::from_static(b"x"),
            received_at: 0,
        };

        let result = bridge.process(&event);
        match result {
            Err(CoreDaemonError::ProcessFailed(msg)) => {
                assert!(msg.contains("echo"), "missing kind: {msg}");
                assert!(
                    msg.contains("AttributeError"),
                    "missing underlying reason: {msg}",
                );
                assert!(msg.contains("process"), "missing op label: {msg}");
            }
            Err(other) => panic!("expected ProcessFailed, got {other:?}"),
            Ok(outputs) => panic!(
                "silent-noop regression: ReconstructionErrorBridge must never return Ok; got {} outputs",
                outputs.len(),
            ),
        }
    }

    #[test]
    fn reconstruction_error_bridge_restore_returns_typed_error() {
        let mut bridge = ReconstructionErrorBridge::new(
            "counter".to_string(),
            "no factory registered for this kind",
        );
        let state = bytes::Bytes::from_static(&[0u8; 16]);

        let result = bridge.restore(state);
        match result {
            Err(CoreDaemonError::RestoreFailed(msg)) => {
                assert!(msg.contains("counter"), "missing kind: {msg}");
                assert!(
                    msg.contains("no factory registered"),
                    "missing underlying reason: {msg}",
                );
                assert!(msg.contains("restore"), "missing op label: {msg}");
            }
            Err(other) => panic!("expected RestoreFailed, got {other:?}"),
            Ok(()) => panic!(
                "silent-noop regression: ReconstructionErrorBridge::restore must never return Ok",
            ),
        }
    }
}
