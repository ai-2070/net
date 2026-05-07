# Capability System — implementation plan

> Unified plan for The Warriors-release capability work: typed taxonomy (`hardware` / `software` / `devices` / `dataforts`), discovery primitive (`causal:` / `heat:` / `fork-of:` / `scope:` tag shapes plus metadata field), federated query primitives, and the generalized 5-axis `PlacementFilter` that Mikoshi consumes for daemon migration. Companion to [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) (which covers Phase 2 of The Warriors). Together they constitute the full Warriors release scope. Builds on the existing [`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md), [`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md), and [`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md) — does not replace them.

## Status

Design only. Activation gate: any one of the Warriors phases needs to land. The pieces here ship together because they compose against each other; shipping only some breaks the architectural symmetry the release depends on.

## Frame

The substrate today has capability tags as a flat opaque set. They work, but the namespace is starting to drift — `scope:`, multi-hop summaries, broadcast machinery, ad-hoc tag conventions. This plan turns the namespace into a **typed four-axis ontology** with two parallel surfaces:

- **Tag set** — fast set-membership (the existing `CapabilitySet.tags`), reorganized under typed prefixes (`hardware:`, `software:`, `devices:`, `dataforts:`)
- **Metadata field** — new `CapabilitySet.metadata: BTreeMap<String, String>` for richer key-value annotations not on the routing hot path

On top of that ontology, this plan ships **federated query primitives** (composable operators over the capability index) and **`PlacementFilter`** (a substrate-level trait that scores candidate nodes for placement of any artifact — chain, replica, or daemon — using a 5-axis filter). Mikoshi's existing daemon migration logic consumes `PlacementFilter` for migration target selection. Replica/fork/standby groups inherit the same primitive.

The result is **placement as a substrate primitive** alongside routing, capability discovery, and ACL — applied uniformly to data and compute, with one well-tested filter implementation backing every placement decision in the substrate.

## Why this exists

Four load-bearing reasons:

1. **The capability namespace is becoming a soup.** Without typed taxonomy, every new feature adds tags to the flat namespace and queries get progressively harder to reason about. Reorganizing now is meaningfully cheaper than reorganizing after Rebel Yell composes against an unstructured namespace.
2. **Mesh-level federated reads need primitives.** Today the capability index supports tag-filter queries (`find peers with tag X`). Future features (Dataforts greedy filter, MeshDB extension, replica discovery) need richer composition — joins, traversals, aggregates. Building primitives once is meaningfully cheaper than each feature inventing them.
3. **Placement is currently ad-hoc.** Mikoshi picks migration targets via single-node logic. Replica/fork/standby groups have their own placement code paths. Phase 1 of Rebel Yell would invent yet another. Consolidating into a single `PlacementFilter` trait shipped here means everything downstream composes the same primitive — and the architectural identity gains "placement" as a first-class concept.
4. **Mikoshi specifically benefits from the upgrade.** Daemon migration today is local-decision, capability-match-only. After this plan, Mikoshi consults the same 5-axis filter as data placement: scope + proximity + capability-preference (intent) + colocation + compute-availability. Daemons gravitate toward nodes that fulfill their work intent automatically; cohort-related daemons stay colocated; the substrate self-organizes around purpose.

## What ships

Six interlocking pieces:

1. **Typed taxonomy reorganization** — flat tag namespace becomes the four-axis ontology (`hardware`, `software`, `devices`, `dataforts`).
2. **Tag shapes for the discovery primitive** — `causal:`, `causal:tip_seq`, `causal:[range]`, `fork-of:`, `heat:` plus the existing `scope:`. Bloom-filter aggregation for nodes holding many tags.
3. **Metadata field on `CapabilitySet`** — new `BTreeMap<String, String>` for richer key-value annotations. Reserved keys (`intent`, `colocate-with`, `colocate-with-strict`, `priority`, `owner`); application-defined keys propagate as opaque pairs.
4. **Federated query primitives** — five operators (`filter`, `match`, `traverse`, `aggregate`, `nearest`) that compose into the user-facing query language Rebel Yell ships, plus the predicate language (numeric / semver / string / metadata predicates with boolean composition).
5. **`PlacementFilter` trait + `StandardPlacement` reference implementation** — substrate-level placement primitive that scores candidate nodes for any artifact using the 5-axis filter.
6. **Mikoshi integration** — `Mikoshi::select_migration_target` consults `PlacementFilter`. Legacy ad-hoc selection preserved as `LegacyPlacement` under a feature flag for one minor version.

What this doc does NOT ship (deferred):

- **Full MeshDB query language** — time-travel queries against historical chain ranges, full lineage-walk traversals, cross-chain joins. The primitives here are the foundation; the language layer parks until a workload demands it (Atomic Playboys candidate, per `RELEASE_ROADMAP.md`).
- **Live daemon migration without snapshot replay** — Mikoshi v2 territory. This plan integrates Mikoshi with `PlacementFilter` but doesn't change the migration mechanism itself.
- **Continuous rebalancing.** The placement filter scores at *placement decision time* (initial placement, replica election, daemon migration trigger). Continuous re-evaluation as capability tags drift is a separate concern (Atomic Playboys candidate — federated mesh-wide scheduler).
- **Encrypted metadata.** Metadata propagates as plaintext key-value pairs gated by the same `subscribe_caps` ACL as tags. Workload-specific encryption of sensitive metadata is a follow-up if a customer needs it.

---

## Design

### 1. Typed taxonomy

The four axes:

| Axis | Meaning | Example tags |
|---|---|---|
| `hardware` | What the node *can do* compute-wise. Objective, measurable. | `hardware.cpu_cores=24`, `hardware.gpu`, `hardware.gpu.vram_gb=80`, `hardware.ram_gb=512`, `hardware.nic_gbps=100`, `hardware.storage_tb=8` |
| `software` | What the node *currently runs*. Configurable. | `software.model:llama-3-70b-fp8`, `software.runtime:cuda-12.4`, `software.daemon:ollama-0.5.1`, `software.tool:ffmpeg` |
| `devices` | Custom semantic role tags. World-facing roles. | `devices.printer`, `devices.temperature-sensor`, `devices.brake-controller`, `devices.lidar`, `devices.pump`, `devices.valve` |
| `dataforts` | Storage capacity + hosted causal chains. The 4th category Rebel Yell adds. | `dataforts.has_chain:<origin_hash>`, `dataforts.free_storage_gb=200`, `dataforts.tier:hot` |

**Encoding.** Tags use a prefix convention: `<axis>.<key>` for boolean presence, `<axis>.<key>=<value>` for keyed presence, `<axis>:<typed-form>` for the dataforts subset that's pre-typed (`dataforts.has_chain:<hash>`, etc.). Prefix dispatch happens at parse time; queries route by axis.

**Migration from flat tags.** Existing tags without an axis prefix continue to work for one minor version (parsed as untyped legacy tags). New code emits with axis prefixes. After deprecation window, untyped tags log a warning. Hard-removal in the next major.

**Reserved prefixes.** `causal:`, `heat:`, `fork-of:`, `scope:` — plus the four axis prefixes — are reserved. Application code emitting tags with reserved prefixes is rejected via `CapabilityError::ReservedPrefix`. Pin in tests.

### 2. Tag shapes — discovery primitive

Five parsed shapes encoded as opaque `Tag` values inside `CapabilitySet.tags`. These are *cross-axis* — they don't fit into one of `hardware` / `software` / `devices` / `dataforts` because they describe properties of the artifact itself, not the node.

| Shape | Purpose |
|---|---|
| `causal:<32-byte hex of origin_hash>` | "I hold (or will serve) this chain" |
| `causal:<hex>:<tip_seq>` | "I hold this chain at least through `tip_seq`" |
| `causal:<hex>[<start>..<end>]` | "I hold this chain across the seq range" — used by Phase 6 (federated query, time-travel) |
| `fork-of:<parent_hex>` | "This chain forked from `parent_hex`" — for lineage / cohort queries |
| `heat:<chain_hex>=<reads_per_window>` | Heat counter; Phase 4 (Rebel Yell) consumes; absence means "not advertising" |

Plus the existing `scope:<label>` (per `SCOPED_CAPABILITIES_PLAN.md`) — set-membership for fleet scoping.

### 3. Metadata field

New field on `CapabilitySet`:

```rust
pub struct CapabilitySet {
    pub tags: HashSet<Tag>,                       // existing — set-membership filtering
    pub metadata: BTreeMap<String, String>,       // NEW — key-value annotations
}
```

The substrate doesn't interpret metadata values; applications and the placement filter do. The Kubernetes parallel: tags = labels (set-membership, scheduler-relevant); metadata = annotations (key-value, freeform per-artifact context).

**Reserved metadata keys** consumed by `PlacementFilter`:

| Key | Type | Meaning |
|---|---|---|
| `metadata.intent` | `String` | "What kind of work is this artifact for" — `ml-training`, `sensor-telemetry`, `billing-settlement`, etc. Drives capability-preference matching. |
| `metadata.colocate-with` | `String` (origin_hash hex) | "Place me near the node holding this chain" (soft preference, scoring boost) |
| `metadata.colocate-with-strict` | `String` (origin_hash hex) | "Refuse placement if target unavailable" (hard requirement) |
| `metadata.priority` | `String` | Optional — application-defined priority hint |
| `metadata.owner` | `String` | Optional — owning team / project |

Application-defined keys propagate through the substrate as opaque pairs. The placement filter ignores them; queries can match against them via the federated query layer.

**Why split tags vs. metadata.** Tags stay fast because they're set-membership over a bounded namespace (the bloom filter handles 10K+ chains in 500 KB). Metadata can be richer because it's not on the routing hot path; lookups happen during placement decisions, not per-routing-hop. Applications can put arbitrary keys in metadata without polluting the tag namespace shared across the substrate.

### 4. Bloom-filter aggregation

For nodes holding many `causal:` advertisements (and other reserved-prefix tags), enumeration becomes expensive. Add an optional `chain_bloom: Option<BloomFilter>` field on `CapabilitySet`.

- **Threshold.** Default 256 tags before switching to bloom-filter mode. Configurable via `CapabilityAnnouncementPolicy`.
- **Target sizing.** 10K chains in ≤ 500 KB at ≤ 1% false-positive rate.
- **Probe pattern.** Nodes that match the bloom probe with a follow-up precise `causal:<hex>` lookup before issuing a real read; false positives become recoverable misses, not correctness bugs.
- **Propagation cost.** ≤ 2× current capability-announcement budget under saturating tag growth. Pin via the existing announcement-budget regression test (the three-layer enforcement detailed in the Test strategy section).

**Hierarchical summarization at gateways** stays a separate, well-understood mechanism (per the README's "Scale" invariant): gateway nodes aggregate health, compress capability summaries, and propagate state for their subnet. A distant node observes the gateway and derives the rest. The bloom filter handles per-node many-tag aggregation; gateway summarization handles cross-subnet aggregation. Two clear mechanisms, both familiar to operators.

**Future extension (parked).** A more general `CapabilityFold` trait — applying the CortEX fold pattern to capability announcements — would unify bloom-filter aggregation, gateway summarization, prefix-based grouping, and custom application aggregations into one composable primitive. Worth doing if/when a second aggregation type emerges (likely candidates: MeshDB extension's time-travel/lineage folds, the federated mesh-wide scheduler's compute-availability folds — both Atomic Playboys territory). Until then, bloom filter + gateway summarization is concrete enough; defer the abstraction.

### 5. Re-announcement throttle + withdrawal

**Re-announcement throttle.** Default policy: emit on whichever fires first — Δ`tip_seq` ≥ 1024 events OR Δt ≥ 10 s. Configurable per channel via `ChannelConfig::chain_announcement: Option<ChainAnnouncementPolicy>`. The chain itself self-verifies on actual read, so the advertisement is a discovery hint, not a security primitive — being slightly stale is recoverable.

**Withdrawal.** Capability index already supports tag removal. Producers wire it into:

- Greedy LRU evict (Rebel Yell Phase 1)
- Replica drop (`REDEX_DISTRIBUTED_PLAN.md` ReplicationCoordinator)
- Graceful daemon shutdown
- Mikoshi migration target switching (old node withdraws; new node announces)

### 6. Federated query primitives

Five composable operators over the capability index. Live in `behavior::capability::query`. **These are primitives, not a query language.** A user-facing language sits on top in Rebel Yell or as an Atomic Playboys candidate; this plan ships the primitives.

```rust
pub trait CapabilityQuery {
    /// Scan the capability index for entries matching a predicate.
    /// Returns matching (NodeId, CapabilitySet) pairs.
    fn filter(&self, predicate: TagPredicate) -> impl Iterator<Item = (NodeId, &CapabilitySet)>;

    /// Type-aware match against a specific axis.
    /// Returns matching (NodeId, ...) for tags under that axis matching the value.
    fn match_axis(&self, axis: TaxonomyAxis, key: &str, value: Option<&str>)
        -> impl Iterator<Item = (NodeId, &CapabilitySet)>;

    /// Walk capability-tag edges (e.g., `fork-of:` parent links) recursively.
    /// Returns the chain of edges from start_tag back to terminal ancestor or up to max_depth.
    fn traverse(&self, start_tag: &Tag, edge: EdgeKind, max_depth: u32)
        -> Vec<(NodeId, Tag)>;

    /// Counts and aggregations over filter results.
    /// No fold required for capability-level aggregates.
    fn aggregate<A>(&self, predicate: TagPredicate, agg: A) -> A::Output where A: Aggregator;

    /// Top-N candidates by proximity weighting.
    fn nearest(&self, predicate: TagPredicate, n: usize) -> Vec<(NodeId, &CapabilitySet, Distance)>;
}
```

**Composability.** Operators compose. The user-facing query Rebel Yell ships:

```
hardware.gpu AND software.model:llama-3-70b AND dataforts.has_chain:Y AND proximity < 50ms
```

decomposes to:

```rust
let candidates = capability_query
    .match_axis(Hardware, "gpu", None)
    .filter(|(_, caps)| caps.tags.contains(&Tag::parse("software.model:llama-3-70b")?))
    .filter(|(_, caps)| caps.tags.contains(&Tag::parse("dataforts.has_chain:Y")?));
let nearest = capability_query.nearest_within(candidates, Duration::from_millis(50));
```

No new operators needed for the dual-axis cross-axis queries. They're compositions of the five primitives.

**Where the operators run.** Local-only (against the local capability-index view) by default. For *federated* execution, `nearest` and `match_axis` consult the proximity graph to route sub-queries to nodes nearer the target chain or capability tag. Federation is opt-in via a `Federated` wrapper trait; local-only is the default to avoid surprising round-trips.

#### 6a. Predicate language

`TagPredicate` (used by `filter`, `aggregate`, `nearest`) is a small composable AST. It expresses **existence checks, equality, numeric comparisons, semver comparisons, string patterns**, and **boolean composition**. This is what makes "find nodes with > 32 GB VRAM running vLLM ≥ 0.5.0" a first-class query rather than a string-matching workaround.

```rust
pub enum Predicate {
    // Existence — does this tag exist on the node?
    Exists(TagKey),

    // Equality — exact value match
    Equals(TagKey, String),

    // Numeric comparisons — parse the tag's value as f64, compare
    NumericAtLeast(TagKey, f64),       // hardware.gpu.vram_gb >= 32
    NumericAtMost(TagKey, f64),        // hardware.cpu_cores <= 16
    NumericInRange(TagKey, f64, f64),  // hardware.ram_gb in 64..512

    // Semver comparisons — parse the tag's value as semver::Version, compare
    SemverAtLeast(TagKey, semver::Version),     // software.daemon:vllm >= 0.5.0
    SemverAtMost(TagKey, semver::Version),
    SemverCompatible(TagKey, semver::VersionReq), // software.runtime:cuda ~= 12 (any 12.x)

    // String patterns
    StringPrefix(TagKey, String),       // software.daemon:ollama (any version)
    StringMatches(TagKey, regex::Regex), // software.model matches ^llama-3-.*-fp8$

    // Metadata predicates (read from CapabilitySet.metadata, not from tags)
    MetadataExists(String),                          // metadata.intent exists
    MetadataEquals(String, String),                  // metadata.owner == "team-x"
    MetadataMatches(String, regex::Regex),
    MetadataNumericAtLeast(String, f64),

    // Boolean composition
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
}

pub struct TagKey {
    /// Axis (Hardware, Software, Devices, Dataforts, or CrossAxis for reserved prefixes)
    pub axis: TaxonomyAxis,
    /// Key path within the axis, e.g. "gpu.vram_gb" or "daemon:vllm"
    pub key: String,
}
```

**Tag value parsing.** Tags are stored as opaque strings; the predicate evaluator parses values lazily based on which predicate is applied:

- `NumericAtLeast` / `AtMost` / `InRange` → parse `tag.value()` as `f64`; predicate fails (returns `false`) if unparseable
- `SemverAtLeast` / `AtMost` / `Compatible` → parse `tag.value()` as `semver::Version`; predicate fails if unparseable. For tags with embedded version strings (`software.daemon:vllm=0.5.3`), the value-side of the `=` is the parsed string. For prefix-encoded versions (`software.daemon:vllm-0.5.3`), the parser strips the prefix before parsing.
- `StringPrefix` / `StringMatches` → straightforward string operations on `tag.value()`

**Tag value encoding conventions.** Two formats both supported:

- `<axis>.<key>=<value>` — recommended for numeric and semver values where the predicate evaluator parses the value cleanly: `hardware.gpu.vram_gb=80`, `software.daemon:vllm=0.5.3`
- `<axis>.<key>:<value>` — for keyed presence where the value is a name or identifier: `software.model:llama-3-70b-fp8`, `dataforts.has_chain:abc123...`

The parser accepts both; predicates work against either. Convention: `=` for "this is a quantity I'll compare numerically/semantically"; `:` for "this is a name I'll match exactly or by prefix."

**Convenience constructors / `pred!` macro.** The boilerplate of constructing `Predicate::NumericAtLeast(TagKey { axis: Hardware, key: "gpu.vram_gb".into() }, 32.0)` gets a small declarative macro:

```rust
// All equivalent to NumericAtLeast(TagKey { Hardware, "gpu.vram_gb" }, 32.0)
let p = pred!("hardware.gpu.vram_gb >= 32");
let p = pred!(hardware.gpu.vram_gb >= 32);
let p = Predicate::numeric_at_least("hardware.gpu.vram_gb", 32.0);
```

The macro parses a small DSL (the same shape as the user-facing query language); the `numeric_at_least` / `semver_at_least` constructors give programmatic construction without macro magic.

**Real query examples:**

```rust
// "Find nodes with > 32 GB VRAM running vLLM >= 0.5.0"
let p = pred!(hardware.gpu.vram_gb > 32 AND software.daemon:vllm >= 0.5.0);
let candidates = capability_query.filter(p).nearest(10);

// "Find nodes that fulfill ml-training intent (GPU + ≥ 24GB VRAM) AND have CUDA ≥ 12"
let p = pred!(hardware.gpu AND hardware.gpu.vram_gb >= 24 AND software.runtime:cuda ~= 12);

// "Find sensor nodes (any device tag) within 50ms with at least 100 GB free storage"
let p = pred!(devices.* AND dataforts.free_storage_gb >= 100);
let candidates = capability_query.nearest_within(p, Duration::from_millis(50));

// "Find any operator-tagged node owned by team-x and currently leading < 30% of channels"
let p = pred!(metadata.owner == "team-x" AND metadata.leadership_concentration < 0.30);
```

**`IntentRegistry` requirements use the same predicate language.**

```rust
let intents = IntentRegistry::defaults()
    .register("ml-training", vec![
        pred!(hardware.gpu),
        pred!(hardware.gpu.vram_gb >= 24),
    ])
    .register("ml-training-large", vec![
        pred!(hardware.gpu),
        pred!(hardware.gpu.vram_gb >= 80),  // H100/H200 tier
    ])
    .register("inference-vllm", vec![
        pred!(hardware.gpu),
        pred!(software.daemon:vllm >= 0.5.0),
    ])
    .register("inference-ollama", vec![
        pred!(software.daemon:ollama >= 0.3.0),
    ])
    .register("sensor-telemetry", vec![
        pred!(devices.*),  // any device tag
    ])
    .register("billing-settlement", vec![
        pred!(hardware.cpu_cores >= 4),
        pred!(software.daemon:postgres >= 14.0),
    ]);
```

The intent registry is just predicate composition; everything queryable via the query API is also expressible as an intent requirement.

**Performance considerations.**

- Existence and string-equality predicates are O(1) bloom-filter / hash-set checks on the tag set
- Numeric / semver predicates require parsing the tag value; cache parsed values per `(NodeId, TagKey)` for repeat evaluation in `aggregate` / `nearest` operators
- Regex predicates are slowest; document expected use only at config / rare-query time, not in hot paths
- `pred!` macro at parse time validates the DSL; runtime evaluation is purely the parsed AST

### 7. `PlacementFilter` trait + `StandardPlacement` reference impl

Substrate-level placement primitive. Lives in `behavior::placement`.

```rust
pub trait PlacementFilter: Send + Sync {
    /// Score a candidate node for placement of an artifact.
    /// Returns `None` if the node is ineligible (hard constraint failed);
    /// returns `Some(score)` where higher = better fit. Score range conventionally [0.0, 1.0].
    fn placement_score(&self, target: &NodeId, artifact: &Artifact<'_>) -> Option<f32>;
}

pub enum Artifact<'a> {
    Chain { origin_hash: [u8; 32], capabilities: &'a CapabilitySet },
    Replica { channel: &'a ChannelName, capabilities: &'a CapabilitySet },
    Daemon { daemon_id: [u8; 32], required: &'a CapabilitySet, optional: &'a CapabilitySet },
}

pub struct StandardPlacement {
    pub scope_filter: Option<Vec<ScopeLabel>>,
    pub proximity_max_rtt: Option<Duration>,
    pub intent_match: IntentMatchPolicy,
    pub colocation_policy: ColocationPolicy,
    pub resource_axis: ResourceAxis,
    pub metadata_keys: PlacementMetadataKeys,
    pub anti_affinity: AntiAffinityConfig,
}

pub enum ResourceAxis {
    Storage,         // Chain / Replica artifacts — uses dataforts.free_storage tags
    Compute,         // Daemon artifacts — uses hardware.cpu_cores, hardware.ram_gb, hardware.gpu.vram_gb
    Both,            // Replicated daemons; weighted average
}

pub struct PlacementMetadataKeys {
    pub intent: String,                // default "intent"
    pub colocate_with: String,         // default "colocate-with"
    pub colocate_with_strict: String,  // default "colocate-with-strict"
}

pub struct AntiAffinityConfig {
    /// Penalize nodes already leading > threshold % of channels in local view.
    pub leadership_concentration_threshold: f32,  // default 0.30
    pub leadership_concentration_penalty: f32,    // default 0.4
}

pub enum IntentMatchPolicy {
    AnyOfLocalCapabilities,    // node fulfills any intent it has capability for
    StrictMatch(IntentRegistry),
    Custom(Box<dyn Fn(&str, &CapabilitySet) -> bool + Send + Sync>),
}

pub enum ColocationPolicy {
    Ignore,
    SoftPreference,    // boost score on affinity match
    StrictRequired,    // refuse placement unless target chain is local
}
```

**`StandardPlacement` evaluates 5 axes:**

1. **Scope** — `scope:` tag set-membership match between artifact and target node (fast bloom-filter check).
2. **Proximity** — RTT bound via the existing proximity graph.
3. **Capability-preference (intent)** — `metadata.intent` value mapped to required capabilities via the `intent → required capabilities` lookup table; target must include all required.
4. **Colocation** — `metadata.colocate-with` / `metadata.colocate-with-strict` resolved against target's local holdings.
5. **Resource-availability** — varies by `ResourceAxis`:
   - `Storage` → free storage capacity advertised via `dataforts.free_storage_gb` tag
   - `Compute` → free compute capacity (CPU cores, available RAM, GPU/VRAM)
   - `Both` → weighted average

Plus an anti-affinity term that penalizes nodes already leading > 30% of channels (prevents leadership concentration; reused by `REDEX_DISTRIBUTED_PLAN.md`'s replica election).

**Intent registry.** A small lookup table `adapter::net::placement::intent::IntentRegistry`:

```rust
pub struct IntentRegistry {
    map: BTreeMap<String, Vec<RequiredCapability>>,
}

impl IntentRegistry {
    pub fn defaults() -> Self {
        let mut m = BTreeMap::new();
        m.insert("ml-training".into(), vec![require!("hardware.gpu"), require!("hardware.gpu.vram_gb >= 24")]);
        m.insert("sensor-telemetry".into(), vec![require_axis!("devices")]);  // any device tag
        m.insert("billing-settlement".into(), vec![require!("hardware.cpu_cores >= 4"), require!("software.daemon:postgres")]);
        m.insert("inference".into(), vec![require!("hardware.gpu"), require_axis_value!("software", "model")]);
        // ... more defaults
        Self { map: m }
    }

    pub fn register(&mut self, intent: String, required: Vec<RequiredCapability>) { ... }
}
```

Applications can register custom intents via `IntentRegistry::register()`.

### 7a. Scope-based latency attraction

The `StandardPlacement` proximity axis already supports "must be within RTT X" as a hard threshold. A small extension turns this into **positive latency concentration toward flagship nodes** — daemons attract themselves toward physical / topological locations marked by scope tags.

**Use cases:** NYSE / CME / LSE / Eurex trading floors (microsecond-class RTT to matching engines), Equinix datacenter peering points (NY4, LD4, FR2, TY3), TSMC fab inspection-image clusters, oil & gas drilling-site clusters, autonomous-vehicle metro-edge POPs, live-event venue networks, anywhere physical proximity to a specific location dominates placement value.

**Mechanism (reuses existing primitives — no new abstractions).**

A flagship node (or set of nodes) at the target location carries a scope tag — e.g., `scope:nyse-trading-floor`. Multiple machines in the NYSE rack space all carry the same tag. The proximity graph already measures RTT between any two nodes; the capability index already routes by tag.

A daemon that needs concentration toward that location declares:

```
metadata.attract-to-scope:nyse-trading-floor
metadata.attract-budget:100us             # max acceptable RTT to the flagship
metadata.attract-fallback-scopes:equinix-ny4,us-east-region   # optional ladder
```

The placement-filter extension scores candidate nodes:

```rust
fn attract_score(candidate: &NodeId, target_scope: &ScopeLabel, budget: Duration) -> f32 {
    if candidate carries target_scope:
        return 1.0;  // co-located, perfect concentration
    let rtt = proximity_graph.nearest_rtt(candidate, |n| n.tags.contains(target_scope));
    if rtt > budget:
        return 0.0;  // out of budget
    1.0 - (rtt.as_nanos() as f32 / budget.as_nanos() as f32)
}
```

This becomes another input to `StandardPlacement::placement_score`, multiplied with the other axes. A daemon that requires GPU + scope:prod + intent:trading-engine + attract-to-scope:nyse-trading-floor + ≥ 80 GB free VRAM lands on the highest-scoring intersection of all five constraints.

**Why this is genuinely simple:**

- No new tag vocabulary — `scope:` already exists; flagship nodes just carry the appropriate scope.
- No new abstraction layer — no "anchor tags," no parallel concept; reuses what's already in the substrate.
- No new placement-filter axis — extends the existing proximity axis from "hard threshold" to "soft scoring against scope-tagged anchors."
- Multiple flagships per location (multiple nodes carrying the same scope) handled automatically — `nearest_rtt` picks whichever NYSE-scoped node is closest.
- Hierarchical fallback (`metadata.attract-fallback-scopes`) lets the daemon express "ideally Wall Street, acceptable Manhattan, fallback NYC" using the same primitive.

**Implementation cost.**

A few hundred lines on top of the existing `StandardPlacement`:

- Read `metadata.attract-to-scope` and `metadata.attract-budget` from the artifact's metadata.
- Use the existing capability-index lookup to find nodes carrying the target scope.
- Use the existing proximity graph to compute the nearest-RTT.
- Compute the score (~10 lines of math).
- Combine with the existing axes via the existing multiplicative composition.

Test surface adds: scoping correctness (candidate carrying target scope scores 1.0); budget cutoff (out-of-budget scores 0); fallback ladder (next scope used when primary unavailable); composition with other axes (a daemon requiring GPU AND attract-to-NYSE picks NYSE-scoped GPU nodes when available, falls back gracefully when not).

**Effort:** ~1-2 days of focused work added to Phase F. Most of the work is testing — the actual logic is small because all the substrate's primitives are already in place.

### 8. Mikoshi integration

Mikoshi today selects migration targets via single-node logic (capability match only, no scope / proximity / intent / colocation filtering). After this phase, Mikoshi consults `PlacementFilter` for migration target selection.

```rust
impl Mikoshi {
    pub fn select_migration_target(
        &self,
        daemon: &Daemon,
        filter: &dyn PlacementFilter,
    ) -> Option<NodeId> {
        let artifact = Artifact::Daemon {
            daemon_id: daemon.id,
            required: &daemon.required_capabilities,
            optional: &daemon.optional_capabilities,
        };

        self.candidate_nodes()
            .filter_map(|node| {
                filter
                    .placement_score(&node, &artifact)
                    .map(|score| (node, score))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
            .map(|(node, _)| node)
    }
}
```

**Feature flag for backward compatibility.** Behind `mikoshi-placement-v2` feature flag (on-by-default in `ai2070-net`; one minor version of compatibility window). The legacy single-node-logic path is preserved as `Mikoshi::select_migration_target_legacy` and made available via `LegacyPlacement` impl of `PlacementFilter`. Operators with custom Mikoshi placement logic get one minor version to migrate.

**Replica/fork/standby groups inherit.** Same primitive applies:

```rust
impl ReplicaGroup {
    fn select_member_node(&self, filter: &dyn PlacementFilter) -> Option<NodeId> {
        // identical structure to Mikoshi::select_migration_target
        // Artifact::Daemon variant; uses the daemon's required/optional capabilities
    }
}

impl StandbyGroup {
    fn select_promotion_target(&self, filter: &dyn PlacementFilter) -> Option<NodeId> {
        // same; picks highest-scoring surviving member on leader-loss
    }
}
```

The filter is the universal placement primitive; group machinery just *uses* it.

### 9. Daemon-side capability authoring

Daemons need to declare what capabilities they require (`required: CapabilitySet`) and what they prefer (`optional: CapabilitySet`). Today this is implicit in the daemon's code; this plan adds explicit declaration:

```rust
impl MeshDaemon for InferenceDaemon {
    fn required_capabilities(&self) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.tags.insert(Tag::parse("hardware.gpu").unwrap());
        caps.tags.insert(Tag::parse("hardware.gpu.vram_gb >= 80").unwrap());
        caps.metadata.insert("intent".into(), "ml-training".into());
        caps
    }

    fn optional_capabilities(&self) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.tags.insert(Tag::parse("hardware.gpu.architecture:hopper").unwrap());  // prefer H100/H200
        caps
    }

    // ... existing process / snapshot / restore methods unchanged
}
```

Backward compat: daemons that don't override `required_capabilities` / `optional_capabilities` get empty defaults (any node accepts placement).

---

## Phasing

Eight phases in dependency order:

### Phase A — Typed taxonomy migration (1 week)

- Add `TaxonomyAxis` enum (`Hardware`, `Software`, `Devices`, `Dataforts`).
- Tag parser recognizes axis-prefixed shapes; legacy untyped tags parse with deprecation warning.
- Existing capability-emitting code paths emit with axis prefixes.
- Documentation update — `CAPABILITIES.md` adds the four-axis model.

### Phase B — Tag shapes for discovery (3 days)

- Extend `Tag` parser to recognize `causal:`, `causal:tip_seq`, `causal:[range]`, `fork-of:`, `heat:` shapes.
- High-level helpers on `Mesh`:
  - `Mesh::announce_chain(origin_hash, tip_seq)`
  - `Mesh::announce_chain_range(origin_hash, start, end)`
  - `Mesh::withdraw_chain(origin_hash)`
  - `Mesh::find_chain_holders(origin_hash) -> Vec<NodeId>` — wraps the existing capability-index query, nearest-first by proximity.
- Reserved-prefix enforcement on user tags (`Tag::reserved_prefix()` check).

### Phase C — Metadata field on `CapabilitySet` (3 days)

- Add `metadata: BTreeMap<String, String>` field.
- Wire into capability-announcement serialization end-to-end (announce → propagate → decode).
- Reserved-key enforcement on the standard set (`intent`, `colocate-with`, etc.); warning-then-reject for unrecognized reserved-prefix keys.
- Bindings: `CapabilitySet::metadata` round-trips through Node, Python, Go, C bindings (mostly serde plumbing).

### Phase D — Bloom-filter aggregation (1 week)

- `BloomFilter` type and serialization.
- `CapabilitySet::chain_bloom: Option<BloomFilter>` field.
- Threshold logic: switch to bloom-mode at 256 tags; revert below.
- Probe pattern in lookup paths: `Mesh::find_chain_holders` does precise lookup after bloom probe.
- Propagation budget regression test (the three-layer enforcement detailed in the Test strategy section).

### Phase E — Federated query primitives (2 weeks)

- New module `behavior::capability::query` with the `CapabilityQuery` trait + reference implementations.
- Five operators: `filter`, `match_axis`, `traverse`, `aggregate`, `nearest`.
- Local-only execution by default; `Federated` wrapper trait for opt-in cross-node execution.
- Composability tests: nested operator chains produce correct results; federation is transparent to composition.
- No user-facing query language — primitives only. (User language is Atomic Playboys territory.)

### Phase F — `PlacementFilter` trait + `StandardPlacement` (1 week)

- New module `behavior::placement` with the `PlacementFilter` trait + `Artifact` enum + `StandardPlacement` reference impl.
- `IntentRegistry` with default mappings + extensibility.
- `IntentMatchPolicy` and `ColocationPolicy` definitions.
- `AntiAffinityConfig` with the leadership-concentration penalty (consumed by `REDEX_DISTRIBUTED_PLAN.md`).
- Reference test suite: each axis individually scored; cross-axis composition matches the product of single-axis scores.

### Phase G — Mikoshi integration (1 week)

- `Mikoshi::select_migration_target` consults `PlacementFilter`.
- Legacy fallback as `Mikoshi::select_migration_target_legacy` + `LegacyPlacement` filter.
- Feature flag `mikoshi-placement-v2` (on-by-default).
- `MeshDaemon` trait extended with `required_capabilities()` / `optional_capabilities()` (empty defaults for backward compat).
- Replica/fork/standby groups extended to use `PlacementFilter` for placement.
- Tests:
  - Daemon with `hardware.gpu` requirement migrates only to GPU nodes.
  - Daemon with `metadata.intent: "sensor-telemetry"` migrates to nodes with `devices.*` tags.
  - Daemon with `metadata.colocate-with: <chain>` migrates to the node holding that chain.
  - Cross-axis: daemon with multiple constraints lands at the intersection.

### Phase H — Bindings (1 week, parallelisable)

- `CapabilitySet::metadata` exposed in Node + Python + Go + C bindings.
- New tag-shape helpers (`Mesh::announce_chain` etc.) exposed across bindings.
- `PlacementFilter` callable from bindings (application-implemented filters cross binding boundary via callback interface — same pattern as `BlobAdapter`).
- `IntentRegistry::register` exposed for custom-intent registration.

**Total: 5–7 focused weeks.** Phases A–D sequence; E builds on D; F builds on A–C; G builds on F; H parallelises with C-H. Single engineer can serialise to ~7 weeks; with parallelism, drops to ~5 weeks.

---

## Test strategy

### Unit

- **Taxonomy parsing.** Each axis prefix recognized; ambiguous shapes rejected; legacy-untyped tags parse with deprecation warning.
- **Tag shape parsing.** `causal:`, `causal:tip_seq`, `causal:[range]`, `fork-of:`, `heat:` all round-trip through serialize/deserialize.
- **Reserved prefix enforcement.** User-emitted tags with reserved prefixes return `CapabilityError::ReservedPrefix`.
- **Bloom-filter behavior.** False-positive rate at 10K chains ≤ 1% at 500 KB; threshold-switch correctness.
- **Metadata round-trip.** Reserved keys recognized; application keys propagate as opaque pairs.
- **Each query operator.** Filter / match / traverse / aggregate / nearest — round-trip with synthetic capability sets.
- **`StandardPlacement` per-axis.** Each axis evaluated alone (others disabled) returns expected scores.

### Integration

- **End-to-end announcement.** 4-node mesh, 1 publisher, 3 observers. Publisher creates chain → observers' indexes converge within heartbeat interval; chain-hash matches across nodes.
- **Bloom aggregation under saturation.** Node creates 5K chains; assert announcement bandwidth bounded ≤ 2× baseline; assert observer queries return correct results via probe-then-precise pattern.
- **Federated query.** 8-node mesh distributed across 3 proximity zones. Query for "all nodes with `hardware.gpu`" issued from zone A returns all GPU nodes regardless of zone, ordered by proximity.
- **Placement filter cross-axis composition.** Daemon with `intent: ml-training` AND `scope:experiment-A` AND `colocate-with: <dataset>`. Asserts placement lands on the node intersecting all three constraints, not on any node satisfying just one.
- **Mikoshi integration round-trip.** Spawn daemon on node A with `required: hardware.gpu`. Trigger migration. Assert daemon lands on a GPU-having node B per `PlacementFilter` scoring.
- **Replica group placement.** 5-node mesh; create replica group of size 3; assert members spread per `StandardPlacement` anti-affinity penalty (no single node carries > 1 replica unless forced).

### Property

- **Score composability.** For any artifact A, `StandardPlacement` with axes X∪Y enabled returns score equal to the product of scores with X enabled and Y enabled (modulo the anti-affinity term, which is additive). Generated via property-based testing across random configurations.
- **Withdrawal idempotency.** `Mesh::withdraw_chain(X)` followed by another withdrawal is a no-op; the capability index reaches the same state.
- **Re-announcement throttle bounds.** Under any sequence of `tip_seq` advances, total re-announcements ≤ ⌈(latest_seq - initial_seq) / 1024⌉ + ⌈total_time / 10s⌉.

### Performance

- **Tag-parse path.** Per-tag parse ≤ 100 ns (existing tag-parse benchmark, regression-pinned).
- **Capability-announcement budget — three-layer enforcement.** Wire-budget control is enforced at three layers: a hard test pin in CI, a soft per-node budget that logs a warning in production, and a hard runtime cap that refuses to emit beyond the limit.

  **Layer 1 — Hard test pin (CI):** total announcement bytes ≤ 2× the pre-Warriors baseline at saturating capability emission. The existing announcement-budget regression test is extended to include the new `metadata` field's contribution to the wire format. Adding metadata to a `CapabilitySet` does NOT silently double the propagation cost — the test fails fast on regression. Specifically:
  - Test workload: a 16-node mesh with each node emitting `CapabilitySet` advertisements at the saturating throttle rate (1024-event burst then 10 s steady-state, repeated for 60 s).
  - Baseline measurement (pre-Warriors): bytes/sec across all relay paths under that workload, no metadata field.
  - Warriors-aware measurement: same workload with metadata at the soft cap (4 KB per CapabilitySet).
  - Pin: Warriors-aware ≤ 2× baseline. Bloom-filter aggregation MUST kick in at the 256-tag threshold to keep the bound; without it, ratio exceeds 2× at high tag counts and the test fails.

  **Layer 2 — Soft per-node budget (production warning):** each node measures its own outbound announcement bytes/sec. When the rate crosses the soft threshold (default 75% of the hard limit), the node emits a structured warning log:
  ```
  WARN dataforts.announcement_budget node=<id> bytes_per_sec=<n> soft_threshold=<m> reason=<bloom_inactive | metadata_growth | tag_burst | unknown>
  ```
  - Hysteresis: the warning fires at most once per 60 s per node to avoid log spam.
  - Reason is best-effort — the budget enforcer reports likely culprits (bloom filter inactive when tag count > 256; metadata field exceeding 50% of total announcement size; sustained tag-burst above the throttle).
  - Configurable via `MeshConfig::announcement_soft_budget_fraction` (default `0.75`); operators can tighten for sensitive deployments or loosen during known burst windows.
  - Counter metric: `dataforts_announcement_budget_warn_total{reason}`. Track in alerts.

  **Layer 3 — Hard runtime cap (production limit):** announcements that would exceed the hard per-node limit (default 2× baseline equivalent) are rejected at emit time with `CapabilityError::AnnouncementBudgetExceeded`. Protects neighboring nodes from a misbehaving local emitter. Defaults configured to be unreachable under normal operation; tripping this is operator-actionable as a hard incident.

  **Visibility surface:** `dataforts_announcement_bytes_per_sec` metric (gauge, per-node) shows current rate; `dataforts_announcement_budget_warn_total{reason}` (counter, per-node) shows soft-budget crossings; `dataforts_announcement_budget_exceeded_total` (counter, per-node) shows hard-cap rejections. All three are part of the standard operator dashboard.
- **Query-operator latency.** Local-only `match_axis` query ≤ 1 μs at 10K nodes in index. `nearest` ≤ 10 μs at the same scale.
- **`PlacementFilter` scoring.** Single-node score ≤ 5 μs across 100 candidate nodes for an artifact with all 5 axes active.

---

## Open design questions to lock before implementation

1. **Legacy tag deprecation window.** One minor version (default), or longer? **Recommendation:** one minor version with hard-removal in the next major. Survey current tag usage in the repo first; if heavy untyped-tag use exists, extend the window.

2. **Metadata size cap.** What's the per-CapabilitySet metadata budget? **Recommendation:** soft cap at 4 KB (most uses are sub-1KB; cap surfaces accidental abuse); hard cap at 16 KB with `CapabilityError::MetadataTooLarge`. Configurable per-channel via `ChannelConfig::metadata_cap_bytes`.

3. **Federated query default.** Local-only by default (recommended) or federated by default? **Recommendation:** local-only. Federated execution can have surprising latency and bandwidth costs; opting in via `Federated` wrapper makes the cost explicit. Pin in test.

4. **Anti-affinity scope.** Is the anti-affinity penalty applied only at placement-decision time, or continuously revisited? **Recommendation:** placement-decision time only for The Warriors. Continuous rebalancing is Atomic Playboys territory (federated mesh-wide scheduler).

5. **`PlacementFilter` thread-safety.** Trait is `Send + Sync`; impls must be thread-safe. The `Custom(Box<dyn Fn>)` variant of `IntentMatchPolicy` carries the same constraint. Pin in tests; document.

6. **Mikoshi v2 boundary.** This plan integrates Mikoshi with `PlacementFilter` but does not change the migration mechanism (snapshot/replay still happens). Live migration without snapshot, delta-based migration, and continuous placement re-evaluation are explicitly Atomic Playboys territory. Document the boundary in `CAPABILITIES.md` and Mikoshi's narrative doc.

---

## Risks

- **Tag-namespace migration breaks downstream code.** Mitigation: legacy-untyped tags continue parsing for one minor version; deprecation warning logs to operator-visible output; downstream code has runway. Don't ship if any production user has a high-volume untyped-tag emission path; extend the deprecation window.
- **Bloom false positives → spurious lookups.** Mitigation: probe-then-precise pattern is correctness-preserving (a false positive is a recoverable miss). Pin FPR in tests; surface `dataforts_chain_bloom_fpr` metric.
- **Metadata size growth.** Applications can put arbitrary data in metadata; this propagates through the substrate. Mitigation: per-`CapabilitySet` size cap (recommendation 4 KB soft / 16 KB hard).
- **`PlacementFilter` performance under churn.** Frequent placement decisions (e.g. high-rate replica creation) hammer the filter. Mitigation: `nearest` operator caches recent scoring within a heartbeat window; placement decisions scoped to candidate nodes (not full-mesh scans).
- **Mikoshi feature-flag rollback.** If `mikoshi-placement-v2` causes production issues, rollback is via feature flag. Test the rollback path explicitly — flipping the flag mid-runtime must not strand in-flight migrations.
- **Intent registry sprawl.** Application-defined intents accumulate over time; lookups may slow. Mitigation: `IntentRegistry` is `BTreeMap`-backed (O(log n) lookup); benchmark at 1000 registered intents and ensure ≤ 10 μs scoring overhead.
- **Cross-binding callback overhead.** Application-implemented `PlacementFilter` impls in non-Rust bindings cross the FFI boundary on every scoring call. Mitigation: provide `StandardPlacement` config-driven impls (no callback) as the recommended default; FFI-callback variants for advanced use only.

---

## Effort

**5–7 focused weeks parallelised.**

- ~3000 LoC core (taxonomy + tag shapes + metadata + bloom-filter aggregation + query operators + predicate language + PlacementFilter + StandardPlacement + IntentRegistry + Mikoshi integration)
- ~3500 LoC tests (unit + integration + property + performance + announcement-budget three-layer enforcement)
- ~1 week bindings (parallelisable across four bindings)
- ~3 days documentation (`CAPABILITIES.md` extension, `MIKOSHI.md` extension, operator-facing config docs)

Bindings are the only piece fully parallelisable; everything else has dependencies as sequenced in Phases A–H.

---

## Activation gate

Ships unconditionally as part of The Warriors release. The trait + reference impls + Mikoshi integration are foundation work — they enable everything Rebel Yell composes on top, plus all current and future placement decisions across the substrate.

The gate that triggers Warriors as a whole: any of the following workloads activates:

- A pilot wanting durability beyond single-node (`REDEX_DISTRIBUTED_PLAN.md`'s gate)
- A query workload that needs federated capability primitives

When any of these fire, Warriors ships as a coherent release; this plan's contents are part of that ship.

---

## Cross-cutting: relationship to other plans

**Builds on:**

- [`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md) — capability-announcement propagation. This plan extends propagation to carry the new metadata field and bloom-filter aggregation.
- [`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md) — broadcast machinery. This plan reuses for tag/metadata propagation; no new broadcast primitive needed.
- [`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md) — `scope:` tag convention. This plan reuses unchanged; reorganization keeps `scope:` as a reserved prefix.

**Consumed by:**

- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — uses `PlacementFilter` for replica placement; uses `causal:` advertisement for replica discovery; uses `metadata.intent` and `metadata.colocate-with` for intent-aware and colocation-aware placement; uses anti-affinity penalty for leader concentration prevention.
- [`misc/DATAFORTS_PLAN.md`](misc/DATAFORTS_PLAN.md) Phase 1 (Greedy LRU dataforts in Rebel Yell) — composes `PlacementFilter` for chain-cache placement; consumes `metadata.intent` and `metadata.colocate-with` for the 5-axis greedy filter.
- Future Atomic Playboys candidates (full MeshDB, Mikoshi v2, federated scheduler) — all build on the primitives shipped here.

**Replaces:**

- The flat untyped tag namespace (one minor version of legacy compatibility, then removed).
- Mikoshi's ad-hoc single-node migration target selection (preserved as `LegacyPlacement` for one minor version).

---

## See also

- [`REDEX_PLAN.md`](REDEX_PLAN.md) — single-node v1 substrate
- [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) — single-node v2 (orthogonal)
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — Phase 2 of The Warriors; consumes everything in this plan
- [`misc/DATAFORTS_PLAN.md`](misc/DATAFORTS_PLAN.md) — phased plan; this doc is the implementation detail for Phase 0 + Phase 6 + Phase 7
- [`misc/DATAFORTS_FEATURES.md`](misc/DATAFORTS_FEATURES.md) — feature audit
- [`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md) — `scope:` convention reused unchanged
- [`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md) — propagation primitive extended for metadata
- [`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md) — broadcast machinery reused
- [`CHANNEL_AUTH_GUARD_PLAN.md`](CHANNEL_AUTH_GUARD_PLAN.md) — ACL gating for capability tags + metadata
- [`misc/NRPC_DESIGN.md`](misc/NRPC_DESIGN.md) — pattern for protocol convention layered on existing reliable-stream + capability infrastructure
- `RELEASE_ROADMAP.md` — The Warriors release context
