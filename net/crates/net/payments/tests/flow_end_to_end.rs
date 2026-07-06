//! Workstream 4 (demand side), end to end in one process: announced
//! terms → provider-signed quote → caller spend policy → x402 payload →
//! provider engine (verify + settle via the mock facilitator) → billing.
//!
//! This is the P0 lifecycle the recorded demo performs across two
//! machines, exercised over the same interfaces ([`ProviderChannel`]
//! carries the machine boundary; the in-process implementation *is* the
//! provider side of the mesh service).

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::billing_event::BillingEvent;
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::engine::{AdmitAll, PaymentEngine, ProviderAdmissionPolicy};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::{
    CallerDecision, CallerPaymentFlow, Clock, InProcessProvider, ProviderChannel,
};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const CAPABILITY: &str = "fixture-provider/fixture-tool";

/// Deterministic, monotonically advancing test clock.
struct TestClock(std::sync::atomic::AtomicU64);
impl TestClock {
    fn new() -> Self {
        Self(std::sync::atomic::AtomicU64::new(1_000_000_000_000_000))
    }
}
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst)
    }
}

struct World {
    flow: CallerPaymentFlow,
    spend_path: std::path::PathBuf,
    terms_json: String,
    provider_log: Arc<BillingLog>,
    _dir: tempfile::TempDir,
}

fn world(profile: SpendProfile) -> World {
    world_with(profile, Arc::new(AdmitAll))
}

fn world_with(profile: SpendProfile, admission: Arc<dyn ProviderAdmissionPolicy>) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());

    // Provider side: engine + mock facilitator + billing stream.
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry: AssetRegistry = default_mock_registry(provider_keys.entity_id().clone());
    let provider_log = Arc::new(BillingLog::new(dir.path().join("provider-billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(MockFacilitator::new()),
            admission,
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(provider_log.clone()),
    );
    let channel = Arc::new(InProcessProvider::new(engine, clock.clone()));

    // The announced pricing terms (what publish attaches to the tool).
    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author template");
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().expect("registry ref"),
    );
    let terms_json = String::from_utf8(
        net_payments::core::canonical::canonical_bytes(&terms).expect("terms canonicalize"),
    )
    .expect("utf8");

    // Caller side: identity + spend policy + the flow.
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
        flow,
        spend_path,
        terms_json,
        provider_log,
        _dir: dir,
    }
}

#[tokio::test]
async fn auto_allow_pays_silently_and_bills_exactly_once() {
    let w = world(SpendProfile::DevTest);

    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Paid {
        quote_id: _,
        binding_sig: _,
        proof,
    } = decision
    else {
        panic!("expected Paid, got {decision:?}");
    };

    // The proof carries the signed billing event — the caller can verify
    // and persist its own copy ("billing events persisted both sides").
    let billing_json = proof["billing_event"]
        .as_str()
        .expect("billing event in proof");
    let billing = BillingEvent::from_json_bytes(billing_json.as_bytes()).expect("verifies");
    assert_eq!(billing.amount, AtomicAmount::from_u128(2500));
    assert_eq!(billing.capability, CAPABILITY);

    // Provider-side stream recorded the same single event.
    let recorded = w.provider_log.read_all().await.expect("read log");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].billing_event_id, billing.billing_event_id);
    recorded[0].verify_signature().expect("signed");

    // Retry of the whole flow: a fresh quote is a fresh spend — but the
    // SAME quote retried at the channel level never double-bills (pinned
    // by the engine's lifecycle tests). Here we pin the flow-level fact:
    // a second full run is a second charge with a distinct billing id.
    let second = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Paid {
        quote_id: _,
        binding_sig: _,
        proof: proof2,
    } = second
    else {
        panic!("expected Paid, got {second:?}");
    };
    assert_ne!(
        proof["quote_id"], proof2["quote_id"],
        "distinct runs get distinct quotes"
    );
    assert_eq!(w.provider_log.read_all().await.unwrap().len(), 2);
}

#[tokio::test]
async fn over_cap_surfaces_structured_approval_and_approval_unblocks() {
    let w = world(SpendProfile::DevTest);
    // Cap below the price: the first run must hold for approval.
    let configurer = SpendPolicyEngine::new(&w.spend_path, SpendProfile::DevTest);
    configurer
        .configure(|defaults, _| {
            defaults.max_per_call = Some(AtomicAmount::from_u128(1000));
        })
        .await
        .expect("configure");

    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::RequiresPaymentApproval {
        quote_id,
        policy_reason,
        approve_hint,
    } = decision
    else {
        panic!("expected RequiresPaymentApproval, got {decision:?}");
    };
    assert!(policy_reason.contains("max_per_call"), "{policy_reason}");
    assert!(approve_hint.contains(&quote_id));
    assert!(
        w.provider_log.read_all().await.unwrap().is_empty(),
        "nothing billed while approval is pending"
    );

    // A human approves through the SDK consent surface. The retry redeems
    // the exact held quote the human saw (the store carries its bytes) —
    // approval of quote X never authorizes some later quote Y.
    let approver = SpendPolicyEngine::new(&w.spend_path, SpendProfile::DevTest);
    assert!(approver.approve(&quote_id).await.expect("approve"));

    let retry = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Paid {
        quote_id: _,
        binding_sig: _,
        proof,
    } = retry
    else {
        panic!("approval must unblock the paid invoke, got {retry:?}");
    };
    assert_eq!(
        proof["quote_id"].as_str(),
        Some(quote_id.as_str()),
        "the redeemed quote is the approved one"
    );
    assert_eq!(
        w.provider_log.read_all().await.unwrap().len(),
        1,
        "exactly one charge"
    );

    // The approval was consumed: a third run holds again on a new quote.
    let third = w.flow.run(CAPABILITY, &w.terms_json).await;
    assert!(
        matches!(third, CallerDecision::RequiresPaymentApproval { .. }),
        "consumed approvals never silently authorize the next spend, got {third:?}"
    );
}

#[tokio::test]
async fn production_profile_holds_every_mock_spend_for_approval() {
    let w = world(SpendProfile::Production);
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::RequiresPaymentApproval { policy_reason, .. } = decision else {
        panic!("expected RequiresPaymentApproval, got {decision:?}");
    };
    assert!(
        policy_reason.contains("dev/test profile"),
        "{policy_reason}"
    );
}

#[tokio::test]
async fn a_denied_caller_is_refused_at_quote_time_and_nothing_reserves() {
    struct DenyEveryone;
    impl ProviderAdmissionPolicy for DenyEveryone {
        fn admit(
            &self,
            _caller: &net::adapter::net::identity::EntityId,
            _capability: &str,
        ) -> Result<(), String> {
            Err("caller not allowlisted".into())
        }
    }
    let w = world_with(SpendProfile::DevTest, Arc::new(DenyEveryone));
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Failed { message, .. } = decision else {
        panic!("expected Failed at quote issuance, got {decision:?}");
    };
    assert!(message.contains("admission denied"), "{message}");
    // Never quoted → nothing reserved, nothing billed.
    let spend = SpendPolicyEngine::new(&w.spend_path, SpendProfile::DevTest);
    assert_eq!(
        spend
            .spent_today(MOCK_NETWORK, "musd", 1_000_000_000_000_000)
            .await
            .unwrap(),
        AtomicAmount::from_u128(0)
    );
    assert!(w.provider_log.read_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn real_network_only_terms_are_denied_at_both_layers() {
    // Hand-craft terms whose only accepts entry is a real network.
    let provider = EntityKeypair::generate();
    let registry = default_mock_registry(provider.entity_id().clone());
    let real = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: "eip155:8453".into(),
        amount: "10000".into(),
        asset: "0xusdc".into(),
        pay_to: "0xpayee".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author");
    let terms = PricingTerms::new(
        provider.entity_id().clone(),
        CAPABILITY,
        vec![real],
        registry.reference().expect("ref"),
    );
    let terms_json = String::from_utf8(
        net_payments::core::canonical::canonical_bytes(&terms).expect("canonicalize"),
    )
    .expect("utf8");

    // Layer 1: without a settlement signer, the entry isn't settleable.
    let w = world(SpendProfile::DevTest);
    let decision = w.flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied, got {decision:?}");
    };
    assert!(policy_reason.contains("no settleable"), "{policy_reason}");

    // Layer 2: with a signer configured AND registries that know the
    // asset, selection and quoting pass — but the spend policy's
    // real-network gate still denies (config-driven allow is P1 WS4),
    // and the signer callback must never even run.
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let provider_keys = Arc::new(EntityKeypair::generate());
    let mut registry = default_mock_registry(provider_keys.entity_id().clone());
    registry
        .assets
        .push(net_payments::core::registry::AssetEntry {
            id: net_payments::x402::caip::AssetId::parse("eip155:8453/erc20:0xusdc").expect("caip"),
            x402_asset: "0xusdc".into(),
            decimals: 6,
            symbol: "USDC".into(),
            display_name: None,
            equivalence_class: None,
        });
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
    let signed_terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![X402Carry::author(&PaymentRequirements {
            scheme: "exact".into(),
            network: "eip155:8453".into(),
            amount: "10000".into(),
            asset: "0xusdc".into(),
            pay_to: "0xpayee".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .expect("author")],
        registry.reference().expect("ref"),
    );
    let signed_terms_json = String::from_utf8(
        net_payments::core::canonical::canonical_bytes(&signed_terms).expect("canonicalize"),
    )
    .expect("utf8");
    let never_signs = net_payments::flow::signer::ExternalSigner::new(
        "0x857b06519E91e3A54538791bDbb0E22373e36b66",
        |_typed| Box::pin(async { panic!("policy must deny before any signature is requested") }),
    );
    let flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest),
        registry,
        Arc::new(InProcessProvider::new(engine, clock.clone())),
        clock,
    )
    .with_signer("eip155", Arc::new(never_signs));

    let decision = flow.run(CAPABILITY, &signed_terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied, got {decision:?}");
    };
    assert!(policy_reason.contains("real network"), "{policy_reason}");
}

/// A channel that tampers with the quote: the provider quotes a higher
/// price than it announced. The flow must refuse before policy runs.
struct GreedyProvider {
    inner: Arc<dyn ProviderChannel>,
    engine: Arc<PaymentEngine>,
    clock: Arc<dyn Clock>,
}

#[async_trait::async_trait]
impl ProviderChannel for GreedyProvider {
    async fn quote(
        &self,
        caller: &net::adapter::net::identity::EntityId,
        capability: &str,
        _template: &X402Carry<PaymentRequirements>,
    ) -> Result<Vec<u8>, net_payments::flow::ChannelError> {
        // Quote 10x the announced price (validly signed!).
        let inflated = X402Carry::author(&PaymentRequirements {
            scheme: MOCK_SCHEME.into(),
            network: MOCK_NETWORK.into(),
            amount: "25000".into(),
            asset: "musd".into(),
            pay_to: "mock-provider-settle-addr".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .map_err(|e| net_payments::flow::ChannelError {
            message: e.to_string(),
            retryable: false,
        })?;
        let quote = self
            .engine
            .issue_quote(
                caller.clone(),
                capability,
                inflated,
                self.clock.now_ns(),
                60_000_000_000,
            )
            .map_err(|e| net_payments::flow::ChannelError {
                message: e.to_string(),
                retryable: false,
            })?;
        net_payments::core::canonical::canonical_bytes(&quote).map_err(|e| {
            net_payments::flow::ChannelError {
                message: e.to_string(),
                retryable: false,
            }
        })
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<net_payments::x402::payload::PaymentPayload>,
    ) -> Result<net_payments::flow::PayResponse, net_payments::flow::ChannelError> {
        self.inner.pay(quote_bytes, payload).await
    }
}

#[tokio::test]
async fn a_quote_that_deviates_from_announced_terms_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
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
        .unwrap(),
    );
    let honest = Arc::new(InProcessProvider::new(engine.clone(), clock.clone()));
    let greedy = Arc::new(GreedyProvider {
        inner: honest,
        engine,
        clock: clock.clone(),
    });

    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .unwrap();
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().unwrap(),
    );
    let terms_json =
        String::from_utf8(net_payments::core::canonical::canonical_bytes(&terms).unwrap()).unwrap();

    let caller = Arc::new(EntityKeypair::generate());
    let flow = CallerPaymentFlow::new(
        caller,
        SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest),
        registry,
        greedy,
        clock,
    );
    let decision = flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied, got {decision:?}");
    };
    assert!(
        policy_reason.contains("deviates from the announced terms"),
        "{policy_reason}"
    );
}

/// LOW (caller-side provenance): terms announced under one provider but
/// answered by a quote from a different provider must be refused — the
/// quote's provider must match the announced terms provider.
#[tokio::test]
async fn a_quote_whose_provider_differs_from_the_announced_terms_is_denied() {
    // Announce settleable mock terms, but under a stranger's identity —
    // not the world engine's provider that will actually issue the quote.
    let stranger = EntityKeypair::generate();
    let registry = default_mock_registry(stranger.entity_id().clone());
    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author template");
    let terms = PricingTerms::new(
        stranger.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().expect("registry ref"),
    );
    let terms_json = String::from_utf8(
        net_payments::core::canonical::canonical_bytes(&terms).expect("terms canonicalize"),
    )
    .expect("utf8");

    let w = world(SpendProfile::DevTest);
    let decision = w.flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied on a provider mismatch, got {decision:?}");
    };
    assert!(
        policy_reason.contains("provider"),
        "denial should name the provider mismatch: {policy_reason}"
    );
}
