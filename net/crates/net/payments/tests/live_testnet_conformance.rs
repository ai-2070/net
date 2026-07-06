//! The network ladder, rung 1: LIVE conformance against the x402.org
//! testnet facilitator (Base Sepolia). Every test is `#[ignore]` — CI
//! never touches the network; an operator runs this suite deliberately:
//!
//! ```text
//! cargo test -p net-payments \
//!   --features http-facilitator,unsafe-dev-signer \
//!   --test live_testnet_conformance -- --ignored --nocapture
//! ```
//!
//! Environment:
//! - `NET_PAYMENTS_LIVE_FACILITATOR` — facilitator base URL (default:
//!   the shipped pack's `https://x402.org/facilitator`)
//! - `NET_PAYMENTS_LIVE_EVM_KEY` — hex 32-byte secp256k1 secret, a
//!   **testnet-only** key (required by the signed tests, which panic
//!   with instructions when it is absent)
//! - `NET_PAYMENTS_LIVE_PAY_TO` — settlement recipient (default: the
//!   key's own address — self-payment keeps the test USDC)
//! - `NET_PAYMENTS_LIVE_AMOUNT` — atomic amount (default `1000` =
//!   0.001 USDC at 6 decimals)
//! - `NET_PAYMENTS_LIVE_SETTLE=1` — explicit opt-in for the one test
//!   that actually moves testnet USDC (skipped loudly otherwise)
//! - `NET_PAYMENTS_LIVE_RPC` — Base Sepolia JSON-RPC for the checker
//!   (default: the shipped pack's `https://sepolia.base.org`)
//!
//! Funding: the payer key needs Base Sepolia USDC only (EIP-3009 is
//! facilitator-submitted; the payer pays no gas) — the Circle faucet
//! dispenses test USDC on Base Sepolia.
//!
//! A failure here is a *finding about the live boundary* (endpoint
//! moved, pair dropped, vocabulary drift), not a test bug — that is
//! exactly what the suite exists to surface before an operator trusts
//! a pack.

#![cfg(all(feature = "http-facilitator", feature = "unsafe-dev-signer"))]

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::checker::eip155::Eip155Checker;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::default_registry_v1;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::core::verification::{InvalidationReason, VerificationTier};
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::client::{HttpFacilitator, NoAuth};
use net_payments::facilitator::packs;
use net_payments::facilitator::Facilitator;
use net_payments::flow::signer::dev::DevLocalSigner;
use net_payments::flow::signer::{ExternalSigner, SchemeSigner};
use net_payments::flow::{
    CallerDecision, CallerPaymentFlow, Clock, InProcessProvider, SystemClock,
};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::schemes::exact_evm;
use net_payments::x402::{X402Carry, X402_VERSION};

const CAPABILITY: &str = "live-conformance/paid-fixture";
const TESTNET_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";

fn endpoint() -> String {
    std::env::var("NET_PAYMENTS_LIVE_FACILITATOR")
        .unwrap_or_else(|_| packs::X402_ORG_FACILITATOR.to_string())
}

fn rpc_endpoint() -> String {
    std::env::var("NET_PAYMENTS_LIVE_RPC").unwrap_or_else(|_| packs::RPC_BASE_SEPOLIA.to_string())
}

fn amount() -> u128 {
    std::env::var("NET_PAYMENTS_LIVE_AMOUNT")
        .map(|v| {
            v.parse()
                .expect("NET_PAYMENTS_LIVE_AMOUNT must be a decimal integer")
        })
        .unwrap_or(1000)
}

/// The testnet payer key — required, and required to be explicit.
fn payer_signer() -> Arc<DevLocalSigner> {
    let hex_key = std::env::var("NET_PAYMENTS_LIVE_EVM_KEY").unwrap_or_else(|_| {
        panic!(
            "this test signs a live payload: set NET_PAYMENTS_LIVE_EVM_KEY to a hex \
             32-byte secp256k1 secret for a TESTNET-ONLY key (fund it with Base \
             Sepolia USDC from the Circle faucet for the settle test)"
        )
    });
    let bytes = hex::decode(hex_key.trim().trim_start_matches("0x")).expect("key hex");
    let secret: [u8; 32] = bytes.try_into().expect("key must be exactly 32 bytes");
    Arc::new(DevLocalSigner::from_secret(secret).expect("signer"))
}

/// The announced requirements for the conformance capability.
fn requirements(pay_to: &str) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: packs::NETWORK_BASE_SEPOLIA.into(),
        amount: amount().to_string(),
        asset: TESTNET_USDC.into(),
        pay_to: pay_to.into(),
        max_timeout_seconds: 300,
        // EIP-712 domain metadata for Base Sepolia USDC, spec-carried.
        extra: Some(serde_json::json!({ "name": "USDC", "version": "2" })),
    })
    .expect("author requirements")
}

/// Rung 1a — the facilitator is up, speaks the pinned `/supported`
/// shape, and still offers the conformance pair at x402Version 2.
#[tokio::test]
#[ignore = "live network — run deliberately with --ignored (see module docs)"]
async fn supported_offers_the_conformance_pair_live() {
    let facilitator = HttpFacilitator::new(endpoint(), Arc::new(NoAuth)).expect("client");
    let supported = facilitator
        .validate_pairs(&[("exact".to_string(), packs::NETWORK_BASE_SEPOLIA.to_string())])
        .await
        .expect("the survey-pinned pair must be offered live — if this fails, the pin is stale");

    println!(
        "facilitator {} offers {} kinds:",
        endpoint(),
        supported.kinds.len()
    );
    for kind in &supported.kinds {
        println!(
            "  v{} ({}, {})",
            kind.x402_version, kind.scheme, kind.network
        );
    }
    if !supported.extensions.is_empty() {
        println!("extensions: {:?}", supported.extensions);
    }
    for (pattern, addresses) in &supported.signers {
        println!("signer {pattern}: {addresses:?}");
    }
}

/// Rung 1b — the shipped Base Sepolia pack passes its own load-time
/// gate against the live facilitator (`from_config` = fetch `/supported`
/// + validate every enabled pair).
#[tokio::test]
#[ignore = "live network — run deliberately with --ignored (see module docs)"]
async fn the_shipped_pack_loads_against_the_live_facilitator() {
    let mut pack = packs::x402_org_base_sepolia();
    pack.endpoint = endpoint(); // env override, defaults to the pack's own
    let client = HttpFacilitator::from_config(&pack, Arc::new(NoAuth))
        .await
        .expect("the shipped pack must load against the live facilitator");
    println!(
        "pack loaded: {:?} via {}",
        pack.pairs,
        client.reference().endpoint
    );
}

/// Rung 1c — a really-signed EIP-3009 payload gets a *structural*
/// answer from live `/verify`: either valid (funded payer) or a
/// spec-vocabulary rejection (e.g. `insufficient_funds`) that maps into
/// the closed [`InvalidationReason`] vocabulary. Both outcomes pass —
/// what must not happen is a transport/protocol failure. Nothing is
/// spent by this test.
#[tokio::test]
#[ignore = "live network — run deliberately with --ignored (see module docs)"]
async fn a_signed_verify_answers_structurally_live() {
    let signer = payer_signer();
    let pay_to = std::env::var("NET_PAYMENTS_LIVE_PAY_TO").unwrap_or_else(|_| signer.address());
    let requirements = requirements(&pay_to);

    // Author the authorization directly (no engine on this rung): a
    // fresh wall-clock window and a content-derived nonce.
    let now_s = SystemClock.now_ns() / 1_000_000_000;
    let nonce = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.live_conformance.nonce@1");
        hasher.update(&now_s.to_be_bytes());
        hasher.update(signer.address().as_bytes());
        format!("0x{}", hex::encode(hasher.finalize().as_bytes()))
    };
    let authorization = exact_evm::ExactEvmAuthorization {
        from: signer.address(),
        to: pay_to.clone(),
        value: amount().to_string(),
        valid_after: now_s.saturating_sub(60),
        valid_before: now_s + 300,
        nonce,
    };
    let typed = exact_evm::typed_data(requirements.view(), &authorization).expect("typed data");
    let signature = signer.sign_typed_data(&typed).await.expect("sign");
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: X402_VERSION,
        resource: None,
        accepted: requirements.view().clone(),
        payload: exact_evm::payload_object(&authorization, &signature),
        extensions: None,
    })
    .expect("author payload");

    let facilitator = HttpFacilitator::new(endpoint(), Arc::new(NoAuth)).expect("client");
    let outcome = facilitator
        .verify(&payload, &requirements)
        .await
        .expect("live /verify must answer structurally, never fail at the transport");

    assert_eq!(
        outcome.tier,
        VerificationTier::Observed,
        "receipts cap at observed"
    );
    let view = outcome.response.view();
    if view.is_valid {
        println!("live /verify: VALID (payer {} is funded)", signer.address());
    } else {
        let reason = view
            .invalid_reason
            .as_deref()
            .expect("an invalid answer must carry the spec's invalidReason");
        let mapped = InvalidationReason::from_facilitator_reason(reason);
        println!("live /verify: invalid (`{reason}` -> {mapped:?}) — structural, as required");
    }
}

/// Rung 1d — THE acceptance: real testnet USDC moves on Base Sepolia
/// through the *unchanged* P0 engine and caller flow — announced terms
/// → provider-signed quote → spend policy (config-enabled network) →
/// EIP-3009 authored through [`ExternalSigner`] → live verify + settle
/// → billed — then the independent chain checker upgrades the
/// verification chain past receipt trust (the shipped pack's
/// `confirmed(1)` posture). Spends ~`NET_PAYMENTS_LIVE_AMOUNT`; opt in
/// with `NET_PAYMENTS_LIVE_SETTLE=1`.
#[tokio::test]
#[ignore = "live network — run deliberately with --ignored (see module docs)"]
async fn the_unchanged_engine_settles_on_base_sepolia_live() {
    if std::env::var("NET_PAYMENTS_LIVE_SETTLE").as_deref() != Ok("1") {
        eprintln!(
            "SKIPPED (not a pass): this test spends testnet USDC — \
             set NET_PAYMENTS_LIVE_SETTLE=1 to opt in"
        );
        return;
    }
    let dev = payer_signer();
    let pay_to = std::env::var("NET_PAYMENTS_LIVE_PAY_TO").unwrap_or_else(|_| dev.address());

    // The production signer shape: the key stays behind the callback
    // boundary (here the dev signer plays the wallet's role).
    let wallet = dev.clone();
    let signer = Arc::new(ExternalSigner::new(dev.address(), move |doc| {
        let wallet = wallet.clone();
        Box::pin(async move { wallet.sign_typed_data(&doc).await })
    }));

    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let pack = packs::x402_org_base_sepolia();

    // ── the pack, applied: registry v1 + enabled network + caps ──
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_registry_v1(provider_keys.entity_id().clone());
    let spend_path = dir.path().join("spend.json");
    SpendPolicyEngine::new(&spend_path, SpendProfile::Production)
        .configure(|defaults, _| {
            defaults.allowed_networks = vec![packs::NETWORK_BASE_SEPOLIA.to_string()];
            defaults.max_per_call = Some(AtomicAmount::from_u128(amount().saturating_mul(10)));
        })
        .await
        .expect("configure");

    // ── provider: the unchanged engine over the LIVE facilitator ──
    let facilitator = Arc::new(HttpFacilitator::new(endpoint(), Arc::new(NoAuth)).expect("client"));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            facilitator,
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine"),
    );
    let provider = Arc::new(
        InProcessProvider::new(engine.clone(), clock.clone()).with_quote_ttl_ns(300_000_000_000),
    );

    let template = requirements(&pay_to);
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        CAPABILITY,
        vec![template],
        registry.reference().expect("ref"),
    );
    let terms_json =
        String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

    // ── caller: the unchanged flow ──
    let flow = CallerPaymentFlow::new(
        Arc::new(EntityKeypair::generate()),
        SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
        registry,
        provider,
        clock.clone(),
    )
    .with_signer("eip155", signer);

    let decision = flow.run(CAPABILITY, &terms_json).await;
    let CallerDecision::Paid {
        quote_id, proof, ..
    } = decision
    else {
        panic!("expected Paid on live Base Sepolia, got {decision:?}");
    };
    let transaction = proof["transaction"]
        .as_str()
        .expect("settlement transaction")
        .to_string();
    println!("SETTLED live: {transaction} (quote {quote_id})");

    // ── past receipt trust: the pack's checker upgrades the chain ──
    let checker = Eip155Checker::new(packs::NETWORK_BASE_SEPOLIA, rpc_endpoint()).expect("checker");
    let required = pack.required_tier(packs::NETWORK_BASE_SEPOLIA);
    let mut upgraded = None;
    for attempt in 0..24 {
        let decision = engine
            .re_verify_with_checker(&quote_id, &checker, required, clock.now_ns())
            .await
            .expect("engine");
        match decision {
            PaymentDecision::Served { tier, .. } if tier.satisfies(&required) => {
                upgraded = Some(tier);
                break;
            }
            other => {
                println!("checker attempt {attempt}: {other:?} — waiting for inclusion");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
    let tier = upgraded.expect("the independent check must reach the pack's tier within ~2min");
    println!("independent check reached {tier:?} (required {required:?})");

    // The signed chain records both verifiers: the live facilitator's
    // observed receipt, then the independent chain check.
    let status = engine
        .status(&quote_id)
        .await
        .expect("status")
        .expect("known quote");
    assert!(status.billing_event_id.is_some(), "served means billed");
    assert!(status.chain.len() >= 2);
    assert_eq!(status.chain[0].verifier.endpoint, endpoint());
    assert!(status
        .chain
        .last()
        .expect("chain")
        .verifier
        .endpoint
        .starts_with("independent-chain-check:"));
    println!(
        "verification chain: {:?}",
        status
            .chain
            .iter()
            .map(|e| format!("{}@{:?}", e.verifier.endpoint, e.tier))
            .collect::<Vec<_>>()
    );
}
