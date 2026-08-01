//! Workstream 3 acceptance: auto-allow is silent; over-cap returns the
//! structured `requires_payment_approval`; approval through the operator
//! verb unblocks; real networks deny with no approval path; and two
//! concurrent engines hammering `max_per_day` never overspend.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, AssetEntry, AssetRegistry, RegistryRef};
use net_payments::core::units::AtomicAmount;
use net_payments::policy::spend::{SpendDecision, SpendLimits, SpendPolicyEngine, SpendProfile};
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
    let decision = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW)
        .await
        .unwrap();
    assert_eq!(decision, SpendDecision::Allowed);
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500)
    );
    assert!(
        s.engine.pending().await.unwrap().is_empty(),
        "silent means no approval records"
    );
}

#[tokio::test]
async fn real_networks_deny_with_no_approval_path_even_with_the_unsafe_flag() {
    let provider = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    // A registry that *does* allow USDC-on-Base — the deny must come from
    // the P0 real-network line, not from registry absence.
    let mut registry = default_mock_registry(provider.entity_id().clone());
    registry.assets.push(AssetEntry {
        id: AssetId::parse("eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(),
        x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        decimals: 6,
        symbol: "USDC".into(),
        display_name: None,
        equivalence_class: None,
        eip712_name: None,
        eip712_version: None,
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

    let decision = engine
        .check_and_reserve(&quote, &registry, NOW)
        .await
        .unwrap();
    match decision {
        SpendDecision::Denied { policy_reason } => {
            assert!(
                policy_reason.contains("real network"),
                "got: {policy_reason}"
            )
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[tokio::test]
async fn production_profile_requires_approval_and_the_operator_verb_unblocks() {
    let s = setup(SpendProfile::Production);
    let quote = s.quote(mock_requirements("2500"), NOW);

    let first = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW)
        .await
        .unwrap();
    let SpendDecision::RequiresPaymentApproval {
        quote_id,
        policy_reason,
        approve_hint,
    } = first
    else {
        panic!("production mock spend must require approval");
    };
    assert_eq!(quote_id, quote.quote_id);
    assert!(policy_reason.contains("dev/test profile"));
    assert!(approve_hint.contains(&quote.quote_id));
    // The request left a pending record for consent UX; nothing reserved.
    assert_eq!(
        s.engine.pending().await.unwrap(),
        vec![quote.quote_id.clone()]
    );
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(0)
    );

    // Operator approves through the consent surface → retry allows and
    // the spend lands in the counter (approved spend is still spending).
    assert!(s.engine.approve(&quote.quote_id).await.unwrap());
    let retry = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW + 1)
        .await
        .unwrap();
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
        id: AssetId::parse("eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(),
        x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        decimals: 6,
        symbol: "USDC".into(),
        display_name: None,
        equivalence_class: None,
        eip712_name: None,
        eip712_version: None,
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
        engine
            .check_and_reserve(&under, &registry, NOW)
            .await
            .unwrap(),
        SpendDecision::Allowed
    );

    // Over the cap: the structured approval hold, same as mock.
    let over = quote(requirements("100000"), NOW + 1);
    assert!(matches!(
        engine
            .check_and_reserve(&over, &registry, NOW)
            .await
            .unwrap(),
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
        eip712_name: None,
        eip712_version: None,
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
    let decision = engine
        .check_and_reserve(&polygon, &registry_with_polygon, NOW)
        .await
        .unwrap();
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
    let decision = engine
        .check_and_reserve(&quote, &registry, NOW)
        .await
        .unwrap();
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
    let decision = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW)
        .await
        .unwrap();
    let SpendDecision::RequiresPaymentApproval { policy_reason, .. } = decision else {
        panic!("expected RequiresPaymentApproval");
    };
    assert!(
        policy_reason.contains("max_per_call"),
        "got: {policy_reason}"
    );

    // Approval overrides the cap for exactly this quote.
    s.engine.approve(&quote.quote_id).await.unwrap();
    let retry = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW + 1)
        .await
        .unwrap();
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
            s.engine
                .check_and_reserve(&quote, &s.registry, NOW)
                .await
                .unwrap(),
            SpendDecision::Allowed,
            "spend {i} fits the cap"
        );
    }
    let third = s.quote(mock_requirements("2500"), NOW + 2);
    let decision = s
        .engine
        .check_and_reserve(&third, &s.registry, NOW)
        .await
        .unwrap();
    let SpendDecision::RequiresPaymentApproval { policy_reason, .. } = decision else {
        panic!("expected RequiresPaymentApproval on the third spend");
    };
    assert!(
        policy_reason.contains("max_per_day"),
        "got: {policy_reason}"
    );

    // Next day the counter is fresh.
    let tomorrow = NOW + NS_PER_DAY;
    let fourth = s.quote(mock_requirements("2500"), tomorrow);
    assert_eq!(
        s.engine
            .check_and_reserve(&fourth, &s.registry, tomorrow)
            .await
            .unwrap(),
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
        s.engine
            .check_and_reserve(&quote, &s.registry, NOW)
            .await
            .unwrap(),
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
        s.engine
            .check_and_reserve(&other, &s.registry, NOW)
            .await
            .unwrap(),
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
    let decision = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW)
        .await
        .unwrap();
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
                match engine
                    .check_and_reserve(&quote, &registry, NOW)
                    .await
                    .unwrap()
                {
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

/// L6 regression: `approve` grants an existing held quote and refuses to
/// invent one.
///
/// The engine writes the pending record — with the exact provider-signed
/// quote bytes attached — when it decides an approval is needed, so an id
/// with no record is one nothing ever asked about. Minting a record here
/// would leave an approval carrying no quote bytes that
/// `check_and_reserve` still reads as approved (bypassing `max_per_call`,
/// `max_per_day`, and `allowed_assets`) while `approved_quote` skips it.
#[tokio::test]
async fn approve_grants_a_held_quote_and_refuses_to_invent_one() {
    let s = setup(SpendProfile::Production);
    let quote = s.quote(mock_requirements("2500"), NOW);

    // An id nobody has asked about: no-op, no record, and crucially the
    // policy gate does not treat it as approved afterwards.
    assert!(
        !s.engine.approve("not-a-quote-id").await.unwrap(),
        "approving an unknown id must be a no-op"
    );
    assert!(
        s.engine.pending().await.unwrap().is_empty(),
        "approve must not mint an approval record"
    );

    // The production profile holds a mock spend for approval, which is
    // what writes the pending record in the first place.
    let held = s
        .engine
        .check_and_reserve(&quote, &s.registry, NOW)
        .await
        .unwrap();
    assert!(matches!(
        held,
        SpendDecision::RequiresPaymentApproval { .. }
    ));

    // Now the id resolves: first approval changes state, second is idempotent.
    assert!(s.engine.approve(&quote.quote_id).await.unwrap());
    assert!(
        !s.engine.approve(&quote.quote_id).await.unwrap(),
        "re-approving an already-approved quote changes nothing"
    );

    // And the approval carries the exact quote bytes a retry redeems —
    // the human's approval applies to the quote they saw, not to some
    // later one with the same id.
    let (held_id, bytes) = s
        .engine
        .approved_quote(CAPABILITY)
        .await
        .unwrap()
        .expect("an approved hold carries its quote");
    assert_eq!(held_id, quote.quote_id);
    assert_eq!(
        bytes,
        net_payments::core::canonical::canonical_bytes(&quote).unwrap(),
        "the held bytes must be the exact quote that was approved"
    );
}

// ---------------------------------------------------------------------------
// L2: reservations are owner-tracked and release is idempotent
// ---------------------------------------------------------------------------

/// A second release for the same quote is a no-op, and cannot free
/// budget belonging to anything else.
///
/// The counters are aggregate (`day|network|asset`), so before the
/// reservation record existed, release had to be told the amount and the
/// day again and could not tell a first release from a second. Two
/// releases subtracted twice; and because the underflow saturated to
/// zero, one over-large release wiped the whole day's counter for that
/// pair — freeing budget for every unrelated reservation in the same
/// bucket and reopening `max_per_day` as a loss bound.
#[tokio::test]
async fn releasing_twice_does_not_refund_twice() {
    let s = setup(SpendProfile::DevTest);
    let first = s.quote(mock_requirements("2500"), NOW);
    let second = s.quote(mock_requirements("1000"), NOW + 1);

    assert_eq!(
        s.engine
            .check_and_reserve(&first, &s.registry, NOW)
            .await
            .unwrap(),
        SpendDecision::Allowed
    );
    assert_eq!(
        s.engine
            .check_and_reserve(&second, &s.registry, NOW)
            .await
            .unwrap(),
        SpendDecision::Allowed
    );
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(3500)
    );

    // Release the first: its own amount comes off, nothing else moves.
    s.engine.release_reservation(&first, NOW).await.unwrap();
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(1000),
        "only the released quote's amount comes off"
    );

    // Release it again, and again: idempotent. The second quote's
    // reservation is untouched.
    s.engine.release_reservation(&first, NOW).await.unwrap();
    s.engine.release_reservation(&first, NOW).await.unwrap();
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(1000),
        "a repeat release must not refund again"
    );

    // And the surviving reservation is still on file, so it can still be
    // released exactly once.
    assert!(s
        .engine
        .reservation(&second.quote_id)
        .await
        .unwrap()
        .is_some());
    assert!(s
        .engine
        .reservation(&first.quote_id)
        .await
        .unwrap()
        .is_none());
    s.engine.release_reservation(&second, NOW).await.unwrap();
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(0)
    );
}

/// Release finds the counter the reservation actually landed in, not the
/// one the caller's clock points at now.
///
/// A payment reserved just before UTC midnight and released just after
/// used to decrement the *new* day's counter — freeing budget on a day
/// nothing was spent. The reservation record carries its own day, so the
/// caller's clock is no longer part of the correctness argument.
#[tokio::test]
async fn release_uses_the_reservations_own_day_not_the_callers_clock() {
    let s = setup(SpendProfile::DevTest);
    let quote = s.quote(mock_requirements("2500"), NOW);
    s.engine
        .check_and_reserve(&quote, &s.registry, NOW)
        .await
        .unwrap();

    // Something else spends on the following day.
    let tomorrow = NOW + NS_PER_DAY;
    let other = s.quote(mock_requirements("1000"), tomorrow);
    s.engine
        .check_and_reserve(&other, &s.registry, tomorrow)
        .await
        .unwrap();
    assert_eq!(
        s.engine
            .spent_today("mock:net", "musd", tomorrow)
            .await
            .unwrap(),
        AtomicAmount::from_u128(1000)
    );

    // Release the first quote with a clock that has rolled over.
    s.engine
        .release_reservation(&quote, tomorrow)
        .await
        .unwrap();

    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(0),
        "the release must land on the day the reservation was taken"
    );
    assert_eq!(
        s.engine
            .spent_today("mock:net", "musd", tomorrow)
            .await
            .unwrap(),
        AtomicAmount::from_u128(1000),
        "and must not touch another day's budget"
    );
}

/// Reserving the same quote twice is a retry, not a second spend.
#[tokio::test]
async fn reserving_the_same_quote_twice_counts_once() {
    let s = setup(SpendProfile::DevTest);
    let quote = s.quote(mock_requirements("2500"), NOW);

    for _ in 0..3 {
        assert_eq!(
            s.engine
                .check_and_reserve(&quote, &s.registry, NOW)
                .await
                .unwrap(),
            SpendDecision::Allowed
        );
    }
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500),
        "a retried reservation must not accumulate"
    );
}

/// A retry of a payment that reached `max_per_day` is still allowed.
///
/// The ownership check has to run before the cap. Evaluating
/// `max_per_day` first would compare the cap against a total that already
/// includes this very reservation, so a retry of the payment that
/// happened to reach the cap would be told it needs approval — for
/// spending it had already been allowed.
#[tokio::test]
async fn a_retry_at_the_daily_cap_is_still_allowed() {
    let s = setup(SpendProfile::DevTest);
    s.engine
        .configure(|defaults, _| {
            defaults.max_per_day = Some(AtomicAmount::from_u128(2500));
        })
        .await
        .unwrap();

    let quote = s.quote(mock_requirements("2500"), NOW);

    // Reserves exactly up to the cap.
    assert_eq!(
        s.engine
            .check_and_reserve(&quote, &s.registry, NOW)
            .await
            .unwrap(),
        SpendDecision::Allowed
    );
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500)
    );

    // The retry must not be told to seek approval for spending it has
    // already been granted.
    assert_eq!(
        s.engine
            .check_and_reserve(&quote, &s.registry, NOW)
            .await
            .unwrap(),
        SpendDecision::Allowed,
        "a retry at the cap must not require approval"
    );
    assert_eq!(
        s.engine.spent_today("mock:net", "musd", NOW).await.unwrap(),
        AtomicAmount::from_u128(2500),
        "and must not count twice"
    );

    // A genuinely new quote at the cap still holds, so the cap works.
    let another = s.quote(mock_requirements("1"), NOW + 1);
    assert!(matches!(
        s.engine
            .check_and_reserve(&another, &s.registry, NOW)
            .await
            .unwrap(),
        SpendDecision::RequiresPaymentApproval { .. }
    ));
}

/// Reservations do not accumulate forever.
///
/// A successful payment never releases — it has nothing to give back — so
/// without pruning the map would grow with lifetime payment volume, on a
/// file every payment parses and rewrites under a lock. Once a
/// reservation's counter has aged out it can no longer be released
/// meaningfully, so it is dead weight.
#[tokio::test]
async fn reservations_are_pruned_with_their_counters() {
    let s = setup(SpendProfile::DevTest);
    let old = s.quote(mock_requirements("2500"), NOW);
    s.engine
        .check_and_reserve(&old, &s.registry, NOW)
        .await
        .unwrap();
    assert!(s.engine.reservation(&old.quote_id).await.unwrap().is_some());

    // Well past the counter-retention horizon, a later reservation sweeps
    // the stale one as it goes.
    let much_later = NOW + NS_PER_DAY * 5;
    let fresh = s.quote(mock_requirements("100"), much_later);
    s.engine
        .check_and_reserve(&fresh, &s.registry, much_later)
        .await
        .unwrap();

    assert!(
        s.engine.reservation(&old.quote_id).await.unwrap().is_none(),
        "a reservation whose counter aged out must be swept"
    );
    assert!(
        s.engine
            .reservation(&fresh.quote_id)
            .await
            .unwrap()
            .is_some(),
        "the live reservation survives"
    );
}
