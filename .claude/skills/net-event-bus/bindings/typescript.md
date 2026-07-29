# Node / TypeScript binding

Read `../apis.md` first for the four surfaces and the cross-SDK rules. This page
is only what is TypeScript-specific.

## Packages and import

```bash
npm install @net-mesh/sdk @net-mesh/core
```

```typescript
import { NetNode } from '@net-mesh/sdk';
```

**Two packages, and the split matters.** `@net-mesh/sdk` is the ergonomic
wrapper; `@net-mesh/core` is the napi binding underneath it. Several surfaces
are reachable *only* from `@net-mesh/core` — payments is entirely there, and
`bindings/coverage.md` marks each one `core-only`. If an import from
`@net-mesh/sdk` does not resolve, check the matrix before concluding the feature
is missing.

## Construction and lifecycle

```typescript
interface TempReading { sensor_id: string; celsius: number }

const node = await NetNode.create({ shards: 4 });
// Other transports: pass `transport: { type: 'redis' | 'jetstream' | 'mesh', ... }`
// to create() — per-transport fields are on `Transport` in src/types.ts.

const temps = node.channel<TempReading>('sensors/temperature');
temps.publish({ sensor_id: 'A1', celsius: 22.5 });

for await (const r of temps.subscribe()) {
  console.log(`${r.sensor_id}: ${r.celsius}°C`);
}

await node.shutdown();
```

**`NetNode.create(config)` is async — you must `await` it.** This is the first
thing that differs from Python, where construction is synchronous.

## Names and shapes

- `node.channel<T>('name', validator?)` — the named-channel surface. The
  optional validator runs on each received event.
- `subscribe()` returns an **async iterable**. Always consume with
  `for await...of`.
- `emitBuffer(Buffer)` is the zero-copy path — use it when the payload is
  already serialized.
- `ingestFire(...)` / `ingestBatchFire(...)` / `ingestRawSync(...)` are the
  lower-level ingestion entry points the channel surface sits on.

## Errors — the failure shape varies by method

Ingestion is synchronous, but there are three distinct failure conventions and
mixing them up is the standard TypeScript bug here.

| Method | Returns | On failure |
|---|---|---|
| `emit(obj)`, `emitRaw(json)` | `Receipt \| null` | **throws** |
| `emitBatch(objs)`, `emitRawBatch(jsons)` | `number` ingested | throws on shutdown; short count on `drop_*` |
| `channel.publish`, `channel.publishBatch`, `emitBuffer`, `fire`, `fireBatch` | `boolean` / `number` | returns `false` or a short count — **never throws** |

- The `| null` on `emit` is **vestigial**. The underlying napi binding throws on
  ingestion failure. Wrap in `try/catch`; do not null-check.
- For the batch forms, compare the returned count against the input length to
  detect partial drops.
- Under the default `drop_oldest` / `drop_newest` modes the throwing methods
  **do not throw on backpressure either** — drops are silent and visible only
  through `node.stats().eventsDropped`.
- `BackpressureError` and `NotConnectedError`, and the `sendWithRetry` helper,
  are **mesh-stream APIs** on `MeshNode` — peer-to-peer streams, not the bus.
  The bus emit path never raises them. See `runtime.md`.

## Shutdown

`await node.shutdown()`. Close channels before shutting the node down.

## Gaps

`bindings/coverage.md` is authoritative. The one to know up front: A2A is
`core-only` here — `serveA2a` is on `@net-mesh/core`, not the wrapper.

## Where to look when this page is not enough

- **Authoritative source:** `net/crates/net/sdk-ts/src/` — `node.ts`,
  `channel.ts`, `stream.ts`, `types.ts` — over
  `net/crates/net/bindings/node/`, where the napi surface is declared in Rust.
- **Checked examples:** `../examples/hello.ts` and `../examples/observe.ts` — the
  second pins the `bigint` counters and the boolean-returning `publish` path.
  Type-checked against the SDK source in CI; nothing proves they run.

## Never infer from another binding

- Construction is **async** here and **synchronous** in Python. `with
  NetNode(...)` has no TypeScript analogue and `await NetNode.create(...)` has no
  Python one.
- Rust has no `channel()` at all — do not port a channel example to Rust.
- Discovery is `findNodes` / `findNodesScoped` / `findServiceNodes` and returns
  a **list**. There is no `findBestNode`; that is Rust and Go only.
- The three-way return convention above is TypeScript's alone. Python raises,
  Go and C return error codes.
