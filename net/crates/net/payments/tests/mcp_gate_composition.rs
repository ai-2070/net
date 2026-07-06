//! Workstream 4, the full composition: `gated_invoke` (describe →
//! validate → consent → **payment** → invoke) driving the real
//! [`CallerPaymentFlow`] against a real [`PaymentEngine`] + mock
//! facilitator. This is the P0 acceptance path minus the wire: auto-allow
//! settles silently and the handler runs; over-cap surfaces the
//! structured `RequiresPaymentApproval`; approval through the SDK consent
//! surface unblocks the retry; and an unpaid call can never reach the
//! handler.
#![cfg(feature = "mcp-gate")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_mcp::serve::{
    gated_invoke, CapabilityDetail, CapabilityGateway, CapabilityId, CapabilitySummary,
    ConsentPolicy, GatedOutcome, GatewayError, InvokeSafety, PaymentAdmission as _, PaymentProof,
};
use net_mcp::spec::CallToolResult;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::mcp_gate::EnginePaymentAdmission;
use net_payments::flow::{CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use serde_json::{json, Value};

const CAP_ID: &str = "fixture-provider/fixture-tool";

struct TestClock(std::sync::atomic::AtomicU64);
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, Ordering::SeqCst)
    }
}

/// A provider-side gateway stub: describe announces the pricing terms;
/// invoke redeems the payment through the REAL engine admission (the
/// exact check `WrapInvokeHandler` runs) and counts handler executions —
/// the thing that must never happen unpaid.
struct PaidToolGateway {
    detail: CapabilityDetail,
    handler_runs: AtomicU32,
    admission: EnginePaymentAdmission,
}

#[async_trait]
impl CapabilityGateway for PaidToolGateway {
    async fn search(&self, _query: &str) -> Result<Vec<CapabilitySummary>, GatewayError> {
        Ok(Vec::new())
    }
    async fn describe(&self, id: &CapabilityId) -> Result<CapabilityDetail, GatewayError> {
        if id == &self.detail.id {
            Ok(self.detail.clone())
        } else {
            Err(GatewayError::NotFound(id.display()))
        }
    }
    async fn invoke(
        &self,
        id: &CapabilityId,
        arguments: Value,
        _safety: InvokeSafety,
        payment: Option<PaymentProof>,
    ) -> Result<CallToolResult, GatewayError> {
        // Mirror WrapInvokeHandler's paid path: no proof or a failed
        // redemption never reaches the handler. The real flow signs the
        // binding, so this composition also proves the signature the
        // flow produced verifies against the paying identity.
        let Some(proof) = payment else {
            return Err(GatewayError::Denied(
                "paid tool invoked without a payment quote".to_string(),
            ));
        };
        assert!(
            proof.binding_sig.is_some(),
            "the flow's caller identity signs; bearer mode is for pre-binding callers"
        );
        self.admission
            .redeem(
                &id.capability,
                &proof.quote_id,
                proof.binding_sig.as_deref(),
            )
            .await
            .map_err(GatewayError::Denied)?;
        self.handler_runs.fetch_add(1, Ordering::SeqCst);
        Ok(CallToolResult::text_ok(format!("handled {arguments}")))
    }
}

struct World {
    gateway: PaidToolGateway,
    consent: ConsentPolicy,
    flow: CallerPaymentFlow,
    spend_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn world(profile: SpendProfile) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock(std::sync::atomic::AtomicU64::new(
        1_000_000_000_000_000,
    )));

    // Provider side.
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(provider_keys.entity_id().clone());
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine"),
    );
    let channel = Arc::new(InProcessProvider::new(engine.clone(), clock.clone()));

    // Announced terms → the describe surface's pricing_terms.
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
        CAP_ID,
        vec![template],
        registry.reference().expect("ref"),
    );
    let terms_json = String::from_utf8(
        net_payments::core::canonical::canonical_bytes(&terms).expect("canonicalize"),
    )
    .expect("utf8");

    let cap = CapabilityId::parse(CAP_ID).expect("cap id");
    let gateway = PaidToolGateway {
        detail: CapabilityDetail {
            id: cap.clone(),
            name: "fixture-tool".into(),
            description: Some("a paid fixture".into()),
            input_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
            }),
            output_schema: None,
            compat_tier: "mcp_bridge".into(),
            credential_status: "none".into(),
            substitutability: "provider_local".into(),
            version: "1.0.0".into(),
            pricing_terms: Some(terms_json),
        },
        handler_runs: AtomicU32::new(0),
        admission: EnginePaymentAdmission::new(engine),
    };

    // Caller side: consent admits the capability (pinning is capability
    // consent, not spending consent — the payment gate still runs).
    let mut consent = ConsentPolicy::new();
    consent.allow(cap);

    let caller_keys = Arc::new(EntityKeypair::generate());
    let spend_path = dir.path().join("spend-policy.json");
    let flow = CallerPaymentFlow::new(
        caller_keys,
        SpendPolicyEngine::new(&spend_path, profile),
        registry,
        channel,
        clock,
    );

    World {
        gateway,
        consent,
        flow,
        spend_path,
        _dir: dir,
    }
}

fn cap() -> CapabilityId {
    CapabilityId::parse(CAP_ID).expect("cap id")
}

#[tokio::test]
async fn auto_allow_settles_silently_and_the_handler_runs() {
    let w = world(SpendProfile::DevTest);
    let out = gated_invoke(
        &w.gateway,
        &w.consent,
        None,
        Some(&w.flow),
        &cap(),
        json!({ "message": "hi" }),
    )
    .await;
    match out {
        GatedOutcome::Invoked(result) => assert!(!result.is_error),
        other => panic!("expected Invoked, got {other:?}"),
    }
    assert_eq!(w.gateway.handler_runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn over_cap_holds_structured_and_approval_unblocks_the_retry() {
    let w = world(SpendProfile::DevTest);
    let configurer = SpendPolicyEngine::new(&w.spend_path, SpendProfile::DevTest);
    configurer
        .configure(|defaults, _| {
            defaults.max_per_call = Some(AtomicAmount::from_u128(1000));
        })
        .await
        .expect("configure");

    let out = gated_invoke(
        &w.gateway,
        &w.consent,
        None,
        Some(&w.flow),
        &cap(),
        json!({ "message": "hi" }),
    )
    .await;
    let GatedOutcome::RequiresPaymentApproval {
        quote_id,
        policy_reason,
        ..
    } = out
    else {
        panic!("expected RequiresPaymentApproval");
    };
    assert!(policy_reason.contains("max_per_call"), "{policy_reason}");
    assert_eq!(
        w.gateway.handler_runs.load(Ordering::SeqCst),
        0,
        "the handler never sees an unpaid call"
    );

    // Human approves via the SDK consent surface → the retry redeems the
    // held quote and invokes.
    configurer.approve(&quote_id).await.expect("approve");
    let retry = gated_invoke(
        &w.gateway,
        &w.consent,
        None,
        Some(&w.flow),
        &cap(),
        json!({ "message": "hi" }),
    )
    .await;
    assert!(matches!(retry, GatedOutcome::Invoked(_)), "{retry:?}");
    assert_eq!(w.gateway.handler_runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn production_profile_never_pays_silently_through_the_gate() {
    let w = world(SpendProfile::Production);
    let out = gated_invoke(
        &w.gateway,
        &w.consent,
        None,
        Some(&w.flow),
        &cap(),
        json!({ "message": "hi" }),
    )
    .await;
    assert!(
        matches!(out, GatedOutcome::RequiresPaymentApproval { .. }),
        "production mock spends hold for approval, got {out:?}"
    );
    assert_eq!(w.gateway.handler_runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn without_a_flow_the_paid_capability_fails_closed() {
    let w = world(SpendProfile::DevTest);
    let out = gated_invoke(
        &w.gateway,
        &w.consent,
        None,
        None,
        &cap(),
        json!({ "message": "hi" }),
    )
    .await;
    assert!(
        matches!(out, GatedOutcome::Failed(GatewayError::Denied(_))),
        "{out:?}"
    );
    assert_eq!(w.gateway.handler_runs.load(Ordering::SeqCst), 0);
}
