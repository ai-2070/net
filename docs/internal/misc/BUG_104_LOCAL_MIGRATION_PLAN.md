# BUG #104 — Local-Source Migration Bypasses `MigrationSourceHandler`

Resolves the deferred entry from `BUG_AUDIT_2026_04_30_CORE.md:348`:

> When `source_node == self.local_node_id`, `start_migration` calls
> `daemon_registry.snapshot()` directly and never invokes
> `MigrationSourceHandler::start_snapshot`. As a result,
> `source_handler.is_migrating(origin)` returns `false`, no events are
> buffered on the source side, and the source daemon stays registered
> — continuing to accept `deliver()` calls and mutating its in-memory
> state *after* the snapshot has been sent to the target. Events
> arriving between snapshot capture and cutover are lost.

## Current shape

`MigrationOrchestrator::start_migration` (`orchestrator.rs:891-967`)
branches on `source_node == self.local_node_id`:

- **Remote source:** returns `MigrationMessage::TakeSnapshot { daemon_origin, target_node }`. The caller forwards this on the wire; the source's `MigrationDispatcher` routes it through `source_handler.start_snapshot(...)` (`migration_handler.rs:310-312`), which registers the migration in the source-side handler and starts buffering events that arrive during the snapshot/transfer window via `source_handler.buffer_event(...)`.

- **Local source (the bug):** captures the snapshot directly via `daemon_registry.snapshot(daemon_origin)`. No `start_snapshot` call. No source-side migration record. `is_migrating(origin)` returns false. `DaemonRegistry::deliver` keeps routing events into the live source daemon's state — past the snapshot's `seq_through`, with no buffer to capture them. At cutover the source unregisters with seq=120 of unsaved state; target activates at seq=100. Events 101-120 are lost.

The dispatcher's path is the correct one. The local-source path needs to invoke the same `source_handler` machinery.

## Design constraint

`MigrationOrchestrator` does not currently hold a reference to
`MigrationSourceHandler`. The dispatcher (`MigrationDispatcher` in
`subprotocol/migration_handler.rs`) holds both `Arc<MigrationOrchestrator>`
and `Arc<MigrationSourceHandler>` and stitches them together at the
wire-message boundary. The orchestrator was designed to be the
"control plane" — it tracks migrations via its own `migrations:
DashMap` and didn't need the source-side handler because the wire
path always went through the dispatcher.

For the local-source case we need the source-handler's state-tracking
*before* the wire path is involved. The cleanest fix is:

**Option A** — give `MigrationOrchestrator` an `Option<Arc<MigrationSourceHandler>>`
field. When present, the local-source path calls
`source_handler.start_snapshot(...)` instead of
`daemon_registry.snapshot(...)`.

**Option B** — pass the source handler in only at `start_migration`
call time (an additional argument). Callers that want the local-source
path correct must pass it; tests that don't go through the dispatcher
can pass `None`.

Option A is cleaner — the orchestrator's `new` already takes
`daemon_registry`; one more `Arc` of related-state machinery is in
keeping. Wire-up site (the dispatcher) gets a tiny constructor
change. Tests construct without the source handler when they don't
exercise local-source migration.

**Pick: Option A.** Add `source_handler: Option<Arc<MigrationSourceHandler>>`.
`new` keeps the original signature for back-compat with tests; a
new `with_source_handler(...)` setter or a second constructor wires
it up at production sites.

## Files touched

1. `src/adapter/net/compute/orchestrator.rs` — `MigrationOrchestrator`
   gains `source_handler: Option<Arc<MigrationSourceHandler>>`. New
   `with_source_handler(self, h: Arc<MigrationSourceHandler>) -> Self`
   builder method. `start_migration` local-source branch calls
   `source_handler.start_snapshot(...)` when the field is `Some`,
   falls back to the current direct-snapshot path with a
   `tracing::warn!` (test-only behavior, not reachable in
   production once the dispatcher wires it up).
2. `src/adapter/net/subprotocol/migration_handler.rs` —
   `MigrationDispatcher::new` (and `_with_target_handler`) wires
   `orchestrator.with_source_handler(source_handler.clone())`. The
   dispatcher already has the source handler; this just makes it
   available on the orchestrator too.
3. `src/adapter/net/compute/migration_target.rs` (or wherever the
   remaining migration tests live) — verify no test references
   `MigrationOrchestrator::new` and then takes a local-source
   path; if any do, they need the new builder.

Tests:
4. New regression test `start_migration_local_source_routes_through_source_handler`:
   - Build a `MigrationSourceHandler` + `MigrationOrchestrator::new(reg).with_source_handler(handler)`.
   - Register a daemon, give it some state, call `start_migration(origin, local_node, target_node)`.
   - Assert `source_handler.is_migrating(origin) == true` AFTER the call returns.
5. New regression test `events_after_local_snapshot_are_buffered_not_lost`:
   - Same setup. Daemon at seq=K when snapshot taken. Subsequent `deliver()` advances source state — events between the snapshot's `seq_through` and cutover MUST be buffered (visible via `source_handler.take_buffered_events()`), not silently mutated into the source daemon.
   - This is the exact #104 failure mode; pin it directly.

Audit doc:
6. Mark #104 FIXED in `BUG_AUDIT_2026_04_30_CORE.md`. Move from the
   Outstanding Deferred list.

## Implementation order

1. Add the new field + `with_source_handler` builder.
2. Branch the local-source path on `source_handler.is_some()`.
3. Wire the dispatcher constructor to set the field.
4. Sweep callers (cargo check).
5. Add the two regression tests.
6. Verify the broader migration test suite still passes.
7. Audit doc update.

## Non-goals

- Refactoring `MigrationOrchestrator` to mandate a source handler.
  Keeping the field optional means existing tests don't need to
  thread the handler through unless they exercise the local-source
  code path.
- Buffering events without a registered source-handler migration.
  That's a different bug (#147-style "deliver during migration
  before start_snapshot is called"). Out of scope here.
