# Net — [*Dataforts Out Now!*](#dataforts)

[![codecov](https://codecov.io/gh/ai-2070/net/graph/badge.svg?token=AOBMOF6LE4)](https://codecov.io/gh/ai-2070/net)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

[![crates.io: net-mesh-sdk](https://img.shields.io/crates/v/net-mesh-sdk.svg?label=crates.io%3A%20net-mesh-sdk)](https://crates.io/crates/net-mesh-sdk)
[![crates.io: net-deck](https://img.shields.io/crates/v/net-deck.svg?label=crates.io%3A%20net-deck)](https://crates.io/crates/net-deck)
[![crates.io: net-cli](https://img.shields.io/crates/v/net-cli.svg?label=crates.io%3A%20net-cli)](https://crates.io/crates/net-cli)
[![docs.rs](https://img.shields.io/docsrs/net-mesh-sdk?label=docs.rs)](https://docs.rs/net-mesh-sdk)

[![npm: @net-mesh/sdk](https://img.shields.io/npm/v/@net-mesh/sdk.svg?label=npm%3A%20%40net-mesh%2Fsdk)](https://www.npmjs.com/package/@net-mesh/sdk)
[![npm: @net-mesh/core](https://img.shields.io/npm/v/@net-mesh/core.svg?label=npm%3A%20%40net-mesh%2Fcore)](https://www.npmjs.com/package/@net-mesh/core)
[![npm: @net-mesh/deck](https://img.shields.io/npm/v/@net-mesh/deck.svg?label=npm%3A%20%40net-mesh%2Fdeck)](https://www.npmjs.com/package/@net-mesh/deck)
[![npm: @net-mesh/cli](https://img.shields.io/npm/v/@net-mesh/cli.svg?label=npm%3A%20%40net-mesh%2Fcli)](https://www.npmjs.com/package/@net-mesh/cli)

[![PyPI: net-mesh-cli](https://img.shields.io/pypi/v/net-mesh-cli.svg?label=PyPI%3A%20net-mesh-cli)](https://pypi.org/project/net-mesh-cli/)
[![PyPI: net-deck](https://img.shields.io/pypi/v/net-deck.svg?label=PyPI%3A%20net-deck)](https://pypi.org/project/net-deck/)

**Network Event Transport** — a latency-first encrypted mesh protocol.

Loosely inspired by the Net from Cyberpunk 2077 — a flat, encrypted mesh where every device is a first-class node. Not affiliated with CD Projekt Red or R. Talsorian Games. This is an engineering take on the concept, not a licensed adaptation.

## What it is

Net is what the internet would look like if it were built today, the network science fiction imagined and systems engineers said was impossible. It is a latency-first encrypted mesh network. Every computer, device, and application is an equal node on a flat topology. There are no clients, no servers, no coordinators. The mesh propagates state, not connections. Existing networks operate in milliseconds (10⁻³). Net operates in nanoseconds (10⁻⁹).

## Install

```bash
# Rust
cargo add net-mesh-sdk

# TypeScript / Node
npm install @net-mesh/sdk @net-mesh/core

# Python
pip install net-mesh-sdk

# Go
go get github.com/ai-2070/net/go
```

The Rust crate, npm scope, and PyPI dist all publish under `net-mesh*` / `@net-mesh/core*`. Source-level imports stay as `net_sdk` / `@net-mesh/sdk` / `from net_sdk import ...`. See [SDKs](#sdks) for the lower-level core packages and full per-language usage.

## Contents

- [Install](#install)
- [Why not best-effort](#why-not-best-effort)
- [A new class of systems](#a-new-class-of-systems)
- [Properties](#properties)
- [Queues](#queues)
- [Backpressure](#backpressure)
- [Rerouting](#rerouting)
- [Topology](#topology)
- [Consistency](#consistency)
- [State, not connections](#state-not-connections)
- [Capabilities](#capabilities)
- [Channels](#channels)
- [nRPC](#nrpc)
- [Subnets](#subnets)
- [Security surface](#security-surface)
- [Daemons](#daemons)
- [Mikoshi](#mikoshi)
- [Dataforts](#dataforts)
- [Invariants](#invariants)
- [Device autonomy](#device-autonomy)
- [Processing without storage](#processing-without-storage)
- [RedEX](#redex)
- [CortEX + NetDB](#cortex--netdb)
- [Non-localized event bus](#non-localized-event-bus)
- [Infinite extensibility via subprotocols](#infinite-extensibility-via-subprotocols)
- [Cost of devices](#cost-of-devices)
- [Applications](#applications)
- [Infrastructure value](#infrastructure-value)
- [The industrial latency gap](#the-industrial-latency-gap)
- [Why not cloud](#why-not-cloud)
- [Security](#security)
- [The Blackwall](#the-blackwall)
- [Implementation](#implementation)
- [Status](#status)
- [SDKs](#sdks)
- [Benchmarks](#benchmarks)

## Why not best-effort

ARPANET assumed scarcity. The Net assumes abundance.

ARPANET was designed during the Cold War. Nuclear war was a real possibility. The network had to survive partial destruction, so TCP guarantees delivery: packets will arrive, eventually, in order, even if half the routers are gone. Nodes were scarce. Bandwidth was scarce. Routes were scarce. Every packet was precious because the infrastructure might not be there for the next one. That was the right design for 1969.

It's the wrong design for now. Data is abundant. Nodes are abundant. Bandwidth is abundant. And the external pressure is constant, overwhelming, and unyielding — sensors don't pause, token streams don't wait, market feeds don't care that your queue is full. The firehose doesn't have a pause button. In a world of scarcity, guaranteeing delivery is a virtue. In a world of abundance, guaranteeing delivery is a threat — you're promising to deliver data that will bury the receiver. The bottleneck isn't delivery — it's processing. A delivery guarantee is only as good as the weakest node in the chain. TCP guarantees delivery to a buffer. It doesn't guarantee the receiver can act on it in time. The guarantee creates false confidence: arrival does not equal usefulness.

TCP also presumes that most actors are good. The protocol is cooperative — it assumes both sides want the connection to succeed, that routers will forward honestly, that congestion control will be respected. The entire internet is built on this assumption. It works until it doesn't.

Net makes no presumptions about actors. The only axiom is self-preservation: a node must survive by not getting overloaded. Everything else follows from that. Nodes drop what they can't handle. Relay nodes can't read what they forward. Capability announcements can be verified against behavior. Trust isn't assumed — it's derived from observation. A node that claims capacity it doesn't have will be routed around when its silence or latency betrays it.

Net inverts the default. TCP starts with trust and detects abuse. Net starts with zero assumptions and lets trust emerge from consistent behavior.

Nodes reject work they can't process within a time window. Dropping a packet and re-requesting from a faster node costs nanoseconds across the mesh. Waiting for a congested node's guaranteed response costs milliseconds. When dropping is cheaper than waiting, delivery guarantees become overhead.

This economic inversion has a physical consequence at the queue level.

## A new class of systems

Existing networking falls into two categories:

**Best-effort networks** (TCP/IP, HTTP, gRPC). Optimized for delivery. Queues absorb bursts. Backpressure is negotiated. Connections are stateful. Consistency is global or eventual. Trust is assumed. The sender slows down when the receiver can't keep up. This model dominates because it was designed first and everything was built on top of it.

**Real-time networks** (CAN bus, EtherCAT, TSN, military MANETs). Optimized for deterministic timing. Fixed topologies. Dedicated hardware. Time-slotted access. These achieve low latency by controlling the physical layer — you get guaranteed timing because you own the wire. They don't scale to heterogeneous, dynamic, adversarial environments because they can't. The guarantees depend on controlling the infrastructure.

Net is neither. It achieves real-time latencies on commodity hardware over commodity networks without controlling the physical layer. It doesn't guarantee timing through infrastructure control — it guarantees it through architectural choices: drop instead of queue, route around instead of wait, observe instead of coordinate, derive instead of query.

The pieces exist independently as solved problems. Event sourcing (Kafka). Process migration (Erlang/OTP). Distributed state (CRDTs). Capability scheduling (Kubernetes). Self-healing mesh (military MANETs). Causal ordering (vector clocks). Nobody composed them into a single runtime at nanosecond speeds because nobody had a transport layer fast enough. You can't migrate state in microseconds if your network adds milliseconds. You can't detect failure in nanoseconds if your heartbeat protocol runs over TCP.

The benchmark numbers aren't performance metrics. They're existence proofs. They measure packet scheduling — the time to process, route, and queue a packet for transmission, not the wire time. But they demonstrate that the software layer is no longer the bottleneck. The scheduling overhead per packet is nanoseconds. The remaining latency is physics: NIC, wire, speed of light. The software got out of the way.

This is the gap: a system that operates at hardware timescales, on commodity hardware, across untrusted infrastructure, with no central coordination, no global consensus, and no assumptions about the goodwill of participants. Best-effort networks can't do this because their queue model is incompatible. Real-time networks can't do this because their guarantees require owning the wire. Net sits in the space between them — fast enough to be real-time, open enough to be general-purpose, hostile enough to survive the actual internet.

## Properties

**Latency-first.** The entire stack is designed so that "within the time window" is measured in nanoseconds. Sub-nanosecond header serialization. Nanosecond-scale heartbeats, forwarding hops, and failure recovery. The floor is low enough that packet scheduling operates at timescales traditionally reserved for local function calls. See [Benchmarks](#benchmarks) for measured numbers.

**Streaming-first.** Data is a continuous flow, not documents. The event bus, sharded ring buffers, and adaptive batching assume data is produced incrementally and consumed incrementally. There are no requests and responses. There are streams.

**Zero-copy.** Ring buffers, no garbage collector, native Rust. Forwarding doesn't allocate or copy payload data. This is what makes the per-hop numbers possible and it's a design principle, not just an optimization.

**Encrypted end-to-end.** Noise protocol handshakes for key exchange. ChaCha20-Poly1305 authenticated encryption with counter-based nonces. Every packet is encrypted between source and destination. Intermediate nodes never see plaintext.

**Untrusted relay.** Nodes forward packets without decrypting payloads. The mesh can route through infrastructure you don't trust. Combined with E2E encryption, this means the network can grow through adversarial nodes.

**Schema-agnostic.** The transport layer moves bytes, not structures. A raw event is payload plus hash. The protocol never inspects content. Structure is optional and emerges where participants agree on it — two nodes can negotiate typed, ordered streams while the rest of the mesh passes opaque bytes.

**Optionally ordered.** Ordering is per-stream, not global. The unordered path is the fast path. Causal ordering is available when streams need it. The cost of ordering is paid only by streams that require it.

**Optionally typed.** The protocol doesn't care what's in the payload. The behavior plane can. Typing is a local agreement between nodes, not a network requirement.

**Native backpressure.** Nodes drop packets without reply. This isn't a failure mode — it's the design. The proximity graph makes silence a signal, not an error. A node that stops responding doesn't need to send an error. Its neighbors already know within a heartbeat interval.

## Queues

In best-effort networks, queues are a virtue. They absorb bursts, smooth jitter, and let slow consumers catch up. TCP's entire flow control model depends on buffers at every hop. Routers queue. Kernels queue. Applications queue. The queue is what makes the delivery guarantee possible — data waits in line until the receiver is ready.

In latency-first networks, queues are failure. Every nanosecond a packet sits in a queue is latency added. A queue means a node accepted work it couldn't immediately process — it violated the self-preservation axiom. In Net, the queue is a ring buffer with a fixed capacity. When it's full, old data is evicted or new data is dropped. There is no unbounded growth. The queue is a speed buffer, not a waiting room.

This creates an incompatibility. Latency-first systems cannot coexist with best-effort systems on the same transport. A latency-first node operating at 10M+ events/sec will fill a best-effort system's kernel socket buffer before a single context switch can happen. The best-effort node hasn't even woken up to read the queue and the queue is already full. From the best-effort node's perspective, this looks like a flood attack. From the latency-first node's perspective, it sent normal traffic to a node that couldn't keep up.

The mismatch is fundamental. Best-effort systems use queues to decouple producers from consumers in time. Latency-first systems require producers and consumers to operate at the same timescale. When they meet, the latency-first system overwhelms the best-effort system's buffers, and the best-effort system's backpressure signals (TCP window scaling, congestion control) operate orders of magnitude too slowly to matter. By the time TCP tells the sender to slow down, the sender has already moved on.

This is why Net runs its own transport (Net over UDP) rather than layering on TCP. It's not an optimization. It's a necessity. The two models are physically incompatible at the queue level.

## Backpressure

In TCP, backpressure is negotiated. The receiver advertises a window size. The sender respects it. If the receiver is slow, the window shrinks. If the network is congested, both sides back off. This takes round trips. At internet scale, that's milliseconds of negotiation before a single byte is throttled.

In Net, backpressure is immediate and unilateral. A node that can't keep up stops processing. That's it. There's no window advertisement, no negotiation, no round trip. The node's ring buffer is full, so new data either evicts the oldest entry or gets dropped at the boundary. The node doesn't tell anyone it's overwhelmed. It just goes silent on that stream.

Silence propagates through the proximity graph. Neighbors observe the silence within a heartbeat interval. The failure detector marks the node as degraded. The circuit breaker trips. From this point, no new traffic is routed to that node.

The critical difference: TCP backpressure slows the sender down. Net backpressure routes around the problem. The sender doesn't slow down. It doesn't need to. The mesh has other nodes.

## Rerouting

When a node goes silent — whether from overload, failure, or a deliberate kill-switch — the mesh reroutes in three phases:

**Detection.** The failure detector runs on heartbeats at nanosecond-scale granularity. A missed heartbeat triggers a status check. The node is marked suspect. After the timeout threshold, the circuit breaker opens. Total time from last heartbeat to confirmed failure: nanoseconds to low microseconds, depending on configuration.

**Recovery.** The recovery manager evaluates alternates in nanoseconds. It looks at the routing table, finds nodes with matching capabilities, and selects the best candidate based on proximity, load, and latency estimates. A full fail-and-recover cycle completes in sub-microsecond time.

**Cutover.** The routing table updates atomically. The next packet on that stream goes to the new node. There is no drain period, no graceful handoff, no connection migration. The state was never tied to the old node's connection — it was in the event stream. The new node picks up from where the stream left off.

From the sender's perspective, nothing happened. One packet went to node A, the next went to node B. The sender didn't decide this. The mesh did. The sender doesn't know or care which node is handling its traffic. It sent data into the mesh and the mesh delivered it to a node that could process it in time.

From the receiver's perspective, the stream continues. If ordering is enabled, the causal chain is intact — every event links to its parent hash. If ordering is disabled, events arrive from wherever the mesh routes them. Either way, the stream didn't stop. A node died and the stream didn't notice.

This is what "propagates state, not connections" means in practice. The connection to node A is gone. The state is on node B. Nothing was lost, nothing was retried, and the reroute was scheduled faster than a context switch on the dead node's operating system.

## Topology

**Flat mesh.** All nodes are protocol-equal. No node has special authority. But nodes aren't equal in capability — the capability system makes that explicit. Every node is a first-class citizen, but not every node can serve every request. The mesh routes based on what nodes can do, not what they are.

**Swarm discovery.** No DNS, no bootstrap servers, no service registry. Nodes discover each other directly through the mesh itself, the same way planets find each other — by existing in proximity and being observable.

**Pingwave.** Nodes emit periodic heartbeats that propagate outward within a hop radius. If you can hear a node's pingwave, you know it exists, how far away it is, and what it can do.

**Proximity graph.** Each node maintains a local view of its neighborhood, not a global directory. The graph is built from direct observation and derivation from neighbors' observations. A node doesn't need to see every other node. It observes enough to derive the rest.

**Capability announcements.** Nodes advertise what they can do — hardware, models, tools, capacity. The mesh uses this for routing decisions. Compute-heavy workloads fall toward GPU-rich nodes the way mass curves spacetime toward other mass.

## Consistency

**Observational.** There is no global truth, only local views. Each node observes its neighborhood and derives the state of the wider mesh from those observations. Two nodes may disagree about the mesh state at a given instant. That's fine. Their views are causally consistent within their own observation window.

No global consensus. No coordinator. No privileged node. Consistency emerges from causal ordering and the speed of propagation.

Everything in the mesh is either observable or derivable. A node doesn't need direct heartbeats from every peer. If it knows a gateway is alive and the gateway reports on its subnet, the subnet's state is derived. The mesh is an inference engine about its own state, not just a forwarding engine for events.

The only authority the mesh respects is physics. Propagation speed, causal ordering, the speed of light. Everything else is negotiable.

## State, not connections

Traditional networking treats the connection as the primary object. You establish a socket, maintain it, tear it down. If the connection breaks, the relationship breaks.

Net propagates state. Connections are ephemeral transport — the current shortest path between where state is and where it needs to be. When a path breaks, state doesn't wait for recovery. It moves. The routing table updates, the proximity graph adjusts, the state continues on a different path. No reconnection, no session resumption, no handshake retry. Identity lives in the state chain, not in the socket.

## Capabilities

Every distributed system assumes a registry. Kubernetes has etcd. Consul has Consul. Nomad has a scheduler. You tell the registry what hardware exists, and the registry tells the scheduler where things can run. The registry is the control plane. If it goes down, the cluster forgets what it is.

Net has no registry. A node announces what it is — cores, memory, GPU, loaded models, installed tools, operator tags — and every peer indexes that announcement locally. Announcements propagate multi-hop; a node four subnets away learns the same fingerprint as a direct peer, without anyone in the middle being a directory. The nodes *are* the control plane, collectively. Nothing to provision, nothing to fail over, nothing to pay for at rest.

This changes what "placement" means. You don't submit a job to a scheduler that queries a registry that talks to a node. You ask the local index — *any peer with an NVIDIA GPU and 40 GB of VRAM, advertising the `prod` tag* — and you get an answer in microseconds. The answer may be a laptop under someone's desk, a server in a rack, or a Jetson on a factory floor. The mesh doesn't care, and neither does your code. Capability is the addressing; location is incidental.

## Channels

Kafka, NATS, Pulsar, Redis Streams — every serious pub/sub is centralized. You run a broker cluster. Producers connect to the cluster. Consumers connect to the cluster. The broker is the bus, and the cluster is the infrastructure. You provision it, scale it, monitor it, patch it. You pay for it whether traffic is flowing or not.

Channels in Net are not a thing you connect to. They are a *name you match on*. A publisher registers `sensors/temperature` with a policy; subscribers ask to join by name; the mesh routes the semantic. A subscriber on a NAT'd laptop, a publisher in a datacenter, and a relay on a jump host all participate in the same channel without anyone connecting to a broker — because there is no broker. The roster is held by the publisher, fan-out is N per-peer unicasts over the existing encrypted sessions, and nothing about "the channel" exists as a standalone process.

This means channels cost nothing when nobody is listening. No queue builds up at a broker. No retention policy has to be configured at a central service. Publish-without-subscribers is literally a no-op — the roster is empty, the fan-out loop runs zero times. Channels with thousands of subscribers work too; they just fan out more packets. The broker was a bottleneck in the first place because it existed. Removing it removes the bottleneck.

## nRPC

gRPC, Twirp, Connect, Thrift — every serious request/response framework is a separate transport. You define a service in an IDL, generate stubs in each language, run a server that speaks HTTP/2, run a client that speaks HTTP/2, and probably run a sidecar (Envoy, Linkerd) to handle retries, deadlines, mTLS, and load balancing. The RPC layer is its own substrate, parallel to whatever pub/sub or messaging system you're already running. Two transports, two failure modes, two sets of metrics, two sets of certs.

nRPC is request/response *on the bus*. There is no second transport. Same niche as gRPC; different shape. No HTTP/2, no protobuf IDL, no codegen step. The wire format is JSON over the existing encrypted UDP transport; the schema is whatever typed serializer both sides agree on (serde, TypeScript interfaces, Pydantic, Go structs). Deadlines, retries, hedging, circuit breakers, and end-to-end cancellation come from the SDK, not a sidecar, are library calls that work the same way across Rust, TypeScript, Python, and Go.

The implication is that an RPC server costs what a subscriber costs. There is no broker to provision, no service mesh to operate, no certificate rotation to coordinate. Spin up a handler with `serve_rpc("echo", echo_handler)` and the service is announced on the mesh; spin up another, and queue-group dispatch load-balances calls between them; let one die and the failure detector evicts it from the roster within a heartbeat. The "RPC infrastructure" is the mesh, and the mesh is already running.

And because nRPC is a CortEX fold over a channel, not a separate transport, several properties come for free that gRPC would charge you for. **Crash recovery** is the same recovery the channel already has — a request that landed before the server crashed is replayed when its fold rehydrates from the log; the strict-prefix watermark guarantees at-least-once handler execution. **Snapshot-based migration** is the same snapshot the daemon layer uses — in-flight RPC state migrates with the rest of the fold's state, so in-flight calls survive a planned move or a process restart. **Audit trail** is the same log — every call is durable on whichever side persists the channel, and operators get a per-service replayable record without instrumenting handlers. **Time-travel debugging** is the same replay — "which request flipped the fold into the broken state?" is a question you answer by re-running the events. **Backpressure** is the same backpressure — when the channel append rate-limits, the caller sees `RpcError::Backpressure` and the retry helper handles it. None of this is RPC-specific scaffolding. It's the storage and folding machinery the rest of the system already uses, repurposed.

## Subnets

Network segmentation has always been a network problem. VLANs partition Layer 2. VPCs partition Layer 3. Firewalls enforce boundaries. You reconfigure who-can-talk-to-whom by touching routers, ACL lists, and subnet masks — the infrastructure, not the application. Changing the boundary means changing the wire.

Subnets in Net are a property of the *application*, derived from capability tags. A policy says "a `region:us` tag maps to subnet level 0, value 2"; every node applies the same policy to every announcement it sees; the geometry emerges without any node holding authority over it. You change the geometry by editing a policy, not by touching a router. The geometry travels with the workload — a node that moves regions announces its new tags, peers re-derive, and the boundary follows.

Enforcement lives at the channel. A channel declared `SubnetLocal` accepts subscribers only from peers whose derived subnet matches exactly; cross-subnet subscribes reject at the publisher. "Development nodes can't see production data" becomes a one-line capability tag plus a subnet policy — not a firewall rule, not a VPN, not a network redesign. The boundary is part of the software, enforced every time a packet tries to cross it.

## Security surface

Most systems ship authentication, capability discovery, and tenancy isolation as three separate products. mTLS for identity. A service mesh for discovery. VPCs or namespaces for isolation. Each is configured independently; each can drift; each has its own blast radius when it misconfigures. Revocation takes effect whenever the slowest of them catches up.

Net fuses them. The same 32-byte ed25519 seed that gives a node its `node_id` signs the capability announcements that drive placement. The same identity issues the permission tokens that authorize channel subscribes. The same tokens delegate down chains whose signatures verify end-to-end. The same capability announcements feed the subnet policy that decides visibility. There is one key, one policy surface, one revocation primitive — and they compose automatically because they share a substrate.

Which means revocation actually works. A subscriber who loses their token stops receiving events on the publisher's next packet — not on the next cluster reconciliation, not after a cache expiry, not when the service mesh pushes a new ACL. The check is a 20 ns bloom filter hit on every publish, and when the filter says no, the subscriber is dropped. Same primitive, same identity, same policy, across the whole fleet. The cost of making security unified is that it's boring; the payoff is that it's actually enforceable.

## Daemons

AWS Lambda is stateless; state lives in DynamoDB and each invocation fetches it. Temporal is stateful, but the state lives in Temporal's database, and the workflow is bound to that cluster. Dapr is a sidecar; coordination runs through Dapr's runtime. Erlang and Akka actors are stateful and addressable, but they live inside one cluster you own — "move this actor to a different cluster" is not a primitive any of them expose.

A daemon in Net is a stateful event processor whose *identity* is cryptographic and whose *location* is the mesh. You don't address it by "node X, slot 3." You address it by its `origin_hash`, a fingerprint of an ed25519 public key that doesn't change when the daemon moves. Every event it produces is signed into a causal chain that any node can verify — no database, no ledger service, no central log. The chain itself is self-authenticating, and the daemon's history travels with it.

Concretely: you ship code that says "I need a GPU." It runs wherever a GPU exists. If that GPU dies, the runtime moves the daemon to another GPU node, carrying its state and its history. Its tokens still verify. Its subscribers don't notice it moved. No operator runs `kubectl drain`; no SRE updates a service registry. The daemon is the object, and the object moves. What "moves" actually means is *Mikoshi*, below.

## Mikoshi

In Cyberpunk, Mikoshi is Arasaka's construct for storing engrams — consciousness held in digital space, minds persisting outside their original hardware.

Mikoshi in Net is how daemons move between machines. A running program on one node becomes a running program on another without losing its history, its pending work, or its place in the conversation. The source packages its state, the target unpacks it, and for a brief moment the entity exists on both nodes at once — spreading, superposed, then collapsed onto the target as routing cuts over.

The daemon doesn't know it moved. Neither does anything talking to it. Observer nodes watching the stream see the same causal chain continue uninterrupted, the same sequence numbers, the same entity speaking. The hardware underneath shifted. The stream didn't notice.

What moved wasn't a copy. It was the thing itself, carried across.

A factory controller hops from a dying edge box to a healthy one mid-shift. An inference daemon follows its user from laptop to desktop as they move through the day. A trading agent migrates to a node closer to the exchange without dropping a single tick.

Migration is 1:1 — one entity, one destination. But the same machinery composes into other patterns.

A daemon that needs horizontal scale becomes a replica group — N interchangeable copies with deterministic identity, load-balanced across the mesh. Each replica has its own causal chain. Any can fail and be re-spawned with the same identity on a different node. No state to transfer, no migration protocol — just spawn a fresh copy with the same cryptographic seed. The mesh routes around the gap before the next event arrives.

A daemon that needs to diverge becomes a fork group — N independent entities with documented lineage. Each fork carries a cryptographic sentinel linking its chain back to the parent at the fork point. Any node on the mesh can verify the lineage by recomputing the sentinel. The forks are siblings, not clones. They share a past but not a future.

A daemon that needs fault tolerance without duplicate work becomes a standby group — one active, N-1 idle. The active processes events. The standbys hold readiness. Periodic snapshots capture how far each standby is synced. When the active dies, the standby with the most recent snapshot promotes and replays the gap — the same replay mechanism migration uses. Zero wasted compute. The standbys are warm, not hot.

All three patterns compose with migration. Any member of any group is a normal daemon in the registry. Mikoshi can move it without knowing it belongs to a group. The group coordinates. The mesh routes. The daemon doesn't know the difference.

## Dataforts

In Cyberpunk, dataforts are fortified pockets of corporate data — encrypted constructs that hold the assets a corporation cares about most, guarded by ICE and reached only by netrunners willing to spend cycles getting past the gate. The point isn't the storage. The point is that data has a posture, and getting to it means asking the thing that holds it.

Dataforts in Net is the data layer that grows on top of the event bus. Every prior approach to "where does the data live" presupposes an answer — S3 holds it in a region, Ceph holds it across racks, IPFS holds it wherever a pin exists. Storage is a place. You go to the place to read. You ship to the place to write. Dataforts inverts that: blobs are content-addressed BLAKE3 chunks, the address of the data is the data, and the chunks live on whichever nodes have capacity and capability to hold them. There is no canonical home.

Data is a fluid. Hot chunks — bytes that some node keeps fetching — get pulled toward the nodes that read them, because re-fetching the same hash leaves a copy behind. Cold chunks stay where they are or drain into nodes with spare disk. The same pressure that fills a near-empty node also empties a near-full one. Nothing tells the cluster to rebalance. The blobs move because the reads moved.

Heat is per-chunk and decays. A chunk read a hundred times in the last minute has gravity; a chunk read once a year ago has none. The capability index advertises heat the same way it advertises disk-free, scope, and class. A peer with gravity for a given hash is the natural target when the chunk's current holder needs to shed, and the natural source when a new reader asks. Migration is the heat reading itself — no scheduler, no shuffle plan, no coordinator deciding where bytes should be. The reads decide.

When a node crosses its high-water disk threshold, it picks the coldest chunks it holds and pushes them to peers with capacity. The receive side is opt-in via a capability tag — operators decide which nodes accept overflow and which stay pull-only. Pushes ride the existing per-chunk replication runtime; the only new wire shape is a one-shot nudge telling the receiver to open the chunk channel. Storage saturation no longer fails closed against new writes — the cluster bleeds pressure into peers that can absorb it, until either the workload subsides or those peers fill up too. A node that fills with cold blobs no longer has to choose between rejecting new bytes and running blind GC against bytes the rest of the mesh still wants.

A producer that publishes a chunk and immediately reads it back never sees a gap. The publish path returns a write token; the read path waits on that token's durability watermark before returning the bytes. Read-your-own-writes is the producer's contract with itself — independent of replication factor, independent of cluster topology, independent of which peer ends up holding the chunk. The mesh doesn't promise global linearizability. It promises that you see what you just wrote, however many hops the bytes had to take to settle.

These properties compose with the rest of the stack. RedEX writes a `BlobRef` into the event chain like any other event — the substrate verifies the BLAKE3 hash, the chain stays causal, the blob payload pulls separately when somebody needs it. CortEX folds events that reference blobs into views; the views pin the chunks they care about so gravity doesn't sweep them away. A drone, a workstation, and a datacenter can hold the same dataforts — different slices of the same content-addressed space, replicating according to who reads what, all encrypted in flight and on disk.

There is no object store to provision. There is no cluster to operate. The data is on the mesh because the mesh is the data.

## Invariants

**Identity.** A node is its keypair. Every node has a long-lived cryptographic identity — the public key is the node ID, the private key is the authority to act as that node. Identity is cryptographic, not topological. A node can roam across networks, change IPs, traverse NAT, switch interfaces, and remain the same node. All communication is authenticated against this identity, independent of network location.

**Bootstrap.** Nodes require at least one known peer to join the mesh. This initial contact is out-of-band — a manual address, LAN broadcast, QR code, config file, cached peers from a previous session. After first contact, discovery is emergent. Pingwaves propagate, the proximity graph builds, and the mesh takes over. "No bootstrap servers" doesn't mean no first contact. It means the protocol doesn't depend on any fixed infrastructure to function after that first handshake.

**Scale.** The mesh is logically flat but scales via hierarchical summarization. At small scale, nodes observe each other directly. At larger scale, nodes form clusters. Gateway nodes aggregate health, compress capability summaries, and propagate state for their subnet. A distant node doesn't need individual heartbeats from every node in a cluster — it observes the gateway and derives the rest. This keeps observation cost bounded: each node observes its peers at its level of the hierarchy, and derivation gives it awareness of everything below.

**Time.** There is no global clock. The system does not require synchronized clocks and has no dependency on NTP or wall-clock agreement. Event ordering is derived from causal relationships — vector clocks, Lamport timestamps, parent hashes in the event chain. Two nodes may assign different wall-clock times to the same event. That doesn't matter. What matters is that they agree on causal order: what happened before what.

**Participation.** Relay is a cost of participation. If you're on the mesh, you forward traffic within your resource limits. This is cooperative, not economic — the current design assumes a mesh of your own machines and trusted participants. Incentive mechanisms for public, multi-party, or adversarial meshes are out of scope. Nodes enforce their own relay limits through the same autonomy rules that govern everything else.

## Device autonomy

Every node sets its own rules. A node can rate-limit, reject, redirect, or kill-switch independently. The mesh doesn't override a node's sovereignty. Autonomy rules, safety envelopes, and resource limits are local decisions, not network policy.

## Processing without storage

In every existing system, the node that processes data is the node that stores it, or the node that stores it is a known, fixed location. Databases, message brokers, file systems — processing and storage are co-located or connected by a stable, long-lived path. If the storage node dies, you fail over to a replica. If the processing node dies, you restart it and reconnect to storage.

Net doesn't have this coupling. The event bus is a ring buffer — it's a speed buffer, not storage. The adapters (Redis, JetStream) are optional persistence layers, not requirements. A node processes events in flight. It doesn't store them unless explicitly configured to. The event stream flows through nodes like current through a wire — the wire doesn't remember the electricity.

This means processing can happen anywhere without first solving "where is the data." The data is in the stream. The stream is on the mesh. Any node with matching capabilities can pick up the stream and process it. If that node dies, another node picks it up. Neither node "had" the data. The data was passing through.

Storage becomes a choice, not an assumption. A node can choose to persist events to Redis. A node can choose to replay from JetStream. But the mesh itself doesn't require storage to function. Events exist in the ring buffers of the nodes they're passing through, for as long as they're relevant, and then they're gone. If you need them later, that's what the persistence adapters are for. But the processing path — the hot path — never touches disk, never queries a database, never waits on storage I/O.

This is why the latency numbers are what they are. Processing isn't waiting on storage. Storage isn't blocking processing. They're independent decisions made by independent nodes.

## RedEX

**The stream is the state.** Every database you've used separates the two — a mutable state somewhere (rows, documents, keys) and a log that records changes to it. The log is secondary: a write-ahead record, a binlog, a replication feed. If the log and the state disagree, the state wins.

RedEX inverts that. The append-only event stream is the source of truth; any "state" anywhere else is just a projection of the stream at a particular point. You don't update a row and then log it. You append an event, and rows derive. The log doesn't drift from the state, because the log *is* the state — re-running the fold on the same events yields the same result every time.

Kafka is a distributed log; you need a cluster to run it. SQLite and DuckDB give you random access, not append-only streaming, and their write path isn't designed for continuous telemetry. Write-ahead logs inside databases are internal — a private detail of Postgres or MySQL, not a product you can point at your own events.

RedEX is the append-only log, unbundled and local. A single file is a single monotonic sequence — 20 bytes of index per record, a heap segment for payloads, optional disk persistence via three append-only files per channel (`idx`, `dat`, and a `ts` sidecar so age-based retention survives restart). That is the whole thing. A node decides, per channel, whether to persist: keep the last 24 hours of `sensors/temp` locally; forget everything on `metrics/debug` the moment it leaves the ring buffer; write `audit/events` to disk with fsync on close. Every decision is local.

Because storage is per-file and per-node, the durability decision scales with the node, not with the cluster. A Raspberry Pi keeps a tiny log of its own sensor readings. A server keeps a huge log of whatever it cares about. Neither participates in a cluster consensus protocol to persist anything, because there is no cluster — the log is local, the replay is local, the retention is local. When higher layers (CortEX, NetDB, your own fold) need durability, they build on RedEX. When they don't, RedEX isn't on the critical path.

## CortEX + NetDB

**CortEX is RedEX, folded.** If RedEX makes the stream the state, CortEX is what you get when you collapse that stream into a usable shape — a reactive, queryable projection of the log, updated event-by-event. The log stays the source of truth; the folded state is a view of it that's cheap to read and always consistent with the events that produced it.

Materialize, RisingWave, Flink — all distributed systems. You run a cluster, connect clients, write SQL, the cluster materializes views and pushes deltas. Elegant for a datacenter; overkill for a drone, a $50 sensor, or a laptop that wants a reactive view of its own history. The smallest useful deployment of any of them is already bigger than most of the devices Net runs on.

CortEX is a *local* fold from a RedEX tail into an in-memory state, kept consistent event-by-event. The "database" isn't a process you connect to. It's a `Vec<Task>` or a `HashMap<Uuid, Memory>` you hold in your Rust, TypeScript, or Python code, that updates as events fold in. Queries are direct memory access — no network, no parser, no planner, no lock contention beyond a single read-lock. A $50 device can run a full reactive view of its own event history, materialize a few hundred tasks or a few thousand memories, and serve filtered queries at cache speed.

NetDB bundles adapters into a query façade — `db.tasks.find_many(filter)`, `db.memories.find_unique(id)` — with whole-database snapshots for persistence and handoff. The surface is identical across Rust, TypeScript, and Python; snapshot bundles round-trip between languages. Because everything is local, two nodes can have completely different NetDB views — one tracks tasks, one tracks memories, one tracks both, one tracks neither. The database is a *choice*, not a dependency of being on the mesh.

## Non-localized event bus

Every prior event bus has a location. LMAX Disruptor is a single-process ring buffer. Kafka is a cluster of brokers at fixed addresses. Pulsar separates compute from storage but retains the broker model. In all cases, the bus is something you connect *to* — it has a process, a machine, a data center. Producers and consumers know where it is.

Net's event bus has no location. The sharded ring buffers on each node are local speed buffers, but the logical event bus spans the mesh. A producer on node A and a consumer on node C interact through the same abstraction regardless of whether they're on the same machine, the same subnet, or separated by five relay hops. The mesh handles routing, encryption, forwarding, and failure recovery transparently. The bus isn't *at* a location — it *is* the mesh.

Three consequences:

**No broker.** There is nothing to provision, scale, or fail over. The bus exists wherever participating nodes exist. Adding a node adds capacity. Removing a node triggers rerouting, not an outage.

**No plaintext at rest.** Broker-based systems hold plaintext at the broker — the broker must read messages to route them. Net's relay nodes forward encrypted bytes they cannot read. The event bus is encrypted end-to-end even though no single node is the "bus."

**No partition-leader bottleneck.** Kafka orders events per partition, creating a single-leader bottleneck per partition. Net orders events per entity via causal chains. There is no partition leader. Every entity maintains its own chain independently. Ordering scales with the number of entities, not with the number of partitions a broker can handle.

**Location-transparent consumption.** A daemon processing events doesn't know — and can't determine from the API — whether the event originated on the same node, a neighbor one hop away, or a node five hops and two subnet boundaries distant. The call signature is the same as a local function call: receive an event, return output. The mesh resolved routing, decrypted the payload, validated the causal chain, and delivered the event before the daemon saw it. From the daemon's perspective, every event is local. Code written for a single-node prototype runs unmodified on a multi-hop mesh. The deployment topology is a runtime decision, not a code change.

This is what makes "processing without storage" possible. The data isn't stored at the bus. The data is in transit through the mesh. Any node with matching capabilities can process it. If that node dies, another picks it up. Storage is a choice made by individual nodes via persistence adapters (Redis, JetStream), not an architectural requirement of the bus itself.

## Infinite extensibility via subprotocols

Every Net packet carries a `subprotocol_id` field. This is 16 bits in the header — 65,536 possible protocols — and it changes everything about how the mesh evolves.

A vendor builds a custom inference protocol for their hardware. They pick an ID in the vendor range, implement the `MeshDaemon` trait, register it in the `SubprotocolRegistry`, and deploy. The mesh already knows how to route their traffic — the node advertises `subprotocol:0x1000` as a capability tag, the existing `CapabilityIndex` indexes it, and any node that needs that protocol can find a handler through the same query path used for GPU discovery or tool matching. No firmware update. No mesh-wide upgrade. No coordination with anyone.

The critical property is the **opaque forwarding guarantee**: nodes that don't understand a subprotocol forward it anyway. They read the routing header, decide the next hop, and pass the encrypted payload through without inspection. The intermediate node doesn't need to know what's inside. It doesn't need the handler installed. It doesn't even need to know the subprotocol exists. It forwards because the routing header says to, and it can't read the payload even if it wanted to.

This means new protocols deploy incrementally. You upgrade the nodes that need to process the protocol. Every other node in the mesh — every relay, every gateway, every forwarding hop — continues working unchanged. There is no flag day. There is no "upgrade the mesh to support protocol X." The mesh already supports protocol X. It just doesn't know what X means, and it doesn't need to.

Version negotiation happens at session establishment, not per-packet. Peers exchange manifests — compact lists of (protocol ID, version, minimum compatible version) — and compute a `NegotiatedSet` of protocols they both understand. This is a pure function. No coordinator, no registry server, no version authority. Two nodes meet, compare notes, and know what they can talk about.

The consequence is that the mesh is not a fixed protocol. It is a protocol runtime. The transport, encryption, routing, forwarding, failure detection — those are fixed. Everything above them is a subprotocol that can be swapped, extended, versioned, and deployed independently. The mesh doesn't have features. It has a feature space.

## Cost of devices

When processing can be offloaded to the mesh, edge devices don't need to be smart. They need to be present.

A sensor node doesn't need a GPU to run inference. It needs a network interface and a microcontroller. It streams raw data into the mesh and the mesh routes the processing to a node that has the capability. A camera doesn't need to run object detection. A thermostat doesn't need to run a language model. A brake sensor doesn't need to run path planning. They produce data. The mesh finds compute.

The entire transport library — Noise protocol, ChaCha20-Poly1305 encryption, routing, swarm discovery, failure detection, capability system — compiles to ~2MB stripped. About two megabytes. It fits on anything with a network interface.

This inverts the economics of edge deployment. Today, every device that needs intelligence must contain intelligence — or pay for a round trip to a cloud that does. That means expensive hardware at the edge, or latency to a data center, or both. Net eliminates this choice. Devices can be cheap, dumb, and deterministic. They do one thing well — sense, actuate, relay — and the mesh provides the intelligence dynamically.

The capability announcement system is what makes this work. A $5 sensor node advertises that it produces temperature data. A $1500 GPU node three hops away advertises that it runs inference models. The mesh connects them automatically. The sensor node didn't need to know the GPU node exists. The GPU node didn't need to be configured to accept sensor data. The capability graph brought them together.

This means you can scale a deployment by adding cheap nodes for coverage and a few expensive nodes for compute. The ratio adjusts dynamically — add more sensors and the compute nodes absorb the load. Add more compute and the sensors' data gets processed faster. Neither side needs to be reconfigured. The mesh adapts.

Compare this to platforms like Nvidia Omniverse, which require DGX systems, certified OVX servers, NVLink interconnects, and dedicated networking. The hardware alone for a factory-scale deployment is millions before software licensing. Net requires anything that can read UDP and is already on the factory floor.

## Applications

**AI runtime.** The original use case. Token streams, tool-call results, guardrail decisions, and consensus votes flowing across heterogeneous GPU nodes. Compute-heavy inference routes to whichever node has capacity. The mesh is the runtime — no orchestrator dispatching work, no queue broker mediating between models.

**Vehicular sensor mesh.** Cars sharing LIDAR, radar, and camera feeds across a local swarm. A vehicle that can't see around a corner derives it from a neighbor that can. Processing — object detection, path planning — routes to whichever vehicle or roadside unit has spare capacity. Vehicles also sync intent — braking, turning, route changes — so every car in the swarm knows what the vehicle ahead will do before it does it. Brake lights are a 200ms visual signal processed by a human. An intent stream on the mesh is scheduled in nanoseconds and processed by software. The car behind doesn't react to braking. It knows about the braking before the brake pads touch the rotor.

**Robotics factory floor.** Robots don't need line-of-sight for networking. The mesh routes through whatever nodes are reachable. A robot behind a steel column relays through one that isn't. If a robot goes offline, the mesh schedules a reroute in sub-microsecond time — the assembly line doesn't stop. No WiFi access points, no central controller, no single point of failure.

**Edge compute.** IoT devices, phones, single-board computers acting as equal peers. A sensor node that can't run inference locally routes to the nearest node that can. Capability announcements make this automatic — the mesh knows what every node can do and routes accordingly.

**Local-first collaboration.** Devices on the same LAN forming a mesh without cloud infrastructure. Pingwave bootstrap on the local network, no configuration, no accounts, no servers. The mesh exists for as long as the devices are in proximity.

**Disaster response.** Phones, drones, portable radios forming a mesh with no surviving infrastructure. Each device contributes what it has. A phone relays. A drone provides compute. A satellite uplink node becomes a gateway. The mesh forms from whatever is present and routes around whatever is gone.

**Remote surgery.** A surgeon operating remotely, with control signals and haptic feedback routed across the mesh. The robot doesn't need a dedicated fiber link to one server. If the primary compute node lags, the mesh reroutes to another mid-operation. The surgeon doesn't notice. The patient doesn't notice. The scalpel doesn't stop.

**Drone swarms.** Coordinated flight without a ground controller. Each drone shares position, velocity, and intent. Formation changes propagate across the swarm faster than aerodynamic forces alter the flight path. A drone that loses a motor broadcasts the failure; the swarm adjusts formation before the drone has begun to fall.

**Live performance.** Lighting, audio, video, and pyrotechnics synchronized across hundreds of nodes on a stage rig. A DMX controller dies, another node picks up the cue list. No show stop. Latency low enough that audio sync across the mesh is tighter than the speed of sound across the venue.

**Precision agriculture.** Tractors, drones, soil sensors, and weather stations forming a field mesh. A tractor that detects a soil condition shares it, and every other tractor adjusts its seeding or irrigation without routing through a cloud service. The field is the network.

**Multiplayer gaming.** Game state propagates peer-to-peer with causal ordering. A player drops, the mesh reroutes. Capability-aware routing means heavier computation — physics, collision, world state — routes toward the gaming PC, not the phone. The weakest device doesn't become the bottleneck; the mesh routes around its limitations. Ping is meaningless here — there's no fixed server to round-trip to. The relevant measurement is observation latency: the time from when a state change is produced to when another node can observe it.

## Infrastructure value

A protocol that operates at nanosecond timescales doesn't just make AI faster. It makes everything that depends on coordination faster.

Manufacturing plants where sensor data reaches decision systems in time to prevent defects, not just log them. Port logistics networks where container routing adapts to delays before ships finish docking. City infrastructure where maintenance signals propagate before failures cascade into outages. Power grids where load balancing happens at the speed of demand, not the speed of SCADA polling. Financial settlement systems where latency isn't a competitive advantage — it's the difference between a correct settlement and a cascade failure.

The value of Net appears in the efficiency gain across every system that runs on it. A Toyota plant that catches a tolerance drift 10ms earlier saves a production run. A port that reroutes containers in real time instead of batch processing saves hours per ship. A smart grid that balances load at the edge instead of round-tripping to a central controller reduces peak infrastructure costs. A distributed AI deployment that runs on local mesh infrastructure doesn't depend on foreign cloud providers.

None of these systems need to be rebuilt. Net sits underneath them. The applications don't change. The coordination layer changes. Everything above it gets faster because the layer below it got out of the way.

This is infrastructure in the classical sense. The Shinkansen didn't generate its value from ticket sales. It generated it from everything the Japanese economy could do because the train existed. Highways don't profit from tolls. They profit from the GDP of every business that ships on them. Net is the same — the value is in what runs on top of it, not in the protocol itself.

## The industrial latency gap

Most industrial coordination is hyper-local. Factory floor, facility campus, vehicle fleet — within a few kilometers. The physics floor at 5km is around 33 microseconds round trip. Light in fiber, there and back. That's the hard limit. No protocol, no architecture, no amount of engineering can beat it.

Current industrial systems don't come close. They route through data centers — cloud PLCs, centralized SCADA, remote monitoring dashboards. A sensor reading on a factory floor travels to a data center and back before anyone acts on it. That adds 10-50ms of round-trip latency on top of the physics. For coordination that is inherently local — two machines on the same floor, two vehicles in the same lot, two robots on the same assembly line — this is 300 to 1500 times slower than the physical limit.

Net's scheduling overhead is nanoseconds. The remaining latency after Net processes a packet is the NIC, the wire, and the speed of light. For a 5km campus, that's ~33 microseconds. For a factory floor, it's single-digit microseconds. The software is no longer the bottleneck. The bottleneck is physics, which is where the bottleneck should be.

This isn't a marginal improvement. It's a category change. When your coordination latency drops from 50ms to 33 microseconds, things that were impossible become trivial. Closed-loop control across a mesh of autonomous devices. Real-time consensus between robots on a factory floor. Swarm coordination where the mesh reacts faster than any individual node's control loop. These aren't theoretical. They're what happens when the software gets out of the way and the only remaining constraint is the speed of light.

Not every event needs to stay local. A motor's torque feedback at 10kHz needs to close the loop in microseconds — that stays on the mesh, between the sensor and the actuator, never leaving the floor. But the vibration pattern that predicts bearing failure next week? That can travel to a data center where a model with 100GB of training data runs inference on it. The anomaly detection that requires comparing this motor's signature against a fleet of 10,000 motors across 200 facilities? That belongs in the cloud, where the compute and the historical data live.

The mesh doesn't replace the data center. It separates what must be fast from what must be smart. Time-critical control loops run locally at microsecond latencies. Expensive analysis, model inference, fleet-wide correlation, long-term storage — those flow to the data center on the mesh's own terms, when the local node decides to send them, not when a polling interval fires. The local node is autonomous. It acts first, reports later. The data center adds intelligence, not authority.

This is the split that current architectures can't make cleanly. When everything routes through the cloud, the 10kHz control loop and the weekly predictive model share the same 50ms round trip. One is 1500x too slow, the other doesn't care. Net uses tiered routing with proximity graphs to find each event's natural home — the fast ones stay local, the complex ones travel to where the compute is. The subnet hierarchy, channel visibility, and capability-based routing make this split explicit in the protocol, not an afterthought bolted onto a cloud API.

## Why not cloud

Cloud infrastructure solves the wrong problem. It moves compute closer to a central provider. Net moves compute closer to the data and the work.

Cloud adds a trusted intermediary by definition — your traffic routes through someone else's infrastructure, on their terms, visible to their systems, subject to their availability. Net has no intermediaries. Relay nodes forward encrypted bytes they cannot read. There is no Cloudflare, no AWS, no Azure in the path because the path is yours.

Cloud economics assume you don't own the hardware. Edge compute assumes the edge is theirs. Net assumes the edge is you — your computer, your servers, your devices, your mesh.

A manufacturing plant running on Net doesn't route sensor data to AWS us-east-1 and back. The sensor talks directly to the decision system on the factory floor. The latency is physics, not geography plus cloud overhead.

This isn't anti-cloud. It's post-cloud. Cloud was the right answer when compute was scarce and hardware was expensive. Compute is abundant. Hardware is cheap. The coordination layer should reflect that.

## Security

The mesh is encrypted end-to-end with no trusted intermediaries. This isn't a layer on top — it's a consequence of how forwarding works.

**No plaintext on relays.** Zero-copy forwarding means relay nodes pass encrypted bytes through without decrypting. There's no moment where the payload is readable in memory on an untrusted node. Nothing to sniff, nothing to dump, nothing to log.

**No parsing means no code execution surface.** A relay never interprets the payload. It doesn't know if it's forwarding JSON, binary, or garbage. It moves bytes. You can't exploit a parser bug in content the relay never parses. The attack surface of a relay is the routing header, not the content.

**No clock dependency.** Zero time synchronization attack surface. The protocol has no dependency on wall clocks, NTP, or synchronized time. Event ordering is causal — parent hashes, sequence numbers, vector clocks — not temporal. An attacker who manipulates a node's system clock, poisons its NTP source, or skews time across a subnet cannot disrupt causal ordering, cannot forge event sequences, and cannot collapse the mesh. A captured tower broadcasting adversarial timestamps disrupts clock-dependent protocols across its coverage area. Net is unaffected because protocol consistency does not depend on timestamp agreement. The network's consistency model is causal, not temporal. Ordering is cryptographic.

**Compromise of a relay leaks nothing.** Even with full root access and memory dumps, an attacker who owns a relay gets encrypted bytes with no key material. Session keys are between source and destination. The relay was never part of the key exchange.

**No connection state to hijack.** There's no TCP session to take over, no cookie to steal, no sequence number to predict. State propagates through the mesh, not through connections. There's nothing persistent on the wire to attack.

This is different from TLS, where every hop that terminates TLS — load balancers, proxies, CDNs — sees plaintext. The standard web architecture is a chain of trusted intermediaries. Net has no trusted intermediaries. There's nothing to trust them with.

## The Blackwall

In Cyberpunk, the Blackwall isn't a wall around the threats. It's a wall around the safe zone. The public net is a small, whitelisted set of servers that Netwatch has cleared. Everything outside — private corps nets, data forts, rogue AIs, uncleared infrastructure — is the vast majority. The wall protects the known from the unknown.

Net works the same way. The "safe mesh" is the part you can observe: nodes that respond within heartbeat intervals, honor their capability announcements, don't flood, respect TTL. Safety isn't declared by an authority. It's derived from consistent, observable behavior. The Blackwall is the boundary where observation ends — beyond your proximity graph, beyond your gateways' subnet summaries, beyond what you can derive. Not necessarily hostile. Just unknown. And unknown gets no trust by default.

The wall isn't one mechanism. It's the emergent effect of every constraint working together:

- **Backpressure.** Nodes limit in-flight events, prevent overload, and apply pushback by going silent. No node can be forced to accept more than it can process.
- **Bounded queues.** No infinite buffers. Ring buffers have explicit capacity limits. Memory usage is predictable and fixed. A flood fills a buffer and gets evicted, it doesn't grow the buffer.
- **Fanout limits.** Events don't propagate to everyone. Dissemination is controlled by the proximity graph and routing table. This prevents O(n²) explosion — an event reaches the nodes that need it, not every node on the mesh.
- **Deduplication.** The same event doesn't explode repeatedly. Idempotency at the event level protects against loops and amplification.
- **TTL and propagation limits.** Events expire. Pingwaves have a hop radius. Nothing propagates forever. A misbehaving node's traffic dies at the boundary of its TTL, not at the edge of the mesh.
- **Rate limiting.** Per-node, per-peer limits. One node cannot flood the mesh. Its neighbors enforce their own limits independently through device autonomy rules.

Any single mechanism can be overwhelmed. All of them together form the wall. An event that bypasses backpressure hits the bounded queue. An event that fills the queue gets evicted. An event that propagates too far hits the TTL. An event that duplicates gets deduplicated. A node that floods gets rate-limited by every neighbor independently. There is no single point to breach because the wall is the mesh itself.

## Implementation

For implementation details — capabilities, proximity graphs, subnets, channels, daemons, safety envelopes, module map, and code examples — see the [crate README](net/crates/net/README.md).

## SDKs

All SDKs wrap the same Rust core. The SDK is the developer experience, the engine is Rust.

| SDK | Package | Install |
|-----|---------|---------|
| **Rust** | [`net-mesh-sdk`](https://crates.io/crates/net-mesh-sdk) ([source](net/crates/net/sdk)) | `cargo add net-mesh-sdk` |
| **TypeScript** | [`@net-mesh/sdk`](https://www.npmjs.com/package/@net-mesh/sdk) ([source](net/crates/net/sdk-ts)) | `npm install @net-mesh/sdk @net-mesh/core` |
| **Python** | [`net-mesh-sdk`](https://pypi.org/project/net-mesh-sdk/) ([source](net/crates/net/sdk-py)) | `pip install net-mesh-sdk` |
| **C** | [`net.h`](net/crates/net/include/net.h) | `cargo build --release --features ffi,net` (build cdylib + bundle the header) |
| **Go** | [`go`](go/) | `go get github.com/ai-2070/net/go` |

Lower-level bindings (skip the SDK ergonomics, talk directly to the engine):

| Binding | Package | Install |
|---------|---------|---------|
| **Rust core** | [`net-mesh`](https://crates.io/crates/net-mesh) | `cargo add net-mesh` |
| **Node binding** | [`@net-mesh/core`](https://www.npmjs.com/package/@net-mesh/core) | `npm install @net-mesh/core` |
| **Python binding** | [`net-mesh`](https://pypi.org/project/net-mesh/) | `pip install net-mesh` |

## Benchmarks

Net doesn't try to beat specialists at their specializations. LMAX Disruptor wins at in-process event passing. Aeron IPC wins at shared-memory transport. DPDK wins at raw UDP throughput. These tools have had 10–15 years of focused work on one problem each. Against their narrow benchmarks, Net loses by 2–15x.

Real workloads don't use one primitive. They stitch specialists together — Kafka plus gRPC plus Redis plus a service mesh plus custom glue. Net composes transport, routing, encryption, identity, causal ordering, and failure recovery as a single substrate at the same speed class as each specialist. Against the composed workflow, Net wins by 100x or more.

Specialists win their single operation. Net wins the full workflow.

All numbers below measure **packet scheduling** — the time to process, route, encrypt, and queue a packet for transmission. They do not include NIC transfer, wire latency, or speed-of-light propagation. The software layer is what these benchmarks prove is no longer the bottleneck.

**Test systems:** Apple M1 Max (macOS) and Intel i9-14900K @5GHz (Windows 11). Full results in [BENCHMARKS.md](net/crates/net/BENCHMARKS.md).

### Routing

| Operation | M1 Max | i9-14900K |
|-----------|--------|-----------|
| Header serialize | 1.98 ns / **505M ops/sec** | 1.31 ns / **762M ops/sec** |
| Header deserialize | 2.11 ns / **475M ops/sec** | 1.21 ns / **829M ops/sec** |
| Routing header serialize | 0.63 ns / **1.59G ops/sec** | 0.46 ns / **2.18G ops/sec** |
| Routing header forward | 0.57 ns / **1.75G ops/sec** | 0.20 ns / **5.06G ops/sec** |
| Routing lookup (hit) | 38.09 ns / **26.3M ops/sec** | 37.52 ns / **26.7M ops/sec** |
| Decision pipeline | 38.89 ns / **25.7M ops/sec** | 38.62 ns / **25.9M ops/sec** |

### Multi-hop Forwarding

| Hops | M1 Max | i9-14900K |
|-----:|--------|-----------|
| 1 | 59.07 ns / **16.9M ops/sec** | 53.37 ns / **18.7M ops/sec** |
| 2 | 117.32 ns / **8.52M ops/sec** | 86.87 ns / **11.5M ops/sec** |
| 3 | 163.16 ns / **6.13M ops/sec** | 120.66 ns / **8.29M ops/sec** |
| 5 | 273.51 ns / **3.66M ops/sec** | 189.73 ns / **5.27M ops/sec** |

| Threads | M1 Max | i9-14900K |
|--------:|--------|-----------|
| 4 | 4.68 M/s | 7.09 M/s |
| 8 | 8.14 M/s | 9.96 M/s |
| 16 | 7.93 M/s | 12.8 M/s |

### Failure Detection & Recovery

| Operation | M1 Max | i9-14900K |
|-----------|--------|-----------|
| Heartbeat (existing node) | 29.03 ns / **34.5M ops/sec** | 35.27 ns / **28.4M ops/sec** |
| Status check | 14.74 ns / **67.8M ops/sec** | 13.42 ns / **74.5M ops/sec** |
| Circuit breaker check | 13.44 ns / **74.4M ops/sec** | 10.17 ns / **98.4M ops/sec** |
| Recovery (evaluate alternates) | 251.67 ns / **3.97M ops/sec** | 254.20 ns / **3.93M ops/sec** |
| Full fail + recover cycle | 287.97 ns / **3.47M ops/sec** | 255.40 ns / **3.92M ops/sec** |

### Swarm / Discovery

| Operation | M1 Max | i9-14900K |
|-----------|--------|-----------|
| Pingwave serialize | 0.78 ns / **1.28G ops/sec** | 0.54 ns / **1.86G ops/sec** |
| Pingwave roundtrip | 0.93 ns / **1.07G ops/sec** | 0.65 ns / **1.55G ops/sec** |
| New peer discovery | 113.27 ns / **8.83M ops/sec** | 151.82 ns / **6.59M ops/sec** |

| Nodes | M1 Max (all_nodes) | i9-14900K (all_nodes) |
|------:|-------------------:|----------------------:|
| 100 | 2.52 us | 7.47 us |
| 500 | 8.09 us | 16.09 us |
| 1,000 | 132.7 us | 26.86 us |
| 5,000 | 113.29 us | 237.66 us |

### Encryption (ChaCha20-Poly1305)

| Payload | M1 Max | i9-14900K |
|--------:|--------|-----------|
| 64B | 483.14 ns / 126.3 MiB/s | 1.14 us / 53.7 MiB/s |
| 256B | 922.69 ns / 264.6 MiB/s | 1.20 us / 203.0 MiB/s |
| 1KB | 2.69 us / 362.8 MiB/s | 1.58 us / 618.4 MiB/s |
| 4KB | 9.74 us / 400.9 MiB/s | 3.10 us / 1.23 GiB/s |

### Capability System

| Operation | M1 Max | i9-14900K |
|-----------|--------|-----------|
| Filter (single tag) | 9.97 ns / **100M ops/sec** | 3.43 ns / **291M ops/sec** |
| Filter (require GPU) | 4.05 ns / **247M ops/sec** | 1.78 ns / **561M ops/sec** |
| GPU check | 0.31 ns / **3.21G ops/sec** | 0.20 ns / **5.01G ops/sec** |
| Capability announcement | 374.61 ns / **2.67M ops/sec** | 2.34 us / **428K ops/sec** |

| Nodes | M1 Max (tag query) | i9-14900K (tag query) |
|------:|-------------------:|----------------------:|
| 1,000 | 12.53 us | 10.35 us |
| 5,000 | 70.27 us | 54.41 us |
| 10,000 | 154.98 us | 171.99 us |
| 50,000 | 2.56 ms | 1.23 ms |

### Multi-threaded Scaling (thread-local pool)

| Threads | M1 Max | i9-14900K |
|--------:|--------|-----------|
| 8 | **9.18 M/s** | **5.45 M/s** |
| 16 | **9.42 M/s** | **8.43 M/s** |
| 32 | **9.80 M/s** | **9.89 M/s** |

Pool contention (thread-local acquire/release):

| Threads | M1 Max | i9-14900K |
|--------:|--------|-----------|
| 8 | **72.5 M/s** | **63.2 M/s** |
| 16 | **70.4 M/s** | **88.0 M/s** |
| 32 | **76.9 M/s** | **110.4 M/s** |

### SDK Ingestion

| SDK | Method | Throughput | Latency |
|-----|--------|------------|---------|
| Go | IngestRaw (9B) | **6.31M/sec** | 158 ns |
| Go | Batch (1000) | **5.71M/sec** | 175 ns/event |
| Python | ingest_raw (9B) | **5.69M/sec** | 0.18 us |
| Python | Batch (1000) | **6.97M/sec** | 0.14 us |
| Node.js | pushBatch | **5.08M/sec** | 0.20 us |
| Node.js | push (single) | **3.96M/sec** | 0.25 us |
| Bun | pushBatch | **5.37M/sec** | 0.19 us |
| Bun | push (single) | **3.93M/sec** | 0.25 us |

All benchmarks re-captured 2026-04-27 on M1 Max with release-mode bindings.

All SDKs exceed **3.93M events/sec** even on single-event ingestion, and **5M+ events/sec** on batch. Go now leads single-event ingestion at **6.31M/sec** (zero allocations on raw ingestion path). Python (via PyO3) is the fastest binding on batch at **6.97M/sec** — the GIL releases for the duration of the FFI call so per-event overhead is the bare PyO3 marshalling. Node.js sync methods are ~41x faster than async (`push` 3.96M vs async `ingestRaw` 96K). Bun batch (5.37M) is ~6% faster than Node.js batch (5.08M) on the same `pushBatch` call.

### RedEX (storage primitive)

Microbenchmarks of the local append-only log on its own, separate from CortEX. Answers "is the log ever the bottleneck?" Numbers below are on M1 Max (macOS).

| Operation | Latency | Throughput |
|-----------|--------:|-----------:|
| Append inline (≤8 B) | 47 ns | **21.3M ops/sec** |
| Append heap (32 B) | 54 ns | **18.6M ops/sec** |
| Append heap (256 B) | 97 ns | **10.3M ops/sec** |
| Append heap (1 KB) | 240 ns | **4.17M ops/sec** |
| Batch append (64 × 64 B) | 1.72 us | **37.2M elements/sec** |
| Append disk (32 B, `redex-disk`) | 3.11 us | **321k ops/sec** |
| Append disk (1 KB, `redex-disk`) | 6.42 us | **156k ops/sec** |
| Tail latency (append → subscriber) | 138 ns | -- |

Disk durability costs ~50x the memory-only append path and caps throughput around **hundreds of thousands of events/sec per file** — ample headroom for every workload where the event rate is bounded by the hardware generating it (sensors, instruments, telemetry) rather than by software replaying synthetic load.

### CortEX + NetDB (end-to-end)

The numbers that matter for real workloads — ingest, fold, query, snapshot — measured through the full `TasksAdapter` / `MemoriesAdapter` / `NetDb` stack with RedEX underneath. This is the slice a factory cell, substation, or Deck runs; the microbenchmarks above are how we know no single layer is load-bearing.

| Operation | Latency | Throughput |
|-----------|--------:|-----------:|
| `tasks.create` ingest (no barrier) | 113 ns | **8.87M ops/sec** |
| `memories.store` ingest | 218 ns | **4.58M ops/sec** |
| Fold round-trip (`create` + `waitForSeq`) | 5.59 us | **179k ops/sec** |
| `find_unique` (state lookup) | 8.98 ns | **111M ops/sec** |
| `find_many` @ 1 K tasks (status filter) | 7.61 us | **131M elements/sec** |
| `find_many` @ 10 K tasks | 125 us | **80.2M elements/sec** |
| `count_where` @ 10 K tasks | 6.67 us | **1.50G elements/sec** |
| `find_many` @ 1 K memories (tag filter) | 49.4 us | **20.3M elements/sec** |
| Tasks snapshot encode @ 10 K | 83.2 us | -- |
| Memories snapshot encode @ 10 K | 697 us | -- |
| `NetDb::open` (both models) | 6.30 us | **159k ops/sec** |
| Bundle encode @ 1 K (48 KB output) | 31.8 us | -- |
| Bundle decode @ 1 K | 26.5 us | -- |
| Bundle decode @ 10 K | 203 us | -- |



### Binary size

`[profile.release]`: `lto = true`, `codegen-units = 1`, `panic = "abort"`, `opt-level = 3`. Three additional profiles ship in the crate's `Cargo.toml`: `release-with-debug` (release + `debug = true` for profiling), `native` (release + thin LTO for faster local links — pair with `RUSTFLAGS="-C target-cpu=native"`), and `bench` (full LTO, single codegen unit).

Feature set affects `.rlib` and `.a` (which keep all compiled code for downstream linking) but is **near-invisible on the shipped cdylib for pure-Rust features** — LTO + dead-code elimination strips unreferenced code across feature boundaries, so the deployed `.dylib`/`.dll`/`.so` stays near-constant across the storage / compute / NAT-classifier combinations. Features that pull in substantial external dependency trees (UPnP's HTTP + XML stack) are the exception, and the table makes that explicit.

**Core cdylib** (`libnet.dylib`, the engine the bindings consume):

| Features | `libnet.dylib` (cdylib) | `libnet.rlib` | `libnet.a` |
|----------|------------------------:|--------------:|-----------:|
| `net` | **1.92 MB** | 22.4 MB | 35.3 MB |
| `net` + `redex` | **1.92 MB** | 22.8 MB | 35.6 MB |
| `net` + `redex` + `redex-disk` | **1.92 MB** | 22.9 MB | 35.7 MB |
| `net` + `redex` + `redex-disk` + `cortex` | **1.92 MB** | 24.8 MB | 36.6 MB |
| `net` + `redex` + `redex-disk` + `cortex` + `netdb` | **2.21 MB** | 25.9 MB | 37.3 MB |
| `net` + `nat-traversal` | **2.03 MB** | 23.6 MB | 36.1 MB |
| `net` + `nat-traversal` + `port-mapping` | **3.44 MB** | 27.0 MB | 47.6 MB |

**Binding cdylib** (`libnet_node.dylib`, the `.node` file shipped to Node users; the Python PyO3 module has the same shape):

| Features | `libnet_node.dylib` |
|----------|--------------------:|
| `net` | **2.68 MB** |
| `net` + `compute` | **3.02 MB** |
| `net` + `compute` + `groups` | **3.23 MB** |

- `libnet.dylib` — shipped core cdylib (consumed by Node / Python / C bindings).
- `libnet.rlib` — Rust static lib with metadata (consumed by other Rust crates).
- `libnet.a` — C/C++ static lib, pre-LTO, expected for `staticlib`.
- `libnet_node.dylib` — Node binding cdylib (what ships as `net.darwin-arm64.node`).

Measured on `aarch64-apple-darwin`, 2026-04-24.

The core cdylib stays at **1.92 MB across the four storage / compute combinations** — opting into RedEX, disk durability, or CortEX adds well under 1% to the deployed binary because dead-code elimination strips whatever the caller doesn't reference. `netdb` (the cross-model query façade that builds on `cortex`) adds **~301 KB** of Prisma-style query code paths. `nat-traversal` adds **~112 KB** (classifier FSM + rendezvous wire codec + the `connect_direct` orchestration path). `port-mapping` is the outlier at **+1.41 MB** — the extra weight is `igd-next`'s UPnP-IGD client, which pulls in a SOAP / XML stack and HTTP machinery that the rest of the mesh doesn't use; NAT-PMP alone is ~100 lines of wire codec inlined in the crate (no external dep), so a deployment that only needs NAT-PMP could strip UPnP support and stay near the `nat-traversal` line.

The `compute` and `groups` features live at the binding / SDK layer (`net-sdk`'s `DaemonRuntime`, `Mikoshi` migration orchestrator, `ReplicaGroup` / `ForkGroup` / `StandbyGroup`) rather than in the core crate, so they don't appear in the core cdylib table. Enabling them on the binding cdylib adds **~349 KB** for `compute` and another **~216 KB** for `groups` on top — the Node `.node` file grows from 2.68 MB to 3.23 MB with the full stack. The `.rlib` and `.a` grow with features because they must preserve every compiled symbol for downstream linkers; only the shipped cdylibs feel the full benefit of LTO.
