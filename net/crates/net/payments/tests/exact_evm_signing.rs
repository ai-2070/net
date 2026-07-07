//! P1 WS2 conformance: EIP-3009 typed-data authoring + the dev signer.
//!
//! The dev signer exists to prove the authoring path end to end before
//! testnet: the typed-data document is built from the quoted
//! requirements, the EIP-712 digest is computed per spec, the secp256k1
//! signature recovers to the signer's address, and the assembled x402
//! payload validates as a carry. The production path swaps
//! `DevLocalSigner` for `ExternalSigner` with zero authoring changes.
#![cfg(feature = "unsafe-dev-signer")]

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::RegistryRef;
use net_payments::flow::exact_evm_authorization_for_quote;
use net_payments::flow::signer::dev::DevLocalSigner;
use net_payments::flow::signer::{ExternalSigner, SchemeSigner};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::schemes::exact_evm;
use net_payments::x402::X402Carry;

fn base_sepolia_requirements() -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: "eip155:84532".into(),
        amount: "10000".into(),
        asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
        pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({ "name": "USDC", "version": "2" })),
    })
    .expect("author")
}

fn quote_for(requirements: X402Carry<PaymentRequirements>) -> PaymentQuote {
    let provider = EntityKeypair::generate();
    let caller = EntityKeypair::generate();
    let mut quote = PaymentQuote::new(
        provider.entity_id().clone(),
        caller.entity_id().clone(),
        "42/paid-tool",
        None,
        requirements,
        RegistryRef {
            version: "net-default-0".into(),
            hash: "aa".into(),
        },
        1_740_672_000_000_000_000,
        1_740_672_060_000_000_000,
    );
    quote.sign_with(&provider).expect("sign");
    quote
}

#[tokio::test]
async fn the_dev_signer_signs_recoverably_and_deterministically() {
    let signer = DevLocalSigner::from_secret([7u8; 32]).expect("signer");
    let quote = quote_for(base_sepolia_requirements());
    let auth = exact_evm_authorization_for_quote(&quote, &signer.address());
    let typed = exact_evm::typed_data(quote.requirements.view(), &auth).expect("typed data");

    let sig_hex = signer.sign_typed_data(&typed).await.expect("sign");
    let sig_bytes = hex::decode(sig_hex.strip_prefix("0x").expect("0x")).expect("hex");
    assert_eq!(sig_bytes.len(), 65, "r||s||v");
    let v = sig_bytes[64];
    assert!(v == 27 || v == 28, "legacy recovery byte, got {v}");

    // RFC 6979: same document, same signature — retries re-present the
    // identical authorization.
    assert_eq!(
        signer.sign_typed_data(&typed).await.expect("sign again"),
        sig_hex
    );

    // The signature recovers to the signer's address over the EIP-712
    // digest — what the token contract will check on-chain.
    let digest = DevLocalSigner::eip712_digest(&typed).expect("digest");
    let signature = k256::ecdsa::Signature::from_slice(&sig_bytes[..64]).expect("sig");
    let recovery = k256::ecdsa::RecoveryId::from_byte(v - 27).expect("recid");
    let recovered = k256::ecdsa::VerifyingKey::recover_from_prehash(&digest, &signature, recovery)
        .expect("recover");
    let pubkey = recovered.to_encoded_point(false);
    let hash = {
        use sha3::{Digest, Keccak256};
        let mut hasher = Keccak256::new();
        hasher.update(&pubkey.as_bytes()[1..]);
        hasher.finalize()
    };
    let recovered_address = format!("0x{}", hex::encode(&hash[12..]));
    assert_eq!(
        recovered_address.to_lowercase(),
        signer.address().to_lowercase(),
        "the authorization must be attributable to the payer"
    );
}

#[tokio::test]
async fn the_authored_payload_is_a_valid_carry_with_quote_bound_fields() {
    let signer = DevLocalSigner::from_secret([9u8; 32]).expect("signer");
    let quote = quote_for(base_sepolia_requirements());
    let auth = exact_evm_authorization_for_quote(&quote, &signer.address());

    // Window derives from the quote's authoritative timestamps.
    assert_eq!(auth.valid_after, 1_740_672_000 - 60);
    assert_eq!(auth.valid_before, 1_740_672_060);
    // Nonce derives from the quote: same quote → same nonce; a second
    // quote never collides.
    let again = exact_evm_authorization_for_quote(&quote, &signer.address());
    assert_eq!(auth.nonce, again.nonce);
    let other_quote = quote_for(base_sepolia_requirements());
    assert_ne!(
        auth.nonce,
        exact_evm_authorization_for_quote(&other_quote, &signer.address()).nonce
    );

    let typed = exact_evm::typed_data(quote.requirements.view(), &auth).expect("typed data");
    let signature = signer.sign_typed_data(&typed).await.expect("sign");
    let carry = X402Carry::<PaymentPayload>::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: exact_evm::payload_object(&auth, &signature),
        extensions: None,
    })
    .expect("payload carry validates");

    let view = carry.view();
    assert_eq!(view.accepted, *quote.requirements.view());
    assert_eq!(view.payload["signature"], serde_json::json!(signature));
    assert_eq!(view.payload["authorization"]["value"], "10000");
    assert_eq!(
        view.payload["authorization"]["to"],
        "0x209693Bc6afc0C5328bA36FaF03C514EF312287C"
    );
}

#[tokio::test]
async fn the_external_signer_shape_carries_the_same_authoring_path() {
    // The production seam: the "KMS" here is a closure that inspects the
    // typed document (a policy-bearing signer can refuse) and delegates
    // to a dev key — proving ExternalSigner and DevLocalSigner are
    // interchangeable to the authoring code.
    let inner = Arc::new(DevLocalSigner::from_secret([11u8; 32]).expect("signer"));
    let inner_for_closure = inner.clone();
    let external = ExternalSigner::new(inner.address(), move |typed| {
        let signer = inner_for_closure.clone();
        Box::pin(async move {
            // The signer SEES the structured authorization — this is the
            // "no arbitrary signing oracle" property in action.
            assert_eq!(typed["primaryType"], "TransferWithAuthorization");
            assert!(typed["message"]["value"].is_string());
            signer.sign_typed_data(&typed).await
        })
    });

    let quote = quote_for(base_sepolia_requirements());
    let auth = exact_evm_authorization_for_quote(&quote, &external.address());
    let typed = exact_evm::typed_data(quote.requirements.view(), &auth).expect("typed data");
    let via_external = external.sign_typed_data(&typed).await.expect("external");
    let via_local = inner.sign_typed_data(&typed).await.expect("local");
    assert_eq!(via_external, via_local);
}
