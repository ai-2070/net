# Architecture

Net is built in three layers, stacked from the bytes on the wire up to the state your application reads. Each layer is independently understandable, and the boundaries between them are sharp on purpose — a problem at one layer doesn't reach into the others, and a change at one layer doesn't ripple up or down.

```
┌─────────────────────────────────────────────────┐
│  State      events, causality, folds, queries   │
├─────────────────────────────────────────────────┤
│  Identity   who, what channel, what allowed     │
├─────────────────────────────────────────────────┤
│  Transport  encrypted bytes, routing, NAT       │
└─────────────────────────────────────────────────┘
```

You work mostly at the top. The bottom two layers are visible only when something goes wrong, or when you're tuning, or when you're writing a new adapter — and even then, each layer has a small, well-defined surface.

## Transport

The transport layer moves encrypted bytes between nodes. It's a UDP-based mesh protocol with a 68-byte header, 8-byte aligned, encrypted with a Noise NKpsk0 handshake and ChaCha20-Poly1305 frames. Forwarding nodes can route a packet by reading the header alone — they never decrypt the payload.

The protocol is designed so that the routing decision, the access-control decision, and the deduplication decision all fit inside that one cache line. That's how Net sustains the throughput it does: nothing on the hot path requires a system call beyond `recvmsg`, and nothing requires a cryptographic operation beyond the one-time AEAD verify on packets destined for the local node.

Above the wire format, transport handles connection setup, session keys, congestion control, NAT traversal (reflex probes, classification, hole-punching rendezvous), and the failure detector that decides when a peer has gone away. None of this surfaces in the application API. You configure listen addresses and initial peers; the mesh forms.

## Identity

The identity layer answers three questions about every packet on the wire: who sent it, what channel it's claiming, and whether the sender is allowed to use that channel. Identity is bound to keys, not addresses — an entity is its ed25519 public key, and that key follows the entity if it migrates to a different node.

Every packet header carries an 8-byte `origin_hash` that's a domain-separated BLAKE2s-MAC of the sender's public key. Forwarding nodes can authorize the packet against a 4 KB bloom filter that fits in L1 cache, in under ten nanoseconds, without ever touching the payload. The full identity check (token signature verification, capability matching) happens at session and subscription time, not per-packet — the bloom filter caches the positive verdict.

On top of identity sit two coordination primitives. Channels are named endpoints with visibility scopes and capability-based access policy. Permission tokens are signed, scoped, time-bound delegations that grant specific entities specific rights on specific channels. Both compose with capabilities, which are tag-and-metadata advertisements that nodes broadcast to describe what they can do.

## State

The state layer is what you actually program against. The unit of work is the event; the unit of organization is the channel; and the unit of ordering is the causal link that every event carries.

A causal link is a 32-byte structure that names the entity that produced the event, the entity's view of what it had observed before producing it, and a hash chaining the event back to its parent. Two events from the same entity are totally ordered. Two events from different entities are ordered only if one's causal cone contains the other — Net doesn't impose a global clock, and the system gets faster the less you ask it to.

Channels carry events. Persist a channel and it becomes a durable log — RedEX. Run reductions over the log and you have folded state — CortEX. Federate queries across folds and you have a database — NetDB. Materialize blobs with deduplication and cache them by access pattern and you have Dataforts. Run long-lived stateful workers against channels and you have MeshOS. Each of these is a different way to use the same primitive; none of them is a different system bolted onto the bus.

## What this gets you

The architecture is deliberate about a small number of properties:

- **Wire-speed authorization.** The header is enough to forward, drop, or accept a packet. There's no decryption on the forwarding path and no central authority to call out to.
- **Identity portable across nodes.** An entity is its key. It can move between nodes, fork into replicas, and merge again without losing causal continuity.
- **One primitive for everything.** Logs, state, queries, RPC, workers — all of them are channels plus interpretation. The same authorization, the same encryption, the same routing apply uniformly.
- **No central state.** No broker, no coordinator, no registry. State lives at the edges, addressed by content and identity, replicated when you ask for it.

The rest of this section walks each layer one at a time. The transport layer mostly stays out of your way; the identity layer is where most operational decisions live; the state layer is where most application code lives.
