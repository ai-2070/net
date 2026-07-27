# Net TypeScript SDK

Ergonomic TypeScript SDK for the Net mesh — a latency-first encrypted mesh
where services and agents announce capabilities, discover each other, and
invoke work over typed RPC.

Wraps the `@net-mesh/core` NAPI bindings with streaming, typed channels, and a
developer-friendly API.

**Docs: <https://ai2070.net/docs/sdk/typescript/quickstart>** ·
[Concepts](https://ai2070.net/docs/concepts/architecture)

## Install

```bash
npm install @net-mesh/sdk @net-mesh/core
```

## Quickstart

```typescript
import { NetNode } from '@net-mesh/sdk';

const node = await NetNode.create({ shards: 4 });   // in-process bus node

node.emit({ sensor: 'lidar', range_m: 12.5 });
node.emitRaw('{"sensor":"radar","range_m":45.0}');
node.emitBatch([{ a: 1 }, { a: 2 }, { a: 3 }]);

await node.flush();

const stats = node.stats();
console.log(`${stats.eventsIngested} ingested, ${stats.eventsDropped} dropped`);

await node.shutdown();   // explicit — Node finalizers are non-deterministic
```

`emit` returns once the event is accepted into the local ring buffer —
acceptance, not delivery. Under backpressure events can drop; check
`stats().eventsDropped`. See
[Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

Consume what you emit:

```typescript
for await (const event of node.subscribe({ limit: 100 })) {
  console.log('event', event);
}
```

## The mesh node

For the agentic surface — capabilities, tools, nRPC — create a `MeshNode`:

```typescript
import { MeshNode } from '@net-mesh/sdk';

const psk = new Uint8Array(32).fill(0x42);   // both peers share the same PSK
const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });
```

From there the loop is
[Announce](https://ai2070.net/docs/sdk/typescript/announce) →
[Discover](https://ai2070.net/docs/sdk/typescript/discover) →
[Invoke](https://ai2070.net/docs/sdk/typescript/invoke) →
[Watch](https://ai2070.net/docs/sdk/typescript/watch).

## Submodules and the Cargo features behind them

`@net-mesh/sdk` is pure TypeScript; every wrapper dispatches into the
`@net-mesh/core` napi-rs binding. **Published `.node` artifacts ship every
feature enabled**, so `npm install` users get everything. This table matters
only if you build from source — a disabled feature's symbols are absent at
runtime and the wrapper's `import` resolves to `undefined`.

| Cargo feature | Submodule | Surface |
|---|---|---|
| `net` | `@net-mesh/sdk/mesh` | `MeshNode`, `NetStream`, channel auth |
| `cortex` | `@net-mesh/sdk/cortex` | `Redex`, `RedexFile`, `TasksAdapter`, `MemoriesAdapter`, `NetDb` |
| `meshdb` | `@net-mesh/sdk/meshdb` | `MeshQuery`, `MeshQueryRunner`, `MeshQueryStream`, `QueryBuilder`, `InMemoryChainReader` |
| `meshos` | `@net-mesh/sdk/meshos` | `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `DaemonHealth`, `CapabilityAdvert` |
| `compute` | `@net-mesh/sdk/compute` | `DaemonRuntime`, `DaemonHandle`, `MigrationHandle` |
| `groups` | `@net-mesh/sdk/groups` | `ReplicaGroup`, `ForkGroup`, `StandbyGroup` |
| `deck` | `@net-mesh/sdk/deck` | `DeckClient`, `OperatorIdentity`, admin / snapshot / status streams |
| `redis` | top-level | `RedisStreamDedup` |

The bus surface — `NetNode`, `EventStream`, capabilities, identity, predicates
— is always present. To slim a build:

```bash
cd net/crates/net/bindings/node
napi build --platform --release --features "cortex netdb redex-disk meshdb meshos"
```

`bindings/node/package.json` → `scripts.build` is the canonical flag list
shipped to npm.

## What's here

| Surface | Guide |
|---|---|
| Event bus — shards, typed streams, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response and streaming | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX / CortEX / NetDB — logs, folds, queries | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries | [NetDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute + Groups — daemons, migration, replica/fork/standby | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Security — identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — all 32 classes and their hierarchy | [Errors](https://ai2070.net/docs/sdk/typescript/errors) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## License

MIT OR Apache-2.0
