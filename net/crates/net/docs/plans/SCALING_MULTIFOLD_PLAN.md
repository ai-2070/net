# Multi-Fold Design Plan

A unified state-aggregation framework over signed announcements. One mechanism, multiple typed instantiations (capability, routing, reservation, plus future folds). Replaces three would-be-separate implementations with one generic runtime and three trait impls.

This plan is scoped to land in the existing Net substrate. It composes with the channel/pub-sub layer, the identity layer, the existing hierarchical subnet system (`subnet::{SubnetId, SubnetGateway, SubnetPolicy}`), and the audit framework that already exist. It does NOT replace the existing `CapabilityIndex` immediately — the migration is staged so both coexist during transition.

---

## Goals

1. **Generic fold runtime** parameterized by typed kind (`FoldKind` trait). One implementation handles apply, expire, query, snapshot, recovery for all folds.

2. **Three concrete folds at launch:**
   - `CapabilityFold` — capability class membership per node
   - `RoutingFold` — destination → next-hop with metric-based selection
   - `ReservationFold` — resource → reservation state machine

3. **Wire format reusable across folds.** One `SignedAnnouncement<P>` envelope; payload type varies by fold.

4. **Subnet-aware delivery.** Folds use channels with appropriate `Visibility` settings (`SubnetLocal`, `ParentVisible`, `Exported`, `Global`); cross-subnet flow is handled by the existing `SubnetGateway`. Folds do not invent their own scoping.

5. **Operator surface.** CLI commands and Deck panels for all folds uniformly.

6. **Audit + snapshot integration.** Folds emit audit events; snapshots can serialize fold state for restart.

7. **Migration path.** Existing `CapabilityIndex` continues to work; new code uses the fold framework; cutover when comfortable.

---

## Non-goals

- Replacing the pub/sub channel mechanism. Folds USE channels for delivery; they don't replace them.
- Cross-fold transactions. Each fold is independent. Application-layer code composes results from multiple folds.
- Strong consistency. Folds are eventually consistent. TTL + generation comparison provide ordering; no global serialization.
- Hierarchical fold federation. At 50K-100K node scale, single-tier folds are sufficient. Cross-tier aggregation is a later concern past this scale.

---

## Architecture

### Layered structure

```
┌─────────────────────────────────────────────────────────────┐
│  Application code (schedulers, market matchers, etc.)        │
│  Queries folds; composes results across folds                │
└────────────────────────────┬─────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────┐
│  Fold instances: Fold<CapabilityFold>, Fold<RoutingFold>,    │
│  Fold<ReservationFold>                                       │
│  Provide: apply(announcement), query(q), snapshot(), etc.    │
└────────────────────────────┬─────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────┐
│  Generic fold runtime                                        │
│  - FoldState<K> container (per-instance state)               │
│  - Expiry task                                               │
│  - Snapshot/restore                                          │
│  - Audit emission                                            │
│  - Metrics                                                   │
└────────────────────────────┬─────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────┐
│  Subscription dispatch                                       │
│  Routes incoming SignedAnnouncement<P> to the right fold     │
│  based on kind tag in the announcement header                │
└────────────────────────────┬─────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────┐
│  Existing channel/pub-sub layer                              │
│  Folds subscribe to specific channels per FoldKind config    │
└──────────────────────────────────────────────────────────────┘
```

### The `FoldKind` trait

```rust
/// A typed fold definition. One impl per fold (capability,
/// routing, reservation, future kinds). All folds share runtime
/// machinery via this trait.
pub trait FoldKind: Send + Sync + 'static {
    /// Stable u16 identifier on the wire. Reserved range:
    /// 0x0000-0x00FF: built-in folds (capability=1, routing=2,
    /// reservation=3). 0x0100-0xFFFF: future / custom folds.
    const KIND_ID: u16;

    /// Channel prefix for this fold's announcements. Used to
    /// derive subscription channel names. e.g., "fold:cap:"
    /// for CapabilityFold; channel for class C is
    /// "fold:cap:" + class_hash.
    ///
    /// Subnet scope is NOT encoded in the channel name. The
    /// substrate's existing `subnet_id` header field on packets,
    /// combined with `ChannelConfig::visibility`
    /// (SubnetLocal / ParentVisible / Exported / Global), handles
    /// scoping at the gateway layer. Folds reuse this; they do
    /// not invent name-based scoping.
    const CHANNEL_PREFIX: &'static str;

    /// Default TTL for entries in this fold. Per-announcement
    /// TTL overrides this when present.
    const DEFAULT_TTL: Duration;

    /// Key type for indexing entries. Must be hashable, cloneable,
    /// and serializable (for snapshots).
    type Key: Hash + Eq + Clone + Send + Sync + Serialize + DeserializeOwned;

    /// Payload type carried in announcements. Domain-specific.
    type Payload: Clone + Send + Sync + Serialize + DeserializeOwned;

    /// Query type accepted by `Fold::query`.
    type Query: Send + Sync;

    /// Result type returned by `Fold::query`.
    type Result: Send + Sync;

    /// Extract the key from an announcement. Defines how
    /// entries are indexed within this fold.
    fn key_for(node_id: NodeId, payload: &Self::Payload) -> Self::Key;

    /// How to handle a new announcement vs an existing entry
    /// at the same key. Default: last-write-wins by generation.
    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction<Self::Payload> {
        match existing {
            None => MergeAction::Insert,
            Some(e) if incoming.generation > e.generation => MergeAction::Replace,
            _ => MergeAction::Reject,
        }
    }

    /// Optional secondary index. Default: no extra index.
    /// Capability fold uses this for tag-inverted lookup;
    /// reservation fold uses it for the "currently free" set.
    type Index: FoldIndex<Self>;
    fn build_index() -> Self::Index;

    /// Evaluate a query against the fold state + index.
    fn query(
        state: &FoldState<Self>,
        index: &Self::Index,
        query: Self::Query,
    ) -> Self::Result;

    /// Optional: emit an audit event when an entry transitions.
    /// Default: no audit emission.
    fn audit_event(
        transition: EntryTransition<Self>,
    ) -> Option<AuditEvent> {
        let _ = transition;
        None
    }
}
```

### The `Fold<K>` runtime

```rust
pub struct Fold<K: FoldKind> {
    state: Arc<RwLock<FoldState<K>>>,
    index: Arc<RwLock<K::Index>>,
    subscription_handles: Vec<SubscriptionHandle>,
    expiry_task: JoinHandle<()>,
    audit_sink: Option<AuditSink>,
    metrics: FoldMetrics,
}

pub struct FoldState<K: FoldKind> {
    /// Primary store: key → entry.
    entries: HashMap<K::Key, FoldEntry<K>>,
    /// Reverse index: node_id → set of keys it owns. Used for
    /// efficient cleanup when a node is declared dead.
    by_node: HashMap<NodeId, HashSet<K::Key>>,
    /// Stats.
    total_entries: AtomicUsize,
}

pub struct FoldEntry<K: FoldKind> {
    pub payload: K::Payload,
    pub node_id: NodeId,
    pub generation: u64,
    pub received_at: Instant,
    pub expires_at: Instant,
}

impl<K: FoldKind> Fold<K> {
    /// Create a fold and subscribe to relevant channels.
    pub async fn new(
        node: Arc<MeshNode>,
        subscriptions: Vec<ChannelSubscription>,
        audit_sink: Option<AuditSink>,
    ) -> Result<Self, FoldError> { ... }

    /// Query the fold. Read-only.
    pub fn query(&self, q: K::Query) -> K::Result {
        let state = self.state.read();
        let index = self.index.read();
        K::query(&state, &index, q)
    }

    /// Take an immutable snapshot for serialization.
    pub fn snapshot(&self) -> FoldSnapshot<K> { ... }

    /// Restore from a snapshot.
    pub async fn restore(&self, snap: FoldSnapshot<K>) -> Result<(), FoldError> { ... }

    /// Force-remove all entries owned by a node. Called when
    /// SWIM declares the node dead.
    pub fn evict_node(&self, node_id: NodeId) { ... }

    /// Metrics: entry count, update rate, expiry rate, query
    /// latency histogram.
    pub fn metrics(&self) -> &FoldMetrics { &self.metrics }
}

// Internal: apply path called by the subscription dispatch
impl<K: FoldKind> Fold<K> {
    pub(crate) async fn apply(
        &self,
        ann: SignedAnnouncement<K::Payload>,
    ) -> Result<ApplyOutcome, FoldError> {
        // 1. Verify signature (done by dispatch layer before
        //    reaching here; this is defensive)
        // 2. Compute key
        // 3. Acquire state write lock
        // 4. Look up existing entry; call K::merge to decide
        // 5. Apply merge action (insert/replace/reject)
        // 6. Update index
        // 7. Update by_node reverse index
        // 8. Emit audit event if K::audit_event returns Some
        // 9. Update metrics
        ...
    }
}
```

### Wire format

The `SignedAnnouncement<P>` is the fold-layer payload. **Subnet routing happens at the substrate's existing `NetHeader.subnet_id` field**, which carries the publisher's `SubnetId` (4-level hierarchical u32) on every packet. The fold announcement itself does NOT re-encode subnet scope — it relies on the existing header field plus `ChannelConfig::visibility` for scoping decisions made by `SubnetGateway`.

```rust
#[derive(Serialize, Deserialize)]
pub struct SignedAnnouncement<P> {
    /// Fold this announcement is for. Dispatched on this.
    pub kind: u16,
    /// Class within the fold. For CapabilityFold, this is
    /// the capability class hash. For RoutingFold, this is
    /// a routing tier identifier. For ReservationFold, this
    /// is a resource pool identifier.
    pub class: u64,
    /// Publisher.
    pub node_id: NodeId,
    /// Monotonic counter per (node_id, kind, class). Anti-
    /// reorder mechanism.
    pub generation: u64,
    /// When the publisher emitted this.
    pub announced_at: u64,  // unix micros
    /// How long this announcement is valid. None = use
    /// FoldKind::DEFAULT_TTL.
    pub ttl_secs: Option<u32>,
    /// Bit flags: join / leave / update / reserved bits.
    pub flags: u8,
    /// Payload (fold-specific).
    pub payload: P,
    /// Ed25519 signature over the canonical encoding of
    /// (kind, class, node_id, generation, announced_at,
    /// ttl_secs, flags, payload).
    pub signature: Signature,
}
```

Note what's NOT in this struct: no `subnet_id` field. The publisher's subnet is on the `NetHeader.subnet_id` of the underlying packet; subscribers can read it from there if they care. This avoids duplicating data and avoids inconsistency between two sources of truth.

Canonical encoding for signing: postcard with field-ordered structs. Locked in v1; version bumps as needed.

### Subscription dispatch

The pub/sub channel layer delivers raw messages. The dispatch layer:

1. Reads the `kind` field from the announcement header.
2. Looks up the registered fold instance for that kind.
3. Verifies the signature against the publisher's identity (using existing identity machinery).
4. Calls `Fold::apply` on the matching fold instance.
5. Errors short-circuit with logging + metric increment.

Registered via:

```rust
pub struct FoldRegistry {
    folds: HashMap<u16, Arc<dyn FoldDispatch>>,
}

impl FoldRegistry {
    pub fn register<K: FoldKind>(&mut self, fold: Arc<Fold<K>>) { ... }
    pub async fn dispatch(&self, msg: ChannelMessage) -> Result<(), DispatchError> { ... }
}
```

### Composition with the existing subnet system

The fold framework is **a layer above subnets, not parallel to them**. Specifically:

- **`NetHeader.subnet_id`** is the wire-level field carrying the publisher's 4-level hierarchical `SubnetId`. Every fold announcement is published in a packet that carries this header field; the substrate handles routing based on it.

- **`ChannelConfig::visibility`** (`SubnetLocal` / `ParentVisible` / `Exported` / `Global`) determines whether announcements on a channel cross subnet boundaries. A channel's visibility is part of its config, not its name.

- **`SubnetGateway`** enforces visibility at subnet boundaries: it reads the `subnet_id` and `channel_hash` on inbound packets, consults `ChannelConfig::visibility` and (for `Exported` channels) the gateway's `export_table`, and decides Forward or Drop. Folds do not duplicate this logic.

- **`SubnetPolicy` + `SubnetRule`** determine each node's `SubnetId` from its capability tags. Folds use whatever subnet a node already belongs to; they don't define their own subnet membership.

The fold framework reuses all of this. When a fold subscriber registers interest in a channel, the channel's visibility config decides which subnets the subscription is satisfied from. A scheduler in `subnet=us-east.dc-1` subscribed to `fold:cap:h100` with `Visibility::SubnetLocal` sees only announcements from that DC's subnet; with `Visibility::ParentVisible`, it sees announcements promoted to the parent (region) subnet; with `Visibility::Global`, anywhere.

**Practical consequences:**

- Fold authors choose the right `Visibility` for each fold's channels based on the use case. The `CapabilityFold` defaults to `Visibility::SubnetLocal` (capability membership is locally relevant); a global capacity summary channel uses `Visibility::Global`.
- Cross-tier visibility is achieved by aggregator daemons (see Subnet Scaling Spec) that subscribe to detail channels in their source subnet and publish summary channels visible to a parent subnet. The aggregator is a normal fold subscriber + publisher; the visibility model handles the rest.
- Fold subscribers do not need subnet-aware code paths. The substrate's existing channel + gateway machinery delivers exactly the announcements the visibility config allows.

This means **the fold framework gets multi-tier subnet scoping for free**. No new scoping mechanism, no subnet-encoded channel names, no parallel visibility model.

---

## The three concrete folds

### CapabilityFold

**Purpose:** Each (capability class, node) is one entry. Subscribers learn which nodes are in which classes. Replaces the broadcast model of the existing `CapabilityIndex`.

```rust
pub struct CapabilityFold;

impl FoldKind for CapabilityFold {
    const KIND_ID: u16 = 1;
    const CHANNEL_PREFIX: &'static str = "fold:cap:";
    const DEFAULT_TTL: Duration = Duration::from_secs(60);

    type Key = (u64 /*class*/, NodeId);
    type Payload = CapabilityMembership;
    type Query = CapabilityQuery;
    type Result = Vec<CapabilityMatch>;
    type Index = CapabilityIndexInner;

    fn key_for(node_id: NodeId, payload: &Self::Payload) -> Self::Key {
        (payload.class_hash, node_id)
    }

    fn build_index() -> Self::Index { CapabilityIndexInner::new() }

    fn query(state: &FoldState<Self>, index: &Self::Index, q: Self::Query) -> Self::Result {
        // Filter entries by class + tag predicates + visibility.
        // Use index for tag-inverted lookup; fall back to full
        // scan for unindexed predicates.
        ...
    }
}

pub struct CapabilityMembership {
    pub class_hash: u64,
    pub tags: Vec<Tag>,
    pub hardware: HardwareInfo,
    pub state: NodeState,  // idle/busy/reserved/faulty
    pub price_quote: Option<u64>,  // u$ per unit
    pub region: Option<String>,
}

pub struct CapabilityQuery {
    pub class: Option<u64>,
    pub tags_any: Vec<Tag>,
    pub tags_all: Vec<Tag>,
    pub state: Option<NodeState>,
    pub region: Option<String>,
    pub limit: usize,
}

pub struct CapabilityMatch {
    pub node_id: NodeId,
    pub membership: CapabilityMembership,
}

pub struct CapabilityIndexInner {
    by_tag: HashMap<Tag, HashSet<(u64, NodeId)>>,
    by_region: HashMap<String, HashSet<(u64, NodeId)>>,
    by_state: HashMap<NodeState, HashSet<(u64, NodeId)>>,
}
```

**Migration from existing `CapabilityIndex`:** existing capability code keeps working. New code uses `Fold<CapabilityFold>`. Both can coexist via a feature-gated bridge during transition. Once new code is validated, old code can be removed.

### RoutingFold

**Purpose:** Each destination has an entry; multiple announcements per destination from different routers; the merge logic picks the best route by metric.

```rust
pub struct RoutingFold;

impl FoldKind for RoutingFold {
    const KIND_ID: u16 = 2;
    const CHANNEL_PREFIX: &'static str = "fold:route:";
    const DEFAULT_TTL: Duration = Duration::from_secs(300);

    type Key = NodeId;  // destination
    type Payload = RouteAnnouncement;
    type Query = RouteQuery;
    type Result = Option<RouteEntry>;
    type Index = RouteIndexInner;

    fn key_for(_publisher: NodeId, payload: &Self::Payload) -> Self::Key {
        payload.destination
    }

    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction<Self::Payload> {
        match existing {
            None => MergeAction::Insert,
            Some(e) => {
                // Pick lower metric. Tie-break on incoming
                // (freshness).
                if incoming.payload.metric <= e.payload.metric {
                    MergeAction::Replace
                } else {
                    MergeAction::Reject
                }
            }
        }
    }

    // ... query implementation
}

pub struct RouteAnnouncement {
    pub destination: NodeId,
    pub next_hop: SocketAddr,
    pub metric: u32,  // RTT or hop count
    pub via: NodeId,  // the router that announced this
}
```

**Bridges with existing `RoutingTable`:** existing pingwave-based routing continues working; `RoutingFold` can ingest the same pingwave data through an adapter that translates pingwave broadcasts into `SignedAnnouncement<RouteAnnouncement>`. Or `RoutingFold` is used directly by new code paths and the old routing table is queried as a fallback.

### ReservationFold

**Purpose:** Each resource (GPU, slot, etc.) has at most one active reservation. State machine: Free → Reserved → Active → Free. Enforces single-holder semantics.

```rust
pub struct ReservationFold;

impl FoldKind for ReservationFold {
    const KIND_ID: u16 = 3;
    const CHANNEL_PREFIX: &'static str = "fold:res:";
    const DEFAULT_TTL: Duration = Duration::from_secs(30);

    type Key = ResourceId;
    type Payload = ReservationState;
    type Query = ReservationQuery;
    type Result = Vec<ResourceId>;
    type Index = ReservationIndexInner;

    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction<Self::Payload> {
        match (existing, &incoming.payload) {
            (None, _) => MergeAction::Insert,
            // Reservation owner can transition through states
            (Some(e), new) if e.node_id == incoming.node_id => {
                if valid_transition(&e.payload, new) {
                    MergeAction::Replace
                } else {
                    MergeAction::Reject
                }
            }
            // Different owner can only claim if current is Free
            (Some(e), ReservationState::Reserved { .. }) 
                if matches!(e.payload, ReservationState::Free) => {
                MergeAction::Replace
            }
            _ => MergeAction::Reject,
        }
    }

    // ... query implementation
}

pub enum ReservationState {
    Free,
    Reserved { holder: NodeId, until: u64 },
    Active { holder: NodeId, job_id: JobId },
}
```

State machine validation in `valid_transition` enforces legal moves; illegal transitions reject with audit event.

---

## Operator surface

### CLI commands

New top-level command group `net-mesh fold`:

```
net-mesh fold list
    List registered folds and their stats.

net-mesh fold stats <kind>
    Detailed stats for one fold: entry count, update rate,
    expiry rate, query latency percentiles, top publishers
    by update volume.

net-mesh fold query <kind> [<query-spec>]
    Run a query against a fold. Format varies by kind.
    Examples:
      net-mesh fold query cap --class h100 --state idle
      net-mesh fold query route --dest 0x1234
      net-mesh fold query res --pool gpu-pool-1 --state free

net-mesh fold snapshot <kind> [--output <path>]
    Dump fold state to file (for diagnostics or backup).

net-mesh fold restore <kind> --input <path>
    Restore fold state from file. Requires fold to be empty
    (or --force).

net-mesh fold evict <kind> --node <id>
    Force-remove entries owned by a node. Operator
    intervention when SWIM hasn't caught up.

net-mesh fold tail <kind>
    Tail apply events as they happen (debugging).
```

These integrate into the existing CLI structure. Implementation lives in `cli/src/commands/fold.rs`.

### Deck panels

The Deck gains a new tab `[7] FOLDS` or similar:

```
FOLDS panel:
  Lists registered folds with live counters:
  
  KIND          ENTRIES  UPDATE/s  EXPIRE/s  QUERY p50  QUERY p99
  Capability    12,847   234       12        0.4ms      2.1ms
  Routing       2,103    8         0         0.1ms      0.3ms
  Reservation   847      45        18        0.2ms      0.9ms

Drilldown per fold shows:
  - Recent applied announcements (timestamp, publisher, key, action)
  - Top publishers by update volume
  - Distribution of entry ages (histogram)
  - Audit event stream
```

---

## Audit integration

Each fold can emit audit events through the existing audit mechanism. Default emissions:

- `FoldEntryCreated { kind, key, node_id, generation }`
- `FoldEntryReplaced { kind, key, old_generation, new_generation, by_node }`
- `FoldEntryExpired { kind, key, node_id, age }`
- `FoldEntryEvicted { kind, key, reason }`
- `FoldEntryRejected { kind, key, reason }` (illegal state transitions, replay attempts, etc.)

These flow through the existing audit chain (signed commits, replayable). Per-fold filtering via `K::audit_event`.

---

## Snapshot integration

Each fold can serialize its state for restart recovery:

```rust
pub struct FoldSnapshot<K: FoldKind> {
    pub kind: u16,
    pub taken_at: u64,
    pub entries: Vec<(K::Key, FoldEntry<K>)>,
}
```

On startup, a node can:
1. Restore each fold from its last snapshot (warm start)
2. Re-subscribe to channels, begin receiving live announcements
3. Apply live announcements; their generation numbers naturally win over restored ones if newer

This avoids the cold-start latency where a restarting node has to wait for everyone to re-announce.

Snapshots are taken periodically (configurable, default 5 min) and on graceful shutdown.

---

## Metrics

Per-fold metrics exposed via the existing metrics layer:

- `fold_entries_total{kind}` — current entry count
- `fold_applies_total{kind, outcome}` — apply count by outcome (inserted, replaced, rejected)
- `fold_expiries_total{kind}` — expiry count
- `fold_queries_total{kind}` — query count
- `fold_query_duration{kind}` — query latency histogram
- `fold_apply_duration{kind}` — apply latency histogram
- `fold_subscription_lag{kind, channel}` — backlog of unprocessed announcements per channel

Exposed in Prometheus format; also visible in the Deck FOLDS panel.

---

## Performance targets

For each fold at 100K-node mesh scale:

- **Apply latency:** p99 < 1ms (signature verify + state mutation + index update)
- **Query latency (point):** p99 < 100µs
- **Query latency (filtered):** p99 < 5ms for typical filters
- **Memory per 10K entries:** < 50MB (matching existing `CapabilityIndex` target)
- **Apply throughput per fold:** > 100K applies/sec/core (signature-verify-bound)
- **Subscription processing lag:** < 10ms p99 from channel receipt to apply complete

These are achievable on commodity hardware with the audit's per-call optimizations (#11 Arc-snapshots, #110 inverted indices, #114 batch shard ops) applied to the fold runtime.

---

## Implementation phases

### Phase 1: Generic runtime (1-2 weeks)

**Deliverable:** `Fold<K>` runtime, `FoldKind` trait, `FoldState`, `FoldEntry`, expiry task, snapshot/restore primitives, audit hooks, metrics. No concrete fold instances yet.

**Tests:**
- Property tests: apply-then-query consistency
- Property tests: TTL expiry deterministic
- Property tests: snapshot-restore round-trips identical state
- Stress test: 100K applies/sec sustained
- Concurrency test: many simultaneous applies + queries

**Files:**
- `crates/net/src/adapter/net/behavior/fold/mod.rs` — trait, runtime
- `crates/net/src/adapter/net/behavior/fold/state.rs` — `FoldState`
- `crates/net/src/adapter/net/behavior/fold/snapshot.rs` — snapshot serialization
- `crates/net/src/adapter/net/behavior/fold/audit.rs` — audit emission helpers
- `crates/net/src/adapter/net/behavior/fold/metrics.rs` — metrics
- `crates/net/src/adapter/net/behavior/fold/tests/*` — property + integration tests

### Phase 2: Wire format + dispatch (1 week)

**Deliverable:** `SignedAnnouncement<P>` codec, signature verification path, `FoldRegistry`, dispatch from channel layer to fold instances.

**Tests:**
- Wire format round-trip
- Signature verification rejects tampered announcements
- Replay protection (same generation rejected)
- Dispatch to correct fold by kind ID

**Files:**
- `crates/net/src/adapter/net/behavior/fold/wire.rs` — codec
- `crates/net/src/adapter/net/behavior/fold/dispatch.rs` — registry + dispatch
- Integration into the existing channel handler

### Phase 3: CapabilityFold (1-2 weeks)

**Deliverable:** `CapabilityFold` impl. Migration bridge from existing `CapabilityIndex` (announcements published to both during transition).

**Tests:**
- Existing capability test suite passes against `Fold<CapabilityFold>` (via bridge)
- New tests: subscription model, class membership, tag-based queries
- Performance: matches or exceeds existing `CapabilityIndex` benchmarks

**Files:**
- `crates/net/src/adapter/net/behavior/fold/capability.rs` — impl
- `crates/net/src/adapter/net/behavior/fold/capability_bridge.rs` — migration adapter
- `crates/net/src/adapter/net/behavior/fold/capability_tests.rs`

### Phase 4: RoutingFold (1 week)

**Deliverable:** `RoutingFold` impl. Bridge from existing pingwave-driven routing.

**Tests:**
- Route table behavior matches existing `RoutingTable`
- Metric-based route selection
- TTL-based eviction matches pingwave timeout semantics

**Files:**
- `crates/net/src/adapter/net/behavior/fold/routing.rs`
- `crates/net/src/adapter/net/behavior/fold/routing_bridge.rs`
- `crates/net/src/adapter/net/behavior/fold/routing_tests.rs`

### Phase 5: ReservationFold (1-2 weeks)

**Deliverable:** `ReservationFold` impl with state machine enforcement. No existing code to bridge from (new functionality).

**Tests:**
- State machine: legal transitions accepted, illegal rejected
- Concurrent reservation: only one wins
- Reservation expiry: TTL releases stale reservations
- Owner-only state changes (third party can't release someone else's reservation)

**Files:**
- `crates/net/src/adapter/net/behavior/fold/reservation.rs`
- `crates/net/src/adapter/net/behavior/fold/reservation_tests.rs`

### Phase 6: CLI + Deck integration (1 week)

**Deliverable:** `net-mesh fold` command group, Deck FOLDS panel, metrics integration with existing observability.

**Files:**
- `crates/net/cli/src/commands/fold.rs`
- Deck UI updates (in whatever path the Deck lives)

### Phase 7: Migration + cutover (1-2 weeks)

**Deliverable:** Existing `CapabilityIndex` users migrated to `Fold<CapabilityFold>`. Existing routing users have option to use `Fold<RoutingFold>`. ReservationFold lights up new product features (compute marketplace, GPU reservation in scheduler).

**Tests:**
- End-to-end: full stack using fold framework
- Performance regression tests vs baseline
- Production replay of historical traffic against new framework

---

## Total timeline

7-10 weeks for full implementation, including migration. The runtime + dispatch + capability fold (the core, 3-4 weeks) is the critical path; routing/reservation can follow at the team's pace.

For pre-investor purposes, Phases 1-3 (4-5 weeks) are sufficient to demonstrate the framework in action. The remaining phases are normal followup.

---

## Risks and mitigations

**Risk: Generic dispatch adds overhead vs purpose-built code.**

Mitigation: benchmark each fold against its baseline (CapabilityIndex, RoutingTable). Generic dispatch has known cost (vtable lookup, dynamic typing); the benchmark numbers should show this is small relative to signature verification + state mutation, which dominate. If generic dispatch becomes the bottleneck, specific folds can implement hot paths directly without going through the trait.

**Risk: Migration creates dual-source-of-truth bugs.**

Mitigation: bridge layer publishes to both old and new during transition. A reconciliation tool compares state between old and new periodically; alerts on divergence. Cutover only after a sustained period of zero divergence. Old code removed in a final cleanup PR.

**Risk: Wire format choice locks in poor decisions.**

Mitigation: postcard encoding with versioned envelopes. Adding fields is backward-compatible if defaulted. Breaking changes bump version; old and new can coexist during multi-version transitions.

**Risk: Subscription lag at high update rates causes fold staleness.**

Mitigation: per-channel lag metrics; alerting on sustained lag. If a fold becomes overwhelmed, the fix is sharding (multiple fold instances handling subsets of the keyspace) — but this is past the 50K-100K node scale where the framework is designed to work without sharding.

**Risk: Signature verification is the bottleneck.**

Mitigation: batch verification (Ed25519 supports it); cache verified signatures from recent peers (the existing token cache infrastructure can be reused); hardware acceleration where available.

**Risk: The generic framework is over-engineered for three folds.**

Mitigation: this is the strongest counter-argument. If only three folds ever exist, three purpose-built implementations might be cleaner. The framework is justified by: (1) operational uniformity across folds (one set of metrics, CLI, Deck panels), (2) extensibility for future folds without re-architecting, (3) the substrate's value to investors and partners increases when it's clearly extensible vs hardcoded. The cost is acceptable: the framework is ~1.5x the code of one fold but services all three (and future ones).

---

## Open questions

1. **Channel-per-class vs channel-per-fold.** CapabilityFold could either subscribe to one channel per capability class (many subscriptions, fine-grained) or one channel per fold with class filtering at the subscriber (one subscription, coarser filter). The first is more efficient at runtime but creates many channels; the second is simpler but ships all announcements to all subscribers within a fold. **Recommendation:** start with per-class channels, since the pub/sub layer handles channel proliferation well, and subscribers naturally want only certain classes.

2. **Generation number bookkeeping.** Each publisher needs to maintain per-(kind, class) counters. Where do these live? **Recommendation:** in a small persistent file per node (similar to identity key storage). Survives restarts; doesn't need to coordinate with anyone.

3. **Eviction policy under memory pressure.** If a fold grows beyond memory budget (unexpected), how does it shed? **Recommendation:** TTL-based natural expiry is the primary mechanism. Under pressure, LRU eviction of entries that aren't being queried frequently. Worst case: per-fold size cap with oldest-first eviction.

4. **Cross-fold consistency.** Application code that reads from multiple folds (e.g., "find GPUs that are in 'idle' capability state AND have a 'free' reservation") will see eventually consistent views. **Recommendation:** document this clearly; provide a helper for "snapshot multiple folds at once" that takes read locks across folds in canonical order (to avoid deadlocks). For most use cases, the slight inconsistency is acceptable; for stricter use cases, application-layer retry on the few wrong answers.

5. **Future federation.** Past 100K nodes, individual folds may need sharding or hierarchical aggregation. **Recommendation:** design the trait so a future `FederatedFold<K>` wrapper can compose multiple `Fold<K>` instances across regions. Don't build it now.

---

## Summary

A unified, typed fold framework that:

- Replaces three would-be-separate implementations with one
- Provides consistent operator surface (CLI, Deck, metrics, audit) across all folds
- Composes with existing pub/sub, identity, and audit mechanisms
- Scales to 100K-200K nodes per mesh with the existing substrate
- Lands in 7-10 weeks total; demo-ready at 4-5 weeks
- Lets future folds be added without re-architecting (a real value-add for the substrate's positioning)

The runtime is small (~2-3K lines of Rust). The concrete folds are 500-1000 lines each. Tests double these numbers. Total: ~8-12K lines of well-isolated, well-tested code. Comparable to the existing `CapabilityIndex` in scope but covering three folds instead of one.

This is buildable.
