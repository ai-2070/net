# MeshOS ↔ Scheduler Integration — implementation plan

> The wiring between *deciding* and *running*. Thunderdome
> ([`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md)) decides who
> holds an island; Time ([`TASK_LIFECYCLE_PLAN.md`](TASK_LIFECYCLE_PLAN.md)) decides what
> state a task is in; **MeshOS runs the daemons.** Today these don't touch — `grep` for
> `meshos`/`DaemonIntent`/`ActionDispatcher` across `workflow/`, `tasks/`, `gang/` returns
> nothing. This doc plans the four state-projections (plus one veto rule) that connect them,
> *without either side calling the other.* Companion to Thunderdome (the claim), Time (the
> task state), [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) (the drift scorer this
> coexists with), and the MeshOS reconcile loop it projects into. **Come Together release**
> (Beatles) — two desired-state machines meet at the boundary and never call each other's
> names.

## Status

Design only. **No new cross-layer machinery** — every prerequisite exists; the work is
drawing four projections between surfaces that already exist and currently ignore each
other.

Prerequisites:
- ~~**`ClaimPipeline` + `drive_capability_step`** (`workflow/step.rs`) — `claim`/`release`,
  returns `StepGate::Running(ActiveClaim)` / `Waiting`. The bypass-impossibility is
  type-enforced (no reservation fold in step scope).~~ ✅ Landed.
- ~~**`release_step` + `StepKind` re-exec guard** (`workflow/step.rs`).~~ ✅ Landed.
- ~~**MeshOS reconcile** (`behavior/meshos/reconcile.rs`) — `reconcile(actual: &MeshOsState,
  desired: &DesiredState, …, scorer) -> Vec<MeshOsAction>`, with `diff_daemons`,
  `diff_forced_placements`, `diff_replicas`, `diff_scheduler` arms.~~ ✅ Landed.
- ~~**Intent + lifecycle vocab** (`behavior/meshos/event.rs`) — `DaemonIntent::{Run,Stop}`,
  `DaemonRef`, `PlacementIntent`, `DaemonLifecycleSignal`.~~ ✅ Landed.
- ~~**`ActionDispatcher`** (`behavior/meshos/executor.rs`) — the execution edge.~~ ✅ Landed.
- ~~**`IslandTopology` fold** (gang scheduler input).~~ ✅ Landed.
- **NEW, this plan:** four projections (§Design) + the migration-veto rule. Zero of these
  exist today; the two halves are unconnected.

Activation gate: a task whose step holds an `ActiveClaim` needs to actually *run* on the
claimed node, and a crashed worker needs to *release* the claim. Both are unreachable until
this plan lands.

## Frame

Both sides are already **desired-state machines.** MeshOS's `reconcile` diffs a `DesiredState`
against observed `MeshOsState` and emits `MeshOsAction`s. The workflow fold *is* a statement
of desired state (which tasks should be running). So the connection is not a call graph — it
is **projection at the desired/observed boundary**, the same "propagate state, not
connections" posture as the rest of NET. Neither module imports the other. The workflow
projects task state *into* `DesiredState`; MeshOS projects observed daemon/node state *back
into* task state and the topology fold. Four projections, one boundary, no RPC.

**Reuses existing primitives:** `reconcile` + its `diff_forced_placements`/`diff_daemons`
arms, `DaemonIntent`, `DaemonLifecycleSignal`, `ActionDispatcher`, `PlacementScorer`,
`ClaimPipeline`, `ActiveClaim`, the `IslandTopology` fold. **Adds:** four projection
functions and the claim-migration veto. No new transport, no new fold, no new consensus.

## Why this exists

1. **`Running(ActiveClaim)` currently runs nothing.** `drive_capability_step` hands back a
   gate saying "the GPU is yours, the task may execute" — and there is no wire that turns
   that into a daemon on the claimed node. The claim succeeds into a void.
2. **Daemon lifecycle is the *only* correct way a crashed job releases its claim.** Without
   the lifecycle→step projection, a worker that dies holding an `ActiveClaim` leaks the
   island forever (corrections #2/#4: stranded GPU). The release path already exists
   (`release_step`); it just needs `DaemonLifecycleSignal` to *trigger* it.
3. **Migration vs. exclusive claims is a correctness rule, not an optimization.** Mikoshi can
   migrate a daemon off the node whose GPU it is using. That either abandons the claim
   (double-book) or needs an impossible re-claim on the destination. The veto must exist
   before migration and claims coexist in production.

## What ships

Five pieces, in dependency order:

1. **Projection: workflow task → `DaemonIntent`.** A task at `Running` projects to
   `DaemonIntent::Run` for a task-keyed `DaemonRef`; terminal/cancel → `Stop`; shard fan-out
   → N intents. Written into `DesiredState`; reconcile diffs and emits the start/stop action.
   The workflow **writes intent only** — never `ActionDispatcher::dispatch`
   ([LD 3](#3-the-workflow-writes-desired-state-never-dispatches)).
2. **Projection: claim → forced placement.** A claim-bearing task emits a *forced placement*
   (existing `diff_forced_placements` arm) pinning its daemon to the `ActiveClaim`'s node.
   The drift `PlacementScorer` is bypassed for claim-pinned daemons — the claim is the
   placement decision ([LD 1](#1-the-claim-wins-over-the-drift-scorer)).
3. **Projection: `DaemonLifecycleSignal` → step state.** Daemon started → task confirmed
   `Running`; daemon failed / abnormal exit → step `Failed` → failure-propagation
   (Time corrections #2) → `release_step` returns the island. This is the release trigger;
   the release mechanism already exists.
4. **Projection: observed node/GPU liveness → `IslandTopology` fold.** MeshOS already
   observes node liveness in `MeshOsState`; project it into the topology fold so the gang
   scheduler's candidate set reflects reality (a dead node's islands drop out of matching).
5. **Migration-veto rule.** An exclusive `ActiveClaim` makes its daemon migration-pinned for
   the claim's lifetime. The Mikoshi/migration path consults claim-state and refuses to move
   a claim-holder; planned drain becomes release → re-claim elsewhere → restart (stop-the-
   world for that task), never live migration ([LD 2](#2-migration-veto-for-exclusive-claims)).

What this doc does NOT ship:
- **Any direct call between the layers.** If a projection is implemented as a method call
  from `workflow` into `meshos` (or back), it is wrong — see
  [LD 5](#5-no-calls-across-layers-everything-is-state-projected).
- **Changes to the drift scorer.** `PlacementScorer` keeps scoring *unpinned* daemons; this
  plan only carves claim-pinned daemons out of its domain.
- **A migration protocol for claim-holders.** The veto forbids it; designing a live-migrate-
  with-claim-handoff is explicitly out of scope (and probably physically meaningless for a
  held GPU).

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

    daemon_ref(task)        = DaemonRef(task_id)
    daemon_ref(shard)       = DaemonRef(task_id, shard_index)   // NEVER attempt number
```
Folded into the `DesiredState` MeshOS already consumes. The workflow has no handle to the
dispatcher; it can only emit intent. The `DaemonRef` **must exclude the attempt number** so a
shard retry is reconcile-invisible (same ref → no spurious stop/start) — see resolved
decision 1.

### 2. Claim → forced placement (desired down, the scheduler seam)
```text
project_forced_placements(workflow, claims) -> Vec<ForcedPlacement>
    for each task in Running(ActiveClaim):
        ForcedPlacement { daemon: daemon_ref(task), node: claim.island.node }
```
Consumed by `diff_forced_placements`. A claim-pinned daemon is invisible to the drift
`PlacementScorer`. The gang scheduler decided the node; MeshOS just honors it. Under
ICE/freeze, forced placements queue in `FrozenActions.forced_placements` and **replay first
on thaw** — before the drift scorer or daemon diff — so a freeze can never strand an
`ActiveClaim` (resolved decision 2).

### 3. `DaemonLifecycleSignal` → step state (observed up)
```text
apply_lifecycle(signal, workflow) -> WorkflowTransition
    Started(d)          → confirm task(d) Running
    Failed/Exited(d)    → fail step → (failure propagation) → release_step(claim)
    Stopped(d)          → expected for terminal/cancel; no-op
```
The release on failure is the corrections-doc cross-cutting rule, now *triggered* by a real
signal rather than hoped for.

### 4. Observed liveness → `IslandTopology` (observed up)
```text
project_topology(meshos: &MeshOsState) -> IslandTopologyDelta
    node down  → drop its islands from the candidate set
    node up    → (re)admit
```
Closes the loop: matching never offers an island on a dead node. **Invariant:** the reserve
TTL must be `> 2 × (liveness heartbeat interval + worst-case propagation)` so the
dead-node→drop sequence always completes before the TTL could expire early — a stale island
cannot be matched-and-claimed in the gap (resolved decision 3).

### 5. Migration veto
```text
can_migrate(daemon, claims) -> bool
    !claims.holds_exclusive(daemon)   // a claim-holder is migration-pinned
```
Consulted by the migration path. Drain of a claim-holder = release → re-claim → restart,
where the **re-claim is an ordinary Thunderdome claim** — it acquires destination islands via
§4 ordered-acquire, so drain-vs-drain (or drain-vs-gang) contention is resolved deadlock-free
by the existing protocol with no separate drain coordinator (resolved decision 4).

## Phasing

### Phase A — Desired-down projections (1 week)
Projections 1 + 2: task→intent, claim→forced-placement. **Done when:** a `Running(ActiveClaim)`
task starts a daemon on the claim's node, and a terminal task stops it — entirely through
`DesiredState`, with no workflow→dispatcher call in the path.

### Phase B — Observed-up projections (1 week)
Projections 3 + 4: lifecycle→step, liveness→topology. **Done when:** killing a worker daemon
fails its step and returns the island to `Free` within bounded time; a downed node's islands
disappear from matching.

### Phase C — Migration veto (3-5 days)
Projection 5. **Done when:** the migration path refuses to move a claim-holder, and a planned
drain of one performs release→re-claim→restart without ever double-holding the island.

## Test strategy

- **DST — claim + migration (gating).** Extend `loom_models.rs`: a claim-holding daemon
  targeted for migration mid-claim must be vetoed; a partition that strands a claim-holder
  must not let migration *and* reconcile both act. This is the interaction the single-box
  tests cannot reach.
- **Integration across the reconciliation boundary.** House pattern (memory transport, two+
  `NetNode`, subscribe-before-publish): project a `Running(ActiveClaim)` task → assert a
  `MeshOsAction` start on the claimed node; flip the task terminal → assert the stop. Verify
  the workflow never holds a dispatcher handle.
- **Daemon failure → claim release correctness.** A daemon `Failed` signal drives the step to
  `Failed` and `release_step` returns the island; assert the island is `Free` and re-claimable
  by another gang within bounded time. The stranded-GPU regression test.
- **Partition recovery semantics.** Split a claim-holder from the majority; on heal, assert
  the claim's island ended `Free` exactly once (no double-run survived — Thunderdome §6),
  the daemon state and the workflow task state agree, and no orphaned daemon kept running on
  the minority side.

## Locked decisions

#### 1. The claim wins over the drift scorer
A claim-pinned daemon is placed by forced placement on the `ActiveClaim`'s node and is
removed from the `PlacementScorer`'s domain. The drift scheduler and the gang claim never
both try to place the same daemon; the claim is authoritative for claim-bearing work.

#### 2. Migration-veto for exclusive claims
An exclusive `ActiveClaim` pins its daemon: migration is refused for the claim's lifetime.
Moving compute off the GPU it is using is either a double-book or an impossible re-claim;
drain is release→re-claim→restart, never live migration.

#### 3. The workflow writes desired state, never dispatches
The workflow emits `DaemonIntent` / forced placements into `DesiredState`. It has no
`ActionDispatcher` handle. This preserves reconcile convergence, supervision, backpressure,
and the audit chain, and prevents split-brain between "what the fold thinks runs" and "what
runs."

#### 4. A step cannot bypass the scheduler — already enforced by the type system
`drive_capability_step` has no reservation fold in scope; the only path to an exclusive
resource is `ClaimPipeline`. Bypass is impossible by signature, not by discipline. This plan
inherits that guarantee and must not introduce a side path that weakens it.

#### 5. No calls across layers — everything is state-projected
`workflow`/`gang` and `meshos` never import or call each other. All five connections are
projections at the desired/observed boundary. A direct call in either direction is a design
regression, not a shortcut — it recouples what the substrate keeps decoupled.

## Resolved design decisions

All four open questions are decided. Rules below are load-bearing, not advisory.

1. **`DaemonRef` is `(task_id, shard_index)` — never the attempt number. CLOSED.** A shard
   that fails → retries → replays must not read to reconcile as "old daemon died, new daemon
   must start" — that produces churn, log spam, and restart storms. Keying the ref on
   `(task_id, shard_index)` and excluding the attempt count makes a retry reconcile-invisible
   at the daemon layer: same ref, same desired state, no diff. Guarantees convergence across
   retries.

2. **Forced placements queue and replay first after a freeze — guaranteed, not best-effort.
   CLOSED.** Because the claim *is* the placement, a forced placement can never be dropped.
   While ICE/freeze is active, forced placements accumulate in `FrozenActions.forced_placements`;
   on thaw they replay **before** the drift scorer or the daemon diff. This preserves claim
   stability and monotonic placement correctness — no thaw can strand an `ActiveClaim`.

3. **Reserve TTL > 2× the MeshOS liveness sampling interval. CLOSED.** The standard
   liveness-vs-lease invariant: `TTL > 2 × (max liveness heartbeat interval + worst-case
   propagation)`. This guarantees the ordering — node dies → MeshOS marks it dead →
   projection 4 drops its islands → matching stops offering them — completes before the TTL
   could expire early, so a stale island can't be matched-and-claimed in the gap. Set once
   from the deployment's liveness config; not a per-workload knob. (Worked example: 1.5s
   liveness sample, <500ms fold-projection lag → 5s reserve TTL is safe and generous.) This
   constrains the Thunderdome reserve-TTL config — see Thunderdome §6 open question 3.

4. **Drain ordering is serial per-island via Thunderdome — no drain-specific arbitration.
   CLOSED.** There is no special drain mechanism: a drain is `release → re-claim → restart`,
   and the `re-claim` is an *ordinary* Thunderdome claim. If two drained daemons target the
   same destination island, Thunderdome arbitrates it like any other contention — no "drain
   priority," no "drain lock," no bypass of the gang scheduler. Deadlock-freedom is automatic
   and inherited, not engineered: **reserves are AP, `Active` is CP with the epoch-fence, so
   no two tasks ever reach `Active` on the same island** — drain contention therefore
   *resolves* rather than deadlocks. Per-island serialization is the single-writer CAS;
   cross-island ordering is the §4 lock-ordering. **Rule: drain ordering = the scheduler's
   natural contention resolution; do not invent drain-specific arbitration.**

## See also
- [`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md) — the claim whose
  `ActiveClaim` node drives forced placement; §6 partition semantics this plan's recovery
  test asserts against.
- [`TASK_LIFECYCLE_PLAN.md`](TASK_LIFECYCLE_PLAN.md) — the task state projected into
  `DaemonIntent`; the failure-propagation path lifecycle signals trigger.
- [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) — the drift `PlacementScorer` that
  coexists with, and yields to, claim-pinned placement.
- `.claude/skills/net-event-bus/testing.md` — the house test harness.
