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
| High | 13 | A-5, R-22, R-23, R-24, R-25, X-2, X-3, X-9, X-18, D-1, D-2, D-11, D-14 |
| Medium | 25 | (see body) |
| Low | 35 | (counts only at the end) |
| Latent | 1 | MD-3 |
| Null | 1 module | `netdb/` clean |

Third pass added 1 H (D-14) + 4 M (D-15, D-16, R-35, X-13) + 8 L
(R-36..R-39, X-14..X-17) — see "Third-pass additions" section.

Fourth pass added 1 H (X-18) + 2 M (O-20, MD-1) + 1 L (MD-2) + 1 latent
(MD-3) — see "Fourth-pass additions" section. `behavior/meshdb/` was
newly in-scope for the fourth pass; the MD-* prefix names that module.

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

### A-5 — In-progress L-13 fix strips reserved metadata before re-forwarding, breaking multi-hop signed propagation
- **File:** `src/adapter/net/mesh.rs:5239` (uncommitted on `bugfixes-15`); `src/adapter/net/behavior/capability.rs:2145-2156` (new `strip_reserved_metadata`); `src/adapter/net/behavior/capability.rs:2084-2093` (`signed_payload`).
- **What:** The uncommitted L-13 fix calls `ann.strip_reserved_metadata()` immediately after the signature verify and TOFU pin, then *later* clones `ann`, bumps `hop_count`, and reserializes via `to_bytes()` to forward to other peers (`mesh.rs:5343-5358`). `signed_payload()` covers the `metadata` field — so the forwarded wire bytes no longer match the signature transcript. Any peer two-plus hops downstream with `require_signed_capabilities = true` rejects the forwarded announcement at the verify step (`mesh.rs:5200`).
- **Impact:** Functional regression in the new fix. Multi-hop signed capability discovery breaks for any receiver that requires signed caps. Fails *closed* (announcement is dropped, not accepted) so it's not an auth bypass — but the feature stops working. The existing strip test (`capability.rs:3712`) only exercises strip in isolation; no multi-hop round-trip test catches it.
- **Fix sketch:** Move `ann.strip_reserved_metadata()` to between the forward block (after `mesh.rs:5358`) and `capability_index.index(ann)` at `:5371`. The only consumer in between — `policy.assign(&ann.capabilities)` at `:5305` — reads `caps.tags` only, not `caps.metadata`, so it's unaffected by the move. Add a multi-hop signed-propagation round-trip test before merging.

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

### R-28 — Catchup busy-loops if leader heartbeats advertise tail past actual log
- **File:** `src/adapter/net/redex/replication_step.rs:227-251`
- **What:** Each tick emits one `SyncRequest` if `peer.tail_seq > local_tail`. A buggy/byzantine leader that emits ever-increasing `tail_seq` but ships `Response{events: []}` (empty because `since_seq >= local_next` on leader) makes the replica spam-request every tick forever. No backoff, no max-attempts.
- **Trigger:** Buggy leader reports `tail_seq=999_999` but file has no such entries. Replica busy-loops at heartbeat cadence (100 ms). Combined with R-25, saturates the leader's inbox.
- **Fix sketch:** Track per-leader "consecutive empty responses with advertised gap" counter; back off exponentially after 3 empty replies despite advertised lag.

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
- R-30 — `wall_clock_ms` collected from heartbeats but never used; either implement the skew gauge or drop the field (`replication.rs:237-244`)
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

#### X-18 — Migration dispatch arms accept arbitrary `from_node`; any peer forces cutover / abort
- **File:** `src/adapter/net/subprotocol/migration_handler.rs:600-642` (`CleanupComplete`, `ActivateTarget`); `:654-690` (`MigrationFailed`); `:558-598` (`SnapshotReady`); `src/adapter/net/compute/migration_target.rs:295-306` (`activate`).
- **What:** The subprotocol's `ActivateTarget` arm invokes `target_handler.activate(daemon_origin)` without comparing the inbound `from_node` against the migration's recorded `orchestrator_node`. The recorded orchestrator is consulted only when routing the *ack reply* (`:626-629`); it is not used to gate entry. The companion arms `CleanupComplete`, `MigrationFailed`, and `SnapshotReady` likewise dispatch state-mutating handler calls without binding `from_node` to the recorded orchestrator. Only the `TakeSnapshot` arm records the orchestrator (`start_snapshot(..., from_node)` at `:312`).
- **Trigger / Attack:** Any peer with subprotocol-0x0500 reach ships `MigrationMessage::ActivateTarget{daemon_origin}` for a migration that is mid-Replay. The target flips to `Cutover` and goes live while the source still believes it owns the daemon → both nodes accept writes to the same origin → divergent chain heads. Same shape as **X-1** (StandbyGroup fencing) but driven by a single wire message rather than a partition heal. Symmetric variants: a forged `MigrationFailed` from any peer drives source rollback after legitimate cutover; a forged `CleanupComplete` makes the orchestrator emit `ActivateTarget` to a target that hasn't fully restored.
- **Same shape as R-20:** R-20 is "no replica-set membership check on replication-subprotocol inbound." X-18 is the migration-subprotocol equivalent. The umbrella's A-1..A-3 capability fixes landed on the publish path; neither subprotocol was in their scope.
- **Fix sketch:** In `dispatch()` for every state-mutating arm (`ActivateTarget`, `CleanupComplete`, `MigrationFailed`, `SnapshotReady`), look up the recorded `orchestrator_node(daemon_origin)`; reject with `MigrationError::WrongPeer` if it's `Some(n) && n != from_node`. The source-side `TakeSnapshot` arm already establishes the binding at `:312`; the symmetric check on later arms enforces "only the recorded orchestrator drives this migration forward." Add a regression test covering forged `ActivateTarget` from a non-orchestrator peer.

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

## Coverage gaps still carried forward

- **Phase 2** (Miri / ASan / TSan / fuzz) — still skipped; existing `fuzz/fuzz_targets/` is wired.
- **Cross-language conformance (Phase 4)** — Rust/TS/Py/Go SDK round-trip property tests not started.
- **Dep audit** — `cargo-audit` / `cargo-machete` / `cargo-deny` / `cargo-udeps` not installed.
- **Adjacent surfaces not reviewed this round:** `src/adapter/net/contested/`, `src/adapter/net/continuity/`, `src/adapter/net/cortex/` (re-review post-fixes), `src/adapter/net/identity/`, `src/adapter/net/subnet/`, `src/adapter/net/state/`, `src/adapter/net/traversal/`. Each is a candidate for a follow-up. `src/adapter/net/subprotocol/` was partially covered by X-18 (migration handler) but the other subprotocol handlers (`redex_handler`, `capability_handler`, `meshdb_handler`, etc.) were not audited for the same "no `from_node` binding" class of issue R-20 + X-18 both share. **Recommend a targeted sweep:** for every subprotocol arm that mutates state, verify `from_node` is bound to a recorded principal before the mutation.
- **`src/adapter/net/behavior/meshdb/`** is now partially covered (MD-1, MD-2, MD-3). The planner / federated executor / cache layer received targeted reads; full module sweep — including `executor.rs` plan execution, `transport.rs` framing, `row.rs` predicate walking, and the `query.rs` request/response surface — is still owed.
