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

/// Default for [`RtcConfig::max_provisional`] (§12 global bound).
pub const DEFAULT_MAX_PROVISIONAL: usize = 64;

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
    /// Bind address for a **second UDP socket that answers STUN and
    /// nothing else** — the endpoint this anchor announces as
    /// `rtc_stun_addr`, distinct from `rtc_addr`.
    ///
    /// It is FOR keeping a leaf's STUN server off its peer's ICE
    /// address. libwebrtc's `UDPPort::OnReadPacket` consumes any
    /// datagram arriving from a configured STUN server address as a
    /// STUN-server response *before* `GetConnection` gets to look
    /// for a candidate pair — in both directions — so an
    /// `iceServers` entry pointing at the peer's RTC endpoint eats
    /// that peer's connectivity checks and the pair never nominates.
    /// A separate port makes the two roles separate tuples, which is
    /// what the peer's ICE agent discriminates on.
    ///
    /// `None` ⇒ no second socket is bound and nothing is announced:
    /// emission and binding are off unless configured. Orthogonal to
    /// [`RtcConfig::serve_stun`], which keeps its meaning for the RTC
    /// socket (the diagnostic `UdpBlocked` probe target); this socket
    /// is additional, never a replacement.
    pub stun_addr: Option<SocketAddr>,
    /// The **externally reachable** endpoint to announce for that
    /// socket, when the bind is not reachable as-is (NAT, container)
    /// — `stun_addr`'s counterpart to
    /// [`RtcConfig::public_addr`].
    ///
    /// Unset with a bind configured, the announcement carries the
    /// driver's resolved bound address, so `:0` works and the value
    /// is the actual endpoint rather than an adjacent-port guess.
    pub stun_public_addr: Option<SocketAddr>,
    /// Serve the browser bootstrap listener. Stage 3 carried the
    /// flag and nothing read it; Stage 4b's listener does.
    pub serve_bootstrap: bool,
    /// The **externally reachable** base URL of that listener, e.g.
    /// `https://anchor.example.com`. Stage 4b: this is what
    /// `rtc_bootstrap` carries on the announcement.
    ///
    /// It has to be configured rather than derived: the listener
    /// binds a socket, but a browser needs the name on the
    /// certificate, which no amount of introspecting a bind address
    /// produces. When it is absent the announcement falls back to
    /// Stage 4a's synthesised `https://<addr>/rtc`, which is
    /// honest about being a placeholder — it names the RTC socket,
    /// not a listener.
    pub bootstrap_url: Option<String>,
    /// §12 global bound: concurrent **provisional** sessions this
    /// anchor will hold. Past it the oldest are closed and
    /// reclaimed, counted — an unenrolled session is the cheapest
    /// thing for an attacker to create and the most expendable
    /// thing for an anchor to drop.
    pub max_provisional: usize,
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
            stun_addr: None,
            stun_public_addr: None,
            serve_bootstrap: false,
            bootstrap_url: None,
            max_provisional: DEFAULT_MAX_PROVISIONAL,
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

    /// Bind a second UDP socket that answers STUN only, at `addr`.
    ///
    /// This is the endpoint announced as `rtc_stun_addr` and the one
    /// a leaf's default `iceServers` points at; it must not be the
    /// RTC socket, or a peer's ICE checks are consumed as
    /// STUN-server responses before any candidate pair is
    /// considered. Port 0 is fine: the driver announces what it
    /// actually bound.
    ///
    /// Off unless called: `Default` leaves it `None`, and then no
    /// second socket is bound and nothing is announced.
    #[must_use]
    #[inline]
    pub fn with_stun_addr(mut self, addr: SocketAddr) -> Self {
        self.stun_addr = Some(addr);
        self
    }

    /// Announce `addr` for that socket instead of the address it
    /// bound — the NAT/container case, as
    /// [`RtcConfig::public_addr`] is for `rtc_addr`.
    #[must_use]
    #[inline]
    pub fn with_stun_public_addr(mut self, addr: SocketAddr) -> Self {
        self.stun_public_addr = Some(addr);
        self
    }

    /// Serve the bootstrap listener at this externally reachable
    /// base URL (Stage 4b). Turns `serve_bootstrap` on: a URL to
    /// advertise and no listener would be worse than neither.
    #[must_use]
    #[inline]
    pub fn with_bootstrap_url(mut self, url: impl Into<String>) -> Self {
        self.bootstrap_url = Some(url.into());
        self.serve_bootstrap = true;
        self
    }

    /// The address the driver should bind, given the node's Net bind
    /// address: the configured one, else that IP on an ephemeral port.
    #[inline]
    pub fn resolved_bind_addr(&self, net_bind_addr: SocketAddr) -> SocketAddr {
        self.bind_addr
            .unwrap_or_else(|| SocketAddr::new(net_bind_addr.ip(), 0))
    }

    /// The endpoint this anchor **advertises** as `rtc_addr`, given
    /// the address its RTC socket bound: the operator's override
    /// when there is one, otherwise the bind's own truth.
    #[inline]
    pub fn advertised_rtc_addr(&self, rtc_bound: SocketAddr) -> SocketAddr {
        self.public_addr.unwrap_or(rtc_bound)
    }

    /// The endpoint this anchor **advertises** as `rtc_stun_addr`,
    /// given the address its second socket bound — `None` when no
    /// second socket exists.
    ///
    /// **The bind is checked first, and the override cannot
    /// substitute for it.** `stun_public_addr` says "announce THIS
    /// instead of what the second socket bound"; with no second
    /// socket there is nothing to announce instead of, and
    /// returning the override anyway put an endpoint this anchor
    /// never served into a signed announcement — a browser then
    /// aimed `iceServers` at a port nobody answers, whose only
    /// symptom is a slow ICE failure. The configuration is refused
    /// outright by [`Self::validate`]; this is the same rule at the
    /// emission point, so no path can announce past it.
    #[inline]
    pub fn advertised_stun_addr(&self, stun_bound: Option<SocketAddr>) -> Option<SocketAddr> {
        let bound = stun_bound?;
        Some(self.stun_public_addr.unwrap_or(bound))
    }

    /// Everything about this configuration that must refuse a
    /// startup, in one call: `Some(explanation)` when the anchor
    /// would come up unable to serve what it announces.
    ///
    /// Fail-fast, before anything is bound. An anchor that comes up
    /// with either defect below has one symptom at the peer — an
    /// ICE deadline — and none at all locally.
    pub fn validate(&self) -> Option<String> {
        self.unserved_stun_endpoint()
            .or_else(|| self.stun_endpoint_conflict())
    }

    /// `Some(explanation)` when a STUN endpoint is announced that no
    /// socket serves: `stun_public_addr` set with no `stun_addr`.
    ///
    /// The two flags are an endpoint and its NAT override, not two
    /// ways of spelling the same thing. `stun_addr` is what binds;
    /// without it the driver opens no second socket, so the
    /// override names a port this anchor does not answer on. There
    /// is no port to guess and nothing to silently strip: the
    /// operator meant to serve STUN and one of the two flags is
    /// missing, which is exactly what fail-fast validation is for.
    pub fn unserved_stun_endpoint(&self) -> Option<String> {
        let public = self.stun_public_addr?;
        if self.stun_addr.is_some() {
            return None;
        }
        Some(format!(
            "rtc: stun_public_addr ({public}) is set without stun_addr, so no second socket is \
             bound and this anchor would announce a STUN endpoint it never serves; set \
             stun_addr (`:0` is fine — the announcement carries what it actually bound) or \
             drop the override"
        ))
    }

    /// The configuration §6.12.1 describes, detected before anything
    /// is bound: **one endpoint in both roles**.
    ///
    /// `Some(explanation)` when this anchor would announce, or bind,
    /// a single UDP endpoint as both its RTC address and its STUN
    /// address. A peer cannot be its own STUN server: libwebrtc's
    /// `UDPPort::OnReadPacket` consumes any datagram arriving from a
    /// configured STUN server as a STUN-server response before it
    /// looks for a candidate pair, so the two roles on one endpoint
    /// eat the peer's connectivity checks and ICE never nominates.
    ///
    /// Detected equality only, and only between values this config
    /// holds — but between the **resolved** values, not merely the
    /// explicitly-paired ones. It cannot see a `stun_public_addr`
    /// that a gateway maps onto `public_addr`, nor a DNS name that
    /// resolves to either — those are the boundary the leaf's
    /// connect-time check and the documentation own.
    pub fn stun_endpoint_conflict(&self) -> Option<String> {
        // The explicitly announced pair first: it is the one a peer
        // acts on, the one that produces a silent 60-second ICE
        // timeout instead of a diagnostic, and the one whose
        // diagnostic can name the two fields the operator set.
        if let (Some(stun), Some(rtc)) = (self.stun_public_addr, self.public_addr) {
            if stun == rtc {
                return Some(format!(
                    "rtc: stun_public_addr ({stun}) is the announced RTC endpoint public_addr \
                     ({rtc}); they must be distinct UDP endpoints, because a peer cannot be its \
                     own STUN server — libwebrtc consumes datagrams from a configured STUN \
                     server before pairing, so the peer's ICE checks would be eaten"
                ));
            }
        }
        // The same mistake one level down. Caught here so it reads as
        // the configuration error it is, rather than as the
        // `AddrInUse` the second bind would produce.
        //
        // Port 0 is exempt: it names no endpoint, it asks the OS for
        // one. Two `ip:0` binds compare equal and are nonetheless
        // two different sockets — refusing them would refuse the
        // ordinary test and container configuration.
        if let (Some(stun), Some(rtc)) = (self.stun_addr, self.bind_addr) {
            if stun == rtc && rtc.port() != 0 {
                return Some(format!(
                    "rtc: stun_addr ({stun}) is the RTC bind_addr ({rtc}); they must be distinct \
                     UDP endpoints, because a peer cannot be its own STUN server — libwebrtc \
                     consumes datagrams from a configured STUN server before pairing, so the \
                     peer's ICE checks would be eaten"
                ));
            }
        }
        // **The RESOLVED announced pair**, which neither arm above
        // can see. Comparing public with public and bind with bind
        // compares like with like; an RTC `public_addr` of
        // `127.0.0.1:7101` beside a STUN *bind* of `127.0.0.1:7101`
        // announces the very same endpoint in both roles, because
        // each half falls back to the other level. That is a known,
        // deliberate collision and it escaped the check entirely.
        //
        // Third rather than first so the two arms above keep naming
        // the fields the operator actually set: a diagnostic that
        // says "the announced STUN endpoint" where it could say
        // "stun_public_addr" is a worse diagnostic.
        let rtc_announced = self.public_addr.or(self.bind_addr);
        let stun_announced = self.stun_public_addr.or(self.stun_addr);
        if let (Some(stun), Some(rtc)) = (stun_announced, rtc_announced) {
            // Port 0 names no endpoint pre-bind;
            // `resolved_endpoint_conflict` covers what it resolves
            // to once the sockets exist.
            if stun == rtc && rtc.port() != 0 {
                let stun_field = if self.stun_public_addr.is_some() {
                    "stun_public_addr"
                } else {
                    "stun_addr"
                };
                let rtc_field = if self.public_addr.is_some() {
                    "public_addr"
                } else {
                    "bind_addr"
                };
                return Some(format!(
                    "rtc: the announced STUN endpoint {stun_field} ({stun}) resolves to the \
                     announced RTC endpoint {rtc_field} ({rtc}); they must be distinct UDP \
                     endpoints, because a peer cannot be its own STUN server — libwebrtc \
                     consumes datagrams from a configured STUN server before pairing, so the \
                     peer's ICE checks would be eaten"
                ));
            }
        }
        None
    }

    /// The same rule against the pair this anchor will **actually**
    /// advertise, once both sockets are bound.
    ///
    /// What the pre-bind check cannot see: a `:0` STUN bind that the
    /// OS happens to place on the port an RTC `public_addr` names.
    /// Port 0 is exempt before binding because it names no
    /// endpoint; afterwards it names exactly one, so the check that
    /// matters is this one, and it is the last thing between a
    /// resolved pair and a signed announcement.
    pub fn resolved_endpoint_conflict(
        &self,
        rtc_bound: SocketAddr,
        stun_bound: Option<SocketAddr>,
    ) -> Option<String> {
        let stun = self.advertised_stun_addr(stun_bound)?;
        let rtc = self.advertised_rtc_addr(rtc_bound);
        if stun != rtc {
            return None;
        }
        Some(format!(
            "rtc: the resolved STUN endpoint ({stun}) is the resolved RTC endpoint ({rtc}); they \
             must be distinct UDP endpoints, because a peer cannot be its own STUN server — \
             libwebrtc consumes datagrams from a configured STUN server before pairing, so the \
             peer's ICE checks would be eaten"
        ))
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
        // A `const` comparison, deliberately made a runtime assertion
        // so the message travels with the failure: if someone raises
        // the default past str0m's cap, the advisory silently stops
        // firing — which is exactly what S0b measured (127 069 real
        // refusals, zero advisory crossings).
        let advisory = std::hint::black_box(DEFAULT_BUFFERED_AMOUNT_ADVISORY);
        assert!(
            advisory < STR0M_MAX_BUFFERED_ACROSS_STREAMS,
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
