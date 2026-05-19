# Migration Gotchas — Broker Thinking → Net Thinking

Read this when the user is migrating from Kafka, NATS, Redis Streams, Pulsar, RabbitMQ, or any other broker, **or** when their language reveals broker assumptions that won't hold in Net.

Each gotcha is a sentence the user might say, followed by what's actually true in Net, followed by what to do about it.

---

## "Where do I run the broker?"

**There is no broker.** The bus is the mesh of nodes themselves. There is nothing to provision, scale, or fail over.

What to do: explain that "the broker" maps to "the publisher's local roster + the mesh transport." If they want operational simplicity, single-process memory transport works for tests. For production, every host that participates is a node — there is no separate cluster to run.

## "How do I create a topic?"

**You don't.** A channel exists as soon as someone publishes or subscribes to a name. There is no "create topic" call, no metadata to register, no replication factor to set.

What to do: just call `node.channel("sensors/temp")` (TS/Python) and start publishing. If no one is subscribing, the publish is a no-op (zero-cost). If a subscriber later asks to join, the publisher's roster updates and they start receiving.

## "What's the partitioning strategy / partition count / partition key?"

**Net doesn't have Kafka-style partitions.** The `shards` parameter is a parallelism knob for local ingestion, not a partitioning scheme. There is no partition leader, no partition assignment, no rebalancing.

What to do: ignore the partitioning concept. If they want ordered delivery per entity, that's a per-stream property (causal chain on the mesh transport). If they want load balancing, the mesh routes by capability — they don't pick which node handles what.

## "How do I configure replication factor?"

**Net doesn't replicate the bus.** The bus is transient. If they want durability, they pick a persistence layer (RedEX, Redis, JetStream) and durability is whatever that layer provides.

What to do: ask what they actually need durability for. If "survive a node restart" → RedEX. If "survive a datacenter outage" → Redis/JetStream with their own replication. The bus itself is not the durability layer.

## "What's my consumer group?"

**No consumer groups.** Each subscriber is independent. There is no "deliver each message to exactly one of N workers" semantics built in.

What to do: if they want work-queue semantics, the publisher can implement it: maintain its own list of workers and pick one per emit. Or use a different abstraction entirely (a daemon group with deterministic identity — see `net/README.md` § Mikoshi). For most event-bus use cases, broadcast-to-all-subscribers is what they want anyway.

## "Where do I commit my offset?"

**There are no offsets** in the bus itself. Subscriptions are hot — you see what arrives after you joined. If they want offsets, they need a persistence layer (RedEX exposes per-channel monotonic sequence numbers).

What to do: ask what they're trying to recover from. "Crashed and need to resume" → RedEX or an adapter. "Just want to start where I left off after restart" → same. "Need exactly-once" → application-level idempotency (event hash dedup), not bus-level.

## "How do I scale the cluster?"

**Adding capacity = adding nodes to the mesh.** No cluster reconfiguration, no rebalancing. Removing a node triggers rerouting around it (sub-microsecond) — not an outage.

What to do: just spin up more `NetNode` instances on more hosts. Configure them to know at least one bootstrap peer; the rest is emergent.

## "How do I monitor my brokers?"

**Monitor the nodes.** Each node exposes `stats()` (events ingested, dropped, shard counts). The mesh has no centralized observability — observability is per-node.

What to do: instrument each node with whatever metrics infrastructure they use (Prometheus, OpenTelemetry, etc.). Pull stats in their own loop. There is no `kafka-consumer-groups.sh` equivalent because there's no equivalent abstraction.

## "I'm getting dropped messages — how do I fix it?"

**Drops are the design**, not a failure. The producer sent faster than the consumer (or the network) could absorb. The default `Backpressure::DropOldest` evicts the oldest entry from the ring buffer when full.

What to do: a few options.
- Tune the producer down (rate-limit at the source).
- Increase ring buffer capacity (`buffer_capacity` / `ring_buffer_capacity`).
- Add more consumer nodes — the mesh fan-out absorbs more.
- Switch to `Backpressure::FailProducer` so the producer learns about the failure and can react.
- If they need lossless delivery, switch to mesh transport with `Reliability::Reliable` per stream, or use a persistence adapter.

Drops in Net are loud-by-stats, silent-by-protocol. Check `node.stats().events_dropped`.

## "How do I do request/reply (RPC)?"

**The bus is one-way, but nRPC isn't.** The bus surface (`channel.publish` / `channel.subscribe` / `node.emit`) is broadcast pub/sub with no return value. **nRPC** is a separate convention layer that ships in the same library and adds typed request/response over the mesh — with deadlines, retries, hedging, response streaming, and end-to-end cancellation.

What to do: read `nrpc.md` and use `TypedMeshRpc.serve` + `TypedMeshRpc.call`. Don't roll your own with two channels + correlation id — that's exactly what nRPC already implements, with the resilience helpers and cross-binding contract baked in.

```rust
// Rust: feature = "cortex"
let _h = server.serve_rpc_typed("echo", |req: Req| async { Ok::<_, String>(handle(req)) })?;
let resp: Resp = client.call_typed(target, "echo", &req,
    CallOptions::default().with_deadline(Duration::from_millis(200))).await?;
```

If RPC is the **dominant** pattern (most calls, no broadcast) and the user wants a stable IDL with codegen, gRPC is still the right tool. nRPC's wire format is JSON over the mesh and there's no IDL step — typed serializers on each side are the contract. nRPC's value-add is "RPC over the same encrypted mesh you're already using for pub/sub, no separate broker / proxy / sidecar."

## "I need ordering guarantees across all consumers"

**Net only orders per-stream, not globally.** There is no global sequence number across the mesh.

What to do: ask why they need global ordering — usually it's a workaround for a stronger guarantee (consistency, idempotency) that has a better solution. If they truly need it, they need a persistence layer that imposes it (Redis Streams' monotonic ID, JetStream's sequence) — and they'll pay the broker tax.

## "How does authentication work?"

**Identity is built in.** Every node has an ed25519 keypair; that's the authentication. Channel-level auth uses signed permission tokens (see `concepts.md` § Identity, capabilities, and routing).

What to do: for a basic trusted mesh, you don't configure anything — identity is automatic. For multi-tenant or untrusted scenarios, point at `net/README.md` § Security surface for permission tokens.

## "Where are my events stored?"

**Nowhere, by default.** The bus is transient. Events live in ring buffers on the nodes they pass through, for as long as they're relevant, then they're gone.

What to do: see "I want events to survive a process restart" in `patterns.md`. Pick a persistence layer if they need durability. The default is "no storage."

## "I need exactly-once delivery"

**Net does not provide this at the bus level.** No bus does, actually — at-least-once + idempotency is the universal pattern.

What to do: include a unique event ID (hash of payload + timestamp) and have consumers deduplicate. Or use a persistence adapter that gives offsets and let consumers track what they've processed.

**If you're on the Redis transport, the dedup primitive is already there.** The Redis adapter writes a stable `dedup_id` field on every XADD entry (`{producer_nonce}:{shard}:{seq_start}:{i}`) so a producer-retry-induced duplicate has the same `dedup_id` as its original. Net ships an LRU-bounded consumer-side helper across every SDK that filters those duplicates without you maintaining the set yourself. Sizing rule of thumb: `events_per_sec × dedup_window_seconds`.

```rust
// Rust
use net_sdk::RedisStreamDedup;
let mut dedup = RedisStreamDedup::with_capacity(600_000);
if !dedup.is_duplicate(&entry.dedup_id) { process(entry); }
```
```typescript
// TypeScript
import { RedisStreamDedup } from '@net-mesh/sdk';
const dedup = new RedisStreamDedup(600_000);
if (!dedup.isDuplicate(entry.message.dedup_id)) await process(entry);
```
```python
# Python
from net import RedisStreamDedup
dedup = RedisStreamDedup(capacity=600_000)
if not dedup.is_duplicate(entry["dedup_id"]): process(entry)
```

Go (`net.NewRedisStreamDedup`) and C (`net_redis_dedup_new`) ship the same surface. The helper is local + per-consumer; on a consumer-group rebalance, call `clear()` rather than trying to share state across consumers.

## "How do I do schema evolution?"

**There is no schema registry.** Wire format is JSON. Producer and consumer must agree on the shape — typically by sharing the type definition (TS interface, Python dataclass, Rust struct) across packages.

What to do: version your event types in the payload (`{ "v": 2, "data": ... }`). Or use a versioning scheme in the channel name (`sensors/temperature/v2`). Don't try to install a registry — that's broker thinking.

## "What's the network port?"

**Mesh transport uses UDP.** The port is configurable per node (default depends on SDK version — check the mesh config). For memory transport, no port.

What to do: configure the bind address on the node (`mesh_bind` parameter or equivalent). Open the UDP port in the firewall. NAT traversal is opt-in (feature flag).

## "Can I use Net as a drop-in replacement for [my broker]?"

**No.** The mental model is different enough that direct drop-in usually produces working-but-wrong code (e.g. assuming durability that isn't there, expecting consumer groups, depending on broker-side ordering). 

What to do: walk them through `concepts.md`. Then map their use case to the recipes in `patterns.md`. The translation usually simplifies their architecture (fewer moving parts) but it isn't mechanical.
