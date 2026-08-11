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

const psk = '42'.repeat(32);   // 64 hex characters = 32 bytes
const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });
```

TypeScript takes the PSK as a **64-character hex string**, the same
representation Python uses — not the raw `Uint8Array` Rust takes. Passing bytes
fails twice over, at compile time and again at the native boundary:

```text
error TS2322: Type 'Uint8Array<ArrayBuffer>' is not assignable to type 'string'.

Error: Failed to convert JavaScript value `Object {...}` into rust type `String`
  on MeshOptions.psk { code: 'StringExpected' }
```

A wrong length fails when the node is created, not when the first peer
disagrees with you.

### The handshake

```typescript
const HOST_ADDR = '127.0.0.1:9001';
const host = await MeshNode.create({ bindAddr: HOST_ADDR, psk });
const agent = await MeshNode.create({ bindAddr: '127.0.0.1:9000', psk });

// Start the responder, then await it — do NOT await it on the line above.
const accepted = host.accept(agent.nodeId());
await agent.connect(HOST_ADDR, host.publicKey(), host.nodeId());
await accepted;

await host.start();
await agent.start();
```

**The missing `await` on that first line is the whole point.** `accept()`
resolves only once an initiator has connected, so `await host.accept(...)`
followed by `agent.connect(...)` never reaches the second line: the handshake
needs both halves in flight at once. Calling `accept` without awaiting it
starts the responder and hands you the promise to settle after `connect`,
which is what `tokio::join!` does on the Rust page and what a second thread
does on the Python one.

Two more things the shape of that snippet is telling you. **`start()` is async in
TypeScript** — forgetting the `await` gives you a node that is not started yet and
no error saying so. And **`localAddr()` is how a `:0` bind becomes connectable**:
port `0` asks the OS to pick a free port, and the port it picked is knowable only
by asking the node back.

```typescript
const host = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });
const agent = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });

const hostAddr = host.localAddr();   // e.g. '127.0.0.1:54417'

const accepted = host.accept(agent.nodeId());
await agent.connect(hostAddr, host.publicKey(), host.nodeId());
await accepted;
```

Hard-coding the port, as the snippet above it does, is fine when you chose the
number and both sides already agree on it. `:0` plus `localAddr()` is what you
want when you did not — in tests, and anywhere a fixed port would collide.

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
