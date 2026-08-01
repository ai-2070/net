//! P1 WS5 adversarial rows, engine side: a misbehaving facilitator (or
//! a replayed captured response) cannot double-serve one on-chain
//! settlement, and a receipt from the wrong network is worth nothing.
//! Both invalidate and freeze — misbehavior of the money machinery is
//! never a retryable shrug.

use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::{InvalidationReason, VerificationTier, VerifierRef};
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::mock::{MOCK_NETWORK, MOCK_SCHEME};
use net_payments::facilitator::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
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
        VerifierRef {
            identity: None,
            endpoint: "adversarial-fixture".into(),
        }
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
    World {
        engine,
        caller: EntityKeypair::generate(),
        _dir: dir,
    }
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

/// A facilitator that parks in `verify` until it is told to answer, so a
/// test can hold one attempt open across the claim boundary and see what
/// a second attempt is told meanwhile.
struct BlockingVerifier {
    release: Arc<tokio::sync::Notify>,
    /// Only the *first* verify blocks. The point is to hold one attempt
    /// open across the claim boundary, not to wedge the fixture: the
    /// retry that follows the race has to be able to complete, and a
    /// facilitator that parked on every call would hang the test rather
    /// than fail it.
    held_one: std::sync::atomic::AtomicBool,
}

/// The forged attempt is the one the facilitator will reject; anything
/// else is the real payer. Keyed on the payload rather than on call
/// order, so the fixture does not depend on who wins the race.
const FORGED: &str = "forged-authorization";

#[async_trait]
impl Facilitator for BlockingVerifier {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "blocking-fixture".into(),
        }
    }

    async fn verify(
        &self,
        payload: &X402Carry<PaymentPayload>,
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        if !self
            .held_one
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.release.notified().await;
        }
        let forged = payload.view().payload["mock_authorization"] == FORGED;
        Ok(VerifyOutcome {
            response: X402Carry::author(&VerifyResponse {
                is_valid: !forged,
                invalid_reason: forged.then(|| "forged".to_string()),
                payer: None,
                extra: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
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
                transaction: "mock:settled".into(),
                network: MOCK_NETWORK.into(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
        })
    }
}

/// An unverified attempt holds a quote, but it does not get to say the
/// quote was paid.
///
/// The claim is taken before the facilitator verifies — verification is
/// network I/O and does not hold the store lock — and the semantic replay
/// key is derived from the authorization's `(from, nonce)` and scope,
/// every part of which an observer of a real authorization knows. So the
/// attempt sitting on the record need not be the payer: a forged payload
/// claims the same quote just as well.
///
/// That race cannot be closed by ordering alone without moving signature
/// verification into the engine, which is the facilitator's job. What it
/// must not do is *lie*: the real payer arriving mid-race used to be told
/// `QuoteAlreadyPaid`, a terminal rejection, when nothing had been paid
/// and the quote would be free again a round trip later. `InProgress` is
/// retryable and true.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unverified_attempt_holds_a_quote_without_claiming_it_was_paid() {
    let release = Arc::new(tokio::sync::Notify::new());
    let w = world(Arc::new(BlockingVerifier {
        release: release.clone(),
        held_one: std::sync::atomic::AtomicBool::new(false),
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

    let payload = |auth: &str| {
        X402Carry::author(&PaymentPayload {
            x402_version: 2,
            resource: None,
            accepted: quote.requirements.view().clone(),
            payload: serde_json::json!({ "mock_authorization": auth }),
            extensions: None,
        })
        .expect("payload")
    };

    // The forgery claims first and parks inside `verify`.
    let forged = payload(FORGED);
    let engine = &w.engine;
    let quote_ref = &quote;
    let racing = async {
        engine
            .accept_payment(quote_ref, &forged, VerificationTier::Observed, NOW + 1)
            .await
            .expect("accept")
    };

    let observed = async {
        // Let the forgery reach `verify` and block there. Polling the
        // record beats sleeping on a hope: once it exists and is
        // unsettled, the claim is taken and the lock is released.
        loop {
            if engine
                .status(&quote_ref.quote_id)
                .await
                .expect("status")
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }

        // The real payer arrives mid-race.
        let honest = payload("the-real-authorization");
        let blocked = engine
            .accept_payment(quote_ref, &honest, VerificationTier::Observed, NOW + 2)
            .await
            .expect("accept");
        // `notify_one`, not `notify_waiters`: the record existing proves
        // the claim is committed, not that `verify` has registered its
        // waiter yet. `notify_waiters` would drop a signal sent in that
        // gap and hang the test; `notify_one` stores a permit.
        release.notify_one();
        blocked
    };

    let (forged_decision, blocked) = tokio::join!(racing, observed);

    assert!(
        matches!(forged_decision, PaymentDecision::Rejected { .. }),
        "the facilitator rejected the forgery: {forged_decision:?}"
    );
    assert!(
        matches!(blocked, PaymentDecision::InProgress),
        "an unverified attempt in flight is InProgress — retryable and true — \
         not a terminal `quote already has a different payment attached`: {blocked:?}"
    );

    // And the retry `InProgress` invites actually works: the forgery
    // released its claim on rejection, so the quote is free again.
    let honest = payload("the-real-authorization");
    let paid = w
        .engine
        .accept_payment(&quote, &honest, VerificationTier::Observed, NOW + 3)
        .await
        .expect("accept");
    assert!(
        matches!(paid, PaymentDecision::Served { .. }),
        "the real payer gets the quote once the forgery fails verification: {paid:?}"
    );
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
        matches!(
            second,
            PaymentDecision::Invalidated {
                reason: InvalidationReason::Replay
            }
        ),
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
    let redemption = w
        .engine
        .redeem_for_invocation("fixture-tool", &second_id, None)
        .await
        .unwrap();
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
        matches!(
            decision,
            PaymentDecision::Invalidated {
                reason: InvalidationReason::Rejected
            }
        ),
        "got {decision:?}"
    );
    let status = w.engine.status(&quote_id).await.unwrap().unwrap();
    assert!(status
        .frozen
        .as_deref()
        .unwrap_or_default()
        .contains("eip155:8453"));
    assert!(status.billing_event_id.is_none());
    let last = status.chain.last().unwrap();
    assert_eq!(
        last.extra.get("network_mismatch"),
        Some(&serde_json::json!("eip155:8453"))
    );
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
