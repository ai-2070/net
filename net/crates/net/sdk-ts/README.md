# Net TypeScript SDK

**A latency-first encrypted mesh where services and agents announce what they
can do, discover each other at runtime, and invoke work over typed RPC.**

[![npm](https://img.shields.io/npm/v/@net-mesh/sdk.svg)](https://www.npmjs.com/package/@net-mesh/sdk)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)

There is no broker. Every node is a peer on a flat, encrypted topology. A node
publishes the capabilities it has — a GPU, a model, a tool, a licensed seat —
and other nodes find it by *what it can do*, not by hostname. Credentials never
leave the node that holds them: the machine with the secret runs the work.

```typescript
// Find something that can do the job, then do it — no registry, no config.
const peers = node.findNodes({ requireTags: ['gpu'], minVramMb: 16_384 });
const resp = await callTool(node, 'summarize', { text });
```

## Why this instead of a queue

Kafka, NATS and Redis Streams move bytes between a fixed producer and a fixed
consumer through a broker you operate. That's a different problem. Net is for
when **the set of participants isn't known in advance** and **work has state
worth observing**:

| You need | Net gives you |
|---|---|
| Tools that appear and vanish at runtime | Capability announce + discovery, event-driven — no polling, no service registry |
| Work spread across machines or organizations | One flat encrypted mesh; multi-hop discovery bounded at 16 hops |
| Credentials that must not travel | The node holding the secret executes; callers never see it |
| More than "did it return 200" | Durable logs, folded state, artifacts, streams, and replayable recovery |
| GPUs matched by capability, not hostname | Hardware is a discoverable characteristic, with atomic gang-claim under contention |

**Don't** reach for it when one API call solves the problem, when a single
server and database are enough, or when you have a fixed producer, a fixed
consumer and a broker you're happy operating. The honest version of that list
is in [When to Use Net](https://ai2070.net/docs/worldview/right-and-wrong-use-cases).

## Install

```bash
npm install @net-mesh/sdk @net-mesh/core
```

`@net-mesh/sdk` is the TypeScript wrapper; `@net-mesh/core` is the napi-rs
native binding it dispatches into. Prebuilt `.node` artifacts ship with every
feature enabled.

## The loop: announce → discover → invoke

**Announce** a tool, and it becomes discoverable across the mesh:

```typescript
import { MeshNode, serveTool } from '@net-mesh/sdk';

const psk = new Uint8Array(32).fill(0x42);   // both peers share the same PSK
const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk });

const handle = serveTool(node, {
  name: 'web_search',
  description: 'Search the web for relevant pages.',
  tags: ['web', 'research'],
}, async (req: { query: string }) => {
  return { results: [`first hit for '${req.query}'`] };
});
// handle.close() when done — always close explicitly.
```

**Discover** — react to the mesh changing rather than polling it:

```typescript
import { listTools, watchTools } from '@net-mesh/sdk';

for (const t of listTools(node)) {
  console.log(`${t.toolId} v${t.version}  tags=${t.tags}`);
}

const controller = new AbortController();
for await (const change of watchTools(node, { signal: controller.signal })) {
  console.log(change);   // pushed on fold mutation — no timer, no re-diff
}
```

**Invoke** — `callTool` finds a provider for the name and calls it:

```typescript
import { callTool } from '@net-mesh/sdk';

const resp = await callTool(node, 'web_search', { query: 'how does the fold work' });
```

For services rather than tools, nRPC gives you the same shape with deadlines,
streaming and cancellation — [Typed RPC](https://ai2070.net/docs/guides/nrpc).

## The bus

`NetNode` is the other node type: a sharded, in-process event bus with explicit
backpressure.

```typescript
import { NetNode } from '@net-mesh/sdk';

const node = await NetNode.create({ shards: 4 });

node.emit({ sensor: 'lidar', range_m: 12.5 });
node.emitRaw('{"sensor":"radar","range_m":45.0}');
node.emitBatch([{ a: 1 }, { a: 2 }, { a: 3 }]);

await node.flush();

const stats = node.stats();
console.log(`${stats.eventsIngested} ingested, ${stats.eventsDropped} dropped`);

await node.shutdown();   // explicit — Node finalizers are non-deterministic
```

Consume what you emit:

```typescript
for await (const event of node.subscribe({ limit: 100 })) {
  console.log('event', event);
}
```

`emit` returns once the event is **accepted into the local ring buffer** — not
that anyone processed it. Under backpressure it drops, and
`stats().eventsDropped` is how you find out. That distinction is the whole
philosophy: [Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

## Building with Claude Code

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that compiles, runs, and is quietly wrong. Install the skills
first:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments ~/.claude/skills/
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. They load automatically when a request matches:

> *"Wire up a Net publisher and subscriber over the mesh in TypeScript."*

`net-event-bus` covers pub/sub, nRPC, the MCP bridge, organization capability
auth, the gang-claim scheduler, and RedEX / CortEX / Dataforts.
`net-payments` covers x402 pricing, quotes, settlement and spend policy. Full
install options — project-scoped, symlinked to stay current — in
[Claude Skills](https://ai2070.net/docs/start/claude-skills).

## Submodules and the Cargo features behind them

Every wrapper dispatches into `@net-mesh/core`. **Published artifacts ship
every feature enabled**, so this table matters only for source builds — a
disabled feature's symbols are absent at runtime and the `import` resolves to
`undefined`.

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

## What's in the box

| Surface | Guide |
|---|---|
| Event bus — shards, typed streams, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response, streaming, cancellation | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX / CortEX / NetDB — logs, folds, queries | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries | [MeshDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute + Groups — daemons, migration, replica/fork/standby | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| MCP bridge — wrap an MCP server, or serve the mesh as MCP | [Wrap MCP](https://ai2070.net/docs/guides/wrap-mcp-server), [Expose as MCP](https://ai2070.net/docs/guides/expose-net-as-mcp) |
| Organizations — capabilities only your org can discover | [Private capabilities](https://ai2070.net/docs/guides/private-capabilities) |
| Security — identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — all 32 classes and their hierarchy | [Errors](https://ai2070.net/docs/sdk/typescript/errors) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## Links

[Docs](https://ai2070.net/docs) ·
[Quickstart](https://ai2070.net/docs/sdk/typescript/quickstart) ·
[Concepts](https://ai2070.net/docs/concepts/architecture) ·
[GitHub](https://github.com/ai-2070/net)

## License

MIT OR Apache-2.0
