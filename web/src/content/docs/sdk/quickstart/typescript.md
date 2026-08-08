## Build it — TypeScript

```bash
npm install @net-mesh/sdk @net-mesh/core
```

Install both. `@net-mesh/sdk` is the ergonomic layer; `@net-mesh/core` is the
native binding it sits on, and several surfaces on this spine are reached only
through `core`. Pin them to the same version.

### A bus node

```typescript
import { NetNode } from '@net-mesh/sdk';

const node = await NetNode.create({ shards: 4 });

node.emit({ sensor: 'lidar', range_m: 12.5 });
node.emitRaw('{"sensor":"radar","range_m":45.0}');
node.emitBatch([{ a: 1 }, { a: 2 }, { a: 3 }]);

await node.flush();
await node.shutdown();   // explicit — Node finalizers are non-deterministic
```

`emit` returns synchronously with a `Receipt`, or **throws**. A drop under
backpressure reaches you as a thrown error, not as a `null` return — the native
`ingestRawSync` returns an error and the wrapper dereferences it unconditionally.
A producer that neither catches nor reads `stats().eventsDropped` is silently
lossy. Use `fire()` when you genuinely want fire-and-forget.

`shutdown()` is not optional housekeeping. Node's finalizers run at a time nobody
promises, so a process that exits without it can lose whatever the drain worker
was still holding.

### A mesh node

```typescript
import { MeshNode } from '@net-mesh/sdk';

const psk = new Uint8Array(32).fill(0x42);   // raw bytes, length 32
const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });
```

TypeScript takes the PSK as a 32-byte `Uint8Array`. A wrong length fails when the
node is created, not when the first peer disagrees with you.

### The handshake

```typescript
const HOST_ADDR = '127.0.0.1:9001';
const host = await MeshNode.create({ bindAddr: HOST_ADDR, psk });
const agent = await MeshNode.create({ bindAddr: '127.0.0.1:9000', psk });

await host.accept(agent.nodeId());
await agent.connect(HOST_ADDR, host.publicKey(), host.nodeId());
await host.start();
await agent.start();
```

Two things the shape of that snippet is telling you. **`start()` is async in
TypeScript** — forgetting the `await` gives you a node that is not started yet and
no error saying so. And **there is no `localAddr()` accessor**: the address you
bound is the address you pass, so bind to a port you chose rather than `:0` when a
second node has to reach you.

Node ids are 64-bit, so they come back as `bigint` rather than `number`. That
matters the moment you use one as an object key or compare it with `===` against a
literal — `1n === 1` is `false`.

### Verify it worked

```typescript
const stats = node.stats();
if (stats.eventsIngested !== 1) throw new Error('the bus did not accept the event');
console.log(`accepted: ingested=${stats.eventsIngested}`);
```

Expect one line, `accepted: ingested=1`, and a clean exit. The counter is a
producer-side count: accepted, not received and not stored.

Next: [Announce a capability](/docs/sdk/typescript/announce).
