//! Causal link and chain validation for distributed state.
//!
//! Every event produced by an entity carries a `CausalLink` (28 bytes) that
//! chains it to the previous event. The chain provides structural integrity
//! via xxh3 hashing — tamper resistance comes from Net's AEAD encryption.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use xxhash_rust::xxh3::xxh3_64;

/// Subprotocol ID for causal-framed events.
pub const SUBPROTOCOL_CAUSAL: u16 = 0x0400;

/// Subprotocol ID for state snapshot transfer.
pub const SUBPROTOCOL_SNAPSHOT: u16 = 0x0401;

/// Wire size of a CausalLink.
///
/// `horizon_encoded` is `u64`-wide so the 64-bit bloom is usable
/// up to ~16 active origins per event. A narrower 16-bit bloom
/// packed into the high half of a u32 would saturate at ~6-8
/// origins, defeating concurrency detection. See
/// `state/horizon.rs` for the FPR table and the
/// out-of-band-fallback escape hatch.
pub const CAUSAL_LINK_SIZE: usize = 28;

/// Causal link — 28 bytes prepended to each event in causal-framed EventFrames.
///
/// Wire format (28 bytes, no padding):
/// ```text
/// origin_hash:      4 bytes (u32) — entity identity
/// horizon_encoded:  8 bytes (u64) — compressed observed horizon
/// sequence:         8 bytes (u64) — monotonic per-entity
/// parent_hash:      8 bytes (u64) — xxh3 of (prev link ++ prev payload)
/// ```
///
/// Fields are ordered with the smallest first; serialization uses
/// explicit `to_bytes`/`from_bytes`, not transmute, so any in-memory
/// padding the compiler chooses doesn't leak to the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CausalLink {
    /// Truncated entity identity (matches Net header origin_hash).
    pub origin_hash: u32,
    /// Compressed observed horizon (64-bit bloom sketch).
    ///
    /// Approximate, with documented FPR-vs-cardinality table on
    /// `HorizonEncoder` — see `state/horizon.rs`. Tuned for
    /// ≲ 16 active origins per event; callers needing exact
    /// horizons at higher cardinalities must fall back to the
    /// out-of-band full-`ObservedHorizon` path
    /// (`ObservedHorizon::has_observed`).
    pub horizon_encoded: u64,
    /// Monotonic sequence number from entity's reference frame.
    pub sequence: u64,
    /// xxh3 hash of the previous event's (CausalLink bytes ++ payload bytes).
    pub parent_hash: u64,
}

impl CausalLink {
    /// Create the genesis link for a new entity (no parent).
    pub fn genesis(origin_hash: u32, horizon_encoded: u64) -> Self {
        Self {
            origin_hash,
            horizon_encoded,
            sequence: 0,
            parent_hash: 0,
        }
    }

    /// Create the next link in a chain given the previous link and payload.
    ///
    /// Returns `None` if the sequence number would overflow `u64::MAX`.
    #[inline]
    pub fn next(&self, payload: &[u8], horizon_encoded: u64) -> Option<Self> {
        let next_seq = self.sequence.checked_add(1)?;
        Some(Self {
            origin_hash: self.origin_hash,
            horizon_encoded,
            sequence: next_seq,
            parent_hash: compute_parent_hash(self, payload),
        })
    }

    /// Serialize to 28 bytes.
    #[inline]
    pub fn to_bytes(&self) -> [u8; CAUSAL_LINK_SIZE] {
        let mut buf = [0u8; CAUSAL_LINK_SIZE];
        let mut cursor = &mut buf[..];
        cursor.put_u32_le(self.origin_hash);
        cursor.put_u64_le(self.horizon_encoded);
        cursor.put_u64_le(self.sequence);
        cursor.put_u64_le(self.parent_hash);
        buf
    }

    /// Deserialize from bytes. Returns None if too short.
    #[inline]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < CAUSAL_LINK_SIZE {
            return None;
        }
        let mut cursor = &data[..CAUSAL_LINK_SIZE];
        Some(Self {
            origin_hash: cursor.get_u32_le(),
            horizon_encoded: cursor.get_u64_le(),
            sequence: cursor.get_u64_le(),
            parent_hash: cursor.get_u64_le(),
        })
    }

    /// Check if this is a genesis link (sequence 0, no parent).
    #[inline]
    pub fn is_genesis(&self) -> bool {
        self.sequence == 0 && self.parent_hash == 0
    }
}

/// Compute the parent hash for the next link in a chain.
///
/// Hash covers the previous link's bytes concatenated with the payload.
/// Uses xxh3 (~50GB/s) — structural integrity, not cryptographic commitment.
#[inline]
pub fn compute_parent_hash(prev_link: &CausalLink, prev_payload: &[u8]) -> u64 {
    let link_bytes = prev_link.to_bytes();
    // For short payloads, concatenate and hash in one shot.
    // For large payloads, use xxh3's incremental API if needed (future optimization).
    let mut combined = Vec::with_capacity(CAUSAL_LINK_SIZE + prev_payload.len());
    combined.extend_from_slice(&link_bytes);
    combined.extend_from_slice(prev_payload);
    xxh3_64(&combined)
}

/// A causal event: link + payload.
#[derive(Debug, Clone)]
pub struct CausalEvent {
    /// The causal link binding this event to its chain.
    pub link: CausalLink,
    /// The event payload (opaque bytes).
    pub payload: Bytes,
    /// Local timestamp when this event was received/created (nanos since epoch).
    pub received_at: u64,
}

/// Chain builder for producing causally-linked events.
///
/// Tracks the head of the chain and produces correctly-linked events.
pub struct CausalChainBuilder {
    origin_hash: u32,
    head: CausalLink,
    head_payload: Bytes,
}

impl CausalChainBuilder {
    /// Create a new chain builder for an entity.
    pub fn new(origin_hash: u32) -> Self {
        let genesis = CausalLink::genesis(origin_hash, 0);
        Self {
            origin_hash,
            head: genesis,
            head_payload: Bytes::new(),
        }
    }

    /// Create from an existing chain head (e.g., after snapshot restore).
    pub fn from_head(head: CausalLink, head_payload: Bytes) -> Self {
        Self {
            origin_hash: head.origin_hash,
            head,
            head_payload,
        }
    }

    /// Produce the next event in the chain.
    ///
    /// Returns `None` if the sequence number would overflow.
    pub fn append(&mut self, payload: Bytes, horizon_encoded: u64) -> Option<CausalEvent> {
        let next_link = self.head.next(&self.head_payload, horizon_encoded)?;
        let event = CausalEvent {
            link: next_link,
            payload: payload.clone(),
            received_at: current_timestamp(),
        };
        self.head = next_link;
        self.head_payload = payload;
        Some(event)
    }

    /// Get the current head link.
    #[inline]
    pub fn head(&self) -> &CausalLink {
        &self.head
    }

    /// Get the current sequence number.
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.head.sequence
    }

    /// Get the origin hash.
    #[inline]
    pub fn origin_hash(&self) -> u32 {
        self.origin_hash
    }
}

/// Validate that a new link correctly extends a chain.
///
/// Checks:
/// 1. Origin hash matches
/// 2. Sequence is exactly prev + 1
/// 3. parent_hash matches xxh3(prev_link ++ prev_payload)
pub fn validate_chain_link(
    prev_link: &CausalLink,
    prev_payload: &[u8],
    new_link: &CausalLink,
) -> Result<(), ChainError> {
    if new_link.origin_hash != prev_link.origin_hash {
        return Err(ChainError::OriginMismatch {
            expected: prev_link.origin_hash,
            got: new_link.origin_hash,
        });
    }
    let expected_seq = prev_link
        .sequence
        .checked_add(1)
        .ok_or(ChainError::SequenceGap {
            expected: u64::MAX,
            got: new_link.sequence,
        })?;
    if new_link.sequence != expected_seq {
        return Err(ChainError::SequenceGap {
            expected: expected_seq,
            got: new_link.sequence,
        });
    }
    let expected_parent = compute_parent_hash(prev_link, prev_payload);
    if new_link.parent_hash != expected_parent {
        return Err(ChainError::ParentHashMismatch {
            expected: expected_parent,
            got: new_link.parent_hash,
        });
    }
    Ok(())
}

/// Write causal events into a buffer (CausalLink prepended to each event).
///
/// Format per event: `[len: u32][CausalLink: 28 bytes][payload: len-28 bytes]`
///
/// Result of a `write_causal_events` call.
///
/// Callers of this writer typically pass the pre-write `events.len()`
/// to a downstream framing layer (e.g. as the `count` field on the
/// next packet header) so the reader can know how many
/// `[len][link][payload]` triples to expect. If the writer silently
/// `continue`s past oversized events, the framing count and the
/// actual events serialized mismatch — and `read_causal_events`
/// parses junk for the missing slots.
///
/// Surface both numbers so the caller can either:
///   - Use `events_written` as the framing count (correct), or
///   - Detect `events_written < events.len()` and retry / split
///     the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteCausalEventsResult {
    /// Bytes appended to `buf`.
    pub bytes_written: usize,
    /// Number of events actually serialized. Equal to
    /// `events.len()` unless one or more events were too large
    /// to encode (payload > `u32::MAX - 24`).
    pub events_written: usize,
    /// Events skipped because their serialized size would
    /// overflow the `u32` length prefix.
    pub events_skipped: usize,
}

/// Events whose serialized size would overflow the `u32` length
/// prefix (payload > ~4 GiB − 28 bytes) are skipped rather than
/// panicking. No realistic caller hands in payloads this size, but
/// an FFI path forwarding arbitrary `Bytes` could — making a crash
/// on oversized input a DoS vector. Callers MUST use the returned
/// `events_written` as the framing count, not the input slice's
/// length, or the reader will parse past valid data into noise.
pub fn write_causal_events(events: &[CausalEvent], buf: &mut BytesMut) -> WriteCausalEventsResult {
    let start = buf.len();
    let mut events_written = 0usize;
    let mut events_skipped = 0usize;
    for event in events {
        let total_len = CAUSAL_LINK_SIZE + event.payload.len();
        let total_len_u32 = match u32::try_from(total_len) {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    payload_len = event.payload.len(),
                    "write_causal_events: skipping event whose serialized \
                     size exceeds u32 — caller MUST use \
                     `events_written` as framing count, not events.len()",
                );
                events_skipped += 1;
                continue;
            }
        };
        buf.put_u32_le(total_len_u32);
        buf.put_slice(&event.link.to_bytes());
        buf.put_slice(&event.payload);
        events_written += 1;
    }
    WriteCausalEventsResult {
        bytes_written: buf.len() - start,
        events_written,
        events_skipped,
    }
}

/// Read causal events from a buffer.
///
/// Each event is `[len: u32][CausalLink: 28 bytes][payload: len-28 bytes]`.
pub fn read_causal_events(data: Bytes, count: u16) -> Vec<CausalEvent> {
    let cap = (count as usize).min(data.len() / (4 + CAUSAL_LINK_SIZE));
    let mut events = Vec::with_capacity(cap);
    let mut remaining = data;
    let mut parse_errors: u64 = 0;

    for _ in 0..count {
        if remaining.len() < 4 {
            parse_errors += 1;
            break;
        }
        let total_len = (&remaining[..4]).get_u32_le() as usize;
        remaining.advance(4);

        if remaining.len() < total_len || total_len < CAUSAL_LINK_SIZE {
            parse_errors += 1;
            break;
        }

        let link = match CausalLink::from_bytes(&remaining[..CAUSAL_LINK_SIZE]) {
            Some(l) => l,
            None => {
                parse_errors += 1;
                break;
            }
        };
        remaining.advance(CAUSAL_LINK_SIZE);

        let payload_len = total_len - CAUSAL_LINK_SIZE;
        let payload = remaining.split_to(payload_len);

        events.push(CausalEvent {
            link,
            payload,
            received_at: current_timestamp(),
        });
    }

    if parse_errors > 0 {
        tracing::warn!(
            parse_errors,
            expected = count,
            parsed = events.len(),
            "read_causal_events dropped malformed events"
        );
    }

    events
}

/// Errors from chain validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    /// Event's origin_hash doesn't match the chain's entity.
    OriginMismatch {
        /// Expected origin_hash.
        expected: u32,
        /// Actual origin_hash.
        got: u32,
    },
    /// Sequence number is not prev + 1.
    SequenceGap {
        /// Expected sequence number.
        expected: u64,
        /// Actual sequence number.
        got: u64,
    },
    /// parent_hash doesn't match xxh3(prev_link ++ prev_payload).
    ParentHashMismatch {
        /// Expected parent hash.
        expected: u64,
        /// Actual parent hash.
        got: u64,
    },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OriginMismatch { expected, got } => {
                write!(
                    f,
                    "origin mismatch: expected {:#x}, got {:#x}",
                    expected, got
                )
            }
            Self::SequenceGap { expected, got } => {
                write!(f, "sequence gap: expected {}, got {}", expected, got)
            }
            Self::ParentHashMismatch { expected, got } => {
                write!(
                    f,
                    "parent hash mismatch: expected {:#x}, got {:#x}",
                    expected, got
                )
            }
        }
    }
}

impl std::error::Error for ChainError {}

use crate::adapter::net::current_timestamp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_causal_link_roundtrip() {
        let link = CausalLink {
            origin_hash: 0xDEADBEEF,
            sequence: 42,
            parent_hash: 0x1234567890ABCDEF,
            horizon_encoded: 0xCAFE,
        };

        let bytes = link.to_bytes();
        assert_eq!(bytes.len(), CAUSAL_LINK_SIZE);

        let parsed = CausalLink::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, link);
    }

    #[test]
    fn test_genesis() {
        let link = CausalLink::genesis(0xABCD, 0);
        assert!(link.is_genesis());
        assert_eq!(link.sequence, 0);
        assert_eq!(link.parent_hash, 0);
    }

    #[test]
    fn test_chain_next() {
        let genesis = CausalLink::genesis(0xABCD, 0);
        let payload = b"hello";
        let next = genesis.next(payload, 0).unwrap();

        assert_eq!(next.sequence, 1);
        assert_eq!(next.origin_hash, 0xABCD);
        assert_ne!(next.parent_hash, 0); // should be hash of genesis + payload
    }

    #[test]
    fn test_chain_builder() {
        let mut builder = CausalChainBuilder::new(0xABCD);
        assert_eq!(builder.sequence(), 0);

        let e1 = builder.append(Bytes::from_static(b"event1"), 0).unwrap();
        assert_eq!(e1.link.sequence, 1);
        assert_eq!(builder.sequence(), 1);

        let e2 = builder.append(Bytes::from_static(b"event2"), 0).unwrap();
        assert_eq!(e2.link.sequence, 2);

        // Verify chain linkage
        assert_eq!(
            e2.link.parent_hash,
            compute_parent_hash(&e1.link, &e1.payload)
        );
    }

    #[test]
    fn test_validate_chain_link() {
        let mut builder = CausalChainBuilder::new(0xABCD);
        let e1 = builder.append(Bytes::from_static(b"event1"), 0).unwrap();
        let e2 = builder.append(Bytes::from_static(b"event2"), 0).unwrap();

        // Valid chain
        assert!(validate_chain_link(&e1.link, &e1.payload, &e2.link).is_ok());
    }

    #[test]
    fn test_validate_rejects_origin_mismatch() {
        let link1 = CausalLink::genesis(0xAAAA, 0);
        let mut link2 = link1.next(b"data", 0).unwrap();
        link2.origin_hash = 0xBBBB;

        assert!(matches!(
            validate_chain_link(&link1, b"data", &link2),
            Err(ChainError::OriginMismatch { .. })
        ));
    }

    #[test]
    fn test_validate_rejects_sequence_gap() {
        let link1 = CausalLink::genesis(0xAAAA, 0);
        let mut link2 = link1.next(b"data", 0).unwrap();
        link2.sequence = 5; // should be 1

        assert!(matches!(
            validate_chain_link(&link1, b"data", &link2),
            Err(ChainError::SequenceGap { .. })
        ));
    }

    #[test]
    fn test_validate_rejects_bad_parent_hash() {
        let link1 = CausalLink::genesis(0xAAAA, 0);
        let mut link2 = link1.next(b"data", 0).unwrap();
        link2.parent_hash = 0xBADBADBAD;

        assert!(matches!(
            validate_chain_link(&link1, b"data", &link2),
            Err(ChainError::ParentHashMismatch { .. })
        ));
    }

    #[test]
    fn test_causal_event_framing_roundtrip() {
        let mut builder = CausalChainBuilder::new(0xABCD);
        let events: Vec<CausalEvent> = (0..3)
            .map(|i| {
                builder
                    .append(Bytes::from(format!("event-{}", i)), 0)
                    .unwrap()
            })
            .collect();

        let mut buf = BytesMut::new();
        let result = write_causal_events(&events, &mut buf);
        assert!(result.bytes_written > 0);
        assert_eq!(result.events_written, events.len());
        assert_eq!(result.events_skipped, 0);

        let parsed = read_causal_events(buf.freeze(), 3);
        assert_eq!(parsed.len(), 3);

        for (orig, parsed) in events.iter().zip(parsed.iter()) {
            assert_eq!(parsed.link, orig.link);
            assert_eq!(parsed.payload, orig.payload);
        }
    }

    #[test]
    fn test_parent_hash_deterministic() {
        let link = CausalLink::genesis(0x1234, 0);
        let payload = b"test payload";

        let h1 = compute_parent_hash(&link, payload);
        let h2 = compute_parent_hash(&link, payload);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_parent_hash_differs_on_payload_change() {
        let link = CausalLink::genesis(0x1234, 0);
        let h1 = compute_parent_hash(&link, b"payload a");
        let h2 = compute_parent_hash(&link, b"payload b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_long_chain_integrity() {
        let mut builder = CausalChainBuilder::new(0xFACE);
        let mut events = Vec::new();

        for i in 0..100 {
            let event = builder
                .append(Bytes::from(format!("data-{}", i)), 0)
                .unwrap();
            events.push(event);
        }

        // Validate every consecutive pair
        for i in 1..events.len() {
            assert!(
                validate_chain_link(&events[i - 1].link, &events[i - 1].payload, &events[i].link)
                    .is_ok(),
                "chain broken at event {}",
                i
            );
        }
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_causal_link_wire_size_is_28() {
        // Regression: original repr(C) with field order u32, u64,
        // u64, u32 padded to 32 bytes; an earlier fix reordered to
        // u32, u32, u64, u64 (24 bytes). The bloom-widening fix widens
        // `horizon_encoded` from u32 to u64 to give the bloom
        // filter enough bits to be useful past ~6 origins, taking
        // the wire size to 28 bytes (4 + 8 + 8 + 8). Pin the new
        // size so a future refactor that drops bytes (or adds
        // padding) trips this test.
        let link = CausalLink::genesis(0xDEADBEEF, 0xCAFE);
        let bytes = link.to_bytes();
        assert_eq!(
            bytes.len(),
            28,
            "CausalLink wire size must be exactly 28 bytes"
        );
        assert_eq!(bytes.len(), CAUSAL_LINK_SIZE);

        // Verify roundtrip preserves all fields
        let parsed = CausalLink::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, link);
    }
}
