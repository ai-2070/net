//! P1 WS4 acceptance, caller side: with a network pack in place —
//! registry entry + enabled `allowed_networks` + a settlement signer —
//! the full exact-scheme lifecycle runs end to end through the
//! *unchanged* flow and engine: announced eip155 terms → provider-signed
//! quote → spend policy (config-enabled real network) → EIP-3009 typed
//! data signed by the dev signer → facilitator verify + settle → billed.
//!
//! The facilitator here is a test stub accepting the exact scheme (the
//! mock facilitator deliberately speaks only `mock:net`; the HTTP
//! conformance suite covers the real client) — it also captures the
//! payload so the test can assert what actually crossed the boundary:
//! a 65-byte recoverable signature over the quoted authorization.
#![cfg(feature = "unsafe-dev-signer")]

use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::{default_registry_v1, AssetRegistry};
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::core::verification::{VerificationTier, VerifierRef};
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use net_payments::flow::signer::dev::DevLocalSigner;
use net_payments::flow::signer::SchemeSigner;
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::settlement::{SettlementResponse, VerifyResponse};
use net_payments::x402::X402Carry;

const NOW: u64 = 1_740_672_000_000_000_000;
const CAPABILITY: &str = "42/paid-eip155-tool";
const TESTNET_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";

struct TestClock;
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        NOW + 1_000
    }
}

/// An exact-scheme facilitator stub: verifies shape, settles by echoing
/// the required amount, and captures every payload it saw.
struct ExactSchemeFacilitator {
    payloads: parking_lot::Mutex<Vec<X402Carry<PaymentPayload>>>,
}

#[async_trait]
impl Facilitator for ExactSchemeFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "test-exact-evm".into(),
        }
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
                payer: payload.view().payload["authorization"]["from"]
                    .as_str()
                    .map(String::from),
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
                transaction: "0xbase5ep011a7e57".into(),
                network: requirements.view().network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }
}

#[tokio::test]
async fn the_full_exact_scheme_lifecycle_runs_on_an_enabled_network() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock);

    // ── the network pack: registry v1 + enabled network + signer ──
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry: AssetRegistry = default_registry_v1(provider_keys.entity_id().clone());
    let spend_path = dir.path().join("spend.json");
    let spend_config = SpendPolicyEngine::new(&spend_path, SpendProfile::Production);
    spend_config
        .configure(|defaults, _| {
            defaults.allowed_networks = vec!["eip155:84532".to_string()];
            defaults.max_per_call = Some(AtomicAmount::from_u128(50_000));
        })
        .await
        .expect("configure");
    let signer = Arc::new(DevLocalSigner::from_secret([21u8; 32]).expect("signer"));

    // ── provider: engine over the exact-scheme facilitator ──
    let facilitator = Arc::new(ExactSchemeFacilitator {
        payloads: parking_lot::Mutex::new(Vec::new()),
    });
    let billing_log = Arc::new(BillingLog::new(dir.path().join("billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            facilitator.clone(),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(billing_log.clone()),
    );

    // Announced terms: testnet USDC on Base Sepolia, with the EIP-712
    // domain metadata the spec carries in `extra`.
    let template = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: "eip155:84532".into(),
        amount: "10000".into(),
        asset: TESTNET_USDC.into(),
        pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({ "name": "USDC", "version": "2" })),
    })
    .expect("template");
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().expect("ref"),
    );
    let terms_json =
        String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

    // ── caller: the flow with the pack wired ──
    let flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        Arc::new(InProcessProvider::new(engine, clock.clone())),
        clock,
    )
    .with_signer("eip155", signer.clone());

    let decision = flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Paid {
        quote_id: _,
        binding_sig: _,
        proof,
    } = decision
    else {
        panic!("expected Paid on the enabled network, got {decision:?}");
    };
    assert_eq!(proof["transaction"], "0xbase5ep011a7e57");

    // What crossed the facilitator boundary: an EIP-3009 authorization
    // from the signer's address, with a 65-byte recoverable signature.
    {
        let payloads = facilitator.payloads.lock();
        assert_eq!(payloads.len(), 1);
        let sent = payloads[0].view();
        assert_eq!(sent.accepted.network, "eip155:84532");
        let authorization = &sent.payload["authorization"];
        assert_eq!(
            authorization["from"].as_str().map(str::to_lowercase),
            Some(signer.address().to_lowercase())
        );
        assert_eq!(
            authorization["to"],
            "0x209693Bc6afc0C5328bA36FaF03C514EF312287C"
        );
        assert_eq!(authorization["value"], "10000");
        let signature = sent.payload["signature"].as_str().expect("signature");
        assert_eq!(
            hex::decode(signature.strip_prefix("0x").expect("0x"))
                .expect("hex")
                .len(),
            65
        );
    }

    // And it billed exactly once, in the real asset on the real network.
    let recorded = billing_log.read_all().await.expect("read");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].network, "eip155:84532");
    assert_eq!(recorded[0].asset, TESTNET_USDC);
    assert_eq!(recorded[0].amount, AtomicAmount::from_u128(10_000));
}

/// An exact-scheme facilitator that rejects at verify — standing in for a
/// provider that reports "no sale" while, in the threat model, it already
/// holds (and could still submit) the bearer EIP-3009 authorization.
struct RejectingExactFacilitator;

#[async_trait]
impl Facilitator for RejectingExactFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "test-exact-reject".into(),
        }
    }

    async fn verify(
        &self,
        _payload: &X402Carry<PaymentPayload>,
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        Ok(VerifyOutcome {
            response: X402Carry::author(&VerifyResponse {
                is_valid: false,
                invalid_reason: Some("provider_says_no".into()),
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
        _requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        Err(FacilitatorError::protocol(
            "settle must not run after a verify reject",
        ))
    }
}

/// M1 regression: on a bearer pull-authorization scheme (exact/EIP-3009)
/// the signed authorization has already crossed to the provider, so a
/// provider "rejected" claim does NOT release the caller's spend
/// reservation. Otherwise a lying provider could settle on-chain while
/// resetting the per-day counter every cycle, defeating `max_per_day` as
/// a loss bound.
#[tokio::test]
async fn a_bearer_scheme_reject_keeps_the_reservation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock);

    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry: AssetRegistry = default_registry_v1(provider_keys.entity_id().clone());
    let spend_path = dir.path().join("spend.json");
    let spend_config = SpendPolicyEngine::new(&spend_path, SpendProfile::Production);
    spend_config
        .configure(|defaults, _| {
            defaults.allowed_networks = vec!["eip155:84532".to_string()];
            defaults.max_per_call = Some(AtomicAmount::from_u128(50_000));
            defaults.max_per_day = Some(AtomicAmount::from_u128(50_000));
        })
        .await
        .expect("configure");
    let signer = Arc::new(DevLocalSigner::from_secret([21u8; 32]).expect("signer"));

    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(RejectingExactFacilitator),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine"),
    );

    let template = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: "eip155:84532".into(),
        amount: "10000".into(),
        asset: TESTNET_USDC.into(),
        pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({ "name": "USDC", "version": "2" })),
    })
    .expect("template");
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().expect("ref"),
    );
    let terms_json =
        String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

    let flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        Arc::new(InProcessProvider::new(engine, clock.clone())),
        clock,
    )
    .with_signer("eip155", signer.clone());

    let decision = flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied on a provider reject, got {decision:?}");
    };
    assert!(policy_reason.contains("rejected"), "{policy_reason}");

    // The reservation stands: the day counter still reflects the spend.
    let spend = SpendPolicyEngine::new(&spend_path, SpendProfile::Production);
    assert_eq!(
        spend
            .spent_today("eip155:84532", TESTNET_USDC, NOW)
            .await
            .unwrap(),
        AtomicAmount::from_u128(10_000),
        "a bearer-scheme reject must NOT release the reservation"
    );
}
