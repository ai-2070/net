# Net Concepts — the Mental Model

This file exists because the Net API looks like Kafka/NATS/Redis Streams but the underlying model is fundamentally different. If you generate code without internalizing these concepts, you'll write something that compiles, runs, and is wrong in subtle ways (the worst kind of wrong).

Read this **once per session** before writing any integration code. It is roughly a 5-minute read.

---

## What Net is, in one sentence

Net is a latency-first encrypted mesh where every device is a peer, the event bus is the mesh itself (not a process you connect to), and overload is handled by dropping rather than queueing.

It's an *engineering take* on the Net concept from Cyberpunk 2077 — flat topology, encrypted hop-to-hop, every device a first-class citizen — built as a Rust library with bindings in TypeScript, Python, Go, and C. Not affiliated with CD Projekt Red.

---

## Node

**A node is the unit of participation.** Every running instance of the SDK — a CLI, a daemon, an embedded device, a test process — is a node. There is no "client SDK" and no "broker process." `NetNode` (TS/Python) and `Net` (Rust/Go/C) are the same primitive. A node can publish, subscribe, relay, and persist all at once.

A node is identified by an **ed25519 keypair**. The public key is the node ID. The private key is the authority to act as that node. Identity is cryptographic, not topological — a node keeps its identity when it changes IPs, traverses NAT, or roams between networks.

**Practical implication:** when you instantiate a node in your application, you are joining the mesh, not connecting to it. There is no server on the other end. Two processes that both create a `NetNode` on the same machine are already two nodes; if they share a transport (memory, mesh, Redis, JetStream), they can already communicate.

## Channel

**A channel is a name, not a thing.** This is the single most important concept to internalize.

In Kafka, a topic is a thing — a partitioned log living on a broker cluster, with retention policy, replication factor, and a leader per partition. In NATS, a subject is also a thing — the broker holds the subscription registry and routes messages.

In Net, a channel is just a *name to match on*. There is no broker holding a subscription registry. The publisher holds the subscriber roster directly. A subscriber asks the publisher (or any reachable node) to be added to the roster for channel name X. When the publisher emits, it does N per-peer unicasts to every roster member. On the **mesh** transport those unicasts ride already-encrypted sessions end-to-end; on **memory** there is no wire; on **Redis / JetStream** the payload sits in plaintext at the broker and transport security is whatever you configured for that system (TLS, etc.). See the "Encryption" section below for the full picture.

Consequences:
- **Publish-without-subscribers is a no-op.** The roster is empty, the fan-out loop runs zero times. No buffer fills up at a broker, because there is no broker.
- **Channel creation is implicit.** No "create topic" API call. A channel exists as soon as someone publishes or subscribes to a name. There is no central registry to update.
- **Channels cost nothing when idle.** A channel with zero subscribers consumes zero resources mesh-wide. There's no metadata to maintain.
- **Channels with thousands of subscribers work.** They just fan out more packets. The cost is linear in subscriber count, paid by the publisher node.

The named-channel API exists in TS and Python (`node.channel("name")`). The other SDKs do not have it, and they don't all replace it the same way:

- **Rust** has a typed firehose: `node.emit(&MyType)` and `node.subscribe_typed::<T>()`. Consumers receive every event of that type and filter on payload content.
- **Go and C** have neither named channels nor a typed firehose. They use raw JSON ingest + cursor-based polling: `bus.IngestRaw(json)` / `bus.Poll(limit, cursor)` (Go) and `net_ingest_raw` / `net_poll_ex` (C). Consumers parse the JSON in their own loop.

(See `apis.md` for the per-SDK code.)

## Subscriber

A subscriber is a node that has joined a channel's roster. Subscriptions are **hot, not cold** — a subscriber receives events emitted *after* it subscribed, plus whatever is still in the publisher's local ring buffer at the moment it joined. There is no "replay from the beginning of time" semantic.

If the user wants durable replay from a specific offset, they need a persistence layer (RedEX, Redis adapter, or JetStream adapter). That is a deliberate, separate decision — not a default.

If a subscriber goes silent (overload, crash, network partition), the publisher's failure detector evicts it from the roster. No in-band error message. The subscriber's neighbors observe the silence and the mesh routes around it.

## Publisher

A publisher is a node that holds a roster and emits to it. Publisher and subscriber are not separate types — they are roles a `NetNode` plays per channel. The same node can publish on `sensors/temp` and subscribe to `commands/actuator` simultaneously.

When a publisher emits, the call returns a `Receipt` (or null/error under backpressure). The receipt confirms the event was accepted into the local ring buffer for fan-out — it does **not** confirm delivery to all subscribers. Net does not guarantee delivery; it guarantees fast attempts.

## Backpressure: silence, not a signal

In TCP, backpressure is negotiated. The receiver advertises a window, the sender respects it, congestion control slows everyone down. Round trips are involved.

In Net, backpressure is **immediate and unilateral**. A node that can't keep up stops processing. Its ring buffer is full, so new data either evicts the oldest entry (`DropOldest`, default) or gets dropped at the boundary (`DropNewest`), or the producer's emit call fails (`FailProducer`). The slow node does not tell anyone — it just goes silent on that stream.

Silence propagates through the proximity graph. Neighbors observe the missed heartbeat within a heartbeat interval, mark the node as degraded, open the circuit breaker, and **route new traffic to other capable nodes**. The sender does not slow down. The mesh has other nodes.

**Practical implication for the SDK user:**
- `emit` / `publish` may silently lose data under backpressure. Check return values: TS returns `null`, batch APIs return ingested counts, Rust returns `Result`.
- "Lossy by default" is the design. If the user needs reliability, they need either `Reliability::Reliable` per-stream (mesh transport), an explicit persistence layer, or end-to-end acks at the application level.
- Don't add timeouts and retries against the bus. Backpressure handling is the bus's job.

## Ring buffer

The local data structure on each node is a fixed-capacity sharded ring buffer. It is **a speed buffer, not a waiting room.** When full, old data is evicted or new data is dropped. There is no unbounded growth.

Capacity is configurable per node (`buffer_capacity` / `ring_buffer_capacity`). **Default: `1 << 20` = 1,048,576 events per shard** (`net/crates/net/src/config.rs:46-60`), must be a power of 2 and ≥ 1024. Tune up if you have bursty traffic and want to absorb more before dropping; tune down if memory is the constraint (each slot is roughly the size of one event payload + header).

Sharding (`shards` / `num_shards`) is the parallelism knob — more shards = more parallel ingestion at the cost of more memory and more drain workers. **Default: `std::thread::available_parallelism()`** (`net/crates/net/src/config.rs:660-664`), which is logical parallelism — counts SMT/hyperthreads, not just physical cores. (The doc-comment on the field still says "physical core count"; the implementation is what runs.) Memory cost is `num_shards × ring_buffer_capacity × avg_event_size`, so the defaults can land in the GB range on a 16-thread box with 1KB events — drop `buffer_capacity` first if that's a problem. The other defaults: `backpressure_mode = DropOldest`, `adapter_timeout = 30s`, `adapter_batch_retries = 0`.

## Transport

A node's transport is what physically moves bytes between nodes. In TS, Python, and Rust, the same publish/subscribe code works across all transports — **the choice is at node construction, not in your application logic.** This is the second most important concept after "a channel is a name." (The Go and C bindings are poll-based and currently expose a smaller, more transport-coupled surface — switching transports there may require code changes; see `apis.md`.)

| Transport | When to use | Notes |
|---|---|---|
| **Memory** (default) | Single process, multiple components, tests | No network, no encryption, no persistence. Pure in-process ring buffer. Fastest. |
| **Mesh** (UDP, peer-to-peer) | Production multi-host deployments | Encrypted (Noise + ChaCha20-Poly1305), no broker, NAT traversal opt-in, automatic rerouting. The "real" Net. |
| **Redis** | When you already run Redis and need cross-process pub/sub with optional persistence | Net publishes to Redis Streams; subscribers read from them. You get Redis's durability semantics. |
| **JetStream** (NATS) | When you already run NATS JetStream and want its durability/retention | Same idea as Redis adapter, different backend. |

Selection happens via constructor parameters — see `apis.md`. **A node can have only one transport at a time** (it's set at construction). To bridge transports, run two nodes in the same process.

In TS, Python, and Rust, application code does not know which transport it got. Code written for the memory transport runs unmodified on mesh transport. This is the "location-transparent consumption" property — the call signature for `publish` is identical whether the subscriber is in the same process or five hops away on a different continent. (Go and C are an exception: their poll-based surface and current binding shape can require explicit changes when switching transports — `NewMeshNode` for mesh, etc.)

## Encryption (for mesh transport)

Every packet on the mesh transport is encrypted **end-to-end** between source and destination using ChaCha20-Poly1305 with counter-based nonces, after a Noise NKpsk0 handshake. Intermediate relay nodes forward encrypted bytes they cannot read.

Practical implications:
- Relay nodes do not need to be trusted with payload content. They are part of the routing path, not the trust path.
- A compromised relay leaks nothing — session keys are between source and destination, never shared with relays.
- The encryption is invisible to your application code. You don't configure it; it's how the mesh transport works.

For memory transport there is no encryption (it's intra-process). Redis and JetStream transports rely on those systems' transport security (TLS, etc.) — the payload is plaintext at the broker, unlike mesh.

## Identity, capabilities, and routing (brief)

Out of scope for most event-bus integrations, but you should know the words:

- **Identity:** the ed25519 keypair that defines a node. Persists across IP changes, NAT traversal, network roaming.
- **Capabilities:** a node announces what it can do — hardware (CPU, GPU, RAM), loaded models, installed tools, operator tags (`region:us`, `env:prod`). The mesh routes based on capabilities, not just node IDs. Relevant if the user wants a channel to land on "any node with a GPU" rather than a specific node.
- **Subnets:** application-derived boundaries from capability tags. A `SubnetLocal` channel only delivers to peers whose tags map to the same subnet. Used for "dev nodes can't see prod data" — a one-line policy, not a firewall rule.
- **Scoped discovery (`scope:*` tags):** a separate, query-side mechanism for narrowing *who finds whom*. A provider tags itself `scope:tenant:oem-123`, `scope:region:eu-west`, or `scope:subnet-local`; consumers call `find_nodes_scoped(filter, scope)` to get only the matching pool. Wire format and forwarders are unchanged — strictly a discovery filter, not a routing gate. Useful for per-tenant GPU pools and regional rendezvous selection without standing up separate subnets.
- **Permission tokens (root-anchored token chains):** a channel registered with `token_roots` enforces auth. A presented credential is a `TokenChain` — honored only if it **roots** at one of the channel's `token_roots`, **binds** at its leaf to the subscribing peer's `EntityId`, and **authorizes** the action (`SUBSCRIBE` / `PUBLISH`) at every link. The chain travels the wire on subscribe (`subscribe_channel_with_chain`, surfaced as `SubscribeOptions.token` on the SDK) and the publisher re-checks expiry + revocation while the subscription lives. A **delegated** publisher (granted via `owner → org → this_node`) must install its full chain locally with `MeshNode::set_publish_chain(channel, chain)` at startup, or its publishes fail the root-anchor check; **direct-issued** publishers (the common case) need no change. Revocation is immediate (a root dropped from `token_roots` revokes inline). A capability announcement's wire `node_id` is bound to the verified signer, so a forged `node_id` is rejected with `WireError::NodeIdMismatch`.

If the user is just publishing and subscribing on a single trusted mesh, you don't need any of this. If they ask "how do I keep dev events out of prod?" or "can I require auth for this channel?" — point at `net/README.md` § Capabilities, Subnets, and Security surface.

## nRPC: request/response on top of the bus (brief)

The bus is **broadcast pub/sub** — fire-and-forget, no return value, hot subscribers. **nRPC** is a separate convention layer that turns a directed channel pair (`<service>.requests` / `<service>.replies.<caller_origin>`) into a typed request/response surface with deadlines, queue-group fan-out, response streaming, and end-to-end cancellation.

The two surfaces share the same encrypted mesh transport but have different costs and contracts:

- **Bus**: zero-cost when idle, broadcast to all subscribers, no return value, drop-on-overload.
- **nRPC**: per-call cost (one extra subscription per `(service, target)` pair, lazily created and reused), typed call → typed reply, deadlines + retry/hedge/circuit-breaker as resilience helpers.

When the user describes a task, decide which surface fits:

| Need                                                     | Surface     |
| -------------------------------------------------------- | ----------- |
| Broadcast event to N subscribers, no reply               | **Bus**     |
| Hot subscriber that sees events emitted after join       | **Bus**     |
| Typed call → typed response                              | **nRPC**    |
| Deadline + retry/hedge                                   | **nRPC**    |
| Server emits a stream of chunks for one request          | **nRPC**    |
| Cancel mid-flight                                        | **nRPC**    |
| Persistence / replay                                     | **Bus + RedEX/adapter** |

If it's nRPC, point at `nrpc.md` — the full surface (Rust `Mesh::serve_rpc_typed` / `call_typed`, TS / Python `TypedMeshRpc`, Go reference cgo wrapper, error model, status codes, resilience helpers, cross-binding contract) lives there.

## Persistence (brief)

The bus itself is **transient by design**. Events flow through ring buffers; if no one consumes them in time, they are gone. This is not a bug — it is the property that makes nanosecond-scale operation possible.

If the user needs durability, they choose how:
- **Redis transport** — events go to Redis Streams with the broker's durability semantics.
- **JetStream transport** — events go to NATS JetStream with its retention policies.
- **RedEX** (memory transport + local append-only log) — per-channel local file (`<base>/<channel_path>/{idx,dat}`), per-node retention policy, no cluster, no consensus. Each node decides what to keep.
- **CortEX** — RedEX, folded into a queryable in-memory state. For tasks/memories/anything you'd otherwise put in SQLite.
- **NetDB** — a query façade over CortEX state with Prisma-style methods.

For a basic event-bus integration these are usually unnecessary. Point at them if the user asks for "exactly-once delivery", "replay from offset N", or "durable subscription" — those are RedEX/persistence concerns, not bus concerns.

## What Net is not

Common analogy traps. If the user is using these mental models, gently redirect:

- **Not a broker.** No central process, no cluster to provision. The mesh is the bus.
- **Not Kafka.** No partition leader, no consumer groups, no offset commits, no replication factor.
- **Not a database.** The stream is transient unless you opt into RedEX or an adapter.
- **Not a service mesh.** No sidecars, no service discovery via DNS, no mTLS configuration. Identity is built in.
- **Not actor model.** Daemons exist (see Mikoshi) but the basic publish/subscribe model is just typed pub/sub, not message-passing-with-mailboxes.
- **Not best-effort UDP.** The mesh transport adds encryption, ordering (optional, per stream), causal chains, and automatic rerouting on top of UDP.
- **Not gRPC** — but nRPC fills the same niche. See `nrpc.md` if the user wants typed call → typed reply with deadlines + cancellation. The wire format is JSON over the mesh, not protobuf over HTTP/2; there is no mandatory IDL — just typed serializers on each side. (An *optional* `net-mesh typegen` codegen path lowers a discovered AI tool's JSON Schema to TS interfaces / Pydantic models plus a typed call helper; the wire stays schemaless JSON, so codegen is a convenience, not a requirement. See `nrpc.md`.)

---

When you've internalized this, go to `apis.md` for the actual code patterns, and `patterns.md` for task-shape recipes.
