---
name: net-event-bus
description: "Use this skill when the user is integrating the Net library (`@net-mesh/sdk`, Rust `net-sdk`, Python `net-sdk`, Go `net` binding, or C `net.h`) as an event bus, for nRPC request/response over the mesh, or for the RedEX / CortEX / Dataforts persistence + folded-state + caching layers on top — anything involving publishing to or subscribing from a Net channel, wiring a producer/consumer/relay against the Net SDK, calling a service via `serve_rpc` / `call_typed` / `TypedMeshRpc`, opening a `RedexFile` for durable append-only logs, building a CortEX adapter / NetDB query surface over folded state, enabling greedy-LRU caching / data gravity / blob refs / read-your-writes, or migrating from Kafka/NATS/Redis Streams/Pulsar/gRPC. Also covers the **gang-claim scheduler** (atomically claiming a contended exclusive resource — a GPU island / accelerator slot / licensed seat — under competition, without double-booking across a partition) and the **task-lifecycle / workflow** layer on top. Triggers on imports of those packages and on phrases like 'use Net for events', 'pub/sub with Net', 'wire up a Net channel', 'Net subscriber', 'Net publisher', 'nRPC', 'mesh RPC', 'request/reply over the mesh', 'RedEX', 'CortEX', 'NetDB', 'Dataforts', 'greedy cache', 'data gravity', 'BlobRef', 'WriteToken', 'wait_for_token', 'gang scheduler', 'claim an island', 'match islands', 'reserve island', 'publish island topology', 'contended GPU / resource claim', 'WorkflowAdapter', 'task lifecycle', 'fan-out/fan-in shards', 'trigger engine'. Also covers **event-representation doctrine** — an event on the bus is a fact observed at one layer, not an end-to-end success/acknowledgement (transport success ≠ application success ≠ business success; 'HTTP 200 is not a business invariant'); distinct outcomes are distinct events; transport truth belongs to the transport, not the payload. Triggers on 'how should I name events', 'event carries status/200/ok/delivered', 'should an event mean success', 'one status event vs a sequence of events', 'is a delivery receipt proof the effect happened'. Also covers the **MCP bridge** — expose an existing stdio MCP server's tools as owner-scoped mesh capabilities (`net wrap`), or front the mesh's capabilities to a local MCP host (Claude Code / Cursor) as a stdio MCP server (`net mcp serve`) with fail-closed consent, human-approved pinning (`net mcp pin`), and credential locality (secrets never transit). Triggers on 'MCP', 'net wrap', 'net mcp serve', 'net mcp pin', 'wrap an MCP server', 'bridge MCP over the mesh', 'expose mesh tools to Claude Code / Cursor', 'pin a capability', 'capability federation'. Skip for unrelated event-bus work or for editing Net's own internals."
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
metadata:
  skill-version: 1.3.0
  net-version: 0.31.0
  last-updated: 2026-07-04
---

# Net as an Event Bus

Net is **not Kafka**. It is not NATS, not Redis Streams, not Pulsar. The API surface looks superficially similar (publish, subscribe, channels) but the underlying model is different in ways that will produce wrong code if you assume it's just another broker.

**Before you write or edit any integration code, read `concepts.md` in this skill directory.** It is the conceptual prerequisite for everything else. The API templates in `apis.md` will look identical to a dozen broker SDKs and you will write something that compiles and runs and is wrong.

## How to use this skill

You have several reference files in this directory. Load them on demand — do not read them all up front.

| File | Read when |
|---|---|
| `concepts.md` | **Always** — before writing any integration code. The mental model. ~5 min read. |
| `apis.md` | When generating actual code. Verified per-SDK templates for publish, subscribe, lifecycle. |
| `patterns.md` | When the user describes a task ("I need a relay", "I need persistence", "I need fan-out across machines"). Maps tasks to recipes. |
| `mesh.md` | When the user is deploying multi-host. Production transport recipe — PSK / identity bootstrap, peer discovery, NAT traversal toggles, port mapping, 2-node and 3-node working configs. |
| `capabilities.md` | When the user wants to route to "the GPU node" or "a node that has model X loaded". `find_nodes` / `find_best_node` / scope filters. The differentiator vs Kafka/NATS/Redis. |
| `scheduler.md` | When the user wants to **atomically claim a contended exclusive resource** under competition (a GPU island / accelerator slot / licensed seat) without double-booking across a partition — `publish_island_topology` / `match_islands` / `reserve_island` / `claim_island` — and/or **drive a task lifecycle** on top (`WorkflowAdapter`, shards, triggers). Contended arbitration + the workflow layer, distinct from advisory capability routing. |
| `streams.md` | When the user needs ordered point-to-point delivery (large payloads, telemetry-to-one-peer, credit-grant backpressure). Per-peer streams — different surface from the bus despite overlapping vocabulary. |
| `nrpc.md` | When the user needs **request/response** (typed call → typed reply, deadlines, retries, hedging, response streaming). Separate convention layer on top of the bus — don't reach for it for fire-and-forget broadcast. Also covers ingress/egress batching (`batched_ingress`) and the optional `net-mesh typegen` codegen path for discovered AI tools. |
| `mcp.md` | When the user wants to **bridge Model Context Protocol tools over the mesh** — wrap an existing stdio MCP server as owner-scoped mesh capabilities (`net wrap`), or expose the mesh's capabilities to a local MCP host (Claude Code / Cursor) via `net mcp serve` with fail-closed consent + human-approved pinning (`net mcp pin`). Rides on `capabilities.md` (discovery) + `nrpc.md` (invoke); credentials stay local. |
| `redex.md` | When the user needs **durable per-channel append-only logs** ("survive a node restart", "replay from offset N", "tail this channel with retention"). Local files per node; cross-node replication is opt-in per file. |
| `cortex.md` | When the user needs **folded queryable state** ("SQLite-shaped queries on the event stream", "react to changes in derived state", `Tasks` / `Memories` / custom adapters, NetDB cross-adapter query façade). Sits on top of RedEX. |
| `dataforts.md` | When the user asks about **greedy caching**, **data gravity** (chains drift toward readers), **blob refs** (substrate carries content-addressed pointers; bytes in S3 / Ceph / IPFS / FS), **read-your-writes** (`WriteToken` + `wait_for_token` so a producer reads its own write deterministically), or **peer-to-peer blob/dir transfer over the mesh** (`fetch_blob` / `store_dir` / `fetch_dir`, the `net-mesh transfer` CLI). Compositional data plane on top of RedEX + CortEX. |
| `runtime.md` | When writing a `shutdown` path, handling errors, integrating into an existing async runtime (axum, FastAPI, Express), or debugging "why are my events missing?" |
| `observability.md` | When the user asks "how do I know events are being dropped?" or wires Prometheus/OTel. Stat fields per SDK, the silent-drop trap, tuning knobs. |
| `payloads.md` | When the user is shaping their event schema or asking about size limits, large blobs, batching, or cross-language schema interop (u64/BigInt edges, casing, optional/null, schema evolution). |
| `filter-dsl.md` | When the user wants a subscriber to receive **only some** events on a channel (equality predicates, `$and`/`$or`/`$not`, dot-paths) — the bus-side filter. Not to be confused with capability predicates (`capabilities.md`), which select *nodes*, not payloads. |
| `error-codes.md` | When the user needs to **classify a specific error variant** (`TokenError::Revoked`, `TagMatcherError::RegexNotBuiltIn`, `ScalingError::ShardInUse`, `StreamError::Reset`, `RpcError::NoMatchingServer`) to decide retry vs. drop vs. re-auth vs. fix-a-bug. The fuller core-crate + subsystem taxonomy under `runtime.md`'s SDK-facing errors. |
| `cli.md` | When the user wants the `net-mesh` command surface — `transfer` (blob/dir `recv`/`send`/`ls`/`status`/`cancel`) and `typegen` (`generate`/`snapshot`/`diff`) — plus exit codes and scripting notes. |
| `testing.md` | When writing unit/integration tests against the SDK. Covers fixtures, race conditions, CI gotchas. |
| `gotchas.md` | When the user is migrating from Kafka / NATS / Redis Streams / Pulsar, or when their question reveals broker-thinking. |
| `event-semantics.md` | When the user is deciding **what an event should assert** — naming events, or shaping payloads that carry success/acknowledgement meaning (`x.ok`, `write.done`, `status: 200`, `delivered: true`) instead of stating a fact. The doctrine: an event is a fact observed at one layer, not an end-to-end "OK." Transport success ≠ application success ≠ business success. |
| `examples/` | When the user is starting from scratch — minimal, runnable hello-world for each SDK. Use as the first thing they run after install, before they write application code. |

## TL;DR mental model (the absolute minimum)

If you remember nothing else from `concepts.md`, remember these five things — they are what makes Net different from every other bus:

1. **There is no broker.** A channel is a name, not a process. The publisher holds the subscriber list. Fan-out is N per-peer unicasts. On the **mesh** transport those unicasts ride already-encrypted sessions end-to-end; on **memory** there is no wire at all; on **Redis / JetStream** payloads sit in plaintext at the broker and rely on that system's TLS for transport security. "The bus" is the mesh of nodes themselves; nothing to provision, scale, or fail over.

2. **Backpressure is silence, not a signal.** Overloaded nodes drop packets and stop responding. They do not tell the sender. Neighbors detect the silence within a heartbeat and the mesh routes around them. Producers do not slow down — the mesh finds a different consumer.

3. **Subscribers are hot, not cold.** A subscriber sees events emitted *after* it subscribed (plus whatever's still in the ring buffer). There is no replay-from-beginning. If you need durable replay, you need a persistence layer (Redis adapter, JetStream adapter, or RedEX) — that's a separate decision, not a default.

4. **Every node is a peer.** No clients, no servers. Producer and consumer are the same primitive (`NetNode` / `Net`). A node can publish, subscribe, relay, and persist all at once.

5. **In TS, Python, and Rust, the transport is a runtime choice, not a code change.** Same publish/subscribe code works on memory, mesh, Redis, or JetStream — you pick at node construction. **Go and C are the exception** (poll-based, transport-specific constructors). See `concepts.md` § Transport for the full picture before designing for portability.

If the user's design language conflicts with any of these (e.g. "the broker", "the cluster", "consumer group", "partition leader"), stop and read `gotchas.md` — they're carrying assumptions from another system that will break here.

## Workflow when integrating

1. **Identify the language** — Rust, TypeScript, Python, Go, or C. There are no other bindings.
2. **Read `concepts.md`** if this is your first invocation in the session.
3. **If the user is starting from scratch**, run the matching script in `examples/` first. Confirm the SDK is installed and working before writing application code.
4. **Clarify the task shape** — single-process or multi-host? Channels (named topics) or raw firehose? Need persistence? Need typed payloads? Read `patterns.md` for the recipe that matches.
5. **Pick the transport** — memory (single process), mesh (peer-to-peer over UDP), Redis, or JetStream. `concepts.md` covers the trade-offs. Default to `memory` for single-process tests and `mesh` for production. **For mesh production deploys, read `mesh.md`** — PSK / identity bootstrap, peer discovery, NAT-traversal opt-ins.
6. **Generate code from `apis.md`** — these templates are verified against the SDK source. Adapt the payload type and channel name; do not invent new methods.
7. **If the task is "route to a specific kind of node"** (GPU box, machine with model X loaded, particular tenant), read `capabilities.md`. Capability filters replace topic-based routing for placement.
7a. **If the task is "atomically claim a contended exclusive resource"** (N jobs want the same GPU island / accelerator slot / licensed seat; must not double-book), read `scheduler.md`. This is contended *arbitration* (exactly-one-winner CAS), not advisory capability routing — and it carries the task-lifecycle (`WorkflowAdapter`) layer that runs on top of a held resource. Resource-agnostic: GPU specifics ride plain tags.
8. **If the task is "ordered point-to-point" or "large payload with backpressure"**, read `streams.md`. The bus is fan-out + transient; per-peer streams are the right primitive when you need order + credit-grant flow control.
9. **If the task is "request → typed reply" / "RPC over the mesh" / "I need a deadline + retry"**, read `nrpc.md`. The bus has no return-value mechanism; nRPC adds typed call / serve, deadlines, response streaming, and end-to-end cancellation as a convention layer on top. Don't reach for it for fire-and-forget broadcast.
9b. **If the task is "bridge MCP" / "wrap an MCP server" / "expose mesh tools to Claude Code / Cursor" / "pin a capability"**, read `mcp.md`. `net wrap` publishes a local stdio MCP server's tools as owner-scoped mesh capabilities (secrets stay in the child process, never on the wire); `net mcp serve` fronts the mesh to a local MCP host with fail-closed consent and human-approved pinning (`net mcp pin`). It rides on capabilities + nRPC — use the CLI/adapter, don't hand-roll it from those primitives, and don't trust a wire-declared credential status.
10. **Wire the lifecycle correctly** — read `runtime.md` for the shutdown contract and async-runtime integration before plugging into the user's existing app. Always add a `shutdown` path. The ring buffer needs a clean drain.
11. **Handle errors per `runtime.md`** — `Backpressure` is the only retry-safe error; everything else indicates state change, bug, or config issue. For a variant this skill's prose doesn't explain (`TokenError::*`, `TagMatcherError::*`, `ScalingError::*`, `StreamError::*`, `RpcError::*`, the core `IngestionError`/`ConsumerError`/`AdapterError` trio), read `error-codes.md` — the full taxonomy with per-variant remediation.
11a. **If the task is "the consumer should only see *some* events"** (by payload content), read `filter-dsl.md`. Equality-only `$and`/`$or`/`$not` predicates evaluated post-retrieval; a missing path and an empty `$and`/`$or` both match *nothing*. This is content filtering — for selecting *which node* answers, that's the capability predicate in `capabilities.md`.
12. **Shape the payload using `payloads.md`** — small JSON events on the bus, references for large blobs, batched events for telemetry streams. The Schema-interop section covers cross-language traps (u64/BigInt, casing, optional/null, schema evolution).
13. **Wire observability per `observability.md`** — under default backpressure modes, drops are silent. Always alert on `events_dropped`. The file lists stat fields per SDK and Prometheus/OTel wiring patterns.
14. **Write tests using `testing.md`** — memory transport, two in-process nodes, subscribe-before-publish, clean shutdown in teardown.
15. **If the user is migrating** from Kafka / NATS / Redis Streams / Pulsar, read `gotchas.md` first — broker assumptions will produce broken-but-compiling code.
15a. **If the user is naming events or shaping event payloads** — especially if an event carries a success/acknowledgement meaning (`x.ok`, `write.done`, `status: 200`, `delivered: true`) instead of stating a fact — read `event-semantics.md`. An event asserts one fact observed at one layer, not an end-to-end "OK"; distinct outcomes are distinct events; transport truth belongs to the transport (`Receipt` / `Reliable` / RedEX cursor), not the payload. `HTTP 200 is not a business invariant`.
16. **If you're unsure about an API**, read the SDK source directly:
   - Rust: `net/crates/net/sdk/src/` and `net/crates/net/sdk/examples/`
   - TypeScript: `net/crates/net/sdk-ts/src/`
   - Python: `net/crates/net/sdk-py/src/net_sdk/`
   - Go: `go/`
   - C: `net/crates/net/include/net.h`
   These are authoritative. The README is a good intro; the source is ground truth.

## What this skill deliberately does not cover

The event-bus surface is a small slice of Net. The following exist but are out of scope here — they have their own primitives and would bloat this skill:

- **Daemons / Mikoshi (live state migration)** — stateful event processors that move between nodes carrying their causal chain. Read `net/README.md` § Daemons + Mikoshi if the user asks.
- **CortEX / NetDB (queryable folded state)** — local materialized views over RedEX logs. Read `net/README.md` § CortEX + NetDB.
- **Subprotocols** — custom protocols deployed incrementally over the same mesh. Out of scope unless the user is building one.
- **Subnets, identity/permission tokens (full surface)** — covered briefly in `concepts.md` and `capabilities.md` because subnet scope filters and token-gated channels shape channel visibility, but the full identity / token issuance + delegation surface is a separate concern. Point at `net/README.md` § Security surface.
- **Mesh transport internals** — packet codec, Noise handshake, routing-table internals. Point at `net/crates/net/docs/`. The application-level mesh setup is in `mesh.md`; capability routing on top of the mesh is in `capabilities.md`; per-peer streams over the mesh are in `streams.md`.

If the user asks about these, point them at the relevant section of `net/README.md` rather than guessing.
