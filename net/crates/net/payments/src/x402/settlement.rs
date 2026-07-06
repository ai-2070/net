//! x402 v2 facilitator response shapes: settle + verify.
//!
//! Per the pinned v2 spec, `POST /settle` returns:
//!
//! ```json
//! {
//!   "success": true,
//!   "errorReason": "optional string",
//!   "payer": "optional string",
//!   "transaction": "0x…",
//!   "network": "eip155:84532",
//!   "amount": "10000",
//!   "extensions": { }
//! }
//! ```
//!
//! and `POST /verify` returns `{ isValid, invalidReason?, payer?, extra }`.
//!
//! A facilitator receipt maps to the `observed`/`confirmed(n)` verification
//! tiers only; `final` requires an independent on-chain check of the tx
//! hash — the facilitator never has to be in anyone's trust root.

use serde::{Deserialize, Serialize};

use super::caip::ChainId;
use super::{validate_atomic_str, X402Error, X402View};

/// Parsed view over an x402 v2 settlement response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementResponse {
    /// Whether settlement succeeded.
    pub success: bool,
    /// Failure reason when `success` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// Payer address, when the facilitator resolved one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// Transaction hash/identifier on the settlement network.
    pub transaction: String,
    /// CAIP-2 network the settlement landed on.
    pub network: String,
    /// Amount actually delivered, atomic units as a string. Verification
    /// checks the amount **delivered**, never sent (fees).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    /// x402 extensions map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
}

impl SettlementResponse {
    /// The `network` field parsed as CAIP-2.
    pub fn chain(&self) -> Result<ChainId, X402Error> {
        ChainId::parse(&self.network)
            .map_err(|e| X402Error::Invalid(format!("SettlementResponse.network: {e}")))
    }
}

impl X402View for SettlementResponse {
    const KIND: &'static str = "SettlementResponse";

    fn validate(&self) -> Result<(), X402Error> {
        self.chain()?;
        if self.success && self.transaction.is_empty() {
            return Err(X402Error::Invalid(
                "SettlementResponse.transaction is empty on a successful settle".into(),
            ));
        }
        if let Some(amount) = &self.amount {
            validate_atomic_str(amount, "SettlementResponse.amount")?;
        }
        Ok(())
    }
}

/// Parsed view over an x402 v2 verify response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    /// Whether the payload satisfies the requirements.
    pub is_valid: bool,
    /// Reason when invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    /// Payer address, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// Scheme-specific extra data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

impl X402View for VerifyResponse {
    const KIND: &'static str = "VerifyResponse";

    fn validate(&self) -> Result<(), X402Error> {
        if !self.is_valid && self.invalid_reason.is_none() {
            return Err(X402Error::Invalid(
                "VerifyResponse invalid without an invalidReason".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::X402Carry;

    #[test]
    fn settle_response_round_trips() {
        let json = r#"{"success":true,"transaction":"0xabc123","network":"eip155:84532","amount":"10000"}"#;
        let carry: X402Carry<SettlementResponse> =
            X402Carry::from_bytes(json.as_bytes().to_vec()).unwrap();
        assert!(carry.view().success);
        assert_eq!(carry.view().transaction, "0xabc123");
        assert_eq!(carry.bytes(), json.as_bytes());
    }

    #[test]
    fn successful_settle_requires_a_transaction() {
        let json = r#"{"success":true,"transaction":"","network":"eip155:84532"}"#;
        assert!(X402Carry::<SettlementResponse>::from_bytes(json.as_bytes().to_vec()).is_err());
    }

    #[test]
    fn failed_settle_may_omit_transaction() {
        let json =
            r#"{"success":false,"errorReason":"insufficient_funds","transaction":"","network":"eip155:84532"}"#;
        let carry: X402Carry<SettlementResponse> =
            X402Carry::from_bytes(json.as_bytes().to_vec()).unwrap();
        assert!(!carry.view().success);
    }

    #[test]
    fn invalid_verify_requires_a_reason() {
        let bad = r#"{"isValid":false}"#;
        assert!(X402Carry::<VerifyResponse>::from_bytes(bad.as_bytes().to_vec()).is_err());
        let ok = r#"{"isValid":false,"invalidReason":"wrong_amount"}"#;
        assert!(X402Carry::<VerifyResponse>::from_bytes(ok.as_bytes().to_vec()).is_ok());
    }
}
