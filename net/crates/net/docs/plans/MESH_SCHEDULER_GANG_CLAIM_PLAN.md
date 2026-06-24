# GPU Gang-Claim Scheduler — implementation plan

> Contended resource arbitration on top of the substrate's `ReservationFold`. Where
> [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) keeps *daemon placements* optimal
> *over time* (score drift → migration), this doc plans the orthogonal problem it never
> touches: deciding **who wins a contended GPU island, atomically, right now** — and not
> double-booking it across a partition. Companion to [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md)
> (placement drift), [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) (whose
> capability fold supplies the coarse match), and [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md)
> (whose single-leader/quorum-irrelevant replication this plan gates `Active` commits
> against). **Thunderdome release** — two gang jobs enter contention, one claim leaves.

## Status

Design only. **Most of the hard primitive already exists** — the work is a claim
*protocol* over it, not new distributed-systems infrastructure.

Prerequisites:
- ~~**`ReservationFold`** (`behavior/fold/reservation.rs`) — per-`ResourceId` CAS state
  machine `Free → Reserved → Active`, foreign claim of a held resource rejected unless
  TTL-expired.~~ ✅ Landed. This is the claim primitive.
- ~~**Capability fold + `CapabilityQuery::Composite`** (`behavior/fold/capability.rs`) —
  tag AND/OR (`tags_all` / `tags_any` / `tag_groups_all`) for the coarse match.~~ ✅ Landed.
- ~~**`contested/` reconcile** (`contested/{partition,correlation,reconcile}.rs`) —
  partition detect + post-heal `ConflictResolution::Winner`.~~ ✅ Landed.
- ~~**`PlacementStrategy::ColocationStrict`** (`redex/replication_config.rs`).~~ ✅ Landed —
  reused here to keep an island's replica set in one fault domain.
- **NEW, this plan:** multi-island atomic gang claim; quorum-witnessed `→ Active` +
  fencing epoch; `IslandTopology` fold; the match→claim pipeline; selection policy.

Activation gate: a GPU fleet running **contended multi-GPU gang jobs** where partial
allocation deadlocks and a cross-DC partition can double-run a job. Realistic trigger:
a neocloud whose margin is utilization and whose jobs are tensor/pipeline-parallel
(multi-GPU, all-or-nothing).

## Frame

`PlacementFilter` and the mesh scheduler answer *"where should this daemon live, and is
it still the best home?"* — placement over time, single-resource, no contention (only
the home node acts). This plan answers a different question: *"three gang jobs want the
same four-GPU NVLink island this microsecond — who gets it, atomically, and what happens
when the network splits mid-claim?"* That's contended resource arbitration, and it is
exactly the case the existing scheduler's "only the home node decides" model does not
cover, because here there is no single home — there are N contenders and one resource.

The architectural posture is unchanged from the rest of the substrate: **no central
coordinator.** Matching is a local read; the claim is a CAS against a single-writer
chain; arbitration falls out of the chain's total order. The one new concession to
physics: starting a GPU job is irreversible, so the `→ Active` edge — and *only* that
edge — gets strong consistency. Everything else stays AP. The cost model follows
directly: match and `Reserved` are microsecond local-AP operations on the hot path;
`→ Active` is the sole quorum (millisecond) operation, and it is rare and amortized
against minutes-to-hours of GPU runtime. The slow edge being slow is the design, not a
regression to optimize away.

**Reuses existing primitives:** `ReservationFold`, the capability fold + `CapabilityQuery`,
the causal-chain / `generation` machinery, single-leader RedEX replication, `contested`
reconcile, `ColocationStrict` placement, MeshOS fan-out, Mikoshi. **Adds:** a gang-claim
protocol, a quorum-gate on `→ Active`, a fencing epoch (riding the causal chain), and the
`IslandTopology` fold.

## Why this exists

Three load-bearing reasons:

1. **The reservation CAS is per single `ResourceId`; gang atomicity does not compose for
   free.** Four-GPU-or-none across an island, with two jobs grabbing overlapping subsets,
   is dining philosophers — partial holds that deadlock. The single-resource CAS is
   correct and is *not* the missing piece; the missing piece is the all-or-none protocol
   over N of them.
2. **RedEX is deliberately AP ("quorum-irrelevant"); under partition both sides can reach
   `Active` and both start the job on real hardware.** Reconcile fixes the *record* after
   healing; it cannot un-run compute. The fix is to make exactly one transition CP, not
   to make the log CP — see [Locked decision 2](#2-cp-on-active-only-ap-everywhere-else).
3. **DST is the gating concern, not LoC.** Correctness lives entirely in partition +
   contention sequences that don't surface in happy-path tests. Allocate ~30% of effort
   to extending the DST harness (`loom_models.rs`) with gang-contention and
   partition-during-claim scenarios; treat it as a precondition, not an afterthought.

## What ships

Eight pieces, in dependency order:

1. **`IslandTopology` fold** (CortEX) — folds capability announcements into island →
   {gpu set, nvlink domain, host, warm models, live load, p50 latency}. Carries the
   **live numeric axes** deliberately kept out of the capability index (they churn).
2. **Match→claim pipeline** — `CapabilityQuery::Composite` coarse prefilter →
   scheduler-side numeric filter over `IslandTopology` → selection → `ReservationFold`
   claim. Matching only ever produces a *set*; only the CAS commits.
3. **Single-island gang claim** — island *is* the `ResourceId`; a single-island gang is
   one existing CAS. Deadlock-impossible by construction.
4. **Multi-island gang protocol** — ordered-acquire + bounded backoff (lock-ordering on
   `ResourceId` ascending) *or* two-phase reserve→commit. The only genuinely new
   distributed-systems work. See [§4](#4-multi-island-gang-protocol).
5. **`ColocationStrict` island placement rule** — pin each island's replica set inside
   one partition/fault domain so a cross-DC split never bisects an island's quorum.
6. **Quorum-witnessed `→ Active` + fencing epoch** — `Active` commits only with a
   majority of the island's replica set; epoch (on the causal chain) fences a stale
   ex-leader's late `Active`. See [§6](#6-partition-safe-active-commit).
7. **Selection policy** — pure fn over `IslandTopology` + reservation state (pack/spread,
   warm-model affinity, load-band).
8. **Queue / retry / backpressure** — workflow layer; the `Reserved` reject loops back to
   match. Reuses `StreamError::Backpressure`.

What this doc does NOT ship (deferred):
- **Escrow / capacity partitioning** — a zero-coordination partition-mode fallback
  (each side claims only its disjoint escrow). Elegant but trades partition-time
  utilization and has an unsolved heal-time reclaim; parked until a workload needs
  graceful partition-mode operation rather than fail-closed. See [§7](#7-escrow-deferred).
- **Sub-island fractional claims** — claiming 2 of a 4-GPU island shared by two jobs.
  Breaks island-as-`ResourceId`; revisit only if the real job mix demands it (verify
  against neocloud workload data first — [open question 1](#open-design-questions)).
- **Request-level / token scheduling** — that's the inference engine (vLLM/SGLang), a
  different layer; this plan stops at "the island is atomically held."
- **Task lifecycle / workflow semantics** — leases, cursors, triggers, shards, DAGs run
  *on top of* a held island and live in [`TASK_LIFECYCLE_PLAN.md`](TASK_LIFECYCLE_PLAN.md)
  (Time). That plan's GPU-bearing steps call this plan's §2 match→claim pipeline;
  the seam is one-directional (lifecycle depends on claim, never the reverse).

---

## Design

### 1. `IslandTopology` fold

```rust
/// island id = hash(host, nvlink_domain); the unit gang jobs actually want.
pub type IslandId = u64; // used directly as the ReservationFold ResourceId

pub struct IslandRecord {
    pub id: IslandId,
    pub gpus: GpuSet,            // members of the NVLink domain
    pub host: NodeId,
    pub warm_models: SmallVec<ModelId>,
    pub load: f32,              // LIVE, fast-changing — fold updates on heartbeat
    pub p50_latency_us: u32,    // LIVE
}
```

Folded from capability announcements (CortEX). The numeric axes (`load`,
`p50_latency_us`) live here and **not** in the capability index — they churn every
heartbeat; baking them into signed/replicated capability tags causes tag-churn and stale
reads ([Locked decision 4](#4-match-narrows-cas-commits)).

### 2. Match→claim pipeline

```text
affinity hint
  └─[1] CapabilityQuery::Composite  → candidate island set      (capability fold, read)
       └─[2] numeric filter         → tightened set             (IslandTopology, read)
            └─[3] select            → ordered island list        (pure fn, read)
                 └─[4] ReservationFold CAS                       (COMMIT — the only one)
```

Steps 1–3 are read-only and cheap; safe to run optimistically and re-run on reject.
`call_typed_where` / `Predicate` are **not** used for placement — that path is
receiver-evaluates-and-refuses (fire-and-match), which does not arbitrate
([Locked decision 4](#4-match-narrows-cas-commits)).

### 3. Single-island gang claim

Island is the `ResourceId`, so a single-island gang is one `ReservationFold` claim —
atomic and deadlock-free with zero new code. Most jobs land here; the protocol in §4 is
only for gangs spanning islands.

### 4. Multi-island gang protocol

```rust
pub struct GangClaim { pub job: JobId, pub islands: SmallVec<IslandId>, pub deadline_us: u64 }
```

**Option 4a — ordered acquire + bounded backoff.** Acquire islands in ascending
`IslandId` order (global lock-ordering ⇒ no deadlock). On any `Reject`, release all held
(holder-only release is already legal in `ReservationFold`) and back off. Cost: latency
under contention. Default.

**Option 4b — two-phase reserve→commit.** `Reserved` all islands (short TTL), then
`→ Active` all iff every reserve held; else release. The fold's TTL-takeover reaps
abandoned reserves. Cost: a per-gang commit point.

Ship 4a; keep 4b behind a flag for gangs whose island count makes ordered-acquire
backoff pathological.

### 5. `ColocationStrict` island placement

Pin each island's RedEX replica set with `PlacementStrategy::ColocationStrict` to one
fault domain. Consequence: a cross-DC partition leaves an island wholly on one side — the
far side can't see it to claim it, and the §6 quorum is LAN-local (sub-ms). Do this
first; it makes §6 affordable.

### 6. Partition-safe `Active` commit

`Reserved` stays AP/optimistic (revocable; reconcile discards a losing reserve, nothing
spent). Gate **only** `→ Active`:

```text
leader may emit Active(island, job, epoch) iff
    majority(island.replica_set) ack the commit       // quorum witness
replica accepts Active iff
    epoch >= highest epoch it has witnessed             // fence stale ex-leader
```

Minority side of a split → no majority → no `Active` → no double-run; job stays
`Reserved`, caller re-queries. The **epoch rides the causal chain / `generation`
machinery already in `ReservationFold::merge`** — do **not** add a parallel Raft term
([Locked decision 3](#3-fence-on-the-causal-chain-not-a-new-term)).

### 7. Escrow (deferred)

Pre-split each island's GPUs into per-zone escrow; during a partition each side claims
only its disjoint escrow — no double-book, zero coordination, zero stall. Deferred:
costs partition-time utilization and the heal-time remainder-reclaim is unsolved without
a coordination round.

---

## Phasing

### Phase A — Topology + pipeline (1–2 weeks)
`IslandTopology` fold; match→claim pipeline (steps 1–3 + single-island CAS). **Done when:**
submit a job, watch it claim one island via the existing CAS, run, release.

### Phase B — Single-island contention (1 week)
Wire gang-of-one-island onto `ReservationFold`. **Brutal test #1:** N daemons, M islands
oversubscribed, sustained → exactly one winner per island per round, losers `Reject` +
re-query, zero partial holds.

### Phase C — Multi-island gang protocol (2–3 weeks) — *the core*
Option 4a (ordered-acquire + backoff). **Brutal test #2:** two multi-island gangs,
overlapping islands, sustained, + node killed mid-claim → single-winner, **bounded retry,
zero deadlock, zero livelock**. This is the difference between "another scheduler" and
the thing nobody else made work.

### Phase D — Partition-safe Active (2 weeks) — *the correctness bar*
`ColocationStrict` island placement; quorum-`Active`; fencing epoch on the causal chain.
**Brutal test #3:** split an island's replica set, both sides attempt `→ Active`, heal →
**at most one side ever reached `Active`**; minority never started compute; fence rejected
the late ex-leader `Active`. This is the test KAI/Run:ai/Dynamo can't pass by construction.

### Phase E — Selection + queue + proof (2 weeks)
Selection policy; queue/retry/backpressure. Then the raise-able proof: real GPUs, gang
jobs contending, node killed mid-stream, partition injected; report single-winner /
bounded-retry / zero-deadlock **+ an MFU or tail-latency delta vs a baseline** (Volcano /
Kueue gang plugin).

DST harness work (~30%) runs across C and D, not after.

## Test strategy

- **Unit** — `ReservationFold` transition gating; ordered-acquire lock-ordering;
  epoch-fence comparison. Pure fns, pure tests.
- **Integration** — house pattern: memory transport, two+ `NetNode` in one process,
  subscribe-before-publish, deterministic `shutdown` teardown (`.claude/skills/net-event-bus/testing.md`).
- **Property** — for any interleaving of K gangs over J islands: no two `Active` claims
  share a GPU; every gang either fully holds or holds nothing.
- **DST (gating)** — extend `loom_models.rs`: gang contention, partition-during-claim,
  leader flap mid-`Active`, reserve TTL takeover. Partition + contention correctness is
  DST's job; it ships *with* Phases C/D, not after.

### Performance

Three budgets that are **go/no-go gates**, not optimization targets. State the bar
relative to a real quantity; let telemetry fill the absolute number (cf.
`MESH_SCHEDULER_PLAN.md`: thresholds are guesses without a baseline).

- **Quorum-`Active` latency vs. gang deadline (usability gate).** The majority round-trip
  on `→ Active` must complete in a small fraction of the gang's deadline, measured
  LAN-local under `ColocationStrict`. If it doesn't, the scheduler is correct and
  unusable. This is the primary justification for ranking §5 (`ColocationStrict`) first —
  it keeps the quorum on one LAN. Measured in Phase D; pass bar set against the target
  workload's deadline, not an absolute µs figure.
- **Reject rate / progress under contention (correctness-adjacent gate).** The optimistic
  match→claim has a stale-read window (read topology → claim → world moved → reject →
  retry). Gate: at target utilization, the reject rate stays bounded and backoff
  guarantees progress — no retry storm onto a momentarily-attractive island, no
  multi-island livelock. This is a *does-it-make-progress* question, so it lives in the
  DST/load characterization as a fail condition, not a throughput nice-to-have.
- **Cost asymmetry (stated invariant).** `Reserved` is local-AP (microsecond, leaderless);
  `→ Active` is the *only* operation that pays quorum (millisecond), and it is rare and
  amortized against minutes-to-hours of GPU time. This asymmetry is the architecture, not
  an accident — do not "optimize" by making `Reserved` consistent, and do not treat
  `Active` quorum cost as a regression. See [Locked decision 2](#2-cp-on-active-only-ap-everywhere-else).

## Locked decisions

#### 1. Island is the `ResourceId`
The NVLink domain is the claim unit; single-island gangs reduce to one existing CAS.
Revisit only if workload data shows fractional sub-island demand is the common case.

#### 2. CP on `Active` only, AP everywhere else
Do not make RedEX globally CP — it would impose quorum cost on the millions of operations
that never see a partition. `Reserved` is CRDT-grade AP; `Active` is CP. The reservation
state machine already drew the line; enforce different consistency on each side of it.

#### 3. Fence on the causal chain, not a new term
The leadership epoch rides the existing causal-chain / `generation` machinery in
`ReservationFold::merge`. No parallel Raft/Paxos term. One consensus mechanism, reused.

#### 4. Match narrows, CAS commits
`CapabilityQuery::Composite` (tags) + scheduler-side numeric filter produce a *candidate
set*. `call_typed_where` / `Predicate` are call-targeting (fire-and-match), never used
for placement. A match result is never treated as a hold.

#### 5. `ColocationStrict` before quorum
Pin island replica sets to one fault domain first; it turns the cross-DC partition into a
non-event and makes the quorum LAN-local. Placement is the cheap mitigation; quorum is
the guarantee.

## Open design questions

1. **Fractional vs whole-island job mix** — island-as-`ResourceId` is free money if jobs
   are whole-island/whole-node, and moves the hard case into the hot path if they're
   fractional. Verify against real neocloud workload data before Phase C.
2. **Island-writer failover latency** — when the node owning an island's chain dies, the
   new-writer handoff must complete under a gang claim's deadline. Measure against the
   nearest-RTT election's convergence.
3. **`Reserved` TTL sizing** — derive from claim-round latency; too short → false
   takeovers under load, too long → slow dead-claimant recovery.
4. **Quorum-`Active` latency budget** — promoted to a stated go/no-go gate under
   [Performance](#performance); the open part is only the absolute number, which Phase D
   telemetry fills against the target workload's gang deadline.
5. **Reconcile only sees reserve-level forks** — with §6, no `Active`-vs-`Active` fork can
   exist; pin a test that it is *unreachable*, not merely resolved.

## See also
- [`TASK_LIFECYCLE_PLAN.md`](TASK_LIFECYCLE_PLAN.md) — the workflow layer (Time)
  that runs on top of a held island; consumes §2's match→claim pipeline.
- [`MESH_SCHEDULER_PLAN.md`](MESH_SCHEDULER_PLAN.md) — placement drift over time (orthogonal).
- [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md) — the single-leader/quorum-irrelevant
  replication §6 gates against; nearest-RTT election; `ColocationStrict`.
- [`CAPABILITY_SYSTEM_PLAN.md`](CAPABILITY_SYSTEM_PLAN.md) — capability fold + `CapabilityQuery`.
- `.claude/skills/net-event-bus/testing.md` — the house test harness this plan's tests use.
