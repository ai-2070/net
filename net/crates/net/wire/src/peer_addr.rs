//! `PeerAddr` — where a peer is reached.
//!
//! Stage 2 moved the endpoint type down into the wire crate
//! (`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` §Stage 1 decision 2):
//! `NetSession::peer_addr` and `ParsedPacket::source` are both
//! `PeerAddr`, so leaving it in the core's `transport.rs` would make
//! the wire layer depend on the native runtime. The **submission**
//! surface (`PeerSink`) stays in the core with the sockets.

use std::net::SocketAddr;

/// Where a peer is reached.
///
/// Stage 1 of `BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`: peer-keyed state
/// names an *endpoint*, not a UDP tuple. [`PeerAddr::Udp`] is the only
/// variant in default builds; `PeerAddr::Rtc` appears under the
/// `webrtc` feature (Stage 3) and names a DataChannel the core's RTC
/// driver owns — the wire layer never touches str0m. (Plain text, not
/// an intra-doc link: with default features the variant does not
/// exist, and a link to an absent item is a denied rustdoc warning —
/// R5-B, which is what reddened CI's `Documentation` job.)
///
/// Deliberately **not** `FromStr` and **not** `serde`: nothing serializes a
/// `PeerAddr`. Operator-facing configuration
/// (`MeshNodeConfig::{bind_addr, peer_addr}`, `reflex_override`) and every
/// wire field (`reflex_addr`, `ReflexMsg`, `RendezvousMsg`) stay
/// `SocketAddr`. Convert at the boundary with [`PeerAddr::Udp`] /
/// [`PeerAddr::udp`], never through a lossy helper that invents a tuple.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PeerAddr {
    /// A UDP tuple — the only variant in default builds.
    Udp(SocketAddr),
    /// A WebRTC DataChannel, identified by the slot the core's RTC
    /// driver owns it in. Only under the `webrtc` feature.
    #[cfg(feature = "webrtc")]
    Rtc(RtcPeerId),
    /// A peer reached through a **blind UDP relay**: every datagram to or
    /// from it is framed with [`relay_data_header`] and exchanged with the
    /// relay's UDP tuple, which forwards it on `channel` without decrypting
    /// anything. The channel is unique to this peer at that relay, so the
    /// pair is a sound endpoint key.
    Relayed {
        /// The relay's UDP tuple.
        relay: SocketAddr,
        /// The channel the relay allocated for this peer.
        channel: u32,
    },
}

/// First byte of a blind-relay data frame (mirrors the core relay's `DATA`).
pub const RELAY_DATA_KIND: u8 = 0x10;
/// Length of the blind-relay data frame header: kind + channel.
pub const RELAY_DATA_HEADER_LEN: usize = 5;

/// The frame header prepended to a datagram sent through a blind relay.
#[inline]
pub fn relay_data_header(channel: u32) -> [u8; RELAY_DATA_HEADER_LEN] {
    let c = channel.to_be_bytes();
    [RELAY_DATA_KIND, c[0], c[1], c[2], c[3]]
}

/// Split a relay datagram into `(channel, payload)` when it is a data frame.
#[inline]
pub fn split_relay_data(frame: &[u8]) -> Option<(u32, &[u8])> {
    if frame.len() < RELAY_DATA_HEADER_LEN || frame[0] != RELAY_DATA_KIND {
        return None;
    }
    let channel = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);
    Some((channel, &frame[RELAY_DATA_HEADER_LEN..]))
}

/// Identifies one DataChannel owned by the core's RTC driver.
///
/// Not an address: the driver holds the `str0m::Rtc` and the channel,
/// and everything outside it names the peer by this handle. `slot` is
/// the driver's table index; `generation` is bumped every time a slot
/// closes, so a handle captured before a close can never address the
/// session that reuses the slot. Ids are never reused — a
/// `(slot, generation)` pair is spent once.
#[cfg(feature = "webrtc")]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RtcPeerId {
    /// Driver table index.
    pub slot: u32,
    /// Incarnation of `slot`; bumped on close.
    pub generation: u32,
}

#[cfg(feature = "webrtc")]
impl std::fmt::Display for RtcPeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rtc:{}.{}", self.slot, self.generation)
    }
}

impl PeerAddr {
    /// The UDP tuple, when this endpoint is one.
    ///
    /// The single conversion back to `SocketAddr`, used at the boundaries
    /// that genuinely need a tuple: binding, `is_loopback` / partition
    /// checks, traversal input and reflex publication.
    #[inline]
    pub fn udp(&self) -> Option<SocketAddr> {
        match self {
            PeerAddr::Udp(addr) => Some(*addr),
            #[cfg(feature = "webrtc")]
            PeerAddr::Rtc(_) => None,
            PeerAddr::Relayed { .. } => None,
        }
    }

    /// `(relay, channel)` when this endpoint is reached through a blind relay.
    #[inline]
    pub fn relayed(&self) -> Option<(SocketAddr, u32)> {
        match self {
            PeerAddr::Relayed { relay, channel } => Some((*relay, *channel)),
            _ => None,
        }
    }

    /// The DataChannel handle, when this endpoint is one.
    #[cfg(feature = "webrtc")]
    #[inline]
    pub fn rtc(&self) -> Option<RtcPeerId> {
        match self {
            PeerAddr::Rtc(id) => Some(*id),
            PeerAddr::Udp(_) | PeerAddr::Relayed { .. } => None,
        }
    }
}

impl From<SocketAddr> for PeerAddr {
    #[inline]
    fn from(addr: SocketAddr) -> Self {
        PeerAddr::Udp(addr)
    }
}

impl std::fmt::Display for PeerAddr {
    /// Renders the inner `SocketAddr` unchanged, so every log line and
    /// error string that formats a peer endpoint is byte-identical to the
    /// pre-`PeerAddr` text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerAddr::Udp(addr) => write!(f, "{addr}"),
            #[cfg(feature = "webrtc")]
            PeerAddr::Rtc(id) => write!(f, "{id}"),
            PeerAddr::Relayed { relay, channel } => write!(f, "relay:{relay}#{channel}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_frames_round_trip_and_reject_other_kinds() {
        let mut frame = relay_data_header(0xDEAD_BEEF).to_vec();
        frame.extend_from_slice(b"ciphertext");
        assert_eq!(
            split_relay_data(&frame),
            Some((0xDEAD_BEEF, &b"ciphertext"[..]))
        );
        assert_eq!(split_relay_data(&frame[..4]), None);
        frame[0] = 0x04;
        assert_eq!(split_relay_data(&frame), None);
        let relayed = PeerAddr::Relayed {
            relay: "203.0.113.1:3478".parse().unwrap(),
            channel: 7,
        };
        assert_eq!(relayed.udp(), None);
        assert_eq!(
            relayed.relayed(),
            Some(("203.0.113.1:3478".parse().unwrap(), 7))
        );
        assert_eq!(relayed.to_string(), "relay:203.0.113.1:3478#7");
    }
}
