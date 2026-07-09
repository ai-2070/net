//! The consent-gated capability gateway, natively — `search` / `describe` /
//! `invoke` over an embedded `NetMesh` node with the one Rust consent gate
//! (`net_mcp::serve::gated_invoke`) applied *inside*, no stdio MCP shim. The
//! Node twin of the Python `capability_gateway.rs`, mirroring its composition
//! exactly: build a `MeshGateway` over the mesh's live node, reload the shared
//! pin store per call, and marshal results.
//!
//! Doctrine #1 (no logic in bindings) holds: the describe → validate → consent
//! → invoke sequencing lives in the Rust adapter; this module builds the
//! gateway from a `NetMesh` and projects results.
//!
//! **Results are structured, never exceptions.** Every method resolves to a
//! JSON object (as a string) with a `status` discriminant (`ok` /
//! `requires_approval` / `requires_payment_approval` / `validation_error` /
//! `denied` / `not_found` / `transport_error` / `no_daemon` / `error`) so an
//! embedding agent can relay a pin instruction or self-repair a bad argument,
//! rather than catch. On a denial the provider's `net.payment.failure@1`
//! schematic rides a `failure` field beside `error`.
//!
//! **Runtime.** Unlike the Python binding (whose `NetMesh` owns a per-instance
//! runtime), napi runs every `async fn` on its process-wide tokio runtime; the
//! gateway drives mesh I/O there, the same way `compute.rs`'s `DaemonRuntime`
//! already does over a shared `MeshNode`.

#![cfg(feature = "payments")]

use std::path::PathBuf;
use std::sync::Arc;

use napi::{Error, Result};
use napi_derive::napi;
use serde_json::{json, Value};

// The gateway trait's `search`/`describe` are methods on `MeshGateway`; bring
// the trait into scope anonymously so they resolve without colliding with this
// module's `CapabilityGateway` napi class.
use net_mcp::serve::CapabilityGateway as _;
use net_mcp::serve::{
    gated_invoke, CapabilityDetail, CapabilityId, ConsentPolicy, GatedOutcome, GatewayError,
    MeshGateway, PaymentFlow, PinStore,
};
use net_sdk::mesh::Mesh as SdkMesh;

use crate::NetMesh;

// ---------------------------------------------------------------------------
// Shared marshaling helpers — the mirror of the Python gateway's helpers, so
// the two surfaces cannot drift.
// ---------------------------------------------------------------------------

/// The `status` discriminant for a gateway failure.
fn gateway_status(e: &GatewayError) -> &'static str {
    match e {
        GatewayError::NotFound(_) => "not_found",
        GatewayError::Denied { .. } => "denied",
        GatewayError::NoDaemon => "no_daemon",
        GatewayError::Transport(_) => "transport_error",
        GatewayError::Other(_) => "error",
    }
}

/// A `{status, error}` JSON string.
fn err_json(status: &str, msg: impl std::fmt::Display) -> String {
    json!({ "status": status, "error": msg.to_string() }).to_string()
}

/// Load a fresh pin-store snapshot. A read/parse error yields `None` — a broken
/// store must never *grant* consent (fail closed), matching the shim.
async fn load_pins(path: &Option<PathBuf>) -> Option<PinStore> {
    match path {
        Some(p) => PinStore::load(p).await.ok(),
        None => None,
    }
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
        "pricing_terms": d.pricing_terms,
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
                "Capability `{}` requires local approval before it can be invoked; \
                 a human approves it out of band via `net mcp pin approve {}`.",
                id.display(),
                id.display(),
            ),
        }),
        // The payment-gate mirror of `requires_approval` — passed through
        // untouched (doctrine #1: the decision came from the Rust spend
        // engine). Approval resolves through the consent API; the shared store
        // holds the decision.
        GatedOutcome::RequiresPaymentApproval {
            quote_id,
            policy_reason,
            approve_hint,
        } => json!({
            "status": "requires_payment_approval",
            "cap_id": id.display(),
            "quote_id": quote_id,
            "policy_reason": policy_reason,
            "approve_hint": approve_hint,
        }),
        GatedOutcome::Failed(e) => {
            let mut failed = json!({
                "status": gateway_status(&e),
                "error": e.to_string(),
            });
            // The provider's structured verdict (`net.payment.failure@1`), when
            // one rode the refusal — beside the error string, never instead of
            // it. Agents branch on `failure.reason` / `failure.recovery`.
            if let GatewayError::Denied {
                schematic: Some(schematic),
                ..
            } = &e
            {
                if let Ok(failure) = serde_json::to_value(schematic.as_ref()) {
                    failed["failure"] = failure;
                }
            }
            failed
        }
    };
    v.to_string()
}

// ---------------------------------------------------------------------------
// Shared gateway ops — the single async body each method runs.
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
    payment: Option<&dyn PaymentFlow>,
    id: CapabilityId,
    args: Value,
) -> String {
    let pins = load_pins(pin_path).await;
    // With no payment flow configured (B1), a paid capability fails closed with
    // a structured `denied` (never a silent unpaid serve) — the payment flow
    // arrives in B2.
    let outcome = gated_invoke(gateway, consent, pins.as_ref(), payment, &id, args).await;
    outcome_to_json(&id, outcome)
}

// ---------------------------------------------------------------------------
// The gateway class
// ---------------------------------------------------------------------------

/// A live, consent-gated capability gateway over an embedded `NetMesh` node.
///
/// Construct with `new CapabilityGateway(mesh, pinStorePath?)` where `mesh` is a
/// started `NetMesh`. `pinStorePath` should be the machine-shared pin store
/// (`net mcp pin`'s file) so approvals are honored bidirectionally; omit it to
/// keep consent in-memory (every gated capability then always requires
/// approval). Every method resolves to a status-JSON string and never rejects
/// for a gate outcome.
#[napi]
pub struct CapabilityGateway {
    gateway: Arc<MeshGateway>,
    /// Config allowlist + in-memory pins. Empty in this first cut — the shared
    /// pin store is the source of approvals.
    consent: Arc<ConsentPolicy>,
    /// The machine-shared pin store path, reloaded fresh per call.
    pin_store_path: Option<PathBuf>,
    /// The caller-side payment flow for paid capabilities (wired in B2). `None`
    /// = paid capabilities fail closed at the gate, per doctrine.
    payment: Option<Arc<dyn PaymentFlow>>,
}

#[napi]
impl CapabilityGateway {
    #[napi(constructor)]
    pub fn new(mesh: &NetMesh, pin_store_path: Option<String>) -> Result<Self> {
        // Reuse the live node + its channel configs so the gateway drives mesh
        // I/O over the same node the caller runs (the `DaemonRuntime::create`
        // precedent). Identity is `None`: the payment identity is derived from
        // the node inside the flow (B2), never handed in.
        let node = mesh
            .node_arc_clone()
            .map_err(|_| Error::from_reason("gateway: mesh node has been shut down"))?;
        let channel_configs = mesh.channel_configs_arc();
        let sdk_mesh = Arc::new(SdkMesh::from_node_arc(node, channel_configs, None));
        Ok(Self {
            gateway: Arc::new(MeshGateway::new(sdk_mesh)),
            consent: Arc::new(ConsentPolicy::new()),
            pin_store_path: pin_store_path.map(PathBuf::from),
            payment: None,
        })
    }

    /// The machine-shared pin store path this gateway consults, if any.
    #[napi(getter)]
    pub fn pin_store_path(&self) -> Option<String> {
        self.pin_store_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Search the mesh for capabilities matching `query` (substring over id /
    /// name / description). Resolves to `{"status":"ok","capabilities":[...]}`
    /// (each row carries `requiresApproval`), or `{"status":"<err>","error":...}`.
    /// An empty index is `ok` with an empty list — never an error.
    #[napi]
    pub async fn search(&self, query: String) -> Result<String> {
        let gateway = self.gateway.clone();
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        Ok(do_search(&gateway, &consent, &pin_path, &query).await)
    }

    /// Describe one capability by its `provider/capability` id. Resolves to a
    /// JSON string with the full schema + `requiresApproval` + `pricingTerms`,
    /// or `{"status":"<err>","error":...}`.
    #[napi]
    pub async fn describe(&self, cap_id: String) -> Result<String> {
        let id = match CapabilityId::parse(&cap_id) {
            Ok(id) => id,
            Err(e) => return Ok(err_json("invalid_capability_id", e)),
        };
        let gateway = self.gateway.clone();
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        Ok(do_describe(&gateway, &consent, &pin_path, id).await)
    }

    /// Invoke a capability through the consent gate. `argumentsJson` is the
    /// tool's own arguments as a JSON object string (default `{}`).
    ///
    /// Resolves to a JSON string whose `status` is one of `ok`,
    /// `requires_approval`, `requires_payment_approval`, `validation_error`,
    /// `denied`, `not_found`, `transport_error`, `no_daemon`, or `error`. Never
    /// rejects for a gate outcome; a malformed id / arguments is itself a
    /// structured error.
    #[napi]
    pub async fn invoke(&self, cap_id: String, arguments_json: Option<String>) -> Result<String> {
        let id = match CapabilityId::parse(&cap_id) {
            Ok(id) => id,
            Err(e) => return Ok(err_json("invalid_capability_id", e)),
        };
        let raw = arguments_json.unwrap_or_else(|| "{}".to_string());
        let args: Value = match serde_json::from_str(&raw) {
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
        let payment = self.payment.clone();
        Ok(do_invoke(&gateway, &consent, &pin_path, payment.as_deref(), id, args).await)
    }
}

// ---------------------------------------------------------------------------
// Contract tests — the structured-JSON projection is the binding's whole job,
// so its shape is pinned here (format-string only; the napi class can't link
// under `cargo test`, so no live gateway — that's vitest's job).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use net_sdk::tool_payment::FailureSchematic;

    #[test]
    fn a_denied_outcome_projects_the_failure_schematic() {
        let id = CapabilityId::parse("prov/tool").expect("cap id");
        let schematic = FailureSchematic::missing_quote("tool");
        let denied = GatedOutcome::Failed(GatewayError::Denied {
            message: "paid tool invoked without a payment quote header".into(),
            schematic: Some(Box::new(schematic)),
        });
        let v: Value = serde_json::from_str(&outcome_to_json(&id, denied)).expect("json");
        assert_eq!(v["status"], "denied");
        assert!(v["error"].as_str().unwrap().contains("payment quote"));
        assert_eq!(v["failure"]["reason"], "missing_quote");
        assert_eq!(v["failure"]["object"], "net.payment.failure@1");

        // A schematic-less denial is exactly the pre-schematic shape.
        let plain = GatedOutcome::Failed(GatewayError::denied("owner scope"));
        let v: Value = serde_json::from_str(&outcome_to_json(&id, plain)).expect("json");
        assert_eq!(v["status"], "denied");
        assert!(v.get("failure").is_none(), "no schematic, no failure field");
    }

    #[test]
    fn a_payment_approval_outcome_projects_its_fields() {
        let id = CapabilityId::parse("prov/tool").expect("cap id");
        let v: Value = serde_json::from_str(&outcome_to_json(
            &id,
            GatedOutcome::RequiresPaymentApproval {
                quote_id: "q-1".into(),
                policy_reason: "over cap".into(),
                approve_hint: "approve q-1".into(),
            },
        ))
        .expect("json");
        assert_eq!(v["status"], "requires_payment_approval");
        assert_eq!(v["quote_id"], "q-1");
        assert_eq!(v["cap_id"], "prov/tool");
    }
}
