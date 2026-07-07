//! x402 v2 `PaymentRequired` — what a resource server demands.
//!
//! On the mesh this object never travels (pricing is announced at
//! discovery; no 402 round-trip). It exists for the **two-way door**:
//! a Net agent paying an external x402 HTTP API receives this in the
//! `PAYMENT-REQUIRED` response header (base64 JSON, per the v2 HTTP
//! transport — all protocol information rides headers).
//!
//! ```json
//! {
//!   "x402Version": 2,
//!   "error": "optional string",
//!   "resource": { "url": "..." },
//!   "accepts": [ { /* PaymentRequirements */ }, ... ],
//!   "extensions": { }
//! }
//! ```

use serde::{Deserialize, Serialize};

use super::requirements::PaymentRequirements;
use super::{X402Error, X402View, X402_VERSION};

/// Parsed view over an x402 v2 `PaymentRequired` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    /// Protocol version — must be 2.
    pub x402_version: u64,
    /// Server-supplied error/context string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The resource being paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<serde_json::Value>,
    /// The payment options; the client picks one it can settle.
    pub accepts: Vec<PaymentRequirements>,
    /// x402 extensions map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
}

impl X402View for PaymentRequired {
    const KIND: &'static str = "PaymentRequired";

    fn validate(&self) -> Result<(), X402Error> {
        if self.x402_version != X402_VERSION {
            return Err(X402Error::UnsupportedX402Version {
                got: self.x402_version,
                expected: X402_VERSION,
            });
        }
        if self.accepts.is_empty() {
            return Err(X402Error::Invalid(
                "PaymentRequired with an empty accepts[] demands nothing payable".into(),
            ));
        }
        for entry in &self.accepts {
            entry.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::X402Carry;

    const FIXTURE: &str = r#"{
  "x402Version": 2,
  "error": "payment required",
  "resource": { "url": "https://api.example.com/paid" },
  "accepts": [{
    "scheme": "exact",
    "network": "eip155:84532",
    "amount": "10000",
    "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
    "maxTimeoutSeconds": 60
  }]
}"#;

    #[test]
    fn parses_and_preserves() {
        let carry: X402Carry<PaymentRequired> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();
        assert_eq!(carry.view().accepts.len(), 1);
        assert_eq!(carry.view().accepts[0].amount, "10000");
        assert_eq!(carry.bytes(), FIXTURE.as_bytes());
    }

    #[test]
    fn rejects_wrong_version_and_empty_accepts() {
        let v1 = FIXTURE.replace("\"x402Version\": 2", "\"x402Version\": 1");
        assert!(X402Carry::<PaymentRequired>::from_bytes(v1.into_bytes()).is_err());
        let empty = r#"{"x402Version":2,"accepts":[]}"#;
        assert!(X402Carry::<PaymentRequired>::from_bytes(empty.as_bytes().to_vec()).is_err());
    }
}
