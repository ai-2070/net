//! `ParsedPacket` — copied from
//! `net/crates/net/src/adapter/net/transport.rs:334-376`.
//!
//! Not in §7's list, but `session.rs:27` imports it and
//! `NetSession::verify_and_touch_heartbeat` takes it by reference, so
//! it has to move (or be re-shaped) for Stage 2. The rest of
//! `transport.rs` — `NetSocket`, `PacketSender`, `PacketReceiver`,
//! `BatchedPacketReceiver` — is tokio/UDP and stays in core.
//!
//! Note the `source: SocketAddr` field: a `std::net` type inside a
//! wire struct. It compiles for `wasm32-unknown-unknown` (the type is
//! plain data; only the syscall surface is missing), but it is exactly
//! the Stage 1 `PeerAddr` generalization's business. Kept verbatim
//! here so the spike measures the real shape.

use bytes::Bytes;
use std::net::SocketAddr;

use crate::protocol::{NetHeader, HEADER_SIZE};

/// Parsed packet for processing
#[derive(Debug)]
pub struct ParsedPacket {
    /// Packet header
    pub header: NetHeader,
    /// Encrypted payload (includes auth tag)
    pub payload: Bytes,
    /// Source address
    pub source: SocketAddr,
}

impl ParsedPacket {
    /// Parse a raw packet
    pub fn parse(data: Bytes, source: SocketAddr) -> Option<Self> {
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
