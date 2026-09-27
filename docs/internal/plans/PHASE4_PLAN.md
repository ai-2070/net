# Phase 4: The Behavior Plane

## Goal

Build the semantic layer on top of theNet's transport (Phase 1-3), enabling:
- Rich capability discovery and matching
- Dynamic API exposure and schema negotiation
- Device autonomy and self-governance
- Cross-node context sharing (Context Fabric)
- Intelligent load balancing based on capabilities
- Safety envelope enforcement

This transforms theNet from a transport layer into a **self-describing, self-organizing compute fabric**.

---

## Sub-Phases

| Phase | Focus | Key Metric |
|-------|-------|------------|
| **4A** | Capability Announcements (CAP-ANN) | 100k+ caps/s indexed, <1ms lookup |
| **4B** | Capability Change Diffs (CAP-DIFF) | <1% bandwidth vs full CAP-ANN |
| **4C** | Node Metadata Surface (NODE-META) | Structured metadata, typed queries |
| **4D** | Node APIs & Schemas (API-SCHEMA) | JSON Schema validation, versioning |
| **4E** | Device Autonomy Rules (DEVICE-RULES) | Policy enforcement <100ns |
| **4F** | Context Fabric (CTXT-FABRIC) | Cross-node context propagation |
| **4G** | Distributed Load Balancing (LOAD-BALANCE) | Capability-aware routing |
| **4H** | Proximity Graph Integration (PINGWAVE++) | Capability + proximity fusion |
| **4I** | Safety Envelope Enforcement | Hard limits, audit trail |

### Extended Phases (4X)

| Phase | Focus | Key Metric |
|-------|-------|------------|
| **4X-A** | Proximity Properties | Static + dynamic proximity metrics |
| **4X-B** | Autonomous Load Balancing | Self-directed load shedding/vacuuming |
| **4X-C** | AI/LLM Node Classes | First-class AI node support |
| **4X-D** | Real-time Typesafe APIs | Device-emitted schemas, intent graphs |

---

## 4A — Capability Announcements (CAP-ANN)

### Objective
Every node can broadcast its capabilities in a structured, versioned format that:
- Is compact and fast to parse
- Supports rich capability types (hardware, software, models, tools)
- Enables efficient indexing and querying
- Supports capability inheritance and composition

### Capability Schema

```rust
/// Core capability announcement
#[derive(Clone, Serialize, Deserialize)]
pub struct CapabilityAnnouncement {
    /// Announcing node ID
    pub node_id: u64,
    /// Monotonic version (for diffing)
    pub version: u64,
    /// Timestamp of announcement
    pub timestamp_ns: u64,
    /// TTL for this announcement
    pub ttl_secs: u32,
    /// Capability set
    pub capabilities: CapabilitySet,
    /// Signature (optional, for verified caps)
    pub signature: Option<[u8; 64]>,
}

/// Structured capability set
#[derive(Clone, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Hardware capabilities
    pub hardware: HardwareCapabilities,
    /// Software/runtime capabilities
    pub software: SoftwareCapabilities,
    /// Model capabilities (for AI nodes)
    pub models: Vec<ModelCapability>,
    /// Tool capabilities
    pub tools: Vec<ToolCapability>,
    /// Custom tags
    pub tags: Vec<String>,
    /// Resource limits
    pub limits: ResourceLimits,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HardwareCapabilities {
    pub cpu_cores: u16,
    pub memory_mb: u32,
    pub gpu: Option<GpuInfo>,
    pub storage_mb: u64,
    pub network_mbps: u32,
    pub accelerators: Vec<AcceleratorInfo>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub model: String,
    pub vram_mb: u32,
    pub compute_units: u16,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelCapability {
    pub model_id: String,
    pub family: String,
    pub parameters: u64,
    pub context_length: u32,
    pub quantization: Option<String>,
    pub modalities: Vec<Modality>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolCapability {
    pub tool_id: String,
    pub name: String,
    pub version: String,
    pub input_schema: Option<String>,  // JSON Schema
    pub output_schema: Option<String>, // JSON Schema
    pub requires: Vec<String>,         // Dependencies
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_concurrent_requests: u32,
    pub max_tokens_per_request: u32,
    pub rate_limit_rpm: u32,
    pub max_batch_size: u32,
}
```

### Capability Index

```rust
/// In-memory capability index for fast queries
pub struct CapabilityIndex {
    /// Node ID -> full capability set
    nodes: DashMap<u64, IndexedNode>,
    /// Inverted indexes for fast lookup
    by_tag: DashMap<String, HashSet<u64>>,
    by_model: DashMap<String, HashSet<u64>>,
    by_tool: DashMap<String, HashSet<u64>>,
    by_gpu_vendor: DashMap<GpuVendor, HashSet<u64>>,
    /// Version tracking for diffs
    versions: DashMap<u64, u64>,
}

impl CapabilityIndex {
    /// Index a capability announcement
    pub fn index(&self, ann: CapabilityAnnouncement);
    
    /// Query nodes by capability filter
    pub fn query(&self, filter: &CapabilityFilter) -> Vec<u64>;
    
    /// Find best match for requirements
    pub fn find_best(&self, req: &CapabilityRequirement) -> Option<u64>;
    
    /// Evict stale entries
    pub fn gc(&self, now: Instant);
}

/// Capability query filter
#[derive(Clone)]
pub struct CapabilityFilter {
    pub require_tags: Vec<String>,
    pub require_models: Vec<String>,
    pub require_tools: Vec<String>,
    pub min_memory_mb: Option<u32>,
    pub require_gpu: bool,
    pub gpu_vendor: Option<GpuVendor>,
    pub min_vram_mb: Option<u32>,
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Index throughput | 100k+ announcements/s |
| Query latency (single tag) | < 100µs |
| Query latency (complex filter) | < 1ms |
| Memory per 10k nodes | < 50MB |
| Serialization size (avg) | < 500 bytes |

### Benchmark CLI
```bash
thenet bench cap-ann --nodes 10000 --queries 100000
thenet bench cap-index --concurrent-updates 1000
```

---

## 4B — Capability Change Diffs (CAP-DIFF)

### Objective
Instead of re-broadcasting full capability sets, nodes send compact diffs:
- Reduce bandwidth by 90%+ for stable networks
- Enable incremental index updates
- Support vector clocks for consistency

### Diff Format

```rust
/// Capability diff message
#[derive(Clone, Serialize, Deserialize)]
pub struct CapabilityDiff {
    /// Source node
    pub node_id: u64,
    /// Base version this diff applies to
    pub base_version: u64,
    /// New version after applying diff
    pub new_version: u64,
    /// Operations to apply
    pub ops: Vec<DiffOp>,
    /// Timestamp
    pub timestamp_ns: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum DiffOp {
    /// Add a tag
    AddTag(String),
    /// Remove a tag
    RemoveTag(String),
    /// Add a model capability
    AddModel(ModelCapability),
    /// Remove a model
    RemoveModel(String),
    /// Add a tool
    AddTool(ToolCapability),
    /// Remove a tool
    RemoveTool(String),
    /// Update resource limits
    UpdateLimits(ResourceLimits),
    /// Update hardware info
    UpdateHardware(HardwareCapabilities),
    /// Set a custom field
    SetField { path: String, value: serde_json::Value },
    /// Unset a custom field
    UnsetField { path: String },
}

/// Diff engine
pub struct DiffEngine {
    /// Generate diff between two capability sets
    pub fn diff(old: &CapabilitySet, new: &CapabilitySet) -> Vec<DiffOp>;
    
    /// Apply diff to capability set
    pub fn apply(base: &mut CapabilitySet, ops: &[DiffOp]) -> Result<(), DiffError>;
    
    /// Validate diff chain
    pub fn validate_chain(diffs: &[CapabilityDiff]) -> bool;
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Diff generation | < 1µs for typical changes |
| Diff application | < 500ns |
| Diff size (1 op) | < 50 bytes |
| Bandwidth savings | > 90% vs full CAP-ANN |

### Benchmark CLI
```bash
thenet bench cap-diff --changes-per-node 10 --nodes 1000
thenet bench cap-diff-bandwidth --duration 60s
```

---

## 4C — Node Metadata Surface (NODE-META)

### Objective
Beyond capabilities, nodes expose rich metadata:
- Human-readable descriptions
- Geographic/network location hints
- Operational status
- Custom application-specific metadata

### Metadata Schema

```rust
/// Node metadata surface
#[derive(Clone, Serialize, Deserialize)]
pub struct NodeMetadata {
    /// Node identity
    pub node_id: u64,
    /// Human-readable name
    pub name: Option<String>,
    /// Description
    pub description: Option<String>,
    /// Owner/operator
    pub owner: Option<String>,
    /// Location hints
    pub location: Option<LocationInfo>,
    /// Network topology hints
    pub topology: TopologyHints,
    /// Operational status
    pub status: NodeStatus,
    /// Last status update
    pub status_updated: u64,
    /// Custom key-value metadata
    pub custom: HashMap<String, MetadataValue>,
    /// Metadata version
    pub version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LocationInfo {
    /// Geographic region (e.g., "us-west-2", "eu-central-1")
    pub region: Option<String>,
    /// Data center / zone
    pub zone: Option<String>,
    /// Latitude (optional, for proximity)
    pub lat: Option<f32>,
    /// Longitude
    pub lon: Option<f32>,
    /// Network AS number
    pub asn: Option<u32>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TopologyHints {
    /// Preferred peers (for sticky routing)
    pub preferred_peers: Vec<u64>,
    /// Network tier (0 = backbone, 1 = regional, 2 = edge)
    pub tier: u8,
    /// Estimated uplink bandwidth
    pub uplink_mbps: u32,
    /// NAT type
    pub nat_type: NatType,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Fully operational
    Online,
    /// Accepting limited traffic
    Degraded,
    /// Draining (no new requests)
    Draining,
    /// Maintenance mode
    Maintenance,
    /// Offline
    Offline,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Bool(bool),
    Array(Vec<MetadataValue>),
    Object(HashMap<String, MetadataValue>),
}
```

### Metadata Store

```rust
/// Local metadata store with query support
pub struct MetadataStore {
    nodes: DashMap<u64, NodeMetadata>,
    /// Index by status
    by_status: DashMap<NodeStatus, HashSet<u64>>,
    /// Index by region
    by_region: DashMap<String, HashSet<u64>>,
    /// Index by tier
    by_tier: DashMap<u8, HashSet<u64>>,
}

impl MetadataStore {
    /// Query nodes by metadata filter
    pub fn query(&self, filter: &MetadataQuery) -> Vec<u64>;
    
    /// Get nodes in same region
    pub fn same_region(&self, node_id: u64) -> Vec<u64>;
    
    /// Get nodes by status
    pub fn by_status(&self, status: NodeStatus) -> Vec<u64>;
}

/// Metadata query
pub struct MetadataQuery {
    pub status: Option<NodeStatus>,
    pub region: Option<String>,
    pub tier: Option<u8>,
    pub custom_filter: Option<Box<dyn Fn(&NodeMetadata) -> bool + Send + Sync>>,
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Metadata update | < 500ns |
| Query by status | < 10µs |
| Query by region | < 10µs |
| Complex query | < 100µs |

---

## 4D — Node APIs & Schemas (API-SCHEMA)

### Objective
Nodes expose typed APIs with JSON Schema validation:
- Self-describing endpoints
- Version negotiation
- Input/output validation
- API discovery

### API Schema

```rust
/// API endpoint definition
#[derive(Clone, Serialize, Deserialize)]
pub struct ApiEndpoint {
    /// Endpoint ID (unique per node)
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    pub description: Option<String>,
    /// API version
    pub version: String,
    /// Input schema (JSON Schema)
    pub input_schema: serde_json::Value,
    /// Output schema (JSON Schema)
    pub output_schema: serde_json::Value,
    /// Error schema
    pub error_schema: Option<serde_json::Value>,
    /// Supported content types
    pub content_types: Vec<String>,
    /// Required capabilities to call
    pub requires: Vec<String>,
    /// Rate limits
    pub rate_limit: Option<RateLimit>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RateLimit {
    pub requests_per_minute: u32,
    pub burst: u32,
    pub per_key: bool,
}

/// Node API surface
#[derive(Clone, Serialize, Deserialize)]
pub struct NodeApiSurface {
    pub node_id: u64,
    pub api_version: String,
    pub endpoints: Vec<ApiEndpoint>,
    pub default_content_type: String,
}

/// API registry with validation
pub struct ApiRegistry {
    /// Compiled JSON Schema validators
    validators: DashMap<(u64, String), CompiledSchema>,
    /// API surfaces by node
    surfaces: DashMap<u64, NodeApiSurface>,
}

impl ApiRegistry {
    /// Register node API surface
    pub fn register(&self, surface: NodeApiSurface) -> Result<(), SchemaError>;
    
    /// Validate request against schema
    pub fn validate_request(
        &self,
        node_id: u64,
        endpoint_id: &str,
        input: &serde_json::Value,
    ) -> Result<(), ValidationError>;
    
    /// Validate response
    pub fn validate_response(
        &self,
        node_id: u64,
        endpoint_id: &str,
        output: &serde_json::Value,
    ) -> Result<(), ValidationError>;
    
    /// Find endpoints matching capability requirements
    pub fn find_endpoints(&self, requires: &[String]) -> Vec<(u64, ApiEndpoint)>;
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Schema compilation | < 1ms per schema |
| Input validation | < 10µs for typical request |
| Output validation | < 10µs |
| Endpoint lookup | < 1µs |

---

## 4E — Device Autonomy Rules (DEVICE-RULES)

### Objective
Nodes enforce local policies for:
- What requests they accept
- Resource allocation
- Privacy boundaries
- Cost/billing limits
- Time-based restrictions

### Rule Engine

```rust
/// Device autonomy rule
#[derive(Clone, Serialize, Deserialize)]
pub struct AutonomyRule {
    pub id: String,
    pub name: String,
    pub priority: i32,  // Higher = evaluated first
    pub condition: RuleCondition,
    pub action: RuleAction,
    pub enabled: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum RuleCondition {
    /// Always matches
    Always,
    /// Never matches
    Never,
    /// Match by source node
    SourceNode { nodes: Vec<u64>, mode: MatchMode },
    /// Match by source tag
    SourceTag { tags: Vec<String>, mode: MatchMode },
    /// Match by request type
    RequestType { types: Vec<String> },
    /// Match by time of day
    TimeOfDay { start_hour: u8, end_hour: u8, timezone: String },
    /// Match by resource usage
    ResourceUsage { metric: ResourceMetric, op: CompareOp, threshold: f64 },
    /// Match by rate
    RateExceeded { window_secs: u32, max_requests: u32, key: RateKey },
    /// Logical AND
    And(Vec<RuleCondition>),
    /// Logical OR
    Or(Vec<RuleCondition>),
    /// Logical NOT
    Not(Box<RuleCondition>),
    /// Custom expression (for advanced users)
    Expression(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub enum RuleAction {
    /// Allow the request
    Allow,
    /// Deny the request
    Deny { reason: String },
    /// Rate limit
    RateLimit { rpm: u32 },
    /// Queue with priority
    Queue { priority: i32 },
    /// Redirect to another node
    Redirect { target: u64 },
    /// Log and continue
    Log { level: LogLevel, message: String },
    /// Set metadata on request
    SetMeta { key: String, value: String },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum MatchMode {
    Any,      // Match if any in list
    All,      // Match if all in list
    None,     // Match if none in list
}

/// Rule engine with fast evaluation
pub struct RuleEngine {
    rules: Vec<AutonomyRule>,
    /// Precompiled conditions for hot path
    compiled: Vec<CompiledCondition>,
    /// Stats
    stats: RuleStats,
}

impl RuleEngine {
    /// Evaluate rules for a request
    pub fn evaluate(&self, ctx: &RequestContext) -> RuleResult;
    
    /// Add a rule
    pub fn add_rule(&mut self, rule: AutonomyRule);
    
    /// Remove a rule
    pub fn remove_rule(&mut self, id: &str);
    
    /// Update rule priority
    pub fn set_priority(&mut self, id: &str, priority: i32);
}

/// Request context for rule evaluation
pub struct RequestContext {
    pub source_node: u64,
    pub source_tags: Vec<String>,
    pub request_type: String,
    pub timestamp: Instant,
    pub resource_usage: ResourceSnapshot,
    pub custom: HashMap<String, String>,
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Rule evaluation (10 rules) | < 100ns |
| Rule evaluation (100 rules) | < 1µs |
| Rule compilation | < 10µs per rule |
| Hot path (cached) | < 50ns |

### Benchmark CLI
```bash
thenet bench device-rules --rules 100 --requests 1000000
thenet bench rule-compilation --complexity high
```

---

## 4F — Context Fabric (CTXT-FABRIC)

### Objective
Enable cross-node context sharing for:
- Distributed inference (share context across model shards)
- Multi-agent coordination
- Session state propagation
- Conversation history

### Context Types

```rust
/// Context chunk for fabric sharing
#[derive(Clone, Serialize, Deserialize)]
pub struct ContextChunk {
    /// Unique context ID
    pub context_id: u128,
    /// Chunk sequence number
    pub seq: u32,
    /// Total chunks (if known)
    pub total: Option<u32>,
    /// Context type
    pub context_type: ContextType,
    /// Payload
    pub payload: Bytes,
    /// TTL in seconds
    pub ttl_secs: u32,
    /// Created timestamp
    pub created_ns: u64,
    /// Origin node
    pub origin: u64,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum ContextType {
    /// Model KV cache
    KvCache,
    /// Conversation history
    Conversation,
    /// Agent state
    AgentState,
    /// Tool execution context
    ToolContext,
    /// Custom application context
    Custom(u16),
}

/// Context fabric for distributed context sharing
pub struct ContextFabric {
    /// Local context store
    local: DashMap<u128, ContextEntry>,
    /// Remote context locations
    remote: DashMap<u128, Vec<u64>>,
    /// Pending transfers
    transfers: DashMap<u128, TransferState>,
    /// Config
    config: ContextFabricConfig,
}

impl ContextFabric {
    /// Store context locally
    pub fn store(&self, chunk: ContextChunk);
    
    /// Retrieve context (local or remote)
    pub async fn get(&self, context_id: u128) -> Option<Vec<ContextChunk>>;
    
    /// Prefetch context from remote node
    pub async fn prefetch(&self, context_id: u128, from: u64);
    
    /// Replicate context to peer nodes
    pub async fn replicate(&self, context_id: u128, to: &[u64]);
    
    /// Evict old contexts
    pub fn gc(&self);
    
    /// Get context location hints
    pub fn locate(&self, context_id: u128) -> Vec<u64>;
}

#[derive(Clone)]
pub struct ContextFabricConfig {
    /// Max local storage bytes
    pub max_local_bytes: usize,
    /// Default TTL
    pub default_ttl_secs: u32,
    /// Replication factor
    pub replication_factor: u8,
    /// Prefetch lookahead
    pub prefetch_lookahead: u32,
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Local store | < 1µs |
| Local retrieve | < 1µs |
| Remote prefetch (1 hop) | < 10ms |
| Context replication | Linear with size |
| GC overhead | < 1% CPU |

---

## 4G — Distributed Load Balancing (LOAD-BALANCE)

### Objective
Route requests to optimal nodes based on:
- Capability matching
- Current load
- Proximity
- Affinity/anti-affinity rules

### Load Balancer

```rust
/// Load balancing strategy
#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum LoadBalanceStrategy {
    /// Round-robin (ignores load)
    RoundRobin,
    /// Least connections
    LeastConnections,
    /// Weighted by capacity
    WeightedCapacity,
    /// Latency-based
    LowestLatency,
    /// Capability-aware weighted
    CapabilityWeighted,
    /// Random
    Random,
    /// Consistent hashing (for affinity)
    ConsistentHash,
}

/// Load balancer with capability awareness
pub struct LoadBalancer {
    /// Strategy
    strategy: LoadBalanceStrategy,
    /// Capability index reference
    cap_index: Arc<CapabilityIndex>,
    /// Node load tracking
    load: DashMap<u64, NodeLoad>,
    /// Latency estimates
    latency: DashMap<u64, LatencyEstimate>,
    /// Consistent hash ring
    hash_ring: Option<HashRing>,
    /// Affinity rules
    affinity: Vec<AffinityRule>,
}

#[derive(Clone)]
pub struct NodeLoad {
    pub active_requests: u32,
    pub queue_depth: u32,
    pub cpu_percent: f32,
    pub memory_percent: f32,
    pub last_updated: Instant,
}

#[derive(Clone)]
pub struct LatencyEstimate {
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    pub samples: u32,
}

#[derive(Clone)]
pub struct AffinityRule {
    /// Rule ID
    pub id: String,
    /// Match requests with these tags
    pub request_tags: Vec<String>,
    /// Prefer nodes with these tags
    pub prefer_nodes: Vec<String>,
    /// Avoid nodes with these tags
    pub avoid_nodes: Vec<String>,
    /// Strength (0.0 = hint, 1.0 = hard requirement)
    pub strength: f32,
}

impl LoadBalancer {
    /// Select best node for request
    pub fn select(
        &self,
        requirement: &CapabilityRequirement,
        ctx: &RequestContext,
    ) -> Option<u64>;
    
    /// Report request completion (for load tracking)
    pub fn report_complete(&self, node_id: u64, latency_us: u32);
    
    /// Report request failure
    pub fn report_failure(&self, node_id: u64);
    
    /// Update node load
    pub fn update_load(&self, node_id: u64, load: NodeLoad);
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Node selection (RoundRobin) | < 50ns |
| Node selection (Weighted) | < 200ns |
| Node selection (CapabilityWeighted) | < 1µs |
| Load update | < 100ns |
| 10k nodes selection | < 10µs |

---

## 4H — Proximity Graph Integration (PINGWAVE++)

### Objective
Enhance Phase 3C pingwaves with capability information:
- Carry capability summaries in pingwaves
- Build proximity + capability graph
- Enable "find nearest node with GPU" queries

### Enhanced Pingwave

```rust
/// Enhanced pingwave with capability summary
#[repr(C)]
pub struct PingwavePlus {
    /// Base pingwave header (24 bytes)
    pub base: Pingwave,
    /// Capability summary flags (compact)
    pub cap_flags: CapabilityFlags,
    /// Load indicator (0-255)
    pub load: u8,
    /// Status
    pub status: NodeStatus,
    /// Reserved
    pub _reserved: [u8; 4],
}

bitflags! {
    /// Compact capability flags for pingwave
    pub struct CapabilityFlags: u32 {
        const HAS_GPU = 0b0000_0001;
        const HAS_TOOLS = 0b0000_0010;
        const HAS_MODELS = 0b0000_0100;
        const HIGH_MEMORY = 0b0000_1000;  // > 32GB
        const HIGH_BANDWIDTH = 0b0001_0000;  // > 1Gbps
        const LOW_LATENCY = 0b0010_0000;  // < 10ms to backbone
        const ACCEPTING = 0b0100_0000;  // Accepting new requests
        const DRAINING = 0b1000_0000;  // Draining
    }
}

/// Proximity + capability graph
pub struct ProximityCapGraph {
    /// Base graph from Phase 3C
    base: LocalGraph,
    /// Capability summaries
    cap_summaries: DashMap<u64, CapabilityFlags>,
    /// Load levels
    load_levels: DashMap<u64, u8>,
}

impl ProximityCapGraph {
    /// Find nearest node matching capability flags
    pub fn find_nearest(
        &self,
        required: CapabilityFlags,
        max_hops: u8,
    ) -> Option<(u64, u8)>;  // (node_id, hop_count)
    
    /// Find all nodes matching within hop radius
    pub fn find_all_matching(
        &self,
        required: CapabilityFlags,
        max_hops: u8,
    ) -> Vec<(u64, u8)>;
    
    /// Get capability-aware path
    pub fn path_with_caps(
        &self,
        dest: u64,
        required: CapabilityFlags,
    ) -> Option<Vec<u64>>;
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| PingwavePlus overhead | < 5% vs base |
| Find nearest (100 nodes) | < 10µs |
| Find nearest (1000 nodes) | < 100µs |
| Cap-aware path finding | < 1ms |

---

## 4I — Safety Envelope Enforcement

### Objective
Enforce hard safety limits that cannot be bypassed:
- Resource quotas
- Rate limits
- Content filtering hooks
- Audit logging
- Kill switches

### Safety Envelope

```rust
/// Safety envelope configuration
#[derive(Clone, Serialize, Deserialize)]
pub struct SafetyEnvelope {
    /// Unique envelope ID
    pub id: String,
    /// Resource limits
    pub resource_limits: ResourceEnvelope,
    /// Rate limits
    pub rate_limits: RateEnvelope,
    /// Content policies
    pub content_policies: Vec<ContentPolicy>,
    /// Audit configuration
    pub audit: AuditConfig,
    /// Kill switch state
    pub kill_switch: KillSwitch,
    /// Enforcement mode
    pub mode: EnforcementMode,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResourceEnvelope {
    /// Max concurrent requests
    pub max_concurrent: u32,
    /// Max tokens per request
    pub max_tokens: u32,
    /// Max memory per request (MB)
    pub max_memory_mb: u32,
    /// Max execution time (ms)
    pub max_time_ms: u32,
    /// Max total cost per hour
    pub max_cost_per_hour: f64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RateEnvelope {
    /// Requests per minute (global)
    pub global_rpm: u32,
    /// Requests per minute (per source)
    pub per_source_rpm: u32,
    /// Tokens per minute
    pub tokens_pm: u64,
    /// Burst allowance
    pub burst_multiplier: f32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ContentPolicy {
    pub id: String,
    pub check: ContentCheck,
    pub action: PolicyAction,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum ContentCheck {
    /// Block specific patterns
    BlockPatterns(Vec<String>),
    /// Require patterns
    RequirePatterns(Vec<String>),
    /// External filter hook
    ExternalHook { url: String, timeout_ms: u32 },
    /// Size limit
    MaxSize(usize),
}

#[derive(Clone, Serialize, Deserialize)]
pub enum PolicyAction {
    Block,
    Warn,
    Log,
    Transform { script: String },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KillSwitch {
    /// Globally disable all processing
    pub enabled: bool,
    /// Reason for kill switch
    pub reason: Option<String>,
    /// Triggered at
    pub triggered_at: Option<u64>,
    /// Auto-reset after (seconds)
    pub auto_reset_secs: Option<u32>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum EnforcementMode {
    /// Enforce all limits
    Enforce,
    /// Log violations but don't block
    AuditOnly,
    /// Disabled
    Disabled,
}

/// Safety enforcer (hot path)
pub struct SafetyEnforcer {
    envelope: RwLock<SafetyEnvelope>,
    /// Current resource usage
    usage: AtomicResourceUsage,
    /// Rate limiter
    rate_limiter: RateLimiter,
    /// Audit log
    audit_log: AuditLog,
}

impl SafetyEnforcer {
    /// Check if request is allowed (hot path)
    pub fn check(&self, req: &Request) -> Result<(), SafetyViolation>;
    
    /// Acquire resources for request
    pub fn acquire(&self, resources: ResourceClaim) -> Result<ResourceGuard, SafetyViolation>;
    
    /// Trigger kill switch
    pub fn kill(&self, reason: &str);
    
    /// Reset kill switch
    pub fn reset(&self);
    
    /// Get current usage stats
    pub fn usage(&self) -> UsageStats;
}

/// RAII guard for acquired resources
pub struct ResourceGuard {
    enforcer: Arc<SafetyEnforcer>,
    claim: ResourceClaim,
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        self.enforcer.release(&self.claim);
    }
}
```

### Audit Trail

```rust
/// Audit log entry
#[derive(Clone, Serialize)]
pub struct AuditEntry {
    pub timestamp_ns: u64,
    pub event_type: AuditEventType,
    pub source_node: Option<u64>,
    pub request_id: Option<u128>,
    pub details: serde_json::Value,
    pub outcome: AuditOutcome,
}

#[derive(Clone, Copy, Serialize)]
pub enum AuditEventType {
    RequestReceived,
    RequestAllowed,
    RequestBlocked,
    RateLimitHit,
    ResourceLimitHit,
    ContentPolicyViolation,
    KillSwitchTriggered,
    KillSwitchReset,
    EnvelopeUpdated,
}

#[derive(Clone, Copy, Serialize)]
pub enum AuditOutcome {
    Success,
    Blocked,
    Warning,
    Error,
}

/// Audit log with rotation
pub struct AuditLog {
    entries: ArrayQueue<AuditEntry>,
    /// Write to external sink
    sink: Option<Box<dyn AuditSink>>,
    config: AuditConfig,
}

pub trait AuditSink: Send + Sync {
    fn write(&self, entry: &AuditEntry);
    fn flush(&self);
}
```

### Benchmarks

| Metric | Target |
|--------|--------|
| Safety check (hot path) | < 50ns |
| Resource acquire | < 100ns |
| Rate limit check | < 50ns |
| Audit log write | < 200ns |
| Content pattern match | < 1µs per pattern |

---

## File Structure

```
src/
├── adapter/
│   └── bltp/
│       ├── mod.rs
│       ├── behavior/           # NEW: Phase 4 behavior plane
│       │   ├── mod.rs
│       │   ├── capability.rs   # 4A: CAP-ANN
│       │   ├── diff.rs         # 4B: CAP-DIFF
│       │   ├── metadata.rs     # 4C: NODE-META
│       │   ├── api.rs          # 4D: API-SCHEMA
│       │   ├── rules.rs        # 4E: DEVICE-RULES
│       │   ├── context.rs      # 4F: CTXT-FABRIC
│       │   ├── loadbalance.rs  # 4G: LOAD-BALANCE
│       │   ├── pingwave.rs     # 4H: PINGWAVE++
│       │   └── safety.rs       # 4I: Safety envelope
benches/
├── bltp.rs                     # Existing
└── behavior.rs                 # NEW: Phase 4 benches
```

---

## Integration Points

### With Phase 3
- 4H (PINGWAVE++) extends 3C (swarm.rs) Pingwave struct
- 4G (LOAD-BALANCE) uses 3A (router.rs) RoutingTable
- 4F (CTXT-FABRIC) uses 3B (proxy.rs) for multi-hop transfer
- 4A (CAP-ANN) builds on 3C (swarm.rs) Capabilities struct

### External Dependencies
- `jsonschema` crate for 4D API validation
- `bitflags` crate for 4H capability flags
- Existing `serde`, `dashmap`, `crossbeam` from Phase 1-3

---

## Success Criteria

### Phase 4A (CAP-ANN)
> "Nodes can announce and index 100k+ capability sets per second with sub-millisecond query latency."

### Phase 4B (CAP-DIFF)
> "Capability updates use <10% of the bandwidth of full announcements while maintaining consistency."

### Phase 4C (NODE-META)
> "Rich node metadata is queryable with microsecond latency, enabling smart routing decisions."

### Phase 4D (API-SCHEMA)
> "Node APIs are self-describing with JSON Schema validation adding <10µs overhead per request."

### Phase 4E (DEVICE-RULES)
> "Autonomy rules evaluate in <100ns, enabling nodes to enforce local policies at wire speed."

### Phase 4F (CTXT-FABRIC)
> "Context can be shared across nodes with minimal latency, enabling distributed inference and multi-agent coordination."

### Phase 4G (LOAD-BALANCE)
> "Capability-aware load balancing selects optimal nodes in <1µs with accurate load tracking."

### Phase 4H (PINGWAVE++)
> "'Find nearest GPU node' queries complete in <100µs across 1000-node networks."

### Phase 4I (SAFETY)
> "Safety envelopes enforce hard limits with <50ns overhead, with complete audit trails."

---

## Implementation Order

| Phase | Dependencies | Complexity |
|-------|--------------|------------|
| 4A CAP-ANN | Phase 3C (Capabilities) | Medium |
| 4B CAP-DIFF | 4A | Low |
| 4C NODE-META | 4A | Low |
| 4D API-SCHEMA | None | Medium |
| 4E DEVICE-RULES | None | Medium |
| 4F CTXT-FABRIC | Phase 3B (multi-hop) | High |
| 4G LOAD-BALANCE | 4A, 4C | Medium |
| 4H PINGWAVE++ | Phase 3C, 4A | Medium |
| 4I SAFETY | 4E | Medium |

**Recommended order**: 4A → 4B → 4C → 4E → 4D → 4I → 4G → 4H → 4F

Start with capability announcements (4A) as the foundation, then layer diffs (4B) and metadata (4C). Device rules (4E) and API schemas (4D) can be done in parallel. Safety (4I) builds on rules. Load balancing (4G) and enhanced pingwaves (4H) integrate capabilities. Context fabric (4F) is the most complex and should be last.

---

## Benchmark CLI

```bash
# Phase 4A
thenet bench cap-ann --nodes 10000 --queries 100000

# Phase 4B  
thenet bench cap-diff --changes-per-node 10 --nodes 1000

# Phase 4C
thenet bench node-meta --nodes 5000 --queries 50000

# Phase 4D
thenet bench api-schema --endpoints 100 --validations 100000

# Phase 4E
thenet bench device-rules --rules 100 --requests 1000000

# Phase 4F
thenet bench context-fabric --nodes 100 --context-size 1MB

# Phase 4G
thenet bench load-balance --nodes 1000 --strategy weighted

# Phase 4H
thenet bench pingwave-plus --nodes 1000 --queries 10000

# Phase 4I
thenet bench safety --checks 1000000 --policies 10
```
