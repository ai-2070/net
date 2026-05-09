# Mesh-wide Scheduler — implementation plan

> Continuous federated rebalancing on top of the Warriors-shipped `PlacementFilter`. Where `PlacementFilter` scores a candidate node *at placement decision time* (initial placement, replica election, daemon migration trigger), the mesh scheduler **continuously re-evaluates existing placements** as capability tags drift, capacity changes, drain signals fire, or workload patterns shift — and triggers migrations when a daemon, replica, or group member would score significantly higher elsewhere. Companion to [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) (whose `PlacementFilter` it composes against) and [`MESHDB_PLAN.md`](MESHDB_PLAN.md) (whose aggregates the scheduler uses for cluster-wide load awareness). **Atomic Playboys release** per [`RELEASE_ROADMAP.md`](RELEASE_ROADMAP.md); not in The Warriors or Rebel Yell.

## Status

Design only. Activation gate: a workload that genuinely benefits from continuous rebalancing — fluctuating operator capacity, ongoing drain operations from the negative-point-migration extension, geographic demand-pattern shifts, or multi-region failover scenarios.

This phase only makes sense after both `PlacementFilter` (Warriors) and Distributed RedEX (Warriors) have shipped and accumulated production telemetry. Without real placement-score data from operating workloads, the scheduler's thresholds + cost model are guesses; with it, they're calibrated. **Do not start without that telemetry baseline.**

## Frame

`PlacementFilter` makes initial placement decisions correctly. The mesh scheduler keeps them correct *over time*.

The substrate's reality after Warriors:

- Daemons are placed at scope ∩ proximity ∩ intent ∩ colocation ∩ resource scoring time.
- Capability tags drift: nodes gain GPUs, lose RAM, change scope, advertise new intents, install new software, age out of bloom filters.
- Resource availability fluctuates: free storage shrinks, compute saturates, network paths degrade, latency to anchors changes.
- Drain signals fire: a factory floor shuts down, a ship leaves port, a zone becomes compromised, a server enters maintenance.
- Workload patterns shift: the data a daemon used to need is now elsewhere; the chain it was colocated with moved.

Without continuous re-evaluation, the substrate's initial placement decisions calcify. A daemon placed correctly at year 1 is placed sub-optimally by year 2 because the world around it has changed. The scheduler is the feedback loop that keeps placements aligned with current reality — **the same `PlacementFilter` primitive applied recursively over time, not just at decision moments.**

The architectural posture: **decentralized scheduling with no central coordinator.** Every node runs its own local scheduler, scoring its own daemons (and replicas it leads). Migration decisions are local; only a daemon's home node decides whether to migrate it. The capability index propagates state; the proximity graph weights candidates; Mikoshi executes the migration. **Same primitives, applied as a feedback loop.**

## Why this exists

Three reasons this needs to be a written plan:

1. **The cost model is non-trivial.** Migration has overhead (state transfer time, brief unavailability, reliability cost). Triggering migrations too eagerly produces thrashing; too laxly produces stale placements. Getting this wrong is expensive (thrash burns CPU + bandwidth; staleness erodes the substrate's value). The cost model deserves serious design before implementation.
2. **Coordination semantics need to be locked.** Multiple nodes can simultaneously observe that daemon D would score higher elsewhere — only the *home* node should act. Replicated daemons (replica/standby/fork groups) need consistent rebalancing decisions across the group's members. Without explicit coordination semantics, the scheduler races itself.
3. **Drain integration is operational reality.** The negative-point migration extension (parked in `CAPABILITY_SYSTEM_PLAN.md`) feeds the scheduler. When a zone enters drain mode, the scheduler is the layer that actually executes the evacuation. The plan needs to wire that integration cleanly, not bolt it on.

## What ships

Six interlocking pieces, in dependency order:

1. **Per-daemon score tracker** — every node continuously computes the `PlacementFilter` score of each daemon it currently runs (and each replica it leads). Time-series of scores per daemon. Reveals score drift.
2. **Re-evaluation engine** — for each daemon whose current-placement score has degraded below threshold, query the capability index for higher-scoring alternative candidates. Compute migration cost. Compute net benefit.
3. **Migration trigger** — a daemon migrates when (current_score < threshold) AND (best_alternative_score - current_score > hysteresis_gap) AND (estimated_benefit > migration_cost) AND (cooldown_elapsed). Migration delegates to Mikoshi.
4. **Drain-signal integration** — `metadata.evacuate`, `metadata.drain_deadline` on chains and `metadata.scope.X.draining` on scope tags trigger urgent (deadline-aware) re-evaluation rather than threshold-based.
5. **Replica/group rebalancing** — same logic applied to replica/standby/fork group members. Group leadership re-evaluates membership periodically; under-scoring members get replaced.
6. **Application-policy hooks** — per-daemon migration policy (Eager / Lazy / Pinned), custom score thresholds per scope/intent, manual override + pinning.

What this doc does NOT ship (deferred):

- **Centralized scheduler.** The substrate's design rules out a central coordinator. The mesh scheduler is per-node, with capability propagation providing eventual mesh-wide convergence. A centralized coordinator would also be a single point of failure / capture / liability — explicitly anti-substrate-design.
- **Predictive scheduling.** The scheduler is reactive: it observes score drift and responds. It does not predict future capacity needs (autoscaling, etc.). Predictive scheduling is a research-grade extension; defer.
- **Cost-aware spot-pricing optimization.** A daemon migrating to a cheaper operator based on real-time price changes is conceptually clean but operationally complex (price oracles, cost-vs-latency trade-offs, contract semantics).
- **Cross-region failover playbooks.** Geographic-DR scenarios benefit from explicit operational playbooks (which scope tags failover to which, in what order, with what RTO). Build the primitive (`PlacementFilter`-driven re-evaluation) here; build the playbook layer in a separate follow-up.
- **ML-based placement.** "Train a model on past placement decisions and apply it" is research-grade. The substrate's first-principles `PlacementFilter` should be sufficient for foreseeable workloads.

---

## Design

### 1. Per-daemon score tracker

Each node runs a `LocalScheduler` daemon (one per node, not per artifact). It tracks:

- Every running daemon on this node (it's their home node)
- Every replicated channel this node leads
- For each, the current `PlacementFilter` score against the local node
- A short history of scores (last 60 minutes, ~one sample per heartbeat)
- A flag for whether the score has crossed below threshold

```rust
pub struct LocalScheduler {
    /// Tracked daemons: those running locally + replicas this node leads.
    artifacts: HashMap<ArtifactId, ScoreHistory>,
    /// Score thresholds (per-scope or per-intent overrides; falls back to default).
    thresholds: ThresholdRegistry,
    /// Cooldown registry: when did we last migrate each artifact?
    cooldowns: HashMap<ArtifactId, Instant>,
    /// Migration cost model.
    cost_model: MigrationCostModel,
    /// PlacementFilter (typically StandardPlacement) used for scoring.
    placement_filter: Arc<dyn PlacementFilter>,
}

pub struct ScoreHistory {
    pub recent: VecDeque<(Instant, f32)>,    // last N samples
    pub current: f32,
    pub trend: Trend,                        // Stable | Degrading | Improving
}
```

**Sampling cadence.** Once per heartbeat (default 500ms via `MeshConfig::heartbeat_ms` from `REDEX_DISTRIBUTED_PLAN.md`). Score computation is local + cheap (the 5-axis evaluation is sub-millisecond per the Capability System plan). Keeping a 60-minute rolling window at heartbeat cadence is ~7200 samples × ~16 bytes = ~115 KB per artifact; bounded.

**Threshold defaults.** A daemon is "below threshold" when its current placement score < 0.5 (out of [0, 1]). Configurable per scope/intent. Some workloads tolerate lower scores (e.g., a low-priority background daemon at 0.3 is fine); others demand high scores (e.g., latency-critical daemons should not run below 0.8).

### 2. Re-evaluation engine

For each artifact whose `current_score < threshold` AND `cooldown_elapsed`:

1. Query the capability index for candidate nodes that satisfy the artifact's required capabilities.
2. For each candidate, compute its `placement_score(candidate, artifact)`.
3. Pick the highest-scoring alternative; call its score `best_alt_score`.
4. Compute `migration_cost(artifact, current_node, best_candidate)` via the cost model.
5. Compute `net_benefit = (best_alt_score - current_score) * service_value - migration_cost`.
6. If `net_benefit > 0` AND `(best_alt_score - current_score) > hysteresis_gap` (default 0.2): trigger migration.
7. Otherwise: keep current placement; record decision in history; defer next re-evaluation by cooldown.

**Migration cost model.** Components:

```rust
pub struct MigrationCost {
    /// State-transfer cost: bytes_to_transfer / bandwidth + serialization overhead.
    pub state_transfer: Duration,
    /// Disruption cost: estimated time the daemon is unavailable during migration.
    pub disruption: Duration,
    /// Network cost: bandwidth consumed during transfer (relevant under saturation).
    pub bandwidth_bytes: u64,
    /// Reliability cost: weighted by daemon's importance.
    pub reliability_factor: f32,
}
```

Migration cost is converted to a "score-equivalent" via a configurable cost-per-second-disruption parameter. A daemon whose `service_value` is high (high-importance, latency-sensitive) needs a bigger score gap to justify migration than a background daemon.

**Hysteresis gap.** Default 0.2 (so a daemon at 0.5 won't migrate to a 0.6-scoring candidate; it would need 0.7+ to trigger). This avoids thrash where small score fluctuations cause continuous migration.

**Cooldown window.** Default 5 minutes after a migration. A daemon that just migrated doesn't get re-evaluated immediately; it gets a settling window. Avoids thrash where a daemon migrates A→B and then immediately B→A as scoring drifts back.

### 3. Migration trigger and Mikoshi integration

When the re-evaluation engine decides to migrate, it calls into Mikoshi:

```rust
impl LocalScheduler {
    async fn execute_migration(&self, artifact: &Artifact, target: NodeId) -> Result<()> {
        // 1. Notify the daemon (graceful drain hook if registered)
        if let Some(daemon) = self.lookup_daemon(artifact) {
            daemon.on_pre_migrate(target).await?;
        }
        // 2. Trigger Mikoshi snapshot + transfer
        self.mikoshi.migrate(artifact, target).await?;
        // 3. Update local state: artifact no longer ours
        self.artifacts.remove(&artifact.id);
        // 4. Record cooldown timestamp on target node (propagated via heartbeat)
        self.cooldowns.insert(artifact.id, Instant::now());
        // 5. Withdraw any capability tags that pointed to us for this artifact
        self.mesh.withdraw_chain(...).await?;
        Ok(())
    }
}
```

Mikoshi handles the actual mechanics. The scheduler just decides *when* and *to where*. The integration is:

- Scheduler → Mikoshi: "migrate artifact X to node Y"
- Mikoshi: snapshot, transfer, restore, cutover (existing primitives)
- Scheduler ↔ capability index: tag advertisements update so subsequent reads route to the new home

**Coordination concern.** Only the *current home node* of a daemon decides to migrate it. Other nodes might score the same daemon and conclude "I'd be a better home"; that's irrelevant. They don't act on it. This avoids the multi-node race that would otherwise require consensus.

For replicated channels, the *leader node* decides replica re-placement (which is itself one of its replication-coordinator's responsibilities; the scheduler just feeds in the score signal).

### 4. Drain-signal integration

The negative-point migration extension (parked in `CAPABILITY_SYSTEM_PLAN.md`) defines:

- `metadata.scope.X.draining = "true"` + `metadata.scope.X.drain_deadline` on a scope tag
- `metadata.evacuate = "true"` + `metadata.evacuate_deadline` on a daemon's metadata

The scheduler integrates these as **forcing functions** that override the standard threshold-based logic:

```rust
impl LocalScheduler {
    fn check_drain_signals(&mut self) {
        for artifact in self.artifacts.values() {
            // 1. Is the artifact's metadata.evacuate flag set?
            if let Some(deadline) = artifact.evacuate_deadline() {
                self.urgent_migrations.push((artifact.id, deadline));
            }
            // 2. Is the artifact's home zone draining?
            if let Some(deadline) = self.local_zone_drain_deadline() {
                self.urgent_migrations.push((artifact.id, deadline));
            }
        }
    }
}
```

Urgent migrations bypass the cooldown and the hysteresis-gap requirement (they're not optimizing for score; they're meeting a deadline). They're rate-limited to avoid melting the source zone during a mass evacuation — N migrations per second per zone (configurable).

Migrations triggered by drain signals report progress via metrics + per-deadline tracking:

- `mesh_scheduler_drain_in_flight{zone}` (gauge — how many drain-triggered migrations are currently executing)
- `mesh_scheduler_drain_completed_total{zone}` (counter — successful drains)
- `mesh_scheduler_drain_deadline_missed_total{zone}` (counter — drains that didn't complete by deadline)

The third metric is the operationally important one. A factory shutdown that misses its drain deadline means daemons running on infrastructure that's about to power off — a real operational concern.

### 5. Replica / group rebalancing

The same scheduler logic applies to:

- **Replicas of a `Distributed RedEX` channel.** The leader's local scheduler scores each replica's placement (uses `Artifact::Replica` variant of `PlacementFilter`). Under-scoring replicas trigger re-election: leader drops the under-scoring node, picks a higher-scoring candidate via the capability index, transfers the replica state.
- **Members of a daemon replica group.** The group's coordinator scores members; under-scoring members get replaced.
- **Standby group members.** Same — under-scoring standbys get replaced before they're needed.
- **Fork group lineage.** Forks are usually fixed (they have semantic meaning), but the *placement* of each fork's daemon is rescheduled normally.

The integration is the same primitive (`PlacementFilter`) applied to the same artifact types defined in the Capability System plan. The scheduler generalizes across artifact kinds via the `Artifact<'a>` enum:

```rust
match artifact {
    Artifact::Chain { .. } => self.score_chain_placement(...),
    Artifact::Replica { .. } => self.score_replica_placement(...),
    Artifact::Daemon { .. } => self.score_daemon_placement(...),
}
```

Each variant produces a score; the same threshold/hysteresis/cooldown logic applies to all of them.

### 6. Application-policy hooks

Per-artifact migration policy:

```rust
pub enum MigrationPolicy {
    /// Migrate eagerly when a better placement is available.
    Eager { score_threshold: f32, hysteresis_gap: f32 },

    /// Migrate only when score falls catastrophically (e.g., < 0.2).
    /// Default for cost-sensitive workloads.
    Lazy { critical_threshold: f32 },

    /// Pinned to current node. Never migrate via scheduler. Mikoshi-only via direct call.
    Pinned,

    /// Custom: caller provides a function that decides per-artifact whether to migrate.
    Custom(Arc<dyn Fn(&ScoreHistory, &Vec<Candidate>) -> Option<NodeId> + Send + Sync>),
}
```

Policy lives in `metadata.migration_policy`. Default is `Lazy { critical_threshold: 0.3 }` for daemons (avoid eager rescheduling); `Eager { score_threshold: 0.5, hysteresis_gap: 0.2 }` for replicas (durability matters); `Pinned` for any artifact whose owner explicitly disables auto-migration.

Manual pinning + override via SDK:

```rust
mesh.scheduler().pin(daemon_id).await?;       // disable scheduler-driven migration
mesh.scheduler().unpin(daemon_id).await?;     // re-enable
mesh.scheduler().force_migrate(daemon_id, target).await?;   // manual trigger, ignores policy
```

### 7. Aggregate cluster-wide load awareness

The local scheduler decides locally, but knowing the cluster-wide picture helps. The MeshDB aggregate operator (`MESHDB_PLAN.md`) provides this:

- "What's the average daemon-placement score across all operators in the marketplace?" — a cluster-wide quality metric.
- "Which scope:X has the most under-scoring daemons?" — surfaces hot zones needing operator attention.
- "What's the daemon migration rate in the last hour?" — surfaces potential thrash.

These don't drive scheduling decisions directly (those stay local), but they expose the cluster's scheduling state to operators for dashboards + alerting. The scheduler emits per-node metrics; MeshDB aggregates across nodes.

### 8. Failure modes

**Migration fails partway.** Mikoshi handles this — snapshot taken; transfer fails; daemon resumes on source node; scheduler records the failed migration + extends cooldown. The state isn't corrupted; the migration is just rolled back.

**Cascading migrations.** A node fails; all its daemons need to migrate; the scheduler triggers them all. Without rate limiting, this melts adjacent nodes with state-transfer load. Mitigation: per-source-node migration rate limits (default: max N concurrent outbound migrations).

**Score-drift oscillation.** Two nodes A and B repeatedly trade a daemon back and forth as their scores fluctuate. Hysteresis + cooldown prevent this in steady state, but pathological cases exist. Mitigation: detect oscillation (3+ migrations of same daemon in 30 min) and pin the daemon temporarily; alert operator.

**Drain deadline missed.** Reported via metric. The scheduler can't always meet drain deadlines (e.g., if every potential target is also draining). Operator playbook: alert, manual intervention, force-pin daemons that can't migrate so they survive the source-zone shutdown.

---

## Phasing

Seven phases, in dependency order. Each is gated by activation of the workload that requires it; do not ship speculatively past phase A.

### Phase A — Per-daemon score tracker (1-2 weeks)

- `LocalScheduler` daemon, one per node.
- Score sampling at heartbeat cadence; bounded history per artifact.
- `PlacementFilter` integration; cost-free local scoring.
- Metrics: `mesh_scheduler_artifact_score{artifact_id}`, score histograms per scope/intent.
- Tests: score history accumulates correctly; thresholds trigger correctly.

### Phase B — Re-evaluation engine (2 weeks)

- Threshold-driven re-evaluation: scan local artifacts, identify under-scoring ones.
- Capability-index queries for alternative candidates.
- Migration cost model (basic: state-size + bandwidth-time estimate).
- Hysteresis-gap and cooldown logic.
- Tests: re-evaluation triggers when expected; doesn't trigger when not; cooldowns enforced.

### Phase C — Migration trigger + Mikoshi integration (1-2 weeks)

- Scheduler → Mikoshi handoff via `Mikoshi::migrate(artifact, target)`.
- Migration result tracking (success / failure / partial-and-rolled-back).
- Capability-index tag updates after migration completes.
- Metrics: `mesh_scheduler_migrations_total{outcome}`, migration latency histograms.
- Integration tests: full end-to-end (score drops → re-evaluation → Mikoshi migration → tags update → reads route correctly).

### Phase D — Drain-signal integration (1 week)

- Read `metadata.scope.X.draining` and `metadata.evacuate` from capability tags.
- Urgent-migration path that bypasses cooldown + hysteresis.
- Per-zone migration rate limits.
- Drain-progress metrics.
- Tests: drain signal triggers urgent migrations; deadlines tracked; rate limits enforced.

### Phase E — Replica + group rebalancing (2 weeks)

- Same scheduler logic applied to `Artifact::Replica` and group-member placements.
- Integration with `Distributed RedEX` replica election.
- Integration with replica/standby/fork group coordinators.
- Tests: under-scoring replica triggers re-election; group member replacement.

### Phase F — Application-policy hooks (1-2 weeks)

- `MigrationPolicy` enum + per-artifact policy registry.
- SDK methods for pin/unpin/force-migrate.
- Custom-policy callbacks (cross-binding via callback-FFI pattern, same as `BlobAdapter`).
- Tests: policy enforcement; manual override; oscillation detection + auto-pin.

### Phase G — Observability + bindings (1 week)

- Cluster-wide scheduling metrics surfaced via MeshDB aggregates.
- SDK bindings: scheduler control, policy registration, metrics access.
- Operator-facing documentation: when to expect migrations, how to read metrics, when to intervene.

**Total: 9-12 focused weeks parallelised; 14-18 single-engineer.**

Phases C, D, E can partially overlap if separate engineers work on them. Phase F is mostly mechanical (cross-binding work), parallel-shippable.

---

## Test strategy

### Unit

- **Score sampling correctness.** Synthetic capability tag changes produce expected score changes; history accumulates correctly; trend detection works.
- **Threshold logic.** Under-threshold detection fires only when score < threshold AND cooldown elapsed.
- **Hysteresis.** Score gaps below hysteresis_gap don't trigger migration; gaps above do.
- **Migration cost model.** Cost estimates monotonic in expected dimensions (state size, network distance, daemon importance).
- **Policy enforcement.** Pinned daemons never migrate via scheduler; eager daemons migrate at threshold; lazy daemons only migrate at critical threshold.

### Integration

- **3-node mesh score evolution.** Daemon placed at score 0.8; node loses GPU (capability tag changes); score drops to 0.4; scheduler triggers migration; daemon lands on a still-GPU-having node; new score 0.85.
- **Drain trigger end-to-end.** Mark `scope:factory-floor-A.draining = "true"` with deadline T+30s; assert all daemons in scope evacuate within deadline; metrics confirm drains completed.
- **Cascading migrations.** Kill a node hosting 50 daemons; assert migrations trigger; assert per-source rate limits prevent melting adjacent nodes; assert eventual convergence.
- **Replica rebalancing.** Replica falls below score threshold; leader triggers re-election; replica state transferred to new node; channel availability uninterrupted.
- **Policy override.** Pin a daemon during active rebalancing; assert it doesn't migrate even when its score drops below threshold.
- **Oscillation detection.** Synthesize alternating capability-tag patterns that would cause score thrash; assert oscillation detection fires and auto-pins after 3 migrations in 30 min.

### Property

- **Convergence.** Under any sequence of capability-tag updates, the scheduler eventually converges to a placement state where no migration would yield net benefit (assuming no continuous tag oscillation).
- **No worse than initial.** A daemon's post-migration score is always ≥ pre-migration score - migration_cost (by construction of the trigger condition).
- **Cooldown invariant.** A daemon never migrates more than (1 / cooldown_duration) times per unit time, except via drain-signal urgent path.

### Performance

- **Score sampling overhead.** < 100 μs per artifact per heartbeat. At 1000 artifacts on a node, < 10% of CPU per second on heartbeat work.
- **Re-evaluation cost.** Triggered re-evaluation completes in < 50 ms (capability-index query + scoring); should not noticeably affect application throughput on the node.
- **Migration throughput.** A node should sustain N concurrent outbound migrations (default N=4) at full bandwidth; per-migration throughput limited by daemon state size + network bandwidth.
- **Drain deadline adherence.** For zones with up to 100 daemons, drain completes within deadline ≥ 30s for 99% of test runs.

### DST (deterministic simulation)

- **Race conditions in coordination.** Multiple nodes simultaneously detect that daemon D's score is sub-optimal; only the home node acts; no double-migration.
- **Mikoshi failure recovery.** Migration trigger → Mikoshi snapshot → network partition → migration aborts → daemon resumes on source; scheduler records failure + extends cooldown; eventually retries.
- **Capability-tag staleness.** A node decides to migrate based on a stale capability-tag advertisement (the candidate node is no longer a good fit); the migration completes anyway; scheduler re-evaluates immediately and either keeps or moves again. No data loss; just brief sub-optimality.
- **Drain race.** Drain signal fires while a daemon is mid-migration; migration completes to new home (which is in a *different* zone, so it correctly evacuates the original zone); deadlines tracked.

---

## Open design questions to lock before implementation

1. **Scoring sample cadence.** Every heartbeat (500ms default) or coarser (e.g., every 5s)? **Recommendation:** every heartbeat for the rolling window; threshold-check cadence can be coarser (every 30s of low-priority check) to reduce CPU. Tunable per-node.

2. **Migration cost model parameters.** What's "service_value" of a daemon? **Recommendation:** default 1.0 for application daemons; configurable per-daemon via `metadata.service_value` (high for trading engines, low for background batch daemons). Migration_cost is dominated by state_size + bandwidth_time; rest is approximated.

3. **Per-source migration rate limit.** Max N concurrent outbound migrations from a single node. Too low = drain doesn't complete in time; too high = melts the source. **Recommendation:** default 4 concurrent (covers most operational scenarios); operators can override under drain. Pin in tests.

4. **Replica rebalancing aggressiveness.** Replicas' availability is more important than daemons'. **Recommendation:** replicas use Eager policy by default (lower threshold gap); replicas-of-critical-channels use Eager with even lower gap. Application-level policy can override.

5. **Oscillation detection window.** 3 migrations in 30 minutes triggers auto-pin. Right? **Recommendation:** start with 3-in-30-min; calibrate after first production deployment; expose tunable.

6. **Drain rate-limiting per-zone.** Drain operations need rate limiting both per-source-node (to not melt) and per-zone (to not flood targets). **Recommendation:** zone-wide rate limit = sum of node-level rate limits; explicit override per drain-signal config.

7. **Custom `PlacementFilter` impls.** Application-defined filters are supported via the trait. Do we sandbox them? **Recommendation:** they run in-process; no sandbox. Buggy filters are an application concern. Document explicitly.

---

## Risks

- **Thrash under capability-tag churn.** Tag changes that propagate slowly across the mesh look like score oscillation; daemons migrate, then capability changes again, daemons migrate back. **Mitigation:** hysteresis + cooldown + oscillation detection; rate limits on tag churn (Capability System's existing throttle).
- **Migration storms during drain.** A large zone draining triggers thousands of migrations; targets get melted. **Mitigation:** per-zone + per-target rate limits; explicit operator dashboard for drain progress; manual intervention when needed.
- **Fairness across operators.** In the marketplace, scheduler decisions affect operator earnings (a daemon migrating to operator B means operator A's earnings stop). **Mitigation:** scheduler operates on technical merit; commercial fairness is a marketplace-policy layer above the scheduler. Explicit documentation that scheduler doesn't optimize for operator fairness; marketplace does.
- **Scheduler-induced single point of failure.** If the local scheduler crashes, no rebalancing happens for that node. **Mitigation:** the scheduler is just a daemon; it auto-restarts via Mikoshi standby groups; missed re-evaluations are recovered when it resumes.
- **Cost-model miscalibration.** Production migrations might be cheaper or costlier than the model estimates. **Mitigation:** report observed migration costs; operators tune model parameters based on telemetry; ship with conservative defaults.
- **Application-policy footguns.** Custom policy that always says "migrate now" produces thrash. **Mitigation:** rate limits enforced *outside* the policy; cooldowns enforced; oscillation detection triggers auto-pin even with custom policies. Document clearly.

---

## Effort

**9-12 focused weeks parallelised across 1-2 engineers; 14-18 single-engineer.**

- ~2500 LoC core (LocalScheduler + score tracker + re-evaluation engine + migration trigger + cost model + drain integration + replica rebalancing + policy hooks)
- ~3500 LoC tests (unit + integration + property + performance + DST)
- ~1 week bindings (parallelisable across four bindings)
- ~3 days documentation (operator guide, policy reference, troubleshooting)

This is medium-complexity. Most of the building blocks exist (`PlacementFilter`, Mikoshi, capability index); the work is composing them into a feedback loop with proper hysteresis + cost modeling + drain integration. Less research-grade than MeshDB; more ops-discipline than novel design.

---

## Activation gate

A workload that genuinely benefits from continuous rebalancing. Realistic triggers:

- **Marketplace operator capacity fluctuation.** Operators in the compute marketplace see variable demand; daemons should redistribute as some operators get busy and others have free capacity. Without the scheduler, daemons stay where initially placed even as the load distribution shifts.
- **Spot-instance / cost-aware migration.**
- **Industrial drain operations.** Factory shutdowns, ship departures, vehicle deliveries — the drain-signal extension from `CAPABILITY_SYSTEM_PLAN.md` requires the scheduler to actually execute the evacuation. Once industrial-telemetry pilots activate, the scheduler is the layer that handles their planned-drain semantics.
- **Multi-region failover scenarios.** When a region goes down (network partition, DC failure, geographic event), the scheduler is the layer that re-homes the affected daemons to other regions. Activates with multi-region deployments.
- **Mikoshi v2 maturation.** Once Mikoshi gains delta-based migration (lower migration cost), continuous rebalancing becomes economically viable for more workload types. Activates together with Mikoshi v2 advances.

When any of these activate, ship Phase A; phases B-G follow as needed by the specific workload.

---

## Cross-cutting: relationship to other plans

**Builds on:**

- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — the entire `PlacementFilter` primitive, capability index, and metadata field. The scheduler is `PlacementFilter` applied as a feedback loop. Without the Capability System primitives, this plan has no foundation.
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — replica placement uses the same scheduler logic. Replication-coordinator's anti-affinity term composes with the scheduler's hysteresis.
- [`MESHDB_PLAN.md`](MESHDB_PLAN.md) — federated query primitives surface cluster-wide scheduling state to operators. Aggregates over per-node metrics produce cluster-wide dashboards.
- **Mikoshi** — existing primitive; the scheduler delegates execution to it.

**Consumes (potentially):**

- The negative-point migration (drain-signal) extension from Capability System Parked Extensions. The scheduler is the layer that actually executes drain-triggered migrations.
- `metadata.intent`, `metadata.colocate-with`, `metadata.scope` for placement scoring (via `StandardPlacement`).

**Replaces or extends:**

- Mikoshi's *placement* logic (current: ad-hoc, single-node-decision). After this plan: continuous, score-driven, hysteresis-bounded. Mikoshi keeps its migration mechanics; the scheduler just decides when/where.
- Replica/standby/fork group's *member-placement* logic. After this plan: scheduler-driven with consistent thresholds across all artifact types.

**Related but separate:**

- **Predictive scheduling** — explicitly out of scope. A model that predicts future load and pre-migrates is research-grade; defer.
- **Cost-aware spot pricing** — separate plan when marketplace pricing surface matures.
- **Geographic failover playbooks** — separate plan; this plan ships the primitive; the playbook layer composes against it.

---

## See also

- [`REDEX_PLAN.md`](REDEX_PLAN.md) — single-node v1 substrate
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — Phase 2 of The Warriors; provides replica placement that this scheduler rebalances
- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — Warriors phase; provides the `PlacementFilter` primitive that this scheduler applies recursively
- [`MESHDB_PLAN.md`](MESHDB_PLAN.md) — federated query layer; surfaces cluster-wide scheduling state
- [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) — local fold layer; daemon state migration relies on its snapshot/restore primitives
- [`../misc/DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md) — original deferral context: continuous rebalancing was named as Atomic Playboys candidate
- `RELEASE_ROADMAP.md` — Atomic Playboys release context
