# RedEX Distributed — implementation plan

> Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md) (single-node v1) and [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) (single-node v2 — tiering, retention, indices). This doc plans the *cross-node replication* layer that v1's "Replication — not planned yet" and v2's "Cross-node RedEX is a later, explicit 'distributed RedEX' plan" both defer to. Ships in **The Warriors** release alongside the capability-taxonomy reorganization, federated query primitives, and the generalized `PlacementFilter`. Marketed in conversation as "RedEX V2" in the Warriors context, but to avoid namespace conflict with the existing single-node v2 plan, this doc uses **RedEX Distributed** throughout.

## Status

**Design locked; blocked on Capability Phase B (and partially F).** All open design questions are ratified — see [Locked decisions](#locked-decisions).

Hard prerequisites:
- **Capability Phase B** (tag-discovery primitives `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders`) — RedEX Phases C/D/E cannot start until this lands.
- **Capability Phase F** (`PlacementFilter` + `IntentRegistry` + anti-affinity) — needed for *replica placement* in Phase C, but **not** for leader election. Election is decentralized and deterministic (nearest-RTT + NodeId tiebreak); see [Locked decision 3](#3-election-strategy--deterministic-nearest-rtt-with-nodeid-tiebreak).

Phase A (wire protocol scaffold), Phase B (this plan's `ReplicationConfig` opt-in), Phases G–I can proceed in isolation as scaffolding.

Activation gate (when Warriors as a whole ships) is unchanged: a workload requesting durability guarantees beyond single-node. Realistic triggers: payment-tier customer, compliance-bound data class, pilot whose RTO is "< 5 s on node failure."

## Frame

RedEX Distributed adds **orchestrated replication** to a v1/v2 RedEX channel. N replicas of the channel's log are maintained explicitly; configurable replication factor; pull-based catch-up on divergence; documented conflict policy (none expected because RedEX is append-only and seq-ordered, but the protocol must say so explicitly). Strong durability guarantee, in contrast to greedy LRU's probabilistic one. **Reuses existing primitives:** `ChannelPublisher`, `SubscriberRoster`, the causal-chain machinery, the standby-group election, the proximity graph. Adds one new wire subprotocol (`SUBPROTOCOL_REDEX`), one new daemon (`ReplicationCoordinator`), and consumes the `PlacementFilter` primitive shipped in the same Warriors release.

## Why this exists

Three load-bearing reasons:

1. **Single-node RedEX is not enough for any workload that loses business on node failure.** Greedy LRU offers probabilistic durability — popular data ends up replicated, unpopular data lives only at origin. For payments, compliance-bound data, and other "this must survive node failure" workloads, that's the wrong shape.
2. **The substrate already has all the primitives except the coordinator.** Standby groups handle daemon-state failover; ChannelPublisher carries reliable streams; capability tags carry replica advertisement; the proximity graph weights placement. The work is *composing* these into a replication coordinator, not building distributed-systems infrastructure from scratch.
3. **DST is the gating concern, not LoC.** Without a deterministic-simulation-test harness covering partition, failover, and rejoin sequences, replication ships with subtle bugs that surface only in production. Allocate ~30% of phase effort to DST harness work; treat it as a precondition, not an afterthought.

## What ships

Five things, in dependency order:

1. **Wire protocol** — `SUBPROTOCOL_REDEX` with `DISPATCH_REPLICA_SYNC` codes; full byte-level layouts pinned in [§2 Wire protocol](#2-wire-protocol--subprotocol_redex). Four dispatch codes total — election needs **no wire protocol** because it's a pure deterministic function over local state.
2. **`ReplicationConfig` in `ChannelConfig`** — opt-in per channel; defaults to `None` (single-node) for backward compatibility.
3. **`ReplicationCoordinator` daemon** — per replicated channel; spawned per replica; consumes `PlacementFilter` (Capability F) for *replica placement* (which N nodes become replicas) at construction time only. Election is independent of `PlacementFilter`.
4. **Failover via deterministic nearest-RTT election** — `StandbyGroup` provides membership + failure detection; the *selection function* it invokes on leader loss is RedEX's: pick the healthy replica with the lowest RTT to self, tie-break by lexicographic NodeId. Pure function; every node computes the same winner from the same inputs. No new election protocol; no broadcast/epoch/collection-window machinery. See [Locked decision 3](#3-election-strategy--deterministic-nearest-rtt-with-nodeid-tiebreak).
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

New subprotocol ID claim. Subprotocol header is the standard `subprotocol_id: u16 LE` + `dispatch_code: u8` = **3-byte prefix** on every message; payload follows. Reserves `DISPATCH_REPLICA_SYNC = 0x20..0x2F` (16 codes; v1 uses 4):

| Dispatch code | Direction | Purpose |
|---|---|---|
| `SYNC_REQUEST = 0x20` | replica → leader | Replica asks for events `[since_seq, since_seq + chunk_max)` |
| `SYNC_RESPONSE = 0x21` | leader → replica | Leader returns a bounded `read_range`-style stream |
| `SYNC_HEARTBEAT = 0x22` | bidirectional | Leader heartbeats `tail_seq`; replicas heartbeat their own `tail_seq` |
| `SYNC_NACK = 0x23` | leader → replica | Structured rejection (NotLeader / BadRange / Backpressure / ChannelClosed) |

Reserved codes `0x24..0x2F` for future variants (range-bounded sync, parallel-stream sync, etc.). Document each new code in `SUBPROTOCOLS.md` as it lands.

**No `LEADER_ELECTION` on the wire.** Election is a pure deterministic function over each node's already-known state (proximity-graph RTT matrix + replica-set membership + NodeId ordering); every node computes the same winner independently. `StandbyGroup`'s existing promotion broadcast announces the result; no RedEX-specific election message is needed. See [Locked decision 3](#3-election-strategy--deterministic-nearest-rtt-with-nodeid-tiebreak).

**Encoding conventions (LOCKED).** All multi-byte integers are **little-endian**, fixed-width (matching `redex-disk`'s existing `to_bytes` convention). No varints — DST modeling is simpler with fixed sizes, and the per-message payloads are bounded enough that the wire-cost difference is negligible. `ChannelId` is the channel name's 32-byte BLAKE2s hash (matches the existing `ChannelName::hash()` convention). Strings are length-prefixed `(u16 LE len, [len] utf-8 bytes)`.

**Byte layouts.**

```
SYNC_REQUEST (replica → leader)
┌──────────────────────────────────────────────────────────────┐
│ subprotocol_id   u16 LE      = SUBPROTOCOL_REDEX             │
│ dispatch_code    u8          = 0x20                          │
│ channel_id       [u8; 32]    BLAKE2s of channel name         │
│ since_seq        u64 LE      first seq the replica wants     │
│ chunk_max        u32 LE      max bytes the leader may send   │
│                              in the matching SYNC_RESPONSE   │
└──────────────────────────────────────────────────────────────┘
fixed size: 3 + 32 + 8 + 4 = 47 bytes

SYNC_RESPONSE (leader → replica)
┌──────────────────────────────────────────────────────────────┐
│ subprotocol_id   u16 LE      = SUBPROTOCOL_REDEX             │
│ dispatch_code    u8          = 0x21                          │
│ channel_id       [u8; 32]                                    │
│ first_seq        u64 LE      seq of events[0] in this chunk  │
│ event_count      u32 LE      number of event records below   │
│ events           [Event; N]  N = event_count                 │
│                                                              │
│   Event record (length-prefixed):                            │
│     event_seq     u64 LE                                     │
│     payload_len   u32 LE                                     │
│     payload       [u8; payload_len]                          │
└──────────────────────────────────────────────────────────────┘
variable size; bounded by chunk_max from the matching SYNC_REQUEST.
event_seq monotonically increases across the chunk;
no gaps within a chunk (gaps are explicit-skip, see Phase D).

SYNC_HEARTBEAT (bidirectional)
┌──────────────────────────────────────────────────────────────┐
│ subprotocol_id   u16 LE      = SUBPROTOCOL_REDEX             │
│ dispatch_code    u8          = 0x22                          │
│ channel_id       [u8; 32]                                    │
│ tail_seq         u64 LE      sender's current tail           │
│ role             u8          0 Leader, 1 Replica,            │
│                              2 Candidate, 3 Idle             │
│ wall_clock_ms    u64 LE      sender's monotonic-clock ms     │
│                              (drift detection only — not     │
│                              used for ordering)              │
└──────────────────────────────────────────────────────────────┘
fixed size: 3 + 32 + 8 + 1 + 8 = 52 bytes

SYNC_NACK (leader → replica)
┌──────────────────────────────────────────────────────────────┐
│ subprotocol_id   u16 LE      = SUBPROTOCOL_REDEX             │
│ dispatch_code    u8          = 0x23                          │
│ channel_id       [u8; 32]                                    │
│ since_seq        u64 LE      echoes the rejected request     │
│ error_code       u8          1 NotLeader, 2 BadRange,        │
│                              3 Backpressure, 4 ChannelClosed │
│ detail_len       u16 LE      length of the optional detail   │
│ detail           [u8; len]   utf-8 diagnostic; may be empty  │
└──────────────────────────────────────────────────────────────┘
NACK semantics: the leader MUST send SYNC_NACK rather than
silently closing on any rejection — silent close is reserved
for transport-level failure only (the reliable-stream layer
surfaces those separately). Replicas treat NACK as a typed
error with retry policy keyed on `error_code`:
  - 1 NotLeader   → re-resolve leader via find_chain_holders
  - 2 BadRange    → trim local tail; retry from leader's first_seq
  - 3 Backpressure → exponential backoff; same SYNC_REQUEST
  - 4 ChannelClosed → withdraw replica role; emit metric
```

**Range encoding** (used by future `0x25..0x2F` variants and by `read_range` parameters within RedEX itself): `(start_seq: u64 LE, end_seq: u64 LE)` pairs. Half-open `[start, end)` per the existing `RedexFile::read_range` convention. No varints; explicit u64 endpoints.

**All `SUBPROTOCOL_REDEX` messages ride on the existing reliable-stream `Mesh::publish` machinery.** No new transport. Backpressure-aware via reliable-stream's flow control.

### 3. `ReplicationCoordinator`

One daemon per replicated channel per replica. Reuses the existing `MeshDaemon` trait (per `REDEX_PLAN.md`'s daemon model).

**State machine (LOCKED — 4 states).**

```rust
pub enum ReplicaState {
    /// Sole appender for the channel. Heartbeats to all replicas at
    /// `heartbeat_ms`; serves SYNC_REQUEST; rejects any local append
    /// from non-self with SYNC_NACK::NotLeader.
    Leader,
    /// Catching up via SYNC_REQUEST or steady-state lagging by ≤ 1
    /// heartbeat. Heartbeats own `tail_seq` to the leader.
    Replica,
    /// Brief transient state: 3 consecutive heartbeats missed, the
    /// coordinator is computing the deterministic election winner
    /// (nearest-RTT in the healthy replica set, NodeId tiebreak —
    /// see §4) and committing the transition. Refuses both append
    /// and serve until the transition resolves to Leader or
    /// Replica. Duration is the elect() function's compute time
    /// (microseconds), NOT a broadcast wait — there is no election
    /// protocol on the wire.
    Candidate,
    /// Node carries the channel's storage but has no replica role
    /// for it (e.g. former replica that withdrew under disk
    /// pressure, or a node that holds the chain via greedy LRU
    /// without being part of the explicit replication set). Does
    /// NOT heartbeat; does NOT participate in election. Reads
    /// fall through to the leader.
    Idle,
}
```

Transitions:
- `Idle → Replica` on join (capability filter chose this node)
- `Replica → Candidate` on 3-missed-heartbeats
- `Candidate → Leader` on winning election
- `Candidate → Replica` on losing election (becoming follower of the new leader)
- `Leader → Idle` on graceful relinquish (admin command, leader_pinned override migrates)
- `Replica → Idle` on disk-pressure withdrawal under `UnderCapacity::Withdraw`
- Any → `Idle` on channel close

The earlier draft inconsistently named 3 states in some places and 4 in others. **The 4-state model above is the single source of truth.**

**Responsibilities:**

- Hold the replica's `tail_seq` for this channel
- Heartbeat to the leader at `heartbeat_ms` cadence (when in `Replica`)
- Serve `SYNC_REQUEST` and emit `SYNC_HEARTBEAT` (when in `Leader`)
- On heartbeat-ack mismatch, issue `SYNC_REQUEST` to leader; apply the `SYNC_RESPONSE` events in seq order
- On leader-loss detection (3 consecutive missed heartbeats with hysteresis), enter `Candidate`, run the deterministic `elect()` function from §4, then transition to `Leader` (if self is the winner) or `Replica` (if a peer is the winner)
- On a leadership transition (own `elect()` result changed from the previously believed leader, or saw a `role=Leader` heartbeat from a peer other than the current believed leader), emit `Mesh::withdraw_chain(channel_id, by_node = previous_leader)` as a witness — see §4 "Witness withdrawal" for rationale. Idempotent across the N witnessing replicas; the capability layer dedups.
- On graceful shutdown, transition to `Idle` and withdraw the replica's `causal:` capability tag via Capability Phase B's `Mesh::withdraw_chain`

**Construction:**

- When a channel with `ReplicationConfig` is opened, the local `Redex` manager checks `PlacementFilter::placement_score` to determine if this node should be a replica (top-N by score).
- If yes, spawn a `ReplicationCoordinator` daemon initialized in `Replica` state. The daemon registers itself with the channel's `StandbyGroup` (membership + failure detection only — see §7).
- If no, the channel opens as a regular non-replicated channel locally; reads route to the nearest replica via `Mesh::find_chain_holders`.

### 4. Replica selection vs. leader election

Two distinct concerns. Keeping them separate is the load-bearing simplification.

**Replica selection (Phase C — placement).** N nodes from the capability-advertising set, selected via `PlacementFilter::placement_score` for `Artifact::Replica`. The top-N by score become replicas. The locked Capability §7 tie-breaking (RTT → free-resource → lexicographic NodeId) applies on equal scores. Anti-affinity at 30% leadership-concentration threshold (`AntiAffinityConfig`) prevents central nodes from accumulating too much replica membership across channels.

Triggers:
- First publish on a newly-replicated channel
- Roster change (replica withdraws / new node advertises matching capabilities)
- Disk-pressure withdrawal by an existing replica

**Leader election (Phase E — deterministic, no `PlacementFilter` dependency).** Pure function over locally-known state. Every node computes the same winner.

```
elect(replica_set, self) -> NodeId:
    R = { r ∈ replica_set : r is healthy in self's local view }
    if R is empty: return None       // partition isolated us; no leader
    sorted = R sorted by (rtt_to(self, r), r.node_id_lex)
                                     // primary: lower RTT wins
                                     // tie-break: lexicographic NodeId
    return sorted[0]                 // self if RTT(self,self)=0 wins
```

This is exactly Cassandra/Scylla coordinator selection, Consul LAN-segment fallback, Dynamo-style preferred replica — a time-tested decentralized pattern. RedEX's invariants (single writer + append-only + monotonic seq) make leadership a centrality concern, not a placement-quality concern: any healthy replica is a valid leader, so picking the most network-central one is enough.

**Why not consult `PlacementFilter` for election:** placement is "which N nodes should be replicas" (a topology + capability + intent question). Election is "which of those existing replicas should be sole appender right now" (a centrality + liveness question). Folding placement scoring into election would re-litigate the placement question on every leader loss, with no benefit — the replicas were already chosen with the right scoring at Phase C.

Triggers:
- Leader-loss detection by any surviving replica (3 consecutive missed heartbeats — the existing hysteresis)
- `StandbyGroup` failure-detection event (membership view shrinks)

**`StandbyGroup` provides membership + failure detection; RedEX provides the selection function.** The pluggable hook is "elect leader given current healthy set" — RedEX supplies the deterministic nearest-RTT function above; `StandbyGroup` invokes it on leader-loss and uses its own promotion broadcast to announce the winner. `StandbyGroup::promote()`'s default "highest synced_through" behavior is preserved for daemon migration; only RedEX channels override the selection function.

**Convergence and split-brain.** Because the selection function is pure and deterministic, every replica computes the same winner from the same RTT matrix + membership set within a single partition. Two partitions with disjoint views can briefly compute different winners; both leaders accept appends until partition heals. This is the same dual-leader-during-partition risk every leader-elected system has — RedEX's mitigation is operator-controlled placement (don't span replica sets across partition boundaries you can't tolerate). The `causal:` capability tag's higher-epoch supersession on partition heal converges routing back to a single leader; divergent appends made during the partition are flagged for operator review via the `dataforts_replication_skip_ahead_total` metric. Pin in DST.

**Witness withdrawal (the herald pattern).** The new leader announces itself via `Mesh::announce_chain` — that's the squire riding to the towns. But the deposed leader's stale `causal:` tag would normally only get reaped when the network observes its disconnect, which can lag (especially in the partition-heal case where the deposed leader is still nominally alive on its own side). Faster reaping comes from the **witnesses** — every peer replica that observes a leadership transition (own `elect()` result changed, or saw a `role=Leader` heartbeat from a non-believed-leader peer) also issues `Mesh::withdraw_chain(channel_id, by_node = previous_leader)`. The withdraw is idempotent across the N witnessing peers; the capability layer dedups. This is free robustness: the peers were already going to observe the transition (they're computing `elect()` in lockstep on the same membership set); we're just letting them speak the fact that the king is dead instead of waiting for the disconnect-observer subsystem to catch up. Pin in DST under the partition-heal scenario — the witness path should reap the stale tag strictly faster than the disconnect-observer path.

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

1. Each surviving replica transitions `Replica → Candidate` and runs `elect(replica_set, self)` (§4) — pure function, ~microseconds.
2. The winner transitions `Candidate → Leader`; losers transition `Candidate → Replica`. No broadcast, no wait, no quorum: every healthy replica converges to the same winner from the same RTT matrix + membership set.
3. The new leader resumes appends from its local `tail_seq`; the old leader's gap (if any) is replayed when it rejoins via the standard rejoin path (§8).
4. The new leader emits `causal:` capability-tag updates via `Mesh::announce_chain`; `StandbyGroup`'s existing promotion broadcast announces the new leader to the rest of the mesh. The channel's routing tag now points to the new leader. The deposed leader's `causal:` tag is reaped by **two paths in parallel**: (a) the standard withdrawal on observed disconnect, and (b) **witness withdrawals** — every peer replica that observes the transition issues `Mesh::withdraw_chain(channel_id, by_node = previous_leader)`. The peers saw the king die; they speak the fact. Idempotent across N witnesses; the capability layer dedups. Witness withdrawal closes the partition-heal stale-tag window faster than waiting on disconnect observation alone.
5. Cutover is atomic from the routing layer's perspective — the next publish on the channel goes to the new leader; in-flight reads against the old leader fall through to the new one via `Mesh::find_chain_holders` re-lookup.

**Critical safety property:** no two nodes can be leader for the same channel at the same time *within a single partition*.

The deterministic election guarantees this in two ways:
- Every replica computes `elect()` over the same locally-known healthy set, so within a partition all replicas pick the same winner. There's no broadcast race because there's no broadcast.
- Membership view convergence is `StandbyGroup`'s job: when a replica's view of "who's healthy" changes, it re-runs `elect()` and either retains its role (still winner) or transitions out (peer is now winner). The transitions are monotonic relative to view changes.

**Cross-partition split-brain.** Two partitions with disjoint views of "who's healthy" will compute different winners. Both leaders accept appends until partition heals; the divergent appends are flagged via `dataforts_replication_skip_ahead_total` and surface to operator review. This risk is intrinsic to leader-elected systems without quorum; RedEX is not solving it here. Operator mitigation: don't span replica sets across partition boundaries you can't tolerate. Pin both the partition-healing convergence behavior and the divergence-flagging metric in DST (Phase F).

**Membership + failure detection delegate to `StandbyGroup`.** RedEX uses standby-group machinery to learn "this replica is alive" and "this replica just disconnected." `StandbyGroup` invokes RedEX's deterministic selection function on leader loss — see [Locked decision 3](#3-election-strategy--deterministic-nearest-rtt-with-nodeid-tiebreak).

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
| `dataforts_replication_witness_withdrawals_total{channel}` | Counter | Times a peer replica issued a witness `Mesh::withdraw_chain` for a deposed leader's tag (per witnessing replica) |

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

Implementable in isolation; does not depend on Capability B/F.

- Add `SUBPROTOCOL_REDEX` ID claim in `SUBPROTOCOLS.md` and `behavior::subprotocol`.
- Add the four `DISPATCH_REPLICA_SYNC` codes (0x20..0x23) per [§2](#2-wire-protocol--subprotocol_redex). Encode/decode each message at the byte layouts pinned there.
- Round-trip tests for each message shape; pin the exact byte layout (fixed-size messages assert byte-count constants; variable-size messages assert layout under a property test).

### Phase B — `ReplicationConfig` + opt-in (3 days)

- Extend `ChannelConfig::replication: Option<ReplicationConfig>`.
- Default behavior: `None` → single-node, no replication. Existing channels unaffected.
- Cross-binding serde for `ReplicationConfig`. ~3 days for all four bindings.

### Phase C — `ReplicationCoordinator` daemon (1.5 weeks)

**Hard prerequisite: Capability Phase B (tag-discovery primitives).** Cannot start until `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` exist.

- New `behavior::replication::ReplicationCoordinator` implementing `MeshDaemon`.
- Spawn / register / withdraw lifecycle wired into `Redex::open_file` for replicated channels.
- Heartbeat loop on `heartbeat_ms`; full 4-state machine `ReplicaState::{Leader, Replica, Candidate, Idle}` per [§3](#3-replicationcoordinator). All transitions exhaustive; pin in unit tests.
- Capability tag emission on `Idle → Replica` and `Replica → Leader`; withdrawal on `* → Idle`. Uses Capability Phase B's `Mesh::announce_chain` / `Mesh::withdraw_chain`.

### Phase D — Pull-based catch-up (1 week)

**Hard prerequisite: Capability Phase B + Phase C of this plan.**

- `SYNC_REQUEST` issuance on heartbeat-ack lag.
- `SYNC_RESPONSE` generation via existing `RedexFile::read_range`.
- `SYNC_NACK` emission on `NotLeader` / `BadRange` / `Backpressure` / `ChannelClosed` per the typed error path in [§2](#2-wire-protocol--subprotocol_redex). Replicas implement the matching retry policy keyed on `error_code`.
- Replica-side `append_batch` application; preserve seq invariants.
- Skip-ahead path for gaps > `skip_threshold`.
- Bandwidth-budget enforcer respecting `replication_budget_fraction`.

### Phase E — Failover via deterministic nearest-RTT election (1 week)

**Hard prerequisite: Phase C of this plan (which transitively requires Capability B). Does NOT depend on Capability F** — election is wire-free and `PlacementFilter`-free.

- `ReplicationCoordinator` consumes `StandbyGroup` events for membership + failure detection only.
- Implement the deterministic `elect()` selection function from [§4](#4-replica-selection-vs-leader-election); register it as `StandbyGroup`'s leader-selection hook for replicated channels (this is the one new `groups::standby` integration point — a pluggable selection-fn slot, NOT a replacement of the daemon-migration default).
- Hysteresis: 3-missed-heartbeats threshold; tunable.
- Capability-tag updates on leader change via `Mesh::announce_chain` (new leader self-announces); `StandbyGroup`'s existing promotion broadcast announces the winner.
- Witness withdrawal: every peer replica that observes a leadership transition issues `Mesh::withdraw_chain(channel_id, by_node = previous_leader)` in parallel with its own state transition. Idempotent across N witnesses. Closes the stale-tag window in the partition-heal scenario faster than disconnect-observation reaping.
- Pin the no-two-leaders-within-a-partition invariant in unit tests AND DST (Phase F). Cross-partition split-brain is documented as out-of-scope (operator concern).

### Phase F — DST harness extension (1.5–3 weeks; widest variance)

**Hard prerequisite: Phases C, D, E of this plan + Capability F.**

This is the gating phase. Plan generously and treat regressions as test failures:

- Extend `loom_models.rs` to model the 4-state replication state machine (`Leader` / `Replica` / `Candidate` / `Idle`) with the locked transitions from [§3](#3-replicationcoordinator).
- Failure-injection scenarios: random partition, leader crash, replica crash, partial-network (one replica isolated), restart-during-sync, partition-heal-with-stale-leader, asymmetric-RTT (two replicas disagree on which peer is "nearest" because their RTT measurements diverged transiently).
- Convergence assertion: all surviving replicas converge to leader's `tail_seq` after recovery.
- Divergence-freedom: no two replicas declare different `tail_seq` for the same `seq` (stronger than convergence — pin in DST).
- Election determinism: given the same RTT matrix + healthy set + NodeId set, every replica computes the same winner. Pin against the asymmetric-RTT scenario — RTT measurements should converge across replicas under the proximity graph's existing smoothing, but transient divergence must not produce dual-leader windows.
- Election-correctness within a partition: at any point, exactly one node believes it is leader for a channel within any single partition. Cross-partition split-brain is explicitly NOT asserted (it's out-of-scope per [Locked decision 3](#3-election-strategy--deterministic-nearest-rtt-with-nodeid-tiebreak)); instead pin that on partition heal, the divergent appends are flagged via `dataforts_replication_skip_ahead_total`.
- Witness-withdrawal timing: under the partition-heal-with-stale-leader scenario, the deposed leader's `causal:` tag is reaped within 1 heartbeat via the witness path (`dataforts_replication_witness_withdrawals_total` increments) — strictly faster than the disconnect-observer reaping path. Pin both paths fire and that the witness path is the first to clear the tag from at least one observer's view.
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

## Locked decisions

The plan's previously-open questions are ratified below. Each is treated as binding for Phase A onward; reverting any of these is a plan-revision concern, not a per-phase implementation concern.

### Hard prerequisites (must ship before Phase C)

#### 1. Capability Phase B (tag discovery) is a hard prerequisite

RedEX Distributed depends on three primitives that **do not exist** in `net/crates/net/src` today:

- `Mesh::announce_chain(origin_hash, tip_seq)` — leader / replica advertises "I hold this chain"
- `Mesh::withdraw_chain(origin_hash)` — graceful tag retraction on shutdown / role-loss
- `Mesh::find_chain_holders(origin_hash) -> Vec<NodeId>` — replica discovery + leader resolution

These ship in **Capability System Plan Phase B** (see `CAPABILITY_SYSTEM_PLAN.md`'s phasing). RedEX Phases C/D/E/F (coordinator, catch-up, failover, DST) **cannot start** until Phase B lands. RedEX Phases A, B (this plan), G, H, I can scaffold in isolation.

#### 2. Capability Phase F (`PlacementFilter`) is a hard prerequisite for *replica placement* (Phase C only)

Used by:
- `PlacementFilter::placement_score(target, &Artifact::Replica { ... })` — top-N replica selection at Phase C.
- `IntentRegistry` (consumed via `metadata.intent`) — placement-time intent matching.
- `AntiAffinityConfig` (the leadership-concentration penalty) — applied at placement time so central nodes don't accumulate replica membership across too many channels.

**NOT used by election.** The earlier draft made Phase E depend on Capability F too, via a placement-scored election. Decision 3 below replaces that with a deterministic nearest-RTT election that consumes only the existing proximity graph + NodeId ordering — no `PlacementFilter` dependency. The Phase E gate drops to "Capability B + Phase C of this plan." The composite gate becomes "Capability B before C/D/E; Capability F before C." (Phase E only needs Phase C, transitively, plus Capability B.)

### Election strategy

#### 3. Election strategy — deterministic nearest-RTT with NodeId tiebreak

RedEX supplies the selection function `StandbyGroup` invokes on leader loss; the function is a pure deterministic computation over each node's already-known state.

```
elect(replica_set, self) -> NodeId:
    R = { r ∈ replica_set : r is healthy in self's local view }
    if R is empty: return None
    sorted = R sorted by (rtt_to(self, r), r.node_id_lex)  // primary RTT, tiebreak NodeId
    return sorted[0]                                        // self at RTT 0 wins ties of self vs others
```

Every healthy node computes the same winner from the same inputs (RTT matrix + membership set + NodeId ordering). No broadcast, no epoch, no collection window, no `PlacementFilter` consultation.

**Why this works for RedEX specifically.** RedEX is single-writer, append-only, and monotonic-seq. Replicas all converge to the same `tail_seq` under normal operation, so any healthy replica is a valid leader. The meaningful axis at election time is *centrality* (which surviving replica is closest to the rest), not *quality* (placement-quality was already settled at Phase C when these N nodes were chosen as the replica set). Folding placement scoring into election would re-litigate the placement question on every leader loss with no benefit.

**Why this is safe.** Centrality, not placement quality, is the right axis because:
- The replica set was already chosen with anti-affinity at Phase C; central nodes can't dominate replica membership across too many channels.
- Any healthy replica has the data to lead (single-writer + monotonic seq guarantees state-equivalence at convergence).
- A central node lowers RTT for the rest of the replica set, minimizing replication-sync round-trip in steady state.

**Why not extend `StandbyGroup::promote()` directly** (Option A in the audit): `StandbyGroup`'s default behavior promotes "the standby with the highest `synced_through`" (per `COMPUTE.md:268`) — a snapshot-currency axis correct for stateful daemons but wrong for an append-only log where all replicas converge. RedEX needs to override the *selection function* without replacing the membership / failure-detection / promotion-broadcast machinery. The pluggable hook is "given current healthy set, pick a leader" — RedEX supplies the nearest-RTT function above; `StandbyGroup` keeps its existing promotion-broadcast behavior to announce the result.

**Real-world precedent.** This pattern is exactly:
- Cassandra/Scylla: replica coordinator selection by network-topology proximity
- Consul: LAN-segment leader fallback
- Dynamo-inspired systems: preferred-replica selection

**Properties this gives us:**
- **Deterministic:** every node computes the same winner; no nondeterministic tie-break races.
- **No election storms:** no broadcast = nothing to thrash.
- **No cross-node disagreement within a partition:** same inputs → same output.
- **O(N) where N = replica-set size** (typically 3-5).
- **Zero new wire protocol:** the existing proximity graph, membership view, RTT matrix, and NodeId ordering are all the inputs needed.

**Cross-partition split-brain** is an unavoidable risk for any leader-elected system without quorum; mitigation is operator-controlled placement (see §7).

### Wire protocol

#### 4. Byte-level layouts pinned

§2 above defines:
- 3-byte subprotocol header (`subprotocol_id u16 LE` + `dispatch_code u8`) on every message
- Four dispatch codes: `SYNC_REQUEST` (0x20), `SYNC_RESPONSE` (0x21), `SYNC_HEARTBEAT` (0x22), `SYNC_NACK` (0x23). **No `LEADER_ELECTION` code** — election is wire-free per Decision 3.
- Reserved range `0x24..0x2F` for future variants
- All multi-byte integers are little-endian fixed-width (no varints)
- 32-byte BLAKE2s `ChannelId` (matches existing `ChannelName::hash()` convention)
- Length-prefixed strings: `(u16 LE len, [len] utf-8 bytes)`
- Range encoding: `(start_seq u64 LE, end_seq u64 LE)` half-open `[start, end)`
- Error path uses `SYNC_NACK` with typed `error_code u8`; silent close is reserved for transport-level failure only

Round-trip tests for each message land in Phase A.

### State machine

#### 5. 4-state machine pinned

`ReplicaState::{Leader, Replica, Candidate, Idle}` per §3. Transitions enumerated. The earlier draft inconsistently named 3 states in some places and 4 in others; the 4-state model is the single source of truth.

`Candidate` is now a brief transient state — the duration of the deterministic `elect()` function (microseconds), NOT a broadcast wait. Kept in the state machine for observability / DST modeling clarity (the "I detected leader loss, computing winner, transitioning" moment is a meaningful sequence point even if it's near-instant). DST scenarios (Phase F) model exactly these four states with the listed transitions.

### Operational decisions (recommendations from the prior open-questions list, ratified)

6. **Leader scope.** Same node as the channel's `ChannelPublisher` home by default (publisher is the natural leader for an append-only channel). Explicit override via `leader_pinned: Option<NodeId>` for split publisher/leader topologies. Pin in test.

7. **`UnderCapacity` default — `Withdraw`.** Replication factor is a hard guarantee on the leader; replicas are best-effort under disk pressure. Replicas under capacity withdraw; reads fall through to greedy LRU if enabled. `UnderCapacity::EvictOldest` available as opt-in.

8. **Cross-segment atomicity — none.** Per `REDEX_PLAN.md` non-goal #23. Replicas catch up segment-by-segment. Documented explicitly in `SUBPROTOCOLS.md`.

9. **Replica rejoin / partition healing.** Replay gap if `gap < skip_threshold` (default 100 MB); skip-ahead + flag for divergence audit if larger. Skip-ahead preserves safety because we're not rewriting history; we're choosing not to retain old history locally on the replica.

10. **Bandwidth budget — 50% of measured NIC peak by default.** Cap replication-sync I/O at `replication_budget_fraction × NIC peak`. Backpressure-aware via reliable-stream's existing flow control. NIC peak is measured via the existing per-link throughput counters surfaced by the proximity graph; sampling window is 60 s rolling.

11. **Leader concentration prevention — anti-affinity at 30% threshold, applied at *placement* time.** Configured via `AntiAffinityConfig::leadership_concentration_threshold` (default 0.30) + `AntiAffinityConfig::leadership_concentration_penalty` (default 0.4) — both from Capability §7. The penalty multiplies the candidate's score per the locked multiplicative composition rule, so a node already leading > 30% of channels has its *replica-set membership* score cut by 60% — making it less likely to be chosen as a replica in the first place. Election itself doesn't apply anti-affinity (it picks the most-central among already-chosen replicas); the concentration prevention is upstream at Phase C placement.

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

This phase consumes (hard prerequisites — see [Locked decisions §1, §2](#hard-prerequisites-must-ship-before-phase-c)):

- **Capability Phase B (tag-discovery primitives).** `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders` for replica advertisement, graceful retraction, and leader resolution. RedEX Phases C/D/E cannot start until this lands.
- **Capability Phase F (`PlacementFilter`).** `placement_score` for `Artifact::Replica`, `IntentRegistry`, `AntiAffinityConfig`. Consumed by RedEX's *replica placement* (Phase C) only — top-N replica selection at construction time. Election (Phase E) is `PlacementFilter`-free; uses only the proximity graph + NodeId ordering.
- **Capability Phase E (federated query primitives).** Used internally for operator-facing introspection ("which replicas hold this chain across the mesh"). Not on the critical path; useful for ops dashboards.

This phase produces:

- A `causal:` capability-tag emitter / withdrawer lifecycle that other Warriors phases consume (Phase 1 greedy reads from the same tag advertisements).
- A `MeshDaemon`-shaped daemon that demonstrates `PlacementFilter` consumption at placement time (template for future placement-driven daemons).
- A reference for "deterministic decentralised election composing on top of `StandbyGroup` membership/failure-detection" — the pluggable selection-fn pattern for RedEX-style append-only-log replication, distinct from the daemon-migration "highest synced_through" default.
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
