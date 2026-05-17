//! PyO3 surface for the MeshOS daemon-author SDK.
//!
//! Slice 1 of `MESHOS_SDK_PLAN.md` Phase 2: `start`, `register_daemon`,
//! `next_control` / `try_next_control`, `publish_log`,
//! `graceful_shutdown`, `metadata` / `refresh_metadata`, and the
//! `publish_capabilities` stub the substrate exposes as a no-op
//! today.
//!
//! # Error envelope
//!
//! Errors raise `MeshOsSdkError` whose message carries the substrate
//! `<<meshos-sdk-kind:KIND>>MSG` discriminator verbatim, plus a
//! `.kind` attribute for programmatic dispatch. The cross-binding
//! convention every language uses — `bindings/node/src/meshos.rs`
//! and `bindings/go/meshos-ffi` parse the same envelope.
//!
//! # Trait routing
//!
//! A Python `MeshOsDaemon` instance is held behind `PyDaemonBridge`;
//! every `MeshDaemon` method call acquires the GIL via
//! `Python::attach` (no cross-thread channel dance — pyo3 allows
//! reentry from any worker holding a valid `Py<PyAny>`). Hot loops
//! still pay the GIL-acquisition cost — match the compute binding's
//! caveat in [`compute.rs`].

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use tokio::runtime::Runtime;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::behavior::meshos::logs::LogLevel as CoreLogLevel;
use net::adapter::net::behavior::meshos::{
    LoggingDispatcher, MaintenanceMirrorSnapshot, MaintenanceStateView, MeshOsConfig,
    MeshOsDaemonHandle as CoreHandle, MeshOsDaemonSdk as CoreSdk, MetadataView, PeerHealthSnapshot,
    PeerSnapshot, SdkError, DEFAULT_GRACEFUL_SHUTDOWN,
};
use net::adapter::net::compute::{
    DaemonControl as CoreDaemonControl, DaemonError as CoreDaemonError, MeshDaemon,
};
use net::adapter::net::state::causal::CausalEvent;

// =========================================================================
// Exception class
// =========================================================================

pyo3::create_exception!(
    _net,
    MeshOsSdkError,
    PyException,
    "MeshOS daemon-author SDK error. The message carries the \
     substrate `<<meshos-sdk-kind:KIND>>MSG` envelope verbatim; \
     programmatic callers should read the `.kind` attribute rather \
     than parsing the message string."
);

/// Build a `MeshOsSdkError` from a (kind, message) pair, attaching
/// `.kind` and `.message` as Python attributes so callers can
/// branch on the discriminator without parsing the envelope.
/// Mirrors the `MeshDbError` pattern at `meshdb.rs:1422-1437`.
fn sdk_err(py: Python<'_>, kind: &str, message: &str) -> PyErr {
    let err = MeshOsSdkError::new_err(format!("<<meshos-sdk-kind:{kind}>>{message}"));
    let _ = err.value(py).setattr("kind", kind);
    let _ = err.value(py).setattr("message", message);
    err
}

fn sdk_err_from(py: Python<'_>, e: SdkError) -> PyErr {
    sdk_err(py, e.kind, &e.message)
}

// =========================================================================
// Helper: parse a Python log-level string
// =========================================================================

fn parse_log_level(level: &str) -> PyResult<CoreLogLevel> {
    Ok(match level {
        "trace" | "TRACE" | "Trace" => CoreLogLevel::Trace,
        "debug" | "DEBUG" | "Debug" => CoreLogLevel::Debug,
        "info" | "INFO" | "Info" => CoreLogLevel::Info,
        "warn" | "WARN" | "Warn" | "warning" | "WARNING" => CoreLogLevel::Warn,
        "error" | "ERROR" | "Error" => CoreLogLevel::Error,
        other => {
            return Python::attach(|py| {
                Err(sdk_err(
                    py,
                    "invalid_log_level",
                    &format!("log level must be one of trace|debug|info|warn|error; got {other:?}"),
                ))
            });
        }
    })
}

// =========================================================================
// Helpers: convert core enums → Python-friendly shapes
// =========================================================================

/// Render a `DaemonControl` variant as a Python dict with a stable
/// `kind` discriminator plus payload fields. Slice 1 uses dicts
/// rather than pyclasses — the cross-binding wire form is
/// `{kind: "...", ...}`. A typed-pyclass overlay (`DaemonControl`
/// enum-like) lands in slice 3 if a consumer asks for it.
fn daemon_control_to_dict<'py>(
    py: Python<'py>,
    ev: CoreDaemonControl,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match ev {
        CoreDaemonControl::Shutdown { grace_period_ms } => {
            d.set_item("kind", "Shutdown")?;
            d.set_item("grace_period_ms", grace_period_ms)?;
        }
        CoreDaemonControl::DrainStart { grace_period_ms } => {
            d.set_item("kind", "DrainStart")?;
            d.set_item("grace_period_ms", grace_period_ms)?;
        }
        CoreDaemonControl::DrainFinish => {
            d.set_item("kind", "DrainFinish")?;
        }
        CoreDaemonControl::BackpressureOn { level } => {
            d.set_item("kind", "BackpressureOn")?;
            d.set_item("level", level)?;
        }
        CoreDaemonControl::BackpressureOff => {
            d.set_item("kind", "BackpressureOff")?;
        }
        // `#[non_exhaustive]` on the substrate — bindings tolerate
        // unknown variants by passing the kind through as
        // "Unknown". Substrate-side additions don't break older
        // wrappers.
        _ => {
            d.set_item("kind", "Unknown")?;
        }
    }
    Ok(d)
}

/// Render a `MaintenanceStateView` as a Python dict carrying the
/// `kind` discriminator + relative-ms timestamps.
fn maintenance_state_to_dict<'py>(
    py: Python<'py>,
    state: &MaintenanceStateView,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match state {
        MaintenanceStateView::Active => {
            d.set_item("kind", "Active")?;
        }
        MaintenanceStateView::EnteringMaintenance {
            since_ms,
            deadline_remaining_ms,
        } => {
            d.set_item("kind", "EnteringMaintenance")?;
            d.set_item("since_ms", since_ms)?;
            d.set_item(
                "deadline_remaining_ms",
                deadline_remaining_ms.as_ref().copied(),
            )?;
        }
        MaintenanceStateView::Maintenance { since_ms } => {
            d.set_item("kind", "Maintenance")?;
            d.set_item("since_ms", since_ms)?;
        }
        MaintenanceStateView::ExitingMaintenance { since_ms } => {
            d.set_item("kind", "ExitingMaintenance")?;
            d.set_item("since_ms", since_ms)?;
        }
        MaintenanceStateView::DrainFailed { since_ms, reason } => {
            d.set_item("kind", "DrainFailed")?;
            d.set_item("since_ms", since_ms)?;
            d.set_item("reason", reason)?;
        }
        MaintenanceStateView::Recovery { since_ms } => {
            d.set_item("kind", "Recovery")?;
            d.set_item("since_ms", since_ms)?;
        }
        _ => {
            d.set_item("kind", "Unknown")?;
        }
    }
    Ok(d)
}

/// Render a `MetadataView` as a Python dict. Peers are emitted as
/// a dict keyed by node id, each value a full `PeerSnapshot`
/// projection (rtt, health, maintenance mirror, host metrics,
/// capabilities, software version). The cross-binding wire form is
/// `peers: {node_id: {rtt_ms, health, maintenance, ...}}`.
fn metadata_view_to_dict<'py>(
    py: Python<'py>,
    view: &MetadataView,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("node_id", view.node_id)?;
    d.set_item("daemon_id", view.daemon_id)?;
    d.set_item("daemon_name", view.daemon_name.clone())?;
    d.set_item(
        "maintenance_state",
        maintenance_state_to_dict(py, &view.maintenance_state)?,
    )?;
    let peers = PyDict::new(py);
    for (node_id, snap) in &view.peers {
        peers.set_item(*node_id, peer_snapshot_to_dict(py, snap)?)?;
    }
    d.set_item("peers", peers)?;
    Ok(d)
}

/// Render a `PeerSnapshot` as a Python dict. Health and maintenance
/// mirror enums are stringified to their variant names; the rest
/// of the fields are passed through as their native scalar types.
fn peer_snapshot_to_dict<'py>(
    py: Python<'py>,
    snap: &PeerSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("rtt_ms", snap.rtt_ms)?;
    d.set_item("health", snap.health.map(peer_health_str))?;
    d.set_item("maintenance", snap.maintenance.map(maintenance_mirror_str))?;
    d.set_item("cpu_load_1m", snap.cpu_load_1m)?;
    d.set_item("mem_used_bytes", snap.mem_used_bytes)?;
    d.set_item("mem_total_bytes", snap.mem_total_bytes)?;
    d.set_item("disk_used_bytes", snap.disk_used_bytes)?;
    d.set_item("disk_total_bytes", snap.disk_total_bytes)?;
    d.set_item("saturation_trend", snap.saturation_trend)?;
    let caps: Vec<String> = snap.capability_set.iter().cloned().collect();
    d.set_item("capability_set", caps)?;
    d.set_item("software_version", snap.software_version.clone())?;
    d.set_item("forked_from", snap.forked_from)?;
    Ok(d)
}

fn peer_health_str(h: PeerHealthSnapshot) -> &'static str {
    match h {
        PeerHealthSnapshot::Healthy => "Healthy",
        PeerHealthSnapshot::Degraded => "Degraded",
        PeerHealthSnapshot::Unreachable => "Unreachable",
        _ => "Unknown",
    }
}

fn maintenance_mirror_str(m: MaintenanceMirrorSnapshot) -> &'static str {
    match m {
        MaintenanceMirrorSnapshot::Active => "Active",
        MaintenanceMirrorSnapshot::EnteringMaintenance => "EnteringMaintenance",
        MaintenanceMirrorSnapshot::Maintenance => "Maintenance",
        MaintenanceMirrorSnapshot::ExitingMaintenance => "ExitingMaintenance",
        MaintenanceMirrorSnapshot::DrainFailed => "DrainFailed",
        MaintenanceMirrorSnapshot::Recovery => "Recovery",
        _ => "Unknown",
    }
}

// =========================================================================
// MeshOsConfig parsing — accept a dict, mirror compute.rs's
// `daemon_host_config_from_dict` pattern.
// =========================================================================

pub(crate) fn meshos_config_from_dict(
    py: Python<'_>,
    config: Option<&Bound<'_, PyDict>>,
) -> PyResult<MeshOsConfig> {
    let mut cfg = MeshOsConfig::default();
    let Some(d) = config else {
        return Ok(cfg);
    };
    if let Some(v) = d.get_item("this_node")? {
        cfg.this_node = v
            .extract::<u64>()
            .map_err(|e| sdk_err(py, "invalid_config", &format!("this_node must be int: {e}")))?;
    }
    if let Some(v) = d.get_item("tick_interval_ms")? {
        let ms: u64 = v.extract().map_err(|e| {
            sdk_err(
                py,
                "invalid_config",
                &format!("tick_interval_ms must be int: {e}"),
            )
        })?;
        cfg.tick_interval = Duration::from_millis(ms);
    }
    if let Some(v) = d.get_item("event_queue_capacity")? {
        cfg.event_queue_capacity = v.extract().map_err(|e| {
            sdk_err(
                py,
                "invalid_config",
                &format!("event_queue_capacity must be int: {e}"),
            )
        })?;
    }
    if let Some(v) = d.get_item("action_queue_capacity")? {
        cfg.action_queue_capacity = v.extract().map_err(|e| {
            sdk_err(
                py,
                "invalid_config",
                &format!("action_queue_capacity must be int: {e}"),
            )
        })?;
    }
    Ok(cfg)
}

// =========================================================================
// PyDaemonBridge — MeshDaemon impl driven by a Python object
// =========================================================================

/// Bridge wrapping a Python `MeshOsDaemon` object. Holds `Py<PyAny>`
/// for the instance + its callable attributes; every `MeshDaemon`
/// method acquires the GIL via `Python::attach` and dispatches
/// inline.
///
/// Unlike the compute binding's `PyDaemonBridge`, this one stays
/// internal to the MeshOS module — the MeshOS SDK's `register_daemon`
/// takes a `Box<dyn MeshDaemon>` directly without a kind-factory
/// indirection (no migration-target reconstruction at this layer).
struct PyDaemonBridge {
    name: String,
    /// Cached `process` callable. Required. Bound-method callables
    /// keep the daemon instance alive transitively, so we don't
    /// need to hold a separate `Py<PyAny>` for the instance.
    process: Py<PyAny>,
    /// Cached optional callables. `None` when the attribute is
    /// absent or evaluates to `None`.
    snapshot: Option<Py<PyAny>>,
    restore: Option<Py<PyAny>>,
    on_control: Option<Py<PyAny>>,
    health: Option<Py<PyAny>>,
    saturation: Option<Py<PyAny>>,
    /// Capability advertisement snapshotted at construction.
    /// The substrate calls `required_capabilities` /
    /// `optional_capabilities` from non-GIL contexts; resolving
    /// once at construction avoids a `Python::attach` per call
    /// and keeps the trait surface infallible (substrate-side
    /// errors here would silently degrade to `default()`). Daemon
    /// authors who need lifetime-dynamic capabilities can call
    /// `handle.publish_capabilities(set)` after a state change.
    required_capabilities: CapabilitySet,
    optional_capabilities: CapabilitySet,
}

impl PyDaemonBridge {
    /// Build a bridge from a live Python daemon instance. Resolves
    /// the required `process` callable; caches the optional ones.
    fn from_instance(py: Python<'_>, instance: Py<PyAny>) -> PyResult<Self> {
        // `name` is required — read once at construction so the
        // `MeshDaemon::name(&self) -> &str` impl can return a
        // borrowed slice without acquiring the GIL on every call.
        let name = match instance.bind(py).getattr("name") {
            Ok(attr) => {
                if attr.is_callable() {
                    attr.call0()?.extract::<String>()?
                } else {
                    attr.extract::<String>()?
                }
            }
            Err(e) => {
                return Err(sdk_err(
                    py,
                    "invalid_daemon",
                    &format!("daemon has no `name` attribute: {e}"),
                ));
            }
        };

        let process = instance.getattr(py, "process").map_err(|e| {
            sdk_err(
                py,
                "invalid_daemon",
                &format!("daemon has no `process` method: {e}"),
            )
        })?;

        let snapshot = optional_callable(py, &instance, "snapshot");
        let restore = optional_callable(py, &instance, "restore");
        let on_control = optional_callable(py, &instance, "on_control");
        let health = optional_callable(py, &instance, "health");
        let saturation = optional_callable(py, &instance, "saturation");

        let required_capabilities = resolve_capabilities(py, &instance, "required_capabilities")?;
        let optional_capabilities = resolve_capabilities(py, &instance, "optional_capabilities")?;

        // `instance` is consumed only to read attributes; the
        // bound-method callables hold strong refs back to it so
        // the daemon stays alive as long as the bridge does.
        let _ = instance;
        Ok(Self {
            name,
            process,
            snapshot,
            restore,
            on_control,
            health,
            saturation,
            required_capabilities,
            optional_capabilities,
        })
    }
}

/// Resolve the daemon's capability advertisement for the given
/// attribute (`"required_capabilities"` or
/// `"optional_capabilities"`). Accepts:
///
/// - `None` / attribute missing → empty `CapabilitySet`.
/// - A list/tuple of tag strings → each tag added via
///   `CapabilitySet::add_tag`.
/// - A callable returning either of the above → called with no
///   args, result resolved the same way.
///
/// Invalid shapes raise `MeshOsSdkError(kind="invalid_daemon")`.
fn resolve_capabilities(
    py: Python<'_>,
    instance: &Py<PyAny>,
    attr: &str,
) -> PyResult<CapabilitySet> {
    let bound = match instance.bind(py).getattr(attr) {
        Ok(v) if !v.is_none() => v,
        _ => return Ok(CapabilitySet::default()),
    };
    let resolved = if bound.is_callable() {
        bound.call0()?
    } else {
        bound
    };
    if resolved.is_none() {
        return Ok(CapabilitySet::default());
    }
    let tags: Vec<String> = resolved.extract().map_err(|e| {
        sdk_err(
            py,
            "invalid_daemon",
            &format!("{attr} must return a list/tuple of tag strings (or None): {e}",),
        )
    })?;
    let mut set = CapabilitySet::new();
    for tag in tags {
        set = set.add_tag(tag);
    }
    Ok(set)
}

/// Fetch an attribute that may be missing or `None`; only return
/// `Some` when the attribute exists and is non-None.
fn optional_callable(py: Python<'_>, instance: &Py<PyAny>, attr: &str) -> Option<Py<PyAny>> {
    match instance.getattr(py, attr) {
        Ok(v) if !v.is_none(py) => Some(v),
        _ => None,
    }
}

impl MeshDaemon for PyDaemonBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        // `requirements()` is a placement filter — what *kind*
        // of node the daemon needs. Distinct from the daemon's
        // own capability advertisement (`required_capabilities`
        // / `optional_capabilities`). The Python surface exposes
        // only the advertise side today; placement-filter
        // routing remains substrate-internal until a consumer
        // workflow needs it.
        CapabilityFilter::default()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        self.required_capabilities.clone()
    }

    fn optional_capabilities(&self) -> CapabilitySet {
        self.optional_capabilities.clone()
    }

    fn process(&mut self, event: &CausalEvent) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        let origin_hash = event.link.origin_hash;
        let sequence = event.link.sequence;
        let payload = event.payload.clone();

        Python::attach(|py| -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
            // Slice 1: hand the daemon a dict matching the
            // cross-binding wire form. A typed `CausalEvent`
            // pyclass overlay can re-use the compute binding's
            // `PyCausalEvent` if the user opts in — slice 2.
            let ev = PyDict::new(py);
            ev.set_item("origin_hash", origin_hash)
                .map_err(|e| CoreDaemonError::ProcessFailed(format!("build event dict: {e}")))?;
            ev.set_item("sequence", sequence)
                .map_err(|e| CoreDaemonError::ProcessFailed(format!("build event dict: {e}")))?;
            ev.set_item("payload", PyBytes::new(py, payload.as_ref()))
                .map_err(|e| CoreDaemonError::ProcessFailed(format!("build event dict: {e}")))?;

            let args = PyTuple::new(py, [ev.into_any()])
                .map_err(|e| CoreDaemonError::ProcessFailed(format!("build args: {e}")))?;
            let result = self
                .process
                .call1(py, args)
                .map_err(|e| CoreDaemonError::ProcessFailed(format!("process raised: {e}")))?;

            let bound = result.into_bound(py);
            if bound.is_none() {
                return Ok(Vec::new());
            }

            let iter = bound.try_iter().map_err(|e| {
                CoreDaemonError::ProcessFailed(format!(
                    "process must return None or an iterable of bytes; got {e}"
                ))
            })?;
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
        })
    }

    fn snapshot(&self) -> Option<Bytes> {
        let cb = self.snapshot.as_ref()?;
        Python::attach(|py| -> Option<Bytes> {
            match cb.call0(py) {
                Ok(ret) => {
                    let bound = ret.into_bound(py);
                    if bound.is_none() {
                        None
                    } else {
                        match bound.extract::<Vec<u8>>() {
                            Ok(v) => Some(Bytes::from(v)),
                            Err(e) => {
                                eprintln!(
                                    "MeshOS daemon snapshot: return value is not bytes: {e}; treating as None"
                                );
                                None
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("MeshOS daemon snapshot raised: {e}; treating as None");
                    None
                }
            }
        })
    }

    fn restore(&mut self, state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        let Some(cb) = self.restore.as_ref() else {
            return Ok(());
        };
        Python::attach(|py| -> std::result::Result<(), CoreDaemonError> {
            let arg = PyBytes::new(py, state.as_ref());
            let args = PyTuple::new(py, [arg.into_any()])
                .map_err(|e| CoreDaemonError::RestoreFailed(format!("build args: {e}")))?;
            cb.call1(py, args)
                .map_err(|e| CoreDaemonError::RestoreFailed(format!("restore raised: {e}")))?;
            Ok(())
        })
    }

    fn health(&self) -> net::adapter::net::compute::DaemonHealth {
        use net::adapter::net::compute::DaemonHealth as H;
        let Some(cb) = self.health.as_ref() else {
            return H::Healthy;
        };
        Python::attach(|py| -> H {
            match cb.call0(py) {
                Ok(ret) => {
                    let bound = ret.into_bound(py);
                    if bound.is_none() {
                        return H::Healthy;
                    }
                    // Slice 1 wire form: a string "healthy" /
                    // "degraded" or a dict {"kind": "degraded",
                    // "reason": "..."}.
                    if let Ok(s) = bound.extract::<String>() {
                        return match s.as_str() {
                            "healthy" | "Healthy" => H::Healthy,
                            "degraded" | "Degraded" => H::Degraded {
                                reason: String::new(),
                            },
                            "unhealthy" | "Unhealthy" => H::Unhealthy,
                            _ => H::Healthy,
                        };
                    }
                    if let Ok(d) = bound.cast::<PyDict>() {
                        let kind: String = d
                            .get_item("kind")
                            .ok()
                            .flatten()
                            .and_then(|v| v.extract().ok())
                            .unwrap_or_else(|| "healthy".to_string());
                        let reason: String = d
                            .get_item("reason")
                            .ok()
                            .flatten()
                            .and_then(|v| v.extract().ok())
                            .unwrap_or_default();
                        return match kind.as_str() {
                            "healthy" | "Healthy" => H::Healthy,
                            "degraded" | "Degraded" => H::Degraded { reason },
                            "unhealthy" | "Unhealthy" => H::Unhealthy,
                            _ => H::Healthy,
                        };
                    }
                    H::Healthy
                }
                Err(e) => {
                    eprintln!("MeshOS daemon health() raised: {e}; treating as Healthy");
                    H::Healthy
                }
            }
        })
    }

    fn saturation(&self) -> f32 {
        let Some(cb) = self.saturation.as_ref() else {
            return 0.0;
        };
        Python::attach(|py| -> f32 {
            match cb.call0(py) {
                Ok(ret) => ret.extract::<f32>(py).unwrap_or(0.0).clamp(0.0, 1.0),
                Err(e) => {
                    eprintln!("MeshOS daemon saturation() raised: {e}; treating as 0.0");
                    0.0
                }
            }
        })
    }

    fn on_control(&mut self, event: CoreDaemonControl) {
        // `on_control` on the trait fires when the supervisor
        // routes a daemon-targeted action. The SDK ALSO delivers
        // the same event over `next_control`; the user picks one
        // delivery model. Calling both is intentional — the
        // pyclass handle is single-consumer (one next_control loop)
        // and the trait callback fires from the executor.
        let Some(cb) = self.on_control.as_ref() else {
            return;
        };
        Python::attach(|py| {
            let dict = match daemon_control_to_dict(py, event) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("MeshOS daemon on_control: failed to build event dict: {e}");
                    return;
                }
            };
            let args = match PyTuple::new(py, [dict.into_any()]) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("MeshOS daemon on_control: failed to build args: {e}");
                    return;
                }
            };
            if let Err(e) = cb.call1(py, args) {
                eprintln!("MeshOS daemon on_control raised: {e}; ignoring");
            }
        });
    }
}

// =========================================================================
// PyMeshOsDaemonHandle — wraps `MeshOsDaemonHandle`
// =========================================================================

/// Handle to a registered daemon. Owns the control-event channel
/// + the publish-log surface + graceful shutdown.
///
/// Dropping the handle without calling `graceful_shutdown` still
/// unregisters cleanly via the Rust-side `Drop` — the daemon
/// leaves the registry; pending control events drop. `graceful_shutdown`
/// is the explicit drain path that fires `Shutdown { grace_period_ms }`
/// first.
#[pyclass(name = "MeshOsDaemonHandle", module = "net._net")]
pub struct PyMeshOsDaemonHandle {
    /// `Option` because `graceful_shutdown` consumes the handle
    /// by value — after a successful shutdown the pyclass holds
    /// `None` and every subsequent method raises `already_shutdown`.
    inner: Option<CoreHandle>,
    runtime: Arc<Runtime>,
    /// Cached identity so `daemon_id`/`daemon_name` work after
    /// shutdown (when `inner` is `None`).
    daemon_id: u64,
    daemon_name: String,
}

impl PyMeshOsDaemonHandle {
    fn require_inner_mut(&mut self) -> PyResult<&mut CoreHandle> {
        if let Some(h) = self.inner.as_mut() {
            Ok(h)
        } else {
            Python::attach(|py| {
                Err(sdk_err(
                    py,
                    "already_shutdown",
                    "daemon handle was already consumed by graceful_shutdown",
                ))
            })
        }
    }

    fn require_inner(&self) -> PyResult<&CoreHandle> {
        if let Some(h) = self.inner.as_ref() {
            Ok(h)
        } else {
            Python::attach(|py| {
                Err(sdk_err(
                    py,
                    "already_shutdown",
                    "daemon handle was already consumed by graceful_shutdown",
                ))
            })
        }
    }
}

#[pymethods]
impl PyMeshOsDaemonHandle {
    /// Substrate identifier (the keypair's origin hash). Stable
    /// across the handle's lifetime, including after shutdown.
    #[getter]
    fn daemon_id(&self) -> u64 {
        self.daemon_id
    }

    /// Daemon's `name` at registration. Stable across the handle's
    /// lifetime, including after shutdown.
    #[getter]
    fn daemon_name(&self) -> String {
        self.daemon_name.clone()
    }

    /// Return the cached metadata view as a dict. Refresh via
    /// `refresh_metadata()` for fresh peer counts / maintenance
    /// state.
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = self.require_inner()?;
        metadata_view_to_dict(py, inner.metadata())
    }

    /// Rebuild the metadata view from the runtime's latest
    /// snapshot. Cheap.
    fn refresh_metadata<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = self.require_inner_mut()?;
        let view = inner.refresh_metadata().clone();
        metadata_view_to_dict(py, &view)
    }

    /// Block until the next control event arrives, or `timeout_ms`
    /// elapses, or the runtime shuts down. Returns the event as a
    /// dict `{kind: "...", ...}` on success, or `None` on
    /// timeout / runtime shutdown.
    #[pyo3(signature = (timeout_ms=None))]
    fn next_control<'py>(
        &mut self,
        py: Python<'py>,
        timeout_ms: Option<u64>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        // Pull the channel receiver out across the GIL release.
        // `require_inner_mut` errors out before we get here if the
        // handle was consumed by `graceful_shutdown`.
        let runtime = self.runtime.clone();
        let result: Option<CoreDaemonControl> = {
            let handle = self.require_inner_mut()?;
            // Borrow the handle across the awaited recv. Use
            // `py.detach` so other Python threads can run while we
            // park.
            py.detach(|| {
                runtime.block_on(async {
                    let next: Option<CoreDaemonControl> = match timeout_ms {
                        Some(ms) => {
                            tokio::time::timeout(Duration::from_millis(ms), handle.next_control())
                                .await
                                .unwrap_or_default()
                        }
                        None => handle.next_control().await,
                    };
                    next
                })
            })
        };
        match result {
            Some(ev) => Ok(Some(daemon_control_to_dict(py, ev)?)),
            None => Ok(None),
        }
    }

    /// Non-blocking control-event receive. Returns the next event
    /// as a dict, or `None` if the channel is empty / closed.
    fn try_next_control<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let inner = self.require_inner_mut()?;
        match inner.try_next_control() {
            Some(ev) => Ok(Some(daemon_control_to_dict(py, ev)?)),
            None => Ok(None),
        }
    }

    /// Publish a log line tagged with this daemon's id. Non-blocking;
    /// raises `MeshOsSdkError` with kind `queue_full` or `loop_closed`
    /// when the substrate's log ring is saturated or the loop has
    /// exited.
    ///
    /// `level` is one of `"trace" | "debug" | "info" | "warn" | "error"`
    /// (case-insensitive).
    fn publish_log(&self, py: Python<'_>, level: &str, message: &str) -> PyResult<()> {
        let lvl = parse_log_level(level)?;
        let inner = self.require_inner()?;
        inner
            .publish_log(lvl, message)
            .map_err(|e| sdk_err_from(py, e))
    }

    /// Publish (or update) the daemon's capability set.
    ///
    /// `caps` is a dict matching the cross-language capability
    /// shape from `bindings/python/src/capabilities.rs` —
    /// `{hardware?, software?, models?, tools?, tags?, limits?, metadata?}`.
    /// `None` clears to the empty default.
    ///
    /// The substrate-side commit is a stub today (returns `Ok(())`
    /// without committing); the conversion still runs so a
    /// malformed dict surfaces a typed error immediately rather
    /// than silently lost when the chain commit lands.
    #[pyo3(signature = (caps=None))]
    fn publish_capabilities(
        &self,
        py: Python<'_>,
        caps: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let inner = self.require_inner()?;
        let cap_set = match caps {
            Some(d) => crate::capabilities::capability_set_from_py(d)
                .map_err(|e| sdk_err(py, "invalid_capabilities", &format!("{e}")))?,
            None => CapabilitySet::default(),
        };
        inner
            .publish_capabilities(cap_set)
            .map_err(|e| sdk_err_from(py, e))
    }

    /// Drive a graceful shutdown. Sends
    /// `Shutdown { grace_period_ms }` on the daemon's control
    /// channel, parks for `grace_ms`, then unregisters. Consumes
    /// the handle — subsequent method calls raise
    /// `MeshOsSdkError(kind="already_shutdown")`.
    #[pyo3(signature = (grace_ms=None))]
    fn graceful_shutdown(&mut self, py: Python<'_>, grace_ms: Option<u64>) -> PyResult<()> {
        let handle = match self.inner.take() {
            Some(h) => h,
            None => {
                return Err(sdk_err(
                    py,
                    "already_shutdown",
                    "daemon handle was already consumed by graceful_shutdown",
                ));
            }
        };
        let grace = grace_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_GRACEFUL_SHUTDOWN);
        let runtime = self.runtime.clone();
        py.detach(move || {
            runtime
                .block_on(async move { handle.graceful_shutdown(grace).await })
                .map_err(|e| Python::attach(|py| sdk_err_from(py, e)))
        })
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_value=None, _exc_traceback=None))]
    fn __exit__(
        &mut self,
        py: Python<'_>,
        _exc_type: Option<Py<PyAny>>,
        _exc_value: Option<Py<PyAny>>,
        _exc_traceback: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        if self.inner.is_some() {
            self.graceful_shutdown(py, None)?;
        }
        Ok(false)
    }

    fn __repr__(&self) -> String {
        format!(
            "MeshOsDaemonHandle(daemon_id={:#x}, name={:?}, active={})",
            self.daemon_id,
            self.daemon_name,
            self.inner.is_some(),
        )
    }
}

// =========================================================================
// PyMeshOsDaemonSdk — wraps `MeshOsDaemonSdk`
// =========================================================================

/// Daemon-author entry point. Construct via
/// `MeshOsDaemonSdk.start(config=None)`; register daemons via
/// `sdk.register_daemon(daemon, identity)`; tear down with
/// `sdk.shutdown()`.
///
/// Slice 1 always uses the substrate's `LoggingDispatcher` —
/// the SDK records dispatched actions in memory. A custom
/// dispatcher trait crossing the FFI lands in slice 3.
#[pyclass(name = "MeshOsDaemonSdk", module = "net._net")]
pub struct PyMeshOsDaemonSdk {
    /// `Option` because `shutdown` consumes the inner SDK by value.
    /// After a successful shutdown the pyclass holds `None` and
    /// every subsequent method raises `already_shutdown`.
    inner: Option<CoreSdk>,
    runtime: Arc<Runtime>,
}

impl PyMeshOsDaemonSdk {
    fn require_inner(&self) -> PyResult<&CoreSdk> {
        if let Some(s) = self.inner.as_ref() {
            Ok(s)
        } else {
            Python::attach(|py| {
                Err(sdk_err(
                    py,
                    "already_shutdown",
                    "MeshOsDaemonSdk was already consumed by shutdown",
                ))
            })
        }
    }

    /// Borrow the tokio runtime shared with the substrate SDK.
    /// Used by sibling modules (currently: the Deck SDK) that
    /// need to drive `block_on` on the same scheduler the
    /// supervisor runs on. Returns `None` when the SDK has been
    /// consumed by `shutdown()`.
    #[cfg(feature = "deck")]
    pub(crate) fn runtime_clone(&self) -> Option<Arc<Runtime>> {
        if self.inner.is_some() {
            Some(self.runtime.clone())
        } else {
            None
        }
    }

    /// Run a closure with a borrow of the inner `CoreSdk`. Returns
    /// `None` when the SDK is shut down. Same callsite shape as
    /// `runtime_clone`; used by the Deck binding to construct a
    /// `DeckClient` against the supervisor's `MeshOsRuntime`.
    #[cfg(feature = "deck")]
    pub(crate) fn with_core<R>(&self, f: impl FnOnce(&CoreSdk) -> R) -> Option<R> {
        self.inner.as_ref().map(f)
    }
}

#[pymethods]
impl PyMeshOsDaemonSdk {
    /// Start the SDK with optional config + the substrate's
    /// `LoggingDispatcher` as the action consumer.
    ///
    /// `config` accepts a dict with keys `this_node` (int),
    /// `tick_interval_ms` (int), `event_queue_capacity` (int),
    /// `action_queue_capacity` (int). Missing keys take substrate
    /// defaults; unknown keys are ignored.
    #[staticmethod]
    #[pyo3(signature = (config=None, control_capacity=None))]
    fn start(
        py: Python<'_>,
        config: Option<&Bound<'_, PyDict>>,
        control_capacity: Option<usize>,
    ) -> PyResult<Self> {
        let cfg = meshos_config_from_dict(py, config)?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    sdk_err(
                        py,
                        "runtime_start_failed",
                        &format!("failed to build tokio runtime: {e}"),
                    )
                })?,
        );
        let dispatcher = Arc::new(LoggingDispatcher::new());
        // `MeshOsDaemonSdk::start` registers a tokio task on the
        // current runtime context — enter the runtime before calling
        // it so the spawn lands on our owned runtime, not whatever
        // thread happens to be running this Python entry.
        let mut sdk = {
            let _enter = runtime.enter();
            CoreSdk::start(cfg, dispatcher)
        };
        if let Some(cap) = control_capacity {
            sdk = sdk.with_control_capacity(cap);
        }
        Ok(Self {
            inner: Some(sdk),
            runtime,
        })
    }

    /// Register a Python daemon under the supplied identity. The
    /// daemon must expose a `name` (str or `() -> str`) and a
    /// `process(event)` method; optional `snapshot`, `restore`,
    /// `on_control`, `health`, `saturation`.
    ///
    /// `identity` is a `net.Identity` (or its `EntityKeypair`-bearing
    /// equivalent). The substrate uses the keypair's `origin_hash`
    /// as the daemon's substrate id.
    fn register_daemon(
        &self,
        py: Python<'_>,
        daemon: Py<PyAny>,
        identity: &crate::identity::Identity,
    ) -> PyResult<PyMeshOsDaemonHandle> {
        let sdk = self.require_inner()?;
        let bridge = PyDaemonBridge::from_instance(py, daemon)?;
        let keypair = (*identity.keypair).clone();
        let handle = py.detach(|| {
            sdk.register_daemon(Box::new(bridge), keypair)
                .map_err(|e| Python::attach(|py| sdk_err_from(py, e)))
        })?;
        let daemon_id = handle.daemon_id();
        let daemon_name = handle.daemon_name().to_string();
        Ok(PyMeshOsDaemonHandle {
            inner: Some(handle),
            runtime: self.runtime.clone(),
            daemon_id,
            daemon_name,
        })
    }

    /// Diagnostic counter — total control events the router dropped
    /// across every registered daemon because a daemon's channel
    /// was full when an event tried to land.
    fn dropped_control_events(&self) -> PyResult<u64> {
        Ok(self.require_inner()?.dropped_control_events())
    }

    /// Tear down the wrapped runtime. Consumes the SDK by value —
    /// subsequent method calls raise `already_shutdown`.
    fn shutdown(&mut self, py: Python<'_>) -> PyResult<()> {
        let sdk = match self.inner.take() {
            Some(s) => s,
            None => {
                return Err(sdk_err(
                    py,
                    "already_shutdown",
                    "MeshOsDaemonSdk was already consumed by shutdown",
                ));
            }
        };
        let runtime = self.runtime.clone();
        py.detach(move || {
            runtime
                .block_on(async move { sdk.shutdown().await })
                .map(|_stats| ())
                .map_err(|e| {
                    Python::attach(|py| {
                        sdk_err(
                            py,
                            "shutdown_failed",
                            &format!("runtime shutdown failed: {e:?}"),
                        )
                    })
                })
        })
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_value=None, _exc_traceback=None))]
    fn __exit__(
        &mut self,
        py: Python<'_>,
        _exc_type: Option<Py<PyAny>>,
        _exc_value: Option<Py<PyAny>>,
        _exc_traceback: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        if self.inner.is_some() {
            self.shutdown(py)?;
        }
        Ok(false)
    }

    fn __repr__(&self) -> String {
        format!(
            "MeshOsDaemonSdk(active={}, dropped_control_events={})",
            self.inner.is_some(),
            self.inner
                .as_ref()
                .map(|s| s.dropped_control_events())
                .unwrap_or(0),
        )
    }
}
