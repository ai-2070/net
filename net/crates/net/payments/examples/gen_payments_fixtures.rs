//! Deterministic golden-vector emitter for `tests/cross_lang_payments/`.
//!
//! Regenerate with:
//!
//! ```text
//! cargo run -p net-payments --example gen_payments_fixtures
//! ```
//!
//! and diff against the committed `payment_vectors.json` — any drift in
//! canonical encoding, signing, or hash derivation shows up as a fixture
//! diff. Everything here is fixed: identity seeds, timestamps, amounts.
//! ed25519 is deterministic (RFC 8032), so signatures reproduce exactly.
//!
//! Timestamps deliberately stay below 2^53 so every verifier language can
//! round-trip the canonical JSON integers losslessly (JS numbers). At
//! runtime, bindings never re-parse envelope JSON — they carry it opaquely
//! — so real ns timestamps are unaffected; the constraint is fixture-only.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::billing_event::BillingEvent;
use net_payments::core::canonical::{canonical_bytes, signed_payload_bytes, SignedEnvelope};
use net_payments::core::idempotency::IdempotencyScope;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::settlement_ref::SettlementRef;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::core::verification::{
    VerificationEvent, VerificationStatus, VerificationTier, VerifierRef,
};
use net_payments::core::versioning::{TAG_BILLING_EVENT, TAG_PAYMENT_VERIFICATION};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::settlement::SettlementResponse;
use net_payments::x402::X402Carry;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::json;

const ISSUED_NS: u64 = 1_000_000_000_000_000;
const EXPIRES_NS: u64 = 1_000_060_000_000_000;
const CHECKED_1_NS: u64 = 1_000_001_000_000_000;
const CHECKED_2_NS: u64 = 1_000_001_000_000_001;
const SETTLED_NS: u64 = 1_000_001_500_000_000;
const OCCURRED_NS: u64 = 1_000_002_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

fn envelope_entry<T: SignedEnvelope>(
    name: &str,
    envelope: &T,
    extras: &[(&str, serde_json::Value)],
) -> Result<serde_json::Value, Box<dyn Error>> {
    let canonical = String::from_utf8(canonical_bytes(envelope)?)?;
    let signed_payload = String::from_utf8(signed_payload_bytes(envelope)?)?;
    let mut entry = serde_json::Map::new();
    entry.insert("name".into(), json!(name));
    entry.insert("object".into(), json!(T::OBJECT_TAG));
    entry.insert("canonical".into(), json!(canonical));
    entry.insert("signed_payload".into(), json!(signed_payload));
    entry.insert(
        "signer_hex".into(),
        json!(hex::encode(envelope.signer().as_bytes())),
    );
    entry.insert(
        "signature_hex".into(),
        match envelope.signature() {
            Some(sig) => json!(hex::encode(sig.0)),
            None => serde_json::Value::Null,
        },
    );
    for (k, v) in extras {
        entry.insert((*k).into(), v.clone());
    }
    Ok(serde_json::Value::Object(entry))
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = manifest_dir
        .join("..")
        .join("tests")
        .join("cross_lang_payments");
    let fixtures_v2 = out_dir.join("fixtures").join("x402").join("v2.0");

    let read = |p: &Path| -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?)
    };
    let req_eip155_bytes = read(&fixtures_v2.join("payment_requirements.json"))?;
    let payload_eip155_bytes = read(&fixtures_v2.join("payment_payload.json"))?;
    let settle_eip155_bytes = read(&fixtures_v2.join("settlement_response.json"))?;

    // Fixed test identities — seeds are public fixture data, never reuse.
    let caller = EntityKeypair::from_bytes([1u8; 32]);
    let provider = EntityKeypair::from_bytes([2u8; 32]);
    let registry_signer = EntityKeypair::from_bytes([3u8; 32]);

    // ---- registry -------------------------------------------------------
    let mut registry = default_mock_registry(registry_signer.entity_id().clone());
    registry.sign_with(&registry_signer)?;
    let registry_ref = registry.reference()?;

    // ---- x402 carries ---------------------------------------------------
    let mock_requirements = X402Carry::author(&PaymentRequirements {
        scheme: "mock".into(),
        network: "mock:net".into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })?;
    let req_eip155: X402Carry<PaymentRequirements> =
        X402Carry::from_bytes(req_eip155_bytes.clone())?;
    // Parsed for validation only — the payload fixture is a pure
    // byte-preservation vector (it travels in the invocation envelope,
    // which is out of this crate's object model).
    let _payload_eip155: X402Carry<PaymentPayload> =
        X402Carry::from_bytes(payload_eip155_bytes.clone())?;
    let settle_eip155: X402Carry<SettlementResponse> =
        X402Carry::from_bytes(settle_eip155_bytes.clone())?;

    // ---- envelopes ------------------------------------------------------
    let terms = PricingTerms::new(
        provider.entity_id().clone(),
        CAPABILITY,
        vec![mock_requirements.clone(), req_eip155.clone()],
        registry_ref.clone(),
    );

    let mut quote_mock = PaymentQuote::new(
        provider.entity_id().clone(),
        caller.entity_id().clone(),
        CAPABILITY,
        None,
        mock_requirements.clone(),
        registry_ref.clone(),
        ISSUED_NS,
        EXPIRES_NS,
    );
    quote_mock.sign_with(&provider)?;

    let mut quote_eip155 = PaymentQuote::new(
        provider.entity_id().clone(),
        caller.entity_id().clone(),
        CAPABILITY,
        Some(hex::encode(blake3::hash(b"fixture-input").as_bytes())),
        req_eip155.clone(),
        registry_ref.clone(),
        ISSUED_NS,
        EXPIRES_NS,
    );
    quote_eip155.sign_with(&provider)?;

    let mut quote_unknowns = quote_mock.clone();
    quote_unknowns.extra = BTreeMap::from([
        ("aa_sorts_first".to_string(), json!("esc\"ape\\é")),
        (
            "zz_future_field".to_string(),
            json!({"nested": [1, true, null]}),
        ),
    ]);
    quote_unknowns.sign_with(&provider)?;

    let mut settlement_ref = SettlementRef::new(
        quote_eip155.quote_id.clone(),
        settle_eip155.clone(),
        VerifierRef {
            identity: None,
            endpoint: "mock".into(),
        },
        SETTLED_NS,
        provider.entity_id().clone(),
    );
    settlement_ref.sign_with(&provider)?;

    let mut verification_1 = VerificationEvent {
        object: TAG_PAYMENT_VERIFICATION.to_string(),
        quote_id: quote_eip155.quote_id.clone(),
        transaction: Some(settle_eip155.view().transaction.clone()),
        tier: VerificationTier::Observed,
        status: VerificationStatus::Verified,
        verifier: VerifierRef {
            identity: None,
            endpoint: "mock".into(),
        },
        prev: None,
        checked_at_ns: CHECKED_1_NS,
        signer: provider.entity_id().clone(),
        signature: None,
        extra: BTreeMap::new(),
    };
    verification_1.sign_with(&provider)?;
    let mut verification_2 = VerificationEvent {
        prev: Some(verification_1.chain_hash()?),
        tier: VerificationTier::Confirmed(1),
        checked_at_ns: CHECKED_2_NS,
        signature: None,
        ..verification_1.clone()
    };
    verification_2.sign_with(&provider)?;

    let idem = IdempotencyScope {
        caller: caller.entity_id().clone(),
        provider: provider.entity_id().clone(),
        capability: CAPABILITY.to_string(),
        quote_id: quote_eip155.quote_id.clone(),
    };
    let mut billing = BillingEvent {
        object: TAG_BILLING_EVENT.to_string(),
        billing_event_id: BillingEvent::derive_id(&idem.key()),
        idempotency_key: idem.key(),
        capability: CAPABILITY.to_string(),
        invocation_id: None,
        quote_id: quote_eip155.quote_id.clone(),
        transaction: Some(settle_eip155.view().transaction.clone()),
        verification_ref: Some(verification_2.chain_hash()?),
        payer: caller.entity_id().clone(),
        payee: provider.entity_id().clone(),
        network: "eip155:84532".to_string(),
        asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".to_string(),
        amount: AtomicAmount::parse("10000")?,
        occurred_at_ns: OCCURRED_NS,
        signature: None,
        extra: BTreeMap::new(),
    };
    billing.sign_with(&provider)?;

    // Terms are unsigned — emit its canonical form directly.
    let terms_canonical = String::from_utf8(canonical_bytes(&terms)?)?;

    // ---- assemble -------------------------------------------------------
    let vectors = json!({
        "description": "Golden vectors for net-payments envelope canonicalization, signing, and x402 byte-preservation. Rust is the source of truth (payments/tests/payments_golden_vectors.rs); the same file drives bindings/node/test/payments_golden_vectors.test.ts, bindings/python/tests/test_payments_golden_vectors.py, and go/payments_golden_vectors_test.go. Adding a case means updating all four verifiers in lockstep. Regenerate: cargo run -p net-payments --example gen_payments_fixtures",
        "x402_spec_pin": "x402-foundation/x402 specs/x402-specification-v2.md @ 087922a5eecc06ea773636b75df205814ba295b5 (2026-05-29)",
        "abi_version_expected": 1,
        "identities": {
            "_note": "fixed test seeds, public fixture data — never reuse outside tests",
            "caller_seed_hex": hex::encode([1u8; 32]),
            "provider_seed_hex": hex::encode([2u8; 32]),
            "registry_signer_seed_hex": hex::encode([3u8; 32]),
            "caller_pub_hex": hex::encode(caller.entity_id().as_bytes()),
            "provider_pub_hex": hex::encode(provider.entity_id().as_bytes()),
            "registry_signer_pub_hex": hex::encode(registry_signer.entity_id().as_bytes()),
        },
        "envelopes": [
            {
                "name": "pricing_terms",
                "object": "net.pricing.terms@1",
                "canonical": terms_canonical,
                "signed_payload": serde_json::Value::Null,
                "signer_hex": serde_json::Value::Null,
                "signature_hex": serde_json::Value::Null,
            },
            envelope_entry("payment_quote_mock", &quote_mock, &[
                ("quote_id", json!(quote_mock.quote_id)),
                ("terms_hash", json!(quote_mock.terms_hash)),
            ])?,
            envelope_entry("payment_quote_eip155", &quote_eip155, &[
                ("quote_id", json!(quote_eip155.quote_id)),
                ("terms_hash", json!(quote_eip155.terms_hash)),
            ])?,
            envelope_entry("payment_quote_with_unknowns", &quote_unknowns, &[])?,
            envelope_entry("asset_registry_default", &registry, &[
                ("registry_ref_version", json!(registry_ref.version)),
                ("registry_ref_hash", json!(registry_ref.hash)),
            ])?,
            envelope_entry("settlement_ref_eip155", &settlement_ref, &[])?,
            envelope_entry("verification_event_observed", &verification_1, &[
                ("chain_hash", json!(verification_1.chain_hash()?)),
            ])?,
            envelope_entry("verification_event_confirmed", &verification_2, &[
                ("chain_hash", json!(verification_2.chain_hash()?)),
            ])?,
            envelope_entry("billing_event", &billing, &[
                ("idempotency_key", json!(billing.idempotency_key)),
            ])?,
        ],
        "x402_byte_preservation": [
            {
                "name": "payment_requirements_v2",
                "file": "fixtures/x402/v2.0/payment_requirements.json",
                "base64": BASE64.encode(&req_eip155_bytes),
                "embedded_in": "payment_quote_eip155",
                "envelope_field": "requirements",
            },
            {
                "name": "payment_payload_v2",
                "file": "fixtures/x402/v2.0/payment_payload.json",
                "base64": BASE64.encode(&payload_eip155_bytes),
                "embedded_in": serde_json::Value::Null,
                "envelope_field": serde_json::Value::Null,
            },
            {
                "name": "settlement_response_v2",
                "file": "fixtures/x402/v2.0/settlement_response.json",
                "base64": BASE64.encode(&settle_eip155_bytes),
                "embedded_in": "settlement_ref_eip155",
                "envelope_field": "settlement",
            },
        ],
        "caip_vectors": [
            {"input": "eip155:8453", "kind": "chain", "valid": true},
            {"input": "eip155:84532", "kind": "chain", "valid": true},
            {"input": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp", "kind": "chain", "valid": true},
            {"input": "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1", "kind": "chain", "valid": true},
            {"input": "mock:net", "kind": "chain", "valid": true},
            {"input": "EIP155:8453", "kind": "chain", "valid": false},
            {"input": "eip155", "kind": "chain", "valid": false},
            {"input": "eip155:", "kind": "chain", "valid": false},
            {"input": "eip155:8453 ", "kind": "chain", "valid": false},
            {"input": "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", "kind": "asset", "valid": true},
            {"input": "mock:net/token:musd", "kind": "asset", "valid": true},
            {"input": "eip155:1/erc721:0xb47e3cd837dDF8e4c57f05d70ab865de6e193bbb/771769", "kind": "asset", "valid": true},
            {"input": "eip155:8453/erc20", "kind": "asset", "valid": false},
            {"input": "eip155:8453/ERC20:0x0", "kind": "asset", "valid": false},
            {"input": "eip155:8453", "kind": "asset", "valid": false},
        ],
        "caip_confusion_note": "distinct-but-confusable pairs: ids compare exact and case-sensitive; equivalence is registry policy, never string normalization",
        "caip_confusion_pairs": [
            ["eip155:8453/erc20:0xabc", "eip155:8453/erc20:0xABC"],
            ["eip155:1/erc20:0xabc", "eip155:8453/erc20:0xabc"],
            ["mock:net/token:musd", "mock:net/token:musd/1"],
            // The P1 mainnet/testnet trap: the same contract address on
            // Base vs Base Sepolia is two different assets.
            [
                "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "eip155:84532/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            ],
            // Solana mainnet vs devnet genesis references.
            [
                "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            ],
        ],
        "atomic_amount_vectors": [
            {"input": "0", "valid": true},
            {"input": "10000", "valid": true},
            {"input": "340282366920938463463374607431768211455", "valid": true},
            {"input": "340282366920938463463374607431768211456", "valid": false},
            {"input": "", "valid": false},
            {"input": "01", "valid": false},
            {"input": "-1", "valid": false},
            {"input": "+1", "valid": false},
            {"input": "1.0", "valid": false},
            {"input": "1e6", "valid": false},
            {"input": " 1", "valid": false},
            {"input": "1_000", "valid": false},
        ],
        "decimals_vectors": [
            {"network": "mock:net", "asset": "musd", "declared_decimals": 6, "valid": true},
            {"network": "mock:net", "asset": "musd", "declared_decimals": null, "valid": true},
            {"network": "mock:net", "asset": "musd", "declared_decimals": 18, "valid": false},
            {"network": "mock:net", "asset": "not-in-registry", "declared_decimals": null, "valid": false},
        ],
    });

    let out_path = out_dir.join("payment_vectors.json");
    let mut bytes = serde_json::to_vec_pretty(&vectors)?;
    bytes.push(b'\n');
    std::fs::write(&out_path, &bytes)?;
    println!("wrote {}", out_path.display());
    Ok(())
}
