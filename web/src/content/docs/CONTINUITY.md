# Observational Continuity

Each node's truth is what it can observe. The causal chain IS identity. This layer formalizes what the chain provides: continuity proofs, causal cone queries, honest discontinuity with deterministic forking, propagation modeling, and superposition during migration.

## Continuity Proofs

A compact 36-byte proof that an entity's chain is intact over a sequence range, without transferring the full log.

```rust
pub struct ContinuityProof {
    pub origin_hash: u32,     // Entity identity
    pub from_seq: u64,        // Start of proven range
    pub to_seq: u64,          // End of proven range
    pub from_hash: u64,       // parent_hash at from_seq
    pub to_hash: u64,         // parent_hash at to_seq
}
```

**Verification:** A node holding the entity's log can verify the proof by recomputing `parent_hash` for the claimed range and comparing. If the hashes match, the chain is intact between `from_seq` and `to_seq`.

**Continuity status** describes an entity's chain from an observer's perspective:

```rust
pub enum ContinuityStatus {
    Continuous { genesis_hash, head_seq, head_hash },
    Forked { fork_point, original_hash, fork_hash },
    Unverifiable { last_verified_seq, gap_start },
    Migrated { migration_seq, source_node, target_node },
}
```

**Subprotocol IDs:** `0x0700` (continuity), `0x0702` (proof transfer).

## Causal Cones

A `CausalCone` answers: "which events could have causally preceded event E?"

```rust
pub struct CausalCone {
    origin_hash: u32,
    sequence: u64,
    horizon: Option<ObservedHorizon>,   // Exact (local)
    horizon_encoded: u32,               // Approximate (wire)
}

pub enum Causality {
    Definite,    // Exact horizon confirms causal precedence
    Possible,    // Bloom filter match (may be false positive)
    No,          // Definitely no causal relationship
    Unknown,     // Insufficient information
}
```

**Local vs. remote:**
- Local node has the full `ObservedHorizon` -- exact `Definite`/`No` answers
- Remote observers only see the 4-byte `horizon_encoded` bloom sketch -- `Possible`/`No` answers

**Same-entity shortcut:** Events from the same entity are strictly ordered by sequence number -- no horizon check needed.

## Honest Discontinuity

When a chain breaks (node crash, data loss, corruption, conflicting chains), the system creates a new entity with documented lineage via a `ForkRecord`. No silent recovery.

```rust
pub struct Discontinuity {
    pub origin_hash: u32,
    pub last_verified: CausalLink,
    pub failed_link: Option<CausalLink>,
    pub reason: DiscontinuityReason,
    pub detected_at: u64,
}

pub enum DiscontinuityReason {
    NodeCrash { last_snapshot_seq: u64 },
    ChainBreak(ChainError),
    ConflictingChains { seq, hash_a, hash_b },
    Corruption,
}
```

### Fork Records

When discontinuity is detected, `fork_entity()` creates a new entity:

```rust
pub struct ForkRecord {
    pub original_origin: u32,
    pub fork_origin: u32,
    pub fork_point: CausalLink,         // Last verified link in original chain
    pub reason: DiscontinuityReason,
    pub new_keypair_id: EntityId,       // New entity's identity
    pub created_at: u64,
}
```

The forked entity gets a new keypair, a new chain, and a genesis link with a deterministic sentinel `parent_hash` so any node can verify the fork is legitimate. The original chain is preserved -- events are not lost, just forked.

**Subprotocol ID:** `0x0701` for fork announcements.

## Observation Windows

`ObservationWindow` defines a time-bounded window for causal queries.

```rust
pub struct ObservationWindow {
    pub start_time: u64,
    pub end_time: u64,
    pub observed_entities: HashSet<u32>,
}
```

`HorizonDivergence` detects when two nodes' observed horizons differ, identifying which entities each side has observed that the other hasn't. Used by partition healing to scope reconciliation.

## Propagation Modeling

`PropagationModel` estimates event propagation latency based on subnet distance.

```rust
impl PropagationModel {
    fn estimated_latency(&self, from: SubnetId, to: SubnetId) -> Duration
    fn depth_crossing_penalty(&self, depth: u8) -> Duration
}
```

Each level of subnet hierarchy crossed adds latency. The model uses configurable per-level penalties. Used for consistency reasoning: "has this event had time to reach that subnet?"

## Superposition During Migration

During daemon migration, the entity exists in a `SuperpositionState` -- both old and new locations are observable.

```rust
pub enum SuperpositionPhase {
    PreMigration,       // Only at source
    Transferring,       // At source, snapshot in flight
    DualActive,         // Both source and target processing
    PostCutover,        // Only at target
    Settled,            // Migration complete, superposition collapsed
}

pub struct SuperpositionState {
    pub entity_origin: u32,
    pub source_node: u64,
    pub target_node: u64,
    pub phase: SuperpositionPhase,
}
```

Observers see the entity at both locations during `DualActive`. The superposition collapses to `Settled` after cutover completes and the source cleans up. This mirrors the migration state machine from Layer 5 but from the observer's perspective.

## Source Files

| File | Purpose |
|------|---------|
| `continuity/chain.rs` | `ContinuityProof`, `ContinuityStatus`, proof verification |
| `continuity/cone.rs` | `CausalCone`, `Causality`, horizon-based causal queries |
| `continuity/discontinuity.rs` | `Discontinuity`, `ForkRecord`, `fork_entity()` |
| `continuity/observation.rs` | `ObservationWindow`, `HorizonDivergence` |
| `continuity/propagation.rs` | `PropagationModel`, subnet-distance latency |
| `continuity/superposition.rs` | `SuperpositionState`, `SuperpositionPhase` |
