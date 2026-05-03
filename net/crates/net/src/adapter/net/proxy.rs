//! Net Proxy for zero-copy multi-hop packet forwarding.
//!
//! The proxy handles:
//! - Zero-copy packet forwarding (no decryption)
//! - TTL enforcement and hop counting
//! - Per-hop latency tracking
//! - Bandwidth metering

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;

use super::route::{RoutingHeader, ROUTING_HEADER_SIZE};

/// Proxy configuration
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Local node ID
    pub local_id: u64,
    /// Bind address
    pub bind_addr: SocketAddr,
    /// Maximum packet size
    pub max_packet_size: usize,
    /// Enable hop latency tracking
    pub track_latency: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            local_id: 0,
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            max_packet_size: 65535,
            track_latency: true,
        }
    }
}

impl ProxyConfig {
    /// Create a new proxy config
    pub fn new(local_id: u64, bind_addr: SocketAddr) -> Self {
        Self {
            local_id,
            bind_addr,
            ..Default::default()
        }
    }
}

/// Per-hop statistics
#[derive(Debug, Default)]
pub struct HopStats {
    /// Packets forwarded
    pub packets_forwarded: AtomicU64,
    /// Packets dropped (TTL, no route, etc.)
    pub packets_dropped: AtomicU64,
    /// Bytes forwarded
    pub bytes_forwarded: AtomicU64,
    /// Total forwarding latency (nanoseconds)
    total_latency_ns: AtomicU64,
    /// Latency sample count
    latency_samples: AtomicU64,
}

impl HopStats {
    /// Create new hop stats
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a forwarded packet
    #[inline]
    pub fn record_forward(&self, bytes: u64, latency_ns: u64) {
        self.packets_forwarded.fetch_add(1, Ordering::Relaxed);
        self.bytes_forwarded.fetch_add(bytes, Ordering::Relaxed);
        if latency_ns > 0 {
            self.total_latency_ns
                .fetch_add(latency_ns, Ordering::Relaxed);
            self.latency_samples.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a dropped packet
    #[inline]
    pub fn record_drop(&self) {
        self.packets_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Get average forwarding latency in nanoseconds
    pub fn avg_latency_ns(&self) -> u64 {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        if samples == 0 {
            return 0;
        }
        self.total_latency_ns.load(Ordering::Relaxed) / samples
    }

    /// Get forwarded packet count
    pub fn forwarded(&self) -> u64 {
        self.packets_forwarded.load(Ordering::Relaxed)
    }

    /// Get dropped packet count
    pub fn dropped(&self) -> u64 {
        self.packets_dropped.load(Ordering::Relaxed)
    }
}

/// Proxy statistics
#[derive(Debug, Clone, Default)]
pub struct ProxyStats {
    /// Total packets received
    pub packets_received: u64,
    /// Total packets forwarded
    pub packets_forwarded: u64,
    /// Total packets dropped
    pub packets_dropped: u64,
    /// Total bytes forwarded
    pub bytes_forwarded: u64,
    /// Average forwarding latency (nanoseconds)
    pub avg_latency_ns: u64,
    /// Active routes
    pub routes: usize,
}

/// Proxy errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    /// Packet too small to contain routing header
    PacketTooSmall,
    /// Invalid routing header
    InvalidHeader,
    /// TTL expired
    TtlExpired,
    /// No route to destination
    NoRoute,
    /// Send failed
    SendFailed(String),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PacketTooSmall => write!(f, "packet too small"),
            Self::InvalidHeader => write!(f, "invalid routing header"),
            Self::TtlExpired => write!(f, "TTL expired"),
            Self::NoRoute => write!(f, "no route to destination"),
            Self::SendFailed(e) => write!(f, "send failed: {}", e),
        }
    }
}

impl std::error::Error for ProxyError {}

/// Result of forwarding a packet
#[derive(Debug)]
pub enum ForwardResult {
    /// Packet forwarded to next hop
    Forwarded {
        /// Next hop address
        next_hop: SocketAddr,
        /// The packet with updated routing header, ready to send
        packet: Bytes,
        /// Forwarding latency in nanoseconds
        latency_ns: u64,
    },
    /// Packet is for local delivery
    Local(Bytes),
    /// Packet dropped
    Dropped(ProxyError),
}

/// Net Proxy for zero-copy multi-hop forwarding.
///
/// The proxy forwards packets without decrypting the payload,
/// only reading and updating the routing header.
pub struct NetProxy {
    /// Configuration
    #[allow(dead_code)]
    config: ProxyConfig,
    /// UDP socket
    socket: Arc<UdpSocket>,
    /// Next-hop routing table (dest_id -> next_hop)
    next_hop: DashMap<u64, SocketAddr>,
    /// Local node ID
    local_id: u64,
    /// Per-destination hop stats
    hop_stats: DashMap<u64, HopStats>,
    /// Global stats
    packets_received: AtomicU64,
    packets_forwarded: AtomicU64,
    packets_dropped: AtomicU64,
    bytes_forwarded: AtomicU64,
    total_latency_ns: AtomicU64,
    latency_samples: AtomicU64,
}

impl NetProxy {
    /// Create a new proxy
    pub async fn new(config: ProxyConfig) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        let local_id = config.local_id;

        Ok(Self {
            config,
            socket: Arc::new(socket),
            next_hop: DashMap::new(),
            local_id,
            hop_stats: DashMap::new(),
            packets_received: AtomicU64::new(0),
            packets_forwarded: AtomicU64::new(0),
            packets_dropped: AtomicU64::new(0),
            bytes_forwarded: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            latency_samples: AtomicU64::new(0),
        })
    }

    /// Get local address
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Add a route to the next-hop table
    pub fn add_route(&self, dest_id: u64, next_hop: SocketAddr) {
        self.next_hop.insert(dest_id, next_hop);
    }

    /// Remove a route.
    ///
    /// Drops both the next_hop entry and the matching `hop_stats`
    /// entry. Removing only the former would let `hop_stats` grow
    /// indefinitely (memory ∝ total-distinct-dest-ids-ever-seen,
    /// not active dest count) for a peer churning through many
    /// destinations.
    pub fn remove_route(&self, dest_id: u64) {
        self.next_hop.remove(&dest_id);
        self.hop_stats.remove(&dest_id);
    }

    /// Lookup next hop for destination
    pub fn lookup(&self, dest_id: u64) -> Option<SocketAddr> {
        self.next_hop.get(&dest_id).map(|r| *r)
    }

    /// Forward a packet (zero-copy, no decryption).
    ///
    /// This is the hot path - it only reads and updates the routing header,
    /// then forwards the entire packet to the next hop.
    pub fn forward(&self, data: Bytes) -> ForwardResult {
        let start = Instant::now();
        let len = data.len() as u64;

        self.packets_received.fetch_add(1, Ordering::Relaxed);

        // Validate packet size
        if data.len() < ROUTING_HEADER_SIZE {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            return ForwardResult::Dropped(ProxyError::PacketTooSmall);
        }

        // Parse routing header (only first 16 bytes)
        let header = match RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE]) {
            Some(h) => h,
            None => {
                self.packets_dropped.fetch_add(1, Ordering::Relaxed);
                return ForwardResult::Dropped(ProxyError::InvalidHeader);
            }
        };

        // Check if local delivery
        if header.dest_id == self.local_id {
            return ForwardResult::Local(data.slice(ROUTING_HEADER_SIZE..));
        }

        // Check TTL
        if header.is_expired() {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            self.record_hop_drop(header.dest_id);
            return ForwardResult::Dropped(ProxyError::TtlExpired);
        }

        // Lookup next hop
        let next_hop = match self.lookup(header.dest_id) {
            Some(addr) => addr,
            None => {
                self.packets_dropped.fetch_add(1, Ordering::Relaxed);
                self.record_hop_drop(header.dest_id);
                return ForwardResult::Dropped(ProxyError::NoRoute);
            }
        };

        // Update routing header (decrement TTL, increment hop count)
        let mut new_header = header;
        new_header.forward();
        // Drop here if forwarding made the TTL 0 — the next hop
        // would just drop it on its own `is_expired()` check,
        // wasting bandwidth and a queue slot.
        if new_header.is_expired() {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            self.record_hop_drop(header.dest_id);
            return ForwardResult::Dropped(ProxyError::TtlExpired);
        }

        // Build forwarded packet with updated header
        let mut fwd_data = BytesMut::with_capacity(data.len());
        new_header.write_to(&mut fwd_data);
        fwd_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);

        let latency_ns = start.elapsed().as_nanos() as u64;

        // Update stats
        self.packets_forwarded.fetch_add(1, Ordering::Relaxed);
        self.bytes_forwarded.fetch_add(len, Ordering::Relaxed);
        self.total_latency_ns
            .fetch_add(latency_ns, Ordering::Relaxed);
        self.latency_samples.fetch_add(1, Ordering::Relaxed);
        self.record_hop_forward(header.dest_id, len, latency_ns);

        ForwardResult::Forwarded {
            next_hop,
            packet: fwd_data.freeze(),
            latency_ns,
        }
    }

    /// Forward and send a packet in one operation
    pub async fn forward_and_send(&self, data: Bytes) -> Result<ForwardResult, ProxyError> {
        match self.forward(data) {
            ForwardResult::Forwarded {
                next_hop,
                ref packet,
                latency_ns,
            } => {
                let packet_len = packet.len() as u64;
                match self.socket.send_to(packet, next_hop).await {
                    Ok(_) => Ok(ForwardResult::Forwarded {
                        next_hop,
                        packet: packet.clone(),
                        latency_ns,
                    }),
                    Err(e) => {
                        // Roll back the telemetry counters
                        // `forward()` bumped speculatively. Pre-fix
                        // `packets_forwarded` / `bytes_forwarded` /
                        // `total_latency_ns` / `latency_samples` all
                        // counted the prepared packet as if it had
                        // shipped, even when the kernel rejected the
                        // send below — operators reading the
                        // forwarded-packet rate saw a count that
                        // included would-be sends that never crossed
                        // the wire. Saturating-sub guards against an
                        // arithmetic underflow if the counter was
                        // somehow reset between the bump and this
                        // rollback.
                        self.packets_forwarded
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                                Some(v.saturating_sub(1))
                            })
                            .ok();
                        self.bytes_forwarded
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                                Some(v.saturating_sub(packet_len))
                            })
                            .ok();
                        self.total_latency_ns
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                                Some(v.saturating_sub(latency_ns))
                            })
                            .ok();
                        self.latency_samples
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                                Some(v.saturating_sub(1))
                            })
                            .ok();
                        Err(ProxyError::SendFailed(e.to_string()))
                    }
                }
            }
            ForwardResult::Local(payload) => Ok(ForwardResult::Local(payload)),
            ForwardResult::Dropped(e) => Err(e),
        }
    }

    /// Send data to a destination
    pub async fn send_to(&self, data: &[u8], dest: SocketAddr) -> std::io::Result<usize> {
        self.socket.send_to(data, dest).await
    }

    /// Receive a packet
    pub async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }

    /// Get proxy statistics
    pub fn stats(&self) -> ProxyStats {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        let avg_latency = self
            .total_latency_ns
            .load(Ordering::Relaxed)
            .checked_div(samples)
            .unwrap_or(0);

        ProxyStats {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_forwarded: self.packets_forwarded.load(Ordering::Relaxed),
            packets_dropped: self.packets_dropped.load(Ordering::Relaxed),
            bytes_forwarded: self.bytes_forwarded.load(Ordering::Relaxed),
            avg_latency_ns: avg_latency,
            routes: self.next_hop.len(),
        }
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        self.packets_received.store(0, Ordering::Relaxed);
        self.packets_forwarded.store(0, Ordering::Relaxed);
        self.packets_dropped.store(0, Ordering::Relaxed);
        self.bytes_forwarded.store(0, Ordering::Relaxed);
        self.total_latency_ns.store(0, Ordering::Relaxed);
        self.latency_samples.store(0, Ordering::Relaxed);
    }

    /// Get hop stats for a destination
    pub fn hop_stats(&self, dest_id: u64) -> Option<(u64, u64, u64)> {
        self.hop_stats
            .get(&dest_id)
            .map(|s| (s.forwarded(), s.dropped(), s.avg_latency_ns()))
    }

    fn record_hop_forward(&self, dest_id: u64, bytes: u64, latency_ns: u64) {
        self.hop_stats
            .entry(dest_id)
            .or_default()
            .record_forward(bytes, latency_ns);
    }

    fn record_hop_drop(&self, dest_id: u64) {
        self.hop_stats.entry(dest_id).or_default().record_drop();
    }

    /// Get route count
    pub fn route_count(&self) -> usize {
        self.next_hop.len()
    }
}

impl std::fmt::Debug for NetProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetProxy")
            .field("local_id", &format!("{:016x}", self.local_id))
            .field("routes", &self.next_hop.len())
            .field(
                "packets_forwarded",
                &self.packets_forwarded.load(Ordering::Relaxed),
            )
            .finish()
    }
}

/// Multi-hop packet builder for creating routed packets
pub struct MultiHopPacketBuilder {
    /// Source node ID
    src_id: u32,
}

impl MultiHopPacketBuilder {
    /// Create a new multi-hop packet builder
    pub fn new(src_id: u32) -> Self {
        Self { src_id }
    }

    /// Build a packet with routing header
    pub fn build(&self, dest_id: u64, ttl: u8, payload: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(ROUTING_HEADER_SIZE + payload.len());

        let header = RoutingHeader::new(dest_id, self.src_id, ttl);
        header.write_to(&mut buf);
        buf.extend_from_slice(payload);

        buf.freeze()
    }

    /// Build a priority packet
    pub fn build_priority(&self, dest_id: u64, ttl: u8, payload: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(ROUTING_HEADER_SIZE + payload.len());

        let header = RoutingHeader::priority(dest_id, self.src_id, ttl);
        header.write_to(&mut buf);
        buf.extend_from_slice(payload);

        buf.freeze()
    }

    /// Build a control packet
    pub fn build_control(&self, dest_id: u64, ttl: u8, payload: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(ROUTING_HEADER_SIZE + payload.len());

        let header = RoutingHeader::control(dest_id, self.src_id, ttl);
        header.write_to(&mut buf);
        buf.extend_from_slice(payload);

        buf.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forward_result() {
        // Test packet building
        let builder = MultiHopPacketBuilder::new(0xABCD);
        let packet = builder.build(0x1234, 8, b"hello world");

        assert_eq!(packet.len(), ROUTING_HEADER_SIZE + 11);

        // Parse header back
        let header = RoutingHeader::from_bytes(&packet[..ROUTING_HEADER_SIZE]).unwrap();
        assert_eq!(header.dest_id, 0x1234);
        assert_eq!(header.src_id, 0xABCD);
        assert_eq!(header.ttl, 8);
        assert_eq!(header.hop_count, 0);
    }

    #[test]
    fn test_priority_packet() {
        let builder = MultiHopPacketBuilder::new(0x1111);
        let packet = builder.build_priority(0x2222, 4, b"urgent");

        let header = RoutingHeader::from_bytes(&packet[..ROUTING_HEADER_SIZE]).unwrap();
        assert!(header.flags.is_priority());
    }

    #[test]
    fn test_control_packet() {
        let builder = MultiHopPacketBuilder::new(0x1111);
        let packet = builder.build_control(0x2222, 2, b"ping");

        let header = RoutingHeader::from_bytes(&packet[..ROUTING_HEADER_SIZE]).unwrap();
        assert!(header.flags.is_control());
    }

    #[tokio::test]
    async fn test_proxy_creation() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        assert_eq!(proxy.route_count(), 0);
        assert_eq!(proxy.stats().packets_received, 0);
    }

    #[tokio::test]
    async fn test_proxy_routing() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        let dest: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        proxy.add_route(0x5678, dest);

        assert_eq!(proxy.lookup(0x5678), Some(dest));
        assert_eq!(proxy.lookup(0x9999), None);
    }

    #[tokio::test]
    async fn test_proxy_forward() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        let next_hop: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        proxy.add_route(0x5678, next_hop);

        // Build a packet
        let builder = MultiHopPacketBuilder::new(0xABCD);
        let packet = builder.build(0x5678, 8, b"test payload");

        // Forward it
        match proxy.forward(packet) {
            ForwardResult::Forwarded { next_hop: addr, .. } => {
                assert_eq!(addr, next_hop);
            }
            _ => panic!("expected forwarded"),
        }

        let stats = proxy.stats();
        assert_eq!(stats.packets_received, 1);
        assert_eq!(stats.packets_forwarded, 1);
        assert_eq!(stats.packets_dropped, 0);
    }

    #[tokio::test]
    async fn test_proxy_local_delivery() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        // Build a packet destined for local node
        let builder = MultiHopPacketBuilder::new(0xABCD);
        let packet = builder.build(0x1234, 8, b"local payload");

        match proxy.forward(packet) {
            ForwardResult::Local(payload) => {
                assert_eq!(&payload[..], b"local payload");
            }
            _ => panic!("expected local delivery"),
        }
    }

    #[tokio::test]
    async fn test_proxy_ttl_expired() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        proxy.add_route(0x5678, "127.0.0.1:9001".parse().unwrap());

        // Build a packet with TTL=0
        let builder = MultiHopPacketBuilder::new(0xABCD);
        let packet = builder.build(0x5678, 0, b"expired");

        match proxy.forward(packet) {
            ForwardResult::Dropped(ProxyError::TtlExpired) => {}
            _ => panic!("expected TTL expired"),
        }

        assert_eq!(proxy.stats().packets_dropped, 1);
    }

    #[tokio::test]
    async fn test_proxy_no_route() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        // Build a packet with no route
        let builder = MultiHopPacketBuilder::new(0xABCD);
        let packet = builder.build(0x9999, 8, b"no route");

        match proxy.forward(packet) {
            ForwardResult::Dropped(ProxyError::NoRoute) => {}
            _ => panic!("expected no route"),
        }
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #116: pre-fix
    /// `remove_route(dest_id)` only deleted the next_hop entry
    /// and left `hop_stats[dest_id]` in place. A peer churning
    /// through many destinations grew `hop_stats` linearly with
    /// total-distinct-dest-ids-ever-seen, not active dest count
    /// — unbounded memory growth. Post-fix `remove_route` also
    /// drops the matching `hop_stats` entry.
    #[tokio::test]
    async fn remove_route_also_drops_hop_stats() {
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        let next_hop: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        proxy.add_route(0x5678, next_hop);

        // Force hop_stats to populate by recording activity.
        proxy.record_hop_forward(0x5678, 100, 1000);
        proxy.record_hop_drop(0x5678);
        assert!(
            proxy.hop_stats(0x5678).is_some(),
            "hop_stats must be present after recording activity"
        );

        // Remove the route — pre-fix would leave the hop_stats
        // entry behind. Post-fix it's dropped.
        proxy.remove_route(0x5678);

        assert!(
            proxy.hop_stats(0x5678).is_none(),
            "hop_stats entry must be dropped along with the route — \
             pre-fix this leaked memory linearly with churned destinations"
        );
    }

    #[test]
    fn test_hop_stats() {
        let stats = HopStats::new();

        stats.record_forward(100, 1000);
        stats.record_forward(200, 2000);
        stats.record_drop();

        assert_eq!(stats.forwarded(), 2);
        assert_eq!(stats.dropped(), 1);
        assert_eq!(stats.avg_latency_ns(), 1500);
    }

    #[tokio::test]
    async fn test_forward_returns_packet_data() {
        // Regression: forward() built fwd_data with the updated routing header
        // but discarded it, returning only next_hop and latency. Callers
        // (including forward_and_send) had no packet to actually transmit.
        let config = ProxyConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let proxy = NetProxy::new(config).await.unwrap();

        let next_hop: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        proxy.add_route(0x5678, next_hop);

        let builder = MultiHopPacketBuilder::new(0xABCD);
        let packet = builder.build(0x5678, 4, b"payload");

        match proxy.forward(packet) {
            ForwardResult::Forwarded {
                next_hop: addr,
                packet: fwd_packet,
                ..
            } => {
                assert_eq!(addr, next_hop);

                // The forwarded packet must contain the updated routing header
                let header = RoutingHeader::from_bytes(&fwd_packet[..ROUTING_HEADER_SIZE]).unwrap();
                assert_eq!(header.ttl, 3, "TTL should be decremented");
                assert_eq!(header.hop_count, 1, "hop_count should be incremented");

                // Payload must be preserved after the routing header
                assert_eq!(&fwd_packet[ROUTING_HEADER_SIZE..], b"payload");
            }
            _ => panic!("expected forwarded"),
        }
    }
}
