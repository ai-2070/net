# Distributed State

Causal ordering for distributed events. Every event carries a 24-byte `CausalLink` chaining it to the previous event. The chain provides structural integrity via xxh3 hashing -- tamper resistance comes from Net's AEAD encryption.

## Causal Links

The fundamental ordering primitive. 24 bytes prepended to each event in causal-framed `EventFrame`s.

```
Wire format (24 bytes, no padding):
  origin_hash:      4 bytes (u32)  -- entity identity
  horizon_encoded:  4 bytes (u32)  -- compressed observed horizon
  sequence:         8 bytes (u64)  -- monotonic per-entity
  parent_hash:      8 bytes (u64)  -- xxh3 of (prev link ++ prev payload)
```

```rust
pub struct CausalLink {
    pub origin_hash: u32,        // Matches Net header origin_hash
    pub horizon_encoded: u32,    // Bloom sketch of observed horizon
    pub sequence: u64,           // Monotonic from entity's reference frame
    pub parent_hash: u64,        // xxh3(prev_link_bytes ++ prev_payload_bytes)
}
```

**Chain construction:**
- `CausalLink::genesis()` creates the first link (sequence 0, parent_hash 0)
- `CausalLink::next()` produces the successor given the previous link and payload
- Chain validation: `validate_chain_link()` verifies parent_hash matches

**Subprotocol ID:** `0x0400` for causal-framed events.

## Causal Chain Builder

`CausalChainBuilder` maintains per-entity chain state and produces new links.

```rust
pub struct CausalChainBuilder {
    origin_hash: u32,
    head: CausalLink,
    last_payload_hash: u64,
    horizon: HorizonEncoder,
    event_count: u64,
}
```

The builder tracks the chain head and the last payload hash to compute `parent_hash` for the next event. The `HorizonEncoder` compresses the entity's observed horizon into 4 bytes.

## Causal Events

A `CausalEvent` pairs a link with its payload:

```rust
pub struct CausalEvent {
    pub link: CausalLink,
    pub payload: Bytes,
}
```

Events are the unit of storage in `EntityLog` and the unit of processing in `MeshDaemon::process()`.

## Observed Horizons

Each entity tracks what it has observed from other entities. The `ObservedHorizon` is the full view; `HorizonEncoder` compresses it to 4 bytes for the wire.

```rust
pub struct ObservedHorizon {
    observations: HashMap<u32, u64>,  // origin_hash -> latest observed sequence
}

pub struct HorizonEncoder { /* bloom sketch parameters */ }
```

`ObservedHorizon` supports:
- `observe(origin_hash, sequence)` -- record an observation
- `has_observed(origin_hash, sequence)` -- exact query
- `merge(other)` -- combine two horizons

`HorizonEncoder` compresses the full horizon into a 4-byte bloom sketch using xxh3 hashing. False positives possible; false negatives impossible. Used in `CausalLink::horizon_encoded` for approximate causal cone queries by remote observers.

## Entity Logs

`EntityLog` is an append-only log of `CausalEvent`s for a single entity. Chain validation is enforced on every append.

```rust
pub struct EntityLog {
    origin_hash: u32,
    events: Vec<CausalEvent>,
}

impl EntityLog {
    fn append(&mut self, event: CausalEvent) -> Result<(), LogError>  // Validates chain
    fn head(&self) -> Option<&CausalLink>                             // Latest link
    fn range(&self, from: u64, to: u64) -> Vec<CausalEvent>          // Sequence range
    fn after(&self, seq: u64) -> Vec<CausalEvent>                     // Events after seq
}
```

`LogIndex` provides secondary indexing over entity logs.

## State Snapshots

`StateSnapshot` captures a point-in-time state for migration and recovery.

```rust
pub struct StateSnapshot {
    pub entity_id: EntityId,
    pub head_link: CausalLink,
    pub state_bytes: Bytes,        // Opaque daemon state
    pub horizon: ObservedHorizon,
    pub created_at: u64,
}
```

`SnapshotStore` persists snapshots with lookup by entity and sequence.

**Subprotocol ID:** `0x0401` for snapshot transfer.

## Source Files

| File | Purpose |
|------|---------|
| `state/causal.rs` | `CausalLink`, `CausalChainBuilder`, `CausalEvent`, chain validation |
| `state/horizon.rs` | `HorizonEncoder`, `ObservedHorizon`, bloom compression |
| `state/log.rs` | `EntityLog`, `LogIndex`, append-only chain storage |
| `state/snapshot.rs` | `StateSnapshot`, `SnapshotStore` |
