//! x402 v2 `PaymentRequirements` — the parsed view.
//!
//! Shape per the pinned v2 spec (one entry of `PaymentRequired.accepts[]`):
//!
//! ```json
//! {
//!   "scheme": "exact",
//!   "network": "eip155:84532",
//!   "amount": "10000",
//!   "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
//!   "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
//!   "maxTimeoutSeconds": 60,
//!   "extra": { }
//! }
//! ```
//!
//! `network` is CAIP-2. `asset` is the scheme-scoped asset locator (token
//! contract address, or ISO 4217 code for fiat rails) — Net's registry
//! maps `(network, asset)` to a CAIP-19 id and policy. `amount` is atomic
//! units as a string.

use serde::{Deserialize, Serialize};

use super::caip::ChainId;
use super::{validate_atomic_str, X402Error, X402View};

/// Parsed view over an x402 v2 `PaymentRequirements` object.
///
/// Unknown fields are tolerated (and survive via the carry's preserved
/// bytes). This view is never re-serialized for transport or signing;
/// [`serde::Serialize`] exists solely so providers can *author* templates
/// via [`crate::x402::X402Carry::author`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    /// Payment scheme identifier (e.g. `exact`; `mock` on the mock network).
    pub scheme: String,
    /// CAIP-2 network identifier.
    pub network: String,
    /// Required amount in atomic token units, as a string. The pinned
    /// spec names this `amount`; the widely-deployed x402 servers name it
    /// `maxAmountRequired`. Accept both on input (the carry preserves the
    /// original bytes for signing regardless) so an outbound payment to a
    /// real server whose requirements use `maxAmountRequired` parses
    /// rather than failing with "missing field `amount`".
    #[serde(alias = "maxAmountRequired")]
    pub amount: String,
    /// Scheme-scoped asset locator (token contract address / currency code).
    pub asset: String,
    /// Recipient wallet address or role constant.
    pub pay_to: String,
    /// Maximum time allowed for payment completion. Advisory on the mesh —
    /// the quote envelope's expiry governs (`net.payment.quote@1`).
    pub max_timeout_seconds: u64,
    /// Scheme-specific additional information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

impl PaymentRequirements {
    /// The `network` field parsed as CAIP-2.
    pub fn chain(&self) -> Result<ChainId, X402Error> {
        ChainId::parse(&self.network)
            .map_err(|e| X402Error::Invalid(format!("PaymentRequirements.network: {e}")))
    }
}

impl X402View for PaymentRequirements {
    const KIND: &'static str = "PaymentRequirements";

    fn validate(&self) -> Result<(), X402Error> {
        if self.scheme.is_empty() {
            return Err(X402Error::Invalid(
                "PaymentRequirements.scheme is empty".into(),
            ));
        }
        self.chain()?;
        validate_atomic_str(&self.amount, "PaymentRequirements.amount")?;
        if self.asset.is_empty() {
            return Err(X402Error::Invalid(
                "PaymentRequirements.asset is empty".into(),
            ));
        }
        if self.pay_to.is_empty() {
            return Err(X402Error::Invalid(
                "PaymentRequirements.payTo is empty".into(),
            ));
        }
        if self.max_timeout_seconds == 0 {
            return Err(X402Error::Invalid(
                "PaymentRequirements.maxTimeoutSeconds must be positive".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::X402Carry;

    const FIXTURE: &str = r#"{
  "scheme": "exact",
  "network": "eip155:84532",
  "amount": "10000",
  "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
  "maxTimeoutSeconds": 60,
  "someFutureSpecField": {"nested": true}
}"#;

    #[test]
    fn parses_spec_shape_and_preserves_bytes() {
        let carry: X402Carry<PaymentRequirements> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();
        assert_eq!(carry.view().scheme, "exact");
        assert_eq!(carry.view().network, "eip155:84532");
        assert_eq!(carry.view().amount, "10000");
        assert_eq!(carry.bytes(), FIXTURE.as_bytes());
        assert_eq!(carry.view().chain().unwrap().namespace(), "eip155");
    }

    /// M9: a real x402 server whose requirements name the amount
    /// `maxAmountRequired` (no `amount` key) must still parse — and its
    /// original bytes are preserved untouched for signing.
    #[test]
    fn accepts_max_amount_required_alias() {
        const DEPLOYED: &str = r#"{
  "scheme": "exact",
  "network": "eip155:8453",
  "maxAmountRequired": "10000",
  "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
  "maxTimeoutSeconds": 60
}"#;
        let carry: X402Carry<PaymentRequirements> =
            X402Carry::from_bytes(DEPLOYED.as_bytes().to_vec())
                .expect("maxAmountRequired must parse");
        assert_eq!(carry.view().amount, "10000");
        // Byte preservation: the original key survives for signing.
        assert_eq!(carry.bytes(), DEPLOYED.as_bytes());
    }

    #[test]
    fn rejects_bad_network_amount_and_empty_fields() {
        let cases = [
            (
                r#"{"scheme":"exact","network":"EIP155:1","amount":"1","asset":"a","payTo":"b","maxTimeoutSeconds":60}"#,
                "uppercase network",
            ),
            (
                r#"{"scheme":"exact","network":"eip155:1","amount":"01","asset":"a","payTo":"b","maxTimeoutSeconds":60}"#,
                "leading-zero amount",
            ),
            (
                r#"{"scheme":"exact","network":"eip155:1","amount":"1.5","asset":"a","payTo":"b","maxTimeoutSeconds":60}"#,
                "decimal amount",
            ),
            (
                r#"{"scheme":"exact","network":"eip155:1","amount":"-1","asset":"a","payTo":"b","maxTimeoutSeconds":60}"#,
                "negative amount",
            ),
            (
                r#"{"scheme":"","network":"eip155:1","amount":"1","asset":"a","payTo":"b","maxTimeoutSeconds":60}"#,
                "empty scheme",
            ),
            (
                r#"{"scheme":"exact","network":"eip155:1","amount":"1","asset":"a","payTo":"","maxTimeoutSeconds":60}"#,
                "empty payTo",
            ),
            (
                r#"{"scheme":"exact","network":"eip155:1","amount":"1","asset":"a","payTo":"b","maxTimeoutSeconds":0}"#,
                "zero timeout",
            ),
        ];
        for (json, why) in cases {
            assert!(
                X402Carry::<PaymentRequirements>::from_bytes(json.as_bytes().to_vec()).is_err(),
                "should reject: {why}"
            );
        }
    }
}
