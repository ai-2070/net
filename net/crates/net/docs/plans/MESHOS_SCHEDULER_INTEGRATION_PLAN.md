# MeshOS ↔ Scheduler Integration — implementation plan (v2)

> The wiring between *deciding* and *running*. Thunderdome
> ([`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md)) decides who
> holds an island; Time ([`TASK_LIFECYCLE_PLAN.md`](TASK_LIFECYCLE_PLAN.md)) decides what
> state a task is in; **MeshOS runs the daemons.** Today these don't touch — `grep` for
> `meshos`/`DaemonIntent`/`ActionDispatcher` across `workflow/`, `tasks/`, `gang/` returns
> nothing. This doc plans the four state-projections (plus one veto rule) that connect
> them, *without either side calling the other.* Companion to Thunderdome (the claim),
> Time (the task state), [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) (the drift
> scorer this coexists with), the MeshOS reconcile loop it projects into, and
> [`PLAN_CORRECTIONS.md`](PLAN_CORRECTIONS.md) (whose `AfterTerminal` trigger this plan's
> Phase B consumes). **Come Together release** (Beatles) — two desired-state machines meet
> at the boundary and never call each other's names.

## Implementation status (as built)

Phases **A, B, and C are implemented** — all five projections, the
`SchedulerBridge` facade (owns the `ClaimRegistry`, composes the projections),
and the `SchedulerBridgeDriver` that runs them against the live handles
(`tick()` feeds `set_liveness_down` + publishes the merged intents; a self-owned
`spawn(interval)` / `shutdown()` loop drives `tick()`; a fan-out
`DaemonLifecycleObserver` applies Projection 3; claim hooks maintain the
registry). What remains is only **assembly** at a deployment site that already
owns a `MeshNode` + `MeshOsRuntime` + `WorkflowAdapter`: call
`driver.spawn(interval)`, install
`fan_out_lifecycle(vec![meshos_sink, driver.lifecycle_observer()])` on the
`DaemonRegistry`, and call `on_running` / `on_released` from the step-driver. All
cross-layer code lives in a neutral bridge module
`src/adapter/net/behavior/scheduler_bridge/` (Decision 1) — the only module
importing both `cortex::workflow` and `behavior::meshos`/`gang`, so LD 5 holds
structurally.

| Projection | As-built symbol | Status |
|---|---|---|
| 1 task → daemon intent | `project_daemon_intents(&WorkflowState) -> Vec<DaemonIntentUpdate>` | ✅ |
| 2 claim → forced placement | `project_forced_placements(&ClaimRegistry, resolve_host) -> Vec<DaemonIntentUpdate>` | ✅ |
| 3 lifecycle → step state | `apply_lifecycle` + `build_daemon_task_map` | ✅ (`AfterTerminal` already existed) |
| 4 liveness → fold delta | `project_liveness` (+ `_from_snapshot`) + `match_islands` host prune | ✅ projection + applier; driven by `SchedulerBridgeDriver::tick` |
| 5 migration veto | `migrate(MigrationEligible, NodeId) -> MigrationPlan` | ✅ type-enforced |

**Decision 1 — neutral bridge module.** The projections do NOT live in
`workflow/` as the design drafts implied: housing `daemon_ref` / `project_*`
there would make `workflow` import `meshos` types (`DaemonRef`,
`DaemonIntentUpdate`, `NodeId`), breaking LD 5. They live in
`behavior/scheduler_bridge/`; only the bridge sees both sides.

**Decision 2 — forced placement is a node-pinned daemon intent (corrects §2).**
The existing `diff_forced_placements` is chain/replica-keyed
(`forced_placements: Vec<(ChainId, NodeId)>` → `RequestPlacement { chain, .. }`);
it does not place daemons, and `StartDaemon { daemon }` carries no node. So a
minimal mechanism was added instead of abusing that arm: `DaemonIntentUpdate`
gained `node: Option<NodeId>`, `DesiredState` a sparse `desired_daemon_nodes`
companion map, and `diff_daemons` a `this_node` pin-gate — a daemon pinned to
`Some(n)` is managed only by node `n`. `None` = run anywhere (drift scorer's
domain); `Some(host)` = claim-pinned, invisible to the scorer by construction.
No new fold.

**Decision 3 — liveness via a match-time host prune, NOT a `CapabilityFold`
suspension flag (refines RD 5).** Projection 4's applier excludes dead nodes by
pruning the candidate-host set inside `gang::match_islands`
(`hosts.retain(|h| !down_nodes.contains(h))`, right after the capability match,
before the island query) — covering *both* folds' exclusion at the one point
they meet. This achieves RD 5's stated goal ("the match pipeline never
considers dead-node islands") while mutating neither fold: apply/merge/query/
the signed payload stay byte-identical, so AP semantics are preserved *more*
cleanly than a per-entry flag, with no merge-path interaction. `MeshNode` holds
the down-set in an `ArcSwap<HashSet<NodeId>>` fed by `set_liveness_down`; its
public `match_islands` signature is unchanged (bindings/FFI untouched).
Tradeoff: the fold itself doesn't *carry* liveness for other consumers
(snapshots/Deck) — acceptable, since RD 5's purpose is purely the match path.
The `GangScheduler` / `GangClaimPipeline` paths and the per-tick
`project_liveness → set_liveness_down` call remain deferred wiring.

**`daemon_ref` keying (refines RD 1).** The projection keys EVERY task — shards
included — by `daemon_ref(task_id)`, not `daemon_ref_shard(parent, index)`: in
this codebase a shard is itself a standalone `TaskId`, so task-id keying is
strictly more stable than `(parent, position)` under sibling delete/reorder,
which is exactly RD 1's retry-invisibility goal. `daemon_ref_shard` is retained
as the by-`(parent, index)` helper.

**Deferred — the appliers / wiring** (no consumer exists yet;
`drive_capability_step` / `release_step` still have no non-test caller):
- the runtime/step-driver that calls the projections, publishes the intents,
  populates `ClaimRegistry` on `Running` / `release_step`, and runs the
  `MigrationPlan` executor;
- the Projection-4 appliers — drop a down node's islands from `IslandTopology`
  and mark its `CapabilityFold` entries liveness-suspended (RD 5: a per-entry
  flag — a change to *hardened core*, not yet made);
- the house-pattern wire-propagation integration test (the in-process
  acceptance test `running_claim_starts_its_daemon_on_the_claim_node_only`
  covers the projection→reconcile boundary meanwhile).

## Status

Design only. **No new cross-layer machinery** — every prerequisite exists; the work is
drawing four projections between surfaces that already exist and currently ignore each
other.

Prerequisites:
- ~~**`ClaimPipeline` + `drive_capability_step`** (`workflow/step.rs`) — `claim`/`release`,
  returns `StepGate::Running(ActiveClaim)` / `Waiting`. The bypass-impossibility is
  type-enforced (no reservation fold in step scope).~~ ✅ Landed.
- ~~**`release_step` + `StepKind` re-exec guard** (`workflow/step.rs`).~~ ✅ Landed.
- ~~**MeshOS reconcile** (`behavior/meshos/reconcile.rs`) — `reconcile(actual, desired,
  …, scorer) -> Vec<MeshOsAction>`, with `diff_daemons`, `diff_forced_placements`,
  `diff_replicas`, `diff_scheduler` arms.~~ ✅ Landed (verified: lines 76/82/83/92).
- ~~**Intent + lifecycle vocab** (`behavior/meshos/event.rs`) — `DaemonIntent::{Run,Stop}`,
  `DaemonRef`, `PlacementIntent`, `DaemonLifecycleSignal`.~~ ✅ Landed (verified: lines
  238/251/551/563).
- ~~**`ActionDispatcher`** (`behavior/meshos/executor.rs`) — the execution edge.~~ ✅ Landed.
- ~~**`IslandTopology` fold** (gang scheduler input).~~ ✅ Landed.
- **PARTIALLY LANDED — needs the corrections plan's primitive:** failure propagation in
  shard fan-out depends on `Trigger::AfterTerminal`, which is introduced in
  [`PLAN_CORRECTIONS.md`](PLAN_CORRECTIONS.md) §2. This plan's Projection 3 consumes it.
  Either ship that primitive first or sequence the corrections-plan work to land before
  Phase B of this plan.
- **NEW, this plan:** four projections (§Design) + the migration-veto rule. Zero of these
  exist today; the two halves are unconnected (verified: `grep meshos` against `workflow/`
  and `gang/` returns no matches; one false positive on an unrelated `ice.rs` comment).

Activation gate: a task whose step holds an `ActiveClaim` needs to actually *run* on the
claimed node, and a crashed worker needs to *release* the claim. Both are unreachable
until this plan lands.

## Frame

Both sides are already **desired-state machines.** MeshOS's `reconcile` diffs a
`DesiredState` against observed `MeshOsState` and emits `MeshOsAction`s. The workflow fold
*is* a statement of desired state (which tasks should be running). So the connection is
not a call graph — it is **projection at the desired/observed boundary**, the same
"propagate state, not connections" posture as the rest of NET. Neither module imports the
other. The workflow projects task state *into* `DesiredState`; MeshOS projects observed
daemon/node state *back into* task state and the topology fold. Four projections, one
boundary, no RPC.

**Reuses existing primitives:** `reconcile` + its `diff_forced_placements`/`diff_daemons`
arms, `DaemonIntent`, `DaemonLifecycleSignal`, `ActionDispatcher`, `PlacementScorer`,
`ClaimPipeline`, `ActiveClaim`, the `IslandTopology` fold. **Adds:** four projection
functions, the claim-migration veto, and a documented `DaemonRef` encoding convention. No
new transport, no new fold, no new consensus.

## Why this exists

1. **`Running(ActiveClaim)` currently runs nothing.** `drive_capability_step` hands back a
   gate saying "the GPU is yours, the task may execute" — and there is no wire that turns
   that into a daemon on the claimed node. The claim succeeds into a void.
2. **Daemon lifecycle is the *only* correct way a crashed job releases its claim.** Without
   the lifecycle→step projection, a worker that dies holding an `ActiveClaim` leaks the
   island forever (corrections #2/#4: stranded GPU). The release path already exists
   (`release_step`); it just needs `DaemonLifecycleSignal` to *trigger* it, and the
   trigger machinery needs `AfterTerminal` from the corrections plan to actually
   propagate the failure to a release.
3. **Migration vs. exclusive claims is a correctness rule, not an optimization.** Mikoshi
   can migrate a daemon off the node whose GPU it is using. That either abandons the
   claim (double-book) or needs an impossible re-claim on the destination. The veto must
   exist before migration and claims coexist in production, and the veto must be
   *enforced* — not just documented as convention.

## What ships

Five pieces, in dependency order:

1. **Projection: workflow task → `DaemonIntent`.** A task at `Running` projects to
   `DaemonIntent::Run` for a task-keyed `DaemonRef`; terminal/cancel → `Stop`; shard
   fan-out → N intents. Written into `DesiredState`; reconcile diffs and emits the
   start/stop action. The workflow **writes intent only** — never
   `ActionDispatcher::dispatch` ([LD 3](#3-the-workflow-writes-desired-state-never-dispatches)).
2. **Projection: claim → forced placement.** A claim-bearing task emits a *forced
   placement* (existing `diff_forced_placements` arm) pinning its daemon to the
   `ActiveClaim`'s node. The drift `PlacementScorer` is bypassed for claim-pinned daemons
   — the claim is the placement decision ([LD 1](#1-the-claim-wins-over-the-drift-scorer)).
3. **Projection: `DaemonLifecycleSignal` → step state.** Daemon started → task confirmed
   `Running`; daemon failed / abnormal exit → step `Failed`. The failure then propagates
   through the corrections-plan `Trigger::AfterTerminal` machinery to release the claim
   via `release_step` and either retry, fail the parent, or fail the gang per the shard
   failure-policy. Fully specified in [§Projection 3](#3-daemonlifecyclesignal--step-state-observed-up).
4. **Projection: observed node/GPU liveness → fold updates.** MeshOS observes node
   liveness in `MeshOsState`; project it into **both** the `IslandTopology` fold (so the
   numeric-filter stage drops dead nodes' islands from candidates) **and** the
   `CapabilityFold` aging (so the coarse-match stage doesn't keep emitting candidates
   that will just be filtered out). See [§Projection 4](#4-observed-liveness--topology--capability-aging-observed-up).
5. **Migration-veto rule.** An exclusive `ActiveClaim` makes its daemon migration-pinned
   for the claim's lifetime. The Mikoshi/migration path consults claim-state and refuses
   to move a claim-holder; planned drain becomes release → re-claim elsewhere → restart
   (stop-the-world for that task), never live migration
   ([LD 2](#2-migration-veto-for-exclusive-claims)). **The veto is enforced by a marker
   trait on the migration path, not by a `can_migrate()` bool that contributors can
   forget to consult** — see [LD 4](#4-veto-enforcement-is-by-type-not-convention).

What this doc does NOT ship:
- **Any direct call between the layers.** If a projection is implemented as a method call
  from `workflow` into `meshos` (or back), it is wrong — see
  [LD 5](#5-no-calls-across-layers-everything-is-state-projected).
- **Changes to the drift scorer.** `PlacementScorer` keeps scoring *unpinned* daemons;
  this plan only carves claim-pinned daemons out of its domain.
- **A migration protocol for claim-holders.** The veto forbids it; designing a
  live-migrate-with-claim-handoff is explicitly out of scope (and probably physically
  meaningless for a held GPU).

---

## Design

All four projections are pure functions at the state boundary — no I/O, no calls into the
other module.

### 1. Task → `DaemonIntent` (desired down)

```text
project_daemon_intents(workflow: &WorkflowState) -> Vec<DaemonIntentUpdate>
    Running(task)        → DaemonIntentUpdate { daemon: daemon_ref(task), intent: Run }
    Failed/Done/Deleted  → ...                                            intent: Stop
    shard fan-out        → one Run per shard daemon_ref
```

**`DaemonRef` encoding (the load-bearing piece).** The existing struct is
`DaemonRef { id: u64, name: String }` — verified at `event.rs:238`. The plan does NOT
change the struct shape. Instead it pins the **encoding convention** for task-derived
refs:

```rust
fn daemon_ref(task: TaskId) -> DaemonRef {
    DaemonRef {
        // splitmix64 over (DOMAIN_TAG ⊕ task_id). Domain tag distinguishes
        // task daemons from system daemons in the same registry so a
        // task_id collision with a system daemon name is impossible.
        id: encode_task_ref(task),
        name: format!("task/{task}"),
    }
}

fn daemon_ref_shard(parent: TaskId, shard_index: u16) -> DaemonRef {
    DaemonRef {
        // splitmix64 over (DOMAIN_TAG ⊕ (parent, shard_index)).
        // Attempt number is DELIBERATELY EXCLUDED so retries
        // are reconcile-invisible at the daemon layer (RD 1).
        id: encode_shard_ref(parent, shard_index),
        name: format!("task/{parent}/shard/{shard_index}"),
    }
}
```

Folded into the `DesiredState` MeshOS already consumes. The workflow has no handle to the
dispatcher; it can only emit intent. **Attempt number is excluded from the encoding** so
a shard retry projects to the same ref → no diff → no spurious stop/start churn (RD 1).

The encoding convention is defined in **one place** (a new `daemon_ref.rs` in the
`scheduler_bridge` module — Decision 1, **not** the workflow module) and re-exported. Any
code path that constructs a task-derived `DaemonRef` without going through these functions
is a regression.

**As built:** the projection keys *every* task by `daemon_ref(task_id)` (shards
included) rather than `daemon_ref_shard` — see Implementation status →
`daemon_ref` keying.

### 2. Claim → forced placement (desired down, the scheduler seam)

**As built (Decision 2) — this corrects the design below.** Forced placement
is a *node-pinned daemon intent*, not a separate `ForcedPlacement` type, and is
consumed by `diff_daemons` — NOT the chain-keyed `diff_forced_placements` arm,
which places replicas (chains), not daemons:

```text
project_forced_placements(claims: &ClaimRegistry, resolve_host) -> Vec<DaemonIntentUpdate>
    for each (daemon, claim) in claims:
        if let Some(host) = resolve_host(claim.island):   // island → IslandRecord.host
            DaemonIntentUpdate { daemon, intent: Run, node: Some(host) }
        // island vanished (dead node, aged out) → emit nothing; the task re-claims at TTL
```

`ActiveClaim` carries only `{ island }`, so the host is resolved through the
`IslandTopology` fold — `resolve_host` is a closure over `IslandQuery::Get`,
which keeps the projection pure. The node-pinned intent makes the claim-pinned
daemon invisible to the drift `PlacementScorer` *by construction* (only the
claim's node acts on it). Composition with Projection 1 is an overlay: apply
Projection 1 (every task `node: None`) then Projection 2; `apply_daemon_intent`
is last-write-wins per daemon, so `None` is overridden by `Some(host)`. See
Implementation status → Decision 2 for the `node` field / `desired_daemon_nodes`
/ `diff_daemons` pin-gate.

*(Original design, superseded:* `project_forced_placements(workflow, claims) ->
Vec<ForcedPlacement>` consumed by `diff_forced_placements` — wrong, see above.)

**Freeze interaction (corrected from v1).** The current reconcile freeze behavior at
`reconcile.rs:67-69` is "drop all output and return early" — there is no `FrozenActions`
queue (verified: `grep FrozenActions` returns zero hits anywhere in the codebase). v1
of this plan proposed adding one. **That isn't needed.** Forced placements are
deterministically re-derived every tick from `(workflow_state, active_claims)`. On thaw,
the next reconcile tick re-projects current state and emits whatever forced placements
are still implied by held claims. As long as the claim is still held, the forced
placement re-emerges naturally — no queue required.

The only failure mode the v1 queue was protecting against is: **reserve TTL expires
during freeze, claim is lost, freeze thaws, and the forced placement points at an island
the task no longer holds.** This is prevented by the invariant in [RD 2](#2-freeze-cannot-outlast-claim):

```
max_freeze_duration < reserve_TTL
```

Concretely: with the corrections-plan reserve TTL of 5s, freeze TTL must be capped below
that (cluster-wide config; rejecting freeze proposals with TTL above the cap is a
one-line check in ICE proposal validation). A freeze that would outlast a claim is
rejected at proposal time, not silently allowed to strand claims.

### 3. `DaemonLifecycleSignal` → step state (observed up)

**Status: ✅ built.** Not actually blocked: `Trigger::AfterTerminal` already
exists in `workflow/trigger.rs` ("fires once `task` reaches `Done` *or*
`Failed`"), and `PLAN_CORRECTIONS.md` isn't in the repo — that corrections work
landed, so the gate is lifted. Implemented as `apply_lifecycle(signal, daemon,
&daemon_task_map) -> Option<LifecycleTransition>` + `build_daemon_task_map`,
the daemon→task reverse map (`daemon_ref` is one-way, so a `DaemonLifecycleSignal`
— which carries only a `DaemonRef` — needs it to recover the `TaskId`). The
projection is pure; applying the resulting `FailStep` via `WorkflowAdapter::fail`
is what fires the existing `AfterTerminal` trigger (runtime, deferred).

```text
apply_lifecycle(signal, workflow) -> WorkflowTransition
    Started(d)          → confirm task(d) Running
    Failed/Exited(d)    → fail step → AfterTerminal trigger fires →
                          (failure policy applied per ShardGroup config) →
                          release_step(claim)
    Stopped(d)          → expected for terminal/cancel; no-op
```

The failure path now has a concrete primitive to ride: `Trigger::AfterTerminal(TaskId)`
from the corrections plan §2. When a daemon `Failed` signal lands, the projection
transitions the step to `Failed`, which fires the `AfterTerminal` trigger, which the
corrections-plan failure-policy code consumes to decide whether to retry the shard,
cancel sibling shards, or propagate failure to the parent. The release of the held
`ActiveClaim` happens as part of step `Failed` cleanup — `release_step` is called by the
worker when its step terminates, the corrections plan does not need to reach into the
claim system.

This is the **one ordering constraint** between the two plans: the corrections plan's
`AfterTerminal` primitive must land before this plan's Phase B is meaningfully testable,
because Phase B's "killing a worker daemon fails its step and returns the island to
`Free` within bounded time" test depends on the failure-propagation path completing.

### 4. Observed liveness → topology + capability aging (observed up)

**Status: ✅ projection + applier built (applier as a match-time host prune,
not the RD 5 suspension flag — see Implementation status → Decision 3).**
`project_liveness(&MeshOsState) -> LivenessDelta` is the *pure, node-level*
projection: it reads the `node_health` fold and classifies nodes
(`Unreachable` → down; `Healthy`/`Degraded` → up — a degraded node stays a
candidate). `LivenessDelta` is therefore node-level
(`{ down: Vec<NodeId>, up: Vec<NodeId> }`), not the island/capability shape
sketched below. The applier is `gang::match_islands`'s `down_nodes` host prune
(covers both folds' exclusion without mutating either), wired into
`MeshNode::set_liveness_down`. Deferred: the per-tick
`project_liveness → set_liveness_down` call and the `GangScheduler` /
`GangClaimPipeline` paths.

```text
project_liveness(meshos: &MeshOsState) -> LivenessDelta
    node down  → topology: drop its islands from candidate set
                  capability: invalidate that node's capability entries
                              (don't wait for natural aging)
    node up    → topology: (re)admit
                  capability: allow next heartbeat to re-establish
```

Both folds get the signal. The gang scheduler's match pipeline reads `CapabilityFold`
first (coarse prefilter) and `IslandTopology` second (numeric filter). If only the
topology fold got the liveness signal — as v1 of this plan proposed — the capability
fold would keep advertising dead-node islands until natural aging, producing candidates
that get filtered out wastefully. Touching both folds means the match pipeline never
considers dead-node islands at any stage.

**Capability fold invalidation is *not* deletion.** The capability fold is a CRDT-grade
AP structure; the projection marks entries as "liveness-suspended" rather than removing
them. On node-up, suspension lifts and the next capability heartbeat refreshes the
entry. Suspension is a single boolean flag per capability entry, indexed on the
publishing node id.

**Invariant** (corrected from v1): the reserve TTL must satisfy:
```
reserve_TTL > 2 × (max_liveness_heartbeat_interval + worst_case_fold_propagation_lag)
```
With the deployment defaults (1.5s liveness sample, <500ms fold-projection lag): reserve
TTL ≥ 4s; safe ceiling 5s; this also gives the bound for [RD 2](#2-freeze-cannot-outlast-claim).

This guarantees the ordering — node dies → MeshOS marks it dead → projection 4
invalidates capability + drops topology → matching stops offering — completes before the
TTL could expire early. A stale island cannot be matched-and-claimed in the gap.

### 5. Migration veto (enforced by type, not convention)

**Status: ✅ implemented** (the veto primitive; the executor that runs the
resulting `MigrationPlan` is deferred wiring). Built as sketched below, plus a
`MigrationPlan { daemon, target }` returned by `migrate` (obtainable only by
consuming a `MigrationEligible`, so it can never name a claim-holder), and a
`compile_fail` doctest pinning the type gate. `ClaimRegistry` is keyed by
`DaemonRef` so `holds_exclusive` answers this directly.

```rust
// The migration entry point takes a marker proving the daemon
// is not a claim-holder. Constructing one consults the claim
// registry; there is no `unsafe` constructor.
pub struct MigrationEligible(DaemonRef);

impl MigrationEligible {
    pub fn check(daemon: DaemonRef, claims: &ClaimRegistry)
        -> Result<Self, ClaimHeld>
    {
        if claims.holds_exclusive(&daemon) {
            return Err(ClaimHeld(daemon));
        }
        Ok(MigrationEligible(daemon))
    }
}

// The only migration entry takes this type. A code path that
// migrates a claim-holder simply cannot type-check.
pub fn migrate(eligible: MigrationEligible, target: NodeId) -> ... { ... }
```

This is the LD 4 enforcement: the veto is impossible to bypass by accident because the
migration entry point only accepts a `MigrationEligible` that can only be constructed
by passing the claim check. A contributor adding a new migration code path has to go
through this function or invent a parallel migration system (which is a much larger and
more visible regression than forgetting a `can_migrate()` call).

Drain of a claim-holder remains `release → re-claim → restart`, where the **re-claim is
an ordinary Thunderdome claim** — it acquires destination islands via §4 ordered-acquire,
so drain-vs-drain (or drain-vs-gang) contention is resolved deadlock-free by the existing
protocol with no separate drain coordinator (RD 3).

## Phasing

### Phase A — Desired-down projections (1 week) — ✅ LANDED
Projections 1 + 2: task→intent, claim→forced-placement. **Done when:** a
`Running(ActiveClaim)` task starts a daemon on the claim's node, and a terminal task
stops it — entirely through `DesiredState`, with no workflow→dispatcher call in the
path. The `daemon_ref` encoding lives in one module and has the visibility constraint
that prevents bypass.

### Phase B — Observed-up projections — ✅ PROJECTIONS LANDED (runtime apply deferred)
Both projections are built: Projection 3 (`apply_lifecycle` +
`build_daemon_task_map` — `AfterTerminal` already existed, so it was never
blocked) and Projection 4 (`project_liveness` + the `gang::match_islands`
host-prune applier wired to `MeshNode::set_liveness_down`, instead of RD 5's
suspension flag — see Decision 3). What's deferred is the runtime that *applies*
them each tick: feeding `set_liveness_down`, and applying `LifecycleTransition`s
via `WorkflowAdapter` (which fires the `AfterTerminal` failure policy).
Projections 3 + 4: lifecycle→step (consuming `AfterTerminal`), liveness→topology +
capability. **Done when:** killing a worker daemon fails its step and returns the island
to `Free` within **≤ 2 × (liveness_heartbeat + fold_propagation_lag) + release_latency**
(at deployment defaults: ≤4s for the liveness-driven path, ≤reserve_TTL for the
TTL-driven path); a downed node's islands disappear from both capability and topology
folds.

### Phase C — Migration veto (3-5 days) — ✅ LANDED (veto primitive; executor deferred)
Projection 5 with type-enforced eligibility. **Done when:** the migration path will not
compile if called without `MigrationEligible`, and a planned drain performs
release → re-claim → restart without ever double-holding the island.

## Test strategy

**Landed (this branch).** Full unit coverage on the bridge — `daemon_ref`
encoding, `ClaimRegistry`, all five projections, the `SchedulerBridge` facade,
and the driver's `event_to_signal` + fan-out — plus the meshos plumbing units
(the `diff_daemons` node-pin gate and the `desired_daemon_nodes` lifecycle) and
the gang `match_islands` dead-host prune. Two end-to-end integration tests in
`tests/scheduler_bridge_driver.rs` (run in CI's `--features meshos` group) drive
the `SchedulerBridgeDriver` over a live `MeshOsLoop` + `MeshNode` +
`WorkflowAdapter`. Status of the specific cases below:

- ⬜ **DST — claim + migration (gating).** Not yet built. Extend `loom_models.rs`:
  a claim-holding daemon targeted for migration mid-claim must be vetoed; a
  partition that strands a claim-holder must not let migration *and* reconcile
  both act. This is the interaction the single-box tests cannot reach.
- ✅ **Type-system test for the migration veto.** Landed — the `compile_fail`
  doctest on `migrate` (`scheduler_bridge/migration.rs`) proves
  `migrate(some_daemon_ref, target)` does not compile without
  `MigrationEligible::check` first.
- ✅ **Integration across the reconciliation boundary** (in-process form).
  `running_claim_starts_its_daemon_on_the_claim_node_only` drives a
  `Running(ActiveClaim)` task → Projections 1+2 → `DesiredState` → `reconcile`
  and asserts a `StartDaemon` on the claimed node and on NO other node; the
  driver test asserts the publish + a terminal stop, and the facade never holds a
  dispatcher handle. The full house two-`NetNode` wire-propagation variant waits
  on the deployment assembly.
- ◐ **Daemon failure → claim release correctness.** Partial — the driver test
  proves a daemon `Crashed` drives the step to `Failed` and releases the claim
  from the registry (the apply path; `AfterTerminal` fires on `fail`). The
  "island `Free` within **≤4s at deployment defaults**, re-claimable by another
  gang" bound needs the worker/runtime `release_step` → reservation-fold wiring,
  still deferred.
- ⬜ **Partition recovery semantics.** Not yet built (needs a multi-node +
  partition harness). Split a claim-holder from the majority; on heal, assert the
  claim's island ended `Free` exactly once (no double-run survived — Thunderdome
  §6), the daemon and workflow task states agree, and no orphaned daemon kept
  running on the minority side.
- ⬜ **Freeze-vs-claim test.** Not yet built — also needs the RD 2 ICE
  freeze-TTL-cap **feature**, not yet implemented. Issue a freeze with TTL <
  reserve_TTL: forced placements re-emerge on thaw because the claim is still
  held. Issue a freeze with TTL ≥ reserve_TTL: **proposal is rejected by ICE
  validation** (test the rejection path exists, not the silent-strand path).
- ⬜ **Replica-set placement test.** Not yet built — also needs the RD 6
  `ColocationStrict` startup-check **feature**, not yet implemented. A config that
  ships without `ColocationStrict` on the topology fold's replica set should fail
  a startup check, not silently degrade to a slower partition-recovery path.

## Locked decisions

#### 1. The claim wins over the drift scorer
A claim-pinned daemon is placed by forced placement on the `ActiveClaim`'s node and is
removed from the `PlacementScorer`'s domain. The drift scheduler and the gang claim
never both try to place the same daemon; the claim is authoritative for claim-bearing
work.

#### 2. Migration-veto for exclusive claims
An exclusive `ActiveClaim` pins its daemon: migration is refused for the claim's
lifetime. Moving compute off the GPU it is using is either a double-book or an
impossible re-claim; drain is release → re-claim → restart, never live migration.

#### 3. The workflow writes desired state, never dispatches
The workflow emits `DaemonIntent` / forced placements into `DesiredState`. It has no
`ActionDispatcher` handle. This preserves reconcile convergence, supervision,
backpressure, and the audit chain, and prevents split-brain between "what the fold
thinks runs" and "what runs."

#### 4. Veto enforcement is by type, not convention
LD 2 is enforced by `MigrationEligible` (Projection 5): the migration entry point only
accepts a marker that can only be constructed by passing the claim check. A bypass would
require either type-defeating `unsafe` or inventing a parallel migration entry point —
both substantially more visible than forgetting a `can_migrate()` call. Promoted from v1
"convention" to type enforcement after review found the convention insufficient against
future contributors.

#### 5. No calls across layers — everything is state-projected
`workflow`/`gang` and `meshos` never import or call each other. All five connections are
projections at the desired/observed boundary. A direct call in either direction is a
design regression, not a shortcut — it recouples what the substrate keeps decoupled.

#### 6. A step cannot bypass the scheduler — already enforced by the type system
`drive_capability_step` has no reservation fold in scope; the only path to an exclusive
resource is `ClaimPipeline`. Bypass is impossible by signature, not by discipline. This
plan inherits that guarantee and must not introduce a side path that weakens it.

## Resolved design decisions (v2)

All resolutions below are load-bearing rules, not advisory. Changes from v1 are flagged.

1. **`DaemonRef` encoding excludes attempt number — encoding lives in one module.
   CLOSED.** The existing `DaemonRef { id, name }` struct is unchanged. Task-derived
   refs are constructed only through `daemon_ref(task)` / `daemon_ref_shard(parent,
   shard)` in `workflow/daemon_ref.rs`. Attempt number is deliberately excluded so retry
   is reconcile-invisible (same ref → same desired state → no diff → no churn). Bypass
   is prevented by module visibility on the encoded id range; a contributor constructing
   a `DaemonRef` directly with a task-shaped id fails the lint. **(v1 implied the struct
   carried `(task_id, shard_index)` natively; corrected.)**

2. **Freeze cannot outlast claim — enforced at ICE proposal validation. CLOSED.**
   Forced placements re-derive deterministically on each reconcile tick from
   `(workflow_state, active_claims)`. Freeze suppresses output but doesn't drop intent —
   thaw re-projects current state. The only stranding risk is reserve TTL expiring
   *during* freeze. Prevented by capping `max_freeze_TTL < reserve_TTL` at the ICE
   proposal validation layer (a freeze proposal with TTL ≥ reserve_TTL is rejected, not
   silently accepted). **(v1 proposed a `FrozenActions.forced_placements` queue. That
   infrastructure does not exist (`grep FrozenActions` → zero hits), and re-derivation
   on thaw is the simpler and correct mechanism. Corrected.)**

3. **Reserve TTL > 2× the MeshOS liveness sampling interval. CLOSED.** Unchanged from
   v1. `TTL > 2 × (max liveness heartbeat interval + worst-case propagation)`. With
   1.5s liveness sample and <500ms fold-projection lag → 5s reserve TTL is safe and
   generous. Set once from the deployment's liveness config. **This is also the cap that
   Resolved Decision 2 references for `max_freeze_TTL`.**

4. **Drain ordering is serial per-island via Thunderdome — no drain-specific
   arbitration. CLOSED.** Unchanged from v1. A drain is `release → re-claim → restart`,
   and `re-claim` is an ordinary Thunderdome claim. Two drained daemons targeting the
   same destination island contend via §4 ordered-acquire — no "drain priority," no
   "drain lock," no bypass. Deadlock-freedom is inherited, not engineered: reserves are
   AP, `Active` is CP with the epoch-fence, so no two tasks ever reach `Active` on the
   same island.

5. **Liveness projection touches both topology and capability folds. CLOSED.** v1
   projected only into `IslandTopology`, which left `CapabilityFold` continuing to
   advertise dead-node islands until natural aging. Projection 4 now invalidates
   capability entries by the down-node's id and (re)admits on node-up, eliminating the
   wasted candidate-then-filter cycles. The capability fold's CRDT-grade AP semantics
   are preserved — invalidation is a per-entry suspension flag, not a delete. **(v1
   skipped capability; corrected.)**

6. **`IslandTopology` fold chain placement requires `ColocationStrict`. CLOSED.** The
   Thunderdome §5 invariant that island replica sets pin to one fault domain extends to
   the topology fold's own chain: the chain that records which nodes own which islands
   must itself be placement-constrained, or a cross-DC partition can bisect the
   topology's quorum and produce inconsistent matching across sides. Pinned with a
   startup check that refuses to run if the topology fold's chain is not
   `ColocationStrict` to a single fault domain.

## See also
- [`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md) — the claim
  whose `ActiveClaim` node drives forced placement; §5 `ColocationStrict` invariant
  that Resolved Decision 6 extends to the topology fold; §6 partition semantics this
  plan's recovery test asserts against.
- [`TASK_LIFECYCLE_PLAN.md`](TASK_LIFECYCLE_PLAN.md) — the task state projected into
  `DaemonIntent`; the failure-propagation path lifecycle signals trigger.
- [`PLAN_CORRECTIONS.md`](PLAN_CORRECTIONS.md) — §2 `Trigger::AfterTerminal` primitive
  that this plan's Projection 3 consumes; §1 `Blocked` decision affects whether the
  workflow state machine has a state that this plan would need to project (if `Blocked`
  is kept and means "submitted but resource-blocked," Projection 1 needs a row for it).
- [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) — the drift `PlacementScorer` that
  coexists with, and yields to, claim-pinned placement.
- `.claude/skills/net-event-bus/testing.md` — the house test harness.
