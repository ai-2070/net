# Phase 3: theNet Network Layer Benchmarks

## Goal

Prove that BLTP + BLTP-based routing is viable for theNet under realistic conditions:
- Thousands of streams
- Multiple hops (proxying via untrusted nodes)
- Mixed payload sizes (control + data)
- Dynamic joins/leaves
- Packet loss & jitter

Without losing:
- Sub-ms routing overhead
- Stream fairness
- Stability under load

---

## Sub-Phases

| Phase | Focus | Key Metric |
|-------|-------|------------|
| **3A** | Single-hop routing & stream multiplexing | 10-20M events/s routed, p99 < 500µs |
| **3B** | Multi-hop & proxy routing | Linear latency scaling, untrusted relay |
| **3C** | Swarm / pingwave / graph maintenance | 1k nodes, <10% CPU, fast convergence |
| **3D** | Failure modes & recovery | Graceful degradation, fast recovery |

---

## 3A — Single-Hop Routing & Stream Multiplexing

### Objective
One BLTP node acting as a router can:
- Multiplex thousands of streams
- Route packets between N producers and M consumers
- Maintain low per-packet overhead

### Scenario
```
        ┌─────────┐
   C1 ──┤         ├── C5
   C2 ──┤  Router ├── C6
   C3 ──┤         ├── C7
   C4 ──┴─────────┴── C8
```
- 1 central Router node
- 8–32 Clients (simulated L0/L1/L2 nodes)
- Each client opens K streams (K = 8, 32, 128)
- Sends at 10k, 50k, 100k packets/s per client
- Mixed: small control frames (64B/128B) + payload frames (256B/1KB)

### Benchmarks

#### 1. Routing Latency
Measure: Client A `send()` → Client B `recv()`

| Percentile | Target |
|------------|--------|
| p50 | < 50µs |
| p95 | < 200µs |
| p99 | < 500µs |

#### 2. Router CPU Usage
- Vary client count (8, 16, 24, 32) & streams (8, 32, 128)
- Plot CPU % vs events/sec routed
- Target: 10–20M events/s on single core

#### 3. Fairness Under Contention
- One "greedy" client floods the router
- Others send at modest rates
- Measure drops/latency spikes per stream
- No stream should starve

### Implementation

```rust
// New module: src/adapter/bltp/router.rs

/// Stream routing table
pub struct RoutingTable {
    /// stream_id -> destination peer
    routes: DashMap<u64, SocketAddr>,
    /// Per-stream stats for fairness monitoring
    stats: DashMap<u64, StreamStats>,
}

/// Single-hop router
pub struct BltpRouter {
    socket: Arc<BltpSocket>,
    routing_table: RoutingTable,
    /// Fair queuing scheduler
    scheduler: FairScheduler,
}

impl BltpRouter {
    /// Route incoming packet to destination
    pub async fn route(&self, packet: ParsedPacket) -> Result<(), RouterError>;
    
    /// Register a stream route
    pub fn add_route(&self, stream_id: u64, dest: SocketAddr);
    
    /// Get routing stats
    pub fn stats(&self) -> RouterStats;
}
```

### Benchmark CLI
```bash
cargo bench --features bltp --bench router -- single_hop
# or
thenet bench single-hop --clients 16 --streams 128 --rate 100000
```

---

## 3B — Multi-Hop & Proxy Routing

### Objective
Prove that:
- Multi-hop routing (2–5 hops) is stable
- Latency scales roughly linearly with hop count
- Proxies don't become correctness bottlenecks
- E2E crypto remains intact regardless of hops

### Topologies

#### Line Topology
```
A → R1 → R2 → R3 → B
    hop1  hop2  hop3
```

#### Diamond Topology
```
      ┌─→ R1 ─┐
  A ──┤       ├──→ B
      └─→ R2 ─┘
```

#### Mesh Topology
```
    R1 ─── R2
   / │ \ / │ \
  A  │  X  │  B
   \ │ / \ │ /
    R3 ─── R4
```
10 nodes, partial connectivity, shortest path routing.

### Benchmarks

#### 1. Hop Latency Scaling
| Hops | p50 | p95 | p99 | Expected |
|------|-----|-----|-----|----------|
| 1 | - | - | - | baseline |
| 2 | - | - | - | ~2x |
| 3 | - | - | - | ~3x |
| 4 | - | - | - | ~4x |
| 5 | - | - | - | ~5x |

#### 2. Throughput Under Multi-Hop
| Rate | 2 hops | 3 hops | 4 hops | 5 hops |
|------|--------|--------|--------|--------|
| 100k/s | - | - | - | - |
| 1M/s | - | - | - | - |
| 5M/s | - | - | - | - |
| 10M/s | - | - | - | - |

#### 3. Proxy CPU & Memory
- Measure resource usage on intermediate nodes
- Confirm they do NOT decrypt payloads (just shuffle frames)

### Implementation

```rust
// Routing header for multi-hop
#[repr(C)]
pub struct RoutingHeader {
    /// Final destination node ID
    pub dest_id: u64,
    /// TTL (decremented at each hop)
    pub ttl: u8,
    /// Hop count so far
    pub hop_count: u8,
    /// Route flags
    pub flags: RouteFlags,
    /// Reserved
    pub _reserved: [u8; 5],
}

/// Proxy node - forwards without decrypting payload
pub struct BltpProxy {
    /// Next-hop routing table
    next_hop: DashMap<u64, SocketAddr>,
    /// Forwarding stats
    stats: ProxyStats,
}

impl BltpProxy {
    /// Forward packet to next hop (zero-copy, no decryption)
    pub async fn forward(&self, packet: Bytes) -> Result<(), ProxyError>;
}
```

### Benchmark CLI
```bash
thenet bench multi-hop --hops 4 --rate 1000000
thenet bench topology --type diamond --clients 8
```

---

## 3C — Swarm / Pingwaves / Graph Maintenance

### Objective
Show that:
- Nodes can discover each other via pingwaves
- Maintain distributed view of local network graph
- Periodically update capabilities
- WITHOUT blowing up CPU/memory or flooding the wire

### Scenario
- 100–1,000 simulated nodes
- Each node has ID + capability set
- Periodically emits pingwaves + capability updates
- Maintains local view (2–3 hop radius)

### Benchmarks

#### 1. Pingwave Overhead
| Nodes | CPU % (target <10%) |
|-------|---------------------|
| 100 | - |
| 250 | - |
| 500 | - |
| 1000 | - |

#### 2. Convergence Time
When a new node joins:
| Metric | p50 | p95 |
|--------|-----|-----|
| Neighbors see it | - | - |
| 2-hop neighbors see it | - | - |

#### 3. Graph Memory Usage
- Per-node memory for 2-hop neighborhood
- Target: O(k) where k = neighbors, NOT O(N²)

#### 4. Update Storms
- 5–10% of nodes change capabilities at once
- Ensure no unusable load spike

### Implementation

```rust
// Pingwave packet
#[repr(C)]
pub struct Pingwave {
    /// Originating node ID
    pub origin_id: u64,
    /// Sequence number (monotonic)
    pub seq: u64,
    /// TTL (usually 2-3)
    pub ttl: u8,
    /// Timestamp (for latency estimation)
    pub timestamp_ns: u64,
}

// Capability advertisement
pub struct CapabilityAd {
    pub node_id: u64,
    pub version: u32,
    pub capabilities: Capabilities,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub gpu: bool,
    pub tools: Vec<String>,
    pub memory_mb: u32,
    pub model_slots: u8,
}

/// Local network graph view
pub struct LocalGraph {
    /// Known nodes within radius
    nodes: DashMap<u64, NodeInfo>,
    /// Edges (connections between nodes)
    edges: DashMap<(u64, u64), EdgeInfo>,
    /// My node ID
    my_id: u64,
    /// Max hops to track
    radius: u8,
}

impl LocalGraph {
    /// Process incoming pingwave
    pub fn on_pingwave(&self, pw: Pingwave, from: SocketAddr);
    
    /// Process capability update
    pub fn on_capability(&self, cap: CapabilityAd);
    
    /// Get nodes with specific capability
    pub fn find_by_capability(&self, cap: &str) -> Vec<u64>;
    
    /// Get shortest path to node
    pub fn path_to(&self, dest: u64) -> Option<Vec<u64>>;
}
```

### Benchmark CLI
```bash
thenet bench swarm --nodes 500 --radius 2
thenet bench convergence --nodes 100 --join-rate 10
```

---

## 3D — Failure Modes & Recovery

### Objective
Prove that theNet can:
- Handle node failures
- Drop + recompute routes
- Tolerate partial packet loss
- Keep streams alive under churn

### Scenarios

#### 1. Node Death Mid-Stream
```
A → R1 → R2 → B
       ↓
    R1 dies
       ↓
A → R3 → R2 → B  (reroute)
```

#### 2. Flapping Nodes
- Nodes appear/disappear repeatedly
- Ensure they don't destabilize the mesh

#### 3. Packet Loss Simulation
- Drop X% of packets (1%, 5%, 10%)
- Measure retransmission behavior
- Impact on stream quality
- Control traffic resilience

#### 4. Hot Restart
- Restart router while streams exist
- Measure reconnection time
- Verify correctness (no misrouted packets)

### Benchmarks

| Scenario | Metric | Target |
|----------|--------|--------|
| Node death | Time-to-recovery | < 1s |
| Node death | Dropped frames | < 100 |
| Flapping | Stability | No cascade |
| 5% packet loss | Throughput impact | < 20% |
| Hot restart | Reconnect time | < 500ms |

### Implementation

```rust
/// Failure detector
pub struct FailureDetector {
    /// Last heartbeat per node
    heartbeats: DashMap<u64, Instant>,
    /// Failure threshold
    timeout: Duration,
    /// Callback on failure
    on_failure: Box<dyn Fn(u64) + Send + Sync>,
}

/// Route recovery
impl BltpRouter {
    /// Handle node failure notification
    pub fn on_node_failure(&self, node_id: u64);
    
    /// Recompute routes avoiding failed node
    pub fn recompute_routes(&self, exclude: &[u64]);
}

/// Packet loss simulation (for testing)
pub struct LossSimulator {
    loss_rate: f32,
    rng: SmallRng,
}

impl LossSimulator {
    pub fn should_drop(&mut self) -> bool {
        self.rng.gen::<f32>() < self.loss_rate
    }
}
```

### Benchmark CLI
```bash
thenet bench failures --scenario node-death
thenet bench failures --scenario flapping --rate 0.1
thenet bench failures --scenario packet-loss --loss 0.05
thenet bench failures --scenario hot-restart
```

---

## CLI Design

```bash
# Single binary with subcommands
thenet bench <subcommand> [options]

# Phase 3A
thenet bench single-hop --clients 16 --streams 128 --rate 100000

# Phase 3B  
thenet bench multi-hop --hops 4 --rate 1000000
thenet bench topology --type mesh --nodes 10

# Phase 3C
thenet bench swarm --nodes 500 --radius 2
thenet bench convergence --nodes 100

# Phase 3D
thenet bench failures --scenario node-death
thenet bench failures --scenario packet-loss --loss 0.05

# Output formats
thenet bench ... --output json
thenet bench ... --output table
thenet bench ... --output chart  # generates PNG
```

---

## File Structure

```
src/
├── adapter/
│   └── bltp/
│       ├── mod.rs
│       ├── router.rs      # NEW: Single-hop router
│       ├── proxy.rs       # NEW: Multi-hop proxy
│       ├── routing.rs     # NEW: Routing table & headers
│       ├── swarm.rs       # NEW: Pingwave & graph
│       └── failure.rs     # NEW: Failure detection
├── bin/
│   └── thenet.rs          # NEW: CLI binary
benches/
├── bltp.rs                # Existing
├── router.rs              # NEW: Phase 3A benches
├── multihop.rs            # NEW: Phase 3B benches
├── swarm.rs               # NEW: Phase 3C benches
└── failures.rs            # NEW: Phase 3D benches
```

---

## Success Criteria

### Phase 3A
> "A single BLTP router can multiplex and route millions of events per second across thousands of streams with sub-millisecond per-hop latency and no starvation."

### Phase 3B
> "With BLTP, multi-hop encrypted routing adds almost no overhead beyond linear hop latency; untrusted nodes can relay millions of events per second without ever touching payloads."

### Phase 3C
> "TheNet can maintain a live, distributed view of node capabilities and proximity across hundreds to thousands of nodes with low overhead and fast convergence."

### Phase 3D
> "TheNet tolerates node failure, churn, and packet loss while keeping routing stable and recovery times low, making it suitable for real-world heterogeneous networks."

---

## Next Steps

1. **Spec routing header format** - exact byte layout, versioning
2. **Spec pingwave encoding** - proximity, capability serialization
3. **Implement Phase 3A** - router + benchmarks
4. **Iterate through 3B, 3C, 3D**
5. **Generate killer graphs** for README/Twitter

---

## Timeline Estimate

| Phase | Complexity | Dependencies |
|-------|------------|--------------|
| 3A | Medium | None |
| 3B | Medium | 3A routing table |
| 3C | High | 3A/3B for transport |
| 3D | Medium | 3A/3B/3C complete |

Recommend starting with 3A to validate the routing core, then layer 3B/3C/3D on top.
