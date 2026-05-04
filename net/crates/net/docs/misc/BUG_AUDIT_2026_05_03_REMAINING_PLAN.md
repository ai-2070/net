# Remaining-bug plan — BUG_AUDIT_2026_05_03

Living plan for the items still unfixed from `BUG_AUDIT_2026_05_03.md`
after the bugfixes-9 series (which closed 149 of 171) and the
post-merge follow-ups on bugfixes-10/11/12 (mesh series, reassembler
age sweep, `compact_to` `MoveFileExW`).

The audit doc itself is the source of truth for the bug descriptions
and `[FIXED in <sha>]` markers. This plan is the **work-ordering
view**: groups the remaining items by subsystem so they can be
attacked in parallel, sequences within each group, and notes
cross-item dependencies.

## Status snapshot (as of bugfixes-12)

- Critical: **0 remaining**.
- High: **7 unfixed entries** (audit's "5" lumps the FFI
  handle-lifetime cluster #23/#24/#25 as one).
- Medium / Lower: **13 unfixed entries**.
- Skipped: #39 (needs persistent-sequence feature work; not a
  bug fix), #97 (audit-suggested reorder conflicts with the
  credit-window invariant — see `f41d9c36`).
- Long-term follow-up: **#1 manifest-pointer atomic-flip** —
  `compact_to` cross-rename mixed-state window. The
  `durable_rename` / `MoveFileExW(MOVEFILE_WRITE_THROUGH)` fix
  in `50ba6ae5` closed the per-call durability hole; the
  cross-file atomicity gap remains and is sized as a separate
  ~600–1k LOC rework (sketch at the bottom).

## Workstreams

The remaining items cluster cleanly into six independent
workstreams. Anything labeled **Cluster** ships as one PR; anything
labeled **Item** is a one-commit fix. Each workstream is sized so
it can run in parallel with the others.

### A — Cortex watermark / snapshot integrity (High, coupled)

Three items that compound: #6 corrupts the watermark, #7
persists the corruption into snapshots, #8 fails to detect on-disk
bit-flips that would mask either.

| #  | File:line                                              | Ask                                                                              | Notes                                                                                                              |
| -- | ------------------------------------------------------ | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| 6  | `adapter/net/cortex/adapter.rs:486-499`                | Don't bump `folded_through_seq` when `recoverable_decode` skipped the event.     | Currently `wait_for_seq(seq)` returns true for events whose state mutation never landed. `Stop` policy only.       |
| 7  | `adapter/net/cortex/adapter.rs:374-386` + `tasks/adapter.rs:340-344` | Snapshot must NOT include `last_seq = N` for skipped events.                     | Couples with #6. After both fixes, "log is the source of truth" actually holds across snapshot/restore.            |
| 8  | `adapter/net/cortex/meta.rs:135-137`                   | Extend `compute_checksum` to cover the meta header, not just `tail`.             | A `STORED → DELETED` bit-flip in the dispatch byte currently re-routes the event to the wrong fold arm undetected. |

**Ordering:** fix #6 first (root cause), then #7 (snapshot
contamination from #6), then #8 (independent — schedule it last
in this cluster but can ship first if convenient).

**Test surface:** unit tests forcing `recoverable_decode` to
skip via a poisoned-payload fixture; snapshot+restore round-trip
that asserts the skipped seq is re-attempted on rehydrate; bit-
flip injection on the meta header confirming the new checksum
catches it.

### B — Compute registry quiescence (High + Medium, coupled)

#13 and #68 share the same root cause: the `Arc<Mutex<DaemonHost>>`
get/replace pattern doesn't quiesce in-flight callers, so a
swap or unregister can leave the old host mutating in parallel
with the new one.

| #  | File:line                                          | Ask                                                                                 | Notes                                                                                                          |
| -- | -------------------------------------------------- | ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| 13 | `adapter/net/compute/registry.rs:71-74` (`replace`) | Caller holding an old `get_arc` mustn't mutate post-swap.                            | Used by `replica_group::on_node_failure`, `fork_group::on_node_failure`, `standby_group::on_node_failure`.     |
| 68 | `adapter/net/compute/registry.rs:91-96` (`unregister`) | Fast `register` of the same origin after `unregister` mustn't produce two parallel Arcs. | Same shape as #13 but for the unregister path. Splits writes between the surviving in-flight Arc and the fresh one. |

**Approach:** introduce a generation counter on `DaemonHost` and a
`with_host(origin, FnOnce(&mut DaemonHost))` accessor that takes the
write lock and verifies the generation hasn't advanced. `replace`
and `unregister` bump the generation under the same lock. In-flight
callers that observe a generation mismatch error out instead of
mutating.

**Test surface:** spawn a slow caller holding `get_arc`, race
`replace` against it, assert no writes from the slow caller land
in the new host (and the slow caller surfaces a typed error).

### C — Cortex/mesh FFI handle-lifetime cluster (High, port from `ffi/mod.rs`)

#23 / #24 / #25 are the same hazard, three different files. The
fix is to port the `active_ops` + `bus_taken` CAS protocol that
`ffi/mod.rs` already implements (and was hardened in
`928bd520` / `41ebbf8f`).

**Status (post-bugfixes-12 work):**

- ✅ Shared helper `ffi::handle_guard::HandleGuard` + `HandleOp` +
  `FFI_HANDLE_FREE_DEADLINE` extracted. Five unit tests pin
  try_enter, post-free bail, drain-wait, drain-timeout, and
  idempotent concurrent free callers.
- ✅ `RedexFileHandle` (audit #23 specific call-out) ported. Three
  regression tests pin post-free `ShuttingDown` on every entry
  point, idempotent `_free`, and `_free` waiting for an in-flight
  `_append` to drain.
- ⏳ Remaining handles: `RedexHandle`, `RedexTailHandle`,
  `TasksAdapterHandle`, `TasksWatchHandle`,
  `MemoriesAdapterHandle`, `MemoriesWatchHandle` (cortex #23
  remaining). All `MeshNodeHandle` / `MeshStreamHandle` /
  `IdentityHandle` / `RedisDedupHandle` entry points (#24, #25).
  All follow the identical recipe — the proof-of-pattern lives in
  `RedexFileHandle`'s structure plus its `_free` and `try_enter`
  call sites.

| #  | File                | Status                                                                                               |
| -- | ------------------- | ---------------------------------------------------------------------------------------------------- |
| 23 | `ffi/cortex.rs`     | RedexFileHandle done. ~25 sites remaining across 5 other handle types; mechanical port of the recipe. |
| 24 | `ffi/mesh.rs`       | Pending. ~60 entry points spread across 4 handle types.                                              |
| 25 | `ffi/mesh.rs:1078-1079` | Pending. The MeshStreamHandle UAF is closed by gating MeshNodeHandle's send-family ops; also pending. |

**Recipe for remaining handles (apply per type):**

1. Add `guard: HandleGuard` field; wrap the inner Arc(s) in
   `ManuallyDrop`.
2. In the constructor: initialize `guard: HandleGuard::new()`,
   wrap inner in `ManuallyDrop::new(...)`.
3. In `_free`: drop the inner via `ManuallyDrop::take` only AFTER
   `begin_free(FFI_HANDLE_FREE_DEADLINE)` returns true. On
   timeout, leak (log a warning). The outer Box is always leaked
   — never `Box::from_raw` here.
4. In every entry point that does `&*handle`: gate on
   `let _op = match h.guard.try_enter() { Some(op) => op, None =>
   return NetError::ShuttingDown.into() };` after the null check
   and before any inner deref.
5. Pin three regression tests per handle: post-free op returns
   `ShuttingDown`, `_free` is idempotent, `_free` waits for
   in-flight ops.

**Test surface:** the helper module already has its own race-
injection tests; per-handle tests need only verify the entry
points are wired (post-free `ShuttingDown`) and `_free` is sound
(idempotent + waits).

**Sequencing:** the remaining handles can be tackled in any
order. Suggested next: `MeshStreamHandle` + `MeshNodeHandle`
together (audit #25 needs both for the `Arc::ptr_eq` UAF in
`handles_match`). Then the rest of cortex (`Tasks`, `Memories`)
together. Then `RedisDedupHandle` and `IdentityHandle` (small).

### D — Identity / envelope hardening (Medium, deferred — both need wire-bump cycle)

| #   | File:line                                          | Ask                                                                                                                                                  |
| --- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| 56  | `adapter/net/identity/origin.rs:42-43`             | `origin_hash: u32` has a ~65k birthday-collision floor — either rename to `origin_tag` (signal not-an-identity) or widen to u64 in a wire bump.      |
| 102 | `adapter/net/identity/envelope.rs:361-396` (v0 fallback) | Pin a wire-format version byte at the envelope head so `open` selects v0/v1 deterministically. Today the v1→v0 retry path does double AEAD per probe. |

**Status: deferred to a wire-format-bump cycle.** Both items
require coordinated wire-format work that exceeds the
single-PR-per-item shape this branch follows:

- **#102:** the proper fix (version byte) bumps
  `IDENTITY_ENVELOPE_SIZE` from 208 to 209. That width is also
  embedded in the snapshot wire format
  (`adapter/net/state/snapshot.rs` reads/writes envelopes at
  fixed offsets) — closing the audit gap requires a snapshot
  format bump too. The CPU-DoS amplification is bounded (2× per
  failing probe; outer Noise NK already authenticates senders so
  the threat is constrained to authenticated peers replaying
  bogus envelopes), so the cost of deferring is small relative
  to the wire-bump scope. The existing
  `open_accepts_v0_envelope_for_rolling_upgrade_compat` test
  pins the v0 fallback explicitly — removing it would conflict
  with the project's deliberate rolling-upgrade compat decision.

- **#56:** rename hits 660+ sites; widen to u64 hits the wire
  format. Either way it's a wire-bump-shape change. The `u32`
  birthday-collision floor is structural; ~65k peers is the
  collision floor under birthday but production deployments
  with that many distinct origins are rare and the cross-channel
  accounting impact is bounded (alias-not-identity-takeover).

**When to revisit:** next time a wire-format bump is on the
roadmap (e.g., another protocol-level change like #1's
manifest-pointer rework). Bundle #102 and #56 into the same
bump to amortize the migration cost.

### E — Compute / orchestrator / merge fixes (Medium, independent)

| #  | File:line                                            | Ask                                                                                                                                                                |
| -- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 64 | `adapter/net/compute/orchestrator.rs:1177-1182`     | `on_replay_complete` synthesizes a `target_head` with `parent_hash: 0`; downstream verifiers can't reconcile. Compute the real parent_hash or surface as typed error. |
| 73 | `consumer/merge.rs:384`                              | Per-shard cap currently rolls the cursor BACK on `unclamped_per_shard > PER_SHARD_FETCH_CAP`; advance it to the last fetched event id instead.                    |

### F — Adapter / behavior / network polish (Lower, independent)

| #   | File:line                                       | Ask                                                                                                                          |
| --- | ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| 81  | `adapter/redis.rs:316-405`                      | Wire `RedisStreamDedup` by default for Go consumers (or surface a config error when it's missing). Pipeline timeout duplicate hazard documented but unfixed. |
| 118 | `adapter/net/behavior/rules.rs:1247-1253`       | Window reset to `0` not `1` so the post-reset window doesn't admit `max-1` further firings.                                  |
| 121 | `adapter/net/behavior/loadbalance.rs:1183-1187` | `select_power_of_two` degenerates to deterministic pair when `len == 2`; fall back to a single-target pick or skew the second draw. |
| 125 | `adapter/net/behavior/safety.rs:451-466`        | `per_source.clear()` at minute boundary drops just-incremented counters; window the reset.                                   |
| 127 | `adapter/net/mod.rs:495-522`                    | Initiator handshake path needs a `HandshakePacer`-style rate limit (responder already has one; #26 added the routed-handshake guard but the initiator side is still uncapped). |
| 128 | `adapter/net/router.rs:198-222`                 | `notify_one` lost-wakeup window currently bounded to 1ms by polling fallback — switch to `Notify::notify_waiters` or document the floor.                 |

## Skipped (do not pick up)

- **#39** — JetStream `(process_nonce, shard_id, sequence_start, i)`
  msg-id assumes monotonic `sequence_start` across restarts. Not a
  bug fix; needs persistent-sequence feature work. The audit
  recommends a unit test pinning the monotonicity invariant — that
  test alone is small and may be worth landing as a regression
  guard while the feature is deferred.
- **#97** — `apply_authoritative_grant` clamp to `tx_bytes_sent`. The
  audit-suggested reorder (`bump tx_bytes_sent before decrementing
  tx_credit_remaining`) conflicts with the credit-window invariant
  enforced by `try_acquire_tx_credit`. See commit `f41d9c36` for
  the reasoning. Self-healing per the original analysis; no further
  action.

## Long-term follow-up: total #1 fix

`compact_to`'s three-rename sequence still has a cross-file
mixed-state window (rename N succeeds, crash, recovery sees idx at
gen N+1 but dat/ts at gen N). The WRITE_THROUGH per-call durability
fix in `50ba6ae5` closes the within-rename gap on Windows; the
cross-rename gap is platform-independent.

**Manifest-pointer scheme:**

- New on-disk layout per channel: `<channel>/manifest` (small
  pointer file: `{generation: u64, checksum}`) plus
  `<channel>/v0000000001/{idx,dat,ts}`,
  `<channel>/v0000000002/{idx,dat,ts}`, …
- `compact_to` writes the next generation `vN+1/`, fsyncs each
  file, writes `manifest.tmp`, then a single
  `durable_rename(manifest.tmp → manifest)`. That single rename IS
  atomic on POSIX and (with WRITE_THROUGH) on Windows.
- Recovery: read `manifest`. If torn, fall back to highest
  validated `vN/`. If `manifest` references files that don't all
  exist, fall back. Sweep orphan `vM/` directories.

**Cost:** on-disk format change (bump `RedexFormatVersion` + one-
shot migration on first open of a pre-rework channel); recovery
rewrite in `DiskSegment::open`; every code path that names
`<channel>/idx` etc. routes through a `current_generation_dir()`
helper. ~600–1000 LOC + ~20 crash-injection tests.

**When to pull the trigger:** if power-loss-mid-compact is on the
real risk profile (bare-metal deployments without battery-backed
write cache). For VM / cloud workloads where the host typically
has write-back cache battery-backing, the per-rename WRITE_THROUGH
fix may be sufficient and the manifest rework can wait until the
next major format bump.

## Suggested attack order

If a single agent picks up the work:

1. **Cluster A** (cortex watermark/snapshot, High). Highest
   user-visible blast radius — can permanently corrupt state.
2. **Cluster C** (FFI handle-lifetime, High). Memory-safety;
   blocks Go/Python multi-threaded SDK use cases.
3. **Cluster B** (compute registry quiescence, High). Lower
   blast radius than A but couples with the standby/replica/fork
   group failure handlers.
4. **Workstream D** (#102 envelope version byte) — small, ships
   in an hour, removes a CPU-DoS amplifier.
5. **Workstream E** (#64, #73) — independent fixes.
6. **Workstream F** — polish, do in any order.

If multiple agents pick up the work in parallel, A / B / C / D
are mutually independent and can ship as four concurrent PRs.
E and F can branch off any of them without conflict.
