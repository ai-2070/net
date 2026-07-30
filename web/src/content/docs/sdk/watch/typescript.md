## Watch it — TypeScript

### Subscribe to typed events

```typescript
import { NetNode } from '@net-mesh/sdk';

interface TemperatureReading { sensorId: string; celsius: number }

const node = await NetNode.create({ shards: 4 });

for await (const reading of node.subscribeTyped<TemperatureReading>({ limit: 100 })) {
  if (reading.celsius > 80) {
    console.log(`HOT: ${reading.sensorId} at ${reading.celsius}C`);
  }
}
```

`subscribeTyped<T>` returns a `TypedEventStream<T>` — an async iterable that
deserializes each event for you. `subscribe({ limit })` gives the raw
`EventStream` instead.

`T` is a compile-time assertion, not a runtime check. The stream `JSON.parse`s and
casts; a payload that does not match your interface produces a wrong-shaped object
rather than an error. Validate at the boundary if the producer is not yours.

### One batch instead of a live loop

```typescript
const batch = await node.poll({ limit: 100 });
```

`poll` returns what is currently available and returns. `pollOne()` takes a single
event or `null`. Use these when you want a drain rather than a subscription — a
`for await` loop over `subscribeTyped` does not end on its own.

### Verify it worked

```typescript
const stats = node.stats();
console.log(`consumed against ${stats.eventsIngested} ingested`);
if (stats.eventsIngested === 0) throw new Error('nothing was ever accepted to watch');
```

If the loop never yields, check the transport before the subscription: the default
memory transport counts events and discards them, so emitting and then subscribing
waits forever for something already gone. See
[Quickstart](/docs/sdk/typescript/quickstart).

Next: [Move artifacts](/docs/sdk/typescript/artifacts).
