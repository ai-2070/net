//! Atomic/minor-unit amounts: strings on the wire, checked math inside.
//!
//! Matching x402: amounts are strings of atomic units. No floats exist
//! anywhere in this crate's money path; no ambiguous decimal strings are
//! accepted. Display/decimal conversion is registry metadata for UX,
//! never an input to verification.

use serde::{Deserialize, Serialize};

/// Errors from amount parsing and arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    #[error("amount `{0}` is not an atomic-unit string: {1}")]
    Malformed(String, &'static str),
    #[error("amount arithmetic overflowed")]
    Overflow,
}

/// An amount in atomic/minor units.
///
/// Grammar (hard-rejected otherwise, never normalized):
/// - ASCII digits only — no sign, decimal point, exponent, whitespace,
///   underscores, or locale separators
/// - no leading zeros (`"0"` itself is the only string starting with 0)
/// - must fit in u128
///
/// Serializes as the canonical digit string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AtomicAmount(u128);

impl AtomicAmount {
    /// Parse an atomic-unit string under the strict grammar.
    pub fn parse(s: &str) -> Result<Self, AmountError> {
        let err = |reason| AmountError::Malformed(s.to_string(), reason);
        if s.is_empty() {
            return Err(err("empty"));
        }
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err("non-digit character"));
        }
        if s.len() > 1 && s.starts_with('0') {
            return Err(err("leading zero"));
        }
        s.parse::<u128>().map(Self).map_err(|_| err("exceeds u128"))
    }

    /// Construct from a raw integer (authoring side).
    pub fn from_u128(v: u128) -> Self {
        Self(v)
    }

    /// The numeric value.
    pub fn value(&self) -> u128 {
        self.0
    }

    /// The canonical wire string.
    pub fn to_canonical_string(&self) -> String {
        self.0.to_string()
    }

    /// Checked addition — spend counters use this; overflow is an error,
    /// never a wrap.
    pub fn checked_add(&self, other: &Self) -> Result<Self, AmountError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(AmountError::Overflow)
    }

    /// Checked subtraction.
    pub fn checked_sub(&self, other: &Self) -> Result<Self, AmountError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(AmountError::Overflow)
    }
}

impl std::fmt::Display for AtomicAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for AtomicAmount {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_canonical_string())
    }
}

impl<'de> Deserialize<'de> for AtomicAmount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_atomic_strings() {
        assert_eq!(AtomicAmount::parse("0").unwrap().value(), 0);
        assert_eq!(AtomicAmount::parse("10000").unwrap().value(), 10_000);
        assert_eq!(
            AtomicAmount::parse(&u128::MAX.to_string()).unwrap().value(),
            u128::MAX
        );
    }

    #[test]
    fn rejects_every_ambiguous_spelling() {
        for bad in [
            "",
            "01",
            "007",
            "-1",
            "+1",
            "1.0",
            "1e6",
            " 1",
            "1 ",
            "1_000",
            "0x10",
            "١٢٣",
            "340282366920938463463374607431768211456", // u128::MAX + 1
        ] {
            assert!(AtomicAmount::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn checked_math_never_wraps() {
        let max = AtomicAmount::from_u128(u128::MAX);
        let one = AtomicAmount::from_u128(1);
        assert_eq!(max.checked_add(&one), Err(AmountError::Overflow));
        assert_eq!(one.checked_sub(&max), Err(AmountError::Overflow));
        assert_eq!(one.checked_add(&one).unwrap(), AtomicAmount::from_u128(2));
    }

    #[test]
    fn serde_is_the_canonical_string() {
        let a = AtomicAmount::parse("10000").unwrap();
        assert_eq!(serde_json::to_string(&a).unwrap(), "\"10000\"");
        let back: AtomicAmount = serde_json::from_str("\"10000\"").unwrap();
        assert_eq!(back, a);
        // JSON numbers are not amounts.
        assert!(serde_json::from_str::<AtomicAmount>("10000").is_err());
    }
}
