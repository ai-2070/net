//! Cross-language payments golden vectors — the Rust source-of-truth
//! verifier.
//!
//! Loads `tests/cross_lang_payments/payment_vectors.json` (regenerate with
//! `cargo run -p net-payments --example gen_payments_fixtures`) and pins:
//! canonical envelope encoding, signature coverage, hash derivations
//! (quote_id / terms_hash / chain links / billing ids), x402 fixture
//! byte-preservation, CAIP grammar + confusion, atomic-amount grammar,
//! and registry decimals cross-checks.
//!
//! The same fixture drives the Node / Python / Go verifiers. Adding a case
//! means updating all four in lockstep.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use net::adapter::net::identity::EntityKeypair;
use net_payments::core::billing_event::BillingEvent;
use net_payments::core::canonical::SignedEnvelope;
use net_payments::core::canonical::{canonical_bytes, signed_payload_bytes};
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::core::settlement_ref::SettlementRef;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::core::verification::{check_chain, VerificationEvent};
use net_payments::x402::caip::{AssetId, ChainId};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use serde_json::Value;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("cross_lang_payments")
}

fn vectors() -> Value {
    let path = fixture_dir().join("payment_vectors.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixture parses")
}

fn envelope<'a>(vectors: &'a Value, name: &str) -> &'a Value {
    vectors["envelopes"]
        .as_array()
        .expect("envelopes array")
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("envelope vector `{name}` missing"))
}

/// Every envelope's canonical string re-canonicalizes to itself, its
/// signed payload derives by dropping the `signature` key, and the typed
/// decoder (which verifies the signature) accepts it.
#[test]
fn envelopes_are_canonical_signed_and_typed_decodable() {
    let v = vectors();
    for entry in v["envelopes"].as_array().expect("envelopes array") {
        let name = entry["name"].as_str().expect("name");
        let canonical = entry["canonical"].as_str().expect("canonical string");

        // Canonical emission is a fixed point.
        let parsed: Value = serde_json::from_str(canonical).expect("canonical parses");
        let re_emitted = canonical_bytes(&parsed).expect("canonicalizes");
        assert_eq!(
            std::str::from_utf8(&re_emitted).expect("utf8"),
            canonical,
            "{name}: canonical emission drifted"
        );

        // Signed payload = canonical minus the signature key.
        if let Some(expected_payload) = entry["signed_payload"].as_str() {
            let derived = signed_payload_bytes(&parsed).expect("payload derives");
            assert_eq!(
                std::str::from_utf8(&derived).expect("utf8"),
                expected_payload,
                "{name}: signed payload derivation drifted"
            );
        }

        // Typed decode — includes tag checks and signature verification.
        let object = entry["object"].as_str().expect("object tag");
        let bytes = canonical.as_bytes();
        match object {
            "net.pricing.terms@1" => {
                PricingTerms::from_json_bytes(bytes).expect("terms decode");
            }
            "net.payment.quote@1" => {
                let quote = PaymentQuote::from_json_bytes(bytes).expect("quote decode+verify");
                if let Some(expected) = entry["quote_id"].as_str() {
                    assert_eq!(quote.quote_id, expected, "{name}: quote_id drifted");
                }
                if let Some(expected) = entry["terms_hash"].as_str() {
                    assert_eq!(quote.terms_hash, expected, "{name}: terms_hash drifted");
                }
            }
            "net.payment.asset_registry@1" => {
                let registry: AssetRegistry =
                    serde_json::from_slice(bytes).expect("registry decode");
                registry.verify_signature().expect("registry signature");
                let reference = registry.reference().expect("registry ref");
                assert_eq!(
                    reference.hash,
                    entry["registry_ref_hash"].as_str().expect("ref hash"),
                    "{name}: registry ref hash drifted"
                );
            }
            "net.settlement.ref@1" => {
                SettlementRef::from_json_bytes(bytes).expect("settlement ref decode+verify");
            }
            "net.payment.verification@1" => {
                let ev =
                    VerificationEvent::from_json_bytes(bytes).expect("verification decode+verify");
                if let Some(expected) = entry["chain_hash"].as_str() {
                    assert_eq!(
                        ev.chain_hash().expect("chain hash"),
                        expected,
                        "{name}: chain hash drifted"
                    );
                }
            }
            "net.billing.event@1" => {
                let ev = BillingEvent::from_json_bytes(bytes).expect("billing decode+verify");
                assert_eq!(
                    ev.billing_event_id,
                    BillingEvent::derive_id(&ev.idempotency_key),
                    "{name}: billing id derivation drifted"
                );
            }
            other => panic!("{name}: unknown object tag {other}"),
        }
    }
}

/// The two verification events form a valid chain; reordering breaks it.
#[test]
fn verification_chain_links() {
    let v = vectors();
    let e1 = VerificationEvent::from_json_bytes(
        envelope(&v, "verification_event_observed")["canonical"]
            .as_str()
            .expect("canonical")
            .as_bytes(),
    )
    .expect("event 1");
    let e2 = VerificationEvent::from_json_bytes(
        envelope(&v, "verification_event_confirmed")["canonical"]
            .as_str()
            .expect("canonical")
            .as_bytes(),
    )
    .expect("event 2");
    check_chain(&[e1.clone(), e2.clone()]).expect("fixture chain is valid");
    assert!(check_chain(&[e2, e1]).is_err(), "reordered chain must fail");
}

/// The captured x402 v2 fixtures survive the carry byte-identically, and
/// the envelopes embed exactly those bytes.
#[test]
fn x402_fixtures_are_byte_preserved() {
    let v = vectors();
    for entry in v["x402_byte_preservation"]
        .as_array()
        .expect("preservation array")
    {
        let name = entry["name"].as_str().expect("name");
        let file = fixture_dir().join(entry["file"].as_str().expect("file"));
        let file_bytes =
            std::fs::read(&file).unwrap_or_else(|e| panic!("{name}: read {}: {e}", file.display()));
        let b64 = entry["base64"].as_str().expect("base64");
        assert_eq!(
            BASE64.decode(b64).expect("decodes"),
            file_bytes,
            "{name}: fixture file and vector base64 disagree"
        );

        // Round-trip through the typed carry: byte-identical.
        match name {
            "payment_requirements_v2" => {
                let carry: X402Carry<PaymentRequirements> =
                    X402Carry::from_bytes(file_bytes.clone()).expect("carry parses");
                assert_eq!(
                    carry.bytes(),
                    &file_bytes[..],
                    "{name}: carry mutated bytes"
                );
                let through_serde: X402Carry<PaymentRequirements> =
                    serde_json::from_str(&serde_json::to_string(&carry).expect("carry serializes"))
                        .expect("carry round-trips");
                assert_eq!(through_serde.bytes(), &file_bytes[..]);
            }
            "payment_payload_v2" => {
                let carry: X402Carry<net_payments::x402::payload::PaymentPayload> =
                    X402Carry::from_bytes(file_bytes.clone()).expect("carry parses");
                assert_eq!(carry.bytes(), &file_bytes[..]);
            }
            "settlement_response_v2" => {
                let carry: X402Carry<net_payments::x402::settlement::SettlementResponse> =
                    X402Carry::from_bytes(file_bytes.clone()).expect("carry parses");
                assert_eq!(carry.bytes(), &file_bytes[..]);
            }
            other => panic!("unknown preservation vector {other}"),
        }

        // The envelope that embeds this fixture carries the same base64.
        if let Some(embedded_in) = entry["embedded_in"].as_str() {
            let field = entry["envelope_field"].as_str().expect("envelope_field");
            let env: Value = serde_json::from_str(
                envelope(&v, embedded_in)["canonical"]
                    .as_str()
                    .expect("canonical"),
            )
            .expect("envelope parses");
            assert_eq!(
                env[field].as_str().expect("field is a base64 string"),
                b64,
                "{name}: envelope `{embedded_in}.{field}` does not embed the fixture bytes"
            );
        }
    }
}

/// CAIP grammar table + confusion pairs.
#[test]
fn caip_vectors_hold() {
    let v = vectors();
    for entry in v["caip_vectors"].as_array().expect("caip array") {
        let input = entry["input"].as_str().expect("input");
        let valid = entry["valid"].as_bool().expect("valid");
        let ok = match entry["kind"].as_str().expect("kind") {
            "chain" => ChainId::parse(input).is_ok(),
            "asset" => AssetId::parse(input).is_ok(),
            other => panic!("unknown kind {other}"),
        };
        assert_eq!(ok, valid, "CAIP vector `{input}` validity drifted");
    }
    for pair in v["caip_confusion_pairs"].as_array().expect("pairs") {
        let a = pair[0].as_str().expect("a");
        let b = pair[1].as_str().expect("b");
        assert_ne!(a, b, "confusion pair must be textually distinct");
        // Both sides that parse must compare unequal as typed ids too.
        if let (Ok(a), Ok(b)) = (AssetId::parse(a), AssetId::parse(b)) {
            assert_ne!(a, b, "confusable ids must stay distinct");
        }
    }
}

/// Atomic-amount grammar table.
#[test]
fn atomic_amount_vectors_hold() {
    let v = vectors();
    for entry in v["atomic_amount_vectors"].as_array().expect("amount array") {
        let input = entry["input"].as_str().expect("input");
        let valid = entry["valid"].as_bool().expect("valid");
        assert_eq!(
            AtomicAmount::parse(input).is_ok(),
            valid,
            "amount vector `{input}` validity drifted"
        );
    }
}

/// Registry decimals cross-check table, against the default mock registry
/// rebuilt from the fixture's pinned signer seed.
#[test]
fn decimals_vectors_hold() {
    let v = vectors();
    let seed: [u8; 32] = hex::decode(
        v["identities"]["registry_signer_seed_hex"]
            .as_str()
            .expect("seed"),
    )
    .expect("hex")
    .try_into()
    .expect("32 bytes");
    let registry = default_mock_registry(EntityKeypair::from_bytes(seed).entity_id().clone());

    for entry in v["decimals_vectors"].as_array().expect("decimals array") {
        let asset = entry["asset"].as_str().expect("asset");
        let declared = entry["declared_decimals"].as_u64();
        let valid = entry["valid"].as_bool().expect("valid");
        let requirements = PaymentRequirements {
            scheme: "mock".into(),
            network: entry["network"].as_str().expect("network").into(),
            amount: "1".into(),
            asset: asset.into(),
            pay_to: "p".into(),
            max_timeout_seconds: 60,
            extra: declared.map(|d| serde_json::json!({ "decimals": d })),
        };
        assert_eq!(
            registry.check_requirements(&requirements).is_ok(),
            valid,
            "decimals vector asset=`{asset}` declared={declared:?} drifted"
        );
    }
}

/// The `net.payment.failure@1` header tolerance contract: the same
/// bytes every language decides schematic-or-not on. Rust decides
/// through the real [`FailureSchematic::from_header_bytes`]; the fixture
/// pins that each other language's tolerant predicate (decode UTF-8 JSON,
/// accept iff an object tagged with the schematic tag) reaches the same
/// verdict, plus field access + byte-stable re-emission on the accepted
/// ones.
#[test]
fn failure_schematic_vectors_hold() {
    use net_sdk::tool_payment::FailureSchematic;
    let v = vectors();
    let block = &v["failure_schematic_vectors"];
    let tag = block["tag"].as_str().expect("tag");
    for case in block["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("name");
        let bytes = failure_case_bytes(case);
        let accepted = case["accepted"].as_bool().expect("accepted");
        let parsed = FailureSchematic::from_header_bytes(&bytes);
        assert_eq!(
            parsed.is_some(),
            accepted,
            "{name}: tolerance verdict drifted"
        );
        let Some(s) = parsed else { continue };
        assert_eq!(s.object, tag, "{name}: accepted schematic carries the tag");

        // The fixture bytes ARE what a producer emits: re-emission is a
        // fixed point (utf8 cases — the byte case is always a reject).
        if let Some(utf8) = case["header_utf8"].as_str() {
            let re = String::from_utf8(s.to_header_bytes().expect("accepted fits")).unwrap();
            assert_eq!(re, utf8, "{name}: schematic re-emission drifted");
        }

        if let Some(expect) = case.get("expect") {
            assert_eq!(s.stage, expect["stage"].as_str().unwrap(), "{name}: stage");
            assert_eq!(
                s.reason,
                expect["reason"].as_str().unwrap(),
                "{name}: reason"
            );
            assert_eq!(
                s.retryable,
                expect["retryable"].as_bool().unwrap(),
                "{name}: retryable"
            );
            assert_eq!(
                s.funds_moved,
                expect["funds_moved"].as_str().unwrap(),
                "{name}: funds_moved"
            );
            assert_eq!(
                s.prior_payment,
                expect["prior_payment"].as_str().unwrap(),
                "{name}: prior_payment"
            );
            let rec = &expect["recovery"];
            assert_eq!(
                s.recovery.class,
                rec["class"].as_str().unwrap(),
                "{name}: recovery.class"
            );
            assert_eq!(
                s.recovery.actor,
                rec["actor"].as_str().unwrap(),
                "{name}: recovery.actor"
            );
            assert_eq!(
                s.recovery.safe_to_retry,
                rec["safe_to_retry"].as_bool().unwrap(),
                "{name}: safe_to_retry"
            );
            assert_eq!(
                s.recovery.safe_to_requote,
                rec["safe_to_requote"].as_bool().unwrap(),
                "{name}: safe_to_requote"
            );
        }
        if let Some(keys) = case.get("expect_extra_keys").and_then(|k| k.as_array()) {
            for k in keys {
                let k = k.as_str().unwrap();
                assert!(s.extra.contains_key(k), "{name}: extra key `{k}` preserved");
            }
        }
    }
}

/// The header bytes a failure-schematic case decides on: `header_utf8`
/// as UTF-8 text, else `header_base64` decoded (the non-UTF-8 case).
fn failure_case_bytes(case: &Value) -> Vec<u8> {
    match case["header_utf8"].as_str() {
        Some(utf8) => utf8.as_bytes().to_vec(),
        None => BASE64
            .decode(case["header_base64"].as_str().expect("header_base64"))
            .expect("base64 decodes"),
    }
}

/// Unknown fields are preserved, sorted into place, and signature-covered.
#[test]
fn unknown_fields_are_covered() {
    let v = vectors();
    let canonical = envelope(&v, "payment_quote_with_unknowns")["canonical"]
        .as_str()
        .expect("canonical");
    let quote = PaymentQuote::from_json_bytes(canonical.as_bytes()).expect("decodes + verifies");
    assert!(
        !quote.extra.is_empty(),
        "unknown fields must survive decode"
    );

    // Stripping them breaks the signature: they are covered, not cosmetic.
    let mut stripped = quote.clone();
    stripped.extra = BTreeMap::new();
    assert!(stripped.verify_signature().is_err());

    // And the canonical form interleaves them in sorted position: the
    // aa_* unknown key sorts before every known key.
    assert!(
        canonical.starts_with("{\"aa_sorts_first\""),
        "sorted-key canonicalization must interleave unknown fields"
    );
}
