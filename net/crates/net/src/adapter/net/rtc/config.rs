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
    /// Bind address of a **second, IPv6 RTC socket** beside the one
    /// [`Self::bind_addr`] names — a dual-stack anchor
    /// (`ANCHOR_DUAL_STACK_PLAN.md`).
    ///
    /// A browser can only reach the families its network routes, so
    /// an anchor with one socket locks out every player who cannot
    /// reach that socket's family. With this set, every session
    /// offers a host candidate per family and ICE picks the one that
    /// works; sessions, identity and relay stay shared.
    ///
    /// Two sockets rather than one dual-stack `[::]` socket: IPv4
    /// peers on a dual-stack socket arrive as IPv4-mapped addresses
    /// (`::ffff:a.b.c.d`) that never equal the candidate a browser
    /// signalled, and `IPV6_V6ONLY` defaults differently per
    /// platform. The socket is bound with `IPV6_V6ONLY` set
    /// explicitly, so it never claims the IPv4 port beside it.
    ///
    /// Must be IPv6, and the primary socket must then be IPv4
    /// ([`Self::validate`]). `None` (the default) binds nothing and
    /// advertises nothing: an anchor configured without it behaves
    /// exactly as before.
    pub bind_addr_v6: Option<SocketAddr>,
    /// The externally reachable IPv6 endpoint to advertise for that
    /// socket — [`Self::public_addr`]'s counterpart. Unset with a
    /// bind, the socket's own bound address is advertised. Set
    /// without [`Self::bind_addr_v6`], it names a socket nobody
    /// binds, and [`Self::validate`] refuses it.
    pub public_addr_v6: Option<SocketAddr>,
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
    /// Bind address of an **IPv6** STUN-only socket, beside the one
    /// [`Self::stun_addr`] names — a dual-stack anchor's STUN endpoint
    /// in the other family.
    ///
    /// Needed, not optional, for an IPv6-only player whose browser
    /// does not enumerate interfaces (Chromium without a media
    /// permission): its only usable local candidate is a
    /// server-reflexive one, and it can only gather that from a STUN
    /// server of its own family. With nothing but an IPv4 STUN
    /// endpoint announced, such a browser pairs nothing and sends no
    /// ICE check at all — measured in natsim
    /// (`ANCHOR_DUAL_STACK_PLAN.md`, slice 4). Bound `IPV6_V6ONLY`;
    /// `None` binds and announces nothing.
    pub stun_addr_v6: Option<SocketAddr>,
    /// The externally reachable endpoint to announce for that socket —
    /// [`Self::stun_public_addr`]'s counterpart.
    pub stun_public_addr_v6: Option<SocketAddr>,
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
            bind_addr_v6: None,
            public_addr_v6: None,
            ice_deadline: DEFAULT_ICE_DEADLINE,
            max_peers: DEFAULT_MAX_PEERS,
            send_queue_packets: DEFAULT_SEND_QUEUE_PACKETS,
            send_queue_bytes: DEFAULT_SEND_QUEUE_BYTES,
            buffered_amount_advisory: DEFAULT_BUFFERED_AMOUNT_ADVISORY,
            ingress_queue_packets: DEFAULT_INGRESS_QUEUE_PACKETS,
            serve_stun: false,
            stun_addr: None,
            stun_public_addr: None,
            stun_addr_v6: None,
            stun_public_addr_v6: None,
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

    /// Bind a second, IPv6 RTC socket at `addr` (dual-stack anchor).
    /// Port 0 is fine: the advertised address is what it bound.
    #[must_use]
    #[inline]
    pub fn with_bind_addr_v6(mut self, addr: SocketAddr) -> Self {
        self.bind_addr_v6 = Some(addr);
        self
    }

    /// Advertise `addr` for the IPv6 RTC socket instead of the
    /// address it bound — the NAT/container case.
    #[must_use]
    #[inline]
    pub fn with_public_addr_v6(mut self, addr: SocketAddr) -> Self {
        self.public_addr_v6 = Some(addr);
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

    /// Bind an IPv6 STUN-only socket at `addr` (dual-stack anchor).
    #[must_use]
    #[inline]
    pub fn with_stun_addr_v6(mut self, addr: SocketAddr) -> Self {
        self.stun_addr_v6 = Some(addr);
        self
    }

    /// Announce `addr` for the IPv6 STUN socket instead of what it
    /// bound.
    #[must_use]
    #[inline]
    pub fn with_stun_public_addr_v6(mut self, addr: SocketAddr) -> Self {
        self.stun_public_addr_v6 = Some(addr);
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

    /// The endpoint this anchor advertises for its **IPv6** RTC
    /// socket, given what that socket bound — `None` when there is
    /// no such socket. The same bind-first rule as
    /// [`Self::advertised_stun_addr`]: the override never stands in
    /// for a socket that does not exist.
    #[inline]
    pub fn advertised_rtc_addr_v6(&self, v6_bound: Option<SocketAddr>) -> Option<SocketAddr> {
        let bound = v6_bound?;
        Some(self.public_addr_v6.unwrap_or(bound))
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

    /// The endpoint announced for the **IPv6** STUN socket, given what
    /// it bound — `None` without one. Bind first, as every other
    /// endpoint here.
    #[inline]
    pub fn advertised_stun_addr_v6(&self, stun_v6_bound: Option<SocketAddr>) -> Option<SocketAddr> {
        let bound = stun_v6_bound?;
        Some(self.stun_public_addr_v6.unwrap_or(bound))
    }

    /// Everything about this configuration that must refuse a
    /// startup, in one call: `Some(explanation)` when the anchor
    /// would come up unable to serve what it announces.
    ///
    /// Fail-fast, before anything is bound. An anchor that comes up
    /// with either defect below has one symptom at the peer — an
    /// ICE deadline — and none at all locally.
    pub fn validate(&self) -> Option<String> {
        self.dual_stack_conflict()
            .or_else(|| self.dual_stack_stun_conflict())
            .or_else(|| self.unserved_stun_endpoint())
            .or_else(|| self.stun_endpoint_conflict())
    }

    /// The IPv6 STUN socket's rules: an override needs its socket,
    /// both must be IPv6, the primary STUN socket (when set) must then
    /// be IPv4, and it may not be the IPv6 RTC endpoint (Stage 6's
    /// rule: a peer cannot be its own STUN server).
    pub fn dual_stack_stun_conflict(&self) -> Option<String> {
        if let Some(public) = self.stun_public_addr_v6 {
            if self.stun_addr_v6.is_none() {
                return Some(format!(
                    "rtc: stun_public_addr_v6 ({public}) is set without stun_addr_v6, so no IPv6 \
                     STUN socket is bound and this anchor would announce an endpoint it never serves"
                ));
            }
            if !public.is_ipv6() {
                return Some(format!(
                    "rtc: stun_public_addr_v6 ({public}) is not an IPv6 address"
                ));
            }
        }
        let bind = self.stun_addr_v6?;
        if !bind.is_ipv6() {
            return Some(format!(
                "rtc: stun_addr_v6 ({bind}) is not an IPv6 address; the second STUN socket \
                 carries IPv6"
            ));
        }
        for (field, addr) in [
            ("stun_addr", self.stun_addr),
            ("stun_public_addr", self.stun_public_addr),
        ] {
            if let Some(addr) = addr {
                if !addr.is_ipv4() {
                    return Some(format!(
                        "rtc: with stun_addr_v6 set, {field} ({addr}) must be IPv4: one family \
                         per STUN socket"
                    ));
                }
            }
        }
        let stun6_announced = self.stun_public_addr_v6.or(self.stun_addr_v6);
        let rtc6_announced = self.public_addr_v6.or(self.bind_addr_v6);
        for (stun6, rtc6) in [
            (stun6_announced, rtc6_announced),
            (self.stun_addr_v6, self.bind_addr_v6),
        ] {
            if let (Some(stun6), Some(rtc6)) = (stun6, rtc6) {
                if stun6 == rtc6 && rtc6.port() != 0 {
                    return Some(format!(
                        "rtc: the IPv6 STUN endpoint ({stun6}) is the IPv6 RTC endpoint ({rtc6}); \
                         they must be distinct UDP endpoints, because a peer cannot be its own \
                         STUN server"
                    ));
                }
            }
        }
        None
    }

    /// [`Self::dual_stack_stun_conflict`]'s collision rule against the
    /// resolved IPv6 sockets, once both exist.
    pub fn resolved_v6_stun_conflict(
        &self,
        rtc_v6_bound: Option<SocketAddr>,
        stun_v6_bound: Option<SocketAddr>,
    ) -> Option<String> {
        let stun6 = self.advertised_stun_addr_v6(stun_v6_bound)?;
        let rtc6 = self.advertised_rtc_addr_v6(rtc_v6_bound)?;
        (stun6 == rtc6).then(|| {
            format!(
                "rtc: the resolved IPv6 STUN endpoint ({stun6}) is the resolved IPv6 RTC \
                 endpoint ({rtc6}); they must be distinct UDP endpoints"
            )
        })
    }

    /// `Some(explanation)` when the IPv6 RTC socket is configured in a
    /// way the anchor cannot serve: an override with no socket
    /// behind it, an address of the wrong family, or a primary socket
    /// that is not IPv4.
    ///
    /// One family per socket is the whole design: the driver routes a
    /// datagram to the socket of its destination's family, so two
    /// sockets of one family would leave that choice ambiguous and an
    /// IPv6 primary would make the second socket a duplicate.
    ///
    /// This sees only what is set. A primary left to default follows
    /// the Net socket's IP, which is known only at spawn;
    /// [`Self::dual_stack_primary_conflict`] checks that.
    pub fn dual_stack_conflict(&self) -> Option<String> {
        if let Some(public) = self.public_addr_v6 {
            if self.bind_addr_v6.is_none() {
                return Some(format!(
                    "rtc: public_addr_v6 ({public}) is set without bind_addr_v6, so no IPv6 \
                     socket is bound and this anchor would advertise an endpoint it never \
                     serves; set bind_addr_v6 (`[::]:0` is fine) or drop the override"
                ));
            }
            if !public.is_ipv6() {
                return Some(format!(
                    "rtc: public_addr_v6 ({public}) is not an IPv6 address; it advertises the \
                     IPv6 RTC socket"
                ));
            }
        }
        let bind = self.bind_addr_v6?;
        if !bind.is_ipv6() {
            return Some(format!(
                "rtc: bind_addr_v6 ({bind}) is not an IPv6 address; the second RTC socket \
                 carries IPv6 and the primary one carries IPv4"
            ));
        }
        for (field, addr) in [
            ("bind_addr", self.bind_addr),
            ("public_addr", self.public_addr),
        ] {
            if let Some(addr) = addr {
                if !addr.is_ipv4() {
                    return Some(format!(
                        "rtc: with bind_addr_v6 set, {field} ({addr}) must be IPv4: the primary \
                         RTC socket carries IPv4 and the second carries IPv6, one family each"
                    ));
                }
            }
        }
        None
    }

    /// [`Self::dual_stack_conflict`]'s rule against the primary
    /// socket's **resolved** bind, which a default `bind_addr` takes
    /// from the Net socket's IP and so cannot be checked before
    /// spawn.
    pub fn dual_stack_primary_conflict(&self, primary_bind: SocketAddr) -> Option<String> {
        self.bind_addr_v6?;
        if primary_bind.is_ipv4() {
            return None;
        }
        Some(format!(
            "rtc: with bind_addr_v6 set, the primary RTC socket must be IPv4, but it resolves to \
             {primary_bind} (the Net socket's IP); set bind_addr to an IPv4 address"
        ))
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
        // **The IPv6 RTC socket is an RTC endpoint too.** A dual-stack
        // anchor has two, and a STUN endpoint on either one eats that
        // family's connectivity checks exactly as above. Announced
        // level first, bind level second, as for the primary.
        let rtc6_announced = self.public_addr_v6.or(self.bind_addr_v6);
        for (stun, rtc6) in [
            (stun_announced, rtc6_announced),
            (self.stun_addr, self.bind_addr_v6),
        ] {
            if let (Some(stun), Some(rtc6)) = (stun, rtc6) {
                if stun == rtc6 && rtc6.port() != 0 {
                    return Some(format!(
                        "rtc: the STUN endpoint ({stun}) is the IPv6 RTC endpoint ({rtc6}); they \
                         must be distinct UDP endpoints, because a peer cannot be its own STUN \
                         server — libwebrtc consumes datagrams from a configured STUN server \
                         before pairing, so the peer's ICE checks would be eaten"
                    ));
                }
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

    /// [`Self::resolved_endpoint_conflict`] for the **IPv6** RTC
    /// socket: the resolved STUN endpoint may not be the IPv6 RTC
    /// endpoint either, and a `:0` bind is only comparable once both
    /// sockets exist.
    pub fn resolved_v6_endpoint_conflict(
        &self,
        v6_bound: Option<SocketAddr>,
        stun_bound: Option<SocketAddr>,
    ) -> Option<String> {
        let stun = self.advertised_stun_addr(stun_bound)?;
        let rtc6 = self.advertised_rtc_addr_v6(v6_bound)?;
        if stun != rtc6 {
            return None;
        }
        Some(format!(
            "rtc: the resolved STUN endpoint ({stun}) is the resolved IPv6 RTC endpoint ({rtc6}); \
             they must be distinct UDP endpoints, because a peer cannot be its own STUN server"
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

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("addr")
    }

    #[test]
    fn a_config_without_an_ipv6_socket_is_unchanged() {
        let config = RtcConfig::new().with_bind_addr(addr("127.0.0.1:7101"));
        assert_eq!(config.validate(), None);
        assert_eq!(config.advertised_rtc_addr_v6(None), None);
        assert_eq!(
            config.dual_stack_primary_conflict(addr("[::1]:0")),
            None,
            "an IPv6 primary is only a conflict when a second socket exists"
        );
    }

    #[test]
    fn a_dual_stack_config_validates_and_advertises_its_override() {
        let config = RtcConfig::new()
            .with_bind_addr(addr("0.0.0.0:7101"))
            .with_bind_addr_v6(addr("[::]:7101"))
            .with_public_addr_v6(addr("[2001:db8::7]:7101"));
        assert_eq!(config.validate(), None);
        assert_eq!(
            config.advertised_rtc_addr_v6(Some(addr("[::]:7101"))),
            Some(addr("[2001:db8::7]:7101"))
        );
        assert_eq!(
            config.advertised_rtc_addr_v6(None),
            None,
            "the override never stands in for a socket that does not exist"
        );
    }

    #[test]
    fn each_misconfigured_ipv6_socket_is_refused() {
        let refused = [
            (
                "override without a socket",
                RtcConfig::new().with_public_addr_v6(addr("[2001:db8::1]:1")),
            ),
            (
                "an IPv4 override",
                RtcConfig::new()
                    .with_bind_addr_v6(addr("[::]:0"))
                    .with_public_addr_v6(addr("198.51.100.1:1")),
            ),
            (
                "an IPv4 bind",
                RtcConfig::new().with_bind_addr_v6(addr("0.0.0.0:0")),
            ),
            (
                "an IPv6 primary bind",
                RtcConfig::new()
                    .with_bind_addr(addr("[::1]:0"))
                    .with_bind_addr_v6(addr("[::1]:0")),
            ),
            ("an IPv6 primary override", {
                let mut config = RtcConfig::new()
                    .with_bind_addr(addr("127.0.0.1:0"))
                    .with_bind_addr_v6(addr("[::1]:0"));
                config.public_addr = Some(addr("[2001:db8::1]:1"));
                config
            }),
        ];
        for (what, config) in refused {
            assert!(config.validate().is_some(), "{what} must be refused");
        }
        assert!(
            RtcConfig::new()
                .with_bind_addr_v6(addr("[::]:0"))
                .dual_stack_primary_conflict(addr("[::]:0"))
                .is_some(),
            "a primary that resolves to IPv6 is refused once a second socket exists"
        );
    }

    /// Stage 6's rule, one socket further: a STUN endpoint may not be
    /// the IPv6 RTC endpoint either, announced or bound, pre-bind or
    /// resolved.
    #[test]
    fn a_stun_endpoint_on_the_ipv6_rtc_endpoint_is_refused() {
        let base = || {
            RtcConfig::new()
                .with_bind_addr(addr("127.0.0.1:7101"))
                .with_bind_addr_v6(addr("[::1]:7101"))
        };
        assert!(base()
            .with_stun_addr(addr("[::1]:7101"))
            .validate()
            .is_some());
        assert!(base()
            .with_public_addr_v6(addr("[2001:db8::7]:7101"))
            .with_stun_addr(addr("[::1]:3479"))
            .with_stun_public_addr(addr("[2001:db8::7]:7101"))
            .validate()
            .is_some());
        let resolved = base().with_stun_addr(addr("[::1]:0"));
        assert_eq!(
            resolved.validate(),
            None,
            "port 0 names no endpoint before binding"
        );
        assert!(resolved
            .resolved_v6_endpoint_conflict(Some(addr("[::1]:7101")), Some(addr("[::1]:7101")))
            .is_some());
        assert_eq!(
            resolved
                .resolved_v6_endpoint_conflict(Some(addr("[::1]:7101")), Some(addr("[::1]:3479"))),
            None
        );
    }

    /// The IPv6 STUN-only socket: an override needs its socket, both
    /// are IPv6, the primary STUN socket is then IPv4, and it may not
    /// be the IPv6 RTC endpoint, pre-bind or resolved.
    #[test]
    fn the_ipv6_stun_socket_follows_the_per_family_rules() {
        let base = || {
            RtcConfig::new()
                .with_bind_addr(addr("127.0.0.1:7101"))
                .with_bind_addr_v6(addr("[::1]:7101"))
                .with_stun_addr(addr("127.0.0.1:3479"))
        };
        assert_eq!(
            base().with_stun_addr_v6(addr("[::1]:3479")).validate(),
            None
        );
        for (what, config) in [
            (
                "override without a socket",
                base().with_stun_public_addr_v6(addr("[2001:db8::7]:3479")),
            ),
            (
                "an IPv4 bind",
                base().with_stun_addr_v6(addr("127.0.0.1:3480")),
            ),
            (
                "the IPv6 RTC endpoint",
                base().with_stun_addr_v6(addr("[::1]:7101")),
            ),
            (
                "an IPv6 primary STUN socket",
                RtcConfig::new()
                    .with_bind_addr(addr("127.0.0.1:7101"))
                    .with_stun_addr(addr("[::1]:3479"))
                    .with_stun_addr_v6(addr("[::1]:3480")),
            ),
        ] {
            assert!(config.validate().is_some(), "{what} must be refused");
        }
        let resolved = base().with_stun_addr_v6(addr("[::1]:0"));
        assert!(resolved
            .resolved_v6_stun_conflict(Some(addr("[::1]:7101")), Some(addr("[::1]:7101")))
            .is_some());
        assert_eq!(
            resolved.resolved_v6_stun_conflict(Some(addr("[::1]:7101")), Some(addr("[::1]:3479"))),
            None
        );
        assert_eq!(
            resolved.advertised_stun_addr_v6(None),
            None,
            "the override never stands in for a socket that does not exist"
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
