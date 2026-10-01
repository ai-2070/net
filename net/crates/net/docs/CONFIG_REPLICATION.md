# Configuring RedEX replication

Operator-facing companion to [`STORAGE_AND_CORTEX.md`](STORAGE_AND_CORTEX.md).
This document covers how to turn on cross-node replication for a
RedEX channel, what each knob does, and what failure modes you'll
see in production.

## When to enable

Replication is opt-in per channel via `RedexFileConfig::replication`.
The default (`None`) keeps every existing channel single-node — no
observable behavior change, no wire traffic on the mesh's
`SUBPROTOCOL_REDEX`.

Turn it on when:

- The channel carries data you can't lose if one node's disk wipes.
- Multiple consumers in different fault domains read the channel
  and you want each to read from the nearest replica rather than
  hairpinning to the publisher.
- The channel's publish rate is bounded (heartbeat traffic + sync
  bandwidth grow linearly with replica count).

Don't turn it on when:

- The channel is local-only telemetry (e.g. per-process metrics).
- Loss of recent events on node-down is acceptable (the heartbeat
  cycle takes ~3 × `heartbeat_ms` to detect leader failure).
- You're not sure how many replicas you want — start single-node,
  measure, then add `ReplicationConfig` later.

## Quick start

```rust
use net::adapter::net::redex::{
    Redex, RedexFileConfig, ReplicationConfig, PlacementStrategy,
};

let mesh: Arc<MeshNode> = build_mesh()?;
let redex = Arc::new(Redex::new());

// Install the replication wiring on every Redex that participates.
// Idempotent — safe to call from multiple call sites.
redex.enable_replication(mesh.clone());

// Open the channel with the SAME config on every node that should be
// a candidate. With the default `Standard` placement every such node
// computes the same `factor` replicas; they join and elect a leader.
// Append on the leader.
let cfg = RedexFileConfig::default()
    .with_replication(Some(
        ReplicationConfig::new()
            .with_factor(3)
            .with_heartbeat_ms(500),
    ));
let file = redex.open_file(&channel_name, cfg)?;

// Or name the replicas yourself:
//   .with_placement(PlacementStrategy::Pinned(vec![node_a, node_b, node_c]))
```

Placement follows capability announcements, which each node
rate-limits (`min_announce_interval`, 10 s by default), so a
`Standard` set can take up to about two windows to form after the
nodes open the channel. A `Pinned` set is known at once.

`enable_replication` installs a per-`Redex` router on the mesh's
`SUBPROTOCOL_REDEX` inbound dispatch; subsequent `open_file` calls
with `replication: Some(_)` spawn one tokio task per channel. The
router auto-registers + unregisters at `open_file` / `close_file`
time.

### Binding-language equivalents

The same surface ships in every language binding. The replication
opt-in is a nested `replication` field on the channel config; the
operator surface (`enable_replication`, `replication_prometheus_text`)
is exposed as methods on the binding's `Redex` handle.

- **Node** (`@net-mesh/core`):
  ```ts
  redex.enableReplication(mesh);
  redex.openFile("my/channel", { replication: { factor: 3, heartbeatMs: 500n } });
  // or { placement: "pinned", pinnedNodes: [a, b, c] }
  ```
- **Python** (`net`):
  ```python
  redex.enable_replication(mesh)
  redex.open_file("my/channel",
                  replication=True, replication_factor=3,
                  replication_heartbeat_ms=500)
  # or replication_placement="pinned", replication_pinned_nodes=[a, b, c]
  ```
- **Go** (cgo wrapper at `bindings/go/net/redex.go`):
  ```go
  redex.EnableReplication(meshArcPtr)
  redex.OpenFile("my/channel", &net.RedexFileConfig{
      Replication: &net.ReplicationConfig{
          Factor: 3, HeartbeatMs: 500,
      },
  })
  ```
- **C/FFI**: the `libnet` cdylib exports `net_redex_*` symbols
  directly. See `bindings/go/net/redex.go`'s cgo header block for
  the canonical extern signatures; non-Go consumers wire to the
  same symbols. Config rides as a JSON string through
  `net_redex_open_file` to keep the C surface narrow.

## `ReplicationConfig` fields

### `factor: u8`

Number of replicas (including the leader) the channel maintains.
Range: `[1, 16]` (default `3`). `1` collapses to single-node-with-
coordinator — useful for testing the daemon lifecycle without
spinning peers. The ceiling is conservative (replication overhead
goes superlinear above ~8 replicas due to heartbeat fanout); plumb
your own ceiling if you have a genuine 16+-replica workload.

When `placement = Pinned(nodes)`, the effective factor is
`nodes.len()` — the operator's explicit list wins over the numeric
hint.

### `placement: PlacementStrategy`

Where replicas live and how they're chosen. Three options:

- **`Standard`** (default) — every node that opens the channel is a
  candidate, and every node picks the same `factor` of them (see
  *How `Standard` and `ColocationStrict` choose* below). Production
  default.
- **`Pinned(Vec<NodeId>)`** — manual placement on a fixed `NodeId`
  set. Used for special-case topologies, integration tests, and
  recovery scenarios. The vector's length pins the effective
  replication factor regardless of `factor`.
- **`ColocationStrict`** — like `Standard`, but only candidates
  already holding the chain named by
  `placement_metadata["colocate-with-strict"]` qualify; `validate()`
  rejects the strategy without it.

#### How `Standard` and `ColocationStrict` choose

Nodes agree on the replica set without coordinating, by computing it
from data every node sees the same way:

1. **Candidates.** A node that opens the channel advertises
   `dataforts:replica-candidate:<channel id>` and withdraws it when
   it closes the channel, disables replication, or withdraws under
   disk pressure.
2. **Scoring.** `StandardPlacement` over `Artifact::Replica`, using
   only announced data: colocation (`colocate-with` prefers,
   `colocate-with-strict` requires), `intent` (against the default
   intent registry) and advertised storage. RTT proximity, leadership
   anti-affinity and node-local custom filters differ by viewer, so
   they don't choose replicas; the election still ranks the chosen
   ones by RTT.
3. **Selection.** The top `factor` by score, ties to the lower
   NodeId.

Each node re-resolves every heartbeat. Selected while `Idle` → it
joins as a replica; no longer selected → it leaves (`Idle`, chain
tag withdrawn). A node that resolves *fewer* than `factor` replicas
waits two announce windows before joining that short set: the other
candidates may not have reached it yet, and joining at once would
make it the leader of a set of one beside another such leader,
diverging writes. A full set joins at once.

Views converge rather than agree instantly: until announcements
propagate, nodes can briefly compute different sets, and a peer
drops replication frames from a node outside its own set. A crashed
candidate stays selected until its announcement expires.

### `heartbeat_ms: u64`

Cadence between leader → replica heartbeats. Range:
`[100, u64::MAX]` (default `500`). Lower for faster failure
detection at the cost of more wire traffic; higher for less
overhead at the cost of slower failover.

Failure-detection window = `3 × heartbeat_ms` (three-missed
hysteresis). With the default `500 ms`, a silent leader is
declared dead after ~1.5 s — well under the activation-gate's "5 s
RTO" target.

Don't go below `100 ms` — heartbeat traffic dominates the
channel's effective throughput.

### `leader_pinned: Option<NodeId>`

Pin the leader to a specific node. `None` (default) lets the
deterministic election pick the lowest-RTT healthy replica. When
`Some(node)`, the election picks `node` whenever it's healthy.

Common reasons to pin:
- A specific node has the lowest write-latency to the publisher.
- An operator is running a blue/green deployment and wants to
  force traffic to a known canary.
- Compliance: the channel's writes must originate from a node in
  a specific data center.

If `placement = Pinned(set)` and `leader_pinned = Some(node)`,
`node` must be in `set` — otherwise `validate()` rejects. Under
`Standard` / `ColocationStrict` the pin applies only while `node` is
in the resolved set.

### `placement_metadata: BTreeMap<String, String>`

Placement hints for `Standard` / `ColocationStrict`, read as the
replica artifact's metadata. Ignored by `Pinned`.

- `colocate-with` — a chain's 16-hex origin hash; candidates
  advertising it (`causal:<hex>`) score above those that don't.
- `colocate-with-strict` — the same, but required; mandatory for
  `ColocationStrict`.
- `intent` — an intent from the default intent registry; candidates
  that don't satisfy it are excluded.

Bindings: `placementMetadata` (Node), `replication_placement_metadata`
(Python), `placement_metadata` (C / Go JSON).

### `on_under_capacity: UnderCapacity`

Behavior when a replica's local file rejects an append because of
disk pressure (heap segment at the 3 GB hard cap, or
persistent-tier write fail).

- **`Withdraw`** (default) — drop the replica role; the
  coordinator transitions to `Idle`, the `causal:<hex>` capability
  tag is withdrawn, and peers re-resolve to a healthy replica via
  `find_chain_holders`. Reads re-route as a natural consequence.
- **`EvictOldest`** — call `RedexFile::sweep_retention()` to free
  space, keep the replica role, retry the apply on the next
  chunk. **Requires `retention_max_*` to be configured on the
  same `RedexFileConfig`** — without retention caps the sweep is
  a no-op and the next apply will fail again.

`under_capacity_total` bumps on both branches regardless of
policy, so the operator-facing counter reflects every disk-pressure
event.

### `replication_budget_fraction: f32`

Fraction of measured NIC peak that replication-sync I/O may
consume. Range: `(0.0, 1.0]` (default `0.5`). The bandwidth
budget is a token bucket; leaders reject `SyncRequest`s with
`SyncNackError::Backpressure` when the bucket is empty.

The denominator is currently a 1 Gbps placeholder; the
proximity-graph throughput probe wires the measured peak in a
follow-up.

## Lifecycle

```text
open_file(channel, cfg with replication=Some(_))
    │
    ▼
spawn ReplicationRuntime (tokio task per channel)
    │  ── Idle  (initial)
    ▼
this node is selected: in the pinned set (checked at start), or in
the set placement resolves (Standard / ColocationStrict, re-checked
every heartbeat; a short set waits two announce windows first)
    │
    ▼
coordinator.transition_to(Replica, CapabilitySelected)
    │  ── Replica  (advertises causal:<hex> capability tag;
    │               starts waiting for a leader)
    ▼
no leader heard within 3 × heartbeat_ms → Candidate → election
    │  (leader_pinned wins when healthy; otherwise lowest RTT, and
    │   self ranks first, so two may win: a leader that hears a peer
    │   leader with a higher tail, or the same tail and a lower
    │   node id, concedes to Replica)
    ▼
heartbeat loop:
  - Leader emits heartbeats every heartbeat_ms
  - Replica observes leader's tail_seq in each heartbeat
  - If replica's local tail < leader's tail, replica emits SyncRequest
  - Leader's handle_sync_request reads from local file, returns SyncResponse
  - Replica's apply_sync_response advances local tail
    │
    ▼  (leader silent for 3 × heartbeat_ms)
coordinator.transition_to(Candidate, MissedHeartbeats)
    │  ── Candidate  (microseconds-scale; deterministic election)
    ▼
elect(replica_set, self, rtt_lookup, healthy_peers) →
    SelfWins → transition_to(Leader, ElectionWon)
    PeerWins(_) → transition_to(Replica, ElectionLost)
    NoEligibleReplica → stay Candidate, next round
    │
    ▼
close_file(channel)
    │
    ▼
coordinator.transition_to(Idle, ChannelClose) + router unregisters
```

## Turning replication off

`Redex::disable_replication()` undoes `enable_replication`: every
channel's runtime is unregistered and shut down, the router comes off
the mesh, and the `Redex` drops its `Arc<MeshNode>`. Idempotent. Open
files stay open as local logs; a later `enable_replication` installs
fresh wiring, and channels opened before it stay unreplicated until
reopened. Inside a tokio runtime each channel shuts down gracefully
in a spawned task (withdrawing its chain advertisement on the way to
`Idle`); outside one, the runtime tasks are aborted.

Bindings: `redex.disableReplication()` (Node), `redex.disable_replication()`
(Python). Call it before `NetMesh.shutdown()` in Node, which needs the
node's only reference; dropping the `Redex` also releases it, but in
JS only when the garbage collector gets to it.

## Observability

Per-channel atomic counters (`ChannelMetricsAtomic`) exposed via
the `ReplicationMetricsRegistry`. Prometheus shapes:

| Metric | Type | Meaning |
|--------|------|---------|
| `dataforts_replication_lag_seconds{channel,role}` | gauge | Leader: max-across-replicas of `now - last_heartbeat`. Replica: `now - believed_leader.last_heartbeat`. |
| `dataforts_replication_sync_bytes_total{channel}` | counter | Cumulative bytes shipped via `SyncResponse`. |
| `dataforts_leader_changes_total{channel}` | counter | Transitions into Leader role. Spikes indicate election thrash. |
| `dataforts_replication_under_capacity_total{channel}` | counter | Disk-pressure events (bumps regardless of policy). |
| `dataforts_replication_skip_ahead_total{channel}` | counter | `BadRange` NACKs received (gap exceeded `skip_threshold`). |
| `dataforts_replication_election_thrash_total{channel}` | counter | `MissedHeartbeats` transitions; > 1/30s indicates instability. |
| `dataforts_replication_witness_withdrawals_total{channel}` | counter | Reserved for Phase E witness coordination. |

Render via `ReplicationMetricsRegistry::snapshot().prometheus_text()`.

For per-channel introspection (current role, manual transition for
recovery), `Redex::replication_coordinator_for(channel_name) ->
Option<Arc<ReplicationCoordinator>>` returns the coordinator handle.

## Failure modes

| Symptom | Likely cause | Resolution |
|---------|--------------|------------|
| Replica's `lag_seconds` keeps growing | Leader's bandwidth budget exhausted, or replica's mesh path saturated | Increase `replication_budget_fraction`, or check the proximity-graph throughput probe for path-level loss |
| Frequent `leader_changes_total` bumps | `heartbeat_ms` too aggressive for the link's typical RTT variance | Bump `heartbeat_ms`, or pin leader with `leader_pinned` |
| `under_capacity_total` > 0 + replica disappeared | `UnderCapacity::Withdraw` fired | Free disk on the replica or switch policy to `EvictOldest` (requires retention caps) |
| `skip_ahead_total` > 0 | Replica fell more than `skip_threshold` behind; leader's retained range trimmed past replica's tail | Either accept the data loss or bump leader's retention caps |
| `election_thrash_total` rising | Two replicas oscillating leadership under flaky connectivity | Investigate the proximity graph; partition-detector should fire if pathology is partition-shaped |

## Limits + non-goals

- **One writer per channel** — the leader is the single writer.
  RedEX is append-only and monotonic on `seq`; multi-writer
  topologies are out of scope.
- **Replication is best-effort under pressure** — the leader's
  replication factor is a hard guarantee, but individual replicas
  fall back to `UnderCapacity` policy when local storage saturates.
- **Skip-ahead is heap-only** — when the leader's `SyncResponse`
  carries `first_seq` above the replica's local tail (the leader
  trimmed past the replica's retained range), the replica calls
  `RedexFile::skip_to(first_seq)` and retries the apply. Persistent
  files (`redex-disk`) reject `skip_to` with a typed error; affected
  replicas fall back to NACK BadRange and heartbeat-cycle recovery
  while the persistent-tier truncate+rebuild path waits for v2.
- **DST coverage is partial** — pure-logic pieces (state machine,
  election, catch-up helpers, runtime tick) have unit tests; the
  full deterministic-simulation harness for partition + retention-
  drift scenarios is Phase F work.
