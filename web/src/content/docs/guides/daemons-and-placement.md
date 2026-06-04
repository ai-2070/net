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
        CapabilityFilter::any()  // No special placement needs
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
            .map_err(|_| DaemonError::InvalidSnapshot)?;
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

A daemon runs inside a `DaemonHost`. You construct the host, the runtime places it, and the host calls `process()` for each event that arrives:

```rust
use net::adapter::net::compute::DaemonHost;

let host = DaemonHost::new(CounterDaemon { count: 0 }, daemon_keypair, host_config);
let registration = mesh.register_daemon(host).await?;
```

`register_daemon` does three things: it queries the placement scheduler for a target node, ships the daemon there if the target isn't this node, and registers the resulting `DaemonHost` in the local `DaemonRegistry`. The `registration` handle is your reference to the running daemon — drop it (or call `stop()`) to take the daemon down.

## Placement

Where a daemon ends up running is decided by the placement scheduler. The scheduler reads the daemon's `requirements()`, queries the mesh's capability fold for matching nodes, and scores each candidate. The default scorer combines five axes:

- **Capability match.** Does the node satisfy the filter? Hard veto if not.
- **Load.** How many other daemons is the node running, and how much spare capacity does it have?
- **Anti-affinity.** Avoid placing replicas of the same daemon on the same node.
- **Resource fit.** Prefer nodes with closer-matching resource availability — don't waste a GPU node on a CPU-bound daemon if a CPU node is available.
- **Proximity.** Prefer nodes physically near the daemon's input traffic.

Each axis is weighted; the highest-scoring candidate wins.

### Custom placement filters

For requirements the built-in axes don't capture, plug in a custom predicate. The runtime calls back per candidate with the candidate's tags and metadata; you return a verdict:

```rust
use net_sdk::placement_filter_from_fn;

let custom = placement_filter_from_fn("gpu-vram-fits", |cand| {
    let vram = cand.metadata.get("hardware.vram_gb")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    vram >= 24
});

let placement = StandardPlacement::with_custom_filter_id("gpu-vram-fits");
```

Custom filters compose with the built-in axes — your predicate's verdict is one input to the score, not a hard override. The same callback shape exists in every binding (Rust, TS, Python, Go, C), and predicates built through the substrate's `Predicate` AST evaluate identically across all of them, so a placement filter written in TS produces the same verdict as one written in Rust.

## Async daemons — the `LifecycleDaemon` trait

Some daemons need an async loop: they publish on a periodic timer, call out to other services, or pump a long-running operation alongside event processing. The `MeshDaemon` trait is synchronous-by-design for WASM compatibility, so for those cases the runtime provides an async sibling: `LifecycleDaemon`.

```rust
use net::adapter::net::compute::lifecycle::{LifecycleDaemon, LifecycleHandle};

#[async_trait::async_trait]
impl LifecycleDaemon for HealthScraper {
    fn requirements(&self) -> CapabilityFilter { CapabilityFilter::any() }

    async fn on_start(&mut self, ctx: &LifecycleContext) -> Result<(), DaemonError> {
        self.client = HttpClient::connect(&self.endpoint).await?;
        Ok(())
    }

    async fn tick(&mut self, ctx: &LifecycleContext) -> Result<(), DaemonError> {
        let metrics = self.client.fetch_metrics().await?;
        ctx.publish("metrics/scraped", metrics).await?;
        Ok(())
    }

    async fn on_stop(&mut self, _: &LifecycleContext) -> Result<(), DaemonError> {
        self.client.shutdown().await.ok();
        Ok(())
    }
}

let handle: LifecycleHandle = LifecycleHandle::start(HealthScraper::new(endpoint)).await?;
```

`LifecycleHandle` is an RAII wrapper that owns the tokio loop; dropping it stops the daemon cleanly. The tick loop checks an internal shutdown flag between iterations so a long-running `tick().await` doesn't get its task dropped mid-flight by the backstop timeout. Combined with `LifecycleGroup<L>` (an N-replica primitive generic over the lifecycle daemon), the same handle drives both single-daemon and replicated-daemon deployments.

When a `LifecycleGroup` is registered with a `HealthMonitor`, the monitor watches per-replica health and re-spawns failed replicas via the group's factory closure with exponential backoff. `register_with_monitor` is the one-call constructor that wires the registry and the monitor together — operators don't thread them by hand.

## Aggregator daemons

The canonical use of `LifecycleDaemon` is the aggregator: a daemon that sits one tier up from a source subnet, subscribes to that subnet's detail channels through the gateway, summarizes what it sees, and publishes the summary upward. The substrate ships an `AggregatorDaemon` implementation plus an `AggregatorRegistry` that lives on `MeshNode` alongside `DaemonRegistry`.

```rust
use net::adapter::net::compute::aggregator::{AggregatorConfig, AggregatorDaemon};

let config = AggregatorConfig {
    source_subnet: SubnetId::new(&[3, 7]),         // a fleet subnet
    summary_visibility: Visibility::ParentVisible, // visible at the region tier
    summary_targets: vec![],
    fold_kinds: vec![FOLD_CAPABILITY, FOLD_RESERVATION],
    summary_interval: Duration::from_secs(30),
    custom_summarizers: vec![],
};

let group = LifecycleGroup::with_factor(3, move || {
    AggregatorDaemon::new(config.clone())
});

mesh.register_aggregator_group(group).await?;
```

Each replica publishes summaries independently — no election machinery, no leader. Subscribers see N summary announcements per cycle and the fold's merge picks the latest by generation. Operators can `scale_to(1)` when availability isn't the constraint. State across re-placements rebuilds from incoming channel announcements + TTL refreshes within one TTL cycle.

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
use net::adapter::net::compute::{ReplicaGroup, LoadBalancer};

let group = ReplicaGroup::new(group_id, group_seed)
    .with_factor(5)
    .with_load_balancer(LoadBalancer::least_connections())
    .with_daemon_factory(|| StatelessWorker::new());

let registration = mesh.register_replica_group(group).await?;
```

Replica groups are for stateless daemons (or daemons whose state is externally partitioned by key and routed accordingly). They scale horizontally with no consensus and no shared state. Identity is deterministic; recovery is just re-derivation.

## Standby groups

For *stateful* daemons that need fault tolerance, `StandbyGroup` runs one active and N − 1 passive copies. The active processes events; the standbys hold readiness to promote. Sync is snapshot-based: the active periodically snapshots, the standby applies the snapshot, and on failure the standby that's furthest along replays the gap of buffered events and promotes:

```rust
let group = StandbyGroup::new(group_id)
    .with_members(3)
    .with_daemon_factory(|| StatefulDaemon::new());

let registration = mesh.register_standby_group(group).await?;
```

The protocol gives you the bookkeeping — active/standby tracking, event buffering, snapshot transfer, promotion on failure. Your daemon supplies the state via `snapshot()` and `restore()`; the runtime does the rest.

A standby group costs you N replicas worth of memory and one replica worth of compute. Use it when the daemon's state is expensive to rebuild and you can't tolerate the time a full re-derivation would take on recovery.

## Fork groups

A `ForkGroup` creates N independent entities from a common parent at a specific causal point. Forks are not replicas — each fork has its own identity and its own chain — but they share a verifiable lineage to the parent. The use case is fan-out where you want each branch to evolve independently: A/B experiments, divergent training runs, multi-strategy execution:

```rust
let group = ForkGroup::from_parent(parent_origin, fork_seq)
    .with_count(3)
    .with_daemon_factory(|| StrategyDaemon::new());

let registration = mesh.register_fork_group(group).await?;
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
impl MeshDaemon for InferenceDaemon {
    fn requirements(&self) -> CapabilityFilter {
        filter![ "hardware.gpu", "software.cuda >= 12" ]
    }

    fn required_capabilities(&self) -> CapabilitySet {
        cap_set![ "role:inference", "tier:production" ]
    }

    fn optional_capabilities(&self) -> CapabilitySet {
        cap_set![ "hardware.vram_gb >= 24" ]
    }
    // ... other methods ...
}
```

Required capabilities are placement-hard: no match, no placement. Optional capabilities are placement-soft: the scheduler prefers nodes that have them but will fall back if it has to. This is the right shape for "I really want a GPU with 24+ GB of VRAM, but I can run on 16 GB if I have to."

## When to use a daemon

Daemons are the right primitive when the work is **long-running**, **stateful in a way that benefits from running on one specific node**, **driven by events**, and **needs to survive failures**. If any of those four are false, simpler primitives apply: a one-shot job for short work, a stateless service for stateless work, a polling consumer for non-event-driven work, a basic bus subscriber for work that doesn't need failure handling.

For work that fits the daemon shape, the runtime is designed so that you stop thinking about placement and failure handling and start thinking about what your daemon actually does. That's the system working as intended.
