# Net TypeScript SDK

Ergonomic TypeScript SDK for the Net mesh network.

Wraps the `@ai2070/net` NAPI bindings with streaming, typed channels, and a developer-friendly API.

## Install

```bash
npm install @ai2070/net-sdk @ai2070/net
```

## Quick Start

```typescript
import { NetNode } from '@ai2070/net-sdk';

const node = await NetNode.create({ shards: 4 });

// Emit events
node.emit({ token: 'hello', index: 0 });
node.emitRaw('{"token": "world"}');
node.emitBuffer(Buffer.from('{"token": "foo"}'));

// Batch
node.emitBatch([{ a: 1 }, { a: 2 }, { a: 3 }]);

await node.flush();

// Poll
const response = await node.poll({ limit: 100 });
for (const event of response.events) {
  console.log(event.raw);
}

// Stream (async iterator)
for await (const event of node.subscribe({ limit: 100 })) {
  console.log(event.raw);
}

await node.shutdown();
```

## Typed Streams

```typescript
interface TokenEvent {
  token: string;
  index: number;
}

for await (const token of node.subscribeTyped<TokenEvent>({ limit: 100 })) {
  console.log(`${token.index}: ${token.token}`);
}
```

## Typed Channels

```typescript
interface TemperatureReading {
  sensor_id: string;
  celsius: number;
  timestamp: number;
}

const temps = node.channel<TemperatureReading>('sensors/temperature');

// Publish
temps.publish({ sensor_id: 'A1', celsius: 22.5, timestamp: Date.now() });

// Subscribe
for await (const reading of temps.subscribe()) {
  console.log(`${reading.sensor_id}: ${reading.celsius}°C`);
}
```

## Ingestion Methods

| Method | Input | Speed | Returns |
|--------|-------|-------|---------|
| `emit(obj)` | Object | Fast | `Receipt` |
| `emitRaw(json)` | String | Fast | `Receipt` |
| `emitBuffer(buf)` | Buffer | Fastest | `boolean` |
| `emitBatch(objs)` | Object[] | Bulk | `number` |
| `emitRawBatch(jsons)` | String[] | Bulk | `number` |
| `fire(json)` | String | Fire-and-forget | `boolean` |
| `fireBatch(jsons)` | String[] | Fire-and-forget | `number` |

## Transports

```typescript
// In-memory (default)
await NetNode.create({ shards: 4 });

// Redis
await NetNode.create({ transport: { type: 'redis', url: 'redis://localhost:6379' } });

// JetStream
await NetNode.create({ transport: { type: 'jetstream', url: 'nats://localhost:4222' } });

// Encrypted mesh
await NetNode.create({
  transport: {
    type: 'mesh',
    bind: '0.0.0.0:9000',
    peer: '192.168.1.10:9001',
    psk: '...',
    peerPublicKey: '...',
  },
});
```

### Persistent producer nonce (cross-restart dedup)

JetStream and Redis adapters key dedup on `(producer_nonce, shard,
sequence_start, i)`. Without persistence the nonce is fresh per
process — a producer that crashes mid-batch and restarts gets a
new nonce, retransmits look fresh, and the backend persists the
partial half twice. Configure
`producerNoncePath` to make the nonce durable:

```typescript
await NetNode.create({
  shards: 4,
  transport: { type: 'redis', url: 'redis://localhost:6379' },
  producerNoncePath: '/var/lib/myapp/producer.nonce',
});
```

The bus loads (or creates on first run) a u64 nonce at this
path. JetStream gets cross-restart dedup automatically;
Redis Streams ships the same id as a `dedup_id` field on every
XADD, filterable via the helper below.

## Redis Streams consumer-side dedup helper

The Redis adapter writes a stable `dedup_id` field on every XADD
entry (`{producer_nonce:hex}:{shard_id}:{sequence_start}:{i}`).
Combined with `producerNoncePath` above, the id is stable across
both retries and process restart, so the `MULTI/EXEC` timeout
race becomes filterable consumer-side.

`RedisStreamDedup` is exposed on the underlying `@ai2070/net`
NAPI module:

```typescript
import { RedisStreamDedup } from '@ai2070/net';
import { createClient } from 'redis';

// Sizing: ~10k events/sec * 1 min dedup window → ~600,000.
const dedup = new RedisStreamDedup(600_000);

const r = createClient();
await r.connect();

let cursor = '0';
while (true) {
  // XRANGE bounds are INCLUSIVE on both ends. After the first
  // page we must use the exclusive form `(<id>` so we don't
  // re-read the entry the cursor points at — a vanilla
  // `xRange(stream, cursor, '+')` loop spins forever once the
  // cursor reaches the tail and the same entry is returned every
  // iteration.
  const start = cursor === '0' ? cursor : `(${cursor}`;
  const entries = await r.xRange('net:shard:0', start, '+', { COUNT: 100 });
  if (entries.length === 0) break;
  for (const entry of entries) {
    const dedupId = entry.message.dedup_id;
    if (!dedupId) {
      // Older entries / non-Net producers: skip dedup.
      await process(entry);
      continue;
    }
    if (!dedup.isDuplicate(dedupId)) {
      await process(entry);
    }
    cursor = entry.id;
  }
}
```

Surface (NAPI class):

```typescript
new RedisStreamDedup(capacity?: number)   // defaults to 4096
dedup.isDuplicate(id: string): boolean
dedup.len: number       // readonly
dedup.capacity: number  // readonly
dedup.isEmpty: boolean  // readonly
dedup.clear(): void
```

The helper is transport-agnostic — bring your own `redis` /
`ioredis` / equivalent client; it just answers the dedup
question against an in-memory LRU. Concurrency: the underlying
handle wraps a Rust mutex, so concurrent calls from worker
threads serialize but are safe. Production-shape is one helper
per consumer worker.

## NAT Traversal (optimization, not correctness)

Two NATed peers already reach each other through the mesh's routed-handshake path. NAT traversal opens a shorter direct path when the NAT shape allows it; it's never required for connectivity. The TS SDK doesn't yet wrap this surface — it's a planned follow-up. For now, construct a `NetMesh` from `@ai2070/net` directly to access the NAPI methods:

```ts
import { NetMesh } from '@ai2070/net';

const mesh = await NetMesh.create({
  bindAddr: '0.0.0.0:9000',
  psk: '00'.repeat(32),
});

await mesh.reclassifyNat();

const klass  = mesh.natType();            // "open" | "cone" | "symmetric" | "unknown"
const reflex = mesh.reflexAddr();         // "203.0.113.5:9001" | null

const observed = await mesh.probeReflex(peerNodeId); // "ip:port"

// Attempt a direct connection via the pair-type matrix.
// `coordinator` mediates the punch when the matrix picks one.
// Always resolves — stats tell you which path won.
await mesh.connectDirect(peerNodeId, peerPubkeyHex, coordinatorNodeId);

// Cumulative counters — all BigInt, monotonic.
const s = mesh.traversalStats();
s.punchesAttempted;   // coordinator mediated a PunchRequest + Introduce
s.punchesSucceeded;   // ack arrived AND direct handshake landed
s.relayFallbacks;     // landed on the routed path after skip/fail
```

Operators with a known-public address skip the classifier sweep entirely. The override pins `"open"` + the supplied address on every capability announcement; call `announceCapabilities()` after to propagate (the setter resets the rate-limit floor so the next announce is guaranteed to broadcast).

```ts
mesh.setReflexOverride('203.0.113.5:9001');
await mesh.announceCapabilities(/* caps */);
// later:
mesh.clearReflexOverride();
await mesh.announceCapabilities(/* caps */);
```

Traversal failures surface as `Error` instances whose `message` follows the stable `traversal: <kind>[: <detail>]` convention. The `<kind>` discriminator is one of `reflex-timeout` | `peer-not-reachable` | `transport` | `rendezvous-no-relay` | `rendezvous-rejected` | `punch-failed` | `port-map-unavailable` | `unsupported`. Match on the prefix:

```ts
try {
  await mesh.connectDirect(peerNodeId, peerPubkeyHex, coordId);
} catch (e) {
  const msg = (e as Error).message;
  if (msg.startsWith('traversal: unsupported')) {
    // native library built without --features nat-traversal
  } else if (msg.startsWith('traversal: peer-not-reachable')) {
    // ...
  }
}
```

A build without the `nat-traversal` feature raises `traversal: unsupported` for every NAT call — the routed path keeps working regardless. The NAPI type declarations for these methods are only generated when the build-time type-gen runs against a build *with* the feature, so a feature-off cdylib may require an `as any` cast or a local `.d.ts` augmentation.

## Mesh Streams (multi-peer + back-pressure)

For direct peer-to-peer messaging — open a stream to a specific peer
and react to back-pressure with first-class error classes:

```typescript
import { MeshNode, BackpressureError, NotConnectedError } from '@ai2070/net-sdk';

const node = await MeshNode.create({
  bindAddr: '127.0.0.1:9000',
  psk: '0'.repeat(64),
});
// ... handshake (node.connect(...) / node.accept(...)) ...

const stream = node.openStream(peerNodeId, {
  streamId: 0x42n,
  reliability: 'reliable',
  windowBytes: 256,   // max in-flight packets before BackpressureError
});

// Three canonical daemon patterns:

// 1. Drop on pressure.
try {
  await node.sendOnStream(stream, [Buffer.from('{}')]);
} catch (e) {
  if (e instanceof BackpressureError) {
    metrics.inc('stream.backpressure_drops');
  } else if (e instanceof NotConnectedError) {
    // peer gone or stream closed — re-open if needed
  } else {
    throw e;
  }
}

// 2. Retry with exponential backoff (5 ms → 200 ms, up to maxRetries).
await node.sendWithRetry(stream, [Buffer.from('{}')], 8);

// 3. Block until the network lets up (bounded retry, ~13 min worst case).
await node.sendBlocking(stream, [Buffer.from('{}')]);

// Live stats — tx/rx seq, in-flight, window, backpressure count (BigInts).
const stats = node.streamStats(peerNodeId, 0x42n);
```

`BackpressureError` and `NotConnectedError` both extend `Error`, so
`instanceof` and `try/catch` work as expected. The transport never
retries or buffers on its own behalf — the helper methods are
opt-in policies, not defaults. See `../docs/TRANSPORT.md` for the full
contract.

## Security (identity, tokens, capabilities, subnets)

Identity, capabilities, and subnets ride the underlying NAPI bindings
as a single security unit — the mesh's subprotocol dispatch threads
identity + capabilities + subnets + channel auth together at runtime,
and the TS SDK surfaces all of it through one type hierarchy.

```typescript
import { randomBytes } from 'node:crypto';
import { Identity, MeshNode } from '@ai2070/net-sdk';

// Load once from caller-owned storage (vault / KMS / env secret).
// The persisted form IS the 32-byte seed; treat as secret material.
const seed = randomBytes(32);
const identity = Identity.fromSeed(seed);

// Stable entity_id / node_id across restarts — derived from the seed.
const mesh = await MeshNode.create({
  bindAddr: '127.0.0.1:9001',
  psk: '42'.repeat(32),
  identitySeed: seed,          // mesh and identity share the keypair
});

// mesh.entityId().equals(identity.entityId) // true — compare via
// Buffer.equals(), since `===` on Buffers checks reference identity
// not byte equality.

// Issue a scoped subscribe grant for another entity.
const grantee = Identity.generate();
const token = identity.issueToken({
  subject: grantee.entityId,
  scope: ['subscribe'],
  channel: 'sensors/temp',
  ttlSeconds: 300,             // `0` throws — zero TTL would mint a born-expired token
  delegationDepth: 0,          // 0 forbids re-delegation
});

// `token.bytes` is the transport-ready 159-byte blob.
// Ship it to the grantee; they hand it back on subscribe.
```

Errors surface as `IdentityError` (malformed inputs — bad seed
length, unknown scope, invalid channel name) and `TokenError` whose
`kind` discriminator is one of `invalid_format` | `invalid_signature`
| `expired` | `not_yet_valid` | `delegation_exhausted` |
`delegation_not_allowed` | `not_authorized`. Both extend `Error`,
so `try/catch` + `instanceof` work as expected.

### Capability announcements

`mesh.announceCapabilities(caps)` broadcasts a `CapabilitySet` to
every directly-connected peer and self-indexes locally.
`mesh.findNodes(filter)` queries the local index — results include
this node's own id when self matches.

```typescript
import { MeshNode } from '@ai2070/net-sdk';

const mesh = await MeshNode.create({
  bindAddr: '127.0.0.1:9002',
  psk: '42'.repeat(32),
});

await mesh.announceCapabilities({
  hardware: {
    cpuCores: 16,
    memoryMb: 65_536,
    gpu: { vendor: 'nvidia', model: 'h100', vramMb: 81_920 },
  },
  models: [
    { modelId: 'llama-3.1-70b', family: 'llama', contextLength: 128_000 },
  ],
  tags: ['gpu', 'prod'],
});

const gpuPeers = mesh.findNodes({
  requireGpu: true,
  gpuVendor: 'nvidia',
  minVramMb: 40_000,
});
// gpuPeers includes mesh.nodeId() on self-match.
```

#### Scoped discovery (reserved `scope:*` tags)

A provider can narrow *who its query result reaches* by tagging
its `CapabilitySet` with reserved `scope:*` tags. Queries call
`mesh.findNodesScoped(filter, scope)` to filter candidates. The
wire format and forwarders are untouched — enforcement is
purely query-side.

```typescript
import { withTenantScope } from '@ai2070/net-sdk';

// GPU pool advertised to one tenant only.
await mesh.announceCapabilities({
  tags: withTenantScope(['model:llama3-70b'], 'oem-123'),
});

// Tenant-scoped query — returns this node + any Global (untagged) peers.
const oemNodes = mesh.findNodesScoped(
  { requireTags: ['model:llama3-70b'] },
  { kind: 'tenant', tenant: 'oem-123' },
);
```

`ScopeFilter` is a tagged union by `kind`:
`{ kind: 'any' }` (default), `{ kind: 'globalOnly' }`,
`{ kind: 'sameSubnet' }`, `{ kind: 'tenant', tenant }`,
`{ kind: 'tenants', tenants: [...] }`,
`{ kind: 'region', region }`,
`{ kind: 'regions', regions: [...] }`. Reserved announcement
tags: `scope:subnet-local` (visible only under `sameSubnet`),
`scope:tenant:<id>`, `scope:region:<name>` — strictest scope
wins. Helpers `withTenantScope`, `withRegionScope`,
`withSubnetLocalScope` build the tag list idempotently.
Untagged peers resolve to `Global` and stay visible under
permissive queries. Full design:
[`docs/SCOPED_CAPABILITIES_PLAN.md`](../docs/SCOPED_CAPABILITIES_PLAN.md).

Propagation is multi-hop, bounded by `MAX_CAPABILITY_HOPS = 16`.
Forwarders re-broadcast every received announcement to their other
peers; dedup on `(origin, version)` drops duplicates at convergence
points, and `hop_count` sits outside the signed envelope so the
origin's signature verifies at every hop.
`capabilityGcIntervalMs` + TTL-driven eviction are configurable on
`MeshNode.create`. See
[`docs/MULTIHOP_CAPABILITY_PLAN.md`](../docs/MULTIHOP_CAPABILITY_PLAN.md).

### Subnets (visibility partitioning)

`subnet` pins a node to a specific 4-level `SubnetId`; `subnetPolicy`
derives each *peer's* subnet from their inbound capability tags so
every node in the mesh agrees on the geometry without a central
directory.

```typescript
import { MeshNode } from '@ai2070/net-sdk';

const policy = {
  rules: [
    { tagPrefix: 'region:', level: 0, values: { us: 3, eu: 4 } },
    { tagPrefix: 'fleet:',  level: 1, values: { blue: 7, green: 8 } },
  ],
};

const mesh = await MeshNode.create({
  bindAddr: '127.0.0.1:9003',
  psk: '42'.repeat(32),
  subnet: { levels: [3, 7] },    // us/blue
  subnetPolicy: policy,
});

// Announce tags matching the policy so peers derive the same
// SubnetId [3, 7] when they apply their own policy to our caps.
await mesh.announceCapabilities({ tags: ['region:us', 'fleet:blue'] });
```

Channel `visibility` gates publish fan-out and subscribe
authorization against the derived geometry. Cross-subnet subscribes
to a `SubnetLocal` channel reject with `Unauthorized`.

### Channel authentication

`ChannelConfig` carries three auth knobs, enforced end-to-end at
both the subscribe gate and the publish path:

- `publishCaps: CapabilityFilter` — publisher must satisfy before
  fan-out. Failing publishes raise an error; no peers are attempted.
- `subscribeCaps: CapabilityFilter` — subscribers must satisfy
  before being added to the roster. Failures surface as
  `ChannelAuthError`.
- `requireToken: true` — subscribers must present a valid `Token`
  whose subject matches their `entityId`. The publisher verifies
  the ed25519 signature, installs the token in its local cache,
  then runs `can_subscribe`.

```typescript
import { Identity, MeshNode } from '@ai2070/net-sdk';

const pubIdentity = Identity.generate();
const subIdentity = Identity.generate();

const publisher = await MeshNode.create({
  bindAddr: '127.0.0.1:9004',
  psk: '42'.repeat(32),
  identitySeed: pubIdentity.toBytes(),
});

// Subscriber-side mesh, pinned to subIdentity so the publisher's
// `require_token` check matches the token's subject against the
// subscribing peer's entityId.
const subscriber = await MeshNode.create({
  bindAddr: '127.0.0.1:9005',
  psk: '42'.repeat(32),
  identitySeed: subIdentity.toBytes(),
});
// Handshake the pair + start receive loops before any subscribe —
// omitted here for brevity; see the `Mesh Streams` section.

publisher.registerChannel({
  name: 'events/inference',
  subscribeCaps: { requireTags: ['gpu'] },
  requireToken: true,
});

// Issue a SUBSCRIBE-scope token for the subscriber.
const token = pubIdentity.issueToken({
  subject: subIdentity.entityId,
  scope: ['subscribe'],
  channel: 'events/inference',
  ttlSeconds: 300,
});

// Subscriber attaches the token on subscribe.
await subscriber.subscribeChannel(
  publisher.nodeId(),
  'events/inference',
  { token },
);
```

Denied subscribes surface as `ChannelAuthError` (a subclass of
`ChannelError`); malformed token bytes raise `TokenError` before
any network I/O. Successful subscribes populate an `AuthGuard`
bloom filter on the publisher so every subsequent publish admits
the subscriber in constant time (~20 ns per check,
single-threaded). Expired tokens evict within the publisher's
`token_sweep_interval` (default 30 s); repeated subscribe
failures from the same peer throttle via `RateLimited` acks so
bad-token storms never tie up ed25519 verification. Cross-SDK
behaviour is fixed by the Rust integration suite — see
[`SDK_SECURITY_SURFACE_PLAN.md`](../docs/SDK_SECURITY_SURFACE_PLAN.md)
and
[`CHANNEL_AUTH_GUARD_PLAN.md`](../docs/CHANNEL_AUTH_GUARD_PLAN.md)
for the full contract.

## Channels (distributed pub/sub)

Named pub/sub across the encrypted mesh. The publisher registers a
channel config; subscribers ask to join via `subscribeChannel` (the
subscribe goes through a dedicated subprotocol with an Ack round-trip);
`publish` fans one payload out to every current subscriber.

```typescript
import { MeshNode, ChannelAuthError } from '@ai2070/net-sdk';

const psk = '0'.repeat(64);

// Publisher side.
const b = await MeshNode.create({ bindAddr: '127.0.0.1:9001', psk });
b.registerChannel({
  name: 'sensors/temp',
  visibility: 'global',           // or 'subnet-local' / 'parent-visible' / 'exported'
  reliable: true,
  priority: 2,
  maxRatePps: 1000,
});

// Subscriber side + full handshake.
const a = await MeshNode.create({ bindAddr: '127.0.0.1:9002', psk });
const aNodeId = a.nodeId();
const bNodeId = b.nodeId();
// connect/accept must race: the initiator blocks on a handshake reply
// that only shows up once the responder is in accept(). Then both
// sides must start() their receive loops before app traffic flows.
await Promise.all([
  b.accept(aNodeId),
  a.connect('127.0.0.1:9001', b.publicKey(), bNodeId),
]);
await a.start();
await b.start();
await a.subscribeChannel(bNodeId, 'sensors/temp');

// Fan out.
const report = await b.publish(
  'sensors/temp',
  Buffer.from(JSON.stringify({ celsius: 22.5 })),
  { reliability: 'reliable', onFailure: 'best_effort', maxInflight: 32 },
);
console.log(`${report.delivered}/${report.attempted} subscribers received`);

// Rejections surface with typed errors:
try {
  await a.subscribeChannel(bNodeId, 'restricted');
} catch (e) {
  if (e instanceof ChannelAuthError) { /* ACL rejected */ }
}
```

**Channel names always cross the boundary as strings.** The u16 hash
is a transport-layer index only; ACL lookups key on the canonical
name to avoid bypass via hash collision (see `../docs/CHANNELS.md`).

Subscribers today receive payloads through the existing event-bus
`poll()` surface — a dedicated per-channel `AsyncIterable` receive
method is a follow-up.

## CortEX & NetDb (event-sourced state)

Typed, event-sourced state on top of RedEX — tasks and memories with
filterable queries and reactive `AsyncIterable` watches. Includes the
`snapshotAndWatch` primitive whose race fix landed on v2, so you can
safely "paint what's there now, then react to changes" without losing
updates that race during construction.

```typescript
import { NetDb, TaskStatus, CortexError } from '@ai2070/net-sdk';

const db = await NetDb.open({
  originHash: 0xABCDEF01,
  withTasks: true,
  withMemories: true,
  // persistentDir + persistent: true for disk-backed files
});

// CRUD through the domain API — no EventMeta plumbing.
try {
  const seq = db.tasks!.create(1n, 'write docs', 100n);
  await db.tasks!.waitForSeq(seq);  // wait for the fold to apply
} catch (e) {
  if (e instanceof CortexError) { /* handle adapter error */ }
  else { throw e; }
}

// Snapshot + watch: one atomic call, no race.
const { snapshot, updates } = await db.tasks!.snapshotAndWatch({
  status: TaskStatus.Pending,
});
render(snapshot);
for await (const next of updates) {
  render(next);
  if (shouldStop) break;   // automatically closes the native iterator
}

db.close();
```

### Plain watches

`watch()` returns the same `AsyncIterable<T[]>` shape without a
snapshot. Prefer `snapshotAndWatch` when the caller needs the initial
result — calling `listTasks()` + `watch()` separately races, and a
mutation landing between them can be silently lost.

```typescript
for await (const batch of await db.tasks!.watch({ titleContains: 'ship' })) {
  // each batch is the current filter result after a deduplicated fold tick
}
```

### Standalone adapters

If you only need one model, skip the `NetDb` facade and open the
adapter directly against a `Redex`:

```typescript
import { Redex, TasksAdapter } from '@ai2070/net-sdk';

const redex = new Redex({ persistentDir: '/var/lib/net/redex' });
const tasks = await TasksAdapter.open(redex, 0xABCDEF01, { persistent: true });
```

### Raw RedEX file (no CortEX fold)

For domain-agnostic persistent logs — your own event schema, no fold,
no typed adapter — open a `RedexFile` directly from a `Redex`. The
tail iterator is the same `AsyncIterable` shape as the CortEX
watches, so `for await` + `break` cleans up native resources.

```typescript
import { Redex, RedexError } from '@ai2070/net-sdk';

const redex = new Redex({ persistentDir: '/var/lib/net/events' });
const file = redex.openFile('analytics/clicks', {
  persistent: true,
  fsyncIntervalMs: 100,           // or fsyncEveryN: 1000n
  retentionMaxEvents: 1_000_000n,
});

// Append (or batch-append).
const seq = file.append(Buffer.from(JSON.stringify({ url: '/home' })));
// `appendBatch` returns the first-seq `bigint` of the batch, or
// `null` for an empty input. The `null` return is the explicit
// "I appended nothing" signal — pre-`bugfixes-8` it returned `0n`,
// which collided with the legitimate "first event of a non-empty
// batch landed at seq 0" return.
const firstSeq = file.appendBatch(payloadBuffers);

// Tail — backfills the retained range, then streams live appends.
const stream = await file.tail(0n);
try {
  for await (const event of stream) {
    const parsed = JSON.parse(event.payload.toString());
    console.log(event.seq, parsed);
    if (shouldStop) break;   // automatically closes the native iterator
  }
} catch (e) {
  if (e instanceof RedexError) { /* ... */ }
  throw e;
} finally {
  // Ensure the file is closed even if tailing / parsing throws.
  file.close();
}
```

### Error classes

CortEX-boundary errors are typed and catchable via `instanceof`:

- `CortexError` — adapter errors (fold halted, RedEX I/O, decode failures).
- `NetDbError` — snapshot/restore bundle errors, missing-model lookups.
- `RedexError` — raw file errors (invalid channel name, bad config,
  append / tail / sync / close failures).

All three are re-exported from `@ai2070/net-sdk`; you don't need a
separate import path.

## nRPC (request / response over the mesh)

nRPC is the request/response convention layer riding on top of the
pub/sub mesh. It turns a directed channel pair
(`<service>.requests` / `<service>.replies.<caller_origin>`) into
a typed RPC surface with deadlines, queue-group fan-out, response
streaming, and end-to-end cancellation.

The typed surface ships in the napi binding at
`@ai2070/net/mesh_rpc` (the SDK's `MeshNode` wraps a `NetMesh`
that nRPC consumes directly):

```typescript
import { MeshNode } from '@ai2070/net-sdk'
import {
  classifyError,
  RpcCancelledError,
  RpcServerError,
} from '@ai2070/net/errors'
import {
  appError,
  CircuitBreaker,
  HedgePolicy,
  NRPC_TYPED_BAD_REQUEST,
  RetryPolicy,
  TypedMeshRpc,
} from '@ai2070/net/mesh_rpc'

const server = await MeshNode.create({ bindAddr: '127.0.0.1:9001', psk })
const client = await MeshNode.create({ bindAddr: '127.0.0.1:9000', psk })
// (handshake omitted — see Mesh Streams example)

interface EchoSumRequest  { text: string; numbers: number[] }
interface EchoSumResponse { echo: string; sum: number }

// Server side: register a typed handler. Returned `serveHandle`
// MUST be `close()`d to stop accepting new requests; in-flight
// handlers complete (no abort).
const serverRpc = TypedMeshRpc.fromMesh((server as any)._native)
const serveHandle = serverRpc.serve<EchoSumRequest, EchoSumResponse>(
  'echo_sum',
  async (req) => ({ echo: req.text, sum: req.numbers.reduce((a, b) => a + b, 0) }),
)

// Client side: typed call with a 200ms deadline.
const clientRpc = TypedMeshRpc.fromMesh((client as any)._native)
try {
  const reply = await clientRpc.call<EchoSumRequest, EchoSumResponse>(
    server.nodeId(),
    'echo_sum',
    { text: 'hi', numbers: [1, 2, 3] },
    { deadlineMs: 200 },
  )
  // reply.sum === 6
} catch (e) {
  // Errors carry a stable `nrpc:` prefix; classifyError() routes
  // them to typed subclasses for instanceof checks.
  const typed = classifyError(e)
  if (typed instanceof RpcServerError && typed.status === NRPC_TYPED_BAD_REQUEST) {
    // handler bad-request
  }
}

await serveHandle.close()
```

### Streaming responses

```typescript
const stream = await clientRpc.callStreaming<MyReq, MyChunk>(
  targetNodeId, 'tail', { tail: 'events' },
  { deadlineMs: 5_000, streamWindowInitial: 8 },  // optional flow control
)
for await (const chunk of stream) {
  // chunk is decoded MyChunk
}
// stream.close() emits CANCEL to the server (best-effort);
// in-flight chunks are silently discarded.
// stream.grant(n) issues an explicit credit publish for batched
// cadence (no-op on streams without flow control).
// stream.flowControlled() reports whether streamWindowInitial was
// set on the call — useful for code that conditionally grants.
```

### Cancellation (`AbortSignal`)

`call` / `callService` accept an `AbortSignal` via `opts.signal`.
The wrapper mints a cancel token, attaches a one-shot abort
listener, and detaches it on settle so the same signal can be
reused. Aborting publishes CANCEL to the server and rejects with
`RpcCancelledError` (caller-fixable; **not** retried by the
default `RetryPolicy` predicate).

```typescript
const ac = new AbortController()
setTimeout(() => ac.abort(), 100)

try {
  await clientRpc.call(targetNodeId, 'slow', {}, { signal: ac.signal })
} catch (e) {
  if (classifyError(e) instanceof RpcCancelledError) {
    // CANCEL fired on the wire; server-side handler observes
    // its `ctx.cancellation` token.
  }
}
```

Pre-aborted signals fail fast — the call rejects with
`nrpc:cancelled:` before any tokio spawn / registry overhead.

### Resilience helpers

Defaults mirror the Rust SDK (`mesh_rpc_resilience`): 3 attempts,
50ms→1s exponential backoff with full-half jitter, retryable
predicate skips `RpcCodecError` / `RpcNoRouteError` /
`RpcCancelledError` and non-transient `RpcServerError` statuses.

```typescript
// RetryPolicy. `jitter` is a boolean (full-half jitter on/off);
// override `retryable` to gate which errors retry.
const policy = new RetryPolicy({
  maxAttempts: 4,
  initialBackoffMs: 50,
  maxBackoffMs: 1000,
  jitter: true,
})
const reply = await clientRpc.callWithRetry(
  targetNodeId, 'echo', { hello: 'world' }, undefined /* opts */, policy,
)

// HedgePolicy fans out parallel attempts on a delay; primary at
// t=0, additional hedges at t=delayMs * idx. First reply (Ok or
// Err) wins; if every hedge fails, the primary's error surfaces
// deterministically.
const hedge = new HedgePolicy({ delayMs: 50, hedges: 2 })  // primary + 2 hedges
await clientRpc.callWithHedgeTo(targetNodeIds, 'echo', { /*...*/ }, undefined, hedge)

// CircuitBreaker — closed → open → half-open with a configurable
// failure predicate. Open breakers throw `BreakerOpenError` carrying
// the `nrpc:breaker_open:` prefix.
const breaker = new CircuitBreaker({ failureThreshold: 5, resetAfterMs: 1000 })
await breaker.call(() => clientRpc.call(targetNodeId, 'echo', {}))
```

### Typed handler bad-request

`appError(code, body)` builds an `Error` whose message follows the
`nrpc:app_error:0x<code>:<body>` contract the napi binding parses
into `RpcStatus::Application(code)`. Mirrors the Python binding's
`RpcAppError`:

```typescript
serverRpc.serve<EchoSumRequest, EchoSumResponse>('echo_sum', (req) => {
  if (typeof req.text !== 'string') {
    throw appError(NRPC_TYPED_BAD_REQUEST, JSON.stringify({
      error: 'invalid_request',
      detail: 'text must be a string',
    }))
  }
  return { echo: req.text, sum: req.numbers.reduce((a, b) => a + b, 0) }
})
```

### Errors

Caller-side failures throw a plain `Error` whose `.message`
starts with the stable `nrpc:` prefix (the binding throws plain
`Error` rather than typed classes to sidestep vitest's
dual-module-instance hazard; `classifyError(e)` reconstructs the
typed subclass at the catch site):

| Kind segment    | Typed class           | Retried by default? |
| --------------- | --------------------- | ------------------- |
| `no_route`      | `RpcNoRouteError`     | no                  |
| `timeout`       | `RpcTimeoutError`     | yes                 |
| `server_error`  | `RpcServerError`      | only `0x0003` / `0x0004` / `0x0006` |
| `transport`     | `RpcTransportError`   | yes                 |
| `codec_encode`  | `RpcCodecError`       | no (caller-fixable) |
| `codec_decode`  | `RpcCodecError`       | no (caller-fixable) |
| `cancelled`     | `RpcCancelledError`   | no (caller-driven)  |
| any other       | `RpcError` (base)     | yes (forward-compat fallback) |

`BreakerOpenError` is thrown directly by `CircuitBreaker.call`
when the breaker is open — catch it via
`instanceof BreakerOpenError` (imported from `@ai2070/net/mesh_rpc`).
It carries the `nrpc:breaker_open:` prefix for log filtering, but
`classifyError` routes it through the base `RpcError` rather than
its own subclass. Server-side `appError(code, body)` rejections
arrive at the caller as `nrpc:server_error: status=0x<code>`, so
they classify as `RpcServerError` with `err.status === code`
(check against `NRPC_TYPED_BAD_REQUEST` etc.).

`classifyError` is duck-typed on `.message`: it accepts real
`Error` instances, plain `{message: string}` objects, and string
rejections — so top-level catch handlers reconstruct typed
errors regardless of what the throw site emitted.

Two stable status constants exposed by `@ai2070/net/mesh_rpc`:

| Constant                       | Hex      | Meaning                                          |
| ------------------------------ | -------- | ------------------------------------------------ |
| `NRPC_TYPED_BAD_REQUEST`       | `0x8000` | Typed handler couldn't decode the request body.  |
| `NRPC_TYPED_HANDLER_ERROR`     | `0x8001` | Typed handler ran but returned an exception.     |

Cross-binding contract spec — including the canonical
`cross_lang_echo_sum` service used by every binding's wire-format
compat test — lives in [`../README.md#nrpc`](../README.md#nrpc).

## Compute (daemons + migration)

Run `MeshDaemon`s directly from TypeScript. `DaemonRuntime` owns
the factory table, per-daemon hosts, and the
`Registering → Ready → ShuttingDown` lifecycle gate that decides
when inbound migrations may land. Daemons are plain JS objects
(or class instances) whose `process(event)` returns an array of
output `Buffer`s — the runtime wraps each output in a causal link
automatically.

Build the `@ai2070/net` NAPI module with `--features compute`
(auto-enabled in the default `local` bundle) to expose the
surface; everything below is re-exported from `@ai2070/net-sdk`.
Full design notes:
[`docs/SDK_COMPUTE_SURFACE_PLAN.md`](../docs/SDK_COMPUTE_SURFACE_PLAN.md).

```typescript
import {
  DaemonRuntime, DaemonError, Identity, MeshNode,
  type CausalEvent, type MeshDaemon,
} from '@ai2070/net-sdk';

// 1. Build a mesh + runtime.
const mesh = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: '42'.repeat(32) });
const rt = DaemonRuntime.create(mesh);

// 2. Register factories BEFORE flipping the runtime to Ready.
rt.registerFactory('echo', (): MeshDaemon => ({
  name: 'echo',
  process: (event: CausalEvent) => [event.payload],
  // optional: snapshot() / restore(state) for migration-capable daemons
}));

// 3. Ready the runtime — after this point spawns + migrations accept.
await rt.start();

// 4. Spawn a daemon. `Identity` pins its ed25519 keypair so
//    `originHash` / `entityId` stay stable across migrations.
const handle = await rt.spawn('echo', Identity.generate());
console.log('origin =', handle.originHash.toString(16));

// 5. Inspect / stop when done.
const stats = handle.stats();       // eventsProcessed / eventsEmitted / ...
await rt.stop(handle.originHash);
await rt.shutdown();
```

`MeshDaemon.process` is synchronous by contract — the NAPI TSFN
bridge blocks the calling tokio task until it returns, so
returning a `Promise` will break event dispatch. Stateful daemons
opt into migration by adding `snapshot(): Buffer | null` and
`restore(state: Buffer): void`.

### Migration

`startMigration(origin, sourceNode, targetNode)` orchestrates the
six-phase cutover (`Snapshot → Transfer → Restore → Replay →
Cutover → Complete`). The source seals the daemon's seed into the
outbound snapshot using the target's X25519 static pubkey; the
target's factory for the same `kind` rebuilds the daemon, replays
any events that arrived during transfer, then activates.

```typescript
import { MigrationError } from '@ai2070/net-sdk';

try {
  const mig = await rtA.startMigration(handle.originHash, nodeA, nodeB);
  console.log('phase =', mig.phase);        // 'snapshot' | 'transfer' | ...
  await mig.wait();                         // drive to completion
} catch (e) {
  if (e instanceof MigrationError) {
    switch (e.kind) {
      case 'not-ready':                 break; // target not started yet
      case 'factory-not-found':         break; // target missing `kind`
      case 'compute-not-supported':     break; // target has no DaemonRuntime
      case 'state-failed':              break; // snapshot / restore threw
      case 'identity-transport-failed': break; // seal / unseal failed
      // ... see MigrationErrorKind for the full set
    }
  }
}
```

`startMigrationWith(origin, src, dst, { sealSeed, ... })` exposes
the advanced knobs. On the target node, call
`rt.registerMigrationTargetIdentity(identity)` before a migration
lands — without it, the runtime rejects sealed-seed envelopes with
`MigrationError.kind === 'identity-transport-failed'`.

### Surface at a glance

| Method | Description |
|---|---|
| `DaemonRuntime.create(mesh)` | Construct a runtime against an existing `MeshNode` |
| `rt.registerFactory(kind, fn)` | Install a factory (must run before `start()`) |
| `rt.start() / rt.shutdown()` | Flip the lifecycle gate |
| `rt.spawn(kind, identity, cfg?)` | Spawn a local daemon |
| `rt.spawnFromSnapshot(kind, identity, bytes, cfg?)` | Rehydrate from a snapshot |
| `rt.stop(origin)` | Stop a local daemon |
| `rt.snapshot(origin)` | Capture a `Buffer` for persistence / migration |
| `rt.deliver(origin, event)` | Feed an event (returns output buffers) |
| `rt.startMigration(origin, src, dst)` | Orchestrate a live migration |
| `rt.registerMigrationTargetIdentity(id)` | Pin the unseal keypair on target nodes |
| `handle.originHash` / `entityId` / `stats()` | Per-daemon identity + observability |
| `DaemonError` / `MigrationError` | Typed catch classes (`instanceof` + `err.kind`) |

## Groups (replica / fork / standby)

HA / scaling overlays on top of `DaemonRuntime`. Build the NAPI
crate with `--features groups` (implies `compute`) to expose
`ReplicaGroup`, `ForkGroup`, and `StandbyGroup`.

- **ReplicaGroup** — N interchangeable copies with deterministic
  identity per index; load-balances inbound events across healthy
  members; auto-replaces on node failure.
- **ForkGroup** — N independent daemons forked from a common parent
  at `forkSeq`. Unique identities, shared ancestry via a verifiable
  `ForkRecord`.
- **StandbyGroup** — active-passive replication. One member processes
  events; standbys hold snapshots via `sync()`. Most-synced standby
  promotes on active failure and replays buffered events.

```typescript
import {
  DaemonRuntime, ForkGroup, GroupError, ReplicaGroup, StandbyGroup,
} from '@ai2070/net-sdk';

const rt = await DaemonRuntime.create(mesh);
rt.registerFactory('counter', () => new CounterDaemon());

// ReplicaGroup — async because the factory round-trips through the
// Node main thread (TSFN).
const replicas = await ReplicaGroup.spawn(rt, 'counter', {
  replicaCount: 3,
  groupSeed: Buffer.alloc(32, 0x11),
  lbStrategy: 'consistent-hash',        // or 'round-robin' | 'least-load' | ...
});

const origin = replicas.routeEvent({ routingKey: 'user:42' });
await rt.deliver(origin, event);

await replicas.scaleTo(5);               // grow
await replicas.onNodeFailure(failedNodeId);   // respawn elsewhere

// ForkGroup
const forks = await ForkGroup.fork(rt, 'counter',
  /* parentOrigin */ 0xabcdef01,
  /* forkSeq     */ 42n,
  { forkCount: 3, lbStrategy: 'round-robin' });
console.log(forks.verifyLineage(), forks.forkRecords.length);

// StandbyGroup — manual event buffering for replay on promotion.
const hot = await StandbyGroup.spawn(rt, 'counter', {
  memberCount: 3,                        // 1 active + 2 standbys
  groupSeed: Buffer.alloc(32, 0x77),
});
await rt.deliver(hot.activeOrigin, event);
hot.onEventDelivered(event);             // keep standbys' replay buffer accurate
await hot.sync();                        // periodic catchup
// await hot.onNodeFailure(failedNodeId); // auto-promotes the most-synced standby
```

### Typed errors

Failures surface as `GroupError` (a subclass of `DaemonError`) with
a stable `kind` discriminator parsed from the Rust side's
`daemon: group: <kind>[: detail]` prefix:

```typescript
import { GroupError } from '@ai2070/net-sdk';

try {
  await ReplicaGroup.spawn(rt, 'never-registered', cfg);
} catch (e) {
  if (e instanceof GroupError) {
    switch (e.kind) {
      case 'not-ready':           break; // runtime.start() hasn't run
      case 'factory-not-found':   break; // e.requestedKind tells you which
      case 'no-healthy-member':   break; // routeEvent on an all-down group
      case 'invalid-config':      break; // e.detail has the specifics
      case 'placement-failed':    break;
      case 'registry-failed':     break;
    }
  }
}
```

Full staging, wire formats, and rationale:
[`docs/SDK_GROUPS_SURFACE_PLAN.md`](../docs/SDK_GROUPS_SURFACE_PLAN.md).
Core semantics (placement spread, health aggregation, failure
domains): [`../README.md#daemons`](../README.md#daemons).

## API

| Method | Description |
|--------|-------------|
| `NetNode.create(config)` | Create a new node |
| `emit(obj)` | Emit a typed event |
| `emitRaw(json)` | Emit a JSON string |
| `emitBuffer(buf)` | Emit a Buffer (fastest) |
| `emitBatch(objs)` | Batch emit |
| `emitRawBatch(jsons)` | Batch emit strings |
| `fire(json)` | Fire-and-forget |
| `fireBatch(jsons)` | Fire-and-forget batch |
| `poll(request)` | One-shot poll |
| `pollOne()` | Poll a single event |
| `subscribe(opts)` | Async iterable stream |
| `subscribeTyped<T>(opts)` | Typed async iterable |
| `channel<T>(name)` | Create a typed channel |
| `stats()` | Ingestion statistics |
| `shards()` | Number of active shards |
| `flush()` | Flush pending batches |
| `shutdown()` | Graceful shutdown |
| `napi` | Access underlying NAPI binding |

### CortEX surface

| Entry point | Description |
|---|---|
| `new Redex({ persistentDir? })` | Local event-log manager |
| `NetDb.open({ originHash, withTasks?, withMemories?, ... })` | Unified handle |
| `NetDb.openFromSnapshot(config, bundle)` | Restore from `db.snapshot()` bundle |
| `db.tasks` / `db.memories` | Typed adapter handles |
| `TasksAdapter.open(redex, origin, opts?)` | Standalone tasks adapter |
| `MemoriesAdapter.open(redex, origin, opts?)` | Standalone memories adapter |
| `adapter.create/rename/complete/delete/...` | Domain CRUD |
| `adapter.listTasks(filter?)` / `listMemories` | Sync snapshot query |
| `adapter.watch(filter?)` | `Promise<AsyncIterable<T[]>>` over deduplicated fold results |
| `adapter.snapshotAndWatch(filter?)` | `Promise<SnapshotAndWatch<T>>` — atomic paint+react |
| `adapter.snapshot()` / `openFromSnapshot` | Model-level persistence |
| `db.snapshot()` / `NetDb.openFromSnapshot` | Bundled multi-model persistence |
| `redex.openFile(name, config?)` | Raw RedEX file — append-only log |
| `file.append(buffer)` / `appendBatch(buffers)` | Append one / many payloads |
| `file.readRange(start, end)` | Range read over retained entries |
| `file.tail(fromSeq?)` | `AsyncIterable<RedexEvent>` |
| `file.sync()` / `file.close()` | Explicit fsync / close |

## License

Apache-2.0
