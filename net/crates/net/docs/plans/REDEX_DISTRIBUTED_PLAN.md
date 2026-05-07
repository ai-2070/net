# RedEX Distributed — implementation plan

> Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md) (single-node v1) and [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) (single-node v2 — tiering, retention, indices). This doc plans the *cross-node replication* layer that v1's "Replication — not planned yet" and v2's "Cross-node RedEX is a later, explicit 'distributed RedEX' plan" both defer to. Ships in **The Warriors** release alongside the capability-taxonomy reorganization, federated query primitives, and the generalized `PlacementFilter`. Marketed in conversation as "RedEX V2" in the Warriors context, but to avoid namespace conflict with the existing single-node v2 plan, this doc uses **RedEX Distributed** throughout.

## Status

Design only. Activation gate: a workload requesting durability guarantees beyond single-node, where Phase 1 greedy LRU's probabilistic story is insufficient. Realistic triggers: payment-tier customer, compliance-bound data class, pilot whose RTO is "< 5 s on node failure."

## Frame

RedEX Distributed adds **orchestrated replication** to a v1/v2 RedEX channel. N replicas of the channel's log are maintained explicitly; configurable replication factor; pull-based catch-up on divergence; documented conflict policy (none expected because RedEX is append-only and seq-ordered, but the protocol must say so explicitly). Strong durability guarantee, in contrast to greedy LRU's probabilistic one. **Reuses existing primitives:** `ChannelPublisher`, `SubscriberRoster`, the causal-chain machinery, the standby-group election, the proximity graph. Adds one new wire subprotocol (`SUBPROTOCOL_REDEX`), one new daemon (`ReplicationCoordinator`), and consumes the `PlacementFilter` primitive shipped in the same Warriors release.

## Why this exists

Three load-bearing reasons:

1. **Single-node RedEX is not enough for any workload that loses business on node failure.** Greedy LRU offers probabilistic durability — popular data ends up replicated, unpopular data lives only at origin. For payments, compliance-bound data, and other "this must survive node failure" workloads, that's the wrong shape.
2. **The substrate already has all the primitives except the coordinator.** Standby groups handle daemon-state failover; ChannelPublisher carries reliable streams; capability tags carry replica advertisement; the proximity graph weights placement. The work is *composing* these into a replication coordinator, not building distributed-systems infrastructure from scratch.
3. **DST is the gating concern, not LoC.** Without a deterministic-simulation-test harness covering partition, failover, and rejoin sequences, replication ships with subtle bugs that surface only in production. Allocate ~30% of phase effort to DST harness work; treat it as a precondition, not an afterthought.

## What ships

Five things, in dependency order:

1. **Wire protocol** — `SUBPROTOCOL_REDEX` with `DISPATCH_REPLICA_SYNC` codes.
2. **`ReplicationConfig` in `ChannelConfig`** — opt-in per channel; defaults to `None` (single-node) for backward compatibility.
3. **`ReplicationCoordinator` daemon** — per replicated channel; spawned per replica; consumes `PlacementFilter` for placement decisions.
4. **Failover via standby groups** — reuses `groups::standby` machinery; no new election primitive.
5. **DST harness extension** — adds replication scenarios (partition, leader flap, replica rejoin) to the existing `loom_models.rs` infrastructure.

What this doc does NOT ship (deferred):

- Cross-channel atomic replication (per `REDEX_PLAN.md` non-goal #23 — RedEX has no cross-segment atomicity, and replication MUST NOT introduce that expectation).
- Multi-leader / writable-anywhere semantics. Append-only + monotonic seq makes single-leader the only sane shape; multi-leader requires conflict resolution that v1 RedEX is not designed for.
- Geo-replication policies beyond `PlacementFilter`'s proximity-bound. Cross-region replication works as a special case of placement; explicit geo-aware replication policies wait for a workload that demands them.
- Encryption-aware replica chaining. Replicas already carry encrypted bytes per the existing untrusted-relay model; nothing additional needed.

---

## Design

### 1. `ReplicationConfig`

Per-channel opt-in configuration. Goes alongside `ChannelConfig::retention`, `ChannelConfig::publish_caps`, etc.

```rust
pub struct ReplicationConfig {
    /// Replication factor — number of replicas (including leader) maintained.
    /// Default 3 when this struct is set.
    pub factor: u8,

    /// How replicas are chosen when first instantiated and on roster change.
    pub placement: PlacementStrategy,

    /// Heartbeat interval between leader and replicas.
    /// Default 500 ms.
    pub heartbeat_ms: u64,

    /// Optional override pinning the leader to a specific node.
    /// `None` = leader is the channel's natural publisher.
    pub leader_pinned: Option<NodeId>,

    /// Behavior when a replica falls below the channel's retention requirement
    /// due to local disk pressure.
    pub on_under_capacity: UnderCapacity,

    /// Bandwidth budget for replication-sync I/O, as a fraction of measured NIC peak.
    /// Default 0.5.
    pub replication_budget_fraction: f32,
}

pub enum PlacementStrategy {
    /// Spread across nodes per the `PlacementFilter` primitive shipped in The Warriors.
    /// Reads `metadata.intent`, `metadata.colocate-with`, `scope:`, proximity, and
    /// resource-availability axes. Default for new channels.
    Standard,

    /// Manual pinning. Used for special-case topologies and tests.
    Pinned(Vec<NodeId>),

    /// Strict colocation — all replicas must be on nodes already holding the chain
    /// referenced by `metadata.colocate-with-strict`. Refuses placement on
    /// insufficient-coverage nodes.
    ColocationStrict,
}

pub enum UnderCapacity {
    /// Fall through to greedy LRU if also enabled. Default.
    /// Replication factor is a hard guarantee on the leader; replicas are best-effort under capacity.
    Withdraw,

    /// Evict oldest local data to maintain the channel's retention even if total data exceeds disk.
    EvictOldest,
}
```

### 2. Wire protocol — `SUBPROTOCOL_REDEX`

New subprotocol ID claim. Reserves `DISPATCH_REPLICA_SYNC = 0x20..0x2F` (16 codes; v1 uses 4):

| Dispatch code | Direction | Purpose |
|---|---|---|
| `SYNC_REQUEST = 0x20` | replica → leader | Replica asks for events `[since_seq, since_seq + chunk_max)` |
| `SYNC_RESPONSE = 0x21` | leader → replica | Leader returns a bounded `read_range`-style stream |
| `SYNC_HEARTBEAT = 0x22` | bidirectional | Leader heartbeats `tail_seq`; replicas heartbeat their own `tail_seq` |
| `LEADER_ELECTION = 0x23` | broadcast within standby group | Triggered by leader-loss detection |

Reserved codes `0x24..0x2F` for future variants (range-bounded sync, parallel-stream sync, etc.). Document each new code in `SUBPROTOCOLS.md` as it lands.

**All `SUBPROTOCOL_REDEX` messages ride on the existing reliable-stream `Mesh::publish` machinery.** No new transport. Backpressure-aware via reliable-stream's flow control.

### 3. `ReplicationCoordinator`

One daemon per replicated channel per replica. Reuses the existing `MeshDaemon` trait (per `REDEX_PLAN.md`'s daemon model).

**Responsibilities:**

- Hold the replica's `tail_seq` for this channel
- Heartbeat to the leader at `heartbeat_ms` cadence
- On heartbeat-ack mismatch, issue `SYNC_REQUEST` to leader; apply the `SYNC_RESPONSE` events in seq order
- On leader-loss detection (3 consecutive missed heartbeats with hysteresis), trigger `LEADER_ELECTION`
- On graceful shutdown, withdraw the replica's `causal:` capability tag via Phase 0's `Mesh::withdraw_chain`

**Construction:**

- When a channel with `ReplicationConfig` is opened, the local `Redex` manager checks the `PlacementFilter` to determine if this node should be a replica.
- If yes, spawn a `ReplicationCoordinator` daemon. The daemon registers itself with the standby group for this channel.
- If no, the channel opens as a regular non-replicated channel locally; reads route to the nearest replica via the capability index.

### 4. Replica election

N nodes from the capability-advertising set, selected via `PlacementFilter::placement_score` for `Artifact::Replica`. The top-N by score become replicas.

**Triggers:**

- First publish on a newly-replicated channel
- Roster change (replica withdraws / new node advertises matching capabilities)
- Leader-loss detection in the standby group

**Anti-affinity (default):** the placement score includes a penalty for nodes already leading > 30% of channels in the local view, to spread leadership naturally across nodes. Configurable via a `StandardPlacement` parameter.

### 5. Pull-based catch-up

When a replica's `tail_seq` lags the leader's `tail_seq` (detected via heartbeat ack mismatch):

```
replica.coordinator
    --SYNC_REQUEST(channel, since_seq=replica.tail_seq, chunk_max=1MB)-->
    leader.coordinator

leader.coordinator
    -- read_range(since_seq, since_seq + chunk_max) -->
    leader.local_redex_file

leader.coordinator
    --SYNC_RESPONSE(events_chunk)-->
    replica.coordinator

replica.coordinator
    --append_batch(events_chunk)-->
    replica.local_redex_file

replica.tail_seq advances; loop until convergence or backpressure halts
```

**Throughput control:**

- Replication-sync I/O capped at `replication_budget_fraction × NIC_peak` (default 0.5).
- Backpressure-aware: reliable-stream flow control already handles this; replication just respects it.
- Per-request `chunk_max` (default 1 MB) bounds memory footprint of any single sync exchange.

### 6. Heartbeat + repair

```
every heartbeat_ms (default 500ms):
  leader -> replicas: SYNC_HEARTBEAT(tail_seq=leader.tail_seq)
  replicas -> leader: SYNC_HEARTBEAT(tail_seq=replica.tail_seq, lag=ack)

if leader sees a replica's lag exceed 3 × heartbeat_ms:
  leader logs replica as "behind" but takes no action — pull-based, replica drives recovery

if replica misses 3 consecutive leader heartbeats:
  replica triggers LEADER_ELECTION via standby-group machinery
```

**Hysteresis:** 3-missed-heartbeats threshold prevents election thrash under transient packet loss. Tunable per channel for tighter SLAs.

### 7. Failover

Leader fails (proximity graph reports `Unhealthy` OR 3 consecutive heartbeats missed):

1. Surviving replicas elect a new leader via the existing `groups::standby` election.
2. The new leader resumes appends from its local `tail_seq`; the old leader's gap (if any) is replayed when it rejoins.
3. The new leader's identity is broadcast via the standard standby-group promotion mechanism; the channel's `causal:` capability tag points to the new leader for routing purposes.
4. Cutover is atomic from the routing layer's perspective — the next publish on the channel goes to the new leader; in-flight reads against the old leader fall through to the new one via capability-index re-lookup.

**Critical property:** no two nodes can be leader for the same channel at the same time. Election is the standby group's responsibility; this design reuses that primitive without modification.

### 8. Replica rejoin

A replica that was partitioned, restarted, or otherwise out-of-sync:

```
replica.coordinator on startup:
  1. Check local tail_seq for the channel.
  2. Query capability index for current leader of channel.
  3. SYNC_REQUEST(since_seq=local.tail_seq).
  4. Apply SYNC_RESPONSE events in order.
  5. If gap > skip_threshold (default 100 MB), skip-ahead instead of replay; flag for divergence audit.
  6. Resume normal heartbeat cadence.
```

**Skip threshold:** 100 MB by default. If a replica is more than 100 MB behind, replaying the entire gap is more expensive than starting fresh from current tail. Skip-ahead preserves safety (chain remains causally valid because we're not rewriting history; we're choosing not to retain old history locally), but the replica logs a divergence event for operator review.

### 9. Conflict policy

**Append-only + monotonic seq → no conflicts possible IF leader is the sole appender.** This is the design constraint; document it explicitly:

- `RedexFile::append` on a non-leader replica returns `RedexError::NotLeader`.
- Pinned in tests; no opportunity for accidental dual-leader behavior.
- Documented in `SUBPROTOCOLS.md` as a hard invariant of `SUBPROTOCOL_REDEX`.

### 10. Cross-binding API surface

`ChannelConfig::replication: Option<ReplicationConfig>` is the only new public field. Round-trips through:

- Rust SDK
- Node binding (serde)
- Python binding (PyO3)
- Go binding (cgo)
- C binding (FFI)

Mostly serde plumbing for the config struct. ~3 days per binding.

### 11. Metrics

Per-channel metrics on the existing `RpcMetricsRegistry` shape (recently extended for nRPC). Pattern: `dataforts_<feature>_<metric>{channel="X"}`.

| Metric | Type | Meaning |
|---|---|---|
| `dataforts_replication_lag_seconds{channel,role}` | Gauge | Replica's `tail_seq` lag behind leader (per replica). Role is `leader` or `replica`. |
| `dataforts_replication_sync_bytes_total{channel}` | Counter | Cumulative bytes shipped via `SYNC_RESPONSE` |
| `dataforts_leader_changes_total{channel}` | Counter | Number of leader elections completed |
| `dataforts_replication_under_capacity_total{channel}` | Counter | Times the channel hit `UnderCapacity` policy |
| `dataforts_replication_skip_ahead_total{channel}` | Counter | Times a replica skipped instead of replaying a large gap |
| `dataforts_replication_election_thrash_total{channel}` | Counter | Elections triggered within 30 s of the previous one (saturation indicator) |

### 12. Observability + operator ergonomics

- `MeshDaemon::snapshot()` for `ReplicationCoordinator` includes `tail_seq`, `last_sync_at`, `last_heartbeat_at`, current role (leader / replica).
- A small `BEHAVIOR.md` section explains the replication model in operator-facing terms.
- A `CONFIG_REPLICATION.md` operational doc explains tunables, expected resource cost, and rollback.
- Feature flag: `dataforts-replication`. Off-by-default in `ai2070-net` and `ai2070-net-sdk`. Pilots opt in.
- Rollback path: flipping the feature off must safely degrade to single-node behavior. Replicas withdraw their `causal:` tags; reads route to the leader; the leader behaves as a normal non-replicated channel. Tested.

---

## Phasing

The 4-9 week range comes from DST depth. Sequence:

### Phase A — Wire protocol scaffold (1 week)

- Add `SUBPROTOCOL_REDEX` ID claim in `SUBPROTOCOLS.md` and `behavior::subprotocol`.
- Add `DISPATCH_REPLICA_SYNC` codes; encode `SYNC_REQUEST`, `SYNC_RESPONSE`, `SYNC_HEARTBEAT`, `LEADER_ELECTION` shapes via serde.
- Round-trip tests for each message shape.

### Phase B — `ReplicationConfig` + opt-in (3 days)

- Extend `ChannelConfig::replication: Option<ReplicationConfig>`.
- Default behavior: `None` → single-node, no replication. Existing channels unaffected.
- Cross-binding serde for `ReplicationConfig`. ~3 days for all four bindings.

### Phase C — `ReplicationCoordinator` daemon (1.5 weeks)

- New `behavior::replication::ReplicationCoordinator` implementing `MeshDaemon`.
- Spawn / register / withdraw lifecycle wired into `Redex::open_file` for replicated channels.
- Heartbeat loop on `heartbeat_ms`; basic state machine for `Leader` / `Replica` / `Candidate` (election in flight).
- Capability tag emission on join, withdrawal on drop. Reuses Phase 0's `Mesh::announce_chain` / `Mesh::withdraw_chain`.

### Phase D — Pull-based catch-up (1 week)

- `SYNC_REQUEST` issuance on heartbeat-ack lag.
- `SYNC_RESPONSE` generation via existing `RedexFile::read_range`.
- Replica-side `append_batch` application; preserve seq invariants.
- Skip-ahead path for gaps > `skip_threshold`.
- Bandwidth-budget enforcer respecting `replication_budget_fraction`.

### Phase E — Failover via standby-group election (1 week)

- Wire `groups::standby` election to `LEADER_ELECTION` triggers.
- Hysteresis: 3-missed-heartbeats threshold; tunable.
- Anti-affinity in `PlacementFilter` to spread leadership.
- Capability-tag updates on leader change; routing follows.

### Phase F — DST harness extension (1.5–3 weeks; widest variance)

This is the gating phase. Plan generously and treat regressions as test failures:

- Extend `loom_models.rs` to model the replication state machine (4 states × event transitions).
- Failure-injection scenarios: random partition, leader crash, replica crash, partial-network (one replica isolated), restart-during-sync.
- Convergence assertion: all surviving replicas converge to leader's `tail_seq` after recovery.
- Divergence-freedom: no two replicas declare different `tail_seq` for the same `seq` (stronger than convergence — pin in DST).
- Election-correctness: at any point, exactly one node believes it is leader for a channel, OR an election is in flight.
- Performance budget: replication overhead ≤ 30% of single-node append throughput at steady state.

### Phase G — Disk pressure + `UnderCapacity` (3 days)

- `ReplicationConfig::on_under_capacity` policy enforcement.
- `Withdraw` path: drop replica role; capability tag withdrawn; reads re-route.
- `EvictOldest` path: aggressive retention sweep; preserves replication factor at the cost of older data.
- Pin both behaviors in test.

### Phase H — Metrics + observability (3 days)

- All `dataforts_replication_*` metrics wired into `RpcMetricsRegistry`.
- `MeshDaemon::snapshot()` for `ReplicationCoordinator`.
- `BEHAVIOR.md` + `CONFIG_REPLICATION.md` operator docs.

### Phase I — Bindings (1 week, parallelisable)

- Mostly serde for `ReplicationConfig` across Node, Python, Go, C bindings.
- Mechanical; single engineer can do all four serially in a week, or four engineers can parallelise to 3 days.

**Total: 4–9 focused weeks**. Lower bound assumes DST harness work fits in 1.5 weeks (existing harness extends cleanly; no new modeling primitives needed). Upper bound assumes DST work hits unforeseen complexity (3 weeks dedicated to harness + scenario depth). Treat the upper bound as the planning target.

---

## Test strategy

### Unit

- `SUBPROTOCOL_REDEX` message round-trip (each dispatch code).
- `ReplicationConfig` field validation (factor ≥ 1, heartbeat_ms ≥ 100, etc.).
- `ReplicationCoordinator` state-machine transitions (Leader → Candidate → Replica, etc.).
- Skip-threshold logic boundaries (gap exactly at threshold, gap > threshold).
- Anti-affinity scoring in `PlacementFilter` with the replication penalty active.

### Integration

- **Steady-state convergence.** 3-replica + 1-publisher mesh. Continuous appends; assert all replicas converge to leader's tail within `2 × heartbeat_ms`.
- **Failover.** Kill leader; assert one replica promotes; new appends land on new leader; old leader on rejoin catches up via skip-or-replay path.
- **Disk pressure.** Replica configured below leader's retention; assert graceful withdrawal under `UnderCapacity::Withdraw`; assert eviction under `UnderCapacity::EvictOldest`.
- **Leader pinning.** With `leader_pinned: Some(N)`, election always returns N when N is healthy; falls through to standard election when N is unhealthy.
- **Bandwidth budget.** Under saturating publisher load, replication-sync I/O ≤ `replication_budget_fraction × NIC_peak`. Treat regression as test failure.
- **Capability-tag consistency.** Replica's `causal:` tag presence ↔ replica's actual replication state. No tag drift on graceful or ungraceful exits.

### DST (gating phase F)

- **Random partition.** Inject network partitions of varying duration; assert convergence on heal.
- **Restart-during-sync.** Replica crashes mid-`SYNC_RESPONSE` application; assert restart resumes correctly without data loss.
- **Election storms.** Sustained leader-loss detection for various reasons (heartbeat loss, proximity-graph false positives); assert election thrash is bounded by hysteresis.
- **Cross-replica divergence-freedom** (the strong assertion): no two replicas declare different `tail_seq` for the same `seq` at any point during any failure scenario.

### Performance

- Replication overhead ≤ 30% of single-node append throughput at steady state.
- Replication-sync I/O ≤ 50% of NIC peak under saturating append rate (default budget; configurable).
- Failover RTO < 5 × heartbeat_ms (default 2.5 s; configurable for tighter SLA workloads).

---

## Open design questions to lock before implementation

These are real decisions. Don't start the implementation without explicit answers; cost of getting them wrong is days of rework each.

1. **Leader scope.** Is the replication leader the same node as the channel's `ChannelPublisher` home, or a separately-elected entity? **Recommendation:** same node by default (publisher is the natural leader for an append-only channel), with explicit override via `leader_pinned: Option<NodeId>` for split publisher/leader topologies. Pin in test.

2. **What does "replicated" mean for retention?** If a channel retains 100 MB and a replica drops below that under disk pressure, does it withdraw replicaship or evict the oldest local data? **Recommendation:** `UnderCapacity::Withdraw` as default — fall through to greedy LRU if also enabled; replication factor is a hard guarantee on the leader, replicas are best-effort under capacity. `UnderCapacity::EvictOldest` available as opt-in. Caller picks.

3. **Cross-segment atomicity.** Per `REDEX_PLAN.md` non-goal #23, RedEX has no cross-segment atomicity. Replication MUST NOT introduce that expectation; replicas catch up segment-by-segment. Document explicitly in `SUBPROTOCOLS.md`.

4. **Membership during partition.** If a replica is partitioned but eventually rejoins, does it re-catch-up from current tail or replay the gap? **Recommendation:** replay gap if `gap < skip_threshold` (default 100 MB); skip-ahead + flag for divergence audit if larger. Reuses standby-group's replay machinery.

5. **Bandwidth budget under network saturation.** Replication sync rides on the same wire as application traffic. Cap replication-sync I/O at `replication_budget_fraction × NIC peak` (default 0.5). Backpressure-aware via reliable-stream's existing flow control.

6. **Leader concentration prevention.** A single node leading > 30% of channels is a write hotspot. **Recommendation:** anti-affinity term in `PlacementFilter::placement_score` for `Artifact::Replica` that penalizes nodes already leading many channels in the local view. Default penalty kicks in at 30% threshold; configurable.

---

## Risks

- **DST story is the hardest part.** No replication design survives without DST coverage for partition + leader-flap + rejoin sequences. **Allocate ~30% of phase effort to DST harness work.** Treat this as non-negotiable; do not ship without it.
- **Leader concentration → write hotspots.** Mitigation above (anti-affinity); if telemetry shows > 30% leadership concentration on a single node, escalate.
- **Subprotocol code surface adds ~1500–2000 LoC to the mesh adapter.** Audit footprint before merge. Coordinator should compose from existing primitives, not invent new ones.
- **Election thrash under transient packet loss.** Aggressive heartbeat timeouts cause spurious elections. Mitigation: hysteresis (3 consecutive missed heartbeats); pin in DST.
- **Capability-tag staleness during election.** During an election, the channel's `causal:` tag may briefly point to the dead leader. Mitigation: fast withdrawal on leader-loss detection; reads gracefully degrade to capability-index re-lookup. Pin in tests.

---

## Effort

**4–9 focused weeks.** Wide range driven by DST harness depth.

- ~2500 LoC core (subprotocol + coordinator + election integration + placement-filter anti-affinity term)
- ~3500 LoC tests (unit + integration + DST scenarios)
- ~2 weeks DST harness extension (lower bound; can extend to 3 if scenarios surface unexpected complexity)
- ~1 week bindings (mostly serde; parallelisable)

Bindings are the only piece that can be done in parallel with core; everything else has linear dependencies.

---

## Activation gate

Workload requesting durability guarantees beyond single-node, where Phase 1 greedy LRU's probabilistic story is insufficient.

Realistic triggers:

- Payment-tier customer (settlement records cannot be probabilistically durable)
- Compliance-bound data class (regulatory retention requires explicit replication)
- Pilot whose RTO is "< 5 s on node failure"
- Multi-datacenter durability story (replicas spread across availability zones)

When any of these activate, ship as part of The Warriors release alongside the other foundation work.

---

## Cross-cutting: relationship to other Warriors phases

This phase consumes:

- **Phase 0 (capability-tag discovery).** `causal:` advertisement / withdrawal for replica join/leave; `metadata.intent` and `metadata.colocate-with` consumed by placement.
- **Phase 6 (federated query primitives).** Used internally for replica discovery (`find_chain_holders`) and operator-facing introspection.
- **Phase 7 (`PlacementFilter` + Mikoshi).** The placement primitive is consumed for replica placement decisions; the anti-affinity penalty for leader concentration is a `StandardPlacement` configuration.

This phase produces:

- A `causal:` capability tag emitter and lifecycle that other Warriors phases consume (Phase 1 greedy reads from the same tag advertisements).
- A `MeshDaemon`-shaped daemon that demonstrates `PlacementFilter` consumption (template for future placement-driven daemons).
- DST scenarios that other Warriors phases can extend (partition + restart sequences applicable to the discovery primitive's announcement traffic, the placement filter's scoring under churn, etc.).

---

## See also

- [`REDEX_PLAN.md`](REDEX_PLAN.md) — single-node v1 substrate (predecessor)
- [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) — single-node v2 (tiering, retention, indices) — explicitly local-only; this doc is the *cross-node* counterpart
- [`misc/DATAFORTS_PLAN.md`](misc/DATAFORTS_PLAN.md) — phased plan covering The Warriors + Rebel Yell scope; this doc is the implementation detail for Phase 2 (RedEX V2 / distributed RedEX) within that plan
- [`misc/DATAFORTS_FEATURES.md`](misc/DATAFORTS_FEATURES.md) — feature audit; replication is the most concrete deferred-but-named item
- [`SUBPROTOCOLS.md`](SUBPROTOCOLS.md) — wire-protocol subprotocol IDs; `SUBPROTOCOL_REDEX` claims its slot here
- [`misc/NRPC_DESIGN.md`](misc/NRPC_DESIGN.md) — pattern for a subprotocol convention layered on existing reliable-stream + capability infrastructure; `SUBPROTOCOL_REDEX` follows the same architectural shape
- `RELEASE_ROADMAP.md` — The Warriors release context; this work ships as part of that release
