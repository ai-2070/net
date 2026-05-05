# Net

High-performance encrypted mesh runtime.

For the design philosophy, architecture rationale, and benchmarks, see the [project README](../../README.md).

## Install

```bash
# Rust SDK
cargo add ai2070-net-sdk

# TypeScript / Node SDK
npm install @ai2070/net-sdk @ai2070/net

# Python SDK
pip install ai2070-net-sdk

# Go binding
go get github.com/ai-2070/net/go
```

Lower-level packages (skip the SDK ergonomics, talk directly to the engine):

```bash
cargo add ai2070-net          # Rust core
npm install @ai2070/net       # NAPI binding
pip install ai2070-net        # PyO3 binding
```

Crate / module names inside source code (`net::`, `net_sdk::`, `from net import`, `from net_sdk import`) stayed stable across the rename via package aliasing. The registry-side names are `ai2070-net*` / `@ai2070/net*`. Per-language usage in [SDKs](#sdks); building the C SDK in [Building](#building).

## Contents

- [Install](#install)
- [Key Concepts](#key-concepts)
- [Stack](#stack)
- [Architecture](#architecture)
- [Net Header](#net-header-64-bytes-cache-line-aligned)
- [Performance](#performance)
- [Capabilities](#capabilities)
- [Proximity & Discovery](#proximity--discovery)
- [Subnets](#subnets)
- [Channels](#channels)
- [Daemons](#daemons)
- [Safety & Autonomy](#safety--autonomy)
- [RedEX](#redex)
- [CortEX](#cortex)
- [NetDB](#netdb)
- [nRPC](#nrpc)
- [Module Map](#module-map)
- [Adapters](#adapters)
- [SDKs](#sdks)
- [Features](#features)
- [Building](#building)
- [Tests](#tests)
- [Benchmarks](#benchmarks)
- [Test Architecture](#test-architecture)
- [Subprotocol ID Space](#subprotocol-id-space)
- [License](#license)

## Key Concepts

**Identity is cryptographic.** Every node has an ed25519 keypair. The public key IS the identity. `origin_hash` (truncated BLAKE2s) is stamped on every outgoing packet. Permission tokens are ed25519-signed, delegatable, and expirable.

**Channels are named and policy-bearing.** Hierarchical names like `sensors/lidar/front`. Access control via capability filters (does this node have a GPU? the right tool? the right tag?) combined with permission tokens. Authorization cached in a bloom filter for <10ns per-packet checks.

**Behavior is declarative.** Nodes announce hardware/software capabilities, expose API schemas, and publish metadata. A rule engine enforces device autonomy policies. Load balancing, proximity-aware routing, and safety envelopes operate on this semantic layer. Distributed context propagation enables cross-node tracing.

**Subnets are hierarchical.** 4-level encoding (region/fleet/vehicle/subsystem) in 4 bytes. Gateways enforce channel visibility at subnet boundaries. Label-based assignment from capability tags.

**State is causal.** Every event carries a 24-byte `CausalLink`: origin, sequence, parent hash, compressed horizon. The chain IS the entity's identity. If the chain breaks, a new entity forks with documented lineage.

**Compute migrates.** The `MeshDaemon` trait defines event processors. The runtime handles causal chain production, horizon tracking, and snapshot packaging. Migration is a 6-phase state machine preserving chain continuity across nodes.

**Compute replicates.** A `ReplicaGroup` manages N copies of a daemon as a logical unit. Each replica has its own identity (derived deterministically from a group seed) and its own causal chain. The group load-balances events across replicas, tracks group-level health, spreads placement across failure domains, and auto-replaces failed replicas without migration — stateless re-spawn with the same deterministic identity.

**Subprotocols are extensible.** `subprotocol_id: u16` in every header. Formal registry with version negotiation. Unknown subprotocols are forwarded opaquely. Vendor protocols get IDs in `0x1000..0xEFFF`.

**Observation is local.** Each node's truth is what it can observe. Causal cones answer "what could have influenced this event?" Propagation modeling estimates latency by subnet distance. Continuity proofs (36 bytes) verify chain integrity without the full log.

**Partitions heal honestly.** Correlated failure detection classifies mass failures by subnet correlation. When partitions heal, divergent entity logs are reconciled: longest chain wins, deterministic tiebreak, losing chains fork with documented lineage.

**The event bus is non-localized.** Unlike broker-based systems (Kafka, Pulsar) or single-process ring buffers (LMAX Disruptor), the event bus has no fixed location. Local ring buffers are speed buffers; the logical bus spans the mesh. No broker to provision or fail over. No plaintext at relay nodes. No partition-leader bottleneck — ordering is per-entity via causal chains, not per-partition via a single leader. Events exist in transit; storage is a choice via adapters, not an architectural requirement.

**Event consumption is location-transparent.** A `MeshDaemon` receives events through the same `process(&CausalEvent)` interface regardless of whether the event originated locally, one hop away, or across the mesh. The mesh handles routing, decryption, and chain validation before the daemon sees the event. Code written for a single-node prototype runs unmodified on a multi-hop deployment. The topology is a runtime decision, not a code change.

**Capability announcements drive routing.** Every node advertises what it can do — hardware (GPU model, VRAM, CPU cores), software (loaded models, tools, supported subprotocols), and capacity (available slots, current load). The `CapabilityIndex` indexes these announcements for sub-microsecond queries. Routing decisions use capability tags: a request for inference routes to the nearest node with a matching GPU, not to a fixed endpoint. `CapabilityDiff` propagates incremental updates — a node that loads a new model announces only the delta.

**The proximity graph is the topology.** Each node maintains a `ProximityGraph` of its neighborhood built from direct observation and `EnhancedPingwave` broadcasts. Edges carry measured latency. The graph answers "who is nearby and how fast can I reach them?" without a global directory. Combined with capability announcements, it answers "who nearby can do what I need?" Routing follows the graph — traffic flows toward nodes that are close and capable.

**Subnets partition the mesh hierarchically.** A `SubnetId` encodes 4 levels (region/fleet/vehicle/subsystem) in 4 bytes. Subnets constrain observation — a node observes its peers at its level and derives the rest through gateways. `SubnetGateway` nodes aggregate health, compress capability summaries, and enforce channel visibility at boundaries. `SubnetPolicy` assigns nodes to subnets from capability labels. This keeps observation cost bounded as the mesh grows.

**Channels are the pub/sub layer.** `ChannelName` uses hierarchical hashing (`sensors/lidar/front`) with wildcard support. `ChannelConfig` sets per-channel policies: visibility (public, subnet-local, private), required capabilities, and retention. `AuthGuard` enforces access control at the channel boundary using a bloom filter — <10ns per-packet authorization checks. Channels are how applications structure communication without coupling to node identity.

**Daemons are the compute unit.** The `MeshDaemon` trait defines a stateful event processor: receive a `CausalEvent`, produce output, maintain a causal chain. `DaemonHost` manages the lifecycle — initialization, event dispatch, chain production, horizon tracking, snapshot packaging. `DaemonRegistry` maps daemon types to constructors. The `PlacementScheduler` decides where to run daemons based on capability requirements. When a node fails, the migration state machine moves the daemon's state (snapshot + chain) to a new host in 6 phases, preserving continuity.

**Safety envelopes enforce autonomy.** Every node runs a `SafetyEnforcer` that defines resource limits, rate caps, and kill-switch conditions via `ResourceEnvelope`. A `RuleEngine` evaluates device autonomy policies — declarative rules that determine what a node will accept, reject, or redirect. No external authority can override a node's safety envelope. The mesh routes around nodes that refuse work, it doesn't force them.

## Stack

| Layer | What it does | Docs |
|-------|--------------|------|
| **Transport** | Encrypted UDP, 64-byte cache-line-aligned header, zero-alloc packet pools, multi-hop forwarding, adaptive batching, fair scheduling, failure detection, pingwave swarm discovery | [TRANSPORT.md](docs/TRANSPORT.md) |
| **Trust & Identity** | ed25519 entity identity, origin binding on every packet, permission tokens with delegation chains | [IDENTITY.md](docs/IDENTITY.md) |
| **Channels & Authorization** | Named hierarchical channels, capability-based access control, bloom filter authorization at <10ns per packet | [CHANNELS.md](docs/CHANNELS.md) |
| **Behavior Plane** | Capability announcements & indexing, capability diffs, node metadata, API schema registry, device autonomy rules, context fabric (distributed tracing), load balancing, proximity graph, safety envelope enforcement | [BEHAVIOR.md](docs/BEHAVIOR.md) |
| **Subnets & Hierarchy** | 4-level subnet hierarchy (8/8/8/8 encoding), label-based assignment, gateway visibility enforcement | [SUBNETS.md](docs/SUBNETS.md) |
| **Distributed State** | 24-byte causal links, compressed observed horizons, append-only entity logs with chain validation, state snapshots for migration | [STATE.md](docs/STATE.md) |
| **Compute Runtime** | MeshDaemon trait, daemon hosting, capability-based placement, 6-phase migration with snapshot chunking, replica groups, fork groups with verifiable lineage, active-passive standby groups, shared group coordination | [COMPUTE.md](docs/COMPUTE.md) |
| **Subprotocols** | Formal protocol registry, version negotiation, capability-aware routing via tags, opaque forwarding guarantee, migration message dispatch | [SUBPROTOCOLS.md](docs/SUBPROTOCOLS.md) |
| **Observational Continuity** | Causal cones, propagation modeling, continuity proofs, honest discontinuity with deterministic forking, superposition during migration | [CONTINUITY.md](docs/CONTINUITY.md) |
| **Contested Environments** | Correlated failure detection, subnet-aware partition classification, partition healing with log reconciliation | [CONTESTED.md](docs/CONTESTED.md) |
| **RedEX (local log)** | 20-byte append-only event records, inline + heap payload hybrid, `ChannelName`-bound files, atomic backfill-then-live tail, count + size retention, optional disk durability via `redex-disk` (torn-write truncation on reopen) | [REDEX_PLAN.md](docs/REDEX_PLAN.md) |
| **CortEX adapter** | Seam between Net events and RedEX storage: 20-byte `EventMeta` prefix projection, fold-driver spawning on a tokio task, `changes()` broadcast primitive for reactive queries, `Arc<RwLock<State>>` as the NetDB read surface, start-position + fold-error policies | [CORTEX_ADAPTER_PLAN.md](docs/CORTEX_ADAPTER_PLAN.md) |
| **CortEX models** | Concrete fold implementations: tasks (CRUD on `Task`) and memories (content + tags + pin, with single/any/all tag predicates). Each ships a Prisma-style query builder and a reactive watcher (initial + deduplicated emissions). Dispatches partitioned under `0x00..0x7F`. | [CORTEX_ADAPTER_PLAN.md](docs/CORTEX_ADAPTER_PLAN.md) |
| **NetDB (query façade)** | Unified `NetDb` handle bundling `TasksAdapter` + `MemoriesAdapter` under one object. Prisma-ish `find_unique` / `find_many(&filter)` / `count_where` / `exists_where` on per-model state. Whole-db snapshot/restore. Cross-language: Rust, Node, Python. | [NETDB_PLAN.md](docs/NETDB_PLAN.md) |

## Architecture

```
                    ┌──────────────────────────────────┐
                    │            EventBus              │
                    │  (sharded ring buffers, < 1us)   │
                    └──────────┬───────────────────────┘
                               │
              ┌────────────────┼────────────────┐
              │                │                │
        ┌─────┴─────┐   ┌─────┴─────┐   ┌──────┴──────┐
        │   Redis    │   │ JetStream │   │    Net     │
        │  Streams   │   │   (NATS)  │   │ (encrypted  │
        └───────────┘   └───────────┘   │  UDP mesh)  │
                                         └──────┬──────┘
                                                │
┌───────────────────────────────────────────────────────────────────┐
│                        Net Mesh Layers                          │
├──────────┬──────────┬──────────┬──────────┬──────────┬───────────┤
│ Identity │ Channels │ Behavior │  State   │ Compute  │ Contested │
│ ed25519  │ AuthGuard│ CAP-ANN  │ Causal   │ Daemon   │ Partition │
│ tokens   │ bloom    │ API-REG  │ chains   │ host     │ healing   │
│ origin   │ caps     │ rules    │ horizons │ scheduler│ reconcile │
├──────────┴──────────┴──────────┴──────────┴──────────┴───────────┤
│        Subnets (4-level hierarchy, gateway enforcement)          │
├──────────────────────────────────────────────────────────────────┤
│           Subprotocols + Observational Continuity                │
│        version negotiation, causal cones, fork records           │
├──────────────────────────────────────────────────────────────────┤
│                       Transport (Net)                           │
│     64B header, ChaCha20-Poly1305, Noise NK, zero-alloc pools   │
│     routing, swarm, failure detection, proximity graph           │
└──────────────────────────────────────────────────────────────────┘
```

Every field is used by at least one layer. Forwarding nodes read one cache line, make a routing decision, and forward without decrypting the payload.

## Net Header (64 bytes, cache-line aligned)

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|         MAGIC (0x4E45)        |     VER       |     FLAGS     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   PRIORITY    |    HOP_TTL    |   HOP_COUNT   |  FRAG_FLAGS   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       SUBPROTOCOL_ID          |        CHANNEL_HASH           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         NONCE (12 bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       SESSION_ID (8 bytes)                    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       STREAM_ID (8 bytes)                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       SEQUENCE (8 bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|      SUBNET_ID (4 bytes)      |     ORIGIN_HASH (4 bytes)     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       FRAGMENT_ID             |        FRAGMENT_OFFSET        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       PAYLOAD_LEN             |        EVENT_COUNT            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

## Routing Header (18 bytes)

Routed (multi-hop) packets prepend an 18-byte routing header to the Net header. Direct packets use the Net header alone.

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   ROUTING_MAGIC ("RT" = 0x52,0x54)  |  TTL  |   HOP_COUNT   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     FLAGS     |   RESERVED    |          SRC_ID (low)         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          SRC_ID (high)        |        DEST_ID (lowest)       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       DEST_ID (middle)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|        DEST_ID (highest)      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

`ROUTING_MAGIC` is the ASCII bytes `"RT"` (`0x52, 0x54`) on the wire, or `0x5452` as a little-endian `u16`. It's chosen disjoint from the Net header's `MAGIC = 0x4E45` so the receive loop distinguishes the two formats by peeking at bytes 0-1 alone. The previous 16-byte layout placed `dest_id` at bytes 0-7, which let a 1-in-65 536 `node_id` collide with `MAGIC` and silently mis-classify its own incoming routed packets as direct packets. Any node controls the collision probability by its own hash, so the 18-byte layout with explicit tag is the only reliable fix.

`SRC_ID` is the 32-bit routing-id projection of a node's 64-bit node_id (top bits truncated). `DEST_ID` is the full 64-bit node_id. `TTL` decrements at each forwarder; `HOP_COUNT` increments. `FLAGS` carry the `RouteFlags` bitmask (control / requires-ack / priority / end-of-stream).

## Performance

Benchmarked on Apple M1 Max, macOS.

| Layer | Operation | Latency | Throughput |
|-------|-----------|---------|------------|
| **Core** | Event ingestion | < 1 us p99 | 10M+ events/sec sustained |
| **Net** | Header serialize | 1.98 ns | 505M ops/sec |
| **Net** | Packet build (50 events) | 8.21 us | -- |
| **Net** | Encryption (ChaCha20) | 483 ns (64B) | -- |
| **Routing** | Header roundtrip | 0.94 ns | 1.07G ops/sec |
| **Routing** | Lookup hit | 38.1 ns | 26.3M ops/sec |
| **Routing** | Decision pipeline | 38.9 ns | 25.7M ops/sec |
| **Forwarding** | Per-hop (64B) | 29.7 ns | -- |
| **Forwarding** | 5-hop chain | 274 ns | 3.66M ops/sec |
| **Swarm** | Pingwave roundtrip | 0.93 ns | 1.07G ops/sec |
| **Swarm** | Graph (5,000 nodes) | 113 us | 44.1M/sec |
| **Failure** | Heartbeat | 29.0 ns | 34.5M ops/sec |
| **Failure** | Full recovery cycle | 288 ns | 3.47M ops/sec |
| **Capability** | Filter (single tag) | 9.97 ns | 100M ops/sec |
| **Capability** | GPU check | 0.31 ns | 3.21G ops/sec |
| **Auth** | Bloom filter check | ~20 ns | 49.3M ops/sec |
| **SDK** | Go raw ingest | 158 ns | 6.31M/sec |
| **SDK** | Python batch ingest | 0.14 us | 6.97M/sec |
| **SDK** | Node.js push batch | 0.20 us | 5.08M/sec |
| **SDK** | Bun push batch | 0.19 us | 5.37M/sec |
| **RedEX** | Append inline (≤8 B) | 47 ns | 21.3M ops/sec |
| **RedEX** | Append heap (32 B) | 54 ns | 18.6M ops/sec |
| **RedEX** | Append heap (256 B) | 97 ns | 10.3M ops/sec |
| **RedEX** | Append heap (1 KB) | 240 ns | 4.17M ops/sec |
| **RedEX** | Batch append (64 × 64 B) | 1.72 us | 37.2M elements/sec |
| **RedEX** | Append disk (32 B, `redex-disk`) | 3.11 us | 321k ops/sec |
| **RedEX** | Append disk (1 KB, `redex-disk`) | 6.42 us | 156k ops/sec |
| **RedEX** | Tail latency (append → subscriber) | 138 ns | -- |
| **CortEX** | `tasks.create` ingest | 113 ns | 8.87M ops/sec |
| **CortEX** | `memories.store` ingest | 218 ns | 4.58M ops/sec |
| **CortEX** | Fold round-trip (`create` + `waitForSeq`) | 5.59 us | 179k ops/sec |
| **CortEX** | `find_unique` (state lookup) | 8.98 ns | 111M ops/sec |
| **CortEX** | `find_many` @ 1 K tasks (status filter) | 7.61 us | 131M elements/sec |
| **CortEX** | `find_many` @ 10 K tasks | 125 us | 80.2M elements/sec |
| **CortEX** | `count_where` @ 10 K tasks | 6.67 us | 1.50G elements/sec |
| **CortEX** | `find_many` @ 1 K memories (tag filter) | 49.4 us | 20.3M elements/sec |
| **CortEX** | Tasks snapshot encode @ 10 K | 83.2 us | -- |
| **CortEX** | Memories snapshot encode @ 10 K | 697 us | -- |
| **NetDB** | `NetDb::open` (both models) | 6.30 us | 159k ops/sec |
| **NetDB** | Bundle encode @ 1 K (48 KB output) | 31.8 us | -- |
| **NetDB** | Bundle decode @ 1 K | 26.5 us | -- |
| **NetDB** | Bundle decode @ 10 K | 203 us | -- |

Benchmarks accurate as of 2026-04-27.

1,146 Rust tests + 36 Node + 33 Python SDK smoke tests. ~2MB deployed binary.

## Capabilities

Every node advertises what it can do. `HardwareCapabilities` describes the machine — GPU model, VRAM, CPU cores, available memory. The `CapabilityIndex` indexes all known nodes' capabilities for sub-microsecond queries.

```
Node A announces:
  gpu: RTX 4090, vram: 24GB
  models: [gemma-21b, llama-7b]
  tags: [inference, cuda]
  capacity: 8 slots available

Node B queries:
  CapabilityIndex::query(require_gpu(24GB) & tag("inference"))
  → returns [Node A] in ~10ns
```

Capabilities are not static. When a node loads a new model, drops a tool, or runs out of capacity, it publishes a `CapabilityDiff` — an incremental update, not a full re-announcement. The `DiffEngine` computes minimal diffs. Neighbors propagate diffs through the proximity graph, so the mesh converges without flooding.

Routing follows capabilities. A request tagged `subprotocol:0x1000` routes to the nearest node that advertises support for that subprotocol. An inference request routes to the nearest node with enough VRAM. The mesh doesn't have fixed endpoints — it has a capability graph, and traffic flows toward capability.

The `CapabilityAd` struct is what travels on the wire: compact, versioned, and signed with the node's identity. A node that claims capabilities it doesn't have will be routed around when its behavior diverges from its advertisement — the proximity graph measures actual latency, not claimed latency.

**Scoped discovery via reserved tags.** Capability announcements gossip permissively across the mesh, but providers can narrow *who their query result reaches* by tagging their `CapabilitySet` with reserved `scope:*` tags. The wire format and forwarders are untouched — `find_nodes_scoped(filter, scope)` evaluates the tags as a post-filter on the index. Useful for per-tenant pools, per-region rendezvous, and subnet-local app discovery.

| Tag                       | Effect                                                                          |
| ------------------------- | ------------------------------------------------------------------------------- |
| _(no `scope:*` tag)_      | `Global` (default) — visible to every query that doesn't explicitly opt out.    |
| `scope:subnet-local`      | Visible only under `ScopeFilter::SameSubnet` queries.                           |
| `scope:tenant:<id>`       | Visible to `ScopeFilter::Tenant(<id>)` (and `Tenants` lists containing `<id>`). Hidden from other tenants and from `GlobalOnly`. |
| `scope:region:<name>`     | Visible to `ScopeFilter::Region(<name>)` (and `Regions` lists containing `<name>`). Hidden from other regions and from `GlobalOnly`. |

Strictest scope wins (`subnet-local` > tenants/regions > global). Enforcement is **query-side only**, not on the path; cross-tenant *routing* still flows freely. Full design: [`SCOPED_CAPABILITIES_PLAN.md`](docs/SCOPED_CAPABILITIES_PLAN.md).

## Proximity & Discovery

Nodes find each other through `Pingwave` — periodic broadcasts that propagate outward within a configurable hop radius. A pingwave carries the node's identity, capabilities summary, and a timestamp. If you can hear a node's pingwave, you know it exists, how far away it is, and what it can do.

The `ProximityGraph` is built from direct observation. Each node maintains a local view of its neighborhood — not a global directory. Edges carry measured RTT latency. The graph is continuously updated from pingwave observations and direct communication.

```
ProximityGraph for Node A:
  Node B — 0.3ms (direct neighbor)
  Node C — 0.7ms (via B)
  Node D — 1.2ms (via B → C)
  Gateway G — 2.1ms (subnet boundary)
```

`EnhancedPingwave` extends the basic pingwave with capability summaries and load indicators, so routing decisions can be made from the proximity graph alone without querying the full `CapabilityIndex`.

**Pingwaves install routes.** On receipt of a pingwave for origin Y forwarded by direct peer Z, node X calls `RoutingTable::add_route_with_metric(Y, next_hop=Z, metric=hop_count+2)` and inserts the `Z → Y` edge into `ProximityGraph::edges`. The `+2` metric keeps direct routes (metric 1) strictly better than any pingwave-installed route. Four loop-avoidance rules sit at the dispatch boundary: origin self-check (drop pingwaves with `origin == self_id`), `MAX_HOPS = 16` receive-time cap, split horizon (don't advertise a route back on the link used to reach it), and unregistered-source rejection (only registered direct peers can inject routing state). Latency EWMA per `(origin, next_hop)` edge provides an equal-hop tie-breaker for future multi-alternate ranking. See [`ROUTING_DV_PLAN.md`](docs/ROUTING_DV_PLAN.md).

Discovery is emergent. There are no bootstrap servers, no DNS, no service registry. After first contact (manual address, LAN broadcast, QR code, cached peers), pingwaves propagate and the proximity graph builds itself. Nodes that go silent are pruned. Nodes that appear are integrated. The graph is always a reflection of current reality.

## NAT Traversal

**Optimization, not correctness.** Two peers behind NATs already reach each other through routed handshakes + relay forwarding — the fallback path never goes away. What NAT traversal adds is a shorter path when a direct punch is feasible, cutting the per-packet relay tax and the load concentrated on topological relays. Nothing below is required to talk to NATed peers; it's required to talk to them *faster*. Full design in [`NAT_TRAVERSAL_PLAN.md`](docs/NAT_TRAVERSAL_PLAN.md).

**Classification is peer-probed, not STUN-style.** Each node sends a reflex probe on `SUBPROTOCOL_REFLEX` (`0x0D00`) to a small set of connected peers and classifies itself as `Open`, `Cone`, `Symmetric`, or `Unknown` from the observed reflex addresses. The result rides on capability announcements as a `nat:*` tag + a dedicated `reflex_addr` field, so every peer gains a direct-connect candidate without a separate discovery round-trip.

**Rendezvous is three messages on `SUBPROTOCOL_RENDEZVOUS` (`0x0D01`).** A sends `PunchRequest` to a mutually-connected coordinator R; R fans out `PunchIntroduce` to both A and B carrying the counterpart's reflex + a synchronized `fire_at`; at `fire_at` each side sends a short keep-alive train to prime NAT state, and the observer fires a `PunchAck` via the routed path to confirm. A pair-type matrix (plan §8) decides per connection whether to punch, skip (Symmetric × Symmetric), or go direct — `MeshNode::connect_direct` drives this end-to-end.

**Port mapping is opt-in.** `MeshNodeConfig::with_try_port_mapping(true)` spawns a task that probes NAT-PMP (RFC 6886, inlined codec with RFC-mandated kernel source-address filter), falls back to UPnP-IGD (`igd-next`), installs a mapping on success, and renews every 30 minutes. On install it calls `set_reflex_override(external)` which promotes the node to `Open` with the mapped address; on 3 consecutive renewal failures or shutdown it revokes and clears. Port mapping is a latency optimization on top of an already-working routed mesh — a router that doesn't speak either protocol leaves the node on the classifier path, which is fine. Full design in [`PORT_MAPPING_PLAN.md`](docs/PORT_MAPPING_PLAN.md).

**Stats are decision / action / outcome, not matrix guesses.** `MeshNode::traversal_stats()` returns three monotonic counters: `punches_attempted` (coordinator mediated a `PunchRequest` + `PunchIntroduce` round-trip — bumped only on successful wire activity), `punches_succeeded` (ack arrived AND direct handshake landed), `relay_fallbacks` (session landed on the routed-handshake path after either a `SkipPunch` decision, a failed punch, or a failed direct attempt — bumped only after the fallback handshake itself succeeds). The counters partition real activity; operators can use them to gauge traversal effectiveness without inflation from matrix-only decisions or double-failed calls.

**Feature-gated.** `nat-traversal` turns on the classifier, rendezvous, and `connect_direct`; `port-mapping` adds the router-control surface. Both are disabled by default so a build without the features produces a cdylib identical to the pre-traversal one — the Go / NAPI / PyO3 bindings keep their NAT-traversal symbols as fallback stubs that return `ErrTraversalUnsupported` (or the binding's equivalent), so callers can link unconditionally and discover the feature gate at runtime.

## Subnets

The mesh is logically flat but scales via hierarchical partitioning. A `SubnetId` packs 4 levels into 4 bytes:

```
SubnetId: [region: u8] [fleet: u8] [vehicle: u8] [subsystem: u8]

Example: 10.3.7.2
  region=10 (EU-West)
  fleet=3   (Factory Floor A)
  vehicle=7 (Robot Arm #7)
  subsystem=2 (Gripper Controller)
```

`SubnetGateway` nodes sit at subnet boundaries. They aggregate health from their subnet, compress capability summaries for external consumption, and enforce channel visibility — a channel marked `subnet-local` doesn't leak through the gateway. Gateways are protocol-equal nodes that happen to be reachable from both sides of a boundary.

`SubnetPolicy` assigns nodes to subnets automatically from capability labels. A node tagged `fleet:factory-a` and `role:robot-arm` gets assigned to the matching subnet without manual configuration.

Subnets bound observation cost. A node observes its peers at its level. For everything beyond, it observes the gateway and derives the rest. A node doesn't need heartbeats from 10,000 peers — it needs heartbeats from its neighbors and health summaries from gateways. Observation scales with the depth of the hierarchy, not the size of the mesh.

## Channels

Channels are how applications structure communication. `ChannelName` uses hierarchical hashing with path components:

```
sensors/lidar/front     → ChannelId(0xa3f1)
sensors/lidar/rear      → ChannelId(0xb7c2)
sensors                 → prefix match on hierarchical names
alerts/temperature      → ChannelId(0x1e09)
```

`ChannelConfig` defines per-channel policy:
- **Visibility**: public (mesh-wide), subnet-local (stays within subnet), private (explicit peer list)
- **Required capabilities**: only nodes with matching capabilities can subscribe
- **Retention**: how long events persist in adapters

Channels without a registered `ChannelConfig` at publish time fall back to `MeshNodeConfig::default_visibility` (default `Visibility::Global` — fail-open, preserves back-compat for registry-less deployments). Fleet operators who want fail-closed behavior — where forgetting to register a channel confines messages to the local subnet rather than leaking mesh-wide — set `MeshNodeConfig::new(..).with_default_visibility(Visibility::SubnetLocal)`. A channel with an explicit registry entry always uses its configured visibility; the knob only covers the unregistered-at-publish-time fallback.

`AuthGuard` enforces authorization at the channel boundary. It combines capability filters with permission tokens. A node needs both the right capabilities (hardware, tags) and a valid token (ed25519-signed, delegatable, expirable) to access a channel. Authorization results cache in a 4 KB bloom filter backed by a verified-subscribe hash — `check_fast` is the per-packet path every publish fan-out takes; microbenchmark at ~20 ns per call including the DashMap probe. Revocations take effect on the very next publish. A periodic sweep evicts subscribers whose tokens expire mid-subscription; a per-peer auth-failure rate limiter throttles bad-token storms so ed25519 verification never becomes a DoS vector. See [`MULTIHOP_CAPABILITY_PLAN.md`](docs/MULTIHOP_CAPABILITY_PLAN.md) and [`CHANNEL_AUTH_GUARD_PLAN.md`](docs/CHANNEL_AUTH_GUARD_PLAN.md).

Channels decouple applications from node identity. A producer emits to `sensors/temperature`. A consumer subscribes to `sensors/temperature`. Neither knows or cares which node the other is. The mesh connects them through the channel, the proximity graph finds the shortest path, and the auth guard ensures both sides are authorized.

## Security Surface

Identity, capability announcements, subnet visibility, and channel authentication work as a single unit behind the `net` feature. Every binding — Rust, TypeScript, Python, Go — surfaces the same pieces with the same wire contract:

- **Ed25519 identities.** `Identity` bundles a caller-owned 32-byte seed with a local `TokenCache`. `node_id` and `entity_id` are reproducible across restarts when the seed is pinned on `MeshBuilder` (or `identity_seed` / `identitySeed` / `IdentitySeedHex` on the Python / TS / Go mesh constructors and configs).
- **Permission tokens.** ed25519-signed grants tying a `(subject, scope, channel, TTL)` tuple together. `TokenScope` is a bitfield of `publish | subscribe | admin | delegate`; delegation is capped per-token and the chain is verified end-to-end. Tokens cross the boundary as 159-byte opaque buffers (no hex round-trip, no JSON tax).
- **Capability announcements.** Multi-hop broadcast (up to `MAX_CAPABILITY_HOPS = 16`) of each node's `CapabilitySet` (hardware, software, models, tools, tags, limits). `find_nodes(filter)` queries the local index in constant time; self-match returns the owning node's id. Forwarders increment `hop_count` outside the signed envelope so the origin's ed25519 signature verifies at every hop; `(origin, version)` dedup drops duplicates at diamond-topology converge points. The `node_id → entity_id` binding is pinned TOFU-style on first sight. See [`MULTIHOP_CAPABILITY_PLAN.md`](docs/MULTIHOP_CAPABILITY_PLAN.md).
- **Subnets.** A `SubnetId` is a 4-level u32; `SubnetPolicy` derives each peer's subnet from their capability tags so every node in the mesh agrees on the geometry without a central directory. `Visibility` on a channel gates publish fan-out and subscribe authorization against that geometry.
- **Channel authentication.** `ChannelConfig` carries `publish_caps`, `subscribe_caps`, and `require_token`. Publishers check their own caps before fan-out; subscribers present a `PermissionToken` whose subject matches their entity id. Successful subscribes populate the `AuthGuard` fast path (4 KB bloom filter + verified-subscribe cache) so every subsequent publish packet admits or drops the subscriber in constant time. A periodic token-expiry sweep (default 30 s) evicts subscribers whose tokens age out; a per-peer auth-failure rate limiter (default 16 failures per 60 s window, 30 s throttle) short-circuits bad-token storms before ed25519 verification runs. Any denial surfaces as `Unauthorized` / `RateLimited` at the subscribe gate or as a `PublishReport` miss on the publish side.

Full staging, wire formats, and rationale: [`docs/SDK_SECURITY_SURFACE_PLAN.md`](docs/SDK_SECURITY_SURFACE_PLAN.md). Per-binding parity details: [`docs/SDK_PYTHON_PARITY_PLAN.md`](docs/SDK_PYTHON_PARITY_PLAN.md), [`docs/SDK_GO_PARITY_PLAN.md`](docs/SDK_GO_PARITY_PLAN.md). Runnable examples in idiomatic form: [Rust](sdk/README.md#security-identity-tokens-capabilities-subnets) · [TypeScript](sdk-ts/README.md#security-identity-tokens-capabilities-subnets) · [Python](bindings/python/README.md#security-surface-stage-ae) · [Go](../../../go/README.md#security-surface-stage-ae).

## Daemons

A `MeshDaemon` is a stateful event processor — the compute unit of the mesh.

```rust
trait MeshDaemon: Send + Sync {
    fn process(&mut self, event: &CausalEvent) -> DaemonOutput;
    fn snapshot(&self) -> StateSnapshot;
    fn restore(&mut self, snapshot: StateSnapshot);
}
```

`DaemonHost` manages the runtime lifecycle: initialization, event dispatch, causal chain production, horizon tracking, and snapshot packaging. Every event a daemon produces is automatically linked into a causal chain with a 24-byte `CausalLink` (origin, sequence, parent hash, compressed horizon).

`DaemonRegistry` maps daemon types to constructors. The `PlacementScheduler` decides where to run each daemon based on capability requirements — a daemon that needs a GPU is placed on a GPU node. If the best candidate is already loaded, the scheduler considers the next-best via the proximity graph.

When a node fails or needs load balancing, migration preserves continuity in 6 phases:

1. **Snapshot** — source captures daemon state, chain head, and horizon
2. **Transfer** — snapshot sent to target (auto-chunked for large state)
3. **Restore** — target reassembles chunks and rebuilds the daemon from the snapshot using a factory + keypair + config resolved from its local `DaemonFactoryRegistry`
4. **Replay** — buffered events (arrived during transfer) replayed in causal order
5. **Cutover** — source stops writes and cleans up; source daemon unregistered
6. **Complete** — orchestrator emits `ActivateTarget`; target drains remaining events, activates, replies with `ActivateAck`; migration record removed

The full chain runs autonomously over `SUBPROTOCOL_MIGRATION` (0x0500); no manual orchestration is required once `start_migration()` is called. The `MigrationOrchestrator` coordinates across three nodes (source, target, controller). `MigrationSourceHandler` manages the source side (snapshot, buffer, quiesce, cleanup). `MigrationTargetHandler`, constructed via `new_with_factories(registry, factories)`, manages the target side (reassemble, restore, ordered replay via `BTreeMap`, activate). Auto-target selection queries the `CapabilityIndex` for nodes advertising `subprotocol:0x0500`.

The daemon's causal chain continues unbroken on the new host. During migration, a `SuperpositionState` tracks which phase the daemon is in — it exists on both nodes briefly, then collapses to the new host.

Every binding — Rust, TypeScript, Python, Go — surfaces `DaemonRuntime`, the `MeshDaemon` trait / interface, and the `start_migration` orchestrator with the same lifecycle gate and the same stable error vocabulary (`daemon: migration: <kind>[: detail]`, where `<kind>` matches the `MigrationFailureReason` enum). Staging, dispatcher design, and per-binding parity notes: [`docs/SDK_COMPUTE_SURFACE_PLAN.md`](docs/SDK_COMPUTE_SURFACE_PLAN.md) and [`docs/DAEMON_RUNTIME_READINESS_PLAN.md`](docs/DAEMON_RUNTIME_READINESS_PLAN.md). Runnable examples in idiomatic form: [Rust](sdk/README.md#compute-daemons--migration) · [TypeScript](sdk-ts/README.md#compute-daemons--migration) · [Python](bindings/python/README.md#compute-daemons--migration) · [Go](../../../go/README.md#compute-daemons--migration).

For daemons that need horizontal scale rather than mobility, `ReplicaGroup` manages N copies as a logical unit. Each replica gets a deterministic identity derived from `group_seed + index` — the same index always produces the same keypair, making replacement idempotent. The group load-balances inbound events across replicas (round-robin, least-connections, consistent hash — any `LoadBalancer` strategy), tracks group-level health (alive as long as one replica is healthy), and spreads placement across nodes for failure-domain isolation. When a node fails, the group re-spawns the affected replicas on new nodes with the same deterministic identity — no migration protocol needed for stateless daemons. Scaling is `scale_to(n)`: scale up appends new replicas, scale down removes the highest-index ones deterministically.

For daemons that need to diverge rather than replicate, `ForkGroup` creates N independent entities forked from a common parent. Each fork has a `ForkRecord` with a cryptographically verifiable sentinel hash linking back to the parent's causal chain at the fork point. Unlike replicas (interchangeable, deterministic per-index identities), forks are independent entities with documented lineage — any node can verify the fork by recomputing the sentinel. Fork keypairs are stored for recovery on failure, preserving identity across re-spawns.

For stateful daemons that need fault tolerance without duplicate compute, `StandbyGroup` implements active-passive replication. One member processes all events. The others hold readiness to promote — they consume memory for their snapshot but do zero event processing. Periodic `sync_standbys()` captures the active's state. On failure, the standby with the most recent snapshot promotes and replays the buffered events since that snapshot — the same replay mechanism MIKOSHI uses for migration. Persistence of snapshot bytes to disk is an application concern; the protocol provides the bytes and the bookkeeping.

All three group types share coordination logic via `GroupCoordinator` — health tracking, member management, and placement work identically. Any member of any group is a normal daemon in the `DaemonRegistry`, so MIKOSHI can migrate it without knowing it belongs to a group.

Every binding — Rust, TypeScript, Python, Go — surfaces all three groups with the same coordination semantics and the same stable error vocabulary (`daemon: group: <kind>[: detail]`, where `<kind>` is one of `not-ready | factory-not-found | no-healthy-member | placement-failed | registry-failed | invalid-config | daemon`). Staging, wire formats, and design notes: [`docs/SDK_GROUPS_SURFACE_PLAN.md`](docs/SDK_GROUPS_SURFACE_PLAN.md). Runnable examples in idiomatic form: [Rust](sdk/README.md#groups-replica--fork--standby) · [TypeScript](sdk-ts/README.md#groups-replica--fork--standby) · [Python](bindings/python/README.md#compute-groups-replica--fork--standby) · [Go](../../../go/README.md#compute-groups-replica--fork--standby).

## Safety & Autonomy

Every node enforces its own rules. The `SafetyEnforcer` evaluates a `ResourceEnvelope` that defines:

- **Rate limits**: max events/sec ingested, max events/sec forwarded
- **Memory limits**: max ring buffer usage, max snapshot size
- **Compute limits**: max concurrent daemons, max CPU time per event
- **Kill switch**: conditions under which the node drops all traffic and goes silent

The `RuleEngine` evaluates declarative `RuleSet` policies:

```
Rule: if load > 90% then reject(priority < 5)
Rule: if peer.subnet != self.subnet then require(token.scope = "cross-subnet")
Rule: if event.size > 64KB then drop
```

Rules are local decisions, not network policy. No external authority can override a node's safety envelope. A node that refuses work is routed around — the proximity graph reflects this within a heartbeat interval. The mesh adapts to the node's boundaries rather than forcing the node to adapt to the mesh.

This is device autonomy in practice. A $5 sensor node sets tight limits — low rate, small buffer, no daemons. A $1500 GPU node sets generous limits — high rate, large buffer, many daemons. Both are equal participants on the mesh. The protocol treats them identically. Their capabilities and autonomy rules determine what they actually do.

## RedEX

RedEX is the local append-only log primitive. A `Redex` manager owns a `ChannelName → RedexFile` map; every file is an independent monotonic sequence of 20-byte index records plus a payload segment. v1 is strictly local — no replication, no subprotocol, no multi-node convergence. Higher layers (CortEX, NetDB) build on top; nothing in RedEX knows about them.

The 20-byte record is fixed:

```
┌──────────────┬────────────────┬────────────────┬────────────────────┐
│ seq (u64 LE) │ offset (u32 LE)│  len (u32 LE)  │ flags+checksum u32 │
│   8 bytes    │   4 bytes      │    4 bytes     │     4 bytes        │
└──────────────┴────────────────┴────────────────┴────────────────────┘
```

Two payload paths:

- **Inline** (`append_inline`): exactly 8 bytes of payload live in the index record itself (reusing the `offset`+`len` fields, discriminated by the `INLINE` flag in the high nibble of `flags+checksum`). Zero segment allocation — the fast path for tick counters, sensor readings, small enums.
- **Heap** (`append`, `append_batch`, `append_postcard`): payload appended to an in-memory `HeapSegment` (grow-only `Vec<u8>`, 3 GB hard cap). The index record carries the `(offset, len)` into the segment.

A monotonic `AtomicU64::fetch_add` assigns the sequence lock-free in the non-ordered path. `OrderedAppender` / `append_ordered` hold the state lock across seq allocation for writers that need strict replay determinism under contention. `append_batch` and `append_batch_ordered` allocate a contiguous seq range atomically; pre-validation under the state lock guarantees a failing batch does NOT advance `next_seq`, so no seq-space gaps appear on `PayloadTooLarge` / `SegmentOffsetOverflow`. Both batch-append paths return `Result<Option<u64>, RedexError>`: `Ok(Some(first_seq))` for a non-empty batch, `Ok(None)` for empty input. The `Option` distinguishes "I appended nothing" from "first event landed at seq 0" — collapsing both into a bare `u64` (the pre-`bugfixes-8` shape) made the empty-input case indistinguishable from a legitimate seq-0 first-write.

`tail(from_seq)` returns a `Stream<Item = Result<RedexEvent, RedexError>>` with an atomic backfill-then-live handoff: under the state lock, it drains every retained entry with `seq >= from_seq` and then registers a live watcher — nothing can interleave between the last backfill event and the first live event. Closed files emit a single `RedexError::Closed` and end.

Retention runs as an on-demand `sweep_retention()` call (no background task in v1). Three policies AND together; the sweep takes the largest eviction count satisfying all active constraints:

- **Count** (`retention_max_events`) — keep newest K entries
- **Size** (`retention_max_bytes`) — keep newest M bytes of payload
- **Age** (`retention_max_age_ns`) — drop entries older than D nanoseconds (wall-clock at append time; persistent files preserve age across reopen via a `ts` sidecar — see Durability below)

`RedexFold<State>` is the integration hook that higher layers consume. RedEX defines the trait and drives it on a tail stream spawned by the caller; CortEX installs its `TasksFold` / `MemoriesFold` against it.

Durability is opt-in behind the `redex-disk` feature and `RedexFileConfig::persistent`. Each persistent file writes three append-only files at `<base>/<channel_path>/{idx,dat,ts}`: `idx` carries the 20-byte records, `dat` carries heap payloads, and the `ts` sidecar carries 8-byte unix-nanos per entry so age-based retention survives restart. On reopen, the full `dat` is replayed into a fresh `HeapSegment`, `idx` restores the index, `ts` rehydrates per-entry timestamps, and a torn trailing record from a crash (partial 20-byte write or `dat`-shorter-than-`idx` from a crash mid-batch) is truncated on recovery. Per-entry checksums are verified during recovery and entries with mismatched checksums (mid-file bit-rot) are dropped without aborting reopen. `close()` and explicit `sync()` fsync `dat` → `idx` → `ts` in that order — the crash-consistency ordering is "payload before index, index before timestamps," so the worst case after a power cut is an index shorter than the payload (truncated by torn-tail logic) or a `ts` shorter than the index (recovered entries past the gap fall back to `now()`).

Append-path fsyncs are governed by `FsyncPolicy`:

- **`Never`** (default) — page cache only; `close()` is the durability barrier.
- **`EveryN(N)`** — fsync after every N appends. The fsync runs on a background tokio worker — the appender returns as soon as bytes land in the page cache and signals the worker via `tokio::sync::Notify`. The worker holds its own file handles, cloned from the appender's via `File::try_clone` (same underlying OS file, separate mutex), so its `sync_all` doesn't lock against the appender's `write_all` — without that decoupling, high-cadence policies serialize every append behind the millisecond-range fsync.  Concurrent notifies during an in-flight fsync coalesce into a single follow-up.
- **`Interval(d)`** — fsync on a per-file timer.
- **`IntervalOrBytes { period, max_bytes }`** — fsync when **either** `period` elapses **or** `max_bytes` of accumulated writes (across `dat` + `idx` + `ts`) crosses the threshold, whichever comes first. Same background-worker shape as `EveryN`; the byte arm is signal-driven (no polling). `period: 0, max_bytes: N` gives byte-only triggering (no timer arm); `period: 0, max_bytes: 0` is equivalent to `Never`.

Batched appends are syscall-coalesced: `append_batch` issues at most three `write_all` calls per batch (one each to `dat`, `idx`, `ts`) instead of three per entry, and the heap segment commits the whole batch with a single `append_many` call. See [`docs/REDEX_DISK_THROUGHPUT_PLAN.md`](docs/REDEX_DISK_THROUGHPUT_PLAN.md) for the full design and shipped invariants.

ACL enforcement happens at `open_file` via the optional `AuthGuard`. The check keys on the canonical `ChannelName` (not the 16-bit wire hash), so two distinct channels can never alias into the same ACL decision — see the Channels section for the two-tier authorization design.

## CortEX

CortEX is the seam between Net events and local state. A `CortexAdapter<State>` wraps a `RedexFile` with:

1. A fixed 20-byte `EventMeta` prefix on every payload (dispatch tag, flags, origin hash, per-origin seq-or-timestamp, xxh3 checksum of the tail).
2. A spawned fold task that tails the file from a chosen start position, decodes the meta, and drives a caller-supplied `RedexFold<State>` against an `Arc<RwLock<State>>`.
3. A read-after-write barrier (`wait_for_seq`) so callers can block until a freshly-appended event has been folded into state.
4. A `changes() -> Stream<Item = u64>` broadcast notification so reactive queries can re-evaluate after every fold tick.

```rust
pub struct EventMeta {
    pub dispatch: u8,       // 0x00..0x7F CortEX-internal; 0x80..0xFF app/vendor
    pub flags: u8,          // FLAG_CAUSAL, FLAG_CONTINUITY_PROOF, ...
    pub _pad: [u8; 2],      // reserved, zero on write, ignored on read
    pub origin_hash: u32,   // producer identity
    pub seq_or_ts: u64,     // per-origin counter OR unix nanos; file picks one
    pub checksum: u32,      // xxh3_64(tail) truncated
}
```

`StartPosition` selects replay semantics: `FromBeginning` (full history), `LiveOnly` (skip pre-open), `FromSeq(n)` (resume after a snapshot). `FoldErrorPolicy` governs what happens when the fold returns `Err`: `Stop` halts the task and records the stopping seq; `LogAndContinue` increments an error counter and keeps going. A single `changes()` broadcast is shared across all reactive subscribers; a subscriber falling more than 64 events behind drops intermediate ticks but always sees the latest state on catch-up. `changes()` carries *successfully-folded* sequences only — on a `Stop`+non-recoverable halt the watermark is not advanced and the failing seq is NOT broadcast. Subscribers that need to react to a halt poll `is_running()` separately (or the broadcast channel ends naturally when the adapter is dropped).

Snapshots compact long-running folds: `snapshot()` serializes the materialized state (under the state write lock so `(bytes, last_seq)` is consistent) via postcard; `open_from_snapshot(bytes, last_seq)` deserializes and resumes tailing at `FromSeq(last_seq + 1)`. `last_seq = u64::MAX` returns `RedexError::Encode` rather than wrapping around silently.

Two concrete models ship in v1:

- **Tasks** — mutate-by-id CRUD. Dispatches `0x01..=0x04` (created / renamed / completed / deleted). `Task { id, title, status, created_ns, updated_ns }`. `TasksState` holds a `HashMap<TaskId, Task>` and exposes a Prisma-style query builder (`state.query().where_status(...).order_by(...).limit(N).collect()`) plus Prisma-ish shortcuts (`find_unique`, `find_many`, `count_where`, `exists_where`).
- **Memories** — content-addressed log with set-valued tags. Dispatches `0x10..=0x14` (stored / retagged / pinned / unpinned / deleted). `Memory { id, content, tags: Vec<String>, source, pinned, ... }`. Same query surface with tag predicates in three flavors (`where_tag`, `where_any_tag`, `where_all_tags`) plus a pin filter.

Both models expose a reactive `watch(filter)` that returns a `Stream<Item = Vec<T>>`: the current filter result on open, then a freshly-evaluated vector on every fold tick where the filter output changes (deduplicated by Vec equality; defaults to `OrderBy::IdAsc` when the caller doesn't specify one, so dedup is deterministic). The stream's backing channel is single-slot (`tokio::sync::watch`), so a slow consumer sees the latest state on the next poll — intermediate results are dropped. The spawned watcher task bails out immediately when the consumer drops the stream via `tokio::select!` on `tx.closed()`.

Tasks and memories coexist on the same `Redex` manager without cross-channel leakage: each model owns a distinct `ChannelName` (`cortex/tasks`, `cortex/memories`) and partitions its dispatches under the CortEX-internal range `0x00..=0x7F` (with static asserts). Application / vendor dispatches get `0x80..=0xFF`.

Typed errors cross the FFI boundary as classes on both Node and Python bindings: `CortexError` for adapter-level failures (`adapter closed`, `fold stopped at seq N`, underlying RedEX errors) and `NetDbError` for handle-level failures (snapshot encode / decode, missing-model accesses). The Node side uses stable `cortex:` / `netdb:` message prefixes classified into typed `Error` subclasses by `@ai2070/net/errors::classifyError`; the Python side exposes `net._net.CortexError` / `net._net.NetDbError` directly via `pyo3::create_exception!`.

## NetDB

NetDB is the unified query façade over one or more CortEX models. A `NetDb` handle bundles enabled adapters behind per-model accessors (`db.tasks()` / `db.memories()`); each Prisma-ish method (`find_unique`, `find_many(&filter)`, `count_where`, `exists_where`) is available both on the per-model state guard and transparently through the handle. NetDB is strictly local and strictly query-oriented — raw events and streams stay at the RedEX / CortEX layer.

```rust
let db = NetDb::builder(Redex::new())
    .origin(origin_hash)
    .with_tasks()
    .with_memories()
    .build()?;

db.tasks().create(1, "write plan", now_ns())?;
let pending = db.tasks().state().read().find_many(&TasksFilter {
    status: Some(TaskStatus::Pending),
    limit: Some(10),
    ..Default::default()
});
```

`NetDbBuilder::build` is failure-atomic: if the second adapter open fails after the first succeeded, the first is closed before the error propagates so no orphan fold task outlives the failed build.

Whole-db snapshot is a single call. `db.snapshot()` walks every enabled model under its own state lock (consistent per-model; there is no cross-model consistency guarantee because each model backs a separate RedEX file), returning a `NetDbSnapshot { tasks, memories }` bundle. `NetDbSnapshot::encode()` produces a single postcard blob for persistence; `NetDbSnapshot::decode(bytes)` round-trips it, and `NetDbBuilder::build_from_snapshot(&bundle)` restores every enabled model in one call. Models enabled via `with_*()` whose bundle entry is `None` are opened from scratch — the same fallback path used by a fresh `build`.

NetDB ships the same surface on Rust, Node (`@ai2070/net` napi bindings), and Python (`net._net` PyO3 bindings). The Node and Python handles carry the same CRUD + query methods; `NetDb.open(config)` on both sides is failure-atomic and supports the same whole-db snapshot bundle cross-language (postcard is stable across the FFI boundary).

## nRPC

nRPC is the request/response convention layer riding on top of the pub/sub mesh + CortEX folds. It turns a directed channel pair (`<service>.requests` / `<service>.replies.<caller_origin>`) into a typed RPC surface with deadlines, queue-group fan-out, response streaming, and end-to-end cancellation.

**Wire shape.** Every RPC is two events on the bus:

- A **REQUEST** on `<service>.requests` carrying `RpcRequestPayload { service, deadline_ns, flags, headers, body }` plus a per-caller `call_id` in the `EventMeta`.
- A **RESPONSE** on `<service>.replies.<caller_origin>` carrying `RpcResponsePayload { status, headers, body }` correlated via the same `call_id`. Streaming RPCs emit multiple chunks plus a terminal end-or-error frame; flow-controlled streams add a GRANT subprotocol.

The reply-channel-per-caller convention keeps subscriptions cheap: a server holds one subscription per service name; a caller holds one subscription per `(service, target)` pair, lazily subscribed on first call and reused. CANCEL fires when the caller drops the future or `RpcStream` mid-stream.

**Status codes.** `RpcStatus` is a `u16`. The protocol-defined band is `0x0000..=0x7FFF` (`Ok`, `Internal`, `Backpressure`, `Timeout`, `NotFound`, `BadRequest`, …); the application-defined band is `0x8000..=0xFFFF`. Two stable application-status constants ship with the SDK:

| Status hex | Constant                       | Trigger                                          |
| ---------- | ------------------------------ | ------------------------------------------------ |
| `0x0000`   | `RpcStatus::Ok`                | Normal response.                                 |
| `0x8000`   | `NRPC_TYPED_BAD_REQUEST`       | Typed handler couldn't decode the request body.  |
| `0x8001`   | `NRPC_TYPED_HANDLER_ERROR`     | Typed handler ran but returned an exception.     |

**Error model (every binding).** Caller-side failures surface with a stable `nrpc:` prefix so cross-language code can pattern-match:

| Kind segment    | Source                                    |
| --------------- | ----------------------------------------- |
| `no_route`      | No session to target / capability gone    |
| `timeout`       | Deadline elapsed before reply             |
| `server_error`  | Handler returned a non-OK status          |
| `transport`     | Wire-level send / receive failure         |
| `codec_encode`  | Caller-side encode failure                |
| `codec_decode`  | Caller-side decode failure                |

Each binding exposes typed error subclasses (`RpcNoRouteError`, `RpcTimeoutError`, `RpcServerError`, `RpcTransportError`, `RpcCodecError`, plus a `BreakerOpenError` from the resilience helpers). The Node + Python wrappers add `classifyError(e)` / `classify_error(e)` to map a raw `nrpc:`-prefixed exception into the typed class.

**Resilience helpers.** Every typed surface ships `call_with_retry` (exponential backoff + jitter, retriable predicate defaulting to `no_route` + `transport`), `call_with_hedge` (parallel races on a delay; first success wins, losers cancelled), and `CircuitBreaker` (closed → open → half-open with configurable failure predicate). The Node binding throws `BreakerOpenError`; the Python binding raises `BreakerOpenError`; the Go binding returns `ErrBreakerOpen`. All three carry the `nrpc:breaker_open:` prefix in the error string.

### Cross-binding contract

The canonical interop contract — used by every binding's wire-format compat test — is the `cross_lang_echo_sum` service:

```jsonc
// Request
{ "text": "string to echo", "numbers": [1, 2, 3] }
// Response
{ "echo": "string from text field", "sum": 6 }
```

**Behavior:** echo `text` as-is, sum `numbers` left-to-right. Empty `numbers` ⇒ `sum = 0`. Missing or wrong-type `text` / `numbers` ⇒ `RpcStatus::Application(0x8000)` surfaced as `nrpc:server_error: status=0x8000 message=…`.

The shared fixture at [`tests/cross_lang_nrpc/golden_vectors.json`](tests/cross_lang_nrpc/golden_vectors.json) is the single source of truth. Every binding loads it and runs the same matrix — 6 ok cases (single number, small array, empty array, negatives, unicode echo, empty text) + 3 error cases (missing text, missing numbers, wrong-type numbers):

| Binding | Test file                                                    | Pattern                                                  |
| ------- | ------------------------------------------------------------ | -------------------------------------------------------- |
| Rust    | `tests/integration_nrpc_cross_lang.rs`                       | In-process loopback handler against the spec.            |
| Node    | `bindings/node/test/cross_lang_compat.test.ts`               | Loads the fixture, runs against `TypedMeshRpc` stubs.    |
| Python  | `bindings/python/tests/test_cross_lang_compat.py`            | Loads the fixture, runs against `TypedMeshRpc` stubs.    |
| Go      | (downstream — reference consumer at `bindings/go/net/`)      | Same shape; downstream fixture-driven test once Go ships. |

These are wire-format compat tests, not subprocess-based interop tests. Cargo can't easily orchestrate Node + Python subprocesses portably (PATH discovery, pre-built native modules); the fixture-driven approach catches the same drift bugs at lower cost. The fixture is versioned via `abi_version_expected` mirroring `NET_RPC_ABI_VERSION` from `bindings/go/rpc-ffi/src/lib.rs` — bumping the ABI invalidates the fixture and forces every binding's compat test to update.

True subprocess-based interop tests (Node caller → Rust server, Python caller → Rust server, Node ↔ Python, etc.) remain out of scope. When Cargo can portably orchestrate Node / Python subprocesses AND both bindings ship pre-built native modules in CI, add a `tests/cross_lang_nrpc.rs` driver that gates on `CROSS_LANG_NRPC=1` + `NET_NODE_BUILT=1` / `NET_PYTHON_BUILT=1` and spawns binding-side caller scripts via `Command::new`.

### Per-binding usage

See each SDK README for the typed surface, resilience helpers, and streaming semantics specific to that language:

- **Rust** — [`sdk/README.md`](sdk/README.md): `Mesh::serve_rpc_typed`, `Mesh::call_typed`, `Mesh::call_streaming_typed`, plus the `mesh_rpc::retry` / `hedge` / `CircuitBreaker` modules.
- **TypeScript** — [`sdk-ts/README.md`](sdk-ts/README.md): `TypedMeshRpc.from(mesh)` with `.serve` / `.call` / `.callService` / `.callStreaming`, plus `RetryPolicy` / `HedgePolicy` / `CircuitBreaker`.
- **Python** — [`sdk-py/README.md`](sdk-py/README.md): `TypedMeshRpc.from_mesh(mesh)` with the same surface; `serve` registers an async-or-sync handler dispatched under `tokio::task::spawn_blocking` so the GIL doesn't starve the runtime.
- **Python (low-level binding)** — [`bindings/python/README.md`](bindings/python/README.md): the raw `net.MeshRpc` pyclass that the typed wrapper sits on top of.
- **Go** — [`bindings/go/net/`](bindings/go/net/): reference cgo wrapper around the C ABI (`libnet_rpc`) at `bindings/go/rpc-ffi/`. Documents `MeshRpc.Call` / `CallService` / `Serve` / `CallStreaming` with ctx-cancel watcher; the Go module ships downstream.

## Module Map

Top-level `src/` is the event-bus core; the heavy mesh code lives under `adapter/net/`.

```
src/
├── lib.rs                 # Crate root, re-exports
├── config.rs              # EventBusConfig, AdapterConfig, ScalingPolicy
├── error.rs               # Crate-wide error types
├── event.rs               # Event, Batch, StoredEvent
├── timestamp.rs           # TimestampGenerator (per-shard monotonic)
├── bus/                   # EventBus orchestrator over shards + adapters
├── shard/                 # SPSC ring buffers, batch assembly, ShardManager
├── consumer/              # Cross-shard poll merging, JSON-predicate filtering
├── ffi/                   # C ABI for Python / Node / Go / C consumers
└── adapter/               # Pluggable durability backends (see below)
    ├── mod.rs             #   Adapter trait, dispatch
    ├── noop.rs            #   NoopAdapter (testing / benchmarking)
    ├── dedup_state.rs     #   PersistentProducerNonce — cross-restart producer identity
    ├── redis.rs           #   RedisAdapter (feature `redis`)
    ├── redis_dedup.rs     #   RedisStreamDedup (feature `redis`)
    ├── jetstream.rs       #   JetStreamAdapter (feature `jetstream`)
    └── net/               #   NetAdapter — UDP mesh transport (feature `net`)
```

```
src/adapter/net/
├── mod.rs                 # NetAdapter, routing utilities
├── mesh.rs                # MeshNode — multi-peer mesh runtime (single socket, forwarding, subprotocol dispatch)
├── config.rs              # NetAdapterConfig
├── crypto.rs              # Noise NKpsk0 handshake, ChaCha20-Poly1305 AEAD
├── protocol.rs            # 64-byte wire header, EventFrame, NackPayload
├── transport.rs           # UDP socket abstraction, batched I/O
├── session.rs             # Session state, stream multiplexing, thread-local pools
├── stream.rs              # Application-facing typed Stream handle over NetSession
├── mesh_rpc.rs            # nRPC client surface — call / call_service / call_streaming + RpcStream
├── mesh_rpc_metrics.rs    # nRPC per-service counters, prometheus_text() formatter
├── router.rs              # FairScheduler, stream routing, priority bypass
├── route.rs               # RoutingTable, multi-hop headers, stream stats
├── reroute.rs             # Automatic rerouting policy — failure-detector-driven route updates
├── proxy.rs               # Zero-copy multi-hop forwarding, TTL enforcement
├── pool.rs                # Zero-alloc PacketPool, PacketBuilder, ThreadLocalPool
├── batch.rs               # AdaptiveBatcher, latency-aware sizing
├── reliability.rs         # FireAndForget / ReliableStream, selective NACKs
├── failure.rs             # FailureDetector, RecoveryManager, CircuitBreaker
├── swarm.rs               # Pingwave discovery, CapabilityAd, LocalGraph
├── linux.rs               # recvmmsg batch reads (Linux-only)
│
├── identity/              # Layer 1 — Trust & Identity
│   ├── entity.rs          #   EntityId, EntityKeypair (ed25519)
│   ├── envelope.rs        #   Encrypted daemon-keypair transport for migration
│   ├── origin.rs          #   OriginStamp binding
│   └── token.rs           #   PermissionToken, TokenScope, TokenCache
│
├── channel/               # Layer 2 — Channels & Authorization
│   ├── config.rs          #   ChannelConfig, Visibility, ChannelConfigRegistry
│   ├── guard.rs           #   AuthGuard, AuthVerdict, bloom filter
│   ├── name.rs            #   ChannelId, ChannelName (hierarchical hashing)
│   ├── membership.rs      #   Subscribe / Unsubscribe / Ack subprotocol
│   ├── roster.rs          #   Per-channel subscriber roster for daemon-layer fan-out
│   └── publisher.rs       #   Thin per-peer fan-out helper for channel publishes
│
├── behavior/              # Behavior Plane — Semantic Layer
│   ├── capability.rs      #   HardwareCapabilities, CapabilityIndex, GpuInfo
│   ├── broadcast.rs       #   Capability-broadcast subprotocol (CapabilityAnnouncement fan-out)
│   ├── diff.rs            #   CapabilityDiff, DiffEngine
│   ├── metadata.rs        #   NodeMetadata, MetadataStore, TopologyHints, NatType
│   ├── api.rs             #   ApiRegistry, ApiSchema, version validation
│   ├── rules.rs           #   RuleEngine, RuleSet, device autonomy policies
│   ├── context.rs         #   Context, ContextStore, Span, distributed tracing
│   ├── loadbalance.rs     #   LoadBalancer, Strategy, health-aware selection
│   ├── proximity.rs       #   ProximityGraph, EnhancedPingwave, latency edges
│   └── safety.rs          #   SafetyEnforcer, ResourceEnvelope, rate limits, kill switch
│
├── subnet/                # Layer 3 — Subnets & Hierarchy
│   ├── id.rs              #   SubnetId (4 x 8-bit levels)
│   ├── assignment.rs      #   SubnetPolicy, label-based rules
│   └── gateway.rs         #   SubnetGateway, visibility enforcement
│
├── state/                 # Layer 4 — Distributed State
│   ├── causal.rs          #   CausalChainBuilder, CausalEvent, CausalLink (24B)
│   ├── horizon.rs         #   HorizonEncoder, ObservedHorizon (compressed)
│   ├── log.rs             #   EntityLog, append-only chain validation
│   └── snapshot.rs        #   StateSnapshot, SnapshotStore
│
├── compute/               # Layer 5 — Compute Runtime
│   ├── daemon.rs          #   MeshDaemon trait
│   ├── daemon_factory.rs  #   DaemonFactoryRegistry (origin_hash → factory + keypair + config) for target-side restore
│   ├── bindings.rs        #   Daemon subscription ledger — replay channel bindings on migration target
│   ├── host.rs            #   DaemonHost runtime, from_snapshot(), from_fork()
│   ├── migration.rs       #   MigrationState, MigrationPhase, 6-phase state machine
│   ├── orchestrator.rs    #   MigrationOrchestrator, wire protocol, snapshot chunking, ActivateTarget/ActivateAck
│   ├── migration_source.rs #  Source-side: snapshot, buffer, cutover, cleanup
│   ├── migration_target.rs #  Target-side: restore, replay, activate
│   ├── group_coord.rs     #   GroupCoordinator, shared LB/health/routing
│   ├── replica_group.rs   #   ReplicaGroup, N-way replication, deterministic identity
│   ├── fork_group.rs      #   ForkGroup, N-way forking, verifiable lineage
│   ├── standby_group.rs   #   StandbyGroup, active-passive stateful replication
│   ├── registry.rs        #   DaemonRegistry
│   └── scheduler.rs       #   Capability-based placement, migration target discovery
│
├── subprotocol/           # Layer 6 — Subprotocol Registry
│   ├── descriptor.rs      #   SubprotocolDescriptor, versioning
│   ├── migration_handler.rs #  Migration message dispatch (0x0500)
│   ├── negotiation.rs     #   Version negotiation, SubprotocolManifest
│   ├── registry.rs        #   SubprotocolRegistry, capability enrichment
│   └── stream_window.rs   #   Receiver → sender credit grants for stream flow control
│
├── continuity/            # Layer 7 — Observational Continuity
│   ├── chain.rs           #   ContinuityProof (36B), ContinuityStatus
│   ├── cone.rs            #   CausalCone, Causality analysis
│   ├── discontinuity.rs   #   ForkRecord, DiscontinuityReason, fork_entity()
│   ├── observation.rs     #   ObservationWindow, HorizonDivergence
│   ├── propagation.rs     #   PropagationModel, subnet-distance latency
│   └── superposition.rs   #   SuperpositionState, migration phase tracking
│
├── contested/             # Layer 8 (Partial) — Contested Environments
│   ├── correlation.rs     #   CorrelatedFailureDetector, subnet correlation
│   ├── partition.rs       #   PartitionDetector, PartitionPhase, healing
│   └── reconcile.rs       #   Log reconciliation, longest-chain-wins, ForkRecord
│
├── traversal/             # NAT Traversal — reflex discovery, classification, hole-punch, port mapping
│   ├── mod.rs             #   Module entry — framing & wire surface
│   ├── config.rs          #   Tunables (probe counts, timeouts, refresh windows)
│   ├── classify.rs        #   Wire NAT taxonomy (Open / Cone / Symmetric / Unknown)
│   ├── reflex.rs          #   Reflex-probe subprotocol — mesh-native STUN analog
│   ├── rendezvous.rs      #   Hole-punch rendezvous — three-message simultaneous-open dance
│   └── portmap/           #   Port mapping (UPnP-IGD + NAT-PMP / PCP)
│       ├── mod.rs         #     PortMapperClient trait + install/renew/revoke task
│       ├── gateway.rs     #     Default-gateway + LAN-IP discovery
│       ├── natpmp.rs      #     NAT-PMP / PCP wire codec + UDP client (RFC 6886 / 6887)
│       ├── upnp.rs        #     UPnP-IGD client backed by `igd-next`
│       └── sequential.rs  #     Composing mapper: NAT-PMP first, UPnP fallback
│
├── redex/                 # RedEX — local append-only event log (feature `redex`)
│   ├── mod.rs             #   Re-exports: Redex, RedexFile, RedexEvent, RedexError, ...
│   ├── entry.rs           #   20-byte RedexEntry codec, RedexFlags, payload_checksum
│   ├── config.rs          #   RedexFileConfig (persistent, retention, sync_interval)
│   ├── event.rs           #   RedexEvent { entry, payload }
│   ├── error.rs           #   RedexError (thiserror-derived)
│   ├── segment.rs         #   HeapSegment (append-only Vec<u8>, evict_prefix_to)
│   ├── retention.rs       #   compute_eviction_count (count + size policy)
│   ├── fold.rs            #   RedexFold<State> trait (CortEX / NetDB integration hook)
│   ├── file.rs            #   RedexFile (append / tail / read_range / close)
│   ├── manager.rs         #   Redex manager (open_file / get_file / with_persistent_dir)
│   ├── ordered.rs         #   OrderedAppender — single-threaded append for deterministic replay
│   ├── typed.rs           #   TypedRedexFile<T> — postcard-backed typed wrapper
│   ├── index.rs           #   RedexIndex<K, V> — generic tail-driven secondary index
│   └── disk.rs            #   DiskSegment (feature `redex-disk`): idx + dat append-only files, torn-write recovery
│
├── cortex/                # CortEX adapter — NetDB fold driver (feature `cortex`)
│   ├── mod.rs             #   Re-exports: CortexAdapter, EventMeta, EventEnvelope, ...
│   ├── meta.rs            #   20-byte EventMeta prefix codec + dispatch/flag constants
│   ├── envelope.rs        #   EventEnvelope + IntoRedexPayload trait
│   ├── config.rs          #   CortexAdapterConfig, StartPosition, FoldErrorPolicy
│   ├── error.rs           #   CortexAdapterError
│   ├── adapter.rs         #   CortexAdapter<State>: fold task, wait_for_seq, changes() broadcast
│   ├── watermark.rs       #   WatermarkingFold — discovers per-origin app_seq during replay
│   ├── rpc.rs             #   nRPC server-side fold + RpcServerFold + RpcClientFold + RpcContext
│   │
│   ├── tasks/             # First CortEX model — mutate-by-id CRUD (feature `cortex`)
│   │   ├── types.rs       #     Task, TaskStatus, TaskId + serde payload structs
│   │   ├── dispatch.rs    #     DISPATCH_TASK_* (0x01..0x04), TASKS_CHANNEL
│   │   ├── state.rs       #     TasksState + basic accessors
│   │   ├── fold.rs        #     TasksFold (decodes EventMeta, routes by dispatch)
│   │   ├── filter.rs      #     Plain-data TasksFilter (Prisma-ish surface, mirrors SDK shape)
│   │   ├── query.rs       #     TasksQuery fluent builder + TasksFilterSpec + OrderBy
│   │   ├── watch.rs       #     TasksWatcher reactive stream (initial + dedup)
│   │   └── adapter.rs     #     TasksAdapter wrapper (typed ingest + watch)
│   │
│   └── memories/          # Second CortEX model — content + tags + pin (feature `cortex`)
│       ├── types.rs       #     Memory, MemoryId + serde payload structs
│       ├── dispatch.rs    #     DISPATCH_MEMORY_* (0x10..0x14), MEMORIES_CHANNEL
│       ├── state.rs       #     MemoriesState + pinned/unpinned splits
│       ├── fold.rs        #     MemoriesFold
│       ├── filter.rs      #     Plain-data MemoriesFilter (Prisma-ish surface)
│       ├── query.rs       #     MemoriesQuery with single/any/all tag predicates
│       ├── watch.rs       #     MemoriesWatcher
│       └── adapter.rs     #     MemoriesAdapter wrapper
│
└── netdb/                 # NetDB — unified query façade over CortEX state (feature `netdb`)
    ├── mod.rs             #   Re-exports: NetDb, NetDbBuilder, NetDbSnapshot, NetDbError + re-exports of TasksFilter / MemoriesFilter
    ├── db.rs              #   NetDb (bundles TasksAdapter + MemoriesAdapter) + NetDbBuilder + whole-db snapshot/restore
    └── error.rs           #   NetDbError (wraps CortexAdapterError + missing-model errors)
```

## Adapters

### In-Memory (default)

```rust
use net::{EventBus, EventBusConfig};

let bus = EventBus::new(EventBusConfig::default()).await?;
bus.ingest(Event::from_str(r#"{"token": "hello"}"#)?)?;
```

### Redis

```toml
net = { path = ".", features = ["redis"] }
```

### JetStream

```toml
net = { path = ".", features = ["jetstream"] }
```

### Net

```toml
net = { path = ".", features = ["net"] }
```

## SDKs

All SDKs wrap the same Rust core. Every language gets the same performance.

| SDK | Package | Install | Highlights |
|-----|---------|---------|------------|
| **Rust** | [`ai2070-net-sdk`](https://crates.io/crates/ai2070-net-sdk) | `cargo add ai2070-net-sdk` | Builder pattern, async streams, typed subscriptions |
| **TypeScript** | [`@ai2070/net-sdk`](https://www.npmjs.com/package/@ai2070/net-sdk) | `npm install @ai2070/net-sdk @ai2070/net` | AsyncIterator, typed channels, Zod support |
| **Python** | [`ai2070-net-sdk`](https://pypi.org/project/ai2070-net-sdk/) | `pip install ai2070-net-sdk` | Generators, dataclass/Pydantic, context manager |
| **Go** | [`go`](../../../go/) | `go get github.com/ai-2070/net/go` | CGO bindings, zero allocations on raw ingest |
| **C** | [`net.h`](include/net.h) | `cargo build --release --features ffi,net` then bundle the header | One header, structured types, zero JSON overhead |

The Rust SDK imports as `use net_sdk::...`; the TypeScript SDK as `from '@ai2070/net-sdk'`; the Python SDK as `from net_sdk import ...`. The Rust core (`ai2070-net`), Node binding (`@ai2070/net`), and Python binding (`ai2070-net`) are the lower-level packages — useful when you want to skip the SDK ergonomics. Crate / module names inside the code (`net::`, `net._net`) stayed stable across the rename via package aliasing.

### Rust

```rust
use net_sdk::{Net, Backpressure};
use futures::StreamExt;

let node = Net::builder()
    .shards(4)
    .backpressure(Backpressure::DropOldest)
    .memory()
    .build()
    .await?;

// Emit
node.emit(&serde_json::json!({"token": "hello"}))?;

// Stream
let mut stream = node.subscribe(Default::default());
while let Some(event) = stream.next().await {
    println!("{}", event?.raw_str());
}

node.shutdown().await?;
```

### TypeScript

```typescript
import { NetNode } from '@ai2070/net-sdk';

const node = await NetNode.create({ shards: 4 });

// Emit
node.emit({ token: 'hello', index: 0 });

// Stream
for await (const event of node.subscribe({ limit: 100 })) {
  console.log(event.raw);
}

// Typed channels
const temps = node.channel<{ celsius: number }>('sensors/temperature');
temps.publish({ celsius: 22.5 });

await node.shutdown();
```

### Python

```python
from net_sdk import NetNode

node = NetNode(shards=4)

# Emit
node.emit({'token': 'hello', 'index': 0})

# Stream (generator)
for event in node.subscribe(limit=100):
    print(event.raw)

# Typed channels with Pydantic
temps = node.channel('sensors/temperature', TemperatureReading)
temps.publish(TemperatureReading(sensor_id='A1', celsius=22.5))

node.shutdown()
```

### Go

```go
node, _ := net.New(&net.Config{NumShards: 4})
defer node.Shutdown()

// Ingest
node.IngestRaw(`{"token": "hello"}`)

// Batch (zero allocations on raw path)
jsons := []string{`{"a":1}`, `{"a":2}`, `{"a":3}`}
count := node.IngestRawBatch(jsons)

// Poll
response, _ := node.Poll(100, "")
for _, event := range response.Events {
    fmt.Println(string(event))
}
```

### C

```c
#include "net.h"

net_handle_t node = net_init("{\"num_shards\": 4}");

// Ingest with receipt
const char* event = "{\"token\": \"hello\"}";
net_receipt_t receipt;
net_ingest_raw_ex(node, event, strlen(event), &receipt);

// Poll (structured, no JSON parsing)
net_poll_result_t result;
net_poll_ex(node, 100, NULL, &result);
for (size_t i = 0; i < result.count; i++) {
    printf("%.*s\n", (int)result.events[i].raw_len, result.events[i].raw);
}
net_free_poll_result(&result);

net_shutdown(node);
```

## Features

| Feature | Flag | Dependencies |
|---------|------|--------------|
| Redis Streams | `redis` | `redis` crate |
| NATS JetStream | `jetstream` | `async-nats` |
| Net transport | `net` | `chacha20poly1305`, `snow`, `blake2`, `dashmap`, `socket2`, `ed25519-dalek` |
| NAT traversal (classifier + rendezvous + `connect_direct`) | `nat-traversal` | `net` |
| Port mapping (NAT-PMP inlined + UPnP-IGD) | `port-mapping` | `nat-traversal`, `igd-next` |
| Regex filters | `regex` | `regex` crate |
| C FFI | `ffi` | -- |
| RedEX (local append-only log) | `redex` | `net`, `tokio-stream`, `postcard` |
| RedEX disk durability | `redex-disk` | `redex` |
| CortEX (adapter core + tasks + memories) | `cortex` | `redex` |
| NetDB (unified query façade) | `netdb` | `cortex` |

No features are enabled by default — opt into `redis`, `jetstream`, `net`, etc. explicitly.

## Building

```bash
# Core only — no adapters (opt in with a feature flag).
cargo build --release

# Redis adapter
cargo build --release --features redis

# Net only (2MB binary)
cargo build --release --features net

# Everything
cargo build --release --all-features
```

## Tests

```bash
# Unit tests (~1,573 with every feature on)
cargo test --all-features --lib

# Migration & group integration tests (53 tests)
cargo test --test migration_integration --features net

# Three-node mesh integration tests (66 tests)
cargo test --test three_node_integration --features net

# Two-node transport integration (13 tests)
cargo test --test integration_net --features net

# RedEX integration tests (27 tests: heap + persistent + age retention + ordered appender + typed wrappers)
cargo test --test integration_redex --features "redex redex-disk"

# CortEX adapter core (9 tests)
cargo test --test integration_cortex_adapter --features cortex

# CortEX tasks model (32 tests: CRUD + query + watch + replay + snapshot)
cargo test --test integration_cortex_tasks --features cortex

# CortEX memories model (25 tests: CRUD + tag queries + watch + coexistence + snapshot)
cargo test --test integration_cortex_memories --features cortex

# NetDB unified façade (13 tests: build, CRUD, filters, whole-db snapshot/restore)
cargo test --test integration_netdb --features netdb

# Rust SDK smoke tests (2 async + 3 doctests)
cargo test --features net -p net-sdk

# Node SDK smoke tests (62 tests — CortEX tasks + memories over napi, plus ABI stability, errors, NetDb handle, RedEX, and integration coverage. Includes watch/AsyncIterator, disk durability, snapshot/restore round-trip, and classified CortexError/NetDbError from @ai2070/net/errors)
cd bindings/node && npx napi build --platform --no-default-features -F cortex && npx vitest run

# Python SDK smoke tests (~190 collected — CortEX, NetDB, RedEX, channels + auth, capabilities, identity, compute + groups, snapshot/watch, subnets, ABI stability, and the Redis dedup helper. Total varies by enabled features.)
cd bindings/python && uv venv .venv && source .venv/bin/activate && \
    uv pip install -e '.[test]' maturin && \
    maturin develop && \
    python -m pytest

# Backend adapters (requires running services)
cargo test --test integration_redis --features redis
cargo test --test integration_jetstream --features jetstream
```

**~1,811 tests total across the Rust stack** — lib (1,573) + migration (53) + three_node (66) + integration_net (13) + integration_redex (27) + integration_cortex_{adapter,tasks,memories} (9+32+25) + integration_netdb (13). Plus 62 Node SDK smoke tests (vitest) and ~190 Python SDK smoke tests (pytest), both covering CRUD, filtered queries, reactive watchers, multi-model coexistence, disk-durability round-trips, whole-db `NetDb` snapshot/restore, per-adapter `open_from_snapshot`, and classified `CortexError` / `NetDbError` via the `@ai2070/net/errors` subpath (Node) / `net._net` module (Python).

### Test Architecture

Unit tests live in `#[cfg(test)]` modules alongside the code they test. Each migration module (orchestrator, source handler, target handler, subprotocol handler) has isolated tests covering happy paths, error paths, and edge cases.

Integration tests in `tests/migration_integration.rs` exercise the full migration system across module boundaries:

| Category | What it validates |
|----------|-------------------|
| **Phase chain** | All 6 phases sequenced end-to-end through the orchestrator, with and without buffered events |
| **End-to-end** | Source handler → orchestrator → target handler composing correctly: snapshot, buffer, restore, replay, cutover, cleanup. Verifies daemon moves between registries. |
| **Auto-target** | Scheduler-driven target selection via `CapabilityIndex` queries for `subprotocol:0x0500` |
| **Handler dispatch** | Each `MigrationMessage` variant dispatched through `MigrationSubprotocolHandler`, verifying correct outbound message types |
| **Handler routing** | Outbound `dest_node` assertions — CutoverNotify reaches source, SnapshotReady reaches target, CleanupComplete reaches orchestrator |
| **Snapshot chunking** | Small (single-chunk), large (multi-chunk), out-of-order reassembly, duplicate chunks, chunk count boundaries |
| **Event flow** | Events buffered on source during migration → drained → replayed on target → daemon stats verify processing |
| **Concurrency** | Two daemons migrating simultaneously without interference |
| **Abort** | Clean abort at every phase (Snapshot, Transfer, Replay, Cutover) |
| **Capability discovery** | `enrich_capabilities()` → `CapabilityAnnouncement` → `CapabilityIndex` → `Scheduler.find_migration_targets()` |
| **Wire format** | Encode/decode roundtrip for all 10 message variants including chunked SnapshotReady, ActivateTarget, ActivateAck |
| **Full lifecycle auto-chaining** | TakeSnapshot through ActivateAck runs end-to-end through the subprotocol handler with a mock message pump — single-chunk and multi-chunk. Failure paths verified: missing `DaemonFactoryRegistry` entry, corrupt snapshot bytes, `ActivateTarget` without prior restore. |

Three-node mesh tests in `tests/three_node_integration.rs` exercise the `MeshNode` runtime over real encrypted UDP:

| Category | What it validates |
|----------|-------------------|
| **Mesh formation** | 3-way handshake, health isolation after node death |
| **Data flow** | Point-to-point, bidirectional, stream isolation, full ring traffic, sustained throughput |
| **Relay** | A→B→C forwarding without decryption, payload integrity over 100 events, **tamper detection** (AEAD rejects corrupted relay) |
| **Rerouting** | Manual route update after failure, **automatic reroute** via ReroutePolicy + failure detector, auto-recovery when peer returns. Resolution order: `RoutingTable::lookup_alternate` → `ProximityGraph::path_to` → any direct peer. |
| **Router** | Forward/local/TTL/hop-count decisions over real UDP, multi-hop with 2 routers |
| **Full stack** | EventBus→NetAdapter→encrypted UDP→poll, bidirectional EventBus, backpressure flood |
| **Subnet gateway** | SubnetLocal blocked, Global forwarded, Exported selective, ParentVisible ancestor-only |
| **Failure detection** | Heartbeat→suspect→fail→recover lifecycle, correlated failure classification |
| **Migration over wire** | Full 6-phase lifecycle (TakeSnapshot → SnapshotReady → Restore → Replay → Cutover → Cleanup → Activate) runs autonomously over encrypted UDP. Three-node test asserts daemon ends up on target, absent from source, orchestrator record cleared. Acks route to the recorded orchestrator, not the wire hop. |
| **Handshake relay** | `connect_via(relay_addr, …)` establishes a Noise NKpsk0 session with a peer that has no direct UDP path. Handshake rides as a routed Net packet (`HANDSHAKE` flag) over existing relay sessions; post-handshake data flows A↔C through B via `send_routed`. |
| **DV routing** | Pingwave-driven route install populates both `RoutingTable` and `ProximityGraph::edges`. 3-hop chain A→B→C→D: A learns the route to D via B; `path_to(D)` returns the full 3-hop path. Regression: `path_to` used to always return `None` because edges were never populated. |
| **Stream multiplexing** | Multiple independent streams per peer, per-stream reliability + fairness weight, epoch-guarded handles reject sends after close+reopen, idle eviction + LRU cap |
| **Stream back-pressure (v1 + v2)** | v1 (concurrent callers racing a window) + v2 (single serial sender outrunning a slow receiver — byte-credit exhaustion). Both surface `StreamError::Backpressure`; `send_with_retry` absorbs transient pressure as receiver `StreamWindow` grants replenish credit. Regression: a serial sender on a small window must hit Backpressure (never `Transport(io::Error)`) and `credit_grants_received` must advance. |
| **Channel fan-out** | `ChannelPublisher` + `SubscriberRoster` over `SUBPROTOCOL_CHANNEL_MEMBERSHIP` — subscribe, publish fan-out reaches every subscriber, unsubscribe + peer-fail eviction from the roster |
| **Partition** | Detection via filter, healing with data flow recovery, asymmetric 3-node partition |

Regression tests are prefixed `test_regression_` and tied to specific bugs found during review. Each documents the original bug in its doc comment and would fail if the fix were reverted.

## Benchmarks

```bash
cargo bench --features net --bench net
cargo bench --bench ingestion
cargo bench --bench parallel
```

## Subprotocol ID Space

| Range | Purpose |
|-------|---------|
| `0x0000` | Plain events (no subprotocol) |
| `0x0001..0x03FF` | Reserved for core |
| `0x0400` | Causal events |
| `0x0401` | State snapshots |
| `0x0500` | Daemon migration (Mikoshi) |
| `0x0600` | Subprotocol negotiation |
| `0x0700..0x0702` | Continuity / fork announce / continuity proof |
| `0x0800..0x0801` | Partition / reconciliation |
| `0x0900` | Replica group coordination |
| `0x0A00` | Channel membership (subscribe / unsubscribe / ack) |
| `0x0B00` | Stream credit window (v2 backpressure — receiver→sender grants, 12-byte fixed message; see [`STREAM_BACKPRESSURE_PLAN_V2.md`](docs/STREAM_BACKPRESSURE_PLAN_V2.md)) |
| `0x0C00` | Capability announcement (signed capability broadcast for find_nodes / scope filtering) |
| `0x0D00` | NAT reflex probe (request / response, `nat-traversal` feature) |
| `0x0D01` | NAT rendezvous (`PunchRequest` / `PunchIntroduce` / `PunchAck`, `nat-traversal` feature) |
| `0x1000..0xEFFF` | Vendor / third-party |
| `0xF000..0xFFFF` | Experimental / ephemeral |

Note: handshake relay no longer consumes a subprotocol ID — it rides as a routed Net packet with the `HANDSHAKE` flag set in the **Net header's** `PacketFlags`, wrapped in the 18-byte routing header for forwarding, sharing the forwarding path with data packets.

## License

Apache-2.0
