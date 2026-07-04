# TypeScript — Quickstart

```bash
npm install @net-mesh/sdk @net-mesh/core
```

## A node that emits events

```typescript
import { NetNode } from '@net-mesh/sdk';

const node = await NetNode.create({ shards: 4 });   // in-process bus node

// Emit structured events (serialized as JSON).
node.emit({ sensor: 'lidar', range_m: 12.5 });
node.emitRaw('{"sensor":"radar","range_m":45.0}');
node.emitBatch([{ a: 1 }, { a: 2 }, { a: 3 }]);

await node.flush();

const stats = node.stats();
console.log(`${stats.eventsIngested} ingested, ${stats.eventsDropped} dropped`);

await node.shutdown();   // explicit — Node finalizers are non-deterministic
```

`emit` returns synchronously once the event is accepted into the local ring
buffer — confirmation of acceptance, not delivery (see
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)). Under
backpressure events can drop; check `stats().eventsDropped`.

## Consume what you emit

```typescript
for await (const event of node.subscribe({ limit: 100 })) {
  console.log('event', event);
}
```

## The mesh node

For the agentic surface — capabilities, tools, nRPC — create a `MeshNode` instead:

```typescript
import { MeshNode } from '@net-mesh/sdk';

const psk = new Uint8Array(32).fill(0x42);           // both peers share the same PSK
const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });
```

A `MeshNode` speaks encrypted UDP to peers and carries capabilities and nRPC. From
here the loop is [Announce](/docs/sdk/typescript/announce) →
[Discover](/docs/sdk/typescript/discover) → [Invoke](/docs/sdk/typescript/invoke).
