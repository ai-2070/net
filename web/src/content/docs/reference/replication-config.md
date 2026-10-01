---
title: Replication Configuration
description: "This page is the reference for the per-channel RedEX replication knobs — what each field does, what ranges are valid, and what failure modes you'll see in production."
---
# Replication Configuration

This page is the reference for the per-channel RedEX replication knobs — what each field does, what ranges are valid, and what failure modes you'll see in production. It's the operator-facing companion to [durable logs](/docs/guides/durable-logs) and goes into the detail the guide doesn't.

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

`enable_replication` installs the per-`Redex` router on the mesh's `SUBPROTOCOL_REDEX` dispatch (`0x0E00`). Subsequent `open_file` calls with `replication: Some(_)` spawn one Tokio task per channel — a replication coordinator that handles placement, leader election, heartbeats, and sync.

Open the channel with the same `RedexFileConfig` (with matching `ReplicationConfig`) on every node that should take part. The nodes choose the replicas and elect a leader among themselves; write on the leader.

The bindings take the same config: `replication: { factor: 3, heartbeatMs: 500n }` on Node's `openFile`, `replication=True, replication_factor=3` on Python's `open_file`, a `placement_metadata` / `PlacementMetadata` field in the C and Go JSON.

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

- **`Standard`** (default). Every node that opens the channel is a candidate, and every node picks the same `factor` of them (below). The production default for most channels.
- **`Pinned(Vec<NodeId>)`**. Manual placement on a fixed `NodeId` set, known at once. The vector's length pins the effective replication factor regardless of `factor`. Useful for special-case topologies, integration tests, and recovery scenarios.
- **`ColocationStrict`**. `Standard`, but only candidates already holding the chain named by `placement_metadata["colocate-with-strict"]` qualify. Validation rejects the strategy without it.

#### How `Standard` and `ColocationStrict` choose

Nodes agree on the replica set without coordinating, by computing it from data every node sees the same way:

1. **Candidates.** A node that opens the channel advertises a `dataforts:replica-candidate:<channel id>` capability tag, and withdraws it when it closes the channel, disables replication, or withdraws under disk pressure.
2. **Scoring.** The placement filter scores each candidate on announced data only: colocation (`colocate-with` prefers, `colocate-with-strict` requires), `intent`, and advertised storage. RTT and leadership load differ from node to node, so they don't choose replicas; the election still ranks the chosen ones by RTT.
3. **Selection.** The top `factor` by score, ties to the lower `NodeId`.

Each node re-checks every heartbeat: a newly selected node joins, a node no longer selected leaves. Capability announcements are rate-limited per node (10 s windows by default), so a set can take up to about two windows to form. A node that sees *fewer* than `factor` candidates waits two windows before joining that short set, so it doesn't become the leader of a set of one beside another such leader; a full set joins at once.

### `placement_metadata: BTreeMap<String, String>`

Placement hints for `Standard` / `ColocationStrict`, read as the replica artifact's metadata. Ignored by `Pinned`.

- `colocate-with`: a chain's origin hash; candidates holding it score above those that don't.
- `colocate-with-strict`: the same, but required. Mandatory for `ColocationStrict`.
- `intent`: an intent from the default intent registry; candidates that don't satisfy it are excluded.

Chain hashes are 16 lowercase hex digits (the form in `causal:` tags); validation rejects anything else, since it could never match.

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

If `placement = Pinned(set)` and `leader_pinned = Some(node)`, `node` must be in `set` — otherwise validation rejects. Under `Standard` / `ColocationStrict` the pin applies only while `node` is in the resolved set.

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

### `default_bandwidth_class: BandwidthClass`

The per-channel default class the runtime stamps on every emitted `SyncRequest`, unless the caller overrides it explicitly.

- **Default:** `Foreground`

Receivers honor a per-request value in preference to this default, so this is the fallback rather than a cap.

### `background_fraction: f32`

The admission-gate parameter that keeps `Background` sync from crowding out `Foreground` sync. A `Background` request is admitted only when `available >= (1 - background_fraction) * capacity`.

- **Range:** `[0.0, 1.0)`
- **Default:** `0.3`

Tune it per channel against what the channel is for: hot channels set it low (a tight bound on Background, more Foreground headroom), archival channels set it high (give Background room to make progress).

`1.0` is rejected at validation — it would deny every `Background` request unconditionally, which is never what anyone means. Note also that a starved request gets a one-shot bypass of the gate after 60 seconds regardless of the configured value, so a low fraction throttles Background traffic without wedging it permanently.

## Lifecycle

```text title="Replication decision flow"
open_file(channel, cfg with replication=Some(_))
    │
    ▼
spawn ReplicationRuntime (one Tokio task per channel)
    │
    ▼  initial state
  ┌─────┐
  │ Idle│
  └──┬──┘
     │  this node is selected: in the pinned set, or in the
     │  set placement resolves (re-checked every heartbeat)
     ▼
  ┌──────────┐
  │ Replica  │  ── advertises causal:<hex> capability tag;
  └────┬─────┘     starts waiting for a leader
       │  heartbeat loop:
       │   - Leader emits heartbeats every heartbeat_ms
       │   - Replica observes leader's tail_seq
       │   - If replica is behind, emit SyncRequest
       │   - Leader returns SyncResponse; replica applies
       │
       ▼  (no leader heard, or leader silent, for 3 × heartbeat_ms)
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
       │  close_file(channel), or no longer selected
       ▼
  ┌─────┐
  │ Idle│ + router unregisters, tag withdrawn
  └─────┘
```

## Turning replication off

`Redex::disable_replication()` undoes `enable_replication`: every channel's runtime shuts down (withdrawing its candidacy and chain tag), the router comes off the mesh, and the `Redex` releases the mesh. Open files stay open as local logs. `disable_replication_and_wait().await` returns once the runtimes have released the mesh; the bindings all wait (`await redex.disableReplication()` in Node, which must come before `NetMesh.shutdown()`; blocking in Python and C).

`open_file` honors a channel's config only on its first open, so a channel that stayed open across a disable must be closed and reopened after re-enabling to replicate again.

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
| `dataforts_replication_announce_divergence_total{channel}` | counter | A `* → Idle` transition's withdraw-chain call failed *after* the state cell already flipped. See below. |

Render via `ReplicationMetricsRegistry::snapshot().prometheus_text()`.

`announce_divergence_total` is the one to put an alert on. Until a retry lands, the mesh may still be advertising this node as a holder of a chain it no longer replicates — readers get routed to a node that won't serve them. The runtime retries a failed announce or withdraw every heartbeat, so a single bump during churn is expected; a counter that keeps climbing means the retries keep failing.

The registry is bounded by `MAX_TRACKED_CHANNELS`; channels past the cap fold into a shared `__overflow__` bucket. If `__overflow__` shows up in a scrape, per-channel attribution has already been lost for some channels — treat it as a cardinality signal, not a channel name.

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
