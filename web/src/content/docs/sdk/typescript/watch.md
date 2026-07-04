# TypeScript — Watch the Event Stream

Invoking gets you one result; watching gets you the ongoing facts. This is the
"observe" half of the loop — what lets an agent recover from a partial failure
instead of trusting a single return value
([Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).

## Subscribe to typed events

`subscribeTyped` returns an async iterable — decode each event into your type as it
arrives:

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

Subscriptions are **hot**: you see events emitted *after* you subscribe (plus
whatever is still in the ring buffer), not the whole history. There's no
replay-from-the-beginning on the bus — that's a durability decision (RedEX / an
adapter), covered in [Durable Logs](/docs/guides/durable-logs).

`subscribe({ limit })` gives the raw events; `subscribeTyped<T>()` decodes each
into `T`. For a one-shot batch instead of a live loop, `await node.poll({ limit })`
returns what's currently available.

## Location transparency

The bus is location-transparent — the same subscribe code works whether the
publisher is in-process or several hops away on the mesh. The concepts are in
[Channels](/docs/concepts/channels) and
[Events and Causality](/docs/concepts/events-and-causality).

## Next

[Artifacts](/docs/sdk/typescript/artifacts) — when the event is too big for the bus.
