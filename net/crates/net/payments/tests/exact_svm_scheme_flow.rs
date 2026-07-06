//! The exact-SVM seam, end to end: with the solana pack's pieces in
//! place — registry v1 (already carries SPL-USDC) + the network in
//! `allowed_networks` + an [`ExternalSvmSigner`] wallet — the full paid
//! lifecycle runs through the *unchanged* flow and engine. The wallet
//! is a scripted closure standing in for the host's real one: it
//! asserts the structured intent it was shown (the no-signing-oracle
//! property, SVM edition) and returns a fake partially-signed blob; the
//! facilitator stub asserts the spec-pinned `{"transaction": ...}`
//! payload shape crossed the boundary.
//!
//! Without a solana signer, the same terms are refused at selection —
//! settleability is capability, and its absence is a structured denial.

use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::default_registry_v1;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::core::verification::{VerificationTier, VerifierRef};
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use net_payments::flow::signer::ExternalSvmSigner;
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::settlement::{SettlementResponse, VerifyResponse};
use net_payments::x402::X402Carry;

const NOW: u64 = 1_740_672_000_000_000_000;
const CAPABILITY: &str = "42/paid-solana-tool";
const SOLANA_MAINNET: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
const SPL_USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const PAY_TO: &str = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";
const FEE_PAYER: &str = "FaciLitator111111111111111111111111111111111";
/// What the scripted wallet returns — valid base64, opaque otherwise.
const SIGNED_BLOB: &str = "cGFydGlhbGx5LXNpZ25lZC1zdmctdHJhbnNhY3Rpb24=";

struct TestClock;
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        NOW + 1_000
    }
}

/// An exact-SVM facilitator stub: asserts the pinned payload shape and
/// settles by echoing the required amount.
struct SvmFacilitator {
    payloads: parking_lot::Mutex<Vec<X402Carry<PaymentPayload>>>,
}

#[async_trait]
impl Facilitator for SvmFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef { identity: None, endpoint: "test-exact-svm".into() }
    }

    async fn verify(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        assert_eq!(requirements.view().scheme, "exact");
        self.payloads.lock().push(payload.clone());
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
                // Spec: a base58 transaction signature.
                transaction: "5VERYrealSVMsignature1111111111111111111111".into(),
                network: requirements.view().network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }
}

fn solana_terms(provider: &EntityKeypair, registry_ref: net_payments::core::registry::RegistryRef) -> String {
    let template = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: SOLANA_MAINNET.into(),
        amount: "10000".into(),
        asset: SPL_USDC.into(),
        pay_to: PAY_TO.into(),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({ "feePayer": FEE_PAYER, "memo": "cap 42" })),
    })
    .expect("template");
    let terms = PricingTerms::new(
        provider.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry_ref,
    );
    String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8")
}

struct World {
    flow: CallerPaymentFlow,
    facilitator: Arc<SvmFacilitator>,
    billing: Arc<BillingLog>,
    terms_json: String,
    _dir: tempfile::TempDir,
}

async fn world(with_signer: bool) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock);

    // ── the solana "pack applied": registry v1 + enabled network ──
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_registry_v1(provider_keys.entity_id().clone());
    let spend_path = dir.path().join("spend.json");
    SpendPolicyEngine::new(&spend_path, SpendProfile::Production)
        .configure(|defaults, _| {
            defaults.allowed_networks = vec![SOLANA_MAINNET.to_string()];
            defaults.max_per_call = Some(AtomicAmount::from_u128(50_000));
        })
        .await
        .expect("configure");

    let facilitator = Arc::new(SvmFacilitator { payloads: parking_lot::Mutex::new(Vec::new()) });
    let billing = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            facilitator.clone(),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(billing.clone()),
    );
    let terms_json = solana_terms(&provider_keys, registry.reference().expect("ref"));

    let mut flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        Arc::new(InProcessProvider::new(engine, clock.clone())),
        clock,
    );
    if with_signer {
        // The scripted wallet: sees the WHOLE structured intent (the
        // policy surface a real wallet refuses on) and returns the blob.
        flow = flow.with_signer(
            "solana",
            Arc::new(ExternalSvmSigner::new(PAY_TO, move |intent| {
                Box::pin(async move {
                    assert_eq!(intent.network, SOLANA_MAINNET);
                    assert_eq!(intent.mint, SPL_USDC);
                    assert_eq!(intent.pay_to, PAY_TO);
                    assert_eq!(intent.amount, "10000");
                    assert_eq!(intent.fee_payer, FEE_PAYER);
                    assert_eq!(intent.memo.as_deref(), Some("cap 42"));
                    Ok(SIGNED_BLOB.to_string())
                })
            })),
        );
    }
    World { flow, facilitator, billing, terms_json, _dir: dir }
}

#[tokio::test]
async fn the_full_exact_svm_lifecycle_runs_on_an_enabled_network() {
    let w = world(true).await;
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Paid { proof, .. } = decision else {
        panic!("expected Paid on the enabled solana network, got {decision:?}");
    };
    assert_eq!(proof["transaction"], "5VERYrealSVMsignature1111111111111111111111");

    // What crossed the facilitator boundary: the spec-pinned payload
    // shape around the wallet's blob, byte-preserved.
    {
        let payloads = w.facilitator.payloads.lock();
        assert_eq!(payloads.len(), 1);
        let sent = payloads[0].view();
        assert_eq!(sent.accepted.network, SOLANA_MAINNET);
        assert_eq!(sent.payload, serde_json::json!({ "transaction": SIGNED_BLOB }));
    }

    // Billed exactly once, in the real asset on the real network.
    let recorded = w.billing.read_all().await.expect("read");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].network, SOLANA_MAINNET);
    assert_eq!(recorded[0].asset, SPL_USDC);
    assert_eq!(recorded[0].amount, AtomicAmount::from_u128(10_000));
}

#[tokio::test]
async fn without_a_solana_signer_the_terms_are_refused_at_selection() {
    let w = world(false).await;
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected a structured denial, got {decision:?}");
    };
    assert!(policy_reason.contains("no settleable"), "{policy_reason}");
    assert!(w.facilitator.payloads.lock().is_empty(), "nothing was authored or sent");
    assert!(w.billing.read_all().await.expect("read").is_empty());
}
