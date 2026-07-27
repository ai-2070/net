//! The compiled counterpart of the docs Payments section.
//!
//! Every Rust snippet under `web/src/content/docs/payments/` appears here so
//! that CI compiles it. The section shipped ten pages of prose with zero
//! code — accurate, but nobody could ship from it, and nothing tied the
//! narrative to the crate. Change one, change the other.
//!
//! Run it: `cargo run -p net-payments --example docs_payments`. It drives the
//! full P0 lifecycle in one process against the mock facilitator: announced
//! terms → provider-signed quote → caller spend policy → x402 payload →
//! verify + settle → billing.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_payments::VerificationTier;

const CAPABILITY: &str = "docs-provider/summarize";

/// Wall-clock source. The engine and flow take time as a dependency so
/// tests can pin it; in production this is the obvious implementation.
struct SystemClock;
impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    // ---- Provider side -----------------------------------------------
    // Docs: payments/the-lifecycle.md § "Standing up a provider"

    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry: AssetRegistry = default_mock_registry(provider_keys.entity_id().clone());
    let billing = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));

    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )?
        .with_billing_log(billing.clone()),
    );

    // ---- Pricing announced at discovery ------------------------------
    // Docs: payments/what-net-payments-is.md § "Pricing rides on discovery"

    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })?;

    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference()?,
    );
    let terms_json = String::from_utf8(net_payments::core::canonical::canonical_bytes(&terms)?)?;

    // ---- Caller side --------------------------------------------------
    // Docs: payments/the-lifecycle.md § "Calling a paid capability"

    let caller_keys = Arc::new(EntityKeypair::generate());
    let spend_path = dir.path().join("spend-policy.json");
    let flow = CallerPaymentFlow::new(
        caller_keys,
        SpendPolicyEngine::new(&spend_path, SpendProfile::DevTest),
        registry,
        Arc::new(InProcessProvider::new(engine.clone(), clock.clone())),
        clock,
    );

    match flow.run(CAPABILITY, &terms_json).await {
        CallerDecision::Paid { quote_id, proof, .. } => {
            println!("paid; redemption binding = {quote_id}");
            let signed = proof["billing_event"].as_str().unwrap_or_default();
            let event = net_payments::BillingEvent::from_json_bytes(signed.as_bytes())?;
            println!("billed {} for {}", event.amount, event.capability);
        }
        CallerDecision::RequiresPaymentApproval {
            quote_id,
            policy_reason,
            approve_hint,
        } => {
            println!("held for approval ({policy_reason}): {quote_id}\n{approve_hint}");
        }
        CallerDecision::Denied { policy_reason } => println!("denied: {policy_reason}"),
        CallerDecision::Failed { message, retryable } => {
            println!("failed (retryable={retryable}): {message}")
        }
    }

    // ---- Billing ------------------------------------------------------
    // Docs: payments/billing.md

    for event in billing.read_all().await? {
        event.verify_signature()?;
        println!("billing event {} — {}", event.billing_event_id, event.amount);
    }

    Ok(())
}

/// Docs: payments/spend-policy-and-approvals.md § "Setting a cap"
#[allow(dead_code)]
async fn cap_per_call(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let policy = SpendPolicyEngine::new(path, SpendProfile::Production);
    policy
        .configure(|defaults, _| {
            defaults.max_per_call = Some(AtomicAmount::from_u128(1000));
        })
        .await?;
    Ok(())
}

/// Docs: payments/spend-policy-and-approvals.md § "Approving a held quote"
#[allow(dead_code)]
async fn approve(path: &std::path::Path, quote_id: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let policy = SpendPolicyEngine::new(path, SpendProfile::Production);
    Ok(policy.approve(quote_id).await?)
}

/// Docs: payments/verification-tiers.md § "Raising the bar after the fact"
#[allow(dead_code)]
async fn confirm_deeper(
    engine: &PaymentEngine,
    checker: &dyn net_payments::checker::ChainChecker,
    quote_id: &str,
    now_ns: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let decision = engine
        .re_verify_with_checker(quote_id, checker, VerificationTier::Confirmed(12), now_ns)
        .await?;
    println!("{decision:?}");
    Ok(())
}

/// Docs: payments/billing.md § "Streaming events as they happen"
#[allow(dead_code)]
async fn stream(billing: Arc<BillingLog>) {
    let mut rx = billing.subscribe();
    while let Ok(event) = rx.recv().await {
        println!("{} {} {}", event.billing_event_id, event.capability, event.amount);
    }
}
