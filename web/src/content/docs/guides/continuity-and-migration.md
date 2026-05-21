# Continuity and Migration

Daemons that survive node failures are the whole point of the runtime, and continuity is the layer that makes survival meaningful. A daemon migrating from one node to another isn't just "the same code running somewhere else" — its identity, its causal chain, its observed history, and the events in flight at the moment of cutover all have to travel with it. Continuity is the protocol that gets that right.

This guide covers the three patterns: migrating a single daemon (planned moves and graceful drains), promoting a standby (failure recovery for stateful daemons), and forking off a new lineage (deliberate divergence with a verifiable lineage back to the parent).

## Migration: moving a live daemon

The migration protocol is a strict six-phase state machine: snapshot, transfer, restore, replay, cutover, complete. Each phase has explicit start and end conditions, the events in flight during the move are buffered and replayed in order, and the daemon's identity stays bound to its keypair across the whole process.

```rust
use net::adapter::net::compute::MigrationOrchestrator;

let orchestrator = MigrationOrchestrator::new(&mesh);

orchestrator.start_migration(
    daemon_origin_hash,
    source_node_id,
    target_node_id,
).await?;
```

Once started, the orchestrator drives every step. The source node snapshots the daemon (state plus causal chain head plus observed horizon); the target restores from the snapshot using a local `DaemonFactoryRegistry` that knows how to construct daemons of this kind; events that arrive on the source during the transfer are buffered and shipped to the target via `BufferedEvents`; the target replays the buffer in strict sequence order; routing flips at cutover; the source cleans up.

The orchestrator can pick a target itself. `start_migration_auto` queries the capability index for migration-capable nodes that match the daemon's requirements:

```rust
orchestrator.start_migration_auto(daemon_origin_hash, source_node_id).await?;
```

What survives a migration:

- **The daemon's identity.** The ed25519 keypair is part of the snapshot. Clients addressing the daemon by origin don't notice the move.
- **The causal chain.** The target resumes at `snapshot.through_seq + 1`. Events from before cutover and after cutover form one contiguous chain.
- **In-flight events.** Events that arrived on the source during the transfer are buffered there, shipped to the target, and replayed in order. Nothing is dropped.
- **Routing.** Peers reach the daemon by `origin_hash`, which doesn't change. The routing plane updates as part of cutover.

What doesn't survive a migration:

- **Host crash before snapshot.** If the source dies before producing a snapshot, there's nothing to migrate. The answer for workloads that can't tolerate this is a standby group.
- **Keypair transport.** The target's `DaemonFactoryRegistry` must already have the daemon's keypair when the snapshot arrives. The keypair is sensitive material and is provisioned out of band, not over the migration wire.

For most workloads — planned moves, load rebalancing, draining a node for maintenance — migration is what you want. It preserves the daemon completely and doesn't take it offline during the move.

## Standby groups: surviving an unplanned failure

When a source node crashes before a migration can run, there's no snapshot to ship. The right primitive for that case is a standby group: one daemon active, N − 1 standbys ready to promote.

```rust
use net::adapter::net::compute::StandbyGroup;

let group = StandbyGroup::new(group_id)
    .with_members(3)
    .with_daemon_factory(|| StatefulDaemon::new());

mesh.register_standby_group(group).await?;
```

How it works: the active daemon processes events normally. Periodically (`sync_standbys()`, called by you or by a policy you wire up), the active produces a snapshot and ships it to each standby. The standbys apply the snapshot but don't run the daemon — they hold readiness. Between syncs, the group buffers the events the active processed; on failure, the standby that's furthest along replays the buffer and promotes.

The trade-off is straightforward. Standbys cost you memory (N − 1 copies of the daemon's state) but no compute (they don't process events). Promotion latency is bounded by the size of the buffered-event replay, which is bounded by the time since the last sync.

For workloads where seconds matter — operations control planes, real-time decision making — standbys give you sub-second recovery from a node failure. For workloads where minutes are fine — most analytics, batch processing — migration on a healthy node plus replay-from-snapshot is simpler and cheaper.

## Replica groups: scaling stateless daemons

Where standby groups are for stateful work, replica groups are for stateless work. A `ReplicaGroup` runs N identical copies of a daemon, each with a deterministic identity derived from the group seed plus an index, with load-balanced routing across them.

```rust
use net::adapter::net::compute::{ReplicaGroup, LoadBalancer};

let group = ReplicaGroup::new(group_id, group_seed)
    .with_factor(5)
    .with_load_balancer(LoadBalancer::least_connections())
    .with_daemon_factory(|| StatelessWorker::new());

mesh.register_replica_group(group).await?;
```

Recovery is automatic and coordination-free. When a node fails, the affected replica is re-spawned on a different node using the same `group_seed + index`, which produces the same keypair — so the replica's origin hash is unchanged, peers routing to it don't notice the move, and the load balancer's view repairs on the next health check.

The model only works for daemons that are *actually* stateless. If your daemon's behavior depends on its own accumulated state, it isn't stateless, and a replica group will give you the wrong answers under failure. Use standby groups for that case, or — if the state is naturally partitioned by some key — use consistent-hash routing across replicas, where each replica owns a slice of the keyspace and re-derives its slice on recovery.

## Fork groups: deliberate divergence

Forking is the opposite of replication. Instead of N copies of the same daemon doing the same work, you make N independent daemons that share a common parent at a specific causal point and then evolve independently. Each fork has its own keypair, its own causal chain, and a verifiable lineage back to the parent:

```rust
use net::adapter::net::compute::ForkGroup;

let group = ForkGroup::from_parent(parent_origin, fork_seq)
    .with_count(3)
    .with_daemon_factory(|| StrategyDaemon::new());

mesh.register_fork_group(group).await?;
```

Each fork records its lineage in a `ForkRecord` carrying a verifiable sentinel hash. The fork's chain starts with a genesis link whose `parent_hash` is the sentinel, so events from the fork chain back through the genesis to the parent's chain at the fork point. Any node on the mesh can verify the lineage by recomputing the sentinel.

The use cases for forking are deliberate divergence. A/B testing on the same workload. Multi-strategy execution where each fork tries a different approach. Experiments where you want to run several variants and keep their results separate but related. The fork lineage gives you the auditability ("this output came from this experiment branch from this parent"); the fork independence gives you the freedom to let each branch evolve.

## Continuity proofs

A `ContinuityProof` is a compact 36-byte structure that proves an entity's causal chain is intact over a sequence range without transferring the full log. It's a primitive that lets one node verify another node's chain claim cheaply:

```rust
use net::adapter::net::continuity::ContinuityProof;

let proof = ContinuityProof {
    origin_hash,
    from_seq: 100,
    to_seq: 200,
    from_hash: link_at_100.parent_hash,
    to_hash: link_at_200.parent_hash,
};

let verified = verifier.verify(&proof, &log)?;
```

The verifier checks that recomputing the parent-hash chain from `from_seq` to `to_seq` lands on `to_hash`. If it does, the chain is intact over that range. Continuity proofs ride on a dedicated subprotocol; they're used in audit flows, in cross-node migration verification, and in any operation that needs a small structural witness without paying for the full log.

The companion type is `ContinuityStatus`, which an observer can use to describe what it sees:

- `Continuous` — chain is intact from genesis to head.
- `Forked` — the chain forked at some sequence; here are the original and the fork hashes.
- `Unverifiable` — there's a gap in observation; here's the last verified sequence and where the gap starts.
- `Migrated` — the entity moved between nodes; here's the migration point.

The four states are the vocabulary the continuity layer uses to talk about an entity's chain. Most application code doesn't reach for them directly — the runtime exposes them in failure logs, in the operator surface, and in tooling that needs to reason about chain health.

## Honest discontinuity

When a chain genuinely breaks — node crash without a recent snapshot, data corruption, conflicting events arriving on different paths — the runtime doesn't silently paper over it. It creates a `ForkRecord`, marks the original chain as discontinued, and starts a new entity with documented lineage:

```rust
pub enum DiscontinuityReason {
    NodeCrash { last_snapshot_seq: u64 },
    ChainBreak(ChainError),
    ConflictingChains { seq, hash_a, hash_b },
    Corruption,
}
```

The fork record is signed by the entity that detected the discontinuity and broadcast on a dedicated subprotocol. Downstream observers see the new entity, see its lineage, and can decide for themselves whether to treat it as a continuation or as a fresh entity. There's no implicit recovery — the discontinuity is visible.

This is the "honest discontinuity" principle. A chain that broke shouldn't pretend it didn't; observers shouldn't be lied to. If you have a workload that genuinely can't tolerate discontinuity, the answer is to make discontinuity less likely (snapshot more often, run with a standby group, replicate the underlying log) rather than to pretend it doesn't happen.

## Superposition

There's one wrinkle in the migration model. During the replay and cutover phases, the daemon is in both places — the source is still buffering, the target is replaying, and both have valid claims to being "the daemon." The continuity layer represents this as a `SuperpositionState`:

```rust
pub enum SuperpositionPhase {
    PreMigration,    // Only at source.
    Transferring,    // At source; snapshot in flight.
    DualActive,      // Both nodes processing in parallel.
    PostCutover,     // Only at target.
    Settled,         // Migration complete; superposition collapsed.
}
```

Observers can see the entity at both locations during `DualActive`. The superposition collapses to `Settled` after cutover completes and the source cleans up. In practice you won't see this state in application code — the runtime handles the routing — but it's the model the protocol uses, and it's the right vocabulary for reasoning about what happens at the cutover instant.

The takeaway: in Net, "an entity exists" is a more nuanced statement than it sounds. During normal operation an entity is at one place. During migration it's at two for a bounded window. After a fork it's at multiple places forever. The continuity layer gives you the tools to ask all three questions precisely.
