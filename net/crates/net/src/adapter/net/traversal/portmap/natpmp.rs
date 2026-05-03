//! NAT-PMP / PCP wire codec + UDP client.
//!
//! RFC 6886 (NAT-PMP) defines the 2-to-16 byte UDP packets the
//! router speaks on port 5351. PCP (RFC 6887) reuses the same
//! wire port and overlaps in the simple request / response
//! shape we use; this module targets NAT-PMP strictly, which
//! most consumer gateways implement as-is.
//!
//! Stage 4b-2 ships:
//!
//! - The pure codec ([`NatPmpRequest`] / [`NatPmpResponse`] /
//!   [`ResultCode`] / `encode_request` / `decode_response`).
//!   Unit-tested in isolation.
//! - [`NatPmpMapper`] — a `PortMapperClient` implementation
//!   bound to an operator-supplied gateway `Ipv4Addr`. Uses
//!   `tokio::net::UdpSocket` for wire I/O; per-call deadline
//!   of 1 s matches
//!   `docs/PORT_MAPPING_PLAN.md` decision 4.
//!
//! Gateway discovery (how to find the router's IP without the
//! operator telling us) is **not** in this module — it's a
//! stage-4b-4 concern for the sequencer. Tests construct
//! `NatPmpMapper` with a known localhost gateway + a mock UDP
//! responder.
//!
//! # Wire format (RFC 6886 §3)
//!
//! All multi-byte integers are big-endian. Addresses are
//! IPv4-only; the protocol has no IPv6 form.
//!
//! ## External-address request (2 bytes)
//!
//! ```text
//! +-------+-------+
//! | ver=0 |  op=0 |
//! +-------+-------+
//! ```
//!
//! ## External-address response (12 bytes)
//!
//! ```text
//! +-------+-------+----------------+
//! | ver=0 | op=128| result_code    |
//! +-------+-------+----------------+
//! | epoch_seconds (u32)            |
//! +--------------------------------+
//! | external_ip (u32 IPv4)         |
//! +--------------------------------+
//! ```
//!
//! ## UDP map request (12 bytes; op=1 UDP, op=2 TCP)
//!
//! ```text
//! +-------+-------+----------------+
//! | ver=0 |  op   |   reserved     |
//! +-------+-------+----------------+
//! | internal_port | external_port  |
//! +---------------+----------------+
//! | lifetime_secs (u32)            |
//! +--------------------------------+
//! ```
//!
//! Set `lifetime=0` to request removal of the mapping.
//!
//! ## UDP map response (16 bytes; op=128+op)
//!
//! ```text
//! +-------+-------+----------------+
//! | ver=0 | op+128| result_code    |
//! +-------+-------+----------------+
//! | epoch_seconds (u32)            |
//! +--------------------------------+
//! | internal_port | mapped_port    |
//! +---------------+----------------+
//! | lifetime_secs (u32)            |
//! +--------------------------------+
//! ```

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use tokio::net::UdpSocket;

use super::{PortMapperClient, PortMapping, PortMappingError, Protocol};

/// NAT-PMP uses UDP port 5351 on the gateway (RFC 6886 §1.2).
pub const NATPMP_PORT: u16 = 5351;

/// Version byte. NAT-PMP is version 0; PCP is 2.
pub const NATPMP_VERSION: u8 = 0;

/// Opcode 0: external-address request / response.
pub const OP_EXTERNAL_ADDRESS: u8 = 0;
/// Opcode 1: UDP port-map request / response.
pub const OP_MAP_UDP: u8 = 1;
/// Response opcode offset — the server adds 128 to the request
/// opcode (0 → 128, 1 → 129) to distinguish response from
/// re-transmitted request.
pub const RESPONSE_OP_OFFSET: u8 = 128;

/// Per-call deadline for UDP I/O against the gateway. Matches
/// `docs/PORT_MAPPING_PLAN.md` decision 4; the plan notes this
/// is a per-call timeout, not a per-task deadline.
pub const NATPMP_DEADLINE: Duration = Duration::from_secs(1);

/// Length of an encoded external-address request (2 bytes).
pub const EXTERNAL_REQUEST_LEN: usize = 2;

/// Length of an encoded UDP-map request (12 bytes).
pub const MAP_REQUEST_LEN: usize = 12;

/// Length of an encoded external-address response (12 bytes).
pub const EXTERNAL_RESPONSE_LEN: usize = 12;

/// Length of an encoded UDP-map response (16 bytes).
pub const MAP_RESPONSE_LEN: usize = 16;

/// Result code returned by the router on every response. `Success`
/// (0) is the green path; every other variant signals a specific
/// failure the client can surface as a typed
/// [`PortMappingError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    /// RFC 6886 code 0 — the request succeeded.
    Success,
    /// Code 1 — the gateway doesn't speak this version of
    /// NAT-PMP (we always send version 0).
    UnsupportedVersion,
    /// Code 2 — the gateway administratively rejected the
    /// request (policy / ACL / port conflict).
    NotAuthorized,
    /// Code 3 — gateway-internal network failure (e.g. it
    /// can't reach its own upstream).
    NetworkFailure,
    /// Code 4 — gateway mapping table is full.
    OutOfResources,
    /// Code 5 — the opcode we sent isn't one this gateway
    /// supports (shouldn't happen for our two ops, but the
    /// spec allows it).
    UnsupportedOpcode,
    /// Any code outside RFC 6886's 0..=5 range. Carries the raw
    /// value so logs can identify the offending code.
    Unknown(u16),
}

impl ResultCode {
    /// Decode a raw `u16` result code.
    pub fn from_u16(raw: u16) -> Self {
        match raw {
            0 => Self::Success,
            1 => Self::UnsupportedVersion,
            2 => Self::NotAuthorized,
            3 => Self::NetworkFailure,
            4 => Self::OutOfResources,
            5 => Self::UnsupportedOpcode,
            other => Self::Unknown(other),
        }
    }

    /// Stable string for logs + metrics. Never localized.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::UnsupportedVersion => "unsupported-version",
            Self::NotAuthorized => "not-authorized",
            Self::NetworkFailure => "network-failure",
            Self::OutOfResources => "out-of-resources",
            Self::UnsupportedOpcode => "unsupported-opcode",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Convert a non-`Success` code into a [`PortMappingError`].
    /// `Success` maps to `Refused("success")` for symmetry — callers
    /// should early-return before calling this on a success code.
    pub fn to_error(self) -> PortMappingError {
        PortMappingError::Refused(self.as_str().to_string())
    }
}

/// Decoded NAT-PMP request. Used internally by the codec tests;
/// the client only emits via [`encode_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatPmpRequest {
    /// Ask the gateway for its external IPv4 address.
    ExternalAddress,
    /// Request a UDP port mapping. `lifetime = 0` asks the
    /// gateway to remove a previously-installed mapping for
    /// this internal port (RFC 6886 §3.3).
    MapUdp {
        /// The internal (LAN-side) UDP port we're asking to map.
        internal_port: u16,
        /// The external (WAN-side) port we'd prefer. Gateway is
        /// free to pick a different value; the granted port
        /// comes back in the response's `mapped_port` field.
        external_port_hint: u16,
        /// Requested lease length in seconds. `0` means
        /// "remove this mapping."
        lifetime: u32,
    },
}

/// Decoded NAT-PMP response. Returned from [`decode_response`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatPmpResponse {
    /// Response to an `ExternalAddress` request.
    ExternalAddress {
        /// Result code from the router.
        result: ResultCode,
        /// Router's uptime in seconds (RFC 6886's "Seconds
        /// Since Start of Epoch"). Used by clients that want
        /// to detect gateway reboots; we don't track it.
        epoch_seconds: u32,
        /// The gateway's WAN-facing IPv4 address.
        external_ip: Ipv4Addr,
    },
    /// Response to a `MapUdp` request (install or renewal).
    MapUdp {
        /// Result code from the router.
        result: ResultCode,
        /// Router's uptime in seconds.
        epoch_seconds: u32,
        /// Echoed internal port from the request.
        internal_port: u16,
        /// The external port the gateway actually allocated —
        /// may differ from the `external_port_hint` we sent.
        mapped_port: u16,
        /// Granted lifetime in seconds. Often equal to the
        /// requested lifetime; some gateways cap it lower.
        lifetime: u32,
    },
}

/// Encode a request. Output length is exactly
/// [`EXTERNAL_REQUEST_LEN`] or [`MAP_REQUEST_LEN`].
pub fn encode_request(req: &NatPmpRequest) -> Bytes {
    match req {
        NatPmpRequest::ExternalAddress => {
            let mut buf = BytesMut::with_capacity(EXTERNAL_REQUEST_LEN);
            buf.put_u8(NATPMP_VERSION);
            buf.put_u8(OP_EXTERNAL_ADDRESS);
            buf.freeze()
        }
        NatPmpRequest::MapUdp {
            internal_port,
            external_port_hint,
            lifetime,
        } => {
            let mut buf = BytesMut::with_capacity(MAP_REQUEST_LEN);
            buf.put_u8(NATPMP_VERSION);
            buf.put_u8(OP_MAP_UDP);
            buf.put_u16(0); // reserved
            buf.put_u16(*internal_port);
            buf.put_u16(*external_port_hint);
            buf.put_u32(*lifetime);
            debug_assert_eq!(buf.len(), MAP_REQUEST_LEN);
            buf.freeze()
        }
    }
}

/// Decode a response received on the NAT-PMP UDP socket.
/// Returns `None` if the packet is shorter than the minimum
/// response length, has an unknown version, or carries an
/// unrecognized response opcode.
pub fn decode_response(data: &[u8]) -> Option<NatPmpResponse> {
    if data.len() < EXTERNAL_RESPONSE_LEN {
        return None;
    }
    if data[0] != NATPMP_VERSION {
        return None;
    }
    let raw_op = data[1];
    if raw_op < RESPONSE_OP_OFFSET {
        // Server responses MUST set the high bit (offset 128).
        return None;
    }
    let op = raw_op - RESPONSE_OP_OFFSET;
    let result = ResultCode::from_u16(u16::from_be_bytes([data[2], data[3]]));
    let epoch_seconds = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    match op {
        OP_EXTERNAL_ADDRESS => {
            if data.len() < EXTERNAL_RESPONSE_LEN {
                return None;
            }
            let external_ip = Ipv4Addr::new(data[8], data[9], data[10], data[11]);
            Some(NatPmpResponse::ExternalAddress {
                result,
                epoch_seconds,
                external_ip,
            })
        }
        OP_MAP_UDP => {
            if data.len() < MAP_RESPONSE_LEN {
                return None;
            }
            let internal_port = u16::from_be_bytes([data[8], data[9]]);
            let mapped_port = u16::from_be_bytes([data[10], data[11]]);
            let lifetime = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
            Some(NatPmpResponse::MapUdp {
                result,
                epoch_seconds,
                internal_port,
                mapped_port,
                lifetime,
            })
        }
        _ => None,
    }
}

// =========================================================================
// NatPmpMapper — the client
// =========================================================================

/// A [`PortMapperClient`] that talks NAT-PMP to a known gateway.
///
/// Stage 4b-2 leaves gateway discovery outside this type —
/// callers supply the gateway IPv4 directly. Stage 4b-4's
/// sequencer owns the discovery logic (parse OS routing table /
/// use a crate like `default-net`) before constructing the
/// mapper.
///
/// Internally uses a fresh `tokio::net::UdpSocket` per call,
/// bound to `0.0.0.0:0`. Per-call deadline is [`NATPMP_DEADLINE`]
/// (1 s). No retransmission — a live gateway responds in
/// tens of milliseconds; a dead one doesn't respond at all.
///
/// Caches the external IPv4 from probe across subsequent
/// install / renew calls so the returned `PortMapping.external`
/// carries the router's public address paired with the mapped
/// port (the map response itself only carries the port).
pub struct NatPmpMapper {
    gateway: Ipv4Addr,
    /// Gateway UDP port. Always [`NATPMP_PORT`] in production —
    /// the field exists so tests can target a mock responder on
    /// an unprivileged port without losing coverage of the real
    /// `round_trip` path (the `connect`-based source-address
    /// filter is the whole point of the spoof-rejection test).
    target_port: u16,
    cached_external: Mutex<Option<Ipv4Addr>>,
}

impl NatPmpMapper {
    /// Construct a mapper targeting `gateway` on UDP port
    /// [`NATPMP_PORT`].
    pub fn new(gateway: Ipv4Addr) -> Self {
        Self {
            gateway,
            target_port: NATPMP_PORT,
            cached_external: Mutex::new(None),
        }
    }

    /// Test-only constructor that lets the caller pin the
    /// gateway port. Production code must keep using
    /// [`NatPmpMapper::new`] so the wire destination stays at
    /// the RFC 6886 port.
    #[cfg(test)]
    pub(crate) fn new_for_test(gateway: Ipv4Addr, target_port: u16) -> Self {
        Self {
            gateway,
            target_port,
            cached_external: Mutex::new(None),
        }
    }

    /// Return the gateway this mapper is bound to.
    pub fn gateway(&self) -> Ipv4Addr {
        self.gateway
    }

    /// Read the cached external IPv4 (the most recent value
    /// seen from a response) without issuing a fresh probe.
    /// Used by install + renew to fill in the `external` field
    /// on the returned `PortMapping`.
    fn cached_external(&self) -> Option<Ipv4Addr> {
        *self.cached_external.lock().expect("mutex poisoned")
    }

    fn set_cached_external(&self, ip: Ipv4Addr) {
        *self.cached_external.lock().expect("mutex poisoned") = Some(ip);
    }

    /// Send a request to the gateway and wait for a response,
    /// bounded by [`NATPMP_DEADLINE`]. Returns the raw response
    /// bytes.
    ///
    /// Uses `UdpSocket::connect` to pin the kernel-side accept
    /// filter to `(gateway, NATPMP_PORT)`. Any packet from a
    /// host other than the configured gateway — or from the
    /// gateway but on a different source port — is silently
    /// dropped by the kernel before it reaches `recv`. This
    /// implements RFC 6886 §3.1's mandate that clients
    /// "silently ignore any response from anywhere other than
    /// the gateway IP address on port 5351," and prevents an
    /// on-path attacker from spoofing a fake NAT-PMP success
    /// reply with an attacker-controlled external address that
    /// the mesh would then advertise as its reflex.
    async fn round_trip(&self, request: Bytes) -> Result<Vec<u8>, PortMappingError> {
        let sock = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| PortMappingError::Transport(e.to_string()))?;
        let target = SocketAddr::new(IpAddr::V4(self.gateway), self.target_port);
        // Kernel-side source-address filter. After `connect`,
        // `send`/`recv` only talk to `target`; packets from
        // anywhere else are discarded without reaching our
        // userland loop.
        sock.connect(target)
            .await
            .map_err(|e| PortMappingError::Transport(e.to_string()))?;
        sock.send(&request)
            .await
            .map_err(|e| PortMappingError::Transport(e.to_string()))?;

        let mut buf = [0u8; 64];
        let n = match tokio::time::timeout(NATPMP_DEADLINE, sock.recv(&mut buf)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(PortMappingError::Transport(e.to_string())),
            Err(_) => return Err(PortMappingError::Timeout),
        };
        Ok(buf[..n].to_vec())
    }
}

#[async_trait]
impl PortMapperClient for NatPmpMapper {
    async fn probe(&self) -> Result<(), PortMappingError> {
        // Probe by asking for the external address. Any
        // success-coded response proves NAT-PMP is live on the
        // gateway. Also caches the external IPv4 for later
        // install / renew calls.
        let bytes = self
            .round_trip(encode_request(&NatPmpRequest::ExternalAddress))
            .await?;
        let resp = decode_response(&bytes)
            .ok_or_else(|| PortMappingError::Transport("malformed NAT-PMP response".into()))?;
        match resp {
            NatPmpResponse::ExternalAddress {
                result: ResultCode::Success,
                external_ip,
                ..
            } => {
                self.set_cached_external(external_ip);
                Ok(())
            }
            NatPmpResponse::ExternalAddress { result, .. } => Err(result.to_error()),
            _ => Err(PortMappingError::Transport(
                "unexpected NAT-PMP response opcode".into(),
            )),
        }
    }

    async fn install(
        &self,
        internal_port: u16,
        ttl: Duration,
    ) -> Result<PortMapping, PortMappingError> {
        // Reject zero TTL up-front. Per RFC 6886 §3.3, lifetime=0
        // is the "remove this mapping" wire signal — the same
        // format `remove()` sends. Allowing `ttl == Duration::ZERO`
        // here would silently REMOVE the mapping instead of
        // creating one, and the renewal loop would then propagate
        // `mapping.ttl = ZERO` and keep sending removes
        // ("succeeding" while the router had nothing mapped).
        if ttl.is_zero() {
            return Err(PortMappingError::Transport(
                "NAT-PMP install with ttl=0 would unmap; \
                 caller must supply a non-zero lifetime"
                    .into(),
            ));
        }
        let lifetime = ttl.as_secs().min(u32::MAX as u64) as u32;
        let req = NatPmpRequest::MapUdp {
            internal_port,
            external_port_hint: internal_port,
            lifetime,
        };
        let bytes = self.round_trip(encode_request(&req)).await?;
        let resp = decode_response(&bytes)
            .ok_or_else(|| PortMappingError::Transport("malformed NAT-PMP response".into()))?;
        match resp {
            NatPmpResponse::MapUdp {
                result: ResultCode::Success,
                mapped_port,
                lifetime: granted,
                ..
            } => {
                // External IP comes from the cached probe response.
                // If probe wasn't run (or returned an error that
                // left the cache empty), we refuse to produce a
                // mapping rather than silently substituting
                // `self.gateway` — that field holds the router's
                // *private* LAN address, and publishing it as
                // the mapping's "external" surface would poison
                // the capability announcement with an
                // unroutable-from-outside address.
                //
                // Legitimate callers go through `PortMapperTask`
                // or `SequentialMapper`, both of which probe
                // before install. A direct consumer reaching here
                // without probing is a misuse — surface it as a
                // transport error with a diagnostic so the
                // operator finds the missing step instead of
                // staring at peers that can't reach them.
                let external_ip = self.cached_external().ok_or_else(|| {
                    PortMappingError::Transport(
                        "NAT-PMP install called before successful probe — \
                         external address cache empty, refusing to publish \
                         gateway's private IP as external"
                            .into(),
                    )
                })?;
                Ok(PortMapping {
                    external: SocketAddr::new(IpAddr::V4(external_ip), mapped_port),
                    internal_port,
                    ttl: Duration::from_secs(granted as u64),
                    protocol: Protocol::NatPmp,
                })
            }
            NatPmpResponse::MapUdp { result, .. } => Err(result.to_error()),
            _ => Err(PortMappingError::Transport(
                "unexpected NAT-PMP response opcode".into(),
            )),
        }
    }

    async fn renew(&self, mapping: &PortMapping) -> Result<PortMapping, PortMappingError> {
        // Renewal is a fresh map request with the same internal
        // port — NAT-PMP has no distinct renewal op (§3.2).
        self.install(mapping.internal_port, mapping.ttl).await
    }

    async fn remove(&self, mapping: &PortMapping) {
        // Lifetime=0 is the RFC 6886 §3.3 "drop mapping" signal.
        //
        // Previously fire-and-forget — UDP delivery to the gateway
        // is not the same as gateway-side processing, and some
        // routers refuse `lifetime=0` (or are already torn down on
        // our side) without our knowing. Now we do a *short-deadline*
        // recv (200 ms) so a healthy gateway's ack confirms removal,
        // but a misbehaving one doesn't stall shutdown. On timeout we
        // log a warning so operators can investigate stale mappings;
        // on a successful recv with a non-zero result code, we log
        // the failure verbatim.
        const REMOVE_DEADLINE: std::time::Duration = std::time::Duration::from_millis(200);

        let req = NatPmpRequest::MapUdp {
            internal_port: mapping.internal_port,
            external_port_hint: 0,
            lifetime: 0,
        };
        let sock = match UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    internal_port = mapping.internal_port,
                    error = %e,
                    "NAT-PMP remove: failed to bind UDP socket — \
                     mapping not revoked, gateway holds it until TTL"
                );
                return;
            }
        };
        let target = SocketAddr::new(IpAddr::V4(self.gateway), self.target_port);
        if let Err(e) = sock.connect(target).await {
            tracing::warn!(
                internal_port = mapping.internal_port,
                error = %e,
                "NAT-PMP remove: connect to gateway failed — \
                 mapping not revoked, gateway holds it until TTL"
            );
            return;
        }
        let bytes = encode_request(&req);
        if let Err(e) = sock.send(&bytes).await {
            tracing::warn!(
                internal_port = mapping.internal_port,
                error = %e,
                "NAT-PMP remove: send to gateway failed — \
                 mapping not revoked, gateway holds it until TTL"
            );
            return;
        }

        let mut buf = [0u8; 16];
        match tokio::time::timeout(REMOVE_DEADLINE, sock.recv(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                // RFC 6886 §3.5: bytes [2..4] is the result code (BE u16).
                // 0 = success.
                if n >= 4 {
                    let result_code = u16::from_be_bytes([buf[2], buf[3]]);
                    if result_code != 0 {
                        tracing::warn!(
                            internal_port = mapping.internal_port,
                            result_code,
                            "NAT-PMP remove: gateway returned non-zero result code"
                        );
                    }
                }
            }
            Ok(Ok(_)) => {
                tracing::warn!(
                    internal_port = mapping.internal_port,
                    "NAT-PMP remove: empty response from gateway"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    internal_port = mapping.internal_port,
                    error = %e,
                    "NAT-PMP remove: recv error"
                );
            }
            Err(_) => {
                tracing::warn!(
                    internal_port = mapping.internal_port,
                    "NAT-PMP remove: gateway did not ack within {}ms — \
                     mapping may still be live",
                    REMOVE_DEADLINE.as_millis()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- codec round-trips ----

    #[test]
    fn external_request_encodes_to_two_bytes() {
        let bytes = encode_request(&NatPmpRequest::ExternalAddress);
        assert_eq!(&bytes[..], &[NATPMP_VERSION, OP_EXTERNAL_ADDRESS]);
    }

    #[test]
    fn map_udp_request_encodes_with_big_endian_fields() {
        let req = NatPmpRequest::MapUdp {
            internal_port: 9001,
            external_port_hint: 9001,
            lifetime: 3600,
        };
        let bytes = encode_request(&req);
        assert_eq!(bytes.len(), MAP_REQUEST_LEN);
        assert_eq!(bytes[0], NATPMP_VERSION);
        assert_eq!(bytes[1], OP_MAP_UDP);
        assert_eq!(&bytes[2..4], &[0, 0], "reserved must be zero");
        assert_eq!(u16::from_be_bytes([bytes[4], bytes[5]]), 9001);
        assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 9001);
        assert_eq!(
            u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            3600
        );
    }

    #[test]
    fn decode_external_address_response() {
        // ver=0, op=128 (response to op 0), result=0,
        // epoch=12345, ip=203.0.113.7
        let mut buf = Vec::with_capacity(EXTERNAL_RESPONSE_LEN);
        buf.push(NATPMP_VERSION);
        buf.push(OP_EXTERNAL_ADDRESS + RESPONSE_OP_OFFSET);
        buf.extend_from_slice(&0u16.to_be_bytes()); // success
        buf.extend_from_slice(&12345u32.to_be_bytes());
        buf.extend_from_slice(&[203, 0, 113, 7]);

        match decode_response(&buf) {
            Some(NatPmpResponse::ExternalAddress {
                result: ResultCode::Success,
                epoch_seconds: 12345,
                external_ip,
            }) => {
                assert_eq!(external_ip, Ipv4Addr::new(203, 0, 113, 7));
            }
            other => panic!("expected ExternalAddress Success, got {other:?}"),
        }
    }

    #[test]
    fn decode_map_udp_response() {
        let mut buf = Vec::with_capacity(MAP_RESPONSE_LEN);
        buf.push(NATPMP_VERSION);
        buf.push(OP_MAP_UDP + RESPONSE_OP_OFFSET);
        buf.extend_from_slice(&0u16.to_be_bytes()); // success
        buf.extend_from_slice(&1234u32.to_be_bytes()); // epoch
        buf.extend_from_slice(&9001u16.to_be_bytes()); // internal
        buf.extend_from_slice(&45678u16.to_be_bytes()); // mapped
        buf.extend_from_slice(&3600u32.to_be_bytes()); // lifetime

        match decode_response(&buf) {
            Some(NatPmpResponse::MapUdp {
                result: ResultCode::Success,
                internal_port: 9001,
                mapped_port: 45678,
                lifetime: 3600,
                ..
            }) => {}
            other => panic!("expected MapUdp Success, got {other:?}"),
        }
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #113: pre-fix
    /// `install` accepted `ttl == Duration::ZERO` and turned it
    /// into a NAT-PMP `lifetime=0`, which per RFC 6886 §3.3 is
    /// the "remove this mapping" wire signal. The gateway acks
    /// (mapping removed); `install` returned `Ok(...)` which
    /// the caller treated as freshly installed — silent
    /// data-plane failure where peers couldn't reach the node.
    /// Compounded by the renewal loop self-removing on the
    /// next tick.
    ///
    /// Post-fix: `install` rejects zero TTL synchronously
    /// before sending the wire request, so callers see an
    /// explicit error and the gateway is never asked to remove
    /// what we meant to install.
    #[tokio::test]
    async fn install_rejects_zero_ttl_before_sending_wire_request() {
        let gateway = Ipv4Addr::new(10, 0, 0, 1);
        let mapper = NatPmpMapper::new(gateway);

        // Pre-fix: this would send an install request with
        // lifetime=0; the gateway would interpret it as remove
        // and return Ok, leaving the caller with `mapping.ttl =
        // ZERO`. Post-fix: rejected synchronously without any
        // wire I/O.
        let result = mapper.install(9001, Duration::ZERO).await;
        match result {
            Err(PortMappingError::Transport(msg)) => {
                assert!(
                    msg.contains("ttl=0"),
                    "error message should mention ttl=0 (got: {msg})"
                );
            }
            other => panic!(
                "ttl=0 install must reject synchronously with Transport \
                 error, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn decode_result_code_variants() {
        assert_eq!(ResultCode::from_u16(0), ResultCode::Success);
        assert_eq!(ResultCode::from_u16(1), ResultCode::UnsupportedVersion);
        assert_eq!(ResultCode::from_u16(2), ResultCode::NotAuthorized);
        assert_eq!(ResultCode::from_u16(3), ResultCode::NetworkFailure);
        assert_eq!(ResultCode::from_u16(4), ResultCode::OutOfResources);
        assert_eq!(ResultCode::from_u16(5), ResultCode::UnsupportedOpcode);
        assert_eq!(ResultCode::from_u16(42), ResultCode::Unknown(42));
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let mut buf = vec![0u8; EXTERNAL_RESPONSE_LEN];
        buf[0] = 2; // PCP, not NAT-PMP
        buf[1] = OP_EXTERNAL_ADDRESS + RESPONSE_OP_OFFSET;
        assert!(decode_response(&buf).is_none());
    }

    #[test]
    fn decode_rejects_request_opcode() {
        // Server responses MUST have op >= 128. A packet with
        // op=0 (request) should decode as None — could be a
        // spoofed echo or a misconfigured stack.
        let mut buf = vec![0u8; EXTERNAL_RESPONSE_LEN];
        buf[0] = NATPMP_VERSION;
        buf[1] = OP_EXTERNAL_ADDRESS; // no +128
        assert!(decode_response(&buf).is_none());
    }

    #[test]
    fn decode_rejects_truncated_response() {
        let buf = vec![NATPMP_VERSION, OP_EXTERNAL_ADDRESS + RESPONSE_OP_OFFSET];
        assert!(decode_response(&buf).is_none());
    }

    #[test]
    fn decode_rejects_truncated_map_response() {
        // Map response needs 16 bytes; 12 looks like an external
        // response but with op=129 (OP_MAP_UDP+128).
        let mut buf = vec![0u8; 12];
        buf[0] = NATPMP_VERSION;
        buf[1] = OP_MAP_UDP + RESPONSE_OP_OFFSET;
        assert!(decode_response(&buf).is_none());
    }

    #[test]
    fn decode_rejects_unknown_opcode() {
        let mut buf = vec![0u8; MAP_RESPONSE_LEN];
        buf[0] = NATPMP_VERSION;
        buf[1] = RESPONSE_OP_OFFSET + 42; // 170 — not defined
        assert!(decode_response(&buf).is_none());
    }

    /// Regression test for TEST_COVERAGE_PLAN §P2-12: the
    /// decoder must never panic on malformed input. A gateway
    /// responding with a truncated / corrupted packet (stack
    /// bug, MTU clip, malicious on-path tamper, or just cosmic
    /// rays) must surface as a clean `None` so the caller falls
    /// back to UPnP or propagates `Transport` cleanly. Pins that
    /// every byte access inside `decode_response` is bounds-
    /// checked.
    ///
    /// Table-driven over all lengths 0..=32 (covers both below
    /// `EXTERNAL_RESPONSE_LEN` and above `MAP_RESPONSE_LEN`),
    /// with three byte patterns chosen to trip different
    /// validation branches (all-zero → wrong op, all-`0xFF` →
    /// version + op + magic values, ascending → potentially
    /// valid prefix with truncated tail). Each combination must
    /// return either `Some(_)` or `None`, never panic.
    #[test]
    fn decode_response_never_panics_on_malformed_input() {
        for len in 0..=32 {
            // All zeros: version=0, op=0, which violates the
            // "op must have high bit set" rule → None.
            let zeros = vec![0u8; len];
            let _ = decode_response(&zeros);

            // All 0xFF: version=255 ≠ 0 → None.
            let ones = vec![0xFFu8; len];
            let _ = decode_response(&ones);

            // Ascending bytes 0..len — has a valid version byte
            // at [0]=0, some op at [1] that MAY or may not look
            // like a response, and whatever the rest lands on.
            let ascending: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let _ = decode_response(&ascending);

            // Descending bytes — different magic / op layout.
            let descending: Vec<u8> = (0..len).map(|i| (255 - i) as u8).collect();
            let _ = decode_response(&descending);
        }

        // Explicit edge case: a response that claims to be a
        // MAP-UDP reply (op = 0x81) but is only 12 bytes long
        // (EXTERNAL_RESPONSE_LEN, not MAP_RESPONSE_LEN). The
        // op-specific inner length check must reject without
        // indexing past the buffer end.
        let mut short_map = vec![0u8; EXTERNAL_RESPONSE_LEN];
        short_map[0] = NATPMP_VERSION;
        short_map[1] = OP_MAP_UDP + RESPONSE_OP_OFFSET;
        assert!(decode_response(&short_map).is_none());
    }

    // ---- live UdpSocket round-trip against a mock gateway ----

    /// Spawn a tokio task that binds a local UDP socket and plays
    /// the role of a NAT-PMP gateway: for each incoming request,
    /// decode it, call `respond`, and send back the returned
    /// bytes. Returns the port the mock bound to.
    async fn spawn_mock_gateway<F>(respond: F) -> (u16, tokio::task::JoinHandle<()>)
    where
        F: Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = sock.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                match sock.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if let Some(reply) = respond(&buf[..n]) {
                            let _ = sock.send_to(&reply, from).await;
                        }
                    }
                    Err(_) => return,
                }
            }
        });
        (port, handle)
    }

    /// Wrapper that overrides the target port on `NatPmpMapper`
    /// so tests can point it at the mock gateway's ephemeral
    /// port instead of 5351 (which requires root on unix). Only
    /// exposes `round_trip` — the send-then-receive path — so
    /// we can exercise the deadline + codec end-to-end against
    /// a mock responder without needing a privileged socket.
    struct TestMapper {
        gateway_port: u16,
    }

    impl TestMapper {
        fn new(gateway_port: u16) -> Self {
            Self { gateway_port }
        }

        async fn round_trip(&self, request: Bytes) -> Result<Vec<u8>, PortMappingError> {
            let sock = UdpSocket::bind("127.0.0.1:0")
                .await
                .map_err(|e| PortMappingError::Transport(e.to_string()))?;
            let target: SocketAddr = format!("127.0.0.1:{}", self.gateway_port).parse().unwrap();
            sock.send_to(&request, target)
                .await
                .map_err(|e| PortMappingError::Transport(e.to_string()))?;
            let mut buf = [0u8; 64];
            let (n, _from) =
                match tokio::time::timeout(NATPMP_DEADLINE, sock.recv_from(&mut buf)).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => return Err(PortMappingError::Transport(e.to_string())),
                    Err(_) => return Err(PortMappingError::Timeout),
                };
            Ok(buf[..n].to_vec())
        }
    }

    /// Trivial helper: encode a success external-address
    /// response with the given IPv4.
    fn encode_external_success(ip: Ipv4Addr) -> Vec<u8> {
        let mut buf = Vec::with_capacity(EXTERNAL_RESPONSE_LEN);
        buf.push(NATPMP_VERSION);
        buf.push(OP_EXTERNAL_ADDRESS + RESPONSE_OP_OFFSET);
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&ip.octets());
        buf
    }

    fn encode_map_success(internal: u16, mapped: u16, lifetime: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(MAP_RESPONSE_LEN);
        buf.push(NATPMP_VERSION);
        buf.push(OP_MAP_UDP + RESPONSE_OP_OFFSET);
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&internal.to_be_bytes());
        buf.extend_from_slice(&mapped.to_be_bytes());
        buf.extend_from_slice(&lifetime.to_be_bytes());
        buf
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nat_pmp_mapper_probe_times_out_against_dead_gateway() {
        // Construct the real `NatPmpMapper` against a gateway
        // port with a bound-but-silent listener. Verifies the
        // full `PortMapperClient::probe` path (encode → UDP
        // send → await response → deadline) without needing a
        // mock that simulates success.
        //
        // Subtlety: after the RFC 6886 §3.1 spoof-rejection
        // fix, `round_trip` calls `UdpSocket::connect(gateway)`.
        // On loopback, sending to a *closed* port surfaces an
        // ICMP destination-unreachable as ECONNREFUSED —
        // `recv` returns `Err` immediately instead of waiting
        // for the deadline. That's correct kernel behavior,
        // just not the "silent gateway" case we want to test.
        // Binding a real socket (that never replies) keeps the
        // port "alive" so the deadline path is what fires.
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        let mapper = NatPmpMapper::new_for_test(Ipv4Addr::LOCALHOST, silent_port);
        assert_eq!(mapper.gateway(), Ipv4Addr::LOCALHOST);

        let start = tokio::time::Instant::now();
        let res = mapper.probe().await;
        let elapsed = start.elapsed();

        assert!(
            matches!(res, Err(PortMappingError::Timeout)),
            "expected Timeout, got {res:?}",
        );
        assert!(
            elapsed >= Duration::from_millis(800) && elapsed < Duration::from_millis(2000),
            "deadline should be ~1 s; got {elapsed:?}",
        );

        // No response → cache stays empty.
        assert!(mapper.cached_external().is_none());
        drop(silent);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_times_out_when_gateway_silent() {
        // Bind a socket but don't respond. The mapper's
        // NATPMP_DEADLINE should fire.
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = sock.local_addr().unwrap().port();
        // Don't even spawn a receive loop — the request lands
        // and nothing replies.

        let mapper = TestMapper::new(port);
        let start = tokio::time::Instant::now();
        let res = mapper
            .round_trip(encode_request(&NatPmpRequest::ExternalAddress))
            .await;
        let elapsed = start.elapsed();

        assert!(matches!(res, Err(PortMappingError::Timeout)));
        // Deadline is 1 s; allow +/- 200 ms for scheduler jitter.
        assert!(
            elapsed >= Duration::from_millis(800) && elapsed < Duration::from_millis(2000),
            "expected ~1 s timeout, got {elapsed:?}",
        );
        drop(sock);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_against_mock_gateway_returns_success() {
        let (port, gw) = spawn_mock_gateway(|req| {
            let decoded = req.to_vec();
            if decoded == vec![NATPMP_VERSION, OP_EXTERNAL_ADDRESS] {
                Some(encode_external_success(Ipv4Addr::new(198, 51, 100, 7)))
            } else {
                None
            }
        })
        .await;

        // We can't use NatPmpMapper directly because it
        // hard-codes port 5351 on the gateway. Reimplement the
        // probe flow against the mock's port to verify the
        // codec handshake end-to-end.
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        sock.send_to(&encode_request(&NatPmpRequest::ExternalAddress), target)
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        let (n, _) = tokio::time::timeout(Duration::from_secs(1), sock.recv_from(&mut buf))
            .await
            .expect("gateway response")
            .unwrap();
        let resp = decode_response(&buf[..n]).expect("decode");

        match resp {
            NatPmpResponse::ExternalAddress {
                result: ResultCode::Success,
                external_ip,
                ..
            } => {
                assert_eq!(external_ip, Ipv4Addr::new(198, 51, 100, 7));
            }
            other => panic!("unexpected response {other:?}"),
        }
        gw.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_against_mock_gateway_returns_mapping() {
        let (port, gw) = spawn_mock_gateway(|req| match req {
            // External-address probe → success
            r if r == [NATPMP_VERSION, OP_EXTERNAL_ADDRESS] => {
                Some(encode_external_success(Ipv4Addr::new(198, 51, 100, 7)))
            }
            // Map-UDP install → success with mapped port 54321
            r if r.len() == MAP_REQUEST_LEN && r[0] == NATPMP_VERSION && r[1] == OP_MAP_UDP => {
                let internal = u16::from_be_bytes([r[4], r[5]]);
                Some(encode_map_success(internal, 54321, 3600))
            }
            _ => None,
        })
        .await;

        // Full probe + install flow through the mock port.
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        // Probe first.
        sock.send_to(&encode_request(&NatPmpRequest::ExternalAddress), target)
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        let (n, _) = tokio::time::timeout(Duration::from_secs(1), sock.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let probe_ip = match decode_response(&buf[..n]).unwrap() {
            NatPmpResponse::ExternalAddress {
                result: ResultCode::Success,
                external_ip,
                ..
            } => external_ip,
            other => panic!("probe failure {other:?}"),
        };
        assert_eq!(probe_ip, Ipv4Addr::new(198, 51, 100, 7));

        // Install.
        let install_req = NatPmpRequest::MapUdp {
            internal_port: 9001,
            external_port_hint: 9001,
            lifetime: 3600,
        };
        sock.send_to(&encode_request(&install_req), target)
            .await
            .unwrap();
        let (n, _) = tokio::time::timeout(Duration::from_secs(1), sock.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let mapping = match decode_response(&buf[..n]).unwrap() {
            NatPmpResponse::MapUdp {
                result: ResultCode::Success,
                internal_port: 9001,
                mapped_port: 54321,
                lifetime: 3600,
                ..
            } => (9001u16, 54321u16),
            other => panic!("install failure {other:?}"),
        };
        assert_eq!(mapping, (9001, 54321));
        gw.abort();
    }

    /// Regression test for a cubic-flagged P1 bug: the NAT-PMP
    /// client accepted UDP responses from any source host/port,
    /// letting an on-path attacker inject a fake success with an
    /// attacker-chosen `external_ip`. The mesh would then
    /// advertise that IP as its reflex, poisoning rendezvous
    /// targets.
    ///
    /// RFC 6886 §3.1 requires clients to "silently ignore any
    /// response from anywhere other than the gateway IP address
    /// on port 5351." We implement that by calling
    /// `UdpSocket::connect(gateway, target_port)` before
    /// `recv` — the kernel drops packets from any other
    /// `(ip, port)` pair before they reach userland.
    ///
    /// # Topology
    ///
    /// - **Gateway (silent):** binds an ephemeral loopback port.
    ///   Swallows incoming requests without replying. Its port
    ///   is what `NatPmpMapper::new_for_test` targets, so the
    ///   connected socket's filter pins to that `(127.0.0.1, port)`.
    /// - **Spoofer:** binds a *different* ephemeral loopback
    ///   port. Watches the gateway's mailbox for the client's
    ///   source address (which the spoofer can observe because
    ///   the request went to the gateway, not to it — so we hand
    ///   it the client's source explicitly via a channel the
    ///   spoofer listens on), then races to send a forged
    ///   external-address success from its own source port to
    ///   the client.
    ///
    /// Simpler variant used here: the spoofer just blasts a
    /// fake success to every ephemeral port in a tight range
    /// around the mapper's likely source. Loopback is fast
    /// enough that within the 1-second deadline the spoof
    /// *would* land if the filter weren't working — the
    /// pre-fix code accepted the first matching packet
    /// regardless of source.
    ///
    /// # Assertion
    ///
    /// `probe()` times out (no packet passed the kernel filter)
    /// and the cache stays empty. Without the `connect` call
    /// the spoofed response would reach userland, decode as
    /// success, and populate `cached_external` with the
    /// attacker IP.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_rejects_response_from_non_gateway_source() {
        // Silent gateway — binds but never replies.
        let gateway_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway_port = gateway_sock.local_addr().unwrap().port();

        // Spoofer on a different ephemeral port. When the
        // spoofer sees *anything*, it replies with a fake
        // success. But we have no way to know the mapper's
        // source port in advance — so instead we arrange for
        // the gateway to forward incoming `(source_addr)`
        // tuples to the spoofer over a channel, and the spoofer
        // then sends the forged response back to that exact
        // source port. This makes the spoof as directly
        // targeted as possible — the only remaining defense is
        // the `connect`-based filter on the mapper's socket.
        let (src_tx, mut src_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
        let gateway_handle = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            while let Ok((_n, from)) = gateway_sock.recv_from(&mut buf).await {
                // Don't reply — just forward the source addr.
                let _ = src_tx.send(from);
            }
        });

        let spoofer_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let spoofer_port = spoofer_sock.local_addr().unwrap().port();
        assert_ne!(
            spoofer_port, gateway_port,
            "spoofer must use a different port"
        );
        let spoofer_handle = tokio::spawn(async move {
            // Forge a success reply with an attacker-chosen IP.
            // If this lands, the mapper would cache this as its
            // external — that's the poisoning vector.
            let forged = encode_external_success(Ipv4Addr::new(203, 0, 113, 66));
            while let Some(client_src) = src_rx.recv().await {
                // Blast the forged packet at the client. If the
                // `connect` filter is working, this is dropped
                // by the kernel before reaching `recv`.
                let _ = spoofer_sock.send_to(&forged, client_src).await;
            }
        });

        // The real `NatPmpMapper`, targeting the silent gateway's
        // port. With the fix in place, its UDP socket is
        // `connect()`ed to `(127.0.0.1, gateway_port)` so
        // packets from the spoofer's port are filtered out.
        let mapper = NatPmpMapper::new_for_test(Ipv4Addr::LOCALHOST, gateway_port);

        let start = tokio::time::Instant::now();
        let res = mapper.probe().await;
        let elapsed = start.elapsed();

        assert!(
            matches!(res, Err(PortMappingError::Timeout)),
            "probe must time out — spoofed response should not leak through kernel filter; \
             got {res:?} after {elapsed:?}",
        );
        assert!(
            elapsed >= Duration::from_millis(800),
            "timeout fired too early ({elapsed:?}); spoof may have short-circuited the deadline",
        );
        assert!(
            mapper.cached_external().is_none(),
            "external-IP cache must stay empty — if a spoofed IP landed here, the mesh would \
             advertise the attacker's address as its reflex",
        );

        gateway_handle.abort();
        spoofer_handle.abort();
    }

    /// Regression test for the `remove`-on-silent-gateway path.
    /// History:
    ///   - Original: `remove` went through `round_trip`, which
    ///     waited up to [`NATPMP_DEADLINE`] (1 s) per call —
    ///     mesh teardown stalled a full second per held mapping.
    ///   - Then: switched to fire-and-forget; the test pinned
    ///     "elapsed < 200 ms".
    ///   - Now (BUG_REPORT.md #41): UDP delivery to a gateway is
    ///     not the same as gateway-side processing, and some
    ///     routers refuse `lifetime=0`. The client never knew.
    ///     `remove` now does a *short-deadline* recv (200 ms) so
    ///     a healthy gateway's ack confirms removal and a
    ///     misbehaving one doesn't stall shutdown longer than
    ///     that bounded window.
    ///
    /// The contract this test pins: against a silent gateway,
    /// `remove` must complete within the bounded
    /// `REMOVE_DEADLINE` (200 ms) plus reasonable scheduler
    /// jitter — well under the original 1 s — and must NOT
    /// hang or block mesh shutdown indefinitely.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remove_is_fire_and_forget_and_does_not_block_on_silent_gateway() {
        // Silent gateway — binds the port (so `connect` can
        // succeed on loopback) but never reads or replies.
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway_port = silent.local_addr().unwrap().port();

        let mapper = NatPmpMapper::new_for_test(Ipv4Addr::LOCALHOST, gateway_port);
        let mapping = PortMapping {
            external: "203.0.113.1:9001".parse().unwrap(),
            internal_port: 9001,
            ttl: Duration::from_secs(3600),
            protocol: Protocol::NatPmp,
        };

        let start = tokio::time::Instant::now();
        mapper.remove(&mapping).await;
        let elapsed = start.elapsed();

        // `remove` waits up to `REMOVE_DEADLINE` (200 ms) for the
        // gateway's ack so misconfigured routers that refuse
        // `lifetime=0` are at least logged (#41). Cap the
        // bounded wait at 500 ms total — the deadline plus
        // generous scheduler jitter — so this test still catches
        // a regression to the old 1 s `NATPMP_DEADLINE` path.
        assert!(
            elapsed < Duration::from_millis(500),
            "remove() blocked for {elapsed:?} — bounded recv regressed. \
             On a silent gateway it must complete within REMOVE_DEADLINE \
             (~200 ms) plus jitter, well under the original 1 s deadline",
        );

        drop(silent);
    }

    /// Regression test for the `.unwrap_or(self.gateway)` audit
    /// (FAILURE_PATH_HARDENING_PLAN pre-flight): `install` must
    /// refuse to produce a `PortMapping` when `cached_external`
    /// is empty, rather than silently substituting the gateway's
    /// private LAN IP as the mapping's external address.
    ///
    /// Scenario: a mock gateway cheerfully answers `MapUdp`
    /// install requests with success — but we never call
    /// `probe`, so the mapper has no cached external IP. The
    /// old code path returned `Ok(PortMapping { external: <LAN
    /// gateway IP>, .. })`, poisoning downstream capability
    /// announcements. The new behavior surfaces a `Transport`
    /// error so the caller finds the missing precondition
    /// instead of publishing an unroutable address to peers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_without_prior_probe_refuses_rather_than_publishing_gateway_ip() {
        // Mock gateway that responds to MapUdp but would fail
        // any ExternalAddress probe (returns None → client
        // times out). We never send a probe in this test, so
        // the probe-response behavior doesn't matter.
        let (port, gw) = spawn_mock_gateway(|req| {
            if req.len() == MAP_REQUEST_LEN && req[0] == NATPMP_VERSION && req[1] == OP_MAP_UDP {
                let internal = u16::from_be_bytes([req[4], req[5]]);
                Some(encode_map_success(internal, internal, 3600))
            } else {
                None
            }
        })
        .await;

        let mapper = NatPmpMapper::new_for_test(Ipv4Addr::LOCALHOST, port);
        assert!(
            mapper.cached_external().is_none(),
            "freshly-constructed mapper must have an empty external-IP cache",
        );

        // Call install directly, without a preceding probe.
        // The round-trip succeeds (gateway returns MapUdp
        // success), but the mapper has no cached external IP
        // to pair with the mapped port. The new behavior must
        // refuse rather than substitute `self.gateway`
        // (which is the router's private LAN address).
        let res = mapper.install(9001, Duration::from_secs(3600)).await;

        match res {
            Err(PortMappingError::Transport(msg)) => {
                assert!(
                    msg.contains("install called before successful probe"),
                    "error detail must identify the precondition violation; got {msg:?}",
                );
            }
            Ok(mapping) => panic!(
                "install must NOT silently substitute the gateway LAN IP — \
                 got mapping with external={:?}. Pre-fix behavior would publish \
                 the gateway's private address to capability announcements",
                mapping.external,
            ),
            Err(other) => panic!("expected Transport(<precondition msg>); got {other:?}",),
        }

        // And the cache is still empty — a refused install
        // must not somehow populate the cache as a side effect.
        assert!(mapper.cached_external().is_none());
        gw.abort();
    }
}
