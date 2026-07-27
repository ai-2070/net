# Mesh Scheduler — drift-scorer implementation plan

> Turns the design in [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) into shipped code, and
> kills the per-tick polling wall in the already-shipped Phase D-1 arm
> (`diff_scheduler`). Implements the design doc's `LocalScheduler` / `ScoreHistory` /
> `MigrationCost` onto the **as-built** MeshOS reconcile architecture rather than the
> design doc's notional standalone daemon. Coexists with — does not touch — the running
> dispatch bridge in [`MESHOS_SCHEDULER_INTEGRATION_PLAN.md`](MESHOS_SCHEDULER_INTEGRATION_PLAN.md);
> migration execution flows through its type-enforced `migrate(MigrationEligible, NodeId)` veto.

## Status

Implementation plan. Phase 0 is shippable immediately (no behavior change). Phases 1–3 are
the drift-scorer build-out; Phase 4 generalizes to replicas/groups.

## What already exists (do not rebuild)

- **Phase D-1 arm** — `diff_scheduler` (`reconcile.rs:460`), a *pure* function over `MeshOsState`,
  emits `RequestEviction` for the worst sub-floor holder when a better alternative clears the
  hysteresis gap and the chain is off cooldown. Two-stage: evict → Phase C refills.
- **`PlacementScorer` trait** (`scheduler.rs:54`): `score(chain, node) -> Option<f32>` +
  `best_alternative(chain, exclude) -> Option<(NodeId, f32)>`. Consumer-installed via
  `SchedulerRegistry` (`scheduler.rs:112`); `src` ships only the test `FixedScorer`.
- **`SchedulerConfig`** (`scheduler.rs:79`): `score_floor` 0.5, `hysteresis_gap` 0.2, `cooldown` 5 min.
- **Cooldown state** lives in `MeshOsState::last_rebalance` (`state.rs:69`), written back by the
  loop in `run_reconcile` (`event_loop.rs:1548-1552`) — *not* inside the fold.
- **Migration bridge** — `MigrationEligible::check(daemon, claims)` → `migrate(eligible, target)
  -> MigrationPlan` (`scheduler_bridge/migration.rs`). Claim-holders can't migrate (type veto).
- **Dirty signal source** — capability-fold entries carry `generation: u64` (`fold/capability.rs`),
  bumped when a node's tags change. Replay-safe.

## Locked architecture decisions

**LD-1 — `ScoreHistory` is loop-side observational state, never in the fold.** Score values come
from live `PlacementFilter` evaluation (RTT, inventory, caps), which are local and non-replicated.
They join `last_rebalance` / `rtt` / `inventory` as loop-maintained state. The fold and the
`reconcile` decision stay replay-deterministic (honors I3: no `Instant::now()` in the fold;
everything anchors on `last_tick`).

**LD-2 — split *sampling* from *decision* (the keystone).** Today `diff_scheduler` calls
`scorer.score()` for every holder of every led chain *inside* the pure reconcile pass, every tick.
We move sampling **out** to the loop: the loop samples into `ScoreHistory`, then passes a
precomputed **score snapshot** into `diff_scheduler`, which becomes a pure decision over given
scores. This is what makes both cadence control and dirty-gating clean — the *loop* owns "when to
sample," reconcile owns "what to do with the scores." `best_alternative` (the expensive
capability-index query) stays gated behind a sub-floor victim, exactly as today.

**LD-3 — dirty = cheap counter/delta compares, never a re-scan.** A chain is dirty when, since its
last sample: (a) any holder's capability-fold `generation` moved, (b) the holder set changed, or
(c) a holder's `rtt`/`inventory` moved beyond an epsilon. All O(holders) counter compares — no
scoring, no index query.

**LD-4 — `MigrationCost` gates the decision; output flows through the existing typed bridge.** Cost
slots into `diff_scheduler` between the hysteresis check and the emit:
`net_benefit = (best_alt - worst) * service_value - cost_score`; emit only if `net_benefit > 0`.
Execution stays two-stage (`RequestEviction` → Phase C refill) for daemons-as-chains; the
claim-veto (`MigrationEligible::check`) is consulted before any eviction of a claim-pinned daemon.

**LD-5 — reuse `ChainId` identity for the meshos arm.** The design doc's generic `ArtifactId` maps
onto existing identity (`origin_hash` / `daemon_id` / `channel` / `blob_hash`, per `Artifact<'a>`
in `placement.rs:57`). Don't introduce a new `ArtifactId` type until Phase 4 needs the replica/group
generalization.

---

## Phase 0 — `diff_scheduler` cleanups (ship now, zero behavior change)

Independent of the rest; pure performance + readability. In `diff_scheduler` (`reconcile.rs:474-530`):

1. **Stop alloc+sorting the whole chain set every tick.** Replace
   `chains = replicas.keys().copied().collect(); chains.sort()` — which also sorts chains this node
   doesn't lead — with iteration over `actual.replicas.iter()` in native order, accumulating the
   rare `(chain, victim)` candidates into a small `Vec`, then `sort()` *that* (k≈0–1) before pushing
   `RequestEviction`. Same byte-stable output; O(C·log C) sort → O(k·log k).
2. **Drop the redundant lookup.** Iterating `replicas.iter()` yields the `BTreeSet` holders
   directly, removing the second `replicas.get(&chain)` at `reconcile.rs:496`.
3. **Defer the `holders` Vec collect.** It feeds only `best_alternative`'s `&[NodeId]`, reached only
   after a sub-floor victim is found. Score holders by iterating the `BTreeSet` directly; collect the
   `Vec` only on the rare candidate path. Saves one alloc per led-chain per tick in the no-op case.

**Tests:** the existing reconcile determinism/idempotence tests (`reconcile.rs` test mod) must pass
unchanged — they pin output ordering, which is the property this refactor preserves.

---

## Phase 1 — sampling/decision split + `LocalScheduler` sidecar + `ScoreHistory`

**`LocalScheduler` sidecar.** A loop-owned struct (sibling of `SchedulerRegistry` on the event-loop
struct, `event_loop.rs:122`), holding the non-replicated observational state:

As-built (the design sketch evolved during implementation):

```rust
pub struct LocalScheduler {
    current: ScoreSnapshot,                   // running snapshot, returned each tick
    history: HashMap<ChainId, ScoreHistory>,  // per led chain
    last_fingerprint: HashMap<ChainId, u64>,  // dirty-bit: last per-chain input fold (Phase 2)
    last_sampled: HashMap<ChainId, Instant>,  // backstop cadence: last sample time (Phase 2)
}
// cost_model (MigrationCostModel) + decision_interval (the cadence) live on
// SchedulerConfig, NOT here — the *decision* arm consumes them, so they ride
// with the other scheduler tunables rather than the loop-side sidecar.

pub struct ScoreHistory {
    recent: VecDeque<(Instant, f32)>,  // bounded ring (HISTORY_CAP = 64)
    current: f32,
    ewma: f32,                         // incremental running mean (trend basis)
    trend: Trend,                      // Stable | Degrading | Improving
}
```

**Sizing note (corrects the design doc).** The design doc budgets ~7200 samples × 1000 artifacts
≈ 115 MB. That's wasteful: trend detection needs only a short window + running aggregates. Cap
`recent` at ~64 samples (a few minutes at heartbeat cadence) and carry an incremental EWMA for
`current`/`trend`. ~1 KB/chain, not 115 KB.

**The split.** In `run_reconcile` (`event_loop.rs:1522`), *before* calling `reconcile`:
1. Build the led-chain set (leader == this_node).
2. For chains due to sample (Phase 2 decides which), call the installed scorer, append to
   `ScoreHistory`, update trend.
3. Produce a `ScoreSnapshot { scores: HashMap<ChainId, HashMap<NodeId, f32>> }` (nested chain →
   holder → score so a dirty chain's scores replace/drop in O(1)) for the chains the decision pass
   will consider this tick.
4. Pass `&ScoreSnapshot` into `reconcile`/`diff_scheduler`; `diff_scheduler` reads scores from the
   snapshot instead of calling `scorer.score()` itself. `best_alternative` stays a scorer call
   inside reconcile (still gated by sub-floor victim).

`diff_scheduler` thus becomes pure over `(MeshOsState, ScoreSnapshot, config)` — still deterministic,
still anchored on `last_tick`.

**Tests:** snapshot-fed `diff_scheduler` reproduces the current arm's decisions for the same scores;
`ScoreHistory` accumulates + bounds correctly; trend transitions (Stable→Degrading→Improving) fire on
synthetic series.

---

## Phase 2 — coarse sub-cadence **and** dirty-bit gating (both; each kills a distinct failure mode)

Both gate *which chains get sampled* in Phase 1 step 2. They are complementary, not redundant:

| Lever | Kills | Failure mode if used alone |
|---|---|---|
| **Coarse sub-cadence** (decision check every `decision_interval`, default 30 s, per design-doc Open Q #1) | the every-500 ms re-score-everything burst | still O(N) bursts every interval; a fast-drifting chain waits up to a full interval |
| **Dirty-bit gating** (sample only chains whose inputs moved, LD-3) | the O(N) steady-state polling wall | a slow sub-epsilon drift or a dirty-tracking bug leaves a chain unscored **indefinitely** (silent staleness) |

**Combined behavior:**
- **Stable mesh:** each tick costs O(led-chains) cheap generation-counter compares (LD-3) and
  *zero* scoring. The full re-score-everything-every-tick is gone.
- **Churny mesh:** only the chains that actually moved get scored, the moment they move (no waiting
  for the interval) — but the per-source/per-zone rate caps (design doc §8) still bound the burst.
- **Backstop:** the coarse `decision_interval` forces an evaluation of every led chain at least once
  per interval *regardless of dirty state* — the correctness net under sub-epsilon drift or a dirty
  bug. Dirty handles the common case cheaply; cadence guarantees eventual re-evaluation.

**Dirty inputs (LD-3), all cheap:**
- capability-fold per-node `generation` (`fold/capability.rs`) vs. `last_sampled_gen`;
- holder-set identity for the chain (`BTreeSet` changed);
- `rtt` / `inventory` (`state.rs:43,55`) delta beyond epsilon for any holder.

**Config (extend `SchedulerConfig`, `#[non_exhaustive]` so it's additive):**
```rust
pub decision_interval: Duration, // default 30s — coarse backstop cadence
pub sample_epsilon_rtt: Duration,// dirty threshold for rtt movement
// history sampling rides the dirty gate; no separate fast cadence needed
```

**Tests:** stable chain → counter compares only, no `score()` calls (assert via a counting mock
scorer); dirty chain → sampled same tick; backstop fires for a chain that never trips dirty;
`decision_interval` boundary respected; determinism preserved (cadence/dirty read `last_tick`,
never wall-clock).

---

## Phase 3 — `MigrationCost` + net-benefit gate

**`MigrationCostModel`** mapping the design doc's struct:
```rust
pub struct MigrationCost {
    pub state_transfer: Duration,   // bytes_to_transfer / bandwidth + serialization
    pub disruption: Duration,       // estimated unavailability window
    pub bandwidth_bytes: u64,
    pub reliability_factor: f32,    // weighted by service_value
}
```
Converted to a score-equivalent via a `cost_per_sec_disruption` knob. Inputs: chain/daemon state
size (from the snapshot/inventory), candidate RTT/bandwidth (`rtt`), `metadata.service_value`
(default 1.0). Keep it *monotonic* in state size, distance, and importance — the property the design
doc's test strategy pins; exact calibration waits on telemetry (per the design doc's activation gate).

**Gate (LD-4)** in `diff_scheduler`, after the hysteresis check, before emit:
```text
net_benefit = (best_alt_score - worst_score) * service_value - cost_score
emit RequestEviction only if net_benefit > 0
```
For a claim-pinned daemon, consult `MigrationEligible::check` first; a held claim vetoes the
eviction (it's invisible to the scorer by construction anyway — Integration plan Decision 2).

**Cost-target approximation.** The cost is estimated for `best_alternative`'s winner, but the
two-stage execution emits an *untargeted* `RequestEviction` → Phase C refill (`RequestPlacement`
with no pinned target), so the daemon may actually land elsewhere. We use the best alternative as the
proxy target because Phase C scores candidates with the same placement logic and lands on a
comparably-good node; `alt_node` is the optimistic (lowest-cost) estimate, making this gate a
best-effort damper rather than exact per-target accounting. Pinning the refill target through the
eviction→placement handshake is deferred to Phase 4.

**Tests:** cost monotonic in each dimension; high-`service_value` daemon needs a bigger gap to move;
net-benefit gate suppresses a marginal migration the hysteresis gap alone would allow; claim-holder
never emits.

---

## Phase 4 — replica / group generalization (deferred; maps `ArtifactId`)

Generalize the same snapshot/decision path to `Artifact::Replica` and group members (design doc §5),
keying `ScoreHistory` by the design-doc `ArtifactId` mapped onto `origin_hash`/`channel`/`daemon_id`.
Wires into Distributed RedEX replica election + the group coordinators. Gated by an actual
replica-rebalancing workload; don't build speculatively.

---

## Determinism & risks

- **Replay determinism (load-bearing).** Sampling is loop-side and anchored on `last_tick`; the fold
  and `reconcile` decision never see wall-clock or live scores except via the passed snapshot. This
  preserves the I3 contract that two replays of the same event stream converge.
- **Dirty false-negative → staleness.** Mitigated structurally by the Phase-2 coarse backstop: a
  missed dirty signal costs at most one `decision_interval` of staleness, never permanent.
- **Snapshot/decision skew.** Within a tick there is no async gap (sampling and decision share the
  `run_reconcile` body). *Across* ticks, though, the dirty-gate intentionally retains a clean chain's
  scores for up to `decision_interval`, so a victim's sampled score can lag a live recovery that the
  fingerprint doesn't capture (e.g. RTT drift) while `best_alternative` is always live. To keep that
  skew from triggering a spurious eviction, `diff_scheduler` re-confirms the chosen victim **live**
  (`PlacementScorer::live_score`) on the rare sub-floor candidate path before applying the floor /
  hysteresis checks — one node, only when a candidate is found, so steady-state zero-scoring holds.
- **Thrash.** Unchanged guards apply: hysteresis gap + `last_rebalance` cooldown + (design doc)
  oscillation auto-pin. Phase 3's net-benefit gate further damps marginal moves.
- **Don't over-store history.** See Phase 1 sizing note; bounded ring + EWMA, not the design doc's
  115 KB/artifact.

## Test strategy (additions beyond per-phase)

- **Property — no-op cost.** On a stable mesh, scorer `score()` call count per tick is 0 after warm-up
  (dirty-gated), proving the polling wall is gone.
- **Property — convergence.** Under any tag-update sequence (no continuous oscillation), the arm
  reaches a state where no chain emits — unchanged from the design doc, re-verified over the snapshot
  path.
- **DST — dirty + backstop interplay.** Inject a sub-epsilon drift; assert the backstop (not dirty)
  triggers the eventual re-evaluation; assert no permanent staleness.
- **Bench.** Per-tick cost on N led chains, stable vs. churny; assert stable-case cost is ~O(N)
  counter compares, not O(N) scores. (Report at the right scale — ns/low-µs deltas are noise.)

## Effort

- Phase 0: ~0.5 day (contained refactor + existing tests).
- Phase 1: ~1 week (sidecar + snapshot plumbing + history/trend + tests).
- Phase 2: ~1 week (cadence + dirty wiring to fold generation/rtt/inventory + tests).
- Phase 3: ~3–4 days (cost model + gate + bridge wiring + tests).
- Phase 4: deferred (workload-gated).

Phases 0 ships standalone today. 1→2→3 are sequential (each builds on the snapshot split). Total
~2.5–3 weeks single-engineer for Phases 0–3.
