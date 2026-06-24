# Plan corrections — mesh-scheduler branch audit

> Confirmed-against-implementation gaps between the shipped `mesh-scheduler` branch and the
> two design documents (`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`, `TASK_LIFECYCLE_PLAN.md`).
> Each item is paired with concrete file references and a recommended fix.

## Status

Six gaps confirmed by reading the branch. Two are design-level (need explicit decisions
documented in the plans), four are implementation-level (need code changes). One concern
from the review (selection policy) is actually well-implemented and only needs the plan to
acknowledge it.

Ordered by load-bearing impact, not by file location.

> **Reviewer addendum (independently verified against `mesh-scheduler`).** Items 1, 2, 4,
> 5, 6, 7 were re-checked against the branch and all confirm — file paths, symbol names,
> and the absence claims (`.block(` call sites = 0; `children|parent|subtree` in
> `workflow/state.rs` = 0; `Trigger` enum has no terminal-state variant) all hold. Item 3
> is a contract observation, not line-checkable, and is correct. **The audit is trustworthy
> and actionable.** Three additions below: (a) #4 is under-rated *for a GPU scheduler* —
> see its addendum; (b) #1's "remove" recommendation should wait on #2 — see its addendum;
> (c) **the most load-bearing work is the part this audit did not touch** — the five
> "didn't audit" items are promoted to a [gating verification checklist](#8-gating-the-hard-layer-is-unverified-reviewer-addendum)
> because they, not the seven above, are where correctness-under-contention lives.

---

## 1. `Blocked` is dead state (DESIGN)

**Confirmed in code.** `TaskStatus::Blocked` is declared in `workflow/types.rs:28` and has
the doc string "Parked on an unmet dependency." The fold materializes it at `state.rs:111`
in `status_counts()`. `WorkflowAdapter::block()` exists at `adapter.rs:128`.

**The method has zero call sites.** `grep -rn "\.block(" crates/net/src` returns nothing
outside the method definition itself.

The actual dependency-wait pattern: a dependent task is never `Submit`'d until its
`AfterTask` trigger fires. There is no intermediate state where the task exists in the
fold as `Blocked`. The plan's "two states" distinction has collapsed in practice to "task
exists and is `Waiting` on a resource" vs "task does not yet exist."

**Two valid resolutions, pick one explicitly:**

**Option A: Remove `Blocked` from the enum.** Simpler state machine. Honest about what
the implementation does. Requires deleting the enum variant, the `block()` method, the
counter field in `StatusCounts`, and updating the plan doc to drop "Blocked" from §5
piece 1's "explicit cursor."

**Option B: Use `Blocked` for a real case the current implementation misses.** Candidate:
a task whose `submit` was triggered but whose prerequisites (shard `join_ready`, blob
replication, etc.) failed pre-execution — distinct from "claim was rejected" (`Waiting`)
because the task can't make progress on its own without external state changing. This
gives both states real semantics.

**Recommendation: Option A.** The semantic split was a guess in the doc; the
implementation found it didn't need it. Keeping unused states invites future divergence —
someone will eventually call `block()` somewhere and the semantics will drift.

If Option B is preferred, then `block()` needs at least one real call site (probably in
`shard.rs` when a shard fails — see issue 2) and the plan must specify the
state-transition rules between `Blocked`, `Waiting`, and `Running`.

> **Reviewer addendum — sequence after #2, don't lead with removal.** The Option A
> recommendation ("remove `Blocked`") is in tension with #2, which independently identifies
> a real case that wants it: a task whose prerequisites failed pre-execution can't progress
> on its own and is genuinely *blocked on external state*, distinct from `Waiting` (claim
> rejected, will retry itself). Resolve #2's failure-propagation design first; if it wants a
> "can't proceed without external change" state, that *is* `Blocked`. The phasing already
> orders this correctly (Phase 5 after Phase 2) — but the recommendation text should match
> the phasing: **decide `Blocked` as an output of the failure design, not before it.**
> Deleting a state you're about to need is churn.

---

## 2. Shard failure propagation hangs the gang (IMPLEMENTATION + DESIGN)

**Confirmed in code.** `ShardGroup::join_ready()` at `shard.rs:78`:

```rust
pub fn join_ready(&self, state: &WorkflowState) -> bool {
    self.shards.iter().all(|s| {
        state.get(*s).map(|t| t.status == TaskStatus::Done).unwrap_or(false)
    })
}
```

And `pending()` at `shard.rs:88` filters `status != Done`. A `Failed` shard is therefore
forever "pending" and never `Done`, so the reduce never fires.

The `Trigger` enum has `AfterTask(TaskId)` which only checks `Done` (`trigger.rs:82`),
`IfResult` which checks result-key contents, and `AtTick`. **There is no `OnFailure`,
`AfterTerminal`, or any other trigger that observes the `Failed` terminal state.**

This is the failure propagation gap from the review, confirmed in production code. A
single shard panic deadlocks the entire gang's reduce.

**Concrete fix, three pieces:**

1. **Add `Trigger::AfterTerminal(TaskId)`** — fires when the task reaches either `Done` or
   `Failed`. This is the primitive failure propagation needs. The trigger payload includes
   `terminal_status` so handlers can branch.

2. **Add `ShardGroup::failed(&self, state)`** — `Vec<TaskId>` of shards in `Failed`.
   Symmetric with `pending()`.

3. **Decide and document the join semantics for failures.** Three reasonable policies:
   - **All-or-nothing (current implementation, but failed-aware):** reduce fires only when
     all shards `Done`; if any shard is `Failed`, propagate failure to the parent
     (cancel pending shards via existing `request_cancel`, then mark parent `Failed`).
   - **Best-effort:** reduce fires when every shard is in some terminal state; reducer
     observes which shards succeeded and decides. Useful for embarrassingly-parallel
     work where partial results are acceptable.
   - **Threshold-based:** reduce fires when N of M shards are `Done`. Parameterized.

   These are user choices; ship all-or-nothing as default with the structure in place to
   add the others without API breakage.

The plan currently doesn't pick. Implementation defaulted to "hang forever," which is
strictly worse than any of the three. **Plan should specify the default and what the
escape hatches are.**

---

## 3. Replay is fold replay, not user-facing rewind (DESIGN)

**Confirmed in code.** Every reference to "replay" in `workflow/` (`adapter.rs:9-10`,
`mod.rs:14`, `state.rs:41`, etc.) means *deterministic fold of the same log produces the
same state* — the correctness property for the metadata state machine. None of it
discusses user-facing rewind of side-effecting steps.

The plan's emergent semantics include: "**Replay** — rewind the cursor, clone the dir to a
new id, or rewind to step N." This implies users can rewind a task's progress. The
implementation supports the *metadata* rewind (delete the task, re-submit, fold replays
identically). It does not address the *side-effect* rewind problem.

If step 5 of a task made an external API call, sent an email, wrote to a database, or
allocated a GPU island via Thunderdome, rewinding the task's cursor to step 3 doesn't undo
those side effects. The task now has a metadata view that says "I'm at step 3" while the
world has been touched by step 5.

**This is not a bug; it's an unspecified contract.** Most workflow engines deal with this
through compensating actions, sagas, or explicit idempotency requirements. The plan
should pick a position rather than leave it implicit.

**Recommendation:**

1. **Document the actual contract:** "Rewind reconstructs the lifecycle metadata
   deterministically. The substrate does not undo side effects caused by previously-
   executed steps. Steps that produce external side effects should be idempotent
   (re-execution leaves the world in the same state) or paired with explicit compensating
   steps if rewind is intended."

2. **Add a step-level marker** (`step_kind: Pure | Idempotent | SideEffecting`) that's
   advisory only — the worker respects it (e.g., refuses to re-run a `SideEffecting` step
   that already completed without explicit compensating step having been registered).
   This is convention, not enforcement; substrate can't actually check side effect
   freedom.

3. **For Thunderdome-acquired resources specifically:** rewinding past a step that holds
   an `Active` claim must release the claim. This is the one case where substrate can
   actually compensate, and it should.

---

## 4. Delete is shallow, not subtree (IMPLEMENTATION)

**Confirmed in code.** `WorkflowFold::apply` handler for `DISPATCH_TASK_DELETED` at
`fold.rs:90-96`:

```rust
DISPATCH_TASK_DELETED => {
    let p: DeletedPayload = postcard::from_bytes(tail).map_err(...)?;
    state.tasks.remove(&p.id);
    state.cancelled.remove(&p.id);
}
```

That's it. No cascade. The plan says delete "reclaims the subtree" but there is no
subtree linkage in `WorkflowState`. Shard ids are derived deterministically from parent id
(`shard.rs:25`) but the state has no record of which shards belong to which parent. If
you `delete()` a parent task, the shards are orphaned and will continue to run.

Similarly: if task B's `AfterTask(A)` trigger is armed and you delete A, the trigger
remains armed forever (or until B's submit window closes — but B's submit window is gated
on the trigger, so the trigger remains armed forever).

**Concrete fix:**

1. **Add parent-child tracking to `WorkflowState`.** `HashMap<TaskId, SmallVec<TaskId>>`
   keyed by parent. Populated when shards are submitted (`fan_out`) or when a step writes
   `task/<new-id>/spec.ref` (the DAG spawn pattern from the plan).

2. **Delete handler cascades.** `delete(parent)` recursively deletes every child.
   Idempotent for children that don't exist.

3. **Trigger engine prunes on delete.** When a task is deleted, all triggers waiting on
   it (in `by_task.get(&id)`) are dropped. Their actions will never fire.

4. **Document the contract:** "Deleting a task deletes its descendants (shards, spawned
   children) and discards triggers waiting on it. References to deleted task results
   (`results/*.ref` content-addressed blobs) survive — they are content-addressed and
   reachable via blob refs from any task that retained them, but the task metadata is
   gone."

The last point matters: content-addressed results don't disappear because they're not
owned by the task. This is correct behavior but should be stated so users don't expect
result GC on delete.

> **Reviewer addendum — this is higher severity than its phase implies, for a GPU
> scheduler specifically.** An orphaned shard doesn't just leak metadata — it keeps
> *running*, which means it keeps holding whatever Thunderdome `Active` claim it acquired.
> An un-released `Active` claim is a **stranded GPU that never returns to the pool**: leaked
> exclusive hardware, i.e. money bleeding out, which for a neocloud is the one failure mode
> the whole product exists to prevent. Recommend treating delete-cascade as a correctness +
> cost bug, not the "mechanical 3–5 day" framing in Phasing. See the cross-cutting decision
> below — #2, #3, and #4 share one missing rule.

---

## 5. Trigger indexing is right but undersold; tick-trigger O is unbounded (IMPLEMENTATION)

**Confirmed in code.** `TriggerEngine` at `trigger.rs:135-184`:

- `by_task: HashMap<TaskId, Vec<(Trigger, Action)>>` — O(1) lookup by task id, then
  O(triggers waiting on that task) for processing. **This is correct and matches the
  plan's perf note.**
- `by_tick: Vec<(Trigger, Action)>` — flat vector, scanned linearly on every `on_tick`.
  **This is O(all tick triggers) per tick, not O(triggers satisfied this tick).**

The task-keyed path is what the plan promised. The tick-keyed path is the naive O(N)
implementation it warned against.

For agentverse use cases where most triggers are `AfterTask` and tick triggers are rare,
this doesn't matter. For substrate use cases at scale where periodic / scheduled tasks
are common (cron-like patterns, backoff retries, deadline enforcement), this becomes the
bottleneck.

**Concrete fix:**

1. **Index `AtTick` triggers in a `BTreeMap<u64, Vec<(Trigger, Action)>>`** keyed by tick
   value. `on_tick(now)` drains entries with key `<= now` rather than scanning every
   armed tick trigger.

2. **Document the complexity bounds in `TriggerEngine` rustdoc:**
   - `on_task_change(task_id)`: O(triggers waiting on `task_id`) — currently true.
   - `on_tick(now)`: O(triggers satisfied at `now`) — after the BTreeMap change.
   - `arm()`: O(log triggers) — required by the BTreeMap.
   - `armed_count()`: O(distinct task ids) — currently O(distinct task ids), good.

3. **Add the `BlobReplicated` / `CapabilityAvailable` trigger types the plan mentioned**
   if they're going to be supported. Currently the `Trigger` enum is `AfterTask`,
   `IfResult`, `AtTick` only. The plan §3 piece 3 listed "timestamp / interval /
   dir-change / capability-arrival / node join-leave / blob-replicated /
   `after_task:<id>` / `if_result:<path matches>`." Most of these aren't implemented.
   Either implement them or drop them from the plan; current state is doc-implementation
   skew.

---

## 6. Option 4b is documented as parked, not present as a flag (DESIGN)

**Confirmed in code.** `multi.rs:22-24`:

```rust
//! Option 4b (two-phase reserve→commit) is parked for gangs whose
//! island count makes ordered-acquire backoff pathological; 4a ships
//! first (plan §4).
```

That's the only mention. No flag, no struct, no stub. The plan said "Ship 4a; keep 4b
behind a flag for gangs whose island count makes ordered-acquire backoff pathological."
The flag doesn't exist.

This isn't a bug — 4b is genuinely not needed yet — but the plan and the code disagree
about whether the flag exists. Two valid resolutions:

**Option A: Update the plan to say "4b deferred until ordered-acquire shows pathological
behavior at measured-N island counts."** Specify the measurement that would trigger
implementation (e.g., "if gang reject rates exceed 30% at 8+ islands under sustained
contention").

**Option B: Add the flag now as a runtime config knob** even if the 4b implementation
is just `unimplemented!()`. This makes the seam visible and forces future implementers
to think about how 4a and 4b should switch.

**Recommendation: Option A.** Premature flag infrastructure is the wrong direction for a
substrate. The plan should describe the measurement that triggers 4b implementation, and
the code should match.

---

## 7. Selection policy is well-implemented; only needs plan acknowledgment (DESIGN)

**Confirmed in code.** `SelectionPolicy` enum at `filter.rs:79-99` covers `LeastLoaded`
(Spread default), `Pack`, `LoadBand(target)`, `LowestId`. Pure `policy_cmp` function.
`select_islands` and `select_with_affinity` for warm-model preferences.

The review flagged this as "design work that needs explicit attention." It's already
done. The plan should reference the implemented policies by name rather than treating
the policy as undetermined.

**Concrete fix:** Update the plan §7 (selection policy) to reference the four implemented
policies and note that warm-model affinity layers on top of any of them via
`select_with_affinity`. One paragraph.

---

## Cross-cutting: abnormal terminal states must release claims (reviewer addendum)

Issues 2, 3, and 4 look separate but share one missing rule. A task can leave the normal
`Running → Done` path three ways:

- a shard **fails** (#2) and the gang can't complete,
- the cursor is **rewound** past a step that holds a resource (#3),
- a parent is **deleted** while children run (#4).

In every case, any Thunderdome `Active` claim the task held must be **released**. The
substrate *can* compensate here — unlike external side effects (#3), a held claim is its
own to revoke. Today none of the three paths release the claim, so each is a stranded-GPU
leak by a different route.

**Decision to make once, applied to all three:** every transition into a terminal-or-abandoned
state (`Failed`, deleted, rewound-past) emits a claim-release for any island the task held.
This is one rule, one place — the workflow fold's terminal handler calls the Thunderdome
release path — not three independent fixes. Specify it in `TASK_LIFECYCLE_PLAN.md` as part
of the seam contract (the seam is currently one-directional *acquire*; it needs the matching
*release*), and pin a test: a failed/deleted/rewound task holding an `Active` claim leaves
the island `Free` within bounded time.

---

## 8. Gating: the hard layer is unverified (reviewer addendum)

The seven issues above are real, but note **where they live**: six are workflow-layer
(Time) — `Blocked`, shards, delete, triggers, replay contract — and the two that touch the
gang layer (#6, #7) are both benign. **Every confirmed bug is in the easy layer.** The
correctness-under-contention thesis — the part nobody else can do, the reason this is a
company — is the "what I didn't audit" list, and it is entirely unverified.

Those items are not optional follow-ups; they are **more load-bearing than the seven
audited issues**, because a partition that double-books a GPU is a correctness failure of
the core claim, where a hung reduce is a (serious but localized) liveness bug in the
periphery. Promoted here to a gating checklist. **Treat green on §1–7 as green on the
workflow layer only — not on the product.**

Gating verification, in priority order (must run on **multi-node hardware**, not a single
box — the single-box benchmarks cannot exercise any of these):

1. **Partition-during-claim (the gate).** Split an island's replica set mid-claim; assert
   at most one side ever reaches `Active`, minority never starts compute, fence rejects the
   late ex-leader `Active`. Confirm the DST harness (`loom_models.rs`) actually contains
   this scenario — the plan allocated 30% to it; verify it's present, not promised.
2. **Quorum-`Active` fencing epoch.** Confirm `gang/active.rs` rides the existing
   causal-chain / `generation` machinery (plan §6, locked decision 3) and did **not**
   introduce a parallel Raft term. A second consensus mechanism here is a design regression.
3. **`ColocationStrict` on island chains.** Confirm the topology fold's chains are pinned to
   one fault domain (plan §5), so a cross-DC split can't bisect an island's quorum and the
   quorum stays LAN-local.
4. **TTL takeover under partition.** Confirm the reserve TTL is configurable and the
   node-killed-mid-claim takeover path is tested *under partition*, not just clean failover.
5. **`requires_capability` never treated as a hold.** Confirm no path in `step.rs` lets a
   match result stand in for a claim (locked decision 4).

Until 1–4 are green on real multi-node hardware, the differentiating claim is unproven
regardless of how clean the workflow layer is.

---

## Phasing

These corrections aren't all the same shape. Group them by where the work happens:

> **Reviewer addendum — Phase 0 gates everything.** The phases below are all workflow-layer
> (Time) work and are correctly scoped. But none of them touch the differentiating
> correctness (§8). Before or in parallel with the below, run the §8 gating checklist on
> multi-node hardware. A clean Phase 1–5 with §8 unverified means "the periphery is solid,
> the core is unproven" — do not ship or pitch on the strength of the phases below alone.
> Also: the cross-cutting **claim-release on abnormal terminal states** rule spans Phases 2
> and 3 — implement it once (workflow terminal handler → Thunderdome release), not twice.

### Phase 0 — Gating verification of the hard layer (§8) — *highest priority*

The five §8 items on real multi-node hardware. Item 1 (partition-during-claim) is the gate.
This is not "audit cleanup" — it is verifying the thesis. Everything below is periphery by
comparison.

### Phase 1 — Plan-only updates (a few hours)

Items 1 (decision: A or B for `Blocked`), 3 (rewind contract), 6 (4b condition), 7
(selection policy acknowledgment). Each is doc work that doesn't touch the implementation.
Resolves the worst of the design ambiguity before the next implementation pass.

### Phase 2 — Failure propagation (1 week)

Item 2 — shard failure handling. The most load-bearing fix because it's a runtime hang
in current code. Three concrete pieces: `Trigger::AfterTerminal`, `ShardGroup::failed`,
join semantics decision + default. Tests: a shard panic propagates to the parent within
bounded time, not "forever."

### Phase 3 — Delete cascade + claim release (3-5 days)

Item 4 — parent-child tracking, recursive delete, trigger pruning. Mechanical work. The
parent-child map is small new state on `WorkflowState`. The trigger pruning is one new
call in the delete handler. **Plus the cross-cutting rule:** the delete (and failure, and
rewind) handlers must release any held Thunderdome `Active` claim — an orphaned shard that
keeps its claim is a stranded GPU. Implement the release once in the terminal handler,
shared across Phases 2 and 3.

### Phase 4 — Trigger indexing + missing trigger types (1 week)

Item 5 — `BTreeMap<u64, ...>` for tick triggers, rustdoc complexity bounds, decision on
which of the plan's listed trigger types to actually implement. Mechanical work for the
indexing; the trigger-type list needs a substrate-level decision about scope.

### Phase 5 — `Blocked` removal or activation (3 days, after Phase 1 decision)

Item 1 — implementation cleanup following the Phase 1 design decision. Either drop the
variant from the enum and clean up references, or wire `block()` to the real call site
identified in Phase 1 / Phase 2.

Total: ~2-3 weeks of focused work plus a few hours of plan revision.

---

## What I didn't audit but probably should be checked

> **Reviewer addendum — these are now [§8, promoted to gating](#8-gating-the-hard-layer-is-unverified-reviewer-addendum).**
> Listed here originally as "known unknowns"; the reviewer pass elevates them above the
> seven audited issues because they are the correctness core, not the periphery. Kept below
> for the original framing and file pointers.

The audit focused on the issues flagged in the document review. Things I didn't look at
in the branch but probably warrant the same scrutiny:

1. **`requires_capability` lowering to Thunderdome match.** Plan locked decision 4 says
   "match narrows, CAS commits." Worth checking that no step path in `step.rs` treats a
   match result as a hold.

2. **DST harness coverage.** Plan said 30% allocation to extending `loom_models.rs` with
   gang-contention and partition-during-claim scenarios. Worth checking that the scenarios
   the plan listed are actually present in the harness.

3. **`ColocationStrict` placement on island chains.** Plan §5 says island replica sets
   must be pinned to one fault domain. Worth checking the actual placement configuration
   for the topology fold's chain.

4. **Quorum-Active fencing epoch.** Plan §6 says the epoch rides the causal chain /
   `generation` machinery, not a separate Raft term. Worth checking that the active.rs
   implementation uses the existing generation rather than introducing a new field.

5. **TTL takeover for abandoned reserves.** Plan §4 says the reserve TTL handles
   node-killed-mid-claim. Worth checking that the TTL value is configurable and that the
   takeover path is tested under partition.

Each is a separate audit pass with its own scope; flagged here as known unknowns rather
than added to the corrective plan because verifying them requires more time in the code
than this pass had.
