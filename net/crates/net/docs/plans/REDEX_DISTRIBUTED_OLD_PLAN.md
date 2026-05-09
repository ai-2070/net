# RedEX Distributed — implementation plan

> Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md) (single-node v1) and [`REDEX_V2_PLAN.md`](REDEX_V2_PLAN.md) (single-node v2 — tiering, retention, indices). This doc plans the *cross-node replication* layer that v1's "Replication — not planned yet" and v2's "Cross-node RedEX is a later, explicit 'distributed RedEX' plan" both defer to. Ships in **The Warriors** release alongside the capability-taxonomy reorganization, federated query primitives, and the generalized `PlacementFilter`. Marketed in conversation as "RedEX V2" in the Warriors context, but to avoid namespace conflict with the existing single-node v2 plan, this doc uses **RedEX Distributed** throughout.

## Status

**Design locked; blocked on Capability Phases B + F.** All open design questions are ratified — see [Locked decisions](#locked-decisions). Hard prerequisites: Capability System Plan's Phase B (tag-discovery primitives `Mesh::announce_chain` / `withdraw_chain` / `find_chain_holders`) and Phase F (`PlacementFilter` + `IntentRegistry` + anti-affinity + `ProximityGraph::nearest_rtt`). RedEX Phases C/D/E cannot start until both land. Phases A, B (this plan's), G, H, I can proceed in isolation as scaffolding.

Activation gate (when Warriors as a whole ships) is unchanged: a workload requesting durability guarantees beyond single-node. Realistic triggers: payment-tier customer, compliance-bound data class, pilot whose RTO is "< 5 s on node failure."

## Frame

RedEX Distributed adds **orchestrated replication** to a v1/v2 RedEX channel. N replicas of the channel's log are maintained explicitly; configurable replication factor; pull-based catch-up on divergence; documented conflict policy (none expected because RedEX is append-only and seq-ordered, but the protocol must say so explicitly). Strong durability guarantee, in contrast to greedy LRU's probabilistic one.

**Leadership is monarchic, not elected.** Within the replica set, leadership is a pure function of proximity graph + node IDs — the lowest-ranked surviving member is leader, deterministically, with no election round-trip. Append-only logs don't need consensus on the writer; they need ordered append, which a single deterministic writer provides. Quorum's value is reconciling concurrent writes from arbitrary nodes; that's not the RedEX model. See [Locked decision 3](#3-leadership-strategy--monarchic-deterministic-ranking).

**Reuses existing primitives:** `ChannelPublisher`, `SubscriberRoster`, the causal-chain machinery, `StandbyGroup` (membership + failure detection only), the proximity graph. Adds one new wire subprotocol (`SUBPROTOCOL_REDEX`), one new daemon (`ReplicationCoordinator`), and consumes the `PlacementFilter` primitive shipped in the same Warriors release for replica-set selection.

## Why this exists

Three load-bearing reasons:

1. **Single-node RedEX is not enough for any workload that loses business on node failure.** Greedy LRU offers probabilistic durability — popular data ends up replicated, unpopular data lives only at origin. For payments, compliance-bound data, and other "this must survive node failure" workloads, that's the wrong shape.
2. **The substrate already has all the primitives except the coordinator.** Standby groups handle daemon-state failover; ChannelPublisher carries reliable streams; capability tags carry replica advertisement; the proximity graph weights placement. The work is *composing* these into a replication coordinator, not building distributed-systems infrastructure from scratch.
3. **DST is the gating concern, not LoC.** Without a deterministic-simulation-test harness covering partition, failover, and rejoin sequences, replication ships with subtle bugs that surface only in production. Allocate ~30% of phase effort to DST harness work; treat it as a precondition, not an afterthought.

## What ships

Five things, in dependency order:

1. **Wire protocol** — `SUBPROTOCOL_REDEX` with `DISPATCH_REPLICA_SYNC` codes; full byte-level layouts pinned in [§2 Wire protocol](#2-wire-protocol--subprotocol_redex). No election message — leadership is deterministic from primitives every node already shares.
2. **`ReplicationConfig` in `ChannelConfig`** — opt-in per channel; defaults to `None` (single-node) for backward compatibility.
3. **`ReplicationCoordinator` daemon** — per replicated channel; spawned per replica; consumes `PlacementFilter` for replica-set selection; computes leadership locally via the deterministic rank function from §4.
4. **Failover via deterministic leadership ranking** — when the current leader's heartbeat goes silent, every surviving replica independently re-runs the rank function over the surviving set; the new leader is the result. No election protocol; no vote round. Membership + failure detection delegate to `groups::standby`; the leadership choice itself is RedEX-local and parameterless (no scoring at election time — the rank function is constant w.r.t. log state). See [Locked decision 3](#3-leadership-strategy--monarchic-deterministic-ranking).
5. **DST harness extension** — adds replication scenarios (partition, leader flap, replica rejoin, partition-heal-with-stale-monarch) to the existing `loom_models.rs` infrastructure.

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
    /// When `Some(n)`, the rank function returns `n` if `n` is in the surviving
    /// set, otherwise falls back to the standard `min(node_id)` rule. Useful
    /// for split publisher/leader topologies and tests.
    /// `None` = leadership is the surviving set's `min(node_id)` per §4.
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
| `SYNC_HEARTBEAT = 0x22` | bidirectional | Leader heartbeats `tail_seq`; replicas heartbeat their own `tail_seq`. Carries the leader's view of the replica-set roster epoch — the only signal needed for monarchic succession (see §4). |
| `SYNC_NACK = 0x23` | leader → replica | Structured rejection (NotLeader / BadRange / Backpressure / ChannelClosed) |

Reserved codes `0x24..0x2F` for future variants (range-bounded sync, parallel-stream sync, etc.). **No election message — succession is deterministic from the rank function in §4 and does not require negotiation.** Document each new code in `SUBPROTOCOLS.md` as it lands.

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
│ role             u8          0 Leader, 1 Replica, 2 Idle     │
│ roster_epoch     u64 LE      sender's view of the replica-   │
│                              set membership version          │
│                              (incremented when join/leave    │
│                              changes the set; used by §4     │
│                              succession to detect stale      │
│                              monarchs from healed splits)    │
│ wall_clock_ms    u64 LE      sender's monotonic-clock ms     │
│                              (drift detection only — not     │
│                              used for ordering)              │
└──────────────────────────────────────────────────────────────┘
fixed size: 3 + 32 + 8 + 1 + 8 + 8 = 60 bytes

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

Succession protocol summary (no wire message — purely local computation per §4):
- Every replica continuously evaluates the rank function over its current
  view of the surviving replica set (proximity-eligibility filter + node_id
  rank).
- On leader-loss detection (3 missed heartbeats from the current leader),
  the replica re-runs the rank function over { current set } \ { dead leader }.
- The result is the new leader. No broadcast, no vote, no collection window.
- The new leader's first SYNC_HEARTBEAT with role=Leader is the externally-
  observable commit signal; it also carries the incremented roster_epoch
  reflecting the membership change.
- A node observing a heartbeat from a Leader whose node_id is HIGHER than
  its own current rank-function answer treats it as a stale monarch (typical
  cause: healed network partition). It demotes the stale monarch in its
  local view; the stale monarch demotes itself on observing a peer with
  lower node_id active in the set.
```

**Range encoding** (used by future `0x25..0x2F` variants and by `read_range` parameters within RedEX itself): `(start_seq: u64 LE, end_seq: u64 LE)` pairs. Half-open `[start, end)` per the existing `RedexFile::read_range` convention. No varints; explicit u64 endpoints.

**All `SUBPROTOCOL_REDEX` messages ride on the existing reliable-stream `Mesh::publish` machinery.** No new transport. Backpressure-aware via reliable-stream's flow control.

### 3. `ReplicationCoordinator`

One daemon per replicated channel per replica. Reuses the existing `MeshDaemon` trait (per `REDEX_PLAN.md`'s daemon model).

**State machine (LOCKED — 3 states).**

```rust
pub enum ReplicaState {
    /// Sole appender for the channel. Determined by being the lowest-
    /// node_id member of the surviving replica set per §4. Heartbeats
    /// to all replicas at `heartbeat_ms`; serves SYNC_REQUEST; rejects
    /// any local append from non-self with SYNC_NACK::NotLeader.
    Leader,
    /// Catching up via SYNC_REQUEST or steady-state lagging by ≤ 1
    /// heartbeat. Heartbeats own `tail_seq` to the leader.
    Replica,
    /// Node carries the channel's storage but has no replica role
    /// for it (e.g. former replica that withdrew under disk
    /// pressure, or a node that holds the chain via greedy LRU
    /// without being part of the explicit replication set). Does
    /// NOT heartbeat; does NOT participate in succession. Reads
    /// fall through to the leader.
    Idle,
}
```

There is **no `Candidate` state** because there is no election. Succession from `Replica → Leader` is instantaneous and local: when the rank function over the surviving set names this node as the new leader, the coordinator transitions and emits its first `role=Leader` heartbeat as the commit signal. Every other replica makes the same transition independently — they all compute the same rank function over the same set.

Transitions:
- `Idle → Replica` on join (replica-set selection chose this node)
- `Replica → Leader` on rank-function naming this node as the new leader (current leader missed 3 consecutive heartbeats; this node is the lowest-node_id member of the surviving set)
- `Leader → Replica` on observing a peer with lower node_id active in the set (stale-monarch self-demotion after partition heal)
- `Leader → Idle` on graceful relinquish (admin command, `leader_pinned` override migrates)
- `Replica → Idle` on disk-pressure withdrawal under `UnderCapacity::Withdraw`
- Any → `Idle` on channel close

**The 3-state model above is the single source of truth.** DST scenarios in Phase F model exactly these three states and the listed transitions.

**Responsibilities:**

- Hold the replica's `tail_seq` for this channel
- Heartbeat to the leader at `heartbeat_ms` cadence (when in `Replica`)
- Serve `SYNC_REQUEST` and emit `SYNC_HEARTBEAT` (when in `Leader`)
- On heartbeat-ack mismatch, issue `SYNC_REQUEST` to leader; apply the `SYNC_RESPONSE` events in seq order
- On leader-loss detection (3 consecutive missed heartbeats with hysteresis), re-run the §4 rank function over the surviving set; transition `Replica → Leader` if this node wins; otherwise stay `Replica` and re-resolve the new leader via `Mesh::find_chain_holders` filtered to the lowest node_id
- On observing an active peer with lower node_id while in `Leader`, transition `Leader → Replica` (stale-monarch self-demotion)
- On observing a leadership transition (own `min(node_id)` view changed, or saw a `role=Leader` heartbeat from a peer other than the current believed leader), emit `Mesh::withdraw_chain(channel_id, by_node = previous_leader)` as a witness — see §4 "Witness withdrawal" for rationale. Idempotent across the N witnessing replicas; the capability layer dedups.
- On graceful shutdown, transition to `Idle` and withdraw the replica's `causal:` capability tag via Capability Phase B's `Mesh::withdraw_chain`

**Construction:**

- When a channel with `ReplicationConfig` is opened, the local `Redex` manager checks `PlacementFilter::placement_score` to determine if this node should be a member of the replica set (top-N by score).
- If yes, spawn a `ReplicationCoordinator` daemon initialized in `Replica` state. The daemon registers itself with the channel's `StandbyGroup` (membership + failure detection only — see §7). It then evaluates the §4 rank function; if it names itself the leader (lowest node_id in the surviving set), it transitions `Replica → Leader` immediately.
- If no, the channel opens as a regular non-replicated channel locally; reads route to the nearest replica via `Mesh::find_chain_holders`.

### 4. Leadership ranking — monarchic succession

The replica set has N members, selected via `PlacementFilter::placement_score` for `Artifact::Replica` (top-N by score, tie-break per Capability §7). **Within the replica set, leadership is a pure function of node_id**: the surviving member with the lowest `NodeId` is leader. There is no election protocol.

```rust
fn leader_of(surviving_set: &BTreeSet<NodeId>) -> Option<NodeId> {
    surviving_set.iter().min().copied()
}
```

That's the entire mechanism. Every replica computes the same answer from the same surviving set; succession is whatever the function returns when the set changes.

**Why this works for append-only logs.** The reason quorum buys consensus is reconciling concurrent writes from arbitrary writers. RedEX is single-writer by design — only the leader appends. The election protocols quorum systems run (Raft, Paxos, ZAB) exist to choose a single writer; we choose the writer deterministically from agreed-upon primitives instead, and skip the protocol. Append-only + monotonic seq + deterministic-leader = no consensus round needed.

**Why proximity-as-eligibility, not proximity-as-rank.** The `PlacementFilter` already selects the replica set via proximity-aware scoring. Once the set is fixed, the rank inside the set must be a value that every member agrees on without negotiation. Node_ids are globally agreed; proximity views are not (different nodes' proximity tables can diverge during churn). So proximity gates *who's in the set*, and node_id orders *within the set*. This is the standard split-brain-mitigation discipline of "make the rank function depend only on globally-agreed primitives."

**Triggers for leadership change:**

- First publish on a newly-replicated channel — initial `leader_of(replica_set)` runs.
- Replica-set membership change (a replica withdraws, a new node joins) — every member re-runs `leader_of` over the new set; the answer changes only if the lowest node_id changed.
- Leader heartbeat silent for 3 consecutive intervals — every surviving member re-runs `leader_of` over { current set } \ { dead leader }.

**Membership + failure detection delegate to `StandbyGroup`.** The standby-group machinery tracks which replicas are alive and surfaces join/leave events. RedEX consumes those events to keep its surviving-set view current, then re-runs `leader_of` whenever the set changes. **`StandbyGroup::promote()` is NOT used** — its "most-up-to-date snapshot" axis is the wrong thing for an append-only log where all replicas converge to the same tail (per [Locked decision 3](#3-leadership-strategy--monarchic-deterministic-ranking)).

**Anti-concentration of leadership.** Because rank within a set is by node_id, channels whose replica sets all happen to contain a low-node_id node will all elect that node as leader. Mitigation: the `PlacementFilter` anti-affinity term *already* penalizes nodes leading > 30% of channels in the local view when scoring replica-set membership. A heavily-leading node falls out of the eligible top-N for new channels — it stops being *in* most replica sets, so it stops being *leader* of most replica sets. Concentration prevention happens at set selection, not at succession. Configurable via `AntiAffinityConfig` from Capability §7.

**Witness withdrawal (the herald pattern).** The new leader announces itself via `Mesh::announce_chain` — that's the squire riding to the towns. But the deposed leader's stale `causal:` tag would normally only get reaped when the network observes its disconnect, which can lag (especially in the partition-heal case where the deposed monarch is still nominally alive on its own side). Faster reaping comes from the **witnesses** — every peer replica that observes the leadership transition also issues `Mesh::withdraw_chain(channel_id, by_node = previous_leader)`. The withdraw is idempotent across the N witnessing peers; the capability layer dedups. This is free robustness: the peers were already going to observe the transition (they're computing `leader_of` in lockstep); we're just letting them speak the fact that the king is dead instead of waiting for the disconnect-observer subsystem to catch up. Pin in DST under the partition-heal scenario.

**Universal failure modes (not unique to monarchy).** Both split-brain (two partitions, each with its own apparent leader) and membership-thrash (nodes flickering in/out cause leader-rank churn) exist in *every* leader-driven replication scheme — Raft, Paxos, primary-backup, push-pull, all of them. Quorum spends bytes on protocol; monarchy doesn't. The mitigations are the same shape in either case:

- **Split-brain on partition heal.** When two partitions reconcile, two nodes may briefly both believe they are leader. Mitigation: the heartbeat carries `roster_epoch`; on observing a peer in the surviving set with lower node_id active, the higher-id "stale monarch" demotes itself to `Replica`. The catch-up path then pulls the newer leader's tail. Append-only seq monotonicity means the surviving leader's log is always a strict superset (or equal prefix) of any partition-isolated leader's log — the stale monarch's writes during the partition were never acknowledged by quorum (there is no quorum) but neither were they advertised to writers (writers route via `find_chain_holders`, which during partition resolves locally; on heal, the lower-id leader becomes the canonical answer). Pin in DST.
- **Membership thrash → leader thrash.** If a low-node_id replica flickers in/out of liveness, leadership flips with it. Mitigation: 3-missed-heartbeats hysteresis on the failure-detection side (already in §6); the rank function only re-runs when liveness state actually transitions, not on transient packet loss.

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
  replica re-runs leader_of(surviving_set \ dead_leader)
  if result == self.node_id:
    transition Replica -> Leader; first heartbeat carries role=Leader
  else:
    re-resolve new leader via Mesh::find_chain_holders, stay Replica
```

**Hysteresis:** 3-missed-heartbeats threshold prevents leadership thrash under transient packet loss. Tunable per channel for tighter SLAs.

### 7. Failover

Leader fails (proximity graph reports `Unhealthy` OR 3 consecutive heartbeats missed by all surviving replicas):

1. Each surviving replica re-runs `leader_of(surviving_set \ dead_leader)`. The lowest-node_id member transitions `Replica → Leader`; everyone else stays `Replica`.
2. The new leader's first `SYNC_HEARTBEAT` with `role=Leader` and incremented `roster_epoch` is the commit signal.
3. The new leader resumes appends from its local `tail_seq`; the old leader's gap (if any) is replayed when it rejoins via the standard rejoin path (§8).
4. The new leader emits `causal:` capability-tag updates via `Mesh::announce_chain`; the channel's routing tag now points to the new leader. The old leader's `causal:` tag is reaped by **two paths in parallel**: (a) the standard withdrawal on observed disconnect, and (b) **witness withdrawals** — every peer replica that observes the transition issues `Mesh::withdraw_chain(channel_id, by_node = previous_leader)`. The peers saw the king die; they speak the fact. Idempotent across N witnesses; the capability layer dedups. Witness withdrawal closes the partition-heal stale-tag window faster than waiting on disconnect observation alone.
5. Cutover is atomic from the routing layer's perspective — the next publish on the channel goes to the new leader; in-flight reads against the old leader fall through to the new one via `Mesh::find_chain_holders` re-lookup.

**Critical safety property:** no two nodes are *acknowledged* leader for the same channel at the same time within any single observer's view.

Monarchy guarantees this within an observer in two ways:
- The rank function `min(node_id)` is deterministic; every replica computes the same winner from the same surviving set.
- A node observing a heartbeat from a `role=Leader` peer with a higher node_id than its own current `leader_of` answer treats the peer as a stale monarch and ignores its writes (the peer self-demotes on its next observation of the lower-id active leader). After partition heal, the lower-id node is canonical; the higher-id node's partition-local writes are not advertised by `find_chain_holders` because the lower-id node now answers that query for the channel.

Across observers during partition, two leaders can transiently exist — same as in any leader-driven scheme without quorum, and same as what quorum systems experience during split-brain when the quorum is unavailable on one side. The monotonic-seq + single-writer-per-partition discipline means partition-local writes are *append-only* and survive heal as a tail-divergence to be reconciled by replay (§8); they never corrupt the canonical leader's log because they were never written to it.

Pin both in-observer and across-observer scenarios in DST (Phase F), specifically including the partition-heal-with-stale-monarch sequence.

**Membership + failure detection delegate to `StandbyGroup`.** RedEX uses standby-group machinery to learn "this replica is alive" and "this replica just disconnected"; it does NOT use `StandbyGroup::promote()` because that primitive picks "most up-to-date snapshot" (per `COMPUTE.md:268`) which is the wrong axis for an append-only log where all replicas converge to the same tail. See [Locked decision 3](#3-leadership-strategy--monarchic-deterministic-ranking).

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
| `dataforts_leader_changes_total{channel}` | Counter | Number of monarchic successions (leadership transitions driven by `min(node_id)` over a changed surviving set) |
| `dataforts_replication_under_capacity_total{channel}` | Counter | Times the channel hit `UnderCapacity` policy |
| `dataforts_replication_skip_ahead_total{channel}` | Counter | Times a replica skipped instead of replaying a large gap |
| `dataforts_replication_leader_thrash_total{channel}` | Counter | Successions triggered within 30 s of the previous one (saturation indicator — typically caused by a low-node_id replica flickering in/out of liveness) |
| `dataforts_replication_stale_monarch_demotions_total{channel}` | Counter | Times a node self-demoted from `Leader → Replica` after observing a lower-node_id active peer (partition-heal indicator) |
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
- Heartbeat loop on `heartbeat_ms`; 3-state machine `ReplicaState::{Leader, Replica, Idle}` per [§3](#3-replicationcoordinator). All transitions exhaustive; pin in unit tests.
- Capability tag emission on `Idle → Replica` and `Replica → Leader`; withdrawal on `* → Idle`. Uses Capability Phase B's `Mesh::announce_chain` / `Mesh::withdraw_chain`.

### Phase D — Pull-based catch-up (1 week)

**Hard prerequisite: Capability Phase B + Phase C of this plan.**

- `SYNC_REQUEST` issuance on heartbeat-ack lag.
- `SYNC_RESPONSE` generation via existing `RedexFile::read_range`.
- `SYNC_NACK` emission on `NotLeader` / `BadRange` / `Backpressure` / `ChannelClosed` per the typed error path in [§2](#2-wire-protocol--subprotocol_redex). Replicas implement the matching retry policy keyed on `error_code`.
- Replica-side `append_batch` application; preserve seq invariants.
- Skip-ahead path for gaps > `skip_threshold`.
- Bandwidth-budget enforcer respecting `replication_budget_fraction`.

### Phase E — Failover via deterministic leadership ranking (3 days)

**Hard prerequisite: Capability Phase F (`PlacementFilter`) for replica-set selection.** No prerequisite for the rank function itself — it's `min(node_id)` over the surviving set and depends only on primitives already shipped.

- `ReplicationCoordinator` consumes `StandbyGroup` events for membership + failure detection only. Does NOT call `StandbyGroup::promote()`. See [Locked decision 3](#3-leadership-strategy--monarchic-deterministic-ranking).
- `leader_of(surviving_set)` invocation on (a) replica-set construction, (b) any `StandbyGroup` membership-change event, (c) 3-missed-heartbeats from the current leader.
- Stale-monarch self-demotion: `Leader → Replica` on observing an active peer in the surviving set with lower node_id. Tested explicitly with the partition-heal-with-stale-monarch DST scenario.
- Hysteresis: 3-missed-heartbeats threshold; tunable.
- Anti-affinity at 30% leadership-concentration threshold via `AntiAffinityConfig` (Capability §7); penalty multiplies into the *replica-set selection* score per the locked multiplicative composition rule. Concentration prevention is structural (heavily-leading nodes drop out of replica sets), not procedural (no penalty applied during succession itself — succession is parameterless).
- Capability-tag updates on leader change via `Mesh::announce_chain` (new leader self-announces); routing follows.
- Witness withdrawal: every peer replica that observes a leadership transition issues `Mesh::withdraw_chain(channel_id, by_node = previous_leader)` in parallel with its own state transition. Idempotent across N witnesses. Closes the stale-tag window in the partition-heal scenario faster than disconnect-observation reaping.
- Pin the no-two-leaders-in-an-observer invariant and the partition-heal stale-monarch demotion path in unit tests AND DST (Phase F).

### Phase F — DST harness extension (1.5–3 weeks; widest variance)

**Hard prerequisite: Phases C, D, E of this plan + Capability F.**

This is the gating phase. Plan generously and treat regressions as test failures:

- Extend `loom_models.rs` to model the 3-state replication state machine (`Leader` / `Replica` / `Idle`) with the locked transitions from [§3](#3-replicationcoordinator).
- Failure-injection scenarios: random partition, leader crash, replica crash, partial-network (one replica isolated), restart-during-sync, partition-heal-with-stale-monarch (the load-bearing scenario for monarchic succession), simultaneous double-leader-loss (back-to-back failures of the lowest-node_id and second-lowest in rapid succession).
- Convergence assertion: all surviving replicas converge to leader's `tail_seq` after recovery.
- Divergence-freedom: no two replicas declare different `tail_seq` for the same `seq` *within a single observer's view* (stronger than convergence — pin in DST). Across observers during partition, divergent tails ARE permitted; reconciliation on heal is via §8 replay.
- Succession-correctness: at any point, every observer's local view names a single leader for a channel (the `min(node_id)` of its surviving set). After partition heal, all observers' views converge on the lowest-node_id leader within bounded time. Pin the partition-heal-with-stale-monarch scenario specifically — the stale-monarch self-demotion rule from §4 is the load-bearing invariant.
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
- `ReplicationCoordinator` state-machine transitions (Idle → Replica → Leader on rank-function win; Leader → Replica on stale-monarch demotion; etc.).
- Skip-threshold logic boundaries (gap exactly at threshold, gap > threshold).
- Anti-affinity scoring in `PlacementFilter` with the replication penalty active.

### Integration

- **Steady-state convergence.** 3-replica + 1-publisher mesh. Continuous appends; assert all replicas converge to leader's tail within `2 × heartbeat_ms`.
- **Failover.** Kill leader; assert one replica promotes; new appends land on new leader; old leader on rejoin catches up via skip-or-replay path.
- **Disk pressure.** Replica configured below leader's retention; assert graceful withdrawal under `UnderCapacity::Withdraw`; assert eviction under `UnderCapacity::EvictOldest`.
- **Leader pinning.** With `leader_pinned: Some(N)`, succession always returns N when N is in the surviving set; falls through to `min(node_id)` over the surviving set when N is unhealthy.
- **Bandwidth budget.** Under saturating publisher load, replication-sync I/O ≤ `replication_budget_fraction × NIC_peak`. Treat regression as test failure.
- **Capability-tag consistency.** Replica's `causal:` tag presence ↔ replica's actual replication state. No tag drift on graceful or ungraceful exits.

### DST (gating phase F)

- **Random partition.** Inject network partitions of varying duration; assert convergence on heal.
- **Restart-during-sync.** Replica crashes mid-`SYNC_RESPONSE` application; assert restart resumes correctly without data loss.
- **Leadership thrash.** Sustained leader-loss detection for various reasons (heartbeat loss, proximity-graph false positives); assert succession thrash is bounded by hysteresis (no succession occurs without 3 consecutive missed heartbeats).
- **Partition heal with stale monarch.** Two-partition split where each side sees its own `min(node_id)` as leader; on heal, assert (a) the higher-node_id side self-demotes within 1 heartbeat of observing the lower-id leader, (b) the deposed leader's `causal:` tag is reaped within 1 heartbeat via witness withdrawal — strictly faster than the disconnect-observer path — and (c) divergent tails are reconciled via §8 replay.
- **Cross-replica divergence-freedom within an observer** (the strong assertion): for any single observer's view, no two replicas declare different `tail_seq` for the same `seq` at any point during any failure scenario.

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

RedEX replica-set selection, leader concentration prevention (structural — heavily-leading nodes drop out of new sets), and fallback placement ALL consume:

- `PlacementFilter::placement_score(target, &Artifact::Replica { ... })`
- `IntentRegistry` (consumed via `metadata.intent`)
- `AntiAffinityConfig` (the leadership-concentration penalty)
- `ProximityGraph::nearest_rtt(candidate, predicate)` (locked in Capability §7a)

None of these exist in code today. They ship in **Capability System Plan Phase F**. RedEX Phases C and E **cannot start** until Phase F lands. The composite gate is therefore "Capability B AND F" before RedEX C/D/E.

### Leadership strategy

#### 3. Leadership strategy — monarchic deterministic ranking

RedEX uses a deterministic rank function (`min(node_id)` over the surviving replica set) to choose the leader. **There is no election protocol.** `StandbyGroup` provides membership + failure detection only; `PlacementFilter` provides replica-set *selection* only. Neither is consulted at succession time — succession is parameterless and instantaneous.

**Why not run an election protocol** (the prior draft of this plan, and the textbook default): elections exist to choose a writer when the rank function isn't already determined. For an append-only log with single-writer semantics, every replica's log is a prefix of (or equal to) the leader's log under normal operation; the only thing succession needs to decide is *which surviving member becomes the new writer*. If the answer is computable from primitives every node already shares — node_ids are globally unique and the surviving-set view is maintained by `StandbyGroup` — then the wire-message-and-collection-window dance is pure overhead. Skip it.

**Why not extend `StandbyGroup::promote()`** (Option A in the original audit): `StandbyGroup::promote` picks "the standby with the highest `synced_through`" (per `COMPUTE.md:268`) — a snapshot-currency axis that's correct for stateful daemons but wrong for an append-only log where all replicas converge to the same `tail_seq` under normal operation. The meaningful axis is "which replica is the agreed-upon successor," and `min(node_id)` answers that without consulting log state.

**Why not score-based monarchy** (use `PlacementFilter::placement_score` as the rank): scores depend on per-node views of proximity, resource availability, etc., which can diverge during churn. A rank function used for succession must depend only on globally-agreed primitives. Node_ids are globally agreed. Use the placement score for *eligibility* (who's in the replica set) where divergence in scoring just means slightly different replica-set memberships; use node_id for *ordering within the set* where divergence would mean dual-leader views.

**Mechanism**: §4 above. No new wire message; succession is computed locally from `StandbyGroup` membership events and signaled to peers via the existing `SYNC_HEARTBEAT` carrying `role=Leader` + incremented `roster_epoch`.

### Wire protocol

#### 4. Byte-level layouts pinned

§2 above defines:
- 3-byte subprotocol header (`subprotocol_id u16 LE` + `dispatch_code u8`) on every message
- Four dispatch codes: `SYNC_REQUEST` (0x20), `SYNC_RESPONSE` (0x21), `SYNC_HEARTBEAT` (0x22), `SYNC_NACK` (0x23)
- Reserved range `0x24..0x2F` for future variants. `0x24` was previously claimed for `LEADER_ELECTION`; reclaimed when the plan moved to monarchic succession.
- `SYNC_HEARTBEAT` carries `roster_epoch u64 LE` so peers can detect stale-monarch heartbeats from healed partitions.
- All multi-byte integers are little-endian fixed-width (no varints)
- 32-byte BLAKE2s `ChannelId` (matches existing `ChannelName::hash()` convention)
- Length-prefixed strings: `(u16 LE len, [len] utf-8 bytes)`
- Range encoding: `(start_seq u64 LE, end_seq u64 LE)` half-open `[start, end)`
- Error path uses `SYNC_NACK` with typed `error_code u8`; silent close is reserved for transport-level failure only

Round-trip tests for each message land in Phase A.

### State machine

#### 5. 3-state machine pinned

`ReplicaState::{Leader, Replica, Idle}` per §3. Transitions enumerated. There is no `Candidate` state because there is no election; succession is a deterministic local computation. DST scenarios (Phase F) model exactly these three states with the listed transitions.

### Operational decisions (recommendations from the prior open-questions list, ratified)

6. **Leader scope.** By default, leader = `min(node_id)` of the surviving replica set per §4. The publisher routes appends to whoever the rank function names. Explicit override via `leader_pinned: Option<NodeId>` for split publisher/leader topologies — when set, the pinned node wins as long as it's in the surviving set, otherwise the standard rule applies. Pin in test.

7. **`UnderCapacity` default — `Withdraw`.** Replication factor is a hard guarantee on the leader; replicas are best-effort under disk pressure. Replicas under capacity withdraw; reads fall through to greedy LRU if enabled. `UnderCapacity::EvictOldest` available as opt-in.

8. **Cross-segment atomicity — none.** Per `REDEX_PLAN.md` non-goal #23. Replicas catch up segment-by-segment. Documented explicitly in `SUBPROTOCOLS.md`.

9. **Replica rejoin / partition healing.** Replay gap if `gap < skip_threshold` (default 100 MB); skip-ahead + flag for divergence audit if larger. Skip-ahead preserves safety because we're not rewriting history; we're choosing not to retain old history locally on the replica.

10. **Bandwidth budget — 50% of measured NIC peak by default.** Cap replication-sync I/O at `replication_budget_fraction × NIC peak`. Backpressure-aware via reliable-stream's existing flow control. NIC peak is measured via the existing per-link throughput counters surfaced by the proximity graph; sampling window is 60 s rolling.

11. **Leader concentration prevention — anti-affinity at 30% threshold (applied at replica-set selection, not at succession).** Configured via `AntiAffinityConfig::leadership_concentration_threshold` (default 0.30) + `AntiAffinityConfig::leadership_concentration_penalty` (default 0.4) — both from Capability §7. The penalty multiplies the node's `placement_score` per the locked multiplicative composition rule when scoring it as a replica-set candidate, so a node already leading > 30% of channels gets its set-selection score cut by 60% and tends to drop out of new replica sets entirely. This is structural concentration prevention: a node that's not in a replica set cannot be its leader. No penalty is applied during succession itself — by the time a set is fixed, succession is just `min(node_id)`.

---

## Risks

- **DST story is the hardest part.** No replication design survives without DST coverage for partition + leader-flap + rejoin sequences. **Allocate ~30% of phase effort to DST harness work.** Treat this as non-negotiable; do not ship without it.
- **Leader concentration → write hotspots.** Mitigation above (anti-affinity); if telemetry shows > 30% leadership concentration on a single node, escalate.
- **Subprotocol code surface adds ~1500–2000 LoC to the mesh adapter.** Audit footprint before merge. Coordinator should compose from existing primitives, not invent new ones.
- **Leadership thrash under transient packet loss.** Aggressive heartbeat timeouts cause spurious successions. Mitigation: hysteresis (3 consecutive missed heartbeats); pin in DST.
- **Capability-tag staleness during succession.** During the brief window between a leader's death and the new leader's first heartbeat, the channel's `causal:` tag may briefly point to the dead leader. Mitigation: fast withdrawal on leader-loss detection; reads gracefully degrade to capability-index re-lookup. Pin in tests.
- **Stale monarch after partition heal.** Two partition halves can each have a `min(node_id)` leader during a split. Mitigation: stale-monarch self-demotion on observing a lower-id active peer; the divergent tail from the higher-id leader is reconciled via §8 replay. This is a fundamental property of any leader-driven scheme without quorum; quorum's "fix" is to deny one side the ability to commit during the split, which is operationally identical to having one of the two leaders' work disappear. Pin in DST.

---

## Effort

**3–8 focused weeks.** Wide range driven by DST harness depth. Reduced from the prior 4–9 estimate because succession is a pure local function instead of an election protocol — Phase E shrinks from 1 week to 3 days, and one fewer wire-message variant ships in Phase A.

- ~2000 LoC core (subprotocol + coordinator + monarchic-succession glue + placement-filter anti-affinity term for replica-set selection)
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
- **Capability Phase F (`PlacementFilter`).** `placement_score` for `Artifact::Replica`, `IntentRegistry`, `AntiAffinityConfig`, `ProximityGraph::nearest_rtt`. Consumed by RedEX's *replica-set selection* (top-N nodes form the eligible set) and *leader-concentration anti-affinity* (heavily-leading nodes drop out of new replica sets). The succession rank function itself is parameterless — it depends only on node_ids. RedEX Phases C and E cannot start until this lands because replica-set selection is needed to bootstrap the surviving-set view.
- **Capability Phase E (federated query primitives).** Used internally for operator-facing introspection ("which replicas hold this chain across the mesh"). Not on the critical path; useful for ops dashboards.

This phase produces:

- A `causal:` capability-tag emitter / withdrawer lifecycle that other Warriors phases consume (Phase 1 greedy reads from the same tag advertisements).
- A `MeshDaemon`-shaped daemon that demonstrates `PlacementFilter` consumption for *set selection* (template for future placement-driven daemons).
- A reference for "monarchic deterministic succession composing on top of `StandbyGroup` membership/failure-detection," documenting that single-writer append-only logs don't need election protocols when the surviving-set view is already maintained by other primitives.
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
