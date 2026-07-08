//! The exact-XRPL seam, end to end (Mode A, XRP-only): with the xrpl
//! pack's pieces in place — registry v1 (now carries XRP) + the network
//! in `allowed_networks` + an [`ExternalXrplSigner`] wallet — the full
//! paid lifecycle runs through the *unchanged* flow and engine. The
//! wallet is a scripted closure standing in for the host's real one: it
//! asserts the structured intent it was shown (the no-signing-oracle
//! property, XRPL edition — including the pinned invoiceId binding) and
//! returns a fake presigned blob; the facilitator stub asserts the
//! pinned `{"signedTxBlob": ...}` payload shape crossed the boundary.
//!
//! Without an xrpl signer, the same terms are refused at selection —
//! settleability is capability, and its absence is a structured denial.
//! And because the presigned blob is a bearer instrument, a facilitator
//! that claims rejection after receiving it keeps the spend reservation
//! (the M1 posture, xrpl edition).

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
use net_payments::flow::signer::ExternalXrplSigner;
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::settlement::{SettlementResponse, VerifyResponse};
use net_payments::x402::X402Carry;

const NOW: u64 = 1_740_672_000_000_000_000;
const CAPABILITY: &str = "42/paid-xrpl-tool";
const XRPL_MAINNET: &str = "xrpl:0";
const PAY_TO: &str = "rMerchant1111111111111111111111111";
const INVOICE: &str = "inv-cap-42";
/// What the scripted wallet returns — valid hex, opaque otherwise.
const SIGNED_BLOB: &str = "1200002280000000c0ffee";

struct TestClock;
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        NOW + 1_000
    }
}

/// An exact-XRPL facilitator stub: asserts the pinned payload shape and
/// settles by echoing the required amount (or rejects, for the bearer
/// row).
struct XrplFacilitator {
    payloads: parking_lot::Mutex<Vec<X402Carry<PaymentPayload>>>,
    reject: bool,
}

#[async_trait]
impl Facilitator for XrplFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "test-exact-xrpl".into(),
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
                is_valid: !self.reject,
                invalid_reason: self.reject.then(|| "facilitator_says_no".to_string()),
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
        assert!(!self.reject, "settle must not run after a verify reject");
        Ok(SettleOutcome {
            response: X402Carry::author(&SettlementResponse {
                success: true,
                error_reason: None,
                payer: Some("rPayerAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()),
                transaction: "C53ECF838647FA5A4C780377025FEC7999AB4182590510CA461444B207AB74A9"
                    .into(),
                network: requirements.view().network.clone(),
                amount: Some(requirements.view().amount.clone()),
                extensions: None,
            })
            .map_err(|e| FacilitatorError::protocol(e.to_string()))?,
            tier: VerificationTier::Observed,
        })
    }
}

fn xrpl_terms(
    provider: &EntityKeypair,
    registry_ref: net_payments::core::registry::RegistryRef,
) -> String {
    let template = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: XRPL_MAINNET.into(),
        amount: "1000000".into(),
        asset: "XRP".into(),
        pay_to: PAY_TO.into(),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({
            "invoiceId": INVOICE,
            "destinationTag": 7,
            "sourceTag": 804681468u32,
        })),
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
    facilitator: Arc<XrplFacilitator>,
    billing: Arc<BillingLog>,
    terms_json: String,
    spend_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn world(with_signer: bool, rejecting: bool) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock);

    // ── the xrpl "pack applied": registry v1 + enabled network ──
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_registry_v1(provider_keys.entity_id().clone());
    let spend_path = dir.path().join("spend.json");
    SpendPolicyEngine::new(&spend_path, SpendProfile::Production)
        .configure(|defaults, _| {
            defaults.allowed_networks = vec![XRPL_MAINNET.to_string()];
            defaults.max_per_call = Some(AtomicAmount::from_u128(5_000_000));
        })
        .await
        .expect("configure");

    let facilitator = Arc::new(XrplFacilitator {
        payloads: parking_lot::Mutex::new(Vec::new()),
        reject: rejecting,
    });
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
    let terms_json = xrpl_terms(&provider_keys, registry.reference().expect("ref"));

    let mut flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        Arc::new(InProcessProvider::new(engine, clock.clone())),
        clock,
    );
    if with_signer {
        // The scripted wallet: sees the WHOLE structured intent (the
        // policy surface a real wallet refuses on — amount, recipient,
        // tags, and the quote-binding invoice) and returns the blob.
        flow = flow.with_signer(
            "xrpl",
            Arc::new(ExternalXrplSigner::new(
                "rPayerAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                move |intent| {
                    Box::pin(async move {
                        assert_eq!(intent.network, XRPL_MAINNET);
                        assert_eq!(intent.asset, "XRP");
                        assert_eq!(intent.pay_to, PAY_TO);
                        assert_eq!(intent.amount, "1000000");
                        assert_eq!(intent.invoice_id, INVOICE);
                        assert_eq!(intent.destination_tag, Some(7));
                        assert_eq!(intent.source_tag, Some(804_681_468));
                        Ok(SIGNED_BLOB.to_string())
                    })
                },
            )),
        );
    }
    World {
        flow,
        facilitator,
        billing,
        terms_json,
        spend_path,
        _dir: dir,
    }
}

#[tokio::test]
async fn the_full_exact_xrpl_lifecycle_runs_on_an_enabled_network() {
    let w = world(true, false).await;
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Paid { proof, .. } = decision else {
        panic!("expected Paid on the enabled network, got {decision:?}");
    };
    assert_eq!(
        proof["transaction"],
        "C53ECF838647FA5A4C780377025FEC7999AB4182590510CA461444B207AB74A9"
    );

    // What crossed the facilitator boundary: the pinned payload shape
    // around the wallet's presigned blob.
    {
        let payloads = w.facilitator.payloads.lock();
        assert_eq!(payloads.len(), 1);
        let sent = payloads[0].view();
        assert_eq!(sent.accepted.network, XRPL_MAINNET);
        assert_eq!(sent.payload["signedTxBlob"], SIGNED_BLOB);
    }

    // And it billed exactly once, in XRP drops on the xrpl network.
    let recorded = w.billing.read_all().await.expect("read");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].network, XRPL_MAINNET);
    assert_eq!(recorded[0].asset, "XRP");
    assert_eq!(recorded[0].amount, AtomicAmount::from_u128(1_000_000));
}

#[tokio::test]
async fn without_an_xrpl_signer_the_terms_are_refused_at_selection() {
    let w = world(false, false).await;
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied without a signer, got {decision:?}");
    };
    assert!(policy_reason.contains("no settleable"), "{policy_reason}");
    assert!(w.facilitator.payloads.lock().is_empty());
}

/// XRPL is **off by default, by construction** — the recorded contract
/// for the ladder's rung 4. Enabling it takes BOTH a settlement signer
/// AND the network in `allowed_networks`; neither gate auto-opens.
/// `without_an_xrpl_signer…` pins the signer gate (no wallet → refused at
/// selection); this pins the allowlist gate: even WITH a wallet, an
/// XRPL price is denied at the spend policy until the network is
/// explicitly enabled — nothing is signed or sent. A future change that
/// silently ambient-enables XRPL fails a clearly-named test.
#[tokio::test]
async fn xrpl_stays_off_by_default_with_a_wallet_until_the_network_is_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock);
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_registry_v1(provider_keys.entity_id().clone());
    // Default Production policy: `allowed_networks` is empty — no real
    // network is enabled. (The registry carrying XRP is the asset
    // allowlist, NOT the enablement switch.)
    let spend_path = dir.path().join("spend.json");
    let facilitator = Arc::new(XrplFacilitator {
        payloads: parking_lot::Mutex::new(Vec::new()),
        reject: false,
    });
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            facilitator.clone(),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine"),
    );
    let terms_json = xrpl_terms(&provider_keys, registry.reference().expect("ref"));
    let flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        Arc::new(InProcessProvider::new(engine, clock.clone())),
        clock,
    )
    // A wallet IS configured — so settleability is not what holds XRPL
    // off here; the allowlist is.
    .with_signer(
        "xrpl",
        Arc::new(ExternalXrplSigner::new(
            "rPayerAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            move |_intent| Box::pin(async move { Ok(SIGNED_BLOB.to_string()) }),
        )),
    );

    let decision = flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("XRPL must be denied until the network is allowlisted, got {decision:?}");
    };
    // Denied at the spend policy (network not enabled), NOT at selection —
    // the wallet made it settleable; enablement is the separate gate.
    assert!(
        policy_reason.contains("not enabled"),
        "the denial names the enablement gate: {policy_reason}"
    );
    assert!(
        facilitator.payloads.lock().is_empty(),
        "nothing is signed or sent for an unenabled network"
    );
}

/// M1 posture, xrpl edition: the presigned blob is a bearer instrument —
/// once it crossed to the provider, a claimed rejection does NOT release
/// the caller's spend reservation.
#[tokio::test]
async fn a_bearer_scheme_reject_keeps_the_reservation() {
    let w = world(true, true).await;
    let decision = w.flow.run(CAPABILITY, &w.terms_json).await;
    let CallerDecision::Denied { policy_reason } = decision else {
        panic!("expected Denied on a provider reject, got {decision:?}");
    };
    assert!(policy_reason.contains("rejected"), "{policy_reason}");

    let spend = SpendPolicyEngine::new(&w.spend_path, SpendProfile::Production);
    assert_eq!(
        spend.spent_today(XRPL_MAINNET, "XRP", NOW).await.unwrap(),
        AtomicAmount::from_u128(1_000_000),
        "a bearer-scheme reject must NOT release the reservation"
    );
}
