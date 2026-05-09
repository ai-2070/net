# RedEX Distributed — implementation plan

> Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md) (single-node v1) and [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) (single-node v2 — tiering, retention, indices). This doc plans the *cross-node replication* layer that v1's "Replication — not planned yet" and v2's "Cross-node RedEX is a later, explicit 'distributed RedEX' plan" both defer to. Ships in **The Warriors** release alongside the capability-taxonomy reorganization, federated query primitives, and the generalized `PlacementFilter`. Marketed in conversation as "RedEX V2" in the Warriors context, but to avoid namespace conflict with the existing single-node v2 plan, this doc uses **RedEX Distributed** throughout.

## Status

**Design locked; blocked on Capability Phases B + F.** All open design questions are ratified — see [Locked decisions](#locked-decisions). Hard prerequisites: Capability System Plan's Phase B (tag-discovery primitives `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders`) and Phase F (`PlacementFilter` + `IntentRegistry` + anti-affinity + `ProximityGraph::nearest_rtt`). RedEX Phases C/D/E cannot start until both land. Phases A, B (this plan's), G, H, I can proceed in isolation as scaffolding.

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

1. **Wire protocol** — `SUBPROTOCOL_REDEX` with `DISPATCH_REPLICA_SYNC` codes; full byte-level layouts pinned in [§2 Wire protocol](#2-wire-protocol--subprotocol_redex).
2. **`ReplicationConfig` in `ChannelConfig`** — opt-in per channel; defaults to `None` (single-node) for backward compatibility.
3. **`ReplicationCoordinator` daemon** — per replicated channel; spawned per replica; consumes `PlacementFilter` for placement decisions; runs the RedEX-local placement-scored election.
4. **Failover via RedEX-local placement-scored election** — `ReplicationCoordinator` performs the leader choice using `PlacementFilter::placement_score` for `Artifact::Replica`. Membership + failure detection delegate to `groups::standby`; the leader-choice itself is RedEX's, because `StandbyGroup::promote()` picks "most up-to-date snapshot" (per `COMPUTE.md:268`) which is the wrong axis for an append-only log replicated across topology-aware placement scores. See [Locked decision 3](#3-election-strategy--redex-local-placement-scored).
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

New subprotocol ID claim. Subprotocol header is the standard `subprotocol_id: u16 LE` + `dispatch_code: u8` = **3-byte prefix** on every message; payload follows. Reserves `DISPATCH_REPLICA_SYNC = 0x20..0x2F` (16 codes; v1 uses 5):

| Dispatch code | Direction | Purpose |
|---|---|---|
| `SYNC_REQUEST = 0x20` | replica → leader | Replica asks for events `[since_seq, since_seq + chunk_max)` |
| `SYNC_RESPONSE = 0x21` | leader → replica | Leader returns a bounded `read_range`-style stream |
| `SYNC_HEARTBEAT = 0x22` | bidirectional | Leader heartbeats `tail_seq`; replicas heartbeat their own `tail_seq` |
| `SYNC_NACK = 0x23` | leader → replica | Structured rejection (NotLeader / BadRange / Backpressure / ChannelClosed) |
| `LEADER_ELECTION = 0x24` | broadcast within replica set | Triggered by leader-loss detection; carries the RedEX-local placement score |

Reserved codes `0x25..0x2F` for future variants (range-bounded sync, parallel-stream sync, etc.). Document each new code in `SUBPROTOCOLS.md` as it lands.

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

LEADER_ELECTION (broadcast within replica set)
┌──────────────────────────────────────────────────────────────┐
│ subprotocol_id   u16 LE      = SUBPROTOCOL_REDEX             │
│ dispatch_code    u8          = 0x24                          │
│ channel_id       [u8; 32]                                    │
│ candidate_id     u64 LE      proposer's NodeId               │
│ proposed_score   f32 LE      candidate's PlacementFilter     │
│                              ::placement_score for itself    │
│                              as Artifact::Replica            │
│ epoch            u64 LE      monotonic election counter      │
│                              (prevents stale-vote replay)    │
└──────────────────────────────────────────────────────────────┘
fixed size: 3 + 32 + 8 + 4 + 8 = 55 bytes

Election protocol summary:
- On leader-loss detection, every surviving replica computes its own
  placement_score and broadcasts LEADER_ELECTION at epoch+1.
- After a 2 × heartbeat_ms collection window, the highest-scored
  candidate (with the locked tie-breaker from Capability §7) wins.
- All replicas update their local view to point to the winner.
- The winner's first heartbeat carries role=Leader; that doubles as
  the election commit signal.
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
    /// Election in flight. Coordinator has detected leader-loss
    /// (3 consecutive missed heartbeats) and is participating in
    /// the placement-scored election (LEADER_ELECTION broadcast +
    /// 2 × heartbeat_ms collection window). Refuses both append
    /// and serve until election commits.
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
- On leader-loss detection (3 consecutive missed heartbeats with hysteresis), enter `Candidate` and broadcast `LEADER_ELECTION`
- On graceful shutdown, transition to `Idle` and withdraw the replica's `causal:` capability tag via Capability Phase B's `Mesh::withdraw_chain`

**Construction:**

- When a channel with `ReplicationConfig` is opened, the local `Redex` manager checks `PlacementFilter::placement_score` to determine if this node should be a replica (top-N by score).
- If yes, spawn a `ReplicationCoordinator` daemon initialized in `Replica` state. The daemon registers itself with the channel's `StandbyGroup` (membership + failure detection only — see §7).
- If no, the channel opens as a regular non-replicated channel locally; reads route to the nearest replica via `Mesh::find_chain_holders`.

### 4. Replica election

N nodes from the capability-advertising set, selected via `PlacementFilter::placement_score` for `Artifact::Replica`. The top-N by score become replicas. The locked tie-breaking from Capability §7 (RTT → free-resource → lexicographic NodeId) applies on equal scores.

**Triggers:**

- First publish on a newly-replicated channel
- Roster change (replica withdraws / new node advertises matching capabilities)
- Leader-loss detection by any surviving replica

**Election mechanism (RedEX-local, NOT delegated to `StandbyGroup::promote`).** See [Locked decision 3](#3-election-strategy--redex-local-placement-scored) for the rationale. Concretely:

1. On leader-loss detection, every surviving `Replica` transitions to `Candidate` and broadcasts `LEADER_ELECTION` carrying its own `placement_score` for itself as `Artifact::Replica`, plus a monotonic `epoch` (last-seen-epoch + 1).
2. Each candidate collects peers' broadcasts for `2 × heartbeat_ms` (the election window).
3. The candidate with the highest score at the highest epoch wins. Tie-break per Capability §7. All candidates compute the winner deterministically from the same broadcast set (no separate "vote" round needed — the score IS the vote).
4. The winner transitions `Candidate → Leader`; its first `SYNC_HEARTBEAT` with `role=Leader` is the commit signal. Losers transition `Candidate → Replica` and follow the winner.
5. If the election window expires with no quorum (e.g. partition splits the replica set evenly), candidates re-broadcast at `epoch + 1`; the higher-epoch broadcast supersedes lower-epoch state on every replica. Convergence is bounded by partition healing.

**`StandbyGroup` provides membership + failure detection, not leader choice.** The standby-group machinery already tracks "which replicas are alive" and surfaces failure events. RedEX consumes those events as election triggers but performs the leader choice itself via the placement-scored protocol above.

**Anti-affinity (default):** the placement score includes a penalty for nodes already leading > 30% of channels in the local view, to spread leadership naturally across nodes. Configurable via the `AntiAffinityConfig` from Capability §7.

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

Leader fails (proximity graph reports `Unhealthy` OR 3 consecutive heartbeats missed by all surviving replicas):

1. Each surviving replica transitions `Replica → Candidate` and broadcasts `LEADER_ELECTION` per [§4](#4-replica-election).
2. After the `2 × heartbeat_ms` collection window, the highest-scored candidate transitions `Candidate → Leader`. The first `SYNC_HEARTBEAT` from the new leader is the commit signal.
3. The new leader resumes appends from its local `tail_seq`; the old leader's gap (if any) is replayed when it rejoins via the standard rejoin path (§8).
4. The new leader emits `causal:` capability-tag updates via `Mesh::announce_chain`; the channel's routing tag now points to the new leader. The old leader's `causal:` tag is reaped by the standard withdrawal path on its next observed disconnect.
5. Cutover is atomic from the routing layer's perspective — the next publish on the channel goes to the new leader; in-flight reads against the old leader fall through to the new one via `Mesh::find_chain_holders` re-lookup.

**Critical safety property:** no two nodes can be leader for the same channel at the same time.

The election protocol guarantees this in three ways:
- The score-and-epoch broadcast is deterministic — every candidate sees the same broadcast set and computes the same winner.
- A higher-epoch `LEADER_ELECTION` always supersedes a lower-epoch one on every replica, so a stale leader from a partition-healed split is demoted as soon as it observes the higher epoch.
- The reliable-stream layer guarantees in-order delivery of `SYNC_HEARTBEAT` within a session, so a node can never observe the new leader's `role=Leader` heartbeat before its own `Candidate → Replica` transition.

Pin all three in DST scenarios (Phase F).

**Membership + failure detection delegate to `StandbyGroup`.** RedEX uses standby-group machinery to learn "this replica is alive" and "this replica just disconnected"; it does NOT use `StandbyGroup::promote()` because that primitive picks "most up-to-date snapshot" (per `COMPUTE.md:268`) which is the wrong axis for an append-only log replicated across topology-aware placement scores. See [Locked decision 3](#3-election-strategy--redex-local-placement-scored).

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

Implementable in isolation; does not depend on Capability B/F.

- Add `SUBPROTOCOL_REDEX` ID claim in `SUBPROTOCOLS.md` and `behavior::subprotocol`.
- Add the five `DISPATCH_REPLICA_SYNC` codes (0x20..0x24) per [§2](#2-wire-protocol--subprotocol_redex). Encode/decode each message at the byte layouts pinned there.
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

### Phase E — Failover via RedEX-local placement-scored election (1 week)

**Hard prerequisite: Capability Phase F (`PlacementFilter`).**

- `ReplicationCoordinator` consumes `StandbyGroup` events for membership + failure detection only. Does NOT call `StandbyGroup::promote()`. See [Locked decision 3](#3-election-strategy--redex-local-placement-scored).
- `LEADER_ELECTION` broadcast on leader-loss; 2 × heartbeat_ms collection window; deterministic winner from highest-(score, epoch) pair with the locked tie-breaker (RTT → free-resource → lexicographic NodeId).
- Hysteresis: 3-missed-heartbeats threshold; tunable.
- Anti-affinity at 30% leadership-concentration threshold via `AntiAffinityConfig` (Capability §7); penalty multiplies into the placement score per the locked multiplicative composition rule.
- Capability-tag updates on leader change via `Mesh::announce_chain`; routing follows. Old leader's tag is reaped on the next observed disconnect.
- Pin the no-two-leaders invariant in unit tests AND DST (Phase F).

### Phase F — DST harness extension (1.5–3 weeks; widest variance)

**Hard prerequisite: Phases C, D, E of this plan + Capability F.**

This is the gating phase. Plan generously and treat regressions as test failures:

- Extend `loom_models.rs` to model the 4-state replication state machine (`Leader` / `Replica` / `Candidate` / `Idle`) with the locked transitions from [§3](#3-replicationcoordinator).
- Failure-injection scenarios: random partition, leader crash, replica crash, partial-network (one replica isolated), restart-during-sync, election-window-collision (two simultaneous candidates), partition-heal-with-stale-leader.
- Convergence assertion: all surviving replicas converge to leader's `tail_seq` after recovery.
- Divergence-freedom: no two replicas declare different `tail_seq` for the same `seq` (stronger than convergence — pin in DST).
- Election-correctness: at any point, exactly one node believes it is leader for a channel, OR an election is in flight (no dual-leader windows). Pin against the partition-heal-with-stale-leader scenario specifically — the higher-epoch supersession rule from §4 is the load-bearing invariant.
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

#### 2. Capability Phase F (`PlacementFilter`) is a hard prerequisite

RedEX leader concentration prevention, replica placement, fallback placement, and the placement-scored election ALL consume:

- `PlacementFilter::placement_score(target, &Artifact::Replica { ... })`
- `IntentRegistry` (consumed via `metadata.intent`)
- `AntiAffinityConfig` (the leadership-concentration penalty)
- `ProximityGraph::nearest_rtt(candidate, predicate)` (locked in Capability §7a)

None of these exist in code today. They ship in **Capability System Plan Phase F**. RedEX Phases C and E **cannot start** until Phase F lands. The composite gate is therefore "Capability B AND F" before RedEX C/D/E.

### Election strategy

#### 3. Election strategy — RedEX-local placement-scored

RedEX runs its own placement-scored election; `StandbyGroup` provides membership + failure detection only.

**Why not extend `StandbyGroup::promote()`** (Option A in the audit): `StandbyGroup` promotes "the standby with the highest `synced_through`" (per `COMPUTE.md:268`) — a snapshot-currency axis that's correct for stateful daemons but **wrong for an append-only log replicated across topology-aware placement scores**. Replicas all converge to the same `tail_seq` under normal operation; the meaningful axis at election time is "which replica is best-placed for the channel," and that's exactly what `PlacementFilter::placement_score` already answers.

**Why RedEX-local is clean:**
- `StandbyGroup` keeps its existing promotion semantics for daemon migration; nothing changes there.
- RedEX's election doesn't need a new primitive — it composes `PlacementFilter` (Capability F) + `LEADER_ELECTION` broadcast (this plan's wire protocol) + the score-and-epoch convergence rule (§4 of this plan).
- Separation of concerns: the standby-group machinery handles "is this replica alive," the placement filter handles "is this replica well-placed," and the RedEX coordinator composes both.

**Mechanism**: §4 above; `LEADER_ELECTION` byte layout in §2.

### Wire protocol

#### 4. Byte-level layouts pinned

§2 above defines:
- 3-byte subprotocol header (`subprotocol_id u16 LE` + `dispatch_code u8`) on every message
- Five dispatch codes: `SYNC_REQUEST` (0x20), `SYNC_RESPONSE` (0x21), `SYNC_HEARTBEAT` (0x22), `SYNC_NACK` (0x23), `LEADER_ELECTION` (0x24)
- Reserved range `0x25..0x2F` for future variants
- All multi-byte integers are little-endian fixed-width (no varints)
- 32-byte BLAKE2s `ChannelId` (matches existing `ChannelName::hash()` convention)
- Length-prefixed strings: `(u16 LE len, [len] utf-8 bytes)`
- Range encoding: `(start_seq u64 LE, end_seq u64 LE)` half-open `[start, end)`
- Error path uses `SYNC_NACK` with typed `error_code u8`; silent close is reserved for transport-level failure only

Round-trip tests for each message land in Phase A.

### State machine

#### 5. 4-state machine pinned

`ReplicaState::{Leader, Replica, Candidate, Idle}` per §3. Transitions enumerated. The earlier draft inconsistently named 3 states in some places and 4 in others; the 4-state model is the single source of truth. DST scenarios (Phase F) model exactly these four states with the listed transitions.

### Operational decisions (recommendations from the prior open-questions list, ratified)

6. **Leader scope.** Same node as the channel's `ChannelPublisher` home by default (publisher is the natural leader for an append-only channel). Explicit override via `leader_pinned: Option<NodeId>` for split publisher/leader topologies. Pin in test.

7. **`UnderCapacity` default — `Withdraw`.** Replication factor is a hard guarantee on the leader; replicas are best-effort under disk pressure. Replicas under capacity withdraw; reads fall through to greedy LRU if enabled. `UnderCapacity::EvictOldest` available as opt-in.

8. **Cross-segment atomicity — none.** Per `REDEX_PLAN.md` non-goal #23. Replicas catch up segment-by-segment. Documented explicitly in `SUBPROTOCOLS.md`.

9. **Replica rejoin / partition healing.** Replay gap if `gap < skip_threshold` (default 100 MB); skip-ahead + flag for divergence audit if larger. Skip-ahead preserves safety because we're not rewriting history; we're choosing not to retain old history locally on the replica.

10. **Bandwidth budget — 50% of measured NIC peak by default.** Cap replication-sync I/O at `replication_budget_fraction × NIC peak`. Backpressure-aware via reliable-stream's existing flow control. NIC peak is measured via the existing per-link throughput counters surfaced by the proximity graph; sampling window is 60 s rolling.

11. **Leader concentration prevention — anti-affinity at 30% threshold.** Configured via `AntiAffinityConfig::leadership_concentration_threshold` (default 0.30) + `AntiAffinityConfig::leadership_concentration_penalty` (default 0.4) — both from Capability §7. The penalty multiplies the candidate's score per the locked multiplicative composition rule, so a node already leading > 30% of channels gets its election score cut by 60%.

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
- **Capability Phase F (`PlacementFilter`).** `placement_score` for `Artifact::Replica`, `IntentRegistry`, `AntiAffinityConfig`, `ProximityGraph::nearest_rtt`. Consumed by RedEX's placement-scored election, top-N replica selection, and leader-concentration anti-affinity. RedEX Phases C and E cannot start until this lands.
- **Capability Phase E (federated query primitives).** Used internally for operator-facing introspection ("which replicas hold this chain across the mesh"). Not on the critical path; useful for ops dashboards.

This phase produces:

- A `causal:` capability-tag emitter / withdrawer lifecycle that other Warriors phases consume (Phase 1 greedy reads from the same tag advertisements).
- A `MeshDaemon`-shaped daemon that demonstrates `PlacementFilter` consumption (template for future placement-driven daemons).
- A reference for "RedEX-local election composing on top of `StandbyGroup` membership/failure-detection," documenting the Option-B separation-of-concerns that `groups::standby` doesn't subsume.
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
