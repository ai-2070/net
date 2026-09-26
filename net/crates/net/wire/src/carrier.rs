//! Which of a WebRTC session's two DataChannels a Net packet rides.
//!
//! A browser session carries Net packets on DataChannels. One of them,
//! [`RELIABLE_CHANNEL_LABEL`], is ordered and fully retransmitted by SCTP:
//! handshakes, control, reliable streams and everything that has always
//! assumed delivery ride it. The other, [`LOSSY_CHANNEL_LABEL`], is
//! **unordered with zero retransmits**: a packet on it arrives promptly or
//! not at all, and a lost one never delays another — no head-of-line
//! blocking. That is the carrier for high-rate state where only the newest
//! value matters (positions, inputs).
//!
//! A packet rides the lossy channel only when its sender asked for it,
//! per packet, with [`PacketFlags::LOSSY`] — set only on the
//! fire-and-forget data of a stream opened for it — and only when nothing
//! else about it needs delivery ([`rides_lossy_carrier`]). The flag is in
//! the plaintext header and authenticated with it, so a relay (the anchor)
//! can classify a packet it forwards without decrypting it, and a receiver
//! that does not know the flag ignores it.
//!
//! Loss and reordering are already tolerated where these packets land: the
//! session's replay window admits out-of-order counters within
//! [`crate::crypto`]'s window, and a fire-and-forget stream skips gaps and
//! drops what arrives after a newer sequence.

use crate::protocol::{PacketFlags, MAGIC};
use crate::route_codec::{ROUTING_HEADER_SIZE, ROUTING_MAGIC};

/// The ordered, fully reliable DataChannel every session has.
pub const RELIABLE_CHANNEL_LABEL: &str = "net";

/// The unordered, zero-retransmit DataChannel for lossy packets.
pub const LOSSY_CHANNEL_LABEL: &str = "net-u";

/// Flags that mean a packet must be delivered — never lossy, whatever
/// else it says.
const NEEDS_DELIVERY: u8 = PacketFlags::RELIABLE.bits()
    | PacketFlags::HANDSHAKE.bits()
    | PacketFlags::HEARTBEAT.bits()
    | PacketFlags::NACK.bits()
    | PacketFlags::MODE_BOUNDARY.bits();

/// May `datagram` — a Net packet as it goes on the wire, direct or behind
/// a routing header — ride the lossy DataChannel?
///
/// `true` only when the sender set [`PacketFlags::LOSSY`] and none of the
/// flags that need delivery. Anything this cannot read as a Net header is
/// `false`: when in doubt, the reliable channel.
pub fn rides_lossy_carrier(datagram: &[u8]) -> bool {
    let header = match datagram.get(..2).map(|m| u16::from_le_bytes([m[0], m[1]])) {
        Some(MAGIC) => datagram,
        Some(ROUTING_MAGIC) => match datagram.get(ROUTING_HEADER_SIZE..) {
            Some(inner) => inner,
            None => return false,
        },
        _ => return false,
    };
    let (Some(magic), Some(&flags)) = (header.get(..2), header.get(3)) else {
        return false;
    };
    u16::from_le_bytes([magic[0], magic[1]]) == MAGIC
        && flags & PacketFlags::LOSSY.bits() != 0
        && flags & NEEDS_DELIVERY == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(flags: u8) -> Vec<u8> {
        let mut h = vec![0u8; crate::protocol::HEADER_SIZE];
        h[..2].copy_from_slice(&MAGIC.to_le_bytes());
        h[3] = flags;
        h
    }

    #[test]
    fn only_a_lossy_flag_with_nothing_needing_delivery_rides_the_lossy_channel() {
        assert!(rides_lossy_carrier(&header(PacketFlags::LOSSY.bits())));
        assert!(!rides_lossy_carrier(&header(0)), "unflagged is not lossy");
        for needs in [
            PacketFlags::RELIABLE,
            PacketFlags::HANDSHAKE,
            PacketFlags::HEARTBEAT,
            PacketFlags::NACK,
            PacketFlags::MODE_BOUNDARY,
        ] {
            assert!(
                !rides_lossy_carrier(&header(PacketFlags::LOSSY.bits() | needs.bits())),
                "{needs:?} needs delivery"
            );
        }
        // PRIORITY and FIN say nothing about delivery.
        assert!(rides_lossy_carrier(&header(
            PacketFlags::LOSSY.bits() | PacketFlags::PRIORITY.bits() | PacketFlags::FIN.bits()
        )));
    }

    #[test]
    fn a_relayed_packet_is_read_past_its_routing_header() {
        let mut routed = vec![0u8; ROUTING_HEADER_SIZE];
        routed[..2].copy_from_slice(&ROUTING_MAGIC.to_le_bytes());
        routed.extend(header(PacketFlags::LOSSY.bits()));
        assert!(rides_lossy_carrier(&routed));
        routed[ROUTING_HEADER_SIZE + 3] = PacketFlags::RELIABLE.bits();
        assert!(!rides_lossy_carrier(&routed));
    }

    #[test]
    fn anything_unreadable_goes_reliable() {
        assert!(!rides_lossy_carrier(&[]));
        assert!(!rides_lossy_carrier(&[0x45]));
        assert!(
            !rides_lossy_carrier(&[0, 0, 0, PacketFlags::LOSSY.bits()]),
            "no magic"
        );
        let mut routed = vec![0u8; ROUTING_HEADER_SIZE];
        routed[..2].copy_from_slice(&ROUTING_MAGIC.to_le_bytes());
        assert!(
            !rides_lossy_carrier(&routed),
            "a routing header with nothing behind it"
        );
    }
}
