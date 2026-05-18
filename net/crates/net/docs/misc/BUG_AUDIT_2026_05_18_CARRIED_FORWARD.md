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
| High | 19 | A-5, R-22, R-23, R-24, R-25, X-2, X-3, X-9, X-18, X-19, D-1, D-2, D-11, D-14, D-17, O-21, S-1, S-2, S-4 |
| Medium | 31 | (see body) |
| Low | 39 | (counts only at the end) |
| Latent | 1 | MD-3 |
| Null | 1 module | `netdb/` clean |

Third pass added 1 H (D-14) + 4 M (D-15, D-16, R-35, X-13) + 8 L
(R-36..R-39, X-14..X-17) — see "Third-pass additions" section.

Fourth pass added 1 H (X-18) + 2 M (O-20, MD-1) + 1 L (MD-2) + 1 latent
(MD-3) — see "Fourth-pass additions" section. `behavior/meshdb/` was
newly in-scope for the fourth pass; the MD-* prefix names that module.

Fifth pass added 3 H (D-17, X-19, O-21) + 5 M (D-18, O-22, O-23, O-24,
X-20) + 4 L (R-40, O-25, X-21, X-22) — see "Fifth-pass additions"
section. The pass independently re-derived R-26, D-11, and O-19 from
the same source lines (confirmation signals, not duplicates).

Sixth pass — targeted subprotocol `from_node`-binding sweep — added
2 H (S-1, S-2) + 1 M (S-3). Cleared 7 subprotocols outright; flagged 4
queue-to-application subprotocols and 5 reserved-unwired IDs as
follow-up audit candidates. See "Sixth pass — targeted subprotocol
sweep" section.

Seventh pass — closes the two sixth-pass follow-ups — added 1 H (S-4
nRPC response spoofing) and 1 architectural carry-forward (A-CON-1
queue-boundary identity loss). Verified clean: MeshDB
`FEDERATED_CALL_ID_COUNTER` (transport binds peer), `next_waiter_gen`
(local-only), `LocalMeshQueryExecutor::next_id` (local-only),
`rpc_round_robin_cursor` (local-only). See "Seventh pass" section.

## Second-pass note (2026-05-18, later same day)

A second parallel-agent pass added the eight new findings prefixed below
(A-5, X-9, X-10, X-11, X-12, D-11, D-12, D-13, R-31, R-32, R-33, R-34,
O-17, O-18, O-19). The same pass independently re-derived R-20
(no replica-set membership check), R-23 (NACK `since_seq` trust),
and D-5 (Manifest fetch OOM) from the same source lines — those three
are not duplicated below; their independent re-discovery is a
confirmation signal rather than a new finding.

## Third-pass note (2026-05-18 evening)

A third parallel-agent pass over the same four modules (skipping the
already-clean `netdb/`) was scoped narrower — "name bug classes the
prior passes did not enumerate" — and surfaced 13 new findings:
**D-14, D-15, D-16, R-35, R-36, R-37, R-38, R-39, X-13, X-14, X-15,
X-16, X-17.** Severity 1 H + 4 M + 8 L. The pass independently
re-derived **R-21** (no `Leader → Replica` transition; "split-brain
after partition heal") and **R-31** (state advances before async
sink completes; "transition_to side-effect failure is unretryable")
from the same source lines — confirmation signals, not duplicates.
The new findings are listed in the "Third-pass additions" section
below rather than threaded through the existing severity buckets
so the prior numbering stays stable.

## Fourth-pass note (2026-05-18 late evening)

A fourth parallel-agent pass expanded scope to include
`behavior/meshdb/` (newly in-scope as part of the carried-forward
batch) alongside the original four modules, and surfaced 4 new
findings plus 1 latent design issue: **X-18, O-20, MD-1, MD-2, MD-3**.
The pass independently re-derived **D-2** (32-bit `usize` truncation
in `MeshBlobAdapter::fetch_range`) from the same source lines —
confirmation signal, not a new finding.

Three agent-rated "highs" the pass produced were verified false
positives against the actual code and not added:

- "Replication leader concession race" at `replication_runtime.rs:730-753`
  — the role re-check at `:736` already exists exactly as the agent
  recommended, with no `.await` between the check and `send_sync_response`.
- "Deferred-heap `pop().expect()` panic" at `meshos/executor.rs:366`
  — `tokio::select!` is single-task and `self.deferred` is `&mut self`-
  owned, so the heap cannot mutate between `peek` (line 358) and `pop`.
- "Snapshot publish torn read of executor failure ring" at
  `meshos/event_loop.rs:1379` — the read goes through a `RwLock`,
  which prevents torn reads; the worst-case is a fresh failure landing
  after the snapshot's clone and surfacing in the next snapshot.

The fourth pass also surfaced four low-severity migration edges
(`replayed_through` saturating-deadlock at `migration_target.rs:452`;
`start_snapshot` single-flight gap between `contains_key` and the
`entry()` insert; orphan `orchestrator_node` ref after failed pre-
complete; `completed`-record TTL gap after failed source cleanup).
Each requires exceptional preconditions (u64::MAX-events, very narrow
race windows, or compound failures); they're listed under "Fourth-pass
edges (not promoted)" rather than threaded into the main severity
buckets.

## Fifth-pass note (2026-05-18 night)

A fifth parallel-agent pass over the same five modules surfaced 12 new
findings: **D-17, D-18, X-19, X-20, X-21, X-22, O-21, O-22, O-23, O-24,
O-25, R-40.** Severity 3 H + 5 M + 4 L. The pass independently
re-derived **R-26** (`record_tail_seq` dead; tag advertisements ship
`tip_seq=0` — verified: only call sites are inside `#[cfg(test)]`
blocks at `manager.rs:1217`, `replication_coordinator.rs:570,594,733,
735,737`), **D-11** (`BlobRef::Manifest` decoder admits arbitrary
per-chunk sizes; `byte_range_to_chunks` returns wrong-offset bytes for
non-4 MiB strides), and **O-19** (`BufferingActionChainAppender::with_capacity(0)`
silently increments `dropped_count` on every append because the
`max(1)` clamp the three sibling appenders apply was missed here) from
the same source lines — confirmation signals, not duplicates. The
fifth-pass scope notably caught O-21 (cluster-backpressure stays
asserted indefinitely in a quiet cluster), which the prior four passes
missed despite covering `meshos/executor.rs` directly.

---

## Critical

### R-20 — No replication-peer authentication; any mesh peer can hijack channel state — **Landed**
- **File:** `src/adapter/net/redex/replication_runtime.rs:651-989` (`on_inbound`)
- **What:** All four inbound handlers (`Heartbeat`, `SyncRequest`, `SyncResponse`, `SyncNack`) validate `msg.channel_id` against `inputs.channel_id` but never check that `from` is in `inputs.replica_set`. `record_heartbeat` accepts any peer; if `role=Leader`, that peer becomes `believed_leader`. A `SyncResponse` from a non-leader peer is applied to disk after only a `coordinator.role() == Replica` check (line 791).
- **Attack/Impact:** Any node with `SUBPROTOCOL_REDEX` reachability can (a) become `believed_leader` for any replicated channel, suppressing real-leader election; (b) ship arbitrary `SyncResponse` chunks that `apply_sync_response` writes to the local log via `append_batch`; (c) inject `SyncNack{BadRange}` to make replicas `skip_to(since_seq+1)`, deleting local log segments. The earlier capability/auth fixes (A-1..A-3) landed on the publish path; the replication subprotocol was not in their scope.
- **Fix:** `on_inbound` builds a `from_node: Option<NodeId>` per Inbound variant (Shutdown is local-only and skipped); membership-checks against `inputs.replica_set` at function entry. SyncResponse and SyncNack additionally require `from == tracker.believed_leader()`. Regression tests: `inbound_from_non_replica_set_peer_is_dropped` (out-of-set heartbeat must not seed believed_leader; out-of-set SyncResponse must not advance local tail); `sync_response_from_non_leader_replica_peer_is_dropped` (in-set non-leader peer cannot ship state-mutating chunks). Two-layer gate keeps the surface narrow even if replica_set membership itself is later compromised. Commit `c62998e3`.

### R-21 — Permanent dual-leader: FSM has no `Leader → Replica` transition — **Landed**
- **File:** `src/adapter/net/redex/replication_state.rs:122-149`; `replication_election.rs:494-532`; `replication_runtime.rs:651-989`
- **What:** The FSM matrix has no `Leader → Replica` transition (only `Leader → Idle` via `GracefulRelinquish` / `ChannelClose`). `elect()` is documented as producing dual-self-winners on symmetric-RTT failover ("Convergence is broader-system's job") but the runtime never *implements* that convergence: a Leader does not check `is_leader_silent`, does not run elections, and ignores inbound heartbeats with `role=Leader` from other peers.
- **Impact:** A network-partition heal leaves both partitions with `role=Leader` permanently. Two divergent histories accrete; `apply_sync_response` will eventually reject one side's chunks as `GapBeforeChunk{divergence_suspected: true}`, but the warn log is the only consequence — data is silently overwritten via `skip_to`.
- **Fix:** Added `TransitionSignal::PeerLeaderObserved` and the `Leader → Replica` cell in `replication_state.rs`'s matrix + `pair_is_valid_for_some_signal`. The Heartbeat arm of `on_inbound` now runs the deterministic tiebreak whenever a Leader receives a Heartbeat with `role=Leader` from another peer: the side with the larger `tail_seq` keeps Leader; on a tie, the numerically smaller `node_id` keeps Leader. The loser concedes via `Leader → Replica` and a warn-level log is emitted so partition-heal convergence is observable. Regression tests in `replication_runtime.rs`: `peer_leader_observation_demotes_loser_to_replica` (tail-loss demotes) and `peer_leader_tail_tie_lower_node_id_wins` (tail-tie symmetric tiebreak).

### X-1 — `StandbyGroup::promote` has no fencing; partition heal yields split-brain — **Deferred (architectural)**
- **File:** `src/adapter/net/compute/standby_group.rs:305-381` (`promote`), `:386-464` (`on_node_failure`); `compute/group_coord.rs:148-153`
- **What:** `promote` flips `active_index` locally and marks the old active "unhealthy" in the local `coord`. No epoch / lease / generation number, no fencing token plumbed through the daemon registry, no out-of-band signal to the OLD active telling it "you are no longer authoritative." Grep for `epoch|lease|fence|generation|term` in `standby_group.rs` / `replica_group.rs` returns zero matches.
- **Trigger:** A partition isolates the active member; a different node observes the active as unreachable and calls `on_node_failure(active_node) → promote`. Partition heals: the OLD active's node has been doing its job the entire time (local `DaemonRegistry` still routes events to it). The mesh now has two daemons with the same `origin_hash` accepting writes → diverging chain heads, conflicting outputs. `on_node_recovery` re-marks the demoted member as `Standby` but does nothing to stop the rogue active.
- **Why deferred:** The fix sketch ("term/generation embedded in routed events, daemon host rejects at lower term, demote-to-standby on observing a higher term") requires wire-level event-envelope changes — events must carry an issuer term, every receiver must track a current_term per origin, and the membership protocol must re-sync terms on reconnect. The alternative ("you have been demoted" control message through `MeshDaemon::on_control`) requires the cross-node mesh path to be reachable; during the partition that's exactly what's broken, so the message would also be unable to fence the rogue active until reconnect — at which point an event-carried term would already have done the job. Neither path is a local-file change. Tracked for the next architectural pass; until then operators can mitigate by requiring `leader_pinned` placement (which avoids auto-promote entirely) or by deploying a fencing token issuer at the daemon host layer.

---

## High

### A-5 — In-progress L-13 fix strips reserved metadata before re-forwarding, breaking multi-hop signed propagation — **Landed**
- **File:** `src/adapter/net/mesh.rs:5239` (uncommitted on `bugfixes-15`); `src/adapter/net/behavior/capability.rs:2145-2156` (new `strip_reserved_metadata`); `src/adapter/net/behavior/capability.rs:2084-2093` (`signed_payload`).
- **What:** The uncommitted L-13 fix calls `ann.strip_reserved_metadata()` immediately after the signature verify and TOFU pin, then *later* clones `ann`, bumps `hop_count`, and reserializes via `to_bytes()` to forward to other peers (`mesh.rs:5343-5358`). `signed_payload()` covers the `metadata` field — so the forwarded wire bytes no longer match the signature transcript. Any peer two-plus hops downstream with `require_signed_capabilities = true` rejects the forwarded announcement at the verify step (`mesh.rs:5200`).
- **Impact:** Functional regression in the new fix. Multi-hop signed capability discovery breaks for any receiver that requires signed caps. Fails *closed* (announcement is dropped, not accepted) so it's not an auth bypass — but the feature stops working. The existing strip test (`capability.rs:3712`) only exercises strip in isolation; no multi-hop round-trip test catches it.
- **Fix:** `ann.strip_reserved_metadata()` now runs at `mesh.rs:5435` — between the forward block (which ships the signature-covered bytes verbatim to downstream peers) and `capability_index.index(ann)` at `:5449`. The local copy is sanitized post-forward so attacker metadata can't steer local placement/admission, while the signature transcript on the wire remains intact for downstream re-verification. The only consumer in the gap is `policy.assign(&ann.capabilities)`, which reads `caps.tags` only — unaffected by the move.

### R-22 — Replica acks tail_seq before fsync; crash loses claimed-applied data — **Landed**
- **File:** `src/adapter/net/redex/replication_runtime.rs:519,791-797`; `replication_catchup.rs:368-376`
- **What:** `apply_sync_response` calls `file.append_batch(&payloads)` then returns `file.next_seq()`. File fsync is policy-driven and async (file.rs Interval/EveryN background tasks). Next tick reads `tail_provider()` (line 519, `file.next_seq()`) and broadcasts that tail in `SyncHeartbeat` — i.e. "I have up to seq=N" advertised before N is durable. The leader treats the heartbeat as a durable ack and may relax retention past the replica's actual durable tail.
- **Impact:** Replica applies chunk in-memory → heartbeat broadcasts new tail → replica crashes pre-fsync → comes back with a lower tail. Leader's retention has already advanced; on rejoin the replica hits `GapBeforeChunk{divergence_suspected}` and `skip_to` silently drops the gap.
- **Fix:** `apply_sync_response` now calls `file.sync()` after `append_batch` and before returning the new tail (under `#[cfg(feature = "redex-disk")]`). `RedexFile::sync()` is a no-op for heap-only files and for `FsyncPolicy::Never`, so the per-chunk fsync cost is paid only on disk-backed channels that already opted into durability. Sync failure surfaces as `ApplyError::AppendFailed("durable-sync: ...")` and routes through the existing disk-pressure handling — the runtime never advertises a tail past durable.

### R-23 — Replica trusts NACK's `since_seq`; spoofed/stale NACK deletes data — **Landed**
- **File:** `src/adapter/net/redex/replication_runtime.rs:914-960`
- **What:** On `SyncNackError::BadRange`, the runtime unconditionally calls `inputs.file.skip_to(msg.since_seq.saturating_add(1))`. The NACK is not bound to any outstanding `SyncRequest` (no request-id correlation) and `from` is not verified against `believed_leader`. The replica also accepts `NotLeader` from any peer and clears its `believed_leader`, churning the election.
- **Impact:** A late-arriving stale NACK from a prior epoch (old leader timed out a request the replica already retried) makes the replica forget local data. Combined with R-20, any peer ships `SyncNack{BadRange, since_seq: <large>}` and the victim wipes local entries up to that seq.
- **Fix:** Two-part. (a) The `from == believed_leader()` gate already landed under R-20 — both `SyncResponse` and `SyncNack` are dropped at the dispatch boundary unless the session peer matches the recorded leader. (b) Added `request_id: u64` to `SyncRequest` / `SyncResponse` / `SyncNack` wire structs. The replica's `tick()` emits SyncRequest with `request_id = 0` placeholder; the runtime mints a random 64-bit token via `getrandom::fill` at outbound dispatch, records `(leader, token)` in a new `OutstandingRequests` registry (30s TTL, 256-entry soft cap with on-insert GC), and stamps the wire frame. The leader echoes verbatim on the matching SyncResponse / SyncNack. Inbound SyncResponse / SyncNack handlers call `outstanding.lock().take(from, request_id, now)`; a `false` return drops the frame. Wire layout grew by 8 bytes per request and per response/nack; encoder/decoder updated symmetrically. Tests cover the negative path implicitly via the harness which now exercises the request-id round trip.

### R-24 — `apply_sync_response` advances tail past a partially-failed `append_batch` — **Verified clean**
- **File:** `src/adapter/net/redex/replication_catchup.rs:369-376`
- **What:** `append_batch(&payloads)` is called with the entire chunk's payloads. On partial failure (e.g., disk pressure between event 5 and 6 of a 10-event chunk) the function returns `ApplyError::AppendFailed`. The error handler routes to `handle_disk_pressure` which may `sweep_retention()` and continue OR `Withdraw` to Idle. No code reads back what was actually persisted — `file.next_seq()` could be at event-6's seq, but the caller doesn't see this; the next inbound chunk may re-supply event 6+ and produce `StaleChunk` or duplicate the first 5.
- **Audit re-examination:** `RedexFile::append_batch` (`file.rs:602`) is documented as failure-atomic and the implementation matches: capacity is pre-validated under the state lock; seq allocation is post-validation; for persistent files the disk write runs **before** any in-memory commit, and the disk-level helper `DiskSegment::append_entries_inner` (`disk.rs:1001`) writes dat → idx → ts with cascading rollback on every failure path (`rollback_truncate` per file at `:1077-1088`, `:1110-1117`, `:1119-1128`) and a `poisoned` segment flag if rollback itself fails. The seq-allocation `fetch_sub` rollback on disk failure (`file.rs:669-671`) ensures `next_seq` is restored before the error returns. The in-memory `segment.append_many` after the successful disk write is `.expect("pre-validated capacity")` — infallible. The pre-condition for R-24's failure mode (partial batch persisted, seq advanced) does not occur.
- **No code change.** Atomicity invariant is now documented in the file's source comment; no separate landing commit.

### R-25 — Inbox saturation: heartbeat flood starves catchup (no priority lane) — **Landed**
- **File:** `src/adapter/net/redex/replication_runtime.rs:358,395,432-455`
- **What:** Single MPSC inbox of capacity 1024 multiplexes Heartbeat, SyncRequest, SyncResponse, SyncNack, Shutdown. A heartbeat flood from many peers fills the inbox so a leader's `SyncResponse` to the local replica is dropped at the router. No priority separation between control and data.
- **Trigger:** 50 peers heartbeating at 100 ms → 500 events/s; one slow `await` in `on_inbound` (e.g., dispatcher's `send_sync_response` blocks on a slow socket) wedges the loop ~2 s and overflows. Catchup permanently stalls; only heartbeats get through after the wedge clears.
- **Fix:** Two MPSC inboxes — low-priority (Heartbeat, SyncRequest, capacity 1024) and high-priority (Shutdown, SyncResponse, SyncNack, capacity 128). `is_priority_event` classifies inbound at the handle's `dispatch` / `try_dispatch` entry points. The `run` loop uses `tokio::select! { biased; ... }` polling priority lane first, then the heartbeat tick, then the low-priority lane. `cancel()` ships Shutdown on the priority lane so a saturated low-priority lane can't block graceful exit. Regression test `priority_lane_drains_under_low_priority_saturation` floods the low-priority lane past capacity, then verifies `cancel().await` completes within 2 s and the coordinator reached Idle via the graceful path (not the abort fallback).

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

### X-9 — `MigrationTargetHandler::pending_events` unbounded; wire-reachable OOM
- **File:** `src/adapter/net/compute/migration_target.rs:274` (`buffer_event`) and `:231` (`replay_events`).
- **What:** `pending_events: BTreeMap<u64, CausalEvent>` is inserted into on every call with no length or byte cap. `drain_pending` only evicts events that form a contiguous run starting at `replayed_through + 1`; out-of-order seq numbers stay parked indefinitely. The migration subprotocol is wire-driven — any peer that can address migration traffic to this node can ship monotonically increasing-but-non-contiguous `CausalEvent`s (skip `replayed_through + 1` forever). Per-event payload is up to `MAX_SNAPSHOT_CHUNK_SIZE = 7000` bytes.
- **Impact:** Targeted resource-exhaustion DoS on any node accepting migration traffic. Grows RSS without bound until OOM.
- **Fix sketch:** Maintain `pending_bytes: usize` alongside the map; add `MAX_PENDING_BUFFER_BYTES` (e.g. 64 MiB, mirroring `MAX_PENDING_REASSEMBLY_BYTES`) and `MAX_PENDING_EVENTS` (e.g. 1_000_000, mirroring `MAX_BUFFERED_EVENTS`). Refuse insertions that would exceed either with a typed `BufferFull` error so the source can back off.

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

### D-11 — `BlobRef::Manifest` decoder accepts arbitrary per-chunk sizes; slice panic in `fetch_range` on untrusted input
- **File:** `src/adapter/net/dataforts/blob/blob_ref.rs:485-553` (`decode_manifest` / `manifest()` constructor); `src/adapter/net/dataforts/blob/blob_ref.rs:746-798` (`byte_range_to_chunks`); `src/adapter/net/dataforts/blob/mesh.rs:1143-1170` (`fetch_range` Manifest arm).
- **What:** The decoder validates `iterated_sum == total_size`, chunk count ≤ `BLOB_MANIFEST_MAX_CHUNKS`, and `total_size ≤ BLOB_REF_MAX_SIZE` (16 GiB) — but never that non-last chunks have `size == BLOB_CHUNK_SIZE_BYTES` (4 MiB). `byte_range_to_chunks` computes chunk positions from the fixed 4 MiB stride at `:776,786` while clamping `local_end` against the attacker-stamped `chunk.size as u64` at `:790`. Then `mesh.rs:1155` slices `chunk_bytes[start_in_chunk..end_in_chunk]` against the actually-fetched bytes (verified only by hash to *some* length).
- **Trigger:** Peer publishes a `BlobRef::Manifest` with one chunk `{hash: H, size: u32::MAX}` and `total_size = u32::MAX`. The decoder accepts it. A consumer calling `fetch_range(blob, 0..total_size)` produces a request with `end_in_chunk = u32::MAX`. The actually-fetched bytes under hash `H` can be any length (say 100 bytes — hash matches what the peer stored); slicing `chunk_bytes[0..u32::MAX as usize]` panics across the `await`. A subtler variant — chunks `[{size: 1}, {size: 1}]` — silently returns wrong-window bytes because the position math still assumes 4 MiB stride.
- **Fix sketch:** In `decode_manifest()` and `manifest()`, reject manifests where any non-last chunk has `size != BLOB_CHUNK_SIZE_BYTES`, or the last chunk has `size > BLOB_CHUNK_SIZE_BYTES`. Independently, replace the panicking slice at `mesh.rs:1155` with `chunk_bytes.get(start..end).ok_or(BlobError::HashMismatch)?`.

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

### R-28 — Catchup busy-loops if leader heartbeats advertise tail past actual log — **Landed**
- **File:** `src/adapter/net/redex/replication_step.rs:227-251`
- **What:** Each tick emits one `SyncRequest` if `peer.tail_seq > local_tail`. A buggy/byzantine leader that emits ever-increasing `tail_seq` but ships `Response{events: []}` (empty because `since_seq >= local_next` on leader) makes the replica spam-request every tick forever. No backoff, no max-attempts.
- **Trigger:** Buggy leader reports `tail_seq=999_999` but file has no such entries. Replica busy-loops at heartbeat cadence (100 ms). Combined with R-25, saturates the leader's inbox.
- **Fix:** Added `CatchupBackoff` struct (per-leader `consecutive_empty` counter + `backoff_until: Option<Instant>`) and threaded `Arc<Mutex<CatchupBackoff>>` through `spawn_replication_runtime` → `run` → `on_tick` / `on_inbound`. The `SyncResponse` apply arm snapshots the pre-apply tail; if the apply advanced the tail, `record_progress(from)` clears any backoff entry. If the apply did not advance the tail while the believed leader's `tail_seq > new_tail`, `record_empty(from, now)` increments the counter. Once `CATCHUP_BACKOFF_THRESHOLD` (3) is crossed, the entry stamps `backoff_until = now + min(1s << k, 30s)` where `k = consecutive_empty - threshold - 1`. The outbound dispatch loop in `on_tick` consults `is_in_backoff(target, now)` before each `SyncRequest` send and skips with a trace log. Unit test `catchup_backoff_threshold_and_reset` covers threshold + reset; the wire path is exercised by the existing replication-runtime test suite.

### R-31 — Replication coordinator: state advances before async sink completes on `* → Idle`
- **File:** `src/adapter/net/redex/replication_coordinator.rs:370-385` (sync state flip) and `:414-423` (async `withdraw_chain` / `announce_chain`).
- **What:** The state cell flips to `target` inside the sync block (line 383). The mesh-side announce/withdraw runs *after* the lock drops, across an `.await`. On `Leader → Idle` graceful relinquish, if `withdraw_chain` fails (mesh queue full, transient error), local state is already `Idle` but the mesh continues advertising this node as the chain holder. The existing regression test (`tag_sink_failure_surfaces_but_state_mutated`) pins this exact divergence.
- **Impact:** Inbound `SyncRequest` traffic from peers that still believe this node leads gets NACKed `NotLeader` (the runtime gates on `coordinator.role() != Leader` at `:695`), peers re-resolve to a phantom holder. The comment ("next heartbeat cycle will retry") is wrong — `Idle` doesn't run heartbeats, so there is no retry. The divergence persists until something else trips a re-announce.
- **Fix sketch:** Roll back the state on sink failure for `* → Idle` transitions, or maintain an explicit retry queue tied to the coordinator's lifetime (not the role state). At minimum, document the divergence window length so operators have a published recovery upper bound.

### R-32 — Replication metrics: TOCTOU on `MAX_TRACKED_CHANNELS` cap
- **File:** `src/adapter/net/redex/replication_metrics.rs:213-232` (`for_channel`).
- **What:** The path is `len() < cap` → `contains_key` probe → `entry().or_insert_with`. Two concurrent callers with the channels map at `cap - 1` both pass the `len()` check, both probe, both insert — net cardinality exceeds the cap by the number of racers.
- **Impact:** A burst of distinct channel-name lookups under contention bypasses the cardinality bomb defense. Defeats the bound the rest of the crate trusts. No data loss — just a memory growth amplifier.
- **Fix sketch:** Use `entry().or_insert_with` first, then post-check `len() > cap`; if exceeded, remove the just-inserted entry and route the caller into the overflow bucket. Or maintain a separate atomic counter CAS'd before insert.

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

### X-10 — Orchestrator forwards wire `seq_through` without checking it against `snapshot.through_seq`
- **File:** `src/adapter/net/compute/orchestrator.rs:1340-1387` (`on_snapshot_ready`).
- **What:** The orchestrator accepts a wire-supplied `seq_through: u64`, never compares it against `snapshot.through_seq` inside the parsed payload, and forwards it verbatim in the `MigrationMessage::SnapshotReady` it emits to the target (`:1383`). Single-chunk path validates `snapshot.entity_id.origin_hash()` only; `seq_through` is a free parameter. In the multi-chunk path the orchestrator commits to the wire `seq_through` as the reassembly key without validating it against any source-of-truth.
- **Impact:** A buggy or malicious source can ship `SnapshotReady` whose wire `seq_through` disagrees with the payload's `snapshot.through_seq`. The disagreement propagates to the target — `restore_snapshot` consumes `snapshot.through_seq` for `replayed_through` but logs/retry decisions/audit using the wire field see a different number. Multi-chunk surfaces a split-state debugging trap.
- **Fix sketch:** In the single-chunk validation branch, assert `snapshot.through_seq == seq_through` and reject as `StateFailed` on mismatch. For multi-chunk, defer the assertion to the target after reassembly; or strip `seq_through` from the wire layout once chunks are reassembled (it's redundant with `snapshot.through_seq`).

### X-11 — `MigrationSourceHandler::buffered_events` unbounded
- **File:** `src/adapter/net/compute/migration_source.rs:182, 221` (`SourceMigrationState.buffered_events`, `buffer_event`).
- **What:** Mirror of X-9 on the source side. The pre-encode `Vec<CausalEvent>` has no cap. If a long-running migration stalls (target stuck in Restore/Replay because of upstream stall), the source-side buffer grows without bound for as long as the migration stays open. The wire-decode cap on `BufferedEvents` (1_000_000 at `orchestrator.rs:436`) is post-encode; the source-side in-memory vec never sees it.
- **Impact:** Threat surface is lower than X-9 (local control plane drives writes), but a stuck migration plus a high-volume daemon produces unbounded memory growth on the source node.
- **Fix sketch:** Add `MAX_SOURCE_BUFFERED_EVENTS` / `MAX_SOURCE_BUFFERED_BYTES`; on overflow, fail the migration with `MigrationFailureReason::StateFailed("source buffer exhausted")` so the orchestrator can abort cleanly rather than OOM'ing the node.

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

### O-4 — Chain record appended AFTER dispatch → audit gap on appender failure — **Landed**
- **File:** `src/adapter/net/behavior/meshos/executor.rs:473-477` (also `:497-498, :502-507, :518`)
- **What:** Executor calls `self.dispatcher.dispatch(...).await` first; on `Ok(())` then `append_dispatched(&self.chain_appender, &action)`. If the chain appender's write fails (disk full, RedEX hiccup), the action *was* executed but the chain has no record. The chain is documented as the "cluster-lifetime replay" of the action stream — a missed entry breaks replay correctness. Current code only logs via `let _ = append_dispatched(...)`.
- **Fix:** Took the "accept the gap and document it loudly" branch of the fix sketch. Added `ExecutorStats::chain_append_failures: AtomicU64` and a `record_chain_append(action_id, kind, result)` helper that bumps the counter and warn-logs every failed chain-append (`executor.rs:595-604`). All five append sites (`failed_defer_budget`, `gated`, `dispatched`, `failed_retry_budget`, `failed_retry`, `failed`) route through the helper. The counter is surfaced in `ExecutorStatsSnapshot.chain_append_failures` and the runtime's reconcile snapshot so operators can dashboard the audit-gap rate. Two-phase commit (the alternative branch) was rejected as too invasive — the chain layer would need a `Pending` disposition new variant and a deterministic outcome-stamp pass.

### O-5 — `record_admin_audit` chain append before ring push → ring/chain divergence — **Landed**
- **File:** `src/adapter/net/behavior/meshos/event_loop.rs:1086-1100` (also `record_log_line:1121-1135`)
- **What:** Loop bumps `admin_audit_seq`, appends to chain, then pushes to in-memory ring. If the chain append fails (e.g., RedEX appender returns Err), the warn log fires and we *still* push to the ring → chain says "seq N missing" but ring says "seq N present." If the chain append panics (OOM in the appender), `seq` has already been incremented and chain holds an entry the ring will never reflect.
- **Fix:** Took the "ring-first + chain_pending flag" branch of the fix sketch. The loop in `event_loop.rs` now pushes to the ring first, then attempts the chain append; on chain failure it sets `chain_pending = true` on the ring entry so consumers can distinguish "ring committed, chain still catching up." Both `AdminAuditRecord` and `LogRecord` gained a `chain_pending: bool` field with `#[serde(default)]` so cross-version replay decodes consistently. Sites that construct these records pre-populate `chain_pending: false`; the runtime flips it to `true` on the chain-failure path.

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

**Dataforts (D-6..D-10, D-12, D-13):**
- D-6 — `BlobAdapter::store_stream` default impl trusts `size_hint` only; concatenates unbounded stream into `buf` (`adapter.rs:155-170`)
- D-7 — `BlobMetrics::set_disk_capacity_bytes` writes `0` for NaN ratio silently (`metrics.rs:244`)
- D-8 — `parse_blob_heat_tag` admits mixed-case hex; canonicalization drift risk (`migration.rs:98-120`)
- D-9 — `chain_blob_refs` ↔ refcount lock ordering asymmetric between callers (`greedy/runtime.rs:419-437`)
- D-10 — `global_blob_adapter_registry` process-static `OnceLock`; no clear path (`registry.rs:112-117`)
- D-12 — `GreedyCacheRegistry::next_lru_pos` `saturating_add(1)`; once saturated every `touch`/`upsert` returns the same `lru_pos` and LRU ordering silently collapses. Unreachable today; fragile under test fixtures that seed the counter (`greedy/cache.rs:218-222`)
- D-13 — `HeatRegistry::tick` prune predicate compares f64 with `==` against `Some(0.0)`; a future caller writing `-0.0` (clamped-from-negative rate) breaks pruning. Use `map_or(false, |v| v <= 0.0)` (`gravity/counter.rs:319-320`)

**Compute (X-6..X-8, X-12):**
- X-6 — `Scheduler::find_migration_targets` allocates const tag string per call (`scheduler.rs:158-173`)
- X-7 — `fork_group.rs:292-298` dead `.unwrap()` lookup discarded via `let _ = ...`
- X-8 — `SnapshotReassembler::feed` sweeps before validating malformed chunks → DoS amplifier (`orchestrator.rs:843-857`)
- X-12 — `MAX_SNAPSHOT_SIZE: usize = u32::MAX as usize * MAX_SNAPSHOT_CHUNK_SIZE` overflows in const on 32-bit (cosmetic — wrong "max" in the `SnapshotTooLarge` error doc-comment, "~28 TB" is then a lie); separately, `record.started_at.elapsed().as_millis() as u64` at `:1710` wraps instead of saturating, inconsistent with the canonical helper `state.elapsed_ms()` used in `migration.rs:283` (`orchestrator.rs:595, 1710`)

**Replication (R-29, R-30, R-33, R-34):**
- R-29 — `replica_set` admits duplicates → double heartbeats; should be `BTreeSet` (`replication_step.rs:186-189`)
- R-30 — `wall_clock_ms` collected from heartbeats but never used; either implement the skew gauge or drop the field (`replication.rs:237-244`) — **Kept as reserved.** Dropping is a wire-format break, and a future drift-gauge feature would need the same 8-byte slot back. Leave the field in place; receivers ignore the value until the gauge ships.
- R-33 — `consumed + payload_len` (where `payload_len` came from a u32 on the wire) is plain `usize` addition; on 32-bit targets it can overflow. Same shape as the umbrella's L-9 fix for `BufferedEvents`; fold into the same commit. Use `checked_add → WireError::Truncated` (`replication.rs:465, 474`)
- R-34 — `acc += cost` is plain addition while the surrounding function at `:249` uses `saturating_add`. Practically unreachable given the 64 MiB hard ceiling; the asymmetry itself is the bug — reviewers will copy the unguarded pattern (`replication_catchup.rs:258`)

**MeshOS (O-6, O-9..O-19):**
- O-6 — `Defer` re-queue never emits a chain record; long-deferred history invisible (`executor.rs:409-427`)
- O-9 — `gc_drain_window` 1-second hardcode disregards `BackpressureConfig` semantics (`backpressure.rs:273-276`)
- O-10 — `BackoffTracker::observe_crash` window-slide non-monotonic on out-of-order crashes (`supervision.rs:186-229`)
- O-11 — `MaintenanceState::EnteringMaintenance.deadline` falls back to wall-clock pre-first-tick (`state.rs:364`)
- O-12 — `emit_maintenance_transitions` skips transitions when `control_sink` is wired late (`event_loop.rs:1235-1244`)
- O-13 — `release_failed_admit` for `PullReplica` clears chain stabilization even if sibling holds it (`backpressure.rs:210-218`)
- O-14 — `MigrationSnapshotSource::list()` called inside loop hot path; slow source stalls reconcile (`event_loop.rs:1383`)
- O-15 — `last_pull_admitted_by` rollback only clears most-recent slot (`backpressure.rs:212-215`)
- O-16 — `WIRE_FORMAT_VERSION = 1` with no migration path documented (`chain.rs:51-79`)
- O-17 — `AdminVerifier::verify_bundle` has no cap on `signatures.len()`; verifier-CPU DoS if `MeshOsEvent` ever crosses a process boundary. Add `MAX_SIGNATURES_PER_BUNDLE = 64` (or whatever exceeds the realistic operator-quorum) (`ice.rs:483-502`, event def `event.rs:82-117`)
- O-18 — `AdminVerifier::verify_commit` drops the `ice_state` mutex between `check_ice_cooldown` and `record_ice_cooldown`. `AdminVerifier: Clone`; two concurrent callers (or any future "verify-from-admin-API while loop runs") fail-open through the cooldown gate. Hold the lock for the whole `verify_commit` body, or split into "reserve / commit" pair (`ice.rs:858-877`)
- O-19 — `BufferingActionChainAppender::with_capacity(0)` accepted; every `append` increments `dropped_count` against an empty deque. Sibling `BufferingAdminAuditChainAppender::with_capacity` clamps to `max(1)` at `audit_chain.rs:97`; mirror that here (`chain.rs:247-289`)

---

## Third-pass additions

Findings from the 2026-05-18 evening pass. IDs continue the existing
per-module sequences (D-13 → D-14, R-34 → R-35, X-12 → X-13).

### High

#### D-14 — `resolve_payload` unconditionally fails on every `BlobRef::Manifest`
- **File:** `src/adapter/net/dataforts/blob/dispatch.rs:82`; `src/adapter/net/dataforts/blob/blob_ref.rs:562-579`
- **What:** `resolve_payload` calls `blob.verify(&fetched)` on the assembled bytes. `BlobRef::verify` hard-errors on the `Manifest` arm with `BlobError::Decode("verify is undefined on a Manifest variant; verify chunks individually")`. Every payload that took the chunked path through `publish_with_blob` (i.e. anything larger than `BLOB_CHUNK_SIZE_BYTES = 4 MiB`) returns that error, even though `MeshBlobAdapter::fetch_chunk` already per-chunk-verified each piece against its manifest entry at `mesh.rs:937-943`.
- **Impact:** Functional regression on any consumer that resolves an event payload via `resolve_payload` — including the FFI surface `src/ffi/blob.rs:319`. Any chunked publish is un-fetchable through the documented helper. Fails closed (Decode error, not silent corruption), so this is availability not safety, but it is end-to-end broken for large payloads.
- **Fix sketch:** Branch on `blob.is_chunked()` in `resolve_payload`: for Manifest, skip the top-level verify (per-chunk hashes were checked at fetch time); for Small, keep the current verify. Or add `BlobRef::verify_manifest(bytes, &chunks)` that recomputes per-chunk hashes against the concatenated buffer. Add a round-trip test exercising the chunked path through `resolve_payload`.

### Medium

#### D-15 — GC `delete_chunk` does not unlink the persistent segment file
- **File:** `src/adapter/net/dataforts/blob/mesh.rs:777-784`; cf. `src/adapter/net/redex/manager.rs:729-740` → `src/adapter/net/redex/file.rs:1399-1435`
- **What:** `delete_chunk` calls `redex.close_file(channel)` and unconditionally `self.refcount.remove(hash)`. `close_file` fsyncs and drops the in-memory `RedexFile` but never `std::fs::remove_file`s the segment on disk. The docstring claims "the chunk's `RedexFile` is closed + removed from the Redex manager"; the on-disk file is *not* removed.
- **Impact:** Slow disk leak on `MeshBlobAdapter::with_persistent(true)` deployments — refcount metadata says the chunk is reclaimed, GC metrics report bytes freed, but the segment file accumulates indefinitely. A future re-store under the same hash stamps fresh `first_seen`, restarting the retention clock; until then the orphaned segment is dead weight.
- **Fix sketch:** Expose `remove_file_and_unlink` (or `unlink_segment_path`) from `RedexManager` and call it after `close_file` returns Ok. Alternatively have `MeshBlobAdapter` retain the segment path and `std::fs::remove_file(path)` directly. Update the docstring to match the actual semantics either way.

#### D-16 — `MeshBlobAdapter::fetch_range` on Small reads the entire chunk before slicing
- **File:** `src/adapter/net/dataforts/blob/mesh.rs:1135-1141`
- **What:** The Small arm calls `self.fetch_chunk(hash)` (which reads the *whole* chunk into a `Vec<u8>`) and then slices `bytes[range.start..range.end]`. A `0..16` range against a 16 GiB Small blob allocates 16 GiB. The early `range.end <= size` check only validates the upper bound, not that the requested slice is small relative to the chunk.
- **Impact:** DoS amplifier — a tiny range request consumes adapter-side RAM proportional to the *blob*, not the *range*. Reachable wherever a peer-controllable `BlobRef::Small` lands and a caller does range-fetches against it (web range requests, partial reassembly, lazy decoders).
- **Fix sketch:** Route the Small arm through a seek-based `read_range` analogous to `FileSystemAdapter::fetch_range` (`fs.rs:288-336`). For the RedEX-backed path that means a `RedexFile::read_at(offset, len)` primitive that doesn't materialize the whole segment.

#### R-35 — Wall-clock `std::time::Instant::now()` inside tokio-virtualized runtime loop
- **File:** `src/adapter/net/redex/replication_runtime.rs:518, 678, 716`
- **What:** Same M-4 anti-pattern fixed elsewhere on this branch. The runtime tick is driven by `tokio::time::interval`, which honors `tokio::time::pause()` for deterministic tests, but the handler at `:518` (silence detection), `:678` (`record_heartbeat`), and `:716` (`BandwidthBudget::try_consume`) feeds `std::time::Instant::now()` into the time-domain calls. Under `tokio::time::pause()` virtual time advances while wall-clock doesn't — silence detection never fires, the bandwidth budget never refills, election logic becomes untestable deterministically.
- **Impact:** Tests under `tokio::time::pause()` get inconsistent behavior between time domains; in production the timer wakeups and clock readings can diverge across sleep/resume or VM migration in subtle ways.
- **Fix sketch:** `tokio::time::Instant::now().into_std()` at all three sites, or thread a `Clock` trait through `inputs` so the tracker / budget take the same time source the interval timer uses.

#### X-13 — Failed placements don't retry on different-node recovery
- **File:** `src/adapter/net/compute/fork_group.rs:289-355`; `replica_group.rs:241-290`; `standby_group.rs:400-461`
- **What:** All three group `on_node_failure*` paths mark the affected slot `mark_unhealthy` *before* attempting placement. On placement failure (`continue`), the slot stays unhealthy with the dead node's `origin_hash` still in the registry. `on_node_recovery` only re-marks the slot healthy when the recovered node id matches the FAILED node id — recovery of a *different* spare node (which arrives later and could host the slot) never retries placement. The slot stays permanently degraded until either the originally-failed node recovers, or another `on_node_failure*` for the same slot fires (which it won't, because the node is already marked unhealthy).
- **Impact:** Hot spares coming online during a partial-outage incident are silently ignored. The group's effective replica count drops and stays dropped past the operator-visible "all nodes recovered" signal. Composes badly with X-1's fencing gap because the still-unhealthy slot looks like an active replica to anything reading group membership.
- **Fix sketch:** Add a `retry_failed_placements(&scheduler, &registry)` helper called from `on_node_recovery` (or driven by a periodic tick) that iterates unhealthy slots and retries `place_with_spread` / `place_member` against the current healthy-node pool. Alternatively, defer `mark_unhealthy` until *after* a successful placement.

### Low

#### R-36 — `ReplicationRuntimeHandle::is_stopped()` flips `true` before joiner's `.await` returns
- **File:** `src/adapter/net/redex/replication_runtime.rs:297-320`
- **What:** The R-11 comment at `:316-318` claims this is fixed: "Flip the 'joined' flag only after the await returns. Concurrent cancel() racers that lost the handle take(): poll `stopped` until our await completes." But the implementation has the bug: the loser of `self.task.lock().take()` enters `cancel()`, sees `handle = None`, skips the entire `if let Some(h)` block, and then unconditionally executes `self.stopped.store(true, Release)` at `:319` — while the winner is still inside `h.await`. The doc on `is_stopped()` (`:322-333`) says "task joined"; the actual semantics is "shutdown initiated."
- **Impact:** Tests / observability racing two `cancel()` calls can observe `is_stopped() == true` before the join completes — but the actual practical risk is small because the winner's join still completes correctly afterward.
- **Fix sketch:** Only the holder of the `JoinHandle` writes `stopped`. Loser races spin on `is_stopped()` or wait on a `tokio::sync::Notify` armed by the winner after `h.await` returns.

#### R-37 — `Drop for ReplicationRuntimeHandle` takes a parking_lot mutex
- **File:** `src/adapter/net/redex/replication_runtime.rs:346-350`
- **What:** Same M-5 anti-pattern fixed in `bus.rs`. `Drop` calls `self.task.lock().take()`. On a single-thread runtime panicking during shutdown, drop can run on a thread already holding `self.task` (e.g. mid-`cancel()` when the future is dropped on panic), producing a deadlock.
- **Fix sketch:** `try_lock` with a short timeout, or store the `JoinHandle` in an `ArcSwapOption` so Drop can swap-and-take without locking.

#### R-38 — Leader disk-pressure transitions through `ChannelClose` signal
- **File:** `src/adapter/net/redex/replication_runtime.rs:1043-1048`
- **What:** When a `Leader` or `Candidate` hits disk pressure, the runtime transitions through `ChannelClose` rather than a disk-pressure-specific signal (the FSM only permits `Replica → Idle` via `DiskPressureWithdraw`; Leader/Candidate fall back to the universal `ChannelClose → Idle` arm). `under_capacity_total` is bumped at `:1018` but the *transition* metric is labeled "channel closed," so operator dashboards see "graceful channel close" rather than "disk-pressure withdraw." Incident triage misroutes.
- **Fix sketch:** Add `LeaderDiskPressure` / `CandidateDiskPressure` signals valid from those roles to Idle, OR document the metric semantic conflation prominently.

#### R-39 — Concurrent `Leader` heartbeats overwrite `believed_leader` without tiebreak
- **File:** `src/adapter/net/redex/replication_heartbeat.rs:131-133`
- **What:** `HeartbeatTracker::record_heartbeat` sets `believed_leader = from` on every heartbeat that arrives with `role == Leader`. Two peers each legitimately claiming Leader during a failover (or one malicious peer asserting Leader against a real Leader's heartbeats) flip `believed_leader` every tick. The replica's `tick()` then issues `SyncRequest` against whichever leader was last seen; if the two have divergent `tail_seq`s, alternating chunks land. No tiebreaker, no fencing token, no two-heartbeat confirmation.
- **Impact:** Mostly subsumed by R-21 once that's fixed (a properly-converging FSM removes the dual-leader scenario), but the race window exists today and amplifies R-21's data-divergence consequences.
- **Fix sketch:** Keep the lex-smallest `NodeId` among concurrent Leader claimants in `believed_leader`, OR require two consecutive heartbeats from the new leader before switching.

#### X-14 — `ReplicaGroup::scale_to` scale-down loop lacks the `let-else+break` guard
- **File:** `src/adapter/net/compute/replica_group.rs:210-215`
- **What:** `while self.coord.member_count() > n { if let Some(info) = self.coord.remove_last() { ... } }`. If `remove_last()` ever returns `None` while `member_count() > n` (i.e. an invariant violation introduced elsewhere), the loop spins forever. The sibling `fork_group.rs:244-260` was hardened against this with `let Some(info) = ... else { debug_assert!(false, ...); break; }`; this sibling missed the same defense.
- **Fix sketch:** Mirror the fork-group pattern. `let Some(info) = self.coord.remove_last() else { debug_assert!(false, "member_count > n but remove_last is None"); break; };`.

#### X-15 — `TargetMigrationState.target_head` is dead-but-misleading state
- **File:** `src/adapter/net/compute/migration_target.rs:37, 175, 193, 488`
- **What:** The field is initialized to `snapshot.chain_link` in `restore_snapshot` and reassigned in `drain_pending` to `last.link` — but `last.link` lives on the SOURCE's chain, not the target daemon's chain, and the field is never *read* anywhere. Dead storage with a misleading value. A future reader wiring it to a continuity check would silently use the wrong chain head.
- **Fix sketch:** Either delete the field, or compute the actual target-chain head via `daemon_registry.with_host(origin, |h| h.head_link())` after each `deliver` so the name matches the value.

#### X-16 — `Scheduler::select_migration_target` v2 short-circuits to local before `PlacementFilter`
- **File:** `src/adapter/net/compute/scheduler.rs:337-339` (and the LocalPreferred branch at `:281-285`)
- **What:** `select_migration_target` returns `local_node_id` immediately whenever local is in the candidate pool and isn't the source, *before* invoking the `PlacementFilter`. A filter that hard-vetoes local (returns `Some(_)` only for remotes, `None` for local) is silently ignored — local always wins. The docstring documents this as a "LocalPreferred fast-path," but a strict-veto filter cannot actually veto the local node, defeating the point of plugging in a filter.
- **Fix sketch:** Score local through the filter first; only short-circuit when its filter result is `Some(_)`. The fast-path then becomes "if local is permitted, pick local."

#### X-17 — `on_snapshot_ready` multi-chunk path skips `set_snapshot` validation
- **File:** `src/adapter/net/compute/orchestrator.rs:1356-1372`
- **What:** The orchestrator validates and calls `set_snapshot` only on the single-chunk path (`chunk_index == 0 && total_chunks == 1`); the multi-chunk path only calls `force_phase(Transfer)` and forwards. Structural validation of any chunk header is deferred entirely to the target after reassembly. Single-chunk corruption is caught at the orchestrator; multi-chunk corruption is not.
- **Impact:** Defense-in-depth gap, not a correctness break (target reassembly + final verify still catches the corrupt snapshot). The asymmetry is the bug: single-chunk and multi-chunk should validate at the same layer. Composes with X-10 (`seq_through` not cross-checked against `snapshot.through_seq`) to make multi-chunk a debugging trap.
- **Fix sketch:** Either drop validation on the single-chunk path to match multi-chunk, or extract a `validate_chunk_header(chunk, record)` helper and call it on every chunk regardless of count.

---

## Fourth-pass additions

Findings from the 2026-05-18 late-evening pass. Adds the `MD-*` prefix
for `behavior/meshdb/` (newly in-scope). ID numbering continues the
existing per-module sequences (X-17 → X-18, O-19 → O-20).

### High

#### X-18 — Migration dispatch arms accept arbitrary `from_node`; any peer forces cutover / abort — **Landed**
- **File:** `src/adapter/net/subprotocol/migration_handler.rs:600-642` (`CleanupComplete`, `ActivateTarget`); `:654-690` (`MigrationFailed`); `:558-598` (`SnapshotReady`); `src/adapter/net/compute/migration_target.rs:295-306` (`activate`).
- **What:** The subprotocol's `ActivateTarget` arm invokes `target_handler.activate(daemon_origin)` without comparing the inbound `from_node` against the migration's recorded `orchestrator_node`. The recorded orchestrator is consulted only when routing the *ack reply* (`:626-629`); it is not used to gate entry. The companion arms `CleanupComplete`, `MigrationFailed`, and `SnapshotReady` likewise dispatch state-mutating handler calls without binding `from_node` to the recorded orchestrator. Only the `TakeSnapshot` arm records the orchestrator (`start_snapshot(..., from_node)` at `:312`).
- **Trigger / Attack:** Any peer with subprotocol-0x0500 reach ships `MigrationMessage::ActivateTarget{daemon_origin}` for a migration that is mid-Replay. The target flips to `Cutover` and goes live while the source still believes it owns the daemon → both nodes accept writes to the same origin → divergent chain heads. Same shape as **X-1** (StandbyGroup fencing) but driven by a single wire message rather than a partition heal. Symmetric variants: a forged `MigrationFailed` from any peer drives source rollback after legitimate cutover; a forged `CleanupComplete` makes the orchestrator emit `ActivateTarget` to a target that hasn't fully restored.
- **Same shape as R-20:** R-20 is "no replica-set membership check on replication-subprotocol inbound." X-18 is the migration-subprotocol equivalent. The umbrella's A-1..A-3 capability fixes landed on the publish path; neither subprotocol was in their scope.
- **Fix:** Added `MigrationError::WrongPeer { daemon_origin, from, expected }` and gated four state-mutating dispatch arms:
  - **`SnapshotReady`** — orchestrator-side requires `orchestrator.source_node(daemon_origin) == Some(from_node)`; target-side requires `target_handler.orchestrator_node(daemon_origin) == Some(from_node)`. Falls through unchecked only on the no-record path (first chunk to a fresh target with no orchestrator-on-this-node).
  - **`CleanupComplete`** — orchestrator-side requires `orchestrator.source_node == Some(from_node)`.
  - **`ActivateTarget`** — target-side requires `target_handler.orchestrator_node == Some(from_node)`.
  - **`MigrationFailed`** — collects the four possible principals (`orch.source_node`, `orch.target_node`, `source_handler.orchestrator_node`, `target_handler.orchestrator_node`); when *any* is recorded, `from_node` must match at least one. Migrations with no local record drop silently — no phantom abort.
- **Regression test:** `test_regression_dispatch_arms_reject_unrelated_from_node` in `tests/migration_integration.rs` covers forged `CleanupComplete` and `MigrationFailed` from a non-participant; asserts `WrongPeer` is returned and that orchestrator state is unchanged. The legitimate source still drives cleanup forward.

### Medium

#### O-20 — `admin_audit_seq` / `log_seq` plain-`+= 1`; wrap collides SDK dedup keys
- **File:** `src/adapter/net/behavior/meshos/event_loop.rs:1078` (`admin_audit_seq`), `:1112` (`log_seq`).
- **What:** Per-runtime monotonic sequence counters are `u64` fields incremented with `self.admin_audit_seq += 1` / `self.log_seq += 1`. On overflow they wrap to 0. The SDK dedup gate is keyed on `(seq, runtime_epoch_id)` — a wrapped `seq=1` collides with the ancient record at `seq=1`, and the dedup gate silently drops the new audit/log record.
- **Impact:** Astronomical in practice (2^64 events ≈ centuries even at 10⁹/s) but the cost-to-fix is trivial and saturating preserves monotonicity rather than producing a key collision. Sibling counters in the same module already use `saturating_add` (compare maintenance loop, snapshot publisher).
- **Fix sketch:** `self.admin_audit_seq = self.admin_audit_seq.saturating_add(1)` (and same for `log_seq`).

#### MD-1 — Federated `drain_rows` unbounded; remote-peer aggregate response OOMs aggregator
- **File:** `src/adapter/net/behavior/meshdb/federated.rs:732` (`execute_aggregate_numeric_federated`), `:952` (`drain_rows` helper).
- **What:** `drain_rows()` collects all rows from a remote `ResultStream` into `Vec<ResultRow>` with no memory budget. Hash join paths *do* pre-check against `HASH_JOIN_MEMORY_BYTES` (256 MiB); federated aggregate and window operators drain the inner stream unbounded into a `BTreeMap<GroupKey, ...>` for grouped processing.
- **Trigger:** Remote peer (or a misconfigured federation target) executes `Aggregate { inner: Between(...wide_seq_range) }` and returns millions of rows; the aggregating node drains them all before producing the grouped output. OOM lands on the aggregator, not the peer that returned the rows.
- **Fix sketch:** Add `AGGREGATE_MAX_BYTES` (analog to `HASH_JOIN_MEMORY_BYTES`); accumulate bytes during `drain_rows` and return `MeshError::QueryBudgetExceeded` once the budget is hit. The planner's `total_cost.bandwidth_bytes` estimate can drive a pre-flight check; the runtime accumulator catches a misestimate.

### Low

#### MD-2 — `WindowSpec::TumblingSeq` saturates near `u64::MAX`; collides bucket boundaries
- **File:** `src/adapter/net/behavior/meshdb/executor.rs:874-875`
- **What:** Window bucketing computes `start = bucket.saturating_mul(size); end = start.saturating_add(size)`. Two adjacent buckets near `u64::MAX` both saturate to `end = u64::MAX`, producing indistinguishable `WindowBoundary` envelopes — breaks downstream `OrderBy` / cache-key disambiguation that treat boundary tuples as unique.
- **Impact:** Centuries-away seq-counter precondition; not exploitable today. Listed for cheap pre-emption.
- **Fix sketch:** At plan time, reject `WindowSpec::TumblingSeq{size}` where `size > u64::MAX / 2` (alongside the existing `size == 0` check at `:857`).

### Latent

#### MD-3 — `ContinuationToken` opaque-but-unsigned; future cross-peer forgery on `Resume`
- **File:** `src/adapter/net/behavior/meshdb/protocol.rs:57` (`pub struct ContinuationToken(pub Vec<u8>)`); current handler at `src/adapter/net/behavior/meshdb/federated.rs:1152` returns `"Resume not yet implemented in LoopbackTransport (Phase B-4+)"`.
- **What:** The token is an unsigned `Vec<u8>` carried verbatim through `MeshDbRequest::Resume`. When Phase B-4 wires real Resume handling, any peer that can address `SUBPROTOCOL_MESHDB` to a federated server can forge a token claiming arbitrary executor-private resumption state — e.g. "skip the row-level capability filter," "resume against a different tenant's cursor," or "jump past pagination." The opaque-bytes design has no peer-binding nonce or HMAC.
- **Impact:** Not exploitable today (Resume errors out before reaching state-mutating code). Flagged here so the design issue is closed during Phase B-4 rather than after.
- **Fix sketch:** Mint tokens as `serialize({peer_id, call_id, executor_state}) || HMAC(server_key, payload)`. On `Resume`, verify the HMAC and reject if `peer_id != from_node` or `call_id` is not in the executor's known-call set.

### Fourth-pass edges (not promoted)

Each requires exceptional preconditions; listed for completeness rather
than threaded into the main severity buckets.

- **`replayed_through.saturating_add(1)` deadlock at `migration_target.rs:452`** — at `u64::MAX` events `replayed_through` saturates and `drain_pending` never advances; further events sit in `pending_events` forever. Same astronomical precondition as **O-20**; reject further buffering with a typed `SequenceOverflow` when `replayed_through == u64::MAX`.
- **`MigrationSourceHandler::start_snapshot` single-flight CAS gap at `migration_source.rs:121-170`** — `contains_key` check followed by `entry()` insert can let two concurrent callers both reach `daemon_registry.snapshot()`. The umbrella's docs claim duplicate snapshot work is acceptable, but if a future `snapshot()` impl has non-idempotent side-effects (counter bumps, deferred I/O) both fire. Rely entirely on `snapshots_in_progress` CAS and remove the separate `contains_key` check.
- **Orphan `orchestrator_node` ref on pre-complete abort (`migration_target.rs:27`)** — if a migration aborts after the orchestrator-to-both link severs but the source-target link survives, the orchestrator never learns the failure. No outbound message-routing exists on the handler today; either add it or document the limitation.
- **`completed`-record orphan on failed source cleanup (`migration_target.rs:337-371`)** — if the orchestrator gives up after a transient source-cleanup failure, the target's `completed` index keeps the entry indefinitely. A later legitimate re-migration of the same daemon shadows under the stale completion. Either gate `forget_completed()` on successful source cleanup, or age-evict `completed` entries.

---

## Sixth pass — targeted subprotocol sweep (2026-05-18)

Follow-up to R-20 + X-18 + the umbrella's coverage-gap recommendation:
*"for every subprotocol arm that mutates state, verify `from_node` is
bound to a recorded principal before the mutation."* Audit covers the
15 remaining subprotocols (REDEX and MIGRATION already covered by R-20
and X-18 respectively):

- `SUBPROTOCOL_CAUSAL` (0x0400), `SUBPROTOCOL_SNAPSHOT` (0x0401)
- `SUBPROTOCOL_NEGOTIATION` (0x0600)
- `SUBPROTOCOL_CONTINUITY` (0x0700), `SUBPROTOCOL_FORK_ANNOUNCE` (0x0701), `SUBPROTOCOL_CONTINUITY_PROOF` (0x0702)
- `SUBPROTOCOL_PARTITION` (0x0800), `SUBPROTOCOL_RECONCILE` (0x0801)
- `SUBPROTOCOL_REPLICA_GROUP` (0x0900)
- `SUBPROTOCOL_CHANNEL_MEMBERSHIP` (0x0A00)
- `SUBPROTOCOL_STREAM_WINDOW` (0x0B00)
- `SUBPROTOCOL_CAPABILITY_ANN` (0x0C00)
- `SUBPROTOCOL_REFLEX` (0x0D00), `SUBPROTOCOL_RENDEZVOUS` (0x0D01)
- `SUBPROTOCOL_MESHDB` (0x0F00)

**Findings: 1 M + 2 H** plus several null results documented for
durability. `S-*` prefix names the subprotocol sweep.

`from_node` is always the *cryptographic* session peer — extracted from
`ctx.peers` by matching `session.session_id()` at the dispatch sites in
`mesh.rs`. A non-session peer cannot inject any of these messages. The
findings below are about session peers whose `from_node` is *unbound*
to the message's logical principal — i.e. a legitimate session peer
can act for/about a different node than itself.

### High

#### S-1 — `RendezvousMsg::PunchIntroduce` correlates on payload-only `intro.peer`; any session peer cancels a victim's introduce waiter — **Landed**
- **File:** `src/adapter/net/mesh.rs:3815-3830`
- **What:** The endpoint-side branch of `PunchIntroduce` does `ctx.pending_punch_introduces.remove(&intro.peer)` and sends the payload through the oneshot, then calls `schedule_punch(from_node, intro, ctx)`. The map key is the *peer* the local node is waiting for an introduce *about* — not the coordinator that should be forwarding it. There is no check that `from_node` is the expected coordinator (the node the local `register_punch_introduce_waiter` was set up against).
- **Trigger / Attack:** Any peer with an established session to the local node sends `PunchIntroduce{peer: V}` where `V` is the node the local node is currently waiting on (for example, learned via prior session-establishment traffic patterns). The local node's oneshot waiter is satisfied with the attacker's payload (`peer_reflex` set to an attacker-chosen address); `schedule_punch` then drives a punch toward that address with `from_node = attacker`. The genuine coordinator's later `PunchIntroduce` finds the entry gone and is silently dropped.
- **Fix:** Extended `pending_punch_introduces` value to `(generation, expected_coordinator, sender)`. `request_punch(relay, target, ...)` records `coordinator = relay`; `await_punch_introduce(counterpart, coordinator)` now takes the coordinator as an explicit argument. The dispatch arm uses `remove_if` guarded by `expected_coord == from_node`; if a waiter exists but the coordinator mismatches, the introduce is dropped without scheduling a punch — `schedule_punch` only fires on the matched-coordinator path. `coordinator_fans_out_to_both_endpoints` test covers the legitimate path; broader call-site changes propagated through the rendezvous and keepalive tests.

#### S-2 — `RendezvousMsg::PunchAck` correlates on payload-only `ack.from_peer`; any session peer hijacks an ack — **Landed**
- **File:** `src/adapter/net/mesh.rs:3831-3841`
- **What:** The final-recipient branch of `PunchAck` does `ctx.pending_punch_acks.remove(&ack.from_peer)`. The key is the *claimed sender* in the payload, not `from_node`. No validation that the session peer is actually `ack.from_peer` (which they should be in the direct-ack final-leg case).
- **Trigger / Attack:** Local node calls `connect_direct(V)`, registering a pending ack waiter keyed on `V`. Any peer with a session sends `PunchAck{from_peer: V, to_peer: local_id, ...}`. The local node's `connect_direct` future resolves with the attacker's ack data — the attacker effectively impersonates `V` for the purpose of completing the local node's punch handshake. The legitimate ack from `V`, if it arrives later, finds nothing to complete and is dropped.
- **Note:** The audit's original fix sketch (`from_node == ack.from_peer`) is wrong for the deployed protocol: acks always reach the final recipient via the coordinator's `forward_punch_ack` (mesh.rs:5850-…), so `from_node` is the coordinator at the recipient hop, not the original sender. Binding to `expected_coordinator` is the correct shape and preserves the forwarded-ack path.
- **Fix:** Extended `pending_punch_acks` value to `(generation, expected_coordinator, sender)`. The SinglePunch arm in `connect_direct` records the `coordinator` argument; `await_punch_ack(counterpart, coordinator)` now takes the coordinator explicitly. Dispatch arm uses `remove_if` guarded by `expected_coord == from_node`. Regression test `punch_ack_forged_by_non_coordinator_session_peer_is_dropped` (rendezvous_ack.rs): an unrelated session peer `x` sends a forged `PunchAck { from_peer: b_id, to_peer: a_id }` to `a` and the waiter times out instead of resolving with the forged payload.

### Medium

#### S-3 — `MembershipMsg::Ack` correlates on nonce alone; sequential nonces let any session peer spoof Subscribe/Unsubscribe responses — **Landed**
- **File:** `src/adapter/net/mesh.rs:5028-5041` (Ack arm); nonce generation at `:4902-4906`
- **What:** Ack is correlated by `ctx.pending_membership_acks.remove(&nonce)` — keyed on `u64` nonce only, not `(nonce, from_node)`. Combined with the nonce generator at `:4902-4906`:
  ```rust
  static COUNTER: AtomicU64 = AtomicU64::new(1);
  COUNTER.fetch_add(1, Ordering::Relaxed)
  ```
  Nonces are a process-global monotonically increasing u64 starting at 1. A peer that establishes a session and triggers its own Subscribe observes its issued nonce, then knows the next-issued nonces will be sequential. Any session peer can send a spoofed `Ack{nonce: N+k, accepted: false, reason: Unauthorized}` for a small `k` to satisfy a victim's in-flight `Subscribe`/`Unsubscribe` request issued from the same process.
- **Trigger / Attack:** Attacker establishes a session, issues a Subscribe (observes nonce `N`), then sends `Ack{nonce: N+1, accepted: false}` repeatedly at small offsets. Any local-process subscribe that issues near that window receives the spoofed denial and reports a false "Unauthorized" to its caller. Cannot grant unauthorized subscribe (the Subscribe arm path runs `authorize_subscribe`, which the attacker cannot influence); the bug is a DoS / fault-injection primitive against in-flight membership flows.
- **Severity choice:** Medium rather than high because the attacker cannot grant access — the bug is a one-way denial primitive. Sequential nonces make it cheaply reachable from any session peer, which is why it isn't low.
- **Fix:** `pending_membership_acks` value extended to `(expected_responder, sender)`. The Subscribe/Unsubscribe issue site at `mesh.rs:4983` stores `publisher_node_id` (the node we sent the request to) as the expected responder. The dispatch arm uses `remove_if` guarded by `expected == from_node` — non-publisher session peers' acks drop with a trace log. Independently, nonces now come from `getrandom::fill` (8 random bytes → u64 LE) instead of the process-global sequential `AtomicU64::new(1)`; failure of the entropy source surfaces as `AdapterError::Connection`. Both halves are necessary: peer-binding alone closes the spoof; random nonces also remove the "observe one nonce, predict the next K" predictability of the pre-fix counter.

### Verified clean

- **STREAM_WINDOW (0x0B00)** — Grants land *inside* the encrypted session that carried them; `from_node` is the cryptographic session peer by construction, and grants only mutate stream state belonging to that same session. No additional binding needed.
- **CHANNEL_MEMBERSHIP `Subscribe` (`mesh.rs:4973-5014`)** — `authorize_subscribe` validates `from_node` against `peer_entity_ids`, `peer_caps`, `peer_subnets`, tokens, and the registry ACL *before* the `auth_guard.allow_channel` + `roster.add_with_mode` mutations.
- **CHANNEL_MEMBERSHIP `Unsubscribe` (`mesh.rs:5016-5027`)** — Although the arm has no payload-side principal field, the mutations apply only to the *session peer's own* subscription (`subscriber_origin_hash(from_node)` and `roster.remove(&id, from_node)`). A peer cannot unsubscribe a different peer this way; the worst they can do is unsubscribe themselves, which is the intended use.
- **CAPABILITY_ANN (0x0C00)** — Direct announcements (`hop_count == 0`) are rejected at `mesh.rs:5153` unless `ann.node_id == from_node`; signature-verified direct anns drive the TOFU pin (`:5265-5276`) and subnet binding (`:5303-5308`). Forwarded announcements (`hop_count > 0`) install a routing-table hint keyed `ann.node_id → from_node` *as a relay route* — the entity identity itself is still signature-bound, the relay route is a discovery hint. This is a design choice (peer discovery accepts weak routing info) rather than the R-20/X-18 class of bug; flagged here for future review if the relay hint ever becomes load-bearing for auth decisions.
- **REFLEX (0x0D00)** — Echo subprotocol; the pending-probe map is keyed by `from_node` (the *responder*) and the response only completes that key's waiter. No payload-side principal field to spoof. Resource-cap question (no per-peer limit on `pending_reflex_probes` map size) is bounded by peer count and is not the R-20/X-18 class.
- **MESHDB (0x0F00)** — In-flight calls are tracked by `(peer, call_id)` and `Cancel` / `Response` arms key on the tuple, not on `call_id` alone (`transport.rs`). `Resume` is currently a stub (`federated.rs:1152`); the latent design issue is already tracked as **MD-3**.
- **CAUSAL (0x0400), SNAPSHOT (0x0401), NEGOTIATION (0x0600), CONTINUITY (0x0700)** — These are *registered* in `SubprotocolRegistry::with_defaults` (`subprotocol/registry.rs`) but are NOT pre-dispatched by `mesh.rs` — they fall through to the standard event-frame path at `:3857-3897` and queue into the application-level `inbound[]` for daemon consumption. The binding check, if any, belongs at the daemon-consumer layer, not the dispatch layer. Each consumer needs its own audit; this sweep covers only the dispatch surface.
- **FORK_ANNOUNCE (0x0701), CONTINUITY_PROOF (0x0702), PARTITION (0x0800), RECONCILE (0x0801), REPLICA_GROUP (0x0900)** — These IDs are *defined* but *no inbound dispatcher* exists in `mesh.rs`, no router, no application consumer. `REPLICA_GROUP` is explicitly documented as "Intentionally NOT in `SubprotocolRegistry::with_defaults()`" with a "register when cross-node group coordination is implemented" comment (`replica_group.rs:35-44`). The others appear to be similar reservations. **No attack surface today** — but when these IDs are wired up, every state-mutating arm needs an R-20/X-18-style `from_node` binding check from the outset.

### Carry-forward audit recommendations

1. **The four queue-to-application subprotocols (CAUSAL, SNAPSHOT, NEGOTIATION, CONTINUITY)** need a follow-up audit at the *consumer* layer. The dispatch is clean; the binding question is whether the daemons that read from `inbound[]` re-key by `from_node` or accept payload-side principal fields verbatim.
2. **The five reserved-but-unwired subprotocols (FORK_ANNOUNCE, CONTINUITY_PROOF, PARTITION, RECONCILE, REPLICA_GROUP)** should each have the `from_node`-binding pattern (R-20 / X-18 / S-1..3 fix shape) baked in at first-implementation time, not retrofit. Reference this sweep in the design docs for each.
3. **Sequential `AtomicU64` nonces** appear elsewhere — every nonce/correlation-id generator in the crate should be re-checked. If correlation depends on the value being unpredictable to other session peers, sequential is wrong.

---

## Seventh pass — sixth-pass follow-ups (2026-05-18)

Closes the two follow-up items the sixth pass surfaced. **One real high
(S-4 — nRPC response spoofing).** MeshDB's matching counter (the agent's
initial second hit) turned out to be already-mitigated by the transport
peer-check; downgraded to "verified clean." The queue-to-application
consumer audit returned an architectural concern rather than a specific
bug: the `StoredEvent` queue boundary drops `from_node`, so any future
consumer of CAUSAL/SNAPSHOT/NEGOTIATION/CONTINUITY events cannot bind
without retro-fitting the queue.

### High

#### S-4 — nRPC response delivery keyed on call_id alone; sequential counter lets any session peer spoof responses to in-flight calls — **Parts 1 + 2 landed; part 3 (reply-channel ACL) intentionally not pursued**
- **File:** `src/adapter/net/cortex/rpc.rs:2105-2106` (`RpcClientFold::apply` delivery path); `:1978-2070` (`RpcClientPending::deliver`); call-id generator at `src/adapter/net/mesh_rpc.rs:800` (and `:1095`); counter init at `src/adapter/net/mesh.rs:1733` (`rpc_next_call_id: Arc<AtomicU64>::new(1)`).
- **What:** Caller allocates `call_id = self.rpc_next_call_id().fetch_add(1, Ordering::Relaxed)` (predictable, process-global sequential u64 starting at 1). The caller subscribes to its own reply channel, ships the REQUEST to `target_node_id`, and registers a oneshot in `RpcClientPending::register(call_id)`. Inbound RESPONSE events on the reply channel are processed by `RpcClientFold::apply`: at `:2106` it pulls `meta.seq_or_ts` (the call_id) and calls `self.pending.deliver(call_id, resp)`. `RpcClientPending::deliver` (`:1978`) keys the `senders` `DashMap` on `call_id` alone — **no `from_node` parameter, no peer-binding check.** Any session peer that can publish on the caller's reply channel can ship a forged `RpcResponsePayload` with a guessed/sequential call_id and have it delivered to the caller's oneshot.
- **Trigger / Attack:** Attacker establishes a session, issues a self-targeted RPC (observes their own call_id `N`), then publishes spoofed RESPONSE frames on a victim's reply channel for `N+k` at small offsets. Spoofed responses with non-Ok status inject false errors into the victim's nRPC results; with Ok status + crafted body, inject false success values. Reach depends on the victim's reply-channel publish ACL — open channels (the default) admit any session peer; channels with `require_token` restrict to authorized publishers.
- **Same shape as S-3** in the rendezvous/membership layer, but **higher impact**: nRPC is a primary data-plane call mechanism. RPC-result corruption propagates straight into application state, unlike membership-Ack spoofing (which is a one-way denial primitive). Sequential `AtomicU64` makes call_ids cheaply predictable from any session peer.
- **Note on streaming:** The streaming-RPC path at `:1991-2050` uses the same key (`call_id`); spoofed frames during a streaming call inject false chunks into the caller's stream.
- **Fix (part 1, landed):** Random call_ids. Both `Mesh::call` (unary, mesh_rpc.rs:1091) and `Mesh::call_streaming` (mesh_rpc.rs:800) now mint call_ids from a new `mint_random_call_id()` helper that pulls 8 bytes from `getrandom::fill`. Field `rpc_next_call_id: Arc<AtomicU64>` and its accessor deleted (round-robin cursor `rpc_round_robin_cursor` is intentionally kept — that one is local-only). This closes the **blind-spoof** attack described in the trigger: an attacker who observes a self-issued call_id can no longer predict a victim's next-allocated call_ids; spoofing now requires guessing a 64-bit value with 2^-64 probability per call.
- **Fix (part 2, landed):** Bind RESPONSE delivery to the wire-session peer. Added `from_node: NodeId` field to `RpcInboundEvent` (the type the registered dispatcher receives). The mesh dispatch site at `mesh.rs:4045+` resolves the session peer's NodeId via `addr_to_node` → peers fast path with a session_id-scan fallback (sentinel `0` when no peer maps). `RpcClientPending`'s `senders` map switched from `DashMap<u64, PendingEntry>` to `DashMap<u64, (NodeId, PendingEntry)>` — every `register` / `register_streaming` now records the `target_node` the request was dispatched to. `deliver(call_id, from_node, resp)` rejects with a trace log when the recorded target is non-zero and doesn't match `from_node`. A new `RpcClientFold::apply_inbound(&RpcInboundEvent)` method is the production path; the legacy `RedexFold::apply` shim delivers with `from_node=0` for loopback test paths (callers that registered with `target_node=0` accept the loopback; callers with a real target reject it). The reply-channel dispatcher in `mesh_rpc.rs::ensure_reply_subscription` switched to `apply_inbound` so the AEAD-verified peer flows through. Regression test `client_pending_drops_response_from_wrong_target` registers a call with `target_node=0x42`, fires a forged RESPONSE with `from_node=0x99` and asserts the waiter stays parked; then fires a legitimate RESPONSE with `from_node=0x42` and asserts it resolves.
- **Fix (part 3, not pursued):** Tightening the reply-channel publish ACL to `{target_node_id}`-only would break the existing multi-target sharing pattern — `ensure_reply_subscription`'s comment at `mesh_rpc.rs:1368-1372` explicitly relies on multiple targets sharing one reply channel + one dispatcher per `(self_origin, service)`. With parts 1 (random 64-bit call_ids) and 2 (per-call target-node binding) landed, the residual attack surface is "attacker who has observed both a victim's call_id AND is sending from the recorded target's session" — i.e. the target itself is compromised, in which case the trust boundary has already failed. Part 3 was therefore not pursued.

### Verified clean

- **`FEDERATED_CALL_ID_COUNTER` (MeshDB federated executor)** at `src/adapter/net/behavior/meshdb/federated.rs:123` — the sequential counter looks identical to the S-3 / S-4 shape, but the transport layer at `src/adapter/net/behavior/meshdb/transport.rs:319-329` verifies `entry.target_node == from_node` *before* delivering the response to the caller's stream. Spoofed responses with a guessed call_id are rejected with `MeshDbRouteError::WrongPeer`. The sequential counter is safe here because correlation is `(call_id, peer)` at the demux, not call_id alone.
- **`next_waiter_gen` (NAT-traversal pending-map race-prevention)** at `src/adapter/net/mesh.rs:1761` — generation stamp on `pending_punch_acks` / `pending_reflex_probes` / `pending_punch_introduces` entries. **Local-only**; never sent on the wire, never used for cross-peer correlation. Sequential is correct.
- **`LocalMeshQueryExecutor::next_id`** at `src/adapter/net/behavior/meshdb/executor.rs:206` — local query-handle id for cancellation tracking. Never crosses a process boundary. Sequential is fine.
- **`rpc_round_robin_cursor`** at `src/adapter/net/mesh.rs:1735` — load-balancing cursor for `RoutingPolicy::RoundRobin` / `Random`. Selects the next target; the value never goes on the wire. Sequential is fine.

### Architectural concern (carry forward)

#### A-CON-1 — `StoredEvent` queue boundary discards `from_node`; future CAUSAL / SNAPSHOT / NEGOTIATION / CONTINUITY consumers cannot bind
- **File:** event-frame fallthrough at `src/adapter/net/mesh.rs:3857-3897`; `StoredEvent` definition (carries `id`, `raw_payload`, `insertion_ts`, `shard_id`, `dedup_id` — no `from_node`); poll-shard consumer surface at `mesh.rs:~6185-6220`.
- **What:** Events for the four queue-to-application subprotocols (CAUSAL 0x0400, SNAPSHOT 0x0401, NEGOTIATION 0x0600, CONTINUITY 0x0700) are queued into `inbound[shard_id]` as `StoredEvent`s without the cryptographic session peer. A future daemon consumer that drains the queue cannot tell which peer sent each event, and therefore cannot bind any payload-side principal field (`origin_hash`, `entity_id`, fork-origin claim, snapshot-target claim) against `from_node`. The R-20 / X-18 / S-1..3 fix shape (verify `from_node == claimed_principal`) is *structurally unavailable* at the consumer until the queue boundary is updated.
- **Impact:** Latent. There are no production consumers of these four subprotocols today — `CAUSAL` and `SNAPSHOT` queue ingestion is unowned, `NEGOTIATION` has only the local-side `negotiate()` API, and `CONTINUITY` data structures exist without an inbound handler. **No exploit today**, but every future implementation will either (a) skip the binding check (recreating the S-3/S-4 class of bug), (b) carry the binding check at the cost of routing `from_node` through the queue (architectural fix), or (c) demux from a separate non-queue path.
- **Recommendation:**
  1. Either extend `StoredEvent` (or its delivery envelope) with `from_node`, or
  2. Route these four subprotocols through `mesh.rs` dispatch arms (like REDEX/MIGRATION/REFLEX/RENDEZVOUS already are) instead of the standard event-frame fallthrough.
  Pick before the first consumer of any of the four lands.
- **Tracking:** This is not an `S-*` bug — there is no extant code path that mutates state on a forgeable principal — but it shapes how the next implementer must approach the four subprotocols. Cross-reference from the design docs for each.

### Sixth-pass follow-up status

- **Queue-to-application consumer audit (CAUSAL/SNAPSHOT/NEGOTIATION/CONTINUITY)** — Complete. No exploit bugs (no consumers exist); architectural concern documented as **A-CON-1**.
- **Sequential-nonce pattern reuse** — Complete. One real bug (**S-4** nRPC); MeshDB's matching counter verified clean (transport binds peer); three other sequential counters (`next_waiter_gen`, `next_id`, `rpc_round_robin_cursor`) verified local-only and safe.

---

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

## Fifth-pass additions

Findings from the 2026-05-18 night pass. IDs continue the existing
per-module sequences (D-16 → D-17, X-18 → X-19, O-20 → O-21, R-39 → R-40).

### High

#### D-17 — Heat emission marks `last_emitted` before async sink confirms; one transient error permanently strands updates — **Landed**
- **File:** `src/adapter/net/dataforts/gravity/counter.rs:291-322` (`HeatRegistry::tick`) and `:441-471` (`BlobHeatRegistry::tick`); call sites at `src/adapter/net/dataforts/blob/mesh.rs:596-613` (`tick_blob_heat`) and `src/adapter/net/dataforts/greedy/runtime.rs:625-667` (chain heat).
- **What:** `tick()` calls `counter.record_emission(rate)` at `counter.rs:303` (and `:458`) *inside* the registry mutex, before returning the emissions list. The caller then awaits `sink.announce_blob_heat_batch(...)` / `sink.announce_heat_batch(...)`; on error `?` propagates but `last_emitted` has already advanced. The next tick's `should_emit_heat(rate, last_emitted ≈ rate, policy)` returns `Suppress` and the rate change is never re-attempted.
- **Impact:** A single `AdapterError` (peer offline mid-tick, RPC blip, queue-full at the sink) silences that chain or blob's heat advertisement indefinitely — `Suppress` until rate decays to zero, at which point Withdraw eventually fires but every intermediate update is lost. The gravity / migration loop downstream stops migrating hot blobs to local nodes that would have qualified; no operator-visible counter surfaces the regression. Same shape as **R-31** (replication state advance before async sink completes) but in the dataforts/gravity subsystem.
- **Fix:** Took the candidate/commit split branch of the fix sketch. `HeatRegistry::tick` and `BlobHeatRegistry::tick` now return the candidate emissions list **without** mutating `last_emitted` or pruning quiescent entries. Callers invoke a new `commit_emissions(&[(K, HeatEmission)])` method only after the async sink confirms `Ok(())`. Mutation paths updated:
  - `tick_blob_heat` (`blob/mesh.rs`): commit runs after `announce_blob_heat_batch` returns Ok; on `?` error path, no commit, next tick reissues.
  - `emit_heat` (`greedy/runtime.rs`): commit runs after `announce_heat_batch` returns Ok; on error path, candidates stay pending. Empty-batch (all Suppress) commits the no-op so pruning still happens.
- **Regression test:** `tick_without_commit_reissues_on_next_tick` simulates a sink failure (caller skips commit), then ticks again and asserts the same `Emit` candidate is reissued — pre-fix the inline `record_emission` had already advanced `last_emitted` and the second tick returned empty (Suppress).

#### X-19 — `StandbyGroup::promote` double-executes events after a partial `sync_standbys` — **Landed**
- **File:** `src/adapter/net/compute/standby_group.rs:230-298` (`sync_standbys`), `:305-381` (`promote`), and the v2 path at `:585-682`.
- **What:** When `sync_standbys` partially fails, succeeded standbys have `synced_through` advanced to the snapshot's `through_seq` but `buffered_since_sync` is intentionally preserved (line 286-290) so the failed standby can catch up later. On a subsequent `promote`, the code picks the highest-`synced_through` standby (a succeeded one) and replays the *entire* `buffered_since_sync` vec onto it via `registry.deliver` — but those events have already been applied via the snapshot.
- **Impact:** Silent state corruption on the promoted daemon — counters double, idempotency keys re-issued, side effects re-fired. Output events re-emitted into the causal chain with new sequence numbers; observers see duplicates. Distinct from **X-1** (partition-heal fencing / split-brain across the *active* member); X-19 is double-execution *within* the promotion path under a partial-sync precondition. Reachable in production any time a partial standby outage is followed by an active-node failure.
- **Fix:** The replay loop in both `promote` (v1) and `promote_with_placement` (v2) now filters by `event.link.sequence > synced_through` before calling `registry.deliver`. The new active's snapshot already covers events up to its `synced_through`; only strictly-greater sequences need to be replayed onto the post-restore state. Regression test `promote_does_not_double_apply_events_within_synced_range` drives the documented partial-sync setup (standby 1 succeeds with `synced_through=10`, standby 2's restore fails, buffer events 6..=10 retained), promotes, and asserts `StatefulDaemon::value == 10` (snapshot value) rather than the pre-fix 15 (snapshot + replayed 6..=10).

#### O-21 — Cluster-backpressure release never fires while the executor is idle
- **File:** `src/adapter/net/behavior/meshos/executor.rs:357-400` (only non-test call site at `:389`, inside `handle_one_retry`); state at `behavior/meshos/backpressure.rs:247` (`update_cluster_backpressure`).
- **What:** `update_cluster_backpressure` is only invoked when the executor is processing an action. If the queue drains below the low-water mark while no new action arrives, the `Released` edge never surfaces and `DaemonControl::BackpressureOff` never fans out. Verified: grep across `src` confirms `:389` is the single non-test call site.
- **Impact:** Daemons stay throttled (cache warmup paused, background indexing off, retry budgets withheld) indefinitely after a burst clears — the very condition `BackpressureOff` exists to relieve. A quiet steady-state cluster is exactly when daemons should be running their optional work; recovery requires *any* fresh action to arrive on the executor queue. Operator visible only via daemon-side gauges; the meshos loop itself shows "queue drained" while daemons remain in suppression.
- **Fix sketch:** Drive `update_cluster_backpressure(actions_rx.len() + deferred.len(), …)` from a periodic tick (e.g. the snapshot publish loop, or a dedicated `tokio::time::interval`) so the release edge fires with zero in-flight actions. Tie release into the same tick that already runs `gc_freeze` to avoid adding new timer overhead. Fixing this also subsumes **O-25**.

### Medium

#### D-18 — `FileSystemAdapter::sanitize_uri_for_error` byte-slices a UTF-8 URI; publisher-crafted multi-byte char straddling byte 256 panics inside `spawn_blocking`
- **File:** `src/adapter/net/dataforts/blob/fs.rs:117-121`.
- **What:** `let trimmed = if uri.len() > MAX_LEN { &uri[..MAX_LEN] } else { uri };` with `MAX_LEN = 256`. The URI passes `from_utf8` on decode but a multi-byte UTF-8 codepoint can straddle byte 256, so the slice indexes mid-char and panics.
- **Trigger:** Any publisher whose channel resolves to an FS adapter ships a `BlobRef` whose URI is 255 ASCII bytes + a 3-byte emoji (or any 2-4-byte UTF-8 sequence at the boundary). Any error path through `sanitize_uri_for_error` — fetch on missing, range out-of-bounds, refcount mismatch — panics inside `spawn_blocking`; the task crashes and the caller observes a `JoinError`. Repeatable DoS via crafted `BlobRef`.
- **Fix sketch:** `uri.char_indices().take_while(|(i, _)| *i < MAX_LEN).last().map_or("", |(end, c)| &uri[..end + c.len_utf8()])`, or `uri.get(..MAX_LEN).unwrap_or(uri)` (returns `None` on a mid-codepoint cut and falls back to the full URI). Add a unit test with `format!("{}{}", "a".repeat(255), "🦀")`.

#### O-22 — `MaintenanceState::is_valid_successor` rank ladder admits `ExitingMaintenance → DrainFailed` regression
- **File:** `src/adapter/net/behavior/meshos/maintenance.rs:104-113`; consumer at `behavior/meshos/state.rs:294`.
- **What:** Both `ExitingMaintenance` and `DrainFailed` resolve to `rank() == 3`, so the rank-based "successor must be ≥" check accepts the backward arc.
- **Trigger:** Operator issues `ExitMaintenance { force: true }` after a stuck drain; a delayed `MaintenanceTransitionObserved(DrainFailed)` arrives seconds later (chain replay, redundant source) and regresses local state to `DrainFailed`. The node refuses fresh placements and the operator's manual override silently undoes itself.
- **Fix sketch:** Replace the rank ladder with an explicit match-table of allowed transitions (e.g. `(ExitingMaintenance, DrainFailed) => false`, `(DrainFailed, ExitingMaintenance) => true`). The ladder is the wrong shape for non-totally-ordered transitions.

#### O-23 — Executor mixes `std::time::Instant` with `tokio::time::sleep_until`
- **File:** `src/adapter/net/behavior/meshos/executor.rs:379-380, 423, 508-513, 603-605`.
- **What:** `DeferredEntry.retry_at` is `std::time::Instant`; the sleep at `sleep_until_opt` calls `tokio::time::Instant::from_std(deadline)`. Same M-4 anti-pattern fixed in `bus.rs` and flagged for replication at **R-35** — different file, same shape.
- **Impact:** Tests using `tokio::time::pause()` (e.g. for `pull_cooldown`) cannot exercise deferred-retry semantics; under heavy load `std::time::Instant` drifts from tokio's monotonic and the deferred-heap deadline diverges from the timer wakeup.
- **Fix sketch:** Swap `std::time::Instant` → `tokio::time::Instant` throughout `BackpressureState` (admit timestamps, deferred-heap entries, daemon gate timestamps). All consumers of the field are already tokio-side.

#### O-24 — `MeshOsDaemonHandle::graceful_shutdown` unconditionally sleeps the full grace
- **File:** `src/adapter/net/behavior/meshos/sdk.rs:509-525`.
- **What:** Doc comment claims "park for `grace` (or until the daemon's task exits — whichever sooner)"; body is `tokio::time::sleep(grace).await; self.unregister_inner();` with no early-exit signal. The handle is consumed by value so a sibling drop cannot shorten the wait.
- **Impact:** Operators issuing a 30 s drain wait the full 30 s on every clean exit — multiplies shutdown latency across a fleet during rolling restarts. A clean exit looks identical to a hung daemon from the operator's vantage.
- **Fix sketch:** Hold a `oneshot::Receiver` plumbed from `DaemonHost`'s task completion (or have the registry expose `wait_for_unregister(origin) -> oneshot::Receiver<()>`). `tokio::select!` between that receiver and the `sleep(grace)` so an early exit returns immediately.

#### X-20 — `MigrationOrchestrator::buffer_event` is plumbed but never used in production
- **File:** `src/adapter/net/compute/orchestrator.rs:1607-1619` (`buffer_event`); state field at `MigrationState::buffered_events`; consumer at `:1416` (`on_restore_complete`).
- **What:** `MigrationOrchestrator::buffer_event` writes into `record.state.buffered_events`, and `on_restore_complete` drains them via `take_buffered_events()` and ships them to the target. A grep across `src` finds no production caller — only tests. The actual buffering goes through `source_handler.buffer_event` and `on_cutover`'s drain.
- **Impact:** Dead API trip-wire on a hot orchestration surface. An SDK consumer reading the public API and wiring `orchestrator.buffer_event` expecting symmetry with `source_handler.buffer_event` would have their events delivered through the migration pipeline a second time — once via `on_restore_complete` (pre-Cutover) and again via the source-handler's `on_cutover` drain. Compounds **X-2**'s duplicate-delivery risk.
- **Fix sketch:** Delete `MigrationOrchestrator::buffer_event` and `MigrationState::{buffer_event, take_buffered_events}`; have `on_restore_complete` ship an empty `BufferedEvents` placeholder. Alternatively, redirect `MigrationOrchestrator::buffer_event` to `self.source_handler.buffer_event` so there is one buffer of record. The dead-code option is preferable — the trip-wire is the bug.

### Low

#### R-40 — `SyncNack::BadRange` retry can thrash without making progress — **Landed**
- **File:** `src/adapter/net/redex/replication_runtime.rs:948` (NACK handler); contrast **R-23** (NACK trust) and **R-28** (catchup busy-loop on phantom tail).
- **What:** On `BadRange`, the runtime calls `inputs.file.skip_to(msg.since_seq.saturating_add(1))`. `since_seq` is the *replica's* asked-for seq, not the leader's first-retained seq. If the leader's retention floor is many seqs above `since_seq`, the next `SyncRequest` is still below the floor and NACKs again. The replica's `skip_to` advances by one per round-trip.
- **Impact:** A replica that fell below retention pins the leader's reject loop until the heartbeat-cycle `SyncResponse` carries `first_seq > local_next` and the `GapBeforeChunk` path catches up. Burns leader CPU + replication-bandwidth budget for no progress; produces high NACK-rate noise on operator dashboards.
- **Fix:** Added `leader_first_retained_seq: u64` field to `SyncNack` (wire layout: header + channel_id(32) + since_seq(8) + error_code(1) + leader_first_retained_seq(8) + detail_len(2) + detail). The leader populates the field from `file.lowest_retained_seq()` in `handle_sync_request` (catchup module's `SyncRequestOutcome::Nack`) and the runtime forwards it verbatim. The replica's `BadRange` handler now calls `skip_to(leader_first_retained_seq)` directly when non-zero, falling back to `since_seq + 1` if the leader sent `0` (channel that never retained data). Regression test in `replication_runtime.rs` asserts `skip_to(100)` lands when `leader_first_retained_seq = 100`, not the pre-fix one-per-round-trip `since_seq + 1 = 43`.

#### O-25 — `release_failed_admit` does not refresh cluster backpressure
- **File:** `src/adapter/net/behavior/meshos/backpressure.rs:210-233`; paired with **O-21** (no idle-tick refresh) and **O-13** (sibling chain-stabilization clear).
- **What:** A failed dispatch rolls back the per-chain reservation but leaves `cluster_backpressure` asserted. Effective load dropped; daemons stay throttled.
- **Fix sketch:** Call `update_cluster_backpressure(self.queue_depth.saturating_sub(1), …)` from `release_failed_admit`, or have the executor re-evaluate after the release. Fixing O-21 with a periodic tick subsumes this; if O-21 is deferred, this stands alone.

#### X-21 — `Scheduler::place` LocalPreferred fast-path skips liveness / drained check
- **File:** `src/adapter/net/compute/scheduler.rs:101-129` (`place`); sibling **X-16** at `:317-341` (`select_migration_target`).
- **What:** The fast-path `if self.can_run_locally(filter)` only checks `CapabilityFilter::matches(&self.local_caps)`; no liveness, saturation, or drained-state signal. A node mid-shutdown or operator-drained continues to get new daemon placements until the process dies. Same fast-path shape as X-16; both miss the same gate.
- **Fix sketch:** Gate the LocalPreferred fast-path on a `local_drained.load(Acquire) == false`, or feed local through the same `PlacementFilter::placement_score` machinery and accept the fast-path only when local scores non-`None` and above a floor. Fix together with X-16 for symmetry.

#### X-22 — `on_replay_complete` falls back to synthetic `parent_hash: 0` when the orchestrator is third-party
- **File:** `src/adapter/net/compute/orchestrator.rs:1458-1496`.
- **What:** The "real head link" lookup goes through `self.daemon_registry.with_host(daemon_origin, …)` — the *orchestrator's* registry, not the target's. When the orchestrator is neither source nor target (the documented third-party-coordinator topology in the module header), the lookup returns `Err`, and `target_head` is synthesized with `parent_hash: 0`. The `warn!` acknowledges the fallback.
- **Impact:** Every migration in a coordinator-as-third-party deployment ships a `SuperpositionState::target_head` whose anchor no downstream verifier can reconcile against the real chain. The continuity-proof feature is unusable for the topology its docs describe.
- **Fix sketch:** Have the target ship its real `head_link` inside `ReplayComplete`'s payload (the target has the live host) so `on_replay_complete` doesn't need a local lookup. Or drop the synthetic-fallback branch and propagate the error so callers explicitly handle the topology mismatch rather than carrying a known-wrong anchor.

---

## Suggested action order

0. **A-5** (capability strip vs. forward order) — regression in the currently-staged L-13 fix on `bugfixes-15`. Block the commit until the strip moves below the forward block and a multi-hop signed-propagation regression test exists. Cheap fix; expensive miss.
1. **R-20** (peer auth) + **R-21** (dual-leader FSM) — the replication subprotocol can be hijacked or wedged by any mesh peer. Wire-protocol change; do them together with a single coordinated rollout.
2. **X-1** (StandbyGroup fencing) + **X-18** (migration-subprotocol peer auth) — same class of bug as R-20/R-21 in two different layers. X-18 is the migration-subprotocol analogue of R-20; the orchestrator-binding check is mechanical (record at `TakeSnapshot`, verify on every later arm) and lands in `subprotocol/migration_handler.rs`. X-1's fix (epoch / generation token) is a primitive R-21 and X-18 can both share.
3. **R-22, R-23, R-24** (replication durability + NACK trust + partial-append accounting) — bundle with the R-20/R-21 wire change.
4. **X-9** (`pending_events` unbounded) — wire-reachable OOM on every node accepting migration traffic; ~10-line fix.
5. **D-11** (`BlobRef::Manifest` chunk-size validation) — wire-reachable slice panic on untrusted input; mechanical fix in the decoder plus a defensive `get(..)` in the consumer.
5a. **D-14** (`resolve_payload` always fails on `BlobRef::Manifest`) — end-to-end-broken FFI surface for any payload >4 MiB; one-line branch on `is_chunked()` plus a chunked-path round-trip test. Highest user-visibility item in the third pass.
6. **D-1** (sweep TOCTOU) — quiet data-loss bug; trivial fix via `remove_if`.
7. **D-2** (32-bit `usize::MAX` guard on `MeshBlobAdapter::fetch_range`) — mechanical, mirrors existing `fs.rs` pattern.
8. **R-25** (priority lane) and **R-28** (catchup backoff) — replication availability hardening.
9. **X-2, X-3** (migration phase guards) — close `pub` API misuse paths.
10. **X-10** (orchestrator `seq_through` validation) + **X-11** (source buffered-events cap) — migration correctness/availability.
11. **R-31** (coordinator state vs. sink decoupling) + **R-32** (metrics TOCTOU) — replication self-consistency.
12. **O-1, O-2** (epoch_id collision + node_id hardcode) — SDK correctness; small but real consumer-facing bugs.
13. **O-3, O-7, O-8** (tick starvation, phantom emissions, atomic-pair publishing) — meshos observability + reconcile correctness.
14. **D-3, D-4, D-5** (blob hardening) and **X-4, X-5** (group lifecycle, panic-across-FFI).
15. **O-4, O-5** (audit-chain durability ordering) — pick one source of truth; document loudly.
16. **R-26, R-27** (dead tip_seq, budget refund) and the remaining lows can batch into a single cleanup commit. Fold **R-33** (replication wire 32-bit add) into the same commit as the umbrella's L-9 fix; **R-34** (catchup saturating-add symmetry) lands alongside.
17. **Third-pass mediums** — **D-15** (GC segment-file unlink), **D-16** (Small `fetch_range` whole-chunk alloc), **R-35** (wall-clock `Instant` in tokio-tick'd loop, same M-4 anti-pattern), **X-13** (failed placements never retried on different-node recovery). Each is independent of the wire-protocol changes above and can land out of order.
18. **Third-pass lows** — **R-36..R-39** (is_stopped race, Drop mutex, Leader disk-pressure signal label, believed_leader tiebreak), **X-14..X-17** (`scale_to` guard, dead `target_head`, scheduler vs `PlacementFilter`, multi-chunk validation asymmetry). Batch into the same cleanup commit as the other lows.
19. **Fourth-pass mediums** — **MD-1** (federated `drain_rows` unbounded → aggregator OOM) + **O-20** (sequence-counter saturating-add). Each is an isolated change. MD-1 is the user-visible one (peer-controllable response can OOM the local aggregator); add a planner-driven byte budget. O-20 is a two-line trivial saturating swap matching sibling counters.
20. **Fourth-pass lows + latent** — **MD-2** (window saturating-add near `u64::MAX`) and the four edges under "Fourth-pass edges (not promoted)" batch into a single defensive commit. **MD-3** (unsigned `ContinuationToken`) is a design issue to fix *during* Phase B-4 implementation rather than after; reference this entry from the Phase B-4 plan.
21. **Fifth-pass highs** — **X-19** (`StandbyGroup::promote` partial-sync double-execution) lands alongside the **X-1** fencing fix; both are silent-corruption bugs in the same file and the regression test for X-1's epoch token should also cover the X-19 replay-filter. **O-21** (cluster-backpressure release never fires while idle) is independent and small — drive the existing snapshot publish tick to also call `update_cluster_backpressure`. **D-17** (heat-emission ordering vs. async sink) is the dataforts analogue of **R-31**; fix both in the same commit by introducing a "candidate / commit" pattern shared between the heat registry and the replication coordinator.
22. **Fifth-pass mediums** — **D-18** (FS adapter UTF-8 boundary panic) is publisher-controllable and trivial to fix; ship a same-day patch with the `char_indices` fix and the `format!("{}🦀", "a".repeat(255))` unit test. **O-22** (maintenance rank-ladder regression), **O-23** (executor `std::Instant` vs tokio sleep — fold into the same M-4 commit as R-35 and the original bus.rs fix), **O-24** (graceful_shutdown unconditional sleep), and **X-20** (dead `MigrationOrchestrator::buffer_event`) are independent; X-20 should be deletion not redirection.
23. **Fifth-pass lows** — **R-40** (NACK BadRange thrash) folds into the same wire-protocol change as R-22/R-23. **O-25** (release backpressure refresh) and **X-21** (LocalPreferred liveness) pair with O-21 and X-16 respectively. **X-22** (third-party orchestrator `parent_hash: 0`) is the only stand-alone fifth-pass low; it breaks the continuity-proof feature in exactly the topology its docs describe, so worth lifting to medium if any deployment uses that topology today.
24. **Sixth-pass subprotocol-binding fixes** — **S-1** (`PunchIntroduce` coordinator binding) + **S-2** (`PunchAck` from-peer binding) land together in `mesh.rs` rendezvous dispatch; both are mechanical (record the expected peer at registration time, verify on dispatch). **S-3** (membership `Ack` peer binding + random-nonce replacement) is independent; the nonce change is a one-line `rand::random()` swap and the peer-binding tuple change is local to `pending_membership_acks`. All three fixes are pure-local and do not require a wire-protocol change.
25. **Seventh-pass highs** — **S-4** (nRPC response delivery keyed on call_id alone; sequential `rpc_next_call_id` makes spoofing cheap). Highest-leverage of the S-series because nRPC is a primary data-plane mechanism. Fix in three parts: (a) `rand::random::<u64>()` for `rpc_next_call_id`; (b) store `(call_id, expected_target_node)` in `RpcClientPending` and verify `from_node` on `deliver` (mirror `behavior/meshdb/transport.rs:323`); (c) tighten the default reply channel publish ACL to `{target_node_id}`-only. Land together with a regression test that publishes a forged RESPONSE on a known reply channel and asserts the caller doesn't observe the forged value.
26. **Seventh-pass architectural** — **A-CON-1** (`StoredEvent` queue boundary discards `from_node`) is not an exploit today (no production consumers of the four affected subprotocols) but blocks correct first-implementation of CAUSAL / SNAPSHOT / NEGOTIATION / CONTINUITY. Decide between (a) carrying `from_node` through the queue, or (b) routing the four through `mesh.rs` dispatch arms like the other subprotocols. Pick before the first consumer of any of the four lands.

## Coverage gaps still carried forward

- **Phase 2** (Miri / ASan / TSan / fuzz) — still skipped; existing `fuzz/fuzz_targets/` is wired.
- **Cross-language conformance (Phase 4)** — Rust/TS/Py/Go SDK round-trip property tests not started.
- **Dep audit** — `cargo-audit` / `cargo-machete` / `cargo-deny` / `cargo-udeps` not installed.
- **Adjacent surfaces not reviewed this round:** `src/adapter/net/contested/`, `src/adapter/net/continuity/`, `src/adapter/net/cortex/` (re-review post-fixes), `src/adapter/net/identity/`, `src/adapter/net/subnet/`, `src/adapter/net/state/`, `src/adapter/net/traversal/`. Each is a candidate for a follow-up. **The subprotocol-binding sweep recommended on the prior pass is now complete** — see the "Sixth pass — targeted subprotocol sweep" section. Three new bugs (S-1, S-2, S-3) plus two follow-up audit areas (queue-to-application consumers; sequential-nonce pattern reuse) came out of it.
- **`src/adapter/net/behavior/meshdb/`** is now partially covered (MD-1, MD-2, MD-3). The planner / federated executor / cache layer received targeted reads; full module sweep — including `executor.rs` plan execution, `transport.rs` framing, `row.rs` predicate walking, and the `query.rs` request/response surface — is still owed.

---

## Fix status (post-audit)

Substantial fix sprint on `bugfixes-15` — 29 commits, ~60 of ~70 findings
landed. Heavy wire-protocol bundles and architectural-decision items
deferred to dedicated follow-up sessions, each documented with the
options considered and the recommended path.

### Fixed (✅)

| ID | Title | Commit |
|---|---|---|
| A-5 | Move capability strip below the forward block | `7428ab5d` |
| D-1 | Close GC-sweep TOCTOU on the blob refcount table | `93448488` |
| D-2 | Guard MeshBlobAdapter::fetch_range against u64→usize truncation | `b27f37cd` |
| D-4 | Floor overflow disk gate at chunk size | `c19c7a57` |
| D-5 | Cap fetch Manifest at 256 MiB | `c19c7a57` |
| D-6 | store_stream cap at BLOB_REF_MAX_SIZE | `5474b551` |
| D-7 | NaN-safe disk ratio in blob metrics | `5474b551` |
| D-8 | parse_blob_heat_tag rejects mixed-case hex | `5474b551` |
| D-9 | Verified clean (lock ordering already consistent) | `fde332ed` |
| D-10 | BlobAdapterRegistry::drain — clear path for the global singleton | `fde332ed` |
| D-11 | Validate BlobRef::Manifest chunk sizes match the substrate stride | `e7d39de7` |
| D-12 | GreedyCacheRegistry::next_lru_pos saturation guard | `5474b551` |
| D-13 | HeatRegistry::tick prune uses `<= 0.0` for f64 robustness | `5474b551` |
| D-14 | Skip top-level verify for Manifest in resolve_payload | `0bee736d` |
| D-15 | Unlink the persistent segment dir on blob GC sweep | `e91d3207` |
| D-18 | Slice URI sanitizer on a char boundary in fs blob adapter | `2a64931e` |
| MD-1 | Bound federated drain_rows | `2e6d93e0` |
| MD-2 | Reject saturating window sizes | `2e6d93e0` |
| O-1 | Random epoch_id via getrandom | `0dd50356` / `0fab8284` |
| O-2 | Plumb this_node into the SDK metadata view | `0dd50356` |
| O-3 | Biased select in MeshOsLoop::run (tick first) | `f945180d` |
| O-7 | Push to recent_emissions only on try_send success | `f945180d` |
| O-8 | Atomic ReplicaBecameHolderAndLeader pair | `f945180d` |
| O-9 | Verified not-a-bug — 1-second window matches `drain_rate_per_zone_per_sec` semantic; see deferred section for documentation note | (no commit) |
| O-17 | Cap verify_bundle signature count | `3075f97a` |
| O-19 | Clamp BufferingActionChainAppender capacity at 1 | `ca1e744a` |
| O-20 | Saturating admin_audit_seq / log_seq | `ca1e744a` |
| O-21 | Drive cluster-backpressure release on idle | `b6f8566b` |
| O-22 | Tighten maintenance is_valid_successor (match-table) | `35105495` |
| O-23 | Tokio-clock executor (std::Instant → tokio's via into_std) | `35105495` |
| O-24 | Early-exit graceful_shutdown via registry poll | `35105495` |
| O-25 | release_failed_admit refreshes cluster backpressure | `b6f8566b` |
| R-26 | Wire record_tail_seq into runtime apply + tick | `bc7837b3` |
| R-27 | Refund BandwidthBudget on send failure | `bc7837b3` |
| R-29 | Dedupe replica_set iteration at heartbeat emit | `bc7837b3` |
| R-32 | Close ReplicationMetrics::for_channel TOCTOU on cap probe | `72d020ab` |
| R-33 | replication wire decode checked_add | `bc7837b3` |
| R-34 | catchup acc saturating_add for symmetry | `bc7837b3` |
| R-35 | tokio::time::Instant inside virtualized runtime loop | `bc7837b3` |
| R-36 | Only JoinHandle holder flips `stopped` | `bc7837b3` |
| R-37 | Drop uses try_lock for deadlock safety | `bc7837b3` |
| R-38 | LeaderDiskPressureWithdraw / CandidateDiskPressureWithdraw signals | `bc7837b3` |
| R-39 | HeartbeatTracker keeps lex-smallest Leader claimant | `bc7837b3` |
| X-2 | replay_events phase guard | `05f7a2f6` |
| X-3 | MigrationSourceHandler::cleanup phase guard | `05f7a2f6` |
| X-4 | Fire Unregistered on registry.replace | `0fab8284` |
| X-5 | DaemonHost::from_fork returns Result instead of panic | `0fab8284` |
| X-6 | Hoist migration subprotocol tag string | `848ef03c` |
| X-7 | Drop dead old_origin_hash lookup with .unwrap() | `848ef03c` |
| X-8 | Reorder reassembler sweep after structural validation | `848ef03c` |
| X-9 | Bound MigrationTargetHandler::pending_events (64 MiB / 1M events) | `d8e7bd4f` |
| X-10 | Validate SnapshotReady seq_through against snapshot.through_seq | `640f160c` |
| X-11 | Cap source buffered events (64 MiB / 1M events) | `640f160c` |
| X-12 | Saturating elapsed_ms in list_migrations | `848ef03c` |
| X-14 | let-else+break guard in ReplicaGroup::scale_to | `ca6b21a1` |
| X-15 | Delete dead TargetMigrationState.target_head field | `ca6b21a1` |
| X-16 | Score local through PlacementFilter before LocalPreferred | `0bcc58d1` |
| X-22 | on_replay_complete returns StateFailed instead of synthesizing parent_hash:0 | `848ef03c` |
| R-31 | Coordinator divergence counter on `* → Idle` sink failure | `d61ee36a` |
| D-3 | FS adapter threat-model docs | `d61ee36a` |
| O-9 | gc_drain_window 1-second hardcode (commented, not a bug) | `d61ee36a` |
| X-21 | `Scheduler::place_with_locality(filter, drained: bool)` | `951085e2` |
| O-4 | Executor chain_append_failures counter | `3210a9d4` |
| X-17 | `validate_chunk_header` on every SnapshotReady chunk 0 | `96d5fba4` |
| X-20 | Delete dead `MigrationOrchestrator::buffer_event` surface | `d271e15c` |
| O-5 | Ring-first + `chain_pending` flag on AdminAuditRecord / LogRecord | `a88e46b4` |
| X-13 (partial) | `UnhealthySlotRecovery` trait + StandbyGroup impl (ForkGroup, ReplicaGroup, meshos tick deferred) | `a148dcaa` |

**Total: 66 fixes, 35 commits** (58 original + 8 locked-decision-driven landings).
Every commit is independently reviewable, isolated to one logical change
(some bundle two or three related lows), and includes regression tests
where a unit-test-scale assertion was meaningful.

### Deferred — wire-protocol bundles

Each needs a dedicated focused session — wire-format changes plus
coordinated rollout planning:

| Bundle | Components | Why bundled |
|---|---|---|
| Replication peer auth | R-20, R-21, R-22, R-23, R-24, R-40 | All touch the on_inbound dispatch + SyncRequest/Response/Nack wire shape. R-22's heartbeat split (`durable_seq` / `applied_seq`), R-23's request token, R-40's `leader_first_retained_seq` field, and R-21's `PeerLeaderObserved` signal all want to land in one coordinated wire-version bump. R-20's `from_node` binding is pure addition but the test surface overlaps. |
| StandbyGroup fencing | X-1, X-19 | X-1 introduces `term: u64` plumbed through DaemonHost + event routing; X-19's partial-sync replay filter lives in the same StandbyGroup::promote path and the regression test for X-1's epoch token should also cover X-19's replay-cursor. |
| Migration dispatch binding | X-18 | Mechanical (record orchestrator_node at TakeSnapshot, verify on every later arm) but needs full regression coverage across ActivateTarget / CleanupComplete / MigrationFailed / SnapshotReady. Standalone commit, but expects a real test for forged ActivateTarget from a non-orchestrator peer. |
| Rendezvous + membership binding | S-1, S-2, S-3 | All in `mesh.rs` rendezvous/membership dispatch. S-3 also swaps the membership-Ack nonce generator to random; that's an internal change with no wire impact. The three fixes share regression-test setup. |
| nRPC binding + random call_id | S-4 | Three sub-changes: random `rpc_next_call_id`, `(call_id, expected_target_node)` tuple in `RpcClientPending`, default reply channel ACL tightened. The peer-binding mirrors `behavior/meshdb/transport.rs:323` exactly. |
| Replication availability hardening | R-25, R-28 | R-25 priority lane (split inbox into high/low + biased select); R-28 catchup backoff (per-leader consecutive-empty counter). Both improve availability under flaky-link conditions; share the runtime hot path. |
| Heat emission ordering | D-17 | Refactor: split `HeatRegistry::tick` into candidate + commit; thread sink `Result` back through. Mirror in `BlobHeatRegistry`. Pure-internal but the test fixtures need updating. |
| Cleanup wire change | R-30 | Drop `wall_clock_ms` from SyncHeartbeat (currently collected but never used). Pure wire-shrink; coordinate with the R-20..R-24 wire-version bump. |

### Locked design decisions

These items needed a deliberate choice between competing approaches.
Decisions below are **locked** — implementation can proceed without
re-litigation. Each entry records the chosen path, the rejected
alternatives (so future readers can see why we didn't take them),
and a concrete implementation plan.

**Implementation status (commits on `bugfixes-15`):**

| ID | Status | Commit |
|---|---|---|
| R-31 | ✅ Implemented | `d61ee36a` |
| D-3 | ✅ Implemented (docs) | `d61ee36a` |
| X-13 | 🟡 Partial — trait + StandbyGroup impl landed; ForkGroup, ReplicaGroup, and meshos tick integration deferred | `a148dcaa` |
| X-17 | ✅ Implemented | `96d5fba4` |
| X-20 | ✅ Implemented | `d271e15c` |
| X-21 | ✅ Implemented | `951085e2` |
| O-4 + O-5 | ✅ Both implemented (O-4: `3210a9d4`, O-5: `a88e46b4`) | — |
| O-9 | ✅ Comment landed (no code change) | `d61ee36a` |

---

#### R-31 — Coordinator state rollback on `* → Idle` sink failure

**Decision: Option C — Document the divergence window + add a metric.**

The existing `tag_sink_failure_surfaces_but_state_mutated` test
deliberately pins the fail-fast-at-sink-boundary semantic. The
team chose that semantic; the operational visibility gap is the
real fix surface, not the code path.

**Rejected: A (state rollback).** Breaks the pinning test and
introduces a new failure mode — concurrent transitions during the
rollback window become observable. The `transition_lock`
serializes them, but the rollback's intermediate state is still a
new race surface.

**Rejected: B (background retry queue).** Breaks the "Idle doesn't
drive heartbeats" invariant. Adds a new lifecycle owner (the
retry task) that has to be tied to coordinator lifetime; the task's
stop semantics on Drop are non-trivial.

**Implementation plan:**

1. Add `coordinator_announce_divergence_total: AtomicU64` to
   `ReplicationMetricsAtomic` (sibling of `leader_changes_total`).
   Increment from the failing `* → Idle` sink-call branch in
   `transition_to`.
2. In the same branch, emit a `tracing::warn!` with:
   `daemon_origin`, `from_role`, `error`, and the literal text
   `"coordinator state advanced to Idle but sink withdraw failed; \
   advertised-vs-local divergence until next transition_to"`.
3. On every successful `transition_to(_, target_role)` where
   `from_role == Idle`, the upstream announce path naturally
   re-aligns the cache — opportunistic recovery, no new code needed.
4. In `docs/REDEX.md` (the operator-facing replication doc), add
   a "Divergence between local role and advertised holder set"
   subsection. State the upper-bound recovery time:
   `divergence_until_next(transition_to) | cancel()`.
5. Regression test:
   `tag_sink_failure_bumps_divergence_counter` — install a sink
   that returns `Err`, call `transition_to(Idle, GracefulRelinquish)`,
   assert the counter incremented by 1 and the state cell IS
   still `Idle` (preserves existing test invariant).
6. Update the existing `tag_sink_failure_surfaces_but_state_mutated`
   test's doc-comment to point at the new counter as the
   observability surface.

**Surface change:** new metric only; no behavior change.

---

#### D-3 — Symlink-swap window between canonicalize and rename

**Decision: Option C — Document the threat model.**

The FS adapter's deployment story already requires exclusive root
ownership in practice — the daemon process needs write access to
the root, and the standard ops pattern is `chown <daemon-user>
<root>` plus restrictive permissions. The symlink-swap attack
assumes write access inside the root by a non-daemon process,
which violates that contract.

**Rejected: A (`rustix` + `openat2`).** Linux 5.6+ only. Adds a
build dep that's only effective on Linux. Doesn't help Windows or
macOS deployments. If we eventually need this, it should be gated
behind the documented threat model rather than offered as a
catch-all.

**Rejected: B (cross-platform dev/inode verify).** Narrows the
race window but doesn't close it — `rename(tmp, path)` resolves
`path` independently of the open parent fd. The added code
complexity doesn't deliver the security property cleanly enough
to justify it.

**Implementation plan:**

1. In `src/adapter/net/dataforts/blob/fs.rs`'s `FileSystemAdapter`
   struct docstring, add a "**Threat model**" section stating:
   - The adapter assumes the configured `root` directory is
     writable only by the substrate process (and any process
     running with the same uid).
   - Cross-process write access inside `root` by a non-substrate
     user enables the documented symlink-swap window between
     canonicalize and rename.
   - Operators MUST enforce the contract via filesystem
     permissions: `chown <daemon-user> <root>` plus mode `0700`
     (or equivalent ACL on Windows).
2. Document the existing in-code defenses (parent-canonicalize
   `starts_with(root)` + per-store hash-verify-on-rename-failure)
   as defense-in-depth — they close the most-obvious paths but
   are not a complete sandbox.
3. Reference the umbrella audit's D-3 entry from the docstring so
   a future review picks up the deliberate decision rather than
   re-flagging.
4. Cross-link from `docs/DATAFORTS.md`'s "Persistent backends"
   section.
5. If a deployment ever needs to host the root in a shared-scratch
   environment, escalate to a follow-up project that adopts
   `rustix::fs::openat2` behind a `unix-strict-sandbox` feature
   flag.

**Surface change:** none. Pure documentation.

---

#### X-13 — Failed placements retry on different-node recovery

**Decision: Option C — Periodic tick from the meshos loop.**

Meshos already owns the reconcile cadence and already has access
to the scheduler registry. Decoupling the recovery timer from the
group type itself keeps each group's API surface stable while
landing the retry behavior in one place.

**Rejected: A (extend `on_node_recovery` signature).** Breaking
API change across every caller. Forces the scheduler + factory
dependency into the recovery boundary even where it doesn't
belong.

**Rejected: B (new `retry_failed_placements` per group).** Pure
addition but pushes the cadence question onto every caller. Most
callers don't know when to call it, so it'd sit unused.

**Rejected: D (defer `mark_unhealthy` until after placement).**
Avoids the stuck-unhealthy state but introduces a worse failure
mode — routes go to a known-dead daemon during the placement-
attempt window. The current pre-mark-unhealthy ordering correctly
quiesces the slot before the placement attempt; the bug is purely
"recovery never retries," not "we shouldn't mark unhealthy."

**Implementation plan:**

1. Add a small trait in `src/adapter/net/compute/mod.rs`:
   ```rust
   pub trait UnhealthySlotRecovery: Send + Sync {
       fn has_unhealthy_slots(&self) -> bool;
       fn try_recover<F>(
           &mut self,
           scheduler: &Scheduler,
           registry: &DaemonRegistry,
           daemon_factory: F,
       ) -> Vec<u8>
       where
           F: Fn() -> Box<dyn MeshDaemon>;
   }
   ```
2. Implement for `ForkGroup`, `ReplicaGroup`, `StandbyGroup`.
   Each impl walks its `coord.members()`, picks slots marked
   `!healthy`, runs `place_with_spread` / `place_member` against
   the current healthy node pool, and returns the recovered slot
   indices.
3. Add a registry on `MeshOsRuntime` for groups that opt in:
   `pub fn register_group_for_recovery(&self, group: Arc<Mutex<dyn UnhealthySlotRecovery>>)`.
   Stores in a `parking_lot::Mutex<Vec<Weak<...>>>` so dropped
   groups are GC'd automatically on the next pass.
4. In `event_loop.rs`'s tick handler (right after `gc_freeze`),
   walk the recovery registry. For each live group with
   unhealthy slots, call `try_recover` with the scheduler from
   `SchedulerRegistry` and the daemon-factory from the group's
   stored config.
5. Cap the recovery work per tick (e.g. 4 slots / tick) so a
   pathological "every slot unhealthy" state doesn't wedge the
   loop. Continue on next tick.
6. Regression tests (one per group type):
   `<group>_recovers_failed_placement_after_different_node_comes_online`
   — set up a 3-replica group, fail a node, observe the slot
   stays unhealthy, bring a different spare online, drive one
   reconcile tick, assert the slot is now healthy + placed on the
   spare.

**Surface change:** new trait + new opt-in registration. Existing
callers that don't register get current behavior.

---

#### X-17 — Multi-chunk validation symmetry

**Decision: Option C — Extract `validate_chunk_header(chunk, record)`
helper.**

The per-chunk envelope (magic, version, source identity claim)
can be validated without forcing the orchestrator to reassemble
the full snapshot. Calling the helper on every chunk regardless
of count closes the asymmetry without breaking the orchestrator's
streaming property.

**Rejected: A (drop single-chunk validation).** Achieves symmetry
by making the system weaker. Single-chunk corruption already
catches at the orchestrator today; we shouldn't lose that just to
match multi-chunk.

**Rejected: B (orchestrator-side reassembly for multi-chunk).**
Heavy. The orchestrator deliberately streams chunks; forcing
buffering per in-flight migration changes its memory profile and
introduces a new OOM surface.

**Implementation plan:**

1. Define `validate_chunk_header(chunk_bytes: &[u8], expected_daemon_origin: u64) -> Result<(), MigrationError>`
   as a free function in `src/adapter/net/compute/orchestrator.rs`.
   Validates:
   - Snapshot magic byte at offset 0 (mirror of what
     `StateSnapshot::from_bytes` does on the single-chunk path,
     but without the postcard decode).
   - Version byte at offset 1 matches `SNAPSHOT_VERSION`.
   - Source-identity claim (`entity_id.origin_hash()` projection
     bytes at the documented offset) matches
     `expected_daemon_origin`.
2. Call from `on_snapshot_ready` on every chunk:
   - For `chunk_index == 0 && total_chunks == 1` (single-chunk
     path), call before the full `StateSnapshot::from_bytes`
     decode (the helper is strictly weaker than the full decode,
     so a failure here fails faster).
   - For multi-chunk paths, call on every chunk header before
     `force_phase(Transfer)` / forwarding.
3. On failure, return `MigrationError::StateFailed(format!(
   "SnapshotReady chunk {} of {} failed header validation: {}",
   ...))`.
4. Regression test:
   `on_snapshot_ready_rejects_multichunk_header_corruption` —
   ship a 3-chunk SnapshotReady where chunk 0 has the wrong
   magic byte; assert StateFailed at orchestrator, not deferred
   to target reassembly.

**Surface change:** new internal helper; on the failure path, a
new orchestrator-side error variant message. Wire format
unchanged.

**Composes with X-10.** The `seq_through` cross-check landed for
single-chunk in commit `640f160c` extends naturally — once
multi-chunk has per-chunk header validation, the same
`seq_through` field can be carried inside the chunk header and
validated against `snapshot.through_seq` after reassembly.

---

#### X-20 — Delete dead `MigrationOrchestrator::buffer_event`

**Decision: Option A — Delete the dead surface entirely.**

The dead surface IS the trip-wire. Keeping it (even redirected)
preserves a misleading API shape that an SDK consumer reading
the public API would wire up expecting symmetry with
`source_handler.buffer_event`.

**Rejected: B (redirect to source_handler.buffer_event).**
Preserves the public API but doesn't remove the conceptual
trip-wire. A consumer who reads the orchestrator's docs and
wires `orchestrator.buffer_event` still has a strictly worse
mental model than one who reads `source_handler.buffer_event`
directly. The API-stability argument doesn't apply because nobody
in production calls this.

**Implementation plan:**

1. Delete from `src/adapter/net/compute/orchestrator.rs`:
   - `pub fn MigrationOrchestrator::buffer_event` (line ~1605).
   - `pub enum BufferOutcome` variants and the enum itself.
2. Delete from `src/adapter/net/compute/migration.rs`'s
   `MigrationState`:
   - `pub buffered_events: Vec<CausalEvent>` field.
   - `pub fn buffer_event(...)` method.
   - `pub fn take_buffered_events(...)` method.
   - `pub fn buffered_event_count(...)` method.
3. Update `MigrationOrchestrator::on_restore_complete` (line
   ~1414): instead of draining `record.state.buffered_events`,
   return `Ok(None)` directly. Document that the
   `BufferedEvents` wire message originates exclusively from
   `MigrationSourceHandler::on_cutover` going forward.
4. Update `list_migrations` to drop the `buffered_events: u32`
   field on `MigrationListItem` (no production consumer reads
   it; tests that assert against `buffered_events: 0` will be
   updated alongside).
5. Delete tests in `orchestrator.rs::tests`:
   - `test_buffer_event_*` (every `orch.buffer_event(...)` call
     site).
   - `buffered_events_saturates_at_u32_max`.
   - `buffer_event_distinguishes_post_cutover_from_no_migration`
     (move the underlying assertion into
     `source_handler.rs::tests` if it's not already pinned there).
6. Audit the SDK / FFI bindings: `sdk-ts`, `sdk-py`, `bindings/`
   for any wire-up that references `BufferOutcome` or the
   removed methods. Should be zero (this is dead code), but
   verify.
7. Regression test: `on_restore_complete_ships_empty_buffered_events`
   — pin that the only buffered-events source is now
   `source_handler.on_cutover`.

**Surface change:** breaking. `MigrationOrchestrator::buffer_event`,
`BufferOutcome`, and the related `MigrationState` methods are
removed. Document in the next version bump's CHANGELOG.

---

#### X-21 — LocalPreferred liveness in `Scheduler::place`

**Decision: Option C — New `place_with_locality(filter, drained: bool)`
method.**

`Scheduler` is positioned as a stateless capability-index helper.
Folding node-lifecycle state into it is a layering violation; the
caller (which knows about maintenance state, drain status, and
process lifecycle) is the right place to supply the answer.

**Rejected: A (`local_drained: AtomicBool` on Scheduler).**
Schedulers across the codebase deliberately keep no lifecycle
state. Adding this one field invites future creep (saturation
levels, RTT cache, etc.) until the scheduler owns mutable
node-state.

**Rejected: B (pull from MeshOsState).** Cross-layer dependency
in the wrong direction. Scheduler would have to depend on
meshos types it currently has no awareness of.

**Implementation plan:**

1. In `src/adapter/net/compute/scheduler.rs`, add a new method:
   ```rust
   /// Like [`Self::place`], but with an explicit `local_drained`
   /// signal. The LocalPreferred fast-path is gated on
   /// `!local_drained` so a caller in mid-shutdown / operator-
   /// drained state can route fresh placements off the local node.
   pub fn place_with_locality(
       &self,
       filter: &CapabilityFilter,
       local_drained: bool,
   ) -> Result<PlacementDecision, SchedulerError> {
       if !local_drained && self.can_run_locally(filter) {
           return Ok(PlacementDecision {
               node_id: self.local_node_id,
               reason: PlacementReason::LocalPreferred,
           });
       }
       // ... rest matches Self::place
   }
   ```
2. Keep `Self::place` as a forwarder for backward compatibility:
   `pub fn place(...) -> ... { self.place_with_locality(filter, false) }`.
   The default (non-drained) preserves current behavior for
   callers that don't know about drain state.
3. In `src/adapter/net/behavior/meshos/reconcile.rs`'s
   placement-issuing path, read `MeshOsState::local_maintenance`
   and translate to a bool: `local_drained = matches!(local_maintenance,
   MaintenanceState::EnteringMaintenance { .. } | Maintenance { .. } |
   ExitingMaintenance { .. } | DrainFailed { .. })` (everything
   except `Active` and `Recovery`).
4. Have the reconcile-driven placement caller route through
   `place_with_locality(filter, local_drained)`.
5. Regression tests on `Scheduler`:
   - `place_with_locality_skips_local_when_drained` — local has
     matching caps, `local_drained == true`, decision routes to
     a remote.
   - `place_with_locality_picks_local_when_not_drained` —
     baseline.
   - `place_with_locality_returns_no_candidate_when_drained_and_no_remote`
     — local drained + no remote candidates → SchedulerError::NoCandidate.

**Surface change:** new public method; `Scheduler::place`
forwarder preserves existing behavior. No breaks.

---

#### O-4 + O-5 — Audit-chain durability ordering

**Decision (O-4, executor): Option C — accept the gap + surface a metric.**

**Decision (O-5, audit/log loop): Option B — ring-first then chain,
with `chain_pending` flag on failure.**

The two surfaces have different invariants:

- **O-4 (executor):** the dispatch already happened. A chain miss
  is a record-keeping gap, not a correctness gap. The action ran;
  the operator just doesn't have a chain record of it. Surface
  the metric so the gap is observable.
- **O-5 (audit/log loop):** the ring is the immediate user-visible
  surface. Users read from the ring directly. The chain is
  durability backup that consumers replay later. Ring-first
  matches the user expectation and `chain_pending` lets chain
  consumers distinguish "this entry never landed" from "this
  entry didn't reach me yet."

**Rejected for both: A (Pending+Outcome chain records).** Doubles
chain volume. Forces chain consumers to reconcile pending+outcome
pairs. The complexity isn't justified by the failure mode.

**Implementation plan — O-4 (executor):**

1. In `src/adapter/net/behavior/meshos/executor.rs`, add
   `chain_append_failures: AtomicU64` to `ExecutorStats`.
2. In `ActionExecutor::handle_one_retry` (line ~470), where
   `append_dispatched` is called and the result is currently
   ignored via `let _ = append_dispatched(...)`:
   - Capture the result.
   - On `Err`: increment `chain_append_failures` and emit a
     `tracing::warn!` with `action_id`, `error`, and the literal
     `"executor: chain_append_failed; dispatch already succeeded \
     — chain record missing for this action"`.
3. Mirror at `:497`, `:502`, and `:518` (the other
   `append_dispatched` / `append_failed` / `append_gated` sites).
4. Expose via `ExecutorStatsSnapshot::chain_append_failures: u64`
   so the Prometheus surface picks it up automatically.
5. In `docs/MESHOS.md`'s "Operability" section, document the new
   metric and what triggers it.
6. Regression test:
   `chain_append_failure_bumps_counter_but_dispatch_still_succeeds`
   — install a chain appender that returns Err, dispatch a
   successful action, assert (a) dispatch succeeded, (b) the
   counter incremented, (c) the warn log fired.

**Implementation plan — O-5 (audit/log loop):**

1. In `src/adapter/net/behavior/meshos/ice.rs`, add
   `chain_pending: bool` to `AdminAuditRecord`.
2. In `src/adapter/net/behavior/meshos/logs.rs`, add
   `chain_pending: bool` to `LogRecord`.
3. In `src/adapter/net/behavior/meshos/event_loop.rs`'s
   `record_admin_audit` (line ~1078) and `record_log_line`
   (line ~1106):
   - Push to the ring FIRST with `chain_pending: false`.
   - Attempt chain append.
   - On `Err`: re-read the just-pushed ring entry (it's the
     newest, at `back()`) and set `chain_pending = true` so
     consumers reading the ring see the gap.
4. Update SDK consumers of these records (any FFI bindings,
   `MeshOsSnapshot::admin_audit` reader path) to surface the
   `chain_pending` field. Default `false` if absent in
   serialization for backward compat.
5. Update `MeshOsSnapshot` doc to say: "Entries with
   `chain_pending: true` did not make it to the durable chain;
   chain consumers replaying the chain after a restart will
   miss them."
6. Regression test:
   `record_admin_audit_marks_chain_pending_on_append_failure`
   — install an audit appender that returns Err, call
   `record_admin_audit`, assert the ring entry exists with
   `chain_pending == true` and the audit_audit_seq has
   advanced (idempotent on retry isn't a concern here — the
   record IS authoritative; the chain just doesn't have it).

**Surface change:**
- O-4: new metric counter + log line. No data shape change.
- O-5: new `chain_pending: bool` field on `AdminAuditRecord` and
  `LogRecord`. Serialization is forward-compatible (default
  `false` if absent).

---

#### O-9 — `gc_drain_window` 1-second hardcode (verified not-a-bug)

**Decision: Document, don't fix.**

The 1-second window IS the natural denominator for
`BackpressureConfig::drain_rate_per_zone_per_sec`. The rate config
field's name says "per second"; the window matches that
definition. There's no bug; the audit over-flagged a config-shape
question as a code-shape question.

**Rejected: change the window.** Doing so requires redefining the
rate config semantic. If we want a tunable drain window, we
should add a separate `drain_window_duration: Duration` field
rather than reinterpreting `drain_rate_per_zone_per_sec`.

**Implementation plan:**

1. In `src/adapter/net/behavior/meshos/backpressure.rs`'s
   `gc_drain_window` (line ~273), add a code comment:
   ```rust
   // 1-second window is by definition the denominator of
   // BackpressureConfig::drain_rate_per_zone_per_sec. The
   // hardcode here matches the config field's name; changing
   // the window without renaming the config field would
   // silently invert the operator-facing rate semantic. If
   // operator-tunable drain windows are ever wanted, add a
   // separate `drain_window_duration: Duration` config rather
   // than reinterpreting the existing rate field.
   ```
2. No code change. No metric. No test.

**Surface change:** none. Pure comment.

### Deferred — remaining lows (low impact, defer to follow-up cleanup commit)

- **O-6** — `Defer` re-queue never emits a chain record. Subsumes
  cleanly under O-4's audit-chain ordering decision.
- **O-10** — `BackoffTracker::observe_crash` window-slide
  non-monotonic on out-of-order crashes. Reachable only under
  clock-skew or test fixture seeding.
- **O-11** — `MaintenanceState::EnteringMaintenance.deadline`
  wall-clock pre-first-tick fallback. Cosmetic; first tick
  recomputes.
- **O-12** — `emit_maintenance_transitions` skips when
  `control_sink` wired late. Initialization-order edge.
- **O-13** — `release_failed_admit` for `PullReplica` clears chain
  stabilization even when a sibling holds it. Bounded by
  `pull_cooldown` recovery.
- **O-14** — `MigrationSnapshotSource::list()` called inside the
  loop hot path; slow source stalls reconcile. Architectural —
  needs a snapshot-source async interface.
- **O-15** — `last_pull_admitted_by` rollback only clears
  most-recent slot. Verified correct under cooldown semantics; no
  fix needed (audit may have over-flagged).
- **O-16** — `WIRE_FORMAT_VERSION = 1` with no migration path
  documented. Pure documentation.
- **O-18** — `AdminVerifier::verify_commit` drops the `ice_state`
  mutex between `check_ice_cooldown` and `record_ice_cooldown`.
  Not reachable today (single-task verifier path).
- **D-16** — `MeshBlobAdapter::fetch_range` on Small reads the
  entire chunk before slicing. Needs a seek-based
  `RedexFile::read_at(offset, len)` primitive; not exposed today.

### Phase coverage status

- **Phase 2** (Miri / ASan / TSan / fuzz): still skipped; existing
  `fuzz/fuzz_targets/` is wired.
- **Cross-language conformance (Phase 4):** Rust/TS/Py/Go SDK
  round-trip property tests not started.
- **Dep audit:** `cargo-audit` / `cargo-machete` / `cargo-deny` /
  `cargo-udeps` not installed.

### Verdict

The `bugfixes-15` branch is in substantially better shape after the
fix sprint. Major DoS surfaces (X-9 wire-reachable OOM, D-11
manifest panic, D-18 UTF-8 panic, MD-1 federated drain, O-21 silent
throttle), data-loss surfaces (D-1 sweep TOCTOU, X-2/X-3 migration
phase guards), and dozens of correctness / observability gaps are
closed. Remaining heavy work — replication peer auth bundle,
StandbyGroup fencing, migration / rendezvous / membership / nRPC
binding — is documented above with the recommended approach for
each; each item is a focused-session task.
