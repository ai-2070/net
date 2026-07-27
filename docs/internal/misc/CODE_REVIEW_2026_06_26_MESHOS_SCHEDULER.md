# MeshOS ↔ Scheduler bridge code review — 2026-06-26

Branch: `meshos-scheduler` (~2.7K LOC: new `behavior::scheduler_bridge`
module of 9 files implementing the five MeshOS↔Scheduler projections, a
node-pin seam threaded through `meshos::{reconcile, state, event}`, a
liveness-prune seam in `gang::match_islands` + `MeshNode`, tests, and
`docs/plans/MESHOS_SCHEDULER_INTEGRATION_PLAN.md`).
Baseline: `master`.

The review was a top-to-bottom read of the new module and every seam it
touches, plus a trace of the desired/observed projection paths through
reconcile. `cargo check -p net --features cortex,meshos --tests` is
**clean** on the branch (exit 0).

## Status

**Resolved (2026-06-26).** All 7 findings (2 Important / 3 Minor / 2 Nit)
fixed on this branch, each as its own commit with tests where reasonable.
The production paths worked as built; every Important item was a *latent*
risk (reachable only under flows beyond the Phase-A scope, or an efficiency
cost at the stated scale) rather than a live defect. Per the
"no review-tracking IDs in code or commit messages" feedback rule, the
labels below are for this doc only.

| Finding | Fix commit (subject) |
| --- | --- |
| I1 — pin gate skips orphan stop | `fix(meshos): pinned daemon stops orphan replicas on non-target nodes` |
| I2 — per-tick snapshot deep clone | `perf(scheduler-bridge): borrow the snapshot in tick() instead of deep-cloning` |
| M1 — merge vs LWW divergence | `fix(scheduler-bridge): make forced-placement return pins, not Run intents` |
| M2 — republish-all per tick | `perf(scheduler-bridge): publish only changed daemon intents per tick` |
| M3 — deleted-task daemon leak | `fix(scheduler-bridge): tear down a deleted task's daemon in tick()` |
| N1 — `daemon_ref` collision overclaim | `docs(scheduler-bridge): correct the daemon_ref collision claim` |
| N2 — per-signal map rebuild | `perf(scheduler-bridge): skip the daemon→task map build for no-op signals` |

Verification after the fixes: the scheduler_bridge lib tests (31),
`reconcile` node-pin tests, and the `scheduler_bridge_driver` (4),
`gang_claim_node` (5), and `meshos_pipeline` (12) integration tests all
pass; `cargo clippy --features cortex,meshos --tests` is clean on the
touched files.

## What's good (kept deliberately)

- The LD-5 boundary is genuinely enforced: `scheduler_bridge` is the only
  module importing both `cortex::workflow` and `meshos`, and every
  projection is pure (reads state, returns a value, no I/O, no call back
  into either layer). The split between the pure facade (`SchedulerBridge`,
  no live handles) and the I/O driver (`SchedulerBridgeDriver`) is clean.
- The migration veto is enforced by type, not convention: `MigrationEligible`
  has a private field and only `check()` constructs it, so a claim-holder
  literally cannot be passed to `migrate()` (the `compile_fail` doctest
  pins this). Nice.
- Liveness pruning is lock-free (`ArcSwap` on `MeshNode::liveness_down`),
  and prunes the candidate **host** set rather than mutating either fold —
  preserving the folds' CRDT/AP semantics. The `down_nodes` empty ⇒ no-op
  fast path keeps the match hot path free for the common case.
- `DaemonIntentUpdate` / `MeshOsEvent` are in-process only (no
  `Serialize`/`Deserialize`), so adding the `node` field carries **no**
  wire-compat risk — verified.
- Test coverage is thorough: Run/Stop mapping, the pin overlay, the
  stale-claim skip, the liveness classification, the driver tick + crash
  paths, and the Phase-A acceptance composition are all exercised.

---

## Important

### I1 — The node-pin gate skips both start *and* stop on non-target nodes → latent double-run

**Where:** `src/adapter/net/behavior/meshos/reconcile.rs:118-122`; test
`node_pinned_stop_is_ignored_by_non_target_nodes` (same file).

The pin gate `continue`s the whole daemon on any node that is not the pin
target — skipping `StopDaemon` as well as `StartDaemon`:

```rust
if let Some(pinned) = desired.desired_daemon_nodes.get(daemon) {
    if *pinned != this_node {
        continue;
    }
}
```

The regression test deliberately constructs a node that holds the daemon
`Running` in its own `actual.daemons`, pins it to `OTHER_NODE`, and asserts
the node emits **nothing** — framed as correct ("must not stop a daemon
the claim placed elsewhere"). But if `actual.daemons[d] == Running` on this
node, this node *is* the one running it. Skipping `Stop` there leaves it
running while the pin target also starts it → the exact double-book that
forced placement exists to prevent.

This is safe only under the invariant **"a daemon is never `Running` on a
node other than its current pin target."** That holds for the Phase-A flow
(claim acquired at `StepGate::Running`, daemon started on the pin node,
never moved), but it is fragile to:

- a claim released-then-reacquired while the task stays `Running` (re-pin
  to a different host), or
- `resolve_island_host` returning a different host for the same claim
  across ticks (island re-announced / host churn).

In either case the previous host keeps an orphaned daemon alive.

**Suggested fix:** the pin gate should guard only the *start* path. A
non-target node that observes the daemon locally `Running`/`Starting`
should still emit `Stop` (stopping-elsewhere is always safe; only
*starting* must be confined to the pin target). At minimum, document the
load-bearing invariant at the gate and rename the test so a future reader
sees the hazard rather than a rationalization of it.

### I2 — `tick()` deep-clones the entire MeshOS snapshot every pass

**Where:** `src/adapter/net/behavior/scheduler_bridge/driver.rs:213`;
`MeshOsSnapshotReader::read` at
`src/adapter/net/behavior/meshos/event_loop.rs:395`; cheaper `load()` at
`:403`.

```rust
let delta = project_liveness_from_snapshot(&self.snapshot.read());
```

`read()` is `(**self.snapshot.load()).clone()` — a full deep clone of the
whole `MeshOsSnapshot` (every daemon, replica, peer, …) on every tick.
`project_liveness_from_snapshot` only borrows `.peers`. The reader exposes
`load()` precisely for this case ("avoids the per-call deep clone when the
caller only needs a few fields"), returning an `Arc` guard:

```rust
let snapshot = self.snapshot.load();
let delta = project_liveness_from_snapshot(&snapshot); // deref-coerces to &MeshOsSnapshot
let down = delta.down.len();
```

At the project's millions-of-nodes target, cloning the full snapshot on a
periodic loop is avoidable waste with a one-line fix. (Keep the borrow
short — it pins the snapshot Arc — but the projection consumes it
immediately, so this is fine.)

---

## Minor

### M1 — `desired_daemon_intents` (merge) and the documented LWW overlay are not equivalent

**Where:** merge in
`src/adapter/net/behavior/scheduler_bridge/runtime.rs:56-68`;
`project_forced_placements` always emits `Run` at
`.../scheduler_bridge/projection.rs:88-92`; doc claiming LWW equivalence
at `projection.rs:73-77`.

The doc presents two interchangeable composition paths: apply Projection 1,
then overlay Projection 2 via `apply_daemon_intent` (last-write-wins). But:

- the production **merge** (`desired_daemon_intents`) overlays only the
  `node` field and *preserves* Projection 1's `Run`/`Stop`;
- `project_forced_placements` hard-codes `intent: DaemonIntent::Run`, so
  the documented **LWW** path lets a claim's `Run` clobber Projection 1's
  `Stop`.

These diverge for a claim still held against a non-`Running` task — e.g.
the window between `wf.fail(task)` and `on_released(task)` in the crash
observer (`driver.rs:120-123`), where the registry still has the claim but
the task is already `Failed`. The merge path yields `Stop`-pinned
(correct); the LWW path yields `Run`-pinned, keeping a failed task's daemon
alive. Production uses the merge path, so this is latent — but
`project_forced_placements` is a public export and the docstring advertises
LWW as equivalent.

**Suggested fix:** either drop the `Run` assertion from
`project_forced_placements` (emit only the pin, leaving intent to
Projection 1), or amend the docstring to state the LWW overlay is lossy
for held-claim/non-`Running` tasks and the in-process merge is the
canonical path.

### M2 — `tick()` republishes all N intents unconditionally each pass

**Where:** `src/adapter/net/behavior/scheduler_bridge/driver.rs:219-236`.

Each tick publishes one `DaemonIntentUpdate` per *live workflow task*, with
no diff against the previously published state. Reconcile is idempotent so
this is correct, but it is O(all tasks) events into a bounded channel per
interval; combined with `try_publish` silently dropping on a full channel
(so `published` under-counts and that tick's intent is simply lost until
the next pass), a busy node can lag its desired state. Acceptable for
Phase A; worth dirty-tracking (publish only changed intents) before scale.

### M3 — deleted-task daemon teardown has no owner

**Where:** documented at
`src/adapter/net/behavior/scheduler_bridge/projection.rs:33-37`; driver at
`driver.rs:211-238`.

`project_daemon_intents` correctly notes that a deleted task vanishes from
`WorkflowState` and so emits no intent, and that tearing down its daemon is
"the wiring layer's concern (re-derive `desired_daemons`, or emit an
explicit `Stop` on the delete edge)." The driver *is* that wiring layer and
does not do it — a deleted task's last `Run` intent persists in
`desired_daemons` and its daemon runs indefinitely. The gap is
acknowledged in the projection doc but currently unassigned at the
runtime; flag it so it isn't lost between "pure projection" and "driver."

---

## Nit

### N1 — `daemon_ref` "can't collide with a small system-daemon id" overclaims

**Where:** `src/adapter/net/behavior/scheduler_bridge/daemon_ref.rs:6-11`,
`:91-96`.

The doc/comment states the namespacing makes task ids unable to collide
with the small sequential ids the registry hands system daemons. But
`splitmix64` is a bijection whose output is uniform over the full `u64`
range, so a task ref *can* land on a small value — just at ~2⁻⁶⁴. The
XOR-domain tag decorrelates the spaces; it does not carve out the low
range. The test (`assert_ne!(daemon_ref(1).id, 1)`) checks specific values,
not a range guarantee. Practically negligible; the wording just slightly
overstates the guarantee ("astronomically unlikely" rather than "cannot").

### N2 — `lifecycle_transition` rebuilds the full daemon→task map per signal

**Where:** `src/adapter/net/behavior/scheduler_bridge/runtime.rs:115-122`;
observer path `driver.rs:107-115`.

Each lifecycle signal rebuilds the entire daemon→task map (O(tasks)) before
resolving one daemon. The docstring acknowledges this and offers the batch
escape hatch (call `build_daemon_task_map` once, then `apply_lifecycle`
per signal), but the shipped observer takes the per-event path, so a burst
of signals is O(signals × tasks). Fine at current scale; revisit if the
lifecycle signal rate climbs.

---

## Verification

- `cargo check -p net --features cortex,meshos --tests` — clean (exit 0).
- `NodeId` is `u64` in both `behavior::fold` and `behavior::meshos`, so the
  cross-module `node: Option<NodeId>` threading is type-consistent.
- The `match_islands` signature change (`down_nodes: &HashSet<NodeId>`) is
  propagated to all four call sites (`schedule_single`, `schedule_gang`,
  `workflow/step.rs`, `MeshNode::match_islands`); the three scheduler-path
  sites pass an empty set by design (liveness is fed only on the node claim
  path via `set_liveness_down` — deferred wiring, documented).
