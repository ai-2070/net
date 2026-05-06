# Net v0.8 — "Killing Moon"

Net is a mesh runtime. Identity is cryptographic, channels are hierarchical, state is causal, and compute moves. There is no broker, no leader, no central directory. Every node is its own keypair. Every event is signed into a chain you can verify without trusting the network underneath. The network is the substrate; the entities are what matter.

This is what we have to show on day one.

## Mikoshi

The piece worth naming first.

A daemon in Net is a stateful event processor whose identity is its public key and whose location is the mesh. You don't address it by "node X, slot 3." You address it by its `origin_hash`, and that fingerprint doesn't change when the daemon moves.

Mikoshi is how it moves.

A running program on one node becomes a running program on another without losing its history, its pending work, or its place in the conversation. The source packages its state, the target unpacks it, and for a brief moment the entity exists on both nodes at once — spreading, superposed, then collapsed onto the target as routing cuts over. The daemon doesn't know it moved. Neither does anything talking to it. Observer nodes watching the stream see the same causal chain continue uninterrupted, the same sequence numbers, the same entity speaking. The hardware underneath shifted. The stream didn't notice.

What moved wasn't a copy. It was the thing itself, carried across.

Six phases, signed at every boundary, with continuity proofs that verify the chain didn't fork. Standby groups and replica groups compose on top — the active dies, the warmest standby promotes, the mesh keeps moving. The daemon is the object, and the object persists.

That is the headline of v0.8.

## What's underneath

A non-localized event bus. Encrypted UDP transport with AEAD on every data packet, multi-hop forwarding, NAT traversal, and pingwave swarm discovery. ed25519 identity stamped on every header. Capability announcements that drive routing — a request for inference flows toward the nearest node with a matching GPU, not toward a fixed endpoint. Permission tokens with delegation chains. Bloom-filter authorization checks at sub-10ns per packet. Hierarchical subnets that keep observation cost bounded as the mesh grows.

A storage stack that is *embedded*, not a service: RedEX as the append-only log, CortEX folding the log into typed domain state, NetDB exposing it as queries and live watches. Disk persistence is a flag. Durability is a knob (`Never`, `EveryN`, `Interval`, `IntervalOrBytes`). Snapshots round-trip the whole stack in one blob. There is no database to run alongside the runtime. The runtime is the database.

Bindings for Node, Python, and Go. Ergonomic SDKs in TypeScript and Python. The same `MeshDaemon` interface whether the event came from this process, the next node over, or three hops away. Code written against a single-node prototype runs unmodified on a multi-hop mesh.

## What this release means

Net is built on the conviction that distributed compute should not be a control-plane problem. No broker to provision, no orchestrator to fail over, no service registry to keep consistent with reality. The mesh routes around what's down. The chain proves what's true. The daemon is wherever it needs to be.

We chose the Cyberpunk frame because it's the right one. Mikoshi is the engram store — minds persisting outside the hardware that bore them. Net's daemons persist outside the nodes that host them. That is not a metaphor we are reaching for. It is what the migration state machine does, packet by packet, with cryptographic receipts.

v0.8 is the version of Net we are willing to put a name on. The codename does double duty. The song — Echo & the Bunnymen, 1984 — is about the part of yourself you don't get to negotiate with. The mission — *Phantom Liberty*'s final act — is V carrying Songbird (Somi) to the Moon, where the system that would destroy her can't reach.

The release ships when it's ready, not when it's convenient. It happens to ship on May 1, 2026, under a full moon. We didn't plan that. We're taking it.

## Codename

"Killing Moon" — Echo & the Bunnymen (1984) / Cyberpunk: Phantom Liberty (2023). Released May 1, 2026.

## License

See [LICENSE](../../../LICENSE).
