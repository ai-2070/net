//! Net Router for single-hop and multi-hop packet routing.
//!
//! The router handles:
//! - Stream multiplexing across thousands of streams
//! - Fair scheduling to prevent stream starvation
//! - Low-latency packet forwarding
//! - Per-stream statistics

use bytes::{Bytes, BytesMut};
use crossbeam_queue::ArrayQueue;
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Notify;

use super::protocol::HEADER_SIZE;
use super::route::{RoutingHeader, RoutingTable, ROUTING_HEADER_SIZE};

/// Router configuration
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Local node ID
    pub local_id: u64,
    /// Bind address
    pub bind_addr: SocketAddr,
    /// Maximum queue depth per stream
    pub max_queue_depth: usize,
    /// Fair scheduling quantum (packets per stream per round)
    pub fair_quantum: usize,
    /// Idle stream timeout (nanoseconds)
    pub idle_timeout_ns: u64,
    /// Enable priority queue bypass
    pub priority_bypass: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            local_id: 0,
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            max_queue_depth: 1024,
            fair_quantum: 16,
            idle_timeout_ns: 30_000_000_000, // 30 seconds
            priority_bypass: true,
        }
    }
}

impl RouterConfig {
    /// Create a new router config with defaults
    pub fn new(local_id: u64, bind_addr: SocketAddr) -> Self {
        Self {
            local_id,
            bind_addr,
            ..Default::default()
        }
    }
}

/// Queued packet for fair scheduling
pub struct QueuedPacket {
    /// Packet data
    pub data: Bytes,
    /// Destination address
    pub dest: SocketAddr,
    /// Stream identifier
    pub stream_id: u64,
    /// Whether this is a priority packet
    pub priority: bool,
    /// Time the packet was queued
    pub queued_at: Instant,
}

/// Per-stream queue for fair scheduling
struct StreamQueue {
    queue: ArrayQueue<QueuedPacket>,
    packets_sent_this_round: AtomicU64,
    /// Fairness weight (quantum multiplier). `1` is equal-share; higher
    /// values let this stream ship `weight × session_quantum` packets
    /// per round before round-reset. Always ≥ 1.
    weight: AtomicU64,
}

impl StreamQueue {
    fn new(capacity: usize) -> Self {
        Self::new_with_weight(capacity, 1)
    }

    fn new_with_weight(capacity: usize, weight: u8) -> Self {
        Self {
            queue: ArrayQueue::new(capacity),
            packets_sent_this_round: AtomicU64::new(0),
            weight: AtomicU64::new(weight.max(1) as u64),
        }
    }

    fn push(&self, packet: QueuedPacket) -> Result<(), QueuedPacket> {
        self.queue.push(packet)
    }

    fn pop(&self) -> Option<QueuedPacket> {
        self.queue.pop()
    }

    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.queue.len()
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn reset_round(&self) {
        self.packets_sent_this_round.store(0, Ordering::Relaxed);
    }

    fn increment_sent(&self) -> u64 {
        self.packets_sent_this_round.fetch_add(1, Ordering::Relaxed)
    }

    fn sent_this_round(&self) -> u64 {
        self.packets_sent_this_round.load(Ordering::Relaxed)
    }

    fn weight(&self) -> u64 {
        self.weight.load(Ordering::Relaxed).max(1)
    }

    #[allow(dead_code)]
    fn set_weight(&self, weight: u8) {
        self.weight.store(weight.max(1) as u64, Ordering::Relaxed);
    }
}

/// Fair scheduler for stream fairness
pub struct FairScheduler {
    /// Per-stream queues
    streams: DashMap<u64, Arc<StreamQueue>>,
    /// Priority queue (bypasses fair scheduling)
    priority_queue: ArrayQueue<QueuedPacket>,
    /// Fair quantum
    quantum: usize,
    /// Max queue depth per stream
    max_depth: usize,
    /// Notification for new packets
    notify: Notify,
    /// Total packets queued
    total_queued: AtomicU64,
    /// Total packets dropped
    total_dropped: AtomicU64,
    /// Rotation index for round-robin fairness across dequeue calls.
    /// Only advances when a dequeue actually pops a packet, so
    /// rotation correlates with service events rather than poll
    /// frequency.
    round_robin_idx: AtomicU64,
    /// Counter incremented on every `dequeue` call (regardless of
    /// whether a packet was popped) for the modulo-1024 cleanup
    /// trigger. Decoupled from `round_robin_idx` so the rotation fix
    /// doesn't accidentally suppress cleanup.
    dequeue_call_count: AtomicU64,
}

impl FairScheduler {
    /// Create a new fair scheduler
    pub fn new(quantum: usize, max_depth: usize) -> Self {
        Self {
            streams: DashMap::new(),
            priority_queue: ArrayQueue::new(max_depth),
            quantum,
            max_depth,
            notify: Notify::new(),
            total_queued: AtomicU64::new(0),
            total_dropped: AtomicU64::new(0),
            round_robin_idx: AtomicU64::new(0),
            dequeue_call_count: AtomicU64::new(0),
        }
    }

    /// Set the fair-scheduling weight for a stream. `weight` is a quantum
    /// multiplier: 1 = equal share (default), higher values give the
    /// stream proportionally more packets per round. Creates the stream
    /// queue if it doesn't exist yet, so callers can set the weight
    /// before any traffic flows.
    pub fn set_stream_weight(&self, stream_id: u64, weight: u8) {
        let weight = weight.max(1);
        self.streams
            .entry(stream_id)
            .and_modify(|q| q.set_weight(weight))
            .or_insert_with(|| Arc::new(StreamQueue::new_with_weight(self.max_depth, weight)));
    }

    /// Enqueue a packet
    pub fn enqueue(&self, packet: QueuedPacket) -> bool {
        if packet.priority {
            // Priority packets bypass fair scheduling
            if self.priority_queue.push(packet).is_ok() {
                self.total_queued.fetch_add(1, Ordering::Relaxed);
                self.notify.notify_one();
                return true;
            }
        } else {
            // Get or create stream queue
            let queue = self
                .streams
                .entry(packet.stream_id)
                .or_insert_with(|| Arc::new(StreamQueue::new(self.max_depth)))
                .clone();

            if queue.push(packet).is_ok() {
                self.total_queued.fetch_add(1, Ordering::Relaxed);
                self.notify.notify_one();
                return true;
            }
        }

        self.total_dropped.fetch_add(1, Ordering::Relaxed);
        false
    }

    /// Dequeue next packet (fair round-robin)
    pub fn dequeue(&self) -> Option<QueuedPacket> {
        // Periodically clean up empty stream queues to prevent unbounded growth
        // of the DashMap. Check every 1024 dequeue calls (cheap modular check).
        // Tracked on a separate counter from `round_robin_idx` so
        // that decoupling rotation from poll frequency doesn't
        // suppress the cleanup trigger.
        let call_count = self
            .dequeue_call_count
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        if call_count.is_multiple_of(1024) {
            self.cleanup_empty();
        }

        // Priority queue first
        if let Some(packet) = self.priority_queue.pop() {
            return Some(packet);
        }

        // Collect stream keys for stable, rotated iteration order
        let keys: Vec<u64> = self.streams.iter().map(|e| *e.key()).collect();
        if keys.is_empty() {
            return None;
        }
        // Advance the round-robin cursor only when we actually pop a
        // packet. Previously this used `fetch_add(1)` unconditionally,
        // so every empty poll advanced the rotation as if we'd
        // serviced a stream. The result was that under bursty mixed
        // traffic, polls that found no packet still nudged the cursor
        // past the stream that *would* have had a packet on the next
        // tick, biasing service away from streams that became
        // non-empty
        // mid-pass.
        //
        // Now: read the cursor for the starting offset, then
        // commit a `fetch_add(1)` only inside the successful pop
        // arm. Behavioral effect is the same when packets are
        // available; the difference shows up under contention
        // and on dequeues that find nothing.
        let start = self.round_robin_idx.load(Ordering::Relaxed) as usize % keys.len();

        // Round-robin across streams, starting from the rotated index.
        // Each stream's quantum is `base_quantum × stream.weight`, so
        // a weight-4 stream gets 4× the packets per round of a weight-1
        // stream before round-reset. Default weight is 1 = unchanged.
        for i in 0..keys.len() {
            let key = keys[(start + i) % keys.len()];
            if let Some(queue) = self.streams.get(&key) {
                let stream_quantum = (self.quantum as u64).saturating_mul(queue.weight());
                if queue.sent_this_round() < stream_quantum && !queue.is_empty() {
                    if let Some(packet) = queue.pop() {
                        queue.increment_sent();
                        // Advance the rotation cursor only on
                        // successful pop.
                        self.round_robin_idx.fetch_add(1, Ordering::Relaxed);
                        return Some(packet);
                    }
                }
            }
        }

        // If all streams exhausted their quantum, reset and try again.
        // Re-collect keys so that streams added since the first snapshot
        // are also considered — using the stale `keys` vec would miss them.
        let keys: Vec<u64> = self.streams.iter().map(|e| *e.key()).collect();
        if keys.is_empty() {
            return None;
        }
        let mut has_packets = false;
        for key in &keys {
            if let Some(queue) = self.streams.get(key) {
                queue.reset_round();
                if !queue.is_empty() {
                    has_packets = true;
                }
            }
        }

        if has_packets {
            let start = self.round_robin_idx.load(Ordering::Relaxed) as usize % keys.len();
            for i in 0..keys.len() {
                let key = keys[(start + i) % keys.len()];
                if let Some(queue) = self.streams.get(&key) {
                    if let Some(packet) = queue.pop() {
                        queue.increment_sent();
                        self.round_robin_idx.fetch_add(1, Ordering::Relaxed);
                        return Some(packet);
                    }
                }
            }
        }

        None
    }

    /// Wait for new packets
    pub async fn wait(&self) {
        self.notify.notified().await;
    }

    /// Get total queued count
    pub fn total_queued(&self) -> u64 {
        self.total_queued.load(Ordering::Relaxed)
    }

    /// Get total dropped count
    pub fn total_dropped(&self) -> u64 {
        self.total_dropped.load(Ordering::Relaxed)
    }

    /// Get number of active streams
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Clean up empty stream queues.
    ///
    /// Only removes queues that are both empty and have no outstanding
    /// `Arc` references (strong_count == 1 means only the DashMap holds it).
    /// This prevents a race where `enqueue` clones the Arc, cleanup removes
    /// the entry, and the enqueued packet becomes unreachable.
    pub fn cleanup_empty(&self) -> usize {
        let mut removed = 0;
        self.streams.retain(|_, queue| {
            if queue.is_empty() && Arc::strong_count(queue) == 1 {
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
    }
}

/// Router statistics
#[derive(Debug, Clone, Default)]
pub struct RouterStats {
    /// Packets received
    pub packets_received: u64,
    /// Packets forwarded
    pub packets_forwarded: u64,
    /// Packets delivered locally
    pub packets_local: u64,
    /// Packets dropped (TTL, no route, queue full)
    pub packets_dropped: u64,
    /// Bytes received
    pub bytes_received: u64,
    /// Bytes forwarded
    pub bytes_forwarded: u64,
    /// Active routes
    pub routes: usize,
    /// Active streams
    pub streams: usize,
    /// Average routing latency (nanoseconds)
    pub avg_latency_ns: u64,
}

/// Net Router
pub struct NetRouter {
    /// Configuration
    #[allow(dead_code)]
    config: RouterConfig,
    /// UDP socket
    socket: Arc<UdpSocket>,
    /// Routing table
    routing_table: Arc<RoutingTable>,
    /// Fair scheduler
    scheduler: Arc<FairScheduler>,
    /// Running flag
    running: Arc<AtomicBool>,
    /// Statistics
    packets_received: AtomicU64,
    packets_forwarded: AtomicU64,
    packets_local: AtomicU64,
    packets_dropped: AtomicU64,
    bytes_received: AtomicU64,
    bytes_forwarded: AtomicU64,
    total_latency_ns: AtomicU64,
    latency_samples: AtomicU64,
}

impl NetRouter {
    /// Create a new router
    pub async fn new(config: RouterConfig) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        let routing_table = Arc::new(RoutingTable::new(config.local_id));
        let scheduler = Arc::new(FairScheduler::new(
            config.fair_quantum,
            config.max_queue_depth,
        ));

        Ok(Self {
            config,
            socket: Arc::new(socket),
            routing_table,
            scheduler,
            running: Arc::new(AtomicBool::new(false)),
            packets_received: AtomicU64::new(0),
            packets_forwarded: AtomicU64::new(0),
            packets_local: AtomicU64::new(0),
            packets_dropped: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_forwarded: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            latency_samples: AtomicU64::new(0),
        })
    }

    /// Get local address
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Get routing table
    pub fn routing_table(&self) -> &Arc<RoutingTable> {
        &self.routing_table
    }

    /// Get the fair scheduler. Exposed so `MeshNode::open_stream` can
    /// propagate a stream's `fairness_weight` to the forwarding path.
    pub fn scheduler(&self) -> &Arc<FairScheduler> {
        &self.scheduler
    }

    /// Add a route
    pub fn add_route(&self, dest_id: u64, next_hop: SocketAddr) {
        self.routing_table.add_route(dest_id, next_hop);
    }

    /// Remove a route
    pub fn remove_route(&self, dest_id: u64) {
        self.routing_table.remove_route(dest_id);
    }

    /// Route a packet (called from receive loop)
    pub fn route_packet(&self, data: Bytes, _from: SocketAddr) -> Result<RouteAction, RouterError> {
        let start = Instant::now();
        let len = data.len() as u64;

        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(len, Ordering::Relaxed);

        // Need at least routing header
        if data.len() < ROUTING_HEADER_SIZE {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            return Err(RouterError::PacketTooSmall);
        }

        // Parse routing header
        let routing_header = RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE])
            .ok_or(RouterError::InvalidHeader)?;

        // Extract stream ID from Net header if present
        let stream_id = if data.len() >= ROUTING_HEADER_SIZE + HEADER_SIZE {
            let net_header = &data[ROUTING_HEADER_SIZE..ROUTING_HEADER_SIZE + HEADER_SIZE];
            u64::from_le_bytes(net_header[32..40].try_into().unwrap_or([0; 8]))
        } else {
            0
        };

        // Per-stream `record_in` is deferred until the packet
        // either delivers locally or survives all drop checks
        // and is queued for forward. Pre-fix the call fired
        // here (immediately after parse), so a packet that
        // failed loop suppression or TTL incremented BOTH
        // `packets_in` and `packets_dropped` for the stream —
        // double-counting against any dashboard computing
        // `delivery rate = packets_out / packets_in` (drops
        // inflated the denominator without affecting the
        // numerator, masking healthy networks behind apparent
        // delivery loss). The drop paths now call
        // `record_drop` only; `record_in` fires once for each
        // successful local-delivery / forward.

        // Check if local delivery
        if self.routing_table.is_local(routing_header.dest_id) {
            self.routing_table.record_in(stream_id, len);
            self.packets_local.fetch_add(1, Ordering::Relaxed);
            self.record_latency(start);
            return Ok(RouteAction::Local(data.slice(ROUTING_HEADER_SIZE..)));
        }

        // Loop suppression: if we're about to forward a packet whose
        // `src_id` is us, we sent it earlier and it has come back via
        // a misconfigured route or a malicious peer. Drop it locally
        // so the only thing breaking the loop is TTL exhaustion;
        // every looping hop wastes one queue slot and 2x bandwidth
        // on the link. The `src_id` field is u32; `local_id` is u64
        // (only the low 32 bits identify us on the wire).
        if routing_header.src_id == (self.routing_table.local_id() as u32) {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            self.routing_table.record_drop(stream_id);
            return Err(RouterError::RoutingLoop);
        }

        // Check TTL
        if routing_header.is_expired() {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            self.routing_table.record_drop(stream_id);
            return Err(RouterError::TtlExpired);
        }

        // Lookup next hop
        let next_hop = self
            .routing_table
            .lookup(routing_header.dest_id)
            .ok_or(RouterError::NoRoute)?;

        // Update header for forwarding
        let mut new_data = BytesMut::with_capacity(data.len());
        let mut fwd_header = routing_header;
        fwd_header.forward();
        // Re-check expiry after `forward()` decrements the TTL: if
        // TTL hit 0, the next hop would just drop the packet on its
        // own `is_expired()` check — wasting one forward, bandwidth,
        // and a queue slot per last-hop packet. Drop locally here so
        // the next hop never sees the doomed packet.
        if fwd_header.is_expired() {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            self.routing_table.record_drop(stream_id);
            return Err(RouterError::TtlExpired);
        }
        // All drop gates passed — count this packet as
        // successfully ingressed for the stream before queueing
        // the forward.
        self.routing_table.record_in(stream_id, len);
        fwd_header.write_to(&mut new_data);
        new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);

        // Queue for sending
        let packet = QueuedPacket {
            data: new_data.freeze(),
            dest: next_hop,
            stream_id,
            priority: routing_header.flags.is_priority(),
            queued_at: Instant::now(),
        };

        if self.scheduler.enqueue(packet) {
            self.packets_forwarded.fetch_add(1, Ordering::Relaxed);
            self.bytes_forwarded.fetch_add(len, Ordering::Relaxed);
            self.routing_table.record_out(stream_id, len);
            self.record_latency(start);
            Ok(RouteAction::Forwarded(next_hop))
        } else {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
            self.routing_table.record_drop(stream_id);
            Err(RouterError::QueueFull)
        }
    }

    /// Send a packet directly (bypassing routing)
    pub async fn send_to(&self, data: &[u8], dest: SocketAddr) -> std::io::Result<usize> {
        self.socket.send_to(data, dest).await
    }

    /// Receive a packet
    pub async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }

    /// Start the router (spawns send loop). Returns `None` if a
    /// dispatch loop is already running for this router; calling
    /// twice would otherwise spawn a second loop racing the first
    /// one's `scheduler.dequeue()`, producing reordered or
    /// duplicate sends.
    pub fn start(&self) -> Option<tokio::task::JoinHandle<()>> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }

        let socket = self.socket.clone();
        let scheduler = self.scheduler.clone();
        let running = self.running.clone();

        Some(tokio::spawn(async move {
            while running.load(Ordering::Acquire) {
                // Dequeue and send
                if let Some(packet) = scheduler.dequeue() {
                    let _ = socket.send_to(&packet.data, packet.dest).await;
                } else {
                    // Wait for new packets (with timeout)
                    tokio::select! {
                        _ = scheduler.wait() => {}
                        _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                    }
                }
            }
        }))
    }

    /// Stop the router
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Get statistics
    pub fn stats(&self) -> RouterStats {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let avg_latency = total_latency.checked_div(samples).unwrap_or(0);

        RouterStats {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_forwarded: self.packets_forwarded.load(Ordering::Relaxed),
            packets_local: self.packets_local.load(Ordering::Relaxed),
            packets_dropped: self.packets_dropped.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_forwarded: self.bytes_forwarded.load(Ordering::Relaxed),
            routes: self.routing_table.route_count(),
            streams: self.routing_table.stream_count(),
            avg_latency_ns: avg_latency,
        }
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        self.packets_received.store(0, Ordering::Relaxed);
        self.packets_forwarded.store(0, Ordering::Relaxed);
        self.packets_local.store(0, Ordering::Relaxed);
        self.packets_dropped.store(0, Ordering::Relaxed);
        self.bytes_received.store(0, Ordering::Relaxed);
        self.bytes_forwarded.store(0, Ordering::Relaxed);
        self.total_latency_ns.store(0, Ordering::Relaxed);
        self.latency_samples.store(0, Ordering::Relaxed);
    }

    fn record_latency(&self, start: Instant) {
        let latency = start.elapsed().as_nanos() as u64;
        self.total_latency_ns.fetch_add(latency, Ordering::Relaxed);
        self.latency_samples.fetch_add(1, Ordering::Relaxed);
    }
}

/// Result of routing a packet
#[derive(Debug)]
pub enum RouteAction {
    /// Packet is for local delivery
    Local(Bytes),
    /// Packet was forwarded to next hop
    Forwarded(SocketAddr),
}

/// Router errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterError {
    /// Packet too small
    PacketTooSmall,
    /// Invalid routing header
    InvalidHeader,
    /// TTL expired
    TtlExpired,
    /// No route to destination
    NoRoute,
    /// Queue full
    QueueFull,
    /// Source is this node — packet looped back via a misconfigured
    /// route or hostile peer.
    RoutingLoop,
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PacketTooSmall => write!(f, "packet too small"),
            Self::InvalidHeader => write!(f, "invalid routing header"),
            Self::TtlExpired => write!(f, "TTL expired"),
            Self::NoRoute => write!(f, "no route to destination"),
            Self::QueueFull => write!(f, "queue full"),
            Self::RoutingLoop => write!(f, "routing loop (packet returned to its source)"),
        }
    }
}

impl std::error::Error for RouterError {}

#[cfg(test)]
mod tests {
    use super::super::protocol::NetHeader;
    use super::*;

    #[test]
    fn test_fair_scheduler_basic() {
        let scheduler = FairScheduler::new(2, 16);

        // Enqueue packets from different streams
        for stream in 0..3 {
            for _ in 0..4 {
                let packet = QueuedPacket {
                    data: Bytes::from(vec![0u8; 64]),
                    dest: "127.0.0.1:9000".parse().unwrap(),
                    stream_id: stream,
                    priority: false,
                    queued_at: Instant::now(),
                };
                assert!(scheduler.enqueue(packet));
            }
        }

        assert_eq!(scheduler.stream_count(), 3);
        assert_eq!(scheduler.total_queued(), 12);

        // Dequeue should round-robin
        let mut stream_order = Vec::new();
        while let Some(packet) = scheduler.dequeue() {
            stream_order.push(packet.stream_id);
        }

        // Should have processed all 12 packets
        assert_eq!(stream_order.len(), 12);

        // Check fairness: each stream should get ~4 packets
        let mut counts = [0; 3];
        for stream in stream_order {
            counts[stream as usize] += 1;
        }
        assert_eq!(counts, [4, 4, 4]);
    }

    #[test]
    fn test_fair_scheduler_priority() {
        let scheduler = FairScheduler::new(2, 16);

        // Enqueue normal packets
        for _ in 0..4 {
            let packet = QueuedPacket {
                data: Bytes::from(vec![0u8; 64]),
                dest: "127.0.0.1:9000".parse().unwrap(),
                stream_id: 0,
                priority: false,
                queued_at: Instant::now(),
            };
            scheduler.enqueue(packet);
        }

        // Enqueue priority packet
        let priority = QueuedPacket {
            data: Bytes::from(vec![1u8; 64]),
            dest: "127.0.0.1:9000".parse().unwrap(),
            stream_id: 1,
            priority: true,
            queued_at: Instant::now(),
        };
        scheduler.enqueue(priority);

        // Priority should come first
        let first = scheduler.dequeue().unwrap();
        assert_eq!(first.data[0], 1);
        assert!(first.priority);
    }

    #[test]
    fn test_fair_scheduler_no_starvation() {
        // Regression: dequeue() always started iterating from the beginning
        // of the DashMap, so streams appearing earlier in iteration order
        // were systematically preferred, starving later streams.
        //
        // With the rotation fix, each dequeue() starts from a different
        // position, so all streams should get roughly equal service.
        let quantum = 1;
        let scheduler = FairScheduler::new(quantum, 64);

        // Use many streams to make DashMap iteration-order bias visible
        let num_streams = 8u64;
        let packets_per_stream = 20;

        for stream in 0..num_streams {
            for _ in 0..packets_per_stream {
                let packet = QueuedPacket {
                    data: Bytes::from(vec![stream as u8; 1]),
                    dest: "127.0.0.1:9000".parse().unwrap(),
                    stream_id: stream,
                    priority: false,
                    queued_at: Instant::now(),
                };
                scheduler.enqueue(packet);
            }
        }

        // Dequeue all packets and track which stream each came from
        let mut first_half_counts = vec![0u32; num_streams as usize];
        let total = num_streams * packets_per_stream as u64;
        let half = total / 2;

        for i in 0..total {
            let packet = scheduler.dequeue().unwrap();
            if i < half {
                first_half_counts[packet.stream_id as usize] += 1;
            }
        }

        // In the first half of dequeues, every stream should have been served
        // at least once. Without rotation, some streams could get 0 service.
        for (stream, &count) in first_half_counts.iter().enumerate() {
            assert!(
                count > 0,
                "stream {} was starved in the first half of dequeues ({} of {} packets)",
                stream,
                count,
                half
            );
        }
    }

    #[tokio::test]
    async fn test_router_creation() {
        let config = RouterConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();

        assert!(!router.is_running());
        assert_eq!(router.stats().routes, 0);
    }

    /// `start()` must spawn at most one dispatch loop. A second
    /// call while the first loop is still running would race the
    /// scheduler's `dequeue` and produce reordered or duplicate
    /// sends.
    #[tokio::test]
    async fn start_is_idempotent_returns_none_when_already_running() {
        let config = RouterConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();

        let first = router.start();
        assert!(first.is_some(), "first start() should spawn a loop");
        assert!(router.is_running());

        let second = router.start();
        assert!(
            second.is_none(),
            "second start() while running must NOT spawn a duplicate loop",
        );

        // After stop(), the first loop exits and a fresh start()
        // is allowed again.
        router.stop();
        // Give the loop a tick to observe `running == false`.
        if let Some(h) = first {
            let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
        }

        let third = router.start();
        assert!(
            third.is_some(),
            "start() after stop() should be allowed to spawn again",
        );
        router.stop();
        if let Some(h) = third {
            let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
        }
    }

    #[tokio::test]
    async fn test_router_routing_table() {
        let config = RouterConfig::new(0x1234, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();

        let dest: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        router.add_route(0x5678, dest);

        assert_eq!(router.routing_table().lookup(0x5678), Some(dest));
        assert_eq!(router.stats().routes, 1);
    }

    #[tokio::test]
    async fn test_router_extracts_stream_id_at_correct_offset() {
        // Regression: stream_id was read from bytes 8..16 (inside the nonce)
        // instead of bytes 40..48 where it actually lives in the Net header.
        let local_id = 0x1234u64;
        let config = RouterConfig::new(local_id, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();

        let expected_stream_id: u64 = 0xDEAD_BEEF_CAFE_BABEu64;

        // Build routing header pointing to local_id so we get RouteAction::Local
        let routing = RoutingHeader::new(local_id, 0x5678, 4);
        let routing_bytes = routing.to_bytes();

        // Build a Net header with a known stream_id
        let net = NetHeader::new(
            0xAAAA,             // session_id
            expected_stream_id, // stream_id
            1,                  // sequence
            [0u8; 12],          // nonce
            0,                  // payload_len
            0,                  // event_count
            super::super::protocol::PacketFlags::NONE,
        );
        let net_bytes = net.to_bytes();

        // Concatenate routing header + Net header
        let mut packet = BytesMut::with_capacity(ROUTING_HEADER_SIZE + HEADER_SIZE);
        packet.extend_from_slice(&routing_bytes);
        packet.extend_from_slice(&net_bytes);

        let from: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let _ = router.route_packet(packet.freeze(), from);

        // The stream stats should be keyed by the correct stream_id
        let stats = router.routing_table().get_stream_stats(expected_stream_id);
        assert_eq!(
            stats.get_packets_in(),
            1,
            "stream stats should record 1 packet for stream_id 0x{:X}",
            expected_stream_id
        );
    }

    /// Pin: a packet whose `src_id` matches this router's
    /// `local_id` must be dropped immediately on receipt with
    /// `RoutingLoop` instead of being forwarded again. Pre-fix
    /// the router happily looped the packet back along the
    /// route, only breaking the cycle on TTL exhaustion (so
    /// every loop wasted up to 64 hops × bandwidth before
    /// dying).
    #[tokio::test]
    async fn route_packet_drops_when_src_id_is_local() {
        let local_id = 0x1234u64;
        let dest_id = 0x9999u64;
        let dest_addr: SocketAddr = "127.0.0.2:6000".parse().unwrap();

        let config = RouterConfig::new(local_id, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();
        router.routing_table().add_route(dest_id, dest_addr);

        // RoutingHeader stores src_id as u32 — the low 32 bits of
        // the local node id are what identifies us on the wire.
        let routing = RoutingHeader::new(dest_id, local_id as u32, 16);
        let routing_bytes = routing.to_bytes();

        let net = NetHeader::new(
            0xAAAA,
            0xBEEF,
            1,
            [0u8; 12],
            0,
            0,
            super::super::protocol::PacketFlags::NONE,
        );
        let net_bytes = net.to_bytes();

        let mut packet = BytesMut::with_capacity(ROUTING_HEADER_SIZE + HEADER_SIZE);
        packet.extend_from_slice(&routing_bytes);
        packet.extend_from_slice(&net_bytes);

        let from: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let result = router.route_packet(packet.freeze(), from);
        match result {
            Err(RouterError::RoutingLoop) => {}
            other => panic!(
                "expected RoutingLoop for src_id == local_id, got {:?}",
                other
            ),
        }

        // Counters: dropped += 1, forwarded unchanged.
        let stats = router.stats();
        assert_eq!(
            stats.packets_dropped, 1,
            "looping packet must increment packets_dropped"
        );
        assert_eq!(
            stats.packets_forwarded, 0,
            "looping packet must NOT be forwarded"
        );
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #119: pre-fix
    /// `route_packet` decremented TTL via `forward()` and queued
    /// the packet for the next hop without checking whether the
    /// post-decrement TTL was 0. The next hop received a TTL=0
    /// packet and dropped it on its own `is_expired()` check —
    /// wasting one forward + bandwidth + queue slot per
    /// last-hop packet. Post-fix the router drops here with
    /// `TtlExpired` if `forward()` made the TTL 0.
    #[tokio::test]
    async fn route_packet_drops_when_forward_makes_ttl_zero() {
        let local_id = 0x1234u64;
        let dest_id = 0x9999u64;
        let dest_addr: SocketAddr = "127.0.0.2:6000".parse().unwrap();

        let config = RouterConfig::new(local_id, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();
        router.routing_table().add_route(dest_id, dest_addr);

        // TTL = 1 — `forward()` will decrement to 0. Pre-fix
        // this would still queue the packet (RouteAction::Forwarded);
        // post-fix it's dropped with TtlExpired.
        let routing = RoutingHeader::new(dest_id, 0x5678, 1);
        let routing_bytes = routing.to_bytes();

        let net = NetHeader::new(
            0xAAAA,
            0xBEEF,
            1,
            [0u8; 12],
            0,
            0,
            super::super::protocol::PacketFlags::NONE,
        );
        let net_bytes = net.to_bytes();

        let mut packet = BytesMut::with_capacity(ROUTING_HEADER_SIZE + HEADER_SIZE);
        packet.extend_from_slice(&routing_bytes);
        packet.extend_from_slice(&net_bytes);

        let from: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let result = router.route_packet(packet.freeze(), from);

        match result {
            Err(RouterError::TtlExpired) => {} // expected
            Ok(action) => panic!(
                "post-fix must drop with TtlExpired, not forward (got {:?})",
                action
            ),
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    /// Sanity: a TTL=2 packet should still forward (decrements
    /// to 1 — non-zero, next hop accepts).
    #[tokio::test]
    async fn route_packet_forwards_when_ttl_remains_positive_after_decrement() {
        let local_id = 0x1234u64;
        let dest_id = 0x9999u64;
        let dest_addr: SocketAddr = "127.0.0.2:6000".parse().unwrap();

        let config = RouterConfig::new(local_id, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();
        router.routing_table().add_route(dest_id, dest_addr);

        let routing = RoutingHeader::new(dest_id, 0x5678, 2);
        let routing_bytes = routing.to_bytes();
        let net = NetHeader::new(
            0xAAAA,
            0xBEEF,
            1,
            [0u8; 12],
            0,
            0,
            super::super::protocol::PacketFlags::NONE,
        );
        let net_bytes = net.to_bytes();
        let mut packet = BytesMut::with_capacity(ROUTING_HEADER_SIZE + HEADER_SIZE);
        packet.extend_from_slice(&routing_bytes);
        packet.extend_from_slice(&net_bytes);
        let from: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let result = router.route_packet(packet.freeze(), from);
        match result {
            Ok(RouteAction::Forwarded(addr)) => assert_eq!(addr, dest_addr),
            other => panic!("expected Forwarded, got {:?}", other),
        }
    }

    /// Regression: a packet that fails any drop gate (loop
    /// suppression, pre-decrement TTL, post-decrement TTL) must
    /// NOT increment the per-stream `packets_in` counter — only
    /// `packets_dropped`. Pre-fix `record_in` fired immediately
    /// after parse, so a dropped packet incremented BOTH stream
    /// counters; a dashboard computing `delivery_rate =
    /// packets_out / packets_in` would see drops inflate the
    /// denominator without affecting the numerator, masking
    /// healthy networks behind apparent delivery loss.
    ///
    /// This pins the post-decrement TTL drop path (the audit's
    /// stated trigger), but the structural change applies to
    /// every drop path; the per-stream invariant is now
    /// "packets_in counts only delivered or forwarded packets."
    #[tokio::test]
    async fn ttl_drop_does_not_double_count_packets_in_for_stream() {
        let local_id = 0x1234u64;
        let dest_id = 0x9999u64;
        let dest_addr: SocketAddr = "127.0.0.2:6000".parse().unwrap();

        let config = RouterConfig::new(local_id, "127.0.0.1:0".parse().unwrap());
        let router = NetRouter::new(config).await.unwrap();
        router.routing_table().add_route(dest_id, dest_addr);

        // TTL = 1 hits the post-decrement drop path. Same
        // setup as `route_packet_drops_when_forward_makes_ttl_zero`.
        let routing = RoutingHeader::new(dest_id, 0x5678, 1);
        let routing_bytes = routing.to_bytes();
        // NetHeader::new params: (session_id, stream_id, sequence, nonce, payload_len, event_count, flags)
        let session_id = 0xAAAAu64;
        let stream_id = 0x4242u64;
        let net = NetHeader::new(
            session_id,
            stream_id,
            1,
            [0u8; 12],
            0,
            0,
            super::super::protocol::PacketFlags::NONE,
        );
        let net_bytes = net.to_bytes();
        let mut packet = BytesMut::with_capacity(ROUTING_HEADER_SIZE + HEADER_SIZE);
        packet.extend_from_slice(&routing_bytes);
        packet.extend_from_slice(&net_bytes);
        let from: SocketAddr = "127.0.0.1:5000".parse().unwrap();

        let result = router.route_packet(packet.freeze(), from);
        assert!(matches!(result, Err(RouterError::TtlExpired)));

        let stats = router.routing_table().get_stream_stats(stream_id);
        assert_eq!(
            stats.get_packets_in(),
            0,
            "regression: a dropped packet must NOT increment per-stream \
             packets_in. Pre-fix this counter was incremented before \
             the TTL/loop checks, so drops double-counted as both \
             ingressed and dropped — masking delivery rate dashboards."
        );
        assert_eq!(
            stats.get_drops(),
            1,
            "drop must still increment per-stream packets_dropped"
        );
    }

    #[test]
    fn test_regression_fair_scheduler_cleanup_called() {
        // Regression: FairScheduler never removed empty stream queues from its
        // DashMap. After many unique stream_ids passed through, the map grew
        // without bound, causing O(n) iteration in dequeue() where n is the
        // number of *ever-seen* streams rather than *active* streams. This
        // degraded dequeue latency over time.
        //
        // Fix: dequeue() calls cleanup_empty() every 1024 iterations,
        // removing streams whose queues have been fully drained.
        let scheduler = FairScheduler::new(4, 16);

        // Enqueue packets across many unique streams
        let num_streams = 200u64;
        for stream in 0..num_streams {
            let packet = QueuedPacket {
                data: Bytes::from(vec![0u8; 8]),
                dest: "127.0.0.1:9000".parse().unwrap(),
                stream_id: stream,
                priority: false,
                queued_at: Instant::now(),
            };
            assert!(scheduler.enqueue(packet));
        }

        assert_eq!(scheduler.stream_count(), num_streams as usize);

        // Drain all packets
        let mut drained = 0;
        while scheduler.dequeue().is_some() {
            drained += 1;
        }
        assert_eq!(drained, num_streams as usize);

        // The cleanup triggers every 1024 dequeue calls. We need enough
        // dequeue calls (even returning None) to cross the 1024 boundary.
        // We already did `num_streams` dequeues above; do more no-op
        // dequeues to push past the threshold.
        for _ in 0..(1025 - drained) {
            let _ = scheduler.dequeue();
        }

        // After cleanup, all empty stream queues should have been removed
        assert_eq!(
            scheduler.stream_count(),
            0,
            "empty stream queues must be cleaned up after enough dequeue \
             iterations to prevent unbounded DashMap growth"
        );
    }

    #[test]
    fn test_regression_scheduler_sees_streams_added_after_quantum_exhaustion() {
        // Regression: dequeue() collected stream keys once, then reused
        // the stale snapshot for the retry loop after quantum reset.
        // Streams added between the two loops were invisible until the
        // next dequeue() call, causing extra latency.
        //
        // Fix: re-collect keys before the retry loop.
        let scheduler = FairScheduler::new(1, 16);

        // Stream 0 with 1 packet (quantum = 1, so first pass drains it)
        scheduler.enqueue(QueuedPacket {
            data: Bytes::from_static(b"s0"),
            dest: "127.0.0.1:9000".parse().unwrap(),
            stream_id: 0,
            priority: false,
            queued_at: Instant::now(),
        });

        // Drain stream 0's quantum
        let pkt = scheduler.dequeue().unwrap();
        assert_eq!(pkt.stream_id, 0);

        // Now add stream 1 while the scheduler is "between rounds"
        scheduler.enqueue(QueuedPacket {
            data: Bytes::from_static(b"s1"),
            dest: "127.0.0.1:9000".parse().unwrap(),
            stream_id: 1,
            priority: false,
            queued_at: Instant::now(),
        });

        // Next dequeue should find stream 1
        let pkt = scheduler.dequeue().unwrap();
        assert_eq!(
            pkt.stream_id, 1,
            "newly added stream should be visible after quantum reset"
        );
    }

    /// Fairness weight: a weight-4 stream should ship 4× the packets per
    /// round of a weight-1 stream. With both queues full and one full
    /// round of dequeues, we should see ~4:1 ratio before any round
    /// reset fires.
    #[test]
    fn test_fair_scheduler_respects_stream_weight() {
        // Base quantum = 1: every stream with weight 1 gets 1 packet per
        // round; a weight-4 stream gets 4 packets per round.
        let scheduler = FairScheduler::new(1, 64);

        scheduler.set_stream_weight(1, 1);
        scheduler.set_stream_weight(2, 4);

        let dest: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        // Fill both streams with 8 packets each.
        for stream_id in [1u64, 2u64] {
            for _ in 0..8 {
                scheduler.enqueue(QueuedPacket {
                    data: Bytes::from_static(&[0u8; 16]),
                    dest,
                    stream_id,
                    priority: false,
                    queued_at: Instant::now(),
                });
            }
        }

        // Dequeue 5 packets. The weight-4 stream should ship at least
        // 4 of those 5 in the first round (its quantum is 4; stream 1's
        // quantum is 1). Depending on round-robin start, stream 1 may
        // ship 1 packet in the middle.
        let mut counts = [0u64; 3];
        for _ in 0..5 {
            if let Some(pkt) = scheduler.dequeue() {
                counts[pkt.stream_id as usize] += 1;
            }
        }
        assert_eq!(counts[0], 0);
        assert!(
            counts[2] >= 4,
            "weight-4 stream should ship >= 4 packets in 5 dequeues; \
             saw weight-1={} weight-4={}",
            counts[1],
            counts[2]
        );
        assert!(
            counts[1] <= 1,
            "weight-1 stream should ship <= 1 packet before round reset; \
             saw {}",
            counts[1]
        );
    }

    /// Regression: BUG_REPORT.md #31 — `round_robin_idx` was
    /// `fetch_add(1)`-ed unconditionally on every `dequeue` call,
    /// so polls that returned `None` (no streams have packets)
    /// still rotated the cursor. Under bursty mixed traffic this
    /// biased service away from streams that became non-empty
    /// mid-pass. The fix advances the cursor only when a packet
    /// is actually popped.
    #[test]
    fn round_robin_idx_advances_only_on_successful_pop() {
        let scheduler = FairScheduler::new(1, 64);

        // Empty scheduler: many polls must NOT advance the index.
        let initial = scheduler.round_robin_idx.load(Ordering::Relaxed);
        for _ in 0..1000 {
            assert!(scheduler.dequeue().is_none());
        }
        assert_eq!(
            scheduler.round_robin_idx.load(Ordering::Relaxed),
            initial,
            "empty-poll dequeues must not advance round_robin_idx (#31)"
        );

        // Now enqueue + drain. Each successful pop should advance
        // the index by exactly 1.
        for stream in 0..5u64 {
            scheduler.enqueue(QueuedPacket {
                data: Bytes::from(vec![0u8; 8]),
                dest: "127.0.0.1:9000".parse().unwrap(),
                stream_id: stream,
                priority: false,
                queued_at: Instant::now(),
            });
        }
        let before_drain = scheduler.round_robin_idx.load(Ordering::Relaxed);
        let mut popped = 0;
        while scheduler.dequeue().is_some() {
            popped += 1;
        }
        let after_drain = scheduler.round_robin_idx.load(Ordering::Relaxed);
        assert_eq!(popped, 5);
        assert_eq!(
            after_drain.wrapping_sub(before_drain),
            popped as u64,
            "round_robin_idx must advance by exactly the number of successful pops (#31)"
        );
    }
}
