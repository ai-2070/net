# A Distributed Daemon That Survives Node Failures

This tutorial walks through building a stateful daemon that runs across a standby group, with an active node processing events and passive replicas ready to take over when the active fails. By the end you'll have a working daemon, a working standby group, and a worked example of what failover actually looks like end to end.

The daemon we'll build is a small one — it tracks running tasks and emits state-change events as tasks transition through their lifecycle — but the pattern is the same one you'd use for any stateful work that needs fault tolerance: long-running ML inference, transaction processors, simulation engines, control planes.

## The shape

```
┌───────────────────────────────────────────────────┐
│              StandbyGroup (3 members)             │
│                                                   │
│  ┌──────────────┐  ┌──────────────┐ ┌──────────────┐ │
│  │   Active     │  │   Standby    │ │   Standby    │ │
│  │  (node A)    │→ │  (node B)    │ │  (node C)    │ │
│  │              │  │              │ │              │ │
│  │  Processing  │  │  Synced to   │ │  Synced to   │ │
│  │  events      │  │  seq=100,    │ │  seq=100,    │ │
│  │  at seq=103  │  │  idle        │ │  idle        │ │
│  └──────┬───────┘  └──────────────┘ └──────────────┘ │
│         │                                            │
│         │  Events buffered for replay on promotion   │
│         ▼                                            │
│  ┌─────────────────────────────────────────────────┐ │
│  │  Event buffer: [101, 102, 103, ...]             │ │
│  └─────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────┘
```

The active processes events. The standbys hold state synced to the last snapshot. The group buffers events the active has processed since the last sync. When the active dies, the standby with the highest sync point promotes, replays the buffered events, and continues.

## The daemon

A `MeshDaemon` is a Rust trait you implement against your own state. Here's a daemon that tracks tasks:

```rust
use net::adapter::net::compute::{MeshDaemon, DaemonError};
use net::adapter::net::behavior::CapabilityFilter;
use net::adapter::net::state::CausalEvent;
use bytes::Bytes;
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

#[derive(Default, Serialize, Deserialize)]
struct TaskTracker {
    tasks: HashMap<u64, TaskState>,
    next_id: u64,
}

#[derive(Serialize, Deserialize, Clone)]
enum TaskState {
    Pending,
    Running { worker: String, started_at: u64 },
    Completed { duration_ms: u64 },
    Failed { reason: String },
}

#[derive(Serialize, Deserialize)]
enum TaskCommand {
    Create { task_id: u64 },
    Start  { task_id: u64, worker: String },
    Finish { task_id: u64, result: TaskResult },
}

impl MeshDaemon for TaskTracker {
    fn name(&self) -> &str { "task-tracker" }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::any()
    }

    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        let cmd: TaskCommand = serde_json::from_slice(&event.payload)
            .map_err(|e| DaemonError::InvalidPayload(e.to_string()))?;

        let output = match cmd {
            TaskCommand::Create { task_id } => {
                self.tasks.insert(task_id, TaskState::Pending);
                serde_json::to_vec(&TaskEvent::Created { task_id })?
            }
            TaskCommand::Start { task_id, worker } => {
                let state = TaskState::Running { worker: worker.clone(), started_at: now_ms() };
                self.tasks.insert(task_id, state);
                serde_json::to_vec(&TaskEvent::Started { task_id, worker })?
            }
            TaskCommand::Finish { task_id, result } => {
                let final_state = match &result {
                    TaskResult::Ok { duration_ms } => TaskState::Completed { duration_ms: *duration_ms },
                    TaskResult::Err { reason }     => TaskState::Failed   { reason: reason.clone() },
                };
                self.tasks.insert(task_id, final_state);
                serde_json::to_vec(&TaskEvent::Finished { task_id, result })?
            }
        };

        Ok(vec![Bytes::from(output)])
    }

    fn snapshot(&self) -> Option<Bytes> {
        Some(Bytes::from(serde_json::to_vec(self).expect("serialization is infallible")))
    }

    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
        *self = serde_json::from_slice(&state)
            .map_err(|e| DaemonError::InvalidSnapshot(e.to_string()))?;
        Ok(())
    }
}
```

A few things worth noting:

- The daemon is small. The hard parts (causal-chain tracking, output framing, snapshot transport) are the runtime's job.
- `process()` is synchronous and fast. If you need to do heavy work, dispatch it to a background task and have the daemon consume the result on a later event.
- `snapshot()` and `restore()` are the migration primitives. The runtime calls them when it needs to move the daemon — including when promoting a standby.

## Building the standby group

A `StandbyGroup` registers the daemon for one active and N−1 standbys:

```rust
use net::adapter::net::compute::StandbyGroup;

let group = StandbyGroup::new(0xAAAA_BBBB)
    .with_members(3)
    .with_daemon_factory(|| TaskTracker::default());

let registration = mesh.register_standby_group(group).await?;
```

The factory closure is called once per member to construct a fresh daemon instance. Each instance gets its own deterministic keypair derived from the group seed + index; on failure recovery, the same index gets the same keypair, so peers don't notice the move beyond the latency of the promotion.

The runtime places the members across the mesh based on the daemon's `requirements()` and the placement scheduler's load balancing. By default it spreads them across distinct nodes (anti-affinity is one of the scoring axes); for stricter placement you can plug in a custom placement filter.

## What happens when the active fails

The failure detector watches every peer. When the active node's heartbeats stop for `3 × heartbeat_ms` (default 1.5 seconds), the runtime declares the active dead. The standby group's coordinator picks the standby with the highest `synced_through` and promotes it.

Promotion is three steps:

1. **Apply the most recent snapshot.** The promoting standby already has a snapshot applied at the last sync point; this step is a no-op if the snapshot is current.
2. **Replay the buffered events.** The group keeps a buffer of events the active processed since the last sync. The promoting standby replays them in strict sequence order — same mechanism the runtime uses for daemon migration.
3. **Activate.** The standby takes the active role. Routing flips to send new events to it. The daemon's `origin_hash` is unchanged (deterministic identity from the group seed), so peers routing to it don't see a different destination.

In application code, none of this is visible. Calls to the daemon's `origin_hash` keep working; events keep flowing; the dashboard might see a brief gap (bounded by the time it takes to replay the buffer) before activity resumes.

## Driving the demonstration

To actually see this work end to end, you need three nodes (locally that's three processes), a producer of task commands, and a way to watch the state. The producer:

```rust
let bus = EventBus::new(EventBusConfig::builder()
    .adapter(AdapterConfig::net().peer("node-a.local:7777"))
    .build()?).await?;

loop {
    let task_id = next_task_id();

    bus.ingest(Event::new(serde_json::to_value(TaskCommand::Create { task_id })?))?;
    sleep(Duration::from_millis(100)).await;

    bus.ingest(Event::new(serde_json::to_value(TaskCommand::Start {
        task_id, worker: "worker-1".into(),
    })?))?;
    sleep(Duration::from_millis(200)).await;

    bus.ingest(Event::new(serde_json::to_value(TaskCommand::Finish {
        task_id, result: TaskResult::Ok { duration_ms: 200 },
    })?))?;
}
```

The watcher (on another node):

```rust
let mut state_stream = mesh.watch_daemon_state::<TaskTracker>(daemon_origin).await?;

while let Some(state) = state_stream.next().await {
    println!("Active tasks: {}", state.tasks.iter()
        .filter(|(_, s)| matches!(s, TaskState::Running { .. }))
        .count());
}
```

Now kill the active node (Ctrl-C on its process). What you'll see:

- The watcher's stream pauses briefly — usually a second or two — as the runtime detects the failure and the standby promotes.
- The producer keeps producing; events are buffered into the channel and replayed once the new active is ready.
- The watcher resumes; the state has every event the producer sent, in order.

Total downtime in a typical run: under three seconds. No events lost. No client code knowing the move happened.

## Tuning the trade-off

Standby groups are not free. Each standby costs you memory for the snapshot but does no compute. The trade-offs are:

- **More standbys** = higher availability (more nodes to lose before the group dies) but more memory and more sync bandwidth.
- **Faster sync cadence** = smaller replay gap on promotion (less time spent recovering) but more snapshot bandwidth.
- **Faster heartbeat** = faster failover (shorter detection window) but more wire traffic.

Defaults are tuned for the typical case: three members, 500 ms heartbeat (1.5 s detection), sync on operator demand or every minute. If your workload's recovery target is sub-second, tighten the heartbeat to 100 ms and sync every 10 seconds; you'll see a measurable drop in event throughput but the recovery becomes nearly invisible.

## What this gets you

The model is composable. A standby group of stateful daemons can run on a fleet where most nodes are also doing other work — the daemon doesn't take a node out of the rotation, it just shares it. Multiple standby groups can run on the same fleet without coordinating with each other. The daemon registry is a local primitive on each node; cross-group coordination happens through the capability index and the failure detector, both of which are already there.

What you've built here is the substrate for stateful microservices that don't go down when a node does. The architecture isn't novel — active-passive is decades old — but the runtime's job here is to make it work without you wiring it up by hand: identity through the keypair model, state transport through the snapshot mechanism, routing through the daemon registry, failure detection through the existing peer monitoring. You wrote a `MeshDaemon` impl and registered a group; the runtime did everything else.
