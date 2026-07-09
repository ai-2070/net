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

use napi::bindgen_prelude::{Function, Promise};
use napi::{Error, Result};
use napi_derive::napi;
use parking_lot::Mutex;
use serde_json::{json, Value};

// The gateway trait's `search`/`describe` are methods on `MeshGateway`; bring
// the trait into scope anonymously so they resolve without colliding with this
// module's `CapabilityGateway` napi class.
use net_mcp::serve::CapabilityGateway as _;
use net_mcp::serve::{
    gated_invoke, CapabilityDetail, CapabilityId, ConsentPolicy, GatedOutcome, GatewayError,
    MeshGateway, PaymentFlow, PinStore,
};
use net_payments::core::registry::default_registry_v1;
use net_payments::flow::mesh::MeshPaymentChannel;
use net_payments::flow::{CallerPaymentFlow, Clock, SystemClock};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
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

/// Parse and normalize the `invoke` arguments string to a JSON *object*.
///
/// A JSON `null` (or an omitted `arguments`, which the caller already mapped to
/// `{}`) is a no-argument invocation — normalized to `{}`, exactly as the SDK's
/// [`gated_invoke`] does at the one chokepoint every demand-side caller routes
/// through. Arrays and primitives are still a caller-shape error: the gateway
/// surface requires an object, and the core does not accept arbitrary-typed
/// args. `Err` carries the human message for an `invalid_arguments` status.
///
/// The twin of the Python gateway's `normalize_invoke_args`, so the two
/// surfaces cannot drift.
fn normalize_invoke_args(raw: &str) -> std::result::Result<Value, String> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| format!("arguments must be a JSON object: {e}"))?;
    let value = if value.is_null() { json!({}) } else { value };
    if !value.is_object() {
        return Err("arguments must be a JSON object".to_string());
    }
    Ok(value)
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
// Payment flow construction + the operator approval verbs. Mock-network paid
// capabilities work with no signer; real-network settlement needs the signer
// seam (a focused follow-up). Doctrine #1: every decision is the Rust flow's;
// this maps config strings to constructors.
// ---------------------------------------------------------------------------

/// Payment config collected from the constructor options.
struct PaymentConfig {
    policy_path: String,
    profile: String,
    unsafe_mock_auto_allow: bool,
}

/// A `paymentProfile` / unsafe flag without a `paymentPolicyPath` is a caller
/// error; the policy path is the shared spend-policy store the flow reserves
/// against.
fn collect_payment_config(
    policy_path: Option<String>,
    profile: Option<String>,
    unsafe_mock_auto_allow: bool,
) -> Result<Option<PaymentConfig>> {
    match policy_path {
        Some(policy_path) => Ok(Some(PaymentConfig {
            policy_path,
            profile: profile.unwrap_or_else(|| "production".to_string()),
            unsafe_mock_auto_allow,
        })),
        None if profile.is_some() || unsafe_mock_auto_allow => Err(Error::from_reason(
            "gateway: paymentProfile / paymentUnsafeMockAutoAllow require paymentPolicyPath \
             (the shared spend-policy store)",
        )),
        None => Ok(None),
    }
}

fn parse_profile(profile: &str) -> Result<SpendProfile> {
    match profile {
        "production" => Ok(SpendProfile::Production),
        "dev_test" | "dev-test" | "devtest" => Ok(SpendProfile::DevTest),
        other => Err(Error::from_reason(format!(
            "gateway: unknown paymentProfile {other:?} (expected \"production\" or \"dev_test\")"
        ))),
    }
}

/// A collected per-scheme signer: `(namespace, signer)`.
type Signer = (
    &'static str,
    Arc<dyn net_payments::flow::signer::SchemeSigner>,
);

/// Validate a signer pair (both-or-neither) and, when present, convert the JS
/// callback to a `ThreadsafeFunction` and wrap it in the scheme's external
/// signer. `build` is the scheme-specific wrapper (eip155 / svm / xrpl).
fn signer_pair(
    address: Option<String>,
    callback: Option<Function<'static, String, Promise<String>>>,
    namespace: &'static str,
    kwarg: &str,
    build: fn(
        String,
        crate::payment_signer::SignerTsfn,
    ) -> Arc<dyn net_payments::flow::signer::SchemeSigner>,
) -> Result<Option<Signer>> {
    match (address, callback) {
        (Some(addr), Some(cb)) => {
            let tsfn = cb.build_threadsafe_function().build()?;
            Ok(Some((namespace, build(addr, tsfn))))
        }
        (None, None) => Ok(None),
        _ => Err(Error::from_reason(format!(
            "gateway: {kwarg} and {kwarg}Address must be provided together \
             (the address names the payer; the callback signs its typed intent)"
        ))),
    }
}

/// Build the caller payment flow. The payment identity **is the node's mesh
/// identity** (`mesh.entity_keypair()`, borrowed in-process — nothing crosses
/// the language boundary). Real (non-mock) networks need a per-scheme
/// `signer`; without one, a real-network `accepts[]` entry is a structured
/// `denied`, never a fallback.
fn build_payment_flow(
    mesh: Arc<SdkMesh>,
    config: Option<PaymentConfig>,
    signers: Vec<Signer>,
) -> Result<Option<Arc<dyn PaymentFlow>>> {
    let Some(config) = config else {
        if !signers.is_empty() {
            return Err(Error::from_reason(
                "gateway: payment signers require paymentPolicyPath (the shared spend-policy store)",
            ));
        }
        return Ok(None);
    };
    let profile = parse_profile(&config.profile)?;
    let caller = Arc::new(mesh.entity_keypair().clone());
    let registry = default_registry_v1(caller.entity_id().clone());
    let spend = SpendPolicyEngine::new(&config.policy_path, profile)
        .with_unsafe_mock_auto_allow(config.unsafe_mock_auto_allow);
    let mut flow = CallerPaymentFlow::new(
        caller,
        spend,
        registry,
        Arc::new(MeshPaymentChannel::new(mesh)),
        Arc::new(SystemClock),
    );
    for (namespace, signer) in signers {
        flow = flow.with_signer(namespace, signer);
    }
    Ok(Some(Arc::new(flow)))
}

/// A `SpendPolicyEngine` for the operator verbs, keyed on the same store the
/// flow reserves against (the unsafe flag doesn't affect these verbs).
fn spend_engine(path: &str, profile: &str) -> SpendPolicyEngine {
    let profile = match profile {
        "dev_test" | "dev-test" | "devtest" => SpendProfile::DevTest,
        _ => SpendProfile::Production,
    };
    SpendPolicyEngine::new(path, profile)
}

fn no_policy_json() -> String {
    err_json(
        "no_payment_policy",
        "no spend policy configured — construct the gateway with paymentPolicyPath \
         (the machine-shared spend-policy store) to use the approval verbs",
    )
}

async fn do_approve_payment(
    policy_path: Option<String>,
    profile: String,
    quote_id: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    match spend_engine(&path, &profile).approve(&quote_id).await {
        Ok(changed) => {
            json!({ "status": "ok", "quote_id": quote_id, "changed": changed }).to_string()
        }
        Err(e) => err_json("error", e),
    }
}

async fn do_reject_payment(
    policy_path: Option<String>,
    profile: String,
    quote_id: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    match spend_engine(&path, &profile).reject(&quote_id).await {
        Ok(changed) => {
            json!({ "status": "ok", "quote_id": quote_id, "changed": changed }).to_string()
        }
        Err(e) => err_json("error", e),
    }
}

async fn do_pending_payments(policy_path: Option<String>, profile: String) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    match spend_engine(&path, &profile).pending().await {
        Ok(quotes) => json!({ "status": "ok", "pending": quotes }).to_string(),
        Err(e) => err_json("error", e),
    }
}

async fn do_spent_today(
    policy_path: Option<String>,
    profile: String,
    network: String,
    asset: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    let now_ns = SystemClock.now_ns();
    match spend_engine(&path, &profile)
        .spent_today(&network, &asset, now_ns)
        .await
    {
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
/// The node-holding handles — both the gateway (via `SdkMesh`) and the payment
/// flow (via `MeshPaymentChannel` → `SdkMesh`) retain a clone of the mesh node.
/// [`close`](CapabilityGateway::close) drops them so a JS caller can
/// deterministically release the node before `NetMesh.shutdown()`.
struct Live {
    gateway: Arc<MeshGateway>,
    /// The caller-side payment flow for paid capabilities. `None` = paid
    /// capabilities fail closed at the gate, per doctrine.
    payment: Option<Arc<dyn PaymentFlow>>,
}

#[napi]
pub struct CapabilityGateway {
    /// The node-holding handles, behind a lock so `close()` can drop them (and
    /// the retained mesh-node reference) from any thread while methods clone
    /// out. `None` once closed — a `#[napi]` class is GC-finalized, not
    /// scope-dropped, so without an explicit release the node clone would keep
    /// `NetMesh.shutdown()` (which needs sole ownership) failing until GC ran.
    live: Mutex<Option<Live>>,
    /// Config allowlist + in-memory pins. Empty in this first cut — the shared
    /// pin store is the source of approvals. Holds no node reference.
    consent: Arc<ConsentPolicy>,
    /// The machine-shared pin store path, reloaded fresh per call.
    pin_store_path: Option<PathBuf>,
    /// The spend-policy store path + profile, retained so the operator approval
    /// verbs reopen the same store the flow reserves against. `None` path =
    /// no `paymentPolicyPath` supplied; the verbs return `no_payment_policy`.
    /// Independent of the node, so the verbs keep working after `close()`.
    spend_policy_path: Option<String>,
    spend_profile: String,
}

/// The live handles cloned out of the lock for one call — the gateway plus the
/// optional payment flow.
type LiveHandles = (Arc<MeshGateway>, Option<Arc<dyn PaymentFlow>>);

impl CapabilityGateway {
    /// Clone the live handles out from behind the lock (so no guard crosses an
    /// `await`). `None` once [`close`](Self::close) has run.
    fn live_handles(&self) -> Option<LiveHandles> {
        self.live
            .lock()
            .as_ref()
            .map(|l| (l.gateway.clone(), l.payment.clone()))
    }
}

/// The structured result every live-path method resolves to once the gateway
/// has been closed — a status, never a throw (consistent with the gate
/// outcomes).
fn closed_json() -> String {
    err_json(
        "closed",
        "gateway has been closed — construct a new one (close() releases the \
         mesh node so NetMesh.shutdown() can run)",
    )
}

#[napi]
impl CapabilityGateway {
    #[napi(constructor)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mesh: &NetMesh,
        pin_store_path: Option<String>,
        payment_policy_path: Option<String>,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: Option<bool>,
        payment_signer_address: Option<String>,
        payment_signer: Option<Function<'static, String, Promise<String>>>,
        payment_signer_svm_address: Option<String>,
        payment_signer_svm: Option<Function<'static, String, Promise<String>>>,
        payment_signer_xrpl_address: Option<String>,
        payment_signer_xrpl: Option<Function<'static, String, Promise<String>>>,
    ) -> Result<Self> {
        // Reuse the live node + its channel configs so the gateway drives mesh
        // I/O over the same node the caller runs (the `DaemonRuntime::create`
        // precedent). Identity is `None`: the payment identity is derived from
        // the node inside the flow, never handed in.
        let node = mesh
            .node_arc_clone()
            .map_err(|_| Error::from_reason("gateway: mesh node has been shut down"))?;
        let channel_configs = mesh.channel_configs_arc();
        let sdk_mesh = Arc::new(SdkMesh::from_node_arc(node, channel_configs, None));

        let config = collect_payment_config(
            payment_policy_path,
            payment_profile,
            payment_unsafe_mock_auto_allow.unwrap_or(false),
        )?;
        // Collect the per-scheme signers (both-or-neither each): JS callbacks
        // become `ThreadsafeFunction`s wrapped in the external signer. Only the
        // typed intent + the artifact cross — key material is unrepresentable.
        let mut signers = Vec::new();
        for pair in [
            signer_pair(
                payment_signer_address,
                payment_signer,
                "eip155",
                "paymentSigner",
                crate::payment_signer::eip155_signer,
            )?,
            signer_pair(
                payment_signer_svm_address,
                payment_signer_svm,
                "solana",
                "paymentSignerSvm",
                crate::payment_signer::svm_signer,
            )?,
            signer_pair(
                payment_signer_xrpl_address,
                payment_signer_xrpl,
                "xrpl",
                "paymentSignerXrpl",
                crate::payment_signer::xrpl_signer,
            )?,
        ]
        .into_iter()
        .flatten()
        {
            signers.push(pair);
        }
        // Retain the store path + profile before `build_payment_flow` consumes
        // the config, so the approval verbs can reopen it.
        let (spend_policy_path, spend_profile) = match &config {
            Some(c) => (Some(c.policy_path.clone()), c.profile.clone()),
            None => (None, "production".to_string()),
        };
        let payment = build_payment_flow(sdk_mesh.clone(), config, signers)?;

        Ok(Self {
            live: Mutex::new(Some(Live {
                gateway: Arc::new(MeshGateway::new(sdk_mesh)),
                payment,
            })),
            consent: Arc::new(ConsentPolicy::new()),
            pin_store_path: pin_store_path.map(PathBuf::from),
            spend_policy_path,
            spend_profile,
        })
    }

    /// Release the internal mesh-node reference so the underlying `NetMesh` can
    /// be `shutdown()` deterministically. A `#[napi]` class is GC-finalized, not
    /// scope-dropped, so without this the gateway's retained node clone keeps
    /// `NetMesh.shutdown()` (which needs sole ownership of the node) failing
    /// until GC runs. Call it before `mesh.shutdown()`. Idempotent; after
    /// `close()`, `search` / `describe` / `invoke` resolve to a structured
    /// `closed` status (never a throw). The operator approval verbs still work —
    /// they reopen the spend-policy store, independent of the node.
    #[napi]
    pub fn close(&self) {
        let _ = self.live.lock().take();
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
    /// (each row carries `requires_approval`), or `{"status":"<err>","error":...}`.
    /// An empty index is `ok` with an empty list — never an error.
    #[napi]
    pub async fn search(&self, query: String) -> Result<String> {
        let Some((gateway, _payment)) = self.live_handles() else {
            return Ok(closed_json());
        };
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        Ok(do_search(&gateway, &consent, &pin_path, &query).await)
    }

    /// Describe one capability by its `provider/capability` id. Resolves to a
    /// JSON string with the full schema + `requires_approval` + `pricing_terms`,
    /// or `{"status":"<err>","error":...}`.
    #[napi]
    pub async fn describe(&self, cap_id: String) -> Result<String> {
        let id = match CapabilityId::parse(&cap_id) {
            Ok(id) => id,
            Err(e) => return Ok(err_json("invalid_capability_id", e)),
        };
        let Some((gateway, _payment)) = self.live_handles() else {
            return Ok(closed_json());
        };
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
        // `null`/omitted normalizes to `{}` (a no-argument invocation, as the
        // gate does); arrays and primitives are a caller-shape error.
        let args = match normalize_invoke_args(&raw) {
            Ok(v) => v,
            Err(e) => return Ok(err_json("invalid_arguments", e)),
        };
        let Some((gateway, payment)) = self.live_handles() else {
            return Ok(closed_json());
        };
        let consent = self.consent.clone();
        let pin_path = self.pin_store_path.clone();
        Ok(do_invoke(&gateway, &consent, &pin_path, payment.as_deref(), id, args).await)
    }

    /// Approve a held payment quote under operator policy, resolving a prior
    /// `requires_payment_approval` so the next `invoke` redeems it. Resolves to
    /// `{"status":"ok","quote_id":...,"changed":bool}`, or a structured
    /// `no_payment_policy` / `error`. Operator surface — `invoke` only *requests*
    /// approval; this grants it.
    #[napi]
    pub async fn approve_payment(&self, quote_id: String) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile.clone();
        Ok(do_approve_payment(path, profile, quote_id).await)
    }

    /// Reject / remove a payment approval record. Resolves to
    /// `{"status":"ok","quote_id":...,"changed":bool}`, or a structured error.
    #[napi]
    pub async fn reject_payment(&self, quote_id: String) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile.clone();
        Ok(do_reject_payment(path, profile, quote_id).await)
    }

    /// The quote ids awaiting approval, for a consent UX to render. Resolves to
    /// `{"status":"ok","pending":[quote_id, ...]}`, or a structured error.
    #[napi]
    pub async fn pending_payments(&self) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile.clone();
        Ok(do_pending_payments(path, profile).await)
    }

    /// Today's reserved spend total for a `(network, x402 asset)` pair, as the
    /// canonical atomic-amount string. Resolves to
    /// `{"status":"ok","network":...,"asset":...,"spent":"<atomic>"}`, or a
    /// structured error. `network` / `asset` are the x402 wire values.
    #[napi]
    pub async fn spent_today(&self, network: String, asset: String) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile.clone();
        Ok(do_spent_today(path, profile, network, asset).await)
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
    fn payment_config_requires_a_policy_path() {
        // No payment kwargs → no flow.
        assert!(collect_payment_config(None, None, false).unwrap().is_none());
        // Profile / unsafe without a policy path is a caller error.
        assert!(collect_payment_config(None, Some("dev_test".into()), false).is_err());
        assert!(collect_payment_config(None, None, true).is_err());
        // A policy path alone defaults the profile fail-closed.
        let c = collect_payment_config(Some("/tmp/p.json".into()), None, false)
            .unwrap()
            .unwrap();
        assert_eq!(c.profile, "production");
        assert!(!c.unsafe_mock_auto_allow);
    }

    #[test]
    fn closed_projection_is_a_structured_status() {
        // The live-path methods resolve to this after `close()` — a status,
        // never a throw.
        let v: Value = serde_json::from_str(&closed_json()).expect("json");
        assert_eq!(v["status"], "closed");
        assert!(v["error"].as_str().unwrap().contains("closed"));
    }

    #[test]
    fn parse_profile_rejects_unknown() {
        assert!(matches!(
            parse_profile("production"),
            Ok(SpendProfile::Production)
        ));
        assert!(matches!(
            parse_profile("dev_test"),
            Ok(SpendProfile::DevTest)
        ));
        assert!(parse_profile("yolo").is_err());
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

    #[test]
    fn normalize_invoke_args_matches_the_gate_contract() {
        // An object passes through untouched.
        assert_eq!(
            normalize_invoke_args(r#"{"m":1}"#).unwrap(),
            json!({ "m": 1 })
        );
        // A no-argument invocation — omitted (`{}`) or explicit `null` — is `{}`
        // (the gate normalizes `null` the same way).
        assert_eq!(normalize_invoke_args("{}").unwrap(), json!({}));
        assert_eq!(normalize_invoke_args("null").unwrap(), json!({}));
        // Arrays / primitives are a caller-shape error, never forwarded.
        for bad in ["[]", "true", "42", "\"str\""] {
            assert!(normalize_invoke_args(bad).is_err(), "{bad} must be rejected");
        }
        // Malformed JSON is also invalid_arguments.
        assert!(normalize_invoke_args("not json").is_err());
    }
}
