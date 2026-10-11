//! Shared helpers used by more than one NAPI module.

use napi::bindgen_prelude::*;

/// Pure-logic validation of a BigInt's `(sign_bit, words)`
/// decomposition against "fits losslessly in `u64`." Returns the
/// word on success or a stable error-message string on failure.
/// Extracted from [`bigint_u64`] so it's unit-testable without
/// linking napi FFI — the NAPI binding's test harness can't
/// stand up a Node.js runtime for Drop-impl symbols.
///
/// The rules (one per failure mode):
///
/// - `sign_bit = true` → negative; reject with `"expected
///   non-negative BigInt"`. A raw `BigInt::get_u64()` would
///   bitwise-reinterpret a negative and silently yield a huge
///   u64.
/// - `words.len() == 0` → zero. Some napi runtime versions
///   encode `0n` as an empty word slice rather than `[0]`.
///   Accept it and return 0. A cubic review flagged a strict
///   "len != 1" check as breakage for any API that legitimately
///   passes 0 (node ids, punch_id, seqs, empty-bigint timestamps).
/// - `words.len() > 1` → doesn't fit in a single u64. Reject
///   with `"BigInt value exceeds u64 range"`. Any extra word —
///   even a zero upper word — is treated as oversize because
///   an idiomatic producer wouldn't emit a trailing zero.
pub(crate) fn validate_bigint_u64_parts(
    sign_bit: bool,
    words: &[u64],
) -> std::result::Result<u64, &'static str> {
    if sign_bit {
        return Err("expected non-negative BigInt");
    }
    match words.len() {
        0 => Ok(0),
        1 => Ok(words[0]),
        _ => Err("BigInt value exceeds u64 range"),
    }
}

/// Convert a JS `BigInt` to `u64`, rejecting negatives and values
/// that exceed `u64::MAX`. The napi `get_u64()` tuple is `(signed,
/// value, lossless)`; silently accepting either flag corrupts ids,
/// timestamps, or sequences since none of them are meaningful as
/// negative or truncated.
///
/// Thin wrapper around [`validate_bigint_u64_parts`] that
/// translates the pure-logic error into a napi `Error`.
#[inline]
pub(crate) fn bigint_u64(b: BigInt) -> Result<u64> {
    validate_bigint_u64_parts(b.sign_bit, &b.words)
        .map_err(|msg| Error::from_reason(msg.to_string()))
}

/// A JS-number TTL in seconds, checked: a finite integer in
/// `1..=u32::MAX`. Taking it as `u32` let napi's `ToUint32` wrap it —
/// `-1` became a 136-year enrollment invite, `2 ** 32` one born
/// expired, `1.5` silently `1` (SDK audit 2026-10-11). Pure, so it is
/// testable without a Node host.
pub(crate) fn validate_ttl_seconds(name: &str, value: f64) -> std::result::Result<u64, String> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(format!("{name} must be a whole number of seconds, got {value}"));
    }
    if value < 1.0 || value > f64::from(u32::MAX) {
        return Err(format!("{name} must be in 1..={}, got {value}", u32::MAX));
    }
    Ok(value as u64)
}

#[cfg(test)]
mod tests {
    //! Regression tests for [`validate_bigint_u64_parts`] — the
    //! pure-logic half of `bigint_u64`. See [`bigint_u64`] docs
    //! for why the split exists (napi `Error`'s Drop calls
    //! Node-provided FFI symbols that aren't available in a
    //! standalone `cargo test` binary).
    //!
    //! History: cubic flagged that several NAT-surface call
    //! sites (`peer_nat_type`, `probe_reflex`, `connect_direct`)
    //! bypassed this validation by destructuring
    //! `BigInt::get_u64()` directly, silently accepting negatives
    //! and truncations. All call sites were rewritten to go
    //! through `bigint_u64` → this validator; these tests pin
    //! the validator so the NAT surface can't silently accept
    //! out-of-range ids again.
    use super::validate_bigint_u64_parts;

    #[test]
    fn accepts_positive_single_word() {
        assert_eq!(
            validate_bigint_u64_parts(false, &[0xDEAD_BEEF_CAFE_F00D]),
            Ok(0xDEAD_BEEF_CAFE_F00D),
        );
    }

    #[test]
    fn accepts_zero() {
        assert_eq!(validate_bigint_u64_parts(false, &[0]), Ok(0));
    }

    /// Regression test for a cubic-flagged P1 bug. Some napi
    /// runtime versions encode the JS value `0n` as `sign_bit:
    /// false, words: []` — an empty slice, not `[0]`. The
    /// earlier strict `words.len() != 1` check rejected that as
    /// "exceeds u64 range," which broke every API in the NAPI
    /// surface that legitimately accepts 0 (node ids,
    /// `punch_id`, sequence numbers, empty timestamps). The fix
    /// treats an empty word slice as zero.
    #[test]
    fn accepts_empty_words_as_zero() {
        assert_eq!(
            validate_bigint_u64_parts(false, &[]),
            Ok(0),
            "empty words slice must decode as 0 — napi runtimes \
             that encode `0n` this way would otherwise fail every \
             API call that passes 0 as a BigInt",
        );
    }

    /// Belt-and-braces: an empty slice combined with a negative
    /// sign bit is degenerate (`-0n`), but the sign check fires
    /// first and rejects before we ever reach the length check.
    /// Pin that ordering so a future refactor doesn't accidentally
    /// surface an empty-slice negative as zero.
    #[test]
    fn rejects_empty_words_when_sign_bit_set() {
        assert!(
            validate_bigint_u64_parts(true, &[]).is_err(),
            "negative sign bit must reject regardless of word slice shape",
        );
    }

    #[test]
    fn accepts_u64_max() {
        assert_eq!(validate_bigint_u64_parts(false, &[u64::MAX]), Ok(u64::MAX));
    }

    #[test]
    fn rejects_negative() {
        // A raw `BigInt::get_u64()` would reinterpret this as a
        // huge u64 and silently target the wrong peer. The
        // validator must reject + surface "non-negative".
        let err = validate_bigint_u64_parts(true, &[5]).unwrap_err();
        assert!(
            err.contains("non-negative"),
            "error should mention non-negative; got {err:?}",
        );
    }

    #[test]
    fn rejects_negative_zero() {
        // Degenerate-but-possible shape. Sign-check fires
        // regardless of magnitude.
        assert!(validate_bigint_u64_parts(true, &[0]).is_err());
    }

    #[test]
    fn rejects_value_exceeding_u64_max() {
        // 2^64 + 1 in two-word form. A raw destructure would
        // silently discard words[1] and return 1.
        let err = validate_bigint_u64_parts(false, &[1, 1]).unwrap_err();
        assert!(
            err.contains("exceeds u64 range"),
            "error should mention range; got {err:?}",
        );
    }

    #[test]
    fn rejects_multi_word_even_when_upper_words_zero() {
        // An idiomatic BigInt producer wouldn't emit trailing
        // zero words, but a misbehaving caller might. The
        // validator is strict on oversize: `words.len() > 1`
        // always rejects, even when the extra words are zero.
        // Empty-slice (zero) and single-word paths are handled
        // separately above.
        assert!(validate_bigint_u64_parts(false, &[42, 0, 0]).is_err());
    }

    // SDK audit 2026-10-11: enrollment TTLs were napi-coerced u32s.
    #[test]
    fn ttl_seconds_rejects_what_tou_int32_would_wrap() {
        use super::validate_ttl_seconds as v;
        assert_eq!(v("ttl", 60.0), Ok(60));
        assert_eq!(v("ttl", f64::from(u32::MAX)), Ok(u64::from(u32::MAX)));
        for bad in [-1.0, 0.0, 1.5, 4_294_967_296.0, f64::NAN, f64::INFINITY] {
            assert!(v("ttl", bad).is_err(), "{bad} must be refused");
        }
    }
}
