# Net v0.28 — "The Final Countdown"

*Named after Europe's 1986 title track — the song Joey Tempest built from a synth riff he'd
carried on a borrowed keyboard for years, conceived not as a single but as the fanfare that
opened the show, the lights down and the crowd already on its feet before a word is sung.
The lyric is a departure: leaving the ground for somewhere there's no coming back from.*

## A transport substrate becomes a scheduler

v0.28 is the largest single feature arc the codebase has landed: the contended-resource
gang-claim scheduler, the task-lifecycle / workflow engine that sits on top of a held
resource, the five-language SDK that surfaces both, the post-audit hardening pass that the
branch review forced, and the MeshOS↔scheduler bridge that wires *deciding* to *running*.
Five branches, ~110 commits, ~10K lines of new Rust plus the binding tiers.

The organizing observation is the same one that has shaped every release since the substrate
stopped being a prototype, turned up to its loudest setting: **the hard distributed-systems
primitives already existed — the work was a protocol over them, not new infrastructure.**
The gang claim is a protocol over the `ReservationFold` CAS that shipped releases ago. The
task lifecycle is a state machine *emergent from* RedEX directories, single-writer chains,
and the CortEX fold — no workflow engine, no DAG DSL, no controller loop. The MeshOS bridge
is five pure *projections* across a boundary two existing desired-state machines already
sat on either side of, neither one calling the other. Nothing here invents a new fold, a new
transport, or a new consensus. v0.28 is assembly — and the discipline is in refusing to
build the engine the elegance keeps threatening to rot into.

The one genuinely new concession is to physics. The reservation state machine has always had
three states — `Free → Reserved → Active`. v0.28 is the release that finally treats the
`→ Active` edge as different in kind from everything around it, because the thing on the
other side of it — a GPU job, real compute, real cost — is the one thing the substrate
cannot reconcile away after a partition heals. So that edge, and *only* that edge, pays for
consensus.

Below: the wins, grouped by the layer they land in.

---

## The gang-claim scheduler — contended exclusive-resource arbitration

The existing placement scheduler answers *"where should this daemon live, and is it still
the best home?"* — placement over time, single-resource, no contention, only the home node
acting. v0.28 answers the orthogonal question it never touched: **"three gang jobs want the
same four-GPU NVLink island this microsecond — who gets it, atomically, and what happens
when the network splits mid-claim?"** That is contended arbitration: N contenders, one
resource, and the case the "only the home node decides" model does not cover, because here
there is no home — there are claimants.

The claim unit is generic — a GPU island, an accelerator slot, a licensed seat, any
contended *exclusive* resource — with the multi-GPU NVLink island as the motivating case.
It lands in `behavior/gang/` plus the new `IslandTopology` fold, wired onto `MeshNode`.

**`IslandTopology` fold (CortEX).** Folds capability announcements into `island → {unit set,
host, resident capabilities / warm models, live load, p50 latency}`. It deliberately carries
the **live numeric axes** — `load`, `p50_latency_us` — that are kept *out* of the signed
capability index precisely because they churn every heartbeat; baking churning numbers into
replicated capability tags causes tag-churn and stale reads. The island id *is* the
`ReservationFold` `ResourceId`, which is what makes a single-island gang reduce to one
existing CAS.

**Match→claim pipeline — match narrows, CAS commits.** A claim is a four-step funnel:
`CapabilityQuery::Composite` coarse match over the capability fold (read) → numeric filter
over `IslandTopology` (read) → selection (pure fn) → `ReservationFold` CAS (**the only
commit**). Steps 1–3 are read-only and cheap, so they run optimistically and re-run on a
reject with nothing spent. A match result is never treated as a hold — the candidate set is
a hint; the CAS is the decision.

**Single-island gangs are deadlock-impossible by construction.** Island-as-`ResourceId`
means the common case — one job, one NVLink domain — is a single atomic CAS with zero new
code and no protocol at all.

**Multi-island gangs use ordered-acquire.** Islands are claimed in ascending `IslandId`
order — a global lock-ordering that makes deadlock impossible — with bounded backoff and
holder-only release-all on any reject, preserving the documented "holds the full set or
nothing" invariant. The two-phase reserve→commit alternative (Option 4b) stays deferred
behind a *measured* condition — gang reject rates exceeding ~30% at 8+ islands under
sustained contention — rather than a premature flag. Until that signal fires, ordered-acquire
is the whole story and no 4b seam exists in the code.

**Partition-safe `→ Active` — the one CP edge.** `Reserved` stays AP/optimistic (revocable;
reconcile discards a losing reserve, nothing spent). Only `→ Active` is gated, two ways:

- **Quorum witness.** A leader may commit `Active` only with a strict majority of the
  island's replica set acking (`acks*2 > n`, empty-set guarded). The minority side of a
  split can never gather a majority → never reaches `Active` → never double-runs; its job
  stays `Reserved` and the caller re-queries.
- **Fencing epoch.** The leadership epoch **rides the existing causal-chain `generation`**
  in `ReservationFold::merge` — no parallel Raft term, one consensus mechanism reused. A
  stale ex-leader's late `Active` is fenced out by a higher witnessed epoch; the fence is
  monotonic (`>=`). And a *live* `Active` is never displaced by a higher-epoch newcomer —
  the install is holder-gated, so the newcomer gets `LostReservation`. At most one side ever
  holds `Active`.

**`ColocationStrict` makes the quorum affordable.** Each island's replica set is pinned to
one fault domain, so a cross-DC partition leaves an island wholly on one side — the far side
can't even see it to claim it, and the quorum round-trip stays LAN-local (sub-millisecond).
Placement is the cheap mitigation that turns the cross-DC split into a non-event; quorum is
the guarantee underneath it.

**Selection policy.** A pure `policy_cmp` over `IslandTopology` + reservation state:
`LeastLoaded` (spread — the default), `Pack`, `LoadBand(target)`, `LowestId`, with NaN-safe
float ordering throughout. Soft capability affinity (`prefer_capability`, e.g. a warm model)
layers on top of any of them via `select_with_affinity`, distinct from the hard
island-resident filters. Network locality — subnet / region / AZ — is a property of the
*node*, not of an NVLink domain inside it, so it rides the step-1 host match
(`CapabilityFilter::region`) and is kept out of `IslandRecord` by design, keeping the two
folds aligned with physical reality.

**The cost asymmetry is the architecture, not a regression.** Match and `Reserved` are
microsecond, leaderless, local-AP operations on the hot path. `→ Active` is the *sole*
operation that pays quorum (milliseconds), and it is rare and amortized against
minutes-to-hours of GPU runtime. The slow edge being slow is the design. Do not "optimize"
by making `Reserved` consistent.

**Verified by the brutal tests, a property test, and a loom DST model.** Single-island
contention (exactly one winner per island per round, zero partial holds); a two-gang
overlapping-island race with a node killed mid-claim (single winner, **bounded retry, zero
deadlock, zero livelock**); a split island replica set with both sides attempting `→ Active`
(at most one side ever reached `Active`, the minority never started compute, the fence
rejected the late ex-leader). The property test pins that for any interleaving of K gangs
over J islands no two `Active` claims share a unit and every gang holds all-or-nothing. The
loom model exercises ordered-acquire deadlock-freedom *and* partition-during-claim fencing
under every interleaving.

**Honestly deferred.** The quorum-ack path is built and tested against an in-process
`ReplicaCohort` that models the split via a reachable subset; wiring those acks to an on-wire
RPC round-trip — and riding the epoch on a durable per-island generation that survives a
leader restart — is the one remaining Phase-D integration. The raise-able **hardware proof**
(real GPUs, node killed mid-stream, a partition injected, an MFU / tail-latency delta vs.
Volcano / Kueue) needs a fleet, not code. Escrow and sub-island fractional claims remain
deferred exactly as planned.

---

## Task lifecycle — workflow semantics as emergence, not an engine

The layer that runs *on top of* a held resource: task state, dependencies, fan-out/fan-in,
retries, branching, DAGs. The discipline that defines it: **every one of those semantics
emerges from a handful of primitives — leases, cursors, triggers, shard directories, blob
refs — with no DAG DSL, no controller loop, no Airflow-style machinery.** "DAGs are tasks
spawning tasks; dependencies are trigger files" is correct and powerful, and exactly the
kind of elegance that rots into an ad-hoc workflow engine if the primitives aren't pinned.
It ships as the `cortex::workflow` CortEX model — **32 tests** under strict lib clippy,
test-target clippy, and rustdoc `-D warnings`.

**State machine.** `TaskStatus { Submitted, Running, Waiting, Blocked, Done, Failed }` over
`TaskState { step, status, attempts }`. The fold is deterministic — same chain, same state —
because time enters *only* as explicit `Tick` events, never `now()` inside the fold. That is
what makes failover replay reproduce the exact state across process restarts.

**The task lease is the *easy* lease — and categorically not the resource claim.**
`task/<id>/lease.json` is one-task-one-owner, AP all the way down, with failover on owner
death. It is a `ReservationFold` claim at task-id granularity. It is **not** the gang
scheduler's exclusive-capability `Active` — different consistency, different problem.
Conflating the two is the central error the plan is written to prevent, and the code holds
the line: the lifecycle layer never appends to a `ReservationFold` for contention and never
reads capability or topology folds for placement.

**Triggers are the substrate of dependencies.** The implemented set is `AfterTask`,
`AfterTerminal` (fires on `Done` *or* `Failed`), `IfResult`, and `AtTick` (tick triggers in a
`BTreeMap` index draining the `tick <= now` prefix, O(due + log T) per tick). Triggers are
indexed by what they wait on — task id, result path, tick — so a fired event touches only the
triggers keyed to it, not every waiter. The speculative `BlobReplicated` /
`CapabilityAvailable` / node-join-leave triggers an earlier draft listed were
doc-implementation skew and are dropped from scope until a concrete user needs them.

**Shards (fan-out / fan-in).** Each shard is a directory with its own lease + cursor (the
map); a reduce step joins on shard status. The join is **failure-aware**: `JoinPolicy` is
`AllOrNothing` by default (any `Failed` shard fails the join) with `BestEffort` and
`Threshold(n)` as escape hatches — a failed shard can no longer hang the reduce forever.

**Capability-bearing steps — the seam, the one cross-plan contract.** A step that requires an
exclusive capability routes its requirement through the gang scheduler's
match→reserve→quorum-`Active` pipeline and **does not start compute until an `Active` claim
handle is held**. It is *structurally* unable to bypass that pipeline — `drive_capability_step`
holds no fold, so it cannot touch a `ReservationFold` directly. A claim reject leaves the step
`Waiting`; a minority-partition leader is quorum-starved and so the step stays `Waiting` and
never starts compute — the gang scheduler's partition guarantee, surfaced intact at the
lifecycle boundary. The lifecycle layer states the requirement; it never evaluates placement
itself.

**Retry / cancel / checkpoint / observability.** Retry lives in state (worker-enforced);
cancel is a written `task/<id>/cancel.json`; checkpoints are content-addressed
`results/stepN.out.ref` blobs; lifecycle and metrics are `events/*.json` / `metrics/*.json`.
Deleting `task/<id>/` reclaims the subtree — no sweeper, no controller.

---

## The hardening pass — what the branch audit forced

A branch audit and a ~7,200-line code review found the gaps between the plans and the shipped
code. The pure predicates were solid from the start — quorum math is correct strict-majority,
the fence is monotonic, ordered-acquire gives deadlock-freedom, arithmetic saturates, float
ordering is NaN-safe. The findings clustered at the **seams**, where pure cores meet live
wiring and where state-machine invariants rested on writer discipline nothing enforced. Every
one is fixed on the branch with a test where reasonable.

**Terminal tasks can no longer be resurrected.** The fold applied status transitions
unconditionally — `complete(id)` then `start(id)` silently moved a `Done`/`Failed` task back
to `Running`, and because replay re-applies the log exactly, the corruption was permanent on
every failover. Every downstream "terminal" guarantee (shard join readiness, `AfterTerminal`
fire-once, status counts) rested on this. A terminal-transition guard now rejects it.

**A failed shard can't strand its dependents.** `propagate_failure` cancels the pending
siblings and fails the parent; `AfterTerminal` fires on `Failed` as well as `Done` so a
failed predecessor can't leave its dependents armed forever. `Blocked` gained real semantics
in the same pass — it now means *parked on external state the task can't change itself* (a
failed prerequisite awaiting an operator or retry), as distinct from `Waiting` (a
self-retrying claim reject).

**Delete cascades.** Deleting a task reclaims its whole linked subtree — shards, spawned
children — in one deterministic folded event, and prunes the triggers waiting on it. An
orphaned shard can no longer keep running and keep holding a claim. Content-addressed
`results/*.ref` blobs survive — they aren't owned by the task.

**The seam is bidirectional — an abnormal exit releases the claim.** `ClaimPipeline` gained
`release`, and every abnormal exit of a step holding an `Active` claim (failed / cancelled /
deleted / rewound-past) *must* `release_step` it. An un-released claim is a stranded GPU — the
one failure the substrate *can* compensate, because a held claim is its own to revoke, unlike
an external side effect. An end-to-end test acquires `Active` and returns the island to
`Free`.

**Rewind is a metadata contract, not an undo.** Reopening or replaying reconstructs lifecycle
metadata deterministically; it does **not** undo the side effects of previously-executed
steps. The advisory `StepKind { Pure, Idempotent, SideEffecting }` marker lets a worker refuse
to silently re-run a completed side-effecting step on rewind. The single substrate-compensable
rewind case — a held claim — is released.

**Plus the seam-level fixes from the code review:** the co-located scheduler now self-applies
its own island announcement (a node could not previously schedule onto its own hardware);
`release_island` reports `Lost` for an island it never held instead of masking a tracking
bug; the `Threshold` join distinguishes an unsatisfiable-by-construction config from a real
shard failure; non-matching `IfResult` arms on a terminal task disarm instead of accumulating
forever; the rollback path is best-effort so all-or-none holds even when a release errors; and
a clutch of efficiency cleanups (HashSet cycle guard, batched host queries, single generation
owner, non-finite `load` rejection).

---

## Scheduler SDK in five languages

Both surfaces — the gang-claim scheduler and the workflow task-lifecycle (`WorkflowAdapter`,
shards, triggers) — are exposed across every binding tier: Rust SDK → napi / PyO3 / C-ABI →
TypeScript / Python / Go / C. It is **pure surfacing**: no new core semantics, the bindings
re-export and wrap, and the caller applies the results.

The Rust SDK core (`sdk/src/gang.rs`, `sdk/src/cortex/workflow.rs`, the `Mesh` methods), the
napi / PyO3 bindings, and the C FFI are clean and idiomatic — consistent `HandleGuard`
quiescing on the async-holding handles, correct two-pass buffer capping on the read-only list
calls, a `TriggerEngineHandle` that clones `Arc<WorkflowAdapter>` so it outlives a freed
adapter, consistent lock ordering across the trigger paths, and panic-safety at the
`extern "C"` boundary under the release profile's `panic = "abort"`. The napi / PyO3 / TS / Py
tiers are test-verified (vitest, pytest, Rust surface tests).

The review concentrated its findings in the Go/C layer, where the two-pass
`(out_buf, cap, out_count)` out-buffer convention had been applied to operations that
**mutate** state or are **not atomic** across the two calls. The headline was a ship-blocker:
Go's `OnTick` / `OnTaskChange` are *consuming* calls — the underlying engine fires and disarms
triggers — so the two-pass "size first, fill second" loop fired and discarded every action on
pass one and returned an empty slice on pass two. The Tier-2 trigger feature was silently
non-functional on Go, with no Go test to catch it. The fix sizes the buffer from
`armed_count()` (a valid upper bound — fired ⊆ armed) and makes a single call; the consuming
contract is now documented in the C header and pinned by a runnable FFI test. The companion
fixes: Go's `Snapshot()` grows-and-retries when the chain grew between the sizing and fill
passes (it previously truncated to a corrupt buffer under a concurrent write); the guard-less
Tier-2 handles are documented; and `ShardGroup` / `TriggerEngine` gained a public `Free()`.

The flat SDK criteria reach both query shapes across both folds — `tags_all` / `tags_any` /
`tag_groups_all` on the host match and `require_all` / `require_any` on the island axis — plus
the `region` (subnet / zone) host filter, exposed through every tier.

---

## MeshOS ↔ scheduler bridge — wiring *deciding* to *running*

Three desired-state machines existed and did not touch: the gang scheduler decides *who holds
the island*, the workflow fold decides *what state a task is in*, and MeshOS *runs the
daemons*. A `grep` for `meshos` / `DaemonIntent` across `workflow/` and `gang/` returned
nothing. Without the bridge, `Running(ActiveClaim)` runs nothing — the claim succeeds into a
void — and a worker that dies holding a claim leaks the island forever. v0.28 draws the five
state-projections (plus one veto) that connect them, **without either side calling the
other** — projection at the desired/observed boundary, the same "propagate state, not
connections" posture as the rest of NET.

All cross-layer code lives in one neutral module, `behavior::scheduler_bridge/` — the only
module importing both `cortex::workflow` and `meshos`/`gang`, so the layering boundary holds
*structurally*, not by convention. The module splits cleanly into a pure facade
(`SchedulerBridge`, owns the `ClaimRegistry`, composes the projections, no live handles) and
an I/O driver (`SchedulerBridgeDriver`, the self-owned `spawn(interval)` / `shutdown()` tick
loop). Every projection is pure: it reads state, returns a value, performs no I/O, and never
calls back into either layer.

- **Task → daemon intent.** A `Running` task projects to `DaemonIntent::Run` for a task-keyed
  `DaemonRef`; terminal/cancel → `Stop`; a shard fan-out → N intents. The workflow writes
  intent only — it never dispatches.
- **Claim → forced placement.** A claim-bearing task pins its daemon to the `ActiveClaim`'s
  node via a node-pinned `DaemonIntentUpdate` (`node: Option<NodeId>`; a daemon pinned to
  `Some(n)` is managed only by node `n`). The drift scorer is bypassed for claim-pinned
  daemons by construction — the claim *is* the placement decision.
- **Daemon lifecycle → step state.** Daemon started → task confirmed `Running`; daemon failed
  or exited abnormally → step `Failed`, which propagates through `AfterTerminal` to release the
  claim and either retry, fail the parent, or fail the gang per the shard policy.
- **Observed liveness → fold delta.** MeshOS observes node liveness and the bridge prunes dead
  nodes from the candidate **host** set inside `gang::match_islands`
  (`hosts.retain(|h| !down_nodes.contains(h))`) — covering *both* folds' exclusion at the one
  point they meet, while mutating neither fold, so their CRDT/AP semantics stay byte-identical.
  The down-set lives in a lock-free `ArcSwap<HashSet<NodeId>>` on `MeshNode` with an
  empty-set fast path that keeps the hot match path free in the common case.
- **Migration veto — enforced by type, not convention.** An exclusive `ActiveClaim` makes its
  daemon migration-pinned for the claim's lifetime; a planned drain becomes release →
  re-claim elsewhere → restart, never live migration. The veto is enforced by a marker type:
  `MigrationEligible` has a private field and only `check()` constructs it, so a claim-holder
  literally *cannot* be passed to `migrate()` — a `compile_fail` doctest pins it. No
  `can_migrate()` bool a contributor can forget to consult.

The projections (Phase A desired-down, Phase B observed-up, Phase C migration veto) and the
driver are landed and tested — 31 bridge lib tests plus the driver, gang-claim-node, and
MeshOS-pipeline integration suites. What remains is **assembly** at a deployment site that
already owns a `MeshNode` + `MeshOsRuntime` + `WorkflowAdapter`: call `driver.spawn(interval)`,
install the fan-out lifecycle observer, and call `on_running` / `on_released` from the
step-driver. The runtime appliers and the per-tick `project_liveness → set_liveness_down` wire
are deferred until a consumer exists. The MeshOS↔scheduler review (2026-06-26) raised seven
findings — a pin gate that skipped an orphan stop (latent double-run), a per-tick snapshot
deep-clone, a merge-vs-LWW divergence, an unconditional republish-all, a deleted-task daemon
leak, and two nits — all resolved on the branch with tests.

---

## Breaking changes

v0.28 is overwhelmingly **additive** — new scheduler and workflow surfaces, new fold kind,
new bridge module — so most existing callers are untouched. The watch-outs:

**`IslandTopology` fold (new CortEX kind).** Registered as a new fold kind and dispatched
alongside the existing folds; it folds from capability announcements already on the wire. A
peer running an older substrate simply won't build the island view — it doesn't break the
capability path.

**napi runtime floor raised to 3.9.4.** The Node binding now requires the napi runtime at
≥ 3.9.4 (`napi-derive` bumped to 3.5.7). Node consumers on an older runtime must update.

**C-ABI trigger calls are consuming and single-shot.** `net_trigger_on_task_change` /
`net_trigger_on_tick` fire-and-consume on every call — they are **not** two-pass-safe like the
genuinely idempotent readers (`net_workflow_subtree`, `net_workflow_snapshot`). Size the
buffer from `armed_count` up front and make a single call. The headers now document this; a
two-pass loop will silently drop every fired action.

**In-process-only fields carry no wire risk.** `DaemonIntentUpdate.node` and
`DesiredState.desired_daemon_nodes` are in-process types with no `Serialize` / `Deserialize`,
so the bridge's additions are wire-compat-neutral. The internal `gang::match_islands`
signature gained a `down_nodes` parameter, but the public `MeshNode::match_islands` signature
and every binding/FFI entry point are unchanged.

---

## How to upgrade

1. **Pull the new scheduler and workflow surfaces** — they are additive; existing callers
   need no change.
2. **Node binding consumers** update to the napi runtime ≥ 3.9.4.
3. **C-ABI trigger callers** size the buffer from `armed_count` and call
   `net_trigger_on_tick` / `net_trigger_on_task_change` once — these consume on every call and
   must not be driven with a two-pass size-then-fill loop.
4. **Deployments wiring the MeshOS↔scheduler bridge** assemble it at a site that owns a
   `MeshNode` + `MeshOsRuntime` + `WorkflowAdapter`: `driver.spawn(interval)`, install the
   fan-out lifecycle observer on the `DaemonRegistry`, and call `on_running` / `on_released`
   from the step-driver. The projections ship landed; this is the remaining assembly.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

A couple of substrate-relevant bumps and a clutch of routine patches.

**Substrate.** `anyhow` (1.0.102 → 1.0.103); the Node binding raises `napi-derive` to 3.5.7
and lifts the napi runtime floor to 3.9.4.

---

Released 2026-06-26.

## License

See [LICENSE](../../LICENSE).
