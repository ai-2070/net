//! Workstream 2 acceptance: one lifecycle test per injectable facilitator
//! mode, asserting the **exact** signed event chain; reorg-after-serve
//! freezes the quote; same-key retry means one settle, one serve, one
//! billing event id; and the engine drives everything through the
//! `Facilitator` trait alone (a bespoke test facilitator proves the P1
//! swap needs zero interface changes).

use std::sync::Arc;

use net::adapter::net::identity::{EntityId, EntityKeypair};
use net_payments::billing::BillingLog;
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::{
    check_chain, ExceptionKind, InvalidationReason, VerificationStatus, VerificationTier,
};
use net_payments::engine::{
    invocation_binding_transcript, AdmitAll, PaymentDecision, PaymentEngine,
    ProviderAdmissionPolicy, RedeemDecision, RejectReason,
};
use net_payments::facilitator::mock::{MockFacilitator, MockMode, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::facilitator::{
    Facilitator, FacilitatorError, FacilitatorErrorKind, SettleOutcome, VerifyOutcome,
};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::settlement::SettlementResponse;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const TTL: u64 = 60_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

struct Harness {
    engine: PaymentEngine,
    facilitator: Arc<MockFacilitator>,
    provider: Arc<EntityKeypair>,
    caller: EntityKeypair,
    _dir: tempfile::TempDir,
}

fn harness() -> Harness {
    harness_with_policy(Arc::new(AdmitAll))
}

fn harness_with_policy(policy: Arc<dyn ProviderAdmissionPolicy>) -> Harness {
    let provider = Arc::new(EntityKeypair::generate());
    let facilitator = Arc::new(MockFacilitator::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        facilitator.clone(),
        policy,
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");
    Harness {
        engine,
        facilitator,
        provider,
        caller: EntityKeypair::generate(),
        _dir: dir,
    }
}

fn requirements(amount: &str) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: amount.into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author requirements")
}

fn payload_for(quote: &PaymentQuote, nonce: &str) -> X402Carry<PaymentPayload> {
    X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": nonce }),
        extensions: None,
    })
    .expect("author payload")
}

impl Harness {
    fn quote(&self, amount: &str) -> PaymentQuote {
        self.quote_at(amount, NOW)
    }
    fn quote_at(&self, amount: &str, issued_ns: u64) -> PaymentQuote {
        self.engine
            .issue_quote(
                self.caller.entity_id().clone(),
                CAPABILITY,
                requirements(amount),
                issued_ns,
                TTL,
            )
            .expect("issue quote")
    }
    async fn chain_statuses(&self, quote_id: &str) -> Vec<(VerificationStatus, VerificationTier)> {
        let status = self
            .engine
            .status(quote_id)
            .await
            .expect("status")
            .expect("record exists");
        check_chain(&status.chain).expect("stored chain is link-valid");
        for ev in &status.chain {
            ev.verify_signature().expect("every chain event is signed");
        }
        status.chain.iter().map(|e| (e.status, e.tier)).collect()
    }
}

// ---------------------------------------------------------------------
// success
// ---------------------------------------------------------------------

#[tokio::test]
async fn success_mode_serves_with_the_exact_chain() {
    let h = harness();
    let quote = h.quote("2500");
    let payload = payload_for(&quote, "payer-1");

    let decision = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .expect("engine");
    let billing = match decision {
        PaymentDecision::Served { billing, tier } => {
            assert_eq!(tier, VerificationTier::Observed);
            billing
        }
        other => panic!("expected Served, got {other:?}"),
    };
    billing.verify_signature().expect("billing event is signed");
    assert_eq!(billing.amount.to_canonical_string(), "2500");
    assert_eq!(billing.quote_id, quote.quote_id);
    assert_eq!(billing.payer, *h.caller.entity_id());
    assert_eq!(billing.payee, *h.provider.entity_id());

    // The exact event chain: one Verified@observed.
    assert_eq!(
        h.chain_statuses(&quote.quote_id).await,
        vec![(VerificationStatus::Verified, VerificationTier::Observed)]
    );

    // The billing event references the chain head.
    let status = h.engine.status(&quote.quote_id).await.unwrap().unwrap();
    assert_eq!(
        billing.verification_ref.as_deref(),
        Some(status.chain[0].chain_hash().unwrap().as_str())
    );
}

// ---------------------------------------------------------------------
// idempotency: one settle, one serve, one billing event id
// ---------------------------------------------------------------------

#[tokio::test]
async fn same_key_retry_is_one_settle_one_billing_event() {
    let h = harness();
    let quote = h.quote("2500");
    let payload = payload_for(&quote, "payer-1");

    let first = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    let second = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();

    let (PaymentDecision::Served { billing: b1, .. }, PaymentDecision::Served { billing: b2, .. }) =
        (first, second)
    else {
        panic!("both attempts must serve");
    };
    assert_eq!(
        b1.billing_event_id, b2.billing_event_id,
        "one billing event id"
    );
    // One settle: the mock rejects a second settle of the same payment, so
    // reaching Served twice proves the engine never re-settled.
    assert_eq!(
        h.chain_statuses(&quote.quote_id).await.len(),
        1,
        "retry appends no new chain events"
    );
}

// ---------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------

#[tokio::test]
async fn replayed_payload_never_satisfies_a_second_quote() {
    let h = harness();
    let q1 = h.quote("2500");
    // Same terms, later issuance → a different quote id.
    let q2 = h.quote_at("2500", NOW + 10);
    assert_ne!(q1.quote_id, q2.quote_id);

    // Payloads accept identical requirements, so the same payload binds
    // to both quotes scheme-wise — the replay index is what stops it.
    let payload = payload_for(&q1, "payer-1");

    let first = h
        .engine
        .accept_payment(&q1, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(matches!(first, PaymentDecision::Served { .. }));

    let replay = h
        .engine
        .accept_payment(&q2, &payload, VerificationTier::Observed, NOW + 11)
        .await
        .unwrap();
    assert!(
        matches!(
            replay,
            PaymentDecision::Rejected {
                reason: RejectReason::Replay
            }
        ),
        "got {replay:?}"
    );
    // The second quote never even reached the facilitator: no record.
    assert!(h.engine.status(&q2.quote_id).await.unwrap().is_none());
}

/// M2 regression: the replay guard keys on the canonical payload, so a
/// second quote presenting a byte-different *re-encoding* of the same
/// authorization is still caught. Before the fix the byte-keyed index
/// missed it and both quotes served for one logical payment.
#[tokio::test]
async fn a_reencoded_payload_never_satisfies_a_second_quote() {
    let h = harness();
    let q1 = h.quote("2500");
    let q2 = h.quote_at("2500", NOW + 10);
    assert_ne!(q1.quote_id, q2.quote_id);

    let payload_a = payload_for(&q1, "payer-1");
    // A byte-different encoding of the identical logical payload: same
    // fields, pretty-printed instead of compact.
    let value: serde_json::Value = serde_json::from_slice(payload_a.bytes()).unwrap();
    let reencoded = serde_json::to_vec_pretty(&value).unwrap();
    let payload_b: X402Carry<PaymentPayload> = X402Carry::from_bytes(reencoded).unwrap();
    assert_ne!(
        payload_a.content_hash(),
        payload_b.content_hash(),
        "the re-encoding must differ byte-for-byte or the test proves nothing"
    );

    let first = h
        .engine
        .accept_payment(&q1, &payload_a, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(matches!(first, PaymentDecision::Served { .. }));

    let replay = h
        .engine
        .accept_payment(&q2, &payload_b, VerificationTier::Observed, NOW + 11)
        .await
        .unwrap();
    assert!(
        matches!(
            replay,
            PaymentDecision::Rejected {
                reason: RejectReason::Replay
            }
        ),
        "a re-encoded payload must still be caught as replay, got {replay:?}"
    );
    assert!(h.engine.status(&q2.quote_id).await.unwrap().is_none());
}

#[tokio::test]
async fn facilitator_reported_replay_rejects_and_consumes_nothing() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::Replay);
    let payload = payload_for(&quote, "payer-1");

    let decision = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    match decision {
        PaymentDecision::Rejected {
            reason: RejectReason::VerifyRejected(r),
        } => {
            assert_eq!(r, "payload_replayed")
        }
        other => panic!("expected VerifyRejected, got {other:?}"),
    }
    assert!(
        h.engine.status(&quote.quote_id).await.unwrap().is_none(),
        "claim released"
    );
}

// ---------------------------------------------------------------------
// wrong_amount
// ---------------------------------------------------------------------

#[tokio::test]
async fn wrong_amount_mode_rejects_with_no_charge_and_releases_the_payload() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::WrongAmount);
    let payload = payload_for(&quote, "payer-1");

    let decision = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    match decision {
        PaymentDecision::Rejected {
            reason: RejectReason::VerifyRejected(r),
        } => {
            assert_eq!(r, "wrong_amount")
        }
        other => panic!("expected VerifyRejected(wrong_amount), got {other:?}"),
    }
    // No chain, no billing, and the payload is free to satisfy a healthy
    // quote afterwards (nothing was consumed).
    assert!(h.engine.status(&quote.quote_id).await.unwrap().is_none());
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::Success);
    let retry = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(retry, PaymentDecision::Served { .. }),
        "got {retry:?}"
    );
}

// ---------------------------------------------------------------------
// late_finality
// ---------------------------------------------------------------------

#[tokio::test]
async fn late_finality_withholds_serving_until_the_tier_is_reached() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::LateFinality);
    let payload = payload_for(&quote, "payer-1");
    let required = VerificationTier::Confirmed(1);

    // Settles, but the receipt is only `observed`.
    let first = h
        .engine
        .accept_payment(&quote, &payload, required, NOW + 1)
        .await
        .unwrap();
    assert!(
        matches!(
            first,
            PaymentDecision::PendingTier {
                reached: VerificationTier::Observed,
                required: VerificationTier::Confirmed(1)
            }
        ),
        "got {first:?}"
    );

    // Second verify: still observed (mock reaches confirmed on call 3).
    let second = h
        .engine
        .re_verify(&quote.quote_id, required, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(second, PaymentDecision::PendingTier { .. }),
        "got {second:?}"
    );

    // Third verify: confirmed(1) → served, billing emitted now.
    let third = h
        .engine
        .re_verify(&quote.quote_id, required, NOW + 3)
        .await
        .unwrap();
    let PaymentDecision::Served { billing, tier } = third else {
        panic!("expected Served");
    };
    assert_eq!(tier, VerificationTier::Confirmed(1));
    billing.verify_signature().unwrap();

    // The exact chain: observed (settle), observed (re-verify), confirmed.
    assert_eq!(
        h.chain_statuses(&quote.quote_id).await,
        vec![
            (VerificationStatus::Verified, VerificationTier::Observed),
            (VerificationStatus::Verified, VerificationTier::Observed),
            (VerificationStatus::Verified, VerificationTier::Confirmed(1)),
        ]
    );

    // An accept retry on the settled quote is a re-verify, never a second
    // settle — and returns the same billing event.
    let retry = h
        .engine
        .accept_payment(&quote, &payload, required, NOW + 4)
        .await
        .unwrap();
    let PaymentDecision::Served {
        billing: retry_billing,
        ..
    } = retry
    else {
        panic!("expected Served on retry");
    };
    assert_eq!(retry_billing.billing_event_id, billing.billing_event_id);
}

// ---------------------------------------------------------------------
// reorg_invalidate
// ---------------------------------------------------------------------

#[tokio::test]
async fn reorg_after_serve_freezes_the_quote_and_keeps_billing_immutable() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::ReorgInvalidate);
    let payload = payload_for(&quote, "payer-1");

    // First pass verifies and serves (receipt issued).
    let served = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    let PaymentDecision::Served { billing, .. } = served else {
        panic!("expected Served");
    };

    // The chain reorgs out: invalidated{reorg}, quote frozen.
    let reorg = h
        .engine
        .re_verify(&quote.quote_id, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(
            reorg,
            PaymentDecision::Invalidated {
                reason: InvalidationReason::Reorg
            }
        ),
        "got {reorg:?}"
    );

    // Exact chain: Verified@observed then Invalidated{reorg}; link-valid.
    assert_eq!(
        h.chain_statuses(&quote.quote_id).await,
        vec![
            (VerificationStatus::Verified, VerificationTier::Observed),
            (
                VerificationStatus::Invalidated {
                    reason: InvalidationReason::Reorg
                },
                VerificationTier::Observed
            ),
        ]
    );

    // Frozen: nothing further serves against this quote.
    let after = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 3)
        .await
        .unwrap();
    assert!(
        matches!(
            after,
            PaymentDecision::Rejected {
                reason: RejectReason::QuoteFrozen(_)
            }
        ),
        "got {after:?}"
    );
    let reverify = h
        .engine
        .re_verify(&quote.quote_id, VerificationTier::Observed, NOW + 4)
        .await
        .unwrap();
    assert!(matches!(
        reverify,
        PaymentDecision::Rejected {
            reason: RejectReason::QuoteFrozen(_)
        }
    ));

    // The billing event is immutable: still present, same id, and the
    // invalidation event references the same quote for the audit trail.
    let status = h.engine.status(&quote.quote_id).await.unwrap().unwrap();
    assert_eq!(
        status.billing_event_id.as_deref(),
        Some(billing.billing_event_id.as_str())
    );
    assert_eq!(status.frozen.as_deref(), Some("reorged_out"));
}

// ---------------------------------------------------------------------
// expired_requirements + engine-side quote expiry
// ---------------------------------------------------------------------

#[tokio::test]
async fn expired_requirements_mode_rejects() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator.arm(
        quote.requirements.content_hash(),
        MockMode::ExpiredRequirements,
    );
    let payload = payload_for(&quote, "payer-1");
    let decision = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    match decision {
        PaymentDecision::Rejected {
            reason: RejectReason::VerifyRejected(r),
        } => {
            assert_eq!(r, "expired_requirements")
        }
        other => panic!("expected VerifyRejected, got {other:?}"),
    }
}

#[tokio::test]
async fn expired_quotes_are_rejected_before_the_facilitator_is_consulted() {
    let h = harness();
    let quote = h.quote("2500");
    // Arm a timeout: if the engine consulted the facilitator, the decision
    // would be FacilitatorFailure, not QuoteExpired.
    h.facilitator.arm(
        quote.requirements.content_hash(),
        MockMode::VerificationTimeout,
    );
    let payload = payload_for(&quote, "payer-1");
    let decision = h
        .engine
        .accept_payment(
            &quote,
            &payload,
            VerificationTier::Observed,
            quote.expires_at_ns,
        )
        .await
        .unwrap();
    assert!(matches!(
        decision,
        PaymentDecision::Rejected {
            reason: RejectReason::QuoteExpired
        }
    ));
}

// ---------------------------------------------------------------------
// verification_timeout: fail-closed, structured, retryable
// ---------------------------------------------------------------------

#[tokio::test]
async fn verification_timeout_fails_closed_and_a_retry_charges_exactly_once() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator.arm(
        quote.requirements.content_hash(),
        MockMode::VerificationTimeout,
    );
    let payload = payload_for(&quote, "payer-1");

    let decision = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    match decision {
        PaymentDecision::FacilitatorFailure {
            kind, retryable, ..
        } => {
            assert_eq!(kind, FacilitatorErrorKind::Timeout);
            assert!(retryable, "timeout is a retryable failure for policy");
        }
        other => panic!("expected FacilitatorFailure, got {other:?}"),
    }
    // Fail-closed and nothing consumed.
    assert!(h.engine.status(&quote.quote_id).await.unwrap().is_none());

    // Facilitator heals → the same payload settles exactly once.
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::Success);
    let retry = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(matches!(retry, PaymentDecision::Served { .. }));
    assert_eq!(h.chain_statuses(&quote.quote_id).await.len(), 1);
}

// ---------------------------------------------------------------------
// overpayment: verification exception, never auto-satisfied
// ---------------------------------------------------------------------

/// A bespoke facilitator that over-delivers — also the proof that the
/// engine needs nothing beyond the `Facilitator` trait (the P1 swap).
struct OverpayingFacilitator;

#[async_trait::async_trait]
impl Facilitator for OverpayingFacilitator {
    fn reference(&self) -> net_payments::core::verification::VerifierRef {
        net_payments::core::verification::VerifierRef {
            identity: None,
            endpoint: "test-overpay".into(),
        }
    }
    async fn verify(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        Ok(VerifyOutcome {
            response: X402Carry::author(&net_payments::x402::settlement::VerifyResponse {
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
                transaction: "test:overpay".into(),
                network: requirements.view().network.clone(),
                amount: Some("9999999".into()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }
}

#[tokio::test]
async fn overpayment_is_an_exception_for_provider_policy_not_a_serve() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(OverpayingFacilitator),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .unwrap();

    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            CAPABILITY,
            requirements("2500"),
            NOW,
            TTL,
        )
        .unwrap();
    let payload = payload_for(&quote, "payer-1");
    let decision = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(
        matches!(
            decision,
            PaymentDecision::Exception {
                kind: ExceptionKind::Overpayment
            }
        ),
        "got {decision:?}"
    );
    let status = engine.status(&quote.quote_id).await.unwrap().unwrap();
    assert!(
        !status.served,
        "the verifier never auto-satisfies on overpayment"
    );
    assert!(status.billing_event_id.is_none());
    assert_eq!(status.chain.len(), 1);
    assert!(matches!(
        status.chain[0].status,
        VerificationStatus::Exception {
            kind: ExceptionKind::Overpayment
        }
    ));
}

/// H1 regression: an overpayment leaves the record settled-but-unbilled
/// and unfrozen, so a retry routes `AlreadySettled → re_verify`. That
/// re-verify must re-apply the amount policy and stay an Exception —
/// never promote the overpaid `delivered` into an auto-billed serve.
#[tokio::test]
async fn overpayment_retry_via_re_verify_never_auto_bills() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(OverpayingFacilitator),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .unwrap();

    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            CAPABILITY,
            requirements("2500"),
            NOW,
            TTL,
        )
        .unwrap();
    let payload = payload_for(&quote, "payer-1");

    // First call: overpayment exception, no serve, no billing.
    let first = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(
        matches!(
            first,
            PaymentDecision::Exception {
                kind: ExceptionKind::Overpayment
            }
        ),
        "got {first:?}"
    );

    // Retry (agents retry constantly): AlreadySettled → re_verify. The
    // facilitator would happily re-`verify` as valid, but the recorded
    // delivered amount is still an overpayment.
    let retry = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(
            retry,
            PaymentDecision::Exception {
                kind: ExceptionKind::Overpayment
            }
        ),
        "retry must stay an overpayment exception, got {retry:?}"
    );

    let status = engine.status(&quote.quote_id).await.unwrap().unwrap();
    assert!(!status.served, "an overpayment retry must never serve");
    assert!(
        status.billing_event_id.is_none(),
        "an overpayment retry must never bill"
    );
    // Two exception events now (original settle + the re-verify re-check),
    // both Overpayment; nothing Verified crept in.
    assert_eq!(status.chain.len(), 2);
    for entry in &status.chain {
        assert!(
            matches!(
                entry.status,
                VerificationStatus::Exception {
                    kind: ExceptionKind::Overpayment
                }
            ),
            "every event stays an overpayment exception, got {:?}",
            entry.status
        );
    }
}

// ---------------------------------------------------------------------
// crash recovery: a stranded in_flight claim is reclaimable (M3)
// ---------------------------------------------------------------------

/// A facilitator whose first `verify` never returns — standing in for an
/// attempt that claims the quote (persisting `in_flight`) and then the
/// process dies before completion. Once healed, later calls settle.
struct CrashSimFacilitator {
    healed: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Facilitator for CrashSimFacilitator {
    fn reference(&self) -> net_payments::core::verification::VerifierRef {
        net_payments::core::verification::VerifierRef {
            identity: None,
            endpoint: "test-crash-sim".into(),
        }
    }
    async fn verify(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        if !self.healed.load(std::sync::atomic::Ordering::SeqCst) {
            // The "crashed" attempt hangs here forever; the test aborts it.
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
        Ok(VerifyOutcome {
            response: X402Carry::author(&net_payments::x402::settlement::VerifyResponse {
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
                transaction: "test:crash-recovered".into(),
                network: requirements.view().network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }
}

/// M3 regression: a claim that persists `in_flight=true` and then never
/// completes (process crash mid verify/settle) must not strand the quote
/// forever. Before the TTL a retry sees InProgress; after it, the stale
/// claim is reclaimed and the payment completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crashed_in_flight_claim_is_reclaimable_after_the_ttl() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let facilitator = Arc::new(CrashSimFacilitator {
        healed: std::sync::atomic::AtomicBool::new(false),
    });
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            facilitator.clone(),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            dir.path().join("engine.json"),
        )
        .unwrap()
        .with_in_flight_ttl_ns(1000),
    );

    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            CAPABILITY,
            requirements("2500"),
            NOW,
            TTL,
        )
        .unwrap();
    let payload = payload_for(&quote, "payer-1");

    // Attempt 1: claims the quote, then hangs in verify. Abort it once the
    // claim (in_flight=true) is durably persisted — that is the "crash".
    let e1 = engine.clone();
    let q1 = quote.clone();
    let p1 = payload.clone();
    let handle = tokio::spawn(async move {
        e1.accept_payment(&q1, &p1, VerificationTier::Observed, NOW + 1)
            .await
    });
    loop {
        if engine.status(&quote.quote_id).await.unwrap().is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    handle.abort();
    let _ = handle.await;

    // Before the TTL: the retry just sees an in-flight attempt.
    let too_soon = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(too_soon, PaymentDecision::InProgress),
        "before the TTL a retry must see InProgress, got {too_soon:?}"
    );

    // Heal and retry past the TTL: the stale claim is reclaimed and served.
    facilitator
        .healed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let recovered = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 5000)
        .await
        .unwrap();
    assert!(
        matches!(recovered, PaymentDecision::Served { .. }),
        "a reclaimed quote must be able to complete, got {recovered:?}"
    );

    let status = engine.status(&quote.quote_id).await.unwrap().unwrap();
    assert!(status.served);
    assert!(status.billing_event_id.is_some());
}

// ---------------------------------------------------------------------
// billing durability: a lost log append is recovered on retry (M4)
// ---------------------------------------------------------------------

/// M4 regression: the billing event is committed to engine state at
/// completion, but the log append happens after the lock. If that append
/// fails, the record shows served-and-billed while the log is empty — and
/// before the fix the idempotent retry returned Served without ever
/// re-appending, so the charge was invisible to accounting forever. The
/// retry must re-publish; the log dedups, so the charge lands exactly once.
#[tokio::test]
async fn a_lost_billing_append_is_recovered_on_retry() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("billing.jsonl");
    // Force the first append to fail: a directory sits where the log file
    // must be written, so OpenOptions can't open it for writing.
    std::fs::create_dir(&log_path).unwrap();

    let billing_log = Arc::new(BillingLog::new(&log_path));
    let engine = PaymentEngine::new(
        provider.clone(),
        Arc::new(net_payments::facilitator::mock::MockFacilitator::new()),
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .unwrap()
    .with_billing_log(billing_log.clone());

    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            CAPABILITY,
            requirements("2500"),
            NOW,
            TTL,
        )
        .unwrap();
    let payload = payload_for(&quote, "payer-1");

    // First serve: billed in state, but the log append fails, so the call
    // surfaces an error and nothing lands in the log.
    let first = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await;
    assert!(
        first.is_err(),
        "a broken billing stream must not report success: {first:?}"
    );

    // Heal the stream and retry: the AlreadyServed path re-publishes. (The
    // directory must go before reading the log — a dir at the path is an
    // I/O error, not an empty log.)
    std::fs::remove_dir(&log_path).unwrap();
    assert!(billing_log.read_all().await.unwrap().is_empty());
    let recovered = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(recovered, PaymentDecision::Served { .. }),
        "got {recovered:?}"
    );

    // The charge is now visible exactly once, and a further retry adds
    // nothing (published flag is set).
    assert_eq!(billing_log.read_all().await.unwrap().len(), 1);
    let again = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 3)
        .await
        .unwrap();
    assert!(matches!(again, PaymentDecision::Served { .. }));
    assert_eq!(billing_log.read_all().await.unwrap().len(), 1);
}

// ---------------------------------------------------------------------
// provider policy at quote issuance
// ---------------------------------------------------------------------

struct DenyEveryone;
impl ProviderAdmissionPolicy for DenyEveryone {
    fn admit(&self, _caller: &EntityId, _capability: &str) -> Result<(), String> {
        Err("caller not allowlisted".into())
    }
}

#[tokio::test]
async fn a_denied_caller_is_never_quoted() {
    let h = harness_with_policy(Arc::new(DenyEveryone));
    let err = h
        .engine
        .issue_quote(
            h.caller.entity_id().clone(),
            CAPABILITY,
            requirements("2500"),
            NOW,
            TTL,
        )
        .unwrap_err();
    assert!(err.to_string().contains("admission denied"));
}

#[tokio::test]
async fn an_unregistered_asset_is_never_quoted() {
    let h = harness();
    let bad = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "not-in-registry".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .unwrap();
    let err = h
        .engine
        .issue_quote(h.caller.entity_id().clone(), CAPABILITY, bad, NOW, TTL)
        .unwrap_err();
    assert!(err.to_string().contains("not in registry"), "got: {err}");
}

// ---------------------------------------------------------------------
// second payload against a paid quote
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// invocation redemption: the provider-side gate's check
// ---------------------------------------------------------------------

#[tokio::test]
async fn redemption_admits_a_paid_quote_exactly_once() {
    let h = harness();
    let quote = h.quote("2500");

    // Unpaid: nothing to redeem.
    let unpaid = h
        .engine
        .redeem_for_invocation("fixture-tool", &quote.quote_id, None)
        .await
        .unwrap();
    assert!(
        matches!(unpaid, RedeemDecision::Denied { .. }),
        "got {unpaid:?}"
    );

    // Pay + serve.
    let payload = payload_for(&quote, "payer-1");
    let served = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(matches!(served, PaymentDecision::Served { .. }));

    // A quote pays for ITS capability's tool, nothing else.
    let wrong_tool = h
        .engine
        .redeem_for_invocation("other-tool", &quote.quote_id, None)
        .await
        .unwrap();
    match wrong_tool {
        RedeemDecision::Denied { reason } => assert!(reason.contains("bound"), "{reason}"),
        other => panic!("expected Denied, got {other:?}"),
    }

    // The one paid invocation.
    assert_eq!(
        h.engine
            .redeem_for_invocation("fixture-tool", &quote.quote_id, None)
            .await
            .unwrap(),
        RedeemDecision::Admitted
    );

    // One payment, one serve — the second redemption bounces.
    let again = h
        .engine
        .redeem_for_invocation("fixture-tool", &quote.quote_id, None)
        .await
        .unwrap();
    match again {
        RedeemDecision::Denied { reason } => {
            assert!(reason.contains("already redeemed"), "{reason}")
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[tokio::test]
async fn a_signed_binding_must_verify_against_the_paying_identity() {
    let h = harness();
    let quote = h.quote("2500");
    let payload = payload_for(&quote, "payer-1");
    let served = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(matches!(served, PaymentDecision::Served { .. }));

    let transcript = invocation_binding_transcript(&quote.quote_id, "fixture-tool");

    // A present-but-wrong binding never falls back to bearer.
    let impostor = EntityKeypair::generate();
    let forged = impostor.try_sign(&transcript).unwrap().to_bytes();
    let denied = h
        .engine
        .redeem_for_invocation("fixture-tool", &quote.quote_id, Some(&forged))
        .await
        .unwrap();
    match denied {
        RedeemDecision::Denied { reason } => {
            assert!(reason.contains("does not verify"), "{reason}")
        }
        other => panic!("expected Denied, got {other:?}"),
    }

    // Garbage-length bindings are rejected outright.
    let denied = h
        .engine
        .redeem_for_invocation("fixture-tool", &quote.quote_id, Some(&[1, 2, 3]))
        .await
        .unwrap();
    assert!(matches!(denied, RedeemDecision::Denied { .. }));

    // A binding signed over the WRONG tool doesn't transfer.
    let wrong_tool_transcript = invocation_binding_transcript(&quote.quote_id, "other-tool");
    let misdirected = h
        .caller
        .try_sign(&wrong_tool_transcript)
        .unwrap()
        .to_bytes();
    let denied = h
        .engine
        .redeem_for_invocation("fixture-tool", &quote.quote_id, Some(&misdirected))
        .await
        .unwrap();
    assert!(matches!(denied, RedeemDecision::Denied { .. }));

    // The paying identity's signature over the right transcript admits —
    // and the failed attempts above consumed nothing.
    let good = h.caller.try_sign(&transcript).unwrap().to_bytes();
    assert_eq!(
        h.engine
            .redeem_for_invocation("fixture-tool", &quote.quote_id, Some(&good))
            .await
            .unwrap(),
        RedeemDecision::Admitted
    );
}

#[tokio::test]
async fn redemption_denies_frozen_quotes() {
    let h = harness();
    let quote = h.quote("2500");
    h.facilitator
        .arm(quote.requirements.content_hash(), MockMode::ReorgInvalidate);
    let payload = payload_for(&quote, "payer-1");

    let served = h
        .engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(matches!(served, PaymentDecision::Served { .. }));

    // The settlement reorgs out before the invocation arrives: the quote
    // freezes, and the (paid! billed!) invocation is still refused —
    // billing stays immutable, serving stops.
    let reorg = h
        .engine
        .re_verify(&quote.quote_id, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(matches!(reorg, PaymentDecision::Invalidated { .. }));

    let redemption = h
        .engine
        .redeem_for_invocation("fixture-tool", &quote.quote_id, None)
        .await
        .unwrap();
    match redemption {
        RedeemDecision::Denied { reason } => assert!(reason.contains("frozen"), "{reason}"),
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[tokio::test]
async fn a_second_payload_for_a_satisfied_quote_is_rejected() {
    let h = harness();
    let quote = h.quote("2500");
    let first = payload_for(&quote, "payer-1");
    let second = payload_for(&quote, "payer-2");

    let served = h
        .engine
        .accept_payment(&quote, &first, VerificationTier::Observed, NOW + 1)
        .await
        .unwrap();
    assert!(matches!(served, PaymentDecision::Served { .. }));

    let dup = h
        .engine
        .accept_payment(&quote, &second, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(
        matches!(
            dup,
            PaymentDecision::Rejected {
                reason: RejectReason::QuoteAlreadyPaid
            }
        ),
        "got {dup:?}"
    );
}
