---
title: Task Lifecycle and Workflows
description: A task in Net is a single-writer RedEX chain, and its state is the deterministic fold of the transitions written to it.
---

# Task Lifecycle and Workflows

A task lifecycle is a durable record of transitions for one unit of work. RedEX
stores the single-writer event chain; a deterministic fold derives the current
status from those events.

This surface records task state. It does not dispatch workers or make external
effects exactly once. The application still executes actions, manages leases or
idempotency where required, and records the resulting transition.

This is the layer that runs on top of a claim. The [gang-claim
scheduler](/docs/guides/gang-scheduler) decides _who_ holds a contended
resource; this decides what happens once it's held, and how that survives a
restart.

## The state machine

```rust
pub enum TaskStatus {
    Submitted, // created, not yet running
    Running,   // a step is executing
    Waiting,   // parked on a trigger or a claim it lost — will re-request
    Blocked,   // parked on an unmet dependency
    Done,      // terminal
    Failed,    // terminal
}
```

`Waiting` and `Blocked` are distinct on purpose, and the distinction is
operationally load-bearing. `Waiting` means _the task will retry by itself_ — it
lost a gang claim, or it's parked on a trigger that hasn't fired. `Blocked`
means _something else must finish first_. A queue full of `Waiting` tasks is a
contention problem; a queue full of `Blocked` tasks is a dependency problem, and
they want opposite responses from you.

## Driving a task

Open an adapter over a RedEX chain and write transitions:

```rust
use net_sdk::cortex::{workflow::WorkflowAdapter, Redex};

let redex = Redex::new();
let wf = WorkflowAdapter::open(&redex, 0xABCD_EF01).await?;

wf.submit(1)?;                  // Submitted
wf.start(1)?;                   // Running
let seq = wf.complete(1)?;      // Done — terminal

// Transitions are appends; `wait_for_seq` waits for the fold to catch up.
wf.wait_for_seq(seq).await.ok();

let state = wf.get(1).expect("task present");
assert!(state.status.is_terminal());
```

Every transition returns the sequence number of the event it appended. That
number is the only handle you need for read-your-writes: `wait_for_seq(seq)`
returns once the fold has applied at least that far.

The other transitions — `wait`, `block`, `fail` — mirror the enum. There is no
"update task" call, because there is no task record to update.

## Restoring recorded state after restart

When the chain is stored durably, a restarted process can rebuild task status by
replaying it. A `Running` status means only that the transition was recorded; the
process must still reconcile any in-flight or external work before continuing.

Single-writer is the constraint that makes this safe. One writer per chain means
transitions are totally ordered without consensus, and the fold can't diverge
between readers. If you need multiple producers, they get multiple chains.

## Fan-out and fan-in

For map-reduce shapes, derive shard ids, fan them out, and join:

```rust
use net_sdk::cortex::workflow::{
    derive_shard_ids, fan_out, try_join_with, Join, JoinPolicy,
};

let shards = derive_shard_ids(parent_id, 16);
fan_out(&wf, parent_id, &shards)?;

match try_join_with(&wf, parent_id, &shards, JoinPolicy::AllOrNothing)? {
    Join::Ready => { /* reduce */ }
    Join::Pending => { /* not yet */ }
    Join::Failed => { /* a shard failed — apply the disposition */ }
}
```

`JoinPolicy` is where you decide what "finished" means, and the default is the
strict one:

- **`AllOrNothing`** (default) — reduce only when every shard is `Done`; any
  failure fails the join.
- **`BestEffort`** — reduce once every shard is terminal, success or not, and
  let the reducer inspect which succeeded. For embarrassingly-parallel work
  where partial results are usable.
- **`Threshold(n)`** — reduce once `n` shards are `Done`. It becomes
  `Failed` as soon as too many shards have failed for `n` to still be reachable,
  rather than hanging until a timeout.

A failed shard surfaces as `Join::Failed` rather than hanging the reduce. Apply
the disposition with `propagate_failure` (fail the parent) or `block_on_failure`
(park it for intervention).

## Triggers

The trigger engine is a **pure, side-effect-free substrate**. You arm a trigger
against an action, drive it with events, and it hands back the actions that are
now satisfied — it starts nothing itself:

```rust
use net_sdk::cortex::workflow::{Action, Trigger, TriggerEngine};

let mut engine = TriggerEngine::new(&wf)?;
engine.arm(trigger, action)?;

for satisfied in engine.on_task_change(task_id, new_status)? {
    // The caller applies it. The engine never runs anything.
}
for satisfied in engine.on_tick(now)? {
    // Time-based triggers fire here.
}
```

The engine returns actions rather than performing them. The caller decides how to
start work, acquire a resource, invoke another system, retry, deduplicate, and
record the result.

## What the lifecycle does not provide

- worker dispatch or process supervision;
- distributed locking beyond a separately acquired claim or lease;
- idempotency for an external side effect;
- compensation after a partial effect;
- reconciliation with an external system of record.

Use the lifecycle to make those decisions and their outcomes visible, not as a
substitute for executing them.

## Naming collision worth knowing

The cortex module has a _separate_ `Task` / `TaskStatus` pair in its tasks
model. The workflow lifecycle types live under `cortex::workflow::` specifically
to avoid colliding with them. If a `TaskStatus` doesn't have the variants above,
you've imported the other one.

## Where to go next

- [Gang-claim scheduler](/docs/guides/gang-scheduler) — claiming the resource a
  task runs on.
- [Durable logs (RedEX)](/docs/guides/durable-logs) — the chain underneath.
- [Folded state (CortEX)](/docs/guides/cortex-folds) — how a fold turns a log
  into state.
- [Recover a failed workflow](/docs/guides/recover-failed-workflow) — the
  operational side.
