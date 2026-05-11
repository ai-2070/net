# Code Review — `redex-distributed` vs `master` (2026-05-11)

Review pass on the `redex-distributed` branch (39 files, +13,685 / -127). The
branch implements Phases A–I of [`REDEX_DISTRIBUTED_PLAN.md`](../plans/REDEX_DISTRIBUTED_PLAN.md):
`SUBPROTOCOL_REDEX` wire codec, 4-state replication coordinator, tokio runtime,
pull-based catch-up, deterministic nearest-RTT election, bandwidth budgets,
Prometheus metrics, DST + loom + e2e tests, and cross-binding surfaces (Node /
Python / Go / C).

Overall the architecture is sound and the plan + code are tightly aligned.
The pure-logic modules (`replication_state`, `replication_election`,
`replication_heartbeat`, `BandwidthBudget`, the wire codec) are defensively
coded with strong unit-test coverage. Risk concentrates in two areas: (a) the
async runtime/coordinator integration, and (b) the cross-binding surface where
typed errors and feature-gating leak.

Tagged `[H | M | L]`:
- H — runtime / wire-protocol correctness gap.
- M — operator-visible footgun or robustness hole.
- L — hygiene, dead code, doc drift.

## Status

| ID    | Pri | Area        | Title                                                                     | Status |
|-------|-----|-------------|---------------------------------------------------------------------------|--------|
| R-1   | H   | runtime     | Role-flip TOCTOU between `coordinator.role()` check and catchup dispatch  | ✅ |
| R-2   | H   | runtime     | `clear_believed_leader` runs after failed Candidate transition            | ✅ |
| R-3   | H   | coordinator | Chain-tag side effects not serialized between concurrent transitions     | ⚪ |
| R-4   | H   | runtime     | NACK `NotLeader` / `BadRange` handlers are placeholder TODOs              | ⚪ |
| R-5   | H   | wire        | Replica cannot disambiguate retention-trim from split-brain divergence    | ⚪ |
| R-6   | H   | py binding  | `replication=False` silently drops every other `replication_*` kwarg     | ⚪ |
| R-7   | H   | bindings    | `enable_replication` silently absent without `net` feature                | ⚪ |
| R-8   | H   | FFI         | `net_redex_enable_replication` leaks `Box<Arc<MeshNode>>` on error paths  | ⚪ |
| R-9   | H   | tests       | DST harness `wall_clock_ms` reads real wall-clock time, not step counter  | ⚪ |
| R-10  | M   | runtime     | `current_role` captured before tracker lock; `tick()` runs against stale  | ✅ |
| R-11  | M   | runtime     | `cancel()` race: `is_stopped()` can return true before task joined        | ⚪ |
| R-12  | M   | runtime     | Only `Heartbeat` validates `channel_id`; other inbound types don't        | ✅ |
| R-13  | M   | runtime     | `GapBeforeChunk` underflow if `first_seq <= local_next`                   | ✅ |
| R-14  | M   | runtime     | Dispatcher `Arc` cycle: MeshNode → router → handle → task → dispatcher    | ⚪ |
| R-15  | M   | step        | Lag-driven `SyncRequest` doesn't filter `believed_leader != self`         | ⚪ |
| R-16  | M   | step        | Dropped-leader leaves Replica without a Candidate path                    | ⚪ |
| R-17  | M   | catchup     | Empty chunk silently accepts any `first_seq`                              | ⚪ |
| R-18  | M   | catchup     | `prev + 1` u64 wrap in monotonicity loop without `checked_add`            | ⚪ |
| R-19  | M   | catchup     | 64 MiB hard ceiling not enforced for oversize first event                 | ⚪ |
| R-20  | M   | catchup     | `append_batch` error stringified — typed variants erased                  | ⚪ |
| R-21  | M   | manager     | `try_dispatch(Shutdown)` on reopen path is lossy at cap-1024              | ⚪ |
| R-22  | M   | manager     | `NIC_PEAK_BYTES_PER_S = 125_000_000` hardcoded without `// TODO` tag      | ⚪ |
| R-23  | M   | wire        | `WireError::Truncated.need` formula reports nonsensical value             | ⚪ |
| R-24  | M   | mesh        | Replication inbound dispatch is O(peers) per frame                        | ⚪ |
| R-25  | M   | mesh        | `from_node` falls back to `0`, a valid NodeId sentinel collision          | ⚪ |
| R-26  | M   | bindings    | Python silently accepts both `colocation_strict` and `colocation-strict`  | ⚪ |
| R-27  | M   | bindings    | `Pinned([])` slips past binding to core validate                          | ⚪ |
| R-28  | M   | bindings    | `leader_pinned` not cross-checked against `pinned_nodes` at binding layer | ⚪ |
| R-29  | M   | node binding| `redex_err` produces untyped `from_reason`; no `RedexError` class on JS   | ⚪ |
| R-30  | M   | go binding  | `OpenFile` returns `ErrReplicationRequiresEnable` for any redex error     | ⚪ |
| R-31  | M   | go binding  | `RedexFile.mu` serializes appends; Rust substrate supports concurrent     | ⚪ |
| R-32  | M   | file        | `skip_to` swap-order: index assigned before `evict_prefix_to`             | ⚪ |
| R-33  | M   | mod         | `pub mod replication` AND flat re-exports of the same types               | ⚪ |
| R-34  | M   | metrics     | `Duration → micros` saturation may collide with `LAG_NOT_OBSERVED`        | ⚪ |
| R-35  | L   | election    | `sort_by` claims to use stability for determinism; should be `unstable`   | ⚪ |
| R-36  | L   | wire codec  | `Vec::with_capacity(event_count.min(4096))` cap is undocumented           | ⚪ |
| R-37  | L   | wire codec  | `u32::try_from(...).unwrap_or(u32::MAX)` silently corrupts data           | ⚪ |
| R-38  | L   | wire codec  | Byte-layout test doc-comment reverses LE order vs asserted bytes          | ⚪ |
| R-39  | L   | loom        | `burst_cas_decrement` comment says "three threads"; code spawns 2         | ⚪ |
| R-40  | L   | dst         | `_unused_imports_workaround` is cargo-cult silencing                      | ⚪ |
| R-41  | L   | e2e         | Fixed 500ms `tokio::time::sleep` before leader close — flake risk         | ⚪ |
| R-42  | L   | node binding| `.d.ts` unconditionally emits `enableReplication` despite `cfg` gating    | ⚪ |
| R-43  | L   | go binding  | `typedef ArcMeshNode` collides with upstream `net_compute_mesh_arc_t`     | ⚪ |
| R-44  | L   | go binding  | `ErrInvalidReplicationConfig` is dead code (never returned)               | ⚪ |
| C-1   | M   | tests       | No DST scenario exercises "election storms" (plan §F)                     | ⚪ |
| C-2   | M   | tests       | Divergence-freedom check only runs on happy path, not after fault         | ⚪ |
| C-3   | M   | tests       | `chain_discovery.rs` tests are single-node only                           | ⚪ |
| C-4   | L   | tests       | No FFI test exercises `net_redex_enable_replication` success path        | ⚪ |

---

## H — fix before merge

### R-1: Role-flip TOCTOU in `replication_runtime` inbound handlers

**Location:** `net/crates/net/src/adapter/net/redex/replication_runtime.rs` (inbound dispatch arms for SyncRequest / SyncResponse).

`coordinator.role()` is sampled, then `handle_sync_request` / `apply_sync_response`
runs against the file with the role check stale. A concurrent transition
(`DiskPressureWithdraw`, peer concession, graceful relinquish) can flip this node
from Leader → Idle/Replica between the check and the dispatcher send. Peers
receive a `SyncResponse` from a node that no longer claims leadership; if the
node is now `Idle` it may also race-respond from a stale `RedexFile`.

**Fix:** Re-check `coordinator.role()` immediately before issuing the outbound
dispatch, and abandon the response (NACK `NotLeader`) if it changed. Same shape
applies to the replica-side `SyncResponse` apply path — re-check role and skip
the apply if we are no longer `Replica`.

### R-2: `clear_believed_leader` runs after failed Candidate transition

**Location:** `replication_runtime.rs` — the post-Candidate election fast-path.

After `coordinator.transition_to(pt.target, pt.signal).await` runs, the result
is logged-and-dropped on failure. But `tracker.lock().clear_believed_leader()`
runs unconditionally afterwards. If a concurrent caller has already advanced the
coordinator's state, the believed leader is wiped while the coordinator is still
in the previous role — the replica enters a state where it has no leader to
follow and no election trigger to enter Candidate again.

**Fix:** Gate `clear_believed_leader()` on the success branch of `transition_to`.

### R-3: Chain-tag side effects not serialized between concurrent transitions

**Location:** `net/crates/net/src/adapter/net/redex/replication_coordinator.rs`
— `transition_to`.

The state `Mutex` is released before the async `announce_chain` /
`withdraw_chain` call on the `ChainTagSink`. Two racing `transition_to` calls
can interleave: T1 sets `Replica` and starts `announce_chain`; T2 sets `Idle`
and runs `withdraw_chain` (completes first); T1's `announce_chain` lands
afterwards. The capability layer ends up advertising a chain that has been
locally withdrawn.

**Fix:** Hold a dedicated `tokio::sync::Mutex` across the entire transition
(state write + sink call) so the two operations are serialized.

### R-4: NACK `NotLeader` / `BadRange` handlers are placeholder TODOs

**Location:** `replication_runtime.rs` — `on_inbound(SyncNack)` arms.

The `NotLeader` arm has a comment "would clear if we had a handle here — plumb
through later slice" but `tracker` is already in scope. Currently the handler
does nothing, so the replica continues sending `SyncRequest`s to the same stale
leader until the heartbeat-silence timer trips (plan §6 hysteresis = 3 ×
heartbeat_ms; default 1.5 s of wasted requests). The `BadRange` arm increments
the metric and "defers" — but the local tail isn't trimmed, so the next
`SyncRequest` fails identically, looping until heartbeat resets state.

**Fix:** `NotLeader` should call `tracker.clear_believed_leader()` so the next
tick re-resolves via `find_chain_holders`. `BadRange` should call
`file.skip_to(0)` (or the leader's advertised first_seq if we extend the wire
per R-5) and re-issue the request.

### R-5: Replica cannot disambiguate retention-trim from split-brain divergence

**Location:** `net/crates/net/src/adapter/net/redex/replication_catchup.rs` —
`SyncResponse` apply path where `first_seq > local_next`.

When the leader's retention trimmed past the replica's tail, the response
carries `first_seq > since_seq`; the replica triggers `skip_to(first_seq)` and
re-applies. When the leader is on a **divergent** log (post-partition with
independent writes), the same `first_seq > local_next` shape appears — but the
replica should NOT skip; it should reject and let the operator decide. There is
no information on the wire to distinguish these.

**Fix:** Add `leader_first_retained_seq: u64` to `SyncResponse`. Replicas treat
`first_seq == leader_first_retained_seq` as a legitimate retention trim and
proceed with skip-ahead; any other case is a divergence signal — bump
`dataforts_replication_skip_ahead_total`, log loudly, and NACK back the leader.

### R-6: Python `replication=False` silently drops every other `replication_*` kwarg

**Location:** `net/crates/net/bindings/python/src/cortex.rs` — `Redex.open_file`.

If an operator writes `redex.open_file("foo", replication_factor=5)` (forgetting
`replication=True`), the channel opens single-node with zero diagnostic. Every
other kwarg sits inside the `if replication { ... }` block.

**Fix:** Add a binding-side guard: if any `replication_*` kwarg is `Some` while
`replication=False`, raise `RedexError("replication_factor/heartbeat_ms/etc. specified without replication=True")`.

### R-7: `enable_replication` silently absent without `net` feature

**Location:** `net/crates/net/bindings/node/src/cortex.rs`,
`net/crates/net/bindings/python/src/cortex.rs`.

Both bindings gate `enable_replication` on `#[cfg(feature = "net")]`. A
downstream wheel/build without `net` will raise
`TypeError: redex.enableReplication is not a function` (Node) or
`AttributeError` (Python) — not a typed `RedexError` with a clear "feature
required" message. Python additionally exposes
`replication_runtime_count` / `replication_prometheus_text` without the gate,
producing the contradictory surface "you can observe a feature that can't be
enabled."

**Fix:** Add `#[cfg(not(feature = "net"))]` stubs that return
`RedexError("redex: enable_replication requires the `net` feature; rebuild with --features net")`.
Same gate on `replication_runtime_count` / `replication_prometheus_text` in
Python (already correct in Node).

### R-8: `net_redex_enable_replication` leaks `Box<Arc<MeshNode>>` on error paths

**Location:** `net/crates/net/src/ffi/cortex.rs` —
`net_redex_enable_replication`.

The function contract claims to consume `mesh_arc` on success. The two error
returns (`redex.is_null()`, `try_enter()` failure) silently leak the boxed Arc
— the Go caller has already given up ownership and won't call
`net_mesh_arc_free` on `rc != 0`.

**Fix:** Free `mesh_arc` on every error path before returning the rc. Update
the doc-comment to explicitly state "consumed regardless of return code."

### R-9: DST harness `wall_clock_ms` reads real wall-clock time

**Location:** `net/crates/net/tests/redex_replication_dst.rs:309`.

`self.now.elapsed().as_millis()` is `Instant::now() - self.now_initial`, i.e.
real wall-clock delta — not a function of the harness's step counter. Currently
`wall_clock_ms` isn't consumed for ordering, so behavior is stable; the file's
"explicit clock + message queue" claim is technically false, and any future
logic that reads `wall_clock_ms` will silently break determinism.

**Fix:** Use a separate `step_counter * STEP_DURATION_MS` derivation, or
`self.now.duration_since(initial_now).as_millis()` with `initial_now` stored on
the cluster.

---

## M — fix before broad rollout

### R-10: Stale `current_role` capture in `tick()` driver
`replication_runtime.rs` — `current_role = coordinator.role()` is captured
before `tracker.lock()`. Between the two reads a transition can land; `tick()`
then drives outbound for the wrong role.

**Fix:** Capture `current_role` inside the same critical section that holds
the tracker lock, or pass a snapshot helper that takes both atomically.

### R-11: `cancel()` race; `is_stopped()` can lie
`replication_runtime.rs` — two concurrent `cancel()` calls race: thread A sends
Shutdown and takes the handle; thread B sends a second Shutdown (silent-err on
closed receiver), then `task.lock().take()` returns `None` and `is_stopped`
returns `true` before the task has actually joined.

**Fix:** Make `is_stopped` consult an explicit `AtomicBool` flipped only after
`task.await` completes, regardless of who holds the JoinHandle.

### R-12: Channel-id validation only on Heartbeat
`replication_runtime.rs` — only the `Heartbeat` arm validates
`msg.channel_id == inputs.channel_id`. SyncRequest / SyncResponse / SyncNack
rely on the inner helpers. Defense-in-depth: validate at the runtime boundary.

### R-13: `GapBeforeChunk` underflow
`replication_runtime.rs` — `gap = first_seq - local_next` will wrap on `u64` if
`first_seq <= local_next`. The match guard doesn't enforce
`first_seq > local_next`; the invariant is documented only on the error type.

**Fix:** `debug_assert!(first_seq > local_next)` in the arm, and a saturating
`first_seq.saturating_sub(local_next)` for the gap.

### R-14: Dispatcher `Arc` cycle
The production path is `MeshNode → ReplicationInboundRouter →
ReplicationRuntimeHandle → task → Arc<dyn ReplicationDispatcher = MeshNode>` —
a strong reference cycle. Today `Redex::drop` un-installs the router which
releases the cycle, but only if drop ordering is correct.

**Fix:** Store the dispatcher in the runtime as `Weak<dyn ReplicationDispatcher>`
and upgrade on use. If upgrade fails, log+drop the outbound (the runtime is on
its way out anyway).

### R-15: Lag-driven `SyncRequest` doesn't filter `believed_leader != self`
`replication_step.rs` — if `record_heartbeat` was ever called for self with
`role = Leader` (test setup, loopback misroute), `believed_leader()` returns
self and `tick()` emits a `SyncRequest` to self.

**Fix:** `if let Some(leader) = tracker.believed_leader().filter(|&l| l != self_node_id)`.

### R-16: Dropped-leader leaves Replica stuck
`replication_step.rs` — if the leader's tracker entry is dropped (no entry, no
`record_heartbeat` for that NodeId) while we're still in `Replica`,
`is_leader_silent` returns `false` (no leader to be silent about), so we never
enter Candidate. The Replica is permanently stuck.

**Fix:** Trigger Candidate on `(believed_leader.is_none() && elapsed_since_last_leader_seen > silent_threshold)`
in addition to `is_leader_silent`.

### R-17: Empty `SyncResponse` chunk silently accepts any `first_seq`
`replication_catchup.rs` — `apply_sync_response` short-circuits on
`response.events.is_empty()` without validating `first_seq`. A leader bug
emitting `first_seq = 999` on an empty chunk is silently accepted.

**Fix:** Validate `first_seq >= local_next` on the empty branch (or `first_seq
== local_next` if the contract is strictly "first_seq must align").

### R-18: `prev + 1` u64 wrap
`replication_catchup.rs` — strict-monotonicity loop computes `prev + 1` without
`checked_add`. Practically unreachable; surrounding code uses `saturating_*`,
so the asymmetry is the real bug.

**Fix:** `prev.checked_add(1).ok_or(ApplyError::NonMonotonic)?` (or saturate).

### R-19: 64 MiB hard ceiling not enforced for oversize first event
`replication_catchup.rs` — the "admit at least one event" branch admits the
first event unconditionally, including events larger than the 64 MiB hard
ceiling. The doc-comment defers to "the replica's local append will accept or
reject," but the leader's wire bytes are already in flight.

**Fix:** Add a per-event size sanity check at assembly. If
`payload.len() > HARD_CEILING`, NACK `BadRange` instead of shipping.

### R-20: `append_batch` error stringified — typed variants erased
`replication_catchup.rs` — `apply_sync_response` maps any `append_batch` failure
to `ApplyError::AppendFailed(format!("{e:?}"))`. The disk-pressure routing in
`handle_disk_pressure` (runtime.rs) expects typed signals; stringified errors
fall through to the generic log+drop arm.

**Fix:** Add `impl From<RedexError> for ApplyError` that preserves the typed
variant (or split `AppendFailed` into typed sub-variants).

### R-21: `try_dispatch(Shutdown)` on reopen is lossy
`manager.rs` — on the reopen path, `register` returns the prior handle; the
caller sends `Shutdown` via `try_dispatch`. At cap-1024 inbox this can return
`Err(...)` silently. The Arc-dropping mechanism (unregister releasing the only
sender) does also shut the task down, but the redundant `try_dispatch` is
either belt-and-suspenders or a comment-needed clarification.

**Fix:** Add a clarifying comment: "Shutdown is best-effort; the unregister
above already dropped the inbox sender, so the task observes a closed receiver
on its next poll." Or convert to `tokio::spawn(async move { prev.cancel().await })`.

### R-22: `NIC_PEAK_BYTES_PER_S` placeholder needs grep-able TODO
`manager.rs` — `const NIC_PEAK_BYTES_PER_S: u64 = 125_000_000` is documented
as "until plan §6 lands" but not tagged.

**Fix:** Add `// TODO(plan-§6): wire the proximity-graph throughput measurement here.`

### R-23: `WireError::Truncated.need` formula is nonsensical
`net/crates/net/src/adapter/net/redex/replication.rs` — in two arms (event-loop
header and payload bytes), the `need` field is computed as
`data.len() + (expected - cursor.remaining())` which mixes "consumed bytes" with
"still-needed bytes." Other truncation sites correctly use the encoded-size
constants.

**Fix:** `need = (data.len() - cursor.remaining()) + payload_len` (consumed-so-far + still-needed)
in the event-loop arms.

### R-24: Replication dispatch is O(peers) per inbound frame
`net/crates/net/src/adapter/net/mesh.rs` — the new `SUBPROTOCOL_REDEX` arm
does a linear scan of `ctx.peers` to resolve `session_id → node_id`. The
standard event path uses the O(1) `addr_to_node` lookup.

**Fix:** Use the same O(1) shape.

### R-25: `from_node == 0` sentinel collision
`mesh.rs` — `from_node` falls back to `0` if no peer matches the session. `0`
is a valid `NodeId` (`MeshNodeConfig::new(addr, [0u8; 32])` produces one).
The reflex handler at `mesh.rs:3441-3443` rejects this; the replication arm
doesn't.

**Fix:** Mirror the reflex handler — `if from_node == 0 { return; }`.

### R-26: Python accepts both `colocation_strict` and `colocation-strict`
`bindings/python/src/cortex.rs` — placement and on-under-capacity parsing
accept both spellings. Same for `evict_oldest` / `evict-oldest`. This makes
error messages inconsistent and round-tripping non-canonical.

**Fix:** Accept only the snake-case form (matches Python convention). Reject
the kebab-case form with a clear error.

### R-27: `Pinned([])` slips past binding to core
Both bindings accept `placement="pinned"` with empty `pinned_nodes = []` and
rely on core `validate()` to reject. Binding-local check would be clearer.

**Fix:** Reject empty `pinned_nodes` at the binding layer with
`RedexError("replication_pinned_nodes must be non-empty when placement is pinned")`.

### R-28: `leader_pinned` not cross-checked at binding layer
Both bindings accept `leader_pinned = Some(X)` without verifying `X ∈ pinned_nodes`
when placement is Pinned. Core catches it, but later-and-worse error message.

**Fix:** Cross-check at the binding layer.

### R-29: Node `redex_err` is untyped
`bindings/node/src/cortex.rs:64-66` — builds plain `napi::Error::from_reason("redex: ...")`.
JS-side has no `RedexError` class; operators string-sniff on `e.message.startsWith("redex:")`.
Python has a typed `RedexError` exception.

**Fix:** Document the prefix as the stable contract in `index.d.ts`, or
construct a typed `RedexError` JS class via `napi::Error::new` with a custom
status. Minimum: pin the prefix in the binding's doc-comment.

### R-30: Go `OpenFile` returns `ErrReplicationRequiresEnable` for any redex error
`bindings/go/net/redex.go` — every `NET_ERR_REDEX` is mapped to
`ErrReplicationRequiresEnable`. The defined `ErrInvalidReplicationConfig` is
never returned. Operators debugging an invalid factor get a misleading message.

**Fix:** Inspect the C error detail buffer and route to the appropriate Go
sentinel, or expose distinct rc codes from Rust.

### R-31: Go `RedexFile.mu` serializes appends
`bindings/go/net/redex.go` — `mu.Lock` around `C.net_redex_file_append`
serializes all I/O per file even though the Rust substrate supports concurrent
writers (`HandleGuard` is a reader-counter).

**Fix:** Use `sync.RWMutex` — RLock on append / NextSeq / Read, Lock on Close.

### R-32: `skip_to` swap-order leaves index referencing pre-eviction offsets on panic
`net/crates/net/src/adapter/net/redex/file.rs` — `skip_to` (and the existing
`sweep_retention`) swap the index FIRST, then call `evict_prefix_to`. If
`evict_prefix_to` panics, the index references payload offsets that no longer
exist.

**Fix:** Build new index into a temp `Vec`, run `evict_prefix_to` (or its
equivalent) against the segment, then assign the new index. `evict_prefix_to`
is panic-free today; the reordering is defense-in-depth.

### R-33: `mod replication` dual public surface
`net/crates/net/src/adapter/net/redex/mod.rs` — `pub mod replication;` is
declared AND the wire codec types are re-exported flat under `redex::`. Two
import paths for the same types is a versioning trap.

**Fix:** Drop the `pub mod replication;` declaration and keep only the flat
re-exports (matches the rest of the module surface).

### R-34: Lag saturation may collide with `LAG_NOT_OBSERVED`
`net/crates/net/src/adapter/net/redex/replication_metrics.rs` — the
`try_from(lag.as_micros())` saturation produces `u64::MAX - 1` which differs
from the `LAG_NOT_OBSERVED = u64::MAX` sentinel by exactly one. A follow-up
arithmetic operation could collide.

**Fix:** Pin the saturation value with a named constant `LAG_SATURATED_MICROS`
and add a test asserting the gap from the sentinel is preserved.

---

## L — hygiene

### R-35: Election `sort_by` claims stability for determinism
`replication_election.rs` — comment claims `sort_by` stability provides
determinism; actually the total compound key (`(rtt, node_id)`) does. Use
`sort_unstable_by` for the perf benefit; update the comment.

### R-36: Event-vec preallocation cap undocumented
`replication.rs` — `Vec::with_capacity(event_count.min(4096))` caps at 4096 to
bound a hostile event_count, but a legitimate 1 MiB chunk of small events can
push past the cap. Document the cap rationale.

### R-37: Saturating `u32::try_from` corrupts data silently
`replication.rs` — `u32::try_from(payload_len).unwrap_or(u32::MAX)` silently
truncates for slices > u32::MAX. Add `debug_assert!` for accidental misuse.

### R-38: Byte-layout test comment reverses LE order
`replication.rs` — comment "0x0E, 0x00, 0x20" precedes the assertion of `[0x00,
0x0E, 0x20]`. Fix the comment to match LE.

### R-39: `burst_cas_decrement` thread count mismatch
`loom_models.rs` — comment "three threads racing"; loop spawns 2.

### R-40: `_unused_imports_workaround` cargo-cult
`tests/redex_replication_dst.rs` — remove the dummy function or use the
imports.

### R-41: Fixed `tokio::time::sleep` before leader close
`tests/redex_replication_e2e.rs` — fixed 500ms wait before kill is the one
blind sleep in the file; replace with a poll loop on `believed_leader()`.

### R-42: `.d.ts` emits `enableReplication` unconditionally
`net/crates/net/bindings/node/index.d.ts` — the `#[cfg(feature = "net")]`
gating doesn't propagate to the regenerated `.d.ts`. Either pin the build
features in the doc-comment, or split the `.d.ts` per feature matrix.

### R-43: Go `typedef ArcMeshNode` alias
`bindings/go/net/redex.go:92` — `typedef struct ArcMeshNode ArcMeshNode;`
collides with the upstream header's `net_compute_mesh_arc_t` for the same
underlying type.

**Fix:** Use the upstream name in the cgo block.

### R-44: `ErrInvalidReplicationConfig` dead code
`bindings/go/net/redex.go` — defined but never returned. Wire it through
once R-30 is addressed.

---

## Coverage gaps

### C-1: DST has no "election storms" scenario
Plan §F lists 4 explicit scenarios; 3 are covered. Election storms (rapid
back-to-back leader loss + thrash bounded by hysteresis) is absent. Add a
scenario that triggers ≥3 consecutive elections within 30 s and asserts
`dataforts_replication_election_thrash_total` bumps.

### C-2: Divergence-freedom only on happy path
`divergence_freedom_no_two_replicas_hold_different_payload_at_same_seq` runs a
real byte-for-byte check, but only after happy-path catch-up. Add the same
check after `partition_heal` and `restart_during_sync`.

### C-3: `chain_discovery.rs` is single-node only
Every test exercises `find_chain_holders` against the local node's own
announces. Multi-peer paths are untested at this layer.

### C-4: No FFI test for `net_redex_enable_replication` success path
All 6 new FFI tests assume "replication not enabled." Add a test that
constructs a `Box<Arc<MeshNode>>`, calls `enable_replication`, observes
`replication_runtime_count > 0` after `open_file`.
