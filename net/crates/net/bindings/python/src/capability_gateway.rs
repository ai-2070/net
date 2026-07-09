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
use net_sdk::delegation::DelegationChain;
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
            // Validate upfront: a malformed chain, or one whose leaf isn't the
            // supplied `delegation_leaf`, would otherwise be accepted silently
            // and fail on EVERY invoke with an opaque provider-side denial. Fail
            // here with a clear error instead.
            let parsed = DelegationChain::from_bytes(&chain).map_err(|e| {
                PyValueError::new_err(format!("delegation_chain is not a valid chain: {e:?}"))
            })?;
            if &parsed.leaf() != leaf.keypair.entity_id() {
                return Err(PyValueError::new_err(
                    "delegation_chain's leaf does not match delegation_leaf's entity id",
                ));
            }
            Ok(Some((
                SdkIdentity::from_seed(*leaf.keypair.secret_bytes()),
                chain,
            )))
        }
        (None, None) => Ok(None),
        _ => Err(PyValueError::new_err(
            "delegation_leaf and delegation_chain must be provided together",
        )),
    }
}

/// Validate + unbind the payment-signer callable at construction, so a
/// non-callable fails here with a clear error instead of on the first
/// paid invoke. `pub(crate)` — the outbound HTTP-402 client
/// ([`crate::payment_http`]) shares this validation.
pub(crate) fn unbind_signer(
    signer: Option<Bound<'_, PyAny>>,
) -> PyResult<Option<pyo3::Py<pyo3::PyAny>>> {
    match signer {
        Some(callable) if callable.is_callable() => Ok(Some(callable.unbind())),
        Some(_) => Err(PyValueError::new_err(
            "payment_signer must be callable: (typed_data_json: str) -> str \
             (the 0x-hex EIP-712 signature)",
        )),
        None => Ok(None),
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

/// Parse and normalize the `invoke` arguments string to a JSON *object*.
///
/// A JSON `null` (or the default `"{}"`) is a no-argument invocation —
/// normalized to `{}`, exactly as the SDK's [`gated_invoke`] does at the one
/// chokepoint every demand-side caller routes through. Arrays and primitives
/// are still a caller-shape error: the gateway surface requires an object, and
/// the core does not accept arbitrary-typed args. `Err` carries the human
/// message for an `invalid_arguments` status.
///
/// The twin of the Node gateway's `normalize_invoke_args`, so the two surfaces
/// cannot drift.
fn normalize_invoke_args(raw: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|e| format!("arguments must be a JSON object: {e}"))?;
    let value = if value.is_null() { json!({}) } else { value };
    if !value.is_object() {
        return Err("arguments must be a JSON object".to_string());
    }
    Ok(value)
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
        // engine; this arm only names the fields). Approval resolves
        // through the SDK consent API; the shared store holds the decision.
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
            // The provider's structured verdict (`net.payment.failure@1`),
            // when one rode the refusal — beside the error string, never
            // instead of it. Agents branch on `failure.reason` /
            // `failure.recovery` without parsing prose.
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
    payment: Option<&dyn net_mcp::serve::PaymentFlow>,
    id: CapabilityId,
    args: Value,
) -> String {
    let pins = load_pins(pin_path).await;
    // With no payment flow configured, a paid capability fails closed with
    // a structured `denied` (never a silent unpaid serve).
    let outcome = gated_invoke(gateway, consent, pins.as_ref(), payment, &id, args).await;
    outcome_to_json(&id, outcome)
}

// ---------------------------------------------------------------------------
// Operator approval verbs — thin wrappers over `SpendPolicyEngine` on the
// same shared spend-policy store the flow reserves against, so an agent can
// resolve its own `requires_payment_approval` under operator policy. The
// store, lock protocol, and Pending -> Approved transition stay in Rust
// (doctrine #1: no logic here — these marshal a quote id / pair in and the
// engine's typed result out as the gateway's status-JSON). Feature-split so
// a build without `payments` (or a gateway built without
// `payment_policy_path`) is a loud structured error, never a silent no-op.
// ---------------------------------------------------------------------------

#[cfg(feature = "payments")]
fn spend_engine(
    path: &str,
    profile: &str,
) -> Result<net_payments::policy::spend::SpendPolicyEngine, String> {
    use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
    // Vocabulary lives once in core (`SpendProfile::parse`). The profile string
    // was validated at construction (`build_payment_flow`), so this parse is
    // expected to succeed — but on the (unreachable) failure we surface a loud
    // error rather than silently defaulting, honoring parse's no-silent-fallback
    // contract. Unlike the Node gateway we can't store the parsed `SpendProfile`
    // in `GatewayState`: `net_payments` is an optional dep and this module
    // compiles even in a payments-off build, so the retained profile stays a
    // `String` and is re-parsed here through the one core parser.
    let profile = SpendProfile::parse(profile).map_err(|e| e.to_string())?;
    Ok(SpendPolicyEngine::new(path, profile))
}

/// `{"status":"no_payment_policy", ...}` — the verb needs the shared
/// spend-policy store, and this gateway was built without
/// `payment_policy_path`.
#[cfg(feature = "payments")]
fn no_policy_json() -> String {
    err_json(
        "no_payment_policy",
        "no spend policy configured — construct the gateway with payment_policy_path \
         (the machine-shared spend-policy store) to use the approval verbs",
    )
}

#[cfg(feature = "payments")]
async fn do_approve_payment(
    policy_path: Option<String>,
    profile: String,
    quote_id: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    let engine = match spend_engine(&path, &profile) {
        Ok(e) => e,
        Err(e) => return err_json("error", e),
    };
    match engine.approve(&quote_id).await {
        Ok(changed) => {
            json!({ "status": "ok", "quote_id": quote_id, "changed": changed }).to_string()
        }
        Err(e) => err_json("error", e),
    }
}

#[cfg(feature = "payments")]
async fn do_reject_payment(
    policy_path: Option<String>,
    profile: String,
    quote_id: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    let engine = match spend_engine(&path, &profile) {
        Ok(e) => e,
        Err(e) => return err_json("error", e),
    };
    match engine.reject(&quote_id).await {
        Ok(changed) => {
            json!({ "status": "ok", "quote_id": quote_id, "changed": changed }).to_string()
        }
        Err(e) => err_json("error", e),
    }
}

#[cfg(feature = "payments")]
async fn do_pending_payments(policy_path: Option<String>, profile: String) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    let engine = match spend_engine(&path, &profile) {
        Ok(e) => e,
        Err(e) => return err_json("error", e),
    };
    match engine.pending().await {
        Ok(quotes) => json!({ "status": "ok", "pending": quotes }).to_string(),
        Err(e) => err_json("error", e),
    }
}

#[cfg(feature = "payments")]
async fn do_spent_today(
    policy_path: Option<String>,
    profile: String,
    network: String,
    asset: String,
) -> String {
    use net_payments::flow::{Clock, SystemClock};
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    let now_ns = SystemClock.now_ns();
    let engine = match spend_engine(&path, &profile) {
        Ok(e) => e,
        Err(e) => return err_json("error", e),
    };
    match engine.spent_today(&network, &asset, now_ns).await {
        Ok(amount) => json!({
            "status": "ok",
            "network": network,
            "asset": asset,
            "spent": amount.to_canonical_string(),
        })
        .to_string(),
        Err(e) => err_json("error", e),
    }
}

/// Without the `payments` build feature the approval verbs are a loud
/// `unsupported` error, matching the construction-time payment-kwarg guard.
#[cfg(not(feature = "payments"))]
fn payments_unsupported_json() -> String {
    err_json(
        "unsupported",
        "this build lacks the `payments` feature; the approval verbs are unavailable",
    )
}

#[cfg(not(feature = "payments"))]
async fn do_approve_payment(
    _policy_path: Option<String>,
    _profile: String,
    _quote_id: String,
) -> String {
    payments_unsupported_json()
}

#[cfg(not(feature = "payments"))]
async fn do_reject_payment(
    _policy_path: Option<String>,
    _profile: String,
    _quote_id: String,
) -> String {
    payments_unsupported_json()
}

#[cfg(not(feature = "payments"))]
async fn do_pending_payments(_policy_path: Option<String>, _profile: String) -> String {
    payments_unsupported_json()
}

#[cfg(not(feature = "payments"))]
async fn do_spent_today(
    _policy_path: Option<String>,
    _profile: String,
    _network: String,
    _asset: String,
) -> String {
    payments_unsupported_json()
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
    /// The caller-side payment flow for paid capabilities (`payments`
    /// build feature + payment kwargs). `None` = paid capabilities fail
    /// closed at the gate, per doctrine.
    payment: Option<Arc<dyn net_mcp::serve::PaymentFlow>>,
    /// The spend-policy store path retained from the payment kwargs, so the
    /// operator approval verbs (approve/reject/pending/spent_today) open the
    /// same shared store the flow reserves against. `None` = no
    /// `payment_policy_path` was supplied; the verbs then return
    /// `no_payment_policy`.
    spend_policy_path: Option<String>,
    /// The spend profile string (`"production"` / `"dev_test"`) the store
    /// was configured with — carried so a reconstructed engine matches.
    spend_profile: String,
    /// The mesh's own runtime — where the node's socket + timers live.
    runtime: Arc<Runtime>,
}

/// Payment kwargs, collected before the cfg boundary so both gateway
/// constructors share the validation.
///
/// Without the `payments` build feature the fields are written by
/// `collect` (so validation stays identical across builds) but only the
/// feature-gated `build_payment_flow` reads them — hence the targeted
/// dead-code allow; `-D warnings` still guards every other lint.
///
/// `pub(crate)` — the outbound HTTP-402 client ([`crate::payment_http`])
/// shares these kwargs and this validation verbatim.
#[cfg_attr(not(feature = "payments"), allow(dead_code))]
pub(crate) struct PaymentConfig {
    pub(crate) policy_path: String,
    pub(crate) profile: String,
    pub(crate) unsafe_mock_auto_allow: bool,
    /// The eip155 settlement signer *reference*: the payer address plus a
    /// Python callable `(typed_data_json: str) -> str` that forwards
    /// the EIP-712 document to the host's wallet / KMS and returns
    /// the 0x-hex signature. Only the typed document and the
    /// signature ever cross the language boundary — key material
    /// remains unrepresentable here (doctrine 4/7/8).
    pub(crate) signer: Option<(String, pyo3::Py<pyo3::PyAny>)>,
    /// The solana signer reference: address + `(intent_json: str) -> str`
    /// returning the base64 partially-signed versioned transaction.
    pub(crate) signer_svm: Option<(String, pyo3::Py<pyo3::PyAny>)>,
    /// The xrpl signer reference: address + `(intent_json: str) -> str`
    /// returning the hex presigned Payment blob.
    pub(crate) signer_xrpl: Option<(String, pyo3::Py<pyo3::PyAny>)>,
}

/// Pair an address with its callable: both-or-neither, else a clear caller
/// error naming the pair.
#[cfg_attr(not(feature = "payments"), allow(dead_code))]
fn signer_pair(
    address: Option<String>,
    callable: Option<pyo3::Py<pyo3::PyAny>>,
    kwarg: &str,
) -> PyResult<Option<(String, pyo3::Py<pyo3::PyAny>)>> {
    match (address, callable) {
        (Some(address), Some(callable)) => Ok(Some((address, callable))),
        (None, None) => Ok(None),
        _ => Err(PyValueError::new_err(format!(
            "{kwarg} and {kwarg}_address must be provided together \
             (the address names the payer; the callable signs its typed intent)"
        ))),
    }
}

impl PaymentConfig {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn collect(
        payment_policy_path: Option<String>,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: bool,
        payment_signer_address: Option<String>,
        payment_signer: Option<pyo3::Py<pyo3::PyAny>>,
        payment_signer_svm_address: Option<String>,
        payment_signer_svm: Option<pyo3::Py<pyo3::PyAny>>,
        payment_signer_xrpl_address: Option<String>,
        payment_signer_xrpl: Option<pyo3::Py<pyo3::PyAny>>,
    ) -> PyResult<Option<Self>> {
        let signer = signer_pair(payment_signer_address, payment_signer, "payment_signer")?;
        let signer_svm = signer_pair(
            payment_signer_svm_address,
            payment_signer_svm,
            "payment_signer_svm",
        )?;
        let signer_xrpl = signer_pair(
            payment_signer_xrpl_address,
            payment_signer_xrpl,
            "payment_signer_xrpl",
        )?;
        let any_signer = signer.is_some() || signer_svm.is_some() || signer_xrpl.is_some();
        match payment_policy_path {
            Some(policy_path) => Ok(Some(Self {
                policy_path,
                profile: payment_profile.unwrap_or_else(|| "production".to_string()),
                unsafe_mock_auto_allow: payment_unsafe_mock_auto_allow,
                signer,
                signer_svm,
                signer_xrpl,
            })),
            None if payment_profile.is_some() || payment_unsafe_mock_auto_allow || any_signer => {
                Err(PyValueError::new_err(
                    "payment_profile / payment_unsafe_mock_auto_allow / a payment signer \
                     require payment_policy_path (the shared spend-policy store)",
                ))
            }
            None => Ok(None),
        }
    }
}

impl GatewayState {
    fn from_mesh(
        mesh: &crate::mesh_bindings::NetMesh,
        pin_store_path: Option<String>,
        delegation: Option<(SdkIdentity, Vec<u8>)>,
        payment_config: Option<PaymentConfig>,
    ) -> PyResult<Self> {
        // Mirror `DaemonRuntime`: reuse the live node + its runtime so the
        // gateway drives mesh I/O on the same scheduler the node runs on.
        let node = mesh.node_arc_clone()?;
        let channel_configs = mesh.channel_configs_arc();
        let runtime = mesh.runtime_arc();
        let sdk_mesh = Arc::new(SdkMesh::from_node_arc(node, channel_configs, None));
        let mut gateway = MeshGateway::new(sdk_mesh.clone());
        if let Some((leaf, chain_bytes)) = delegation {
            // Phase 3 Slice B2: sign + attach the delegation chain on every
            // invoke, so a provider running a `DelegationGate` admits by
            // verified delegation (not the spoofable owner-scope origin) and
            // audits this gateway's leaf.
            gateway = gateway.with_delegation(Arc::new(DelegationSigner::new(leaf, chain_bytes)));
        }
        // Retain the store path + profile before `build_payment_flow`
        // consumes the config, so the approval verbs can reopen it.
        let (spend_policy_path, spend_profile) = match &payment_config {
            Some(c) => (Some(c.policy_path.clone()), c.profile.clone()),
            None => (None, "production".to_string()),
        };
        let payment = build_payment_flow(sdk_mesh, payment_config)?;
        Ok(Self {
            gateway: Arc::new(gateway),
            consent: Arc::new(ConsentPolicy::new()),
            pin_store_path: pin_store_path.map(PathBuf::from),
            payment,
            spend_policy_path,
            spend_profile,
            runtime,
        })
    }
}

/// Build the caller payment flow from the payment kwargs (doctrine #1:
/// zero decisions here — the flow and spend engine own them; this maps
/// config strings to constructors).
///
/// The payment identity **is the node's mesh identity**: quotes are
/// issued to, spend is tracked against, and invocation bindings are
/// signed by the same ed25519 identity peers see on the mesh. The
/// keypair is borrowed in-process from the node — nothing crosses the
/// language boundary.
#[cfg(feature = "payments")]
fn build_payment_flow(
    mesh: Arc<SdkMesh>,
    config: Option<PaymentConfig>,
) -> PyResult<Option<Arc<dyn net_mcp::serve::PaymentFlow>>> {
    use net_payments::flow::mesh::MeshPaymentChannel;
    use net_payments::flow::{CallerPaymentFlow, SystemClock};
    use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};

    let Some(config) = config else {
        return Ok(None);
    };
    // Vocabulary lives once in core (`SpendProfile::parse`), so the node and
    // python bindings cannot drift on the profile spellings.
    let profile =
        SpendProfile::parse(&config.profile).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let caller = Arc::new(mesh.entity_keypair().clone());
    // The v1 default registry (mock + the survey networks). A superset
    // of P0's mock-only default: real networks stay unspendable until
    // the operator lists them in the spend policy's `allowed_networks`
    // AND configures a signer — the registry is the asset allowlist,
    // never the enablement switch.
    let registry = net_payments::core::registry::default_registry_v1(caller.entity_id().clone());
    let spend = SpendPolicyEngine::new(&config.policy_path, profile)
        .with_unsafe_mock_auto_allow(config.unsafe_mock_auto_allow);
    let mut flow = CallerPaymentFlow::new(
        caller,
        spend,
        registry,
        Arc::new(MeshPaymentChannel::new(mesh)),
        Arc::new(SystemClock),
    );
    if let Some((address, callable)) = config.signer {
        flow = flow.with_signer("eip155", python_external_signer(address, callable));
    }
    if let Some((address, callable)) = config.signer_svm {
        flow = flow.with_signer("solana", python_svm_signer(address, callable));
    }
    if let Some((address, callable)) = config.signer_xrpl {
        flow = flow.with_signer("xrpl", python_xrpl_signer(address, callable));
    }
    Ok(Some(Arc::new(flow)))
}

/// Bridge a Python signing callable into the payments
/// [`ExternalSigner`](net_payments::flow::signer::ExternalSigner) shape.
///
/// The callable receives the full `eth_signTypedData_v4` document as a
/// JSON string — a policy-bearing wallet can inspect the amount and
/// recipient it is authorizing — and returns the 65-byte `r‖s‖v`
/// signature as `0x…` hex. There is no raw-bytes path: the *only* thing
/// this surface can ask Python to sign is a logged, typed transfer
/// authorization ("no arbitrary signing oracle"). Invoked via
/// `spawn_blocking` + `Python::attach` (the blob-store FFI pattern) so
/// GIL acquisition never stalls the mesh reactor.
#[cfg(feature = "payments")]
pub(crate) fn python_external_signer(
    address: String,
    callable: pyo3::Py<pyo3::PyAny>,
) -> Arc<dyn net_payments::flow::signer::SchemeSigner> {
    use net_payments::flow::signer::{ExternalSigner, SignerError};

    let callable = Arc::new(callable);
    Arc::new(ExternalSigner::new(address, move |typed| {
        let callable = callable.clone();
        Box::pin(async move {
            let typed_json = typed.to_string();
            let signed = tokio::task::spawn_blocking(move || {
                Python::attach(|py| -> Result<String, String> {
                    let out = callable
                        .bind(py)
                        .call1((typed_json,))
                        .map_err(|e| format!("payment signer raised: {e}"))?;
                    out.extract::<String>().map_err(|e| {
                        format!("payment signer must return the 0x-hex signature string: {e}")
                    })
                })
            })
            .await
            .map_err(|e| SignerError::new(format!("payment signer task: {e}")))?;
            signed.map_err(SignerError::new)
        })
    }))
}

/// Bridge a Python signing callable into the
/// [`ExternalSvmSigner`](net_payments::flow::signer::ExternalSvmSigner)
/// shape (registered under the `solana` namespace).
///
/// Same doctrine as the eip155 seam: the callable receives the typed
/// `SvmTransferIntent` as a JSON string — a policy-bearing wallet inspects
/// the amount, mint, and recipient it is authorizing — and returns the
/// base64 partially-signed versioned transaction. No raw-bytes path; the
/// wallet owns the key + the SPL transaction machinery. `spawn_blocking` +
/// `Python::attach` so GIL acquisition never stalls the reactor.
#[cfg(feature = "payments")]
pub(crate) fn python_svm_signer(
    address: String,
    callable: pyo3::Py<pyo3::PyAny>,
) -> Arc<dyn net_payments::flow::signer::SchemeSigner> {
    use net_payments::flow::signer::{ExternalSvmSigner, SignerError};

    let callable = Arc::new(callable);
    Arc::new(ExternalSvmSigner::new(address, move |intent| {
        let callable = callable.clone();
        Box::pin(async move {
            let intent_json = serde_json::to_string(&intent)
                .map_err(|e| SignerError::new(format!("svm transfer intent serialize: {e}")))?;
            let signed = tokio::task::spawn_blocking(move || {
                Python::attach(|py| -> Result<String, String> {
                    let out = callable
                        .bind(py)
                        .call1((intent_json,))
                        .map_err(|e| format!("svm payment signer raised: {e}"))?;
                    out.extract::<String>().map_err(|e| {
                        format!("svm payment signer must return the base64 signed transaction: {e}")
                    })
                })
            })
            .await
            .map_err(|e| SignerError::new(format!("svm payment signer task: {e}")))?;
            signed.map_err(SignerError::new)
        })
    }))
}

/// Bridge a Python signing callable into the
/// [`ExternalXrplSigner`](net_payments::flow::signer::ExternalXrplSigner)
/// shape (registered under the `xrpl` namespace).
///
/// The callable receives the typed `XrplPaymentIntent` as a JSON string and
/// returns the hex presigned `Payment` blob. Retry honesty is the wallet's
/// contract (a same-quote retry re-presents the identical blob); here we
/// only marshal typed intent in, hex artifact out — no raw-bytes path.
#[cfg(feature = "payments")]
pub(crate) fn python_xrpl_signer(
    address: String,
    callable: pyo3::Py<pyo3::PyAny>,
) -> Arc<dyn net_payments::flow::signer::SchemeSigner> {
    use net_payments::flow::signer::{ExternalXrplSigner, SignerError};

    let callable = Arc::new(callable);
    Arc::new(ExternalXrplSigner::new(address, move |intent| {
        let callable = callable.clone();
        Box::pin(async move {
            let intent_json = serde_json::to_string(&intent)
                .map_err(|e| SignerError::new(format!("xrpl payment intent serialize: {e}")))?;
            let signed = tokio::task::spawn_blocking(move || {
                Python::attach(|py| -> Result<String, String> {
                    let out = callable
                        .bind(py)
                        .call1((intent_json,))
                        .map_err(|e| format!("xrpl payment signer raised: {e}"))?;
                    out.extract::<String>().map_err(|e| {
                        format!("xrpl payment signer must return the hex signed Payment blob: {e}")
                    })
                })
            })
            .await
            .map_err(|e| SignerError::new(format!("xrpl payment signer task: {e}")))?;
            signed.map_err(SignerError::new)
        })
    }))
}

/// Without the `payments` build feature, payment kwargs are a loud
/// config error — never a silently free paid capability.
#[cfg(not(feature = "payments"))]
fn build_payment_flow(
    _mesh: Arc<SdkMesh>,
    config: Option<PaymentConfig>,
) -> PyResult<Option<Arc<dyn net_mcp::serve::PaymentFlow>>> {
    match config {
        Some(_) => Err(PyValueError::new_err(
            "this build lacks the `payments` feature; payment_policy_path is unavailable",
        )),
        None => Ok(None),
    }
}

/// A Python awaitable that resolves immediately to `s` (for pre-flight errors
/// that never touch the mesh).
fn immediate(py: Python<'_>, s: String) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(s) })
}

/// Aborts the mesh-runtime task if its owning future is dropped before the task
/// finishes — i.e. the Python awaitable was cancelled (`asyncio.wait_for` /
/// `task.cancel()`). Dropping a `JoinHandle` only *detaches* the task, so
/// without this the spawned mesh op would keep running after the caller stopped
/// awaiting. Aborting an already-finished task is a documented no-op, so the
/// guard is safe to hold across the happy path too.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Bridge a mesh-runtime `JoinHandle<String>` to a Python awaitable. The task
/// ran on the mesh's runtime; awaiting the handle here (on the process-global
/// `future_into_py` runtime) is a plain channel await — no reactor affinity.
///
/// **Cancellation.** The gateway spawns on the mesh's own runtime for reactor
/// affinity, so the vanilla `async_bridge::await_with_cancel` (which runs on the
/// global runtime) doesn't fit. Instead an [`AbortOnDrop`] guard rides inside
/// the wrapper future: a Python `task.cancel()` drops the future, aborting the
/// mesh-runtime task — matching the client-side-cancel semantics `await_substrate`
/// documents. (A server-side CANCEL frame would need a cancel-token threaded
/// through `MeshGateway::{search,describe,invoke}` -> `CallOptions`, a deeper
/// substrate-surface follow-up.)
fn spawn_bridge(py: Python<'_>, join: JoinHandle<String>) -> PyResult<Bound<'_, PyAny>> {
    let abort = AbortOnDrop(join.abort_handle());
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let out = join
            .await
            .map_err(|e| PyRuntimeError::new_err(format!("gateway task failed: {e}")))?;
        // Completed — release the guard explicitly (abort would be a no-op now).
        drop(abort);
        Ok(out)
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
    #[pyo3(signature = (mesh, pin_store_path=None, delegation_leaf=None, delegation_chain=None, payment_policy_path=None, payment_profile=None, payment_unsafe_mock_auto_allow=false, payment_signer_address=None, payment_signer=None, payment_signer_svm_address=None, payment_signer_svm=None, payment_signer_xrpl_address=None, payment_signer_xrpl=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        mesh: &crate::mesh_bindings::NetMesh,
        pin_store_path: Option<String>,
        delegation_leaf: Option<&crate::identity::Identity>,
        delegation_chain: Option<Vec<u8>>,
        payment_policy_path: Option<String>,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: bool,
        payment_signer_address: Option<String>,
        payment_signer: Option<Bound<'_, PyAny>>,
        payment_signer_svm_address: Option<String>,
        payment_signer_svm: Option<Bound<'_, PyAny>>,
        payment_signer_xrpl_address: Option<String>,
        payment_signer_xrpl: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let delegation = build_delegation(delegation_leaf, delegation_chain)?;
        let payment = PaymentConfig::collect(
            payment_policy_path,
            payment_profile,
            payment_unsafe_mock_auto_allow,
            payment_signer_address,
            unbind_signer(payment_signer)?,
            payment_signer_svm_address,
            unbind_signer(payment_signer_svm)?,
            payment_signer_xrpl_address,
            unbind_signer(payment_signer_xrpl)?,
        )?;
        Ok(Self {
            state: GatewayState::from_mesh(mesh, pin_store_path, delegation, payment)?,
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
        py.detach(move || runtime.block_on(do_search(&h.gateway, &h.consent, &h.pin_path, &query)))
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
        // `null`/default `"{}"` normalizes to `{}` (a no-argument invocation, as
        // the gate does); arrays and primitives are a caller-shape error.
        let args = match normalize_invoke_args(arguments_json) {
            Ok(v) => v,
            Err(e) => return err_json("invalid_arguments", e),
        };
        let h = self.state.handles();
        let runtime = self.state.runtime.clone();
        py.detach(move || {
            runtime.block_on(do_invoke(
                &h.gateway,
                &h.consent,
                &h.pin_path,
                h.payment.as_deref(),
                id,
                args,
            ))
        })
    }

    /// Approve a held payment quote under operator policy, resolving a
    /// prior `requires_payment_approval` so the next `invoke` redeems it.
    /// Returns `{"status":"ok","quote_id":...,"changed":bool}` (`changed`
    /// is whether the record moved to approved), or a structured
    /// `no_payment_policy` / `error`. Operator surface — the model-reachable
    /// `invoke` only ever *requests* approval; this grants it.
    fn approve_payment(&self, py: Python<'_>, quote_id: &str) -> String {
        let runtime = self.state.runtime.clone();
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let quote_id = quote_id.to_string();
        py.detach(move || runtime.block_on(do_approve_payment(path, profile, quote_id)))
    }

    /// Reject/remove a payment approval record. Returns
    /// `{"status":"ok","quote_id":...,"changed":bool}` (`changed` is
    /// whether a record existed), or a structured error.
    fn reject_payment(&self, py: Python<'_>, quote_id: &str) -> String {
        let runtime = self.state.runtime.clone();
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let quote_id = quote_id.to_string();
        py.detach(move || runtime.block_on(do_reject_payment(path, profile, quote_id)))
    }

    /// The quote ids awaiting approval, for a consent UX to render. Returns
    /// `{"status":"ok","pending":[quote_id, ...]}`, or a structured error.
    fn pending_payments(&self, py: Python<'_>) -> String {
        let runtime = self.state.runtime.clone();
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        py.detach(move || runtime.block_on(do_pending_payments(path, profile)))
    }

    /// Today's reserved spend total for a `(network, x402 asset)` pair, as
    /// the canonical atomic-amount string. Returns
    /// `{"status":"ok","network":...,"asset":...,"spent":"<atomic>"}`, or a
    /// structured error.
    fn spent_today(&self, py: Python<'_>, network: &str, asset: &str) -> String {
        let runtime = self.state.runtime.clone();
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let network = network.to_string();
        let asset = asset.to_string();
        py.detach(move || runtime.block_on(do_spent_today(path, profile, network, asset)))
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
    #[pyo3(signature = (mesh, pin_store_path=None, delegation_leaf=None, delegation_chain=None, payment_policy_path=None, payment_profile=None, payment_unsafe_mock_auto_allow=false, payment_signer_address=None, payment_signer=None, payment_signer_svm_address=None, payment_signer_svm=None, payment_signer_xrpl_address=None, payment_signer_xrpl=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        mesh: &crate::mesh_bindings::NetMesh,
        pin_store_path: Option<String>,
        delegation_leaf: Option<&crate::identity::Identity>,
        delegation_chain: Option<Vec<u8>>,
        payment_policy_path: Option<String>,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: bool,
        payment_signer_address: Option<String>,
        payment_signer: Option<Bound<'_, PyAny>>,
        payment_signer_svm_address: Option<String>,
        payment_signer_svm: Option<Bound<'_, PyAny>>,
        payment_signer_xrpl_address: Option<String>,
        payment_signer_xrpl: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let delegation = build_delegation(delegation_leaf, delegation_chain)?;
        let payment = PaymentConfig::collect(
            payment_policy_path,
            payment_profile,
            payment_unsafe_mock_auto_allow,
            payment_signer_address,
            unbind_signer(payment_signer)?,
            payment_signer_svm_address,
            unbind_signer(payment_signer_svm)?,
            payment_signer_xrpl_address,
            unbind_signer(payment_signer_xrpl)?,
        )?;
        Ok(Self {
            state: GatewayState::from_mesh(mesh, pin_store_path, delegation, payment)?,
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
        let join = self
            .state
            .runtime
            .spawn(async move { do_search(&h.gateway, &h.consent, &h.pin_path, &query).await });
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
        // `null`/default `"{}"` normalizes to `{}` (a no-argument invocation, as
        // the gate does); arrays and primitives are a caller-shape error.
        let args = match normalize_invoke_args(arguments_json) {
            Ok(v) => v,
            Err(e) => return immediate(py, err_json("invalid_arguments", e)),
        };
        let h = self.state.handles();
        let join = self.state.runtime.spawn(async move {
            do_invoke(
                &h.gateway,
                &h.consent,
                &h.pin_path,
                h.payment.as_deref(),
                id,
                args,
            )
            .await
        });
        spawn_bridge(py, join)
    }

    /// Awaitable :meth:`CapabilityGateway.approve_payment`.
    fn approve_payment<'py>(&self, py: Python<'py>, quote_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let quote_id = quote_id.to_string();
        let join = self
            .state
            .runtime
            .spawn(async move { do_approve_payment(path, profile, quote_id).await });
        spawn_bridge(py, join)
    }

    /// Awaitable :meth:`CapabilityGateway.reject_payment`.
    fn reject_payment<'py>(&self, py: Python<'py>, quote_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let quote_id = quote_id.to_string();
        let join = self
            .state
            .runtime
            .spawn(async move { do_reject_payment(path, profile, quote_id).await });
        spawn_bridge(py, join)
    }

    /// Awaitable :meth:`CapabilityGateway.pending_payments`.
    fn pending_payments<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let join = self
            .state
            .runtime
            .spawn(async move { do_pending_payments(path, profile).await });
        spawn_bridge(py, join)
    }

    /// Awaitable :meth:`CapabilityGateway.spent_today`.
    fn spent_today<'py>(
        &self,
        py: Python<'py>,
        network: &str,
        asset: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let path = self.state.spend_policy_path.clone();
        let profile = self.state.spend_profile.clone();
        let network = network.to_string();
        let asset = asset.to_string();
        let join = self
            .state
            .runtime
            .spawn(async move { do_spent_today(path, profile, network, asset).await });
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
    payment: Option<Arc<dyn net_mcp::serve::PaymentFlow>>,
}

impl GatewayState {
    fn handles(&self) -> GatewayHandles {
        GatewayHandles {
            gateway: self.gateway.clone(),
            consent: self.consent.clone(),
            pin_path: self.pin_store_path.clone(),
            payment: self.payment.clone(),
        }
    }

    fn repr(&self, name: &str) -> String {
        match &self.pin_store_path {
            Some(p) => format!("{name}(pin_store={:?})", p.display()),
            None => format!("{name}(pin_store=None)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Contract tests — the structured-JSON passthrough is the binding's whole
// job, so its shape is pinned here (the Python-level twin lives in
// tests/test_capability_gateway.py).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The payment gate's structured error passes through untouched:
    /// same fields the Rust spend engine produced, plus the status
    /// discriminant and the capability id — nothing re-decided here.
    #[test]
    fn requires_payment_approval_passes_through_untouched() {
        let id = CapabilityId::parse("42/fixture-tool").unwrap();
        let json = outcome_to_json(
            &id,
            GatedOutcome::RequiresPaymentApproval {
                quote_id: "q-77".to_string(),
                policy_reason: "amount 2500 exceeds max_per_call 1000".to_string(),
                approve_hint: "approve quote q-77 via the payments consent API".to_string(),
            },
        );
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["status"], "requires_payment_approval");
        assert_eq!(v["cap_id"], "42/fixture-tool");
        assert_eq!(v["quote_id"], "q-77");
        assert_eq!(v["policy_reason"], "amount 2500 exceeds max_per_call 1000");
        assert_eq!(
            v["approve_hint"],
            "approve quote q-77 via the payments consent API"
        );
    }

    /// `spend_engine` routes the profile through the one core parser and does
    /// NOT silently default: an unknown profile is a loud error, never a quiet
    /// fall back to `Production` (which would contradict `SpendProfile::parse`'s
    /// no-silent-fallback contract). Construction validates the profile first,
    /// so this error is unreachable in practice — the test pins that the fallback
    /// stays fail-loud, not fail-silent.
    ///
    /// Gated on `payments`: `spend_engine` has no payments-off variant (unlike
    /// the `do_*` verbs), so it only exists under the feature.
    #[cfg(feature = "payments")]
    #[test]
    fn spend_engine_rejects_an_unknown_profile() {
        assert!(spend_engine("/tmp/net-spend-test", "production").is_ok());
        assert!(spend_engine("/tmp/net-spend-test", "dev_test").is_ok());
        assert!(spend_engine("/tmp/net-spend-test", "yolo").is_err());
    }

    /// Payment kwargs are validated before any mesh state exists:
    /// profile/unsafe/signer without a policy path is a config error,
    /// and a signer address without its callable is incomplete. (The
    /// callable-present arms need a live interpreter and are covered
    /// by the pytest twin.)
    #[test]
    fn payment_kwargs_require_the_policy_path() {
        // Terse helper: the common all-None-signers call shape.
        let collect = |path: Option<&str>, profile: Option<&str>, unsafe_flag: bool| {
            PaymentConfig::collect(
                path.map(Into::into),
                profile.map(Into::into),
                unsafe_flag,
                None,
                None,
                None,
                None,
                None,
                None,
            )
        };
        assert!(collect(None, None, false).unwrap().is_none());
        assert!(collect(None, Some("dev_test"), false).is_err());
        assert!(collect(None, None, true).is_err());
        // A signer reference is half of a pair: address alone is a caller
        // error even with a policy path present — for every scheme.
        assert!(PaymentConfig::collect(
            Some("/tmp/p.json".into()),
            None,
            false,
            Some("0xpayer".into()),
            None,
            None,
            None,
            None,
            None,
        )
        .is_err());
        assert!(PaymentConfig::collect(
            Some("/tmp/p.json".into()),
            None,
            false,
            None,
            None,
            Some("svm-payer".into()),
            None,
            None,
            None,
        )
        .is_err());
        assert!(PaymentConfig::collect(
            Some("/tmp/p.json".into()),
            None,
            false,
            None,
            None,
            None,
            None,
            Some("r-payer".into()),
            None,
        )
        .is_err());
        // An svm/xrpl signer without a policy path is a config error too.
        assert!(
            PaymentConfig::collect(None, None, false, None, None, None, None, None, None,)
                .unwrap()
                .is_none()
        );
        let c = collect(Some("/tmp/p.json"), None, false).unwrap().unwrap();
        assert_eq!(c.profile, "production", "fail-closed default profile");
        assert!(c.signer.is_none() && c.signer_svm.is_none() && c.signer_xrpl.is_none());
    }

    /// A denied outcome projects the provider's failure schematic as a
    /// `failure` object beside the `error` string — never instead of it;
    /// a schematic-less denial is exactly the pre-schematic shape.
    #[test]
    fn a_denied_outcome_projects_the_failure_schematic() {
        let id = CapabilityId::parse("prov/tool").expect("cap id");
        let schematic = net_sdk::tool_payment::FailureSchematic::missing_quote("tool");
        let denied = GatedOutcome::Failed(GatewayError::Denied {
            message: "paid tool invoked without a payment quote header".into(),
            schematic: Some(Box::new(schematic)),
        });
        let v: serde_json::Value =
            serde_json::from_str(&outcome_to_json(&id, denied)).expect("json");
        assert_eq!(v["status"], "denied");
        assert!(v["error"].as_str().unwrap().contains("payment quote"));
        assert_eq!(v["failure"]["reason"], "missing_quote");
        assert_eq!(v["failure"]["object"], "net.payment.failure@1");

        let plain = GatedOutcome::Failed(GatewayError::denied("owner scope"));
        let v: serde_json::Value =
            serde_json::from_str(&outcome_to_json(&id, plain)).expect("json");
        assert_eq!(v["status"], "denied");
        assert!(v.get("failure").is_none(), "no schematic, no failure field");
    }

    /// A served invocation projects `status:"ok"` with the tool result —
    /// the driven-success branch of `outcome_to_json` (the constructed
    /// twin of what the paid e2e below asserts over the wire).
    #[test]
    fn an_invoked_outcome_projects_status_ok() {
        let id = CapabilityId::parse("prov/tool").expect("cap id");
        let mut result = net_mcp::spec::CallToolResult::text_ok("42");
        result.structured_content = Some(json!({ "sum": 42 }));
        let v: serde_json::Value =
            serde_json::from_str(&outcome_to_json(&id, GatedOutcome::Invoked(result)))
                .expect("json");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["text"], "42");
        assert_eq!(v["structured_content"]["sum"], 42);
    }
}

/// The driven paid invoke through the **actual Python demand surface**
/// (M3 of `docs/plans/PAYMENTS_TEST_MATRIX.md`): `build_payment_flow` →
/// `do_invoke` (`gated_invoke` over a real `MeshGateway`) →
/// `outcome_to_json`, exactly the composition `PyCapabilityGateway.invoke`
/// wraps, against a real two-node paid provider.
///
/// The other e2es prove the *provider* gate over the wire
/// (`mesh_paid_capability_e2e`, `mcp_wrap_paid_e2e`); this proves the
/// *demand* side the binding owns: it discovers the announced pricing,
/// runs the caller flow the Python kwargs build, invokes through the
/// gateway, and projects the outcome to the status-JSON a Python caller
/// reads. No Python interpreter is touched (`signer = None`), so it runs
/// as a plain Rust test under `--features payments`.
#[cfg(all(test, feature = "payments"))]
mod paid_invoke_e2e {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use net::adapter::net::identity::EntityKeypair;
    use net_mcp::spec::{CallToolResult, Implementation, Tool};
    use net_mcp::wrap::{
        CredentialStatus, LoweringContext, McpError, ServerPublisher, Substitutability,
        ToolInvoker, WrapConfig,
    };
    use net_payments::core::canonical::canonical_bytes;
    use net_payments::core::registry::default_registry_v1;
    use net_payments::core::terms::PricingTerms;
    use net_payments::engine::{AdmitAll, PaymentEngine};
    use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
    use net_payments::flow::mcp_gate::EnginePaymentAdmission;
    use net_payments::flow::mesh::serve_payments;
    use net_payments::flow::{Clock, InProcessProvider, SystemClock};
    use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
    use net_payments::x402::requirements::PaymentRequirements;
    use net_payments::x402::X402Carry;
    use net_sdk::mesh::MeshBuilder;

    /// The provider's wrapped tool: `add` sums two integers. A counter
    /// proves it runs only on the paid, admitted invokes.
    #[derive(Default)]
    struct AddTool {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ToolInvoker for AddTool {
        async fn call_tool(
            &self,
            name: &str,
            arguments: Value,
        ) -> Result<CallToolResult, McpError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let a = arguments.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = arguments.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
            let _ = name;
            Ok(CallToolResult::text_ok((a + b).to_string()))
        }
    }

    async fn handshake(server: &SdkMesh, caller: &SdkMesh) {
        let server_addr = server.inner().local_addr();
        let server_pub = *server.inner().public_key();
        let server_id = server.inner().node_id();
        let caller_id = caller.inner().node_id();
        let (accept, connect) = tokio::join!(server.inner().accept(caller_id), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            caller
                .inner()
                .connect(server_addr, &server_pub, server_id)
                .await
        });
        accept.expect("accept");
        connect.expect("connect");
        server.inner().start();
        caller.inner().start();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_python_surface_drives_a_paid_invoke_and_projects_the_outcome() {
        let psk = [0x63u8; 32];
        let dir = tempfile::tempdir().expect("tempdir");

        // ── two real nodes ─────────────────────────────────────────
        let provider_mesh = Arc::new(
            MeshBuilder::new("127.0.0.1:0", &psk)
                .expect("builder")
                .build()
                .await
                .expect("provider mesh"),
        );
        let caller_mesh = Arc::new(
            MeshBuilder::new("127.0.0.1:0", &psk)
                .expect("builder")
                .build()
                .await
                .expect("caller mesh"),
        );
        handshake(&provider_mesh, &caller_mesh).await;
        let provider_id = provider_mesh.inner().node_id();

        // ── provider: engine behind the payment wire + the wrap gate ─
        let provider_keys = Arc::new(EntityKeypair::generate());
        // `build_payment_flow` uses `default_registry_v1`; the provider
        // matches it (a superset of the mock registry, so a mock-scheme
        // quote validates on both sides).
        let registry = default_registry_v1(provider_keys.entity_id().clone());
        let provider_log = Arc::new(net_payments::billing::BillingLog::new(
            dir.path().join("provider-billing.jsonl"),
        ));
        let engine = Arc::new(
            PaymentEngine::new(
                provider_keys.clone(),
                Arc::new(MockFacilitator::new()),
                Arc::new(AdmitAll),
                registry.clone(),
                dir.path().join("engine.json"),
            )
            .expect("engine")
            .with_billing_log(provider_log.clone()),
        );
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let in_process = Arc::new(InProcessProvider::new(engine.clone(), clock));
        let _payments = serve_payments(&provider_mesh, in_process).expect("serve payments");

        // The capability id the whole demand path keys on: describe by
        // it, pay `id.display()`, redeem `id.capability`.
        let id = CapabilityId::parse(&format!("{provider_id}/add")).expect("cap id");

        // The announced pricing the caller will discover (the FULL terms
        // with the mock template — `gated_invoke` fetches this via
        // `describe` and hands it to the flow).
        let template = X402Carry::author(&PaymentRequirements {
            scheme: MOCK_SCHEME.into(),
            network: MOCK_NETWORK.into(),
            amount: "2500".into(),
            asset: "musd".into(),
            pay_to: "mock-provider-settle-addr".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .expect("author");
        let terms = PricingTerms::new(
            provider_keys.entity_id().clone(),
            id.display(),
            vec![template],
            registry.reference().expect("registry reference"),
        );
        let terms_json =
            String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

        // Publish the priced tool through the wrap path, gated by the
        // real engine admission — the announced pricing is the full
        // terms so the demand-side describe returns something payable.
        let admission = Arc::new(EnginePaymentAdmission::new(engine.clone()));
        let mut config = WrapConfig::owner_only(
            Implementation {
                name: "payments-e2e".to_string(),
                version: "1.0".to_string(),
            },
            caller_mesh.origin_hash(),
        );
        config.pricing.insert("add".to_string(), terms_json);
        config.payment_admission = Some(admission);
        let invoker = Arc::new(AddTool::default());
        let publisher = ServerPublisher::new(Arc::clone(&provider_mesh));
        let _publication = publisher
            .publish_tools(
                &[Tool {
                    name: "add".to_string(),
                    title: None,
                    description: Some("Sum two integers.".to_string()),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }
                    }),
                    output_schema: None,
                }],
                invoker.clone() as Arc<dyn ToolInvoker>,
                LoweringContext {
                    server_version: "1.0".to_string(),
                    credential_status: CredentialStatus::None,
                    substitutability: Substitutability::ProviderLocal,
                    pricing: Default::default(),
                },
                config,
            )
            .await
            .expect("publish the priced wrapped tool");

        // ── caller: the EXACT Python demand surface ────────────────
        // `build_payment_flow` is what the `payment_*` kwargs construct;
        // `MeshGateway` is what the constructor builds; `do_invoke` is
        // what `PyCapabilityGateway.invoke` calls.
        let policy_path = dir.path().join("spend-policy.json");
        let config = PaymentConfig {
            policy_path: policy_path.to_string_lossy().into_owned(),
            profile: "dev_test".to_string(),
            unsafe_mock_auto_allow: false,
            signer: None,
            signer_svm: None,
            signer_xrpl: None,
        };
        let flow = build_payment_flow(caller_mesh.clone(), Some(config))
            .expect("build the payment flow")
            .expect("a flow, since a config was supplied");
        let gateway = MeshGateway::new(caller_mesh.clone());
        // Consent is the gate *before* payment: allow the capability so
        // the flow under test is the payment path, not the pin prompt.
        let mut consent = ConsentPolicy::new();
        consent.allow(id.clone());
        let args = json!({ "a": 2, "b": 3 });

        // (1) Paid invoke, DevTest auto-allow → the Python status-JSON
        //     reports `ok` with the served result.
        let mut ok = String::new();
        for _ in 0..5 {
            ok = do_invoke(
                &gateway,
                &consent,
                &None,
                Some(flow.as_ref()),
                id.clone(),
                args.clone(),
            )
            .await;
            if serde_json::from_str::<Value>(&ok)
                .ok()
                .map(|v| v["status"] == "ok")
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let v: Value = serde_json::from_str(&ok).expect("json");
        assert_eq!(v["status"], "ok", "the paid invoke projects ok: {ok}");
        assert_eq!(v["text"], "5", "the served result rides the projection");
        assert_eq!(
            invoker.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the invoker ran once for the paid call"
        );
        assert_eq!(
            provider_log.read_all().await.expect("read").len(),
            1,
            "one paid serve, one billing event"
        );

        // (2) Tighten the spend cap below the price → the flow holds and
        //     the Python surface projects `requires_payment_approval`
        //     with the quote id + approve hint (the loop's first half).
        let configurer = SpendPolicyEngine::new(&policy_path, SpendProfile::DevTest);
        configurer
            .configure(|defaults, _| {
                defaults.max_per_call =
                    Some(net_payments::core::units::AtomicAmount::from_u128(1000));
            })
            .await
            .expect("configure");
        let held = do_invoke(
            &gateway,
            &consent,
            &None,
            Some(flow.as_ref()),
            id.clone(),
            args.clone(),
        )
        .await;
        let v: Value = serde_json::from_str(&held).expect("json");
        assert_eq!(
            v["status"], "requires_payment_approval",
            "an over-cap invoke holds for approval: {held}"
        );
        let quote_id = v["quote_id"].as_str().expect("a held quote id").to_string();
        assert!(
            v["approve_hint"].is_string(),
            "the projection hints how to approve"
        );
        assert_eq!(
            invoker.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a held invoke never reaches the tool"
        );

        // (3) Approve the held quote → the retry projects `ok` (the
        //     loop's second half, entirely through the binding's JSON).
        configurer.approve(&quote_id).await.expect("approve");
        let mut approved = String::new();
        for _ in 0..5 {
            approved = do_invoke(
                &gateway,
                &consent,
                &None,
                Some(flow.as_ref()),
                id.clone(),
                args.clone(),
            )
            .await;
            if serde_json::from_str::<Value>(&approved)
                .ok()
                .map(|v| v["status"] == "ok")
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let v: Value = serde_json::from_str(&approved).expect("json");
        assert_eq!(
            v["status"], "ok",
            "approval unblocks the retry through the Python surface: {approved}"
        );
        assert_eq!(v["text"], "5");
        assert_eq!(
            invoker.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the approved retry served"
        );
    }
}

/// The operator approval verbs (WS-P1) drive `SpendPolicyEngine` over the
/// shared spend-policy store and project its typed results to the gateway's
/// status-JSON — the exact bodies `approve_payment` / `reject_payment` /
/// `pending_payments` / `spent_today` wrap. No mesh, no interpreter; a plain
/// Rust test under `--features payments`.
#[cfg(all(test, feature = "payments"))]
mod approval_verbs {
    use super::*;

    fn policy_path(dir: &tempfile::TempDir) -> String {
        dir.path()
            .join("spend-policy.json")
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn the_verbs_marshal_the_spend_store_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = policy_path(&dir);
        let profile = "dev_test".to_string();

        // Fresh store: nothing pending, nothing spent today.
        let v: Value =
            serde_json::from_str(&do_pending_payments(Some(path.clone()), profile.clone()).await)
                .expect("json");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["pending"].as_array().expect("array").len(), 0);

        let v: Value = serde_json::from_str(
            &do_spent_today(
                Some(path.clone()),
                profile.clone(),
                "mock:net".into(),
                "musd".into(),
            )
            .await,
        )
        .expect("json");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["spent"], "0");

        // Approve a quote id: the record moves to approved (changed), and a
        // second approve is idempotent (no change).
        let v: Value = serde_json::from_str(
            &do_approve_payment(Some(path.clone()), profile.clone(), "q-1".into()).await,
        )
        .expect("json");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["quote_id"], "q-1");
        assert_eq!(v["changed"], true);
        let v: Value = serde_json::from_str(
            &do_approve_payment(Some(path.clone()), profile.clone(), "q-1".into()).await,
        )
        .expect("json");
        assert_eq!(v["changed"], false, "re-approve is a no-op");

        // Reject removes it (changed), then a second reject is a no-op.
        let v: Value = serde_json::from_str(
            &do_reject_payment(Some(path.clone()), profile.clone(), "q-1".into()).await,
        )
        .expect("json");
        assert_eq!(v["changed"], true);
        let v: Value = serde_json::from_str(
            &do_reject_payment(Some(path.clone()), profile.clone(), "q-1".into()).await,
        )
        .expect("json");
        assert_eq!(v["changed"], false, "re-reject is a no-op");
    }

    #[tokio::test]
    async fn the_verbs_without_a_policy_path_report_no_payment_policy() {
        let v: Value = serde_json::from_str(&do_pending_payments(None, "production".into()).await)
            .expect("json");
        assert_eq!(v["status"], "no_payment_policy");
        let v: Value =
            serde_json::from_str(&do_approve_payment(None, "production".into(), "q".into()).await)
                .expect("json");
        assert_eq!(v["status"], "no_payment_policy");
    }
}
