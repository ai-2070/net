# Mesh Transport — Production Recipe

This is the file that turns "memory transport works in tests" into "two hosts in two regions exchange events over UDP." Read it when:
- You're moving from `memory` transport to `mesh` for multi-host pub/sub.
- You need to pick PSK vs identity keypair, decide on bootstrap topology, or wire NAT traversal.
- You're standing up a relay / coordinator node and need it to actually accept peers.

What is **not** here: mesh internals (forwarder loops, capability-index TTLs, AEAD tag handling — read `net/crates/net/src/adapter/net/`), custom subprotocols (`README.md` § Infinite extensibility via subprotocols), token issuance for permissioned channels (`runtime.md` § Errors mentions `ChannelRejected`; the issuance path is `Identity::issue_token` — `net/crates/net/sdk/src/identity.rs`).

For the shutdown lifecycle, see `runtime.md`. For per-language kwargs not covered here, see the SDK READMEs (`net/crates/net/sdk-ts/README.md`, `sdk-py/README.md`, `sdk/README.md`). If the user is migrating from a broker (Kafka, NATS, Redis Streams) and reaching for "where do I run the mesh server," start with `gotchas.md` — there is no server.

---

## Two surfaces, pick one

The Rust SDK exposes the mesh transport two ways. They are **not interchangeable**:

| Surface | Use it when |
|---|---|
| `Net::builder().mesh(NetAdapterConfig)` | You want a point-to-point Net adapter under the **event bus** API (`emit` / `subscribe`). One `NetAdapterConfig` = one peer. Initiator or responder, not both. Useful for a 2-node firehose; awkward for fan-out. |
| `Mesh::builder(bind, psk).build()` (Rust) / `MeshNode.create(...)` (TS) / `from net import NetMesh` (Python PyO3 binding) | You want **multi-peer pub/sub**: connect to N peers, register channels, publish to subscriber rosters. This is the canonical multi-host case. |

The recipes below all use the multi-peer surface — that's the reason most readers are here. `NetAdapterConfig::initiator(...)` / `responder(...)` (`net/crates/net/src/adapter/net/config.rs:99-157`) stays available for the 2-node-firehose case but doesn't generalize.

---

## The minimum viable mesh — 2 nodes

Every example shows the same call sequence: **create → accept (responder) / connect (initiator) → start → publish/subscribe → shutdown**. `accept()` MUST be called for every responder peer **before** `start()` — the dispatch loop consumes inbound UDP datagrams and races the responder handshake otherwise (`net/crates/net/src/adapter/net/mesh.rs:1798-1838` — calling `accept()` after `start()` returns `AdapterError::Fatal`).

### Rust

```rust
use net_sdk::mesh::Mesh;
use net_sdk::Identity;
use net::adapter::net::{ChannelName, PublishConfig};
use bytes::Bytes;

#[tokio::main]
async fn main() -> net_sdk::error::Result<()> {
    let psk = *b"my-32-byte-preshared-key-here!!!";          // shared across the mesh
    let identity = Identity::from_seed([0x42; 32]);          // load from disk in production

    // Node A — accept incoming peer
    let node_a = Mesh::builder("0.0.0.0:9000", &psk)?
        .identity(identity.clone())
        .build()
        .await?;
    // (node_b's pubkey + node_id are exchanged out of band — config / service registry / etc.)
    let _peer_addr = node_a.accept(node_b_node_id).await?;   // BEFORE start
    node_a.start();

    let channel = ChannelName::new("sensors/temp").unwrap();
    let report = node_a
        .publish(&channel, Bytes::from_static(b"{\"c\":22.5}"), PublishConfig::default())
        .await?;
    println!("delivered to {} subscribers", report.delivered);

    node_a.shutdown().await?;
    Ok(())
}
```

**Key facts:**
- `Mesh::builder(bind_addr, &psk)` returns a `MeshBuilder` (`net/crates/net/sdk/src/mesh.rs:384`). PSK is `&[u8; 32]` — 32 bytes, not a passphrase.
- `Mesh::public_key()` (`mesh.rs:391`) returns the Noise static pubkey to share with initiators. `Mesh::node_id()` (`mesh.rs:396`) returns the u64 routing id derived from the entity keypair.
- `Mesh::local_addr()` (`mesh.rs:411`) returns the node's **actual bound socket address**. Call it when you bind to `:0` (OS-assigned port) — a peer's `connect()` needs the real `ip:port`, not `:0`. Bound in every language: Python `NetMesh.local_addr()` (`bindings/python/src/lib.rs:1402`), Node `NetMesh.localAddr()` (`bindings/node/index.d.ts:2098`).
- `connect(peer_addr, &peer_pubkey, peer_node_id)` for the initiator side (`mesh.rs:459`); `accept(peer_node_id)` for the responder (`mesh.rs:476`). Pair them: one side connects, the other accepts. Symmetric "both sides connect" doesn't work — the second handshake collides with the first.
- `register_channel(ChannelConfig)` (publisher only) + `subscribe_channel(publisher_node_id, &channel)` (subscriber). `publish(&channel, payload, config)` returns a `PublishReport` (`mesh.rs:710`). For typed payloads, serialize with serde and pass `Bytes`.
- **Channel authorization is on by default.** The SDK `Mesh` (`mesh.rs:296`) and the Python/Node `NetMesh` bindings all install a `ChannelConfigRegistry`, so a peer's subscribe to a channel with **no registered config** is rejected (`UnknownChannel`) — `register_channel` adds the config (and nRPC's `serve_rpc` auto-registers its own service channels, so direct RPC needs no manual entry). The escape hatch, for test rigs and dynamic-channel surfaces that don't pre-register, is the bindings' `permissive_channels=True` (Python, `bindings/python/src/lib.rs:1258`) / `permissiveChannels: true` on `NetMesh.create({...})` (Node, `bindings/node/index.d.ts:4744`) — installs no registry, so there's no ACL. Default `false`.

### TypeScript

```typescript
import { MeshNode } from '@net-mesh/sdk';

const psk = '0'.repeat(64);                       // 64 hex chars = 32 bytes
const seed = Buffer.alloc(32, 0x42);              // load from secret store in production

const nodeA = await MeshNode.create({
  bindAddr: '0.0.0.0:9000',
  psk,
  identitySeed: seed,
});

await nodeA.accept(nodeBNodeId);                  // BEFORE start
await nodeA.start();

nodeA.registerChannel({ name: 'sensors/temp', visibility: 'global', reliable: true });
const report = await nodeA.publish('sensors/temp', Buffer.from('{"c":22.5}'));
console.log(`delivered to ${report.delivered} subscribers`);

await nodeA.shutdown();
```

**Key facts:**
- `MeshNode.create(config)` is async; `start()` is async too (napi binding requires the tokio reactor) — both must be `await`ed.
- `psk` is hex-encoded (`'0'.repeat(64)` = 32 zero bytes). `identitySeed` is a 32-byte `Buffer`; omit to mint an ephemeral one (entity_id changes every restart — almost never what you want).
- `accept(peerNodeId)` returns the resolved wire address (`bindings/node/index.d.ts:655`). Ordering contract is the same as Rust: every `accept()` must complete before `start()`.
- The lower-level NAPI surface (`@net-mesh/core`'s `NetMesh`) exposes `findNodes`, `connectDirect`, `natType`, etc. The SDK wrapper (`@net-mesh/sdk`'s `MeshNode`) covers the common pub/sub path — drop down to NAPI for NAT-traversal knobs.

### Python

The `net_sdk.MeshNode` wrapper today exposes connect / accept / start / open_stream / send_on_stream / shutdown — enough for stream-based pub/sub. The publish/subscribe-channel + capability discovery surface lives on the lower-level PyO3 binding (`from net import NetMesh`); use that directly when you need them.

```python
from net import NetMesh, generate_net_keypair

psk = "00" * 32                                   # 32 hex bytes (not a passphrase)
seed = bytes([0x42] * 32)                         # load from secret store in production

node = NetMesh(
    "0.0.0.0:9000",
    psk,
    identity_seed=seed,
)

node.accept(peer_b_node_id)                       # BEFORE start
node.start()

node.register_channel({"name": "sensors/temp", "visibility": "global", "reliable": True})
report = node.publish("sensors/temp", b'{"c":22.5}')
print(f"delivered to {report['delivered']} subscribers")

node.shutdown()
```

**Key facts:**
- `NetMesh(bind_addr, psk, **kwargs)` is the PyO3 class (`bindings/python/src/lib.rs:1079-1220`). PSK is a 64-char hex string. `identity_seed` is 32 raw bytes; `permissive_channels=True` opts out of the channel-config ACL (see Key facts above).
- `generate_net_keypair()` (`bindings/python/src/lib.rs:208`) mints a Noise keypair — that's the **transport** keypair, separate from the ed25519 identity. Use `Identity.from_seed(seed)` (`bindings/python/src/identity.rs:170`) for the identity layer.
- `register_channel`, `subscribe_channel`, `publish`, `find_nodes`, `connect_direct`, `local_addr`, `nat_type`, `traversal_stats` are all methods on `NetMesh` (`bindings/python/src/lib.rs:1576+`). The `net_sdk.MeshNode` wrapper does not yet re-export them — reach through to the binding.

---

## Identity — PSK vs ed25519 keypair

Two secrets, different jobs:

| Secret | What it does | Persist? |
|---|---|---|
| **PSK** (32 bytes) | Symmetric pre-shared key mixed into the Noise NKpsk0 handshake. Every node in the mesh holds the same value. A peer without the matching PSK can't complete the handshake — period. | Yes, distribute to every node, treat as shared secret. Rotation requires a synchronized rollout (or a dual-PSK overlap window run by your deployment, the SDK doesn't manage rotation). |
| **Identity keypair** (ed25519, 32-byte seed) | Per-node ed25519. Public key = `EntityId`. Derives `node_id` (u64, routing) and `origin_hash` (u32, packet header) — see `net/crates/net/src/adapter/net/identity/entity.rs:42-49`. Signs capability announcements + permission tokens. | Yes, **per-node**, treat like an SSH host key. Persist the seed; `entity_id` is derived. |

Generate once on first boot, persist to disk / vault / k8s secret, reload every subsequent run:

```rust
let id = Identity::generate();                    // fresh seed
std::fs::write("node.seed", id.to_bytes())?;      // 32 bytes — secret material
// later:
let seed = std::fs::read("node.seed")?;
let id = Identity::from_bytes(&seed)?;            // entity_id reproduces
```

```typescript
// TS: Buffer in/out. The mesh consumes the seed via `identitySeed` on create().
const seed = randomBytes(32);
fs.writeFileSync('node.seed', seed);
const node = await MeshNode.create({ bindAddr, psk, identitySeed: fs.readFileSync('node.seed') });
```

```python
from net import generate_net_keypair, Identity
identity = Identity.generate()
open('node.seed', 'wb').write(identity.to_bytes())
# later:
identity = Identity.from_seed(open('node.seed', 'rb').read())
node = NetMesh("0.0.0.0:9000", psk, identity_seed=identity.to_bytes())
```

When to use which:
- **PSK only, ephemeral keypair**: throwaway nodes (test rigs, short-lived workers). `node_id` changes every restart, which means the routing table churns and other peers see a "new" node. Fine for stateless workers; bad for anything tracked by name.
- **PSK + persisted seed** (the production default): stable `node_id` + `entity_id` across restarts; tokens issued against this entity stay valid; capability announcements de-dupe correctly.

---

## Peer discovery — your problem, not Net's

Net does **not** discover peers. There is no mDNS, no SRV record probing, no built-in service registry. The application gives the mesh a list of `(peer_addr, peer_pubkey, peer_node_id)` triples and calls `connect` / `accept`. Three patterns cover almost everything:

1. **Static peer list.** Config file or env var. Each node knows the others up front. Works for home labs, fixed fleets, two-node setups.
2. **Bootstrap node.** One well-known peer (DNS A record, or a known internal IP). New nodes connect to it; pingwave-driven proximity propagation (`net/crates/net/src/adapter/net/behavior/proximity.rs`) populates the rest of the routing table over the next few heartbeats. Subsequent nodes are visible to `find_nodes` even though you never connected directly.
3. **Out-of-band coordinator.** A service registry (Consul, etcd, your own DB) hands new joiners the bootstrap node's pubkey + node_id. The same coordinator is what you'd pass as the `coordinator` argument to `connect_direct` for NAT'd peers (rendezvous path — see below).

Don't try to make Net do mDNS / SRV / DNS-SD. It's the wrong layer — pick a service registry your ops already runs and feed it into the mesh.

---

## NAT traversal — opt-in optimization

**The routed-handshake fallback always works.** Two NATed peers behind any combination of cones, symmetrics, or unknown classifications still reach each other through encrypted relay forwarding. NAT traversal cuts the per-packet relay tax when a direct path is feasible — that's it. See <https://ai2070.net/docs/guides/nat-and-traversal>.

Cargo feature: `nat-traversal` (Rust SDK). The TS / Python bindings ship with stubs that no-op or return a "feature not built" error when the underlying cdylib was built without it.

```rust
// Rust — Cargo.toml
// net-sdk = { version = "...", features = ["nat-traversal"] }

// Ergonomic path — auto-selects the rendezvous coordinator for you:
let session = mesh.connect_direct_auto(peer_node_id, &peer_pubkey).await?;

// Explicit coordinator — when you want to name the mediator yourself:
let session = mesh.connect_direct(peer_node_id, &peer_pubkey, coordinator_node_id).await?;
//                                                                ^^^^^^^^^^^^^^^^^^^
//                                                a peer we already have a session with;
//                                                mediates the introduction.

let class = mesh.nat_type();                      // NatClass::{Open, Cone, Symmetric, Unknown}
let stats = mesh.traversal_stats();               // TraversalStatsSnapshot
println!("attempts={} succeeded={} failed={} relay_fallbacks={}",
    stats.punches_attempted, stats.punches_succeeded, stats.punches_failed, stats.relay_fallbacks);
```

Prefer `connect_direct_auto` — it selects the rendezvous coordinator (a peer you already share a session with) instead of making you pass one. Reach for the explicit `connect_direct` only when you want to name the mediator. When no coordinator candidate exists, `connect_direct_auto` returns `SdkError::Traversal { kind: "rendezvous-no-relay", .. }` — the routed handshake still works, you just don't get a punch. `Direct` / `Open` pairs need no coordinator and always succeed.

`traversal_stats()` returns a **`TraversalStatsSnapshot`**. The old three-counter view grew into a full effectiveness-plus-diagnosis surface in NAT-Traversal V2 — all `u64` counters (most cumulative; `punches_failed` is derived per snapshot) plus three port-mapping status fields. Read them as a group:

| Field | Meaning |
|---|---|
| `punches_attempted` / `punches_succeeded` | Mediated punch attempts, and the subset that produced a direct session. |
| `punches_failed` | **Derived** at snapshot time (`attempted − succeeded`); a punch in flight counts as failed until it resolves. |
| `relay_fallbacks` | `connect_direct` calls that ended on the routed-handshake path (matrix-skipped `Symmetric × Symmetric`, punch-failed, or direct-handshake-failed). |
| `punch_timeouts` / `punch_rejections` / `rendezvous_no_relay` | **Cause** counters — deadline give-ups, typed `PunchReject`s, and pairs skipped for want of any coordinator. *Not* partitions of `punches_failed`: they can fire before an attempt is even counted. |
| `upgrades_attempted` / `upgrades_succeeded` / `upgrades_deferred_busy` | **Background direct-path upgrade** (V2 Stage 3): a relay-routed session the runtime later re-punches into a direct one, no new call needed. `deferred_busy` = swap postponed because the session had open streams / unacked data; retried later, not a failure. |
| `port_mapping_active` (`bool`) / `port_mapping_external` (`Option<SocketAddr>`) / `port_mapping_renewals` (`u64`) | Current router port-mapping status — see § Port mapping. |

Effectiveness signal: `punches_succeeded / punches_attempted` near zero means the NATs in your environment don't punch — the mesh is mostly relayed; correct, but you pay the relay tax. The cause counters tell you *why* (timeouts vs. rejections vs. no coordinator).

`connect_direct` / `connect_direct_auto` always resolve: on a punch-failed outcome the session lands on the routed-handshake path. Inspect `traversal_stats` afterward to distinguish a successful punch from a relay fallback. Because of background upgrade, a session that started relayed may quietly become direct later — watch `upgrades_succeeded`.

The `nat_type` / `connect_direct` / `connect_direct_auto` / `traversal_stats` surface is on the lower-level binding (`@net-mesh/core`'s `NetMesh` in TS, `from net import NetMesh` in Python), with full stats parity across the FFI, Go, Node, and Python bindings. The Rust SDK exposes it on `Mesh` directly behind `#[cfg(feature = "nat-traversal")]`.

---

## Port mapping — opt-in shortcut

Cargo feature: `port-mapping` (builds on `nat-traversal`).

```rust
let mesh = Mesh::builder("0.0.0.0:9000", &psk)?
    .identity(identity)
    .try_port_mapping(true)                       // probes NAT-PMP, falls back to UPnP-IGD
    .build()
    .await?;
```

```typescript
const node = await MeshNode.create({
  bindAddr: '0.0.0.0:9000',
  psk,
  identitySeed: seed,
  tryPortMapping: true,
});
```

```python
node = NetMesh("0.0.0.0:9000", psk, identity_seed=seed, try_port_mapping=True)
```

What it does, on `start()`:

1. Probes NAT-PMP (1 s deadline), falls back to UPnP-IGD (2 s).
2. On success: calls `set_reflex_override(external_addr)` so peers see this node as `Open` with the mapped address.
3. Renews every 30 min. Three consecutive renewal failures = clear the override and exit cleanly (classifier takes over).
4. Revokes the mapping on graceful shutdown.

A router that doesn't speak either protocol leaves the node on the classifier path — that's fine, this is an optimization. Port-forwarded servers with a known external `ip:port` should pin it via `reflex_override(addr)` (Rust `MeshBuilder`) / `reflexOverride: 'ip:port'` (TS) / `reflex_override='ip:port'` (Python) instead of running the probe.

---

## 3-node example — verifying full-mesh discovery

Spin up A, B, C on three hosts. Each pair connects in one direction:

| Pair | Initiator | Responder |
|---|---|---|
| A ↔ B | A connects | B accepts |
| A ↔ C | C connects | A accepts |
| B ↔ C | B connects | C accepts |

Each node calls **all** of its `accept()` calls before any `start()`. After `start()`, every node calls `announce_capabilities` once (e.g. `caps.with_tag("role:worker")`). Within a few heartbeats, `find_nodes(filter)` on any node returns the other two:

```rust
use net_sdk::capabilities::{CapabilityFilter, CapabilitySet};

mesh.announce_capabilities(CapabilitySet::default().with_tag("role:worker")).await?;
// ... wait one heartbeat (~5 s default) ...
let peers = mesh.find_nodes(&CapabilityFilter::any().with_tag("role:worker"));
assert_eq!(peers.len(), 3);                       // A, B, C — including self
```

A publish from A reaches B and C if both have `subscribe_channel(a_node_id, &channel)`'d successfully. Verify via `report.delivered` on the publish.

---

## Capability sensing — leave it off

A whole plane exists for the existential question *"can **any** authorized provider currently satisfy capability Y under constraints C and latency envelope L?"* — asked once per distinct interest instead of once per watcher, coalesced locally and again at a rendezvous leader.

**As of 0.34 you should almost certainly not turn it on.** It ships dark: `enable_sensing_coalescing = false` by default, the rendezvous-leader role additionally gated behind the `redex` build feature, and there is **no TS / Python / Go SDK surface** — Rust core only. A mesh that leaves the flag off is byte-for-byte the previous release on the wire; its two frames (`0x0C02` `SensingInterestFrame`, `0x0C03` `ReadinessAttestation`) are never emitted.

The knobs, all defaulted inert, so you recognize them in a config diff: `enable_sensing_coalescing` (`false`), `sensing_interest_ttl` (30 s), `max_interests_per_peer` (512), `max_constraint_bytes` (1 KiB), `attestation_cadence_floor` (50 ms), `continuity_factor` (3), and the candidate-exploration bounds, which live on the sensing controller's own config rather than `MeshNodeConfig` and carry no `candidate_` prefix: `initial_fanout` (1), `standby_count` (1), `maximum_fanout` (3), `each_mode_max_providers` (32).

If you do evaluate it: **everything the plane reports is advisory.** Proofs are soft state, origin-signed, converging by expiry, and always defer the final yes/no to the admission path below. Follow a `Ready` with your own admission recheck — never treat it as authorization to proceed. Its first consumer is the gang scheduler's candidate pruning (`scheduler.md`). Operator surface: `net/crates/net/docs/SENSING.md`.

---

## Subnets — the spatial boundary on top of the mesh

Subnets are how you answer "which nodes can see what" once the deploy is bigger than one trust domain: keeping one fleet's telemetry out of another's, keeping a vehicle's internal channels internal on a public mesh, keeping tenants honest. They are a **scope** mechanism, not an encryption or consensus one — two nodes in the same subnet still use end-to-end encrypted sessions, and a subnet elects nothing and shares no state.

**The hierarchy is four 8-bit levels packed into one `u32`** (`SubnetId`), 256 children per level. The conventional read is region → fleet → vehicle → subsystem, but nothing in Net imposes that — the structure is fixed, the labels are yours. `SubnetId::new(&[3])` is a level-0 subnet; `SubnetId::new(&[3, 7, 1, 4])` is fully qualified; `SubnetId::GLOBAL` (all-zero) is the unrestricted root. Parent / child / sibling / distance all resolve with bitwise ops at wire speed.

**Assignment is data-driven, from capability tags.** A `SubnetPolicy` is a list of `SubnetRule { tag_prefix, level, values }` — each rule maps a capability-tag *prefix* to a hierarchy level (0–3) and a tag-value → byte map:

```rust
let policy = SubnetPolicy::new()
    .add_rule(SubnetRule::new("region:", 0).map("us-west", 1).map("eu-central", 2))
    .add_rule(SubnetRule::new("fleet:",  1).map("alpha", 7));
// a node tagged region:us-west, fleet:alpha  ->  SubnetId::new(&[1, 7])
```

Rules combine across levels to fill the id; **any level no rule fills stays `0` — i.e. unrestricted.** There is no separate "default subnet," and a node whose tags match nothing lands on `GLOBAL`. The consequence worth internalizing: subnet membership *changes as capabilities change*. Add a `fleet:` tag and the node moves; the matching gateway picks it up; the matching channel scopes start applying. No separate config push, and no way for a node's claimed subnet to disagree with its capability set. (`add_rule` panics on `level >= 4`; use `try_add_rule` for config-file / FFI / JSON input.)

**Gateways enforce channel `Visibility` at the boundary, header-only:**

| `Visibility` | Decision at a subnet boundary |
|---|---|
| `SubnetLocal` | always dropped |
| `ParentVisible` | forwarded only toward ancestor subnets |
| `Exported` | forwarded only to subnets in the channel's export table |
| `Global` | always forwarded (**the default**) |

The gateway reads `channel_hash` + `subnet_id` from the header and consults its `ChannelConfigRegistry` — **it never decrypts.** So a `SubnetLocal` channel cannot leak across a gateway by construction, not by convention. Gateways also enforce a hop TTL so a mesh loop can't burn forever, and track drop reasons in atomic counters (visibility vs TTL vs unknown-subnet — the last usually means config drift).

## Channel authorization — cap filters are advisory; tokens are the boundary

This is the trap in `ChannelConfig`, and it's stated verbatim in the source (`channel/config.rs`):

> `publish_caps` / `subscribe_caps` match against a node's **self-advertised** `CapabilitySet`. A peer declares its own capabilities in its own signed announcement, so **any peer can satisfy a cap-filter simply by advertising the required tag** (e.g. self-asserting `role:admin`).

Treat capability filters as **matchmaking / intent routing, not a security boundary.** The signature proves *who announced it*, never that the claim is true.

The actual boundary is `require_token` + `token_roots`: a root-anchored `TokenChain` can't be forged, because every link is signature-verified up to a root the channel explicitly trusts. **Any channel that must restrict who publishes or subscribes needs token enforcement — a cap-filter alone restricts nothing.** See `concepts.md` § Permission tokens for the chain rules (root-anchor, leaf-binding, per-link scope).

Both checks run at subscription time; on success `(origin_hash, channel_hash)` lands in the `AuthGuard` and the per-packet path is a constant-time bloom probe thereafter. That caching is also why revocation is checked at session/subscription time rather than per packet.

---

## Route churn — nothing to configure

Control-plane hardening landed on by default and needs no action; it matters only when you're reading logs and wondering why a route moved. Pingwaves pass an admission gate (dedup) **before** touching the proximity graph or routing table, so a replayed pingwave can't reinstall a just-withdrawn route. A direct peer transitioning to `Failed` **always** floods a route withdrawal and drops the dead edge, regardless of what a stale graph still shows a path for. Alternate-path promotion looks past the shortest path, so a reroute can still succeed when the shortest path starts with the peer being withdrawn. Withdraw floods are damped per `(destination, exclude)` recipient rather than globally.

All of it degrades cleanly against un-upgraded peers.

---

## Production checklist

- **PSK is a shared secret.** Distribute over a secure channel (k8s secret, vault). Rotate via a dual-PSK overlap if your fleet allows it; otherwise plan a short outage.
- **Identity seeds are secret material.** Treat like SSH host keys. Per-node, persisted, never committed to git.
- **Bind to a real interface in production.** `127.0.0.1:9000` is for tests. Use `0.0.0.0:9000` (or a specific public IP) on the host you actually want peers to reach.
- **Open the UDP port in your firewall.** Outbound + inbound on the bind port. The mesh is UDP — security groups that only allow TCP will silently break the handshake.
- **Opt into `nat-traversal` + `port-mapping` only when the deploy actually crosses NAT boundaries.** For a LAN-only deploy, leave them off — there's nothing to traverse.
- **Test the routed-handshake fallback explicitly.** Simulate one peer with no direct UDP path (block its port, or use `block_peer` in a test harness). Confirm events still flow via a relay. If they don't, your topology has no relay-capable node and the fallback isn't actually working.
- **Call `accept()` for every responder peer BEFORE `start()`.** Calling `accept()` after `start()` returns `AdapterError::Fatal` (`net/crates/net/src/adapter/net/mesh.rs:4386`) — the runtime rejects it explicitly to prevent the handshake-race hang.
- **Watch `traversal_stats` if NAT traversal is on.** A `relay_fallbacks` counter that grows much faster than `punches_succeeded` says the punch path isn't earning its keep — fine for correctness, expensive for relays. When it does, the cause counters (`punch_timeouts` / `punch_rejections` / `rendezvous_no_relay`) tell you which failure mode dominates, and a healthy `upgrades_succeeded` means relayed sessions are still being reclaimed into direct ones in the background.
- **Shutdown cleanly.** See `runtime.md` — same contract as memory transport, plus the mesh closes peer sessions on the way out (peers see "graceful departure" rather than "suspect").

## Further reading

- [NAT and Traversal](https://ai2070.net/docs/guides/nat-and-traversal)
- [Subnets](https://ai2070.net/docs/concepts/subnets)
- [Channels](https://ai2070.net/docs/concepts/channels)
- [Security Model](https://ai2070.net/docs/concepts/security-model)
