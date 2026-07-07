//! The `exact` scheme on `solana` networks: a partially-signed SPL
//! `TransferChecked` transaction, presented as an opaque base64 blob
//! (spec-pinned shape: `payload = {"transaction": "<base64>"}`, the
//! facilitator named by `requirements.extra.feePayer` sponsors fees and
//! completes the signature set).
//!
//! This module builds **documents**, nothing else — and for SVM the
//! document is the [`SvmTransferIntent`]: the structured transfer the
//! wallet is being asked to author. The wallet (KMS / embedded wallet /
//! MPC — behind [`crate::flow::signer::SchemeSigner::sign_svm_transfer`])
//! builds the versioned transaction itself: it holds the key, the SPL
//! libraries, and the RPC connection for the recent blockhash; Net holds
//! none of those, on purpose. The doctrine is unchanged from exact-EVM:
//! the signer sees the *structured* intent — amount, mint, recipient,
//! fee payer — never raw bytes, so a policy-bearing wallet can refuse.
//!
//! What Net **cannot** do here is decode the returned blob to re-verify
//! it matches the intent (that would mean SVM transaction machinery in
//! the money path). The trust chain is honest instead: the wallet is the
//! user's own trusted component authoring exactly the intent it was
//! shown, and the facilitator's `/verify` + the chain itself reject a
//! transaction that doesn't pay `payTo` the quoted amount.
//!
//! Intent fields are **derived from the quoted requirements**, never
//! caller-supplied — the mismatch class `exact_evm::typed_data` has to
//! cross-check away cannot arise by construction.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Error;

/// Spec bound on the optional seller-defined memo (UTF-8 bytes).
const MAX_MEMO_BYTES: usize = 256;

/// The typed transfer the wallet authors a transaction for. String-typed
/// as the wire carries them (base58 addresses, decimal atomic amount).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SvmTransferIntent {
    /// CAIP-2 network (`solana:<genesis-hash-prefix>`).
    pub network: String,
    /// The SPL token mint — `requirements.asset`.
    pub mint: String,
    /// Recipient owner address — `requirements.payTo`.
    pub pay_to: String,
    /// Atomic amount — `requirements.amount`.
    pub amount: String,
    /// The transaction fee payer (typically the facilitator), from
    /// `requirements.extra.feePayer` — spec-required: the payer signs a
    /// transaction it does not pay fees on.
    pub fee_payer: String,
    /// Seller-defined memo (`requirements.extra.memo`), ≤ 256 bytes.
    pub memo: Option<String>,
}

/// Derive the transfer intent from quoted requirements. Everything the
/// wallet sees comes from here; nothing is caller-supplied.
pub fn transfer_intent(requirements: &PaymentRequirements) -> Result<SvmTransferIntent, X402Error> {
    if requirements.scheme != "exact" {
        return Err(X402Error::Invalid(format!(
            "exact-SVM authoring got scheme `{}`",
            requirements.scheme
        )));
    }
    let namespace = requirements.network.split(':').next().unwrap_or_default();
    if namespace != "solana" {
        return Err(X402Error::Invalid(format!(
            "exact-SVM authoring needs a solana network, got `{}`",
            requirements.network
        )));
    }
    let extra = requirements.extra.as_ref().ok_or_else(|| {
        X402Error::Invalid(
            "exact-SVM requirements carry no `extra` — the spec requires extra.feePayer \
             (the facilitator account sponsoring transaction fees)"
                .to_string(),
        )
    })?;
    let fee_payer = extra
        .get("feePayer")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            X402Error::Invalid(
                "requirements.extra.feePayer (spec-required fee sponsor) missing".to_string(),
            )
        })?;
    let memo = match extra.get("memo") {
        None => None,
        Some(Value::String(m)) if m.len() <= MAX_MEMO_BYTES => Some(m.clone()),
        Some(Value::String(m)) => {
            return Err(X402Error::Invalid(format!(
                "requirements.extra.memo is {} bytes; the spec caps it at {MAX_MEMO_BYTES}",
                m.len()
            )))
        }
        Some(_) => {
            return Err(X402Error::Invalid(
                "requirements.extra.memo must be a string".to_string(),
            ))
        }
    };
    Ok(SvmTransferIntent {
        network: requirements.network.clone(),
        mint: requirements.asset.clone(),
        pay_to: requirements.pay_to.clone(),
        amount: requirements.amount.clone(),
        fee_payer: fee_payer.to_string(),
        memo,
    })
}

/// The x402 exact-SVM `payload` object around the wallet's
/// partially-signed transaction. The blob is opaque policy-wise, but a
/// wallet returning something that is not even base64 is a fault worth
/// refusing before it crosses any boundary.
pub fn payload_object(transaction_b64: &str) -> Result<Value, X402Error> {
    if transaction_b64.is_empty() {
        return Err(X402Error::Invalid(
            "exact-SVM wallet returned an empty transaction".to_string(),
        ));
    }
    base64::engine::general_purpose::STANDARD
        .decode(transaction_b64)
        .map_err(|e| {
            X402Error::Invalid(format!(
                "exact-SVM wallet returned a non-base64 transaction: {e}"
            ))
        })?;
    Ok(json!({ "transaction": transaction_b64 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements(extra: Option<Value>) -> PaymentRequirements {
        PaymentRequirements {
            scheme: "exact".into(),
            network: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into(),
            amount: "10000".into(),
            asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            pay_to: "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin".into(),
            max_timeout_seconds: 60,
            extra,
        }
    }

    #[test]
    fn intent_derives_from_requirements_and_requires_fee_payer() {
        let intent = transfer_intent(&requirements(Some(
            json!({ "feePayer": "FaciLitator111111111111111111111111111111111" }),
        )))
        .unwrap();
        assert_eq!(intent.mint, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(
            intent.pay_to,
            "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin"
        );
        assert_eq!(intent.amount, "10000");
        assert_eq!(
            intent.fee_payer,
            "FaciLitator111111111111111111111111111111111"
        );
        assert_eq!(intent.memo, None);

        // The spec requires the fee sponsor: absent extra, absent field,
        // and an empty value all refuse.
        assert!(transfer_intent(&requirements(None)).is_err());
        assert!(transfer_intent(&requirements(Some(json!({})))).is_err());
        assert!(transfer_intent(&requirements(Some(json!({ "feePayer": "" })))).is_err());
    }

    #[test]
    fn memo_passes_through_within_the_spec_bound() {
        let with_memo = requirements(Some(json!({ "feePayer": "F1", "memo": "order #42" })));
        assert_eq!(
            transfer_intent(&with_memo).unwrap().memo.as_deref(),
            Some("order #42")
        );
        let oversized = requirements(Some(json!({ "feePayer": "F1", "memo": "x".repeat(257) })));
        assert!(transfer_intent(&oversized).is_err());
        let wrong_type = requirements(Some(json!({ "feePayer": "F1", "memo": 42 })));
        assert!(transfer_intent(&wrong_type).is_err());
    }

    #[test]
    fn wrong_scheme_or_namespace_refuses() {
        let mut evm = requirements(Some(json!({ "feePayer": "F1" })));
        evm.network = "eip155:8453".into();
        assert!(transfer_intent(&evm).is_err());
        let mut upto = requirements(Some(json!({ "feePayer": "F1" })));
        upto.scheme = "upto".into();
        assert!(transfer_intent(&upto).is_err());
    }

    #[test]
    fn payload_object_is_the_pinned_shape_and_rejects_non_base64() {
        let payload = payload_object("AAECAw==").unwrap();
        assert_eq!(payload, json!({ "transaction": "AAECAw==" }));
        assert!(payload_object("").is_err());
        assert!(payload_object("not!!base64??").is_err());
    }
}
