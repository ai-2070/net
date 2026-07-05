//! Native consent-gated capability surface (`HERMES_INTEGRATION_PLAN.md`
//! Phase 1 enabler).
//!
//! [`PyCapabilityGateway`] is the demand side of the bridge, *natively* — the
//! same `search` / `describe` / `invoke` a first-class SDK node needs, without
//! the stdio MCP shim in the middle. It embeds a [`MeshGateway`] over a joined
//! `NetMesh` node and applies the **one** consent gate
//! ([`net_mcp::serve::gated_invoke`]) that the `net mcp serve` shim also uses,
//! so the gate can never fork between the MCP-compat path and the native path
//! (bridge doctrine H2).
//!
//! Doctrine #1 (no logic in bindings) holds: the describe → validate → consent
//! → invoke sequencing lives in the Rust adapter; this module only builds the
//! gateway from a `NetMesh`, reloads the shared pin store per call, and
//! marshals results.
//!
//! **Results are structured, never exceptions.** `invoke` returns a JSON object
//! with a `status` discriminant (`ok` / `requires_approval` / `validation_error`
//! / `denied` / `not_found` / `transport_error` / `no_daemon` / `error`) so an
//! embedding agent can relay a pin instruction or let a model self-repair a bad
//! argument, rather than catch an exception. JSON crosses the boundary as a
//! string, matching the MCP helper surface (`classify` / `lower`).
//!
//! Consent state is the shared, machine-wide pin store: with an empty in-memory
//! policy, a capability is invocable only once its pin is `approved` in the same
//! file `net mcp pin` writes — so "approved anywhere is approved everywhere"
//! holds for a native SDK client exactly as it does for the shim.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::prelude::*;
use serde_json::{json, Value};
use tokio::runtime::Runtime;

use net_mcp::serve::{
    gated_invoke, CapabilityDetail, CapabilityGateway, CapabilityId, ConsentPolicy, GatedOutcome,
    GatewayError, MeshGateway, PinStore,
};
use net_sdk::mesh::Mesh as SdkMesh;

/// A live, consent-gated capability gateway over an embedded `NetMesh` node.
///
/// Construct with `CapabilityGateway(mesh, pin_store_path=...)` where `mesh` is
/// a started `NetMesh`. `pin_store_path` should be the machine-shared pin store
/// (`net mcp pin`'s file) so approvals are honored bidirectionally; omit it to
/// keep consent in-memory (every gated capability then always requires
/// approval, since nothing can grant it).
#[pyclass(name = "CapabilityGateway", module = "net._net")]
pub struct PyCapabilityGateway {
    gateway: Arc<MeshGateway>,
    /// Config allowlist + in-memory pins. Empty in this first cut — the shared
    /// pin store is the source of approvals. `Arc` so it is cheaply cloned into
    /// the GIL-released blocking closures.
    consent: Arc<ConsentPolicy>,
    /// The machine-shared pin store path, reloaded fresh per call so an
    /// out-of-band `net mcp pin approve` takes effect immediately.
    pin_store_path: Option<PathBuf>,
    /// The mesh's tokio runtime — the blocking entry points drive the async
    /// gateway on it while the GIL is released.
    runtime: Arc<Runtime>,
}

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

#[pymethods]
impl PyCapabilityGateway {
    /// Build a gateway over an already-started `NetMesh`.
    #[new]
    #[pyo3(signature = (mesh, pin_store_path=None))]
    fn new(mesh: &crate::mesh_bindings::NetMesh, pin_store_path: Option<String>) -> PyResult<Self> {
        // Mirror `DaemonRuntime`: reuse the live node + its runtime so the
        // gateway drives mesh I/O on the same scheduler the node runs on.
        let node = mesh.node_arc_clone()?;
        let channel_configs = mesh.channel_configs_arc();
        let runtime = mesh.runtime_arc();
        let sdk_mesh = SdkMesh::from_node_arc(node, channel_configs, None);
        let gateway = MeshGateway::new(Arc::new(sdk_mesh));
        Ok(Self {
            gateway: Arc::new(gateway),
            consent: Arc::new(ConsentPolicy::new()),
            pin_store_path: pin_store_path.map(PathBuf::from),
            runtime,
        })
    }

    /// The machine-shared pin store path this gateway consults, if any.
    #[getter]
    fn pin_store_path(&self) -> Option<String> {
        self.pin_store_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Search the mesh for capabilities matching `query` (substring over id /
    /// name / description). Returns a JSON string
    /// `{"status":"ok","capabilities":[{cap_id, name, description, compat_tier,
    /// credential_status, providers, requires_approval}, ...]}`, or
    /// `{"status":"<err>","error":...}`. An empty index is `ok` with an empty
    /// list — never an error.
    fn search(&self, py: Python<'_>, query: &str) -> PyResult<String> {
        let gateway = self.gateway.clone();
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        let runtime = self.runtime.clone();
        let query = query.to_string();
        let rows = py.detach(move || {
            runtime.block_on(async move {
                let summaries = gateway.search(&query).await?;
                let pins = load_pins(&pin_path).await;
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
                Ok::<Vec<Value>, GatewayError>(rows)
            })
        });
        Ok(match rows {
            Ok(rows) => json!({ "status": "ok", "capabilities": rows }).to_string(),
            Err(e) => err_json(gateway_status(&e), e),
        })
    }

    /// Describe one capability by its `provider/capability` id. Returns a JSON
    /// string with the full schema + `requires_approval`, or
    /// `{"status":"<err>","error":...}`.
    fn describe(&self, py: Python<'_>, cap_id: &str) -> PyResult<String> {
        let id = match CapabilityId::parse(cap_id) {
            Ok(id) => id,
            Err(e) => return Ok(err_json("invalid_capability_id", e)),
        };
        let gateway = self.gateway.clone();
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        let runtime = self.runtime.clone();
        let (result, requires_approval) = py.detach(move || {
            runtime.block_on(async move {
                match gateway.describe(&id).await {
                    Ok(detail) => {
                        let pins = load_pins(&pin_path).await;
                        let gated = consent.requires_approval(&id, &detail.credential_status)
                            && !pins.map(|p| p.is_approved(&id)).unwrap_or(false);
                        (Ok(detail), gated)
                    }
                    Err(e) => (Err(e), false),
                }
            })
        });
        Ok(match result {
            Ok(detail) => detail_to_json(&detail, requires_approval),
            Err(e) => err_json(gateway_status(&e), e),
        })
    }

    /// Invoke a capability through the consent gate. `arguments_json` is the
    /// tool's own arguments as a JSON object string (default `{}`).
    ///
    /// Returns a JSON string whose `status` is one of `ok` (the provider
    /// answered — inspect `is_error` for a tool-level failure),
    /// `requires_approval` (relay the `approve_command`), `validation_error`
    /// (the model can self-repair against `describe`'s schema), `denied`,
    /// `not_found`, `transport_error`, `no_daemon`, or `error`. Never raises for
    /// a gate outcome; a malformed id / arguments is itself a structured error.
    #[pyo3(signature = (cap_id, arguments_json="{}"))]
    fn invoke(&self, py: Python<'_>, cap_id: &str, arguments_json: &str) -> PyResult<String> {
        let id = match CapabilityId::parse(cap_id) {
            Ok(id) => id,
            Err(e) => return Ok(err_json("invalid_capability_id", e)),
        };
        let args: Value = match serde_json::from_str(arguments_json) {
            Ok(v) => v,
            Err(e) => {
                return Ok(err_json(
                    "invalid_arguments",
                    format!("arguments must be a JSON object: {e}"),
                ))
            }
        };
        let gateway = self.gateway.clone();
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        let runtime = self.runtime.clone();
        let id_for_call = id.clone();
        let outcome = py.detach(move || {
            runtime.block_on(async move {
                let pins = load_pins(&pin_path).await;
                gated_invoke(&*gateway, &consent, pins.as_ref(), &id_for_call, args).await
            })
        });
        Ok(outcome_to_json(&id, outcome))
    }

    fn __repr__(&self) -> String {
        match &self.pin_store_path {
            Some(p) => format!("CapabilityGateway(pin_store={:?})", p.display()),
            None => "CapabilityGateway(pin_store=None)".to_string(),
        }
    }
}
