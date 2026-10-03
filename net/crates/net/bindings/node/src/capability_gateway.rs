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
    let value: Value =
        serde_json::from_str(raw).map_err(|e| format!("arguments must be a JSON object: {e}"))?;
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

/// Payment config collected from the constructor options. The profile string is
/// parsed to a [`SpendProfile`] here, once — the operator verbs and the flow
/// then share that value, so there is no second (divergent) parse.
struct PaymentConfig {
    policy_path: String,
    profile: SpendProfile,
    unsafe_mock_auto_allow: bool,
}

/// A `paymentProfile` / unsafe flag without a `paymentPolicyPath` is a caller
/// error; the policy path is the shared spend-policy store the flow reserves
/// against. An unknown `paymentProfile` is a construction error (no silent
/// fallback — the vocabulary is [`SpendProfile::parse`] in core).
fn collect_payment_config(
    policy_path: Option<String>,
    profile: Option<String>,
    unsafe_mock_auto_allow: bool,
) -> Result<Option<PaymentConfig>> {
    match policy_path {
        Some(policy_path) => {
            let profile = match profile {
                Some(p) => SpendProfile::parse(&p)
                    .map_err(|e| Error::from_reason(format!("gateway: {e}")))?,
                None => SpendProfile::default(),
            };
            Ok(Some(PaymentConfig {
                policy_path,
                profile,
                unsafe_mock_auto_allow,
            }))
        }
        None if profile.is_some() || unsafe_mock_auto_allow => Err(Error::from_reason(
            "gateway: paymentProfile / paymentUnsafeMockAutoAllow require paymentPolicyPath \
             (the shared spend-policy store)",
        )),
        None => Ok(None),
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
) -> Result<Option<Arc<CallerPaymentFlow>>> {
    let Some(config) = config else {
        if !signers.is_empty() {
            return Err(Error::from_reason(
                "gateway: payment signers require paymentPolicyPath (the shared spend-policy store)",
            ));
        }
        return Ok(None);
    };
    let caller = Arc::new(mesh.entity_keypair().clone());
    let registry = default_registry_v1(caller.entity_id().clone());
    let spend = SpendPolicyEngine::new(&config.policy_path, config.profile)
        .with_unsafe_mock_auto_allow(config.unsafe_mock_auto_allow);
    // One clock for both halves: the channel stamps each signed quote
    // request's freshness window and the provider checks it, so a
    // divergent clock would make every request look future-dated.
    let clock: Arc<dyn net_payments::flow::Clock> = Arc::new(SystemClock);
    let mut flow = CallerPaymentFlow::new(
        caller.clone(),
        spend,
        registry,
        Arc::new(MeshPaymentChannel::new(mesh, caller, clock.clone())),
        clock,
    );
    for (namespace, signer) in signers {
        flow = flow.with_signer(namespace, signer);
    }
    Ok(Some(Arc::new(flow)))
}

/// The paid-A2A caller flow over the gateway's own payment flow, or why
/// there is none.
///
/// Opt-in on its own path: the invoke gate needs no purchase store, and
/// opening one a caller never asked for would create a financial record
/// file as a side effect of building a gateway. A purchase path without a
/// spend policy is refused at construction — a purchase store with no spend
/// policy would record payments nothing ever authorized.
#[cfg(feature = "a2a")]
fn build_a2a_flow(
    payments: Option<&Arc<CallerPaymentFlow>>,
    mesh: Arc<SdkMesh>,
    purchase_path: Option<String>,
) -> Result<std::result::Result<Arc<net_payments::flow::a2a::A2aCallerFlow>, &'static str>> {
    match (payments, purchase_path) {
        (Some(flow), Some(path)) => {
            let clock: Arc<dyn Clock> = Arc::new(SystemClock);
            Ok(Ok(net_payments::flow::a2a::json::build_flow(
                flow.clone(),
                mesh,
                &path,
                clock,
            )))
        }
        (None, Some(_)) => Err(Error::from_reason(
            "gateway: a2aPurchasePath requires paymentPolicyPath (the shared spend-policy \
             store every outbound payment is reserved against) — a purchase store with \
             no spend policy would record payments nothing ever authorized",
        )),
        (Some(_), None) => Ok(Err(
            "gateway: paid A2A needs a2aPurchasePath at construction (the durable purchase \
             store: one attempt per (caller, provider, task id), which is what makes a lost \
             pay reply recoverable instead of a second charge)",
        )),
        (None, None) => Ok(Err(
            "gateway: paid A2A needs paymentPolicyPath and a2aPurchasePath at construction \
             (the shared spend-policy store, and the durable purchase store)",
        )),
    }
}

/// A `SpendPolicyEngine` for the operator verbs, keyed on the same store the
/// flow reserves against (the unsafe flag doesn't affect these verbs). Takes the
/// already-parsed [`SpendProfile`] the gateway retained at construction — no
/// re-parse, so the verbs and the flow can never disagree on the profile.
fn spend_engine(path: &str, profile: SpendProfile) -> SpendPolicyEngine {
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
    profile: SpendProfile,
    quote_id: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    match spend_engine(&path, profile).approve(&quote_id).await {
        Ok(changed) => {
            json!({ "status": "ok", "quote_id": quote_id, "changed": changed }).to_string()
        }
        Err(e) => err_json("error", e),
    }
}

async fn do_reject_payment(
    policy_path: Option<String>,
    profile: SpendProfile,
    quote_id: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    match spend_engine(&path, profile).reject(&quote_id).await {
        Ok(changed) => {
            json!({ "status": "ok", "quote_id": quote_id, "changed": changed }).to_string()
        }
        Err(e) => err_json("error", e),
    }
}

async fn do_pending_payments(policy_path: Option<String>, profile: SpendProfile) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    match spend_engine(&path, profile).pending().await {
        Ok(quotes) => json!({ "status": "ok", "pending": quotes }).to_string(),
        Err(e) => err_json("error", e),
    }
}

async fn do_spent_today(
    policy_path: Option<String>,
    profile: SpendProfile,
    network: String,
    asset: String,
) -> String {
    let Some(path) = policy_path else {
        return no_policy_json();
    };
    let now_ns = SystemClock.now_ns();
    match spend_engine(&path, profile)
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
    /// The paid-A2A caller flow over the SAME `CallerPaymentFlow` (one
    /// identity, one spend store, one signer set). `Err` names the missing
    /// constructor argument, so the paid verbs can refuse loudly rather
    /// than default to a purchase kept only in memory.
    #[cfg(feature = "a2a")]
    a2a: std::result::Result<Arc<net_payments::flow::a2a::A2aCallerFlow>, &'static str>,
    /// The gateway's own SDK mesh — the one its A2A flow composes over, and
    /// so the one whose org-caller slot `setA2aOrgCaller` sets (not the
    /// native `NetMesh`'s, which the raw verbs use).
    #[cfg(feature = "a2a")]
    mesh: Arc<SdkMesh>,
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
    spend_profile: SpendProfile,
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
        a2a_purchase_path: Option<String>,
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
            Some(c) => (Some(c.policy_path.clone()), c.profile),
            None => (None, SpendProfile::default()),
        };
        let payment = build_payment_flow(sdk_mesh.clone(), config, signers)?;
        #[cfg(feature = "a2a")]
        let a2a = build_a2a_flow(payment.as_ref(), sdk_mesh.clone(), a2a_purchase_path)?;
        // A build without `a2a` has no requester verbs to compose, so the
        // argument is a loud config error rather than a silently ignored path.
        #[cfg(not(feature = "a2a"))]
        if a2a_purchase_path.is_some() {
            return Err(Error::from_reason(
                "gateway: this build lacks the `a2a` feature; a2aPurchasePath is unavailable",
            ));
        }
        let payment: Option<Arc<dyn PaymentFlow>> = payment.map(|f| f as Arc<dyn PaymentFlow>);

        Ok(Self {
            live: Mutex::new(Some(Live {
                gateway: Arc::new(MeshGateway::new(sdk_mesh.clone())),
                payment,
                #[cfg(feature = "a2a")]
                a2a,
                #[cfg(feature = "a2a")]
                mesh: sdk_mesh,
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
        let profile = self.spend_profile;
        Ok(do_approve_payment(path, profile, quote_id).await)
    }

    /// Reject / remove a payment approval record. Resolves to
    /// `{"status":"ok","quote_id":...,"changed":bool}`, or a structured error.
    #[napi]
    pub async fn reject_payment(&self, quote_id: String) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile;
        Ok(do_reject_payment(path, profile, quote_id).await)
    }

    /// The quote ids awaiting approval, for a consent UX to render. Resolves to
    /// `{"status":"ok","pending":[quote_id, ...]}`, or a structured error.
    #[napi]
    pub async fn pending_payments(&self) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile;
        Ok(do_pending_payments(path, profile).await)
    }

    /// Today's reserved spend total for a `(network, x402 asset)` pair, as the
    /// canonical atomic-amount string. Resolves to
    /// `{"status":"ok","network":...,"asset":...,"spent":"<atomic>"}`, or a
    /// structured error. `network` / `asset` are the x402 wire values.
    #[napi]
    pub async fn spent_today(&self, network: String, asset: String) -> Result<String> {
        let path = self.spend_policy_path.clone();
        let profile = self.spend_profile;
        Ok(do_spent_today(path, profile, network, asset).await)
    }
}

// ---------------------------------------------------------------------------
// Paid A2A, caller half (NODE_A2A_PAID_ADMISSION_PLAN.md WS-D)
// ---------------------------------------------------------------------------

/// [`A2aBoundaryError`](net_payments::flow::a2a::json::A2aBoundaryError) onto
/// the binding's prefixes.
#[cfg(feature = "a2a")]
fn boundary_err(e: net_payments::flow::a2a::json::A2aBoundaryError) -> Error {
    use net_payments::flow::a2a::json::A2aBoundaryError;
    match e {
        A2aBoundaryError::Invalid(message) => crate::a2a::invalid(message),
        A2aBoundaryError::Failed(message) => Error::from_reason(format!("a2a: {message}")),
        A2aBoundaryError::Journal(e) => Error::from_reason(format!("a2a: {e}")),
    }
}

/// What a paid verb needs from the live state, cloned out of the lock.
#[cfg(feature = "a2a")]
enum A2aLive {
    Ready(Arc<net_payments::flow::a2a::A2aCallerFlow>, Arc<SdkMesh>),
    /// The gateway was closed: the verb resolves to the `closed` status,
    /// like every other live-path verb.
    Closed,
}

#[cfg(feature = "a2a")]
impl CapabilityGateway {
    /// The A2A flow, `Closed` after `close()`, or a rejection naming the
    /// missing constructor argument.
    fn a2a_live(&self) -> Result<A2aLive> {
        match self.live.lock().as_ref() {
            None => Ok(A2aLive::Closed),
            Some(live) => match &live.a2a {
                Ok(flow) => Ok(A2aLive::Ready(flow.clone(), live.mesh.clone())),
                Err(why) => Err(Error::from_reason(*why)),
            },
        }
    }
}

// Its own `#[napi] impl` block, cfg'd as a whole: napi-derive registers every
// method of a block, so a per-method `#[cfg]` leaves a dangling registration
// in builds without `a2a`.
#[cfg(feature = "a2a")]
#[napi]
impl CapabilityGateway {
    /// **Paid A2A, step 1 — no funds move.** Ask `targetNodeId` to validate
    /// this brief against `service`'s catalog entry and reserve capacity for
    /// it, and obtain a provider-signed quote bound to that exact
    /// reservation. It does reserve capacity, obtain a quote and persist the
    /// attempt — but no spend is reserved and no payment is made, so it is
    /// safe to call merely to display a price.
    ///
    /// Resolves to `{"status": ok|rejected|busy|retired|conflict,
    /// "prepared": {...}, "quote": {"quote_id", "amount", "network", "asset",
    /// "expires_at_ns"}}`. Keep `prepared`: it is the complete document
    /// `purchaseTask` and `submitTask` take back. **Read it out with
    /// `a2aDocument(envelope, '/prepared')`, never `JSON.parse` /
    /// `JSON.stringify`** — its `provider_node` is a u64, and so is
    /// `quote.expires_at_ns` (`a2aU64`).
    ///
    /// `busy`: nothing was reserved or quoted — retry. A service the
    /// provider does not serve is `rejected`, never `busy`. `taskId` retains
    /// a caller-chosen id (omit for random); retaining one is what makes the
    /// purchase resumable. Needs `paymentPolicyPath` + `a2aPurchasePath` at
    /// construction, else rejects `gateway:`.
    #[napi(js_name = "prepareTask")]
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_task(
        &self,
        target_node_id: napi::bindgen_prelude::BigInt,
        service: String,
        prompt: String,
        context_refs: Option<Vec<String>>,
        tags: Option<Vec<String>>,
        task_id: Option<String>,
    ) -> Result<String> {
        let target = crate::common::bigint_u64(target_node_id)
            .map_err(|e| crate::a2a::invalid(format!("targetNodeId: {}", e.reason)))?;
        let (flow, mesh) = match self.a2a_live()? {
            A2aLive::Ready(flow, mesh) => (flow, mesh),
            A2aLive::Closed => return Ok(closed_json()),
        };
        let mut brief = net_sdk::a2a::TaskBrief::new(prompt)
            .with_context_refs(context_refs.unwrap_or_default())
            .with_tags(tags.unwrap_or_default());
        if let Some(task_id) = task_id {
            if task_id.is_empty() {
                return Err(crate::a2a::invalid(
                    "taskId must be a non-empty string (omit it for a random id)",
                ));
            }
            brief = brief.with_task_id(task_id);
        }
        Ok(net_payments::flow::a2a::json::prepare_json(&flow, &mesh, target, service, brief).await)
    }

    /// **Paid A2A, step 2 — this is where money moves.** Consume the quote
    /// `prepareTask` stored for `preparedJson` and pay it under spend policy.
    ///
    /// Resolves to `{"status": paid|requires_payment_approval|denied|unknown|
    /// failed, "task_id", "quote_id", "proof"?, "policy_reason"?,
    /// "approve_hint"?, "retryable"?, "funds_ambiguous"?}`.
    ///
    /// - `paid` carries the `proof` (`a2aDocument(env, '/proof')`).
    /// - `requires_payment_approval` holds *this* quote for an operator:
    ///   `approvePayment(quoteId)`, then call this again.
    /// - `denied` with `funds_ambiguous: false` is proven non-settlement — a
    ///   fresh `prepareTask` is allowed.
    /// - `denied` with `funds_ambiguous: true` means an authorization was
    ///   exposed: the spend reservation is held and `a2aResolveAttempt` is
    ///   the only exit.
    /// - `unknown` is a lost reply, not a failure: call this again and it
    ///   re-sends the *stored* payload rather than buying twice.
    #[napi(js_name = "purchaseTask")]
    pub async fn purchase_task(&self, prepared_json: String) -> Result<String> {
        let (flow, _mesh) = match self.a2a_live()? {
            A2aLive::Ready(flow, mesh) => (flow, mesh),
            A2aLive::Closed => return Ok(closed_json()),
        };
        let prepared =
            net_payments::flow::a2a::json::parse_prepared(&prepared_json).map_err(boundary_err)?;
        Ok(net_payments::flow::a2a::json::purchase_json(
            &flow,
            prepared.provider_node,
            &prepared.brief.task_id,
        )
        .await)
    }

    /// **Paid A2A, step 3.** Submit the prepared brief with the **stored**
    /// proof, and record the outcome on the attempt. Resolves to
    /// `{"status": accepted|retry|unexecutable, "task_id", "message"?,
    /// "schematic"?}`. `retry` keeps the purchase good — re-submit, never
    /// re-purchase. `unexecutable` means the provider will not run this
    /// purchase; the evidence is retained and `a2aResolveAttempt` is the
    /// exit.
    #[napi(js_name = "submitTask")]
    pub async fn submit_task(&self, prepared_json: String) -> Result<String> {
        let (flow, _mesh) = match self.a2a_live()? {
            A2aLive::Ready(flow, mesh) => (flow, mesh),
            A2aLive::Closed => return Ok(closed_json()),
        };
        let prepared =
            net_payments::flow::a2a::json::parse_prepared(&prepared_json).map_err(boundary_err)?;
        Ok(net_payments::flow::a2a::json::submit_json(
            &flow,
            prepared.provider_node,
            &prepared.brief.task_id,
        )
        .await)
    }

    /// Every purchase attempt this gateway's identity owns, as a JSON array
    /// — the operator's queue. Each row carries `retained`: `false` is the
    /// live attempt (resolve it by omitting `generationJson`), `true` a
    /// retained superseded incarnation (resolve it by passing the row's
    /// `generation`, read with `a2aDocument(rows, '/i/generation')`). The
    /// provider id is `key.provider_node`, a u64 —
    /// `a2aU64(rows, '/i/key/provider_node')`.
    #[napi(js_name = "a2aAttempts")]
    pub async fn a2a_attempts(&self) -> Result<String> {
        let flow = match self.a2a_live()? {
            A2aLive::Ready(flow, _) => flow,
            A2aLive::Closed => return Ok(closed_json()),
        };
        net_payments::flow::a2a::json::attempts_json(&flow)
            .await
            .map_err(boundary_err)
    }

    /// Close an attempt the automatic path cannot: `unknown`, `denied` with
    /// `funds_ambiguous`, or `unexecutable`. `outcomeJson` is
    /// `{"resolution":"paid","proof":…,"billing":…}`,
    /// `{"resolution":"not_paid","reason":…}` or
    /// `{"resolution":"closed","outcome":"refunded","evidence":…}`.
    ///
    /// `providerNode` completes the purchase key; omit it and the id is
    /// resolved against this caller's own rows (refused, listing the
    /// providers, when the id names attempts on several). `generationJson`
    /// is a **dispatch, not a universal selector**: omit it to resolve the
    /// live attempt (`retained: false`); pass a `retained: true` row's
    /// `generation` to resolve exactly that archived incarnation — passing a
    /// live row's own generation is refused, not resolved.
    #[napi(js_name = "a2aResolveAttempt")]
    pub async fn a2a_resolve_attempt(
        &self,
        task_id: String,
        outcome_json: String,
        provider_node: Option<napi::bindgen_prelude::BigInt>,
        generation_json: Option<String>,
    ) -> Result<()> {
        let flow = match self.a2a_live()? {
            A2aLive::Ready(flow, _) => flow,
            // A void verb cannot resolve to the `closed` status the others
            // do, so it rejects — with the gateway's own prefix.
            A2aLive::Closed => {
                return Err(Error::from_reason(
                    "gateway: the gateway has been closed — construct a new one",
                ))
            }
        };
        let provider_node = match provider_node {
            Some(n) => Some(
                crate::common::bigint_u64(n)
                    .map_err(|e| crate::a2a::invalid(format!("providerNode: {}", e.reason)))?,
            ),
            None => None,
        };
        let resolution =
            net_payments::flow::a2a::json::parse_resolution(&outcome_json).map_err(boundary_err)?;
        let generation = match generation_json {
            Some(g) => {
                Some(net_payments::flow::a2a::json::parse_generation(&g).map_err(boundary_err)?)
            }
            None => None,
        };
        net_payments::flow::a2a::json::resolve_attempt(
            &flow,
            &task_id,
            provider_node,
            generation,
            resolution,
        )
        .await
        .map_err(boundary_err)
    }
}

#[cfg(all(feature = "a2a", feature = "org"))]
#[napi]
impl CapabilityGateway {
    /// Install (or clear with `null`) the organization identity this
    /// gateway's paid-A2A lifecycle presents to a PROTECTED provider (one
    /// serving its catalog under `principal: "same_org"` or `"granted"`).
    /// Applies to `prepareTask`, `purchaseTask` and `submitTask` from the
    /// next call on.
    ///
    /// This is the gateway's own slot, separate from
    /// `NetMesh.setA2aOrgCaller` (the raw verbs'): the lifecycle is composed
    /// over the gateway's mesh, so the identity is installed here once
    /// rather than passed per verb. Rejects `org:credentials:closed` for a
    /// closed client, and `gateway:` after `close()`.
    #[napi(js_name = "setA2aOrgCaller")]
    pub fn set_a2a_org_caller(&self, org_client: Option<&crate::org::OrgClient>) -> Result<()> {
        let installed = match org_client {
            Some(client) => Some(client.shared().ok_or_else(|| {
                Error::from_reason("org:credentials:closed: this OrgClient has been closed")
            })?),
            None => None,
        };
        let live = self.live.lock();
        let Some(live) = live.as_ref() else {
            return Err(Error::from_reason(
                "gateway: the gateway has been closed — construct a new one",
            ));
        };
        live.mesh.set_a2a_org_caller(installed);
        Ok(())
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
        assert_eq!(c.profile, SpendProfile::Production);
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
    fn collect_payment_config_parses_the_profile_and_rejects_unknown() {
        // The profile vocabulary lives in core (SpendProfile::parse); the config
        // holds the parsed enum, so there is no second (divergent) parse.
        let c = collect_payment_config(Some("/tmp/p.json".into()), Some("dev_test".into()), false)
            .unwrap()
            .unwrap();
        assert_eq!(c.profile, SpendProfile::DevTest);
        // An unknown profile is a construction error, never a silent fallback.
        assert!(
            collect_payment_config(Some("/tmp/p.json".into()), Some("yolo".into()), false).is_err()
        );
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
            assert!(
                normalize_invoke_args(bad).is_err(),
                "{bad} must be rejected"
            );
        }
        // Malformed JSON is also invalid_arguments.
        assert!(normalize_invoke_args("not json").is_err());
    }
}
