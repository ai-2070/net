# Net SDK API — routing

**Load one binding companion, not five.** The five SDKs differ in ways that
change generated code — construction is async in Node and synchronous in Python,
Rust has no named-channel API at all, Go and C have no async surface — so
reading all of them to write one is both wasteful and a good way to blend two
APIs into a third that does not exist.

| Your project is in | Read |
|---|---|
| Rust | `bindings/rust.md` |
| Node / TypeScript | `bindings/typescript.md` |
| Python | `bindings/python.md` |
| Go | `bindings/go.md` |
| C | `bindings/c.md` |

**If no language is established yet, ask.** Do not default to Rust because the
substrate is Rust. And before promising any surface exists in a given language,
check `bindings/coverage.md` — the bindings are not at parity.

This page holds only what is true in every binding. Everything below applies
whichever companion you load.

## Drift check before trusting any signature

From a checkout of the Net repo, one command verifies this whole skill against
the tree — cited paths, documented symbols, CLI verbs, coverage anchors, and the
API-surface counts the companions depend on:

```bash
.github/scripts/check-skills.sh
```

If a count moves, the SDK has churned underneath this doc — re-verify from
source. **The SDK source is ground truth; this skill is a shadow copy of it.**

## The four surfaces (do not skip)

Choosing the wrong one is the most expensive mistake available here, and it is
made before any code is written.

| Surface | What it is | Available in |
|---|---|---|
| **Named channels** (`node.channel("name")` → publish/subscribe) | Topic-based pub/sub. Channel name is embedded in the payload as `_channel` and used as a subscribe filter. Subscriber roster held by the publisher. | TypeScript, Python |
| **Raw typed firehose** (`node.emit(struct)` → `node.subscribe()`) | One stream of typed events. Consumers receive everything and discriminate on the receive side. | Rust, TypeScript, Python |
| **Raw poll** (`bus.IngestRaw` → `bus.Poll(cursor)`) | Push JSON in, poll JSON out with a cursor. No async, no channels. | Go, C |
| **nRPC** (`TypedMeshRpc.serve` + `TypedMeshRpc.call`) | Typed call → typed reply, with deadlines, retries, hedging and response streaming. **A different surface from this file** — see `nrpc.md`. | all five |

Two decisions follow directly:

- **Wants topic-based fan-out, in Rust / Go / C?** There is no built-in
  named-channel API. Filter on the consumer.
- **Wants request/response — a call that returns a value?** Stop here and read
  `nrpc.md`. The bus surface has no return-value mechanism, and building one out
  of two channels is a well-worn way to reinvent nRPC badly.

## Cross-SDK gotchas

These bite regardless of language.

- **JSON everywhere.** The wire format is JSON bytes. There is no schema
  registry. The JSON either parses on the consumer or it does not.
- **Shutdown is required.** Do not rely on process exit. The ring buffer needs a
  clean drain. Each companion states its exact obligation.
- **Subscribe is hot.** A subscriber sees events emitted *after* it subscribed,
  plus whatever is still in the ring buffer. There is no replay-from-zero. If
  the user wants replay, that is RedEX or an adapter — not the bus.
- **`events_dropped` does not detect the default mode's losses.** Both counters
  sit at the *producer* boundary: they record what the bus accepted or refused
  from you, not what survived to an adapter. `drop_oldest` — the default —
  evicts to make room, so the producer always succeeds and the counter never
  moves. `drop_newest` and `fail_producer` refuse the producer, so the emit call
  and the counter agree. If you need to know that events were lost under the
  default, compare against what the adapter received downstream.
  `examples/observe.*` prints all three side by side.
- **`_channel` is reserved** in TypeScript and Python channel payloads. Do not
  put your own field there.
- **Transport is set at construction.** A node has exactly one. To bridge
  transports, run two nodes in one process and forward between them.
- **Memory transport does not deliver events.** It selects the Noop adapter,
  which counts batches and discards them (`adapter/noop.rs`: "Just count, don't
  store"; its `poll_shard` returns an empty result). Events flow producer → ring
  buffer → drain worker → adapter, so with Noop there is nothing to read:
  `subscribe()` never yields and `poll()` always returns zero. A round-trip on a
  memory node **hangs** rather than failing. Use it for construction, config,
  ingestion and backpressure work; use mesh, Redis or JetStream when a
  subscriber has to actually receive something.
- **`shards` is a parallelism knob, not a partitioning scheme.** It does not
  give Kafka-style ordered partitions; it parallelizes ingestion. The default is
  fine for most workloads.
- **Ring buffer capacity must be a power of two and at least 1024**, validated
  in the shared core config at construction. No compile or type check catches a
  bad value. The default is 1,048,576 events *per shard*, which is also why a
  snippet meant to demonstrate backpressure has to lower it — a few thousand
  events into a default node drop nothing. Each companion gives the spelling.
- **Backpressure mode accepts either casing.** `"DropOldest"` and
  `"drop_oldest"` both parse, so the Go docs' CamelCase and the TypeScript
  type's snake_case are the same thing. An *unrecognised* value is rejected
  outright rather than falling back to a default.

## When the bus surface is not enough

Out of scope for the companions — read these from source or their own chapter:

- **nRPC (request/response)** — `nrpc.md`.
- **Mesh transport configuration** (peer discovery, NAT traversal, port mapping,
  identity keys) — `mesh.md`.
- **Subnets and capability tags** — set at construction; they affect channel
  visibility. `capabilities.md`.
- **Capability discovery** — `mesh.find_nodes(filter)` /
  `find_nodes_scoped(filter, scope)` / `find_best_node(req)` for picking a peer
  by hardware, model or tag. Reserved `scope:tenant:*` / `scope:region:*` /
  `scope:subnet-local` tags narrow discovery without channel-level subnet
  routing. `capabilities.md`.
- **Permission tokens for channel auth** — `concepts.md` § Security surface.
- **RedEX / CortEX / NetDB** — persistence and queryable state. `redex.md`,
  `cortex.md`.
- **Daemons and live migration** — `patterns.md`.

## Further reading

- [Rust quickstart](https://ai2070.net/docs/sdk/rust/quickstart)
- [TypeScript quickstart](https://ai2070.net/docs/sdk/typescript/quickstart)
- [Python quickstart](https://ai2070.net/docs/sdk/python/quickstart)
- [Go quickstart](https://ai2070.net/docs/sdk/go/quickstart)
- [C quickstart](https://ai2070.net/docs/sdk/c/quickstart)
