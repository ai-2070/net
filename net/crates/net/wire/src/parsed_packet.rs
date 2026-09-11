//! `ParsedPacket` — the receive-side view of one Net datagram.
//!
//! Split out of the core's `transport.rs` in Stage 2: everything else
//! in that file (`NetSocket`, `PeerSink`, `PacketSender`,
//! `PacketReceiver`, `BatchedPacketReceiver`) is tokio/UDP and stays
//! there, but `NetSession::verify_and_touch_heartbeat` takes a
//! `&ParsedPacket`, so the type itself is wire surface.

use bytes::Bytes;

use crate::peer_addr::PeerAddr;
use crate::protocol::{NetHeader, HEADER_SIZE};

/// Parsed packet for processing
#[derive(Debug)]
pub struct ParsedPacket {
    /// Packet header
    pub header: NetHeader,
    /// Encrypted payload (includes auth tag)
    pub payload: Bytes,
    /// Endpoint the packet arrived from.
    pub source: PeerAddr,
}

impl ParsedPacket {
    /// Parse a raw packet
    pub fn parse(data: Bytes, source: PeerAddr) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }

        let header = NetHeader::from_bytes(&data)?;
        if !header.validate() {
            return None;
        }

        let payload = data.slice(HEADER_SIZE..);

        Some(Self {
            header,
            payload,
            source,
        })
    }

    /// Get the expected payload length (ciphertext + tag)
    pub fn expected_payload_len(&self) -> usize {
        self.header.payload_len as usize + crate::protocol::TAG_SIZE
    }

    /// Validate payload length
    pub fn is_valid_length(&self) -> bool {
        self.payload.len() == self.expected_payload_len()
    }
}
