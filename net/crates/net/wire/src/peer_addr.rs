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
/// names an *endpoint*, not a UDP tuple. Only [`PeerAddr::Udp`] exists in
/// this stage; the RTC variant is Stage 3's and is feature-gated there.
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
        }
    }
}
