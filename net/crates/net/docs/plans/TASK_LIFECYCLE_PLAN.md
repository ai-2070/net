# Task Lifecycle — implementation plan

> The workflow layer that runs *on top of* a held resource. Where
> [`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md) (Thunderdome)
> decides *who atomically gets an exclusive capability under contention*, this doc plans what
> happens *after* it is held: task state, dependencies, fan-out/fan-in, retries, branching,
> DAGs — all as **emergence from RedEX primitives**, no workflow engine. Companion to
> [`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md) (the claim it
> sits on), [`MESHOS_PLAN.md`](MESHOS_PLAN.md) (triggers + fan-out execution), and
> [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) (the fold these states live in).
> **Time release** (Pink Floyd) — tasks advance one tick at a time, and because every tick
> is in the log, you can always rewind.

## Status

Design only. **Strictly downstream of Thunderdome** — this layer assumes the resource is
already atomically held; it never arbitrates contention itself.

Prerequisites:
- **Thunderdome Phases C + D** (multi-resource atomic claim + partition-safe `Active`) —
  **hard** prerequisite for the *capability-bearing* paths only. A step that requires an
  exclusive capability must obtain it through the Thunderdome claim pipeline; building those
  paths before the claim is proven yields a workflow engine that double-books exclusive
  resources the first time two jobs contend. Hardware-free tasks (Phases A–C, E) have no such
  dependency and can proceed independently.
- ~~**RedEX directories + single-writer chains** (`redex/`).~~ ✅ Landed.
- ~~**CortEX fold/replay/tail** (`behavior/meshos/`, `CORTEX_ADAPTER_PLAN.md`).~~ ✅ Landed.
- ~~**nRPC streaming + capability-gated tools** (`nrpc`).~~ ✅ Landed — step execution.

Activation gate: a workload with multi-step jobs — dependencies, map-reduce fan-out,
conditional branches, resumable long-running pipelines — i.e. the first user who wants
more than "run one tool once."

## Frame

A task is a RedEX directory with a single-writer chain. Workers advance an explicit cursor;
schedulers read it. **All workflow semantics emerge from a handful of primitives — leases,
cursors, triggers, shard directories, blob refs — with no DAG DSL, no controller loops, no
Airflow-style machinery.** (Source: Kyra's spec, 6/11.)

The boundary that defines this plan: **this layer coordinates *task execution*, never
*resource contention*.** The task lease here is the *easy* lease — one task, at most one
owner, AP all the way down, no two contenders for the same id. That is a categorically
different problem from Thunderdome's *exclusive-capability* claim (N contenders, one
resource, CP on commit). Conflating the two is the central error this plan is written to
prevent — see [Locked decisions 1 and 2](#locked-decisions).

**Reuses existing primitives:** RedEX directories + single-writer chains, the CortEX fold,
MeshOS triggers + fan-out, nRPC streaming, content-addressed blob refs. **Adds:** the task
state machine, the trigger→spawn wiring, and the shard lease/cursor convention. No new
distributed-systems infrastructure.

## Why this exists

1. **Emergence needs to be written down to stay disciplined.** "DAGs are tasks spawning
   tasks; dependencies are trigger files" is correct and powerful — and exactly the kind of
   elegance that rots into an ad-hoc workflow engine if the primitives aren't pinned. This
   doc pins them so nobody reintroduces a controller loop.
2. **The Thunderdome seam must be explicit.** A step that requires an exclusive capability
   is the one place this layer touches contention, and it must do so *only* by calling the
   Thunderdome claim pipeline. Left implicit, someone wires a step straight to a
   `ReservationFold` append and bypasses the atomicity/partition guarantees.
3. **Two of the source spec's rules are wrong if read as covering resources.** They are
   correct for task coordination and dangerous for resource contention; they are corrected
   as locked decisions below rather than silently dropped, because they are the *seductive*
   errors.

## What ships

Seven pieces, in dependency order:

1. **Task state machine** — `task/<id>/state.json` `{ step, status, attempts }`; explicit
   cursor, single-writer chain. Workers advance; schedulers read.
2. **Task lease** — `task/<id>/lease.json`; ownership of *executing* a task, failover on
   owner death. Single-resource (easy) lease — a `ReservationFold` claim at task-id
   granularity, **not** an exclusive-capability claim.
3. **Triggers** — timestamp / interval / dir-change / capability-arrival / node join-leave /
   blob-replicated / `after_task:<id>` / `if_result:<path matches>`. The substrate of
   dependencies, branches, and DAGs.
4. **Shards (fan-out/in)** — `task/<id>/shards/<k>/` each with its own lease + cursor (map);
   a reduce step gated on `all shards/* == done` (join).
5. **Steps = tools** — `{ tool, input }`, JSON-Schema typed, capability-gated, nRPC
   streaming. **Steps that require an exclusive capability acquire it via the Thunderdome
   claim pipeline and do not run until an `Active` claim handle is held** — the one
   cross-plan contract.
6. **Retry / cancel / checkpoint** — `retry` in state (worker-enforced); cancel = write
   `task/<id>/cancel.json`; checkpoints = content-addressed `results/stepN.out.ref`.
7. **Lifecycle / observability** — `events/*.json`, `metrics/*.json`; delete `task/<id>/`
   reclaims the subtree (no sweeper).

What this doc does NOT ship:
- **Any resource arbitration.** All contention, multi-resource atomicity, and partition-safe
  commit live in Thunderdome. This layer calls it; it never reimplements it.
- **A DAG DSL / controller loops / multiphase-commit machinery.** Semantics emerge from
  primitives (see [Locked decision 3](#3-emergence-over-engines)).
- **Cross-task atomicity.** RedEX has no cross-chain atomicity; this layer must not imply it.

---

## Design

### 1. State machine
```rust
pub enum TaskStatus { Submitted, Running, Waiting, Blocked, Done, Failed }
pub struct TaskState { pub step: u32, pub status: TaskStatus, pub attempts: u32 }
```
Deterministic fold: same chain → same state. Time enters only as explicit `Tick` events
(never `now()` inside the fold), preserving replay.

### 2. Emergent semantics (no engine)
- **Replay** — rewind the cursor, clone the dir to a new id, or rewind to step N.
- **Dependencies** — `trigger: after_task:<id>` fires when that cursor hits `Done`.
- **Fan-out / fan-in** — shard dirs with independent leases/cursors; reduce gated on
  `all shards/* done`.
- **Branching** — triggers keyed on `results/*.ref` contents.
- **DAGs** — a step writes `task/<new-id>/spec.ref`; MeshOS observes and triggers it.

### 3. The Thunderdome seam (the one integration point)
```text
step requires an exclusive capability
  └─ Thunderdome match→claim pipeline   (returns an Active claim handle, or reject)
       └─ Active handle held?  yes → run step      no → step stays Waiting, re-requests
```
The task layer never appends to `ReservationFold` directly and never reads capability or
topology folds for placement — it hands the requirement to Thunderdome and waits on the
claim result. This is the entire cross-plan contract.

## Phasing

### Phase A — State machine + lease + replay (1–2 weeks)
Fold, cursor, task lease, replay. **Done when:** submit a task, watch states advance,
confirm a replay reproduces them; kill the owner, confirm failover.

### Phase B — Triggers + dependencies (1–2 weeks)
Trigger engine; `after_task` / `if_result`. **Done when:** task B auto-starts on A's
`Done`; a result-conditioned branch fires.

### Phase C — Shards (fan-out/in) (1–2 weeks)
Shard dirs + independent leases; reduce join. **Done when:** a map-reduce runs with
per-shard retry and a correct join.

### Phase D — Capability-bearing steps (1 week) — *gated on Thunderdome C/D*
Steps requiring an exclusive capability route through the claim pipeline. **Done when:** a
step requiring an exclusive resource runs only after an `Active` claim handle is returned,
and a claim reject leaves the step `Waiting`, not running.

### Phase E — Retry / cancel / checkpoint / observability (1 week)
Mechanical; mostly cross-binding.

## Test strategy
- **Unit** — fold determinism; trigger predicate evaluation; shard join condition.
- **Integration** — house pattern: memory transport, two+ `NetNode`, subscribe-before-publish,
  deterministic `shutdown` (`.claude/skills/net-event-bus/testing.md`).
- **Replay** — same chain replays to identical state across process restarts.
- **Seam** — a capability-requiring step with a forced claim-reject stays `Waiting` and never
  executes; pin that a step can never bypass Thunderdome to touch a `ReservationFold`
  directly.

### Performance

Two scale hotspots — **notes, not gates** (no go/no-go number; design to avoid the cliff):

- **Replay/catchup scales with event volume.** Task lifecycle emits far more events per
  task than a memory/reservation fold — every state transition, shard update, retry. The
  0.14.0 O(N²)→O(N) replay fix bought headroom; this layer spends it. Bound failover
  replay with periodic checkpoint/snapshot per task chain so resuming a long-running job
  doesn't re-fold its entire history.
- **Trigger evaluation fan-out.** Naively, every capability announcement / blob-replicate /
  `after_task` completion re-scans every waiting trigger — O(triggers × events). Index
  triggers by what they wait on (task-id, result path, capability tag) so a fired event
  touches only the triggers keyed to it. Hotspot at high task counts, not at demo scale.

## Locked decisions

#### 1. The task lease is not the resource claim
`task/<id>/lease.json` is the easy single-resource lease (one task, one owner, AP).
Exclusive-capability contention is Thunderdome's CP `Active`. These are different leases with
different consistency; never let the task lease stand in for resource arbitration.

#### 2. "Zero worker comms ⇒ no deadlocks" is true for tasks, false for resources
Correct for *task* coordination (workers coordinate only via written state). It does **not**
prevent *resource* deadlock — dining philosophers don't communicate; that's the cause, not
the cure. Multi-resource claim deadlock is prevented by Thunderdome's ordered-acquire, not by
this layer's stateless workers. (Corrects source-spec #12.)

#### 3. Emergence over engines — but multiphase commit is not "noise"
DAGs/dependencies/joins emerge from primitives; no DSL or controller loops here. **However**,
two-phase reserve→commit and ordered-acquire are *load-bearing* in Thunderdome §4 — they are
the only deadlock-free multi-resource claim, not Kubernetes cruft. This plan forbids workflow
engines; it does **not** forbid Thunderdome's commit protocol. (Corrects source-spec #14.)

#### 4. `requires_capability` is a filter, not a claim
A step's capability requirement lowers to a Thunderdome match (`CapabilityQuery::Composite` +
any scheduler-side filter) — a *match*. The claim is the subsequent CAS. A hint is never a
hold. The lifecycle layer states the requirement; it never evaluates placement itself.

## See also
- [`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`](MESH_SCHEDULER_GANG_CLAIM_PLAN.md) — the claim this
  layer sits on; §2 match→claim pipeline is the seam.
- [`MESHOS_PLAN.md`](MESHOS_PLAN.md) — triggers + fan-out execution.
- [`CORTEX_ADAPTER_PLAN.md`](CORTEX_ADAPTER_PLAN.md) — the fold task state lives in.
- `.claude/skills/net-event-bus/testing.md` — the house test harness.
