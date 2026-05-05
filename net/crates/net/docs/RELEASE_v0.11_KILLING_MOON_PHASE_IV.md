# Net v0.11 — "Killing Moon" Phase IV

v0.11 closes the audit work that v0.10 left open. Same shape: a hardening release with no new transports, no new SDK surfaces, no new feature gates. Every commit on this branch is a bug fix, a regression test, a triage decision, or a wire-format bump that closes a structural gap the previous release flagged but couldn't ship inside its envelope.

The work is sourced from two queues that v0.10 explicitly deferred: the 22 unfixed items from `BUG_AUDIT_2026_05_03.md` (1 critical, 7 high, 13 medium-or-lower; the audit's "5" lumps the FFI handle-lifetime cluster as one) and the 9-item single-file deep read of `adapter/net/mesh.rs`. Both are now drained: every High closed, every Medium either closed or triaged with a written reason, every Low resolved.

Two of the closures required a wire-format bump that v0.10 deliberately avoided. They land here together because amortizing them across one upgrade cycle costs operators less than two: the `IdentityEnvelope` gains a leading version byte, and the application-layer `origin_hash` is widened from 32 to 64 bits. v0.10 ↔ v0.11 nodes do not interop on the wire — see *Breaking changes* and *How to upgrade*.

---

## Addressed in this release

### CortEX watermark, snapshot, and per-event integrity

- **`folded_through_seq` advanced past unfolded events** — under `Stop` policy, `recoverable_decode` could publish a watermark for events whose state mutation never landed; `wait_for_seq(seq)` returned true incorrectly and downstream readers acted on never-applied state. Split the watermark in two: `applied_through_seq` (strict-prefix, advances only on `Ok(())` AND only when `seq` is the immediate successor of the previous applied) and `folded_through_seq` (live-progress, retained for low-latency observers). `snapshot()` writes `applied_through_seq`; restore re-attempts the previously-skipped event so the post-restore state matches what fold *committed*, not what fold *attempted*.
- **Snapshot persisted `last_seq` for skipped events** — same root cause as the watermark fix above. Once the strict-prefix watermark is the source of truth, snapshots no longer carry sequence numbers for events whose state was never applied; the on-disk log remains the source of truth on restore.
- **Per-event checksum did not cover the EventMeta header** — `compute_checksum(tail)` was xxh3 over only the payload tail; a stray bit-flip in the 20-byte `EventMeta` header (e.g. `dispatch: STORED → DELETED`) was undetected by the per-event integrity check and silently re-routed the event to the wrong fold arm. The new `compute_checksum_with_meta(&meta, tail)` covers both the header (with the `checksum` slot zeroed) and the tail. Producers stamp v2; readers try v2 first and fall back to v1 to keep pre-fix on-disk records readable. Downgrading to a pre-v0.11 binary will skip every event written by a v0.11 producer (the legacy verifier expects `xxh3(tail)`, which v2 records won't match) — the migration is effectively one-way.

### RedEX `compact_to` durability + atomicity (manifest-pointer flip)

Two layered fixes; the first patches per-call durability on Windows, the second closes the cross-file mixed-state window structurally.

- **Per-rename `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`** — `compact_to`'s rename calls used `std::fs::rename`, which on Windows is `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` with no write-through — the destination metadata could be cached and lost on power-loss. Now driven through a `durable_rename` helper that calls `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` on Windows; POSIX is unchanged (`fs::rename` is durable as long as the directory is `fsync`'d, which the surrounding code already does).

- **Cross-file atomicity via manifest-pointer layout.** The old `compact_to` did three sequential renames (`idx`, `dat`, `ts`). A crash between rename N and N+1 left the on-disk channel in a mixed state (`idx` at gen K+1 paired with `dat`/`ts` still at gen K) that recovery could not distinguish from a clean half-finished compact. The new layout puts each generation's files under its own directory and atomically swaps a single manifest pointer:

    ```text
    <channel>/manifest                        # 16-byte pointer file
    <channel>/v0000000001/{idx,dat,ts}        # current live generation
    <channel>/v0000000002/{idx,dat,ts}        # next generation (mid-compact)
    ```

    `compact_to` writes the new generation's files in full, fsyncs them, then `durable_rename(manifest.tmp → manifest)` is the single linearizing event. Before the rename, recovery sees the old manifest and uses `v<N>/`. After it, recovery sees the new manifest and uses `v<N+1>/`. There is no mixed state — every generation directory is either complete or orphaned, never partially live. Recovery falls back to the highest validated `v<NNN>/` if the manifest is torn or missing, and sweeps every generation directory other than the live one on every open (cleaning up orphans left by a crashed prior compact).

    Legacy v0.10 / v0.11 channels with the flat `<channel>/{idx,dat,ts}` layout migrate transparently into `<channel>/v0000000001/{idx,dat,ts}` on first open. The migration is one-shot per channel and idempotent. Pinned by 12 new regression tests including all 10 crash-injection points sketched in `BUG_AUDIT_2026_05_03_REMAINING_PLAN.md`'s long-term-follow-up section. Design recorded in `docs/misc/REDEX_MANIFEST_POINTER_DESIGN.md`.

### Compute registry quiescence

- **In-flight `Arc<Mutex<DaemonHost>>` callers mutated through swap and unregister** — `replace` and `unregister` rotated the registry's `Arc` slot but a concurrent caller that had already cloned the prior `Arc` out of the map (between `get_arc` and `arc.lock()`) would land its mutation on the now-orphaned host. The replacement was correct from the registry's point of view but the orphaned host had already been removed from delivery routing, so writes to it disappeared into nothing. Introduced a `guard_identity(origin_hash, &held_arc)` helper that runs after `arc.lock()` and re-checks `Arc::ptr_eq` against the current registry slot. On mismatch the helper surfaces a typed `DaemonError::Stale(u32)` and the caller bails before mutating; the new variant lets callers distinguish "I lost the swap race" from "the daemon was never registered" without inspecting registry state.

### FFI handle lifetime — cortex, mesh, identity, redis-dedup

A foreign caller (Go cgo, Python threads, Node.js workers) racing a `_free` against an active op against the same handle could (a) UAF on the inner `Arc` after `_free` did `Box::from_raw → drop`, or (b) UAF on the outer handle box itself even when the inner was held alive via an `Arc<Inner>` clone. The shape was filed as three separate audit items because three separate handle families exhibited it; the underlying race is one race.

- **Shared `ffi::handle_guard::HandleGuard` extracted** with `try_enter() -> Option<HandleOp<'_>>` and `begin_free(deadline) -> bool`. Packed atomics (`freeing: AtomicBool`, `active_ops: AtomicU32`); SeqCst-ordered Dekker-style "set freeing, check active_ops" handshake; per-handle `FFI_HANDLE_FREE_DEADLINE: Duration = 5s`. Soundness rule: the handle box is never deallocated once handed to C — `_free` takes the inner out via `ManuallyDrop::take` only after `begin_free` returns true, and the outer Box (carrying `HandleGuard`'s atomics) is intentionally leaked. Concurrent ops doing `try_enter` after free safely fetch_add on still-valid memory, observe `freeing=true`, decrement, and bail.
- **All 11 cortex/mesh/identity/redis-dedup handle types ported.** `RedexHandle`, `RedexFileHandle`, `RedexTailHandle`, `TasksAdapterHandle`, `TasksWatchHandle`, `MemoriesAdapterHandle`, `MemoriesWatchHandle` (cortex side); `MeshNodeHandle`, `MeshStreamHandle` (mesh side, including the `Arc::ptr_eq` UAF in `handles_match` that audit #25 specifically called out); `IdentityHandle`, `RedisDedupHandle`. Every entry point gates on `try_enter`; every `_free` drives `begin_free`. `_free` is idempotent — a second/concurrent `_free` caller observes the lost CAS, returns false, and bails before the double-take that would UAF the inner allocation.
- **Per-handle regression coverage.** Three pinned tests per handle: post-`_free` op returns `ShuttingDown`, `_free` is idempotent under concurrent callers, `_free` waits for an in-flight op to drain (or timeouts and leaks rather than UAF). Plus five tests on the `HandleGuard` helper itself (try_enter, post-free bail, drain-wait, drain-timeout, idempotent concurrent free).

### Identity & envelope

- **`IdentityEnvelope` wire format gains a 1-byte version prefix.** Pre-fix the AEAD `open()` path tried v1, and on failure retried v0 — the documented rolling-upgrade fallback. The new layout puts a single `IDENTITY_ENVELOPE_VERSION = 1` byte at offset 0; readers reject any other byte via `EnvelopeError::UnknownVersion` and skip the AEAD attempt entirely. The CPU-DoS amplification framing in the original audit was overstated (the ed25519 signature check fail-fasts random ciphertext before either AEAD attempt fires; only legitimate-but-replayed v0 envelopes ever reached the second AEAD), but the structural improvement of "version byte at offset 0, deterministic dispatch, no v0 fallback at all" closes the gap with one extra byte. `IDENTITY_ENVELOPE_SIZE` 208 → 209; `SNAPSHOT_VERSION` 1 → 2.
- **`origin_hash` widened from `u32` to `u64` across the application layer.** Pre-fix `EntityKeypair::origin_hash()` returned a 32-bit BLAKE2s projection; with ~65 K distinct daemon identities the birthday probability of two daemons aliasing the same `origin_hash` crossed 50 %, and cross-channel accounting keyed by `origin_hash` silently conflated them. Now widened to 64 bits at the application layer (`EntityKeypair`, `EntityId`, `OriginStamp`, `CausalLink`, `EventMeta`, `ContinuityProof`, `ForkRecord`, `DaemonRegistry`, `daemon_factory`, the SDK's public surface). The per-packet `NetHeader.origin_hash` deliberately stays `u32` — that field is the routing fast path's pre-AEAD filter and width matters for cache-line packing; the `with_origin(u64)` setter downcasts to the routing-side projection. Wire-format constants: `CAUSAL_LINK_SIZE` 28 → 32, `EVENT_META_SIZE` 20 → 24, `CONTINUITY_PROOF_SIZE` 36 → 40.
- **The widening cascade flowed through the SDK, the Node binding (`u64` → JS `bigint`, matching the existing `node_id` convention), the Python binding (pyo3 maps `u64` to native `int` transparently), and the Go binding (`uint32_t` → `uint64_t` in `include/net.go.h`).**

### Compute orchestrator & merge

- **`on_replay_complete` synthesized `target_head` with `parent_hash: 0`** — downstream verifiers couldn't reconcile a chain head whose parent was the literal zero hash; reconciliation surfaced `Forked` against legitimate replay-completion messages. Now queries `daemon_registry.with_host(...)` for the real chain head and stamps the actual parent hash. The audit's separate report against `consumer/merge.rs:384` (per-shard cap rolling the cursor backward on `unclamped_per_shard > PER_SHARD_FETCH_CAP`) was re-triaged as obsolete: the current code already advances the cursor to the last fetched event id; the audit was reading a prior revision. Pinned by a new regression test (`poll_merger_does_not_stall_on_single_shard_filter_under_cap`).

### Mesh transport — `mesh.rs` deep-read audit

The 9 items the v0.10 release note flagged "queued for the next release" all land here.

- **`spawn_heartbeat_loop` held a DashMap shard guard across `.await`** — the heartbeat broadcast loop iterated `peers.iter()` and awaited `socket.send_to(...)` (heartbeat then pingwave, twice per peer) while still holding the iterator's `Ref`. Every other task touching the same shard blocked for the cumulative round-trip. Now snapshots `(node_id, addr, Arc<NetSession>)` tuples into a `Vec` first and awaits without the iterator alive.
- **`accept` / `start` mutual exclusion used `AcqRel` where the comment relied on SeqCst** — the doc-comment argued correctness from "the SeqCst total order on these two atomics," but the `accept_in_flight.fetch_add(1, AcqRel)` and the matching `fetch_sub` in `AcceptGuard::drop` were not part of the SC total order. On x86 the LOCK'd RMW happened to fully fence so the race was unobservable; on AArch64 / RISC-V the dispatcher could race `handshake_responder` for the inbound msg1. Both increments now `SeqCst`.
- **Routed-handshake key rotation silently overwrote a live session** — the replay guard only fired for the same `remote_static_pub`; a routed msg1 with a *different* static for the same `peer_node_id` fell through and `peers.insert` overwrote the existing legitimate session. The legitimate peer's subsequent AEAD packets (encrypted under the old session key) failed to verify and were silently dropped. The trusted-PSK threat model rationalised this only if PSK compromise was treated as "any node can DoS any other node's sessions" — which contradicted the rest of the auth surface (entity-ID TOFU pinning, signed capability announcements). Rotation is now refused while the existing session is still within its idle / heartbeat window.
- **`handle_routed_handshake` `peers.get` → `peers.insert` was not atomic** — two concurrent routed handshakes for the same `peer_node_id` (e.g. a flaky peer retrying under a fresh ephemeral) could both pass the replay-guard `existing.remote_static_pub` check and race the insert; the loser's `pending_handshakes` initiator state stayed armed waiting for a msg2 now bound to the winner's session, until `handshake_timeout` fired. Decision and insert now hold a single `peers.entry(peer_node_id)` write guard.
- **`commit_reclassify_observations` torn `(nat_class, reflex_addr)` snapshot** — when every probe failed, `latest_reflex == None`. The code still updated `nat_class` (typically to `Unknown`) but left `reflex_addr` at its previous value; subsequent `announce_capabilities_with` reads under `traversal_publish_mu` saw `(fresh class, stale reflex)`. The whole `traversal_publish_mu` invariant was silently violated on this branch. `reflex_addr` is now reset to `None` when `latest_reflex` is `None`, keeping the pair coherent.
- **`authorize_subscribe` rejected idempotent re-subscribes with `TooManyChannels`** — a peer at the channel cap that retransmitted/re-subscribed to a channel it already held was rejected even though `SubscriberRoster` is set-typed and the operation is a no-op. Now short-circuits `(true, None)` when the roster already contains the channel, before the cap-check fires.
- **`publish_to_peer` did not propagate the reliable flag to the packet header** — every other sender (`send_to_peer`, `send_routed`, `send_on_stream`, `mod.rs:1016/1063`) computed `if reliable { PacketFlags::RELIABLE } else { PacketFlags::NONE }` and threaded it into the packet builder; `publish_to_peer` hard-coded `PacketFlags::NONE` and only fed `reliable` into `open_stream_with`. Latent today (the dispatch path doesn't yet inspect `flags.is_reliable()`) but the per-call-site inconsistency would silently bite when a receiver-side path consults the packet flag — `proxy.rs` / `route.rs` / `router.rs` already inspect `is_priority` / `is_control`. Same ternary as the other senders now applied.
- **`process_local_packet` migration loopback unbounded synchronous self-bounce** — the in-place `pending: VecDeque` kept draining as long as the handler emitted self-bound follow-ups. A buggy or attacker-influenced trusted handler that always emitted a self-bound message would spin the dispatch task synchronously, starving every other peer's packets. Now caps loopback depth (`tracing::warn!` past it).
- **`connect_via` did not refresh `addr_to_node` after a successful direct upgrade** — after `connect_direct → connect_via(peer_reflex, …)` succeeded, the upgraded session's dispatch fast path missed on `peer_reflex` and fell back to a linear `peers.iter().find(|e| session_id == ...)` per packet. Performance only, but it defeated the addr → nid index for exactly the sessions that benefit most from it. The `connect_direct` `Ok` path now inserts the `(peer_reflex, peer_node_id)` mapping; the relayed-session note in `connect_via` itself is unchanged (the upgrade is a separate caller).

### Behavior / safety / rate limiting

- **`per_source.clear()` minute-boundary RPM cap exceedance** — the periodic sweep cleared the per-source rate-bucket map at the minute boundary, which momentarily zeroed every active source's count and let the next 60 seconds of traffic through unmetered before the budget gate observed it again. Replaced with a packed-atomic `RateBucket` carrying `(window_floor: u32, count: u32)` in a single `AtomicU64`; CAS-based atomic reset on window rollover, no clear-and-reinsert race, no stale-count window. `gc_per_source_stale` now sweeps stale entries based on observed window age rather than stomping the live state. `try_acquire` computes its `Ok` value from the CAS `prev`, not a racy reload — avoids a second lost-update window.

### Cluster F triage (lower-severity items)

- **#81 `adapter/redis.rs` pipeline timeout duplicate hazard** — config-deployment-shape issue; closed with a one-time-per-process `tracing::warn!` from `RedisAdapter::init` pointing at `net_sdk::RedisStreamDedup` so misconfigured deployments are surfaced at boot rather than as silent duplicate publishes under retry.
- **#125 `behavior/safety.rs` per-source RPM cap** — closed via the packed-atomic `RateBucket` rework above.
- **#127 initiator handshake `HandshakePacer`** — re-triaged as obsolete; the structural fix (per-(peer, us) in-flight handshake registry) is a separate refactor and the existing per-call timeout already bounds the worst case to a known floor.
- **#128 `router.rs` `notify_one` + permit-stash soundness** — re-triaged as obsolete; the notify-with-stashed-permit pattern is sound vs `notify_waiters` for this use case (all waiters drain at most-once, no lost-wakeup window). Documented in-line so the design rationale survives the next reader.
- **#73 `consumer/merge.rs` per-shard cap rolling cursor backward** — re-triaged as obsolete; current code advances. Pinned by `poll_merger_does_not_stall_on_single_shard_filter_under_cap`.
- **#118 `behavior/rules.rs` rate-limit reset semantics** — re-triaged as obsolete; the current `reset to 1` is the correct semantic (the audit's `reset to 0` would allow `max+1` firings per window).
- **#121 `behavior/loadbalance.rs` P2C with `len == 2`** — re-triaged as obsolete; the degenerate case IS the P2C algorithm with 2 inputs.

### Test hygiene

- **`HandleGuard` race injection** — five tests on the helper module: try_enter, post-free bail, drain-wait, drain-timeout, idempotent concurrent free. Three pinned tests per ported handle (post-free `ShuttingDown`, idempotent `_free`, `_free` waits for in-flight op).
- **Cortex `applied_through_seq` strict-prefix** — five regression tests pinning the watermark advances only on `Ok(())`-and-immediate-successor; snapshot reflects the strict-prefix value; restore re-attempts the previously skipped event (so post-restore state matches what fold *committed*, not what fold *attempted*).
- **`compute_checksum_with_meta` v2 coverage** — pins that v2 detects bit-flips in `dispatch`, `flags`, `origin_hash`, `seq_or_ts`; pins that v1 fallback still accepts pre-fix on-disk records; pins that v1 and v2 of the same input differ for typical tails (so the fold-side fallback can't accidentally accept a v2 record by numerical coincidence).
- **`DaemonRegistry::Stale` quiescing** — five regression tests pinning that an in-flight mutator holding a now-orphan `Arc` surfaces `DaemonError::Stale(u32)` instead of mutating; that `replace` and `unregister` both trip the check; that the surviving in-flight Arc and the fresh registration don't produce two parallel writers.
- **`durable_rename` Windows behavior** — three regression tests pinning the `MoveFileExW(MOVEFILE_WRITE_THROUGH)` path on Windows and the POSIX fast-path passthrough.
- **Identity envelope version-byte rejection** — pins that envelopes with any leading byte other than `IDENTITY_ENVELOPE_VERSION = 1` surface `EnvelopeError::UnknownVersion` and never reach the AEAD path.
- **Mesh-audit regression coverage** — the heartbeat snapshot, `accept`/`start` SeqCst, routed-handshake atomic entry, NAT class/reflex coherence, idempotent re-subscribe, reliable flag propagation, loopback depth cap, and `addr_to_node` direct-upgrade refresh each carry a pinned regression test in `tests/mesh_audit.rs`.
- **JetStream msg-id `sequence_start` per-shard monotonicity** — pins that within one bus instance, every shard's batches advance their `sequence_start` strictly monotonically AND gap-free (`seq_start[n+1] == seq_start[n] + len(events[n])`). A regression that introduced a gap would let `(process_nonce, shard, seq, i)` tuples be reused after the JetStream / Redis dedup window closes; an overlap would silently overlay a later batch on an earlier one's slot. Pinned by `bus::tests::sequence_start_is_per_shard_monotonic_and_gap_free`. The cross-restart variant (persistent `next_sequence` across process boots) remains feature-shaped and is not in this release; today's invariant relies on `process_nonce` rotating to disjoin the msg-id namespace.
- **Manifest-pointer crash-injection** — 12 regression tests covering manifest codec round-trip + corruption rejection, brand-new-channel init, flat-layout migration, fallback when manifest is missing or torn, sweep of orphan newer / older generation directories, generation advancement + manifest atomicity, and recovery convergence in one open. Maps onto the 10-row crash-injection table in `docs/misc/REDEX_MANIFEST_POINTER_DESIGN.md`.

---

## Triage decisions recorded in code

One audit item resolved as "no code change needed, but the rationale must live in code so a future contributor doesn't re-open the question":

- **`apply_authoritative_grant` clamp ordering** — the audit recommended reordering the `tx_bytes_sent` bump and the `tx_credit_remaining` decrement. The current form uses a CAS-with-delta against `max_consumed_seen` and adds the delta to `tx_credit_remaining` via `fetch_update`; this composes atomically with the CAS in `try_acquire_tx_credit` and the `fetch_update` in `refund_tx_credit`. The audit's reorder presumed a `.store()`-based recompute from a racy snapshot of `tx_bytes_sent` — a shape the current code deliberately avoids. The rationale is documented in code at `adapter/net/session.rs::apply_authoritative_grant` and the codec-side abstract at `adapter/net/subprotocol/stream_window.rs::StreamWindow`.

---

## Known issues — queued for the next release

Both audit queues (`BUG_AUDIT_2026_05_03.md` and `BUG_AUDIT_2026_05_03_MESH.md`) are drained. The next release will pick up structural feature work (`MigrationOrchestrator` placement-policy, persistent JetStream sequence numbering for cross-restart msg-id durability) rather than another bug-fix sweep.

---

## Breaking changes

### Wire format (v0.10 ↔ v0.11 do not interop)

This is the consequential upgrade. Three structural format changes land together; the wire-format pair are NOT backwards-compatible across the wire (v0.10 ↔ v0.11 do not interop), and the RedEX on-disk layout migrates automatically on first open per channel.

#### `IdentityEnvelope` v0 → v1 (208 B → 209 B)

`IdentityEnvelope::to_bytes` now writes a leading `IDENTITY_ENVELOPE_VERSION = 1` byte; `from_bytes` rejects any other leading byte via `EnvelopeError::UnknownVersion`. The v0 fallback in `open()` is removed entirely. `IDENTITY_ENVELOPE_SIZE` is `1 + 32 + 80 + 32 + 64 = 209`.

`SNAPSHOT_VERSION` bumps 1 → 2 because the snapshot wire format embeds the envelope at fixed offsets and the version byte shifts every subsequent field. v0.10's `from_bytes_v0` is removed; `from_bytes_v1` was renamed to `from_bytes_v2`.

**Impact:** v0.10 → v0.11 must upgrade in lockstep. A v0.10 sender to a v0.11 receiver will get `UnknownVersion` on every envelope; a v0.11 sender to a v0.10 receiver will fail signature verification because v0.10 doesn't account for the leading byte in its AAD construction.

#### `origin_hash` widening: `u32` → `u64`

`EntityKeypair::origin_hash()`, `EntityId::origin_hash()`, and `OriginStamp::origin_hash()` now return `u64` (the full 8-byte BLAKE2s value, not a 4-byte truncation). The struct fields `CausalLink.origin_hash`, `EventMeta.origin_hash`, `ContinuityProof.origin_hash`, and `ForkRecord.origin_hash` widen accordingly. The wire-format constants:

| Type | Old size | New size |
|---|---|---|
| `CAUSAL_LINK_SIZE` | 28 | 32 |
| `EVENT_META_SIZE` | 20 | 24 |
| `CONTINUITY_PROOF_SIZE` | 36 | 40 |

`NetHeader.origin_hash` deliberately stays `u32`. That field is the per-packet routing fast path's pre-AEAD filter and width matters for cache-line packing. The setter `with_origin(u64)` downcasts to the routing-side projection (`as u32`); the `OriginStamp::origin_hash()` doc explicitly notes this convention.

The `DaemonRegistry`'s public surface (`register`, `unregister`, `snapshot`, `deliver`, `with_host`, `stats`, `contains`) and the `daemon_factory::FactoryEntry` map are keyed by `u64`. All SDK methods that take or return an `origin_hash` (`DaemonRuntime::stop`, `snapshot`, `deliver`, `migration_phase`, `peek_migration_failure`, `inject_migration_failure`, `subscriptions`, `expect_migration`, `start_migration`, etc.) take/return `u64`. The `DaemonHandle.origin_hash`, `MigrationHandle.origin_hash`, and `CausalEvent.origin_hash` fields widen accordingly.

**Impact:** on-disk RedEX files written by v0.10 cannot be read by v0.11's cortex adapters — the meta header layout shifts. Re-tail from the source of truth (the bus / publisher) on upgrade. The cortex per-event checksum's v1 fallback path keeps reading legacy *checksums*, but the meta-size shift means the byte slicing itself differs.

#### Cortex per-event checksum v1 → v2

Producers stamp `compute_checksum_with_meta(&meta, tail)` (header-covering). Readers try v2 first and fall back to v1 (`compute_checksum(tail)`) so pre-v0.11 records remain readable. New writes are v2-only. Downgrading to a pre-v0.11 binary will skip every event written by a v0.11 producer — the migration is one-way.

#### RedEX on-disk layout: flat → manifest-pointer + generation directories

Each channel's `<base>/<channel>/{idx,dat,ts}` files now live one level deeper at `<base>/<channel>/v0000000001/{idx,dat,ts}`, alongside a single `<base>/<channel>/manifest` pointer file (16 bytes) that names the live generation. Compactions roll the live generation by writing a fresh `v<N+1>/` directory and atomically swapping the manifest.

**Migration is automatic and transparent.** On first open, a v0.10 / v0.11 channel with the flat layout is migrated by renaming each of `{idx,dat,ts}` into `v0000000001/`, then writing a manifest pointing at it. The migration is one-shot per channel and idempotent; failure mid-migration leaves the per-file moves in whichever state they reached and the next open re-runs the migration.

**Tools that read RedEX files directly** (rare; the supported access path is the `RedexFile` API) need to read the manifest first and follow it to the live generation directory. The 16-byte manifest format is documented in `docs/misc/REDEX_MANIFEST_POINTER_DESIGN.md`.

### Rust core (`net` crate) — API surface

- **`origin_hash` types widen to `u64`** at every public API point listed above. The `as u32` downcast at the routing-fast-path boundary (`NetHeader::with_origin`) is the only place in the new code where the projection survives.
- **`DaemonError::Stale(u32)` is a new variant.** Match arms over `DaemonError` need to add it; `#[non_exhaustive]` was already in place so this is forward-compatible, but exhaustive match-on-variant code refuses to compile.
- **`compute_checksum_with_meta(meta: &EventMeta, tail: &[u8]) -> u32`** is a new public function. `compute_checksum(tail: &[u8]) -> u32` remains and is now described as the v1 fallback used only on the read side; new writers must use `compute_checksum_with_meta`. Both are re-exported from `adapter::net::cortex`.
- **`IDENTITY_ENVELOPE_VERSION: u8 = 1`** is a new public constant re-exported from `adapter::net::identity`. Pin against this instead of literal `1` so a future bump auto-propagates.
- **CortexAdapter splits the watermark.** `applied_through_seq` is the new strict-prefix watermark used by `snapshot()`; `folded_through_seq` is the live-progress watermark used by `wait_for_seq`. Existing snapshot consumers that read `last_seq` get the strict-prefix value automatically; tests asserting that `wait_for_seq(seq)` implied `state was applied for seq` need to be re-read against the new semantic (`wait_for_seq` indicates fold *attempted*; restore re-attempts skipped events).
- **`HandleGuard` is a new public module under `ffi::handle_guard`** (`pub mod handle_guard`). Custom FFI wrappers built against the crate (rare — most consumers use the bundled bindings) need to embed `HandleGuard` and route every entry point through `try_enter` / `begin_free` to keep the same memory-safety guarantees the bundled bindings now have.

### Rust SDK (`net-sdk`)

- **All `origin_hash` parameters and fields widen to `u64`.** `Identity::origin_hash() -> u64`. `DaemonHandle.origin_hash: u64`. `MigrationHandle.origin_hash: u64`. Closures `move |origin_hash: u64|` in `PostRestoreCallback`, `PreCleanupCallback`, `MigrationFailureCallback`. `DaemonRuntime::stop`, `snapshot`, `deliver`, `migration_phase`, `peek_migration_failure`, `inject_migration_failure`, `subscriptions`, `subscribe_channel`, `unsubscribe_channel`, `expect_migration`, `start_migration`, `start_migration_with`. The `groups/{fork,replica,standby}` surface widens parent_origin / active_origin / route_event return types. `group_id` in `groups/replica` deliberately stays `u32` — that's a `group_seed` hash, distinct from `origin_hash`.
- **The brute-force u32 collision fixture in `compute_runtime.rs` (`spawn_from_snapshot_checks_full_entity_id_not_just_origin_hash`)** searches for a collision on the `as u32` projection rather than the full u64 — the SDK's identity-mismatch guard fires on the routing-side u32 collision, so the test's intent (entity_id check, not origin_hash check) is preserved at the original ~2^16 birthday-bound runtime.

### FFI / bindings

| Binding | Change |
|---|---|
| **All** | Every FFI handle type (cortex, mesh, identity, redis-dedup) now embeds `HandleGuard`. `_free` is idempotent across all 11 types; entry points after `_free` return typed `ShuttingDown` instead of segfaulting. Behavior change for callers that depended on `_free` being one-shot or used double-free as a way to detect prior frees — those patterns now silently succeed where they previously crashed. |
| **All** | `EntityKeypair::origin_hash()` and friends return `u64`. The bundled bindings handle the marshalling per-language; consumers that called these APIs via raw FFI need to widen the receiving type. |
| **C** (`include/net.go.h`) | `net_identity_origin_hash`, `net_compute_daemon_handle_origin_hash`, `net_compute_migration_handle_origin_hash`, every `net_compute_*` function with an `origin_hash` parameter, all replica/fork/standby out-params, and the cortex `net_tasks_adapter_open` / `net_memories_adapter_open` `origin_hash` parameters are now `uint64_t`. C consumers must widen their typed pointers. |
| **Node** (`@net/sdk`) | The TypeScript surface declares `originHash: bigint` (matching the existing `nodeId: bigint` convention). Existing callers using JS `Number` literals must switch to `BigInt` literals (`0xabcdef01n`) or wrap with `BigInt(value)`. The auto-generated `index.d.ts` reflects the new types. |
| **Python** (`net-py`) | Python `int` is arbitrary precision; the surface is unchanged for callers (PyO3 marshals `u64` ↔ `int` transparently). One pytest fixture literal was extended from `0xdead_beef` to `0xdead_beef_dead_beef` to actually exercise the upper 32 bits. |
| **Go** (`compute-ffi`) | All `origin_hash` parameters and out-params are `uint64_t` in the cgo header; Go callers must use `uint64` typed locals where they previously used `uint32`. |

### Behavioral fixes that may surface as test breakage

These aren't strictly API-breaking but tests that asserted the pre-fix behavior will need updating:

- **Cortex snapshot `last_seq` reflects `applied_through_seq`, not `folded_through_seq`** — tests that asserted snapshots include sequence numbers for skipped events will fail. The strict-prefix semantic is the correct one; the assertion was reading the bug.
- **Cortex restore re-attempts the previously-skipped event** — tests that asserted `state` was preserved verbatim across snapshot+restore (treating the skip as a permanent state change) will see the post-restore state include the re-attempted event. The asymmetric trade-off is documented on `snapshot()`'s rustdoc.
- **`DaemonRegistry::replace` / `unregister` followed by an in-flight mutator returns `DaemonError::Stale(u32)`** — tests that asserted the mutation landed on the orphan host will see the typed error instead.
- **FFI `_free` is idempotent and returns success on second-call** — tests that asserted second-call returned an error code will see success.
- **FFI entry points after `_free` return `ShuttingDown`** — tests that asserted post-free behavior was undefined / panicked will see the typed error.
- **Per-event cortex checksum is the v2 header-covering hash** — tests asserting `meta.checksum == compute_checksum(tail)` (v1) will fail; switch to `compute_checksum_with_meta(&meta, tail)`. Two pinned tests under `tests/integration_cortex_{tasks,memories}.rs` already had this issue and were updated.
- **`IdentityEnvelope::open` rejects v0 envelopes outright** — tests that asserted the v0 fallback path engaged will fail. The `open_accepts_v0_envelope_for_rolling_upgrade_compat` fixture from v0.10 has been removed (it explicitly pinned the now-removed fallback); the new equivalent pins `EnvelopeError::UnknownVersion` on a leading-byte mismatch.
- **Mesh `accept` / `start` use SeqCst on `accept_in_flight`** — tests on AArch64 / RISC-V hardware that relied on the pre-fix race window to construct concurrent-accept-and-start state will see the documented mutual exclusion.
- **Mesh routed-handshake refuses key rotation while a session is live** — tests that asserted the silent overwrite (e.g. simulating a Sybil swap-in via routed msg1) will see the rotation refused.
- **`authorize_subscribe` short-circuits idempotent re-subscribes ahead of the cap-check** — tests that asserted at-cap re-subscribe surfaced `TooManyChannels` will see success instead.

---

## How to upgrade

1. **Coordinate the upgrade across all peers in a deployment.** v0.10 and v0.11 do not interop on the wire — the envelope version byte and the EventMeta size both changed. Stand the new version up across the fleet in one window rather than rolling upgrades.
2. **Re-tail from your source of truth (bus / publisher) for any RedEX channels carrying state you need to retain.** v0.10's on-disk EventMeta layout (`origin_hash` at bytes [4..8], `seq_or_ts` at [8..16], `checksum` at [16..20]) does not match v0.11's (`origin_hash` at [4..12], `seq_or_ts` at [12..20], `checksum` at [20..24]). The cortex per-event checksum's v1 fallback path reads checksums from pre-v0.11 *records*, but the meta-size shift means the byte slicing itself is different.
3. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.11 line.** Recompile. The Rust signature changes (`u32` → `u64` on `origin_hash`, `DaemonError::Stale` variant, `applied_through_seq` watermark) will surface as compile errors at the exact call sites that need updating.
4. **JS / TypeScript callers: switch `originHash` literals to `BigInt`.** `0xabcdef01` → `0xabcdef01n`. The TypeScript surface declares `originHash: bigint`; existing call sites using `Number` will fail at runtime against the new declarations.
5. **Go callers: widen `uint32` locals to `uint64` for every `origin_hash` parameter, return value, or struct field.** The cgo header (`include/net.go.h`) reflects the new ABI.
6. **Python callers need no source changes** — `int` is arbitrary precision and PyO3 handles the marshalling transparently. Re-test fixtures that round-trip an `origin_hash` through external storage (databases, message queues) to confirm the upper 32 bits are preserved.
7. **C callers: widen `uint32_t` typed pointers to `uint64_t` for every `origin_hash` parameter and out-param.** Anyone hand-rolling against `include/net.go.h` must regenerate their bindings.
8. **If your tests covered any of the items in *Behavioral fixes that may surface as test breakage*, update the assertions.** The cortex `applied_through_seq` semantic and the v2 checksum migration each have a one-line fix at the assertion site; the v0 envelope removal requires deleting the fixture entirely.
9. **RedEX on-disk layout has changed.** Each channel now stores its files under `<channel>/v0000000001/{idx,dat,ts}` plus a 16-byte `<channel>/manifest` pointer file, replacing the flat `<channel>/{idx,dat,ts}` layout. The migration runs automatically on first open of a v0.10 / v0.11 channel (one-shot, idempotent) — no code change required from callers. Tools or scripts that read RedEX files directly (rare; the supported access path is the `RedexFile` API) need to follow the manifest to the live generation directory.
10. **If you embed FFI handles in a custom Rust wrapper** (rare), embed `HandleGuard` from the new `ffi::handle_guard` module and route every entry point through `try_enter` / `begin_free`. The recipe matches the bundled handles' implementation; the helper module's tests double as documentation.

The audit queue is now drained. The next release will pick up structural items (`MigrationOrchestrator` placement-policy work, persistent JetStream sequence numbering for cross-restart msg-id durability) rather than another bug-fix sweep.

---

Released 2026-05-05.

## License

See [LICENSE](../../LICENSE).
