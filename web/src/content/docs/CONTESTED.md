# Contested Environments

Correlated failure handling and partition healing. Detects when mass failures are correlated (subnet outage vs. broad infrastructure failure), tracks partition state, and reconciles divergent entity logs when partitions heal.

## Correlated Failure Detection

`CorrelatedFailureDetector` wraps the base `FailureDetector` with a time-windowed correlation layer. It classifies failures as independent or correlated.

```rust
pub struct CorrelatedFailureConfig {
    pub correlation_window: Duration,              // Default: 2 seconds
    pub mass_failure_threshold: f32,               // Default: 0.30 (30% of nodes)
    pub subnet_correlation_threshold: f32,         // Default: 0.80 (80% in one subnet)
    pub max_concurrent_migrations: usize,          // Default: 3
}
```

**Ratio thresholds, not absolute counts** -- scales with mesh size.

### Classification Algorithm

1. Collect failures within the `correlation_window`
2. If `failed_count / total_tracked < mass_failure_threshold` -> `Independent`
3. Otherwise, classify the root cause:
   - Walk failed nodes' `SubnetId`s up the hierarchy via `parent()`
   - Count failures per subnet ancestor
   - If any ancestor has >= `subnet_correlation_threshold` of failures -> `SubnetFailure`
   - Otherwise -> `BroadOutage`

```rust
pub enum CorrelationVerdict {
    Independent { failed_nodes: Vec<u64> },
    MassFailure {
        failed_nodes: Vec<u64>,
        failure_ratio: f32,
        suspected_cause: FailureCause,
    },
}

pub enum FailureCause {
    SubnetFailure { subnet: SubnetId, affected_ratio: f32 },
    BroadOutage,
    Unknown,
}
```

During mass failure, `recovery_budget()` returns a throttled count (`max_concurrent_migrations`) to prevent recovery storms. Independent failures get unlimited recovery.

## Partition Detection

`PartitionDetector` identifies when a `SubnetFailure` is actually a network partition -- asymmetric visibility where both sides are alive but can't reach each other.

```rust
pub enum PartitionPhase {
    Suspected,
    Confirmed,
    Healing { reappeared: Vec<u64> },
    Healed,
}

pub struct PartitionRecord {
    id: u64,
    our_side: Vec<u64>,
    other_side: Vec<u64>,
    partition_subnet: Option<SubnetId>,
    phase: PartitionPhase,
    our_horizon_at_split: ObservedHorizon,  // Reconciliation baseline
}
```

**Key insight:** Each side independently detects "mass failure in subnet X" and enters partition mode. No coordination needed.

**Healing detection:** When nodes from `other_side` reappear in the `FailureDetector`'s recovery events, the partition transitions to `Healing`. When 50%+ of `other_side` reappears, it transitions to `Healed`.

**Subprotocol ID:** `0x0800` for partition messages.

## Log Reconciliation

After a partition heals, `reconcile_entity()` merges divergent `EntityLog`s from both sides.

```rust
pub enum ReconcileOutcome {
    AlreadyConverged,
    Catchup {
        origin_hash: u32,
        missing_events: Vec<CausalEvent>,
        behind_side: Side,
    },
    Conflict {
        origin_hash: u32,
        diverge_seq: u64,
        resolution: ConflictResolution,
    },
}

pub enum Side { Ours, Theirs }
```

### Reconciliation Algorithm

Uses the `our_horizon_at_split` from the `PartitionRecord` as the baseline (`split_seq`):

1. Collect events after `split_seq` from both sides
2. **Both empty** -> `AlreadyConverged`
3. **One side empty** -> `Catchup` (send missing events to the behind side)
4. **Both have events** -> check for divergence:
   - Walk both chains from `split_seq`
   - If chains are identical -> `AlreadyConverged`
   - If one is a prefix of the other -> `Catchup`
   - If chains diverge at some sequence -> `Conflict`

### Conflict Resolution

Deterministic, coordination-free resolution:

```rust
pub enum ConflictResolution {
    Winner {
        winning_side: Side,
        fork_record: ForkRecord,
    },
}
```

1. **Longest chain wins** (more active during partition)
2. **Equal length:** lower `parent_hash` wins (deterministic, both sides agree independently)
3. Losing chain becomes a `ForkRecord` via `fork_entity()` from Layer 7
4. Events are not lost -- the losing chain is preserved as a fork with documented lineage

Both sides reach the same conclusion independently. No coordination protocol needed.

**Subprotocol ID:** `0x0801` for reconciliation messages.

## Source Files

| File | Purpose |
|------|---------|
| `contested/correlation.rs` | `CorrelatedFailureDetector`, `CorrelationVerdict`, `FailureCause` |
| `contested/partition.rs` | `PartitionDetector`, `PartitionRecord`, `PartitionPhase` |
| `contested/reconcile.rs` | `reconcile_entity()`, `ReconcileOutcome`, `ConflictResolution` |
