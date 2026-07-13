# Daemons and Placement

A daemon in Net is a long-running stateful event processor. You write a small piece of code that consumes events and produces events; the runtime handles where it runs, what it can see, when it gets migrated, and how it survives node failures. The whole point is to let you write the *what* — the business logic — and leave the *where* and *when* to the system.

Daemons are how you turn the event bus from a transport into a substrate for distributed compute. Anything you'd otherwise build as "a service that subscribes to a queue and updates some state" is a daemon. The runtime gives you the placement, the failure handling, and the causal-continuity guarantees on top.

## The `MeshDaemon` trait

The contract for synchronous, WASM-friendly daemons is small. Implement five methods:

```rust
use net::adapter::net::compute::{MeshDaemon, DaemonError};
use net::adapter::net::behavior::CapabilityFilter;
use net::adapter::net::state::CausalEvent;
use bytes::Bytes;

struct CounterDaemon {
    count: u64,
}

impl MeshDaemon for CounterDaemon {
    fn name(&self) -> &str { "counter" }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()  // No special placement needs
    }

    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        self.count += 1;
        Ok(vec![Bytes::from(format!("count={}", self.count))])
    }

    fn snapshot(&self) -> Option<Bytes> {
        Some(Bytes::from(self.count.to_le_bytes().to_vec()))
    }

    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
        let bytes: [u8; 8] = state[..8].try_into()
            .map_err(|_| DaemonError::RestoreFailed("expected 8-byte counter state".into()))?;
        self.count = u64::from_le_bytes(bytes);
        Ok(())
    }
}
```

Five methods, four concerns:

- **`name()`** identifies the daemon in logs and metrics. Convention is module-style: `inference.scorer`, `audit.collector`.
- **`requirements()`** declares what kind of node the daemon needs. Capability filters here ("must have a GPU," "must be on tier=production") drive placement.
- **`process()`** is the hot path. Each event in, zero or more output payloads out. The runtime wraps your outputs in causal links automatically and publishes them on the daemon's output channel.
- **`snapshot()` and `restore()`** are the migration primitives. The runtime calls `snapshot` to capture state before a migration; `restore` rehydrates the state on the destination node. If your daemon is stateless, return `None` from `snapshot` and the migration becomes a re-spawn instead of a state transfer.

A few constraints worth honoring:

- **`process()` must be fast.** Tens of microseconds, ideally. Heavy work should be deferred to a background task that publishes back into the bus.
- **All methods are synchronous.** This is for WASM compatibility — `MeshDaemon` is designed to be runnable both in-process and as WASM modules, and the WASM ABI doesn't allow async. For daemons that need an async loop, use the `LifecycleDaemon` sibling trait described below.
- **No generics or associated types.** The daemon trait is `dyn`-compatible because the runtime tracks daemons as trait objects.

## Spawning

A daemon runs inside a `DaemonHost` — the wrapper that owns the daemon's keypair, its causal chain, and its host config, and calls `process()` for each event that arrives. You don't build the host by hand; you hand the daemon and its keypair to the MeshOS daemon SDK, which constructs the host and registers it:

```rust
use net::adapter::net::behavior::meshos::MeshOsDaemonSdk;
use net::adapter::net::identity::EntityKeypair;

// `sdk: MeshOsDaemonSdk` was started once for this node.
let handle = sdk.register_daemon(
    Box::new(CounterDaemon { count: 0 }),
    daemon_keypair,
)?;
```

`register_daemon` does three things: it builds a `DaemonHost` from the daemon plus its keypair, inserts the host into the runtime's `DaemonRegistry`, and wires up the daemon's control channel. It's synchronous — registration is a local bookkeeping step, not a network round-trip (placement, described next, is a separate concern). The returned `handle` is your reference to the running daemon — drop it (or call `graceful_shutdown()`) to take the daemon down.

## Placement

Where a daemon ends up running is decided by the placement scheduler. The scheduler reads the daemon's `requirements()`, queries the mesh's capability fold for matching nodes, and scores each candidate. The default scorer combines five axes:

- **Capability match.** Does the node satisfy the filter? Hard veto if not.
- **Load.** How many other daemons is the node running, and how much spare capacity does it have?
- **Anti-affinity.** Avoid placing replicas of the same daemon on the same node.
- **Resource fit.** Prefer nodes with closer-matching resource availability — don't waste a GPU node on a CPU-bound daemon if a CPU node is available.
- **Proximity.** Prefer nodes physically near the daemon's input traffic.

Each axis is weighted; the highest-scoring candidate wins.

### Custom placement filters

For requirements the built-in axes don't capture, plug in a custom predicate. In Rust you implement the `PlacementFilter` trait — the runtime calls `placement_score` per candidate node with the artifact being placed, and you return a verdict — then register the filter under a stable id and point a `StandardPlacement` at that id:

```rust
use std::sync::Arc;
use net::adapter::net::behavior::placement::{
    Artifact, NodeId, PlacementFilter, StandardPlacement,
};
use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;

/// Keep a candidate only if it advertises enough VRAM.
struct GpuVramFits {
    min_vram_gb: u32,
}

impl PlacementFilter for GpuVramFits {
    fn placement_score(&self, _target: &NodeId, _artifact: &Artifact<'_>) -> Option<f32> {
        let vram_gb: u32 = /* read the candidate's advertised `hardware.vram_gb` */ 0;
        // `None` is a hard veto; `Some(score)` composes into the score.
        (vram_gb >= self.min_vram_gb).then_some(1.0)
    }
}

// Register the filter under an id, then reference it by that id.
global_placement_filter_registry().register(
    "gpu-vram-fits".to_string(),
    Arc::new(GpuVramFits { min_vram_gb: 24 }),
    "rust",
);

let placement = StandardPlacement::new(&fold).with_custom_filter_id("gpu-vram-fits");
```

A filter's `Some(score)` composes multiplicatively with the built-in axes; a `None` is a hard veto that removes the candidate entirely. The other bindings (TS, Python, Go, C) register the same kind of filter through a `placementFilterFromFn` helper that wraps the closure across the FFI, and predicates built through the substrate's `Predicate` AST evaluate identically across all of them — so a placement filter written in TS produces the same verdict as one written in Rust.

## Async daemons — the `LifecycleDaemon` trait

Some daemons need an async loop: they publish on a periodic timer, call out to other services, or pump a long-running operation alongside event processing. The `MeshDaemon` trait is synchronous-by-design for WASM compatibility, so for those cases the runtime provides an async sibling: `LifecycleDaemon`.

```rust
use std::sync::Arc;
use async_trait::async_trait;
use net::adapter::net::behavior::lifecycle::{
    LifecycleDaemon, LifecycleError, LifecycleHandle, ReplicaHealth,
};
use net::adapter::net::behavior::CapabilityFilter;

#[async_trait]
impl LifecycleDaemon for HealthScraper {
    fn name(&self) -> &str { "health.scraper" }

    fn requirements(&self) -> CapabilityFilter { CapabilityFilter::default() }

    async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
        // Spawn the periodic loop here, moving `self` (an Arc) into the task.
        let daemon = Arc::clone(&self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                ticker.tick().await;
                if daemon.shutting_down() { break; }
                daemon.scrape_once().await;  // publishes back into the bus
            }
        });
        Ok(())
    }

    async fn on_stop(&self) {
        // Signal the spawned loop to wind down (e.g. flip a shutdown flag).
        self.request_shutdown();
    }

    async fn health(&self) -> ReplicaHealth {
        ReplicaHealth::healthy()
    }
}

let handle: LifecycleHandle =
    LifecycleHandle::start(Arc::new(HealthScraper::new(endpoint))).await?;
```

`LifecycleHandle` is an RAII wrapper: `start` runs `on_start` once, and dropping the handle schedules `on_stop` on a detached task (call `handle.stop().await` when you need deterministic shutdown ordering). There is no `tick` hook — the daemon owns its own loop, spawned inside `on_start`, and watches an internal shutdown flag so `on_stop` can wind it down cleanly. Combined with `LifecycleGroup<L>` (an N-replica primitive generic over the lifecycle daemon), the same lifecycle drives both single-daemon and replicated-daemon deployments.

When a `LifecycleGroup` is registered with a `HealthMonitor`, the monitor watches per-replica health and re-spawns failed replicas via the group's factory closure with exponential backoff. `register_with_monitor` is the one-call constructor that wires the registry and the monitor together — operators don't thread them by hand.

## Aggregator daemons

The canonical use of `LifecycleDaemon` is the aggregator: a daemon that sits one tier up from a source subnet, subscribes to that subnet's detail channels through the gateway, summarizes what it sees, and publishes the summary upward. The substrate ships an `AggregatorDaemon` implementation plus an `AggregatorRegistry` that lives on `MeshNode` alongside `DaemonRegistry`.

```rust
use std::sync::Arc;
use std::time::Duration;
use net::adapter::net::behavior::aggregator::{
    AggregatorConfig, AggregatorDaemon, AggregatorRegistry,
};
use net::adapter::net::behavior::fold::{
    capability::CapabilityFold, reservation::ReservationFold, FoldKind,
};
use net::adapter::net::behavior::lifecycle::LifecycleGroup;
use net::adapter::net::subnet::SubnetId;
use net::adapter::net::Visibility;

let config = AggregatorConfig {
    source_subnet: SubnetId::new(&[3, 7]),         // a fleet subnet
    summary_visibility: Visibility::ParentVisible, // visible at the region tier
    summary_targets: vec![],
    fold_kinds: vec![CapabilityFold::KIND_ID, ReservationFold::KIND_ID],
    summary_interval: Duration::from_secs(30),
    custom_summarizers: Default::default(),
};

// Three replicas, each an `AggregatorDaemon` bound to the live mesh.
// The factory returns an `Arc<AggregatorDaemon>` per replica index.
let group = LifecycleGroup::spawn(3, group_seed, {
    let mesh = mesh.clone();
    move |_index| {
        Arc::new(
            AggregatorDaemon::new(config.clone(), mesh.clone())
                .expect("valid aggregator config"),
        )
    }
}).await?;

// Hand the group to a registry installed on the node via
// `mesh.set_aggregator_registry(...)` (called before `MeshNode::start`).
let registry = Arc::new(AggregatorRegistry::new());
registry.register("fleet-west", group)?;
```

Each replica publishes summaries independently — no election machinery, no leader. Subscribers see N summary announcements per cycle and the fold's merge picks the latest by generation. Operators can scale the group down to a single replica through the registry when availability isn't the constraint. State across re-placements rebuilds from incoming channel announcements + TTL refreshes within one TTL cycle.

For operators who don't want to embed the substrate in their own process, the `net-aggregator-daemon` binary boots from a TOML config, registers templates the operator can instantiate by name, defaults to auto-respawn-on-failure, and prints a single JSON bootstrap line on stdout (`{"node_id": …, "bound_addr": …, "public_key_hex": …}`) so tools that orchestrate it can find its address and pubkey without parsing logs.

```toml
# net-aggregator-daemon.toml
[[template]]
name = "fleet-summary"
source_subnet = [3, 7]
summary_visibility = "parent-visible"
fold_kinds = ["capability", "reservation"]
summary_interval = "30s"

[[group]]
template = "fleet-summary"
name = "fleet-west"
replicas = 3
```

The `aggregator.registry` RPC service lets any node enumerate, spawn, scale, and unregister aggregator groups on any other node. The CLI exposes `net aggregator spawn / scale / ls / query --remote --node-addr <ip:port> --node-pubkey <hex>` for operating against a live daemon over the wire.

## Replica groups

A `ReplicaGroup` runs N copies of the same daemon across the mesh, with load-balanced routing to whichever replica is closest or least loaded. Replica identities are deterministic — they're derived from a group seed plus an index — so a failed replica re-spawns with the same identity on a different node without coordination:

```rust
use net_sdk::compute::DaemonRuntime;
use net_sdk::groups::{ReplicaGroup, ReplicaGroupConfig};
use net_sdk::DaemonHostConfig;
use net::adapter::net::behavior::loadbalance::Strategy;

// A `kind` is a named daemon factory registered once on the runtime.
runtime.register_factory("worker", || Box::new(StatelessWorker::new()))?;

let group = ReplicaGroup::spawn(&runtime, "worker", ReplicaGroupConfig {
    replica_count: 5,
    group_seed: [0u8; 32],
    lb_strategy: Strategy::LeastConnections,
    host_config: DaemonHostConfig::default(),
})?;
```

Replica groups are for stateless daemons (or daemons whose state is externally partitioned by key and routed accordingly). They scale horizontally with no consensus and no shared state. Identity is deterministic; recovery is just re-derivation.

## Standby groups

For *stateful* daemons that need fault tolerance, `StandbyGroup` runs one active and N − 1 passive copies. The active processes events; the standbys hold readiness to promote. Sync is snapshot-based: the active periodically snapshots, the standby applies the snapshot, and on failure the standby that's furthest along replays the gap of buffered events and promotes:

```rust
use net_sdk::groups::{StandbyGroup, StandbyGroupConfig};
use net_sdk::DaemonHostConfig;

runtime.register_factory("stateful", || Box::new(StatefulDaemon::new()))?;

let group = StandbyGroup::spawn(&runtime, "stateful", StandbyGroupConfig {
    member_count: 3,
    group_seed: [0u8; 32],
    host_config: DaemonHostConfig::default(),
})?;
```

The protocol gives you the bookkeeping — active/standby tracking, event buffering, snapshot transfer, promotion on failure. Your daemon supplies the state via `snapshot()` and `restore()`; the runtime does the rest.

A standby group costs you N replicas worth of memory and one replica worth of compute. Use it when the daemon's state is expensive to rebuild and you can't tolerate the time a full re-derivation would take on recovery.

## Fork groups

A `ForkGroup` creates N independent entities from a common parent at a specific causal point. Forks are not replicas — each fork has its own identity and its own chain — but they share a verifiable lineage to the parent. The use case is fan-out where you want each branch to evolve independently: A/B experiments, divergent training runs, multi-strategy execution:

```rust
use net_sdk::groups::{ForkGroup, ForkGroupConfig};
use net_sdk::DaemonHostConfig;
use net::adapter::net::behavior::loadbalance::Strategy;

runtime.register_factory("strategy", || Box::new(StrategyDaemon::new()))?;

let group = ForkGroup::fork(&runtime, "strategy", parent_origin, fork_seq, ForkGroupConfig {
    fork_count: 3,
    lb_strategy: Strategy::RoundRobin,
    host_config: DaemonHostConfig::default(),
})?;
```

Each fork records its lineage in a `ForkRecord` carrying a verifiable sentinel hash; any node on the mesh can verify the fork is legitimate. The forks themselves are normal daemons, registered in the daemon registry, addressable by their own identities.

| Pattern        | Identity                              | Routing            | State    | Recovery                                  |
|----------------|---------------------------------------|--------------------|----------|-------------------------------------------|
| Single daemon  | One ed25519 key                       | Direct             | Either   | Migration if alive; replay from snapshot |
| Replica group  | Deterministic from seed + index       | Load-balanced     | Stateless| Re-derive on a new node                   |
| Standby group  | Deterministic from seed; active flag  | Always to active  | Stateful | Standby promotes + replays buffer        |
| Fork group     | Random per fork, stored for recovery  | Per-fork direct   | Either   | Re-spawn from stored secret               |
| Lifecycle group| As per the inner `LifecycleDaemon`    | Per replica       | Either   | `HealthMonitor` respawn via factory       |

## Capability-aware daemons

The capability system extends to daemons. A daemon advertises its required and optional capability sets as part of its identity-bound announcement, and the runtime uses that advertisement to drive placement and discovery:

```rust
use net::adapter::net::behavior::{CapabilityFilter, CapabilitySet};

impl MeshDaemon for InferenceDaemon {
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::new()
            .require_gpu()
            .with_min_vram(16)          // hard floor
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("role:inference")
            .add_tag("tier:production")
    }

    fn optional_capabilities(&self) -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("hardware.vram_gb=24") // preferred, not required
    }
    // ... other methods ...
}
```

Required capabilities are placement-hard: no match, no placement. Optional capabilities are placement-soft: the scheduler prefers nodes that have them but will fall back if it has to. This is the right shape for "I really want a GPU with 24+ GB of VRAM, but I can run on 16 GB if I have to."

## When to use a daemon

Daemons are the right primitive when the work is **long-running**, **stateful in a way that benefits from running on one specific node**, **driven by events**, and **needs to survive failures**. If any of those four are false, simpler primitives apply: a one-shot job for short work, a stateless service for stateless work, a polling consumer for non-event-driven work, a basic bus subscriber for work that doesn't need failure handling.

For work that fits the daemon shape, the runtime is designed so that you stop thinking about placement and failure handling and start thinking about what your daemon actually does. That's the system working as intended.
