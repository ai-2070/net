# What is Net?

Net's flagship use is **agentic capability federation** — agents discovering,
invoking, observing, and recovering work across a trusted mesh. If you want that
story first, start with [The Agentic Mesh](/docs/worldview/agentic-mesh). This
page is the mechanism underneath it: what Net actually *is* as a system.

Net is a distributed event bus that runs as a peer-to-peer mesh — no broker to provision, no central service to depend on. Producers and consumers connect directly through the mesh, and every event they exchange is encrypted end-to-end, signed by its origin, and ordered by cause rather than by clock.

If you've used Kafka, NATS, or Redis Streams, the surface will feel familiar: you publish to a channel, subscribe with a filter, replay from a cursor. What changes is everything underneath. There is no broker process to operate, there are no partitions to rebalance, and the identity and routing layers that normally live in a separate service mesh are folded into the bus itself.

## The shape of it

```
your code  ─►  channel  ─►  mesh  ─►  channel  ─►  your code
```

A channel is a named endpoint — something like `vehicles/fleet-7/telemetry`, or `chat/lobby`, or `metrics/$node` — and it's the only abstraction you reach for in day-to-day code. You publish to it; anyone authorized subscribes to it. The mesh figures out how to get bytes from one side to the other, and Net's identity layer decides who is allowed to participate.

Channels carry events, and each event has three things attached to it: an identity that says who produced it, a causal lineage that places it in relation to everything that came before, and a payload that's whatever your code put there. The mesh routes events to subscribers, and when you ask it to, it persists them as well.

That's the whole surface area. Everything else in Net — durable logs, materialized views, federated queries, distributed daemons — is built on top of channels and events.

## What you get

At the core of the bus you get pub/sub over hierarchical channel names with visibility scopes (local, parent-visible, exported, global), wire-speed authorization in which the packet header itself is enough to make routing and access decisions without decrypting the payload, causal ordering so that out-of-order delivery is recoverable rather than catastrophic, and encrypted transport using Noise NKpsk0 handshakes, ChaCha20-Poly1305 frames, and ed25519 identities — none of which you configure by hand.

Built on top of the bus you'll find a stack of optional layers that share the same channel and identity primitives:

- **RedEX** turns a channel into a durable append-only log you can subscribe to from any cursor and replay deterministically.
- **CortEX** runs reductions over those logs to materialize views, react to changes, and answer queries against folded state.
- **NetDB** exposes a single query surface that federates across channels, nodes, and chains.
- **Dataforts** provides content-addressed blob storage with a greedy LRU cache and gravity-based placement, so large payloads live near the code that reads them.
- **MeshOS** runs long-lived stateful daemons that are placed by capability and migrate between nodes without losing causal continuity.
- **nRPC** layers typed request/response semantics on top of channels, so you can call a service the same way you'd publish to a topic.

You opt into the layers you need; the ones you don't compile away entirely.

## How Net thinks

The system is built in three layers, and you can reason about each one without the others.

**Transport** is the bottom: encrypted bytes between nodes, mesh-routed, with NAT traversal and connection management handled for you. It's deliberately invisible — if you find yourself thinking about it, something is wrong.

**Identity** sits in the middle and answers three questions about every packet on the wire: who sent it, what channel it claims, and what that sender is allowed to do. Identity lives in the 64-byte packet header so that forwarding nodes can make the call in a single cache line, without ever touching the payload.

**State** is the layer you actually program against. Channels carry causally-ordered events; persist a channel and it becomes a durable log; fold the log and it becomes the state your application reads from. Everything else — RPC handlers, materialized views, distributed workers — is some variation on that pattern.

That's the entire mental model. The rest is API.

## When to use Net

Net fits when you need durable pub/sub but don't want to operate a broker, when your system spans devices or edge nodes or services that come and go and need to find each other, when you'd otherwise be gluing together a message bus and an identity service and a CRDT library to build event-sourced state, or when you need stateful workers that survive node failure and migrate cleanly.

It's less of a fit when what you really want is a SQL database (use Postgres), or a one-shot HTTP request/response API with no event semantics (use HTTP), or a total order across unrelated channels — Net orders events within a channel, not across the mesh.
