//! Envelope object tags and version discipline.
//!
//! The `@N` suffix in an object tag IS the version marker. Breaking
//! envelope changes mint `@2`; converters live in the SDK,
//! lossless-or-explicit; endpoints reject unnegotiated versions with the
//! structured [`VersionError::UnsupportedVersion`]; relays forward
//! opaquely. Additive change within a version rides the canonical
//! regime's sorted-key emission (see [`crate::core::canonical`]).

use serde::{Deserialize, Serialize};

/// `net.pricing.terms@1` — accepts[] templates in the capability
/// announcement. Discovery/UX metadata, non-binding until instantiated in
/// a quote.
pub const TAG_PRICING_TERMS: &str = "net.pricing.terms@1";
/// `net.payment.quote@1` — provider-identity-signed envelope over
/// instantiated x402 PaymentRequirements + capability binding.
pub const TAG_PAYMENT_QUOTE: &str = "net.payment.quote@1";
/// `net.settlement.ref@1` — wraps the x402 settlement response + tx hash.
pub const TAG_SETTLEMENT_REF: &str = "net.settlement.ref@1";
/// `net.payment.verification@1` — tiered, chained, immutable.
pub const TAG_PAYMENT_VERIFICATION: &str = "net.payment.verification@1";
/// `net.billing.event@1` — the signed usage record.
pub const TAG_BILLING_EVENT: &str = "net.billing.event@1";
/// `net.payment.dispute@1` — **reserved**: flag-only lifecycle extension.
/// No dispute semantics exist before P5; the constant reserves the name so
/// nothing else squats on it.
pub const TAG_PAYMENT_DISPUTE: &str = "net.payment.dispute@1";

// `net.payment.failure@1` — the failure schematic (structured refusal
// verdict on the `net-failure-schematic` reply header) — is deliberately
// NOT minted here: it is SDK wire vocabulary like `ERR_PAYMENT`, owned by
// `net_sdk::tool_payment::TAG_PAYMENT_FAILURE` (unsigned, additive within
// `@1`, consumers tolerate unknown reasons/fields). Listed for registry
// completeness only.

/// Structured version/tag rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VersionError {
    /// Same object family, unnegotiated version — the structured
    /// `unsupported_version` rejection the plan requires.
    #[error("unsupported_version: {object} got @{got}, this build speaks @{expected}")]
    UnsupportedVersion {
        object: String,
        got: u32,
        expected: u32,
    },
    /// A different object entirely.
    #[error("wrong_object: expected `{expected}`, got `{got}`")]
    WrongObject { expected: String, got: String },
    /// Not parseable as `name@version` at all.
    #[error("malformed_tag: `{0}`")]
    Malformed(String),
}

/// A parsed `name@version` object tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTag<'a> {
    pub name: &'a str,
    pub version: u32,
}

/// Parse a `net.….…@N` tag.
pub fn parse_tag(tag: &str) -> Result<ObjectTag<'_>, VersionError> {
    let (name, version) = tag
        .rsplit_once('@')
        .ok_or_else(|| VersionError::Malformed(tag.to_string()))?;
    if name.is_empty() {
        return Err(VersionError::Malformed(tag.to_string()));
    }
    let version: u32 = version
        .parse()
        .map_err(|_| VersionError::Malformed(tag.to_string()))?;
    Ok(ObjectTag { name, version })
}

/// Decode-time tag check: same family at a different version yields the
/// structured `unsupported_version`; anything else is `wrong_object`.
pub fn ensure_tag(expected: &str, got: &str) -> Result<(), VersionError> {
    if expected == got {
        return Ok(());
    }
    let exp = parse_tag(expected)?;
    let g = parse_tag(got)?;
    if exp.name == g.name {
        // Same object AND version, only spelled differently (e.g. a
        // leading zero: `@01` parses to version 1). Accept it rather than
        // report the nonsensical "unsupported_version: got @1, expected @1".
        if exp.version == g.version {
            return Ok(());
        }
        return Err(VersionError::UnsupportedVersion {
            object: exp.name.to_string(),
            got: g.version,
            expected: exp.version,
        });
    }
    Err(VersionError::WrongObject {
        expected: expected.to_string(),
        got: got.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags() {
        let t = parse_tag("net.payment.quote@1").unwrap();
        assert_eq!(t.name, "net.payment.quote");
        assert_eq!(t.version, 1);
        for bad in ["", "net.payment.quote", "@1", "net.payment.quote@x"] {
            assert!(parse_tag(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn version_mismatch_is_structured_unsupported_version() {
        assert_eq!(ensure_tag(TAG_PAYMENT_QUOTE, TAG_PAYMENT_QUOTE), Ok(()));
        assert_eq!(
            ensure_tag(TAG_PAYMENT_QUOTE, "net.payment.quote@2"),
            Err(VersionError::UnsupportedVersion {
                object: "net.payment.quote".into(),
                got: 2,
                expected: 1,
            })
        );
        assert!(matches!(
            ensure_tag(TAG_PAYMENT_QUOTE, TAG_BILLING_EVENT),
            Err(VersionError::WrongObject { .. })
        ));
    }

    #[test]
    fn a_leading_zero_version_spelling_is_the_same_version() {
        // `@01` parses to version 1, so it must be accepted as TAG@1, not
        // rejected with the nonsensical "got @1, expected @1".
        assert_eq!(
            ensure_tag(TAG_PAYMENT_QUOTE, "net.payment.quote@01"),
            Ok(())
        );
    }

    #[test]
    fn unsupported_version_serializes_with_a_stable_discriminant() {
        let e = VersionError::UnsupportedVersion {
            object: "net.payment.quote".into(),
            got: 2,
            expected: 1,
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "unsupported_version");
    }
}
