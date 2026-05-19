# Mesh Transport — Production Recipe

This is the file that turns "memory transport works in tests" into "two hosts in two regions exchange events over UDP." Read it when:
- You're moving from `memory` transport to `mesh` for multi-host pub/sub.
- You need to pick PSK vs identity keypair, decide on bootstrap topology, or wire NAT traversal.
- You're standing up a relay / coordinator node and need it to actually accept peers.

What is **not** here: mesh internals (forwarder loops, capability-index TTLs, AEAD tag handling — read `net/crates/net/src/adapter/net/`), custom subprotocols (`net/README.md` § Subprotocols), token issuance for permissioned channels (`runtime.md` § Errors mentions `ChannelRejected`; the issuance path is `Identity::issue_token` — `net/crates/net/sdk/src/identity.rs`).

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
- `Mesh::builder(bind_addr, &psk)` returns a `MeshBuilder` (`net/crates/net/sdk/src/mesh.rs:341-343`). PSK is `&[u8; 32]` — 32 bytes, not a passphrase.
- `Mesh::public_key()` (`mesh.rs:348`) returns the Noise static pubkey to share with initiators. `Mesh::node_id()` (`mesh.rs:353`) returns the u64 routing id derived from the entity keypair.
- `connect(peer_addr, &peer_pubkey, peer_node_id)` for the initiator side (`mesh.rs:366-377`); `accept(peer_node_id)` for the responder (`mesh.rs:383-386`). Pair them: one side connects, the other accepts. Symmetric "both sides connect" doesn't work — the second handshake collides with the first.
- `register_channel(ChannelConfig)` (publisher only) + `subscribe_channel(publisher_node_id, &channel)` (subscriber). `publish(&channel, payload, config)` returns a `PublishReport` (`mesh.rs:561-568`). For typed payloads, serialize with serde and pass `Bytes`.

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
- `NetMesh(bind_addr, psk, **kwargs)` is the PyO3 class (`bindings/python/src/lib.rs:1079-1220`). PSK is a 64-char hex string. `identity_seed` is 32 raw bytes.
- `generate_net_keypair()` (`bindings/python/src/lib.rs:208`) mints a Noise keypair — that's the **transport** keypair, separate from the ed25519 identity. Use `Identity.from_seed(seed)` (`bindings/python/src/identity.rs:170`) for the identity layer.
- `register_channel`, `subscribe_channel`, `publish`, `find_nodes`, `connect_direct`, `nat_type`, `traversal_stats` are all methods on `NetMesh` (`bindings/python/src/lib.rs:1576+`). The `net_sdk.MeshNode` wrapper does not yet re-export them — reach through to the binding.

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

**The routed-handshake fallback always works.** Two NATed peers behind any combination of cones, symmetrics, or unknown classifications still reach each other through encrypted relay forwarding. NAT traversal cuts the per-packet relay tax when a direct path is feasible — that's it. Full design: `net/crates/net/docs/NAT_TRAVERSAL_PLAN.md`. Operator-facing summary: `net/crates/net/README.md` § NAT Traversal.

Cargo feature: `nat-traversal` (Rust SDK). The TS / Python bindings ship with stubs that no-op or return a "feature not built" error when the underlying cdylib was built without it.

```rust
// Rust — Cargo.toml
// net-sdk = { version = "...", features = ["nat-traversal"] }

let session = mesh.connect_direct(peer_node_id, &peer_pubkey, coordinator_node_id).await?;
//                                                                ^^^^^^^^^^^^^^^^^^^
//                                                a peer we already have a session with;
//                                                mediates the introduction.

let class = mesh.nat_type();                      // NatClass::{Open, Cone, Symmetric, Unknown}
let stats = mesh.traversal_stats();
println!("attempts={} succeeded={} relay_fallbacks={}",
    stats.punches_attempted, stats.punches_succeeded, stats.relay_fallbacks);
```

The three counters (`punches_attempted`, `punches_succeeded`, `relay_fallbacks`) are monotonic u64s. Operators read them for an effectiveness signal — a deploy where `punches_succeeded / punches_attempted` is near zero says "the NATs in your environment don't punch; the mesh is mostly relayed; that's fine, but you're paying the relay tax."

`connect_direct` always resolves: on a punch-failed outcome, the session lands on the routed-handshake path. Inspect `traversal_stats` afterward to distinguish a successful punch from a relay fallback (`net/crates/net/sdk/src/mesh.rs:937-973`).

The `nat_type` / `connect_direct` / `traversal_stats` surface is on the lower-level binding (`@net-mesh/core`'s `NetMesh` in TS, `from net import NetMesh` in Python). The Rust SDK exposes them on `Mesh` directly behind `#[cfg(feature = "nat-traversal")]`.

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

A router that doesn't speak either protocol leaves the node on the classifier path — that's fine, this is an optimization. Port-forwarded servers with a known external `ip:port` should pin it via `with_reflex_override(addr)` (Rust) / `reflexOverride: 'ip:port'` (TS) / `reflex_override='ip:port'` (Python) instead of running the probe.

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

## Production checklist

- **PSK is a shared secret.** Distribute over a secure channel (k8s secret, vault). Rotate via a dual-PSK overlap if your fleet allows it; otherwise plan a short outage.
- **Identity seeds are secret material.** Treat like SSH host keys. Per-node, persisted, never committed to git.
- **Bind to a real interface in production.** `127.0.0.1:9000` is for tests. Use `0.0.0.0:9000` (or a specific public IP) on the host you actually want peers to reach.
- **Open the UDP port in your firewall.** Outbound + inbound on the bind port. The mesh is UDP — security groups that only allow TCP will silently break the handshake.
- **Opt into `nat-traversal` + `port-mapping` only when the deploy actually crosses NAT boundaries.** For a LAN-only deploy, leave them off — there's nothing to traverse.
- **Test the routed-handshake fallback explicitly.** Simulate one peer with no direct UDP path (block its port, or use `block_peer` in a test harness). Confirm events still flow via a relay. If they don't, your topology has no relay-capable node and the fallback isn't actually working.
- **Call `accept()` for every responder peer BEFORE `start()`.** Calling `accept()` after `start()` returns `AdapterError::Fatal` (`net/crates/net/src/adapter/net/mesh.rs:1798-1838`) — the runtime rejects it explicitly to prevent the handshake-race hang.
- **Watch `traversal_stats` if NAT traversal is on.** A `relay_fallbacks` counter that grows much faster than `punches_succeeded` says the punch path isn't earning its keep — fine for correctness, expensive for relays.
- **Shutdown cleanly.** See `runtime.md` — same contract as memory transport, plus the mesh closes peer sessions on the way out (peers see "graceful departure" rather than "suspect").
