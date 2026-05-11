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

// Open a replicated channel. The same RedexFileConfig (with
// matching ReplicationConfig) should be used on every node that
// hosts a replica.
let cfg = RedexFileConfig::default()
    .with_replication(Some(
        ReplicationConfig::new()
            .with_factor(3)
            .with_heartbeat_ms(500),
    ));
let file = redex.open_file(&channel_name, cfg)?;
```

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

- **Node** (`@ai2070/net`):
  ```ts
  redex.enableReplication(mesh);
  await redex.openFile("my/channel", {
    replication: { factor: 3, heartbeatMs: 500n, placement: "standard" },
  });
  ```
- **Python** (`net`):
  ```python
  redex.enable_replication(mesh)
  redex.open_file("my/channel",
                  replication=True, replication_factor=3,
                  replication_heartbeat_ms=500)
  ```
- **Go** (cgo wrapper at `bindings/go/net/redex.go`):
  ```go
  redex.EnableReplication(meshArcPtr)
  redex.OpenFile("my/channel", &net.RedexFileConfig{
      Replication: &net.ReplicationConfig{
          Factor: 3, HeartbeatMs: 500, Placement: net.PlacementStandard,
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

- **`Standard`** (default) — let `PlacementFilter` decide based on
  `metadata.intent`, `metadata.colocate-with`, `scope:` tags,
  proximity, and resource availability. Production default.
- **`Pinned(Vec<NodeId>)`** — manual placement on a fixed `NodeId`
  set. Used for special-case topologies, integration tests, and
  recovery scenarios. The vector's length pins the effective
  replication factor regardless of `factor`.
- **`ColocationStrict`** — every replica must live on a node
  already holding the chain referenced by
  `metadata.colocate-with-strict`. Refuses placement on nodes
  with insufficient coverage.

**Phase F gap**: `Standard` and `ColocationStrict` currently
bootstrap with an empty replica set; the placement filter's
re-selection on roster change lands with Phase F. Until then, use
`Pinned` for production channels where you need deterministic
membership.

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
`node` must be in `set` — otherwise `validate()` rejects.

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
placement filter / pinned set selects this node
    │
    ▼
coordinator.transition_to(Replica, CapabilitySelected)
    │  ── Replica  (advertises causal:<hex> capability tag)
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
