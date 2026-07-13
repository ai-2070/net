# Subprotocol Registry

Every Net packet carries a 16-bit `subprotocol_id` in its header. The id tells the receiver how to interpret the payload — plain event, causal event, snapshot, daemon migration message, NAT-traversal probe, and so on. This page is the canonical list of assigned ids, the ranges reserved for future use, and the rules that govern how the space evolves.

## ID space

The space is 16 bits — 65,536 IDs. The substrate has carved out the first `0x2200` for its own use; vendors get most of the rest; the top `0x1000` is for experiments that don't need a permanent assignment.

| ID                 | Purpose                                                    |
|--------------------|------------------------------------------------------------|
| `0x0000`           | Plain events — no subprotocol, raw payload                 |
| `0x0001..=0x21FF`  | Reserved for substrate / core protocols (see assignments below) |
| `0x0400`           | Causal events                                              |
| `0x0401`           | State snapshots                                            |
| `0x0500`           | Daemon migration                                           |
| `0x0600`           | Subprotocol negotiation                                    |
| `0x0700`           | Continuity proofs                                          |
| `0x0701`           | Fork announcements                                         |
| `0x0702`           | Continuity proof transfer                                  |
| `0x0800`           | Partition detection                                        |
| `0x0801`           | Log reconciliation                                         |
| `0x0900`           | Replica group coordination (reserved, not active)          |
| `0x0A00`           | Channel membership                                         |
| `0x0B00`           | Stream-window flow control (24-byte payload, carries `ack_seq` for ack-driven retransmit pruning) |
| `0x0B01`           | Stream NACK — reliable-stream retransmit signaling         |
| `0x0B02`           | Stream RESET — reliable-stream hard-failure signal         |
| `0x0B03`           | Stream ACK — SACK-range acknowledgements for retransmit pruning |
| `0x0C00`           | Capability announcement                                    |
| `0x0C01`           | Route withdrawal — poison-reverse; a node floods "destination unreachable *via me*" on peer failure (0.32) |
| `0x0D00`           | NAT-traversal reflex                                       |
| `0x0D01`           | NAT-traversal rendezvous                                   |
| `0x0E00`           | RedEX distributed replication                              |
| `0x0F00`           | MeshDB federated query                                     |
| `0x1000`           | Fold framework dispatch (typed fold envelopes, `kind` selects the fold)  |
| `0x1100`           | Blob transfer (content-addressed fetch over scheduled streams) |
| `0x1200..=0x21FF`  | Reserved for future substrate subprotocols                 |
| `0x2200..=0xEFFF`  | Vendor / third-party                                       |
| `0xF000..=0xFFFF`  | Experimental / ephemeral                                   |

## The opaque-forwarding guarantee

Packets carrying an unregistered `subprotocol_id` are forwarded by intermediate nodes without modification or decryption. This is the rule that makes incremental subprotocol deployment possible: a new subprotocol can roll out to producers and consumers before every node in the mesh understands it, and the in-between nodes will pass the packets through.

The guarantee applies to forwarding only — the destination must understand the subprotocol to do anything with the packet beyond delivering it. The capability layer is the typical way to know in advance whether a destination supports a subprotocol; nodes advertise `subprotocol:0x<id>` tags for every protocol they handle.

## Version negotiation

Each subprotocol has a major/minor version. When two peers establish a session, they exchange `SubprotocolManifest`s listing what they support; the result is a `NegotiatedSet` of subprotocols both peers handle at compatible versions:

```rust
pub struct SubprotocolDescriptor {
    pub id: u16,
    pub name: String,
    pub version: SubprotocolVersion,
    pub min_compatible: SubprotocolVersion,
    pub handler_present: bool,
}

pub struct SubprotocolVersion {
    pub major: u8,
    pub minor: u8,
}
```

Compatibility check: both peers' version must satisfy the other's `min_compatible`. Subprotocols not in the negotiated set fall back to opaque forwarding — they can still travel through the mesh, they just can't be interpreted by the peer that didn't sign up for them.

The negotiation itself rides on subprotocol `0x0600`.

## `SUBPROTOCOL_REDEX` dispatch codes

The RedEX replication subprotocol (`0x0E00`) partitions its payload by a single-byte dispatch code immediately after the subprotocol id:

| Code   | Direction               | Purpose                                                                 |
|--------|-------------------------|-------------------------------------------------------------------------|
| `0x20` | replica → leader        | `SyncRequest` — replica asks for events `[since_seq, since_seq+chunk)` |
| `0x21` | leader → replica        | `SyncResponse` — bounded chunk of in-order events                       |
| `0x22` | bidirectional           | `SyncHeartbeat` — liveness + tail-seq exchange                          |
| `0x23` | leader → replica        | `SyncNack` — typed rejection (see error codes below)                   |
| `0x24..=0x2F` | reserved          | Future variants (range-bounded sync, parallel-stream sync, etc.)        |

`SyncNack` carries one of four typed error codes:

| Code | Name               | Replica retry policy                                          |
|------|--------------------|---------------------------------------------------------------|
| `1`  | `NotLeader`        | Re-resolve leader via `Mesh::find_chain_holders` and retry    |
| `2`  | `BadRange`         | Trim local tail; retry from leader's first available `seq`    |
| `3`  | `Backpressure`     | Exponential backoff; retry the same `SyncRequest`             |
| `4`  | `ChannelClosed`    | Withdraw replica role; emit metric                            |

There's deliberately no `LeaderElection` code in the replication subprotocol. Leader election is a deterministic function over each node's locally-known state (proximity-graph RTT, replica-set membership, NodeId ordering) — no election message rides on the wire. The capability-tag layer is what peers observe; the result of `elect()` drives a `transition_to` call that announces or withdraws the channel's capability tag.

## Capability advertisement

Nodes advertise the subprotocols they handle through the capability graph. `SubprotocolRegistry::enrich_capabilities()` adds a tag `subprotocol:0x<id>` for each handled subprotocol to the node's `CapabilitySet`. Other nodes use these tags to find peers for capability-routed operations — a daemon migration request looks for nodes advertising `subprotocol:0x0500`, a partition-recovery handshake looks for `subprotocol:0x0801`.

```rust
let caps = subprotocol_registry.enrich_capabilities(CapabilitySet::new());
// caps now carries: "subprotocol:0x0400", "subprotocol:0x0500", ...

let filter = SubprotocolRegistry::capability_filter_for(0x0500);
let migration_targets = capability_index.query(&filter);
```

## Adding a new subprotocol

The process is the same whether you're adding to core or to a vendor extension:

1. **Pick an id.** Core subprotocols go into `0x0001..=0x21FF` — by convention each new subsystem claims a `0xXY00` base and uses the low byte for variants (e.g. `0x0B00` / `0x0B01` / `0x0B02` for the stream-window family). Vendor extensions go into `0x2200..=0xEFFF`. Experimental work goes into `0xF000..=0xFFFF`.
2. **Define a descriptor.** Name, version, `min_compatible`. The version pair tracks breaking changes (major) and backward-compatible changes (minor).
3. **Register a handler.** `SubprotocolRegistry::register(descriptor)`; the dispatch surface picks up the registration.
4. **Enrich capabilities.** Once the handler is registered, `enrich_capabilities()` will add the `subprotocol:0x<id>` tag automatically.
5. **Advertise the descriptor.** Other peers see the new subprotocol on session establishment via the manifest exchange.

Forward compatibility is on by default — peers that don't understand the new subprotocol fall back to opaque forwarding, so deployment is rolling, not coordinated.
