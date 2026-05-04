//! PyO3 surface for the groups feature — `ReplicaGroup` /
//! `ForkGroup` / `StandbyGroup`. Stage 3 of
//! `SDK_GROUPS_SURFACE_PLAN.md`.
//!
//! Each group takes a `DaemonRuntime` + a previously-registered
//! factory kind; the SDK-side group wrappers reach into the
//! runtime's factory map and re-invoke the same `Py<PyAny>`-backed
//! factory used for migration-target reconstruction. No new
//! dispatcher infrastructure is needed here.
//!
//! # Thread model
//!
//! Unlike the Node binding — where the NAPI methods must be async
//! to avoid TSFN deadlocks on the main thread — PyO3's
//! `Python::attach` from any tokio worker is safe. The Python
//! side's factory round-trip is inline (no cross-thread channel),
//! so `scale_to` / `on_node_failure` / `promote` stay synchronous
//! on the Python side. The only async boundary is the shared
//! tokio runtime the bindings own.
//!
//! The methods above can each block on internal locks for
//! milliseconds-to-seconds (scheduler placement, registry I/O,
//! snapshot serialization). They take a `py: Python<'_>` and call
//! `py.detach(|| self.inner.<method>(...))` so the GIL is
//! released during the blocking work. Daemon factory callbacks
//! re-acquire the GIL via `Python::attach` (see compute.rs:963 et
//! seq.), so this is safe.
//!
//! # Error prefix
//!
//! Compute errors use `daemon: <msg>`; migration errors use
//! `daemon: migration: <kind>[: detail]`; group errors use
//! `daemon: group: <kind>[: detail]`. Python callers catch
//! `GroupError` (subclass of `DaemonError`) and parse the kind
//! via the `group_error_kind(exc)` helper.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use net::adapter::net::behavior::loadbalance::{RequestContext as CoreRequestContext, Strategy};
use net::adapter::net::compute::DaemonHostConfig;

use net_sdk::groups::{
    ForkGroup as SdkForkGroup, ForkGroupConfig as SdkForkGroupConfig, ForkRecord as SdkForkRecord,
    GroupError as SdkGroupError, GroupHealth as SdkGroupHealth, MemberInfo as SdkMemberInfo,
    MemberRole as SdkMemberRole, ReplicaGroup as SdkReplicaGroup,
    ReplicaGroupConfig as SdkReplicaGroupConfig, StandbyGroup as SdkStandbyGroup,
    StandbyGroupConfig as SdkStandbyGroupConfig,
};

use crate::compute::{daemon_err, DaemonError, PyDaemonRuntime};

// =========================================================================
// GroupError exception class — subclass of DaemonError
// =========================================================================

pyo3::create_exception!(
    _net,
    GroupError,
    DaemonError,
    "HA / scaling group failure. Message has the form \
     `daemon: group: <kind>[: <detail>]`, where `<kind>` is one \
     of `not-ready` | `factory-not-found` | `no-healthy-member` | \
     `placement-failed` | `registry-failed` | `invalid-config` | \
     `daemon`. Use the `net.group_error_kind` helper to extract \
     the kind programmatically."
);

fn group_err_str(e: &SdkGroupError) -> String {
    match e {
        SdkGroupError::NotReady => "not-ready".to_string(),
        SdkGroupError::FactoryNotFound(kind) => format!("factory-not-found: {kind}"),
        SdkGroupError::Daemon(d) => format!("daemon: {d}"),
        SdkGroupError::Core(core) => core_group_err_str(core),
    }
}

fn core_group_err_str(e: &net::adapter::net::compute::GroupError) -> String {
    use net::adapter::net::compute::GroupError as C;
    match e {
        C::NoHealthyMember => "no-healthy-member".to_string(),
        C::PlacementFailed(msg) => format!("placement-failed: {msg}"),
        C::RegistryFailed(msg) => format!("registry-failed: {msg}"),
        C::InvalidConfig(msg) => format!("invalid-config: {msg}"),
    }
}

fn group_err(e: SdkGroupError) -> PyErr {
    PyErr::new::<GroupError, _>(format!("daemon: group: {}", group_err_str(&e)))
}

// =========================================================================
// Config parsing helpers
// =========================================================================

fn parse_strategy(s: &str) -> PyResult<Strategy> {
    match s {
        "round-robin" => Ok(Strategy::RoundRobin),
        "consistent-hash" => Ok(Strategy::ConsistentHash),
        "least-load" => Ok(Strategy::LeastLoad),
        "least-connections" => Ok(Strategy::LeastConnections),
        "random" => Ok(Strategy::Random),
        other => Err(daemon_err(format!(
            "group: invalid-config: unknown lb strategy '{other}'"
        ))),
    }
}

fn parse_seed(bytes: &[u8]) -> PyResult<[u8; 32]> {
    if bytes.len() != 32 {
        return Err(daemon_err(format!(
            "group: invalid-config: group_seed must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(bytes);
    Ok(seed)
}

fn host_config_from_dict(d: Option<&Bound<'_, PyDict>>) -> PyResult<DaemonHostConfig> {
    let mut cfg = DaemonHostConfig::default();
    let Some(d) = d else {
        return Ok(cfg);
    };
    if let Some(v) = d.get_item("auto_snapshot_interval")? {
        cfg.auto_snapshot_interval = v.extract()?;
    }
    if let Some(v) = d.get_item("max_log_entries")? {
        cfg.max_log_entries = v.extract()?;
    }
    Ok(cfg)
}

fn request_context_from_dict(d: Option<&Bound<'_, PyDict>>) -> PyResult<CoreRequestContext> {
    let mut rc = CoreRequestContext::new();
    let Some(d) = d else {
        return Ok(rc);
    };
    if let Some(v) = d.get_item("routing_key")? {
        rc = rc.with_routing_key(v.extract::<String>()?);
    }
    if let Some(v) = d.get_item("session_id")? {
        rc = rc.with_session(v.extract::<String>()?);
    }
    if let Some(v) = d.get_item("request_id")? {
        rc.request_id = Some(v.extract::<String>()?);
    }
    Ok(rc)
}

// =========================================================================
// Conversion — member info / health / fork record → Python dicts
// =========================================================================

fn health_to_dict<'py>(py: Python<'py>, h: SdkGroupHealth) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match h {
        SdkGroupHealth::Healthy => {
            d.set_item("status", "healthy")?;
        }
        SdkGroupHealth::Degraded { healthy, total } => {
            d.set_item("status", "degraded")?;
            d.set_item("healthy", healthy)?;
            d.set_item("total", total)?;
        }
        SdkGroupHealth::Dead => {
            d.set_item("status", "dead")?;
        }
    }
    Ok(d)
}

fn member_to_dict<'py>(py: Python<'py>, m: &SdkMemberInfo) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("index", m.index)?;
    d.set_item("origin_hash", m.origin_hash)?;
    d.set_item("node_id", m.node_id)?;
    d.set_item(
        "entity_id",
        pyo3::types::PyBytes::new(py, &m.entity_id_bytes),
    )?;
    d.set_item("healthy", m.healthy)?;
    Ok(d)
}

fn fork_record_to_dict<'py>(py: Python<'py>, r: &SdkForkRecord) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("original_origin", r.original_origin)?;
    d.set_item("forked_origin", r.forked_origin)?;
    d.set_item("fork_seq", r.fork_seq)?;
    d.set_item("from_snapshot_seq", r.from_snapshot_seq)?;
    Ok(d)
}

// =========================================================================
// PyReplicaGroup
// =========================================================================

/// N interchangeable copies of a daemon with deterministic
/// per-replica identity, load-balanced inbound routing, and
/// auto-replacement on node failure.
#[pyclass(name = "ReplicaGroup", module = "net._net")]
pub struct PyReplicaGroup {
    inner: Arc<SdkReplicaGroup>,
}

#[pymethods]
impl PyReplicaGroup {
    /// Spawn a replica group bound to `runtime`. `kind` must have
    /// been registered via `runtime.register_factory`.
    ///
    /// Args:
    ///     runtime: A started `DaemonRuntime`.
    ///     kind: The factory kind to materialize each replica.
    ///     replica_count: Desired number of replicas (≥ 1).
    ///     group_seed: 32-byte seed for deterministic keypair
    ///         derivation.
    ///     lb_strategy: One of `"round-robin"`,
    ///         `"consistent-hash"`, `"least-load"`,
    ///         `"least-connections"`, `"random"`.
    ///     host_config: Optional dict with keys
    ///         `auto_snapshot_interval`, `max_log_entries`.
    ///
    /// Raises:
    ///     GroupError: with `kind` one of `not-ready`,
    ///         `factory-not-found`, `placement-failed`,
    ///         `invalid-config`, `registry-failed`.
    #[staticmethod]
    #[pyo3(signature = (runtime, kind, replica_count, group_seed, lb_strategy, host_config=None))]
    fn spawn(
        runtime: &PyDaemonRuntime,
        kind: String,
        replica_count: u8,
        group_seed: &[u8],
        lb_strategy: &str,
        host_config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyReplicaGroup> {
        let cfg = SdkReplicaGroupConfig {
            replica_count,
            group_seed: parse_seed(group_seed)?,
            lb_strategy: parse_strategy(lb_strategy)?,
            host_config: host_config_from_dict(host_config)?,
        };
        let group = SdkReplicaGroup::spawn(runtime.sdk_runtime(), &kind, cfg).map_err(group_err)?;
        Ok(PyReplicaGroup {
            inner: Arc::new(group),
        })
    }

    /// Route to the best-available replica. Returns the target
    /// `origin_hash`; caller hands it to `runtime.deliver(...)`.
    #[pyo3(signature = (ctx=None))]
    fn route_event(&self, ctx: Option<&Bound<'_, PyDict>>) -> PyResult<u64> {
        let rc = request_context_from_dict(ctx)?;
        self.inner.route_event(&rc).map_err(group_err)
    }

    /// Resize the group to `n` replicas. The kind is fixed at
    /// spawn time and not accepted as a parameter — passing a
    /// different kind would silently grow the group with a
    /// different daemon implementation.
    ///
    /// Pre-fix this held the GIL across a scheduler
    /// placement + registry insert that can block on internal
    /// locks for milliseconds-to-seconds. A Python program with
    /// a watchdog thread or asyncio loop was frozen for the
    /// duration. Daemon factory callbacks re-acquire the GIL via
    /// `Python::attach` (compute.rs:963/984/1016/1145) so
    /// releasing here is safe.
    fn scale_to(&self, py: Python<'_>, n: u8) -> PyResult<()> {
        py.detach(|| self.inner.scale_to(n)).map_err(group_err)
    }

    /// Handle failure of a node hosting one or more replicas.
    /// Returns the indices of replicas that were re-spawned.
    /// Reuses the group's spawn kind.
    ///
    /// Same GIL-blocking concern as `scale_to` —
    /// re-spawning replicas can take seconds when the registry
    /// is contended.
    fn on_node_failure(&self, py: Python<'_>, failed_node_id: u64) -> PyResult<Vec<u8>> {
        py.detach(|| self.inner.on_node_failure(failed_node_id))
            .map_err(group_err)
    }

    fn on_node_recovery(&self, py: Python<'_>, recovered_node_id: u64) {
        py.detach(|| self.inner.on_node_recovery(recovered_node_id));
    }

    #[getter]
    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        health_to_dict(py, self.inner.health())
    }

    #[getter]
    fn group_id(&self) -> u32 {
        self.inner.group_id()
    }

    #[getter]
    fn replicas<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.inner
            .replicas()
            .iter()
            .map(|m| member_to_dict(py, m))
            .collect()
    }

    #[getter]
    fn replica_count(&self) -> u8 {
        self.inner.replica_count()
    }

    #[getter]
    fn healthy_count(&self) -> u8 {
        self.inner.healthy_count()
    }

    fn __repr__(&self) -> String {
        format!(
            "ReplicaGroup(group_id={:#x}, replicas={}, healthy={})",
            self.inner.group_id(),
            self.inner.replica_count(),
            self.inner.healthy_count()
        )
    }
}

// =========================================================================
// PyForkGroup
// =========================================================================

#[pyclass(name = "ForkGroup", module = "net._net")]
pub struct PyForkGroup {
    inner: Arc<SdkForkGroup>,
}

#[pymethods]
impl PyForkGroup {
    /// Fork N new daemons from `parent_origin` at `fork_seq`.
    #[staticmethod]
    #[pyo3(signature = (runtime, kind, parent_origin, fork_seq, fork_count, lb_strategy, host_config=None))]
    fn fork(
        runtime: &PyDaemonRuntime,
        kind: String,
        parent_origin: u64,
        fork_seq: u64,
        fork_count: u8,
        lb_strategy: &str,
        host_config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyForkGroup> {
        let cfg = SdkForkGroupConfig {
            fork_count,
            lb_strategy: parse_strategy(lb_strategy)?,
            host_config: host_config_from_dict(host_config)?,
        };
        let group = SdkForkGroup::fork(runtime.sdk_runtime(), &kind, parent_origin, fork_seq, cfg)
            .map_err(group_err)?;
        Ok(PyForkGroup {
            inner: Arc::new(group),
        })
    }

    #[pyo3(signature = (ctx=None))]
    fn route_event(&self, ctx: Option<&Bound<'_, PyDict>>) -> PyResult<u64> {
        let rc = request_context_from_dict(ctx)?;
        self.inner.route_event(&rc).map_err(group_err)
    }

    /// See `PyReplicaGroup::scale_to`; same fix applies
    /// to fork groups.
    fn scale_to(&self, py: Python<'_>, n: u8) -> PyResult<()> {
        py.detach(|| self.inner.scale_to(n)).map_err(group_err)
    }

    fn on_node_failure(&self, py: Python<'_>, failed_node_id: u64) -> PyResult<Vec<u8>> {
        py.detach(|| self.inner.on_node_failure(failed_node_id))
            .map_err(group_err)
    }

    fn on_node_recovery(&self, py: Python<'_>, recovered_node_id: u64) {
        py.detach(|| self.inner.on_node_recovery(recovered_node_id));
    }

    #[getter]
    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        health_to_dict(py, self.inner.health())
    }

    #[getter]
    fn parent_origin(&self) -> u64 {
        self.inner.parent_origin()
    }

    #[getter]
    fn fork_seq(&self) -> u64 {
        self.inner.fork_seq()
    }

    #[getter]
    fn fork_records<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.inner
            .fork_records()
            .iter()
            .map(|r| fork_record_to_dict(py, r))
            .collect()
    }

    fn verify_lineage(&self) -> bool {
        self.inner.verify_lineage()
    }

    #[getter]
    fn members<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.inner
            .members()
            .iter()
            .map(|m| member_to_dict(py, m))
            .collect()
    }

    #[getter]
    fn fork_count(&self) -> u8 {
        self.inner.fork_count()
    }

    #[getter]
    fn healthy_count(&self) -> u8 {
        self.inner.healthy_count()
    }

    fn __repr__(&self) -> String {
        format!(
            "ForkGroup(parent={:#x}, fork_seq={}, forks={}, healthy={})",
            self.inner.parent_origin(),
            self.inner.fork_seq(),
            self.inner.fork_count(),
            self.inner.healthy_count(),
        )
    }
}

// =========================================================================
// PyStandbyGroup
// =========================================================================

#[pyclass(name = "StandbyGroup", module = "net._net")]
pub struct PyStandbyGroup {
    inner: Arc<SdkStandbyGroup>,
}

#[pymethods]
impl PyStandbyGroup {
    /// Spawn a standby group. Member 0 starts as active; the rest
    /// start as standbys with no snapshot (`synced_through == 0`).
    #[staticmethod]
    #[pyo3(signature = (runtime, kind, member_count, group_seed, host_config=None))]
    fn spawn(
        runtime: &PyDaemonRuntime,
        kind: String,
        member_count: u8,
        group_seed: &[u8],
        host_config: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyStandbyGroup> {
        let cfg = SdkStandbyGroupConfig {
            member_count,
            group_seed: parse_seed(group_seed)?,
            host_config: host_config_from_dict(host_config)?,
        };
        let group = SdkStandbyGroup::spawn(runtime.sdk_runtime(), &kind, cfg).map_err(group_err)?;
        Ok(PyStandbyGroup {
            inner: Arc::new(group),
        })
    }

    #[getter]
    fn active_origin(&self) -> u64 {
        self.inner.active_origin()
    }

    /// Snapshot serialization can take significant time
    /// for large daemon states; release the GIL while it runs.
    fn sync_standbys(&self, py: Python<'_>) -> PyResult<u64> {
        py.detach(|| self.inner.sync_standbys()).map_err(group_err)
    }

    /// Promotion may run a snapshot-restore on the
    /// promoted standby; release the GIL.
    fn promote(&self, py: Python<'_>) -> PyResult<u64> {
        py.detach(|| self.inner.promote()).map_err(group_err)
    }

    fn on_node_failure(&self, py: Python<'_>, failed_node_id: u64) -> PyResult<Option<u64>> {
        py.detach(|| self.inner.on_node_failure(failed_node_id))
            .map_err(group_err)
    }

    fn on_node_recovery(&self, py: Python<'_>, recovered_node_id: u64) {
        py.detach(|| self.inner.on_node_recovery(recovered_node_id));
    }

    #[getter]
    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        health_to_dict(py, self.inner.health())
    }

    #[getter]
    fn active_healthy(&self) -> bool {
        self.inner.active_healthy()
    }

    #[getter]
    fn active_index(&self) -> u8 {
        self.inner.active_index()
    }

    /// `"active"` | `"standby"` | `None` (out-of-range index).
    fn member_role(&self, index: u8) -> Option<&'static str> {
        self.inner.member_role(index).map(member_role_str)
    }

    fn synced_through(&self, index: u8) -> Option<u64> {
        self.inner.synced_through(index)
    }

    #[getter]
    fn buffered_event_count(&self) -> usize {
        self.inner.buffered_event_count()
    }

    #[getter]
    fn group_id(&self) -> u32 {
        self.inner.group_id()
    }

    #[getter]
    fn members<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.inner
            .members()
            .iter()
            .map(|m| member_to_dict(py, m))
            .collect()
    }

    #[getter]
    fn member_count(&self) -> u8 {
        self.inner.member_count()
    }

    #[getter]
    fn standby_count(&self) -> u8 {
        self.inner.standby_count()
    }

    fn __repr__(&self) -> String {
        format!(
            "StandbyGroup(group_id={:#x}, active_index={}, members={}, buffered={})",
            self.inner.group_id(),
            self.inner.active_index(),
            self.inner.member_count(),
            self.inner.buffered_event_count(),
        )
    }
}

fn member_role_str(role: SdkMemberRole) -> &'static str {
    match role {
        SdkMemberRole::Active => "active",
        SdkMemberRole::Standby => "standby",
    }
}
