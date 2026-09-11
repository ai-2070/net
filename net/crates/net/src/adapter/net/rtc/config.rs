//! `RtcConfig` — what an operator sets for the RTC transport.
//!
//! `MeshNodeConfig::rtc` is `Option<RtcConfig>` and defaults to `None`:
//! compiling the `webrtc` feature changes nothing on its own. With
//! `Some(..)` the node binds a **second** UDP socket for RTC traffic
//! (§6: no demultiplexing on the Net socket) and spawns the driver.
//!
//! Operator-typed fields stay `SocketAddr`, as everywhere else.

use std::net::SocketAddr;
use std::time::Duration;

/// Reserved outbound queue depth per peer, in packets.
///
/// This and [`RtcConfig::send_queue_bytes`] are the **hard admission
/// bound**: `PeerSink::try_send` refuses when either is exhausted, and
/// nothing refuses after acceptance (§2).
pub const DEFAULT_SEND_QUEUE_PACKETS: usize = 256;

/// Reserved outbound queue size per peer, in bytes.
pub const DEFAULT_SEND_QUEUE_BYTES: usize = 256 * 1024;

/// Advisory `buffered_amount` threshold, in bytes.
///
/// **96 KiB, deliberately below str0m's cap.** `str0m` 0.23.1 refuses
/// a write once `MAX_BUFFERED_ACROSS_STREAMS` (128 KiB,
/// `src/sctp/mod.rs:30`) would be exceeded *across all streams*, so the
/// plan's original 256 KiB default could never fire: S0b measured
/// 127 069 post-acceptance refusals with **zero** advisory crossings.
/// A number under the cap is what makes the advisory an input at all.
///
/// It is an input, never the bound: the reading is published by the
/// driver just before each write and is stale for any peer the driver
/// is not currently writing to.
pub const DEFAULT_BUFFERED_AMOUNT_ADVISORY: usize = 96 * 1024;

/// Bounded RTC ingress depth, in packets (§3.4).
pub const DEFAULT_INGRESS_QUEUE_PACKETS: usize = 1024;

/// Default ICE/DataChannel establishment deadline.
pub const DEFAULT_ICE_DEADLINE: Duration = Duration::from_secs(10);

/// Default ceiling on concurrent RTC sessions.
pub const DEFAULT_MAX_PEERS: usize = 256;

/// RTC transport configuration.
#[derive(Debug, Clone)]
pub struct RtcConfig {
    /// Address for the dedicated RTC socket. `None` ⇒ the Net socket's
    /// IP on an ephemeral port, which is what §6 specifies: a second
    /// socket, never a demux of the first.
    pub bind_addr: Option<SocketAddr>,
    /// Address to advertise as the host candidate when the bound
    /// address is not reachable as-is (NAT, container).
    pub public_addr: Option<SocketAddr>,
    /// How long a session may take to reach an open DataChannel.
    pub ice_deadline: Duration,
    /// Ceiling on concurrent RTC sessions.
    pub max_peers: usize,
    /// Hard admission bound: reserved queue slots per peer.
    pub send_queue_packets: usize,
    /// Hard admission bound: reserved queue bytes per peer.
    pub send_queue_bytes: usize,
    /// Advisory `buffered_amount` threshold; see
    /// [`DEFAULT_BUFFERED_AMOUNT_ADVISORY`].
    pub buffered_amount_advisory: usize,
    /// Bounded RTC ingress depth. A full input drops with a counter —
    /// it must never block the driver, because a blocked driver stalls
    /// every peer's `poll_output`.
    pub ingress_queue_packets: usize,
    /// Answer RFC 5389 binding requests on the RTC socket.
    pub serve_stun: bool,
    /// Serve the browser bootstrap listener. **Stage 4 consumes this**;
    /// Stage 3 carries the flag and nothing reads it.
    pub serve_bootstrap: bool,
}

impl Default for RtcConfig {
    fn default() -> Self {
        Self {
            bind_addr: None,
            public_addr: None,
            ice_deadline: DEFAULT_ICE_DEADLINE,
            max_peers: DEFAULT_MAX_PEERS,
            send_queue_packets: DEFAULT_SEND_QUEUE_PACKETS,
            send_queue_bytes: DEFAULT_SEND_QUEUE_BYTES,
            buffered_amount_advisory: DEFAULT_BUFFERED_AMOUNT_ADVISORY,
            ingress_queue_packets: DEFAULT_INGRESS_QUEUE_PACKETS,
            serve_stun: false,
            serve_bootstrap: false,
        }
    }
}

impl RtcConfig {
    /// A config with every default.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the RTC bind address.
    #[must_use]
    #[inline]
    pub fn with_bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = Some(addr);
        self
    }

    /// Answer STUN binding requests on the RTC socket.
    #[must_use]
    #[inline]
    pub fn with_serve_stun(mut self, serve: bool) -> Self {
        self.serve_stun = serve;
        self
    }

    /// The address the driver should bind, given the node's Net bind
    /// address: the configured one, else that IP on an ephemeral port.
    #[inline]
    pub fn resolved_bind_addr(&self, net_bind_addr: SocketAddr) -> SocketAddr {
        self.bind_addr
            .unwrap_or_else(|| SocketAddr::new(net_bind_addr.ip(), 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The advisory default must stay under str0m's across-stream cap,
    /// or it is dead code: S0b saw 127 069 real refusals and zero
    /// advisory crossings with the plan's original 256 KiB.
    #[test]
    fn the_advisory_default_is_below_str0ms_buffer_cap() {
        const STR0M_MAX_BUFFERED_ACROSS_STREAMS: usize = 128 * 1024;
        assert!(
            DEFAULT_BUFFERED_AMOUNT_ADVISORY < STR0M_MAX_BUFFERED_ACROSS_STREAMS,
            "an advisory threshold at or above str0m's {STR0M_MAX_BUFFERED_ACROSS_STREAMS}-byte \
             cap can never fire"
        );
    }

    #[test]
    fn the_default_bind_address_follows_the_net_socket_ip_on_an_ephemeral_port() {
        let net: SocketAddr = "192.0.2.7:4242".parse().expect("addr");
        let resolved = RtcConfig::new().resolved_bind_addr(net);
        assert_eq!(resolved.ip(), net.ip());
        assert_eq!(resolved.port(), 0, "ephemeral: the RTC socket is its own");
        assert_ne!(
            resolved, net,
            "§6: a dedicated socket, never a demux of the Net socket"
        );
    }

    #[test]
    fn an_explicit_bind_address_wins() {
        let net: SocketAddr = "192.0.2.7:4242".parse().expect("addr");
        let explicit: SocketAddr = "127.0.0.1:9999".parse().expect("addr");
        assert_eq!(
            RtcConfig::new()
                .with_bind_addr(explicit)
                .resolved_bind_addr(net),
            explicit
        );
    }
}
