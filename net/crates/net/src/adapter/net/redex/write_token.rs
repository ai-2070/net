//! `WriteToken` — typed handle to a specific write on a specific
//! origin's chain. Returned by event-ingest paths; consumed by
//! read-your-writes wait primitives.
//!
//! The substrate uses a 64-bit `origin_hash` throughout (see
//! `identity::entity::EntityKeypair::origin_hash`). An earlier draft
//! of the Dataforts plan speculated a 32-byte origin; the substrate
//! shape wins because every causal-chain primitive already keys on
//! `u64`.

use std::fmt;
use std::str::FromStr;

/// Address of a write — origin (which chain) + seq (which event on
/// that chain). Round-trips through every binding as a typed value.
///
/// `WriteToken` is opaque to callers: it exists to be passed back
/// into a wait / read primitive. The fields are public so the FFI
/// layer can encode/decode without going through this crate's serde,
/// but bindings are expected to treat them as a single unit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WriteToken {
    /// 64-bit hash of the entity whose chain this write landed on
    /// (`EntityKeypair::origin_hash`).
    pub origin_hash: u64,
    /// Per-chain monotonic sequence assigned by `RedexFile::append`.
    pub seq: u64,
}

impl WriteToken {
    /// Construct a token from its components. The caller is
    /// responsible for ensuring `origin_hash` matches the chain
    /// `seq` was assigned on; mixing the two yields a token that
    /// no `wait_for_token` impl will satisfy.
    pub const fn new(origin_hash: u64, seq: u64) -> Self {
        Self { origin_hash, seq }
    }
}

/// `<origin_hex>:<seq>` — chosen for grep-ability against the
/// `causal:<hex>:<seq>` reserved-prefix tag shape. Stable wire
/// form for log/CLI surfaces; bindings serialise the struct
/// directly instead.
impl fmt::Display for WriteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}:{}", self.origin_hash, self.seq)
    }
}

/// Errors returned by [`WriteToken::from_str`].
#[derive(Debug, PartialEq, Eq)]
pub enum WriteTokenParseError {
    /// Input did not contain the `:` that separates origin from seq.
    MissingSeparator,
    /// Origin portion was not 16 lowercase hex characters.
    BadOrigin,
    /// Seq portion did not parse as a decimal `u64`.
    BadSeq,
}

impl fmt::Display for WriteTokenParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => f.write_str("expected `<origin_hex>:<seq>`"),
            Self::BadOrigin => f.write_str("origin must be 16 lowercase hex chars"),
            Self::BadSeq => f.write_str("seq must be a decimal u64"),
        }
    }
}

impl std::error::Error for WriteTokenParseError {}

impl FromStr for WriteToken {
    type Err = WriteTokenParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (origin_str, seq_str) = s
            .split_once(':')
            .ok_or(WriteTokenParseError::MissingSeparator)?;
        if origin_str.len() != 16 {
            return Err(WriteTokenParseError::BadOrigin);
        }
        let origin_hash =
            u64::from_str_radix(origin_str, 16).map_err(|_| WriteTokenParseError::BadOrigin)?;
        let seq: u64 = seq_str.parse().map_err(|_| WriteTokenParseError::BadSeq)?;
        Ok(Self::new(origin_hash, seq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_pads_origin_to_16_hex() {
        let token = WriteToken::new(0xDEAD_BEEF, 42);
        assert_eq!(token.to_string(), "00000000deadbeef:42");
    }

    #[test]
    fn display_round_trips_via_from_str() {
        let token = WriteToken::new(0x0123_4567_89AB_CDEF, 12345);
        let parsed: WriteToken = token.to_string().parse().unwrap();
        assert_eq!(parsed, token);
    }

    #[test]
    fn from_str_rejects_missing_separator() {
        assert_eq!(
            "deadbeef".parse::<WriteToken>(),
            Err(WriteTokenParseError::MissingSeparator)
        );
    }

    #[test]
    fn from_str_rejects_short_origin() {
        assert_eq!(
            "deadbeef:1".parse::<WriteToken>(),
            Err(WriteTokenParseError::BadOrigin)
        );
    }

    #[test]
    fn from_str_rejects_non_hex_origin() {
        assert_eq!(
            "zzzzzzzzzzzzzzzz:1".parse::<WriteToken>(),
            Err(WriteTokenParseError::BadOrigin)
        );
    }

    #[test]
    fn from_str_rejects_bad_seq() {
        assert_eq!(
            "0000000000000001:-1".parse::<WriteToken>(),
            Err(WriteTokenParseError::BadSeq)
        );
    }

    #[test]
    fn equality_is_componentwise() {
        let a = WriteToken::new(1, 2);
        let b = WriteToken::new(1, 2);
        let c = WriteToken::new(1, 3);
        let d = WriteToken::new(2, 2);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn token_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<WriteToken>();
    }
}
