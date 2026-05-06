# Dataforts — Implementation Plan

> Companion to `DATAFORTS_FEATURES.md`. The features doc audits which wishlist items already ship vs. genuinely require new work. This doc sequences the new work — phase order, gate criteria, design unknowns, effort, and dependencies. Any phase **stays parked until its activation gate fires**; this is not a "build everything" roadmap. The point is: when a real workload demands one of these, we already know how to start.

## TL;DR

Six phases, sequenced by dependency:

| # | Phase | Effort (focused) | Activation gate | Depends on |
|---|---|---|---|---|
| 0 | Capability-tag discovery primitive | 1–2 weeks | First time any other phase activates | — |
| 1 | Greedy-LRU dataforts | 1–2 weeks | Pilot wants cheap durability/data-gravity wins | 0 |
| 2 | Raw RedEX log-segment replication | 4–9 weeks | Workload needs durability beyond single-node | 0 |
| 3 | Content-addressable blob store | 6–12 weeks | Payloads systematically exceed segment-friendly size | 0 (independent of 1, 2) |
| 4 | Data gravity (read-pattern migration) | 3–6 weeks | Production telemetry shows access skew Phase 1 can't absorb | 0, 1 |
| 5 | Read-your-writes guarantees | 2–4 weeks | App ergonomics request session-bounded consistency | — |
| 6 | Federated query layer (time-travel, lineage walks, joins) | unbounded — research-grade | Operational use case that single-node CortEX/NetDB can't handle | 0, 2 |

Phase 0 is the only phase that ships **proactively** — every later phase consumes it, so it's cheaper to land it once than to bolt it onto each. Phases 1–6 are demand-driven.

Total focused effort if everything activates: **~4–6 months parallelized**, **~7–9 months sequential**. Don't read this as a budget commitment — read it as "if/when we have to, we know what to build."

---

## Phase 0 — Capability-tag discovery primitive

The unlock. The features doc identifies `causal:origin_hash[:tip_seq]` capability tags as the discovery layer that collapses every other deferred phase's coordination problem. Build it first; everything else routes through it.

**Scope.**
- Wire format for the tag itself: `causal:<32-byte hex of origin_hash>[:<tip_seq>]` plus a range variant `causal:<hex>[<start>..<end>]` for time-travel queries (Phase 6) and a fork-parent variant `fork-of:<parent_hex>`.
- Bloom-filter aggregation for nodes carrying many chains. Target: 10K chains in <500 KB, propagation cost ≤2× current capability announcement.
- Re-announcement throttle. Default: emit on whichever fires first — Δ`tip_seq` ≥ 1024 events OR Δt ≥ 10s. Configurable per channel.
- Withdrawal path: capability index already supports tag removal; just need the producer hook (greedy LRU evict, replica drop, blob GC).
- ACL: tag is encrypted via `subscribe_caps` for the channel like any other capability tag. No leak of which chains a node holds to peers without cap.

**Concrete tasks.**
1. Extend `behavior::capability::Tag` parsing to recognize the four shapes above.
2. Add `Mesh::announce_chain(origin_hash, tip_seq)` / `Mesh::withdraw_chain(origin_hash)` — thin wrappers that bump the capability set and trigger announcement on the throttle.
3. Add bloom-filter aggregation as an optional encoding when count ≥ threshold (default 256). Include via a new `CapabilitySet::chain_bloom: Option<BloomFilter>` field.
4. Per-channel announcement throttle (`ChannelConfig::chain_announcement: Option<ChainAnnouncementPolicy>`).

**Test strategy.**
- Unit: tag parse round-trip, bloom-filter false-positive rate at target sizes, throttle behavior.
- Integration: 4-node mesh, 1 publisher, 3 observers; assert observer indexes converge within heartbeat interval; assert announcement bandwidth bounded.
- Negative: ACL-blocked node never sees tags for chains it can't subscribe to.

**Risks.**
- Bloom-filter false positives → spurious read attempts to nodes that don't hold the chain. Mitigation: read path must already handle "node advertised but doesn't have it" (Phase 2 makes this explicit; Phase 1 just retries elsewhere). Falsely advertised tags are a recoverable miss, not a correctness bug.
- Re-announcement traffic if throttle is too aggressive. Mitigation: ship with conservative defaults; expose tunables.

**Effort.** 1–2 focused weeks. ~600 LoC core + ~400 LoC tests. **No state-machine work.**

**Activation gate.** Land at least the first time any of Phases 1–4, 6 activates. Cheap enough that we could ship it speculatively if we want everything else to slot in fast.

---

## Phase 1 — Greedy-LRU dataforts

A node observes streams flowing past via the existing tail subscription path. If it has spare disk and ACL access, it caches a copy. When disk fills, LRU evicts. Withdraws the capability tag on eviction so reads route elsewhere. **No coordination protocol.** Smallest deferred phase; ships fastest; fixes 60–80% of the perceived-durability story without orchestrated replication.

**Scope.**
- Local-only opt-in via `MeshNode::enable_greedy_dataforts(GreedyConfig)`.
- Cache substrate: a per-channel RedEX file with size cap (default: per-channel 100 MB, total 10 GB). Caches are normal RedEX files, just owned by the cache layer instead of the application.
- Eviction: track `(channel, last_read_at)`; LRU on cache full. Eviction emits the corresponding tag withdrawal.
- Cap-gated: only chains with valid `subscribe_caps` get cached. AuthGuard already enforces this on the inbound observe path; cache layer just inherits.
- Per-chain advertisement on first cache, withdrawal on full eviction. Phase 0 carries the announcements.

**Concrete tasks.**
1. New module `adapter::net::dataforts::greedy` with `GreedyCache` struct.
2. Hook into `MeshNode::dispatch_event` (the inbound delivery point used by the per-shard inbound queue): if greedy enabled AND node has cap AND total cache below soft cap → write to cache file in addition to the application's tail.
3. Tag emission via Phase 0's `Mesh::announce_chain` on first append per chain; tag withdrawal on full eviction.
4. Read path: when reading a chain the local node holds in greedy cache, serve from cache instead of going to origin. Add a `Mesh::greedy_serve_count` metric so we can measure the win.
5. Sanity: bound total greedy I/O at a fraction of NIC budget (default: 25% of measured peak); back off if greedy starts crowding application traffic.

**Test strategy.**
- 3-node mesh, 1 publisher, 2 greedy observers. Stream 100 MB through the channel. Both observers cache; tag advertisement reaches every other node; reads from a 4th node route to the nearest cacher.
- Eviction: fill the cache to 110% of cap, drive evictions, assert tag withdrawals.
- ACL: 3rd node without `subscribe_caps` does NOT cache. Pin via cache file size = 0.
- Bandwidth budget: under saturating publisher load, greedy I/O does not exceed configured fraction.

**Risks.**
- Cache write amplification under bursty publishers (every observer writes the same data). Mitigation: greedy is opt-in per node; not all nodes turn it on.
- LRU thrashing under uniform-random access. Mitigation: standard LRU pathology, not specific to dataforts. Document; revisit only if telemetry shows it.

**Effort.** 1–2 focused weeks (per features doc estimate, post-Phase-0 collapse). ~900 LoC core + ~500 LoC tests.

**Activation gate.** Pilot deployment requests "make popular data fast without standing up replica groups." Realistic trigger.

---

## Phase 2 — Raw RedEX log-segment replication

Orchestrated replication. N replicas of a channel's RedEX file maintained explicitly; configurable replication factor; pull/repair on divergence; conflict resolution (none expected, since RedEX is append-only and seq-ordered, but the protocol must say so explicitly). Strong durability guarantee, in contrast to Phase 1's probabilistic one.

**Scope.**
- New `ChannelConfig::replication: Option<ReplicationConfig { factor: u8, placement: PlacementStrategy }>`.
- Replica election: N nodes from the capability-advertising set, weighted by proximity + free capacity. Elections happen on first publish AND on roster change.
- Wire protocol: `RedexReplicaSync` subprotocol — extends the existing `MembershipSubprotocol` shape with an additional dispatch byte. Pull-based: replica observes its current `tail_seq`, requests `(channel, since_seq)`, leader responds with `read_range`.
- Repair: replicas heartbeat their `tail_seq` to leader; leader detects gap, replays. Heartbeat interval = `ChannelConfig::replication_heartbeat_ms` (default 500ms).
- Failover: if leader fails (proximity graph reports `Unhealthy`), surviving replicas elect a new leader via the existing standby-group election. Reuses `groups::standby` machinery.
- Subprotocol leverages existing `Mesh::publish` reliable streams — append events stream to replicas exactly as they would to a regular subscriber, with explicit ack on append.

**Concrete tasks.**
1. New `ReplicationCoordinator` daemon spawned per replicated channel on each replica.
2. Wire format: extend `EventMeta::dispatch` with `DISPATCH_REPLICA_SYNC = 0x20..0x2F` range. Reserve 16 codes; v1 uses 4 (`SYNC_REQUEST`, `SYNC_RESPONSE`, `SYNC_HEARTBEAT`, `LEADER_ELECTION`).
3. Pull-based catch-up: replica computes gap from heartbeat ack mismatch; issues `SYNC_REQUEST`; leader responds with `read_range`. Bounded request size (default: 1 MB per request).
4. Conflict policy: append-only + monotonic seq → no conflicts possible IF leader is the sole appender. Document this assumption; reject "writes" from non-leader replicas with `RedexError::NotLeader`.
5. Failover: integrate with `groups::standby::StandbyGroup` for promotion. Reuse existing election; just add the leader-pre-empt-on-replication-divergence trigger.
6. Replica withdrawal: drop replication on graceful shutdown (`Coordinator::Drop`). Capability tag withdrawn via Phase 0.
7. Cross-cutting: per-channel replication metrics (lag, sync rate, leader changes) on the existing `RpcMetricsRegistry` shape (Phase 3 of nRPC; the metrics surface is general).

**Open design questions to lock before implementation.**
- **Leader scope.** Is the replication leader the same as the `ChannelPublisher`'s home, or a separately-elected entity? Recommend: same node by default (publisher is the natural leader for an append-only channel), with explicit override via `ReplicationConfig::leader_pinned: Option<NodeId>` for split publisher/leader topologies.
- **What does "replicated" mean for retention?** If channel retains 100 MB and a replica drops below that under disk pressure, does it withdraw replicaship or evict the oldest local data? Recommend: withdraw, fall through to greedy LRU if also enabled. Replication factor is a hard guarantee on the leader; replicas are best-effort under capacity.
- **Cross-segment atomicity (per features doc §23 non-goal).** Replication must NOT introduce cross-segment atomicity expectations; replicas catch up segment-by-segment. Document explicitly.
- **Membership during partition.** If a replica is partitioned but eventually rejoins, does it re-catch-up from current tail or replay the gap? Reuses standby-group's replay machinery; but needs explicit decision on "skip ahead vs. replay" when gap > threshold.

**Test strategy.**
- 3-replica + 1-publisher mesh. Steady-state appends; assert all replicas converge to leader's tail within heartbeat × 2.
- Failover: kill leader; assert one replica promotes; new appends land on new leader; old leader on rejoin catches up.
- Disk pressure: replica configured below leader's retention; assert graceful withdrawal, not silent corruption.
- DST scenarios: random partition + restart sequences; assert all surviving replicas converge eventually.
- Performance budget: replication overhead ≤ 30% of single-node append throughput (replica sync runs at heartbeat cadence, so the steady-state cost is just the 1× publish-to-replica cost).

**Risks.**
- **DST story is the hardest part.** No replication design survives without a deterministic-simulation-test plan for partition + leader-flap + rejoin sequences. Allocate ~30% of phase effort to DST harness work. The features doc explicitly cites `REDEX_PLAN.md`'s "needs a clear DST story" as the gating condition.
- Leader concentration → write hotspots. Mitigation: per-channel leader, not per-node; large deployments distribute leadership naturally.
- Subprotocol code surface adds ~1500–2000 LoC to the mesh adapter. Audit footprint before merge.

**Effort.** 4–9 focused weeks. Wide range driven by DST harness depth. ~2500 LoC core + ~3500 LoC tests.

**Activation gate.** Workload requesting durability guarantees beyond single-node, where Phase 1's probabilistic story is insufficient. Realistic triggers: payment-tier customer; compliance-bound data class; pilot whose RTO is "<5s on node failure."

---

## Phase 3 — Content-addressable blob storage

Independent track from Phases 1–2. Addresses payloads where the streaming-log + INLINE+heap pattern is wrong-shaped — large blobs (≥ MB-class) where chunk-as-event is operationally awkward. Manifest-pointer indirection: RedEX events carry blob hashes, not bytes; blob bytes live in a separate CAS pool.

`REDEX_MANIFEST_POINTER_DESIGN.md` already specifies the on-disk layout. This phase implements it across the mesh.

**Scope.**
- Local CAS pool: content-addressed by `blake2s-256(blob_bytes)`. Pool sits alongside RedEX segments; per-pool size cap.
- Manifest pointer: `(blob_hash: [u8; 32], blob_size: u64)` embedded in RedEX events instead of inline payload. Requires a new `RedexFlags::MANIFEST_POINTER` flag bit.
- Read path: on event read, resolve pointer → CAS lookup → return bytes (or stream if blob_size > stream threshold).
- GC: blobs ref-counted via the events that point to them; when retention drops the last-referencing event, blob is eligible for eviction.
- Mesh-level: blobs advertised via `blob:<hash>:<size>` capability tag (Phase 0 variant); fetches route to the nearest holder.
- Optional dedup: identical blobs across channels share one CAS entry. Free win since hashes match.

**Concrete tasks.**
1. New module `adapter::net::redex::blob` with `BlobPool` struct.
2. Wire `RedexFlags::MANIFEST_POINTER`; encode `(hash, size)` into the existing payload region as `[hash..32, size_be..8] = 40 bytes`.
3. CAS write API: `BlobPool::put(bytes) -> Result<(BlobHash, u64), BlobError>`. Hash + write + advertise.
4. CAS read API: `BlobPool::get(hash) -> Result<Bytes, BlobError>`. Local-first; cap-mesh-fetch on miss.
5. Mesh fetch: capability-tag query → reliable-stream pull from nearest holder. Reuses `Mesh::publish_to_peer` + a new `BlobFetch` subprotocol (~200 LoC, 2 dispatch codes).
6. GC: per-CAS-pool refcount table, decremented on event retention drop. Periodic sweep.
7. RedEX read path: on `MANIFEST_POINTER` flag, resolve via `BlobPool::get` instead of returning inline payload.

**Open design questions.**
- **Manifest-pointer back-compat.** Existing RedEX files don't have the flag; readers must handle absence cleanly. Already covered by the v1 RedEX flag-bit design (unknown flags ignored), but pin in tests.
- **Dedup vs. ACL.** If two channels have different ACLs but the same blob hash, serving one channel's reader from the dedup-shared blob is a cap-leak risk. Mitigation: per-blob ACL pinning — first-writer's `subscribe_caps` is recorded with the blob; subsequent dedupers must match. If they don't, store separately. Cost: dedup hit rate drops in mixed-ACL scenarios; correctness preserved.
- **Streaming reads of giant blobs.** A 1 GB blob shouldn't materialize in memory. The `BlobPool::get` API needs a stream variant (`get_stream(hash) -> impl Stream<Item = Bytes>`) for large blobs. Threshold default: 8 MB.
- **CAS pool size cap interaction with refcount.** Hard cap means evicting a still-referenced blob is forbidden; soft cap admits over-cap with eviction pressure on next sweep. Recommend hard cap; surface backpressure to writers via `BlobError::PoolFull`.

**Test strategy.**
- Unit: hash determinism, refcount lifecycle, GC sweeps, evict-while-referenced rejection.
- Integration: 3-node mesh, 1 publisher writes 100 blobs of 10 MB each. Reader on a 4th node fetches all 100; assert routing to the nearest holder; assert no double-fetch (capability tag → single source per fetch).
- Stream variant: 1 GB blob written, streamed read. Memory ceiling stays bounded.
- Cross-channel dedup: two channels, identical bytes. Assert single CAS entry; ACL-divergent case stores separately.
- Failure: source holder partitioned mid-fetch; assert reader retries via capability index to a different holder.

**Risks.**
- Blob pool corruption (manifest-pointer points to nothing). Crash recovery: pool fsck on mesh node startup; orphan blobs purged or quarantined depending on operator policy.
- Refcount drift over time (rare but theoretically possible under concurrent retention drop). Mitigation: periodic full reconcile pass; quarantine + log on mismatch.
- Hash collision (operationally impossible with blake2s-256, but pin assumption in code with `debug_assert!`).

**Effort.** 6–12 focused weeks. ~2500 LoC core + ~2500 LoC tests + significant DST harness work for the GC sweep.

**Activation gate.** Workload with payloads ≥ MB-class where the inline+heap RedEX pattern is operationally awkward. Concrete trigger: user-uploaded media, model artifacts, large batch inference outputs.

**Independence.** This phase doesn't depend on Phases 1, 2, or 4. Can run in parallel with Phase 2 if the team has bandwidth.

---

## Phase 4 — Data gravity (read-pattern migration)

Once Phases 0 + 1 ship, the mesh observes which chains are most-read and which nodes are doing the reading. Phase 4 closes the loop: nodes pull data toward themselves when read pressure justifies it. Telemetry-driven, decentralized, opt-in per channel.

**Premature without telemetry from production.** Plan it now; don't build until we have read-pattern data.

**Scope.**
- Per-`(channel, byte-range)` read-pattern telemetry, locally aggregated. Window: rolling 1h.
- Migration policy: if local read rate for chain X ≥ threshold AND chain X is held by a sufficiently-distant node (proximity ≥ N hops), pull a copy. Reuses Phase 1's caching machinery with a different trigger.
- Withdraw on cooldown: if read rate falls below threshold for ≥ 1h, evict (LRU-style; same machinery as Phase 1).
- Consistency during migration: append-only log + monotonic seq → migration is just a `read_range(0..tail)` followed by a tail subscription. No special protocol.

**Concrete tasks.**
1. Per-chain read-rate counter on every read path.
2. Migration policy daemon: scans the read-rate table every N seconds; selects pull candidates per the policy.
3. Pull execution: reuses Phase 1's cache infrastructure + Phase 0's tag emission.
4. Configurable per `ChannelConfig::data_gravity: Option<DataGravityPolicy>`.

**Open design questions.**
- **Telemetry scope.** Per-chain or per-`(channel, byte-range)`? Recommend per-chain to start; byte-range is a future optimization.
- **Anti-thrash.** Hysteresis: pull threshold > evict threshold + 2× (conservative). Document the gap.
- **Mesh-wide vs. node-local decision.** Local decision is simpler (decentralized, no consensus). Mesh-wide could optimize replica placement globally but requires coordination. Recommend local-only for v1; revisit if telemetry shows gaps.

**Effort.** 3–6 focused weeks atop Phase 1.

**Activation gate.** Production telemetry showing access skew Phase 1's purely-greedy LRU doesn't capture (e.g. read patterns where greedy nodes don't happen to sit on the routing path).

---

## Phase 5 — Read-your-writes guarantees

Independent of all other phases. Smallest scope. Useful when application semantics require the writer to immediately see its own writes (currently the system is causally consistent but with no RYW guarantee — a writer may briefly observe state lagging its own publish).

**Scope.**
- Session-token API: writers receive a `WriteToken { origin_hash, seq }` on every publish; readers can present it to a read API that blocks until the local fold has applied that seq.
- Per-fold `applied_through_seq` already exists in CortEX; expose via `CortexAdapter::wait_for_seq(seq).await`.
- Bound: deadline on the wait; surface `RpcError::Timeout` if applied seq doesn't catch up. Default 1s.

**Concrete tasks.**
1. `WriteToken` type encoding `(origin_hash, seq)`; emit from `RedexFile::append`.
2. `CortexAdapter::wait_for_seq(seq, deadline)` — uses existing tail-fold notify primitive.
3. Higher-level wrapper: `MeshNode::publish_with_token` returns the token; `MeshNode::read_at_token` waits.

**Effort.** 2–4 weeks.

**Activation gate.** Application that reads-its-own-writes immediately and finds the eventual-consistency lag operationally surprising. Common trigger: synchronous UI flows where the user expects to see their own change.

---

## Phase 6 — Federated query layer

Above all storage layers. The capability-tag discovery primitive (Phase 0) makes mesh-level federated reads tractable. This phase formalizes them: time-travel, lineage walks, cross-chain joins, aggregate queries — a distributed query substrate over CortEX's local query layer.

**Scope is open-ended.** Park until a workload demands it. Documenting here so the design space is named.

**What this would be, sketched.**
- `MeshDB` query API atop `NetDB`: `MeshDB::query(query: MeshQuery) -> Stream<Row>`.
- `MeshQuery` types: `time_travel_at(origin, seq)`, `lineage_walk(origin)`, `aggregate_by(filter, agg)`, `cross_chain_join(origins, predicate)`.
- Query planning via the capability index: locate the node nearest to each chain reference; dispatch sub-reads in parallel; join in caller's process.
- Time-travel: depends on Phase 0's range-variant tag (`causal:X[start..end]`) and Phase 2's replication (so historical ranges can be recovered after origin compaction).
- Lineage: `CausalLink`-walk via the tag chain, recursing through `fork-of:` parent tags.

**Effort.** Research-grade; multiple months of design before implementation. Out of scope until there's a use case.

**Activation gate.** Workload that genuinely needs distributed queries, where federating reads from multiple nodes' CortEX state is the only path. Realistic triggers: incident-investigation tooling that needs cross-site joins; replay debugging on retained chain history; aggregate analytics over a fleet.

---

## Cross-cutting: phase-independent concerns

### Test infrastructure shared across phases

- **DST harness.** Phases 2 (replication) and 3 (blob CAS GC) need deterministic-simulation tests for partition / failover / restart scenarios. Plan: extend the existing `loom_models.rs` infrastructure (Phase 2-A: 1–2 weeks), share it across both phases.
- **Failure-injection.** Per-phase needs network partition, disk-fill, and process-crash injection. Build once into the existing integration-test harness; reuse.
- **Bandwidth budgets.** Every phase that adds wire traffic gets a regression test pinning the budget (Phase 0: announcement size; Phase 1: greedy I/O; Phase 2: replication overhead; Phase 3: blob fetch). Treat regressions as test failures, not perf-only signals.

### Observability

- Every phase emits per-channel metrics into the existing `RpcMetricsRegistry` shape (recently extended for nRPC). Pattern: `dataforts_<feature>_<metric>{channel="X"}`. No new metric registry.
- Phase 0: `dataforts_chain_announcements_total`, `dataforts_chain_advertisement_bytes`.
- Phase 1: `dataforts_greedy_cache_hits_total`, `dataforts_greedy_evictions_total`, `dataforts_greedy_serve_count`.
- Phase 2: `dataforts_replication_lag_seconds{role="leader|replica"}`, `dataforts_replication_sync_bytes_total`, `dataforts_leader_changes_total`.
- Phase 3: `dataforts_blob_pool_size_bytes`, `dataforts_blob_dedup_hits_total`, `dataforts_blob_fetch_remote_total`.

### Feature flags + rollout

- Each phase ships gated behind a Cargo feature: `dataforts-greedy`, `dataforts-replication`, `dataforts-blob`, `dataforts-gravity`, `dataforts-ryw`, `dataforts-query`. Phase 0 is unconditional (it's a general capability-tag enhancement, not Dataforts-specific).
- Off-by-default in `ai2070-net` and `ai2070-net-sdk`. Pilots opt in.
- Each phase ships a `ConfigReadme.md`-style operational doc explaining tunables, expected resource cost, and rollback path.

### Cross-binding work

- Phase 0 needs Node + Python + Go + C binding updates for the new capability-tag shapes. Mechanical; ~1 week per binding.
- Phases 1–4 expose new config but reuse existing pub/sub/storage APIs — no binding changes required.
- Phases 5, 6 add new public API (`WriteToken`, `MeshDB`); estimate +1–2 weeks per binding when those phases ship.

---

## Sequencing recommendations

If we ship reactively (every phase parked until activation):

```
Phase 0 [first activation gate fires]
  ↓
Phase 1 ─→ Phase 4 (telemetry-driven)
  ↓
Phase 2 ─→ Phase 6 (parked unless query workload)
  ↓
Phase 3 (parallel with 2 if bandwidth)
  ↓
Phase 5 (slot anywhere)
```

If we ship proactively as a single "Dataforts v0" release:

```
Phase 0 (1–2 weeks)
  ↓ parallel
Phase 1 (1–2 weeks) ┐
Phase 2 (4–9 weeks) ├─→ Phase 4 once Phase 1 ships
Phase 3 (6–12 weeks)┘
  ↓
Phase 5 (2–4 weeks, anywhere)
```

Wall-clock for v0: **~2–3 months parallelized** with one engineer per track, **~6–9 months serialized**. The features doc's "2–3 months" estimate matches the parallel-track scenario.

**Default recommendation: ship reactively.** None of these has a real activation gate today. Build Phase 0 once we have a concrete consumer for any of 1–6, then sequence by demand. Most likely first trigger is Phase 1 (greedy LRU, smallest cost, broadest applicability).

---

## See also

- `DATAFORTS_FEATURES.md` — the audit of which wishlist items already ship vs. need this plan
- `REDEX_PLAN.md` — single-node v1 substrate (phase predecessor)
- `REDEX_V2_PLAN.md` — single-node v2 (tiering, indices, typed wrappers — orthogonal to this plan)
- `REDEX_MANIFEST_POINTER_DESIGN.md` — on-disk layout for Phase 3's blob CAS
- `NRPC_DESIGN.md` — the metrics + reliability surfaces Dataforts phases reuse
- `CORTEX_ADAPTER_PLAN.md` — local query layer that Phase 6's federated query layer would sit above
