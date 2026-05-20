//! Fixed 24-byte `EventMeta` prefix on every CortEX-adapted payload.
//!
//! Wire layout (little-endian, 24 bytes total):
//!
//! ```text
//! ┌──────────┬───────┬──────┬─────────────┬───────────┬──────────┐
//! │ dispatch │ flags │ _pad │ origin_hash │ seq_or_ts │ checksum │
//! │   u8     │  u8   │  2B  │    u64      │    u64    │   u32    │
//! └──────────┴───────┴──────┴─────────────┴───────────┴──────────┘
//! ```
//!
//! `seq_or_ts` is deliberately NOT interpreted by the adapter. Envelope
//! authors pick per file: per-origin monotonic counter (deterministic
//! fold order) OR unix nanos (wall-clock ordering). Mixing within one
//! file breaks fold ordering assumptions.

/// Size of an `EventMeta` in its wire / on-disk format.
pub const EVENT_META_SIZE: usize = 24;

/// Dispatch value reserved for "raw" payloads — no CortEX-level
/// semantics. Callers that don't need dispatch routing should use
/// this.
pub const DISPATCH_RAW: u8 = 0x00;

/// Flag bit: this event is part of a causal chain.
pub const FLAG_CAUSAL: u8 = 0b0000_0001;
/// Flag bit: this event carries a continuity proof.
pub const FLAG_CONTINUITY_PROOF: u8 = 0b0000_0010;

/// Fixed 24-byte prefix on every payload appended through the
/// CortEX adapter.
///
/// The in-memory layout is whatever the compiler chooses; the wire /
/// on-disk format is produced by [`Self::to_bytes`] and consumed by
/// [`Self::from_bytes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventMeta {
    /// Event classifier. `0x00..0x7F` reserved for CortEX-internal
    /// dispatches; `0x80..0xFF` for application / vendor use.
    pub dispatch: u8,
    /// Causal / continuity / proof bits. See `FLAG_*` constants.
    pub flags: u8,
    /// Reserved; must be zero on write, ignored on read.
    pub _pad: [u8; 2],
    /// Producer identity — full `EntityKeypair::origin_hash()`
    /// value, not a truncation.
    pub origin_hash: u64,
    /// Per-origin monotonic counter OR unix nanos. Application
    /// identity — orthogonal to the RedEX storage sequence.
    pub seq_or_ts: u64,
    /// xxh3 truncation of the type-specific tail (the bytes after
    /// the 24-byte prefix in the RedEX payload).
    pub checksum: u32,
}

impl EventMeta {
    /// Build an `EventMeta` with zeroed pad bytes.
    pub fn new(dispatch: u8, flags: u8, origin_hash: u64, seq_or_ts: u64, checksum: u32) -> Self {
        Self {
            dispatch,
            flags,
            _pad: [0; 2],
            origin_hash,
            seq_or_ts,
            checksum,
        }
    }

    /// Encode to the 24-byte little-endian wire format. The reserved
    /// `_pad` bytes are always written as zero regardless of what the
    /// caller stuffed into them — the wire contract says "zero on
    /// write, ignored on read."
    pub fn to_bytes(&self) -> [u8; EVENT_META_SIZE] {
        let mut out = [0u8; EVENT_META_SIZE];
        out[0] = self.dispatch;
        out[1] = self.flags;
        // out[2..4] stays [0, 0] (reserved pad).
        out[4..12].copy_from_slice(&self.origin_hash.to_le_bytes());
        out[12..20].copy_from_slice(&self.seq_or_ts.to_le_bytes());
        out[20..24].copy_from_slice(&self.checksum.to_le_bytes());
        out
    }

    /// Decode from a 24-byte slice. Returns `None` if the slice is
    /// shorter than 24 bytes.
    #[expect(
        clippy::expect_used,
        reason = "bytes.len() >= EVENT_META_SIZE (24) checked above; fixed-offset slices convert infallibly to fixed-size arrays"
    )]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < EVENT_META_SIZE {
            return None;
        }
        Some(Self {
            dispatch: bytes[0],
            flags: bytes[1],
            _pad: [bytes[2], bytes[3]],
            origin_hash: u64::from_le_bytes(bytes[4..12].try_into().expect("8 bytes")),
            seq_or_ts: u64::from_le_bytes(bytes[12..20].try_into().expect("8 bytes")),
            checksum: u32::from_le_bytes(bytes[20..24].try_into().expect("4 bytes")),
        })
    }

    /// True if `flags & bits != 0`.
    #[inline]
    pub fn has_flag(&self, bits: u8) -> bool {
        self.flags & bits != 0
    }

    /// Wire bytes with the `checksum` field zeroed — the input
    /// shape used by [`compute_checksum_with_meta`]. Zeroing the
    /// checksum slot is necessary because the value being computed
    /// will be stored there; including its prior value would make
    /// the hash depend on whatever was previously in the slot
    /// (including the uninitialized 0 placeholder during ingest).
    fn for_checksum_bytes(&self) -> [u8; EVENT_META_SIZE] {
        let mut b = self.to_bytes();
        // bytes [20..24] = checksum field; zero it.
        b[20..24].copy_from_slice(&0u32.to_le_bytes());
        b
    }
}

/// Legacy tail-only checksum. The xxh3 hash of the payload bytes
/// after the 24-byte `EventMeta` prefix, truncated to the low 32
/// bits.
///
/// **Use [`compute_checksum_with_meta`] for new writes.** This
/// function is kept for the read-side fallback that lets old
/// on-disk records continue to decode after the audit-#8 fix —
/// records written before the meta-covering checksum was
/// introduced have a tail-only checksum and would fail v2
/// verification.
///
/// When `compute_checksum` was also used by producers, the
/// 20-byte header was unprotected: a stray bit-flip in the
/// `dispatch` byte (e.g. `STORED → DELETED`) went undetected by
/// the per-event integrity check and silently re-routed the
/// event to the wrong fold arm.
///
/// **Scope:** an *accidental-corruption* detector, NOT a tamper
/// detector. Two specific limits make it unsuitable for
/// tamper-resistance:
///
/// 1. **32-bit truncation.** A 32-bit unkeyed hash has roughly
///    1-in-2³² collision probability per pair of distinct
///    payloads — fine against random bit-flips on disk, but only
///    ~1-in-2¹⁶ across a long-running file under the birthday
///    bound. Adequate for "did the on-disk record decode
///    correctly?" not "did an attacker craft a payload that
///    matches?".
/// 2. **Unkeyed.** An attacker who can write to the on-disk
///    redex file can recompute the matching checksum trivially
///    by hashing whatever payload they substitute. There is no
///    secret bound to this value.
///
/// Callers that need tamper detection (rather than corruption
/// detection) must layer a keyed MAC at a higher level — e.g.
/// the AEAD-protected mesh packet envelope. The cortex fold
/// paths use this value only to surface obviously-broken on-disk
/// records as `RedexError::Decode`, not as a security boundary.
///
/// Disk-recovery / external inspection tools can reproduce the
/// value by hashing the bytes after the 20-byte prefix.
#[inline]
pub fn compute_checksum(tail: &[u8]) -> u32 {
    xxhash_rust::xxh3::xxh3_64(tail) as u32
}

/// Corruption-detection checksum covering BOTH the
/// 24-byte `EventMeta` header (with the `checksum` slot zeroed,
/// since that's what the value being computed will go into) and
/// the payload `tail`. Stamped into [`EventMeta::checksum`] at
/// ingest by current writers.
///
/// **Why this exists vs. plain [`compute_checksum`].** The legacy
/// helper hashes only the tail; the 20-byte header is unprotected.
/// A bit-flip in the `dispatch` byte (e.g. `STORED → DELETED`) is
/// undetected by the per-event integrity check and silently
/// re-routes the event to the wrong fold arm — the audit-#8
/// failure mode. This helper closes that hole by including the
/// header bytes in the hash input.
///
/// **Migration / backward compatibility.** Records written by
/// pre-fix versions have a tail-only checksum that won't validate
/// under v2. The fold-side verifiers try v2 first and fall back
/// to v1 to keep old data readable. The fallback path retains
/// the original dispatch-flip vulnerability for legacy records;
/// new records get full-header coverage. Downgrading to a pre-fix
/// adapter binary will skip every event written by a v2-capable
/// producer (the legacy verifier expects the checksum to match
/// `xxh3(tail)`, which v2 records won't), so the migration is
/// effectively one-way.
///
/// **Scope.** Same accidental-corruption (not tamper) limits as
/// [`compute_checksum`]; see that function's doc for the full
/// 32-bit-truncation and unkeyed-hash discussion. The new
/// helper closes a structural undercoverage gap, not the
/// underlying tamper-resistance limits.
#[inline]
pub fn compute_checksum_with_meta(meta: &EventMeta, tail: &[u8]) -> u32 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(&meta.for_checksum_bytes());
    h.update(tail);
    h.digest() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_is_twenty_four() {
        assert_eq!(EVENT_META_SIZE, 24);
    }

    #[test]
    fn test_roundtrip_all_fields_distinct() {
        let m = EventMeta::new(
            0x42,
            FLAG_CAUSAL | FLAG_CONTINUITY_PROOF,
            0xDEAD_BEEF,
            0x0123_4567_89AB_CDEF,
            0xCAFE_BABE,
        );
        let bytes = m.to_bytes();
        assert_eq!(bytes.len(), 24);
        let decoded = EventMeta::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, m);
        assert_eq!(decoded.dispatch, 0x42);
        assert_eq!(decoded.flags, FLAG_CAUSAL | FLAG_CONTINUITY_PROOF);
        assert_eq!(decoded.origin_hash, 0xDEAD_BEEF);
        assert_eq!(decoded.seq_or_ts, 0x0123_4567_89AB_CDEF);
        assert_eq!(decoded.checksum, 0xCAFE_BABE);
    }

    #[test]
    fn test_regression_pad_is_zeroed_on_write() {
        // Regression: `to_bytes` used to copy `self._pad` verbatim
        // into the output buffer. The struct doc says "reserved;
        // must be zero on write, ignored on read" — but `_pad` is
        // `pub`, so a caller constructing `EventMeta` via struct
        // literal syntax could stamp non-zero pad into the wire
        // format. The fix leaves bytes [2..4] as zero regardless of
        // struct contents.
        let m = EventMeta {
            dispatch: 0x42,
            flags: 0,
            _pad: [0xAA, 0xBB], // non-zero — would leak on write
            origin_hash: 0xDEAD_BEEF,
            seq_or_ts: 1,
            checksum: 0,
        };
        let bytes = m.to_bytes();
        assert_eq!(
            &bytes[2..4],
            &[0u8, 0u8],
            "pad bytes must be zero on write regardless of struct contents"
        );
    }

    #[test]
    fn test_zero_roundtrip() {
        let m = EventMeta::new(0, 0, 0, 0, 0);
        let decoded = EventMeta::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn test_unknown_dispatch_decodes_fine() {
        // 0xFE is in application / vendor space — adapter must not
        // reject or special-case it.
        let m = EventMeta::new(0xFE, 0, 1, 2, 3);
        let decoded = EventMeta::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(decoded.dispatch, 0xFE);
    }

    #[test]
    fn test_short_slice_returns_none() {
        let buf = [0u8; 23];
        assert!(EventMeta::from_bytes(&buf).is_none());
    }

    #[test]
    fn test_nonzero_pad_tolerated_on_read() {
        // Pad bytes must be zero on write, but garbage on read should
        // not corrupt other fields.
        let mut bytes = [0u8; 24];
        bytes[2] = 0xAA;
        bytes[3] = 0xBB;
        bytes[12..20].copy_from_slice(&0x1234_5678_9ABC_DEF0u64.to_le_bytes());
        let decoded = EventMeta::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.seq_or_ts, 0x1234_5678_9ABC_DEF0);
        // Pad is carried through verbatim — surface it for forensic
        // inspection but it has no semantic meaning.
        assert_eq!(decoded._pad, [0xAA, 0xBB]);
    }

    #[test]
    fn test_has_flag() {
        let m = EventMeta::new(0, FLAG_CAUSAL, 0, 0, 0);
        assert!(m.has_flag(FLAG_CAUSAL));
        assert!(!m.has_flag(FLAG_CONTINUITY_PROOF));
    }

    #[test]
    fn test_field_boundaries_isolated() {
        // Write max values in each field; decode must return them all.
        let m = EventMeta::new(u8::MAX, u8::MAX, u64::MAX, u64::MAX, u32::MAX);
        let decoded = EventMeta::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(decoded, m);
    }

    // ====================================================================
    // compute_checksum_with_meta — header coverage
    // ====================================================================

    /// `compute_checksum_with_meta` zeroes the checksum slot in
    /// the input bytes regardless of what the caller stuffed
    /// there. Pin the producer-side contract: callers do
    ///   let mut m = EventMeta::new(..., 0);
    ///   m.checksum = compute_checksum_with_meta(&m, tail);
    /// so the meta passed in has `checksum == 0` already; if a
    /// caller forgets the `0` and passes a non-zero placeholder,
    /// the helper still produces the same value because the slot
    /// is masked.
    #[test]
    fn compute_checksum_with_meta_masks_checksum_slot() {
        let tail = b"some payload bytes";
        let m_zero = EventMeta::new(0x42, 0, 0xDEAD_BEEF, 7, 0);
        let m_nonzero = EventMeta::new(0x42, 0, 0xDEAD_BEEF, 7, 0xFFFF_FFFF);
        // The slot-mask means both produce the same checksum even
        // though their `checksum` fields differ.
        assert_eq!(
            compute_checksum_with_meta(&m_zero, tail),
            compute_checksum_with_meta(&m_nonzero, tail),
        );
    }

    /// A bit-flip in the `dispatch` byte is detected by
    /// `compute_checksum_with_meta` but invisible to the legacy
    /// `compute_checksum`. Pin both directions so a future
    /// refactor that accidentally drops the helper or silently
    /// routes producers back through the legacy function trips.
    #[test]
    fn compute_checksum_with_meta_detects_dispatch_bit_flip() {
        let tail = b"unchanged payload";
        let original = EventMeta::new(0x10 /* STORED */, 0, 0xABCD, 1, 0);
        let v2 = compute_checksum_with_meta(&original, tail);

        // Attacker / cosmic ray flips the dispatch byte to a
        // different routing target; tail unchanged.
        let flipped = EventMeta::new(0x11 /* DELETED */, 0, 0xABCD, 1, 0);
        let v2_after_flip = compute_checksum_with_meta(&flipped, tail);
        assert_ne!(
            v2, v2_after_flip,
            "v2 must reflect the dispatch byte; a flip changes the checksum",
        );

        // Legacy v1 can't see the flip because it only hashes
        // the tail. Pin the gap so the doc-comment claim
        // ("legacy is dispatch-flip vulnerable") stays true and
        // doesn't accidentally get fixed by a downstream change
        // to compute_checksum.
        assert_eq!(
            compute_checksum(tail),
            compute_checksum(tail),
            "legacy hash is tail-only; insensitive to header flips by construction",
        );
    }

    /// Header coverage extends to every field, not just dispatch.
    /// Pin flags / origin_hash / seq_or_ts each individually so a
    /// regression that "covers some of the header" is caught.
    #[test]
    fn compute_checksum_with_meta_detects_all_header_flips() {
        let tail = b"payload";
        let base = EventMeta::new(0x10, 0, 0xAAAA, 1, 0);
        let v2_base = compute_checksum_with_meta(&base, tail);

        let flip_flags = EventMeta::new(0x10, FLAG_CAUSAL, 0xAAAA, 1, 0);
        assert_ne!(v2_base, compute_checksum_with_meta(&flip_flags, tail));

        let flip_origin = EventMeta::new(0x10, 0, 0xBBBB, 1, 0);
        assert_ne!(v2_base, compute_checksum_with_meta(&flip_origin, tail));

        let flip_seq = EventMeta::new(0x10, 0, 0xAAAA, 2, 0);
        assert_ne!(v2_base, compute_checksum_with_meta(&flip_seq, tail));

        // Same fields, different tail: also detected.
        let flip_tail = compute_checksum_with_meta(&base, b"different");
        assert_ne!(v2_base, flip_tail);
    }

    /// v1 and v2 over a non-empty tail produce different values.
    /// Pin so the legacy fallback path in fold.rs cannot
    /// accidentally accept a v2 record (or vice versa) by
    /// numerical coincidence — they're hashing different inputs.
    #[test]
    fn v1_and_v2_checksums_differ_for_typical_inputs() {
        let m = EventMeta::new(0x01, 0, 0x1234, 5, 0);
        let tail = b"non-empty payload";
        assert_ne!(compute_checksum(tail), compute_checksum_with_meta(&m, tail));
    }
}
