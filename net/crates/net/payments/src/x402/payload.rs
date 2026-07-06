//! x402 v2 `PaymentPayload` — the client-signed payment authorization.
//!
//! Shape per the pinned v2 spec:
//!
//! ```json
//! {
//!   "x402Version": 2,
//!   "resource": { "url": "..." },
//!   "accepted": { /* the PaymentRequirements the client accepted */ },
//!   "payload": { /* scheme-specific, e.g. EIP-3009 signature+authorization */ },
//!   "extensions": { }
//! }
//! ```
//!
//! There is **no separate Net intent object** — this payload travels in
//! the invocation envelope, byte-preserved. Binding of payload to
//! requirements is x402-internal (scheme-level), and that's the point:
//! Net's quote binds to the requirements; the scheme binds the payment to
//! them.

use serde::{Deserialize, Serialize};

use super::requirements::PaymentRequirements;
use super::{X402Carry, X402Error, X402View, X402_VERSION};

/// Parsed view over an x402 v2 `PaymentPayload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    /// Protocol version — must be 2.
    pub x402_version: u64,
    /// Optional echo of the resource being paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<serde_json::Value>,
    /// The `PaymentRequirements` entry the client accepted, echoed back.
    pub accepted: PaymentRequirements,
    /// Scheme-specific payment authorization (opaque to Net; the scheme
    /// binds it to the accepted requirements).
    pub payload: serde_json::Value,
    /// x402 extensions map (consumed for interop only — never a substitute
    /// for Net identity, consent, or billing semantics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
}

impl X402View for PaymentPayload {
    const KIND: &'static str = "PaymentPayload";

    fn validate(&self) -> Result<(), X402Error> {
        if self.x402_version != X402_VERSION {
            return Err(X402Error::UnsupportedX402Version {
                got: self.x402_version,
                expected: X402_VERSION,
            });
        }
        self.accepted.validate()?;
        if !self.payload.is_object() {
            return Err(X402Error::Invalid(
                "PaymentPayload.payload must be a scheme-specific object".into(),
            ));
        }
        Ok(())
    }
}

impl X402Carry<PaymentPayload> {
    /// The **encoding-agnostic** replay identity of this payment. Byte
    /// preservation ([`X402Carry::content_hash`]) exists so signatures
    /// survive round-trips untouched; it is the wrong key for the engine's
    /// "one payload → one quote" replay guard, because two byte-different
    /// JSON encodings of the *same* authorization (whitespace, key order)
    /// hash differently and would each be admitted. Keying on the
    /// canonical payload — which carries the scheme's nonce/authorization —
    /// collapses those to one identity, so a re-encoded resubmission
    /// cannot double-serve. Fails closed: an uncanonicalizable payload
    /// (e.g. a float smuggled into the scheme object) yields an error the
    /// caller turns into a rejection rather than a byte-keyed fallback.
    pub fn replay_key(&self) -> Result<String, X402Error> {
        let bytes = crate::core::canonical::canonical_bytes(self.view())
            .map_err(|e| X402Error::Invalid(format!("payload not canonicalizable: {e}")))?;
        Ok(hex::encode(blake3::hash(&bytes).as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::X402Carry;

    const FIXTURE: &str = r#"{
  "x402Version": 2,
  "accepted": {
    "scheme": "exact",
    "network": "eip155:84532",
    "amount": "10000",
    "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
    "maxTimeoutSeconds": 60
  },
  "payload": {
    "signature": "0xdeadbeef",
    "authorization": {
      "from": "0xPayer", "to": "0xPayee", "value": "10000",
      "validAfter": "1740672089", "validBefore": "1740672154",
      "nonce": "0xf3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480"
    }
  }
}"#;

    #[test]
    fn parses_and_validates_v2_payload() {
        let carry: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();
        assert_eq!(carry.view().x402_version, 2);
        assert_eq!(carry.view().accepted.amount, "10000");
        assert_eq!(carry.bytes(), FIXTURE.as_bytes());
    }

    #[test]
    fn rejects_wrong_version() {
        let v1 = FIXTURE.replace("\"x402Version\": 2", "\"x402Version\": 1");
        let err = X402Carry::<PaymentPayload>::from_bytes(v1.into_bytes()).unwrap_err();
        assert_eq!(
            err,
            X402Error::UnsupportedX402Version {
                got: 1,
                expected: 2
            }
        );
    }

    #[test]
    fn rejects_non_object_scheme_payload() {
        let bad = FIXTURE.replace(
            "\"payload\": {\n    \"signature\": \"0xdeadbeef\",",
            "\"payload\": \"oops\", \"ignored\": {\"signature\": \"0xdeadbeef\",",
        );
        assert!(X402Carry::<PaymentPayload>::from_bytes(bad.into_bytes()).is_err());
    }

    /// M2: the replay key ignores serialization. Two byte-different
    /// encodings of the same authorization must collapse to one replay
    /// identity even though their preserved-byte content hashes differ.
    #[test]
    fn replay_key_is_encoding_agnostic() {
        // A re-encoding of FIXTURE: keys reordered inside `authorization`
        // and `accepted`, extra whitespace — identical logical content.
        const REENCODED: &str = r#"{
  "payload": {
    "authorization": {
      "nonce": "0xf3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480",
      "validBefore": "1740672154", "validAfter": "1740672089",
      "value": "10000", "to": "0xPayee", "from": "0xPayer"
    },
    "signature": "0xdeadbeef"
  },
  "accepted": {
    "maxTimeoutSeconds": 60,
    "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
    "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    "amount": "10000", "network": "eip155:84532", "scheme": "exact"
  },
  "x402Version": 2
}"#;

        let a: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();
        let b: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(REENCODED.as_bytes().to_vec()).unwrap();

        // Preserved bytes differ — so the old byte-keyed index would have
        // treated these as two distinct payloads.
        assert_ne!(a.content_hash(), b.content_hash());
        // But the canonical replay identity is one and the same.
        assert_eq!(a.replay_key().unwrap(), b.replay_key().unwrap());
    }
}
