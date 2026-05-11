# Behavior Plane

The semantic layer on top of transport. Nodes declare what they are, what they can do, and what they offer. Nine submodules provide capability discovery, API schemas, device autonomy rules, distributed tracing, load balancing, proximity-aware routing, and safety enforcement.

## Capability Announcements (CAP-ANN)

Nodes announce capabilities. Storage shape:

```rust
pub struct CapabilitySet {
    pub tags: HashSet<Tag>,
    pub metadata: BTreeMap<String, String>,
}
```

`Tag` is a typed enum over the four-axis ontology (`Hardware` / `Software` / `Devices` / `Dataforts`):

```rust
pub enum Tag {
    AxisPresent { axis: TaxonomyAxis, key: String },
    AxisValue   { axis: TaxonomyAxis, key: String, value: String, separator: AxisSeparator },
    Reserved    { prefix: String, body: String },   // scope:* / causal:* / fork-of:* / heat:*
    Legacy(String),                                  // pre-A.5 untyped strings
}
```

`HardwareCapabilities` / `SoftwareCapabilities` / `Vec<ModelCapability>` / `Vec<ToolCapability>` / `ResourceLimits` are *projections* of the tag set, lazily decoded via `caps.views()`. Encoding scheme: `hardware.cpu_cores=N` / `hardware.gpu` / `hardware.gpu.vram_mb=N` / `software.os=linux` / `software.model.0.id=...` / `hardware.limits.max_concurrent_requests=N`. Tool JSON-Schema strings (which can't safely round-trip through the tag wire format) live in `metadata` under `tool::<id>::input_schema` / `tool::<id>::output_schema`.

Wire format emits tags in sorted `Tag::to_string()` order — the `HashSet` keeps O(1) membership for in-memory lookups; the `serialize_with` hook flattens to a sorted `Vec` on the way out. Without this, two ends of a signed announcement round-trip would produce different bytes (HashSet iteration is process-local random) and the verifier would reject as `InvalidSignature`.

`CapabilityFilter` matches against a `CapabilitySet` — used by channel authorization, daemon placement, and API routing.

`CapabilityIndex` stores all known nodes' capabilities in a `DashMap` with secondary indexes for fast GPU/tool/tag queries.

**Benchmark:** Single tag filter in 10.5 ns, GPU check in 0.33 ns.

### Predicate AST + evaluator

For arbitrary boolean queries beyond `CapabilityFilter`, the substrate ships a typed `Predicate` AST in `behavior::predicate`. Variants: `Exists` / `Equals` / `NumericAtLeast` / `NumericAtMost` / `NumericInRange` / `SemverAtLeast` / `SemverAtMost` / `SemverCompatible` / `StringPrefix` / `StringMatches` / `MetadataExists` / `MetadataEquals` / `MetadataMatches` / `MetadataNumericAtLeast` / `And` / `Or` / `Not`. Authored via the `pred!` macro or builder helpers; evaluated against `EvalContext::new(&tags, &metadata)`.

```rust
let p = pred!(and [
    pred!(exists "hardware.gpu"),
    pred!(num_at_least "hardware.memory_mb", 65536.0),
    pred!(semver_compatible "software.runtime.python", "3.11.0"),
    pred!(metadata_equals "intent", "ml-training"),
]);
let ok = p.evaluate(&EvalContext::new(&tags, &metadata));
```

Predicates encode losslessly to a `net-where:` nRPC header pair (`predicate_to_rpc_header` / `predicate_from_rpc_headers`) so server-side filtering picks the right candidate without re-running the predicate per hop. Trace and per-corpus debug-report variants (`evaluate_with_trace`, `predicate_debug_report`) record each clause's verdict count and short-circuit decisions; `redact_metadata_keys` scrubs metadata-equality / -matches values before persisting the report.

### Capability validation

`validate_capabilities(caps)` returns a `ValidationReport` of `errors` (operator-must-fix: `UnknownAxis`, `TypeMismatch`, `IndexMalformed`) + `warnings` (forward-compat / hygiene: `UnknownKey`, `MetadataOversize`, `LegacyTag`). The validator runs against a canonical `AXIS_SCHEMA` baked in at substrate build time. Both lists are sorted by JSON-stringified entry so cross-binding fixture comparisons stay order-independent. Soft cap on metadata is 4 KB.

### Bloom-filter primitive

`behavior::bloom::BloomFilter` (`{ len_bits, k, bits: Vec<u64> }`) backs compact chain-tag membership probes via xxh3-128 double-hashing. ~1% FPR at 10 K items in ≤ 500 KB. Probe pattern: callers that match the bloom run a follow-up precise lookup (existing `causal:<hex>` tag membership) before issuing real reads — false positives become recoverable misses, false negatives are impossible by construction.

### CapabilityQuery trait

`behavior::query::CapabilityQuery` lifts five composable ops over `CapabilityIndex`: `filter` (predicate-driven candidate set), `match_axis` (axis-shaped tag scan), `aggregate` (per-key cardinality / numeric reductions), `traverse` (graph-style join over peer capability links), `nearest` (combine with proximity to score the top-K best matches). Implementations on `CapabilityIndex` are O(log n) for indexed predicates and O(n) for the residual scan.

### Scoped discovery (`scope:*` reserved tags)

Capability announcements gossip permissively across the mesh. To narrow *who sees what* at query time, providers tag their `CapabilitySet` with reserved `scope:*` tags. The `scope:*` namespace is owned by the discovery layer; user tags must not start with `scope:`.

| Tag                       | Meaning                                                                                          |
| ------------------------- | ------------------------------------------------------------------------------------------------ |
| _(no `scope:*` tag)_      | Global (default) — discoverable to every query that doesn't explicitly opt out.                  |
| `scope:global`            | Global (explicit form) — same effect as no tag.                                                  |
| `scope:subnet-local`      | Visible only to peers in the same subnet as the announcer.                                       |
| `scope:tenant:<id>`       | Visible to queries with `ScopeFilter::Tenant(<id>)` (and to global queries that aren't tenant-scoped). |
| `scope:region:<name>`     | Visible to queries with `ScopeFilter::Region(<name>)` (and to global queries that aren't region-scoped). |

A node may carry multiple `scope:tenant:*` / `scope:region:*` tags simultaneously. **Precedence:** `scope:subnet-local` dominates — when present, tenant/region tags are ignored by the resolver. Strictest scope wins.

Enforcement is **query-side only**. The wire format, forwarder logic, and gateway rules stay untouched — `find_nodes_by_filter_scoped(filter, scope)` evaluates `scope:*` tags as a post-filter on the index. Cross-tenant *routing* still flows freely; what changes is which peers a tenant-scoped query *returns*. See `docs/SCOPED_CAPABILITIES_PLAN.md` for the full design and the v3 deferred work (path-level enforcement, signed-scope, audience ACLs).

```rust
// Provider tags itself for tenant `oem-123`.
let caps = CapabilitySet::new()
    .add_tag("model:llama3-70b")
    .with_tenant_scope("oem-123");
mesh.announce_capabilities(caps).await?;

// Query that filters to only that tenant + globally-tagged peers.
let peers = mesh.find_nodes_by_filter_scoped(
    &CapabilityFilter::new().require_tag("model:llama3-70b"),
    &ScopeFilter::Tenant("oem-123"),
);
```

The `Visibility` enum on `ChannelConfig` is a separate concept — channel visibility is a *routing* concern enforced by `SubnetGateway::should_forward`. Capability scope is a *discovery* concern. They can be combined (a `SubnetLocal` channel that publishes only to subnet-local capability matches) but neither implies the other.

## Capability Diffs (CAP-DIFF)

`DiffEngine` computes minimal diffs when capabilities change, avoiding full re-announcement.

```rust
pub enum DiffOp {
    Added { key: String, value: String },
    Removed { key: String },
    Modified { key: String, old: String, new: String },
}

pub struct CapabilityDiff {
    pub node_id: u64,
    pub ops: Vec<DiffOp>,
    pub seq: u64,
}
```

Diffs are sequenced for ordering. The receiver applies ops incrementally to its local `CapabilityIndex`.

## Node Metadata (NODE-META)

`MetadataStore` tracks per-node metadata beyond capabilities: location, network tier, NAT type, topology hints.

```rust
pub struct NodeMetadata {
    pub node_id: NodeId,
    pub status: NodeStatus,           // Online, Degraded, Offline, Unknown
    pub location: Option<LocationInfo>,
    pub network_tier: NetworkTier,    // EdgeDevice, EdgeGateway, Cloud, Unknown
    pub nat_type: NatType,
    pub topology_hints: TopologyHints,
    pub last_seen: u64,
}
```

`MetadataQuery` supports filtering by status, region, network tier, and custom predicates.

## API Schema Registry (API-SCHEMA)

Nodes register API endpoints with typed schemas. Other nodes discover and validate calls against the schema.

```rust
pub struct ApiSchema {
    pub endpoints: Vec<ApiEndpoint>,
    pub version: ApiVersion,
}

pub struct ApiEndpoint {
    pub path: String,
    pub method: ApiMethod,
    pub parameters: Vec<ApiParameter>,
    pub response_type: Option<SchemaType>,
    pub description: String,
}
```

`ApiRegistry` indexes schemas by node, path, and capability. `ApiAnnouncement` broadcasts schema availability. `ApiQuery` discovers endpoints matching a path pattern or capability requirement.

## Device Autonomy Rules (DEVICE-RULES)

`RuleEngine` enforces local policies on a node. Rules are condition-action pairs evaluated against a `RuleContext`.

```rust
pub struct Rule {
    pub name: String,
    pub priority: Priority,
    pub condition: ConditionExpr,
    pub action: Action,
    pub rate_limit: Option<RateLimit>,
}

pub enum Action {
    Allow,
    Deny,
    Log { level: LogLevel, message: String },
    Alert { severity: AlertSeverity, message: String },
    Scale { direction: ScaleDirection, target: String },
    Custom(String),
}
```

`ConditionExpr` supports boolean logic (`And`, `Or`, `Not`) over `Condition` predicates with `CompareOp` comparisons (Eq, Ne, Gt, Lt, Gte, Lte, Contains, Matches).

`RuleSet` groups rules for batch evaluation. `RuleEngine` evaluates rules in priority order, returning the first matching `RuleResult`.

## Context Fabric (CTXT-FABRIC)

Distributed context propagation for cross-node tracing and correlation.

```rust
pub struct Context {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub trace_flags: TraceFlags,
    pub baggage: Baggage,
}

pub struct Span {
    pub span_id: SpanId,
    pub parent_id: Option<SpanId>,
    pub name: String,
    pub kind: SpanKind,       // Client, Server, Producer, Consumer, Internal
    pub status: SpanStatus,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
    pub start_time: u64,
    pub end_time: Option<u64>,
}
```

`ContextStore` manages active contexts with `ContextScope` for automatic cleanup. `PropagationContext` carries trace state across network boundaries. `Sampler` controls trace volume via `SamplingStrategy` (AlwaysOn, AlwaysOff, Ratio, RateLimited).

## Load Balancing (LOAD-BALANCE)

`LoadBalancer` distributes requests across endpoints using pluggable strategies.

```rust
pub enum Strategy {
    RoundRobin,
    LeastConnections,
    WeightedRoundRobin,
    Random,
    ConsistentHash,
}

pub struct Endpoint {
    pub id: u64,
    pub address: String,
    pub weight: u32,
    pub health: HealthStatus,
    pub metrics: LoadMetrics,
}
```

`Selection` returns the chosen endpoint with a `SelectionReason`. Health checks integrate with `FailureDetector` -- unhealthy endpoints are automatically excluded.

## Proximity Graph (PINGWAVE++)

`ProximityGraph` enhances the base `Pingwave` discovery with measured latency edges and capability-weighted routing.

```rust
pub struct ProximityNode {
    pub node_id: u64,
    pub capabilities: PrimaryCapabilities,
    pub subnet: SubnetId,
}

pub struct ProximityEdge {
    pub latency_us: u32,
    pub jitter_us: u16,
    pub loss_ratio: f32,
}
```

`EnhancedPingwave` extends base Pingwave with latency measurement. `ProximityConfig` tunes TTL, measurement intervals, and cleanup thresholds. The graph provides nearest-neighbor queries weighted by both latency and capability match.

## Safety Envelope Enforcement

`SafetyEnforcer` enforces resource limits, rate limits, content policies, and kill switches.

```rust
pub struct SafetyEnvelope {
    pub resource_envelope: ResourceEnvelope,
    pub rate_envelope: RateEnvelope,
    pub content_policy: ContentPolicy,
    pub kill_switch: Option<KillSwitchConfig>,
}
```

**Resource limits:** `ResourceEnvelope` caps CPU, memory, network, and storage per-node or per-daemon. `ResourceGuard` tracks claims and rejects requests that would exceed the envelope.

**Rate limits:** `RateEnvelope` enforces per-entity, per-channel, and global rate limits with configurable burst allowances.

**Content policy:** `ContentPolicy` with `ContentCheck` rules for payload validation.

**Kill switch:** `KillSwitchConfig` for emergency shutdown of specific daemons, channels, or entire nodes. Audit trail via `AuditEntry` with `AuditEventType` and `AuditOutcome`.

**Enforcement modes:** `EnforcementMode::Enforce` (reject violations), `EnforcementMode::Monitor` (log only), `EnforcementMode::Disabled`.

## Source Files

| File | Purpose |
|------|---------|
| `behavior/capability.rs` | `CapabilitySet`, `HardwareCapabilities`, `CapabilityIndex`, `CapabilityFilter`, `CapabilityViews` |
| `behavior/tag.rs` | `Tag`, `TagKey`, `TaxonomyAxis`, `AxisSeparator`, `RESERVED_PREFIXES` |
| `behavior/tag_codec.rs` | Round-trip codecs `*_to_tags` / `*_from_tags` for each axis projection |
| `behavior/predicate.rs` | `Predicate` AST, `EvalContext`, `pred!` macro, trace + debug-report aggregator |
| `behavior/required_capability.rs` | `RequiredCapability` + the `require!` / `require_axis!` / `require_axis_value!` macros |
| `behavior/schema.rs` | `validate_capabilities`, `ValidationReport`, `AXIS_SCHEMA`, `SchemaError`, `ValidationWarning` |
| `behavior/bloom.rs` | `BloomFilter` primitive (xxh3-128 double-hashing, serde-canonical wire) |
| `behavior/query.rs` | `CapabilityQuery` trait — `filter` / `match_axis` / `aggregate` / `traverse` / `nearest` |
| `behavior/diff.rs` | `CapabilityDiff`, `DiffEngine`, `DiffOp` |
| `behavior/metadata.rs` | `NodeMetadata`, `MetadataStore`, `MetadataQuery` |
| `behavior/api.rs` | `ApiRegistry`, `ApiSchema`, `ApiEndpoint`, validation |
| `behavior/rules.rs` | `RuleEngine`, `RuleSet`, `ConditionExpr`, `Action` |
| `behavior/context.rs` | `Context`, `ContextStore`, `Span`, `Sampler` |
| `behavior/loadbalance.rs` | `LoadBalancer`, `Strategy`, `Endpoint`, health |
| `behavior/placement.rs` | `StandardPlacement` scorer, custom-filter callback dispatcher |
| `behavior/proximity.rs` | `ProximityGraph`, `EnhancedPingwave`, latency edges |
| `behavior/safety.rs` | `SafetyEnforcer`, `ResourceEnvelope`, `KillSwitchConfig` |
