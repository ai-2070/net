//! P1 WS5 adversarial rows, engine side: a misbehaving facilitator (or
//! a replayed captured response) cannot double-serve one on-chain
//! settlement, and a receipt from the wrong network is worth nothing.
//! Both invalidate and freeze — misbehavior of the money machinery is
//! never a retryable shrug.

use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::{
    InvalidationReason, VerificationTier, VerifierRef,
};
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::mock::{MOCK_NETWORK, MOCK_SCHEME};
use net_payments::facilitator::{
    Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome,
};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::settlement::{SettlementResponse, VerifyResponse};
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

/// A facilitator that always verifies, and settles with a SCRIPTED
/// transaction + network — the adversary's lever.
struct ScriptedSettler {
    transaction: String,
    network: String,
}

#[async_trait]
impl Facilitator for ScriptedSettler {
    fn reference(&self) -> VerifierRef {
        VerifierRef { identity: None, endpoint: "adversarial-fixture".into() }
    }

    async fn verify(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        Ok(VerifyOutcome {
            response: X402Carry::author(&VerifyResponse {
                is_valid: true,
                invalid_reason: None,
                payer: None,
                extra: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }

    async fn settle(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        Ok(SettleOutcome {
            response: X402Carry::author(&SettlementResponse {
                success: true,
                error_reason: None,
                payer: None,
                transaction: self.transaction.clone(),
                network: self.network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }
}

struct World {
    engine: PaymentEngine,
    caller: EntityKeypair,
    _dir: tempfile::TempDir,
}

fn world(facilitator: Arc<dyn Facilitator>) -> World {
    let provider = Arc::new(EntityKeypair::generate());
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        facilitator,
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");
    World { engine, caller: EntityKeypair::generate(), _dir: dir }
}

fn requirements() -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author")
}

impl World {
    async fn pay(&self, nonce: &str, issued: u64) -> (String, PaymentDecision) {
        let quote = self
            .engine
            .issue_quote(
                self.caller.entity_id().clone(),
                CAPABILITY,
                requirements(),
                issued,
                60_000_000_000,
            )
            .expect("quote");
        let payload = X402Carry::author(&PaymentPayload {
            x402_version: 2,
            resource: None,
            accepted: quote.requirements.view().clone(),
            payload: serde_json::json!({ "mock_authorization": nonce }),
            extensions: None,
        })
        .expect("payload");
        let decision = self
            .engine
            .accept_payment(&quote, &payload, VerificationTier::Observed, issued + 1)
            .await
            .expect("accept");
        (quote.quote_id.clone(), decision)
    }
}

#[tokio::test]
async fn a_replayed_settlement_transaction_never_serves_a_second_quote() {
    // The facilitator echoes ONE transaction hash for every settle: the
    // first quote is genuinely settled; the second "settlement" is a
    // receipt replay of the first.
    let w = world(Arc::new(ScriptedSettler {
        transaction: "mock:the-one-real-settlement".into(),
        network: MOCK_NETWORK.into(),
    }));

    let (first_id, first) = w.pay("payer-1", NOW).await;
    assert!(matches!(first, PaymentDecision::Served { .. }), "{first:?}");

    let (second_id, second) = w.pay("payer-2", NOW + 1_000).await;
    assert!(
        matches!(second, PaymentDecision::Invalidated { reason: InvalidationReason::Replay }),
        "one on-chain settlement, one serve — got {second:?}"
    );

    // The replayed quote is frozen with the audit trail pointing at the
    // quote the transaction really satisfies; the first is untouched.
    let status = w.engine.status(&second_id).await.unwrap().unwrap();
    assert!(status.frozen.is_some());
    assert!(status.billing_event_id.is_none(), "the replay never bills");
    let last = status.chain.last().unwrap();
    assert_eq!(
        last.extra.get("transaction_already_satisfies"),
        Some(&serde_json::json!(first_id))
    );
    let first_status = w.engine.status(&first_id).await.unwrap().unwrap();
    assert!(first_status.frozen.is_none());
    assert!(first_status.billing_event_id.is_some());

    // And the frozen quote's invocation is refused at the gate.
    let redemption =
        w.engine.redeem_for_invocation("fixture-tool", &second_id, None).await.unwrap();
    assert!(matches!(
        redemption,
        net_payments::engine::RedeemDecision::Denied { .. }
    ));
}

#[tokio::test]
async fn a_settlement_on_the_wrong_network_is_worth_nothing() {
    // The facilitator "settles" a mock:net quote with a receipt claiming
    // some other chain — CAIP confusion at the settlement boundary.
    let w = world(Arc::new(ScriptedSettler {
        transaction: "0xf00d".into(),
        network: "eip155:8453".into(),
    }));

    let (quote_id, decision) = w.pay("payer-1", NOW).await;
    assert!(
        matches!(decision, PaymentDecision::Invalidated { reason: InvalidationReason::Rejected }),
        "got {decision:?}"
    );
    let status = w.engine.status(&quote_id).await.unwrap().unwrap();
    assert!(status.frozen.as_deref().unwrap_or_default().contains("eip155:8453"));
    assert!(status.billing_event_id.is_none());
    let last = status.chain.last().unwrap();
    assert_eq!(last.extra.get("network_mismatch"), Some(&serde_json::json!("eip155:8453")));
}

#[tokio::test]
async fn same_quote_retries_still_idempotent_under_the_transaction_guard() {
    // The guard must not break the legitimate case: the SAME quote
    // retried presents the same transaction and stays one charge.
    let w = world(Arc::new(ScriptedSettler {
        transaction: "mock:stable-tx".into(),
        network: MOCK_NETWORK.into(),
    }));
    let quote = w
        .engine
        .issue_quote(
            w.caller.entity_id().clone(),
            CAPABILITY,
            requirements(),
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

    let first = w
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .expect("accept");
    let second = w
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .expect("accept again");
    let (PaymentDecision::Served { billing: b1, .. }, PaymentDecision::Served { billing: b2, .. }) =
        (first, second)
    else {
        panic!("both attempts must serve");
    };
    assert_eq!(b1.billing_event_id, b2.billing_event_id);
}
