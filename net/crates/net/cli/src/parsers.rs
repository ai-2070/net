//! Shared value parsers for clap-derived argv structs.
//!
//! Operator-facing flags reuse a small set of parse rules
//! (`0x`-prefixed hex for node / chain / daemon ids, decimal
//! fallback). Defining them once here keeps every subcommand's
//! `value_parser = ...` attribute consistent and avoids drift if
//! we later widen the accepted syntax (underscores, etc.).

/// Parse a `u64` from either decimal or a `0x`-prefixed hex
/// literal. Operators read node ids as hex from snapshot output;
/// rejecting `0xABCD` because clap's default parser only takes
/// decimal is a needless papercut.
pub(crate) fn parse_u64_flexible(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(rest, 16).map_err(|e| format!("invalid hex: {e}"))
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("invalid integer: {e}"))
    }
}

/// `u16` variant of [`parse_u64_flexible`]. Used for fold-kind
/// ids and wire-hash literals on subcommand args.
pub(crate) fn parse_u16_flexible(s: &str) -> Result<u16, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(rest, 16).map_err(|e| format!("invalid hex: {e}"))
    } else {
        s.parse::<u16>()
            .map_err(|e| format!("invalid integer: {e}"))
    }
}

/// Decode a 32-byte literal from hex. Accepts an optional `0x`
/// prefix; rejects any other length. Shared between
/// `--psk-hex` / `--node-pubkey` / `--group-seed` so the error
/// shape matches operator-facing copy across commands.
pub(crate) fn hex_decode_32(s: &str) -> Result<[u8; 32], String> {
    let trimmed = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let bytes = hex::decode(trimmed).map_err(|e| format!("invalid hex: {e}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("expected 32 bytes, got {}", v.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_round_trip() {
        assert_eq!(parse_u64_flexible("42").unwrap(), 42);
    }

    #[test]
    fn hex_lowercase_and_uppercase() {
        assert_eq!(parse_u64_flexible("0xabcd").unwrap(), 0xabcd);
        assert_eq!(parse_u64_flexible("0XABCD").unwrap(), 0xabcd);
    }

    #[test]
    fn whitespace_tolerant() {
        assert_eq!(parse_u64_flexible("  0x10  ").unwrap(), 0x10);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_u64_flexible("").is_err());
        assert!(parse_u64_flexible("0xzz").is_err());
        assert!(parse_u64_flexible("abc").is_err());
    }

    #[test]
    fn u16_decimal_and_hex_round_trip() {
        assert_eq!(parse_u16_flexible("0").unwrap(), 0);
        assert_eq!(parse_u16_flexible("42").unwrap(), 42);
        assert_eq!(parse_u16_flexible("0x0001").unwrap(), 1);
        assert_eq!(parse_u16_flexible("0XBEEF").unwrap(), 0xBEEF);
    }

    #[test]
    fn u16_rejects_overflow_and_garbage() {
        assert!(parse_u16_flexible("65536").is_err());
        assert!(parse_u16_flexible("0x1FFFF").is_err());
        assert!(parse_u16_flexible("").is_err());
        assert!(parse_u16_flexible("not-a-number").is_err());
    }
}
