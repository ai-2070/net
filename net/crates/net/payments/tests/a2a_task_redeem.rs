//! The engine half of paid A2A admission
//! (`docs/internal/plans/A2A_PAID_ADMISSION_PLAN.md` §D3): a quote that
//! commits to one **purchase** of one unit of work, and a redemption gate
//! that admits exactly that purchase.
//!
//! Every test here runs against the real `PaymentEngine` over the mock
//! facilitator — the same lifecycle the tool path uses, so a regression in
//! the shared precondition sequence shows up here too.
//!
//! The expected input hash is treated as what it is at this layer: an
//! opaque 64-hex string minted by the provider's admission journal. The
//! SDK's `purchase_hash` is what produces it in production; the engine
//! never parses or recomputes it, and these tests deliberately do not
//! depend on that helper.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{
    invocation_binding_transcript, AdmitAll, PaymentDecision, PaymentEngine, RedeemDecision,
    RedeemDenialReason,
};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::{CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendDecision, SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const TTL: u64 = 60_000_000_000;
/// `"{node_id}/net.a2a.task/{service_id}"` — the capability shape the
/// configured A2A path quotes under. The engine's tool binding compares
/// the tail after the FIRST `/`, so the tool id keeps its own slash.
const CAPABILITY: &str = "7/net.a2a.task/summarize";
const TOOL_ID: &str = "net.a2a.task/summarize";

/// Two distinct purchase hashes. Opaque to the engine; in production each
/// is `purchase_hash(admission_id, commitment)` for a different
/// provider-minted reservation.
const PURCHASE_A: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const PURCHASE_B: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

struct Provider {
    engine: Arc<PaymentEngine>,
    billing: Arc<BillingLog>,
    caller: EntityKeypair,
    _dir: tempfile::TempDir,
}

fn provider(require_binding: bool) -> Provider {
    let keys = Arc::new(EntityKeypair::generate());
    let dir = tempfile::tempdir().expect("tempdir");
    let billing = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            keys.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(keys.entity_id().clone()),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_require_invocation_binding(require_binding)
        .with_billing_log(billing.clone()),
    );
    Provider {
        engine,
        billing,
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
    .expect("author requirements")
}

impl Provider {
    fn issue(&self, input_hash: Option<&str>, issued_ns: u64) -> PaymentQuote {
        self.engine
            .issue_quote(
                self.caller.entity_id().clone(),
                CAPABILITY,
                requirements(),
                input_hash,
                issued_ns,
                TTL,
            )
            .expect("issue quote")
    }

    /// Issue + pay a quote, leaving it settled, billed, and unredeemed.
    async fn settled(&self, input_hash: Option<&str>, issued_ns: u64) -> PaymentQuote {
        let quote = self.issue(input_hash, issued_ns);
        let payload = X402Carry::author(&PaymentPayload {
            x402_version: 2,
            resource: None,
            accepted: quote.requirements.view().clone(),
            payload: serde_json::json!({ "mock_authorization": quote.quote_id }),
            extensions: None,
        })
        .expect("author payload");
        let decision = self
            .engine
            .accept_payment(&quote, &payload, VerificationTier::Observed, issued_ns + 1)
            .await
            .expect("accept_payment");
        assert!(
            matches!(decision, PaymentDecision::Served { .. }),
            "setup must settle: {decision:?}"
        );
        quote
    }

    fn binding(&self, quote_id: &str) -> Vec<u8> {
        self.caller
            .try_sign(&invocation_binding_transcript(quote_id, TOOL_ID))
            .expect("sign binding")
            .to_bytes()
            .to_vec()
    }

    /// `(billing events published, billing event id recorded on the
    /// record)` — the pair that moves if and only if money moved.
    async fn money(&self, quote_id: &str) -> (usize, Option<String>) {
        let published = self.billing.read_all().await.expect("billing log").len();
        let recorded = self
            .engine
            .status(quote_id)
            .await
            .expect("status")
            .expect("record exists")
            .billing_event_id;
        (published, recorded)
    }
}

fn denied(decision: &RedeemDecision) -> &RedeemDenialReason {
    match decision {
        RedeemDecision::Denied { reason } => reason,
        other => panic!("expected a denial, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// The quote commits to the purchase
// ---------------------------------------------------------------------

/// `input_hash` feeds `terms_hash`, which feeds `quote_id`. That chain is
/// the whole reason no new transcript or signature scheme is needed: the
/// caller's existing binding signature over `quote_id ‖ tool_id` already
/// names *which purchase* was authorized.
#[tokio::test]
async fn a_quote_carries_the_purchase_hash_into_its_id() {
    let p = provider(true);

    // Same provider, same caller, same terms, same instant — only the
    // purchase differs.
    let unbound = p.issue(None, NOW);
    let bound_a = p.issue(Some(PURCHASE_A), NOW);
    let bound_b = p.issue(Some(PURCHASE_B), NOW);

    assert_eq!(bound_a.input_hash.as_deref(), Some(PURCHASE_A));
    let ids = [&unbound.quote_id, &bound_a.quote_id, &bound_b.quote_id];
    for (i, left) in ids.iter().enumerate() {
        for right in ids.iter().skip(i + 1) {
            assert_ne!(
                left, right,
                "two purchases (or a purchase and a plain capability call) \
                 must never share a quote id"
            );
        }
    }
    // Control: holding the purchase fixed reproduces the id exactly, so
    // the inequalities above are about the hash and not about entropy.
    assert_eq!(p.issue(Some(PURCHASE_A), NOW).quote_id, bound_a.quote_id);

    // And the commitment survives the wire: re-pointing the hash at
    // another purchase breaks the envelope rather than quietly re-binding
    // it, because `from_json_bytes` recomputes `terms_hash` and the id.
    let bytes = canonical_bytes(&bound_a).expect("canonical");
    let mut doc: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    doc["input_hash"] = serde_json::Value::String(PURCHASE_B.to_string());
    let tampered = serde_json::to_vec(&doc).expect("re-encode");
    assert!(
        PaymentQuote::from_json_bytes(&tampered).is_err(),
        "a quote whose input hash was swapped must not decode as valid"
    );
    // Control: the untampered bytes do decode.
    PaymentQuote::from_json_bytes(&bytes).expect("the original quote is valid");
}

// ---------------------------------------------------------------------
// Admission is for one purchase
// ---------------------------------------------------------------------

/// The cross-reservation replay guard. A proof minted for one reservation
/// (or for none) is presented against another owner's purchase hash: it
/// must die at the binding check, before the redeemed arm, and must leave
/// the quote exactly as it found it — still spendable by its rightful
/// purchase, with no money moved.
#[tokio::test]
async fn redeem_for_task_refuses_a_mismatched_purchase_hash() {
    let p = provider(true);
    let quote = p.settled(Some(PURCHASE_A), NOW).await;
    let binding = p.binding(&quote.quote_id);
    let before = p.money(&quote.quote_id).await;
    assert_eq!(before.0, 1, "setup published exactly one billing event");
    assert!(before.1.is_some(), "setup billed the quote");

    let refused = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_B)
        .await
        .expect("engine");
    assert_eq!(denied(&refused), &RedeemDenialReason::InputBindingMismatch);
    assert_eq!(
        refused,
        RedeemDecision::Denied {
            reason: RedeemDenialReason::InputBindingMismatch
        }
    );

    // Nothing settled, nothing billed, nothing re-billed.
    assert_eq!(
        p.money(&quote.quote_id).await,
        before,
        "a refused admission must not move billing or settlement"
    );

    // `redeemed` is still false — proven by the rightful purchase still
    // being admitted. Had the mismatch consumed the record, this would
    // answer `already_redeemed`.
    let admitted = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_A)
        .await
        .expect("engine");
    assert!(
        matches!(admitted, RedeemDecision::Admitted { .. }),
        "the rightful purchase must still be admissible, got {admitted:?}"
    );
    assert_eq!(
        p.money(&quote.quote_id).await,
        before,
        "admitting moves no new money either — the payment already settled"
    );
}

/// Idempotent per purchase, and only per purchase. A provider that
/// crashed between the engine write and its own journal write retries the
/// identical claim and must be reconciled, not charged again and not
/// refused — at-most-once *execution* is the journal's job, not this
/// gate's.
#[tokio::test]
async fn redeem_for_task_is_idempotent_for_the_same_purchase_hash() {
    let p = provider(true);
    let quote = p.settled(Some(PURCHASE_A), NOW).await;
    let binding = p.binding(&quote.quote_id);

    let first = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_A)
        .await
        .expect("engine");
    let second = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_A)
        .await
        .expect("engine");
    let third = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_A)
        .await
        .expect("engine");

    assert_eq!(first, second, "the same purchase must re-admit identically");
    assert_eq!(second, third);
    let RedeemDecision::Admitted { payer } = first else {
        panic!("the first admission must succeed, got {first:?}");
    };
    assert_eq!(
        payer,
        *p.caller.entity_id(),
        "the admission names the identity that actually paid"
    );
    assert_eq!(
        p.billing.read_all().await.expect("billing").len(),
        1,
        "three admissions of one purchase are still one payment"
    );

    // Idempotency is scoped to THIS purchase: another one is refused, not
    // waved through by the record already being redeemed.
    let other = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_B)
        .await
        .expect("engine");
    assert_eq!(denied(&other), &RedeemDenialReason::InputBindingMismatch);
}

/// The quote was consumed by a claim that is not this one — here the
/// strict tool gate, which records no purchase identity at all. The task
/// gate must call that `already_redeemed` rather than treat an unrelated
/// redemption as its own.
#[tokio::test]
async fn a_second_purchase_hash_on_a_redeemed_quote_is_already_redeemed() {
    let p = provider(true);
    let quote = p.settled(Some(PURCHASE_A), NOW).await;
    let binding = p.binding(&quote.quote_id);

    // A different claim consumes the quote first.
    let consumed = p
        .engine
        .redeem_for_invocation(TOOL_ID, &quote.quote_id, Some(&binding))
        .await
        .expect("engine");
    assert!(
        matches!(consumed, RedeemDecision::Admitted { .. }),
        "control: the invocation gate admits once, got {consumed:?}"
    );

    let refused = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &binding, PURCHASE_A)
        .await
        .expect("engine");
    assert_eq!(
        denied(&refused),
        &RedeemDenialReason::AlreadyRedeemed,
        "a redemption this purchase did not perform is not its to reuse"
    );
    assert_eq!(
        p.billing.read_all().await.expect("billing").len(),
        1,
        "the refusal moved no money"
    );
}

/// Bearer redemption is a documented compatibility mode for *tools*. It
/// must not leak onto the task path: admitting a purchase on possession
/// of a quote id alone would let anyone who saw the id claim another
/// agent's paid work.
#[tokio::test]
async fn a_bearer_task_redeem_is_binding_required_even_when_the_engine_allows_bearer_tools() {
    let p = provider(false);

    // Control: on THIS engine bearer really is enabled, so the refusals
    // below are about the task path and not about the engine's posture.
    let bearer_tool_quote = p.settled(None, NOW).await;
    let bearer = p
        .engine
        .redeem_for_invocation(TOOL_ID, &bearer_tool_quote.quote_id, None)
        .await
        .expect("engine");
    assert!(
        matches!(bearer, RedeemDecision::Admitted { .. }),
        "control: this engine admits bearer tool redemptions, got {bearer:?}"
    );

    let quote = p.settled(Some(PURCHASE_A), NOW + 10).await;

    // No possession proof at all.
    let empty = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &[], PURCHASE_A)
        .await
        .expect("engine");
    assert_eq!(denied(&empty), &RedeemDenialReason::BindingMalformed);

    // Somebody else's signature over the right transcript.
    let impostor = EntityKeypair::generate()
        .try_sign(&invocation_binding_transcript(&quote.quote_id, TOOL_ID))
        .expect("sign")
        .to_bytes()
        .to_vec();
    let forged = p
        .engine
        .redeem_for_task(TOOL_ID, &quote.quote_id, &impostor, PURCHASE_A)
        .await
        .expect("engine");
    assert_eq!(denied(&forged), &RedeemDenialReason::BindingRejected);

    // Neither refusal consumed anything: the payer's own signature still
    // admits.
    let admitted = p
        .engine
        .redeem_for_task(
            TOOL_ID,
            &quote.quote_id,
            &p.binding(&quote.quote_id),
            PURCHASE_A,
        )
        .await
        .expect("engine");
    assert!(
        matches!(admitted, RedeemDecision::Admitted { .. }),
        "the payer's own binding must still admit, got {admitted:?}"
    );
}

// ---------------------------------------------------------------------
// Caller side: an approval is for one purchase
// ---------------------------------------------------------------------

struct FixedClock(u64);
impl Clock for FixedClock {
    fn now_ns(&self) -> u64 {
        self.0
    }
}

/// A human approves the quote they were shown. Resuming that approval for
/// a *different* purchase would spend their authorization on work they
/// never saw — so the held quote is reused only when its input hash is
/// the one being resumed, and the hold survives untouched otherwise.
#[tokio::test]
async fn an_approved_quote_resumes_only_for_its_own_purchase_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(NOW));
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry: AssetRegistry = default_mock_registry(provider_keys.entity_id().clone());
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
    let channel = Arc::new(InProcessProvider::new(engine, clock.clone()));
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![requirements()],
        registry.reference().expect("registry ref"),
    );
    let terms_json = String::from_utf8(canonical_bytes(&terms).expect("canonical")).expect("utf8");

    let spend_path = dir.path().join("spend-policy.json");
    // `Production` holds every mock spend for a human decision, which is
    // the state this test is about.
    let flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        channel,
        clock,
    );
    let operator = SpendPolicyEngine::new(&spend_path, SpendProfile::Production);

    // Purchase A is quoted and parked for approval.
    let bound_a = flow
        .quote_bound(CAPABILITY, &terms_json, Some(PURCHASE_A))
        .await
        .expect("quote for purchase A");
    assert_eq!(bound_a.quote.input_hash.as_deref(), Some(PURCHASE_A));
    assert!(
        bound_a.approval_id.is_none(),
        "a first quote resumes nothing"
    );
    let SpendDecision::RequiresPaymentApproval { quote_id, .. } =
        flow.reserve_spend(&bound_a.quote).await.expect("spend")
    else {
        panic!("the production profile must hold a mock spend for approval");
    };
    assert_eq!(quote_id, bound_a.quote.quote_id);
    assert!(operator.approve(&quote_id).await.expect("approve"));

    // Resuming the SAME purchase picks the approved quote back up.
    let resumed = flow
        .quote_bound(CAPABILITY, &terms_json, Some(PURCHASE_A))
        .await
        .expect("resume purchase A");
    assert_eq!(resumed.approval_id.as_deref(), Some(quote_id.as_str()));
    assert_eq!(resumed.quote.quote_id, bound_a.quote.quote_id);
    assert_eq!(
        resumed.quote_bytes, bound_a.quote_bytes,
        "the resumed quote is the exact envelope the human approved"
    );

    // A DIFFERENT purchase does not inherit it.
    let other = flow
        .quote_bound(CAPABILITY, &terms_json, Some(PURCHASE_B))
        .await
        .expect("quote for purchase B");
    assert!(
        other.approval_id.is_none(),
        "purchase B must not ride purchase A's approval"
    );
    assert_ne!(other.quote.quote_id, bound_a.quote.quote_id);
    assert_eq!(other.quote.input_hash.as_deref(), Some(PURCHASE_B));
    assert!(
        matches!(
            flow.reserve_spend(&other.quote).await.expect("spend"),
            SpendDecision::RequiresPaymentApproval { .. }
        ),
        "purchase B needs its own human decision"
    );

    // ...and purchase A's approval is still on file, unspent.
    assert_eq!(
        operator
            .approved_quote(CAPABILITY)
            .await
            .expect("approved_quote")
            .map(|(id, _)| id)
            .as_deref(),
        Some(quote_id.as_str()),
        "quoting another purchase must not clear a live approval"
    );
}
