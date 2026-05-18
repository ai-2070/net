# Bug Audit — 2026-05-18 — Carried-forward surfaces

**Crate:** `ai2070-net v0.18.0`
**Scope:** five modules the [`PHASE3_UMBRELLA.md`](./bug-audit-2026-05-18/PHASE3_UMBRELLA.md) explicitly carried forward as not-yet-reviewed:

- `src/adapter/net/dataforts/` (blob/, gravity/, greedy/) — ~19k LOC
- `src/adapter/net/compute/` (orchestrator, scheduler, fork/replica/standby groups, migration) — ~15k LOC
- `src/adapter/net/redex/replication_*` (coordinator, runtime, election, catchup, state, ...) — ~8.7k LOC
- `src/adapter/net/behavior/meshos/` (event_loop, ice, reconcile, executor, sdk, chains, snapshot) — ~19k LOC
- `src/adapter/net/netdb/` (db, error, mod) — ~400 LOC

**Method:** five parallel deep-read audits using the same hazard-class checklist the umbrella applied (concurrency, FFI, panic-across-await, overflow, auth, durability, lock ordering, resource bounds, distributed-systems split-brain / data loss / lost-update). New IDs use distinct per-module prefixes so they don't collide with the umbrella's A-/C-/H-/M-/L- families:

- `D-*` — dataforts
- `X-*` — compute
- `R-*` — replication (continues the existing `R-` series past R-19)
- `O-*` — meshos (O ≠ M-umbrella)
- `N-*` — netdb (none surfaced)

## Headline

The replication subprotocol is the highest-leverage surface in this batch — **2 Criticals + 4 Highs** that together amount to "any mesh peer can hijack channel state, and a healed partition leaves the cluster permanently bi-leader." `compute/standby_group.rs::promote` (X-1) is the same fencing gap from a different angle: leadership change with no epoch token. `dataforts/blob/mesh.rs` has a sweep TOCTOU (D-1) that silently loses freshly-referenced data, and a 32-bit truncation gap (D-2) the sibling FS path already guards. `meshos/` and `netdb/` are mostly correctness / observability gaps; `netdb/` is a null result.

| Severity | Count | Top items |
|---|---|---|
| Critical | 3 | R-20, R-21, X-1 |
| High | 8 | R-22, R-23, R-24, R-25, X-2, X-3, D-1, D-2 |
| Medium | 15 | (see body) |
| Low | 19 | (counts only at the end) |
| Null | 1 module | `netdb/` clean |

---

## Critical

### R-20 — No replication-peer authentication; any mesh peer can hijack channel state
- **File:** `src/adapter/net/redex/replication_runtime.rs:651-989` (`on_inbound`)
- **What:** All four inbound handlers (`Heartbeat`, `SyncRequest`, `SyncResponse`, `SyncNack`) validate `msg.channel_id` against `inputs.channel_id` but never check that `from` is in `inputs.replica_set`. `record_heartbeat` accepts any peer; if `role=Leader`, that peer becomes `believed_leader`. A `SyncResponse` from a non-leader peer is applied to disk after only a `coordinator.role() == Replica` check (line 791).
- **Attack/Impact:** Any node with `SUBPROTOCOL_REDEX` reachability can (a) become `believed_leader` for any replicated channel, suppressing real-leader election; (b) ship arbitrary `SyncResponse` chunks that `apply_sync_response` writes to the local log via `append_batch`; (c) inject `SyncNack{BadRange}` to make replicas `skip_to(since_seq+1)`, deleting local log segments. The earlier capability/auth fixes (A-1..A-3) landed on the publish path; the replication subprotocol was not in their scope.
- **Fix sketch:** Gate every `on_inbound` entry on `inputs.replica_set.contains(&from)`; for `SyncResponse`/`SyncNack` additionally require `from == tracker.believed_leader()`.

### R-21 — Permanent dual-leader: FSM has no `Leader → Replica` transition
- **File:** `src/adapter/net/redex/replication_state.rs:122-149`; `replication_election.rs:494-532`; `replication_runtime.rs:651-989`
- **What:** The FSM matrix has no `Leader → Replica` transition (only `Leader → Idle` via `GracefulRelinquish` / `ChannelClose`). `elect()` is documented as producing dual-self-winners on symmetric-RTT failover ("Convergence is broader-system's job") but the runtime never *implements* that convergence: a Leader does not check `is_leader_silent`, does not run elections, and ignores inbound heartbeats with `role=Leader` from other peers.
- **Impact:** A network-partition heal leaves both partitions with `role=Leader` permanently. Two divergent histories accrete; `apply_sync_response` will eventually reject one side's chunks as `GapBeforeChunk{divergence_suspected: true}`, but the warn log is the only consequence — data is silently overwritten via `skip_to`.
- **Fix sketch:** Add `Leader → Replica` to the FSM with a new `PeerLeaderObserved` signal (winner determined by tail_seq tiebreaker or lower NodeId concedes). On any inbound `Heartbeat{role=Leader, from=p}` while self is Leader, run the tiebreak and transition.

### X-1 — `StandbyGroup::promote` has no fencing; partition heal yields split-brain
- **File:** `src/adapter/net/compute/standby_group.rs:305-381` (`promote`), `:386-464` (`on_node_failure`); `compute/group_coord.rs:148-153`
- **What:** `promote` flips `active_index` locally and marks the old active "unhealthy" in the local `coord`. No epoch / lease / generation number, no fencing token plumbed through the daemon registry, no out-of-band signal to the OLD active telling it "you are no longer authoritative." Grep for `epoch|lease|fence|generation|term` in `standby_group.rs` / `replica_group.rs` returns zero matches.
- **Trigger:** A partition isolates the active member; a different node observes the active as unreachable and calls `on_node_failure(active_node) → promote`. Partition heals: the OLD active's node has been doing its job the entire time (local `DaemonRegistry` still routes events to it). The mesh now has two daemons with the same `origin_hash` accepting writes → diverging chain heads, conflicting outputs. `on_node_recovery` re-marks the demoted member as `Standby` but does nothing to stop the rogue active.
- **Fix sketch:** Add a `term: u64` (or `generation`) bumped on every `promote`; embed term in routed events; the daemon host rejects events at lower term; demote-to-standby on observing a higher term. Or plumb a "you have been demoted" control message through `MeshDaemon::on_control` and broadcast on promote.

---

## High

### R-22 — Replica acks tail_seq before fsync; crash loses claimed-applied data
- **File:** `src/adapter/net/redex/replication_runtime.rs:519,791-797`; `replication_catchup.rs:368-376`
- **What:** `apply_sync_response` calls `file.append_batch(&payloads)` then returns `file.next_seq()`. File fsync is policy-driven and async (file.rs Interval/EveryN background tasks). Next tick reads `tail_provider()` (line 519, `file.next_seq()`) and broadcasts that tail in `SyncHeartbeat` — i.e. "I have up to seq=N" advertised before N is durable. The leader treats the heartbeat as a durable ack and may relax retention past the replica's actual durable tail.
- **Impact:** Replica applies chunk in-memory → heartbeat broadcasts new tail → replica crashes pre-fsync → comes back with a lower tail. Leader's retention has already advanced; on rejoin the replica hits `GapBeforeChunk{divergence_suspected}` and `skip_to` silently drops the gap.
- **Fix sketch:** Either (a) `file.flush_sync()` before returning from `apply_sync_response` when the config requires a durable ack, or (b) split the heartbeat into `durable_seq` (post-fsync) and `applied_seq` (post-append) and have the leader's retention wait on the former.

### R-23 — Replica trusts NACK's `since_seq`; spoofed/stale NACK deletes data
- **File:** `src/adapter/net/redex/replication_runtime.rs:914-960`
- **What:** On `SyncNackError::BadRange`, the runtime unconditionally calls `inputs.file.skip_to(msg.since_seq.saturating_add(1))`. The NACK is not bound to any outstanding `SyncRequest` (no request-id correlation) and `from` is not verified against `believed_leader`. The replica also accepts `NotLeader` from any peer and clears its `believed_leader`, churning the election.
- **Impact:** A late-arriving stale NACK from a prior epoch (old leader timed out a request the replica already retried) makes the replica forget local data. Combined with R-20, any peer ships `SyncNack{BadRange, since_seq: <large>}` and the victim wipes local entries up to that seq.
- **Fix sketch:** Add a u64 request token to `SyncRequest`/`SyncResponse`/`SyncNack`; the replica drops NACKs whose token isn't in its in-flight set. Also require `from == believed_leader()`.

### R-24 — `apply_sync_response` advances tail past a partially-failed `append_batch`
- **File:** `src/adapter/net/redex/replication_catchup.rs:369-376`
- **What:** `append_batch(&payloads)` is called with the entire chunk's payloads. On partial failure (e.g., disk pressure between event 5 and 6 of a 10-event chunk) the function returns `ApplyError::AppendFailed`. The error handler routes to `handle_disk_pressure` which may `sweep_retention()` and continue OR `Withdraw` to Idle. No code reads back what was actually persisted — `file.next_seq()` could be at event-6's seq, but the caller doesn't see this; the next inbound chunk may re-supply event 6+ and produce `StaleChunk` or duplicate the first 5.
- **Impact:** Disk pressure during a multi-event chunk + `UnderCapacity::EvictOldest` policy produces lost-write or duplicate-apply depending on `append_batch`'s atomicity guarantees (undocumented).
- **Fix sketch:** Make `append_batch` atomic per chunk, or have `ApplyError::AppendFailed` carry the count actually persisted so the apply path can rebuild the next request from the correct seq.

### R-25 — Inbox saturation: heartbeat flood starves catchup (no priority lane)
- **File:** `src/adapter/net/redex/replication_runtime.rs:358,395,432-455`
- **What:** Single MPSC inbox of capacity 1024 multiplexes Heartbeat, SyncRequest, SyncResponse, SyncNack, Shutdown. A heartbeat flood from many peers fills the inbox so a leader's `SyncResponse` to the local replica is dropped at the router. No priority separation between control and data.
- **Trigger:** 50 peers heartbeating at 100 ms → 500 events/s; one slow `await` in `on_inbound` (e.g., dispatcher's `send_sync_response` blocks on a slow socket) wedges the loop ~2 s and overflows. Catchup permanently stalls; only heartbeats get through after the wedge clears.
- **Fix sketch:** Two inboxes — high-priority (Shutdown, SyncResponse, SyncNack) + low-priority (Heartbeat, SyncRequest) — selected via `tokio::select! { biased; ... }`. Or move outbound dispatch sends off the inbox-drain task via a separate spawn so `on_inbound` can't block.

### X-2 — `MigrationTargetHandler::replay_events` rewinds Cutover → Replay; enables double-delivery
- **File:** `src/adapter/net/compute/migration_target.rs:216-238`
- **What:** `replay_events` does `state.phase = MigrationPhase::Replay;` with no phase precondition. Compare `buffer_event` at `:271` which explicitly rejects post-Cutover events (regression test `buffer_event_rejects_post_cutover_events`). `replay_events` has no such guard.
- **Trigger:** Wire-level retry of `BufferedEvents` (source retransmits because the ack was dropped) arrives after `ActivateTarget`/`activate()` flipped phase to `Cutover` and the target is now serving live traffic. `replay_events` flips back to `Replay`. Duplicate events are filtered (`seq <= replayed_through`) but a subsequent `buffer_event` for a fresh event will pass its `phase != Cutover` guard and double-deliver alongside the normal path.
- **Fix sketch:** Mirror `buffer_event`'s guard: if `state.phase == MigrationPhase::Cutover`, return early with the recorded `replayed_through`.

### X-3 — `MigrationSourceHandler::cleanup` has no phase guard; pre-cutover call destroys live daemon
- **File:** `src/adapter/net/compute/migration_source.rs:294-302`
- **What:** `cleanup` unconditionally calls `daemon_registry.unregister(daemon_origin)` and removes the migration record. No check that `phase == Cutover` or `Complete`. The only in-tree caller is gated correctly, but `cleanup` is `pub` and exposed via the orchestrator's source-handler accessor (SDK/FFI consumers).
- **Trigger:** A retry path, malformed dispatcher, or future caller invokes `cleanup` during Snapshot/Transfer/Restore/Replay — source's live daemon is unregistered while the target is still restoring. Events arriving for that origin hit `DaemonNotFound`; buffered events in `SourceMigrationState` are lost; target eventually fails restore and aborts; source has nothing to roll back to.
- **Fix sketch:** Reject `cleanup` unless `phase == Cutover` (or `Complete`). Return `WrongPhase`. Mirrors the guard `take_buffered_events` got at `:265-274`.

### D-1 — Blob `sweep_gc` TOCTOU: concurrent `incr` lost; chunk + refcount silently dropped
- **File:** `src/adapter/net/dataforts/blob/mesh.rs:739-760` (`sweep_gc`) + `:777-784` (`delete_chunk`)
- **What:** `sweep_gc` snapshots `deletable_hashes()` then loops `delete_chunk(hash).await`. `delete_chunk` calls `redex.close_file(...)` then unconditionally `self.refcount.remove(hash)`. Between snapshot and per-hash delete, another caller can `refcount.incr(hash, ...)` (e.g. a freshly-folded chain event). The sweep deletes the chunk file AND removes the brand-new refcount entry — a subsequent `fetch` returns `NotFound`, and the refcount table no longer remembers the hash was referenced.
- **Impact:** Silent data loss for any blob that becomes newly referenced inside the sweep window.
- **Fix sketch:** In `delete_chunk` re-check the refcount entry's `should_sweep` predicate under the dashmap entry lock before `close_file`/`remove`. Use `inner.remove_if(hash, |_, e| should_sweep(e, now, floor, false))`. Failing entries skip and retry next sweep.

### D-2 — `MeshBlobAdapter::fetch_range` missing 32-bit `usize::MAX` guard
- **File:** `src/adapter/net/dataforts/blob/mesh.rs:1112-1170`
- **What:** `len = range.end - range.start` is `u64`; `Vec::with_capacity(len as usize)` (line 1145) and `bytes[range.start as usize..range.end as usize]` (line 1137) cast `u64 → usize` without the `len > usize::MAX as u64` guard that `FileSystemAdapter::fetch_range` has at `fs.rs:326`. `byte_range_to_chunks` only bounds against `total_size` ≤ 16 GiB; on a 32-bit target, 16 GiB > `usize::MAX` (4 GiB).
- **Impact:** 32-bit only. Peer-supplied `BlobRef::Small`/`Manifest` plus a wide caller-supplied range trips truncation: capacity is wrong, slice indices alias to a different offset (silent wrong-bytes for the Small path), or `Vec` extend later panics.
- **Fix sketch:** Mirror `fs.rs:326-331` — return `BlobError::Backend(...)` when `len > usize::MAX as u64`, likewise for `range.start`/`range.end` casts in the Small arm.

---

## Medium

### D-3 — Symlink-swap window between canonicalize and rename
- **File:** `src/adapter/net/dataforts/blob/fs.rs:172-282`
- **What:** `create_dir_all → canonicalize → starts_with(root) → write tmp → rename(tmp, path)`. The `starts_with` check happens at one instant; the subsequent `fs::rename(&tmp, &path)` resolves `path` again. An attacker with write access to `<root>/<shard>/` can swap the shard directory for a symlink elsewhere — the rename lands outside the root. The existing test covers the pre-existing-symlink case, not the post-canonicalize swap.
- **Impact:** Narrow — requires an attacker who can already write inside `<root>`. Plausible where ops co-locate the root with shared scratch.
- **Fix sketch:** Open the parent dir handle (`openat2 RESOLVE_BENEATH` / Linux; `FILE_FLAG_OPEN_REPARSE_POINT` on Windows) before canonicalize, then `renameat` against that fd.

### D-4 — `OverflowPushHandler` trusts sender-supplied `size_bytes`
- **File:** `src/adapter/net/dataforts/blob/overflow.rs:73-87,342-400`; admission at `admission.rs:246-275`
- **What:** `OverflowPush.size_bytes` is the only size signal the receive-side disk gate sees. After Admit, the handler builds `BlobRef::small(..., request.size_bytes)` and prefetches. A malicious sender stamps `size_bytes = 1` to pass the `InsufficientDisk` gate; the actual chunk arriving via the replication runtime is up to `BLOB_REF_MAX_SIZE` (16 GiB).
- **Impact:** Disk-budget evasion + per-peer prefetch amplification past budget. Bounded above by `BLOB_REF_MAX_SIZE` and per-chunk hash verify, so not OOB; sustained DoS against overflow-enabled hosts.
- **Fix sketch:** After `prefetch`, compare observed chunk len against `request.size_bytes`; on mismatch, close the chunk channel, bump `dataforts_blob_overflow_size_mismatch_total`, demote the sender's reputation. Optionally floor the disk-gate at `max(size_bytes, BLOB_CHUNK_SIZE_BYTES)`.

### D-5 — `MeshBlobAdapter::fetch` Manifest path can OOM
- **File:** `src/adapter/net/dataforts/blob/mesh.rs:1049-1095`
- **What:** No upfront alloc (deliberate, to avoid 16 GiB pre-alloc), but `out.extend_from_slice(&chunk_bytes)` grows `out` to the full Manifest size if a peer publishes a maximally-sized manifest and a local consumer calls `fetch`. No streaming, no per-fetch byte cap, no concurrent-fetch semaphore.
- **Impact:** A peer-controllable manifest pointing at locally-resident chunks lets a few concurrent `fetch` calls exhaust process memory.
- **Fix sketch:** (a) per-adapter `fetch` semaphore proportional to RAM, (b) route `BlobRef::Manifest` larger than threshold (e.g. 64 MiB) through `fetch_stream` only and have `fetch` return `BlobError::Backend("use fetch_stream for large manifests")`, or (c) bound `out` capacity at the operator-configured fetch budget.

### R-26 — `record_tail_seq` is dead code; tag advertisements ship `tip_seq=0`
- **File:** `src/adapter/net/redex/replication_coordinator.rs:289-306` + `replication_runtime.rs:229,519`
- **What:** Coordinator exposes `record_tail_seq` with CAS-monotonic guards; the runtime never calls it. Heartbeats read `tail_provider()` directly via `file.next_seq()`. The atomic `tail_seq` field is always 0; `announce_chain(origin, tip)` at `:417` advertises 0 instead of the real tail on every Leader/Replica entry.
- **Impact:** Capability-tag advertisements carry `tip_seq=0` regardless of actual log state. Holders looking up `find_chain_holders` with tip-seq ordering pick the wrong holder (lex-smallest, not freshest) → stale holders win selection during failover.
- **Fix sketch:** Either drop the dead atomic and have `transition_to` accept `tip_seq`, or wire the runtime/append path to call `coordinator.record_tail_seq(file.next_seq())` after every append/apply.

### R-27 — Budget not refunded on send failure → drift to permanent backpressure
- **File:** `src/adapter/net/redex/replication_runtime.rs:748-755`
- **What:** Bytes are deducted from `BandwidthBudget` BEFORE the wire send; the metric is bumped after the role re-check. On `dispatcher.send_sync_response` Err, the budget is not refunded. Over time the budget drifts low under flaky links.
- **Impact:** Slow socket → repeated send failures → budget consumed but no traffic shipped → leader nacks subsequent requests with Backpressure even on an idle wire.
- **Fix sketch:** Refund the budget on send failure, or move `try_consume` after a successful send. Same applies to the heartbeat path (which isn't budgeted at all — separate sub-finding).

### R-28 — Catchup busy-loops if leader heartbeats advertise tail past actual log
- **File:** `src/adapter/net/redex/replication_step.rs:227-251`
- **What:** Each tick emits one `SyncRequest` if `peer.tail_seq > local_tail`. A buggy/byzantine leader that emits ever-increasing `tail_seq` but ships `Response{events: []}` (empty because `since_seq >= local_next` on leader) makes the replica spam-request every tick forever. No backoff, no max-attempts.
- **Trigger:** Buggy leader reports `tail_seq=999_999` but file has no such entries. Replica busy-loops at heartbeat cadence (100 ms). Combined with R-25, saturates the leader's inbox.
- **Fix sketch:** Track per-leader "consecutive empty responses with advertised gap" counter; back off exponentially after 3 empty replies despite advertised lag.

### X-4 — Group `on_node_failure` skips `Unregistered` event on `registry.replace`
- **File:** `src/adapter/net/compute/standby_group.rs:454`; `replica_group.rs:284`; `fork_group.rs:349`; `registry.rs:143-157` (`replace`)
- **What:** `registry.replace(host)` does `daemons.insert` which silently overwrites the old slot AND only fires `Registered` (doc at `registry.rs:147-156` explicitly says "the caller is responsible for firing `Unregistered` first"). None of the three group `on_node_failure` paths fire it.
- **Impact:** Operator audit log, MeshOS dashboard, or any `DaemonLifecycleObserver` that pairs Registered/Unregistered to build a live-set leaks one entry per node-failure-recovery cycle.
- **Fix sketch:** Fire `Unregistered { id: old_origin_hash, ... }` before `registry.replace(host)` — or have `replace` itself read the prior entry's name and fire `Unregistered` then `Registered`.

### X-5 — `DaemonHost::from_fork` panics via `assert_eq!` on chain/keypair mismatch
- **File:** `src/adapter/net/compute/host.rs:85-91`
- **What:** `assert_eq!(chain.origin_hash(), keypair.origin_hash(), ...)`. The only in-tree caller derives chain from the keypair so the assert is unreachable in production today, but `DaemonHost::from_fork` is `pub` and SDK/FFI consumers may construct it with mismatched inputs.
- **Impact:** Panic across FFI is undefined behavior on Windows MSVC and aborts the host on Unix.
- **Fix sketch:** Convert to `Result<Self, DaemonError>` returning `DaemonError::RestoreFailed`. Same pattern `from_snapshot` already uses at `:119-125`.

### O-1 — `runtime_epoch_id` collides across same-nanosecond restarts; SDK dedup-reset defeated
- **File:** `src/adapter/net/behavior/meshos/event_loop.rs:482-487, 1078, 1112`
- **What:** `runtime_epoch_id` is built from `SystemTime::now().as_nanos() ^ static_counter.fetch_add(1)`. The static counter resets to 1 each process start; two processes booting in the same nanosecond (CI, VM resume) XOR identical `(epoch, counter)` and produce identical `runtime_epoch_id`. The SDK consumers' watermark-reset gate (snapshot's `runtime_epoch_id` vs last-seen) is then defeated: post-restart `admin_audit_seq` / `log_seq` / `failure_seq` start back at 1 and pass the consumer's dedup gate as "already seen," silently filtering valid post-restart audit records.
- **Fix sketch:** Use a UUID/random per-runtime stamp instead of `now ^ counter`.

### O-2 — `MeshOsDaemonSdk::runtime_this_node()` hardcoded to `0`
- **File:** `src/adapter/net/behavior/meshos/sdk.rs:738-747`
- **What:** Every `MeshOsDaemonHandle` built by the SDK has `MetadataView { node_id: 0, ... }` regardless of `MeshOsConfig::this_node`. Comment ("defer to a future slice") confirms it's unfinished. Daemons that read `handle.metadata().node_id` to identify themselves, route work, or stamp self-attributed messages all see `0`; two daemons on different nodes are indistinguishable.
- **Fix sketch:** Plumb `config.this_node` into `MeshOsDaemonSdk::start*` (store on the struct) or expose `MeshOsRuntime::this_node()` and read through it.

### O-3 — Loop's `tokio::select!` is not biased; ticks starve under sustained event load
- **File:** `src/adapter/net/behavior/meshos/event_loop.rs:709-741`
- **What:** Two-arm `tokio::select!` over `events_rx.recv()` and `tick.tick()` uses default (pseudo-random) arm selection. With `MissedTickBehavior::Delay` and a saturated source channel, the events arm wins repeatedly; reconcile passes are deferred until the channel drains. The `dropped_actions` counter only covers executor-side drops, not reconcile starvation. Manifests as stale `local_maintenance`, stuck `applied_backoffs`, and `freeze_until` never GC'd because `gc_freeze` only runs on Tick.
- **Fix sketch:** `tokio::select! { biased; _ = tick.tick() => ...; event = ... }` or force a reconcile every N events.

### O-4 — Chain record appended AFTER dispatch → audit gap on appender failure
- **File:** `src/adapter/net/behavior/meshos/executor.rs:473-477` (also `:497-498, :502-507, :518`)
- **What:** Executor calls `self.dispatcher.dispatch(...).await` first; on `Ok(())` then `append_dispatched(&self.chain_appender, &action)`. If the chain appender's write fails (disk full, RedEX hiccup), the action *was* executed but the chain has no record. The chain is documented as the "cluster-lifetime replay" of the action stream — a missed entry breaks replay correctness. Current code only logs via `let _ = append_dispatched(...)`.
- **Fix sketch:** Append the record with a `Pending` disposition before dispatch, then a follow-up `Outcome` record after — or accept the gap and document it loudly.

### O-5 — `record_admin_audit` chain append before ring push → ring/chain divergence
- **File:** `src/adapter/net/behavior/meshos/event_loop.rs:1086-1100` (also `record_log_line:1121-1135`)
- **What:** Loop bumps `admin_audit_seq`, appends to chain, then pushes to in-memory ring. If the chain append fails (e.g., RedEX appender returns Err), the warn log fires and we *still* push to the ring → chain says "seq N missing" but ring says "seq N present." If the chain append panics (OOM in the appender), `seq` has already been incremented and chain holds an entry the ring will never reflect.
- **Fix sketch:** Pick one source of truth or two-phase commit. Easiest: push to ring first, attempt chain append second; on chain failure, mark the ring entry with a "chain_pending" flag so consumers can distinguish.

### O-7 — `recent_emissions.push()` runs even when `try_send` fails → phantom snapshot entries
- **File:** `src/adapter/net/behavior/meshos/event_loop.rs:1332-1352`
- **What:** `self.recent_emissions.push(pending.clone())` happens unconditionally; only afterwards does `self.actions_tx.try_send(pending)` get checked. When the executor queue is full and `try_send` returns `Full`, the action is in `recent_emissions` (feeds snapshot `recently_emitted`) but the executor will never run it. Reconcile *also* counts the drop on `dropped_actions`, so the two metrics disagree.
- **Fix sketch:** Push to `recent_emissions` only on `try_send` success, or stamp a `dropped` flag on the snapshot variant.

### O-8 — `BecameHolder` + `LeaderChanged` not atomic under backpressure
- **File:** `src/adapter/net/behavior/meshos/sources.rs:144-188`
- **What:** A leader-promotion produces two separate `ReplicaTransitionEvent`s. Each is a separate `try_publish`; first may succeed and second drop on `QueueFull`, leaving the snapshot with a holder set but no leader (or vice versa). The atomic-pair handling for `LeaderLostAndIdled` already exists; the symmetric promotion case does not.
- **Impact:** Flapping leader under load → stuck reconcile decisions because the leader-gate (`replica_leader.get(chain) != Some(this_node)`) is wrong.

---

## Low

Counts and one-liners only; full text in the agent reports.

**Dataforts (D-6..D-10):**
- D-6 — `BlobAdapter::store_stream` default impl trusts `size_hint` only; concatenates unbounded stream into `buf` (`adapter.rs:155-170`)
- D-7 — `BlobMetrics::set_disk_capacity_bytes` writes `0` for NaN ratio silently (`metrics.rs:244`)
- D-8 — `parse_blob_heat_tag` admits mixed-case hex; canonicalization drift risk (`migration.rs:98-120`)
- D-9 — `chain_blob_refs` ↔ refcount lock ordering asymmetric between callers (`greedy/runtime.rs:419-437`)
- D-10 — `global_blob_adapter_registry` process-static `OnceLock`; no clear path (`registry.rs:112-117`)

**Compute (X-6..X-8):**
- X-6 — `Scheduler::find_migration_targets` allocates const tag string per call (`scheduler.rs:158-173`)
- X-7 — `fork_group.rs:292-298` dead `.unwrap()` lookup discarded via `let _ = ...`
- X-8 — `SnapshotReassembler::feed` sweeps before validating malformed chunks → DoS amplifier (`orchestrator.rs:843-857`)

**Replication (R-29, R-30):**
- R-29 — `replica_set` admits duplicates → double heartbeats; should be `BTreeSet` (`replication_step.rs:186-189`)
- R-30 — `wall_clock_ms` collected from heartbeats but never used; either implement the skew gauge or drop the field (`replication.rs:237-244`)

**MeshOS (O-6, O-9..O-16):**
- O-6 — `Defer` re-queue never emits a chain record; long-deferred history invisible (`executor.rs:409-427`)
- O-9 — `gc_drain_window` 1-second hardcode disregards `BackpressureConfig` semantics (`backpressure.rs:273-276`)
- O-10 — `BackoffTracker::observe_crash` window-slide non-monotonic on out-of-order crashes (`supervision.rs:186-229`)
- O-11 — `MaintenanceState::EnteringMaintenance.deadline` falls back to wall-clock pre-first-tick (`state.rs:364`)
- O-12 — `emit_maintenance_transitions` skips transitions when `control_sink` is wired late (`event_loop.rs:1235-1244`)
- O-13 — `release_failed_admit` for `PullReplica` clears chain stabilization even if sibling holds it (`backpressure.rs:210-218`)
- O-14 — `MigrationSnapshotSource::list()` called inside loop hot path; slow source stalls reconcile (`event_loop.rs:1383`)
- O-15 — `last_pull_admitted_by` rollback only clears most-recent slot (`backpressure.rs:212-215`)
- O-16 — `WIRE_FORMAT_VERSION = 1` with no migration path documented (`chain.rs:51-79`)

---

## Null results

### `netdb/` — clean

`src/adapter/net/netdb/{db.rs, error.rs, mod.rs}` (399 LOC) is a thin builder + façade. All query/predicate/filter logic lives in `cortex::tasks` / `cortex::memories` (covered in `PHASE3_CORTEX_RPC_DROP.md`). No query expression is parsed, evaluated, or locked inside `netdb/`.

Categories explicitly checked and clean: query injection (no string DSL parsed here), authorization (delegated to the channel layer per the documented model), TOCTOU (no plan/execute split), float/integer/locale predicate hazards (no arithmetic in this module), result-set resource bounds (per-model snapshot delegates downward), lock ordering (no locks held), read-your-writes (no caching layer), Debug/Display panic or unbounded alloc.

The four `.expect()`/`unwrap_or` sites in `db.rs` are documented or trivially safe; `NetDbBuilder::build()` refuses a zero-model build, so the only way to trip the panic is to call `tasks()` on a NetDb built with `with_memories()` only — the expect messages name the fix.

### Categories checked across the other four modules and clean

- `tokio::spawn` join-handle discipline (all spawns tracked or aborted on Drop).
- No locks held across `.await` in any of the four modules.
- No `block_on` in production async paths.
- No `unsafe`, `from_raw_parts`, or `mem::transmute` in dataforts/, compute/, replication, or meshos/.
- BlobRef wire decode (magic + version + body bounds + postcard `MAX_MANIFEST_WIRE_BYTES` cap + per-chunk BLAKE3 verify) cannot be escaped by a hostile peer.
- `byte_range_to_chunks` arithmetic on `u64`; `start > end` and `end > total_size` both rejected.
- Per-hash advisory lock in `store_chunk` correctly serializes concurrent stores; idempotent-skip verifies existing bytes against hash.
- Path traversal in `fs.rs` — content-addressed paths derived solely from hash hex; URI never reaches the filesystem path.
- `fsync`/`sync_all` on temp file + parent dir best-effort; `sync_blob` exists for the durability tier.
- Atomic rename present (`fs.rs:240-266`); fallback verifies existing-content hash on rename failure.
- Migration handshake (compute) — `local_source_migration_registers_in_source_handler`, `local_source_cutover_drains_buffered_events_through_source_handler`, `abort_migration_propagates_to_source_handler`, `buffer_event_distinguishes_post_cutover_from_no_migration` all pin the correct sequencing.
- Snapshot reassembler (compute) — byte cap, total-chunks cap, age sweep, stale-seq rejection, zero-byte chunk rejection all pinned.
- StandbyGroup promote half-mutation safety pinned by `promote_does_not_half_mutate_on_no_healthy_member`.
- Replication monotonic `prev + 1` check uses `checked_add` (R-18 carried over).
- Replication state-machine matrix exhaustively pinned in `replication_state.rs:385-441`; no invalid pair accepted (R-21 is a *missing* edge).
- Replication `transition_lock` correctly serializes tag side-effects.
- MeshOS `ActionExecutor::run` `catch_unwind`s the dispatcher future; `poll_probes` `catch_unwind`s every probe.
- MeshOS ed25519 signature verify in `OperatorRegistry::verify_bundle` correctly counts *distinct* operators.
- MeshOS ICE freshness `check_freshness` arithmetic u64-safe.

---

## Suggested action order

1. **R-20** (peer auth) + **R-21** (dual-leader FSM) — the replication subprotocol can be hijacked or wedged by any mesh peer. Wire-protocol change; do them together with a single coordinated rollout.
2. **X-1** (StandbyGroup fencing) — same class of bug as R-21 in a different layer. The fix (epoch/generation token) is a primitive both can share.
3. **R-22, R-23, R-24** (replication durability + NACK trust + partial-append accounting) — bundle with the R-20/R-21 wire change.
4. **D-1** (sweep TOCTOU) — quiet data-loss bug; trivial fix via `remove_if`.
5. **D-2** (32-bit `usize::MAX` guard on `MeshBlobAdapter::fetch_range`) — mechanical, mirrors existing `fs.rs` pattern.
6. **R-25** (priority lane) and **R-28** (catchup backoff) — replication availability hardening.
7. **X-2, X-3** (migration phase guards) — close `pub` API misuse paths.
8. **O-1, O-2** (epoch_id collision + node_id hardcode) — SDK correctness; small but real consumer-facing bugs.
9. **O-3, O-7, O-8** (tick starvation, phantom emissions, atomic-pair publishing) — meshos observability + reconcile correctness.
10. **D-3, D-4, D-5** (blob hardening) and **X-4, X-5** (group lifecycle, panic-across-FFI).
11. **O-4, O-5** (audit-chain durability ordering) — pick one source of truth; document loudly.
12. **R-26, R-27** (dead tip_seq, budget refund) and the remaining lows can batch into a single cleanup commit.

## Coverage gaps still carried forward

- **Phase 2** (Miri / ASan / TSan / fuzz) — still skipped; existing `fuzz/fuzz_targets/` is wired.
- **Cross-language conformance (Phase 4)** — Rust/TS/Py/Go SDK round-trip property tests not started.
- **Dep audit** — `cargo-audit` / `cargo-machete` / `cargo-deny` / `cargo-udeps` not installed.
- **Adjacent surfaces not reviewed this round:** `src/adapter/net/contested/`, `src/adapter/net/continuity/`, `src/adapter/net/cortex/` (re-review post-fixes), `src/adapter/net/identity/`, `src/adapter/net/subnet/`, `src/adapter/net/subprotocol/`, `src/adapter/net/state/`, `src/adapter/net/traversal/`. Each is a candidate for a follow-up.
