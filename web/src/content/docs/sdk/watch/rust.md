## Watch it — Rust

### Subscribe to typed events

```rust
use net_sdk::{Net, stream::SubscribeOpts};
use futures::StreamExt;   // brings `.next()` onto the stream

#[derive(serde::Deserialize, Debug)]
struct TemperatureReading { sensor_id: String, celsius: f64 }

let node = Net::builder().memory().build().await?;

let mut stream = node.subscribe_typed::<TemperatureReading>(SubscribeOpts::default());
while let Some(reading) = stream.next().await {
    if reading.celsius > 80.0 {
        println!("HOT: {} at {:.1}C", reading.sensor_id, reading.celsius);
    }
}
```

`subscribe` returns an untyped `EventStream` if you would rather decode yourself;
`subscribe_typed::<T>` decodes each event into `T`.

You need `futures::StreamExt` in scope for `.next()`. Without it the stream
compiles and has no methods, which reads as the type being wrong.

### Cross-node channels

```rust
mesh.subscribe_channel(publisher_node_id, &channel).await?;
let events = mesh.recv(64).await?;          // Vec<StoredEvent>
```

`recv(limit)` polls a batch; `recv_shard(shard_id, limit)` narrows to one shard.
Note that the mesh side is batch-oriented even though the bus side is a stream —
the two surfaces are not the same shape.

### Verify it worked

```rust
let stats = node.stats();
println!("consumed against {} ingested", stats.events_ingested);
assert!(stats.events_ingested > 0, "nothing was ever accepted to watch");
```

If the loop above never yields, check the transport before the subscription: on
the default memory transport events are counted and discarded, so a program that
emits and then subscribes waits forever for something already gone. That is the
first-run surprise described in [Quickstart](/docs/sdk/rust/quickstart).

Next: [Move artifacts](/docs/sdk/rust/artifacts).
