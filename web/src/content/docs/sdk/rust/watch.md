# Rust — Watch the Event Stream

Invoking gets you one result. Watching gets you the ongoing facts — the events the
work emits as it happens. This is the "observe" half of the agent loop, and it's
what lets you recover from a partial failure instead of trusting a single return
value ([Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).

## Subscribe to typed events

On a `Net` node, subscribe for a type and consume the stream:

```rust
use net_sdk::{Net, stream::SubscribeOpts};

#[derive(serde::Deserialize, Debug)]
struct TemperatureReading { sensor_id: String, celsius: f64 }

let node = Net::builder().memory().build().await?;

let mut stream = node.subscribe_typed::<TemperatureReading>(SubscribeOpts::default());
while let Some(reading) = stream.next().await {
    // each item is a decoded TemperatureReading emitted after you subscribed
    if reading.celsius > 80.0 {
        println!("HOT: {} at {:.1}C", reading.sensor_id, reading.celsius);
    }
}
```

Subscriptions are **hot**: you see events emitted *after* you subscribe (plus
whatever is still in the ring buffer), not the whole history. There is no
replay-from-the-beginning on the bus — that's a durability decision (RedEX / an
adapter), covered in [Durable Logs](/docs/guides/durable-logs).

`subscribe` (untyped `EventStream`) gives you the raw events if you'd rather
decode yourself; `subscribe_typed::<T>` decodes each event into `T` for you.
`SubscribeOpts::default().poll_interval(…)` tunes the receive cadence.

## Cross-node channels

Between mesh nodes, a subscriber joins a named channel by the publisher's node id,
and the publisher fans out to its roster:

```rust
// subscriber side (Mesh node)
mesh.subscribe_channel(publisher_node_id, &channel).await?;
let events = mesh.recv(64).await?;         // poll a batch of StoredEvent
```

The bus is location-transparent — the same subscribe/consume code works whether the
publisher is in-process or several hops away. The concepts are in
[Channels](/docs/concepts/channels) and [Events and Causality](/docs/concepts/events-and-causality).
