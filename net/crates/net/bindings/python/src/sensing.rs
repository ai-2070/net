//! pyo3 bindings for capability sensing (SENSING_OLB_COMPLETION_PLAN B1).
//!
//! Every behaviour here is the Rust SDK's (`net_sdk::sensing`), forwarded:
//!
//! - **Provider.** `sensing_provide(mesh, capability, state, ...)` returns a
//!   [`PyReadinessProvider`] over the SDK's value-backed provider. Python sets
//!   a value; Rust's evaluator reads it on the attestation path. No Python
//!   runs per beat, and no GIL is taken on the attestation path.
//! - **Consumer.** `sensing_watch(mesh, capability, ...)` returns a
//!   [`PySensingWatch`]. `changed(timeout_ms=None)` blocks with the GIL
//!   released and returns `False` on timeout. The Rust wait is cancel-safe,
//!   so a timeout consumes no edge: the next call still sees a change that
//!   landed meanwhile. `close()`, from any thread, ends a blocked `changed()`
//!   with `SensingError("sensing:watch_closed: ...")` (B1-pre).
//! - **Errors.** Every refusal raises `SensingError`, whose message is the
//!   SDK's `sensing:<kind>: <message>` wire string. `sensing_error_class(wire)`
//!   answers kind and terminal from Rust's own classifier (C5), so this
//!   binding never matches `SensingError` variants.

use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use net_sdk::sensing::{self as sdk, ReadinessValue, SensedViability, SensingQuery};

use crate::runtime_guard::GuardedRuntime;

pyo3::create_exception!(
    _net,
    SensingError,
    pyo3::exceptions::PyException,
    "A capability-sensing refusal. The message is the Rust SDK's \
     `sensing:<kind>: <message>` wire string; `sensing_error_class(str(e))` \
     returns Rust's classification of it as {'kind': ..., 'terminal': ...}."
);

fn sensing_err(error: sdk::SensingError) -> PyErr {
    SensingError::new_err(error.to_wire())
}

fn millis(value: Option<f64>, name: &str) -> PyResult<Option<Duration>> {
    match value {
        None => Ok(None),
        Some(ms) if ms.is_finite() && ms >= 0.0 => Ok(Some(Duration::from_secs_f64(ms / 1000.0))),
        Some(ms) => Err(PyValueError::new_err(format!(
            "{name} must be a finite, non-negative number of milliseconds; got {ms}"
        ))),
    }
}

fn as_ms(value: Option<Duration>) -> Option<f64> {
    value.map(|d| d.as_secs_f64() * 1000.0)
}

fn value_from_args(
    state: &str,
    estimated_start_ms: Option<f64>,
    reason: Option<u32>,
) -> PyResult<ReadinessValue> {
    match state {
        "ready" => Ok(ReadinessValue::Ready {
            estimated_start: millis(estimated_start_ms, "estimated_start_ms")?,
        }),
        "not_ready" => {
            let reason = reason.unwrap_or(0);
            let reason = u16::try_from(reason).map_err(|_| {
                PyValueError::new_err(format!("reason must fit in 16 bits; got {reason}"))
            })?;
            Ok(ReadinessValue::NotReady { reason })
        }
        "unevaluable" => Ok(ReadinessValue::Unevaluable),
        other => Err(PyValueError::new_err(format!(
            "state must be 'ready', 'not_ready' or 'unevaluable'; got {other:?}"
        ))),
    }
}

fn value_to_dict<'py>(py: Python<'py>, value: ReadinessValue) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    match value {
        ReadinessValue::Ready { estimated_start } => {
            dict.set_item("state", "ready")?;
            dict.set_item("estimated_start_ms", as_ms(estimated_start))?;
        }
        ReadinessValue::NotReady { reason } => {
            dict.set_item("state", "not_ready")?;
            dict.set_item("reason", reason)?;
        }
        ReadinessValue::Unevaluable => {
            dict.set_item("state", "unevaluable")?;
        }
    }
    Ok(dict)
}

/// Rust's classification of a `sensing:` error message, as
/// `{'kind': str, 'terminal': bool}`, or `None` when `wire` is not a sensing
/// wire string this build knows.
#[pyfunction]
pub fn sensing_error_class<'py>(
    py: Python<'py>,
    wire: &str,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let Some(class) = sdk::classify_sensing_wire(wire) else {
        return Ok(None);
    };
    let dict = PyDict::new(py);
    dict.set_item("kind", class.kind)?;
    dict.set_item("terminal", class.terminal)?;
    Ok(Some(dict))
}

fn sensing_client(mesh: &crate::mesh_bindings::NetMesh) -> PyResult<sdk::SensingClient> {
    let node = mesh.node_arc_clone()?;
    let sdk_mesh = net_sdk::mesh::Mesh::from_node_arc(
        node,
        Arc::new(net::adapter::net::ChannelConfigRegistry::new()),
        None,
    );
    sdk_mesh.sensing().map_err(sensing_err)
}

/// An owning handle on one value-backed readiness registration.
#[pyclass(name = "ReadinessProvider", module = "net._net")]
pub struct PyReadinessProvider {
    inner: sdk::ValueReadiness,
}

#[pymethods]
impl PyReadinessProvider {
    /// Publish a new value, then wake every live observation path. Returns
    /// whether any observation moved; a set after `close()` stores nothing.
    #[pyo3(signature = (state, estimated_start_ms=None, reason=None))]
    fn set(
        &self,
        state: &str,
        estimated_start_ms: Option<f64>,
        reason: Option<u32>,
    ) -> PyResult<bool> {
        Ok(self
            .inner
            .set(value_from_args(state, estimated_start_ms, reason)?))
    }

    /// The value currently published, as a dict.
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        value_to_dict(py, self.inner.get())
    }

    /// Stop serving readiness. True only for the call that removed it.
    fn close(&self) -> bool {
        self.inner.close()
    }

    #[getter]
    fn closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[getter]
    fn capability(&self) -> String {
        self.inner.capability().as_str().to_string()
    }
}

/// One immutable capture of an authorized population's readiness.
#[pyclass(name = "SensingSnapshot", module = "net._net", frozen)]
pub struct PySensingSnapshot {
    pub(crate) inner: sdk::SensingSnapshot,
}

#[pymethods]
impl PySensingSnapshot {
    #[getter]
    fn capability(&self) -> String {
        self.inner.capability().to_string()
    }

    /// One dict per authorized provider, in ascending node-id order:
    /// `node_id`, `readiness` ('ready' | 'not_ready' | 'unknown'),
    /// `viability` ('viable' | 'potential' | 'not_viable'),
    /// `estimated_start_ms`, `route_estimate_ms`.
    #[getter]
    fn providers<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.inner
            .providers()
            .iter()
            .map(|row| {
                let dict = PyDict::new(py);
                dict.set_item("node_id", row.node_id())?;
                dict.set_item(
                    "readiness",
                    match row.readiness() {
                        sdk::ProjectedReadiness::Ready => "ready",
                        sdk::ProjectedReadiness::NotReady => "not_ready",
                        sdk::ProjectedReadiness::Unknown => "unknown",
                    },
                )?;
                dict.set_item(
                    "viability",
                    match row.viability() {
                        SensedViability::Viable => "viable",
                        SensedViability::Potential => "potential",
                        SensedViability::NotViable => "not_viable",
                    },
                )?;
                dict.set_item("estimated_start_ms", as_ms(row.estimated_start()))?;
                dict.set_item("route_estimate_ms", as_ms(row.route_estimate()))?;
                Ok(dict)
            })
            .collect()
    }

    /// The viable providers in sensed rank order.
    #[getter]
    fn ranked(&self) -> Vec<u64> {
        self.inner.ranked().to_vec()
    }

    /// The provider to try first, or None when nothing is viable.
    #[getter]
    fn preferred(&self) -> Option<u64> {
        self.inner.preferred()
    }
}

/// An owning observation of one capability's authorized exact-provider
/// readiness.
#[pyclass(name = "SensingWatch", module = "net._net")]
pub struct PySensingWatch {
    inner: Arc<tokio::sync::Mutex<sdk::SensingWatch>>,
    closer: sdk::SensingWatchCloser,
    runtime: Arc<GuardedRuntime>,
    capability: String,
}

#[pymethods]
impl PySensingWatch {
    /// Take a snapshot. Blocks (GIL released) only while a `changed()` on
    /// another thread is still pending.
    fn snapshot(&self, py: Python<'_>) -> PyResult<PySensingSnapshot> {
        let inner = self.inner.clone();
        let closer = self.closer.clone();
        let runtime = self.runtime.clone();
        py.detach(move || {
            runtime.block_on(async move {
                if closer.is_closed() {
                    return Err(sdk::SensingError::WatchClosed);
                }
                inner.lock().await.snapshot()
            })
        })
        .map(|inner| PySensingSnapshot { inner })
        .map_err(sensing_err)
    }

    /// Block (GIL released) until something that can change the answer
    /// moved. Returns True on a wake, False when `timeout_ms` elapsed first.
    /// A timeout consumes no edge. Raises `SensingError` (watch_closed) when
    /// the watch is closed, including by `close()` from another thread while
    /// this call is blocked.
    #[pyo3(signature = (timeout_ms=None))]
    fn changed(&self, py: Python<'_>, timeout_ms: Option<f64>) -> PyResult<bool> {
        let timeout = millis(timeout_ms, "timeout_ms")?;
        let inner = self.inner.clone();
        let closer = self.closer.clone();
        let runtime = self.runtime.clone();
        let outcome = py.detach(move || {
            runtime.block_on(async move {
                if closer.is_closed() {
                    return Some(Err(sdk::SensingError::WatchClosed));
                }
                let wait = async move {
                    let mut watch = inner.lock().await;
                    watch.changed().await
                };
                match timeout {
                    Some(limit) => tokio::time::timeout(limit, wait).await.ok(),
                    None => Some(wait.await),
                }
            })
        });
        match outcome {
            None => Ok(false),
            Some(Ok(())) => Ok(true),
            Some(Err(error)) => Err(sensing_err(error)),
        }
    }

    /// Stop observing; ends any blocked `changed()`. True only for the call
    /// that closed it.
    fn close(&self) -> bool {
        self.closer.close()
    }

    #[getter]
    fn closed(&self) -> bool {
        self.closer.is_closed()
    }

    #[getter]
    fn capability(&self) -> String {
        self.capability.clone()
    }
}

/// Serve readiness for `capability` from a stored value: `state` is 'ready'
/// (with optional `estimated_start_ms`), 'not_ready' (with optional
/// `reason`), or 'unevaluable'. Refuses exactly as the Rust SDK does.
#[pyfunction]
#[pyo3(signature = (mesh, capability, state, estimated_start_ms=None, reason=None))]
pub fn sensing_provide(
    mesh: &crate::mesh_bindings::NetMesh,
    capability: String,
    state: &str,
    estimated_start_ms: Option<f64>,
    reason: Option<u32>,
) -> PyResult<PyReadinessProvider> {
    let initial = value_from_args(state, estimated_start_ms, reason)?;
    let runtime = mesh.runtime_arc();
    let _enter = runtime.enter();
    let inner = sensing_client(mesh)?
        .provide_value(capability, initial)
        .map_err(sensing_err)?;
    Ok(PyReadinessProvider { inner })
}

/// Observe the readiness of this node's authorized own-organization providers
/// of `capability`. `within_ms` is the consumer-local end-to-end budget;
/// `start_within_ms` is the provider-start predicate that rides the signed
/// interest.
#[pyfunction]
#[pyo3(signature = (mesh, capability, within_ms=None, start_within_ms=None))]
pub fn sensing_watch(
    mesh: &crate::mesh_bindings::NetMesh,
    capability: String,
    within_ms: Option<f64>,
    start_within_ms: Option<f64>,
) -> PyResult<PySensingWatch> {
    let mut query = SensingQuery::new(capability.clone());
    if let Some(within) = millis(within_ms, "within_ms")? {
        query = query.within(within);
    }
    if let Some(start) = millis(start_within_ms, "start_within_ms")? {
        query = query.start_within(start);
    }
    let runtime = mesh.runtime_arc();
    let watch = {
        let _enter = runtime.enter();
        sensing_client(mesh)?.watch(query).map_err(sensing_err)?
    };
    let closer = watch.closer();
    Ok(PySensingWatch {
        inner: Arc::new(tokio::sync::Mutex::new(watch)),
        closer,
        runtime,
        capability,
    })
}
