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
        }
    }

    /// The DataChannel handle, when this endpoint is one.
    #[cfg(feature = "webrtc")]
    #[inline]
    pub fn rtc(&self) -> Option<RtcPeerId> {
        match self {
            PeerAddr::Rtc(id) => Some(*id),
            PeerAddr::Udp(_) => None,
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
        }
    }
}
