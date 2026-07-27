# MeshOS branch code review — 2026-05-13

Branch: `meshos-2` (~9.5K LOC: Rust substrate + tests + docs). Baseline: `master`.

Three parallel review passes covered the async stitching layer
(`event_loop` + `runtime`), the pure-sync decision logic
(`reconcile` + `scheduler` + `supervision` + `maintenance` + `state`),
and the backpressure / dispatch / chain integration
(`backpressure` + `executor` + `chain` + `action`). Findings below —
punch list grouped by severity, each item labeled for tracking in
this doc only (per the "no review-tracking IDs in code or commit
messages" feedback rule).

## Status

**Closed.** Every Critical / Important / Nit was fixed in-tree
with per-item regression coverage where reasonable. Tests added
under the `meshos` feature: `BackpressureState::release_failed_admit_*`
(3), `executor::cluster_backpressure_edges_surface_through_dispatcher_hook`,
`executor::dispatch_failure_with_retry_releases_pull_cooldown`,
`executor::dispatcher_panic_does_not_kill_executor`,
`executor::dispatch_retry_drops_after_exceeding_max_defer_count`,
`event_loop::snapshot_reader_does_not_stall_under_concurrent_reads`,
`event_loop::dropped_actions_counter_increments_when_action_queue_is_full`,
`event_loop::panicking_probe_does_not_kill_the_loop`,
`event_loop::publish_timeout_returns_queue_full_when_loop_is_wedged`,
`event_loop::shutdown_event_short_circuits_pending_events_after_it`
(re-pinned to actually assert),
`reconcile::scheduler_eviction_is_idempotent_when_loop_writes_back_last_rebalance`,
`reconcile::phase_c_overcount_eviction_suppresses_phase_d1_eviction_for_same_chain`,
`reconcile::apply_backoff_is_not_re_emitted_after_the_loop_records_it`,
`state::enter_maintenance_since_is_anchored_on_last_tick_for_replay_determinism`,
`runtime::dropping_runtime_without_shutdown_aborts_tasks`,
`chain::buffering_appender_drops_oldest_when_at_capacity`,
`chain::decode_rejects_payload_with_unknown_wire_version`,
`chain::decode_rejects_empty_payload`,
`chain::encode_decode_round_trip_preserves_record`,
`snapshot::failure_record_age_ms_derives_from_recorded_at_ms`,
`sources::leader_lost_event_clears_replica_leader_via_none_update`.

`cargo clippy --features meshos --lib --tests -- -D warnings`
clean. `RUSTDOCFLAGS="-D warnings" cargo doc --features meshos
--no-deps --lib` clean. 172 meshos unit tests + 11 pipeline
integration tests pass.

## Critical

### C1 — Retry path bypasses `admit()`

**Where:** `src/adapter/net/behavior/meshos/executor.rs:316-352`.

When `dispatcher.dispatch().await` returns `Err(DispatchError { retry_after: Some(_), .. })`,
the action is re-pushed onto the deferred-action `BinaryHeap` without
going through `BackpressureState::admit` again. Resources `admit`
reserved on the first pass (`drain_window.push(now)` at `backpressure.rs:160`;
`last_pull_admitted = Some(now)` at `:119`) are never un-reserved on
failure. Two consequences: counters drift permanently after every
transient dispatch error, and pull-cooldown / drain rate-limit
throttles can be silently bypassed on retry.

**Fix shape:** route retries through `admit` like the deferred path,
or surface an explicit `admit_release()` hook so dispatch errors
can roll back reservations.

### C2 — Cluster backpressure broadcast is dead code

**Where:** `src/adapter/net/behavior/meshos/executor.rs:262-278`,
`src/adapter/net/behavior/meshos/backpressure.rs:194-218`.

`BackpressureState::update_cluster_backpressure` exists, is unit-tested,
and surfaces `ClusterBackpressureChange` events — but `ActionExecutor::run`
never calls it. The plan's promise to "broadcast `MeshOsControl::BackpressureOn { level }`
to supervised daemons" is implemented in isolation but disconnected
at runtime.

**Fix shape:** call `update_cluster_backpressure` once per `handle_one`
with the current queue depth; surface the returned `ClusterBackpressureChange`
through a dispatcher hook (or the chain appender) so daemons can
receive `MeshOsControl::BackpressureOn`/`Off`.

### C3 — Scheduler eviction not idempotent

**Where:** `src/adapter/net/behavior/meshos/reconcile.rs:317-380`.

The Phase D-1 scheduler arm reads cooldown state from
`actual.last_rebalance[chain]`, which is fold-side; reconcile is pure
and never writes back. Between the `RequestEviction` emission and
the fold updating `last_rebalance`, every reconcile call re-emits
the same eviction. At 500 ms ticks this is an action-queue growth
bug. The test at `reconcile.rs:467-492` only covers empty state; no
test pins double-reconcile idempotence with an active scheduler arm.

**Fix shape:** maintain `pending_evictions: HashSet<ChainId>` on
`MeshOsState`, written when reconcile emits a `RequestEviction` and
cleared when the fold observes the holder count drop. Reconcile gates
on this set instead of (or in addition to) `last_rebalance`.

### C4 — Silent drop of reconcile output

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:372`.

`let _ = self.actions_tx.try_send(pending);` discards actions when
the executor channel is full, with no counter, no `tracing::warn`,
and no entry in `recent_failures`. The defense in the existing
comment ("Phase A is a no-op") no longer holds — Phase B through D-1
reconcile arms do emit.

**Fix shape:** record drops via an `AtomicU64` counter on the loop;
surface via `RuntimeStats`; log on first occurrence per tick at
`tracing::warn` with the action kind.

### C5 — Snapshot publish is not lock-free despite the docs

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:152-154, 391`.

`*self.snapshot.write() = snap;` is an exclusive `parking_lot::RwLock`
write; `MeshOsSnapshotReader::read()` clones the snapshot under a
read lock. The "live, lock-free snapshot view" claim in
`MeshOsSnapshotReader`'s doc-comment and the plan does not match
reality. Under reader contention this can stall the loop.

**Fix shape:** switch to `arc_swap::ArcSwap<MeshOsSnapshot>`. Publish
becomes one atomic pointer store; read becomes one atomic load plus
`Arc` clone. Already a workspace dep (`arc-swap = "1.7.1"`).

## Important

### I1 — `ApplyBackoff` re-emits every tick while a daemon is `BackingOff`

**Where:** `src/adapter/net/behavior/meshos/reconcile.rs:86-97, 405-420`.

A `Stopped` daemon with `is_admissible == false` and a `release_at()`
always emits a fresh `ApplyBackoff { daemon, until }` action. No
"already applied" gate. The supervisor likely treats it as idempotent
at the executor, but the reconcile contract per the plan is
"second pass empty."

**Fix shape:** add a `last_applied_backoff: HashMap<DaemonId, Instant>`
sentinel on `MeshOsState`; only re-emit when the `until` actually
changes.

### I2 — Phase C and Phase D-1 can double-evict the same chain

**Where:** `src/adapter/net/behavior/meshos/reconcile.rs:130-188, 317-380`.

Both arms run every tick on the elected leader. `actual_count > desired_count`
(Phase C) and `worst_score < score_floor` (Phase D-1) can coincide;
they emit *different* `RequestEviction { victim: … }` per tick — different
victims, no de-dup. No interleaving test.

**Fix shape:** order the arms; have Phase D-1 short-circuit when
Phase C already emitted an eviction for the chain this tick. Add a
test for the interleaving.

### I3 — `Instant::now()` inside the fold breaks replay determinism

**Where:** `src/adapter/net/behavior/meshos/state.rs:287, 302, 332-335`.

Locked decision #5 in the plan: "maintenance state lives in chain
metadata, every node converges via RedEX." Two replays of the same
admin-event sequence produce different `since` values. `gc_avoid_list`
(line 333) calls `Instant::now()` even though `apply(Tick)` already
stamped `last_tick` two lines earlier.

**Fix shape:** plumb `now: Instant` into `apply` (or read `last_tick`
which is already populated) and use it everywhere inside the fold.

### I4 — `replicas: Vec<NodeId>` causes O(N·M) reconcile scans

**Where:** `src/adapter/net/behavior/meshos/state.rs:33`,
`reconcile.rs:139-201, 332-363`.

`pick_pull_source`, `all_replicas_drained_locally`, and the Phase D-1
inner loop linear-scan it; `apply_replica` does `contains` checks.
At 500 ms ticks across many chains, the quadratic cost dominates.

**Fix shape:** switch to `BTreeSet<NodeId>` — preserves the
deterministic-iteration property needed for lex-smallest source/victim
selection.

### I5 — Missing `Drop` on `MeshOsRuntime`

**Where:** `src/adapter/net/behavior/meshos/runtime.rs:40-54`.

Dropping the runtime without calling `shutdown()` detaches both
`JoinHandle`s: the loop + executor live forever, holding `Arc<Config>`,
the snapshot lock, the dispatcher `Arc`.

**Fix shape:** `Drop` impl that aborts both handles and emits a
`tracing::warn` if shutdown wasn't called explicitly.

### I6 — Probe / dispatcher panics kill the task with no restart

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:308-321`,
`executor.rs:316-352`.

A panicking probe unwinds the loop task; a panicking dispatcher
unwinds the executor task. No supervisor, no auto-restart, no
`tracing::error!` on the panic path.

**Fix shape:** wrap each probe call in `std::panic::catch_unwind`
(or `tokio::task::spawn`-and-watch for the dispatcher); on panic,
log + record a `FailureRecord` + carry on.

### I7 — `BinaryHeap` defer queue has no max-defer count or TTL

**Where:** `src/adapter/net/behavior/meshos/executor.rs:280-314`.

A persistently-deferring action accumulates in the heap forever.
No poison-pill detection.

**Fix shape:** track `defer_count` on each heap entry; drop with a
`FailureRecord` when it exceeds `max_defer_count` (default 16).

### I8 — `MissedTickBehavior::Skip` rather than `Delay`

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:276`.

Plan says heartbeat-aligned 500 ms; `Skip` drops missed ticks
silently under load. `Delay` is the conventional reconcile cadence
(drift but no skipped passes).

**Fix shape:** switch to `Delay`. Doc-comment the rationale.

### I9 — `BufferingActionChainAppender` is unbounded

**Where:** `src/adapter/net/behavior/meshos/chain.rs:147-178`.

Test-only, but a retry storm under `tokio::time::pause` OOMs.

**Fix shape:** add `with_capacity(N)` constructor + drop-oldest
semantics.

### I10 — Wire-compat gap on `ActionChainRecord`

**Where:** `src/adapter/net/behavior/meshos/chain.rs:43-59, :63`.

No `#[serde(deny_unknown_fields)]`. `ActionDisposition` is Rust-side
`#[non_exhaustive]` but not declared so on the wire — adding a
variant decodes as garbage on older nodes.

**Fix shape:** add a wire-format version byte to `ActionChainRecord`;
write a postcard cross-version compatibility test.

### I11 — `MeshOsHandle::publish` blocks indefinitely on slow consumer

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:172-177`.

With a wedged loop, `.send().await` parks the caller. `attach_to_*`
helpers use `try_publish` (good), but the `publish` surface is
documented as the source-side API.

**Fix shape:** add `publish_timeout(event, Duration)` with a
`tokio::time::timeout` wrapper; document `publish` as preferring
`try_publish` for non-blocking sources.

### I12 — `FailureRecord.age_ms` is always 0

**Where:** `src/adapter/net/behavior/meshos/chain.rs:246`,
`src/adapter/net/behavior/meshos/executor.rs:361`.

Records carry `emitted_at_ms` (replay-stable), but the fold throws
it away. The field is meaningless rather than wrong.

**Fix shape:** derive `age_ms` on snapshot read from `emitted_at_ms`
versus a fold-supplied "now," or remove the field. Keep the
serialized-snapshot shape stable.

## Nit

### N1 — Zero `tracing` calls across the entire `meshos` module

Verified by grep — neighboring adapters (`jetstream`, `redis`,
`mesh_rpc`, `orchestrator`) use `tracing` heavily. Loop start,
reconcile fire, action drops, shutdown phases, probe install — none
emit spans / events.

**Fix shape:** add `tracing::debug!` on loop start / shutdown,
`tracing::warn!` on dropped action / panicking probe.

### N2 — `Instant + Duration` without `checked_add`

**Where:** `src/adapter/net/behavior/meshos/supervision.rs:200, 212`;
`src/adapter/net/behavior/meshos/reconcile.rs:104`.

Panics on overflow in both debug and release. The codebase elsewhere
uses `saturating_duration_since` — be consistent.

**Fix shape:** switch to `checked_add(...).unwrap_or(now)` or the
equivalent saturating helper used in the substrate.

### N3 — No `LeaderLost` event

**Where:** `src/adapter/net/redex/replication_coordinator.rs:104-117, 395-423`.

`ReplicaTransitionEvent::LeaderChanged` fires only on `_ → Leader`.
`Leader → Replica` fires nothing — so `MeshOsState::replica_leader[chain]`
is only ever cleared by a different node's observer overwriting.
Steady-state OK; edge cases lossy.

**Fix shape:** add `ReplicaTransitionEvent::LeaderLost { origin_hash, at }`
fired on `Leader → {Replica, Idle}` transitions. Sink translates to
`MeshOsEvent::ReplicaLeaderUpdate { chain, leader: None }`.

### N4 — Doc says "Wall time" but type is `Instant`

**Where:** `src/adapter/net/redex/replication_coordinator.rs:104, 110, 116`;
`src/adapter/net/compute/daemon.rs` (lifecycle events).

`Instant` is monotonic and process-relative, not wall-clock. Pick
one and fix the docs.

**Fix shape:** change doc comments to "Monotonic timestamp of the
transition" (the type is correct for in-process ordering).

### N5 — `MeshOsLoop::new` returns a 4-tuple

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:215-217`.

Adding a metrics handle later is a breaking change.

**Fix shape:** return a `MeshOsLoopParts` struct. Adjust callers.

### N6 — `shutdown_event_short_circuits_pending_events_after_it` is misnamed

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:691-718`.

Test binds `count`, then `let _ = count;` — no assertion that
post-`Shutdown` events were skipped.

**Fix shape:** capture the reconcile count before publishing the
post-Shutdown event; assert it doesn't change.

### N7 — `probe_counts()` race

**Where:** `src/adapter/net/behavior/meshos/event_loop.rs:130-132`.

Two separate `.read()` calls; the pair isn't atomic. Diagnostic-only,
but the doc-comment markets it as a startup-readiness check, which
is exactly the wrong use of a non-atomic pair.

**Fix shape:** single `.read()` then read both lengths under the
same guard.

### N8 — Aspirational header comment about allocations

**Where:** `src/adapter/net/behavior/meshos/reconcile.rs` header
comment vs `reconcile.rs:234` and other arms.

`format!(...)` and Vec sort allocations happen every tick. Either
fix the comment or pre-allocate the `String` constants.

**Fix shape:** soften the header comment to "no allocations on the
common no-op path" — the reality.
