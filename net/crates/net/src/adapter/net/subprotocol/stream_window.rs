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

/// Fixed wire size of a [`StreamReset`] in bytes.
pub const STREAM_RESET_SIZE: usize = 8;

/// Fixed wire size in bytes (`stream_id` + `total_consumed` + `ack_seq`).
pub const STREAM_WINDOW_SIZE: usize = 24;

/// Fixed wire size of a [`StreamNack`] in bytes.
pub const STREAM_NACK_SIZE: usize = 24;

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

/// Errors produced by the codec.
#[derive(Debug, thiserror::Error)]
pub enum StreamWindowCodecError {
    /// Buffer shorter than the fixed wire size.
    #[error("truncated stream-window message: {0} bytes (need 16)")]
    Truncated(usize),
    /// Buffer longer than the fixed wire size. Rejects garbage
    /// trailers rather than silently ignoring them.
    #[error("oversize stream-window message: {0} bytes (need 16)")]
    Oversize(usize),
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
            n if n < STREAM_WINDOW_SIZE => Err(StreamWindowCodecError::Truncated(n)),
            n if n > STREAM_WINDOW_SIZE => Err(StreamWindowCodecError::Oversize(n)),
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
            n if n < STREAM_NACK_SIZE => Err(StreamWindowCodecError::Truncated(n)),
            n if n > STREAM_NACK_SIZE => Err(StreamWindowCodecError::Oversize(n)),
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
            n if n < STREAM_RESET_SIZE => Err(StreamWindowCodecError::Truncated(n)),
            n if n > STREAM_RESET_SIZE => Err(StreamWindowCodecError::Oversize(n)),
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
        assert!(matches!(err, StreamWindowCodecError::Truncated(_)));
    }

    #[test]
    fn test_decode_oversize_rejected() {
        let err = StreamWindow::decode(&[0u8; STREAM_WINDOW_SIZE + 1]).unwrap_err();
        assert!(matches!(err, StreamWindowCodecError::Oversize(_)));
    }

    #[test]
    fn test_decode_empty_rejected() {
        let err = StreamWindow::decode(&[]).unwrap_err();
        assert!(matches!(err, StreamWindowCodecError::Truncated(0)));
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
        let msg = StreamReset { stream_id: 0x2000_0000_0000_0001 };
        assert_eq!(StreamReset::decode(&msg.encode()).unwrap(), msg);
    }
}
