//! The `exact` scheme on `xrpl` networks: a presigned XRPL `Payment`
//! transaction, presented as an opaque hex blob (pinned shape:
//! `payload = {"signedTxBlob": "<hex>"}` — t54's canonical scheme doc,
//! `xrpl-x402.t54.ai/docs/xrpl-scheme`, retrieved 2026-07-08; there is
//! no upstream `scheme_exact_xrpl.md` at the pinned x402 commit, so the
//! dated t54 doc is the operative pin — see
//! `PAYMENTS_XRPL_ENABLEMENT_PLAN.md` WS-0).
//!
//! This module builds **documents**, nothing else — the document here is
//! the [`XrplPaymentIntent`]: the structured Payment the wallet is being
//! asked to author. The wallet (KMS / embedded wallet / MPC — behind
//! [`crate::flow::signer::SchemeSigner::sign_xrpl_payment`]) serializes
//! and signs the transaction itself: it holds the key, the XRPL
//! serialization machinery, and the account `Sequence` /
//! `LastLedgerSequence` bookkeeping; Net holds none of those, on
//! purpose. Same doctrine as exact-EVM/exact-SVM: the signer sees the
//! *structured* intent — amount, recipient, invoice binding — never raw
//! bytes, so a policy-bearing wallet can refuse.
//!
//! **Quote binding (the pinned replay rule):** `extra.invoiceId` is
//! required, and the wallet must bind it into the transaction as
//! `MemoData = HEX(UTF-8(invoiceId))` or `InvoiceID = SHA256(invoiceId)`
//! — "without invoice binding, a single valid payment could be
//! replayed" (pinned doc). The independent checker binds delivery to it.
//!
//! **Exactness by construction:** the intent has no flags field and no
//! path/`SendMax` representation — partial payments (the classic XRPL
//! delivered-less-than-`Amount` exploit) and cross-currency routing are
//! unrepresentable, not merely validated away. The facilitator enforces
//! the same rules server-side; ours is defense in depth, not delegation.
//!
//! **XRP-only (adopted in writing, 2026-07-08):** IOU values on the
//! ledger are decimal strings, which [`AtomicAmount`]'s integer grammar
//! deliberately rejects. RLUSD waits on the atomic-unit-convention
//! review; an IOU entry (non-`XRP` asset or `extra.issuer`) is a
//! structured refusal here until that review lands.
//!
//! [`AtomicAmount`]: crate::core::units::AtomicAmount

use serde_json::{json, Value};

use crate::core::units::AtomicAmount;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Error;

/// The typed Payment the wallet authors a presigned blob for.
/// String-typed as the wire carries them (classic base58 address,
/// decimal drop amount).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct XrplPaymentIntent {
    /// CAIP-2 network (`xrpl:0` mainnet, `xrpl:1` testnet, `xrpl:2`
    /// devnet).
    pub network: String,
    /// The asset — `"XRP"` only until the IOU amount-domain review.
    pub asset: String,
    /// Recipient classic address — `requirements.payTo`.
    pub pay_to: String,
    /// Atomic amount in drops — `requirements.amount` (integer string).
    pub amount: String,
    /// The quote-binding invoice id — `requirements.extra.invoiceId`
    /// (spec-required). The wallet binds it into the transaction as
    /// `MemoData = hex(invoiceId)` or `InvoiceID = SHA256(invoiceId)`.
    pub invoice_id: String,
    /// Optional `DestinationTag` (`requirements.extra.destinationTag`)
    /// — shared-address merchants disambiguate by tag.
    pub destination_tag: Option<u32>,
    /// Optional `SourceTag` (`requirements.extra.sourceTag`; t54's
    /// default is `804681468` when the demand carries one).
    pub source_tag: Option<u32>,
}

/// Read an optional u32 tag from `extra`.
fn optional_tag(extra: &Value, key: &str) -> Result<Option<u32>, X402Error> {
    match extra.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                X402Error::Invalid(format!("requirements.extra.{key} must be a u32 tag"))
            }),
    }
}

/// Derive the Payment intent from quoted requirements. Everything the
/// wallet sees comes from here; nothing is caller-supplied.
pub fn payment_intent(requirements: &PaymentRequirements) -> Result<XrplPaymentIntent, X402Error> {
    if requirements.scheme != "exact" {
        return Err(X402Error::Invalid(format!(
            "exact-XRPL authoring got scheme `{}`",
            requirements.scheme
        )));
    }
    // CAIP-2 shape, validated here too (callable outside the carry
    // path): a reference-less `xrpl` is not a network anyone settles on.
    let reference = requirements
        .network
        .strip_prefix("xrpl:")
        .unwrap_or_default();
    if reference.is_empty() {
        return Err(X402Error::Invalid(format!(
            "exact-XRPL authoring needs a CAIP-2 xrpl network (`xrpl:<reference>`), got `{}`",
            requirements.network
        )));
    }
    let extra = requirements.extra.as_ref().ok_or_else(|| {
        X402Error::Invalid(
            "exact-XRPL requirements carry no `extra` — the pinned scheme requires \
             extra.invoiceId (the quote-binding invoice)"
                .to_string(),
        )
    })?;
    // XRP-only until the IOU amount-domain review: an IOU asset or an
    // issuer field is a structured refusal, never a silent decimal parse.
    if requirements.asset != "XRP" || extra.get("issuer").is_some() {
        return Err(X402Error::Invalid(format!(
            "exact-XRPL is XRP-only pending the IOU amount-domain review \
             (asset `{}`{}) — see PAYMENTS_XRPL_ENABLEMENT_PLAN.md WS-0",
            requirements.asset,
            if extra.get("issuer").is_some() {
                ", issuer present"
            } else {
                ""
            }
        )));
    }
    // Drops are integer atomic units — the existing strict grammar
    // applies verbatim (no leading zeros / signs / decimals).
    AtomicAmount::parse(&requirements.amount)
        .map_err(|e| X402Error::Invalid(format!("exact-XRPL amount (drops): {e}")))?;
    let invoice_id = extra
        .get("invoiceId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            X402Error::Invalid(
                "requirements.extra.invoiceId (spec-required quote binding) missing".to_string(),
            )
        })?;
    Ok(XrplPaymentIntent {
        network: requirements.network.clone(),
        asset: requirements.asset.clone(),
        pay_to: requirements.pay_to.clone(),
        amount: requirements.amount.clone(),
        invoice_id: invoice_id.to_string(),
        destination_tag: optional_tag(extra, "destinationTag")?,
        source_tag: optional_tag(extra, "sourceTag")?,
    })
}

/// The pinned x402 exact-XRPL `payload` object around the wallet's
/// presigned Payment blob. The blob is opaque policy-wise, but a wallet
/// returning something that is not even hex is a fault worth refusing
/// before it crosses any boundary.
pub fn payload_object(signed_tx_blob_hex: &str) -> Result<Value, X402Error> {
    if signed_tx_blob_hex.is_empty() {
        return Err(X402Error::Invalid(
            "exact-XRPL wallet returned an empty transaction blob".to_string(),
        ));
    }
    hex::decode(signed_tx_blob_hex).map_err(|e| {
        X402Error::Invalid(format!(
            "exact-XRPL wallet returned a non-hex transaction blob: {e}"
        ))
    })?;
    Ok(json!({ "signedTxBlob": signed_tx_blob_hex }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements(extra: Option<Value>) -> PaymentRequirements {
        PaymentRequirements {
            scheme: "exact".into(),
            network: "xrpl:0".into(),
            amount: "1000000".into(),
            asset: "XRP".into(),
            pay_to: "rMerchant1111111111111111111111111".into(),
            max_timeout_seconds: 60,
            extra,
        }
    }

    fn base_extra() -> Value {
        json!({ "invoiceId": "inv-quote-42" })
    }

    #[test]
    fn intent_derives_from_requirements_and_requires_invoice_id() {
        let intent = payment_intent(&requirements(Some(base_extra()))).unwrap();
        assert_eq!(intent.network, "xrpl:0");
        assert_eq!(intent.asset, "XRP");
        assert_eq!(intent.pay_to, "rMerchant1111111111111111111111111");
        assert_eq!(intent.amount, "1000000");
        assert_eq!(intent.invoice_id, "inv-quote-42");
        assert_eq!(intent.destination_tag, None);
        assert_eq!(intent.source_tag, None);

        // The pinned scheme requires the invoice binding: absent extra,
        // absent field, and an empty value all refuse.
        assert!(payment_intent(&requirements(None)).is_err());
        assert!(payment_intent(&requirements(Some(json!({})))).is_err());
        assert!(payment_intent(&requirements(Some(json!({ "invoiceId": "" })))).is_err());
    }

    #[test]
    fn tags_pass_through_and_bad_tags_refuse() {
        let tagged = requirements(Some(json!({
            "invoiceId": "inv-1",
            "destinationTag": 7,
            "sourceTag": 804681468u32,
        })));
        let intent = payment_intent(&tagged).unwrap();
        assert_eq!(intent.destination_tag, Some(7));
        assert_eq!(intent.source_tag, Some(804_681_468));

        let bad = requirements(Some(json!({ "invoiceId": "inv-1", "destinationTag": "seven" })));
        assert!(payment_intent(&bad).is_err());
        let oversized =
            requirements(Some(json!({ "invoiceId": "inv-1", "destinationTag": 4294967296u64 })));
        assert!(payment_intent(&oversized).is_err());
    }

    #[test]
    fn iou_entries_refuse_until_the_amount_domain_review() {
        // A non-XRP asset (RLUSD's 40-hex currency code) refuses.
        let mut rlusd = requirements(Some(base_extra()));
        rlusd.asset = "524C555344000000000000000000000000000000".into();
        let err = payment_intent(&rlusd).unwrap_err();
        assert!(err.to_string().contains("XRP-only"), "{err}");

        // Even with asset `XRP`, an issuer field marks an IOU demand.
        let issuer = requirements(Some(json!({
            "invoiceId": "inv-1",
            "issuer": "rIssuer111111111111111111111111111",
        })));
        assert!(payment_intent(&issuer).is_err());

        // Decimal amounts (the IOU domain) refuse through the grammar.
        let mut decimal = requirements(Some(base_extra()));
        decimal.amount = "0.01".into();
        assert!(payment_intent(&decimal).is_err());
    }

    #[test]
    fn wrong_scheme_or_namespace_refuses() {
        let mut evm = requirements(Some(base_extra()));
        evm.network = "eip155:8453".into();
        assert!(payment_intent(&evm).is_err());
        let mut upto = requirements(Some(base_extra()));
        upto.scheme = "upto".into();
        assert!(payment_intent(&upto).is_err());
        for bad in ["xrpl", "xrpl:"] {
            let mut no_ref = requirements(Some(base_extra()));
            no_ref.network = bad.into();
            assert!(payment_intent(&no_ref).is_err(), "`{bad}` must be refused");
        }
    }

    #[test]
    fn payload_object_is_the_pinned_shape_and_rejects_non_hex() {
        let payload = payload_object("1200002280000000").unwrap();
        assert_eq!(payload, json!({ "signedTxBlob": "1200002280000000" }));
        assert!(payload_object("").is_err());
        assert!(payload_object("not-hex!!").is_err());
    }
}
