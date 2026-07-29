//! Default-gateway + local-LAN-IP discovery for port mapping.
//!
//! The NAT-PMP client needs the gateway's IPv4 to send the
//! 12-byte UDP probe to; the UPnP client needs the LAN IP the
//! router should forward matched traffic to. Both are
//! OS-routing-table facts; this module resolves them without
//! pulling in a crate.
//!
//! # Platform coverage
//!
//! - **Linux**: parse `/proc/net/route` — text, one line per
//!   route, default route is the one with destination
//!   `0.0.0.0` / hex `00000000`.
//! - **macOS**: shell-out to `/usr/sbin/route -n get default`
//!   and parse the `gateway:` line. The `route` command is in
//!   the base system since System 7.
//! - **Other** (Windows, *BSD, etc.): returns `None`; the
//!   port-mapping sequencer falls back to UPnP-only, which
//!   handles its own SSDP-based discovery.
//!
//! Local LAN IP resolution uses the `UdpSocket::connect` /
//! `local_addr` trick: a UDP "connect" against the gateway
//! primes the OS with a source-address choice; `local_addr`
//! then reports which interface address would be used. This
//! works on every platform tokio supports and matches the LAN
//! IP the router sees as our source.
//!
//! Framing: none of this is load-bearing. If discovery fails,
//! port-mapping is skipped — the mesh still reaches every peer
//! through the routed-handshake path. `docs/internal/plans/PORT_MAPPING_PLAN.md`
//! decision 2 + parent framing apply.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

/// Discover the default IPv4 gateway via the OS routing table.
/// Returns `None` on platforms without an implemented path or
/// when no default route is configured.
pub fn default_ipv4_gateway() -> Option<Ipv4Addr> {
    #[cfg(target_os = "linux")]
    {
        parse_proc_net_route()
    }
    #[cfg(target_os = "macos")]
    {
        run_route_command()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Parse `/proc/net/route` for the default gateway.
///
/// Column layout (tab-separated, header on line 1):
///
/// ```text
/// Iface  Destination  Gateway   Flags  RefCnt  Use  Metric  Mask  MTU  Window  IRTT
/// eth0   00000000     0101A8C0  0003   0       0    100     00000000  0  0  0
/// ```
///
/// Destination + Gateway are little-endian hex of IPv4 bytes —
/// `0101A8C0` parses as `0xC0A80101` in host order → 192.168.1.1.
#[cfg(target_os = "linux")]
fn parse_proc_net_route() -> Option<Ipv4Addr> {
    let content = std::fs::read_to_string("/proc/net/route").ok()?;
    parse_proc_net_route_content(&content)
}

/// Split out so tests can hit the parser without filesystem access.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_net_route_content(content: &str) -> Option<Ipv4Addr> {
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 3 {
            continue;
        }
        // Destination column: "00000000" for the default route.
        if cols[1] != "00000000" {
            continue;
        }
        // Gateway column: little-endian hex of IPv4. Parse as
        // u32 and flip to LE bytes to reconstruct the address.
        //
        // Multiple default-route rows are legitimate: a multi-
        // homed host, a point-to-point interface that emits a
        // `0.0.0.0` gateway (WireGuard, PPP), a transient row
        // during DHCP lease renewal, or a malformed kernel
        // entry. Return the first *usable* row — skip rows
        // whose gateway doesn't parse as hex, and skip
        // all-zero gateways (there's nobody on the other end
        // to send NAT-PMP to). Falling out of the whole scan on
        // the first bad row would strand a perfectly good
        // default route sitting below it.
        let Some(raw) = u32::from_str_radix(cols[2], 16).ok() else {
            continue;
        };
        if raw == 0 {
            continue;
        }
        let bytes = raw.to_le_bytes();
        return Some(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]));
    }
    None
}

/// Shell out to `/usr/sbin/route -n get default` and parse the
/// `gateway:` line.
///
/// Sample output:
///
/// ```text
///    route to: default
/// destination: default
///        mask: default
///     gateway: 192.168.1.1
///   interface: en0
///       flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
/// ```
///
/// We look for a line whose trimmed text starts with `gateway:`
/// and take the rest as an IPv4 string.
#[cfg(target_os = "macos")]
fn run_route_command() -> Option<Ipv4Addr> {
    let output = std::process::Command::new("/usr/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    parse_route_get_default(text)
}

/// Split out so macOS tests can pass a fixture string through
/// the parser without running the `route` command.
#[cfg(any(target_os = "macos", test))]
fn parse_route_get_default(text: &str) -> Option<Ipv4Addr> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("gateway:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Resolve the LAN IPv4 address the OS would use to send traffic
/// to `gateway`. Works cross-platform — `UdpSocket::connect`
/// doesn't actually send a packet; it just primes the kernel's
/// source-address selection, and `local_addr()` returns the
/// bound address after the implicit bind.
///
/// Returns `None` if socket binding fails or the resolved
/// address isn't IPv4 (UPnP is IPv4-only; v6 gateways should
/// degrade to SSDP/UPnP-v6 in a future stage).
pub async fn local_ipv4_for_gateway(gateway: Ipv4Addr) -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    // Port is arbitrary; NAT-PMP is 5351 but we don't actually
    // send anything — this is pure source-address resolution.
    sock.connect(SocketAddr::new(IpAddr::V4(gateway), 5351))
        .await
        .ok()?;
    let local = sock.local_addr().ok()?;
    match local.ip() {
        IpAddr::V4(v4) if !v4.is_unspecified() => Some(IpAddr::V4(v4)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- /proc/net/route parser ----

    #[test]
    fn proc_net_route_parses_default_gateway() {
        // Standard Linux /proc/net/route fixture. Default route
        // on eth0 to 192.168.1.1.
        //
        // Column order: Iface Destination Gateway Flags RefCnt
        // Use Metric Mask MTU Window IRTT.
        let fixture = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth0\t0000A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";
        assert_eq!(
            parse_proc_net_route_content(fixture),
            Some(Ipv4Addr::new(192, 168, 1, 1)),
        );
    }

    #[test]
    fn proc_net_route_skips_non_default_routes() {
        // First route isn't the default (destination != 0). Must
        // scan past it to the default on line 3.
        let fixture = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t0000A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
eth0\t00000000\t010010AC\t0003\t0\t0\t100\t00000000\t0\t0\t0
";
        // 010010AC = little-endian for 172.16.0.1 (AC.10.00.01
        // in network byte order; as u32 LE: 0xAC001001 bytes
        // [0x01, 0x10, 0x00, 0xAC]).
        assert_eq!(
            parse_proc_net_route_content(fixture),
            Some(Ipv4Addr::new(172, 16, 0, 1)),
        );
    }

    #[test]
    fn proc_net_route_returns_none_when_no_default() {
        let fixture = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t0000A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";
        assert!(parse_proc_net_route_content(fixture).is_none());
    }

    #[test]
    fn proc_net_route_handles_empty_file() {
        assert!(parse_proc_net_route_content("").is_none());
        assert!(parse_proc_net_route_content(
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n"
        )
        .is_none());
    }

    /// Regression test for a cubic-flagged P2 bug: the parser
    /// used to abort on the first row where the gateway column
    /// didn't parse as hex. A multi-homed host that carries a
    /// malformed default-route entry above the real one would
    /// return `None` and skip NAT-PMP entirely even though a
    /// valid gateway sat one row further down.
    ///
    /// The fix changes the behavior from "bail on first bad
    /// row" to "skip + continue."
    #[test]
    fn proc_net_route_skips_unparseable_default_row_and_continues() {
        let fixture = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
tun0\t00000000\tNOT_HEX!\t0003\t0\t0\t50\t00000000\t0\t0\t0
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
";
        assert_eq!(
            parse_proc_net_route_content(fixture),
            Some(Ipv4Addr::new(192, 168, 1, 1)),
            "unparseable default-route row must not abort the scan — \
             the eth0 row below should still resolve",
        );
    }

    /// Complement: a zero-gateway default row (point-to-point
    /// interface, transient DHCP state, WireGuard) should be
    /// skipped, not accepted as `0.0.0.0`. Accepting `0.0.0.0`
    /// would make the NAT-PMP client try to talk to the
    /// unspecified address — always a dead channel.
    #[test]
    fn proc_net_route_skips_zero_gateway_row() {
        let fixture = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wg0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
eth0\t00000000\t010010AC\t0003\t0\t0\t100\t00000000\t0\t0\t0
";
        assert_eq!(
            parse_proc_net_route_content(fixture),
            Some(Ipv4Addr::new(172, 16, 0, 1)),
            "zero-gateway default row must not be returned — it's a \
             point-to-point or transient state, not a routable gateway",
        );
    }

    /// Belt-and-braces: the all-zero gateway check doesn't mask
    /// a valid 0.x.x.x gateway above it (0.0.0.0 is the
    /// canonical "unspecified" sentinel — no valid gateway
    /// would share that exact little-endian encoding). Pin the
    /// terminal case: only a zero row followed by nothing else
    /// returns None.
    #[test]
    fn proc_net_route_returns_none_when_only_zero_default_present() {
        let fixture = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wg0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
";
        assert!(
            parse_proc_net_route_content(fixture).is_none(),
            "a single all-zero default-route row should resolve to None, not 0.0.0.0",
        );
    }

    // ---- macOS `route get default` parser ----

    #[test]
    fn route_get_default_parses_gateway() {
        let fixture = "   route to: default
destination: default
       mask: default
    gateway: 192.168.1.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
";
        assert_eq!(
            parse_route_get_default(fixture),
            Some(Ipv4Addr::new(192, 168, 1, 1)),
        );
    }

    #[test]
    fn route_get_default_handles_missing_gateway() {
        let fixture = "   route to: default
destination: default
  interface: en0
";
        assert!(parse_route_get_default(fixture).is_none());
    }

    #[test]
    fn route_get_default_ignores_malformed_ip() {
        let fixture = "gateway: not-an-ip\n";
        assert!(parse_route_get_default(fixture).is_none());
    }

    // ---- local LAN IP resolution ----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_ipv4_for_gateway_resolves_against_loopback() {
        // Loopback is routable from any machine; binding against
        // 127.0.0.1 returns a 127.x.x.x local address.
        let ip = local_ipv4_for_gateway(Ipv4Addr::new(127, 0, 0, 1)).await;
        assert!(ip.is_some(), "loopback should always resolve");
        if let Some(IpAddr::V4(v4)) = ip {
            assert!(v4.is_loopback());
        } else {
            panic!("expected IPv4, got {ip:?}");
        }
    }
}
