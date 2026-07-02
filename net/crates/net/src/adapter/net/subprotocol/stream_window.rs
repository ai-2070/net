//! Stream window subprotocol — receiver → sender credit grants.
//!
//! Ships over `SUBPROTOCOL_STREAM_WINDOW` on existing encrypted
//! sessions. Each grant carries the receiver's **absolute**
//! cumulative bytes-consumed count on the named stream; the sender
//! reconciles its credit state from that authoritative value.
//!
//! ## Why absolute, not additive
//!
//! An additive grant (`credit_bytes` added to the sender's
//! remaining credit) permanently strands credit when either a data
//! packet OR a grant is dropped on the wire. An absolute
//! `total_consumed` grant is self-healing: every arriving grant
//! carries the receiver's full accounting, so any single lost
//! grant is reconciled by the next one. Lost data packets leave
//! `tx_bytes_sent - total_consumed` elevated until recovery
//! (retransmit for `Reliable`, stream reset for `FireAndForget`),
//! but the sender's credit view converges exactly when the
//! receiver's view does.
//!
//! Wire layout: 16 bytes per message (`u64 stream_id LE` +
//! `u64 total_consumed LE`).

use bytes::{Buf, BufMut};

/// Subprotocol ID for stream-window credit grants.
pub const SUBPROTOCOL_STREAM_WINDOW: u16 = 0x0B00;

/// Subprotocol ID for receiver → sender retransmit NACKs
/// ([`StreamNack`]). Sibling of the window grant: both are small
/// receiver-driven control messages about a stream's progress.
pub const SUBPROTOCOL_STREAM_NACK: u16 = 0x0B01;

/// Subprotocol ID for a sender → receiver stream reset ([`StreamReset`]):
/// the sender's reliable layer gave up retransmitting a gap, so the
/// receiver should fail any pending read on this stream now rather than
/// stall to a timeout (H-3).
pub const SUBPROTOCOL_STREAM_RESET: u16 = 0x0B02;

/// Subprotocol ID for receiver → sender positive SACK-range ACKs
/// ([`StreamAckRanges`]) — STREAM_ACK_BATCHING_AND_RANGES R-1.
/// Emission is capability-gated (`net.reliable.stream_ack_ranges@1`);
/// receivers accept unconditionally.
pub const SUBPROTOCOL_STREAM_ACK: u16 = 0x0B03;

/// Fixed wire size of a [`StreamReset`] in bytes.
pub const STREAM_RESET_SIZE: usize = 8;

/// Fixed wire size in bytes (`stream_id` + `total_consumed` + `ack_seq`).
pub const STREAM_WINDOW_SIZE: usize = 24;

/// Fixed wire size of a [`StreamNack`] in bytes.
pub const STREAM_NACK_SIZE: usize = 24;

/// Fixed header size of a [`StreamAckRanges`] (`stream_id` + `ack_seq`);
/// each range adds [`STREAM_ACK_RANGE_SIZE`] bytes.
pub const STREAM_ACK_HEADER_SIZE: usize = 16;

/// Wire size of one half-open `[start, end)` range in a
/// [`StreamAckRanges`].
pub const STREAM_ACK_RANGE_SIZE: usize = 16;

/// Max ranges carried per [`StreamAckRanges`] message. 16 ranges is
/// 272 wire bytes — comfortably one event inside a batched control
/// frame. The receiver's range index truncates newest-first to this
/// cap; the dropped oldest ranges are exactly the ones the next
/// cumulative-ack advance covers first.
pub const MAX_ACK_RANGES: usize = 16;

/// Receiver → sender credit grant. Authoritative: `total_consumed`
/// is the receiver's cumulative bytes-consumed count on the named
/// stream since it was opened. The sender uses this to recompute
/// `tx_credit_remaining = tx_window - (tx_bytes_sent - total_consumed)`,
/// making the mechanism self-healing against lost grants.
///
/// # Consumer-side validation
///
/// The codec accepts any `total_consumed: u64`. Pre-fix the doc-
/// comment's "self-healing" framing implied no further validation
/// was needed, but the formula
/// `tx_credit_remaining = tx_window - (tx_bytes_sent - total_consumed)`
/// underflows if a malformed or hostile peer sends
/// `total_consumed > tx_bytes_sent`. **The consumer MUST clamp
/// `total_consumed` to its local `tx_bytes_sent` watermark before
/// applying.** `StreamState::apply_authoritative_grant`
/// (`adapter/net/session.rs:1153-1154`) does this today; any
/// future consumer of this codec must do the same. The codec
/// layer cannot do the clamp itself because it doesn't know the
/// sender's local state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamWindow {
    /// Stream the grant applies to.
    pub stream_id: u64,
    /// Receiver's cumulative consumed-byte count on this stream.
    ///
    /// Consumers MUST clamp this to the local `tx_bytes_sent`
    /// watermark before deriving credit.
    pub total_consumed: u64,
    /// Receiver's cumulative reliable ack — the lowest sequence not yet
    /// contiguously received (`next_expected`). The sender prunes its
    /// retransmit window of everything below this (H-9). 0 for
    /// non-reliable receive streams (nothing to prune).
    pub ack_seq: u64,
}

/// Errors produced by the codec. Shared by all three fixed-size
/// messages in this module, so the expected size is carried per
/// error rather than hardcoded in the message.
#[derive(Debug, thiserror::Error)]
pub enum StreamWindowCodecError {
    /// Buffer shorter than the fixed wire size.
    #[error("truncated stream subprotocol message: {got} bytes (need {need})")]
    Truncated {
        /// Bytes received.
        got: usize,
        /// The message's fixed wire size.
        need: usize,
    },
    /// Buffer longer than the fixed wire size. Rejects garbage
    /// trailers rather than silently ignoring them.
    #[error("oversize stream subprotocol message: {got} bytes (need {need})")]
    Oversize {
        /// Bytes received.
        got: usize,
        /// The message's fixed wire size.
        need: usize,
    },
    /// Structurally invalid [`StreamAckRanges`] payload — bad length
    /// shape, range count, range bounds, ordering, or overlap.
    #[error("invalid stream-ack ranges: {reason}")]
    InvalidRanges {
        /// Which validation failed.
        reason: &'static str,
    },
}

impl StreamWindow {
    /// Encode to a fixed 16-byte buffer.
    #[inline]
    pub fn encode(&self) -> [u8; STREAM_WINDOW_SIZE] {
        let mut buf = [0u8; STREAM_WINDOW_SIZE];
        (&mut buf[..8]).put_u64_le(self.stream_id);
        (&mut buf[8..16]).put_u64_le(self.total_consumed);
        (&mut buf[16..]).put_u64_le(self.ack_seq);
        buf
    }

    /// Decode a fixed-size message. Returns an error on truncated or
    /// oversize input.
    pub fn decode(data: &[u8]) -> Result<Self, StreamWindowCodecError> {
        match data.len() {
            n if n < STREAM_WINDOW_SIZE => Err(StreamWindowCodecError::Truncated {
                got: n,
                need: STREAM_WINDOW_SIZE,
            }),
            n if n > STREAM_WINDOW_SIZE => Err(StreamWindowCodecError::Oversize {
                got: n,
                need: STREAM_WINDOW_SIZE,
            }),
            _ => {
                let mut cur = std::io::Cursor::new(data);
                let stream_id = cur.get_u64_le();
                let total_consumed = cur.get_u64_le();
                let ack_seq = cur.get_u64_le();
                Ok(Self {
                    stream_id,
                    total_consumed,
                    ack_seq,
                })
            }
        }
    }
}

/// Receiver → sender retransmit request. Names a stream and the gaps
/// the receiver is missing: `next_expected` is the lowest sequence not
/// yet received contiguously, and `missing_bitmap` bit `i` is set iff
/// `next_expected + 1 + i` is also still missing. The sender feeds this
/// to `ReliableStream::on_nack` to pull the matching retransmit
/// descriptors. Carries `stream_id` itself (rides `CONTROL_STREAM_ID`
/// like the window grant), so the sender doesn't depend on the packet
/// header's stream field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamNack {
    /// Stream the NACK applies to.
    pub stream_id: u64,
    /// Lowest sequence the receiver has not received contiguously.
    pub next_expected: u64,
    /// Bitmap of further missing sequences after `next_expected`.
    pub missing_bitmap: u64,
}

impl StreamNack {
    /// Encode to a fixed 24-byte buffer.
    #[inline]
    pub fn encode(&self) -> [u8; STREAM_NACK_SIZE] {
        let mut buf = [0u8; STREAM_NACK_SIZE];
        (&mut buf[..8]).put_u64_le(self.stream_id);
        (&mut buf[8..16]).put_u64_le(self.next_expected);
        (&mut buf[16..]).put_u64_le(self.missing_bitmap);
        buf
    }

    /// Decode a 24-byte message. Errors on truncated / oversize input.
    pub fn decode(data: &[u8]) -> Result<Self, StreamWindowCodecError> {
        match data.len() {
            n if n < STREAM_NACK_SIZE => Err(StreamWindowCodecError::Truncated {
                got: n,
                need: STREAM_NACK_SIZE,
            }),
            n if n > STREAM_NACK_SIZE => Err(StreamWindowCodecError::Oversize {
                got: n,
                need: STREAM_NACK_SIZE,
            }),
            _ => {
                let mut cur = std::io::Cursor::new(data);
                let stream_id = cur.get_u64_le();
                let next_expected = cur.get_u64_le();
                let missing_bitmap = cur.get_u64_le();
                Ok(Self {
                    stream_id,
                    next_expected,
                    missing_bitmap,
                })
            }
        }
    }
}

/// Receiver → sender positive SACK-range ACK (STREAM_ACK_BATCHING R-1).
///
/// Semantics, precisely: `ack_seq` cumulatively acknowledges every
/// sequence `< ack_seq` (identical to `StreamWindow::ack_seq` — the
/// receiver's `next_expected`). `ranges` selectively acknowledge
/// received runs **strictly above** `ack_seq` as half-open
/// `[start, end)` intervals. `ack_seq` itself is by definition the
/// missing head — it is never inside a range, and the cumulative run
/// is never duplicated as a range. Example: `ack_seq = 101`,
/// `ranges = [(102, 10001)]` ⇒ everything below 101 received, 101
/// missing, 102..=10000 received.
///
/// Wire order is DESCENDING by `end` (newest first): when the
/// receiver truncates to [`MAX_ACK_RANGES`] it drops the *oldest*
/// ranges — the ones the next cumulative advance covers first.
/// Ranges are fully merged: non-overlapping and non-adjacent.
///
/// The sender feeds this to `ReliableStream::on_ack_ranges`, which
/// removes SACKed packets from the retransmit window so one lost
/// head packet no longer RTO-floods everything behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAckRanges {
    /// Stream the ack applies to.
    pub stream_id: u64,
    /// Cumulative ack — every sequence `< ack_seq` is received.
    pub ack_seq: u64,
    /// Received runs strictly above `ack_seq`: half-open
    /// `[start, end)`, descending by `end`, merged,
    /// `1..=MAX_ACK_RANGES` entries.
    pub ranges: Vec<(u64, u64)>,
}

impl StreamAckRanges {
    /// Minimum wire size: header + one range. A rangeless message is
    /// meaningless (the grant's `ack_seq` already covers the
    /// contiguous case) and is rejected by [`Self::decode`].
    pub const MIN_SIZE: usize = STREAM_ACK_HEADER_SIZE + STREAM_ACK_RANGE_SIZE;

    /// Maximum wire size (header + [`MAX_ACK_RANGES`] ranges).
    pub const MAX_SIZE: usize = STREAM_ACK_HEADER_SIZE + MAX_ACK_RANGES * STREAM_ACK_RANGE_SIZE;

    /// Encode to `16 + 16·n` bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf =
            Vec::with_capacity(STREAM_ACK_HEADER_SIZE + self.ranges.len() * STREAM_ACK_RANGE_SIZE);
        buf.put_u64_le(self.stream_id);
        buf.put_u64_le(self.ack_seq);
        for &(start, end) in &self.ranges {
            buf.put_u64_le(start);
            buf.put_u64_le(end);
        }
        buf
    }

    /// Decode with strict validation. Rejects: truncated / non-`16·n`
    /// bodies, zero or more than [`MAX_ACK_RANGES`] ranges, empty or
    /// inverted ranges, ranges not strictly above `ack_seq`, and any
    /// ordering that is not strictly-descending / merged (overlap or
    /// adjacency means the producer failed to merge — treat as
    /// malformed rather than guessing).
    pub fn decode(data: &[u8]) -> Result<Self, StreamWindowCodecError> {
        if data.len() < Self::MIN_SIZE {
            return Err(StreamWindowCodecError::Truncated {
                got: data.len(),
                need: Self::MIN_SIZE,
            });
        }
        let body = data.len() - STREAM_ACK_HEADER_SIZE;
        if !body.is_multiple_of(STREAM_ACK_RANGE_SIZE) {
            return Err(StreamWindowCodecError::InvalidRanges {
                reason: "length is not header + 16*n",
            });
        }
        let n = body / STREAM_ACK_RANGE_SIZE;
        if n > MAX_ACK_RANGES {
            return Err(StreamWindowCodecError::InvalidRanges {
                reason: "range count exceeds MAX_ACK_RANGES",
            });
        }
        let mut cur = std::io::Cursor::new(data);
        let stream_id = cur.get_u64_le();
        let ack_seq = cur.get_u64_le();
        let mut ranges = Vec::with_capacity(n);
        // Descending, merged: each range must end strictly below the
        // previous (newer) range's start — `end == prev_start` would
        // be adjacency the producer should have merged.
        let mut prev_start: Option<u64> = None;
        for _ in 0..n {
            let start = cur.get_u64_le();
            let end = cur.get_u64_le();
            if start >= end {
                return Err(StreamWindowCodecError::InvalidRanges {
                    reason: "empty or inverted range",
                });
            }
            if start <= ack_seq {
                return Err(StreamWindowCodecError::InvalidRanges {
                    reason: "range not strictly above ack_seq",
                });
            }
            if let Some(ps) = prev_start {
                if end >= ps {
                    return Err(StreamWindowCodecError::InvalidRanges {
                        reason: "ranges not descending/merged",
                    });
                }
            }
            prev_start = Some(start);
            ranges.push((start, end));
        }
        Ok(Self {
            stream_id,
            ack_seq,
            ranges,
        })
    }
}

/// Sender → receiver stream reset (H-3). Names a stream the sender has
/// given up retransmitting; the receiver fails any pending read on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamReset {
    /// Stream the reset applies to.
    pub stream_id: u64,
}

impl StreamReset {
    /// Encode to a fixed 8-byte buffer.
    #[inline]
    pub fn encode(&self) -> [u8; STREAM_RESET_SIZE] {
        self.stream_id.to_le_bytes()
    }

    /// Decode an 8-byte message. Errors on truncated / oversize input.
    pub fn decode(data: &[u8]) -> Result<Self, StreamWindowCodecError> {
        match data.len() {
            n if n < STREAM_RESET_SIZE => Err(StreamWindowCodecError::Truncated {
                got: n,
                need: STREAM_RESET_SIZE,
            }),
            n if n > STREAM_RESET_SIZE => Err(StreamWindowCodecError::Oversize {
                got: n,
                need: STREAM_RESET_SIZE,
            }),
            _ => {
                let mut cur = std::io::Cursor::new(data);
                Ok(Self {
                    stream_id: cur.get_u64_le(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip() {
        let msg = StreamWindow {
            stream_id: 0xDEAD_BEEF_CAFE_F00D,
            total_consumed: 0x0102_0304_0506_0708,
            ack_seq: 0x1122_3344_5566_7788,
        };
        let bytes = msg.encode();
        assert_eq!(bytes.len(), STREAM_WINDOW_SIZE);
        let parsed = StreamWindow::decode(&bytes).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_decode_truncated_rejected() {
        let err = StreamWindow::decode(&[0u8; STREAM_WINDOW_SIZE - 1]).unwrap_err();
        assert!(matches!(
            err,
            StreamWindowCodecError::Truncated {
                need: STREAM_WINDOW_SIZE,
                ..
            }
        ));
        // The message must report the real expected size, not a
        // stale hardcoded one (pre-fix it said "need 16" while the
        // wire size had grown to 24 with `ack_seq`).
        assert!(err.to_string().contains("need 24"), "got: {err}");
    }

    #[test]
    fn test_decode_oversize_rejected() {
        let err = StreamWindow::decode(&[0u8; STREAM_WINDOW_SIZE + 1]).unwrap_err();
        assert!(matches!(
            err,
            StreamWindowCodecError::Oversize {
                need: STREAM_WINDOW_SIZE,
                ..
            }
        ));
    }

    #[test]
    fn test_decode_empty_rejected() {
        let err = StreamWindow::decode(&[]).unwrap_err();
        assert!(matches!(
            err,
            StreamWindowCodecError::Truncated { got: 0, .. }
        ));
    }

    #[test]
    fn test_endianness_is_little_endian() {
        // Explicit LE check — stream_id=1 must produce `01 00 ... 00`.
        let msg = StreamWindow {
            stream_id: 1,
            total_consumed: 1,
            ack_seq: 1,
        };
        let bytes = msg.encode();
        assert_eq!(bytes[0], 0x01);
        assert_eq!(bytes[1], 0x00);
        assert_eq!(bytes[8], 0x01);
        assert_eq!(bytes[9], 0x00);
        assert_eq!(bytes[16], 0x01);
        assert_eq!(bytes[17], 0x00);
    }

    #[test]
    fn stream_nack_round_trip() {
        let msg = StreamNack {
            stream_id: 0xABCD,
            next_expected: 7,
            missing_bitmap: 0b1010,
        };
        assert_eq!(StreamNack::decode(&msg.encode()).unwrap(), msg);
    }

    #[test]
    fn stream_reset_round_trip() {
        let msg = StreamReset {
            stream_id: 0x2000_0000_0000_0001,
        };
        assert_eq!(StreamReset::decode(&msg.encode()).unwrap(), msg);
    }

    // ── StreamAckRanges (R-1) ────────────────────────────────────

    fn ack(ack_seq: u64, ranges: &[(u64, u64)]) -> StreamAckRanges {
        StreamAckRanges {
            stream_id: 0xF00D,
            ack_seq,
            ranges: ranges.to_vec(),
        }
    }

    #[test]
    fn ack_ranges_round_trip_single_and_max() {
        // The plan's canonical example: <101 received, 101 missing,
        // 102..=10000 received.
        let one = ack(101, &[(102, 10_001)]);
        assert_eq!(StreamAckRanges::decode(&one.encode()).unwrap(), one);

        // MAX_ACK_RANGES descending disjoint ranges.
        let ranges: Vec<(u64, u64)> = (0..MAX_ACK_RANGES as u64)
            .map(|i| {
                let hi = 10_000 - i * 100;
                (hi - 10, hi)
            })
            .collect();
        let full = ack(5, &ranges);
        let bytes = full.encode();
        assert_eq!(bytes.len(), StreamAckRanges::MAX_SIZE);
        assert_eq!(StreamAckRanges::decode(&bytes).unwrap(), full);
    }

    #[test]
    fn ack_ranges_rejects_rangeless_and_truncated() {
        // Header only (rangeless) is below MIN_SIZE.
        let err = StreamAckRanges::decode(&[0u8; STREAM_ACK_HEADER_SIZE]).unwrap_err();
        assert!(matches!(err, StreamWindowCodecError::Truncated { .. }));
        let err = StreamAckRanges::decode(&[]).unwrap_err();
        assert!(matches!(err, StreamWindowCodecError::Truncated { .. }));
    }

    #[test]
    fn ack_ranges_rejects_non_multiple_length() {
        let bytes = ack(1, &[(2, 3)]).encode();
        let mut long = bytes.clone();
        long.push(0);
        assert!(matches!(
            StreamAckRanges::decode(&long).unwrap_err(),
            StreamWindowCodecError::InvalidRanges { .. }
        ));
    }

    #[test]
    fn ack_ranges_rejects_too_many_ranges() {
        let ranges: Vec<(u64, u64)> = (0..(MAX_ACK_RANGES as u64 + 1))
            .map(|i| {
                let hi = 100_000 - i * 10;
                (hi - 2, hi)
            })
            .collect();
        let bytes = ack(1, &ranges).encode();
        assert!(matches!(
            StreamAckRanges::decode(&bytes).unwrap_err(),
            StreamWindowCodecError::InvalidRanges {
                reason: "range count exceeds MAX_ACK_RANGES"
            }
        ));
    }

    #[test]
    fn ack_ranges_rejects_bad_range_shapes() {
        // Empty range.
        assert!(matches!(
            StreamAckRanges::decode(&ack(1, &[(5, 5)]).encode()).unwrap_err(),
            StreamWindowCodecError::InvalidRanges {
                reason: "empty or inverted range"
            }
        ));
        // Inverted range.
        assert!(StreamAckRanges::decode(&ack(1, &[(9, 5)]).encode()).is_err());
        // Range at ack_seq (must be strictly above — ack_seq is the
        // missing head by definition).
        assert!(matches!(
            StreamAckRanges::decode(&ack(10, &[(10, 12)]).encode()).unwrap_err(),
            StreamWindowCodecError::InvalidRanges {
                reason: "range not strictly above ack_seq"
            }
        ));
        // Range below ack_seq (would duplicate the cumulative run).
        assert!(StreamAckRanges::decode(&ack(10, &[(3, 6)]).encode()).is_err());
    }

    #[test]
    fn ack_ranges_rejects_unmerged_or_ascending_order() {
        // Ascending (oldest first) — wire order must be newest first.
        assert!(matches!(
            StreamAckRanges::decode(&ack(1, &[(2, 4), (6, 8)]).encode()).unwrap_err(),
            StreamWindowCodecError::InvalidRanges {
                reason: "ranges not descending/merged"
            }
        ));
        // Adjacent (end == next start) — producer failed to merge.
        assert!(StreamAckRanges::decode(&ack(1, &[(6, 8), (4, 6)]).encode()).is_err());
        // Overlapping.
        assert!(StreamAckRanges::decode(&ack(1, &[(5, 9), (3, 7)]).encode()).is_err());
    }
}
