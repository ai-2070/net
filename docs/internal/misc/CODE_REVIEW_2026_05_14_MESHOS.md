# MeshOS branch code review — 2026-05-14

Branch: `meshos-sdk-plan` (~12K LOC: same substrate as 2026-05-13 +
new `MESHOS_SDK_PLAN.md` design doc + README MeshOS section).
Baseline: `master`. Companion to
[`CODE_REVIEW_2026_05_13_MESHOS.md`](CODE_REVIEW_2026_05_13_MESHOS.md).

Three parallel passes covered (1) verification that every item on
the previous punch list is still fixed in the current tree,
(2) a fresh top-to-bottom read of the substrate for new issues the
prior pass missed, and (3) a design review of the new SDK plan +
README MeshOS section against the substrate code they reference.

## Status

**Open.** Previous 25-item punch list re-verified clean (all PASS;
see [Section A](#a-previous-punch-list-re-verification)). New findings
in this pass: **5 Critical / 8 Important / 5 Nit** across substrate
and design docs. Per the "no review-tracking IDs in code or commit
messages" feedback rule, labels are for this doc only.

## A. Previous punch-list re-verification

Every C1-C5 / I1-I12 / N1-N8 fix from the 2026-05-13 review is
present in the current tree at the equivalent location (line
numbers have shifted with the formatting pass; the symbols and
regression tests are intact). Spot-checked:

- `BackpressureState::release_failed_admit` at `backpressure.rs:195`,
  called from `executor.rs:418`
- `update_cluster_backpressure` invoked from executor at `executor.rs:322`
- `evicted_this_tick` set gates Phase D-1 at `reconcile.rs:374`
- `dropped_actions` `AtomicU64` + `tracing::warn!` at `event_loop.rs:537`
- `arc_swap::ArcSwap<MeshOsSnapshot>` at `event_loop.rs:69`,
  reader at `:180`
- `MeshOsLoopParts` struct return at `event_loop.rs:283`
- `WIRE_FORMAT_VERSION: u8 = 1` + `DecodeError::UnsupportedVersion`
  at `chain.rs:51, 73`
- `impl Drop for MeshOsRuntime` aborting both tasks at `runtime.rs:312`
- `ReplicaTransitionEvent::LeaderLost` fired at
  `replication_coordinator.rs:453`
- Regression tests for every closed item are still present and
  asserting

No PARTIAL/FAIL findings on the previous closure.

## B. New findings — substrate

### Critical

#### C1 — `MeshOsSnapshot::from_state` hard-codes empty `recent_failures`

**Where:** `src/adapter/net/behavior/meshos/snapshot.rs:447`,
`src/adapter/net/behavior/meshos/executor.rs:231, 465`,
`src/adapter/net/behavior/meshos/event_loop.rs:556`.

Every snapshot the loop publishes is built via `from_state`, which
unconditionally sets `recent_failures: VecDeque::new()`. The
executor maintains its own failure ring (push at `executor.rs:465`,
state at `:231`) but nothing routes it into the snapshot. The
`MeshOsSnapshotFold::apply` path does populate `recent_failures` —
but only when records flow through a RedEX `ActionChainAppender`,
and `NoOpActionChainAppender` is the default appender at
`executor.rs:256`. Net effect with the shipped default: every
consumer reading `runtime.snapshot().recent_failures` sees `[]`
regardless of how many dispatch failures occurred. The README's
behavior-snapshot section advertises `recent_failures` as a
first-class field; the regression test
`failure_record_age_ms_derives_from_recorded_at_ms` (snapshot.rs:520)
exercises the fold path, not the publish path, so the bug is
invisible to the existing suite.

**Fix shape:** plumb the executor's `recent_failures` deque into
the snapshot — either by giving the loop a reference and copying
on publish, or by having the executor push a `FailureRecord` into
`MeshOsState` through the default `NoOpActionChainAppender` so
`from_state` reads it. Add a regression test that runs
`LoggingDispatcher::fail_next` and asserts
`runtime.snapshot().recent_failures` is non-empty.

#### C2 — `MeshOsRuntime` exposes no daemon-registration path

**Where:** `src/adapter/net/behavior/meshos/runtime.rs:81-309`
(full impl), `docs/plans/MESHOS_SDK_PLAN.md:118-122` (SDK depends on it).

The substrate's `MeshOsRuntime` exposes `handle`, `handle_clone`,
`snapshot`, `snapshot_reader`, `probe_registry`,
`scheduler_registry`, `executor_stats`, `dropped_actions`,
`shutdown`, `shutdown_with_timeout` — but **no accessor to
`DaemonRegistry` and no `register_daemon` method**. The
registry lives wherever the consumer constructs it (see
`tests/meshos_pipeline.rs:147` — the test wires the registry
separately from the runtime). The SDK plan's primary entry point
(`MeshOsDaemonHandle::register(runtime, daemon, keypair)`) has no
substrate to bind against. This is a critical scope-blocker for
all five language bindings, not a design flaw in the plan: the
substrate needs a `pub fn daemon_registry(&self) -> &Arc<DaemonRegistry>`
or the equivalent before any binding can land.

**Fix shape:** add `MeshOsRuntime` ownership of the `DaemonRegistry`
behind a `pub fn daemon_registry(&self) -> &Arc<DaemonRegistry>`
accessor (or take `Arc<DaemonRegistry>` at `start(...)` and store
it). Alternatively, update the SDK plan to accept a separate
`&DaemonRegistry` arg on `register()` — but this leaks the registry
as a user-visible top-level type, which is exactly what locked
decision 10 wants to prevent.

#### C3 — `pending_snapshot_actions` is "recently emitted," not "pending"

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:71-75, 300, 523, 548`.

The field is documented as "Pending-action ring buffer the snapshot
folds into its `pending` field. Each Tick rebuilds the snapshot
from it before clearing." The code does the opposite: actions are
pushed on emission (`:523`) and only evicted FIFO past
`action_queue_capacity` (`:548-550`). Nothing observes executor
drain. The snapshot's `pending` field thus surfaces the *last N
emitted* actions, not the actions currently in flight. Deck's
`pending → in-flight → completed` lifecycle (advertised in the
README) is unimplementable on this field: there is no transition
out of `pending`. Bounded memory because of the FIFO cap, but the
semantics are wrong.

**Fix shape:** rename the field to `recent_emissions` and adjust
the doc to "ring of the last N emitted actions; the executor does
not signal completion back, so this is *not* what is in flight,"
**OR** wire a completion signal from the executor (a
`tokio::sync::mpsc` of `ActionId` outcomes the loop drains on each
tick) and remove drained entries on observation. The first is the
honest cheap path; the second is what Deck wants.

### Important

#### I1 — `CommitMaintenanceTransition { DrainFailed }` drops the reason

**Where:** `src/adapter/net/behavior/meshos/action.rs:167-172`
(action variant), `src/adapter/net/behavior/meshos/maintenance.rs:83-88`
(state variant), `src/adapter/net/behavior/meshos/reconcile.rs:301`
(emission site).

`MaintenanceState::DrainFailed { since, reason: String }` requires
a `reason`; `MaintenanceTransition::DrainFailed` (carried in the
`CommitMaintenanceTransition` action) has no `reason` field.
Reconcile emits the action when the drain deadline elapses with
no reason context; whatever dispatches the chain commit must
either invent the string or surface "deadline elapsed" as a
hard-coded constant. The `MaintenanceTransitionObserved` event
fold path overwrites local state with whatever the dispatcher
chose, losing the information that drove the transition.

**Fix shape:** add `reason: Option<String>` to
`MaintenanceTransition::DrainFailed`; reconcile populates it from
the deadline-elapsed branch (e.g., `format!("drain timed out
after {ms}ms")`); state fold writes it into the local maintenance
state.

#### I2 — Public re-exports leak fold-target internals through `pub` fields

**Where:** `src/adapter/net/behavior/meshos/mod.rs:71, 109-112`,
`src/adapter/net/behavior/meshos/state.rs:30-72`.

`MeshOsState`, `DesiredState`, `BackpressureState` are re-exported
publicly and have `pub` fields (`replicas: BTreeMap<…>`,
`daemons: HashMap<…>`, `applied_backoffs: HashMap<…>`, etc.).
External consumers can mutate substrate state directly,
bypassing the fold — every idempotence assumption reconcile
relies on (only `apply(MeshOsEvent)` writes the actual side) is
unenforced. This is a footgun more than a runtime bug, but every
SDK binding that re-exports these types in turn (the plan
suggests sharing types across languages) propagates the
unsoundness.

**Fix shape:** make the field set `pub(crate)` and expose
read-only accessors (`fn replicas(&self) -> &BTreeMap<…>`,
`fn daemon(&self, id: DaemonId) -> Option<&DaemonRecord>`).
Drop the public re-exports from `mod.rs` unless an external
contract genuinely requires them — the SDK plan's locked
decisions say the daemon should not see substrate state, so
the re-exports are not needed for the binding surface anyway.

#### I3 — No `#[non_exhaustive]` on the six public Config structs

**Where:** `src/adapter/net/behavior/meshos/config.rs:11, 73, 123, 148`
(`MeshOsConfig`, `BackpressureConfig`, `LocalityConfig`,
`MaintenanceConfig`), `src/adapter/net/behavior/meshos/supervision.rs:24`
(`BackoffConfig`), `src/adapter/net/behavior/meshos/scheduler.rs`
(`SchedulerConfig`).

Every test in-tree builds these via struct-literal
(`event_loop.rs:565`, `runtime.rs:369`, `tests/meshos_pipeline.rs:36`,
etc.). Add a field tomorrow and every external call-site breaks
loudly — but adding a field is the natural extension path
(new throttle, new threshold, new ramp window). `MeshOsEvent` and
`MeshOsAction` correctly mark themselves `#[non_exhaustive]`; the
config side is the only public-API surface left unprotected.

**Fix shape:** mark all six `#[non_exhaustive]` and expose
`Default::default()` plus builder-style `fn with_*` setters for
each field that today appears in literals. Updates the in-tree
test sites to use the builders or `Default::default()`.

#### I4 — `BackpressureState::release_failed_admit` pop-by-equality drains the wrong entry

**Where:** `src/adapter/net/behavior/meshos/backpressure.rs:206-208`.

```rust
if self.drain_window.last() == Some(&now) {
    self.drain_window.pop();
}
```

The guard prevents popping when the matching push has aged out,
but doesn't prevent popping a *sibling* admit's slot pushed at
the same `Instant`. Two `MigrateBlob` admits at the same `now`
(possible when the executor handles two back-to-back actions in
the same scheduler quantum; `Instant::now()` resolution is
typically 100 ns on Windows / 1 ns on Linux but the clock is
sampled per `admit` call, not per action) where the second fails
— `release_failed_admit` pops the first's slot, silently lifting
its drain-rate reservation. Single-threaded executor makes this
unlikely but not impossible; the test
`release_failed_admit_drain_window` (backpressure.rs:594) only
covers the one-entry case.

**Fix shape:** tag drain-window entries with a fresh `u64`
allocation id at admit, return it as part of `AdmissionResult`,
and have `release_failed_admit` remove by id. Same shape for the
`pull_admitted_at` site (a single `Option<Instant>` — there the
race surfaces as restoring a *stale* `last_pull_admitted` rather
than dropping a sibling).

#### I5 — Replica step-down emits two events that can fragment under back-pressure

**Where:** `src/adapter/net/redex/replication_coordinator.rs:418-457`,
`src/adapter/net/behavior/meshos/sources.rs:102, 165`.

A `Leader → Idle` transition fires both `Idled` and `LeaderLost`.
The MeshOS sink translates these to `ReplicaUpdate::Removed`
followed by `ReplicaLeaderUpdate { leader: None }`. Sinks use
`try_publish` and increment a drop counter on `QueueFull`. If the
events channel is at capacity, one of the two can land while the
other drops — leaving the snapshot with either a phantom leader
on a non-holder, or a holder set with a leader-less entry. Both
states are visible to Deck and to reconcile (Phase C reads the
leader to gate eviction emission).

**Fix shape:** bundle the two transitions into one event variant
(`MeshOsEvent::ReplicaLeaderTransition { chain, holders_delta:
HoldersDelta, leader_now: Option<NodeId> }`) so the pair cannot
fragment. Alternatively, on the sink side, hold the second event
back if the first failed to publish; but the bundled-variant fix
is the canonically right shape.

#### I6 — `run_reconcile` samples `Instant::now()` three separate times per tick

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:496, 509`,
`src/adapter/net/behavior/meshos/state.rs:167`
(set in `apply(Tick)`).

The previous review's I3 anchored admin transitions on `last_tick`
to make replay deterministic. The reconcile path still calls
`Instant::now()` twice — once for `last_rebalance` / `applied_backoffs`
writeback, once for the `PendingAction.emitted_at` stamp — and
the `Tick` event's own `Instant::now()` is a third value yet again.
Reconcile itself reads `actual.last_tick` once, so idempotence is
fine, but the `emitted_at` writeback drifts across replays — the
exact pattern I3 fixed for admin transitions, present elsewhere.

**Fix shape:** compute `let now =
self.actual.last_tick.unwrap_or_else(Instant::now);` once at the
top of `run_reconcile` and pass it through every subsequent write.

#### I7 — `runtime.rs:269-271` comment misrepresents shutdown sequence

**Where:** `src/adapter/net/behavior/meshos/runtime.rs:269-275`.

The comment says

> Drop the handle so the executor's mpsc receiver sees None and
> exits. Replace with a closed sender clone so the `Drop` path
> doesn't fault.

Neither happens. The handle in question is the events sender
(not the actions channel); no replacement is performed; the
actions sender is dropped implicitly when the loop task ends.
The comment will mislead a future maintainer trying to follow
the shutdown protocol.

**Fix shape:** rewrite to match reality — "Loop already published
`MeshOsEvent::Shutdown`, exited, and dropped `actions_tx` as it
unwound. Executor sees `Recv::None` next pop and drains the
deferred heap before returning."

#### I8 — Module-level example perpetuates the `publish` wedge

**Where:** `src/adapter/net/behavior/meshos/runtime.rs:7-13` (ignored doctest
example) and the SDK plan's signatures that mirror it.

The prior review's I11 added `publish_timeout` as an escape hatch,
but the module-level documentation example still demonstrates
`publish` (the version that parks indefinitely on a wedged loop).
First contact with the API is via this example; consumers will
copy it. The SDK plan's pseudocode for every language likewise
calls a `publish`-shape API without acknowledging the wedge risk.

**Fix shape:** rewrite the example block to use `publish_timeout`,
or — better — add a comment on `publish` recommending
`publish_timeout` in production. The SDK plan should call out
the wedge behavior of any blocking-by-default publish surface in
section 8 (locked decision: "control-event delivery is at-most-once").

### Nit

#### N1 — `leader_lost_event_clears_replica_leader_via_none_update` accepts a false positive

**Where:** `src/adapter/net/behavior/meshos/sources.rs:435-441`.

The test asserts on the *post-state* via
`match snap.replicas.get(&0xBADC0DE)`, accepting `None` as a pass
("either acceptable"). If the fold-path translation regresses to
silently dropping the LeaderLost event, the test still passes —
exactly the regression a test on this code path needs to catch.

**Fix shape:** seed the fold with a replica entry that should
have its `leader` cleared, then assert `Some(record)` with
`record.leader == None`.

#### N2 — `apply(MaintenanceTransitionObserved)` unconditionally overwrites local state

**Where:** `src/adapter/net/behavior/meshos/state.rs:204-207, 227-230`.

A late-arriving observed event for an older state can push the
local machine backward (e.g., a queued `Maintenance →
ExitingMaintenance` chain record arriving after `Recovery`). The
state machine in `maintenance.rs` is forward-only; the fold
should reject backward transitions.

**Fix shape:** add a `is_valid_transition(current, new) -> bool`
helper that mirrors the diagram in `maintenance.rs:10-28`; gate
the assignment on it. Log + drop with `tracing::warn!` on
rejection.

#### N3 — `pending_snapshot_actions` doc comment contradicts the code

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:71-75`.

Independently of [C3](#c3--pending_snapshot_actions-is-recently-emitted-not-pending),
the doc text "each Tick rebuilds the snapshot from it before
clearing" is false. Even if C3 is closed by renaming the field,
the doc-comment fix is its own line item.

**Fix shape:** rewrite once C3's resolution is chosen.

#### N4 — `MeshOsAction::StopDaemon` / `ApplyBackoff` carry `Instant` deadlines

**Where:** `src/adapter/net/behavior/meshos/action.rs:84, 160`.

`Instant` is process-relative; two nodes folding the same action
record cannot compare them. The `ActionChainRecord` form flattens
to `_at_ms`, but the in-process action surface (what the
dispatcher consumes) keeps the `Instant`, which means a remote
node replaying the chain to populate its own dispatcher view
sees garbage deadlines if anything mirrors actions cross-node.

**Fix shape:** action variants carry `Duration since emission`
instead of `Instant`; dispatcher converts to a local `Instant` at
the call site. Or leave `Instant` and document the in-process
restriction explicitly.

#### N5 — Test `runtime_executor_stats_increment_on_dispatch` is race-prone

**Where:** `src/adapter/net/behavior/meshos/runtime.rs:474-492`.

Tests `sleep(Duration::from_millis(100))` then samples
`executor_stats().dispatched`. No explicit drain barrier —
the action may still be sitting on the deferred heap or
in-flight in the dispatcher future when the assertion fires.
Test is currently passing in CI but is one scheduler change
away from a flake.

**Fix shape:** use `tokio::time::pause()` + explicit `advance`,
or replace the sleep with a "wait until `dispatched > 0` with
timeout" poll.

## C. New findings — SDK plan + README

### Critical

#### C4 — C SDK header location contradicts MeshDB precedent

**Where:** `docs/plans/MESHOS_SDK_PLAN.md:66, 347`.

The plan places `net_meshos.h` at
`bindings/go/meshos-ffi/include/net_meshos.h`, "shared with Go."
The shipped MeshDB precedent puts the C header at the top-level
`include/net_meshdb.h` — next to `net_rpc.h` (verified:
`include/` contains `net_rpc.h` and `net_meshdb.h`; no header
lives under `bindings/go/`). Section 5 cites the MeshDB FFI
pattern explicitly; the citation is wrong about header layout.
The plan also references the cdylib at
`bindings/go/meshos-ffi/`, which doesn't exist yet — fine for
forward-looking, but the wording reads as if it does.

**Fix shape:** move the planned header to `include/net_meshos.h`;
drop "shared with Go" (the Go binding loads from the cdylib —
the C header is a standalone artifact); soften "Lives in
`bindings/go/meshos-ffi/`" to "will live in…" and add the FFI
crate to the explicit Phase-5 deliverables.

#### C5 — SDK omits `MeshDaemon::is_stateful()` from every binding

**Where:** `docs/plans/MESHOS_SDK_PLAN.md:70` and per-language
sections; `src/adapter/net/compute/daemon.rs:112-114` (the
substrate method).

The shipped `MeshDaemon` trait has `is_stateful() -> bool` as a
load-bearing method: the default `restore` impl
(`daemon.rs:129-139`) rejects non-empty bytes when `is_stateful()
== false`, surfacing migration misconfiguration. The plan
enumerates "name, process, snapshot, restore, requirements,
required_capabilities, optional_capabilities, health, saturation,
on_control" — `is_stateful` is missing from every binding's
surface. Without it, a Python/Node/Go daemon that *intends* to
be stateful will hit the default-restore reject the first time
MeshOS migrates it and silently fail. Locked decision 5
("Snapshot / restore is opaque bytes") understates the contract
— the SDK must expose the statefulness declaration alongside
snapshot/restore, even if the bytes are opaque.

**Fix shape:** add `is_stateful` to every binding's surface;
document that `snapshot` / `restore` / `is_stateful` must be
overridden together; tighten locked decision 5 to "Snapshot
content is opaque bytes; the daemon declares
`is_stateful()` separately."

### Important

#### I9 — Decision 8 (at-most-once control delivery) has no substrate backing

**Where:** `docs/plans/MESHOS_SDK_PLAN.md:215, 220, 432`
(plan side); `src/adapter/net/compute/daemon.rs:170-178`
(substrate side).

The plan locks: "If the daemon doesn't consume a control event
before the next one fires, the older event is dropped + a metric
increments." But the substrate path runs through
`MeshDaemon::on_control(&mut self, event)` — synchronous, called
between `process()` events on the daemon task. There is no mpsc,
no buffering, no drop counter visible anywhere in
`compute::daemon` or `meshos::control`. The plan's "per-daemon
mpsc" (line 222) is also an invention — the SDK has to build it.
The "older event is dropped" rule needs an explicit backing
channel shape, otherwise it's unimplementable.

**Fix shape:** spell out — "Each binding builds a per-daemon
bounded channel (capacity 1) inside the wrapper. `try_send` on
overflow drops the older event and bumps a counter exposed on
`MeshOsDaemonHandle::dropped_control_events()`. Substrate-side
`on_control` continues to be a sync callback the binding
adapter feeds the channel from." Add it to locked decision 8.

#### I10 — Python sync `next_control()` GIL contract unspecified

**Where:** `docs/plans/MESHOS_SDK_PLAN.md:215, 220`.

"`next_control()` is sync-blocking (per Python convention)." A
`with_gil` block that parks on a Tokio receiver freezes every
other Python thread in the process. MeshDB's Python binding
doesn't expose long-blocking calls so the precedent doesn't
transfer cleanly. The plan needs an explicit GIL-release shape
(`py.allow_threads` around the receiver wait) or a polling
`next_control(timeout_ms=...)` shape. The C SDK already takes
`timeout_ms` (line 369) — the same shape would work in Python.

**Fix shape:** add a paragraph under section 2 specifying the
GIL contract; use `py.allow_threads` for the receiver wait;
optionally accept a `timeout_ms` parameter mirroring the C
shape.

#### I11 — Phase 1 gate language is stale

**Where:** `docs/plans/MESHOS_SDK_PLAN.md:495`.

"the Rust SDK (Phase 1) lands once the daemon-trait extension
settles in production." The trait extension (`health` /
`saturation` / `on_control`) has already shipped on
`MeshDaemon` (`daemon.rs:152-178`) and is documented in the
README at line 880 as live. The gating language is stale.

**Fix shape:** change to "the daemon-trait extension has shipped
(`compute::daemon`); Phase 1 unblocks on the first non-Rust
consumer."

### Nit

#### N6 — `MESHOS_PLAN.md:186` shows stale `MeshOsControl` trait shape

Out of scope for the SDK plan PR but worth a follow-up edit:
`MESHOS_PLAN.md:186` still shows `fn on_control(&mut self, _event:
MeshOsControl) {}`, even though the shipped trait takes
`DaemonControl` (the SDK plan correctly references this). Refresh
the older plan to match shipped substrate.

#### N7 — Forward refs to non-existent plans

`docs/plans/MESHOS_SDK_PLAN.md:42, 84` reference
`MESHAPP_SDK_PLAN.md` and `MESHOS_OPS_PLAN.md`, neither of which
exists in the tree. Acceptable as forward refs; consider adding
"(future, not yet drafted)" so a reader following links isn't
confused.

#### N8 — README numeric / structural claims all verify clean

For the record, every numeric and structural claim in the new
README "## MeshOS" section is correct against the substrate:
250 ms `pull_cooldown`, 60 s `replica_stabilization_window`,
10/s `drain_rate_per_zone_per_sec`, 1000/200 cluster-backpressure
high/low (`config.rs:108-118, 160-167`); 500 ms tick +
`MissedTickBehavior::Delay` (`event_loop.rs:361-369`);
`ArcSwap<MeshOsSnapshot>` (`event_loop.rs:69`); `non_exhaustive`
on `MeshOsAction` (`action.rs:64`); `meshos = ["cortex"]` off
by default (`Cargo.toml:78`); 5 min recovery ramp, `max_defer_count`
= 16, `WIRE_FORMAT_VERSION = 1`. No README findings.

## Suggested triage

- **Land first** (true correctness gaps that affect Deck or downstream
  consumers today): C1 (empty `recent_failures`), C3 (`pending` is
  recently-emitted), I5 (replica events fragment), I6 (replay drift on
  `emitted_at`).
- **Land before any SDK binding ships:** C2 (no register path), C5
  (`is_stateful` missing), I9 (decision 8 backing).
- **Land before publishing the README as canonical:** I8 (publish
  wedge in the module example).
- **Defer** until the public API is being firmed up: I2 (re-export
  hygiene), I3 (`#[non_exhaustive]` on configs), N4 (Instant in
  actions) — these are footguns, not bugs.
