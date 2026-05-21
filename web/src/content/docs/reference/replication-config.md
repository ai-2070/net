# Replication Configuration

This page is the reference for the per-channel RedEX replication knobs — what each field does, what ranges are valid, and what failure modes you'll see in production. It's the operator-facing companion to [durable logs](../guides/durable-logs) and goes into the detail the guide doesn't.

Replication is opt-in per channel. The default behavior (`replication: None` on `RedexFileConfig`) keeps every existing channel single-node — no observable change, no wire traffic on the replication subprotocol.

## Enabling replication

Two things have to happen to make a channel replicated:

```rust
// 1. Install the replication wiring on the Redex manager. Idempotent;
//    safe to call from multiple sites.
redex.enable_replication(mesh.clone());

// 2. Open the channel with replication configured.
let cfg = RedexFileConfig::default()
    .with_replication(Some(
        ReplicationConfig::new()
            .with_factor(3)
            .with_heartbeat_ms(500),
    ));
let file = redex.open_file(&channel_name, cfg)?;
```

`enable_replication` installs the per-`Redex` router on the mesh's `SUBPROTOCOL_REDEX` dispatch (`0x0E00`). Subsequent `open_file` calls with `replication: Some(_)` spawn one Tokio task per channel — a replication coordinator that handles leader election, heartbeats, and sync.

The same `RedexFileConfig` (with matching `ReplicationConfig`) should be used on every node that hosts a replica.

## `ReplicationConfig` fields

### `factor: u8`

The number of replicas (including the leader) the channel maintains.

- **Range:** `[1, 16]`
- **Default:** `3`

A factor of `1` collapses to single-node-with-coordinator — useful for testing the daemon lifecycle without spinning peers. The upper bound of `16` is conservative; replication overhead goes superlinear above ~8 replicas due to heartbeat fanout. Plumb your own ceiling if you have a genuine 16+-replica workload, but expect the cost.

When `placement = Pinned(nodes)`, the effective factor is `nodes.len()` — the explicit list wins over the numeric hint.

### `placement: PlacementStrategy`

Where replicas live and how they're chosen.

```rust
pub enum PlacementStrategy {
    Standard,
    Pinned(Vec<NodeId>),
    ColocationStrict,
}
```

- **`Standard`** (default). The placement filter scores candidates based on `metadata.intent`, `metadata.colocate-with`, `scope:` tags, proximity, and resource availability. The production default for most channels.
- **`Pinned(Vec<NodeId>)`**. Manual placement on a fixed `NodeId` set. The vector's length pins the effective replication factor regardless of `factor`. Useful for special-case topologies, integration tests, and recovery scenarios.
- **`ColocationStrict`**. Every replica must live on a node already holding the chain referenced by `metadata.colocate-with-strict`. Refuses placement on nodes with insufficient coverage.

### `heartbeat_ms: u64`

Cadence between leader-to-replica heartbeats.

- **Range:** `[100, u64::MAX]`
- **Default:** `500`

Lower values give faster failure detection at the cost of more wire traffic; higher values give less overhead at the cost of slower failover.

The failure-detection window is `3 × heartbeat_ms` (three-missed hysteresis). With the default `500 ms`, a silent leader is declared dead after about 1.5 seconds — well under the typical "5-second RTO" target.

Don't go below `100 ms`. Heartbeat traffic dominates the channel's effective throughput at that point.

### `leader_pinned: Option<NodeId>`

Pin the leader to a specific node.

- **Default:** `None`

`None` lets the deterministic election pick the lowest-RTT healthy replica. `Some(node)` forces election to favor `node` whenever it's healthy.

Common reasons to pin:

- A specific node has the lowest write latency to the publisher.
- An operator is running a blue/green deployment and wants to force traffic to a known canary.
- Compliance: writes must originate from a node in a specific data center.

If `placement = Pinned(set)` and `leader_pinned = Some(node)`, `node` must be in `set` — otherwise validation rejects.

### `on_under_capacity: UnderCapacity`

Behavior when a replica's local file rejects an append because of disk pressure (heap segment at the 3 GB hard cap, or persistent-tier write fail).

```rust
pub enum UnderCapacity {
    Withdraw,        // default
    EvictOldest,
}
```

- **`Withdraw`** (default). Drop the replica role. The coordinator transitions to `Idle`, the `causal:<hex>` capability tag is withdrawn, and peers re-resolve to a healthy replica via `find_chain_holders`. Reads re-route automatically.
- **`EvictOldest`**. Call `RedexFile::sweep_retention()` to free space, keep the replica role, retry the apply on the next chunk. **Requires `retention_max_*` configured on the same `RedexFileConfig`** — without retention caps the sweep is a no-op and the next apply fails again.

The `under_capacity_total` counter increments on both branches regardless of policy, so the operator-facing metric reflects every disk-pressure event.

### `replication_budget_fraction: f32`

Fraction of measured NIC peak that replication-sync I/O may consume.

- **Range:** `(0.0, 1.0]`
- **Default:** `0.5`

The bandwidth budget is a token bucket; leaders reject `SyncRequest`s with `SyncNackError::Backpressure` when the bucket is empty. Replicas back off and retry with the same request.

The denominator is currently a 1 Gbps placeholder; the proximity-graph throughput probe wires the measured peak in a follow-up.

## Lifecycle

```
open_file(channel, cfg with replication=Some(_))
    │
    ▼
spawn ReplicationRuntime (one Tokio task per channel)
    │
    ▼  initial state
  ┌─────┐
  │ Idle│
  └──┬──┘
     │  placement filter / pinned set selects this node
     ▼
  ┌──────────┐
  │ Replica  │  ── advertises causal:<hex> capability tag
  └────┬─────┘
       │  heartbeat loop:
       │   - Leader emits heartbeats every heartbeat_ms
       │   - Replica observes leader's tail_seq
       │   - If replica is behind, emit SyncRequest
       │   - Leader returns SyncResponse; replica applies
       │
       ▼  (leader silent for 3 × heartbeat_ms)
  ┌──────────┐
  │Candidate │  ── microseconds; deterministic election
  └────┬─────┘
       │
       ▼
       elect(...) →
         SelfWins   → transition_to(Leader)
         PeerWins   → transition_to(Replica)
         NoEligible → stay Candidate, retry next round
       │
       ▼
  ┌──────────┐
  │ Leader   │
  └────┬─────┘
       │  close_file(channel)
       ▼
  ┌─────┐
  │ Idle│ + router unregisters
  └─────┘
```

## Observability

Per-channel atomic counters exposed via the `ReplicationMetricsRegistry`. Prometheus shapes:

| Metric                                            | Type    | Meaning                                                                                  |
|---------------------------------------------------|---------|------------------------------------------------------------------------------------------|
| `dataforts_replication_lag_seconds{channel,role}` | gauge   | Leader: max-across-replicas of `now - last_heartbeat`. Replica: `now - believed_leader.last_heartbeat`. |
| `dataforts_replication_sync_bytes_total{channel}` | counter | Cumulative bytes shipped via `SyncResponse`.                                             |
| `dataforts_leader_changes_total{channel}`         | counter | Transitions into `Leader` role. Spikes indicate election thrash.                          |
| `dataforts_replication_under_capacity_total{channel}` | counter | Disk-pressure events. Increments on both `Withdraw` and `EvictOldest`.                |
| `dataforts_replication_skip_ahead_total{channel}` | counter | `BadRange` NACKs received (replica fell more than `skip_threshold` behind).               |
| `dataforts_replication_election_thrash_total{channel}` | counter | `MissedHeartbeats` transitions. > 1 per 30 s indicates instability.                  |
| `dataforts_replication_witness_withdrawals_total{channel}` | counter | Reserved for future witness-coordination phase.                                  |

Render via `ReplicationMetricsRegistry::snapshot().prometheus_text()`.

For per-channel introspection (current role, manual transition for recovery), use `Redex::replication_coordinator_for(channel_name)` to obtain an `Arc<ReplicationCoordinator>` handle.

## Failure modes

| Symptom                                              | Likely cause                                                              | Resolution                                                                       |
|------------------------------------------------------|---------------------------------------------------------------------------|----------------------------------------------------------------------------------|
| Replica's `lag_seconds` keeps growing                | Leader's bandwidth budget exhausted, or replica's mesh path saturated     | Raise `replication_budget_fraction` or investigate the proximity-graph throughput probe |
| Frequent `leader_changes_total` bumps                | `heartbeat_ms` too aggressive for the link's typical RTT variance         | Raise `heartbeat_ms` or pin leader with `leader_pinned`                          |
| `under_capacity_total` > 0 + replica disappeared     | `UnderCapacity::Withdraw` fired                                           | Free disk on the replica, or switch policy to `EvictOldest` (needs retention caps) |
| `skip_ahead_total` > 0                               | Replica fell more than `skip_threshold` behind; leader's retained range trimmed past replica's tail | Accept the data loss, or raise the leader's retention caps         |
| `election_thrash_total` rising                       | Two replicas oscillating leadership under flaky connectivity              | Investigate the proximity graph; partition detector should fire if pathology is partition-shaped |

## Limits and non-goals

- **One writer per channel.** The leader is the single writer. RedEX is append-only and monotonic on `seq`; multi-writer topologies are out of scope.
- **Best-effort under pressure.** The leader's replication factor is a hard guarantee, but individual replicas fall back to `UnderCapacity` policy when local storage saturates.
- **Skip-ahead is heap-only.** When the leader trims past a replica's local tail, in-memory replicas can `skip_to(first_seq)` and retry. Persistent files reject `skip_to` with a typed error; affected replicas fall back to NACK + heartbeat-cycle recovery while the persistent-tier rebuild path is in development.
