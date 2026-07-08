//! `EngineToolPaymentGate` — the engine-backed implementation of the
//! SDK's native tool payment gate (`net_sdk::tool_payment::ToolPaymentGate`,
//! consumed by `Mesh::serve_tool_paid`). Semantics must be identical to
//! the MCP wrap path's `EnginePaymentAdmission`: a paid quote redeems
//! exactly once, bound to its tool; everything else is a structured
//! denial, fail-closed.
#![cfg(feature = "mesh")]

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::mesh::EngineToolPaymentGate;
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::tool_payment::ToolPaymentGate as _;

const NOW: u64 = 1_000_000_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

async fn paid_quote_id(engine: &Arc<PaymentEngine>, caller: &EntityKeypair) -> String {
    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author");
    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            CAPABILITY,
            requirements,
            NOW,
            60_000_000_000,
        )
        .expect("quote");
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": "payer-1" }),
        extensions: None,
    })
    .expect("payload");
    let decision = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .expect("accept");
    assert!(matches!(decision, PaymentDecision::Served { .. }));
    quote.quote_id
}

#[tokio::test]
async fn the_engine_gate_redeems_a_paid_quote_exactly_once() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            dir.path().join("engine.json"),
        )
        .expect("engine"),
    );
    let quote_id = paid_quote_id(&engine, &caller).await;
    let gate = EngineToolPaymentGate::new(engine.clone());

    // A paid, billed, unfrozen quote bound to this tool: admitted once.
    gate.redeem("fixture-tool", &quote_id, None)
        .await
        .expect("first redemption admits");

    // At-most-once: the same quote never serves twice.
    let err = gate
        .redeem("fixture-tool", &quote_id, None)
        .await
        .expect_err("a second redemption must be denied");
    assert!(err.contains("redeem"), "{err}");

    // Bound to the capability's tool: another tool never redeems it.
    let fresh = paid_quote_id(&engine, &caller).await;
    let err = gate
        .redeem("some-other-tool", &fresh, None)
        .await
        .expect_err("a quote never redeems for a different tool");
    assert!(!err.is_empty());

    // An unknown quote is a structured denial, not a panic.
    let err = gate
        .redeem("fixture-tool", "no-such-quote", None)
        .await
        .expect_err("unknown quote denied");
    assert!(err.contains("unknown quote"), "{err}");
}
