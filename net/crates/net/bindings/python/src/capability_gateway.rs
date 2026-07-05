//! Native consent-gated capability surface (`HERMES_INTEGRATION_PLAN.md`
//! Phase 1 enabler).
//!
//! [`PyCapabilityGateway`] (and its awaitable dual [`PyAsyncCapabilityGateway`])
//! is the demand side of the bridge, *natively* — the same `search` /
//! `describe` / `invoke` a first-class SDK node needs, without the stdio MCP
//! shim in the middle. It embeds a [`MeshGateway`] over a joined `NetMesh` node
//! and applies the **one** consent gate ([`net_mcp::serve::gated_invoke`]) that
//! the `net mcp serve` shim also uses, so the gate can never fork between the
//! MCP-compat path and the native path (bridge doctrine H2).
//!
//! Doctrine #1 (no logic in bindings) holds: the describe → validate → consent
//! → invoke sequencing lives in the Rust adapter; this module only builds the
//! gateway from a `NetMesh`, reloads the shared pin store per call, and
//! marshals results. The sync and async classes share the same `do_*` async
//! bodies, so they cannot drift.
//!
//! **Results are structured, never exceptions.** Every method returns a JSON
//! object (as a string) with a `status` discriminant (`ok` / `requires_approval`
//! / `validation_error` / `denied` / `not_found` / `transport_error` /
//! `no_daemon` / `error`) so an embedding agent can relay a pin instruction or
//! let a model self-repair a bad argument, rather than catch an exception. JSON
//! crosses the boundary as a string, matching the MCP helper surface
//! (`classify` / `lower`).
//!
//! Consent state is the shared, machine-wide pin store: with an empty in-memory
//! policy, a capability is invocable only once its pin is `approved` in the same
//! file `net mcp pin` writes — so "approved anywhere is approved everywhere"
//! holds for a native SDK client exactly as it does for the shim.
//!
//! **Runtime note.** A `NetMesh` owns a per-instance tokio runtime distinct from
//! the process-global `future_into_py` runtime. The sync methods drive the
//! gateway on the mesh's runtime via `block_on` with the GIL released; the async
//! methods *spawn* the gateway op onto the mesh's runtime (where the node's
//! socket and timers live) and bridge only the `JoinHandle` to a Python
//! awaitable — so mesh I/O never runs on the wrong reactor.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::{json, Value};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use net_mcp::serve::{
    gated_invoke, CapabilityDetail, CapabilityGateway, CapabilityId, ConsentPolicy, GatedOutcome,
    GatewayError, MeshGateway, PinStore,
};
use net_mcp::wrap::DelegationSigner;
use net_sdk::mesh::Mesh as SdkMesh;
use net_sdk::Identity as SdkIdentity;

/// Build the caller-side delegation input from the Python kwargs. `leaf` is the
/// gateway `Identity` handle (its signing key stays in Rust — H8) and `chain`
/// is the serialized `DelegationChain`. Both or neither: passing exactly one is
/// a caller error.
fn build_delegation(
    delegation_leaf: Option<&crate::identity::Identity>,
    delegation_chain: Option<Vec<u8>>,
) -> PyResult<Option<(SdkIdentity, Vec<u8>)>> {
    match (delegation_leaf, delegation_chain) {
        (Some(leaf), Some(chain)) => {
            Ok(Some((SdkIdentity::from_seed(*leaf.keypair.secret_bytes()), chain)))
        }
        (None, None) => Ok(None),
        _ => Err(PyValueError::new_err(
            "delegation_leaf and delegation_chain must be provided together",
        )),
    }
}

// ---------------------------------------------------------------------------
// Shared marshaling helpers
// ---------------------------------------------------------------------------

/// Load a fresh pin-store snapshot. A read/parse error yields `None` — a broken
/// store must never *grant* consent (fail closed), matching the shim.
async fn load_pins(path: &Option<PathBuf>) -> Option<PinStore> {
    match path {
        Some(p) => PinStore::load(p).await.ok(),
        None => None,
    }
}

/// The `status` discriminant for a gateway failure.
fn gateway_status(e: &GatewayError) -> &'static str {
    match e {
        GatewayError::NotFound(_) => "not_found",
        GatewayError::Denied(_) => "denied",
        GatewayError::NoDaemon => "no_daemon",
        GatewayError::Transport(_) => "transport_error",
        GatewayError::Other(_) => "error",
    }
}

/// A `{status, error}` JSON string.
fn err_json(status: &str, msg: impl std::fmt::Display) -> String {
    json!({ "status": status, "error": msg.to_string() }).to_string()
}

/// Map a describe result to a JSON object, adding the caller-side
/// `requires_approval` flag.
fn detail_to_json(d: &CapabilityDetail, requires_approval: bool) -> String {
    json!({
        "status": "ok",
        "cap_id": d.id.display(),
        "name": d.name,
        "description": d.description,
        "input_schema": d.input_schema,
        "output_schema": d.output_schema,
        "compat_tier": d.compat_tier,
        "credential_status": d.credential_status,
        "substitutability": d.substitutability,
        "version": d.version,
        "requires_approval": requires_approval,
    })
    .to_string()
}

/// Flatten a [`GatedOutcome`] to the structured invoke result.
fn outcome_to_json(id: &CapabilityId, outcome: GatedOutcome) -> String {
    let v = match outcome {
        GatedOutcome::Invoked(result) => json!({
            "status": "ok",
            "is_error": result.is_error,
            "text": result.text(),
            "content": result.content,
            "structured_content": result.structured_content,
        }),
        GatedOutcome::ValidationFailed(reason) => json!({
            "status": "validation_error",
            "error": reason,
        }),
        GatedOutcome::RequiresApproval => json!({
            "status": "requires_approval",
            "cap_id": id.display(),
            "approve_command": format!("net mcp pin approve {}", id.display()),
            "message": format!(
                "Capability `{}` requires local approval before it can be invoked. \
                 Request it with net_request_capability; a human approves it out of \
                 band via `net mcp pin approve {}`.",
                id.display(),
                id.display(),
            ),
        }),
        GatedOutcome::Failed(e) => json!({
            "status": gateway_status(&e),
            "error": e.to_string(),
        }),
    };
    v.to_string()
}

// ---------------------------------------------------------------------------
// Shared gateway ops — the single async body each surface (sync + async) runs.
// ---------------------------------------------------------------------------

async fn do_search(
    gateway: &MeshGateway,
    consent: &ConsentPolicy,
    pin_path: &Option<PathBuf>,
    query: &str,
) -> String {
    match gateway.search(query).await {
        Ok(summaries) => {
            let pins = load_pins(pin_path).await;
            let rows: Vec<Value> = summaries
                .iter()
                .map(|s| {
                    let gated = consent.requires_approval(&s.id, &s.credential_status)
                        && !pins.as_ref().map(|p| p.is_approved(&s.id)).unwrap_or(false);
                    json!({
                        "cap_id": s.id.display(),
                        "name": s.name,
                        "description": s.description,
                        "compat_tier": s.compat_tier,
                        "credential_status": s.credential_status,
                        "providers": s.providers,
                        "requires_approval": gated,
                    })
                })
                .collect();
            json!({ "status": "ok", "capabilities": rows }).to_string()
        }
        Err(e) => err_json(gateway_status(&e), e),
    }
}

async fn do_describe(
    gateway: &MeshGateway,
    consent: &ConsentPolicy,
    pin_path: &Option<PathBuf>,
    id: CapabilityId,
) -> String {
    match gateway.describe(&id).await {
        Ok(detail) => {
            let pins = load_pins(pin_path).await;
            let gated = consent.requires_approval(&id, &detail.credential_status)
                && !pins.map(|p| p.is_approved(&id)).unwrap_or(false);
            detail_to_json(&detail, gated)
        }
        Err(e) => err_json(gateway_status(&e), e),
    }
}

async fn do_invoke(
    gateway: &MeshGateway,
    consent: &ConsentPolicy,
    pin_path: &Option<PathBuf>,
    id: CapabilityId,
    args: Value,
) -> String {
    let pins = load_pins(pin_path).await;
    let outcome = gated_invoke(gateway, consent, pins.as_ref(), &id, args).await;
    outcome_to_json(&id, outcome)
}

// ---------------------------------------------------------------------------
// Construction + async bridging
// ---------------------------------------------------------------------------

/// The gateway's shared state — built once from a `NetMesh`, cloned into each
/// GIL-released blocking closure / spawned task.
struct GatewayState {
    gateway: Arc<MeshGateway>,
    /// Config allowlist + in-memory pins. Empty in this first cut — the shared
    /// pin store is the source of approvals.
    consent: Arc<ConsentPolicy>,
    /// The machine-shared pin store path, reloaded fresh per call.
    pin_store_path: Option<PathBuf>,
    /// The mesh's own runtime — where the node's socket + timers live.
    runtime: Arc<Runtime>,
}

impl GatewayState {
    fn from_mesh(
        mesh: &crate::mesh_bindings::NetMesh,
        pin_store_path: Option<String>,
        delegation: Option<(SdkIdentity, Vec<u8>)>,
    ) -> PyResult<Self> {
        // Mirror `DaemonRuntime`: reuse the live node + its runtime so the
        // gateway drives mesh I/O on the same scheduler the node runs on.
        let node = mesh.node_arc_clone()?;
        let channel_configs = mesh.channel_configs_arc();
        let runtime = mesh.runtime_arc();
        let sdk_mesh = SdkMesh::from_node_arc(node, channel_configs, None);
        let mut gateway = MeshGateway::new(Arc::new(sdk_mesh));
        if let Some((leaf, chain_bytes)) = delegation {
            // Phase 3 Slice B2: sign + attach the delegation chain on every
            // invoke, so a provider running a `DelegationGate` admits by
            // verified delegation (not the spoofable owner-scope origin) and
            // audits this gateway's leaf.
            gateway = gateway.with_delegation(Arc::new(DelegationSigner::new(leaf, chain_bytes)));
        }
        Ok(Self {
            gateway: Arc::new(gateway),
            consent: Arc::new(ConsentPolicy::new()),
            pin_store_path: pin_store_path.map(PathBuf::from),
            runtime,
        })
    }
}

/// A Python awaitable that resolves immediately to `s` (for pre-flight errors
/// that never touch the mesh).
fn immediate(py: Python<'_>, s: String) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(s) })
}

/// Bridge a mesh-runtime `JoinHandle<String>` to a Python awaitable. The task
/// ran on the mesh's runtime; awaiting the handle here (on the process-global
/// `future_into_py` runtime) is a plain channel await — no reactor affinity.
fn spawn_bridge(py: Python<'_>, join: JoinHandle<String>) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        join.await
            .map_err(|e| PyRuntimeError::new_err(format!("gateway task failed: {e}")))
    })
}

// ---------------------------------------------------------------------------
// Sync gateway
// ---------------------------------------------------------------------------

/// A live, consent-gated capability gateway over an embedded `NetMesh` node.
///
/// Construct with `CapabilityGateway(mesh, pin_store_path=...)` where `mesh` is
/// a started `NetMesh`. `pin_store_path` should be the machine-shared pin store
/// (`net mcp pin`'s file) so approvals are honored bidirectionally; omit it to
/// keep consent in-memory (every gated capability then always requires
/// approval, since nothing can grant it).
///
/// The methods release the GIL while the mesh call is in flight, so an ``async``
/// caller can await them off the event loop with ``asyncio.to_thread`` — or use
/// :class:`AsyncCapabilityGateway` for a native awaitable surface.
#[pyclass(name = "CapabilityGateway", module = "net._net")]
pub struct PyCapabilityGateway {
    state: GatewayState,
}

#[pymethods]
impl PyCapabilityGateway {
    #[new]
    #[pyo3(signature = (mesh, pin_store_path=None, delegation_leaf=None, delegation_chain=None))]
    fn new(
        mesh: &crate::mesh_bindings::NetMesh,
        pin_store_path: Option<String>,
        delegation_leaf: Option<&crate::identity::Identity>,
        delegation_chain: Option<Vec<u8>>,
    ) -> PyResult<Self> {
        let delegation = build_delegation(delegation_leaf, delegation_chain)?;
        Ok(Self {
            state: GatewayState::from_mesh(mesh, pin_store_path, delegation)?,
        })
    }

    /// The machine-shared pin store path this gateway consults, if any.
    #[getter]
    fn pin_store_path(&self) -> Option<String> {
        self.state
            .pin_store_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Search the mesh for capabilities matching `query` (substring over id /
    /// name / description). Returns a JSON string
    /// `{"status":"ok","capabilities":[...]}` (each row carries
    /// `requires_approval`), or `{"status":"<err>","error":...}`. An empty index
    /// is `ok` with an empty list — never an error.
    fn search(&self, py: Python<'_>, query: &str) -> String {
        let h = self.state.handles();
        let runtime = self.state.runtime.clone();
        let query = query.to_string();
        py.detach(move || {
            runtime.block_on(do_search(&h.gateway, &h.consent, &h.pin_path, &query))
        })
    }

    /// Describe one capability by its `provider/capability` id. Returns a JSON
    /// string with the full schema + `requires_approval`, or
    /// `{"status":"<err>","error":...}`.
    fn describe(&self, py: Python<'_>, cap_id: &str) -> String {
        let id = match CapabilityId::parse(cap_id) {
            Ok(id) => id,
            Err(e) => return err_json("invalid_capability_id", e),
        };
        let h = self.state.handles();
        let runtime = self.state.runtime.clone();
        py.detach(move || runtime.block_on(do_describe(&h.gateway, &h.consent, &h.pin_path, id)))
    }

    /// Invoke a capability through the consent gate. `arguments_json` is the
    /// tool's own arguments as a JSON object string (default `{}`).
    ///
    /// Returns a JSON string whose `status` is one of `ok` (the provider
    /// answered — inspect `is_error` for a tool-level failure),
    /// `requires_approval`, `validation_error`, `denied`, `not_found`,
    /// `transport_error`, `no_daemon`, or `error`. Never raises for a gate
    /// outcome; a malformed id / arguments is itself a structured error.
    #[pyo3(signature = (cap_id, arguments_json="{}"))]
    fn invoke(&self, py: Python<'_>, cap_id: &str, arguments_json: &str) -> String {
        let id = match CapabilityId::parse(cap_id) {
            Ok(id) => id,
            Err(e) => return err_json("invalid_capability_id", e),
        };
        let args: Value = match serde_json::from_str(arguments_json) {
            Ok(v) => v,
            Err(e) => {
                return err_json("invalid_arguments", format!("arguments must be a JSON object: {e}"))
            }
        };
        let h = self.state.handles();
        let runtime = self.state.runtime.clone();
        py.detach(move || runtime.block_on(do_invoke(&h.gateway, &h.consent, &h.pin_path, id, args)))
    }

    fn __repr__(&self) -> String {
        self.state.repr("CapabilityGateway")
    }
}

// ---------------------------------------------------------------------------
// Async gateway
// ---------------------------------------------------------------------------

/// Awaitable dual of :class:`CapabilityGateway` — the same `search` / `describe`
/// / `invoke` as coroutines for `asyncio` code. Each awaits the gateway op on
/// the mesh's own runtime (spawned there so socket I/O stays on the right
/// reactor) and resolves to the same structured JSON string.
#[pyclass(name = "AsyncCapabilityGateway", module = "net._net")]
pub struct PyAsyncCapabilityGateway {
    state: GatewayState,
}

#[pymethods]
impl PyAsyncCapabilityGateway {
    #[new]
    #[pyo3(signature = (mesh, pin_store_path=None, delegation_leaf=None, delegation_chain=None))]
    fn new(
        mesh: &crate::mesh_bindings::NetMesh,
        pin_store_path: Option<String>,
        delegation_leaf: Option<&crate::identity::Identity>,
        delegation_chain: Option<Vec<u8>>,
    ) -> PyResult<Self> {
        let delegation = build_delegation(delegation_leaf, delegation_chain)?;
        Ok(Self {
            state: GatewayState::from_mesh(mesh, pin_store_path, delegation)?,
        })
    }

    /// The machine-shared pin store path this gateway consults, if any.
    #[getter]
    fn pin_store_path(&self) -> Option<String> {
        self.state
            .pin_store_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Awaitable :meth:`CapabilityGateway.search`.
    fn search<'py>(&self, py: Python<'py>, query: &str) -> PyResult<Bound<'py, PyAny>> {
        let h = self.state.handles();
        let query = query.to_string();
        let join = self.state.runtime.spawn(async move {
            do_search(&h.gateway, &h.consent, &h.pin_path, &query).await
        });
        spawn_bridge(py, join)
    }

    /// Awaitable :meth:`CapabilityGateway.describe`.
    fn describe<'py>(&self, py: Python<'py>, cap_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = match CapabilityId::parse(cap_id) {
            Ok(id) => id,
            Err(e) => return immediate(py, err_json("invalid_capability_id", e)),
        };
        let h = self.state.handles();
        let join = self
            .state
            .runtime
            .spawn(async move { do_describe(&h.gateway, &h.consent, &h.pin_path, id).await });
        spawn_bridge(py, join)
    }

    /// Awaitable :meth:`CapabilityGateway.invoke`.
    #[pyo3(signature = (cap_id, arguments_json="{}"))]
    fn invoke<'py>(
        &self,
        py: Python<'py>,
        cap_id: &str,
        arguments_json: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let id = match CapabilityId::parse(cap_id) {
            Ok(id) => id,
            Err(e) => return immediate(py, err_json("invalid_capability_id", e)),
        };
        let args: Value = match serde_json::from_str(arguments_json) {
            Ok(v) => v,
            Err(e) => {
                return immediate(
                    py,
                    err_json("invalid_arguments", format!("arguments must be a JSON object: {e}")),
                )
            }
        };
        let h = self.state.handles();
        let join = self
            .state
            .runtime
            .spawn(async move { do_invoke(&h.gateway, &h.consent, &h.pin_path, id, args).await });
        spawn_bridge(py, join)
    }

    fn __repr__(&self) -> String {
        self.state.repr("AsyncCapabilityGateway")
    }
}

/// Owned clones of the gateway state, moved into a blocking closure or a spawned
/// task (both need `'static`, so borrowing `&self` won't do).
struct GatewayHandles {
    gateway: Arc<MeshGateway>,
    consent: Arc<ConsentPolicy>,
    pin_path: Option<PathBuf>,
}

impl GatewayState {
    fn handles(&self) -> GatewayHandles {
        GatewayHandles {
            gateway: self.gateway.clone(),
            consent: self.consent.clone(),
            pin_path: self.pin_store_path.clone(),
        }
    }

    fn repr(&self, name: &str) -> String {
        match &self.pin_store_path {
            Some(p) => format!("{name}(pin_store={:?})", p.display()),
            None => format!("{name}(pin_store=None)"),
        }
    }
}
