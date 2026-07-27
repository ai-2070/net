# Code Review — `bugfixes-15` vs `master`

**Date:** 2026-05-18
**Scope:** All non-doc changes on branch `bugfixes-15` (110 commits, ~98 code files, +7,401 / −1,382 LOC).
**Method:** Five parallel deep-read passes — replication; compute; meshos + capability/auth; dataforts + adapters; FFI + bus/shard/consumer/tests.
**Focus:** Bugs introduced by the fixes themselves, or hazards the fixes leave open. Items where the fix lands cleanly are not listed.

Most of the sweep is solid. The findings below are where reviewers identified the fix as itself buggy or where a new bug was introduced.

---

## Critical

### C-1. `compute/migration_source.rs:330-348` — `cleanup` unregisters daemon on lookup miss
`MigrationSourceHandler::cleanup`'s new `Cutover` phase guard runs `if let Some(entry) = ...`, but the `migrations.get` miss falls through to `daemon_registry.unregister(daemon_origin)` unconditionally. A spurious or replayed `CleanupComplete` for an origin we never migrated tears down a live local daemon.
**Fix:** Move `unregister(daemon_origin)` inside the `Some` branch, or fail-closed on miss.

### C-2. `compute/standby_group.rs:889-916` — `try_recover_inner` clobbers the active
The unhealthy filter does not exclude `self.active_index`. If the active is briefly marked unhealthy (e.g., its node heartbeats stale), recovery constructs a fresh `DaemonHost::new` (empty state, `synced_through = 0`, `last_sync = None`) and `registry.replace`s the live active — silently dropping all committed state and the post-sync buffer.
**Fix:** Route active-side unhealthiness through `promote`, not slot re-placement. (ForkGroup / ReplicaGroup have no "active" concept and are unaffected.)

---

## High

### H-1. Replication dual-leader sticky-tiebreak inconsistency
- `replication_runtime.rs:861-873` (dual-leader convergence): tiebreaks on `(higher tail_seq, lower node_id)`.
- `replication_heartbeat.rs:142-148` (`record_heartbeat`): tiebreaks on `lower node_id` only and is **sticky** — the test at `:308-318` asserts an established small-id leader is never overwritten even by a high-id leader with newer tail.

Net effect: a real leader L1 (high tail, high id) can stay Leader while *also* recording L2 (low tail, low id) as `believed_leader`. L1's replica-side gates (`leader_belief != Some(from)` at `:1182`, `:1380`) then trust L2's SyncResponses while L1 still emits Leader heartbeats. R-22's stickiness re-opens what R-21 patches.

### H-2. `compute/migration_target.rs:377-388` — `activate` flips `Cutover` before `drain_pending`
If `drain_pending` returns `Err` mid-batch, phase is already `Cutover`; the new `replay_events` guard no-ops in `Cutover` and `buffer_event` rejects in `Cutover`. The undelivered tail is reinserted into `pending_events` but no future call can drain it. Source's retry returns the old `replayed_through` and considers it handled. Lost events, no retry path.
**Fix:** Set `Cutover` only on successful drain, or have `activate` retry the drain on next call without the early-return.

### H-3. `compute/standby_group.rs:889-920` — `try_recover_inner` does not bump `term`
This is exactly the X-1 fencing gap that `ForkGroup::try_recover_inner:806-812` and `ReplicaGroup::try_recover_inner:667-671` were written to guard against. StandbyGroup bumps `term` in `promote` / `promote_with_placement` but not in `try_recover` — the analogous window stays open.

### H-4. `consumer/merge.rs:639` and `:837` — `PollMerger::poll` discards `set_checked` bool
Both Step-1 (`adapter next_id`) and Step-2 (last-event override) writes route through `set_checked` but ignore the `bool` return. On a format-mismatch refusal, fetched events are still returned in `all_events`, yet the cursor is not advanced. The caller next-polls with the same input cursor → identical events → infinite duplicate-delivery loop. Pre-fix the cursor at least advanced.
**Fix:** On refusal, drop the offending shard's events from `all_events`, or mark the shard in `failed_shards`.

### H-5. `adapter/net/subprotocol/migration_handler.rs:486-502` — `SnapshotReady` TOFU into orchestrator binding
For a `daemon_origin` with no prior record, `restore_on_target` runs and `target_handler.orchestrator_node` is recorded as `from_node` (`mesh.rs:947`). Any session peer that beats the legitimate orchestrator with a forged `SnapshotReady` becomes the bound orchestrator and can drive `ActivateTarget` / `MigrationFailed` past the new peer-auth gates — the gates check against an attacker-established binding.
**Fix:** Bind the orchestrator out-of-band (e.g., when the factory is installed).

### H-6. `dataforts/blob/mesh.rs:772-794` — D-1 sweep can orphan on-disk chunks
`remove_if_deletable` now drops the refcount entry **before** `close_and_unlink_file`. On a close failure the refcount is gone (warn log openly admits "chunk file may persist"), so no future GC sweep (which enumerates refcounts) can find the orphan. Pre-fix order closed first, then `refcount.remove` on success.
**Fix:** Reverse the order, or schedule a disk-inventory orphan sweep.

### H-7. `redex/replication_runtime.rs:1248-1260` — Empty-response backoff misfires on stale leader_tail
Backoff records "empty" whenever `new_tail == pre_apply_tail && leader_tail > new_tail`, but `leader_tail` is the cached value from the last received Heartbeat (can be hundreds of ms stale). If the replica was momentarily ahead of its cached snapshot when SyncRequest fired and then the leader trims/idles, the replica accumulates empty strikes against a leader with nothing to send → 1–30 s backoff pause while nothing is wrong.
**Fix:** Use the response's `leader_first_retained_seq` / leader-tip hint, or only count empties when the request explicitly asked above tail.

### H-8. `redex/replication_runtime.rs:406-416` — `record_tail_seq` from `on_tick` advertises pre-quorum tail
The leader bumps `tail_provider` the moment a local write lands (pre-quorum). Advertising that via `record_tail_seq` (and thus via capability tags and the dual-leader tiebreak rule "higher tail wins") biases future elections toward the partition with un-replicated writes, increasing the chance the post-crash winner is the side that lost data.

---

## Medium

### M-1. `meshos/event_loop.rs:715` — `biased;` select inverts starvation direction
`biased` polls `tick.tick()` before `events_rx.recv()`. `run_reconcile().await` can yield while the executor channel is full, re-enter select, and tick wins again. Heavy reconcile + 100 ms tick (or paused tokio clock in tests) now starves `events_rx` instead of the reverse.
**Fix:** Gate with `if tick.has_pending()`, or poll events at least once between ticks via a counter.

### M-2. `meshos/event_loop.rs:1147` and `:1200` — `saturating_add` on audit seq is silent-loss trap
At `u64::MAX` every subsequent record gets the same seq; the SDK dedup gate keys on seq equality and collapses them all to a single entry. A panic on overflow (unreachable in practice — ~58 million years at 100 µs/event) would be safer than silent permanent audit-loss.

### M-3. `adapter/mod.rs:69-89` — `redact_url` leaks password suffix on unencoded `@`
The pinned test `"nats://admin:p@ss@nats.svc:4222"` → `"nats://[REDACTED]@ss@nats.svc:4222"` still emits the half of the password after the unencoded `@`. URI userinfo terminates at the **last** `@` of the authority, not the first.
**Fix:** `rfind('@')` within the authority slice.

### M-4. `dataforts/blob/mesh.rs:596-625` — D-17 candidate/commit split allows duplicate emissions
The mutex is dropped between `tick` and `commit_emissions`. A concurrent `tick_blob_heat` (overlapping scheduled and on-demand triggers) recomputes against the still-unmutated `last_emitted` and re-emits the same candidates.
**Fix:** Hold `reg.lock()` across `tick` / sink / commit, or stamp a generation on each candidate that `commit` can reject as stale.

### M-5. `compute/migration_source.rs:238-244` — Source-side buffer cap is O(N) per insert
Target was given a `pending_bytes` running counter (`migration_target.rs:34-39`) with explicit "O(1) — otherwise a wire-driven OOM" rationale; source walks `buffered_events` summing payload lengths on every `buffer_event`. Under the exact shape the cap was added to bound, this is O(N²) admission work over the migration life.
**Fix:** Mirror the running-counter approach.

### M-6. `dataforts/blob/mesh.rs:1226-1244` — Wrong error variant on short chunk
Defensive `get` on an over-short chunk returns `BlobError::HashMismatch { expected, actual: blake3::hash(&chunk_bytes) }`. Cause is a size disagreement, not a hash mismatch — and `actual` often equals `expected` for truncated tails, confusing retry logic.
**Fix:** Introduce `BlobError::ShortChunk`, or reuse `BlobError::Backend(...)`.

### M-7. `adapter/net/mesh.rs:4046-4068` — `from_node=0` resolver fallback weakens loopback gate
On resolver failure, dispatch falls through to sentinel `0`. Production `target_node_id != 0` callbacks still fail closed, but any callback registered with `target=0` (loopback test fold mixed into a session) is unconditionally satisfied by a real peer that happened to fail resolution.
**Fix:** Reject delivery entirely on resolution failure rather than falling through to sentinel.

### M-8. `event.rs:467` — Conditional 5th `dedup_id` field breaks `deny_unknown_fields`
`Serialize` emits `field_count = if some { 5 } else { 4 }`. Downstream Node/Python/Go consumers with `deny_unknown_fields` will reject any event whose adapter populated `dedup_id`. The shape changes based on *data*, not version.
**Fix:** Always emit the field with `null`. (Also verify the matching `Deserialize` impl tolerates absence.)

### M-9. `redex/replication_runtime.rs:1138-1141` — Budget refund only on send-call `Err`
`dispatcher.send_sync_response` returning `Ok` means "queued for send" on most transports, not delivered. Flaky links that silently drop post-queue still drain the budget.

### M-10. `redex/replication_runtime.rs:1194-1201` — Outstanding token leak on role-flip drop
SyncResponse arriving while the coordinator is briefly non-Replica returns before `outstanding.lock().take()`. Token stays in the set for the 30 s TTL; under role thrash, the SOFT_CAP GC starts dropping entries from other leaders.

### M-11. `redex/replication_runtime.rs:211-213` — `clear_leader` defined but never called
Documented to be called on believed-leader change so a re-elected peer doesn't inherit prior leader's tokens, but there is no call site. Re-election leaves prior leader's tokens in the set until TTL.

### M-12. `compute/mod.rs:141-161` — `RecoveryRegistry::try_run_all` holds `parking_lot::Mutex` across handler invocation
`parking_lot::Mutex` is non-reentrant and does not poison. A handler that re-enters the registry self-deadlocks; a panic mid-handler leaves the group in a torn state with the handler evicted (no future repair).
**Fix:** Snapshot the closures out and run them lock-free, or document the reentrancy ban.

---

## Low

- **`identity/token.rs:480`** — `signed_payload` made `pub`. Any caller can mint arbitrary signed transcripts under a held private key. Likely should be `pub(crate)`.
- **`meshos/event_loop.rs:495`** — random `runtime_epoch_id` fallback (on `getrandom` failure) is the exact pre-fix `(epoch ^ counter)` shape the change set was eliminating. Two processes booting same nanosecond under fallback still collide.
- **`meshos/migration_snapshot_source.rs:79`** — `buffered_events: 0` hardcoded. Consumers making admission decisions on this field are now blind.
- **`redex/replication_budget.rs:157-162`** — `BandwidthBudget::refund` accumulates `f64` rounding; `try_consume`'s `>=` can drift by ~1 byte over many non-power-of-two refunds.
- **`redex/replication_runtime.rs:230-242`** — `CatchupBackoff` HashMap entries never expire for demoted leaders; grows unbounded under churn.
- **`redex/manager.rs:746-756`** — `close_and_unlink_file` then `remove_dir_all` races with concurrent `open_file` for the same name; persistent dir is unlinked under a fresh heap entry.
- **`adapter/net/route.rs:50-66`** — High-nibble flags silently stripped; future peers using them go invisible. Worth a `warn!` so skew is observable.
- **`adapter/net/dataforts/blob/adapter.rs:166-180`** — `MAX_STREAM_BYTES = 16 GiB` exceeds `usize::MAX` on 32-bit targets; OOM panic precedes the typed error. Mirror the `mesh.rs:1188-1197` guard.
- **`adapter/redis.rs:189-200`** — Re-init race returns `Shutdown`; callers retrying on `Shutdown` give up. Use a transient classification.
- **`compute/migration_source.rs:246-249`** — `BufferFull { bytes }` reports pre-insert total; operator dashboards see `bytes < cap` for a `BufferFull` error.

---

## Nit

- **`redex/replication_heartbeat.rs:142-148`** — comment says "lex-smallest" but `NodeId` is `u64` numerical compare. Same wording elsewhere.
- **`adapter/net/channel/name.rs:104-119`** — uppercase rejected in a separate check; the subsequent `matches!` already excludes it. Fold together.
- **`bus.rs:1592-1612`** — small-N lossy shutdown can now report `was_lossy=true, events_dropped=0` due to the `saturating_sub` in `actual_drops`.
- **`meshos/maintenance.rs:131`** — discriminant-based "no-op replay" accepts inner-field changes as same-variant; confirm downstream fold doesn't depend on `is_valid_successor`.
- **`adapter/net/mesh.rs:6615-6623`** and **`dataforts/blob/blob_ref.rs`** — doc-comments still say "u32" / "32-bit hash" after the u64 widening.
- **`compute/fork_group.rs:153-160`** — removed dead `old_origin_hash` lookup but the prefix paragraph explaining it still remains.

---

## Verified clean (signal, not noise)

- Major peer-binding rebinds (membership Ack, PunchAck/Introduce, nRPC RESPONSE, migration dispatch arms) check `from_node` against a recorded principal under per-shard lock via `remove_if`; random `call_id` / nonces close the prediction surface.
- GC sweep TOCTOU `remove_if` predicate itself, manifest chunk-size validator, fs URI char-boundary sanitizer, mesh-blob `u64→usize` range guard, JetStream drain timeout, capability `is_direct` dedup axis, `apply_sync_response` fsync, `for_channel` TOCTOU close, `BecameHolderAndLeader` bundled event.
- ABI changes (`net_channel_hash` u32→u64, `TokenInfo.channel_hash`, Go header `uint64_t*`) consistent across Node/Python/Go.
- Tests not silently weakened: deleted coverage tracks deleted API; new regression tests added for peer-auth gates.

---

## Suggested order of remediation

1. **C-1, C-2** — silent state loss on legitimate operator actions.
2. **H-4** — duplicate-delivery infinite loop on format mismatch is observable in production.
3. **H-2** — Cutover-before-drain leaves migrations wedged with no retry path.
4. **H-6** — orphan-chunk leak compounds over time and is hard to detect.
5. **H-1, H-3, H-7, H-8** — distributed-consistency hazards; harder to repro but worse blast radius.
6. **H-5** — orchestrator TOFU; only exploitable by mesh-peer, but bypasses the gates the sweep just installed.
7. **Mediums and below** as bandwidth permits.
