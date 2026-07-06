//! Workstream 3 acceptance: auto-allow is silent; over-cap returns the
//! structured `requires_payment_approval`; approval through the operator
//! verb unblocks; real networks deny with no approval path; and two
//! concurrent engines hammering `max_per_day` never overspend.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{
    default_mock_registry, AssetEntry, AssetRegistry, RegistryRef,
};
use net_payments::core::units::AtomicAmount;
use net_payments::policy::spend::{
    SpendDecision, SpendLimits, SpendPolicyEngine, SpendProfile,
};
use net_payments::x402::caip::AssetId;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const NS_PER_DAY: u64 = 86_400_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

struct Setup {
    engine: SpendPolicyEngine,
    registry: AssetRegistry,
    provider: EntityKeypair,
    caller: EntityKeypair,
    _dir: tempfile::TempDir,
}

fn setup(profile: SpendProfile) -> Setup {
    let provider = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    Setup {
        engine: SpendPolicyEngine::new(dir.path().join("policy.json"), profile),
        registry: default_mock_registry(provider.entity_id().clone()),
        provider,
        caller: EntityKeypair::generate(),
        _dir: dir,
    }
}

fn mock_requirements(amount: &str) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: "mock".into(),
        network: "mock:net".into(),
        amount: amount.into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author")
}

impl Setup {
    fn quote(&self, requirements: X402Carry<PaymentRequirements>, issued_ns: u64) -> PaymentQuote {
        let registry_ref = self.registry.reference().expect("ref");
        PaymentQuote::new(
            self.provider.entity_id().clone(),
            self.caller.entity_id().clone(),
            CAPABILITY,
            None,
            requirements,
            registry_ref,
            issued_ns,
            issued_ns + 60_000_000_000,
        )
    }
}

#[tokio::test]
async fn dev_profile_auto_allows_silently_and_counts_the_spend() {
    let s = setup(SpendProfile::DevTest);
    let quote = s.quote(mock_requirements("2500"), NOW);
    let decision = s.engine.check_and_reserve(&quote, &s.registry, NOW).await.unwrap();
    assert_eq!(decision, SpendDecision::Allowed);
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500)
    );
    assert!(s.engine.pending().await.unwrap().is_empty(), "silent means no approval records");
}

#[tokio::test]
async fn real_networks_deny_with_no_approval_path_even_with_the_unsafe_flag() {
    let provider = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    // A registry that *does* allow USDC-on-Base — the deny must come from
    // the P0 real-network line, not from registry absence.
    let mut registry = default_mock_registry(provider.entity_id().clone());
    registry.assets.push(AssetEntry {
        id: AssetId::parse("eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
            .unwrap(),
        x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        decimals: 6,
        symbol: "USDC".into(),
        display_name: None,
        equivalence_class: None,
    });
    let engine = SpendPolicyEngine::new(dir.path().join("policy.json"), SpendProfile::DevTest)
        .with_unsafe_mock_auto_allow(true);

    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: "eip155:8453".into(),
        amount: "10000".into(),
        asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .unwrap();
    let caller = EntityKeypair::generate();
    let quote = PaymentQuote::new(
        provider.entity_id().clone(),
        caller.entity_id().clone(),
        CAPABILITY,
        None,
        requirements,
        registry.reference().unwrap(),
        NOW,
        NOW + 60_000_000_000,
    );

    let decision = engine.check_and_reserve(&quote, &registry, NOW).await.unwrap();
    match decision {
        SpendDecision::Denied { policy_reason } => {
            assert!(policy_reason.contains("real network"), "got: {policy_reason}")
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[tokio::test]
async fn production_profile_requires_approval_and_the_operator_verb_unblocks() {
    let s = setup(SpendProfile::Production);
    let quote = s.quote(mock_requirements("2500"), NOW);

    let first = s.engine.check_and_reserve(&quote, &s.registry, NOW).await.unwrap();
    let SpendDecision::RequiresPaymentApproval { quote_id, policy_reason, approve_hint } = first
    else {
        panic!("production mock spend must require approval");
    };
    assert_eq!(quote_id, quote.quote_id);
    assert!(policy_reason.contains("dev/test profile"));
    assert!(approve_hint.contains(&quote.quote_id));
    // The request left a pending record for consent UX; nothing reserved.
    assert_eq!(s.engine.pending().await.unwrap(), vec![quote.quote_id.clone()]);
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(0)
    );

    // Operator approves through the consent surface → retry allows and
    // the spend lands in the counter (approved spend is still spending).
    assert!(s.engine.approve(&quote.quote_id).await.unwrap());
    let retry = s.engine.check_and_reserve(&quote, &s.registry, NOW + 1).await.unwrap();
    assert_eq!(retry, SpendDecision::Allowed);
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500)
    );
}

#[tokio::test]
async fn an_enabled_real_network_spends_under_caps_and_holds_over_them() {
    // The P1 posture: explicit allowed_networks listing enables a real
    // network; caps and approvals then work exactly as on mock.
    let provider = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = default_mock_registry(provider.entity_id().clone());
    registry.assets.push(AssetEntry {
        id: AssetId::parse("eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
            .unwrap(),
        x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        decimals: 6,
        symbol: "USDC".into(),
        display_name: None,
        equivalence_class: None,
    });
    let engine = SpendPolicyEngine::new(dir.path().join("policy.json"), SpendProfile::Production);
    engine
        .configure(|defaults, _| {
            defaults.allowed_networks = vec!["eip155:8453".to_string()];
            defaults.max_per_call = Some(AtomicAmount::from_u128(50_000));
        })
        .await
        .unwrap();

    let requirements = |amount: &str| {
        X402Carry::author(&PaymentRequirements {
            scheme: "exact".into(),
            network: "eip155:8453".into(),
            amount: amount.into(),
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .unwrap()
    };
    let caller = EntityKeypair::generate();
    let quote = |reqs, issued: u64| {
        PaymentQuote::new(
            provider.entity_id().clone(),
            caller.entity_id().clone(),
            CAPABILITY,
            None,
            reqs,
            registry.reference().unwrap(),
            issued,
            issued + 60_000_000_000,
        )
    };

    // Under the cap: allowed silently, even in production profile — the
    // explicit network listing IS the operator's production consent.
    let under = quote(requirements("10000"), NOW);
    assert_eq!(
        engine.check_and_reserve(&under, &registry, NOW).await.unwrap(),
        SpendDecision::Allowed
    );

    // Over the cap: the structured approval hold, same as mock.
    let over = quote(requirements("100000"), NOW + 1);
    assert!(matches!(
        engine.check_and_reserve(&over, &registry, NOW).await.unwrap(),
        SpendDecision::RequiresPaymentApproval { .. }
    ));

    // A different real network stays denied: enablement is per network.
    let mut registry_with_polygon = registry.clone();
    registry_with_polygon.assets.push(AssetEntry {
        id: AssetId::parse("eip155:137/erc20:0xusdcpolygon").unwrap(),
        x402_asset: "0xusdcpolygon".into(),
        decimals: 6,
        symbol: "USDC".into(),
        display_name: None,
        equivalence_class: None,
    });
    let polygon = quote(
        X402Carry::author(&PaymentRequirements {
            scheme: "exact".into(),
            network: "eip155:137".into(),
            amount: "10000".into(),
            asset: "0xusdcpolygon".into(),
            pay_to: "0xpayee".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .unwrap(),
        NOW + 2,
    );
    let decision = engine.check_and_reserve(&polygon, &registry_with_polygon, NOW).await.unwrap();
    let SpendDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied for the unlisted network, got {decision:?}");
    };
    assert!(policy_reason.contains("not enabled"), "{policy_reason}");
}

#[tokio::test]
async fn the_unsafe_flag_auto_allows_mock_in_production() {
    let provider = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let registry = default_mock_registry(provider.entity_id().clone());
    let engine = SpendPolicyEngine::new(dir.path().join("policy.json"), SpendProfile::Production)
        .with_unsafe_mock_auto_allow(true);
    let caller = EntityKeypair::generate();
    let quote = PaymentQuote::new(
        provider.entity_id().clone(),
        caller.entity_id().clone(),
        CAPABILITY,
        None,
        mock_requirements("2500"),
        registry.reference().unwrap(),
        NOW,
        NOW + 60_000_000_000,
    );
    let decision = engine.check_and_reserve(&quote, &registry, NOW).await.unwrap();
    assert_eq!(decision, SpendDecision::Allowed);
}

#[tokio::test]
async fn over_cap_per_call_returns_the_structured_error() {
    let s = setup(SpendProfile::DevTest);
    s.engine
        .configure(|defaults, _| {
            defaults.max_per_call = Some(AtomicAmount::from_u128(1000));
        })
        .await
        .unwrap();
    let quote = s.quote(mock_requirements("2500"), NOW);
    let decision = s.engine.check_and_reserve(&quote, &s.registry, NOW).await.unwrap();
    let SpendDecision::RequiresPaymentApproval { policy_reason, .. } = decision else {
        panic!("expected RequiresPaymentApproval");
    };
    assert!(policy_reason.contains("max_per_call"), "got: {policy_reason}");

    // Approval overrides the cap for exactly this quote.
    s.engine.approve(&quote.quote_id).await.unwrap();
    let retry = s.engine.check_and_reserve(&quote, &s.registry, NOW + 1).await.unwrap();
    assert_eq!(retry, SpendDecision::Allowed);
}

#[tokio::test]
async fn per_day_cap_accumulates_and_rolls_over_at_the_day_boundary() {
    let s = setup(SpendProfile::DevTest);
    s.engine
        .configure(|defaults, _| {
            defaults.max_per_day = Some(AtomicAmount::from_u128(5000));
        })
        .await
        .unwrap();

    for i in 0..2 {
        let quote = s.quote(mock_requirements("2500"), NOW + i);
        assert_eq!(
            s.engine.check_and_reserve(&quote, &s.registry, NOW).await.unwrap(),
            SpendDecision::Allowed,
            "spend {i} fits the cap"
        );
    }
    let third = s.quote(mock_requirements("2500"), NOW + 2);
    let decision = s.engine.check_and_reserve(&third, &s.registry, NOW).await.unwrap();
    let SpendDecision::RequiresPaymentApproval { policy_reason, .. } = decision else {
        panic!("expected RequiresPaymentApproval on the third spend");
    };
    assert!(policy_reason.contains("max_per_day"), "got: {policy_reason}");

    // Next day the counter is fresh.
    let tomorrow = NOW + NS_PER_DAY;
    let fourth = s.quote(mock_requirements("2500"), tomorrow);
    assert_eq!(
        s.engine.check_and_reserve(&fourth, &s.registry, tomorrow).await.unwrap(),
        SpendDecision::Allowed
    );
}

#[tokio::test]
async fn per_capability_overrides_replace_the_defaults() {
    let s = setup(SpendProfile::DevTest);
    s.engine
        .configure(|defaults, per_capability| {
            defaults.max_per_call = Some(AtomicAmount::from_u128(1));
            per_capability.insert(
                CAPABILITY.to_string(),
                SpendLimits {
                    max_per_call: Some(AtomicAmount::from_u128(1_000_000)),
                    ..SpendLimits::default()
                },
            );
        })
        .await
        .unwrap();

    // The overridden capability clears its generous cap...
    let quote = s.quote(mock_requirements("2500"), NOW);
    assert_eq!(
        s.engine.check_and_reserve(&quote, &s.registry, NOW).await.unwrap(),
        SpendDecision::Allowed
    );

    // ...while a capability without an override hits the tiny default.
    let registry_ref: RegistryRef = s.registry.reference().unwrap();
    let other = PaymentQuote::new(
        s.provider.entity_id().clone(),
        s.caller.entity_id().clone(),
        "fixture-provider/other-tool",
        None,
        mock_requirements("2500"),
        registry_ref,
        NOW + 1,
        NOW + 60_000_000_000,
    );
    assert!(matches!(
        s.engine.check_and_reserve(&other, &s.registry, NOW).await.unwrap(),
        SpendDecision::RequiresPaymentApproval { .. }
    ));
}

#[tokio::test]
async fn allowlists_gate_even_in_dev_when_configured() {
    let s = setup(SpendProfile::DevTest);
    s.engine
        .configure(|defaults, _| {
            defaults.allowed_networks = vec!["mock:other".to_string()];
        })
        .await
        .unwrap();
    let quote = s.quote(mock_requirements("2500"), NOW);
    let decision = s.engine.check_and_reserve(&quote, &s.registry, NOW).await.unwrap();
    let SpendDecision::RequiresPaymentApproval { policy_reason, .. } = decision else {
        panic!("expected RequiresPaymentApproval");
    };
    assert!(policy_reason.contains("allowed_networks"));
}

/// The acceptance loop test: two engine instances (two "processes")
/// hammer one shared policy file with concurrent spends against a
/// `max_per_day` cap. Exactly the affordable number may pass; the
/// counter never overshoots.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_processes_hammering_max_per_day_never_overspend() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let registry = Arc::new(default_mock_registry(provider.entity_id().clone()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.json");

    // Cap 10_000; each spend is 2_500 → exactly 4 can ever pass today.
    let configurer = SpendPolicyEngine::new(&path, SpendProfile::DevTest);
    configurer
        .configure(|defaults, _| {
            defaults.max_per_day = Some(AtomicAmount::from_u128(10_000));
        })
        .await
        .unwrap();

    let registry_ref = registry.reference().unwrap();
    let mut tasks = Vec::new();
    for engine_idx in 0..2u64 {
        // Each "process" gets its own engine instance over the same file.
        let path = path.clone();
        let provider = provider.clone();
        let registry = registry.clone();
        let registry_ref = registry_ref.clone();
        let caller_id = caller.entity_id().clone();
        tasks.push(tokio::spawn(async move {
            let engine = SpendPolicyEngine::new(&path, SpendProfile::DevTest);
            let mut allowed = 0u32;
            for i in 0..10u64 {
                let quote = PaymentQuote::new(
                    provider.entity_id().clone(),
                    caller_id.clone(),
                    CAPABILITY,
                    None,
                    mock_requirements("2500"),
                    registry_ref.clone(),
                    NOW + engine_idx * 1_000 + i,
                    NOW + 60_000_000_000,
                );
                match engine.check_and_reserve(&quote, &registry, NOW).await.unwrap() {
                    SpendDecision::Allowed => allowed += 1,
                    SpendDecision::RequiresPaymentApproval { .. } => {}
                    SpendDecision::Denied { policy_reason } => {
                        panic!("unexpected deny: {policy_reason}")
                    }
                }
            }
            allowed
        }));
    }

    let mut total_allowed = 0;
    for t in tasks {
        total_allowed += t.await.unwrap();
    }
    assert_eq!(total_allowed, 4, "exactly cap/amount spends may pass");

    let checker = SpendPolicyEngine::new(&path, SpendProfile::DevTest);
    assert_eq!(
        checker.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(10_000),
        "the counter never overshoots the cap"
    );
}
