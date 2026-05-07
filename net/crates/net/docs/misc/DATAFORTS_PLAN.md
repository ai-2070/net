# Dataforts — Implementation Plan

> Companion to [`misc/DATAFORTS_FEATURES.md`](misc/DATAFORTS_FEATURES.md). The features doc is the audit: which 25-of-28 wishlist items already ship and which are genuinely new work. **This doc sequences the new work across two coordinated releases** — phase order, gate criteria, scope boundaries, design decisions to lock, test strategy, risks, and effort. The frame: every phase **stays parked until its activation gate fires**. This is a "we know what to build when we have to" plan, not a build-everything roadmap.

## Status

Design only. Nothing in this plan is in flight. The release split below ensures Rebel Yell (Dataforts) is a *thin compositional layer* on top of The Warriors (substrate foundations) rather than a separate engineering project. Phases inside The Warriors are the precondition for Rebel Yell to land cleanly.

## Release plan: The Warriors → Rebel Yell

The seven phases ship across two coordinated releases:

### The Warriors (precursor) — substrate foundations

Three pieces of work that turn the substrate's primitives into a structured foundation Dataforts can compose against:

1. **Capability taxonomy reorganization.** The flat capability-tag namespace becomes a typed three-axis ontology:
   - **`hardware`** — what the node *can do* compute-wise (CPU cores, GPU, RAM, NIC, storage). Objective, measurable.
   - **`software`** — what the node *currently runs* (models loaded, daemons installed, tools available). Configurable.
   - **`devices`** — custom semantic tags / role identifiers (e.g. `printer`, `temperature-sensor`, `brake-controller`, `LIDAR`, `pump`, `valve`). World-facing roles.
2. **Capability-tag discovery primitive (Phase 0).** Adds the `causal:`, `blob:`, `heat:`, `fork-of:` tag shapes plus bloom-filter aggregation. The discovery layer that collapses every later phase's coordination problem.
3. **Federated query primitives (Phase 6, restricted scope).** Query operators over the capability index — filter, match, traverse, aggregate. Not a full MeshDB; just the primitives Rebel Yell composes against. Full MeshDB stays parked until a workload demands it.
4. **RedEX V2 — raw log-segment replication (Phase 2).** The wire protocol (`SUBPROTOCOL_REDEX`) that v1 explicitly defers. Strong durability beyond single-node. Lands in The Warriors so Rebel Yell can rely on it.

### Rebel Yell (Dataforts) — thin compositional layer on top of The Warriors

After The Warriors, Dataforts is **just a 4th capability category** alongside hardware/software/devices — storage capacity + hosted causal chains advertised via the same tag namespace as compute capabilities. The remaining phases compose against the foundations:

5. **Greedy-LRU dataforts (Phase 1).** Now with three orthogonal filters: **scope label + proximity threshold + capability-preference** (intent-tagged replication — chains advertise `intent:ml-training` / `intent:sensor-telemetry` / etc.; greedy nodes pull chains whose intent matches their advertised capability set).
6. **Data gravity (Phase 4).** Heat-counter annotations on capability tags; gravity emerges from greedy + heat + capability-preference automatically. No separate migration engine.
7. **Content-addressable blob storage (Phase 3).** Independent track; can ship parallel with Warriors.
8. **Read-your-writes (Phase 5).** Optional, post-replication.

Post-Rebel-Yell capability ontology: **four orthogonal axes** (`hardware`, `software`, `devices`, `dataforts`) all queryable via the same federated query primitives. A user can issue a single composable query like `hardware.gpu AND software.model:llama-3-70b AND dataforts.has_chain:Y AND proximity < 50ms` — that is the visible product win Rebel Yell delivers.

## Why the split exists

Three reasons this needs to be sequenced as Warriors → Rebel Yell rather than shipped as one body of work:

1. **Foundation discipline.** Without the taxonomy reorganization and replication primitive in place, Rebel Yell would have to bolt them on per-phase, multiplying coordination cost. Building Warriors first is meaningfully cheaper than retrofitting.
2. **Most of the wishlist already ships.** The features doc audits 25-of-28 items as already-shipping or free-via-existing-primitives. Warriors prepares the few primitives that genuinely needed building; Rebel Yell composes against them.
3. **DST is the gating concern, not LoC.** Phase 2 (replication, in Warriors) and Phase 3 (blob CAS GC, in Rebel Yell) are gated by deterministic-simulation-test depth, per [`REDEX_PLAN.md`](REDEX_PLAN.md)'s explicit "needs a clear DST story" condition. Acknowledging that up front avoids surprise when the actual implementation hits the testing wall.

## TL;DR

Seven phases across two releases, sequenced by dependency:

| # | Phase | Release | Effort (focused) | Activation gate | Depends on |
|---|---|---|---|---|---|
| 0 | Capability-tag discovery + taxonomy reorganization | **Warriors** | 2–3 weeks | First time Warriors lands (foundation; unconditional within Warriors) | — |
| 6 | Federated query primitives | **Warriors** | 2–4 weeks (primitives only) | Foundation for Rebel Yell's cross-axis queries | 0 |
| 7 | Generalized 5-axis `PlacementFilter` + Mikoshi integration | **Warriors** | 1–2 weeks | Foundation for placement decisions across substrate (data + compute) | 0, 6 |
| 2 | RedEX V2 — raw log-segment replication | **Warriors** | 4–9 weeks | Workload needs durability beyond single node | 0, 7 |
| 1 | Greedy-LRU dataforts (composes `PlacementFilter`) | **Rebel Yell** | 1–2 weeks | Rebel Yell ships | 0, 7 |
| 4 | Data gravity (heat-counter migration) | **Rebel Yell** | 1–2 weeks | Production telemetry shows access skew Phase 1 doesn't absorb | 0, 1 |
| 3 | BlobRef + BlobAdapter hook trait | **Rebel Yell** (parallel-shippable with Warriors) | 1–2 weeks | Payloads systematically exceed inline threshold (default 1 MB) | 0 (independent of 1, 2) |
| 5 | Read-your-writes guarantees | **Rebel Yell** | 2–4 weeks | App ergonomics request session-bounded consistency | — |

Phase 4 collapsed from "3–6 weeks" to "1–2 weeks" once we accepted the features-doc framing of gravity as **emergent behavior of greedy + heat counters + capability-preference + colocation**, not a separate migration engine. Phase 6 collapsed from "research-grade; multiple months" to "2–4 weeks" once we restricted Warriors-scope to *primitives* (filter, match, traverse, aggregate operators over the capability index) — full MeshDB with time-travel, lineage walks, and cross-chain joins stays parked as a research-grade extension. Phase 3 collapsed from 6–12 weeks (full substrate-owned blob CAS) to 1–2 weeks (BlobRef + BlobAdapter hook trait) once we accepted the architectural separation: streaming + coordination is the substrate's job, blob storage is the customer's existing system's job (S3, IPFS, Ceph, etc.). Net carries the reference, never owns the bytes.

**Total focused effort:**
- **The Warriors:** ~8–16 weeks (capability work + replication + query primitives, parallel where possible)
- **Rebel Yell:** ~5–10 weeks if all phases activate (greedy + gravity + blob hook + RYW; mostly parallel-shippable)
- **Worst case:** ~4–6 months parallelised across both releases. **Likely real case:** Warriors only, with Rebel Yell phases activated reactively as workloads demand them.

---

## Phase 0 — Capability-tag discovery primitive

The unlock. The features doc identifies `causal:origin_hash[:tip_seq]` capability tags as the discovery layer that collapses every other deferred phase's coordination problem. Build it once; everything else routes through it.

### Scope

**Tag shapes.** Parsed forms, all encoded as opaque `Tag` values inside the existing `CapabilitySet.tags` set, organized under the Warriors-shipped four-axis taxonomy (`hardware`, `software`, `devices`, `dataforts`):

| Shape | Axis | Meaning |
|---|---|---|
| `causal:<32-byte hex of origin_hash>` | `dataforts` | "I hold (or will serve) this chain — current tip unknown / not advertised" |
| `causal:<hex>:<tip_seq>` | `dataforts` | "I hold this chain at least through `tip_seq`" |
| `causal:<hex>[<start>..<end>]` | `dataforts` | "I hold this chain across the `[start, end]` seq range" — for time-travel queries (Phase 6) |
| `fork-of:<parent_hex>` | `dataforts` | "This chain forked from `parent_hex` — for lineage/cohort queries" |
| `intent:<label>` | chain-side | "This chain is for X kind of work" — e.g. `intent:ml-training`, `intent:sensor-telemetry`, `intent:billing-settlement`. Drives capability-preference matching in Phase 1's greedy filter. |
| `colocate-with:<other_origin_hash>` | chain-side | "Place me on the same node as that chain (soft preference)." Drives causal-affinity placement in Phase 1's greedy filter. |
| `colocate-with-strict:<other_origin_hash>` | chain-side | Hard variant — refuses placement if target unavailable. |

Three non-shape extensions, all reserved keys on capability tags:

- `heat:<chain_hex>=<reads_per_window>` — heat counter for Phase 4. Annotated optionally; absence means "not advertising heat."
- `scope:<label>` — the existing scoped-capability tag (see `SCOPED_CAPABILITIES_PLAN.md`); reused by Phase 1's greedy filter.

(The blob CAS storage tag `blob:<hex>:<size>` referenced in earlier drafts is removed — Phase 3 ships as a `BlobAdapter` hook trait carrying URI + hash + size in event payloads, not as a substrate-owned blob tag.)

**Bloom-filter aggregation.** A node holding many chains advertises a bloom filter rather than enumerating each tag. Target: 10K chains in ≤ 500 KB; propagation cost ≤ 2× current capability-announcement budget. Adds a new optional field `CapabilitySet::chain_bloom: Option<BloomFilter>`. Nodes that match the bloom probe with a follow-up `causal:<hex>` precise lookup before issuing a real read.

**Re-announcement throttle.** Default policy: emit on whichever fires first — Δ`tip_seq` ≥ 1024 events OR Δt ≥ 10 s. Configurable per channel via `ChannelConfig::chain_announcement: Option<ChainAnnouncementPolicy>`. The chain itself self-verifies on actual read, so the advertisement is a discovery hint, not a security primitive — being slightly stale is recoverable.

**Withdrawal.** Capability index already supports tag removal. Producers wire it into:
- Greedy LRU evict (Phase 1)
- Replica drop (Phase 2)
- Blob GC (Phase 3)
- Graceful daemon shutdown

**ACL.** Tags are already gated by `subscribe_caps` per channel through AuthGuard. A node that lacks the cap for a channel cannot decrypt — and therefore cannot see — its chain advertisements. ACL compliance falls out for free; no new wire-level encryption.

### Concrete tasks

1. Extend `behavior::capability::Tag` parsing to recognize the four shapes above (and reject the reserved prefixes when applied at the wrong layer — e.g. user tags can't start with `causal:`).
2. Add the high-level helpers on `Mesh`:
   - `Mesh::announce_chain(origin_hash, tip_seq)` — bumps the local capability set, triggers announcement on the throttle.
   - `Mesh::announce_chain_range(origin_hash, start, end)` — range variant for historical advertising.
   - `Mesh::withdraw_chain(origin_hash)` — removes the tag, triggers withdrawal announcement.
   - `Mesh::find_chain_holders(origin_hash) -> Vec<NodeId>` — wraps the existing capability-index query, returns nearest-first by proximity.
3. Bloom-filter aggregation as an optional encoding when the per-node chain count crosses a threshold (default 256). Plumb `CapabilitySet::chain_bloom` end-to-end through announcement, propagation, and local indexing.
4. Per-channel announcement throttle (`ChannelConfig::chain_announcement`).
5. Binding surface: Node + Python + Go + C bindings get the new `announce_chain` / `withdraw_chain` / `find_chain_holders` calls. Mechanical; ~1 week per binding.

### Test strategy

- **Unit.** Tag parse round-trip; bloom-filter false-positive rate at target sizes (10K chains, ≤ 1% FPR at 500 KB); throttle behavior under burst; withdrawal idempotency.
- **Integration.** 4-node mesh, 1 publisher, 3 observers. Assert observer indexes converge within a heartbeat interval; assert announcement bandwidth bounded under saturating chain creation.
- **Negative ACL.** A node without `subscribe_caps` for the channel never sees the corresponding `causal:` tag, even via bloom-filter aggregate.
- **DST hook.** Add the announce/withdraw operations to the existing `loom_models.rs` model surface so later phases can drive announcement traffic deterministically in their failure-injection tests.

### Risks

- **Bloom false positives → spurious read attempts.** Mitigation: read path must already handle "node advertised but doesn't have it" (Phase 2 makes this explicit; Phase 1 retries elsewhere). Falsely advertised is a recoverable miss, not a correctness bug.
- **Re-announcement traffic if throttle is too aggressive.** Mitigation: ship with conservative defaults (1024 events / 10 s); expose tunables; add the `dataforts_chain_advertisement_bytes` metric so saturation is visible.
- **Tag space pollution.** Reserved-prefix policing only works if it's enforced. Add a `Tag::reserved_prefix()` check on every external-facing tag write path; reject with `CapabilityError::ReservedPrefix` in tests.

### Effort

1–2 focused weeks. ~600 LoC core + ~400 LoC tests + ~3 weeks across all four bindings (parallelizable). **No state-machine work.** No DST harness work beyond the model-surface hooks.

### Activation gate

Land the first time any of Phases 1–4, 6 activates. Cheap enough that we should ship speculatively if even one of those phases is on the near-term horizon.

---

## Phase 1 — Greedy-LRU dataforts (Rebel Yell)

A node observes streams flowing past via the existing tail subscription path. If it has spare disk, ACL access, a scope match, AND its capability set matches the chain's intent, it caches a copy. When disk fills, LRU evicts. Withdraws the capability tag on eviction so reads route elsewhere. **No coordination protocol.** Smallest Rebel Yell phase; ships fastest; covers 60–80% of the perceived-durability story without orchestrated replication.

### Scope

- Local-only opt-in via `MeshNode::enable_greedy_dataforts(GreedyConfig)`.
- Configurable via `GreedyConfig`:
  ```rust
  pub struct GreedyConfig {
      pub scopes: Vec<ScopeLabel>,                // e.g. ["scope:industrial-telemetry"]
      pub proximity_max_rtt: Duration,             // e.g. 200ms
      pub per_channel_cap_bytes: u64,              // default 100 MB
      pub total_cap_bytes: u64,                    // default 10 GB
      pub bandwidth_budget_fraction: f32,          // default 0.25 of measured NIC peak
      pub intent_match: IntentMatchPolicy,         // default ::AnyOfLocalCapabilities
      pub colocation_policy: ColocationPolicy,     // default ::SoftPreference (boost score on match)
  }

  pub enum ColocationPolicy {
      Ignore,             // colocate-with: tags ignored
      SoftPreference,     // boost placement scoring on affinity match (default)
      StrictRequired,     // refuse placement unless target chain is local
  }
  ```
- Cache substrate: a per-channel RedEX file with a size cap. Caches are normal RedEX files, just owned by the cache layer instead of the application. Reuse v1 retention machinery (`Retention::Bytes`) for the size cap.
- **Pull condition is a quintuple AND** (per features-doc spec, extended with Rebel Yell's capability-preference and colocation dimensions):
  1. **Scope match** — the chain advertises a `scope:` tag matching one of the local node's configured scopes.
  2. **Proximity bound** — the chain's home is within `proximity_max_rtt` per the existing proximity graph.
  3. **Capability-preference match (intent-tagged replication)** — the chain advertises an `intent:` tag (e.g. `intent:ml-training`, `intent:sensor-telemetry`, `intent:billing-settlement`); the local node's advertised capability set (`hardware`, `software`, `devices` axes from The Warriors taxonomy) must include capabilities that *fulfill* that intent. Defaults: a GPU-rich node fulfills `intent:ml-training`; an edge node with sensor `devices` tags fulfills `intent:sensor-telemetry`; a stable datacenter node fulfills `intent:billing-settlement`. Concrete intent-to-capability mappings live in a small lookup table (`adapter::net::dataforts::intent`); applications may register custom intents. **This is the dimension that produces emergent specialization** — different node fleets become specialized for different workloads automatically because their capability sets fulfill different intents.
  4. **Colocation preference (causal-chain affinity)** — if the chain advertises a `colocate-with:<other_origin_hash>` tag and the local node already holds (or already replicates) the target chain, the chain prefers to land here. **Default behavior is a soft scoring boost**, not a hard gate — colocation tilts placement toward affinity but doesn't override capacity constraints. A strict variant `colocate-with-strict:<hash>` is available for hard requirements (refuses placement if target is unavailable). Use cases: chained processing pipelines (`A → B → C` colocated on one node minimizes hops); fork chains colocated with parents for fast lineage walks; cohort chains for multi-channel correlation analytics.
  5. **Storage available** — local node decision; LRU eviction when the total cap is hit.
- ACL gating falls through automatically — only chains with valid `subscribe_caps` reach the inbound observe path; the cache layer just inherits.
- Per-chain advertisement on first cache, withdrawal on full eviction. Phase 0 carries the announcements.

**What this produces:** replication routed by *purpose* AND *affinity*, not just by past usage. Training data gravitates toward GPU nodes regardless of historical read patterns. Sensor data gravitates toward edge nodes regardless of where historical analytics ran. Causally-related chains stay colocated, minimizing cross-node hops for chained processing. Different node fleets become specialized for different workloads automatically; chains that should be processed together stay together automatically.

### Concrete tasks

1. New module `adapter::net::dataforts::greedy` with `GreedyCache` struct.
2. Hook into `MeshNode::dispatch_event` (the inbound delivery point used by per-shard inbound queues): if greedy is enabled AND node has cap AND scope matches AND proximity gate passes AND total cache below soft cap → write to cache file in addition to the application's tail.
3. Tag emission via Phase 0's `Mesh::announce_chain` on first append per chain; tag withdrawal on full eviction via `Mesh::withdraw_chain`.
4. **Read path serves from cache.** When the local node holds a chain in greedy cache and a remote read would otherwise route there, serve from cache directly. Add a `Mesh::greedy_serve_count` metric so the cache-hit win is measurable from day one.
5. Bandwidth budget enforcer — bound total greedy I/O at `bandwidth_budget_fraction × measured_NIC_peak`; back off if greedy starts crowding application traffic. Reuse the existing rate-limit primitive.

### Test strategy

- **Steady-state.** 3-node mesh, 1 publisher, 2 greedy observers. Stream 100 MB through the channel. Both observers cache; tag advertisements reach every other node within a heartbeat; reads from a 4th node route to the nearest cacher.
- **Eviction.** Fill the cache to 110% of cap, drive evictions, assert tag withdrawals. Assert in-flight reads against an evicted entry transparently retry to a different holder.
- **Scope filter.** Configure node A with `scope:industrial-telemetry`; publish a chain tagged `scope:webcam-streams`; assert A does NOT cache.
- **Proximity gate.** Inject `RTT > proximity_max_rtt` via the simulation harness; assert no cache pull happens.
- **ACL.** 3rd node without `subscribe_caps` does NOT cache. Pin via cache file size = 0 byte.
- **Bandwidth budget.** Under saturating publisher load, greedy I/O does not exceed configured fraction. Treat regression as test failure.

### Risks

- **Cache write amplification under bursty publishers** (every observer writes the same data). Mitigation: greedy is opt-in per node; not all nodes turn it on. If telemetry shows amplification problems in production, add randomised admission control as a follow-up.
- **LRU thrashing under uniform-random access.** Mitigation: standard LRU pathology, not specific to dataforts. Document; revisit only if telemetry shows it.
- **Hot-spot pile-on.** A very popular chain is cached by *every* node in scope, wasting disk. Mitigation: per-scope replica budget — cap the number of in-scope cachers via the proximity graph's existing summarisation. Defer until telemetry shows the problem.

### Effort

1–2 focused weeks (per features-doc estimate, post-Phase-0 collapse). ~900 LoC core + ~500 LoC tests. No DST harness work.

### Activation gate

Pilot deployment requests "make popular data fast without standing up replica groups." Realistic trigger and the path of least resistance for data-locality wins.

---

## Phase 7 — Generalized 5-axis `PlacementFilter` primitive + Mikoshi integration (The Warriors)

**Placement is a substrate primitive, not a per-feature decision.** The 5-axis filter (scope + proximity + capability-preference + colocation + resource-availability) generalizes from "data placement" to "compute placement" — the same primitive applies to chains (caching), replicas, daemons (Mikoshi migrations), and replica/fork/standby group members. Build it once in The Warriors; everything Rebel Yell composes inherits it; future features (scheduler, mesh-wide load balancing, etc.) reuse it.

### Scope

A trait surface in `behavior::placement` plus integration into Mikoshi's existing migration logic.

```rust
pub trait PlacementFilter: Send + Sync {
    /// Score a candidate node for placement of an artifact.
    /// Returns `None` if the node is ineligible (hard constraint failed);
    /// returns `Some(score)` where higher = better fit.
    fn placement_score(&self, target: &NodeId, artifact: &Artifact) -> Option<f32>;
}

pub enum Artifact<'a> {
    Chain { origin_hash: [u8; 32], tags: &'a CapabilitySet },
    Replica { channel: &'a ChannelName, tags: &'a CapabilitySet },
    Daemon { daemon_id: [u8; 32], required: &'a CapabilitySet, optional: &'a CapabilitySet },
}

pub struct StandardPlacement {
    pub scope_filter: Option<Vec<ScopeLabel>>,
    pub proximity_max_rtt: Option<Duration>,
    pub intent_match: IntentMatchPolicy,
    pub colocation_policy: ColocationPolicy,
    pub resource_axis: ResourceAxis,        // Storage | Compute | Both
}
```

The reference implementation `StandardPlacement` evaluates all 5 axes:

1. **Scope** — `scope:` tag match between artifact and target node.
2. **Proximity** — RTT bound via the existing proximity graph.
3. **Capability-preference** — `intent:` tag on artifact mapped to required capabilities (`hardware`, `software`, `devices`); target must include all required.
4. **Colocation** — `colocate-with:` / `colocate-with-strict:` tags on artifact resolved against target's local holdings or already-replicated chains.
5. **Resource-availability** — varies by artifact:
   - Chain / Replica → free storage capacity (advertised via `dataforts.free_storage:` tag)
   - Daemon → free compute capacity (CPU cores, available RAM, GPU/VRAM if required)
   - Choose via `ResourceAxis::Storage | ResourceAxis::Compute | ResourceAxis::Both`

### Mikoshi integration

Mikoshi today selects migration targets ad-hoc / single-node. After this phase, Mikoshi consults `PlacementFilter` to rank candidate targets:

```rust
impl Mikoshi {
    fn select_migration_target(&self, daemon: &Daemon, filter: &dyn PlacementFilter) -> Option<NodeId> {
        self.candidate_nodes()
            .filter_map(|node| {
                filter.placement_score(&node, &Artifact::Daemon {
                    daemon_id: daemon.id,
                    required: &daemon.required_capabilities,
                    optional: &daemon.optional_capabilities,
                }).map(|score| (node, score))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Equal))
            .map(|(node, _)| node)
    }
}
```

Same filter, same scoring, applied to compute placement instead of data placement. Replica/fork/standby groups inherit the same logic for their member-placement decisions.

### Concrete tasks

1. New module `behavior::placement` with `PlacementFilter` trait + `Artifact` enum + `StandardPlacement` reference impl.
2. `IntentMatchPolicy` + `ColocationPolicy` definitions (used by both Phase 1 in Rebel Yell and this phase in Warriors; declared here, consumed in both).
3. Intent → required-capabilities lookup table (`adapter::net::placement::intent`), application-extensible.
4. Mikoshi extended: `Mikoshi::select_migration_target` consults `PlacementFilter`; legacy ad-hoc selection becomes a `LegacyPlacement` impl preserved for backward compatibility under a feature flag.
5. Replica/fork/standby groups extended to use `PlacementFilter` for member placement.
6. Bindings: `PlacementFilter`, `StandardPlacement`, `IntentMatchPolicy`, `ColocationPolicy` callable in Node + Python + Go + C bindings. Application-implemented filters cross binding boundary via callback interface (same shape as `BlobAdapter`).

### Test strategy

- **Unit.** `StandardPlacement` returns expected scores for each of the 5 axes independently (turn off the others, vary one). Composability — multi-axis evaluation matches the product of single-axis evaluations.
- **Mikoshi integration.** Daemon with `required: hardware.gpu` migrates to a GPU node; daemon with `intent:sensor-telemetry` migrates to a node with sensor `devices` tags; daemon with `colocate-with:<chain_X>` migrates to the node holding chain X.
- **Group placement.** Replica group of size 3 spreads across nodes per `StandardPlacement` scoring; standby group's promote-on-failure picks the highest-scoring surviving member.
- **Cross-axis composition.** A daemon with `intent:ml-training` AND `scope:experiment-cluster-A` AND `colocate-with:<dataset_chain>` lands on a node satisfying all three, even when individual axes alone would route elsewhere.

### Risks

- **Score function tuning.** The 5-axis weights interact non-trivially. Mitigation: ship sane defaults; expose tunables; add `placement_score_distribution` metrics so operators can see how scores distribute in production.
- **Backwards compatibility.** Existing Mikoshi migrations must not regress in single-node deployments. Mitigation: legacy fallback under feature flag; migrate-by-default to the new path with an opt-out for one minor version.
- **Capacity advertisement freshness.** Daemons placed based on `compute_free` tags only as fresh as the announcement throttle. Mitigation: tighter throttle for capacity tags (default 1s) than for chain tags; document the freshness floor.

### Effort

1–2 focused weeks. ~600 LoC core (trait + impl + intent table + Mikoshi integration) + ~600 LoC tests + ~3 days per binding.

### Activation gate

Ships unconditionally with The Warriors. The trait + reference impl + Mikoshi integration are all foundation work — they enable everything Rebel Yell composes on top, plus all current and future placement decisions across the substrate.

---

## Phase 2 — Raw RedEX log-segment replication (RedEX V2, The Warriors)

Orchestrated replication. N replicas of a channel's RedEX file maintained explicitly; configurable replication factor; pull/repair on divergence; documented conflict policy (none expected because RedEX is append-only and seq-ordered, but the protocol must say so explicitly). Strong durability guarantee, in contrast to Phase 1's probabilistic one.

This phase is the heaviest one in the plan because it lands the wire protocol (`SUBPROTOCOL_REDEX`) that v1 explicitly defers and because DST coverage for partition / failover / rejoin is non-negotiable. **It ships in The Warriors release** as a foundation for everything Rebel Yell composes on top — Rebel Yell's gravity, capability-preference replication, and federated reads all assume RedEX V2 is in place.

**Capability-preference integration with Rebel Yell.** When Rebel Yell ships, replica placement uses the same dimensions as Phase 1's greedy filter — scope + proximity + capability-preference (intent matching) + heat. The placement strategy `PlacementStrategy::IntentWeighted` (added in Rebel Yell, not Warriors) routes replicas toward nodes whose capability sets fulfill the chain's `intent:` tag. The Warriors-shipped replication primitive simply needs to expose a placement-policy hook; the intent-matching logic plugs in when Rebel Yell activates.

### Scope

- New `ChannelConfig::replication: Option<ReplicationConfig>`:
  ```rust
  pub struct ReplicationConfig {
      pub factor: u8,                          // e.g. 3
      pub placement: PlacementStrategy,        // Spread, ProximityWeighted, Pinned(Vec<NodeId>)
      pub heartbeat_ms: u64,                   // default 500
      pub leader_pinned: Option<NodeId>,       // optional override
      pub on_under_capacity: UnderCapacity,    // Withdraw | EvictOldest
  }
  ```
- **Replica election.** N nodes from the capability-advertising set, weighted by proximity + free capacity. Elections happen on first publish AND on roster change.
- **Wire protocol.** New `SUBPROTOCOL_REDEX` (the v1 plan explicitly defers this). Rides on the existing reliable-stream `Mesh::publish` machinery; adds a dispatch-byte range:
  - `DISPATCH_REPLICA_SYNC = 0x20..0x2F` — 16 codes reserved; v1 uses 4: `SYNC_REQUEST`, `SYNC_RESPONSE`, `SYNC_HEARTBEAT`, `LEADER_ELECTION`.
- **Pull-based catch-up.** Replica observes its current `tail_seq`; requests `(channel, since_seq)`; leader responds with a bounded `read_range` (default cap 1 MB per request). Replica streams in until it converges.
- **Repair.** Replicas heartbeat their `tail_seq` to leader; leader detects gap, replays gap to replica. Heartbeat interval = `heartbeat_ms` (default 500).
- **Failover.** If leader fails (proximity graph reports `Unhealthy` or heartbeats time out), surviving replicas elect a new leader via the existing standby-group election. **Reuses `groups::standby` machinery.** No new election primitive.
- **Conflict policy.** Append-only + monotonic seq → no conflicts possible IF leader is the sole appender. Document this assumption explicitly; reject "writes" from non-leader replicas with `RedexError::NotLeader`.

### Concrete tasks

1. New `ReplicationCoordinator` daemon spawned per replicated channel on each replica.
2. Wire format: extend `EventMeta::dispatch` with `DISPATCH_REPLICA_SYNC` codes. Document each in `SUBPROTOCOLS.md`.
3. Pull-based catch-up — replica computes gap from heartbeat ack mismatch; issues `SYNC_REQUEST`; leader responds with bounded `read_range`. Reuses RedEX's existing `read_range` API end-to-end.
4. Conflict policy enforcement — `append` on non-leader returns `RedexError::NotLeader`. Pin in tests.
5. Failover integration — wire the standby-group election to a replication-divergence trigger (leader heartbeat lost OR replica reports unrecoverable seq gap).
6. Replica withdrawal — drop replication on graceful shutdown (`Coordinator::Drop`). Capability tag withdrawn via Phase 0's `Mesh::withdraw_chain`.
7. Per-channel replication metrics on the existing `RpcMetricsRegistry` shape: `dataforts_replication_lag_seconds`, `dataforts_replication_sync_bytes_total`, `dataforts_leader_changes_total`.
8. Cross-binding work: `ChannelConfig::replication` must round-trip through Node, Python, Go, C bindings. Mostly serde plumbing.

### Open design questions to lock before implementation

These are real decisions. Don't start the implementation without explicit answers; cost of getting them wrong is days of rework each.

- **Leader scope.** Is the replication leader the same as the `ChannelPublisher`'s home, or a separately-elected entity? **Recommendation:** same node by default (publisher is the natural leader for an append-only channel), with explicit override via `leader_pinned: Option<NodeId>` for split publisher/leader topologies. Pin in test.
- **What does "replicated" mean for retention?** If a channel retains 100 MB and a replica drops below that under disk pressure, does it withdraw replicaship or evict the oldest local data? **Recommendation:** `UnderCapacity::Withdraw` as default — fall through to greedy LRU if also enabled. `UnderCapacity::EvictOldest` available as opt-in. Replication factor is a hard guarantee on the leader; replicas are best-effort under capacity. Caller picks.
- **Cross-segment atomicity.** Per `REDEX_PLAN.md` non-goal #23, RedEX has no cross-segment atomicity. Replication must NOT introduce that expectation; replicas catch up segment-by-segment. Document explicitly in `SUBPROTOCOLS.md`.
- **Membership during partition.** If a replica is partitioned but eventually rejoins, does it re-catch-up from current tail or replay the gap? **Recommendation:** replay gap if `gap < skip_threshold` (default 100 MB); skip-ahead + flag for divergence audit if larger. Reuses standby-group's replay machinery.
- **Bandwidth budget.** Replication sync rides on the same wire as application traffic. Cap replication-sync I/O at `replication_budget_fraction × NIC peak` (default 0.5). Backpressure-aware via reliable-stream's existing flow control.

### Test strategy

- **Steady-state convergence.** 3-replica + 1-publisher mesh. Continuous appends; assert all replicas converge to leader's tail within `heartbeat_ms × 2`.
- **Failover.** Kill leader; assert one replica promotes; new appends land on new leader; old leader on rejoin catches up (and does not over-promote).
- **Disk pressure.** Replica configured below leader's retention; assert graceful withdrawal, NOT silent corruption. Both `Withdraw` and `EvictOldest` policies covered.
- **DST coverage** — *the gating concern.* Random partition + restart sequences via `loom_models.rs` extension. Asserts: all surviving replicas converge eventually; no two replicas declare different `tail_seq` for the same `seq` (stronger than convergence — divergence-freedom).
- **Performance budget.** Replication overhead ≤ 30% of single-node append throughput at steady state. Replication-sync I/O ≤ 50% of NIC peak under saturating append rate. Treat regression as test failure.
- **Leader pinning.** With `leader_pinned: Some(N)`, election always returns N when N is healthy.

### Risks

- **DST story is the hardest part.** No replication design survives without a DST plan for partition + leader-flap + rejoin sequences. **Allocate ~30% of phase effort to DST harness work.** The features doc explicitly cites `REDEX_PLAN.md`'s "needs a clear DST story" as the gating condition; this phase is where we pay that cost.
- **Leader concentration → write hotspots.** Mitigation: per-channel leader, not per-node; large deployments distribute leadership naturally. If we see a single node leading > 30% of channels in production telemetry, add anti-affinity to placement.
- **Subprotocol code surface adds ~1500–2000 LoC to the mesh adapter.** Audit footprint before merge. Coordinator daemons should compose from existing primitives, not invent new ones.
- **Election thrash.** Aggressive heartbeat timeouts cause spurious elections under transient packet loss. Mitigation: hysteresis on leader-loss detection (3 consecutive missed heartbeats by default); pin in DST.

### Effort

4–9 focused weeks. Wide range driven by DST harness depth. ~2500 LoC core + ~3500 LoC tests + ~2 weeks DST harness extension. Bindings are mostly serde for `ReplicationConfig`; ~3 days each.

### Activation gate

Workload requesting durability guarantees beyond single-node, where Phase 1's probabilistic story is insufficient. Realistic triggers: payment-tier customer; compliance-bound data class; pilot whose RTO is "< 5 s on node failure."

---

## Phase 3 — BlobRef + BlobAdapter hook trait (Rebel Yell)

**Decision: do not build a substrate-owned blob CAS layer.** The substrate is streaming + coordination + metadata + lineage. Blob storage is a fundamentally different data shape (object PUT/GET, byte-range reads, immutable artifacts). Forcing blob CAS into a streaming substrate creates impedance mismatch.

**The 2 TB constraint as the design boundary.** Modern server memory ranges from 256 GB (mid-tier) to 8 TB (Epyc 9684X with 12 DIMMs). If a single payload exceeds memory, you're in object-storage territory, not streaming territory. **Net should not transfer what cannot fit in server memory.** For payloads beyond that, integration with the customer's existing object storage (S3, R2, B2, Ceph, IPFS, on-prem ceph cluster, NetApp, Isilon) is the right answer.

This phase delivers integration *hooks*, not a storage system. The substrate carries a content-addressed *reference* through events; bytes live wherever the customer already stores them.

### Scope

- **`BlobRef` reference type.** Carried inline in RedEX events when payloads exceed an inline threshold (default 1 MB):
  ```rust
  pub struct BlobRef {
      pub uri: String,    // s3://bucket/key, ceph://cluster/object, file:///path, ipfs://CID, custom
      pub hash: [u8; 32], // BLAKE3 for content verification on fetch
      pub size: u64,
  }
  ```
- **`BlobAdapter` trait.** Customer-implemented integration with their preferred storage backend:
  ```rust
  pub trait BlobAdapter: Send + Sync {
      fn store(&self, blob: &[u8]) -> Result<BlobRef>;
      fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>>;
      fn fetch_range(&self, blob_ref: &BlobRef, range: Range<u64>) -> Result<Vec<u8>>;
      fn exists(&self, blob_ref: &BlobRef) -> bool;
  }
  ```
- **Hash verification on fetch.** When a `BlobAdapter::fetch` returns bytes, the substrate verifies the BLAKE3 hash before delivering to the application. Tampering / corruption / wrong-blob-returned all surface as `BlobError::HashMismatch`.
- **Read path integration.** RedEX events with a `BlobRef` payload route through the adapter on read; events with inline payloads use the existing path. No new RedEX flag required if `BlobRef` is encoded as an event-level type discriminator.
- **No GC, no refcount, no CAS pool, no blob discovery via capability tags.** All of those are the customer's storage system's responsibility (S3 lifecycle policies, IPFS pinning, Ceph PG management). The substrate stays out of it.
- **Size threshold.** Configurable per-channel via `ChannelConfig::blob_threshold: u64` (default 1 MB). Below threshold: inline payload as today. Above threshold: caller responsible for storing via `BlobAdapter::store` and emitting an event with the returned `BlobRef`.
- **Reference adapters provided in the SDK.** Out of the box: `S3Adapter`, `FileSystemAdapter`, `IpfsAdapter`, `NoopAdapter` (for testing). Customers can implement their own for proprietary backends.

### Concrete tasks

1. New module `adapter::net::dataforts::blob` with `BlobRef` and `BlobAdapter` definitions.
2. Encode `BlobRef` as a typed event payload — discriminator byte + serde-encoded URI/hash/size.
3. Read path: when an event payload deserializes as `BlobRef`, dispatch to the configured `BlobAdapter` for resolution.
4. Hash verification — `BLAKE3` of the fetched bytes must match the `BlobRef::hash`; return `BlobError::HashMismatch` on divergence.
5. Reference adapters: `S3Adapter` (uses `aws-sdk-s3`), `FileSystemAdapter` (paths only; opt-in for trusted-host scenarios), `IpfsAdapter` (uses local IPFS daemon HTTP API), `NoopAdapter` (testing).
6. Bindings: `BlobRef`, `BlobAdapter` callable in Node + Python + Go + C bindings. Customer-implemented adapters cross the binding boundary via callback interfaces.

### Open design questions

- **Range fetch encoding.** Should `BlobAdapter::fetch_range` be in the trait, or should range fetches require multiple full fetches? **Recommendation:** in the trait — most modern blob backends support byte-range natively (S3 GET with Range header, IPFS HTTP, file `seek`); not exposing it leaves performance on the table for video / imagery use cases.
- **Async vs sync trait.** Customer adapters may need to be async for proper backend integration. **Recommendation:** trait is async (`async fn`); sync adapters wrap with `tokio::task::block_in_place`.
- **Encryption at rest.** Do we encrypt blob bytes before sending to the adapter, or trust the adapter's own encryption? **Recommendation:** trust the adapter — substrate-level encryption would defeat dedup at the adapter layer (S3 server-side encryption, IPFS encryption-at-rest, etc.). Caller's choice if they need substrate-level on top.

### Test strategy

- **Unit.** `BlobRef` round-trip; hash verification fail-fast on tampered bytes; size threshold gating; inline-vs-blob dispatch correctness.
- **Adapter conformance.** All four reference adapters pass the same conformance test (store → fetch → exists → fetch_range correctness). Customers implementing their own adapters use this suite.
- **Integration.** 3-node mesh, publisher emits 10 events with 10 MB `BlobRef` payloads to S3-backed `BlobAdapter`. Subscriber on 4th node receives events, resolves `BlobRef`s via local `S3Adapter`, verifies hashes, delivers to app.
- **Hash mismatch.** Inject corrupted bytes from the adapter; assert `BlobError::HashMismatch` returned, no app delivery.
- **Backend independence.** Same test suite runs against `S3Adapter`, `FileSystemAdapter`, `IpfsAdapter` — adapter is interchangeable.

### Risks

- **Customer's storage backend becomes a mesh dependency.** If their S3 bucket is misconfigured / their IPFS daemon dies, blob fetches fail. Mitigation: surface adapter health via metrics; document that BlobRef resolution is *not* mesh-resilient — it's the customer's storage layer's responsibility.
- **URI scheme drift.** Different backends use different URI schemes; standardising is non-goal. Mitigation: `BlobAdapter` is a per-channel-or-per-node config; mismatched URIs surface as `BlobError::UnsupportedScheme`. Caller picks one adapter per deployment.
- **Hash algorithm churn.** BLAKE3 is the choice today; if it gets superseded, `BlobRef` versioning is needed. Mitigation: reserve a version byte in the encoded form; ignore today, parse on next algorithm.

### Effort

**1–2 focused weeks.** ~400 LoC core (trait + ref type + dispatch + hash verify) + ~600 LoC tests + ~400 LoC reference adapters. Bindings ~3 days each (the callback interface for customer-implemented adapters is the only non-trivial cross-binding work).

Down from 6–12 weeks for a full substrate-owned blob CAS. The savings come from not building: the local CAS pool, refcount tracking, GC, blob-discovery wire protocol, dedup logic, ACL-aware blob sharing, and the DST coverage all of those would require.

### Activation gate

Workload with payloads ≥ MB-class. Realistic triggers: customers integrating media / imagery / model-artefact pipelines via the substrate.

### Independence

Doesn't depend on Phases 1, 2, or 4. Can run in parallel with The Warriors if the team has bandwidth.

### Deferred-but-named: full substrate-owned blob CAS

If a customer specifically cannot use any existing blob backend (extreme isolation, novel content-addressed storage requirements, etc.), a full mesh-owned CAS layer remains theoretically possible as a research-grade extension. The original 6–12 week plan for that work is preserved in the doc history. **Not in either Warriors or Rebel Yell.** Activates only if a workload genuinely requires it, which is unlikely.

---

## Phase 4 — Data gravity (heat-counter migration)

Once Phases 0 + 1 ship, the mesh has the substrate to observe which chains are most-read. Phase 4 closes the loop: nodes pull data toward themselves when read pressure justifies it. The features doc reframed this as **emergent behavior of greedy + heat counters**, not a separate migration engine — which collapses the effort estimate dramatically.

### Scope

**Heat counter as a capability-tag annotation.** Each chain's capability tag carries an optional `heat:<chain_hex>=<reads_per_window>` field, propagated via the existing capability-announcement machinery. Phase 1's greedy LRU treats high-heat in-scope chains as preferred pull candidates. More replicas in the high-heat zone → reads served locally → reads stop crossing zones → chain "gravitates" toward the zone where it's actually consumed.

**No separate migration engine.** Two primitives compose into the desired property:
1. Phase 0 advertises chains as capability tags.
2. Phase 1 pulls chains within scope+proximity+budget.

Adding a heat counter to (1) and a heat-weighted preference in (2) gets gravity for free.

### Concrete tasks

1. **Per-chain read-rate counter** on every read path. Local aggregation; window = rolling 1 h; TTL/decay function on the counter (default: half-life 30 min).
2. **Heat tag emission.** When local read rate for a chain crosses an emission threshold, annotate the existing `causal:` tag with a `heat:` field. Reuses Phase 0's announcement throttle.
3. **Heat-weighted greedy preference.** In Phase 1's pull-candidate selection, sort by `heat × scope-match × proximity-rank`. High-heat in-scope chains pull preferentially; cold chains evict first under LRU pressure.
4. **Hysteresis on emission.** Don't toggle the heat tag every announcement window — emit only when the heat bucket crosses a threshold (default: ×2 change since last emission, or `0` → withdraw).
5. **Configurable per-channel** via `ChannelConfig::data_gravity: Option<DataGravityPolicy>`.

### Open design questions

- **Telemetry scope.** Per-chain or per-`(channel, byte-range)`? **Recommendation:** per-chain to start; byte-range is a future optimisation and can be layered in without breaking the tag shape.
- **Anti-thrash.** Hysteresis: pull threshold > evict threshold + 2× (conservative). Document the gap. Pin in test under uniform-random access — must NOT thrash.
- **Mesh-wide vs. node-local decision.** Local decision is simpler (decentralised, no consensus). Mesh-wide could optimise replica placement globally but requires coordination. **Recommendation:** local-only for v1; revisit if telemetry shows gaps. (This is the choice that most aggressively collapses the effort estimate — don't backslide.)
- **Heat across ACL boundaries.** A node observing reads from a peer that lacks `subscribe_caps` for the chain shouldn't count those reads. Already handled — AuthGuard rejects the read before it reaches the counter — but pin in test.

### Test strategy

- **Emergent gravity.** 5-node mesh; 1 publisher; 4 readers all in the same scope but on different proximity-distant nodes. Inject a read-skew: 80% of reads come from node 4. Assert that node 4 starts caching the chain within 2 announcement windows; assert that subsequent reads from node 4 are served locally.
- **Anti-thrash.** Uniform-random access pattern across 100 chains. Assert that no chain oscillates between cached / not-cached more than 1× per hour (well below the natural LRU churn rate).
- **Hysteresis.** Heat bumps below threshold do NOT trigger re-announcement. Pin via metric `dataforts_chain_announcements_total` not bumping.
- **ACL.** Reads rejected by AuthGuard do not increment heat. Pin via fault injection.

### Risks

- **Heat metric becoming a privacy leak.** Read patterns are sensitive. Mitigation: heat tags scoped via the existing `subscribe_caps` ACL; only nodes with cap see heat. Pin in test.
- **Heat-driven thrash if cooldowns are wrong.** Mitigation: hysteresis (above) + decay half-life. Default conservatively; tune via telemetry.

### Effort

1–2 focused weeks atop Phases 0 + 1. Most of the work is the heat-counter + emission throttle; greedy-preference change in Phase 1 is ~50 lines. ~400 LoC core + ~600 LoC tests.

### Activation gate

Production telemetry showing access skew Phase 1's purely-greedy LRU doesn't capture (e.g. read patterns where greedy nodes don't happen to sit on the routing path). Until we have that telemetry, this is a paper exercise.

---

## Phase 5 — Read-your-writes guarantees

Independent of all other phases. Smallest scope. Useful when application semantics require the writer to immediately see its own writes (currently the system is causally consistent with no RYW guarantee — a writer may briefly observe state lagging its own publish).

### Scope

- **Session-token API.** Writers receive a `WriteToken { origin_hash, seq }` on every publish; readers can present it to a read API that blocks until the local fold has applied that seq.
- **`CortexAdapter::wait_for_seq(seq, deadline).await`** — uses the existing tail-fold notify primitive. `applied_through_seq` is already tracked in CortEX; this just exposes the wait.
- **Bound.** Deadline on the wait; surface `RpcError::Timeout` if applied seq doesn't catch up. Default 1 s; configurable per call.

### Concrete tasks

1. `WriteToken` type encoding `(origin_hash, seq)`; emit from `RedexFile::append` and the mesh's `publish` path.
2. `CortexAdapter::wait_for_seq(seq, deadline)` — uses existing tail-fold notify primitive. No new locking.
3. Higher-level wrappers: `MeshNode::publish_with_token` returns the token; `MeshNode::read_at_token` waits on the relevant adapter.
4. Bindings — token type round-trips through Node, Python, Go, C bindings. ~3 days each.

### Test strategy

- **Happy path.** Writer publishes, gets token, immediately reads. Returns within deadline; sees own write.
- **Stale-fold timeout.** Suspend the local fold; writer publishes; reader gets `RpcError::Timeout` after deadline. Fold resumes; subsequent read succeeds.
- **Cross-node RYW.** Writer on node A; reader on node B with token from A. Reader waits for B's local fold to catch up — this is a meaningful test of cross-node fold-propagation latency.
- **Deadline tuning.** Histogram of `wait_for_seq` latencies across realistic loads; verify 99th-percentile < 1 s default deadline.

### Risks

- **Hidden coupling between RYW and replication.** If a chain is replicated (Phase 2), "applied to local fold" might not mean "durable on N replicas." Document explicitly: RYW is a *visibility* guarantee, not a *durability* guarantee. They compose, but they're not the same.
- **Deadline-driven cascades.** A misconfigured deadline + stalled fold could pile up RYW waiters. Mitigation: bound the per-channel wait queue; surface backpressure via `RpcError::QueueFull`.

### Effort

2–4 focused weeks. ~500 LoC core + ~600 LoC tests + bindings.

### Activation gate

Application that reads-its-own-writes immediately and finds the eventual-consistency lag operationally surprising. Common trigger: synchronous UI flows where the user expects to see their own change.

---

## Phase 6 — Federated query primitives (The Warriors) + MeshDB extension (deferred)

This phase splits into two scopes:

**Warriors-scope (ships in The Warriors): query primitives over the capability index.**

A small set of composable operators that turn the capability index into a queryable surface. Not a full distributed query language; just the primitives Rebel Yell composes against and any future MeshDB extension would build on. These are the "primitives to build on" that justify The Warriors precursor release.

Concrete operator set (~2–4 weeks of focused work):

- `filter(predicate)` — scan the capability index for tags matching a predicate; uses existing index machinery
- `match(taxonomy_axis, value)` — type-aware match against `hardware:` / `software:` / `devices:` / `dataforts:` taxonomy
- `traverse(start_tag, edge)` — walk capability-tag edges (e.g. `fork-of:` parent links) recursively
- `aggregate(filter, agg)` — counts and aggregations over filter results (no fold required for capability-level aggregates)
- `nearest(predicate, n)` — top-N by proximity weighting

These compose into the user-facing query language Rebel Yell ships. Example query the Warriors primitives must support:

```
hardware.gpu AND software.model:llama-3-70b AND dataforts.has_chain:Y AND proximity < 50ms
```

That is a `match`-then-`filter`-then-`nearest` composition. Operators are composable; there is no SQL surface, just the primitives.

**Rebel Yell extensions on top:** the dual-axis cross-axis query (find by file AND find by hardware in one query) is a use of the Warriors primitives; no new operators needed.

**Deferred-MeshDB scope (parked, not in either release): time-travel, lineage walks, cross-chain joins.**

Above the Warriors primitives sits a more research-grade extension: time-travel queries against historical chain ranges, full lineage-walk traversals via the `fork-of:` and `CausalLink` graph, cross-chain joins with bounded result streaming. Park until a workload genuinely needs it (incident-investigation tooling that needs cross-site joins; replay debugging on retained chain history; aggregate analytics over a fleet). The Warriors primitives reserve the seam; the extension can be designed and shipped without touching the Warriors-scope code.

### What this would be, sketched

- **`MeshDB` query API** atop `NetDB`: `MeshDB::query(query: MeshQuery) -> Stream<Row>`.
- **`MeshQuery` types:**
  - `time_travel_at(origin_hash, seq)` — depends on Phase 0's range-variant tag (`causal:X[start..end]`) and ideally Phase 2's replication (so historical ranges can be recovered after origin compaction).
  - `lineage_walk(origin_hash)` — traverses `CausalLink` parents via the tag chain, recursing through `fork-of:` parent tags.
  - `aggregate_by(filter, agg)` — tag-match counts and aggregations against the capability index (no fold required for capability-level aggregates).
  - `cross_chain_join(origins, predicate)` — join across multiple chains, with the capability index handling routing for each input.
- **Query planning** via the capability index: locate the node nearest to each chain reference; dispatch sub-reads in parallel; join in caller's process.
- **Result streaming** — federated reads return as they arrive; ordering guarantees scoped per chain, not global.

### Trade-offs to handle (deferred-but-named)

- **Tag richness vs. announcement size.** Every additional metadata bit costs announcement size and propagation cost. Aggregate richer metadata into bloom filters or hierarchical summaries; advertise full schema only on demand or via a follow-up RPC after an initial match.
- **Privacy.** Rich tags leak more metadata. ACL gating and subnet-local advertisement scope are the first lines of defence; encrypted tags for sensitive metadata are possible but add complexity. The general rule: only advertise what's already ACL-permitted to leak.
- **Staleness.** Tags propagate at heartbeat intervals; query results are eventually consistent over the metadata layer. The standard pattern is re-read on capability-miss; queries treat capability data as a hint, not a guarantee.
- **Query language.** Today the capability index is queried via tag filters. Joins, aggregates, and time-travel may want a higher-level query API. Could live in NetDB as an extension, or as a separate `MeshDB` layer over the federated read path. Decide when activated.

### Activation gate

Workload that genuinely needs distributed queries, where federating reads from multiple nodes' CortEX state is the only path. Realistic triggers: incident-investigation tooling that needs cross-site joins; replay debugging on retained chain history; aggregate analytics over a fleet.

### Effort

Research-grade; multiple months of design before implementation. Out of scope until there's a concrete use case.

---

## Cross-cutting concerns

### Test infrastructure shared across phases

- **DST harness.** Phases 2 (replication) and 3 (blob CAS GC) need deterministic-simulation tests for partition / failover / restart scenarios. Plan: extend the existing `loom_models.rs` infrastructure as part of Phase 2's first week; share the extension across Phase 3.
- **Failure-injection.** Per-phase needs network partition, disk-fill, and process-crash injection. Build once into the existing integration-test harness; reuse.
- **Bandwidth budgets.** Every phase that adds wire traffic gets a regression test pinning the budget (Phase 0: announcement size; Phase 1: greedy I/O; Phase 2: replication overhead; Phase 3: blob fetch). Treat regressions as test failures, not perf-only signals.
- **Cross-binding parity.** Each phase that adds public API runs the same test suite across Node, Python, Go, C bindings. Reuses the existing parity test infrastructure; new phase tests must be written cross-binding from day one.

### Observability

Every phase emits per-channel metrics into the existing `RpcMetricsRegistry` shape (recently extended for nRPC). Pattern: `dataforts_<feature>_<metric>{channel="X"}`. **No new metric registry.**

| Phase | Metrics |
|---|---|
| 0 | `dataforts_chain_announcements_total`, `dataforts_chain_advertisement_bytes`, `dataforts_chain_bloom_fpr` |
| 1 | `dataforts_greedy_cache_hits_total`, `dataforts_greedy_evictions_total`, `dataforts_greedy_serve_count`, `dataforts_greedy_io_budget_used_bytes` |
| 2 | `dataforts_replication_lag_seconds{role="leader\|replica"}`, `dataforts_replication_sync_bytes_total`, `dataforts_leader_changes_total`, `dataforts_replication_under_capacity_total` |
| 3 | `dataforts_blob_pool_size_bytes`, `dataforts_blob_dedup_hits_total`, `dataforts_blob_fetch_remote_total`, `dataforts_blob_refcount_drift_total` |
| 4 | `dataforts_chain_heat`, `dataforts_gravity_pull_total`, `dataforts_gravity_thrash_total` |
| 5 | `dataforts_ryw_wait_duration_seconds`, `dataforts_ryw_timeouts_total` |

### Feature flags + rollout

- Each phase ships gated behind a Cargo feature: `dataforts-greedy`, `dataforts-replication`, `dataforts-blob`, `dataforts-gravity`, `dataforts-ryw`, `dataforts-query`. **Phase 0 is unconditional** — it's a general capability-tag enhancement, not Dataforts-specific, and other parts of Net (compute placement, scope filtering) benefit from it for free.
- Off-by-default in `ai2070-net` and `ai2070-net-sdk`. Pilots opt in.
- Each phase ships a `CONFIG_<phase>.md`-style operational doc explaining tunables, expected resource cost, and rollback path.
- Rollback path is non-negotiable: every phase must be flippable off in production without restarting the daemon.

### Cross-binding work

- Phase 0 needs Node + Python + Go + C binding updates for the new capability-tag shapes. Mechanical; ~1 week per binding, parallelisable.
- Phases 1–4 expose new config but reuse existing pub/sub/storage APIs — no binding changes required beyond serde for the new config structs.
- Phases 5, 6 add new public API (`WriteToken`, `MeshDB`); estimate +1–2 weeks per binding when those phases ship.

### Documentation

- Each phase ships a user-facing narrative section in `STORAGE_AND_CORTEX.md` (or a sibling doc) that names the feature, points the operator at the config knobs, and describes the failure modes.
- Each phase updates `BEHAVIOR.md` if it changes observable mesh behaviour.
- Each phase appends to `CHANGELOG.md` with the activation gate that justified the work.

---

## Sequencing recommendations

### The Warriors (precursor release)

```
Phase 0 — Capability discovery + taxonomy reorganization (2–3 weeks) ┐
                                                                     ├─→ Warriors release ships
Phase 6 — Federated query primitives (2–4 weeks)                     │
                                                                     │
Phase 2 — RedEX V2 / replication (4–9 weeks; DST gates the timeline) ┘
```

Wall-clock for The Warriors: **~2–3 months parallelised** with one engineer focused on Phase 2's DST work and another on Phase 0 + 6 in parallel. **~4–5 months serialised** if a single engineer is sequencing all three.

The Warriors is the *foundation* release. It ships once and earns its place by making everything Rebel Yell composes on top dramatically cheaper. Trying to ship Rebel Yell without first landing Warriors means retrofitting the taxonomy + replication + query primitives per Rebel Yell phase, which multiplies the coordination cost.

### Rebel Yell (Dataforts release)

```
[Warriors must be shipped or partially landed before this starts]

Phase 1 — Greedy-LRU dataforts (1–2 weeks)  ┐
                                            ├─→ Phase 4 once Phase 1 ships (1–2 weeks)
Phase 3 — Blob CAS (6–12 weeks; can shift   │   [emergent gravity]
          parallel with Warriors if         │
          bandwidth allows)                 │
                                            │
Phase 5 — Read-your-writes (2–4 weeks; slot anywhere — independent of replication once Warriors lands)
```

Wall-clock for Rebel Yell: **~2–3 months parallelised** assuming The Warriors is in place. The headline product win — Dataforts as a 4th capability category, intent-tagged replication, native cross-axis queries — falls out of composing Phase 1 + 4 + Warriors-built primitives.

### Reactive shipping (default — recommended)

**The Warriors should ship reactively but proactively-within-itself.** When a workload activates *any* Warriors phase, ship the whole Warriors release at once because the three phases compose tightly and are foundation-grade. Don't ship Phase 0 alone, Phase 2 alone, or Phase 6-primitives alone — they earn their effort together.

**Rebel Yell ships reactively and phase-by-phase.** Only when a specific Rebel Yell phase has an activation gate firing does the corresponding work happen. Most likely first trigger is Phase 1 (greedy LRU); next likely is Phase 4 (gravity, once Phase 1 telemetry shows skew); Phase 3 (blob CAS) and Phase 5 (RYW) are workload-specific and may never activate.

### Proactive shipping path (only if a pilot demands it)

```
The Warriors (Q3 2026) ──→ Rebel Yell v0.1 (Q4 2026 / Q1 2027)
   ↓ unconditional once             ↓ Phase 1 + 4 minimum;
   any phase activates              Phases 3 + 5 by demand
```

Wall-clock for the full proactive path: **~5–7 months parallelised** across both releases. **Don't take this path without a concrete pilot.** Speculative replication, speculative blob-CAS, speculative RYW are exactly the kind of premature engineering this plan is structured to avoid.

### Default recommendation

**Ship Warriors reactively (when any activation gate fires inside it). Ship Rebel Yell phase-by-phase as workloads demand each piece.** The compute-marketplace use case explicitly does not need any of it — Postgres handles its queries fine. The most likely first trigger for Warriors is a pilot wanting durability beyond single-node (Phase 2's gate) or a query workload that needs Phase 6's primitives. The most likely first trigger for Rebel Yell is a pilot wanting cheap data-locality wins (Phase 1's gate).

Anything built without an active workload requiring it is patronage-network discipline failing — exactly the failure mode the substrate's operating philosophy is designed to avoid. The plan exists so that *when* the workload demands the work, the path is clear; until then, none of this gets built.

---

## See also

- [`misc/DATAFORTS_FEATURES.md`](misc/DATAFORTS_FEATURES.md) — the audit that produced this plan
- [`REDEX_PLAN.md`](REDEX_PLAN.md) — single-node v1 substrate (phase predecessor)
- [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) — single-node v2 (tiering, indices, typed wrappers — orthogonal to this plan)
- [`misc/REDEX_MANIFEST_POINTER_DESIGN.md`](misc/REDEX_MANIFEST_POINTER_DESIGN.md) — on-disk layout for Phase 3's blob CAS
- [`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md) — `scope:` tag convention reused by Phase 1
- [`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md) — capability-announcement propagation that Phase 0 extends
- [`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md) — broadcast machinery Phase 0 reuses
- [`misc/NRPC_DESIGN.md`](misc/NRPC_DESIGN.md) — metrics + reliability surfaces Dataforts phases reuse
- [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) — local query layer that Phase 6's MeshDB would sit above
- [`NETDB_PLAN.md`](NETDB_PLAN.md) — single-node query façade that MeshDB extends
- [`STORAGE_AND_CORTEX.md`](STORAGE_AND_CORTEX.md) — user-facing storage narrative (each phase ships an additive section here)
