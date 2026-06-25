# Mesh Scheduler — Code Review Findings

Review of branch `mesh-scheduler` vs `master` (~7,200 lines of new Rust).

**Scope:** `IslandTopologyFold` (`behavior/fold/island.rs`), the gang-claim
scheduler (`behavior/gang/*`), the cortex workflow / task-lifecycle engine
(`cortex/workflow/*`), node wiring (`mesh.rs`), and the loom/proptest harnesses.
All-additive; no deletions.

**Overall:** high-quality, well-tested code. The *pure predicates* are solid —
quorum math is correct strict-majority (`acks*2 > n`, empty-set guarded), the
fence is monotonic (`>=`), the ordered-acquire lock-order gives deadlock-freedom,
arithmetic is saturating throughout, and float ordering is NaN-safe. The findings
below cluster at the **seams**: where the pure cores meet the live node wiring,
and where state-machine invariants are enforced by convention rather than by code.

Line numbers are anchors at review time and may drift as the branch evolves.

---

## Status — all findings resolved

Every item below has been fixed on `mesh-scheduler` with a test where
reasonable; each was committed separately (see `git log`, commit subjects
tagged `(review #N)`). Highlights:

- **#1–#8** (correctness): fixed. #1 self-index, #2 terminal-transition
  guard, #3 loom model rewritten to production semantics (verified under
  `--cfg loom`), #4 seedable epoch + documented Phase-D durability seam,
  #5 holder-gated release, #6 Threshold `debug_assert`, #7 dead-arm
  disarm, #8 best-effort rollback.
- **#9–#14 + advisory** (quality/efficiency/altitude): fixed. #9 HashSet
  cycle guard, #10 broadcast-error logging, #11 single `Claimant`
  generation owner, #12 batched `HostedByAny` query, #13 `has_quorum`
  reuse, #14 release orphaned reserve; plus non-finite `load` rejection
  and the proptest checked cast.

Note on **#4**: the fence/cohort durability across leader restarts is
genuinely Phase-D live-wiring work; the commit adds the *seam*
(`with_generation` to seed the epoch) and documents the requirement
rather than building the unfinished durable store.

---

## Correctness

### 1. `publish_island_topology` never applies to the node's own island fold
**`mesh.rs:9252`** · severity: high · confidence: confirmed

`publish_island_topology` → `publish_fold` (`mesh.rs:9159`) only calls
`publish_fold_broadcast` — it broadcasts outward and never does
`self.island_fold.apply(...)`. Both sibling publish paths *do* self-apply:
`announce_capabilities` (`mesh.rs:9367`, comment "Self-index so local queries see
our own caps") and `apply_and_broadcast_reservation` (`mesh.rs:11173`).

Since `match_gpu_islands` / `claim_gpu_island` (`mesh.rs:11082` / `11127`) read
`self.island_fold`, a co-located scheduler+GPU-host that publishes its island and
then calls `claim_gpu_island` gets `Ok(None)` forever — it can never schedule onto
its own hardware. Only peer-hosted islands are ever visible. The 2-node broadcast
test only asserts the *peer's* fold converges, so the gap is uncovered.

**Fix:** self-apply the announcement to `self.island_fold` in (or alongside)
`publish_island_topology`, mirroring the capability/reservation paths.

### 2. Workflow state machine has no transition guard — terminal tasks can be resurrected
**`fold.rs:64` / `adapter.rs:114`** · severity: high · confidence: confirmed mechanism

`DISPATCH_TASK_TRANSITIONED` does `t.status = p.status` unconditionally
(`fold.rs:68`); `DISPATCH_TASK_RETRIED` sets `status = Running` for any existing
task (`fold.rs:87`). The adapter's `start` / `complete` / `fail` / `retry`
(`adapter.rs:119-152`) are thin wrappers with no checks. `TaskStatus::is_terminal()`
exists but is consulted **nowhere** on the write path.

So `complete(id)` followed by `start(id)` or `retry(id)` silently moves a
`Done`/`Failed` task back to `Running`, and because the fold replays the log
exactly, the corruption is permanent on every failover replay. Every downstream
"terminal" guarantee (shard join readiness, `AfterTerminal` fire-once, status
counts) rests on writer discipline that nothing enforces.

**Fix:** a guarded `try_transition(from, to)` table on the state machine, or at
minimum reject transitions out of `is_terminal()` in the fold. This is the
deepest altitude issue and would also harden findings #6 and #7.

### 3. Loom partition-safety model asserts semantics production doesn't implement
**`tests/loom_models.rs:853`** · severity: medium (test-only) · confidence: confirmed

`commit_active_fenced` lets a strictly-higher epoch **displace a live Active**
(`epoch <= epoch_of(cur)` → reject, else CAS-replace) and the test asserts "the
higher epoch always wins … epoch-3 leader holds Active" (`:894-895`).

Production does the opposite: `reservation.rs:244` rejects cross-publisher
`Active → *` unconditionally — a different leader can **never** displace a live
Active regardless of epoch (it gets `LostReservation`), and the fence uses `>=`
not `>`. The model "verifies" the exact partition-during-claim invariant it claims
to pin while encoding a different mechanism — false assurance on the headline CP
property.

**Fix:** model the holder-gated install + ack-fence faithfully, or relabel the
test as an abstract CAS exercise rather than a mirror of `commit_active`.

### 4. CP fence/epoch is rebuilt per-pipeline, so it doesn't fence across leaders
**`step.rs:184`, `step.rs:187`, `step.rs:239`** · severity: medium (latent) · confidence: confirmed

`GangClaimPipeline::new` builds a fresh `ReplicaCohort::new(...)` (empty fences)
and starts `generation: 1`; `commit_active`'s epoch is `self.next_gen()`
(`step.rs:239`). Across pipeline instances (a restart, or a second job on the same
island) both the fence *and* the epoch reset.

Today the fence is effectively inert because the cohort resets with the epoch.
Once wired to durable/shared fences (the quorum module notes this is "remaining
Phase D integration"), a fresh pipeline proposing epoch 2 against an island a prior
leader fenced high would get `NoQuorum` forever (livelock).

**Fix:** the epoch must ride a durable per-island generation, and the cohort must
be shared/persistent, before this gate provides real cross-leader safety. Track as
part of the Phase D live-wiring work.

### 5. `release_island` returns `Won` for an island this node never held
**`mesh.rs:11113` / `claim.rs:193`** · severity: medium-low · confidence: confirmed

`ReservationFold::merge(None, _)` returns `Insert` unconditionally
(`reservation.rs:191-196`), so a `Free` first-write to an absent key inserts →
`ApplyOutcome::Inserted` → `ClaimOutcome::Won`. But both docs claim "`Lost` if this
node wasn't the holder." That only holds when an entry already exists (the
`foreign_release_is_rejected_as_lost` test). Releasing an island with no local entry
returns `Won` and inserts a spurious `Free` entry — masking a reservation-tracking
bug instead of surfacing it.

**Fix:** treat a release of an absent/un-held island as `Lost` (or a distinct
no-op outcome), and/or correct the doc contract.

### 6. Threshold join returns `Failed(vec![])` with an empty failed-list
**`shard.rs:185`** · severity: medium-low · confidence: confirmed

For `JoinPolicy::Threshold(n)`, when `total - failed.len() < n` while shards are
still `Running` (e.g. `n` misconfigured above the shard count: 3 shards,
`Threshold(5)`, all running → `done=0, failed=[]`, `3 < 5` → `Failed([])`), it
reports a shard failure with no failed shard. `propagate_failure` then fails the
parent and cancels pending shards citing nothing, and the caller's failure handler
gets an empty set — violating the `Failed(ids)` = "these shards failed" contract.

**Fix:** distinguish an unsatisfiable-by-construction config (a distinct status or
a debug-assert at construction) from an actual shard failure.

### 7. `IfResult` non-matching arms leak on a terminal task
**`trigger.rs:110` / `trigger.rs:197`** · severity: medium-low · confidence: confirmed

`is_satisfied` for `IfResult` requires `task_done && result == value` (`:111-118`);
`on_task_change` re-arms everything not satisfied (`:199-201`). When a task reaches
`Done` with one value, the other branch arms can never satisfy (the result is now
immutable) yet are re-armed on every change — accumulating permanently in `by_task`,
reclaimed only by `on_delete`. Over many branch points this is unbounded growth.
(The `branch_fires_exactly_the_matching_arm` test even asserts `armed_count` stays
1, documenting the leak as accepted.)

**Fix:** on a terminal task, disarm non-matching `IfResult` arms instead of
re-arming them.

### 8. `try_acquire_gang` can leave a partial hold if a rollback release errors
**`multi.rs:102`** · severity: low (rare error path) · confidence: confirmed

In the `Lost` rollback loop, `release_island(...)?` propagates a `Sign`/`Apply`
error and returns `Err` mid-rollback, leaving earlier-grabbed islands still
`Reserved` — breaking the documented "holds the full set or nothing" invariant
(`multi.rs:75`). Low-probability (signing/applying a locally-built announcement
rarely fails), but real.

**Fix:** make the rollback best-effort — collect/ignore per-release errors so
all-or-none holds even on the error path.

---

## Quality / efficiency / altitude

### 9. `descendants` is O(n²) in subtree size
**`state.rs:128`** · efficiency

`out.contains(&t)` (linear `Vec` scan) inside the BFS cycle guard makes a delete of
a wide/deep parent quadratic. Use a `HashSet` `seen` for the membership check, keep
`out` for ordered output.

### 10. Node reservation API swallows the broadcast error
**`mesh.rs:11175`** · altitude / observability

`let _ = self.publish_fold_broadcast(&ann).await;` returns `Won` from the local CAS
even when propagation failed; the `ClaimOutcome` can't express "won locally, never
propagated." (The optimistic-AP `Won` itself is by design — CP safety lives at
`commit_active` — but the dropped error is a real observability hole.)

### 11. `GangClaimPipeline` duplicates `Claimant`'s generation machinery
**`step.rs:166` / `195` / `240`** · simplification

Its own `generation` field + `next_gen()` are byte-identical to `Claimant`'s, and
`claim` builds a throwaway `Claimant::new()` (which resets generation to 1) just for
the commit. Make a single `Claimant` the generation owner (lifting the
`#[cfg(test)]` on `Claimant::next_gen`).

### 12. `match_islands` clones the whole island table before filtering
**`behavior/gang/mod.rs` (`match_islands`, `IslandQuery::All`)** · efficiency

Clones every `IslandRecord` (with its `GpuSet`/`warm_models` Vecs) then discards all
but a handful, on a path that re-runs on every claim retry. Use a `HostedBy` /
filtering query that clones only survivors.

### 13. `has_quorum` re-derives the majority rule
**`quorum.rs:89`** · reuse

`acks*2 > n` duplicates `quorum_threshold()`'s `n/2 + 1`. Two definitions of
"majority" that must stay in lockstep for the CP gate to be sound; have
`has_quorum` call `quorum_threshold()`.

### 14. `GangClaimPipeline::claim` strands a `Reserved` on `NoQuorum`
**`step.rs:254`** · altitude

Documented as "the orphaned reserve TTL-expires," but an explicit `release_island`
on the reject path would free the island immediately instead of blocking other
claimants for the full `reserve_ttl_us` while this step is parked `Waiting`.

---

## Minor / advisory

- **Non-finite peer-announced `load`** (`filter.rs:107`): `IslandRecord.load`
  (`f32`) is validated only for `host == node_id`, so a peer can announce `NaN`,
  making `policy_cmp` a non-total comparator via `partial_cmp().unwrap_or(Equal)`.
  Placement order becomes arbitrary, though the reservation CAS remains the real
  arbiter. Consider rejecting non-finite `load` in `merge`.
- **u64→u32 truncation in proptest** (`proptest.rs:42`): `gpus_of` casts
  `island as u32` before `* 4` — safe at the current 5-island cap, but it's the
  encoder of the disjoint-GPU invariant; use u64 arithmetic so large ids can't
  alias.

---

## Verified solid (checked, no defect)

- Module wiring/registration: `IslandTopologyFold` (KIND_ID=4, no collision)
  registered + dispatched; all `gang`/`workflow` submodules declared, none orphaned.
- Dispatch encode/decode symmetry across all 7 `DISPATCH_TASK_*` tags
  (`adapter::ingest_typed` ↔ `WorkflowFold::apply`), each using the same payload
  struct.
- Checksum coverage over the rewritten meta + tail; `WatermarkingFold` forwarding.
- Quorum/fence pure predicates (`quorum.rs`): strict majority, monotonic `>=`
  fence, empty-set guard, distinct-ack counting.
- Ordered-acquire deadlock-freedom (`multi.rs`), saturating arithmetic throughout.

---

## Suggested order of attack

1. **#1** (one-line self-apply) and **#2** (transition guard) — highest impact,
   clear fixes.
2. **#3 / #4** — align the partition-safety story (model + live wiring) before the
   CP gate is relied upon.
3. **#5, #6, #7** — contract/leak fixes, individually small.
4. **#9, #11, #13** — low-risk cleanups.
