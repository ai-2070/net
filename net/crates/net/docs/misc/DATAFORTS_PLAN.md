# Dataforts — Implementation Plan

> Companion to [`misc/DATAFORTS_FEATURES.md`](misc/DATAFORTS_FEATURES.md). The features doc is the audit: which 25-of-28 wishlist items already ship and which 4-or-so are genuinely new work. **This doc sequences the new work** — phase order, gate criteria, scope boundaries, design decisions to lock, test strategy, risks, and effort. The frame: every phase **stays parked until its activation gate fires**. This is a "we know what to build when we have to" plan, not a build-everything roadmap.

## Status

Design only. Nothing in this plan is in flight. Phases 0 + 1 are cheap enough to ship speculatively if we want every later phase to slot in fast; everything else waits for a workload to demand it.

## Why this exists

Three reasons this needs to be a written plan and not just "we'll figure it out":

1. **Most of the wishlist already ships.** It is easy to redo work that already exists if we don't have the audit + sequence in one place. The features doc handles the audit; this doc handles the sequence.
2. **Phase ordering matters.** Phase 0 (capability-tag discovery) collapses the coordination problem in every later phase — building it first is meaningfully cheaper than retrofitting it phase-by-phase.
3. **DST is the gating concern, not LoC.** Phase 2 (replication) and Phase 3 (blob CAS GC) are gated by deterministic-simulation-test depth, per [`REDEX_PLAN.md`](REDEX_PLAN.md)'s explicit "needs a clear DST story" condition. Acknowledging that up front avoids surprise when the actual implementation hits the testing wall.

## TL;DR

Seven phases, sequenced by dependency:

| # | Phase | Effort (focused) | Activation gate | Depends on |
|---|---|---|---|---|
| 0 | Capability-tag discovery primitive | 1–2 weeks | First time any later phase activates (cheap; ship speculatively) | — |
| 1 | Greedy-LRU dataforts | 1–2 weeks | Pilot wants cheap durability / data-locality wins | 0 |
| 2 | Raw RedEX log-segment replication | 4–9 weeks | Workload needs durability beyond single node | 0 |
| 3 | Content-addressable blob store | 6–12 weeks | Payloads systematically exceed segment-friendly size | 0 (independent of 1, 2) |
| 4 | Data gravity (heat-counter migration) | 1–2 weeks | Production telemetry shows access skew Phase 1 doesn't absorb | 0, 1 |
| 5 | Read-your-writes guarantees | 2–4 weeks | App ergonomics request session-bounded consistency | — |
| 6 | Federated query layer (MeshDB) | research-grade; multiple months | Workload that single-node CortEX/NetDB can't satisfy | 0, 2 |

Phase 0 is the only phase that earns proactive shipping — every other phase consumes it, so it's cheaper to land it once than to bolt it onto each. Phases 1–6 are demand-driven. Phase 4 collapsed from "3–6 weeks" to "1–2 weeks" once we accepted the features-doc framing of gravity as **emergent behavior of greedy + heat counters**, not a separate migration engine.

Total focused effort if everything activates: **~4–6 months parallelized**, **~7–9 months sequential**. Don't read this as a budget commitment — read it as the worst-case shape of the design space.

---

## Phase 0 — Capability-tag discovery primitive

The unlock. The features doc identifies `causal:origin_hash[:tip_seq]` capability tags as the discovery layer that collapses every other deferred phase's coordination problem. Build it once; everything else routes through it.

### Scope

**Tag shapes.** Four parsed forms, all encoded as opaque `Tag` values inside the existing `CapabilitySet.tags` set:

| Shape | Meaning |
|---|---|
| `causal:<32-byte hex of origin_hash>` | "I hold (or will serve) this chain — current tip unknown / not advertised" |
| `causal:<hex>:<tip_seq>` | "I hold this chain at least through `tip_seq`" |
| `causal:<hex>[<start>..<end>]` | "I hold this chain across the `[start, end]` seq range" — for time-travel queries (Phase 6) |
| `fork-of:<parent_hex>` | "This chain forked from `parent_hex` — for lineage/cohort queries" |
| `blob:<32-byte hex>:<size>` | "I hold this blob in my CAS pool" — used by Phase 3 |

Two non-shape extensions, both reserved keys on capability tags:

- `heat:<chain_hex>=<reads_per_window>` — heat counter for Phase 4. Annotated optionally; absence means "not advertising heat."
- `scope:<label>` — the existing scoped-capability tag (see `SCOPED_CAPABILITIES_PLAN.md`); reused by Phase 1's greedy filter.

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

## Phase 1 — Greedy-LRU dataforts

A node observes streams flowing past via the existing tail subscription path. If it has spare disk, ACL access, and a scope match, it caches a copy. When disk fills, LRU evicts. Withdraws the capability tag on eviction so reads route elsewhere. **No coordination protocol.** Smallest deferred phase; ships fastest; covers 60–80% of the perceived-durability story without orchestrated replication.

### Scope

- Local-only opt-in via `MeshNode::enable_greedy_dataforts(GreedyConfig)`.
- Configurable via `GreedyConfig`:
  ```rust
  pub struct GreedyConfig {
      pub scopes: Vec<ScopeLabel>,            // e.g. ["scope:industrial-telemetry"]
      pub proximity_max_rtt: Duration,         // e.g. 200ms
      pub per_channel_cap_bytes: u64,          // default 100 MB
      pub total_cap_bytes: u64,                // default 10 GB
      pub bandwidth_budget_fraction: f32,      // default 0.25 of measured NIC peak
  }
  ```
- Cache substrate: a per-channel RedEX file with a size cap. Caches are normal RedEX files, just owned by the cache layer instead of the application. Reuse v1 retention machinery (`Retention::Bytes`) for the size cap.
- **Pull condition is a triple AND** (per features-doc spec):
  1. Scope match — the chain advertises a `scope:` tag matching one of the local node's configured scopes.
  2. Proximity bound — the chain's home is within `proximity_max_rtt` per the existing proximity graph.
  3. Storage available — local node decision; LRU eviction when the total cap is hit.
- ACL gating falls through automatically — only chains with valid `subscribe_caps` reach the inbound observe path; the cache layer just inherits.
- Per-chain advertisement on first cache, withdrawal on full eviction. Phase 0 carries the announcements.

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

## Phase 2 — Raw RedEX log-segment replication

Orchestrated replication. N replicas of a channel's RedEX file maintained explicitly; configurable replication factor; pull/repair on divergence; documented conflict policy (none expected because RedEX is append-only and seq-ordered, but the protocol must say so explicitly). Strong durability guarantee, in contrast to Phase 1's probabilistic one.

This phase is the heaviest one in the plan because it lands the wire protocol (`SUBPROTOCOL_REDEX`) that v1 explicitly defers and because DST coverage for partition / failover / rejoin is non-negotiable.

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

## Phase 3 — Content-addressable blob storage

Independent track from Phases 1–2. Addresses payloads where the streaming-log + INLINE+heap pattern is wrong-shaped — large blobs (≥ MB-class) where chunk-as-event is operationally awkward. Manifest-pointer indirection: RedEX events carry blob hashes, not bytes; blob bytes live in a separate CAS pool.

[`misc/REDEX_MANIFEST_POINTER_DESIGN.md`](misc/REDEX_MANIFEST_POINTER_DESIGN.md) already specifies the on-disk layout. This phase implements it across the mesh.

### Scope

- **Local CAS pool.** Content-addressed by `blake2s-256(blob_bytes)`. Pool sits alongside RedEX segments; per-pool size cap.
- **Manifest pointer.** `(blob_hash: [u8; 32], blob_size: u64)` embedded in RedEX events instead of inline payload. New `RedexFlags::MANIFEST_POINTER` flag bit. The 40-byte payload region is `[hash..32, size_be..8]`.
- **Read path.** On event read, if `MANIFEST_POINTER` set → resolve pointer → CAS lookup → return bytes (or stream if `blob_size > stream_threshold`, default 8 MB).
- **GC.** Blobs ref-counted via the events that point to them; when retention drops the last-referencing event, blob is eligible for eviction. Periodic sweep + per-CAS-pool refcount table.
- **Mesh-level discovery.** Blobs advertised via `blob:<hash>:<size>` capability tag (Phase 0 variant); fetches route to the nearest holder via the existing capability index.
- **Optional dedup.** Identical blobs across channels share one CAS entry. Free win since hashes match — but ACL-gated, see open question.

### Concrete tasks

1. New module `adapter::net::redex::blob` with `BlobPool` struct.
2. Wire `RedexFlags::MANIFEST_POINTER`; encode `(hash, size)` into the existing payload region.
3. CAS write API: `BlobPool::put(bytes) -> Result<(BlobHash, u64), BlobError>`. Hash + write + advertise.
4. CAS read API: `BlobPool::get(hash) -> Result<Bytes, BlobError>` (small) and `BlobPool::get_stream(hash) -> impl Stream<Item = Bytes>` (large). Local-first; cap-mesh-fetch on miss.
5. Mesh fetch: capability-tag query → reliable-stream pull from nearest holder. New `BlobFetch` subprotocol (~200 LoC, 2 dispatch codes: `BLOB_REQUEST`, `BLOB_RESPONSE`).
6. GC: per-CAS-pool refcount table, decremented on event retention drop. Periodic sweep — gated by `BlobPool::gc_interval` (default 60 s).
7. RedEX read path: on `MANIFEST_POINTER` flag, resolve via `BlobPool::get` instead of returning inline payload. Existing readers MUST handle the flag transparently — otherwise they corrupt downstream.
8. Bindings: `BlobPool::put` / `get` / `get_stream` exposed in Node + Python + Go + C bindings. Stream variant in async languages; sync chunked-iterator equivalent in C.

### Open design questions

- **Manifest-pointer back-compat.** Existing RedEX files don't have the flag; readers must handle absence cleanly. Already covered by v1 RedEX flag-bit design (unknown flags ignored), but **pin in test**: a pre-Phase-3 reader on a Phase-3-written file produces correct (if uninterpreted) output. A Phase-3 reader on a pre-Phase-3 file produces correct (inline-payload) output.
- **Dedup vs. ACL.** If two channels have different ACLs but the same blob hash, serving channel A's reader from channel B's deduped blob is a cap-leak risk. **Recommendation:** per-blob ACL pinning — first-writer's `subscribe_caps` is recorded with the blob; subsequent dedupers must match. If they don't, store separately. Cost: dedup hit rate drops in mixed-ACL scenarios; correctness preserved.
- **Streaming reads of giant blobs.** A 1 GB blob shouldn't materialise in memory. `BlobPool::get_stream` is the answer. Threshold for stream-vs-buffer: default 8 MB, configurable per-pool.
- **CAS pool size cap interaction with refcount.** Hard cap means evicting a still-referenced blob is forbidden; soft cap admits over-cap with eviction pressure on next sweep. **Recommendation:** hard cap; surface backpressure to writers via `BlobError::PoolFull`.
- **Hash collision.** Operationally impossible with blake2s-256, but pin assumption with `debug_assert!` on collision-detected-different-bytes.

### Test strategy

- **Unit.** Hash determinism, refcount lifecycle, GC sweeps, evict-while-referenced rejection, pool-full backpressure.
- **Integration.** 3-node mesh, 1 publisher writes 100 blobs of 10 MB each. Reader on a 4th node fetches all 100; assert routing to the nearest holder; assert no double-fetch (capability tag → single source per fetch).
- **Stream variant.** 1 GB blob written, streamed read. Memory ceiling stays bounded (< 64 MB for the entire fetch path).
- **Cross-channel dedup.** Two channels, identical bytes. Assert single CAS entry; ACL-divergent case stores separately.
- **Failure recovery.** Source holder partitioned mid-fetch; assert reader retries via capability index to a different holder. No partial-blob materialisation.
- **GC under churn.** Continuous publish + retention-drop loop. Assert refcount converges; no orphaned blobs after sweep; no premature eviction of still-referenced.

### Risks

- **Blob pool corruption (manifest-pointer points to nothing).** Crash recovery: pool fsck on mesh node startup; orphan blobs purged or quarantined depending on operator policy. Document the recovery semantics.
- **Refcount drift over time** (rare but theoretically possible under concurrent retention drop). Mitigation: periodic full reconcile pass; quarantine + log on mismatch. Surface as `dataforts_blob_refcount_drift_total` metric.
- **GC sweep latency.** A 1 TB pool with 10M blobs at 60 s sweep is a real CPU cost. Mitigation: incremental sweep — partition the refcount table; sweep one partition per interval. Defer until pool size telemetry justifies it.

### Effort

6–12 focused weeks. ~2500 LoC core + ~2500 LoC tests + significant DST harness work for the GC sweep (~2 weeks). Bindings ~1 week each (stream APIs make this slightly heavier than Phase 0/1).

### Activation gate

Workload with payloads ≥ MB-class where the inline+heap RedEX pattern is operationally awkward. Concrete triggers: user-uploaded media, model artefacts, large batch-inference outputs.

### Independence

Doesn't depend on Phases 1, 2, or 4. Can run in parallel with Phase 2 if the team has bandwidth.

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

## Phase 6 — Federated query layer (MeshDB)

Above all storage layers. The capability-tag discovery primitive (Phase 0) makes mesh-level federated reads tractable. This phase formalises them: time-travel, lineage walks, cross-chain joins, aggregate queries — a distributed query substrate sitting above CortEX/NetDB's local query layer.

**Scope is open-ended.** Park until a workload demands it. Documenting here so the design space is named, the dependency on Phases 0 + 2 is explicit, and the interface seam between local NetDB and mesh-level MeshDB is reserved.

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

### Reactive shipping (default)

```
Phase 0 [first activation gate fires]
  ↓
Phase 1 ─→ Phase 4 (telemetry-driven)
  ↓
Phase 2 ─→ Phase 6 (parked unless query workload)
  ↓
Phase 3 (parallel with 2 if bandwidth)
  ↓
Phase 5 (slot anywhere — independent)
```

### Proactive "Dataforts v0" release

If the team wants to land the cluster as a single product release rather than reactively:

```
Phase 0 (1–2 weeks)                      [unconditional]
  ↓ parallel
Phase 1 (1–2 weeks)  ┐
Phase 2 (4–9 weeks)  ├─→ Phase 4 once Phase 1 ships (1–2 weeks)
Phase 3 (6–12 weeks) ┘
  ↓
Phase 5 (2–4 weeks, anywhere)
```

Wall-clock for v0: **~2–3 months parallelised** with one engineer per track, **~6–9 months serialised**. The features doc's "2–3 months" estimate matches the parallel-track scenario.

### Default recommendation

**Ship reactively.** None of these has a real activation gate today (the compute-marketplace use case explicitly does not need any of it — Postgres handles its queries fine). Build Phase 0 once we have a concrete consumer for any of 1–6, then sequence by demand. Most likely first trigger is Phase 1 (greedy LRU, smallest cost, broadest applicability).

The temptation to ship Phase 0 + 1 speculatively *is* defensible — they are cheap (~3 weeks combined), they unlock most of the perceived-durability story, and they make every later phase substantially cheaper. If a pilot is on the horizon and Dataforts is plausibly part of it, that's the case for paying the speculative cost.

Anything beyond Phase 0 + 1 should not be built without an active workload requiring it. Speculative replication or blob-CAS is exactly the kind of premature engineering this plan is structured to avoid.

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
