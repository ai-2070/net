# Fleet Telemetry

This tutorial walks through a complete deployment: a fleet of edge nodes (think delivery vehicles, sensor stations, kiosks) publishing telemetry to a central operations cluster, with subnet boundaries keeping each vehicle's internal traffic isolated from every other vehicle, and a fold in the operations cluster materializing aggregate metrics for an operator dashboard.

By the end you'll have a working three-tier deployment — edge devices, a regional gateway, an operations cluster — with the channel hierarchy, subnet rules, and fold logic that ties them together. The architecture is one any production fleet deployment would use; the code is small enough to follow in one sitting.

## The shape

```
┌─────────────────────────────────────────────┐
│       Operations cluster (region: ops)      │
│  ┌──────────────────┐ ┌──────────────────┐  │
│  │ Telemetry fold   │ │ Dashboard reader │  │
│  │ (CortEX adapter) │ │ (watcher stream) │  │
│  └─────────┬────────┘ └─────────┬────────┘  │
└────────────┼─────────────────────┼──────────┘
             │                     │
        ┌────┴─────────────────────┴────┐
        │  Regional gateway (subnet 3)  │
        │  Channel: telemetry/*         │
        │  Visibility: Exported → ops   │
        └────────────────┬──────────────┘
                         │
         ┌───────────────┼───────────────┐
         ▼               ▼               ▼
    ┌────────┐      ┌────────┐      ┌────────┐
    │ veh-1  │      │ veh-2  │      │ veh-3  │
    │ subnet │      │ subnet │      │ subnet │
    │ 3.7.1  │      │ 3.7.2  │      │ 3.7.3  │
    └────────┘      └────────┘      └────────┘
```

Three concepts do the work. Channels with hierarchical names carry events upward. Subnets keep each vehicle's internal traffic isolated. A CortEX fold materializes the aggregate view the operations cluster queries.

## Setting up the channel hierarchy

Each vehicle publishes to a channel rooted at its identity:

- `vehicles/v-001/telemetry/imu`
- `vehicles/v-001/telemetry/gps`
- `vehicles/v-001/telemetry/battery`

Internal vehicle channels (e.g. `vehicles/v-001/internal/diagnostics`) are configured `SubnetLocal` and never leave the vehicle. The telemetry channels are configured `Exported`, with the operations cluster's subnet (`region: ops`, `SubnetId::new(&[0])`) as the destination:

```rust
use net::adapter::net::channel::{ChannelConfig, ChannelName, Visibility};

let telemetry_cfg = ChannelConfig {
    channel_id: ChannelName::new("vehicles/v-001/telemetry/imu")?.id(),
    visibility: Visibility::Exported,
    publish_caps: Some(filter![ "role.vehicle" ]),
    subscribe_caps: Some(filter![ "role.operator", "tier.production" ]),
    require_token: true,
    priority: 4,
    reliable: false,
    max_rate_pps: Some(100),
};

mesh.register_channel(telemetry_cfg).await?;
```

The capability filter on `publish_caps` ensures only nodes advertising `role.vehicle` can publish; the filter on `subscribe_caps` ensures only operator nodes can subscribe. Permission tokens layer on top — each vehicle holds a token scoped to its own telemetry channels, and the operations cluster holds tokens scoped to the export destinations.

## Configuring subnets

Each vehicle is its own subnet at the third level of the hierarchy. The four-level subnet ID maps to (region, fleet, vehicle, subsystem):

```rust
use net::adapter::net::subnet::{SubnetId, SubnetPolicy, SubnetRule};

let vehicle_policy = SubnetPolicy {
    rules: vec![
        SubnetRule {
            match_tags: vec!["vehicle".into(), "fleet.west".into()],
            target_subnet: SubnetId::new(&[3, 7, 1]),  // region 3, fleet 7, vehicle 1
        },
    ],
    default_subnet: SubnetId::new(&[3, 7]),  // fleet root if no rule matches
};

mesh.set_subnet_policy(vehicle_policy);
```

The fleet's regional gateway sits at `SubnetId::new(&[3, 7])` and handles the export rules — telemetry channels marked `Exported` ride the export table to the operations subnet (`SubnetId::new(&[0])`), and everything else stops at the gateway.

The gateway's enforcement is header-only: it reads `subnet_id` and `channel_hash` from the packet header, looks up the channel's `Visibility`, and forwards or drops accordingly. No payload decryption, no per-flow state, no opportunity for an internal channel to leak.

## The vehicle's publisher

Each vehicle runs a small process that reads its sensors and publishes:

```rust
use net::{EventBus, EventBusConfig, Event, AdapterConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EventBusConfig::builder()
        .adapter(AdapterConfig::net()
            .listen("0.0.0.0:7777")
            .peer("gateway.fleet-west.local:7777"))
        .build()?;

    let bus = EventBus::new(config).await?;

    let mut interval = tokio::time::interval(Duration::from_millis(10));
    let mut sensors = open_sensors();

    loop {
        interval.tick().await;
        let reading = sensors.read();
        let event = Event::from_str(&serde_json::to_string(&reading)?)?;
        bus.ingest(event)?;
    }
}
```

The vehicle ingests onto its local bus; the `NetAdapter` ships events through the mesh to anyone subscribed. The gateway picks them up because it's the next-hop forwarder; subscribers in the operations cluster pick them up because they're the destination subnet.

## The operations fold

The operations cluster materializes a per-vehicle view from the incoming telemetry. A CortEX fold consumes the events and updates an aggregate state:

```rust
use net::adapter::net::cortex::{CortexAdapter, RedexFold, FoldError};
use net::adapter::net::state::CausalEvent;
use std::collections::HashMap;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct FleetState {
    vehicles: HashMap<String, VehicleSummary>,
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct VehicleSummary {
    last_imu:     ImuReading,
    last_gps:     GpsReading,
    battery_pct:  u8,
    health_score: f32,
    updated_at:   u64,
}

struct FleetFold;

impl RedexFold<FleetState> for FleetFold {
    fn apply(&self, state: &mut FleetState, event: &CausalEvent) -> Result<(), FoldError> {
        let parsed: VehicleEvent = serde_json::from_slice(&event.payload)
            .map_err(|e| FoldError::InvalidPayload(e.to_string()))?;
        let summary = state.vehicles.entry(parsed.vehicle_id).or_default();
        match parsed.kind {
            VehicleEventKind::Imu(r)     => summary.last_imu = r,
            VehicleEventKind::Gps(r)     => summary.last_gps = r,
            VehicleEventKind::Battery(p) => summary.battery_pct = p,
        }
        summary.updated_at = parsed.timestamp_ms;
        summary.health_score = compute_health(summary);
        Ok(())
    }
}
```

Open a CortEX adapter against the channel that catches all incoming telemetry (a RedEX log subscribed to `vehicles/*/telemetry/*`):

```rust
let fleet_adapter = CortexAdapter::open(
    &redex,
    "fleet-summary",
    operator_origin_hash,
    FleetFold,
).await?;
```

The fold task subscribes to the RedEX tail, applies events as they arrive, and persists the resulting state. Any operator-side reader can query the state:

```rust
let snapshot = fleet_adapter.state().read();
for (vehicle_id, summary) in &snapshot.vehicles {
    println!("{}: battery {}%, health {:.2}", vehicle_id, summary.battery_pct, summary.health_score);
}
```

## The dashboard

The operator's dashboard wants live updates, not polling. The CortEX watcher API gives them deltas as the fold updates:

```rust
use futures::StreamExt;

let mut stream = Box::pin(
    fleet_adapter
        .watch()
        .stream(),
);

while let Some(state) = stream.next().await {
    dashboard.render(&state);
}
```

The watcher emits the current state on subscribe, then dedupe-emits on every state change. The dashboard renders deterministically; the operator sees telemetry update in close to real time without writing a polling loop.

## Putting it together

The deployment has three roles, each with a small responsibility:

- **Vehicles** run a publisher process that reads sensors and ingests events. They're in their own subnet, they hold per-vehicle permission tokens, and their `SubnetLocal` channels never leave the vehicle's mesh.
- **Gateways** are configured with the export table for the fleet — they know which `Exported` channels can travel to which destination subnets, and they enforce the rules at packet-header speed.
- **Operations** runs the fold, the watchers, and the dashboard. It subscribes to telemetry on the export, materializes per-vehicle state through the fold, and exposes live views to operators.

Everything else falls out automatically. The mesh routes packets through the gateway because the gateway is the only path from the vehicle subnet to the operations subnet. The auth guards in the operations cluster reject any packet from an unauthorized origin because the operator's channel config required capability matching. The fold catches every event from every vehicle because it subscribes to a channel pattern, not to individual channels.

## Adding a new vehicle

The operational cost of adding a vehicle to the fleet is one provisioning step: give the new node a keypair, an `EntityKeypair`, a capability set including `role.vehicle` and `fleet.west`, and a token scoped to `vehicles/v-NNN/telemetry/*`. The subnet policy picks it up automatically (the rule matches on `vehicle` and `fleet.west`), the gateway picks up the new export automatically (channels marked `Exported` flow through to the operations subnet), and the operations fold picks up the new events automatically (it subscribes to the pattern, not to individual channels).

No central registry to update, no service-discovery layer to ping, no configuration push to schedule. The capability advertisement is enough.

## What this gives you

For a fleet deployment the size of this example, the runtime cost is small: one process per vehicle, one process per gateway, the operations cluster as you'd size it for the dashboard read load. The wire traffic per vehicle is bounded by the rate limit on each channel. The operational overhead is bounded by the subnet hierarchy — a problem in one vehicle's subnet doesn't propagate beyond the gateway, by construction.

The same shape scales up to hundreds of thousands of vehicles. Add more gateways (one per region, one per fleet-of-fleets); the subnet hierarchy already accommodates four levels. Add more fold instances in the operations cluster (a replica group running the same fold against the same RedEX log); the dashboard sees the union of their views.

This is what the Net architecture is built for. The primitives compose; you don't outgrow them as you scale.
