//! Verbatim x402 v2 structures and the byte-preserving carry.
//!
//! Everything that parses or interprets x402 lives in this module — all
//! spec churn is quarantined here. The load-bearing type is [`X402Carry`]:
//! it holds the **original bytes** of an x402 JSON document alongside a
//! validated, read-only parsed view. Net envelopes embed the carry; the
//! carry serializes as base64 of the original bytes, so no serializer in
//! any language can accidentally re-encode the x402 JSON (re-serializing
//! x402 through Net types is the envelope-drift bug class, and a rejected
//! PR per the review invariant).
//!
//! Pinned spec revision: `specs/x402-specification-v2.md` at
//! x402-foundation/x402 commit `087922a5eecc06ea773636b75df205814ba295b5`.

pub mod caip;
pub mod payload;
pub mod requirements;
pub mod schemes;
pub mod settlement;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Errors from parsing/validating x402 structures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum X402Error {
    #[error("x402 document is not UTF-8 JSON: {0}")]
    NotJson(String),
    #[error("x402 document failed validation: {0}")]
    Invalid(String),
    #[error("unsupported x402Version {got} (this build speaks {expected})")]
    UnsupportedX402Version { got: u64, expected: u64 },
    #[error("x402 carry is not valid base64: {0}")]
    NotBase64(String),
}

/// The x402 major version this build speaks.
pub const X402_VERSION: u64 = 2;

/// A parsed, validated view over an x402 document.
///
/// Views are *interpretation only*: they are deserialized from the carried
/// bytes and never serialized back for signing or transport. Views must
/// tolerate unknown fields (the spec is additive; the bytes are preserved
/// regardless of what the view understands).
pub trait X402View: DeserializeOwned {
    /// Human name for error messages (e.g. `PaymentRequirements`).
    const KIND: &'static str;

    /// Spec-level validation beyond shape: version checks, CAIP parses,
    /// atomic-amount grammar, required non-empty fields.
    fn validate(&self) -> Result<(), X402Error>;
}

/// Byte-preserved x402 document + validated view.
///
/// Invariants:
/// - `bytes` are the exact bytes the document arrived with (or was
///   authored with) — whitespace, key order, and all. They are what
///   signatures cover and what travels.
/// - `view` was parsed from those bytes and passed [`X402View::validate`].
/// - No API re-serializes the view. There is deliberately no
///   `fn to_bytes(&self.view)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X402Carry<T: X402View> {
    bytes: Vec<u8>,
    view: T,
}

impl<T: X402View> X402Carry<T> {
    /// Wrap received bytes: parse, validate, preserve.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, X402Error> {
        let view: T = serde_json::from_slice(&bytes)
            .map_err(|e| X402Error::NotJson(format!("{}: {e}", T::KIND)))?;
        view.validate()?;
        Ok(Self { bytes, view })
    }

    /// Author a new x402 document locally.
    ///
    /// Authoring is the one place serialization through our types is
    /// allowed — the document *originates* here, so these bytes become the
    /// preserved originals from this point on. (The forbidden move is
    /// re-serializing a *received* document.)
    pub fn author(view: &T) -> Result<Self, X402Error>
    where
        T: Serialize + Clone,
    {
        let bytes = serde_json::to_vec(view)
            .map_err(|e| X402Error::Invalid(format!("{}: {e}", T::KIND)))?;
        // Round-trip through from_bytes so authored carries satisfy the
        // exact same invariants as received ones.
        Self::from_bytes(bytes)
    }

    /// The preserved original bytes. This is what signatures cover.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The validated read-only view.
    pub fn view(&self) -> &T {
        &self.view
    }

    /// The document as a JSON string slice (bytes are valid UTF-8 by
    /// construction — serde_json rejected non-UTF-8 input at parse time).
    pub fn as_json_str(&self) -> &str {
        // Validity established in from_bytes; avoid unwrap per lint policy.
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    /// blake3 content hash of the preserved bytes, hex-encoded. Used by
    /// envelope bindings (terms_hash inputs, replay index keys).
    pub fn content_hash(&self) -> String {
        hex::encode(blake3::hash(&self.bytes).as_bytes())
    }
}

// The carry travels inside envelopes as a base64 string of the original
// bytes. Base64 (not nested JSON) so that a binding's JSON encoder can
// never normalize whitespace/key order and silently break signatures —
// byte preservation becomes trivially checkable in every language.
impl<T: X402View> Serialize for X402Carry<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64.encode(&self.bytes))
    }
}

impl<'de, T: X402View> Deserialize<'de> for X402Carry<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let b64 = String::deserialize(deserializer)?;
        let bytes = BASE64
            .decode(b64.as_bytes())
            .map_err(|e| serde::de::Error::custom(X402Error::NotBase64(e.to_string())))?;
        Self::from_bytes(bytes).map_err(serde::de::Error::custom)
    }
}

/// Shared validation helper: the x402 atomic-amount grammar. Amounts are
/// strings of ASCII digits in atomic/minor units — no sign, no decimal
/// point, no exponent, no leading zeros (except `"0"` itself). Ambiguous
/// spellings hard-fail rather than being normalized.
pub(crate) fn validate_atomic_str(s: &str, field: &str) -> Result<(), X402Error> {
    crate::core::units::AtomicAmount::parse(s)
        .map(|_| ())
        .map_err(|e| X402Error::Invalid(format!("{field}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Probe {
        a: u64,
    }
    impl X402View for Probe {
        const KIND: &'static str = "Probe";
        fn validate(&self) -> Result<(), X402Error> {
            if self.a == 0 {
                return Err(X402Error::Invalid("a must be nonzero".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn carry_preserves_exact_bytes_including_whitespace_and_unknown_fields() {
        let original = b"{ \"a\": 7,\n  \"unknown_field\": [1, 2, 3] }".to_vec();
        let carry: X402Carry<Probe> = X402Carry::from_bytes(original.clone()).unwrap();
        assert_eq!(carry.bytes(), &original[..]);
        assert_eq!(carry.view().a, 7);

        // Through envelope serde (base64) and back: byte-identical.
        let b64 = serde_json::to_string(&carry).unwrap();
        let back: X402Carry<Probe> = serde_json::from_str(&b64).unwrap();
        assert_eq!(back.bytes(), &original[..]);
    }

    #[test]
    fn validation_failures_reject_the_carry() {
        let bad = b"{\"a\": 0}".to_vec();
        let err = X402Carry::<Probe>::from_bytes(bad).unwrap_err();
        assert!(matches!(err, X402Error::Invalid(_)));
    }

    #[test]
    fn authored_carries_are_stable() {
        let carry = X402Carry::author(&Probe { a: 3 }).unwrap();
        let again: X402Carry<Probe> = X402Carry::from_bytes(carry.bytes().to_vec()).unwrap();
        assert_eq!(carry, again);
        assert_eq!(carry.content_hash(), again.content_hash());
    }
}
