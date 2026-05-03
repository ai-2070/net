//! Phase 4H: Proximity Graph Integration (PINGWAVE++)
//!
//! This module integrates pingwave discovery with the behavior plane:
//! - Enhanced pingwaves carrying capability summaries
//! - Proximity-aware capability routing
//! - Latency-weighted graph for routing decisions
//! - Integration with load balancer for locality-aware selection
//! - Automatic capability index updates from pingwave data

use dashmap::DashMap;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use super::capability::{CapabilityFilter, CapabilitySet};
use super::loadbalance::{Endpoint, HealthStatus, LoadBalancer, LoadMetrics};
use super::metadata::NodeId;

/// Enhanced pingwave with capability summary
#[derive(Debug, Clone)]
pub struct EnhancedPingwave {
    /// Originating node ID
    pub origin_id: NodeId,
    /// Sequence number (monotonic per origin)
    pub seq: u64,
    /// Time-to-live (hop count remaining)
    pub ttl: u8,
    /// Hops traversed so far
    pub hop_count: u8,
    /// Origin timestamp (microseconds since epoch)
    pub origin_timestamp_us: u64,
    /// Capability summary hash (for quick change detection)
    pub capability_hash: u64,
    /// Capability version
    pub capability_version: u32,
    /// Load summary (0-255, 0=idle, 255=overloaded)
    pub load_level: u8,
    /// Health status
    pub health: HealthStatus,
    /// Primary capabilities (compact representation)
    pub primary_caps: PrimaryCapabilities,
}

/// Compact primary capabilities (fits in 8 bytes)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrimaryCapabilities {
    /// Has GPU
    pub gpu: bool,
    /// Number of model slots
    pub model_slots: u8,
    /// Memory tier (0-7, 0=<1GB, 7=>256GB)
    pub memory_tier: u8,
    /// Available tools bitmap (first 8 common tools)
    pub tools_bitmap: u8,
    /// Custom flags
    pub flags: u32,
}

impl PrimaryCapabilities {
    /// Encode to 8 bytes
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0] = if self.gpu { 1 } else { 0 };
        buf[1] = self.model_slots;
        buf[2] = self.memory_tier;
        buf[3] = self.tools_bitmap;
        buf[4..8].copy_from_slice(&self.flags.to_le_bytes());
        buf
    }

    /// Decode from 8 bytes
    pub fn from_bytes(buf: &[u8; 8]) -> Self {
        Self {
            gpu: buf[0] != 0,
            model_slots: buf[1],
            memory_tier: buf[2],
            tools_bitmap: buf[3],
            flags: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
        }
    }

    /// Create from full capability set
    pub fn from_capability_set(caps: &CapabilitySet) -> Self {
        let memory_tier = match caps.hardware.memory_mb {
            0..=1024 => 0,
            1025..=4096 => 1,
            4097..=8192 => 2,
            8193..=16384 => 3,
            16385..=32768 => 4,
            32769..=65536 => 5,
            65537..=131072 => 6,
            _ => 7,
        };

        Self {
            gpu: caps.hardware.gpu.is_some(),
            model_slots: caps.models.len() as u8,
            memory_tier,
            tools_bitmap: 0, // Could map common tools to bits
            flags: 0,
        }
    }

    /// Quick check if this matches a filter
    pub fn matches_basic(&self, filter: &CapabilityFilter) -> bool {
        if filter.require_gpu && !self.gpu {
            return false;
        }
        true
    }
}

impl EnhancedPingwave {
    /// Wire size in bytes (64 base + 8 primary_caps)
    pub const SIZE: usize = 72;

    /// Create a new enhanced pingwave
    pub fn new(origin_id: NodeId, seq: u64, ttl: u8) -> Self {
        Self {
            origin_id,
            seq,
            ttl,
            hop_count: 0,
            origin_timestamp_us: current_time_us(),
            capability_hash: 0,
            capability_version: 0,
            load_level: 0,
            health: HealthStatus::Healthy,
            primary_caps: PrimaryCapabilities::default(),
        }
    }

    /// Set capability info
    pub fn with_capabilities(
        mut self,
        hash: u64,
        version: u32,
        primary: PrimaryCapabilities,
    ) -> Self {
        self.capability_hash = hash;
        self.capability_version = version;
        self.primary_caps = primary;
        self
    }

    /// Set load info
    pub fn with_load(mut self, load_level: u8, health: HealthStatus) -> Self {
        self.load_level = load_level;
        self.health = health;
        self
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..32].copy_from_slice(&self.origin_id);
        buf[32..40].copy_from_slice(&self.seq.to_le_bytes());
        buf[40] = self.ttl;
        buf[41] = self.hop_count;
        buf[42..50].copy_from_slice(&self.origin_timestamp_us.to_le_bytes());
        buf[50..58].copy_from_slice(&self.capability_hash.to_le_bytes());
        buf[58..62].copy_from_slice(&self.capability_version.to_le_bytes());
        buf[62] = self.load_level;
        buf[63] = self.health as u8;
        buf[64..72].copy_from_slice(&self.primary_caps.to_bytes());
        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        let mut origin_id = [0u8; 32];
        origin_id.copy_from_slice(&buf[0..32]);

        let mut caps_buf = [0u8; 8];
        caps_buf.copy_from_slice(&buf[64..72]);

        Some(Self {
            origin_id,
            seq: u64::from_le_bytes(buf[32..40].try_into().ok()?),
            ttl: buf[40],
            hop_count: buf[41],
            origin_timestamp_us: u64::from_le_bytes(buf[42..50].try_into().ok()?),
            capability_hash: u64::from_le_bytes(buf[50..58].try_into().ok()?),
            capability_version: u32::from_le_bytes(buf[58..62].try_into().ok()?),
            load_level: buf[62],
            // Previously coerced any unknown byte to
            // `HealthStatus::Unknown`. A flipped byte downgrades the
            // peer to `Unknown`, which `can_receive_traffic()` treats
            // as unroutable — silent peer eviction on a single
            // bit-flip. `from_bytes` callers already handle `None`
            // (this returns `Option<Self>`), so refuse the parse on
            // unknown discriminants instead of guessing.
            health: match buf[63] {
                0 => HealthStatus::Healthy,
                1 => HealthStatus::Degraded,
                2 => HealthStatus::Unhealthy,
                3 => HealthStatus::Unknown,
                _ => return None,
            },
            primary_caps: PrimaryCapabilities::from_bytes(&caps_buf),
        })
    }

    /// Check if expired
    pub fn is_expired(&self) -> bool {
        self.ttl == 0
    }

    /// Forward (decrement TTL, increment hop count)
    pub fn forward(&mut self) -> bool {
        if self.ttl == 0 {
            return false;
        }
        self.ttl -= 1;
        self.hop_count = self.hop_count.saturating_add(1);
        true
    }

    /// Calculate one-way latency estimate (microseconds)
    pub fn latency_estimate_us(&self) -> u64 {
        let now = current_time_us();
        now.saturating_sub(self.origin_timestamp_us)
    }
}

/// Proximity node info combining discovery and capability data
#[derive(Debug)]
pub struct ProximityNode {
    /// Node ID
    pub node_id: NodeId,
    /// Network address
    pub addr: SocketAddr,
    /// Hop distance
    pub hops: u8,
    /// Estimated latency in microseconds
    pub latency_us: u64,
    /// Last seen timestamp
    pub last_seen: Instant,
    /// Latest pingwave sequence
    pub last_seq: u64,
    /// Capability hash (for change detection)
    pub capability_hash: u64,
    /// Capability version
    pub capability_version: u32,
    /// Primary capabilities (quick filter)
    pub primary_caps: PrimaryCapabilities,
    /// Current load level
    pub load_level: u8,
    /// Health status
    pub health: HealthStatus,
    /// Full capabilities (lazy loaded)
    full_capabilities: RwLock<Option<CapabilitySet>>,
}

impl Clone for ProximityNode {
    fn clone(&self) -> Self {
        Self {
            node_id: self.node_id,
            addr: self.addr,
            hops: self.hops,
            latency_us: self.latency_us,
            last_seen: self.last_seen,
            last_seq: self.last_seq,
            capability_hash: self.capability_hash,
            capability_version: self.capability_version,
            primary_caps: self.primary_caps,
            load_level: self.load_level,
            health: self.health,
            full_capabilities: RwLock::new(self.full_capabilities.read().unwrap().clone()),
        }
    }
}

impl ProximityNode {
    /// Create new proximity node from pingwave
    pub fn from_pingwave(pw: &EnhancedPingwave, addr: SocketAddr) -> Self {
        // `pw.hop_count + 1` would panic in debug at u8::MAX and
        // silently wrap to 0 in release. A buggy or malicious peer
        // can advertise `hop_count == 255`, after which:
        //   - Debug builds would panic the receive loop.
        //   - Release builds would record `hops=0`, falsely
        //     promoting the node to "directly connected" status —
        //     a proximity-routing poisoning vector.
        // `saturating_add(1)` keeps hops at 255 in the overflow
        // case; combined with `MAX_HOPS` cap on routing
        // installation, this is a non-poisoning floor.
        Self {
            node_id: pw.origin_id,
            addr,
            hops: pw.hop_count.saturating_add(1),
            latency_us: pw.latency_estimate_us(),
            last_seen: Instant::now(),
            last_seq: pw.seq,
            capability_hash: pw.capability_hash,
            capability_version: pw.capability_version,
            primary_caps: pw.primary_caps,
            load_level: pw.load_level,
            health: pw.health,
            full_capabilities: RwLock::new(None),
        }
    }

    /// Update from new pingwave
    pub fn update_from_pingwave(&mut self, pw: &EnhancedPingwave, addr: SocketAddr) {
        // Same `+ 1` overflow concern as `from_pingwave`. Use
        // `saturating_add` here too. The "better path" comparison
        // also uses the saturated value so a 255-hop pingwave
        // can never falsely beat a real path.
        let new_hops = pw.hop_count.saturating_add(1);

        // Separate freshness from path quality. Pre-fix the OR
        // (`seq > last_seq || new_hops < self.hops`) let a flooded
        // high-seq pingwave delivered through a long route demote
        // a previously-cached direct route — the freshness arm
        // accepted the new (worse) path purely because of seq
        // monotonicity. Now: track `last_seq` as the freshness
        // signal even on long routes, but only adopt the new
        // `addr` / `hops` / `latency_us` when the path is
        // genuinely no worse than what we have.
        if pw.seq > self.last_seq {
            self.last_seq = pw.seq;
        }
        if new_hops <= self.hops {
            self.addr = addr;
            self.hops = new_hops;
            self.latency_us = pw.latency_estimate_us();
        }

        // Always update load/health from latest
        self.load_level = pw.load_level;
        self.health = pw.health;
        self.last_seen = Instant::now();

        // Check capability change
        if pw.capability_version > self.capability_version {
            self.capability_hash = pw.capability_hash;
            self.capability_version = pw.capability_version;
            self.primary_caps = pw.primary_caps;
            // Clear cached full capabilities
            *self.full_capabilities.write().unwrap() = None;
        }
    }

    /// Check if node is stale
    pub fn is_stale(&self, timeout: Duration) -> bool {
        self.last_seen.elapsed() > timeout
    }

    /// Check if node is available for routing
    pub fn is_available(&self) -> bool {
        self.health.can_receive_traffic()
    }

    /// Get or fetch full capabilities
    pub fn get_capabilities(&self) -> Option<CapabilitySet> {
        self.full_capabilities.read().unwrap().clone()
    }

    /// Set full capabilities (after fetching)
    pub fn set_capabilities(&self, caps: CapabilitySet) {
        *self.full_capabilities.write().unwrap() = Some(caps);
    }

    /// Calculate routing score (lower is better)
    pub fn routing_score(&self, prefer_low_latency: bool) -> f64 {
        let latency_factor = if prefer_low_latency {
            (self.latency_us as f64) / 1000.0 // Convert to ms
        } else {
            self.hops as f64 * 10.0 // 10ms per hop estimate
        };

        let load_factor = (self.load_level as f64) / 255.0 * 50.0; // 0-50 penalty

        let health_factor = match self.health {
            HealthStatus::Healthy => 0.0,
            HealthStatus::Degraded => 25.0,
            HealthStatus::Unhealthy => 1000.0,
            HealthStatus::Unknown => 50.0,
        };

        latency_factor + load_factor + health_factor
    }
}

/// Edge in the proximity graph
#[derive(Debug, Clone)]
pub struct ProximityEdge {
    /// Source node
    pub from: NodeId,
    /// Destination node
    pub to: NodeId,
    /// Latency in microseconds
    pub latency_us: u64,
    /// Last updated
    pub last_updated: Instant,
    /// Reliability (0.0-1.0, based on packet loss)
    pub reliability: f32,
}

/// Configuration for the proximity graph
#[derive(Debug, Clone)]
pub struct ProximityConfig {
    /// Maximum hops to track
    pub radius: u8,
    /// Node timeout
    pub node_timeout: Duration,
    /// Pingwave dedup cache timeout
    pub dedup_timeout: Duration,
    /// Pingwave interval
    pub pingwave_interval: Duration,
    /// Whether to prefer low latency over hop count
    pub prefer_low_latency: bool,
    /// Maximum nodes to track
    pub max_nodes: usize,
    /// Whether to auto-update capability index
    pub auto_index_update: bool,
}

impl Default for ProximityConfig {
    fn default() -> Self {
        Self {
            radius: 3,
            node_timeout: Duration::from_secs(30),
            dedup_timeout: Duration::from_secs(10),
            pingwave_interval: Duration::from_secs(5),
            prefer_low_latency: true,
            max_nodes: 10000,
            auto_index_update: true,
        }
    }
}

/// Proximity graph integrating discovery with behavior plane
pub struct ProximityGraph {
    /// Local node ID
    my_id: NodeId,
    /// Configuration
    config: ProximityConfig,
    /// Known nodes
    nodes: DashMap<NodeId, ProximityNode>,
    /// Edges (from, to) -> edge info
    edges: DashMap<(NodeId, NodeId), ProximityEdge>,
    /// Seen pingwaves for deduplication
    seen_pingwaves: DashMap<(NodeId, u64), Instant>,
    /// Next pingwave sequence
    next_seq: AtomicU64,
    /// Local capability hash
    local_capability_hash: AtomicU64,
    /// Local capability version
    local_capability_version: AtomicU64,
    /// Local capabilities
    local_capabilities: RwLock<Option<CapabilitySet>>,
    /// Local load level
    local_load_level: AtomicU64,
    /// Statistics
    stats: ProximityStats,
}

/// Proximity graph statistics
#[derive(Debug, Default)]
pub struct ProximityStats {
    /// Number of ping waves initiated by this node
    pub pingwaves_sent: AtomicU64,
    /// Number of ping waves received from other nodes
    pub pingwaves_received: AtomicU64,
    /// Number of ping waves forwarded to neighbors
    pub pingwaves_forwarded: AtomicU64,
    /// Number of ping waves dropped due to deduplication or TTL expiry
    pub pingwaves_dropped: AtomicU64,
    /// Number of new nodes discovered through ping waves
    pub nodes_discovered: AtomicU64,
    /// Number of nodes removed after failing liveness checks
    pub nodes_expired: AtomicU64,
    /// Number of capability set updates processed
    pub capability_updates: AtomicU64,
}

impl ProximityGraph {
    /// Create a new proximity graph
    pub fn new(my_id: NodeId, config: ProximityConfig) -> Self {
        Self {
            my_id,
            config,
            nodes: DashMap::new(),
            edges: DashMap::new(),
            seen_pingwaves: DashMap::new(),
            next_seq: AtomicU64::new(1),
            local_capability_hash: AtomicU64::new(0),
            local_capability_version: AtomicU64::new(0),
            local_capabilities: RwLock::new(None),
            local_load_level: AtomicU64::new(0),
            stats: ProximityStats::default(),
        }
    }

    /// Get local node ID
    pub fn my_id(&self) -> NodeId {
        self.my_id
    }

    /// Set local capabilities
    pub fn set_local_capabilities(&self, caps: CapabilitySet) {
        let hash = hash_capabilities(&caps);
        let version = self
            .local_capability_version
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        self.local_capability_hash.store(hash, Ordering::Relaxed);
        self.local_capability_version
            .store(version, Ordering::Relaxed);
        *self.local_capabilities.write().unwrap() = Some(caps);
    }

    /// Set local load level (0-255)
    pub fn set_local_load(&self, load_level: u8) {
        self.local_load_level
            .store(load_level as u64, Ordering::Relaxed);
    }

    /// Create a pingwave to broadcast
    pub fn create_pingwave(&self, health: HealthStatus) -> EnhancedPingwave {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let caps = self.local_capabilities.read().unwrap();
        let primary = caps
            .as_ref()
            .map(PrimaryCapabilities::from_capability_set)
            .unwrap_or_default();

        self.stats.pingwaves_sent.fetch_add(1, Ordering::Relaxed);

        EnhancedPingwave::new(self.my_id, seq, self.config.radius)
            .with_capabilities(
                self.local_capability_hash.load(Ordering::Relaxed),
                self.local_capability_version.load(Ordering::Relaxed) as u32,
                primary,
            )
            .with_load(self.local_load_level.load(Ordering::Relaxed) as u8, health)
    }

    /// Back-compat shim: attribute the pingwave as if it arrived
    /// directly from its origin (i.e. `from_node = pw.origin_id`).
    /// Tests and benchmarks that don't model a separate forwarding
    /// hop call this shape; production dispatch should use the full
    /// [`Self::on_pingwave_from`] so multi-hop edge attribution is
    /// correct.
    pub fn on_pingwave(
        &self,
        pw: EnhancedPingwave,
        from_addr: SocketAddr,
    ) -> Option<EnhancedPingwave> {
        let from_node = pw.origin_id;
        self.on_pingwave_from(pw, from_node, from_addr)
    }

    /// Process incoming pingwave.
    ///
    /// `from_node` is the graph-id of the **direct peer** that just
    /// forwarded this pingwave to us (i.e. the sender on the wire), not
    /// the pingwave's origin. On multi-hop paths `from_node` differs
    /// from `pw.origin_id`; on the first-hop case (a pingwave direct
    /// from its origin) they match.
    ///
    /// Returns `Some(pingwave)` if it should be forwarded, `None`
    /// otherwise.
    pub fn on_pingwave_from(
        &self,
        mut pw: EnhancedPingwave,
        from_node: NodeId,
        from_addr: SocketAddr,
    ) -> Option<EnhancedPingwave> {
        self.stats
            .pingwaves_received
            .fetch_add(1, Ordering::Relaxed);

        // Ignore our own pingwaves (origin self-check — also defends
        // against a buffered pingwave we emitted earlier being replayed
        // back at us by a partitioned-then-healed peer).
        if pw.origin_id == self.my_id {
            return None;
        }

        // Check dedup cache
        let key = (pw.origin_id, pw.seq);
        if self.seen_pingwaves.contains_key(&key) {
            self.stats.pingwaves_dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.seen_pingwaves.insert(key, Instant::now());

        // Update or create node
        let _is_new = !self.nodes.contains_key(&pw.origin_id);
        self.nodes
            .entry(pw.origin_id)
            .and_modify(|node| node.update_from_pingwave(&pw, from_addr))
            .or_insert_with(|| {
                self.stats.nodes_discovered.fetch_add(1, Ordering::Relaxed);
                ProximityNode::from_pingwave(&pw, from_addr)
            });

        // Topology edges: a pingwave carrying origin Y that we just
        // received via direct peer Z tells us two facts:
        //   * we have a direct edge to Z (already true by
        //     construction — Z is our direct peer),
        //   * Z has a route to Y (otherwise Z wouldn't be forwarding).
        //
        // The first is redundant after the initial insert; the
        // `last_updated` refresh on re-insert is the cheap liveness
        // signal. The second is what makes `path_to(Y)` able to
        // return multi-hop paths.
        //
        // Latency estimate: `now_us − pw.origin_timestamp_us` is a
        // noisy one-way delay; clock-skew-sensitive, but good enough
        // as an equal-hop tiebreaker. EWMA (α = 1/8) smooths
        // successive samples per `(from, to)` pair.
        //
        // Throttle the self-edge `(my_id → Z)` update: a hot
        // pingwave-receive path (one per peer per heartbeat
        // interval, scaled across N peers) hit the DashMap
        // entry lock + `Instant::now()` on every receive even
        // though the liveness signal only needs second-level
        // freshness. Skip the update when the existing edge is
        // less than a second old; the multi-hop edge below still
        // refreshes unconditionally because it carries a fresh
        // latency sample.
        let now_us = current_time_us();
        let sample_us = now_us.saturating_sub(pw.origin_timestamp_us);
        let needs_self_edge_refresh = self
            .edges
            .get(&(self.my_id, from_node))
            .map(|e| e.last_updated.elapsed() >= Duration::from_secs(1))
            .unwrap_or(true);
        if needs_self_edge_refresh {
            self.insert_or_update_edge(self.my_id, from_node, 0);
        }
        if from_node != pw.origin_id {
            self.insert_or_update_edge(from_node, pw.origin_id, sample_us);
        }

        // Check if should forward
        if pw.is_expired() {
            return None;
        }

        // Forward
        pw.forward();
        self.stats
            .pingwaves_forwarded
            .fetch_add(1, Ordering::Relaxed);
        Some(pw)
    }

    /// Insert or refresh an edge. If the edge already exists, EWMA the
    /// latency sample into `latency_us` (α = 1/8) and bump
    /// `last_updated`. `sample_us == 0` means "no latency info" (e.g.
    /// the self → peer edge added at session setup); leave the
    /// existing latency alone in that case.
    fn insert_or_update_edge(&self, from: NodeId, to: NodeId, sample_us: u64) {
        self.edges
            .entry((from, to))
            .and_modify(|edge| {
                if sample_us > 0 {
                    // α = 1/8 EWMA on integer microseconds.
                    let prev = edge.latency_us;
                    edge.latency_us = prev - prev / 8 + sample_us / 8;
                }
                edge.last_updated = Instant::now();
            })
            .or_insert(ProximityEdge {
                from,
                to,
                latency_us: sample_us,
                last_updated: Instant::now(),
                reliability: 1.0,
            });
    }

    /// Drop edges whose `last_updated` is older than `max_age`. Called
    /// from the heartbeat-loop tick alongside `RoutingTable::sweep_stale`
    /// so the graph and the routing table age out in lockstep. Returns
    /// the number of edges removed.
    ///
    /// Uses `DashMap::retain` so the staleness check + remove is
    /// atomic per entry. A collect-stale-keys-then-remove shape would
    /// race with concurrent pingwave receipt: another thread could
    /// refresh an edge's `last_updated` between the collect and
    /// remove phases, and we'd delete a freshly-alive edge.
    pub fn sweep_stale_edges(&self, max_age: Duration) -> usize {
        let cutoff = match Instant::now().checked_sub(max_age) {
            Some(c) => c,
            None => return 0,
        };
        let mut removed = 0usize;
        self.edges.retain(|_, edge| {
            let is_stale = edge.last_updated < cutoff;
            if is_stale {
                removed += 1;
            }
            !is_stale
        });
        removed
    }

    /// Update capabilities for a node (from full capability fetch)
    pub fn update_node_capabilities(&self, node_id: &NodeId, caps: CapabilitySet) {
        if let Some(node) = self.nodes.get(node_id) {
            node.set_capabilities(caps);
            self.stats
                .capability_updates
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Get node info
    pub fn get_node(&self, node_id: &NodeId) -> Option<ProximityNode> {
        self.nodes.get(node_id).map(|r| r.clone())
    }

    /// Get all nodes
    pub fn all_nodes(&self) -> Vec<ProximityNode> {
        self.nodes.iter().map(|r| r.value().clone()).collect()
    }

    /// Get nodes within hop distance
    pub fn nodes_within_hops(&self, max_hops: u8) -> Vec<ProximityNode> {
        self.nodes
            .iter()
            .filter(|r| r.hops <= max_hops)
            .map(|r| r.value().clone())
            .collect()
    }

    /// Find nodes matching a capability filter (quick check using primary caps)
    pub fn find_matching(&self, filter: &CapabilityFilter) -> Vec<ProximityNode> {
        self.nodes
            .iter()
            .filter(|r| r.is_available() && r.primary_caps.matches_basic(filter))
            .map(|r| r.value().clone())
            .collect()
    }

    /// Find best node for a capability filter
    pub fn find_best(&self, filter: &CapabilityFilter) -> Option<ProximityNode> {
        self.find_matching(filter).into_iter().min_by(|a, b| {
            a.routing_score(self.config.prefer_low_latency)
                .total_cmp(&b.routing_score(self.config.prefer_low_latency))
        })
    }

    /// Find k best nodes for a capability filter
    pub fn find_k_best(&self, filter: &CapabilityFilter, k: usize) -> Vec<ProximityNode> {
        let mut matching = self.find_matching(filter);
        matching.sort_by(|a, b| {
            a.routing_score(self.config.prefer_low_latency)
                .total_cmp(&b.routing_score(self.config.prefer_low_latency))
        });
        matching.truncate(k);
        matching
    }

    /// Get shortest path to node (BFS).
    ///
    /// Uses a parent map to reconstruct the path once on arrival,
    /// avoiding the quadratic `path.clone()`-per-neighbor cost of the
    /// naive "queue of paths" BFS.
    pub fn path_to(&self, dest: &NodeId) -> Option<Vec<NodeId>> {
        if *dest == self.my_id {
            return Some(vec![self.my_id]);
        }

        // Build adjacency from edges
        let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for edge in self.edges.iter() {
            adjacency.entry(edge.from).or_default().push(edge.to);
        }

        // BFS with parent pointers
        let mut parent: HashMap<NodeId, NodeId> = HashMap::new();
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<NodeId> = VecDeque::new();

        queue.push_back(self.my_id);
        visited.insert(self.my_id);

        while let Some(current) = queue.pop_front() {
            if current == *dest {
                // Walk back through the parent map to recover the path.
                let mut path = vec![current];
                let mut node = current;
                while node != self.my_id {
                    node = *parent.get(&node)?;
                    path.push(node);
                }
                path.reverse();
                return Some(path);
            }

            if let Some(neighbors) = adjacency.get(&current) {
                for &neighbor in neighbors {
                    if visited.insert(neighbor) {
                        parent.insert(neighbor, current);
                        queue.push_back(neighbor);
                    }
                }
            }
        }

        None
    }

    /// Create load balancer endpoints from proximity nodes
    pub fn to_endpoints(&self, filter: Option<&CapabilityFilter>) -> Vec<Endpoint> {
        self.nodes
            .iter()
            .filter(|r| {
                r.is_available()
                    && filter
                        .map(|f| r.primary_caps.matches_basic(f))
                        .unwrap_or(true)
            })
            .map(|r| {
                let node = r.value();
                // Weight inversely proportional to latency/hops
                let base_weight = 1000u32;
                let latency_penalty = (node.latency_us / 100) as u32; // 1 weight per 100us
                let weight = base_weight.saturating_sub(latency_penalty).max(1);

                Endpoint::new(node.node_id)
                    .with_weight(weight)
                    .with_priority(node.hops as u32)
            })
            .collect()
    }

    /// Update load balancer from proximity data
    pub fn update_load_balancer(&self, lb: &LoadBalancer, filter: Option<&CapabilityFilter>) {
        for entry in self.nodes.iter() {
            let node = entry.value();

            if !filter
                .map(|f| node.primary_caps.matches_basic(f))
                .unwrap_or(true)
            {
                continue;
            }

            // Update health
            lb.update_health(&node.node_id, node.health);

            // Update metrics
            let metrics = LoadMetrics {
                cpu_usage: (node.load_level as f64) / 255.0,
                avg_response_time_ms: (node.latency_us as f64) / 1000.0,
                ..Default::default()
            };
            lb.update_metrics(&node.node_id, metrics);
        }
    }

    /// Sync discovered nodes to capability index
    ///
    /// Note: This requires the caller to handle index updates appropriately.
    /// The CapabilityIndex uses announcements, so this returns nodes with capabilities
    /// that need to be announced.
    pub fn nodes_with_capabilities(&self) -> Vec<(NodeId, CapabilitySet)> {
        self.nodes
            .iter()
            .filter_map(|entry| {
                let node = entry.value();
                node.get_capabilities().map(|caps| (node.node_id, caps))
            })
            .collect()
    }

    /// Clean up stale entries
    pub fn cleanup(&self) -> CleanupStats {
        let mut removed_nodes = 0;
        let mut removed_pingwaves = 0;

        // Remove stale nodes
        self.nodes.retain(|_, node| {
            if node.is_stale(self.config.node_timeout) {
                removed_nodes += 1;
                self.stats.nodes_expired.fetch_add(1, Ordering::Relaxed);
                false
            } else {
                true
            }
        });

        // Remove old dedup entries
        self.seen_pingwaves.retain(|_, instant| {
            if instant.elapsed() > self.config.dedup_timeout {
                removed_pingwaves += 1;
                false
            } else {
                true
            }
        });

        CleanupStats {
            removed_nodes,
            removed_pingwaves,
        }
    }

    /// Get statistics snapshot
    pub fn stats(&self) -> ProximityStatsSnapshot {
        ProximityStatsSnapshot {
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
            dedup_cache_size: self.seen_pingwaves.len(),
            pingwaves_sent: self.stats.pingwaves_sent.load(Ordering::Relaxed),
            pingwaves_received: self.stats.pingwaves_received.load(Ordering::Relaxed),
            pingwaves_forwarded: self.stats.pingwaves_forwarded.load(Ordering::Relaxed),
            pingwaves_dropped: self.stats.pingwaves_dropped.load(Ordering::Relaxed),
            nodes_discovered: self.stats.nodes_discovered.load(Ordering::Relaxed),
            nodes_expired: self.stats.nodes_expired.load(Ordering::Relaxed),
            capability_updates: self.stats.capability_updates.load(Ordering::Relaxed),
        }
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// Cleanup statistics
#[derive(Debug, Clone, Default)]
pub struct CleanupStats {
    /// Number of expired nodes removed from the graph
    pub removed_nodes: usize,
    /// Number of stale ping wave deduplication entries removed
    pub removed_pingwaves: usize,
}

/// Statistics snapshot
#[derive(Debug, Clone, Default)]
pub struct ProximityStatsSnapshot {
    /// Number of nodes currently tracked in the proximity graph
    pub node_count: usize,
    /// Number of edges currently in the proximity graph
    pub edge_count: usize,
    /// Number of entries in the ping wave deduplication cache
    pub dedup_cache_size: usize,
    /// Total ping waves sent since startup
    pub pingwaves_sent: u64,
    /// Total ping waves received since startup
    pub pingwaves_received: u64,
    /// Total ping waves forwarded since startup
    pub pingwaves_forwarded: u64,
    /// Total ping waves dropped since startup
    pub pingwaves_dropped: u64,
    /// Total nodes discovered since startup
    pub nodes_discovered: u64,
    /// Total nodes expired since startup
    pub nodes_expired: u64,
    /// Total capability updates processed since startup
    pub capability_updates: u64,
}

/// Get current time in microseconds
fn current_time_us() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Hash capabilities for quick comparison
fn hash_capabilities(caps: &CapabilitySet) -> u64 {
    // Simple FNV-1a hash of key capability fields
    let mut hash: u64 = 0xcbf29ce484222325;

    // Hash hardware memory
    hash ^= caps.hardware.memory_mb as u64;
    hash = hash.wrapping_mul(0x100000001b3);

    // Hash GPU presence
    hash ^= if caps.hardware.gpu.is_some() { 1 } else { 0 };
    hash = hash.wrapping_mul(0x100000001b3);

    // Hash accelerator count
    hash ^= caps.hardware.accelerators.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);

    // Hash tool count
    hash ^= caps.tools.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);

    // Hash model count
    hash ^= caps.models.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);

    // Hash tag count
    hash ^= caps.tags.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);

    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_primary_capabilities_roundtrip() {
        let caps = PrimaryCapabilities {
            gpu: true,
            model_slots: 4,
            memory_tier: 5,
            tools_bitmap: 0b10101010,
            flags: 0x12345678,
        };

        let bytes = caps.to_bytes();
        let parsed = PrimaryCapabilities::from_bytes(&bytes);

        assert_eq!(caps, parsed);
    }

    #[test]
    fn test_enhanced_pingwave_roundtrip() {
        let pw = EnhancedPingwave::new(make_node_id(1), 42, 3)
            .with_capabilities(0xDEADBEEF, 5, PrimaryCapabilities::default())
            .with_load(128, HealthStatus::Healthy);

        let bytes = pw.to_bytes();
        let parsed = EnhancedPingwave::from_bytes(&bytes).unwrap();

        assert_eq!(pw.origin_id, parsed.origin_id);
        assert_eq!(pw.seq, parsed.seq);
        assert_eq!(pw.ttl, parsed.ttl);
        assert_eq!(pw.capability_hash, parsed.capability_hash);
        assert_eq!(pw.load_level, parsed.load_level);
    }

    /// Regression: BUG_REPORT.md #38 — `from_bytes` previously
    /// coerced any unknown discriminant on the `health` byte (63)
    /// into `HealthStatus::Unknown`. A single bit-flip in transit
    /// could downgrade a peer to `Unknown`, which
    /// `can_receive_traffic()` treats as unroutable — silent peer
    /// eviction. The fix returns `None` on unknown discriminants
    /// so the caller drops the malformed pingwave entirely.
    #[test]
    fn from_bytes_rejects_unknown_health_discriminant() {
        let pw = EnhancedPingwave::new(make_node_id(1), 1, 3).with_load(64, HealthStatus::Healthy);
        let mut bytes = pw.to_bytes().to_vec();

        // Sanity: round-trip works at the legitimate value.
        assert!(EnhancedPingwave::from_bytes(&bytes).is_some());

        // Mutate the health byte to an out-of-range discriminant.
        // 4..=255 are all unknown; sample a few across the range.
        for bad in [4u8, 99, 200, 255] {
            bytes[63] = bad;
            assert!(
                EnhancedPingwave::from_bytes(&bytes).is_none(),
                "health discriminant {} should be rejected, not coerced (#38)",
                bad
            );
        }

        // The four legitimate values still round-trip.
        for ok in 0u8..=3 {
            bytes[63] = ok;
            assert!(
                EnhancedPingwave::from_bytes(&bytes).is_some(),
                "health discriminant {} must still parse",
                ok
            );
        }
    }

    #[test]
    fn test_pingwave_forward() {
        let mut pw = EnhancedPingwave::new(make_node_id(1), 1, 3);
        assert_eq!(pw.ttl, 3);
        assert_eq!(pw.hop_count, 0);

        assert!(pw.forward());
        assert_eq!(pw.ttl, 2);
        assert_eq!(pw.hop_count, 1);

        assert!(pw.forward());
        assert!(pw.forward());
        assert_eq!(pw.ttl, 0);
        assert!(!pw.forward()); // Can't forward when expired
    }

    #[test]
    fn test_proximity_graph_pingwave_processing() {
        let my_id = make_node_id(1);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());

        let pw = EnhancedPingwave::new(make_node_id(2), 1, 3);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Process pingwave
        let forwarded = graph.on_pingwave(pw, from);
        assert!(forwarded.is_some());

        // Node should be added
        let node = graph.get_node(&make_node_id(2)).unwrap();
        assert_eq!(node.hops, 1);

        // Duplicate should be dropped
        let pw2 = EnhancedPingwave::new(make_node_id(2), 1, 3);
        assert!(graph.on_pingwave(pw2, from).is_none());

        // New sequence should work
        let pw3 = EnhancedPingwave::new(make_node_id(2), 2, 3);
        assert!(graph.on_pingwave(pw3, from).is_some());
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #108: pre-fix
    /// `from_pingwave` and `update_from_pingwave` used raw
    /// `pw.hop_count + 1`, which panics in debug at `u8::MAX`
    /// and silently wraps to 0 in release. A peer advertising
    /// `hop_count == 255` could either crash the receive loop
    /// or falsely promote itself to "0 hops" (directly
    /// connected) — a proximity-routing poisoning vector.
    /// Post-fix uses `saturating_add(1)` so 255 stays at 255.
    #[test]
    fn proximity_node_from_pingwave_saturates_at_max_hop_count() {
        let mut pw = EnhancedPingwave::new(make_node_id(2), 1, 3);
        pw.hop_count = u8::MAX;
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Pre-fix this would panic in debug builds and wrap to
        // 0 in release builds. Post-fix it saturates at 255.
        let node = ProximityNode::from_pingwave(&pw, from);
        assert_eq!(
            node.hops,
            u8::MAX,
            "saturating_add must clamp at u8::MAX, NOT wrap to 0"
        );
        assert_ne!(
            node.hops, 0,
            "a 255-hop peer must NOT be reported as 0 hops"
        );
    }

    #[test]
    fn proximity_node_update_from_pingwave_saturates_at_max_hop_count() {
        let mut pw_initial = EnhancedPingwave::new(make_node_id(2), 1, 3);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let mut node = ProximityNode::from_pingwave(&pw_initial, from);
        let initial_hops = node.hops;

        // Update with hop_count = 255. The saturating bump
        // (`pw.hop_count.saturating_add(1) = 255`) must not panic
        // in debug or wrap to 0 in release.
        pw_initial.hop_count = u8::MAX;
        pw_initial.seq = 2;
        node.update_from_pingwave(&pw_initial, from);
        // The path-quality arm rejects `new_hops=255 > self.hops`,
        // so the better cached hop count survives. Freshness still
        // advances `last_seq`. Sanity: no panic, no wrap.
        assert_eq!(node.hops, initial_hops);
        assert_eq!(node.last_seq, 2);
    }

    /// Regression for the "worse path overwrites better" hazard.
    /// Pre-fix `update_from_pingwave` used an OR predicate
    /// (`seq > last_seq || new_hops < hops`), so a flooded
    /// high-seq pingwave reaching us through a long route demoted
    /// a previously-cached direct route purely on freshness.
    /// Post-fix `last_seq` always advances on a newer pingwave,
    /// but `addr` / `hops` / `latency_us` only update when the
    /// new path is no worse.
    #[test]
    fn update_from_pingwave_keeps_better_path_when_newer_seq_arrives_via_longer_route() {
        // Direct path: 1 hop after the +1 bump.
        let mut pw_direct = EnhancedPingwave::new(make_node_id(2), 5, 0);
        let direct_addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let mut node = ProximityNode::from_pingwave(&pw_direct, direct_addr);
        let direct_hops = node.hops;
        let direct_last_seq = node.last_seq;
        assert_eq!(direct_hops, 1, "test setup: direct route is 1 hop");

        // A later, higher-seq pingwave for the same node arrives via
        // a 7-hop indirect path from a different source address.
        let indirect_addr: SocketAddr = "10.0.0.5:9000".parse().unwrap();
        pw_direct.seq = 9;
        pw_direct.hop_count = 7;
        node.update_from_pingwave(&pw_direct, indirect_addr);

        // Path-quality arm: the longer route MUST NOT overwrite the
        // direct route's address or hop count.
        assert_eq!(
            node.hops, direct_hops,
            "longer-route pingwave must not demote a better cached path",
        );
        assert_eq!(
            node.addr, direct_addr,
            "longer-route pingwave must not redirect to the indirect source",
        );
        // Freshness arm: `last_seq` still advances on the newer
        // pingwave so subsequent staleness / restart checks see the
        // current sequence number.
        assert!(
            node.last_seq > direct_last_seq,
            "freshness must still advance"
        );
        assert_eq!(node.last_seq, 9);
    }

    #[test]
    fn test_proximity_graph_find_matching() {
        let my_id = make_node_id(1);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());

        // Add some nodes via pingwaves
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        let mut pw1 = EnhancedPingwave::new(make_node_id(2), 1, 3);
        pw1.primary_caps = PrimaryCapabilities {
            gpu: true,
            model_slots: 4,
            ..Default::default()
        };
        graph.on_pingwave(pw1, from);

        let mut pw2 = EnhancedPingwave::new(make_node_id(3), 1, 3);
        pw2.primary_caps = PrimaryCapabilities {
            gpu: false,
            model_slots: 2,
            ..Default::default()
        };
        graph.on_pingwave(pw2, from);

        // Find GPU nodes
        let filter = CapabilityFilter {
            require_gpu: true,
            ..Default::default()
        };
        let gpu_nodes = graph.find_matching(&filter);
        assert_eq!(gpu_nodes.len(), 1);
        assert_eq!(gpu_nodes[0].node_id, make_node_id(2));
    }

    #[test]
    fn test_proximity_graph_to_endpoints() {
        let my_id = make_node_id(1);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());

        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Add nodes
        graph.on_pingwave(EnhancedPingwave::new(make_node_id(2), 1, 3), from);
        graph.on_pingwave(EnhancedPingwave::new(make_node_id(3), 1, 3), from);

        // Get endpoints
        let endpoints = graph.to_endpoints(None);
        assert_eq!(endpoints.len(), 2);
    }

    #[test]
    fn test_routing_score() {
        let pw = EnhancedPingwave::new(make_node_id(1), 1, 3);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        let mut node = ProximityNode::from_pingwave(&pw, from);
        node.latency_us = 1000; // 1ms
        node.load_level = 128; // 50% load
        node.health = HealthStatus::Healthy;

        let score = node.routing_score(true);
        assert!(score > 0.0);

        // Degraded health should increase score
        node.health = HealthStatus::Degraded;
        let degraded_score = node.routing_score(true);
        assert!(degraded_score > score);
    }

    #[test]
    fn test_cleanup() {
        let my_id = make_node_id(1);
        let config = ProximityConfig {
            node_timeout: Duration::from_millis(10),
            dedup_timeout: Duration::from_millis(10),
            ..Default::default()
        };
        let graph = ProximityGraph::new(my_id, config);

        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        graph.on_pingwave(EnhancedPingwave::new(make_node_id(2), 1, 3), from);

        assert_eq!(graph.node_count(), 1);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(20));

        let stats = graph.cleanup();
        assert_eq!(stats.removed_nodes, 1);
        assert_eq!(graph.node_count(), 0);
    }

    #[test]
    fn test_regression_pingwave_primary_caps_survive_roundtrip() {
        // Regression: EnhancedPingwave::to_bytes/from_bytes did not
        // serialize primary_caps (gpu, model_slots, etc.), so after
        // crossing the wire all capabilities were reset to defaults.
        // This made capability-based routing silently fail for remote
        // nodes — e.g., `require_gpu: true` never matched anyone.
        let caps = PrimaryCapabilities {
            gpu: true,
            model_slots: 4,
            memory_tier: 5,
            tools_bitmap: 0b10101010,
            flags: 0x12345678,
        };
        let pw = EnhancedPingwave::new(make_node_id(1), 42, 3).with_capabilities(0xDEAD, 7, caps);

        let bytes = pw.to_bytes();
        let parsed = EnhancedPingwave::from_bytes(&bytes).unwrap();

        assert!(
            parsed.primary_caps.gpu,
            "gpu capability must survive serialization"
        );
        assert_eq!(parsed.primary_caps.model_slots, 4);
        assert_eq!(parsed.primary_caps.memory_tier, 5);
        assert_eq!(parsed.primary_caps.tools_bitmap, 0b10101010);
        assert_eq!(parsed.primary_caps.flags, 0x12345678);
    }

    #[test]
    fn test_regression_find_best_no_panic_on_nan() {
        // Regression: find_best() used partial_cmp().unwrap() which
        // panics on NaN routing scores. Now uses total_cmp().
        let my_id = make_node_id(1);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());

        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Add nodes with very high latency (edge case for routing_score)
        let pw = EnhancedPingwave::new(make_node_id(2), 1, 3).with_load(0, HealthStatus::Healthy);
        graph.on_pingwave(pw, from);

        let filter = CapabilityFilter::default();
        // This should not panic
        let _best = graph.find_best(&filter);
        let _k_best = graph.find_k_best(&filter, 5);
    }

    #[test]
    fn test_regression_hop_count_saturates() {
        // Regression: forward() used `hop_count += 1` which wraps at
        // u8::MAX (255 → 0), making a distant node appear 1 hop away.
        // Now uses saturating_add.
        let mut pw = EnhancedPingwave::new(make_node_id(1), 1, 255);
        pw.hop_count = 254;

        assert!(pw.forward());
        assert_eq!(pw.hop_count, 255);

        // At 255, saturating_add should keep it at 255
        assert!(pw.forward());
        assert_eq!(
            pw.hop_count, 255,
            "hop_count should saturate at 255, not wrap to 0"
        );
    }

    #[test]
    fn test_edge_insert_on_pingwave_receipt() {
        // On pingwave receipt for origin Y via peer Z, two edges
        // materialize: (self → Z) and (Z → Y). `path_to(Y)` then
        // returns the 3-step path [self, Z, Y].
        let my_id = make_node_id(1);
        let z = make_node_id(2);
        let y = make_node_id(3);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Pingwave carries origin Y, arrived via Z (hop_count=1).
        let pw = EnhancedPingwave::new(y, 1, 3).with_load(0, HealthStatus::Healthy);
        let mut pw = pw;
        pw.hop_count = 1;
        graph.on_pingwave_from(pw, z, from);

        let path = graph.path_to(&y).expect("path_to(y) should return Some");
        assert_eq!(path, vec![my_id, z, y]);
    }

    #[test]
    fn test_edge_sweep_removes_stale() {
        use std::time::Duration;
        let my_id = make_node_id(1);
        let z = make_node_id(2);
        let y = make_node_id(3);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        let mut pw = EnhancedPingwave::new(y, 1, 3).with_load(0, HealthStatus::Healthy);
        pw.hop_count = 1;
        graph.on_pingwave_from(pw, z, from);
        assert!(graph.path_to(&y).is_some());

        // Backdate both edges so the sweep finds them stale.
        for mut entry in graph.edges.iter_mut() {
            entry.last_updated = Instant::now() - Duration::from_secs(3600);
        }
        let removed = graph.sweep_stale_edges(Duration::from_secs(60));
        assert_eq!(removed, 2, "both synthetic edges should be swept");
        assert!(graph.path_to(&y).is_none());
    }

    #[test]
    fn test_origin_self_check_drops_pingwave() {
        // Pingwave claiming `origin == self_id` must be dropped.
        // The graph's `on_pingwave` already has this check; the same
        // rule is enforced in `mesh.rs` dispatch earlier.
        let my_id = make_node_id(1);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        let pw = EnhancedPingwave::new(my_id, 1, 3);
        let forwarded = graph.on_pingwave(pw, from);
        assert!(forwarded.is_none(), "self-origin pingwave must be dropped");
        assert!(graph.get_node(&my_id).is_none());
    }

    #[test]
    fn test_latency_ewma_smooths_successive_samples() {
        let my_id = make_node_id(1);
        let z = make_node_id(2);
        let y = make_node_id(3);
        let graph = ProximityGraph::new(my_id, ProximityConfig::default());
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Two pingwaves with known timestamps → two latency samples.
        let now = current_time_us();
        let mut pw1 = EnhancedPingwave::new(y, 1, 3).with_load(0, HealthStatus::Healthy);
        pw1.hop_count = 1;
        pw1.origin_timestamp_us = now.saturating_sub(10_000); // 10 ms ago
        graph.on_pingwave_from(pw1, z, from);

        // Edge should have latency ≈ 10_000 us after first insert.
        let edge1 = graph.edges.get(&(z, y)).expect("z→y edge");
        assert!(edge1.latency_us > 0);
        let first = edge1.latency_us;
        drop(edge1);

        // Second sample with a different latency — EWMA drags it.
        let mut pw2 = EnhancedPingwave::new(y, 2, 3).with_load(0, HealthStatus::Healthy);
        pw2.hop_count = 1;
        pw2.origin_timestamp_us = current_time_us().saturating_sub(50_000); // 50 ms ago
        graph.on_pingwave_from(pw2, z, from);

        let edge2 = graph.edges.get(&(z, y)).unwrap();
        // EWMA α=1/8 → edge.latency_us moved toward 50_000 from
        // `first`, but not all the way.
        assert_ne!(edge2.latency_us, first, "EWMA should shift latency");
        assert!(
            edge2.latency_us < 50_000,
            "EWMA should not snap to the new sample"
        );
    }
}
