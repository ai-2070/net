//! Swarm discovery and graph maintenance for Net.
//!
//! This module provides:
//! - `Pingwave` - Periodic discovery packets for neighbor detection
//! - `CapabilityAd` - Node capability advertisements
//! - `LocalGraph` - Local view of the network topology (k-hop radius)
//! - `NodeInfo` / `EdgeInfo` - Graph node and edge metadata

use bytes::{Buf, BufMut, Bytes, BytesMut};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Pingwave packet size in bytes
pub const PINGWAVE_SIZE: usize = 24;

/// Maximum capabilities string length
#[allow(dead_code)]
pub const MAX_CAPABILITIES_LEN: usize = 256;

/// Pingwave packet for neighbor discovery.
///
/// Layout (24 bytes):
/// ```text
/// ┌────────────────────────────────────────────────────────────┐
/// │ origin_id (8) │ seq (8) │ ttl (1) │ hops (1) │ reserved (6)│
/// └────────────────────────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Pingwave {
    /// Originating node ID
    pub origin_id: u64,
    /// Sequence number (monotonic per origin)
    pub seq: u64,
    /// Time-to-live (usually 2-3 hops)
    pub ttl: u8,
    /// Hop count so far
    pub hop_count: u8,
    /// Reserved for future use
    pub _reserved: [u8; 6],
}

impl Pingwave {
    /// Create a new pingwave
    pub fn new(origin_id: u64, seq: u64, ttl: u8) -> Self {
        Self {
            origin_id,
            seq,
            ttl,
            hop_count: 0,
            _reserved: [0; 6],
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; PINGWAVE_SIZE] {
        let mut buf = [0u8; PINGWAVE_SIZE];
        buf[0..8].copy_from_slice(&self.origin_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.seq.to_le_bytes());
        buf[16] = self.ttl;
        buf[17] = self.hop_count;
        // reserved bytes already zero
        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < PINGWAVE_SIZE {
            return None;
        }
        Some(Self {
            origin_id: u64::from_le_bytes(buf[0..8].try_into().ok()?),
            seq: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            ttl: buf[16],
            hop_count: buf[17],
            _reserved: [0; 6],
        })
    }

    /// Write to buffer
    pub fn write_to(&self, buf: &mut BytesMut) {
        buf.put_u64_le(self.origin_id);
        buf.put_u64_le(self.seq);
        buf.put_u8(self.ttl);
        buf.put_u8(self.hop_count);
        buf.put_slice(&[0u8; 6]); // reserved
    }

    /// Read from buffer
    pub fn read_from(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < PINGWAVE_SIZE {
            return None;
        }
        Some(Self {
            origin_id: buf.get_u64_le(),
            seq: buf.get_u64_le(),
            ttl: buf.get_u8(),
            hop_count: buf.get_u8(),
            _reserved: {
                buf.advance(6);
                [0; 6]
            },
        })
    }

    /// Check if pingwave has expired (TTL = 0)
    #[inline]
    pub fn is_expired(&self) -> bool {
        self.ttl == 0
    }

    /// Forward the pingwave (decrement TTL, increment hop count)
    #[inline]
    pub fn forward(&mut self) -> bool {
        if self.ttl == 0 {
            return false;
        }
        self.ttl -= 1;
        self.hop_count = self.hop_count.saturating_add(1);
        true
    }
}

/// Node capabilities for capability-based routing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Has GPU acceleration
    pub gpu: bool,
    /// Available tools/functions
    pub tools: Vec<String>,
    /// Available memory in MB
    pub memory_mb: u32,
    /// Number of model slots available
    pub model_slots: u8,
    /// Custom tags
    pub tags: Vec<String>,
}

impl Capabilities {
    /// Create empty capabilities
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with GPU flag
    pub fn with_gpu(mut self, gpu: bool) -> Self {
        self.gpu = gpu;
        self
    }

    /// Add a tool
    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tools.push(tool.into());
        self
    }

    /// Set memory
    pub fn with_memory(mut self, memory_mb: u32) -> Self {
        self.memory_mb = memory_mb;
        self
    }

    /// Set model slots
    pub fn with_model_slots(mut self, slots: u8) -> Self {
        self.model_slots = slots;
        self
    }

    /// Add a tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Check if node has a specific tool
    pub fn has_tool(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Check if node has a specific tag
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// Serialize to bytes (simple format)
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);

        // Flags byte: bit 0 = gpu
        let flags = if self.gpu { 0x01 } else { 0x00 };
        buf.push(flags);

        // Memory (4 bytes)
        buf.extend_from_slice(&self.memory_mb.to_le_bytes());

        // Model slots (1 byte)
        buf.push(self.model_slots);

        // Tool count + tools (capped at 255 items, 255 bytes per string)
        let tool_count = self.tools.len().min(255);
        buf.push(tool_count as u8);
        for tool in &self.tools[..tool_count] {
            let bytes = tool.as_bytes();
            let len = bytes.len().min(255);
            buf.push(len as u8);
            buf.extend_from_slice(&bytes[..len]);
        }

        // Tag count + tags (capped at 255 items, 255 bytes per string)
        let tag_count = self.tags.len().min(255);
        buf.push(tag_count as u8);
        for tag in &self.tags[..tag_count] {
            let bytes = tag.as_bytes();
            let len = bytes.len().min(255);
            buf.push(len as u8);
            buf.extend_from_slice(&bytes[..len]);
        }

        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(mut buf: &[u8]) -> Option<Self> {
        if buf.len() < 7 {
            return None;
        }

        let flags = buf[0];
        let gpu = (flags & 0x01) != 0;

        let memory_mb = u32::from_le_bytes(buf[1..5].try_into().ok()?);
        let model_slots = buf[5];
        let tool_count = buf[6] as usize;

        buf = &buf[7..];

        let mut tools = Vec::with_capacity(tool_count);
        for _ in 0..tool_count {
            if buf.is_empty() {
                return None;
            }
            let len = buf[0] as usize;
            buf = &buf[1..];
            if buf.len() < len {
                return None;
            }
            let tool = std::str::from_utf8(&buf[..len]).ok()?.to_string();
            tools.push(tool);
            buf = &buf[len..];
        }

        if buf.is_empty() {
            return None;
        }
        let tag_count = buf[0] as usize;
        buf = &buf[1..];

        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            if buf.is_empty() {
                return None;
            }
            let len = buf[0] as usize;
            buf = &buf[1..];
            if buf.len() < len {
                return None;
            }
            let tag = std::str::from_utf8(&buf[..len]).ok()?.to_string();
            tags.push(tag);
            buf = &buf[len..];
        }

        Some(Self {
            gpu,
            tools,
            memory_mb,
            model_slots,
            tags,
        })
    }
}

/// Capability advertisement packet.
#[derive(Debug, Clone)]
pub struct CapabilityAd {
    /// Node ID
    pub node_id: u64,
    /// Version (for updates)
    pub version: u32,
    /// Capabilities
    pub capabilities: Capabilities,
}

impl CapabilityAd {
    /// Create a new capability advertisement
    pub fn new(node_id: u64, version: u32, capabilities: Capabilities) -> Self {
        Self {
            node_id,
            version,
            capabilities,
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let cap_bytes = self.capabilities.to_bytes();
        let mut buf = Vec::with_capacity(12 + cap_bytes.len());

        buf.extend_from_slice(&self.node_id.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&cap_bytes);

        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 12 {
            return None;
        }

        let node_id = u64::from_le_bytes(buf[0..8].try_into().ok()?);
        let version = u32::from_le_bytes(buf[8..12].try_into().ok()?);
        let capabilities = Capabilities::from_bytes(&buf[12..])?;

        Some(Self {
            node_id,
            version,
            capabilities,
        })
    }
}

/// Information about a node in the local graph.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Node ID
    pub node_id: u64,
    /// Network address
    pub addr: SocketAddr,
    /// Hop distance from local node
    pub hops: u8,
    /// Last seen timestamp
    pub last_seen: Instant,
    /// Latest pingwave sequence from this node
    pub last_seq: u64,
    /// Capabilities (if known)
    pub capabilities: Option<Capabilities>,
    /// Capability version
    pub cap_version: u32,
}

impl NodeInfo {
    /// Create new node info
    pub fn new(node_id: u64, addr: SocketAddr, hops: u8) -> Self {
        Self {
            node_id,
            addr,
            hops,
            last_seen: Instant::now(),
            last_seq: 0,
            capabilities: None,
            cap_version: 0,
        }
    }

    /// Update last seen
    pub fn touch(&mut self) {
        self.last_seen = Instant::now();
    }

    /// Check if node is stale
    pub fn is_stale(&self, timeout: Duration) -> bool {
        self.last_seen.elapsed() > timeout
    }

    /// Update capabilities if newer version
    pub fn update_capabilities(&mut self, version: u32, caps: Capabilities) -> bool {
        if version > self.cap_version {
            self.capabilities = Some(caps);
            self.cap_version = version;
            true
        } else {
            false
        }
    }
}

/// Information about an edge (connection) between nodes.
#[derive(Debug, Clone)]
pub struct EdgeInfo {
    /// Source node ID
    pub from: u64,
    /// Destination node ID
    pub to: u64,
    /// Estimated latency in microseconds
    pub latency_us: u32,
    /// Last update timestamp
    pub last_updated: Instant,
}

impl EdgeInfo {
    /// Create new edge info
    pub fn new(from: u64, to: u64) -> Self {
        Self {
            from,
            to,
            latency_us: 0,
            last_updated: Instant::now(),
        }
    }

    /// Create with latency
    pub fn with_latency(from: u64, to: u64, latency_us: u32) -> Self {
        Self {
            from,
            to,
            latency_us,
            last_updated: Instant::now(),
        }
    }
}

/// Local view of the network graph.
///
/// Maintains a k-hop neighborhood view of the network topology.
/// Soft caps on `LocalGraph`'s DashMaps.
///
/// `on_pingwave` inserts into both `nodes` and `seen_pingwaves`,
/// so without these caps a peer that completed the cheap mesh-
/// handshake gate could flood pingwaves with random
/// `origin_id` / `seq` combinations and grow both maps at
/// line-rate; cleanup runs on a 30s/10s timer, so per-window
/// growth would otherwise be bounded only by link bandwidth.
/// The caps turn that into a bounded memory footprint:
/// - Below the cap: insertion proceeds normally.
/// - At or above the cap: novel keys are NOT inserted (existing
///   keys still update); periodic eviction reclaims slots for
///   legitimate nodes/pingwaves as they idle out.
///
/// Sized to keep the worst-case memory bounded while leaving
/// headroom for real workloads — peer mesh sizes ≤ a few
/// thousand nodes rarely populate more than a few thousand
/// distinct origin_ids in the graph.
pub const MAX_GRAPH_NODES: usize = 65_536;

/// Soft cap on the `seen_pingwaves` dedup map.
/// 4× `MAX_GRAPH_NODES` to absorb a typical multi-second pingwave
/// burst per node before reaching the cap. Exceeding the cap
/// drops novel `(origin_id, seq)` pairs at admission; legitimate
/// peers still update existing entries.
pub const MAX_SEEN_PINGWAVES: usize = 262_144;

/// Local view of the swarm: known nodes, edges, and a dedup
/// cache for incoming pingwaves. Holds the proximity-routing
/// state — see `BehaviorContext::propagate` for the consumer
/// side and `mesh.rs` pingwave dispatch for the producer side.
pub struct LocalGraph {
    /// Local node ID
    my_id: u64,
    /// Maximum hops to track
    radius: u8,
    /// Known nodes
    nodes: DashMap<u64, NodeInfo>,
    /// Edges (from, to) -> EdgeInfo
    edges: DashMap<(u64, u64), EdgeInfo>,
    /// Seen pingwaves (origin_id, seq) for deduplication
    seen_pingwaves: DashMap<(u64, u64), Instant>,
    /// Next pingwave sequence number
    next_seq: AtomicU64,
    /// Node timeout
    node_timeout: Duration,
    /// Pingwave cache timeout
    pingwave_cache_timeout: Duration,
}

impl LocalGraph {
    /// Create a new local graph
    pub fn new(my_id: u64, radius: u8) -> Self {
        Self {
            my_id,
            radius,
            nodes: DashMap::new(),
            edges: DashMap::new(),
            seen_pingwaves: DashMap::new(),
            next_seq: AtomicU64::new(1),
            node_timeout: Duration::from_secs(30),
            pingwave_cache_timeout: Duration::from_secs(10),
        }
    }

    /// Set node timeout
    pub fn with_node_timeout(mut self, timeout: Duration) -> Self {
        self.node_timeout = timeout;
        self
    }

    /// Get local node ID
    pub fn my_id(&self) -> u64 {
        self.my_id
    }

    /// Get radius
    pub fn radius(&self) -> u8 {
        self.radius
    }

    /// Create a new pingwave to broadcast
    pub fn create_pingwave(&self) -> Pingwave {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        Pingwave::new(self.my_id, seq, self.radius)
    }

    /// Process an incoming pingwave.
    ///
    /// Returns Some(pingwave) if it should be forwarded, None otherwise.
    ///
    /// `seen_pingwaves` and `nodes` are soft-capped via
    /// [`MAX_SEEN_PINGWAVES`] / [`MAX_GRAPH_NODES`]: existing
    /// entries continue to update, but novel keys are dropped
    /// once the cap is reached. The next periodic
    /// `evict_stale_*` sweep reclaims slots for legitimate
    /// nodes/pingwaves so admission resumes once memory pressure
    /// eases. Without the caps, a peer flooding pingwaves with
    /// random `(origin_id, seq)` could grow both maps at
    /// line-rate between the periodic eviction sweeps.
    pub fn on_pingwave(&self, mut pw: Pingwave, from: SocketAddr) -> Option<Pingwave> {
        // Ignore our own pingwaves
        if pw.origin_id == self.my_id {
            return None;
        }

        // Check if already seen
        let key = (pw.origin_id, pw.seq);
        if self.seen_pingwaves.contains_key(&key) {
            return None;
        }

        // Gate `seen_pingwaves` insertion on the soft cap. If
        // we're at the cap, drop the pingwave entirely (don't
        // track it, don't forward) — better to lose a legitimate
        // one than open the flood gate.
        if self.seen_pingwaves.len() >= MAX_SEEN_PINGWAVES {
            return None;
        }

        // Mark as seen
        self.seen_pingwaves.insert(key, Instant::now());

        // `pw.hop_count + 1` would panic in debug at u8::MAX and
        // silently wrap to 0 in release. A peer can advertise
        // `hop_count == 255` to either crash the receive loop
        // (debug) or falsely promote itself to "0 hops away"
        // (release) — proximity-routing poisoning vector.
        // `saturating_add(1)` caps at 255.
        let hops = pw.hop_count.saturating_add(1);
        // Gate `nodes` insertion on the soft cap. Existing nodes
        // keep updating regardless (so legitimate peers don't get
        // kicked out mid-flight); only novel origin_ids are
        // blocked. Combined with periodic eviction this bounds
        // memory while preserving liveness for already-known
        // peers.
        if !self.nodes.contains_key(&pw.origin_id) && self.nodes.len() >= MAX_GRAPH_NODES {
            return None;
        }
        self.nodes
            .entry(pw.origin_id)
            .and_modify(|n| {
                // Pingwaves are unauthenticated UDP, so we cannot
                // trust an apparent seq regression (e.g. `pw.seq`
                // far below `n.last_seq`) to reflect a legitimate
                // peer restart. Refreshing only `n.touch()` on
                // such a "likely restart" signal — without
                // overwriting `addr`/`hops` and without lowering
                // `last_seq` — is the safe shape:
                //
                //   * Liveness half: a real peer that has restarted
                //     and is now sending fresh seq=1, 2, ... still
                //     keeps the entry off the stale-eviction path.
                //   * Authenticity half: any subsequent seq from
                //     the legitimate restarted peer must climb
                //     back above the pre-restart high-water mark
                //     via strict-progress to earn an address
                //     update.
                //
                // Otherwise an attacker with line-of-sight to the
                // wire could spoof `(origin_id=Y, seq=1, hops=0)`
                // from their own UDP source and repoint Y's
                // recorded routing target. Even just *lowering*
                // `last_seq` on a restart-only branch leaves a
                // slow-rolling address-overwrite vector: spoof
                // seq=1 to drop the high-water, then follow up
                // with seq=2 which looks like strict-progress and
                // DOES overwrite `n.addr`.
                let likely_restart = n.last_seq > 1 && pw.seq < n.last_seq.saturating_div(2);
                // Require BOTH a non-regressing sequence AND a
                // path no worse than what we have. Pre-fix this
                // was an OR: `pw.seq > n.last_seq || hops <
                // n.hops`. The OR's `hops < n.hops` arm let an
                // attacker who had observed legitimate pingwaves
                // spoof `(origin_id=Y, seq=K, hops=0)` for any K
                // strictly less than the current `last_seq`
                // (e.g. via a not-yet-seen K from before the
                // dedup-filter window). The dedup filter
                // admitted the entry, `hops < n.hops` flipped
                // strict-progress true, and `n.addr` was
                // overwritten with the attacker's UDP source.
                //
                // The AND form requires the attacker to
                // simultaneously land a fresh seq AND a
                // non-worse hop count — the fresh-seq half
                // forces them to have already heard our peer's
                // current state, and the legitimate "shorter
                // path discovered" case still triggers because
                // a real peer producing forward progress also
                // emits fresh seqs.
                let strict_progress = pw.seq >= n.last_seq && hops <= n.hops;
                // Reject the degenerate "no progress at all"
                // sub-case: when both seq and hops match exactly,
                // it's a duplicate — touch only.
                let strict_progress = strict_progress && (pw.seq > n.last_seq || hops < n.hops);
                if strict_progress {
                    n.last_seq = pw.seq;
                    n.hops = hops;
                    n.addr = from;
                    n.touch();
                } else if likely_restart {
                    // Liveness-only refresh — DO NOT overwrite
                    // addr/hops AND do NOT lower last_seq on an
                    // unauthenticated seq-regression signal.
                    // touch() alone keeps the entry off the
                    // stale-eviction path until a strict-progress
                    // pingwave arrives.
                    n.touch();
                }
            })
            .or_insert_with(|| {
                let mut info = NodeInfo::new(pw.origin_id, from, hops);
                info.last_seq = pw.seq;
                info
            });

        // Check if we should forward
        if pw.is_expired() {
            return None;
        }

        // Forward
        pw.forward();
        Some(pw)
    }

    /// Process a capability advertisement
    pub fn on_capability(&self, ad: CapabilityAd, from: SocketAddr) {
        self.nodes
            .entry(ad.node_id)
            .and_modify(|n| {
                n.update_capabilities(ad.version, ad.capabilities.clone());
            })
            .or_insert_with(|| {
                let mut info = NodeInfo::new(ad.node_id, from, 0);
                info.update_capabilities(ad.version, ad.capabilities.clone());
                info
            });
    }

    /// Add or update an edge
    pub fn add_edge(&self, from: u64, to: u64, latency_us: u32) {
        let key = (from, to);
        self.edges
            .entry(key)
            .and_modify(|e| {
                e.latency_us = latency_us;
                e.last_updated = Instant::now();
            })
            .or_insert_with(|| EdgeInfo::with_latency(from, to, latency_us));
    }

    /// Get node info
    pub fn get_node(&self, node_id: u64) -> Option<NodeInfo> {
        self.nodes.get(&node_id).map(|r| r.clone())
    }

    /// Get all known nodes
    pub fn all_nodes(&self) -> Vec<NodeInfo> {
        self.nodes.iter().map(|r| r.value().clone()).collect()
    }

    /// Get nodes within a specific hop distance
    pub fn nodes_within_hops(&self, max_hops: u8) -> Vec<NodeInfo> {
        self.nodes
            .iter()
            .filter(|r| r.hops <= max_hops)
            .map(|r| r.value().clone())
            .collect()
    }

    /// Find nodes with a specific capability (tool)
    pub fn find_by_tool(&self, tool: &str) -> Vec<NodeInfo> {
        self.nodes
            .iter()
            .filter(|r| {
                r.capabilities
                    .as_ref()
                    .map(|c| c.has_tool(tool))
                    .unwrap_or(false)
            })
            .map(|r| r.value().clone())
            .collect()
    }

    /// Find nodes with a specific tag
    pub fn find_by_tag(&self, tag: &str) -> Vec<NodeInfo> {
        self.nodes
            .iter()
            .filter(|r| {
                r.capabilities
                    .as_ref()
                    .map(|c| c.has_tag(tag))
                    .unwrap_or(false)
            })
            .map(|r| r.value().clone())
            .collect()
    }

    /// Find nodes with GPU
    pub fn find_with_gpu(&self) -> Vec<NodeInfo> {
        self.nodes
            .iter()
            .filter(|r| r.capabilities.as_ref().map(|c| c.gpu).unwrap_or(false))
            .map(|r| r.value().clone())
            .collect()
    }

    /// Get shortest path to a node (BFS).
    ///
    /// Reconstructs the path from a parent map at the end rather than
    /// cloning the full path per neighbor — avoids quadratic behavior
    /// on long paths or wide frontiers.
    pub fn path_to(&self, dest: u64) -> Option<Vec<u64>> {
        if dest == self.my_id {
            return Some(vec![self.my_id]);
        }

        // Build adjacency list from edges
        let mut adjacency: HashMap<u64, Vec<u64>> = HashMap::new();
        for edge in self.edges.iter() {
            adjacency.entry(edge.from).or_default().push(edge.to);
        }

        // BFS from my_id to dest, tracking parents for path reconstruction
        let mut parent: HashMap<u64, u64> = HashMap::new();
        let mut visited: HashSet<u64> = HashSet::new();
        let mut queue: VecDeque<u64> = VecDeque::new();

        queue.push_back(self.my_id);
        visited.insert(self.my_id);

        while let Some(current) = queue.pop_front() {
            if current == dest {
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

    /// Clean up stale nodes and old pingwave cache entries
    pub fn cleanup(&self) -> (usize, usize) {
        let mut removed_nodes = 0;
        let mut removed_pingwaves = 0;

        // Remove stale nodes
        self.nodes.retain(|_, node| {
            if node.is_stale(self.node_timeout) {
                removed_nodes += 1;
                false
            } else {
                true
            }
        });

        // Remove old pingwave cache entries
        self.seen_pingwaves.retain(|_, instant| {
            if instant.elapsed() > self.pingwave_cache_timeout {
                removed_pingwaves += 1;
                false
            } else {
                true
            }
        });

        (removed_nodes, removed_pingwaves)
    }

    /// Get graph statistics
    pub fn stats(&self) -> GraphStats {
        GraphStats {
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
            pingwave_cache_size: self.seen_pingwaves.len(),
        }
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get edge count
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

impl std::fmt::Debug for LocalGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalGraph")
            .field("my_id", &format!("{:016x}", self.my_id))
            .field("radius", &self.radius)
            .field("nodes", &self.nodes.len())
            .field("edges", &self.edges.len())
            .finish()
    }
}

/// Graph statistics
#[derive(Debug, Clone, Default)]
pub struct GraphStats {
    /// Number of known nodes
    pub node_count: usize,
    /// Number of known edges
    pub edge_count: usize,
    /// Pingwave cache size
    pub pingwave_cache_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pingwave_roundtrip() {
        let pw = Pingwave::new(0x123456789ABCDEF0, 42, 3);
        let bytes = pw.to_bytes();
        let parsed = Pingwave::from_bytes(&bytes).unwrap();
        assert_eq!(pw, parsed);
    }

    #[test]
    fn test_pingwave_forward() {
        let mut pw = Pingwave::new(0x1234, 1, 3);
        assert_eq!(pw.ttl, 3);
        assert_eq!(pw.hop_count, 0);

        assert!(pw.forward());
        assert_eq!(pw.ttl, 2);
        assert_eq!(pw.hop_count, 1);

        assert!(pw.forward());
        assert!(pw.forward());
        assert_eq!(pw.ttl, 0);
        assert_eq!(pw.hop_count, 3);

        // Can't forward with TTL=0
        assert!(!pw.forward());
    }

    #[test]
    fn test_capabilities_roundtrip() {
        let caps = Capabilities::new()
            .with_gpu(true)
            .with_memory(16384)
            .with_model_slots(4)
            .with_tool("python")
            .with_tool("rust")
            .with_tag("inference")
            .with_tag("training");

        let bytes = caps.to_bytes();
        let parsed = Capabilities::from_bytes(&bytes).unwrap();

        assert_eq!(caps.gpu, parsed.gpu);
        assert_eq!(caps.memory_mb, parsed.memory_mb);
        assert_eq!(caps.model_slots, parsed.model_slots);
        assert_eq!(caps.tools, parsed.tools);
        assert_eq!(caps.tags, parsed.tags);
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #100: pre-fix
    /// `LocalGraph::on_pingwave` had no upper bound on
    /// `seen_pingwaves` or `nodes`. A peer flooding pingwaves
    /// with random `(origin_id, seq)` could grow both maps at
    /// line-rate between the periodic eviction sweeps. Post-fix
    /// both insertions are gated on soft caps:
    /// `MAX_SEEN_PINGWAVES` and `MAX_GRAPH_NODES`. Existing
    /// entries continue to update; only novel keys are dropped
    /// when at the cap.
    ///
    /// We pin the soft-cap behaviour by:
    ///   1. Pre-fill `seen_pingwaves` to the cap directly
    ///      (bypasses the slow `on_pingwave` path).
    ///   2. Send a novel pingwave — must be rejected (not
    ///      forwarded, not added to either map).
    ///   3. Send a pingwave with the SAME `(origin_id, seq)` as
    ///      an already-seen entry — also rejected (idempotency
    ///      preserved, regardless of cap).
    ///   4. Repeat for the `nodes` cap by pre-filling and
    ///      verifying novel `origin_id`s are dropped while
    ///      already-known origins are still updated.
    #[test]
    fn on_pingwave_drops_novel_entries_when_seen_pingwaves_is_at_cap() {
        let graph = LocalGraph::new(0x1, 8);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Pre-fill seen_pingwaves to the cap with synthetic keys
        // that won't collide with the test's input.
        for i in 0..MAX_SEEN_PINGWAVES as u64 {
            graph
                .seen_pingwaves
                .insert((0xDEAD_BEEF_0000 + i, 0), Instant::now());
        }
        assert_eq!(graph.seen_pingwaves.len(), MAX_SEEN_PINGWAVES);

        // Send a novel pingwave → must be dropped.
        let novel_pw = Pingwave::new(0xCAFE, 1, 3);
        let result = graph.on_pingwave(novel_pw, from);
        assert!(
            result.is_none(),
            "novel pingwave at cap must NOT be forwarded"
        );
        assert!(
            !graph.seen_pingwaves.contains_key(&(0xCAFE, 1)),
            "novel pingwave must NOT be inserted at cap"
        );
        assert!(
            !graph.nodes.contains_key(&0xCAFE),
            "novel origin must NOT be inserted at cap"
        );
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #120: pre-fix
    /// `on_pingwave` only updated `last_seq`/`last_seen` when
    /// `pw.seq > n.last_seq`. After a peer restart its
    /// `next_seq` resets to 1; with our `n.last_seq` still at
    /// the old high-water mark, every post-restart pingwave was
    /// dropped from updating. The node entered `is_stale` after
    /// 30s and got removed by cleanup — capability lookups
    /// against the stale entry returned outdated info in the
    /// gap.
    ///
    /// Post-fix: a likely restart (`pw.seq <= n.last_seq / 2`
    /// when `n.last_seq > 1`) accepts the seq regression so
    /// `last_seq` advances forward and `last_seen` refreshes,
    /// keeping the entry off the stale-cleanup path.
    ///
    /// CR-6 + Cubic P1 update: the LIVENESS half is
    /// preserved via `touch()` only — neither `last_seq` nor
    /// `addr` updates on the unauthenticated restart-only path.
    /// Pingwaves are unauthenticated UDP and the original fix
    /// let any peer spoof a restart to repoint a victim's
    /// address (CR-6); a follow-up Cubic P1 review noted that
    /// merely lowering `last_seq` opens a slow-rolling vector
    /// where a second spoofed pingwave at seq=2 looks like
    /// strict progress (2 > 1) and DOES overwrite the address.
    ///
    /// The legitimate restarted peer's seq must climb back above
    /// the recorded high-water mark via strict-progress
    /// pingwaves (which the attacker can't beat without seeing
    /// the actual peer's output) before any address update.
    /// In the meantime, `touch()` keeps the entry alive so
    /// stale-eviction doesn't fire.
    #[test]
    fn on_pingwave_likely_restart_only_touches_does_not_lower_last_seq() {
        let graph = LocalGraph::new(0x1, 8);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Bring the peer up to a high seq.
        for seq in [100u64, 200, 500, 1000].iter() {
            let pw = Pingwave::new(0xCAFE, *seq, 3);
            graph.on_pingwave(pw, from);
        }
        let pre_restart_last_seq = graph.nodes.get(&0xCAFE).map(|n| n.last_seq).unwrap();
        assert_eq!(pre_restart_last_seq, 1000);

        // "Restart" pingwave (or, equivalently, an attacker spoof
        // — we can't tell from the wire) at seq=1.
        let restart_from: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        let pw = Pingwave::new(0xCAFE, 1, 3);
        graph.on_pingwave(pw, restart_from);

        let (post_restart_last_seq, post_restart_addr) = graph
            .nodes
            .get(&0xCAFE)
            .map(|n| (n.last_seq, n.addr))
            .unwrap();
        // Cubic P1: last_seq MUST stay at 1000 — lowering it
        // would let a follow-up seq=2 spoof look like strict
        // progress and overwrite the address.
        assert_eq!(
            post_restart_last_seq, 1000,
            "Cubic P1: unauthenticated restart-only path must NOT lower \
             last_seq; the high-water mark is the only credential blocking \
             a spoofed seq=2 from looking like strict progress"
        );
        // CR-6: address stays at the pre-restart address.
        assert_eq!(
            post_restart_addr, from,
            "CR-6: address must NOT auto-update on the restart-only path"
        );

        // The legitimate restarted peer's seq must climb back
        // above 1000 before any address update fires. seq=1001
        // from restart_from — strict progress.
        let pw2 = Pingwave::new(0xCAFE, 1001, 3);
        graph.on_pingwave(pw2, restart_from);
        let (final_last_seq, final_addr) = graph
            .nodes
            .get(&0xCAFE)
            .map(|n| (n.last_seq, n.addr))
            .unwrap();
        assert_eq!(final_last_seq, 1001);
        assert_eq!(
            final_addr, restart_from,
            "strict-progress pingwave (seq > prior high-water) updates addr"
        );
    }

    /// CR-6: pin the security invariant. An attacker who has
    /// observed at least one pingwave for `origin_id` (so they
    /// know it exists in the graph at some seq) can craft a
    /// `(origin_id, seq=1, hops=0)` packet from their own UDP
    /// source. Pre-CR-6 the `likely_restart` heuristic accepted
    /// this and overwrote `n.addr = attacker_addr`, repointing
    /// the victim's recorded routing target at the attacker.
    /// Post-CR-6 + Cubic-P1 the address stays at the legitimately-
    /// recorded value AND `last_seq` stays at the high-water mark
    /// — only `last_seen` (touch) refreshes.
    #[test]
    fn on_pingwave_likely_restart_must_not_overwrite_addr() {
        let graph = LocalGraph::new(0x1, 8);
        let legit: SocketAddr = "10.0.0.5:9000".parse().unwrap();

        // Establish a high seq from the legitimate peer.
        for seq in [100u64, 500, 1000].iter() {
            let pw = Pingwave::new(0xBEEF, *seq, 3);
            graph.on_pingwave(pw, legit);
        }
        assert_eq!(
            graph.nodes.get(&0xBEEF).map(|n| n.addr).unwrap(),
            legit,
            "sanity: legit addr is recorded"
        );

        // Attacker spoofs a restart-shaped pingwave from THEIR
        // address. Pre-CR-6 this overwrote n.addr.
        let attacker: SocketAddr = "192.0.2.99:31337".parse().unwrap();
        let spoof = Pingwave::new(0xBEEF, 1, 3);
        graph.on_pingwave(spoof, attacker);

        let (recorded_addr, recorded_last_seq) = graph
            .nodes
            .get(&0xBEEF)
            .map(|n| (n.addr, n.last_seq))
            .unwrap();
        assert_eq!(
            recorded_addr, legit,
            "CR-6: spoofed restart MUST NOT repoint the recorded address \
             to the attacker; got {:?}",
            recorded_addr
        );
        // Cubic P1: last_seq must STAY at the pre-spoof high-water
        // mark. If it lowered to 1, a follow-up spoofed seq=2
        // would look like strict progress (2 > 1) and overwrite
        // n.addr — the slow-rolling vector that motivated this
        // tightening.
        assert_eq!(
            recorded_last_seq, 1000,
            "Cubic P1: last_seq must STAY at the pre-spoof high-water mark; \
             a lowered last_seq lets a follow-up seq=2 spoof masquerade as \
             strict progress and overwrite n.addr"
        );
    }

    /// Pin: a pingwave whose `seq` is below the recorded
    /// `last_seq` but whose `hops` is shorter MUST NOT
    /// overwrite the recorded address. Pre-fix the predicate
    /// was `pw.seq > last_seq || hops < n.hops` — the OR
    /// arm let an attacker spoofing
    /// `(origin_id=Y, seq=K, hops=0)` for any K below
    /// `last_seq` repoint Y's recorded address. Post-fix
    /// requires BOTH a non-regressing seq AND a non-worse
    /// hop count.
    #[test]
    fn on_pingwave_below_last_seq_with_shorter_hops_does_not_overwrite_addr() {
        let graph = LocalGraph::new(0x1, 8);
        let legit: SocketAddr = "10.0.0.5:9000".parse().unwrap();

        // Establish a high seq from the legitimate peer at
        // recorded hops=3 (constructor's hop_count starts at 0;
        // we bump it so the +1 inside on_pingwave records 3).
        for seq in [100u64, 500, 1000].iter() {
            let mut pw = Pingwave::new(0xBEEF, *seq, 8);
            pw.hop_count = 2;
            graph.on_pingwave(pw, legit);
        }
        assert_eq!(
            graph
                .nodes
                .get(&0xBEEF)
                .map(|n| (n.addr, n.last_seq, n.hops))
                .unwrap(),
            (legit, 1000, 3),
        );

        // Attacker spoofs a stale seq with shorter hops
        // (recorded hops would become 1) from their own UDP
        // source. Pre-fix `hops < n.hops` (1 < 3) flipped
        // strict_progress true and overwrote n.addr.
        let attacker: SocketAddr = "192.0.2.99:31337".parse().unwrap();
        let spoof = Pingwave::new(0xBEEF, 800, 8);
        graph.on_pingwave(spoof, attacker);

        let (recorded_addr, recorded_last_seq, recorded_hops) = graph
            .nodes
            .get(&0xBEEF)
            .map(|n| (n.addr, n.last_seq, n.hops))
            .unwrap();
        assert_eq!(
            recorded_addr, legit,
            "stale-seq + shorter-hops spoof must NOT repoint addr; \
             got {:?}",
            recorded_addr,
        );
        assert_eq!(recorded_last_seq, 1000, "last_seq must not regress");
        assert_eq!(recorded_hops, 3, "hops must not be lowered by stale seq");
    }

    /// Sanity: a small seq regression that is NOT below
    /// `last_seq / 2` should still be ignored — protects against
    /// out-of-order pingwaves on a non-restarted peer.
    #[test]
    fn on_pingwave_ignores_small_seq_regression_without_restart_signal() {
        let graph = LocalGraph::new(0x1, 8);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        for seq in [10u64, 20].iter() {
            let pw = Pingwave::new(0xCAFE, *seq, 3);
            graph.on_pingwave(pw, from);
        }

        // seq=15 is below 20 but above 20/2=10 — out-of-order,
        // not a restart. Don't update.
        let pw = Pingwave::new(0xCAFE, 15, 3);
        graph.on_pingwave(pw, from);
        let last_seq = graph.nodes.get(&0xCAFE).map(|n| n.last_seq).unwrap();
        assert_eq!(
            last_seq, 20,
            "small seq regression (out-of-order) must NOT update last_seq"
        );
    }

    #[test]
    fn on_pingwave_drops_novel_origin_when_nodes_is_at_cap() {
        let graph = LocalGraph::new(0x1, 8);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // Pre-populate `nodes` to the cap with synthetic ids.
        for i in 0..MAX_GRAPH_NODES as u64 {
            let id = 0xDEAD_BEEF_0000 + i;
            graph.nodes.insert(id, NodeInfo::new(id, from, 1));
        }
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES);

        // A pingwave from a novel origin must NOT be inserted.
        let novel_pw = Pingwave::new(0xFACE, 1, 3);
        graph.on_pingwave(novel_pw, from);
        assert!(
            !graph.nodes.contains_key(&0xFACE),
            "novel origin at cap must NOT be inserted"
        );

        // BUT an existing origin should still update on a fresh
        // pingwave (caps don't kick out legitimate peers).
        let existing_id = 0xDEAD_BEEF_0000u64;
        let existing_pw = Pingwave::new(existing_id, 99, 3);
        let pre_seq = graph.nodes.get(&existing_id).unwrap().last_seq;
        graph.on_pingwave(existing_pw, from);
        let post_seq = graph.nodes.get(&existing_id).unwrap().last_seq;
        assert!(
            post_seq > pre_seq,
            "already-known origin must keep updating despite cap"
        );
    }

    #[test]
    fn test_capability_ad_roundtrip() {
        let caps = Capabilities::new().with_gpu(true).with_tool("test");
        let ad = CapabilityAd::new(0x1234, 5, caps);

        let bytes = ad.to_bytes();
        let parsed = CapabilityAd::from_bytes(&bytes).unwrap();

        assert_eq!(ad.node_id, parsed.node_id);
        assert_eq!(ad.version, parsed.version);
        assert_eq!(ad.capabilities.gpu, parsed.capabilities.gpu);
    }

    #[test]
    fn test_capabilities_large_strings_capped() {
        // Regression: tools.len() and string lengths were cast to u8 without
        // bounds checks, silently truncating counts > 255 and strings > 255 bytes.
        let long_tool = "x".repeat(300);
        let caps = Capabilities::new().with_tool(&long_tool);

        let bytes = caps.to_bytes();
        let parsed = Capabilities::from_bytes(&bytes).unwrap();

        // String should be truncated to 255 bytes, not wrap around
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.tools[0].len(), 255);
    }

    #[test]
    fn test_capabilities_many_items_capped() {
        // More than 255 tools should be capped, not truncated via `as u8`
        let mut caps = Capabilities::new();
        for i in 0..300 {
            caps = caps.with_tool(format!("t{}", i));
        }

        let bytes = caps.to_bytes();
        let parsed = Capabilities::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.tools.len(), 255);
    }

    #[test]
    fn test_local_graph_pingwave_processing() {
        let graph = LocalGraph::new(0x1111, 3);

        // Simulate receiving a pingwave from a neighbor
        let pw = Pingwave::new(0x2222, 1, 3);
        let from: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        let forwarded = graph.on_pingwave(pw, from);
        assert!(forwarded.is_some());

        // Check node was added
        let node = graph.get_node(0x2222).unwrap();
        assert_eq!(node.hops, 1);
        assert_eq!(node.addr, from);

        // Same pingwave should be deduplicated
        let pw2 = Pingwave::new(0x2222, 1, 3);
        assert!(graph.on_pingwave(pw2, from).is_none());

        // New sequence should be accepted
        let pw3 = Pingwave::new(0x2222, 2, 3);
        assert!(graph.on_pingwave(pw3, from).is_some());
    }

    #[test]
    fn test_local_graph_capability_search() {
        let graph = LocalGraph::new(0x1111, 3);

        // Add some nodes
        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        graph.nodes.insert(0x2222, NodeInfo::new(0x2222, addr, 1));
        graph.nodes.insert(0x3333, NodeInfo::new(0x3333, addr, 2));

        // Add capabilities
        let caps1 = Capabilities::new().with_gpu(true).with_tool("python");
        let caps2 = Capabilities::new().with_gpu(false).with_tool("rust");

        graph.on_capability(CapabilityAd::new(0x2222, 1, caps1), addr);
        graph.on_capability(CapabilityAd::new(0x3333, 1, caps2), addr);

        // Search by tool
        let python_nodes = graph.find_by_tool("python");
        assert_eq!(python_nodes.len(), 1);
        assert_eq!(python_nodes[0].node_id, 0x2222);

        // Search by GPU
        let gpu_nodes = graph.find_with_gpu();
        assert_eq!(gpu_nodes.len(), 1);
        assert_eq!(gpu_nodes[0].node_id, 0x2222);
    }

    #[test]
    fn test_capability_ad_creates_unknown_node() {
        // Regression: on_capability() only called and_modify(), so capability
        // advertisements for nodes not yet seen via pingwave were silently
        // dropped. The node never became searchable by capability.
        let graph = LocalGraph::new(0x1111, 3);
        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();

        // No pingwave — node 0x2222 is completely unknown
        assert!(graph.get_node(0x2222).is_none());

        // Send a capability advertisement directly
        let caps = Capabilities::new().with_gpu(true).with_tool("python");
        graph.on_capability(CapabilityAd::new(0x2222, 1, caps), addr);

        // Node should now exist and be searchable
        let node = graph.get_node(0x2222);
        assert!(node.is_some(), "node should be created from capability ad");
        let node = node.unwrap();
        assert!(node.capabilities.is_some());

        let gpu_nodes = graph.find_with_gpu();
        assert_eq!(gpu_nodes.len(), 1);
        assert_eq!(gpu_nodes[0].node_id, 0x2222);
    }

    #[test]
    fn test_local_graph_path_finding() {
        let graph = LocalGraph::new(0x1111, 3);

        // Add edges: 1111 -> 2222 -> 3333 -> 4444
        graph.add_edge(0x1111, 0x2222, 100);
        graph.add_edge(0x2222, 0x3333, 100);
        graph.add_edge(0x3333, 0x4444, 100);

        // Find path to 4444
        let path = graph.path_to(0x4444);
        assert!(path.is_some());
        let path = path.unwrap();
        assert_eq!(path, vec![0x1111, 0x2222, 0x3333, 0x4444]);

        // No path to unknown node
        let no_path = graph.path_to(0x9999);
        assert!(no_path.is_none());
    }
}
