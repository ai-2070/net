//! P1 WS3, engine side: the independent checker's verdicts land as
//! first-class chain events. A facilitator receipt caps at `observed`;
//! the checker upgrades to `confirmed(n)`/`final` (billing once the
//! required tier is reached), a reverted settlement invalidates and
//! freezes, an on-chain delivered-amount mismatch invalidates, and
//! `Pending` claims nothing either way.

use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_payments::checker::ChainVerdict;
use net_payments::core::registry::{default_mock_registry, default_registry_v1};
use net_payments::core::verification::{
    check_chain, InvalidationReason, VerificationStatus, VerificationTier, VerifierRef,
};
use net_payments::engine::{
    AdmitAll, PaymentDecision, PaymentEngine, RedeemDecision, RedeemDenialReason, RejectReason,
};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

mod scripted_checker;
use scripted_checker::ScriptedChecker;

const NOW: u64 = 1_000_000_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

struct World {
    engine: PaymentEngine,
    quote_id: String,
    _dir: tempfile::TempDir,
}

/// Settle a mock payment at `required_tier` and return the engine +
/// quote (Served when observed suffices; PendingTier otherwise).
async fn settled_world(required_tier: VerificationTier) -> (World, PaymentDecision) {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(MockFacilitator::new()),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");

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
        .accept_payment(&quote, &payload, required_tier, NOW + 1)
        .await
        .expect("accept");
    (
        World {
            engine,
            quote_id: quote.quote_id.clone(),
            _dir: dir,
        },
        decision,
    )
}

#[tokio::test]
async fn the_checker_upgrades_the_tier_and_bills_at_the_required_depth() {
    // Provider requires confirmed(1): the receipt alone (observed) holds.
    let (w, first) = settled_world(VerificationTier::Confirmed(1)).await;
    assert!(
        matches!(first, PaymentDecision::PendingTier { .. }),
        "{first:?}"
    );

    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Confirmed(3),
        delivered: Some("2500".into()),
    }]);
    let decision = w
        .engine
        .re_verify_with_checker(
            &w.quote_id,
            &checker,
            VerificationTier::Confirmed(1),
            NOW + 2,
        )
        .await
        .expect("engine");
    let PaymentDecision::Served { billing, tier } = decision else {
        panic!("expected Served after the independent check, got {decision:?}");
    };
    assert_eq!(tier, VerificationTier::Confirmed(3));
    assert_eq!(billing.amount.to_canonical_string(), "2500");

    // The chain records both verifiers: the facilitator's observed
    // receipt, then the checker's confirmed event.
    let status = w.engine.status(&w.quote_id).await.unwrap().unwrap();
    check_chain(&status.chain).expect("link-valid");
    assert_eq!(status.chain.len(), 2);
    assert_eq!(status.chain[0].verifier.endpoint, "mock");
    assert_eq!(
        status.chain[1].verifier.endpoint,
        "independent-chain-check:scripted"
    );
    assert_eq!(status.chain[1].tier, VerificationTier::Confirmed(3));

    // The checker was asked about the right transfer.
    let queries = checker.queries.lock();
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].0, MOCK_NETWORK);
    assert_eq!(
        queries[0].2.as_ref().map(|q| q.to.as_str()),
        Some("mock-provider-settle-addr")
    );
}

#[tokio::test]
async fn final_is_reachable_only_through_the_checker() {
    let (w, served) = settled_world(VerificationTier::Observed).await;
    assert!(matches!(served, PaymentDecision::Served { .. }));

    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Final,
        delivered: Some("2500".into()),
    }]);
    let decision = w
        .engine
        .re_verify_with_checker(&w.quote_id, &checker, VerificationTier::Final, NOW + 2)
        .await
        .expect("engine");
    let PaymentDecision::Served { tier, .. } = decision else {
        panic!("expected Served@final, got {decision:?}");
    };
    assert_eq!(tier, VerificationTier::Final);
    let status = w.engine.status(&w.quote_id).await.unwrap().unwrap();
    assert_eq!(status.tier, Some(VerificationTier::Final));
}

#[tokio::test]
async fn pending_claims_nothing_and_appends_nothing() {
    let (w, _) = settled_world(VerificationTier::Confirmed(1)).await;
    let before = w
        .engine
        .status(&w.quote_id)
        .await
        .unwrap()
        .unwrap()
        .chain
        .len();

    let checker = ScriptedChecker::new(vec![ChainVerdict::Pending]);
    let decision = w
        .engine
        .re_verify_with_checker(
            &w.quote_id,
            &checker,
            VerificationTier::Confirmed(1),
            NOW + 2,
        )
        .await
        .expect("engine");
    assert!(
        matches!(
            decision,
            PaymentDecision::PendingTier {
                reached: VerificationTier::Observed,
                ..
            }
        ),
        "{decision:?}"
    );
    let after = w
        .engine
        .status(&w.quote_id)
        .await
        .unwrap()
        .unwrap()
        .chain
        .len();
    assert_eq!(
        before, after,
        "pending is the absence of a fact, never an event"
    );
}

#[tokio::test]
async fn a_reverted_settlement_invalidates_and_freezes() {
    let (w, served) = settled_world(VerificationTier::Observed).await;
    assert!(matches!(served, PaymentDecision::Served { .. }));

    let checker = ScriptedChecker::new(vec![ChainVerdict::Reverted]);
    let decision = w
        .engine
        .re_verify_with_checker(&w.quote_id, &checker, VerificationTier::Observed, NOW + 2)
        .await
        .expect("engine");
    assert!(
        matches!(
            decision,
            PaymentDecision::Invalidated {
                reason: InvalidationReason::Rejected
            }
        ),
        "{decision:?}"
    );
    let status = w.engine.status(&w.quote_id).await.unwrap().unwrap();
    assert!(status.frozen.is_some());
    assert!(matches!(
        status.chain.last().unwrap().status,
        VerificationStatus::Invalidated {
            reason: InvalidationReason::Rejected
        }
    ));

    // Frozen means frozen: the redemption gate refuses too.
    let redemption = w
        .engine
        .redeem_for_invocation("fixture-tool", &w.quote_id, None)
        .await
        .unwrap();
    assert!(matches!(
        redemption,
        net_payments::engine::RedeemDecision::Denied { .. }
    ));
}

/// A settlement below the required tier denies redemption with the
/// *pending* vocabulary, never "never completed": the payment exists
/// and awaits confidence — "never paid" and "paid, awaiting confidence"
/// route differently on the caller side.
#[tokio::test]
async fn a_pending_settlement_denies_redemption_as_pending_not_unpaid() {
    let (w, decision) = settled_world(VerificationTier::Confirmed(1)).await;
    assert!(
        matches!(decision, PaymentDecision::PendingTier { .. }),
        "{decision:?}"
    );

    let redemption = w
        .engine
        .redeem_for_invocation("fixture-tool", &w.quote_id, None)
        .await
        .unwrap();
    match redemption {
        RedeemDecision::Denied { reason } => {
            assert_eq!(reason, RedeemDenialReason::SettlementPending);
            assert!(reason.to_string().contains("not yet billed"), "{reason}");
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[tokio::test]
async fn an_on_chain_delivered_mismatch_invalidates() {
    let (w, _) = settled_world(VerificationTier::Confirmed(1)).await;

    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Final,
        delivered: Some("2499".into()),
    }]);
    let decision = w
        .engine
        .re_verify_with_checker(
            &w.quote_id,
            &checker,
            VerificationTier::Confirmed(1),
            NOW + 2,
        )
        .await
        .expect("engine");
    assert!(
        matches!(
            decision,
            PaymentDecision::Invalidated {
                reason: InvalidationReason::AmountMismatch
            }
        ),
        "{decision:?}"
    );
    let status = w.engine.status(&w.quote_id).await.unwrap().unwrap();
    assert!(status.frozen.is_some());
    assert!(
        status.billing_event_id.is_none(),
        "an underdelivered settlement never bills"
    );
}

#[tokio::test]
async fn checker_failure_is_structured_and_frozen_quotes_stay_refused() {
    let (w, _) = settled_world(VerificationTier::Confirmed(1)).await;

    let exhausted = ScriptedChecker::new(vec![]);
    let decision = w
        .engine
        .re_verify_with_checker(
            &w.quote_id,
            &exhausted,
            VerificationTier::Confirmed(1),
            NOW + 2,
        )
        .await
        .expect("engine");
    assert!(
        matches!(
            decision,
            PaymentDecision::FacilitatorFailure {
                retryable: true,
                ..
            }
        ),
        "{decision:?}"
    );

    // Freeze it, then the checker path refuses before any I/O matters.
    let reverter = ScriptedChecker::new(vec![ChainVerdict::Reverted]);
    let _ = w
        .engine
        .re_verify_with_checker(
            &w.quote_id,
            &reverter,
            VerificationTier::Confirmed(1),
            NOW + 3,
        )
        .await
        .expect("engine");
    let after = ScriptedChecker::new(vec![ChainVerdict::Pending]);
    let decision = w
        .engine
        .re_verify_with_checker(&w.quote_id, &after, VerificationTier::Confirmed(1), NOW + 4)
        .await
        .expect("engine");
    assert!(matches!(
        decision,
        PaymentDecision::Rejected {
            reason: RejectReason::QuoteFrozen(_)
        }
    ));
}

/// A facilitator that names the on-chain payer in its settle response —
/// the shape of an exact-SVM settlement, where the payload is an opaque
/// wallet blob with no `authorization.from` for the engine to read.
struct PayerNamingFacilitator;

const NAMED_PAYER: &str = "PayerWa11et11111111111111111111111111111111";

#[async_trait]
impl net_payments::facilitator::Facilitator for PayerNamingFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "test-payer-naming".into(),
        }
    }
    async fn verify(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<net_payments::facilitator::VerifyOutcome, net_payments::facilitator::FacilitatorError>
    {
        Ok(net_payments::facilitator::VerifyOutcome {
            response: X402Carry::author(&net_payments::x402::settlement::VerifyResponse {
                is_valid: true,
                invalid_reason: None,
                payer: Some(NAMED_PAYER.into()),
                extra: None,
            })
            .map_err(|e| net_payments::facilitator::FacilitatorError::protocol(e.to_string()))?,
        })
    }
    async fn settle(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<net_payments::facilitator::SettleOutcome, net_payments::facilitator::FacilitatorError>
    {
        Ok(net_payments::facilitator::SettleOutcome {
            response: X402Carry::author(&net_payments::x402::settlement::SettlementResponse {
                success: true,
                error_reason: None,
                payer: Some(NAMED_PAYER.into()),
                transaction: "svm:settled-1".into(),
                network: requirements.view().network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| net_payments::facilitator::FacilitatorError::protocol(e.to_string()))?,
        })
    }
}

/// P2 WS-A: when the payload carries no `authorization.from` (exact-SVM's
/// opaque wallet blob — the mock payload here has the same property), the
/// engine records the facilitator's settle-time payer claim as a chain
/// fact and threads it into the checker's query. A facilitator that later
/// substitutes some other customer's transaction is pinned to the payer
/// it named when it first settled.
#[tokio::test]
async fn the_recorded_settle_payer_reaches_the_checker_when_the_payload_names_none() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(PayerNamingFacilitator),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");

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
    // No `authorization` object anywhere in the payload.
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "transaction": "b64-opaque-wallet-blob" }),
        extensions: None,
    })
    .expect("payload");

    // Settle at observed while the provider requires confirmed(1).
    let first = engine
        .accept_payment(&quote, &payload, VerificationTier::Confirmed(1), NOW + 1)
        .await
        .expect("accept");
    assert!(
        matches!(first, PaymentDecision::PendingTier { .. }),
        "{first:?}"
    );

    // The independent re-check receives the recorded settle-time payer.
    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Confirmed(3),
        delivered: Some("2500".into()),
    }]);
    let decision = engine
        .re_verify_with_checker(
            &quote.quote_id,
            &checker,
            VerificationTier::Confirmed(1),
            NOW + 2,
        )
        .await
        .expect("engine");
    assert!(
        matches!(decision, PaymentDecision::Served { .. }),
        "{decision:?}"
    );

    let queries = checker.queries.lock();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0].2.as_ref().and_then(|q| q.from.as_deref()),
        Some(NAMED_PAYER),
        "the checker must be asked to bind delivery to the settle-time payer"
    );
}

/// N-2 regression: on a non-eip155 network the reference threaded to the
/// checker must be the PROVIDER-authored `invoiceId`, never a
/// caller-supplied `payload.authorization.nonce`. Off-EVM payloads sign
/// only their wallet blob (not the surrounding JSON), so a caller who
/// injects an unsigned `authorization.nonce` must not be able to
/// override the provider's invoice bind. (On eip155 the nonce is
/// caller-SIGNED and correctly wins — that path is exercised by the
/// eip155 checker suite.)
#[tokio::test]
async fn an_injected_nonce_does_not_override_the_provider_invoice_off_evm() {
    const PROVIDER_INVOICE: &str = "inv-provider-7";
    const CALLER_INJECTED_NONCE: &str = "0xdeadbeef-not-the-invoice";

    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(MockFacilitator::new()),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");

    // The provider authors an invoiceId in requirements.extra (exact-XRPL's
    // vocabulary); MOCK_NETWORK is not eip155.
    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({ "invoiceId": PROVIDER_INVOICE })),
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
    // The caller smuggles an unsigned `authorization.nonce` into the
    // opaque payload, hoping to override the invoice bind.
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({
            "mock_authorization": "payer-1",
            "authorization": { "nonce": CALLER_INJECTED_NONCE },
        }),
        extensions: None,
    })
    .expect("payload");

    let first = engine
        .accept_payment(&quote, &payload, VerificationTier::Confirmed(1), NOW + 1)
        .await
        .expect("accept");
    assert!(
        matches!(first, PaymentDecision::PendingTier { .. }),
        "{first:?}"
    );

    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Confirmed(3),
        delivered: Some("2500".into()),
    }]);
    let decision = engine
        .re_verify_with_checker(
            &quote.quote_id,
            &checker,
            VerificationTier::Confirmed(1),
            NOW + 2,
        )
        .await
        .expect("engine");
    assert!(
        matches!(decision, PaymentDecision::Served { .. }),
        "{decision:?}"
    );

    let queries = checker.queries.lock();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0].2.as_ref().and_then(|q| q.reference.as_deref()),
        Some(PROVIDER_INVOICE),
        "the provider invoiceId must win; a caller-injected nonce must not override it off-EVM"
    );
}

// ---------------------------------------------------------------------
// eip155 nonce bind is MANDATORY at re-verification (cubic P1 follow-up)
// ---------------------------------------------------------------------

const BASE_SEPOLIA: &str = "eip155:84532";
const TESTNET_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const MERCHANT_ADDR: &str = "0x000000000000000000000000000000000000dEaD";
// 0x + 64 hex — a well-formed EIP-3009 bytes32 nonce.
const VALID_NONCE: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

/// Settle one Base-Sepolia (eip155) quote at observed with the given
/// opaque payload body and return the engine + quote id. Fresh engine per
/// call: `PayerNamingFacilitator` reports a fixed transaction hash, so
/// distinct quotes must not share a `consumed_transactions` namespace.
/// Stand up an eip155 engine + quote + payload and run `accept_payment`,
/// handing back the engine, the quote id, and the raw decision.
///
/// The two wrappers below differ only in what they do with the decision,
/// so the setup lives here once.
async fn accept_eip155_inner(
    payload_body: serde_json::Value,
) -> (PaymentEngine, String, PaymentDecision, tempfile::TempDir) {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(PayerNamingFacilitator),
        Arc::new(AdmitAll),
        default_registry_v1(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");
    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: BASE_SEPOLIA.into(),
        amount: "2500".into(),
        asset: TESTNET_USDC.into(),
        pay_to: MERCHANT_ADDR.into(),
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
        payload: payload_body,
        extensions: None,
    })
    .expect("payload");
    let decision = engine
        .accept_payment(&quote, &payload, VerificationTier::Confirmed(1), NOW + 1)
        .await
        .expect("accept");
    (engine, quote.quote_id, decision, dir)
}

/// The raw acceptance decision — for cases where the interesting outcome
/// is a refusal *before* settlement.
async fn accept_eip155(payload_body: serde_json::Value) -> PaymentDecision {
    accept_eip155_inner(payload_body).await.2
}

/// A settled eip155 quote, ready to re-verify. Asserts the settlement
/// actually happened, so a test that meant to reach re-verification does
/// not silently assert against a refusal.
async fn settled_eip155(
    payload_body: serde_json::Value,
) -> (PaymentEngine, String, tempfile::TempDir) {
    let (engine, quote_id, decision, dir) = accept_eip155_inner(payload_body).await;
    assert!(
        matches!(decision, PaymentDecision::PendingTier { .. }),
        "{decision:?}"
    );
    (engine, quote_id, dir)
}

/// The happy path: a valid caller-signed nonce is threaded to the checker
/// as the reference (so the `AuthorizationUsed` bind can fire).
#[tokio::test]
async fn eip155_reverify_threads_the_signed_nonce() {
    let (engine, quote_id, _dir) = settled_eip155(serde_json::json!({
        "authorization": { "from": MERCHANT_ADDR, "nonce": VALID_NONCE }
    }))
    .await;
    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Confirmed(3),
        delivered: Some("2500".into()),
    }]);
    let decision = engine
        .re_verify_with_checker(&quote_id, &checker, VerificationTier::Confirmed(1), NOW + 2)
        .await
        .expect("engine");
    assert!(
        matches!(decision, PaymentDecision::Served { .. }),
        "{decision:?}"
    );
    let queries = checker.queries.lock();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0].2.as_ref().and_then(|q| q.reference.as_deref()),
        Some(VALID_NONCE),
        "the eip155 reference must be the caller-signed nonce"
    );
}

/// Fail-closed, and now earlier than it used to be: an eip155 `exact`
/// payload carrying no usable EIP-3009 authorization is refused at
/// **accept**, so it never settles at all.
///
/// It used to settle and then be refused at re-verification, because the
/// replay index keyed on the whole payload wrapper and did not care what
/// the scheme object held. Keying replay on the scheme's own signed
/// material (M5) means a payload with no authorization has no replay
/// identity — and something the engine cannot identify is something it
/// must not accept. The old late refusal is still there for a payload
/// that has an authorization but a malformed nonce.
#[tokio::test]
async fn eip155_refuses_a_payload_with_no_usable_authorization() {
    // (a) No `authorization` in the payload at all: refused at accept,
    //     because there is no replay identity to key on. The payment
    //     never settles, so there is nothing for a checker to be asked
    //     about later.
    let refused = accept_eip155(serde_json::json!({ "signature": "0xsig" })).await;
    assert!(
        matches!(&refused, PaymentDecision::Rejected { reason: RejectReason::BadQuote(m) } if m.contains("replay identity")),
        "a payload with no authorization must be refused at accept, got {refused:?}"
    );

    // (b) A well-formed authorization whose nonce is not a 32-byte hex
    //     word. This one HAS a replay identity, so it settles — and is
    //     then refused at re-verification, where the nonce has to bind an
    //     `AuthorizationUsed` event. The late refusal still matters.
    let (engine, quote_id, _dir) = settled_eip155(serde_json::json!({
        "authorization": { "from": MERCHANT_ADDR, "nonce": "not-a-nonce" }
    }))
    .await;
    let checker = ScriptedChecker::new(vec![ChainVerdict::Included {
        tier: VerificationTier::Confirmed(3),
        delivered: Some("2500".into()),
    }]);
    let decision = engine
        .re_verify_with_checker(&quote_id, &checker, VerificationTier::Confirmed(1), NOW + 2)
        .await
        .expect("engine");
    assert!(
        matches!(
            &decision,
            PaymentDecision::Rejected {
                reason: RejectReason::BadQuote(_)
            }
        ),
        "malformed nonce must be refused, got {decision:?}"
    );
    assert!(checker.queries.lock().is_empty());
}
