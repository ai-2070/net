//! 20-byte `RedexEntry` record with little-endian on-disk codec.
//!
//! An entry is 20 bytes on the wire / on disk:
//!
//! ```text
//! ┌──────────────┬────────────────┬──────────────┬────────────────────┐
//! │ seq (u64 LE) │ offset (u32 LE)│  len (u32 LE)│ flags+checksum u32 │
//! │   8 bytes    │   4 bytes      │  4 bytes     │     4 bytes        │
//! └──────────────┴────────────────┴────────────────┴──────────────────┘
//! ```
//!
//! When `flags_and_checksum`'s high bit is set ([`RedexFlags::INLINE`]),
//! the 8 bytes normally occupied by `payload_offset` + `payload_len` are
//! reinterpreted as **exactly 8 bytes** of inline payload. Inline entries
//! are fixed-length; callers emitting shorter fixed-size events pad the
//! remainder themselves.
//!
//! Checksum is an xxh3 truncation (low 28 bits) for tamper/dedup
//! detection.

use xxhash_rust::xxh3::xxh3_64;

/// Size in bytes of one `RedexEntry` in the wire/disk format.
pub const REDEX_ENTRY_SIZE: usize = 20;

/// Inline payload size in bytes. Inline entries are fixed-length.
pub const INLINE_PAYLOAD_SIZE: usize = 8;

/// High-nibble flag constants packed into `flags_and_checksum`.
pub struct RedexFlags;

impl RedexFlags {
    /// Payload is carried inline as exactly 8 bytes in `offset`+`len`.
    pub const INLINE: u32 = 0x8000_0000;
    /// Logical delete marker. Reserved for v2 compaction.
    pub const TOMBSTONE: u32 = 0x4000_0000;
    /// This entry has been compacted. Reserved for v2.
    pub const COMPACTED: u32 = 0x2000_0000;
}

const FLAG_MASK: u32 = 0xF000_0000;
const CHECKSUM_MASK: u32 = 0x0FFF_FFFF;

/// A single index record.
///
/// The in-memory representation is whatever the compiler picks; the wire
/// format (20 bytes, little-endian) is produced by [`Self::to_bytes`] and
/// consumed by [`Self::from_bytes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedexEntry {
    /// Monotonic per-file sequence.
    pub seq: u64,
    /// Byte offset into the file's payload segment. When `INLINE` is
    /// set, this field + `payload_len` together hold exactly 8 bytes
    /// of inline payload (little-endian: bytes 0..4 in `payload_offset`,
    /// bytes 4..8 in `payload_len`).
    pub payload_offset: u32,
    /// Payload length in bytes. When `INLINE` is set, this carries
    /// bytes 4..8 of the inline payload (not a length).
    pub payload_len: u32,
    /// Top 4 bits: flags. Low 28 bits: xxh3 truncation of payload.
    pub flags_and_checksum: u32,
}

impl RedexEntry {
    /// Build a heap-backed entry (payload lives in a segment).
    ///
    /// `flags` is masked to the high nibble and `checksum` is masked to
    /// the low 28 bits, so callers can't accidentally corrupt one by
    /// overloading the other.
    ///
    /// # Panics
    ///
    /// Panics if `flags` includes `RedexFlags::INLINE`.
    ///
    /// Pre-fix, this silently accepted the INLINE flag. The
    /// resulting entry's `is_inline()` returned true but
    /// `payload_offset` / `payload_len` were real heap fields
    /// rather than payload bytes. `materialize` would then call
    /// `inline_payload()` and reinterpret the offset/length as
    /// data, returning corrupt bytes that *match* a (different)
    /// checksum recomputation — silent corruption. Currently no
    /// caller passes a non-zero `flags`, but unguarded the API
    /// is a future footgun. Reject INLINE explicitly so any
    /// future caller surfaces the misuse as a clear panic.
    pub fn new_heap(seq: u64, offset: u32, len: u32, flags: u32, checksum: u32) -> Self {
        assert!(
            flags & RedexFlags::INLINE == 0,
            "new_heap rejects flags=INLINE; use new_inline for inline payloads"
        );
        Self {
            seq,
            payload_offset: offset,
            payload_len: len,
            flags_and_checksum: (flags & FLAG_MASK) | (checksum & CHECKSUM_MASK),
        }
    }

    /// Build an inline entry from exactly 8 payload bytes.
    #[expect(
        clippy::expect_used,
        reason = "input is &[u8; INLINE_PAYLOAD_SIZE]; fixed slice converts are statically infallible"
    )]
    pub fn new_inline(seq: u64, payload: &[u8; INLINE_PAYLOAD_SIZE], checksum: u32) -> Self {
        let offset = u32::from_le_bytes(payload[0..4].try_into().expect("4 bytes"));
        let len = u32::from_le_bytes(payload[4..8].try_into().expect("4 bytes"));
        Self {
            seq,
            payload_offset: offset,
            payload_len: len,
            flags_and_checksum: RedexFlags::INLINE | (checksum & CHECKSUM_MASK),
        }
    }

    /// True if the payload is inline.
    #[inline]
    pub fn is_inline(&self) -> bool {
        self.flags_and_checksum & RedexFlags::INLINE != 0
    }

    /// Flag bits only (top nibble).
    #[inline]
    pub fn flags(&self) -> u32 {
        self.flags_and_checksum & FLAG_MASK
    }

    /// Payload checksum (low 28 bits).
    #[inline]
    pub fn checksum(&self) -> u32 {
        self.flags_and_checksum & CHECKSUM_MASK
    }

    /// If `is_inline()`, return the 8 inline payload bytes.
    pub fn inline_payload(&self) -> Option<[u8; INLINE_PAYLOAD_SIZE]> {
        if !self.is_inline() {
            return None;
        }
        let mut out = [0u8; INLINE_PAYLOAD_SIZE];
        out[0..4].copy_from_slice(&self.payload_offset.to_le_bytes());
        out[4..8].copy_from_slice(&self.payload_len.to_le_bytes());
        Some(out)
    }

    /// Encode to the 20-byte little-endian wire format.
    ///
    /// `#[inline(always)]` per perf #70 — called in the inner loop
    /// of disk read/write paths (`disk::read_index`, append).
    /// The body is four `copy_from_slice` calls on `u64`/`u32`
    /// le-bytes, which the optimizer fuses into a small fixed
    /// sequence once inlined.
    #[inline(always)]
    pub fn to_bytes(&self) -> [u8; REDEX_ENTRY_SIZE] {
        let mut out = [0u8; REDEX_ENTRY_SIZE];
        out[0..8].copy_from_slice(&self.seq.to_le_bytes());
        out[8..12].copy_from_slice(&self.payload_offset.to_le_bytes());
        out[12..16].copy_from_slice(&self.payload_len.to_le_bytes());
        out[16..20].copy_from_slice(&self.flags_and_checksum.to_le_bytes());
        out
    }

    /// Decode from the 20-byte little-endian wire format.
    ///
    /// `#[inline(always)]` per perf #70 — same justification as
    /// [`Self::to_bytes`].
    #[expect(
        clippy::expect_used,
        reason = "input is &[u8; REDEX_ENTRY_SIZE]; fixed slice converts are statically infallible"
    )]
    #[inline(always)]
    pub fn from_bytes(bytes: &[u8; REDEX_ENTRY_SIZE]) -> Self {
        Self {
            seq: u64::from_le_bytes(bytes[0..8].try_into().expect("8 bytes")),
            payload_offset: u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")),
            payload_len: u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")),
            flags_and_checksum: u32::from_le_bytes(bytes[16..20].try_into().expect("4 bytes")),
        }
    }
}

/// Compute the low-28-bit xxh3 truncation used as the record checksum.
#[inline]
pub fn payload_checksum(payload: &[u8]) -> u32 {
    (xxh3_64(payload) as u32) & CHECKSUM_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_size() {
        assert_eq!(REDEX_ENTRY_SIZE, 20);
    }

    #[test]
    fn test_heap_roundtrip() {
        let e = RedexEntry::new_heap(42, 1024, 256, 0, 0x0123_4567);
        let bytes = e.to_bytes();
        assert_eq!(bytes.len(), 20);
        let decoded = RedexEntry::from_bytes(&bytes);
        assert_eq!(e, decoded);
        assert!(!decoded.is_inline());
        assert_eq!(decoded.payload_offset, 1024);
        assert_eq!(decoded.payload_len, 256);
        assert_eq!(decoded.checksum(), 0x0123_4567);
    }

    #[test]
    fn test_inline_roundtrip() {
        let payload: [u8; 8] = *b"abcd1234";
        let cks = payload_checksum(&payload);
        let e = RedexEntry::new_inline(7, &payload, cks);
        let bytes = e.to_bytes();
        let decoded = RedexEntry::from_bytes(&bytes);
        assert_eq!(e, decoded);
        assert!(decoded.is_inline());
        assert_eq!(decoded.inline_payload().unwrap(), payload);
    }

    #[test]
    fn test_inline_zeros() {
        let payload = [0u8; 8];
        let e = RedexEntry::new_inline(0, &payload, 0);
        let decoded = RedexEntry::from_bytes(&e.to_bytes());
        assert!(decoded.is_inline());
        assert_eq!(decoded.inline_payload().unwrap(), payload);
    }

    #[test]
    fn test_inline_all_high_bits() {
        // Ensure INLINE interpretation doesn't get confused when
        // the inline bytes happen to look like a flag.
        let payload = [0xFFu8; 8];
        let e = RedexEntry::new_inline(3, &payload, 0);
        let decoded = RedexEntry::from_bytes(&e.to_bytes());
        assert!(decoded.is_inline());
        assert_eq!(decoded.inline_payload().unwrap(), payload);
    }

    #[test]
    fn test_checksum_does_not_alias_flags() {
        let e = RedexEntry::new_heap(1, 0, 0, 0, 0xFFFF_FFFF);
        assert!(!e.is_inline());
        assert_eq!(e.checksum(), CHECKSUM_MASK);
    }

    #[test]
    fn test_flags_are_preserved() {
        let e = RedexEntry::new_heap(0, 0, 0, RedexFlags::TOMBSTONE, 0);
        let decoded = RedexEntry::from_bytes(&e.to_bytes());
        assert_eq!(decoded.flags(), RedexFlags::TOMBSTONE);
    }

    #[test]
    fn test_payload_checksum_deterministic() {
        assert_eq!(payload_checksum(b"abc"), payload_checksum(b"abc"));
        assert_ne!(payload_checksum(b"abc"), payload_checksum(b"abd"));
        assert_eq!(payload_checksum(b"abc") & !CHECKSUM_MASK, 0);
    }

    #[test]
    fn test_non_inline_entry_reports_no_inline_payload() {
        let e = RedexEntry::new_heap(0, 4, 100, 0, 0);
        assert!(e.inline_payload().is_none());
    }

    /// Passing `RedexFlags::INLINE` to `new_heap` must
    /// panic. Pre-fix it silently accepted the flag and produced
    /// an entry where `is_inline()` returned true but `payload_offset`
    /// / `payload_len` carried real heap fields, leading to
    /// silent corruption when `materialize` reinterpreted them
    /// as inline payload bytes.
    #[test]
    #[should_panic(expected = "rejects flags=INLINE")]
    fn new_heap_with_inline_flag_panics() {
        // offset/len/checksum are arbitrary; the assertion fires
        // before they're used.
        let _ = RedexEntry::new_heap(0, 4, 100, RedexFlags::INLINE, 0);
    }

    /// TOMBSTONE (a non-INLINE flag) must
    /// still go through cleanly.
    #[test]
    fn new_heap_with_tombstone_flag_succeeds() {
        let e = RedexEntry::new_heap(0, 4, 100, RedexFlags::TOMBSTONE, 0);
        assert!(!e.is_inline());
    }
}
