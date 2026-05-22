# Subnet-Aware Scaling: Aggregators on top of existing primitives

## Substrate Work Needed for Beyond-100K-Node Scale

This document specifies the substrate-level changes required to scale Net meshes past ~100K nodes using the **existing** subnet, channel, and replica-group primitives more deliberately. The bulk of what's needed is already in the substrate; the remaining work is aggregator daemons (built on `ReplicaGroup` of `MeshDaemon`), CLI ergonomics, and the fold framework that consumes these primitives.

This is a complement to the [Multi-Fold Plan](./MULTIFOLD_PLAN.md). The multi-fold framework handles per-fold state aggregation; the work here describes how that aggregation composes with existing subnet hierarchy to scale to millions of nodes.

The principle: **scale by subdivision (existing hierarchical subnets) and summarization (new aggregator daemons via ReplicaGroup), using primitives already in the substrate. No new architecture tier; no parallel scoping mechanism.**

---

## What's already in the substrate

Before listing what needs to be added, an inventory of what's already there:

### Hierarchical `SubnetId`

Defined in `crates/net/src/adapter/net/subnet/id.rs`:

- 4-level hierarchy encoded in a `u32`: `[region (8 bits) | fleet (8 bits) | vehicle (8 bits) | subsystem (8 bits)]`
- 256 values per level, 4 levels deep. Sized for hierarchical deployment of any practical scale.
- Bitwise hierarchy operations at wire speed: `parent()`, `is_ancestor_of()`, `is_sibling()`, `is_same_subnet()`, `mask_for_depth()`, `depth()`.
- `SubnetId::GLOBAL` (0) means "no subnet / unrestricted."
- The `SubnetId` is carried on `NetHeader.subnet_id` — every packet identifies its subnet at the wire layer.

### `SubnetGateway`

Defined in `crates/net/src/adapter/net/subnet/gateway.rs`:

- Enforces visibility policy at subnet boundaries — the "causal membrane" between subnets.
- Reads only header fields (no decryption, no payload inspection): `subnet_id`, `channel_hash`, `hop_ttl`, `hop_count`.
- `should_forward(source_subnet, dest_subnet, channel_hash, hop_ttl, hop_count) -> ForwardDecision`
- Drop reasons: `TtlExpired`, plus visibility-based drops per `ChannelConfig`.
- `export_channel(channel_hash, targets)` — exports a channel to specific target subnets (for `Visibility::Exported` channels).
- TTL/hop-count loop prevention built in.
- Counts: `forwarded_count()`, `dropped_count()` for observability.

### Channel `Visibility`

Defined in `crates/net/src/adapter/net/channel/config.rs`:

- `Visibility::SubnetLocal` — packets never leave the subnet
- `Visibility::ParentVisible` — visible to the parent subnet but not siblings
- `Visibility::Exported` — explicitly exported to specific target subnets (via the gateway's `export_table`)
- `Visibility::Global` — no subnet restriction

Set per channel via `ChannelConfig::visibility`. The substrate already supports the full scope ladder; no name-based scoping convention needed.

### Label-based subnet assignment

Defined in `crates/net/src/adapter/net/subnet/assignment.rs`:

- `SubnetPolicy` + `SubnetRule` — derives a node's `SubnetId` from its capability tags.
- Rules are evaluated in declaration order; later-rule-wins at the same level.
- Tag-prefix-based matching with explicit value maps.
- Rule semantics documented and test-pinned.

A node with tags `["region:us-west", "fleet:alpha"]` and rules mapping `region:` → level 0 / `fleet:` → level 1 gets `SubnetId::new(&[1, 2])`. Operators configure the policy; the substrate handles assignment.

### Partition correlation

Defined in `crates/net/src/adapter/net/contested/`:

- Partition detection and correlation analysis.
- Subnet-level partition tracking; understands when an entire subnet has been cut off.
- Foundation for failure analysis at subnet granularity.

### `ReplicaGroup`

Defined in `crates/net/src/adapter/net/compute/replica_group.rs`:

- N interchangeable daemon replicas managed as a unit.
- Deterministic per-replica keypairs via `derive_replica_keypair(group_seed, index)`.
- Placement spread across failure domains via `GroupCoordinator::place_with_spread`.
- Load-balanced routing to the nearest healthy replica via `route_event`.
- Group-level health (alive as long as ≥1 replica healthy).
- Auto-replacement on node failure via `on_node_failure`.
- Term/epoch fencing prevents resurrected old replicas from acting authoritatively.
- Dynamic scaling via `scale_to`.

This is the HA primitive aggregators will use.

---

## What this means for scale

Combining what's already there:

- **Subnet hierarchy bounds discovery scope.** A node in `subnet=us-east.dc-1.rack-5` sees `SubnetLocal` channels for that subnet (rack-local), inherits visibility into the parent subnets (DC, region) per channel config.
- **The gateway enforces these boundaries at wire speed.** No application-level logic needed for cross-subnet routing decisions; the substrate handles it.
- **Channel visibility is configured per channel.** Sensitive channels stay subnet-local; aggregate channels become `Global` or `Exported`. Operators control the topology by setting visibility on channels.
- **Label-based assignment lets operators express any topology.** Tag-based rules derive `SubnetId` from each node's identity; operators choose the tagging scheme that matches their deployment.

At 100K-1M-node scale, the existing substrate handles:
- Subnet membership and assignment
- Cross-subnet routing decisions
- Per-channel scoping
- Partition detection
- HA via replica groups

What's NOT yet in the substrate, and is required for genuine multi-tier scale:

1. **The multi-fold framework** that consumes these primitives to maintain typed, signed aggregate state per fold per subnet scope. (Covered in the Multi-Fold Plan.)
2. **Aggregator daemons** that bridge tiers by subscribing to detail channels in a source subnet and publishing summary channels visible to a parent or peer subnet.
3. **CLI ergonomics** for inspecting subnet hierarchy, channel visibility, gateway state, and aggregator deployments.

That's the work. Three pieces, not six. The substrate has been thought through more carefully than a naive scaling spec would suggest.

---

## Goals

1. **Bounded per-node work regardless of mesh size.** Fold state, subscription fan-in, and channel fanout all scale with subnet participation and channel visibility, not mesh size.

2. **Reuse existing primitives.** All scaling expressed through existing `SubnetId`, `SubnetGateway`, `ChannelConfig::visibility`, `SubnetPolicy`, and `ReplicaGroup`. New work is in *composition*, not new primitives.

3. **Aggregator role as a `MeshDaemon` deployed via `ReplicaGroup`.** No new HA mechanism; the existing replica-group infrastructure handles placement, failover, identity continuity.

4. **Operator-controlled topology.** Operators express their deployment hierarchy via subnet policy (`SubnetPolicy`) and channel visibility (`ChannelConfig::visibility`). The substrate accommodates any topology; it doesn't impose one.

5. **Backward compatibility.** Existing flat-mesh deployments and existing subnet-aware deployments both continue working. The fold framework and aggregator daemons are additions, not replacements.

---

## Non-goals

- **Inventing a new subnet system.** The substrate already has 4-level hierarchical subnets with gateway enforcement. We use it; we don't replace it.
- **Inventing a new channel scoping mechanism.** `ChannelConfig::visibility` is the mechanism. We configure folds' channels appropriately; we don't create parallel naming conventions.
- **Inventing a new HA mechanism.** `ReplicaGroup` is the mechanism. Aggregators are `MeshDaemon` impls deployed via it.
- **Cross-mesh federation.** Each mesh is independent; multi-mesh federation is a separate concern past where subnet scaling reaches.
- **Strong consistency across subnets.** Summaries lag detail; aggregation is eventually consistent. Applications that need strong consistency do direct RPC to the source subnet's aggregator.
- **Automatic subnet topology discovery.** Operators define topology via `SubnetPolicy`; the substrate doesn't infer it.

---

## What needs to be built

Three pieces of work, in dependency order:

### 1. Aggregator daemon (`AggregatorDaemon` deployed via `ReplicaGroup`)

**Problem:** to bridge tiers (rack subnets → DC summaries → region summaries → global summaries), some node in each subnet needs to maintain a detailed view of fold state and republish summaries to channels visible at a parent or peer tier. This is the missing piece: the substrate has the routing/visibility machinery, but no daemon that actually does the summarization work.

**Solution:** implement the aggregator as a `MeshDaemon` and deploy it through the existing `ReplicaGroup` infrastructure. No new HA mechanism, no new placement logic, no new identity-failover protocol — the substrate already has all of this.

#### What `ReplicaGroup` provides for free

The existing replica group infrastructure handles every aspect of multi-instance, failure-tolerant daemon deployment that the aggregator role needs:

- **Deterministic identity per replica.** `derive_replica_keypair(group_seed, index)` produces the same keypair for the same (group, index) pair. When a replica is re-placed onto a different physical node after failure, it keeps the same cryptographic identity. Subscribers to summary channels see continuity of publisher identity across re-placements — no resubscription, no key churn, signatures keep verifying.

- **Placement spread across failure domains.** `GroupCoordinator::place_with_spread` ensures N aggregator replicas land on N different physical nodes within the source subnet.

- **Load-balanced routing to healthy replicas.** `route_event` picks the closest healthy replica for inbound queries.

- **Group-level health.** Group is alive as long as ≥1 replica is healthy.

- **Auto-replacement on node failure.** `on_node_failure` re-spawns a replica's slot on a different node with the same derived identity. The aggregator role survives physical node failure transparently. The new replica subscribes to detail channels, rebuilds its fold from incoming announcements + TTL refreshes, resumes publishing summaries within one TTL cycle (~30-60s).

- **Term/epoch fencing.** Bumps on every recovery-driven re-placement. Prevents resurrected old replicas from continuing to act authoritatively.

- **Dynamic scaling.** `scale_to` adds or removes replicas without restarting the group.

#### `AggregatorDaemon` as a `MeshDaemon`

```rust
pub struct AggregatorDaemon {
    config: AggregatorConfig,
    fold_subscriptions: Vec<Box<dyn FoldSubscriber>>,
    last_summary_at: HashMap<u16, Instant>,  // per fold kind
}

pub struct AggregatorConfig {
    /// Subnet this aggregator covers. Replicas must be members
    /// of this subnet (enforced via capability requirements in
    /// the daemon's `Requirements`).
    pub source_subnet: SubnetId,
    
    /// Subnet visibility of summary channels this aggregator
    /// publishes to. Typically `Visibility::ParentVisible` so
    /// the parent subnet sees summaries; or `Visibility::Exported`
    /// with explicit target subnets; or `Visibility::Global` for
    /// cluster-wide summaries.
    pub summary_visibility: Visibility,
    
    /// For Exported visibility: which subnets the summaries
    /// should reach.
    pub summary_targets: Vec<SubnetId>,
    
    /// Which fold types to aggregate.
    pub fold_kinds: Vec<u16>,
    
    /// How often to recompute and republish summaries.
    pub summary_interval: Duration,
    
    /// Optional custom summarization logic per fold kind;
    /// default is the built-in summarizer for that kind.
    pub custom_summarizers: HashMap<u16, Box<dyn Summarizer>>,
}

impl MeshDaemon for AggregatorDaemon {
    fn requirements(&self) -> Requirements {
        Requirements {
            // Capability tags that the SubnetPolicy will use
            // to place replicas in the source subnet.
            capabilities_required: subnet_membership_tags(&self.config.source_subnet),
            // Plus an aggregator-role tag for explicit selection.
            additional_tags: vec!["role:aggregator"],
            min_memory_mb: estimated_fold_memory(&self.config),
        }
    }

    async fn on_start(&mut self) -> Result<(), DaemonError> {
        // Subscribe to detail channels in the source subnet.
        // The channels are scoped via Visibility::SubnetLocal
        // (or whatever the underlying fold's channels use).
        // The gateway ensures only same-subnet announcements
        // reach this subscriber.
        for kind in &self.config.fold_kinds {
            let subscription = self.subscribe_to_source(*kind)?;
            self.fold_subscriptions.push(subscription);
        }
        // Configure the summary channels' visibility per the
        // config (ParentVisible / Exported / Global).
        self.configure_summary_channels()?;
        Ok(())
    }

    async fn handle_event(&mut self, event: Event) -> Result<(), DaemonError> {
        match event {
            Event::Channel(msg) => {
                // Fold announcement received: dispatch to the
                // matching fold subscription
                self.dispatch_to_fold(msg).await
            }
            Event::Rpc(req) => {
                // Cross-subnet detail RPC: respond from local
                // fold state
                self.handle_query(req).await
            }
            Event::Tick(now) => {
                // Periodic: check each fold kind for summary
                // interval elapsed; recompute and publish if so
                self.maybe_summarize(now).await
            }
        }
    }
}

pub trait Summarizer: Send + Sync {
    /// Given the current fold state, produce summary
    /// announcements to publish to the target visibility.
    fn summarize(&self, state: &dyn FoldStateView) -> Vec<SummaryAnnouncement>;
}
```

The daemon's `Requirements` declares it needs membership in the source subnet — placement automatically restricts to nodes in that subnet via the existing capability-based placement filter. The replica's identity is derived deterministically from the group seed, which is itself derived from `(source_subnet, summary_visibility, fold_kinds)`.

#### Spawning aggregators via `ReplicaGroup`

```rust
let config = ReplicaGroupConfig {
    replica_count: 3,
    group_seed: derive_aggregator_seed(
        &source_subnet,
        &summary_visibility,
        &fold_kinds,
    ),
    lb_strategy: Strategy::NearestHealthy,
    host_config: DaemonHostConfig::default(),
};

let aggregator_factory = move || {
    Box::new(AggregatorDaemon::new(aggregator_config.clone())) 
        as Box<dyn MeshDaemon>
};

let group = ReplicaGroup::spawn(
    config,
    aggregator_factory,
    &scheduler,
    &registry,
)?;
```

Idempotent: re-running the same spawn command produces the same group identity (deterministic seed).

#### Summary publication: all replicas publish

All 3 replicas publish summaries independently. Each runs its own summarizer on its own (eventually consistent) view of the source fold, publishes its summary at the configured interval. Subscribers see three summary announcements per cycle and use the fold framework's generation comparison to pick the latest.

Reasons:
- No election machinery needed
- Faster failover (any healthy replica's summary suffices)
- Tolerable redundancy cost (~100 bytes/sec aggregate per fold per subnet)
- Natural staleness detection via generation comparison
- Operator visibility: replica index in summary payload for debugging

Operator can `scale_to(1)` to reduce summary traffic at cost of availability.

#### Built-in summarizers per fold

- **CapabilityFold:** count by (class, state), aggregate hardware capacity, distribution across sub-subnets. Summary entry per class.
- **RoutingFold:** typically not summarized — routing usually wants full detail or none. Built-in summarizer omitted; can be added if a use case emerges.
- **ReservationFold:** count by (resource class, state), aggregate availability. Summary entry per resource class.

Custom summarizers can be plugged in for domain-specific summarization (a Hermes deployment might summarize agent capabilities differently from a Palantir deployment).

#### State across replica re-placements

Fold state is not persisted — it rebuilds from incoming channel announcements + TTL refreshes within ~30-60 seconds (one TTL cycle for capability fold). During the rebuild window, other replicas in the group continue publishing full summaries, so subscribers see continuous coverage.

#### Files affected

- `crates/net/src/adapter/net/behavior/aggregator/mod.rs` — `AggregatorDaemon` impl
- `crates/net/src/adapter/net/behavior/aggregator/summarizer.rs` — `Summarizer` trait + built-in summarizers
- `crates/net/src/adapter/net/behavior/aggregator/config.rs` — `AggregatorConfig`
- `crates/net/cli/src/commands/aggregator.rs` — CLI wrapping `ReplicaGroup` ops
- Integration with `compute/replica_group.rs` (consumer of existing API)

#### Estimated effort

**~2 weeks total:**
- ~1 week: `AggregatorDaemon` as `MeshDaemon` impl + integration with `Fold<K>` framework.
- ~1 week: built-in summarizers for capability and reservation folds.
- ~few days: CLI commands wrapping `ReplicaGroup::spawn`/`scale_to`/teardown.

The effort is bounded because `ReplicaGroup`, `SubnetId`, `SubnetGateway`, `ChannelConfig::visibility`, and the fold framework do all the heavy lifting. Aggregator is glue + summarization logic.

### 2. Cross-subnet detail-on-demand RPC

**Problem:** when a subscriber sees a summary (via `Visibility::ParentVisible` or `Visibility::Global` summary channels) and wants detail from the source subnet, it needs to RPC to the source subnet's aggregator for the full data. The substrate has the RPC machinery and the gateway will forward the RPC; what's missing is the service definition and the client-side ergonomics.

**Solution:** standard RPC pattern using existing RPC primitives. The aggregator exposes a query endpoint:

```rust
#[rpc_service(name = "fold.query")]
pub trait FoldQueryService {
    /// Query the aggregator's local fold for detail.
    async fn query(
        &self,
        kind: u16,
        class: u64,
        query: Bytes,  // fold-specific query, postcard-encoded
    ) -> Result<Bytes, FoldQueryError>;
}
```

The aggregator daemon implements this service automatically. The querier uses the existing RPC machinery and routes through the substrate's normal gateway (which forwards based on `ChannelConfig::visibility` and `subnet_id` header). No new wire protocol; no special-case routing.

**Discovery:** the querier finds the aggregator group via the capability index — replicas are tagged with `role:aggregator` and their source subnet is in their identity. Once a replica is identified, RPC is routed to it. Since aggregators are deployed as `ReplicaGroup`, the group's `route_event` naturally picks the closest healthy replica.

**Caching:** the RPC client caches recent query results with a short TTL (configurable, default 5s). Repeated queries for the same data don't re-hit the aggregator.

**Files affected:**
- `crates/net/src/adapter/net/behavior/aggregator/query_service.rs` — RPC service definition
- `crates/net/src/adapter/net/behavior/fold/query_client.rs` — client-side helper
- Integration with the existing RPC machinery in `crates/net/src/adapter/net/rpc/`

**Estimated effort:** 1 week.

### 3. CLI + Deck integration

**Problem:** operators need visibility into subnet hierarchy, channel visibility configs, gateway state, and aggregator deployments. The substrate has primitives; operators need the surface to inspect and manage them.

**Solution:** extend the CLI and Deck with subnet/gateway/aggregator awareness. Some commands likely already exist in some form (need audit); others are new.

#### CLI

```
net-mesh subnet show
    Show this node's SubnetId, the SubnetPolicy in effect, 
    capability tags driving the assignment.

net-mesh subnet ls
    List subnets known to this node (from capability index)
    with member counts and hierarchy depth.

net-mesh subnet tree
    Show the subnet hierarchy as a tree.

net-mesh gateway stats
    Show this node's gateway forwarded/dropped counters,
    drop reasons distribution, top channels by traffic.

net-mesh gateway export <channel> <target-subnet> [<target-subnet>...]
    Add an export rule for an Exported-visibility channel.

net-mesh channel visibility <channel>
    Show a channel's visibility config.

net-mesh aggregator spawn \
    --source-subnet <subnet-id> \
    --summary-visibility parent|global|exported \
    --summary-targets <subnet-ids>... (if exported) \
    --fold-kinds capability,reservation \
    --replicas 3 \
    --summary-interval 30s

net-mesh aggregator ls
    List active aggregator groups and their health.

net-mesh aggregator scale <group-id> --replicas N
    Scale a group up or down.

net-mesh aggregator inspect <group-id>
    Show replica placement, fold sizes, recent summaries.
```

Some of these may exist already or be straightforward extensions of existing commands. The CLI module structure (`crates/net/cli/src/commands/`) suggests adding new modules for `subnet.rs`, `gateway.rs`, `aggregator.rs` or extending existing modules.

#### Deck

New or extended panels:

- **SUBNETS panel** — show subnet hierarchy, member counts per subnet, this node's subnet.
- **GATEWAYS panel** — show gateway forwarded/dropped counts, top channels by cross-subnet traffic.
- **AGGREGATORS panel** — show aggregator groups, their source subnets, summary cadence, recent summaries.

The Deck is already a mature operator tool. Adding panels follows the existing pattern.

**Estimated effort:** ~1 week for CLI commands, ~1 week for Deck panels (depending on Deck's panel-addition workflow).

---

## Phasing

```
Phase A (week 1):
  - CLI commands for subnet/gateway/channel-visibility inspection
  - Deck SUBNETS and GATEWAYS panels
  
  Deliverable: operators have visibility into the existing
  subnet machinery. No new substrate behavior; just surfacing
  what's there.

Phase B (week 2-3):
  - AggregatorDaemon as MeshDaemon
  - Built-in summarizers for capability and reservation folds
  - CLI aggregator commands
  
  Deliverable: aggregators can be spawned for any subnet,
  publish summaries at configurable visibility.

Phase C (week 4):
  - Cross-subnet detail-on-demand RPC
  - Deck AGGREGATORS panel
  
  Deliverable: subscribers can query aggregators for fresh
  detail beyond what summaries carry.
```

**Total estimated effort:** 3-4 weeks for full implementation, with Phase A (CLI/Deck surfacing of existing machinery) being the minimum useful surface even without aggregators.

The further reduction from earlier drafts (5-7 weeks → 4-5 weeks → 3-4 weeks) comes entirely from recognizing that the substrate already has hierarchical subnets, the gateway, visibility per channel, and `SubnetPolicy`. The new work is the aggregator daemon, the detail-RPC service, and operator-facing surface.

---

## How this composes with the Multi-Fold Plan

The fold framework consumes the subnet primitives transparently:

**A fold's channels have visibility configs.** When a `CapabilityFold` instance is created, its channels (e.g., `fold:cap:h100`) are registered with appropriate `ChannelConfig::visibility`. Most fold channels are `SubnetLocal` by default; summary channels (published by aggregators) are `ParentVisible`, `Exported`, or `Global` as configured.

**Subscribers see what visibility allows.** A subscriber in `subnet=us-east.dc-1.rack-5` subscribed to `fold:cap:h100` with `Visibility::SubnetLocal` sees only the rack's announcements; with `ParentVisible`, the DC's; with `Global`, all.

**Aggregators bridge tiers using normal fold semantics.** An aggregator's source subscription is just a `Fold<K>` subscriber configured for `SubnetLocal` channels in its source subnet. Its summary publication is just a `Fold<K>` publisher writing to channels with higher visibility. No special bridging logic — the same fold primitives, used at both ends of the aggregation.

**The gateway handles cross-tier routing.** When a summary channel has `Visibility::ParentVisible`, the gateway in the source subnet forwards announcements upward; the parent subnet's nodes see them. No application-level cross-subnet routing logic needed.

This is the architectural payoff of using existing primitives: the fold framework doesn't have to know about subnets at all. Folds operate on channels; channels have visibility; visibility is enforced by gateways; gateways read packet headers. Each layer does one thing.

---

## What this gives you commercially

The pitch story:

**"Hierarchical subnets are at the wire layer, not bolted on."** `SubnetId` is in `NetHeader`. Bitwise hierarchy operations at wire speed. The substrate doesn't infer hierarchy from labels at runtime — it carries it explicitly on every packet.

**"Subnet boundary enforcement is the substrate's job, not the application's."** `SubnetGateway` makes forward/drop decisions reading only header fields. Applications don't write cross-subnet routing logic; they configure channel visibility and let the substrate handle it.

**"Aggregation is a daemon role, not specialized infrastructure."** Aggregators are `MeshDaemon` deployed via `ReplicaGroup`. Failover, placement, identity continuity all from existing primitives. New tier services (market matchers, settlement bridges, reputation oracles) follow the same pattern.

**"Scale by subdivision and summarization, both using primitives already in the substrate."** This is the architectural story, and it's true: the substrate scales because the substrate was built with scale in mind.

---

## Risks and mitigations

**Risk: subnet membership inconsistency across the mesh.** If node X thinks it's in subnet Y but other nodes don't see that, packets may route incorrectly.

Mitigation: subnet assignment via `SubnetPolicy` is deterministic from capability tags. Tags are announced via the capability advertisement mechanism (which exists and is well-tested). Membership changes propagate via the normal capability flow; eventually consistent within seconds.

**Risk: aggregator failures cause summary gaps.**

Mitigation: aggregators are deployed via `ReplicaGroup` with N replicas (typically 3) spread across failure domains within the source subnet. All replicas publish summaries independently; subscribers see continuous output as long as ≥1 replica is healthy. Node failure triggers `ReplicaGroup::on_node_failure`, which re-spawns the failed slot on a different node with the same derived identity. New replica rebuilds fold state from source channels within one TTL cycle (~30-60s); other replicas continue publishing during rebuild.

**Risk: summary staleness causes scheduling decisions on stale data.**

Mitigation: summaries carry a freshness timestamp (the announcement's `announced_at` field). Consumers decide whether to trust the summary or escalate to detail RPC based on staleness tolerance. For time-critical decisions, the RPC path provides fresh data at ~hundreds of ms latency.

**Risk: gateway becomes a bottleneck for high cross-subnet traffic.**

Mitigation: most traffic is intra-subnet by design (`Visibility::SubnetLocal` channels never cross gateways). Cross-subnet traffic is bounded by what `ParentVisible` / `Exported` / `Global` channels carry, which should be predominantly summaries (small, low-rate) rather than detail. Detail RPC is on-demand. If gateway capacity becomes constrained, the gateway can itself be deployed as a `ReplicaGroup` (multiple gateway nodes per subnet); load balances across them.

**Risk: misconfigured channel visibility leaks data across subnets.**

Mitigation: visibility is set at channel registration time. The gateway enforces it on every cross-subnet forward attempt. Misconfigurations are detectable via gateway drop counts and audit events. CLI commands to inspect channel visibility should be standard operational hygiene.

**Risk: subnet hierarchy depth (4 levels) is insufficient for some deployments.**

Mitigation: 4 levels × 256 values per level = 4 billion distinct subnet IDs. Sized for any realistic deployment. If a customer somehow needs more, the design could be extended (e.g., `SubnetId` as `u64` with 8 levels), but the existing 4-level scheme accommodates region/fleet/vehicle/subsystem semantics adequately.

---

## What this doesn't solve

A few honest limits:

**Per-aggregator capacity.** A single aggregator can handle ~10K detailed entries comfortably; past that, per-subnet aggregators may need sharding by fold kind or by class hash. Not addressed here; expected to be a concern past several million nodes per mesh.

**Cross-mesh federation.** Multiple meshes don't share subnets. If you operate multiple meshes (perhaps for organizational reasons), they're independent. Federation across meshes is a separate concern.

**Strongly consistent cross-subnet operations.** A scheduler wanting "atomically reserve one GPU in any of these regions" doesn't get strong consistency from this design. Same model as the multi-fold reservation pattern. Sufficient for most workloads; insufficient for some.

**Discovery of subnet topology by external tools.** Subnet topology is operator-defined and operator-discovered. Tools can inspect via the CLI, but there's no machine-readable topology export by default. Could be added if a use case emerges.

---

## Summary

Three pieces of work, totaling 3-4 weeks, that extend the substrate's existing primitives (hierarchical subnets, gateway, channel visibility, replica groups) to support millions of nodes per mesh through:

1. **`AggregatorDaemon`** — `MeshDaemon` deployed via `ReplicaGroup` for tier-bridging summarization.
2. **Cross-subnet detail-on-demand RPC** — service definition + client helper on top of existing RPC + gateway.
3. **CLI + Deck integration** — operator-facing surface for inspecting and managing subnet/gateway/aggregator state.

None of this is new architecture. All of it is more deliberate use of what exists. The substrate already has 4-level hierarchical subnets, gateway enforcement at boundaries, per-channel visibility scoping, label-based subnet assignment, partition correlation tracking, and `ReplicaGroup` for HA of any daemon role.

This composes with the Multi-Fold Plan: folds operate on channels; channels have visibility; visibility is enforced by gateways. The fold framework doesn't need to know about subnets at all. Each layer does one thing.

The architectural pattern that emerges is worth naming explicitly: **any role in the substrate that needs replication is a `ReplicaGroup` of `MeshDaemon` instances.** Aggregators are the first application of this pattern; future tier services (market matchers, settlement bridges, reputation oracles, gateway services) will use the same primitive. One HA mechanism, many daemon types.

Buildable, well-aligned with the existing codebase, and stronger as a scaling story than building specialized hierarchical infrastructure would have been — because the hierarchical infrastructure is already there.
