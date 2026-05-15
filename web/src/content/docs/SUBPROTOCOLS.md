# Subprotocol Registry

Formalizes the 16-bit `subprotocol_id` field in every Net header. Provides a registry for protocol handlers, version negotiation between peers, and an opaque forwarding guarantee for unknown protocols.

## Subprotocol IDs

Every Net packet carries a `subprotocol_id: u16` identifying how the payload should be interpreted. The ID space is partitioned:

| Range | Purpose |
|-------|---------|
| `0x0000` | Plain events (no subprotocol) |
| `0x0001..0x03FF` | Reserved for core |
| `0x0400` | Causal events |
| `0x0401` | State snapshots |
| `0x0500` | Daemon migration |
| `0x0600` | Subprotocol negotiation |
| `0x0601` | Handshake relay (relayed Noise NKpsk0) |
| `0x0700` | Continuity proofs |
| `0x0701` | Fork announcements |
| `0x0702` | Continuity proof transfer |
| `0x0800` | Partition detection |
| `0x0801` | Log reconciliation |
| `0x0900` | Replica group coordination (reserved) |
| `0x0A00` | Channel membership |
| `0x0B00` | Stream-window flow control |
| `0x0C00` | Capability announcement |
| `0x0D00` | NAT-traversal reflex |
| `0x0D01` | NAT-traversal rendezvous |
| `0x0E00` | RedEX Distributed replication |
| `0x1000..0xEFFF` | Vendor / third-party |
| `0xF000..0xFFFF` | Experimental / ephemeral |

### `SUBPROTOCOL_REDEX` dispatch codes (`0x0E00`)

The replication subprotocol partitions its payload via a single `dispatch_code: u8` byte immediately after the 2-byte `subprotocol_id`. All multi-byte integers are **little-endian fixed-width** (no varints). `channel_id` is the 32-byte BLAKE2s hash of the channel name with the domain-separation label `"redex-channel-id-v1"`. See `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §2 for full byte layouts.

| Code | Direction | Purpose | Size |
|------|-----------|---------|------|
| `0x20` `SYNC_REQUEST` | replica → leader | Replica asks for events `[since_seq, since_seq + chunk_max)` | 47 B (fixed) |
| `0x21` `SYNC_RESPONSE` | leader → replica | Bounded chunk of in-order events | variable |
| `0x22` `SYNC_HEARTBEAT` | bidirectional | Liveness + tail-seq exchange | 52 B (fixed) |
| `0x23` `SYNC_NACK` | leader → replica | Structured rejection (typed `error_code`) | variable |
| `0x24..0x2F` | reserved | Future variants (range-bounded sync, parallel-stream sync, etc.) | — |

**No `LEADER_ELECTION` code** — election is a pure deterministic function over each node's locally-known state (proximity-graph RTT + replica-set membership + NodeId ordering). The per-channel `ReplicationCoordinator` runtime task runs `elect()` directly when entering `Candidate`; the result drives a `transition_to(Leader | Replica)` call that emits the capability tag via `Mesh::announce_chain` / withdraws via `Mesh::withdraw_chain`. The capability-tag layer is what peers observe; no RedEX-specific election message rides on the wire.

**`SyncNack` error codes** (replica retry-policy key):
- `1` `NotLeader` → re-resolve leader via `Mesh::find_chain_holders`.
- `2` `BadRange` → trim local tail; retry from leader's first available `seq`.
- `3` `Backpressure` → exponential backoff; same `SYNC_REQUEST`.
- `4` `ChannelClosed` → withdraw replica role; emit metric.

Silent stream close is reserved for transport-level failure only; every application-level rejection MUST surface as `SyncNack`.

## Descriptors

Each registered subprotocol has a `SubprotocolDescriptor`:

```rust
pub struct SubprotocolDescriptor {
    pub id: u16,                              // Header field value
    pub name: String,                         // Human-readable (e.g., "causal")
    pub version: SubprotocolVersion,          // This handler's version
    pub min_compatible: SubprotocolVersion,   // Minimum peer version accepted
    pub handler_present: bool,                // false = opaque forwarding only
}

pub struct SubprotocolVersion {
    pub major: u8,   // Breaking changes
    pub minor: u8,   // Backward-compatible changes
}
```

Versions use 2-byte wire format (`[major, minor]`). Compatibility check: both peers' version must satisfy the other's `min_compatible`.

## Registry

`SubprotocolRegistry` maps IDs to descriptors and handlers.

```rust
impl SubprotocolRegistry {
    fn register(&self, descriptor: SubprotocolDescriptor) -> Result<(), RegistryError>
    fn lookup(&self, id: u16) -> Option<SubprotocolDescriptor>
    fn is_handled(&self, id: u16) -> bool
    fn all_descriptors(&self) -> Vec<SubprotocolDescriptor>
}
```

**Opaque forwarding guarantee:** Packets with unregistered `subprotocol_id` values are forwarded to the next hop without modification. This allows new protocols to be deployed incrementally -- intermediate nodes don't need to understand a protocol to forward it.

## Version Negotiation

When two peers establish a session, they exchange `SubprotocolManifest`s listing their supported subprotocols and versions.

```rust
pub struct SubprotocolManifest {
    pub entries: Vec<ManifestEntry>,
}

pub struct ManifestEntry {
    pub id: u16,
    pub version: SubprotocolVersion,
    pub min_compatible: SubprotocolVersion,
}
```

`NegotiatedSet` is the result of negotiation -- the subset of subprotocols both peers support at compatible versions. Packets using non-negotiated subprotocols fall back to opaque forwarding.

**Subprotocol ID for negotiation itself:** `0x0600`.

## Capability Advertisement

Nodes announce supported subprotocols through the capability graph. `SubprotocolRegistry::enrich_capabilities()` adds a tag `subprotocol:0x{id:04x}` for each handled subprotocol to the node's `CapabilitySet`. This enables capability-driven routing -- a migration request routes to the nearest node advertising `subprotocol:0x0500`.

```rust
// On node startup: enrich capabilities with subprotocol tags
let caps = subprotocol_registry.enrich_capabilities(CapabilitySet::new());
// caps now has tags: "subprotocol:0x0400", "subprotocol:0x0500", etc.

// Other nodes discover migration-capable targets via the index
let filter = SubprotocolRegistry::capability_filter_for(0x0500);
let targets = capability_index.query(&filter);
```

## Migration Handler

`MigrationSubprotocolHandler` dispatches inbound migration messages (0x0500) to the `MigrationOrchestrator`, `MigrationSourceHandler`, or `MigrationTargetHandler` as appropriate. It produces outbound messages with correct destination routing. See [COMPUTE.md](COMPUTE.md) for the full migration protocol.

## Source Files

| File | Purpose |
|------|---------|
| `subprotocol/descriptor.rs` | `SubprotocolDescriptor`, `SubprotocolVersion` |
| `subprotocol/registry.rs` | `SubprotocolRegistry`, ID-to-handler mapping, `enrich_capabilities()` |
| `subprotocol/negotiation.rs` | `SubprotocolManifest`, `NegotiatedSet`, version negotiation |
| `subprotocol/migration_handler.rs` | `MigrationSubprotocolHandler`, migration message dispatch |
| `redex/replication.rs` | `SUBPROTOCOL_REDEX` wire codec — `SyncRequest` / `SyncResponse` / `SyncHeartbeat` / `SyncNack` |
