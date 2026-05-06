# Net v0.10 — "Killing Moon" Phase III

v0.10 continues the v0.9 line. Same conviction, same shape: a hardening release with no new transports, no new SDK surfaces, and no new feature gates. Every commit on this branch is a bug fix, a regression test, or a documentation tightening sourced from a fresh round of multi-pass internal audits.

The work was driven by two parallel audit reports against the v0.9 line: a 171-item full-crate sweep across the bus, shard manager, RedEX/CortEX, adapters, FFI, mesh transport, compute / migration, and bindings — of which 149 items have been addressed on `bugfixes-9` — and a separate single-file deep read of `mesh.rs` that surfaced 9 additional defects scoped to that file. The mesh-specific findings are queued for the next release; this note covers what landed.

---

## Addressed in this release

### RedEX & CortEX (storage + folded state)

- **Compact temp-file leak on reopen failure** — `compact_to`'s cleanup path ran *after* the post-rename `open_or_poison` / `clone_or_poison` fallibles, so a reopen failure left three placeholder files behind in `/tmp` forever. Cleanup now runs before the fallible reopen.
- **Truncate-on-recovery without `sync_all`** — torn-tail repair `set_len` was not durable; a crash before the next write reverted the recovery and the same torn bytes were re-read. Now `sync_all` + `fsync_dir` after the truncate.
- **Best-effort rollback silently swallowed open errors** — `if let Ok(f) = OpenOptions::new().write(true).open(...)` quietly skipped rollback when the dat/idx open failed; subsequent appends produced permanent dat/idx divergence. Now propagated as `RedexError`.
- **In-memory index corruption on panic between drain and renormalize** — `sweep_retention` could leave rebased `base_offset` against absolute payload offsets if it panicked mid-rewrite. Now builds the renormalized index in a temp `Vec` and atomically replaces.
- **`saturating_sub(dat_base) as u32` masks heap corruption** — silently wrote offset 0 for stale heap entries. Now hardened so the cast never silently squashes a real offset error.
- **`next_seq` rollback skipped if `disk` is `None`** — currently safe path; documented and pinned by an invariant comment.
- **Stale watermark advances past unfolded events under `Stop` policy** — `recoverable_decode` published `folded_through_seq.store(seq)` for events whose state mutation never landed; `wait_for_seq(seq)` returned true incorrectly. Now gated on the actual fold result.
- **Snapshot persists `last_seq` for skipped events** — when the watermark fix above lands, `snapshot()` no longer emits a `last_seq` for events whose state was never applied; the log remains the source of truth on restore.
- **Cortex `WatermarkingFold` saturates `app_seq` at `u64::MAX`** — a peer publishing `seq_or_ts == u64::MAX` could pin our `app_seq`; the next `fetch_add(1)` panicked in debug or wrapped in release, breaking per-origin monotonicity. Inputs are now capped at `u64::MAX - 1`.
- **Memories upsert was asymmetric and tombstone-less** — existing-id `STORED` partial-updated, missing-id inserted with `pinned: false`, and a `STORED → DELETED → STORED` sequence resurrected the deleted entry. Now consistent and tombstone-aware.
- **Memories empty-vec filter footgun** — `Some(vec![])` for `require_any_tag` excluded everything (`any` over empty = false); `Some(vec![])` for `require_all_tags` excluded nothing (`all` over empty = true). UI forms emitting empty multi-selects broke silently. Both empty cases now treated as "no filter."
- **Cortex/memories watch strict-bound mismatch** — doc said `>` / `<`, code used `>=` / `<=`. Strict-bound consumers received boundary events. Now matches the doc.
- **`StoredEvent::Serialize` round-trips bytes through `Value`** — re-encoding through `serde_json::Value` discarded original whitespace, normalized number formatting (`1.0` → `1`), and reordered keys. Any downstream that hashed or signed the serialized form silently failed verification. Now passes the raw bytes through `&serde_json::value::RawValue`.

### Bus, shards, and dispatch

- **`remove_shard_internal` awaited batch worker before drain** — contradicting the function's own doc comment. Drain still owned a sender clone, so a wedged adapter pinned this function indefinitely (no `tokio::time::timeout` shell on this path). Order swapped to drain → batch and the same timeout the rollback path uses now wraps the await.
- **`add_shard_internal` rollback dispatched stranded batch with stale `next_sequence` after worker timeout** — the still-detached worker may not have published its final flush, so the rollback emitted overlapping msg-ids. Rollback now refuses to dispatch on the timeout path; the JoinHandle leak is acknowledged in the comment.
- **`manual_scale_up` cooldown loop invariant violated whenever `cooldown > 0`** — each iteration bumped `last_scaling = Instant::now()`; iteration 1 immediately failed `InCooldown` (default 30 s), leaving the first shard half-added. Operator-initiated scale-up now bypasses the auto-scaling cooldown via a dedicated `scale_up_provisioning_force` path.
- **Scaling monitor and `manual_scale_down` raced `finalize_draining`** — non-target qualifying `Draining` shards were silently transitioned to `Stopped`, dropped on the floor by the `target.contains(&shard_id)` filter, and leaked. Non-target ids are now still routed through `remove_shard_internal`.
- **`flush()` Phase 2 barrier satisfied by post-flush traffic** — `dispatched` was a running counter, not a snapshot; with asymmetric per-shard latency the inequality could be satisfied while pre-flush events were still queued. Now snapshots `dispatched + dropped` at flush entry and gates on the *delta*.
- **`shutdown()` deadline path double-counted in-flight events** — `events_dropped += in_flight_ingests` then the final two-pass sweep also drained those events into `events_dispatched`, violating `events_ingested = events_dispatched + events_dropped` on every deadline-triggered shutdown. Now subtracts the events the final sweep drained.
- **`Drop` did not surface stranded ring-buffer events** — bus dropped without `await shutdown()` lost ring contents but never bumped `events_dropped` or set `shutdown_was_lossy`. Operators reading post-mortem stats saw no record of the loss. Now snapshots `shard_stats()` in `Drop`.
- **`PollMerger` topology swap had a lost-update race** — concurrent `add_shard_internal` / `remove_shard_internal` could each read `shard_ids()` and serialize their `store(...)` in the wrong order, leaving the published merger view including a removed shard until the next topology change. The `shard_ids() → store` block is now serialized.
- **`PollMerger::poll` lost cursor context on stalled poll** — `next_id` was `None` when no shards made progress, even with a valid `request.from_id`. Callers re-fetched from zero — silent pagination regression. Now echoes back the original `from_id`.
- **`mapper.activate` `active_count.fetch_add` outside the held write lock** — three concurrent activates could pass the budget gate against a stale count and transiently overshoot `max_shards`. Increment moved before `drop(shards)`.
- **`mapper.finalize_draining` read `pushes_since_drain_start` with `Relaxed`** — the field's docstring required `Acquire` to pair with the writer's `SeqCst` reset. Now matches.
- **JoinHandle errors silently dropped in shutdown** — `let _ = futures::future::join_all(drains).await;` ate panicked drain workers (default Tokio doesn't log task panics). Now captured and surfaced via `events_dropped`.
- **`shutdown_via_ref` and in-flight wait loops thrashed the runtime** — bare `tokio::task::yield_now` re-queued the task without parking; tight loops under contention starved the workers they were waiting on. Switched to short `tokio::time::sleep`.
- **`flush()` held a sync `parking_lot::Mutex` inside `async fn`** — replaced with the async-safe variant.
- **JSON cursor key `"00"` parsed to `0`** — collided with shard 0 across rebuilds. Cursor codec now treats string keys as opaque.
- **`std::time::Instant` mixed with tokio time in shutdown** — wall-clock `5s` broke `tokio::time::pause()`-based tests. Now consistent.
- **Drain worker `mem::replace`/`send` ordering** — swapped `scratch` *before* the awaited `sender.send(batch)`; channel-close mid-await silently dropped the batch. Documented as load-bearing under shutdown ordering and pinned by a regression test.

### Atomics, timestamps, and counters

- **`raw_to_nanos(raw)` quanta semantics** — clarified to use `delta_as_nanos(0, raw)` consistently.
- **`TimestampGenerator::next` re-reads `raw` inside the CAS loop** — pre-fix `now` was read once outside the loop; on contention, retries reused the stale `now` and the returned timestamp drifted as `last + 1` arbitrarily far behind real time.
- **`shard/batch.rs` `current_batch_size * 3 + target` overflow** — debug panic / release wrap on adversarial config. `BatchConfig::validate` now bounds `max_size <= 1_000_000`.
- **`shard/batch.rs` velocity-window `Instant - Duration` underflow** — Windows `Instant` is QPC-relative-to-boot; immediately-after-boot processes aborted the batch worker. Now `checked_sub`.
- **`f64 → usize` `as` cast in batch** — added `clamp` first.
- **`shard/mapper.rs` `next_shard_id.store(first_id + count)`** — `checked_add` on the bump path.
- **`shard/mapper.rs` `overloaded_count` used stale-metric placeholders for freshly-added shards** — newly-active shards no longer skew the load signal until they have at least one observation window.
- **`record_flush` / `collect_and_reset` latency-sum/count desync** — two independent `fetch_add`s vs two independent `swap`s let `avg_flush_latency = sum.checked_div(count).unwrap_or(0)` silently zero out under sustained load, suppressing the scale-up flush-latency trigger. `(sum, count)` now packed into a single `u128` and CAS'd together. Same fix applied to `push_latency_sum_ns / push_count`.

### Adapters (JetStream / Redis / dedup)

- **JetStream `Other` `PublishErrorKind` classified as transient** — auth failures, permission denied, malformed-subject all retried forever against a backend that would never succeed. Now enumerates the truly transient variants and treats `Other` as fatal.
- **JetStream "pipelined" publish was actually serial** — loop `await`ed `publish_with_headers` per event before moving on; only the server-ack join was parallel. 1k-event batch on a 1 ms RTT cost ~1 s instead of "~1 RTT per batch." Now pushes the publish-future into the join set.
- **JetStream per-event `serde_json::Value` allocation** — violated the per-event no-alloc contract. Now mirrors Redis's `RawValue` borrow + `Bytes::copy_from_slice`.
- **JetStream one RTT per sequence in steady state** — `direct_get(seq)` per sequence on a 1 ms RTT cost ≥100 ms wall for a 100-event poll. Now `direct_batch_get`.
- **JetStream cold-stream bail enabled on transient `info()` failure** — fallback fabricated `first_seq = 0`, enabling the cold-stream bail; populated streams returning NotFound in deletion gaps bailed after 64 NotFounds with events still ahead. Now propagates `Transient`.
- **JetStream `Fatal` decode discarded already-decoded prefix** — function returned immediately, dropping the events accumulated so far without advancing the cursor; recovery re-emitted the prefix. Now returns `Ok` on the good prefix and surfaces the corruption on the next poll.
- **JetStream `shutdown` retained `self.jetstream` / `self.client`** — post-shutdown `on_batch` proceeded against a drained client (typically erroring, sometimes hanging). Both fields now cleared.
- **JetStream init-after-shutdown silently overwrote client without `drain()`** — losing in-flight publishes piggybacking on the prior client. Now drains first.
- **JetStream partial-failure produced duplicate publishes** — mid-batch error dropped in-flight `PublishAckFuture`s but bytes were already on the wire; retry re-published, and `Nats-Msg-Id` deduped only within the dedup window. Documented and pinned; retry path now wraps `publish_with_headers` in `tokio::time::timeout` to bound the cancellation surface.
- **JetStream missing `r` field stored `b"null"`** — could surprise downstream consumers expecting either present-or-absent. Now passes through unchanged.
- **Redis cluster errors classified as fatal** — `MOVED` / `ASK` / `READONLY` / `CLUSTERDOWN` / `NOREPLICAS` were not in the substring set; after any Redis Cluster failover, every batch failed permanently until process restart. Added.
- **Redis `is_healthy` PING timeout cancellation** — wrapped in `command_timeout`, with a dedicated health-check connection so a desynced `ConnectionManager` doesn't serve a stale PING reply on the next real command.
- **Redis `poll_shard` XRANGE had no `command_timeout` wrapper** — `on_batch` and `is_healthy` honored the timeout contract; `poll_shard` could block indefinitely. Now wrapped.
- **Redis `shutdown` didn't drop `self.conn`** — pure advisory flag; `get_conn` ignored `initialized = false`. `on_batch` could write to Redis silently after shutdown. Connection now dropped, `get_conn` errors with `Fatal` when the adapter has shut down.
- **`RedisStreamDedup` 4096-entry default was two orders of magnitude too small** — at 10 K events/sec that's a 0.4 s window; the doc described "~minutes of in-flight." Default raised; capacity required at construction.
- **`dedup_state` startup nonce non-cryptographic** — `xxh3_64` of `(pid, tid, ns, stack_addr, ...)` narrowed entropy on 32-bit targets. Now mixes a `/dev/urandom` seed.
- **`limit + 1` overflow** (Redis & JetStream poll request shaping) on adversarial limits — `saturating_add(1)`.

### Mesh transport, sessions, routing

- **`handle_routed_handshake` Case 2 — replay nuked the live session, no rate limit** — Noise NKpsk0's responder uses a fresh ephemeral on each reply, deriving a brand-new session key per replay; an attacker replaying a captured msg1 replaced the legitimate session keys, the legitimate sender kept the old keys, every subsequent packet failed AEAD. Now drops the replay when the live session matches the same `remote_static_pub`, and the `HandshakePacer` from the legacy adapter has been added.
- **Pingwave `strict_progress` permitted address-poisoning via the `hops < n.hops` arm** — an attacker who had observed pingwaves could spoof `(origin_id=Y, seq=K, hop_count=0)` for `K < n.last_seq` and overwrite `n.addr` to their UDP source. The conditions are now AND'd: `pw.seq >= n.last_seq` AND `hops <= n.hops`.
- **`ThreadLocalPool` per-thread cache leaked forever** — every connect/disconnect/NAT-rebind/mesh-rebuild cycle leaked ~16 KB × `local_capacity` × `num_threads`. Long-lived daemons OOM'd in proportion to peer-churn count. Now `Drop` walks every thread's `LOCAL_BUILDERS` to evict its `pool_id` slot.
- **`MAX_PACKET_POOL_SIZE = 1<<20` was OOM-on-first-session** — `with_local_capacity` pre-allocated `size × ~16 KB` ≈ 16 GiB up front. The cap was *meant* to prevent OOM. Lowered to a few thousand; remaining budget covered by lazy-on-first-use.
- **Anti-replay window forward-jump > 1024 zeroed state instead of refusing** — `MAX_FORWARD = 65_536`, `WINDOW_SIZE = 1024`; a single authenticated jump in `(1025, 65_536]` zeroed the bitmap and left previously-seen counters in `rx_counter - 1024 .. rx_counter` replayable. The slide is now refused past `WINDOW_SIZE`; a fresh handshake is required.
- **Anti-replay `received == u64::MAX`** — first authenticated packet at the boundary saturated `rx_counter` and rejected every subsequent counter; one hostile authenticated packet could permanently poison the receive path. Now rejected at `is_valid`.
- **`TokenScope::contains(NONE)` returned `true` unconditionally** — `(self.bits & 0) == 0`. Compounded with `authorizes(NONE, ch)` returning unconditional `true`, so any token authorized the no-op action; callers building `action: TokenScope` from external input where the input masked to `NONE` saw `true` for every token. Short-circuits at the top of `contains`.
- **`route.rs` tie-break used `<=`** — doc said "preserved if strictly better." Now `<`.
- **`router.rs` `route_packet` had no source/loop suppression** — TTL exhaustion was the only loop-breaker; `add_route_with_metric` flap or a malicious peer could set up a 2-hop loop. Now drops when `routing_header.src_id == routing_table.local_id` and inspects a small `(src_id, stream_id, sequence)` LRU.
- **`router.rs` `RouterError::TtlExpired` recheck after `forward()` double-counted** — both `record_in` and `record_drop` ran. `record_in` deferred until after the post-decrement TTL check.
- **`linux.rs` `BatchedTransport::send_batch` silently truncated above 64** — `len.min(MAX_BATCH_SIZE)` returned ≤ 64 unconditionally; reliable streams stashed the rest via `on_send` and only learned via NACK/RTO. Now returns `InvalidInput` over the cap; chunked-internally is a follow-up.
- **`linux.rs` `iov_base: packet.as_ptr() as *mut _` provenance laundering** — sound under the kernel-reads-only invariant, but documented at the call site so a future Miri pass doesn't have to re-derive it.
- **`mod.rs` handshake retry sleep had no upper bound** — `100 * attempt` over `MAX_HANDSHAKE_RETRIES = 1024` summed to ~14 hours total with the last attempt sleeping ~102 s. Capped at 5 s per attempt.
- **`mod.rs` handshake recv loop allocated `BytesMut::with_capacity(MAX_PACKET_SIZE)` per iteration** — allocator pressure under stray traffic. Buffer now reused across iterations.
- **`session.rs` `evict_idle_streams` LRU vs concurrent open race** — `min_by_key` then `remove` was non-atomic; a freshly-opened stream could be torn down between selection and removal. Now uses `remove_if` with a freshness predicate.
- **`session.rs` `verify_and_touch_heartbeat` did not pre-check `parsed.payload.len() == TAG_SIZE`** — AEAD caught the mismatch but a length check shortcuts cleartext-flood probes before they touch the cipher.
- **`session.rs` `RxCreditState::on_bytes_consumed` `consumed`/`granted` not jointly atomic** — concurrent calls could publish `consumed > granted` transiently; observability/metrics showed flicker. Now packed `u128` AcqRel CAS.
- **`route.rs` capability-announcement `hop_count += 1`** — every other hop-count increment in the crate uses `saturating_add(1)`; this one was bounded today by the `< MAX_CAPABILITY_HOPS - 1 = 15` guard but one constant change from a debug panic. Now matches the rest.
- **Static-mode `select_shard_by_hash` used raw modulo** — dynamic-mode was already on Lemire's unbiased `(hash * len) >> 64`. Same bias, same fix; both paths now consistent.
- **`gateway.rs` `ParentVisible` over-permissive direction** — predicate accepted both `dest.is_ancestor_of(source)` and `source.is_ancestor_of(dest)`; the second clause leaked parent-region traffic *down* into descendants. Now strictly upward.
- **`pool.rs` `(payload.len() - 16) as u16` truncation** — currently safe under `MAX_PAYLOAD_SIZE = 8112`; `debug_assert!` added so a future cap-raise past `u16::MAX + 16` doesn't silently mis-frame on the wire.
- **`failure.rs` `unwrap()` on poisoned `std::sync::Mutex`** — the rest of the crate uses `unwrap_or_else(|p| p.into_inner())`; a single panic anywhere holding these locks would have turned every subsequent unwrap into a runtime panic that took down the failure-detection loop. Switched.
- **`failure.rs` `RecoveryManager::on_failure` overwrote `FailedNodeState` on insert** — `failed_at` and `retry_count` reset to 0 each time; flapping peers never hit `max_retries`. Now `entry().or_insert(...)` and bumps `retry_count`.
- **`failure.rs` `get_action` returned `Retry { delay_ms: 0 }` for healthy nodes** — busy-loop footgun for callers using the action on the healthy path. Now returns the no-op variant.
- **`transport.rs` `BatchedPacketReceiver` thread spun at 1 ms on persistent socket errors** — `EBADF` / `ENOTSOCK` / permission-revoke ate a CPU forever. Now exponential backoff with hard-error early return.
- **`proxy.rs` telemetry counters incremented before send succeeded** — counters drifted high under partial failure. Now incremented on success.
- **`proximity.rs` `update_from_pingwave` worse path overwrote better** — high-seq pingwave through a long route demoted the cached direct route. Freshness (always take latest seq) is now separate from path quality (only update `hops`/`addr`/`latency_us` when `new_hops <= self.hops`).
- **`proximity.rs` self-edge `insert_or_update_edge` per-pingwave** — hot-path noise; skipped.

### Compute, daemons, migration

- **`start_migration` always emitted a single `SnapshotReady` regardless of size** — `chunk_index: 0, total_chunks: 1` whether the snapshot was 12 B or 12 MB; the wire encoder rejected any chunk over `MAX_SNAPSHOT_CHUNK_SIZE = 7000`. Locally-initiated migration of any daemon whose serialized state exceeded 7 KB couldn't be sent. Now routes through `chunk_snapshot(daemon_origin, snapshot_bytes, seq_through)`. **Breaking — see breaking-changes section.**
- **Snapshot reassembly unbounded chunk hold via `seq_through == latest`** — eviction only fired for *strictly greater*; an attacker could park up to ~4.3 GiB of unfinished reassembly per `(origin, seq)` and refresh forever. Per-entry byte cap (`MAX_PENDING_REASSEMBLY_BYTES = 64 MiB`) plus a per-entry age sweep (`MAX_PENDING_REASSEMBLY_AGE = 5 min`, opportunistic at the head of every `feed` plus a public `sweep_stale` for external timers) close the at-cap-and-quiet residual hole.
- **`abort_migration_with_reason` did not propagate to `MigrationSourceHandler`** — source-side `migrations` map retained the entry; `is_migrating()` stayed true, `buffer_event` kept buffering into an undrained vector, retries tripped `AlreadyMigrating`. Now dispatched.
- **`standby_group` replaced standby marked healthy with `synced_through = 0`** — a subsequent active failure could promote the fresh zero-state standby and lose all pre-buffer state. Now keeps the replaced standby unhealthy until after a successful sync, and `promote()` candidates are filtered to `last_sync.is_some()`.
- **`migration_target::buffer_event` had no phase guard** — could insert/deliver post-cutover; combined with normal-path delivery yielded duplicate execution. Now guarded.
- **`migration_source::start_snapshot` was a `contains_key` → `entry()` race** — two concurrent snapshots of the same origin could both call user-supplied `MeshDaemon::snapshot()` (DashMap entry guard was held across user I/O — a separate fix moves the entry-guard drop ahead of the snapshot). The trait API doesn't enforce idempotency; the race is now serialized.
- **`migration_source::take_buffered_events` had no phase guard** — misuse-prone. Now guarded.
- **`migration_target::abort` did not clear `completed` index** — minor leak. Cleared.
- **`orchestrator` returned `MigrationError::TargetUnavailable(0)` from auto-placement** — surfaced "target node 0x0 unavailable" to operators when no specific node had ever been tried. Now typed `NoTargetAvailable` (variant addition).
- **`orchestrator::buffer_event` returned `false` at Cutover** — downstream caller could route to source post-handoff. Now correctly buffers through Cutover.
- **`migration.rs` `started_at: u64` saturated on clock jump backward** — switched to `Instant`.
- **`fork_group` `forks.pop()` and `coord.remove_last()` invariant unenforced** — brittle. Now enforced.
- **`bindings.rs` `Vec::with_capacity` from peer-supplied `u32`** — declared count of ~4 B entries → ~96 GiB allocation before truncation. Now bounded by `data.len() / MIN_BINDING_SIZE`.
- **`reconcile.rs` `unreachable!()` reachable on signed but divergent input** — equal-length-equal-payload tiebreak panicked on the chain's reconciliation thread. Now a deterministic tiebreak on `parent_hash`.
- **`reliability.rs` silent reliability drop** — when `pending.len() >= max_pending`, the oldest *unacknowledged* packet was popped; subsequent NACK could never recover that seq because the entry was gone. Now backpressures callers; doesn't drop tracking for in-flight packets.
- **`router.rs` `NetRouter::start` had no re-entry guard** — a second call spawned a competing dequeue loop. Now `compare_exchange` on `running`.
- **`continuity/chain` `(0, Some(non-empty payload))` accepted as genesis-shaped** — chain reported `Forked` against junk. Now `Unverifiable`.
- **`state/log` genesis-shaped event with un-validated payload** — peer-injected attacker-chosen anchor. Now pinned to the canonical genesis payload.
- **`contested/correlation` capability-index parent walk loops forever** — defensive depth cap (matches the 4-level hierarchy).
- **`contested/observation` unbounded `HashMap` + `seq_diff_sum` overflow** — long chains accumulated forever. LRU + `saturating_add`.
- **`contested/superposition` `target_replayed` only advanced from `Superposed`** — `Spreading` (target catches up before `advance(Replay)`) stalled forever; `ReadyToCollapse` never fired. Now both arms advance.
- **`contested/propagation` lossy `f64 → u64` poisoned EWMA** — a pathological RTT clamped `per_hop` to `u64::MAX` permanently. NaN check tightened.
- **`contested/correlation` `Instant` subtraction panicked** — `now - correlation_window` panicked if the window exceeded uptime. Now `checked_sub`.
- **`partition.rs` `NaN >= threshold` blocked healing** — when `other_side.is_empty()` the ratio was NaN. Empty case now treated as "fully healed."
- **`failure.rs` `RecoveryManager` flapping peers** (see *Mesh transport, sessions, routing* — the recovery and the failure detection both lived in this file).
- **`identity/origin.rs` `origin_hash: u32` collision floor documented** — ~65 K peer birthday collision; cross-channel accounting keyed by `origin_hash` aliases distinct entities. Documented as the boundary; the rename to `origin_tag` and the wire bump are deferred to the next phase.

### Behavior, identity, security

- **`safety.rs` AuditOnly silently dropped violation logs** — `check_rate_limits` only logged when `mode == Enforce`; the documented "log violations but don't block" stance simply didn't log. Now logs unconditionally; only the `return Err` is gated.
- **`safety.rs` `Relaxed` / `AcqRel` mismatch** — `release` paired against `acquire`'s `AcqRel`; observable counter drift on weakly-ordered cores. Both sides now `AcqRel`.
- **`safety.rs` audit-only token counter `fetch_add` without saturating** — wraps under hostile traffic. Now saturating.
- **`loadbalance.rs` NaN slipped past `total_weight <= 0.0`** — switched to `!(total_weight > 0.0)` which captures NaN.
- **`token.rs` slot-cap race unbounded** — `contains_key` then `entry()` overshoot bounded by concurrent calls, not shards. Now `entry().or_insert_with()` then drop on overflow.
- **`token.rs` `signed_payload()` allocated 95 bytes per verify** — hot-path waste. Now stack-buffered.
- **`channel/roster` `is_empty()` → `remove_if` TOCTOU** — idempotent today but fragile. Tightened.
- **`channel/guard` `revoke()` did not rebuild bloom** — false-positive rate climbed until manual `rebuild_bloom`. Now triggers rebuild.
- **`behavior/diff::to_bytes` returned `Vec::new()` on cap-violation** — indistinguishable from a legitimate empty diff; senders silently transmitted zero bytes, receiver dropped. Deprecated in favor of `try_to_bytes`.
- **`crypto.rs` `ReplayWindow::commit`** — see *Mesh transport, sessions, routing*: `received == u64::MAX` poisoning fixed at `is_valid` instead of `commit`.

### Bindings (Node, Python, Go, C) & FFI

- **`net_poll` buffer-too-small dropped already-consumed events** — `bus.poll(request)` advanced the cursor *before* the response was serialized; an undersized buffer returned `BufferTooSmall` and dropped the entire response, but the next call started at the now-advanced cursor. Every event in the failed serialization was silently lost. Buffer is now sized-checked first and the response is buffered so a retry can resume.
- **`net_poll_ex` allocation failure dropped the entire batch** — `Layout::array::<NetEvent>(count)` and `std::alloc::alloc(layout)` failures returned `Unknown` and dropped the response. Now pre-validates `count` against a max event-count.
- **Panic across FFI on OOM in `net_poll_ex`** — `event.id.as_bytes().to_vec().into_boxed_slice()` and `event.raw.to_vec().into_boxed_slice()` could panic mid-loop and leak earlier `Box::into_raw`s plus the `std::alloc::alloc(layout)` array. Entry points now `catch_unwind`; `panic = "abort"` for the cdylib closes the residual.
- **`slice::from_raw_parts(ptr, len)` lacked `len <= isize::MAX` validation** — a C caller passing sign-extended `-1` triggered immediate UB before any guard fired. Affects every wide-input FFI entry point: `net_ingest`, `net_ingest_raw`, `net_ingest_raw_batch`, `net_ingest_raw_ex`, `mesh.rs::collect_payloads`, `net_mesh_publish`, `net_redex_file_append`, `net_identity_sign`, `net_identity_install_token`, `net_parse_token`. All now reject above the `isize::MAX` boundary.
- **`net_generate_keypair` / `net_free_string` feature-gated, header unconditional** — consumers linking against a cdylib built without `net` got load-time missing-symbol errors despite the header promising the symbol. Stubs added.
- **`net_free_poll_result` not idempotent** — frees `events` and `next_id` but left the struct fields holding the freed pointers. A defensive caller / destructor wrapper double-free'd. Now nulls fields after free; subsequent calls and null-pointer calls are no-ops.
- **`bus_taken` defense-in-depth claim was doc-only** — doc said "FFI ops also check this," but the field was read only inside `net_shutdown`. Either gate or remove the doc; we gated.
- **Concurrent `net_shutdown` callers raced the `bus_taken` swap** — a second/third caller returned `Success` while the first was still inside `runtime.block_on(bus.shutdown())`, falsely signaling completion. Now serialized.
- **`runtime().block_on(...)` panics unwound across `extern "C"`** — `Handle::try_current()` guard added at every `cortex.rs` and `mesh.rs` `block_on` site; `catch_unwind` shim added.
- **FFI handle accessors `&*handle` without alignment check** — misaligned `*mut NetHandle` from C is immediate UB before the null check. `is_aligned_to::<HandleType>()` now precedes every dereference.
- **`Arc<InnerType>`-wrapped FFI handles lacked compile-time `Send + Sync` audit** — `static_assertions::assert_impl_all!(InnerType: Send + Sync);` placed next to each handle.
- **`c_str_to_str` lifetime elision dangled** — signature `unsafe fn c_str_to_str(p: &*const c_char) -> Option<&str>` bound the returned `&str` to the *local* stack slot, not the underlying C buffer. Today's call sites are stack-only, but a future refactor moving the result into `tokio::spawn(async move { ... })` would have compiled cleanly and dangled. Now `unsafe fn c_str_to_str<'a>(p: *const c_char) -> Option<&'a str>` with explicit lifetime.
- **`net_ingest_raw_batch` silently dropped null and invalid-UTF-8 entries** — function returned `count - 1` accepted; bindings attributed the drop to backpressure, retried the wrong indices, and double-published the good ones. Now surfaces dropped indices via `out_failed_indices: *mut size_t, out_failed_len: *mut size_t`.
- **`parse_config_json` silently fell back to `DropNewest` on unknown `backpressure_mode`** — `"DropOldset"` (typo) or `"FailProduce"` got a different durability profile with no error at deploy time. Now errors on unknown values; added the `Sample { rate }` arm with rate validation.
- **`retention_max_*` accepted zero, fsync params did not** — `retention_max_events = 0` meant "evict everything immediately on first append" — almost certainly a config mistake intended as "no limit." Now rejected at the same gate.
- **Net `heartbeat_interval_ms` / `session_timeout_ms` and mesh `heartbeat_ms` accepted zero** — heartbeat-every-0ms busy-looped the heartbeat task and saturated a CPU. Now validated.
- **Cortex non-success paths didn't write `*out_json`/`*out_len`** — pre-zero is the contract; some paths violated it. Fixed.
- **`CString::new` failure reported as `InvalidUtf8` but caused by interior NUL** — error variant retitled.
- **`NetEvent` / `NetReceipt` `#[repr(C)]` lacked cross-arch alignment pinning** — const asserts on layout added.
- **`TokioMutex` held across JSON serialization in cortex FFI** — per-cursor latency stall. Serialization now happens outside the held mutex.
- **Mesh FFI `g.fp16_tflops_x10.map(|tf| tf as f32 / 10.0)` lossy for `u32 ≥ 2²⁴`** — the neighboring `tops_x10` already used `saturating_u16_cap`. Matched.
- **`parse_modality_cap` unknown modality strings silently fell back to `Modality::Text`** — used for both capability announcements and capability filters; a typo in `require_modalities` returned wrong nodes with no error. Switched to `Option<Modality>` and surfaces `NET_ERR_CHANNEL` on unknown.

### Compute SDK error surface

- **`MigrationError::TargetUnavailable(0)` → `NoTargetAvailable`** — variant addition; the integration test that asserted the pre-fix variant has been updated.
- **`start_migration` returns `Vec<MigrationMessage>`** instead of single — see breaking changes.

### Test hygiene

- **Migration chunked-snapshot regression** — pins that locally-initiated migration of a daemon with a serialized state ≥ 7 KB chunks correctly, and the SDK's transport-identity seal path reassembles, seals, and rechunks in order.
- **Snapshot reassembly age-sweep regression** — pins that the pending entry is evicted at the head of the next `feed` past the age cap.
- **`active_count` budget under concurrent activate** — pins that three concurrent activates can't transiently overshoot `max_shards`.
- **`PollMerger` `from_id` echo on stalled poll** — pins the cursor-context preservation.
- **`flush()` Phase 2 barrier delta-snapshot** — pins that post-flush ingest can't satisfy the inequality.
- **`shutdown_was_lossy` no longer false-positives on deadline-triggered shutdown** — pins that final-sweep drains are not counted against `events_dropped`.
- **`next_seq` observer consistency** — `committed_seq` is the lock-free invariant readers see.
- **Anti-replay `received == u64::MAX` rejection** — pins that one hostile authenticated packet can't poison the receive path.
- **`TokenScope::contains(NONE)` is `false`** — pins the no-op-action authorization closure.
- **JetStream cold-stream bail gated only on `first_seq == 0`** — pins that populated sparse streams are walked past arbitrary deletion gaps.
- **`net_free_poll_result` idempotency** — pins single + multiple + null-pointer free.
- **`net_poll` minimum-buffer rejection** — pins that buffers below `MIN_RESPONSE_BUFFER` are rejected before the cursor is touched.

---

## Known issues — queued for the next release

### `mesh.rs` deep-read audit

A separate single-file audit of `adapter/net/mesh.rs` (~8 K LOC) surfaced 9 additional defects that are scoped to that file. None of them are addressed in this release; all are slated for the next phase. For consumers running production deployments, the most consequential are listed below — the full audit is in [`docs/misc/BUG_AUDIT_2026_05_03_MESH.md`](misc/BUG_AUDIT_2026_05_03_MESH.md).

- **`spawn_heartbeat_loop` holds a DashMap shard guard across `.await`** — the heartbeat broadcast loop iterates `peers.iter()` and awaits `socket.send_to(...)` (heartbeat + pingwave, twice per peer) while still holding the iterator's `Ref` guard. Every other task touching the same shard blocks for the cumulative round-trip.
- **`accept` / `start` mutual exclusion uses `AcqRel` where the comment relies on `SeqCst`** — Dekker-style mutual exclusion needs both sides SC. On x86 the LOCK'd RMW happens to fully fence so the race is unobservable; on AArch64 / RISC-V the dispatcher can race `handshake_responder` for the inbound msg1.
- **Routed-handshake key rotation silently overwrites a live session** — the replay guard only fires for the same `remote_static_pub`; a routed msg1 with a *different* static for the same `peer_node_id` falls through and `peers.insert` overwrites the existing legitimate session.
- **`commit_reclassify_observations` torn `(nat_class, reflex_addr)` snapshot** — when every probe failed, `nat_class` is updated but `reflex_addr` keeps its previous value, violating the `traversal_publish_mu` invariant.
- **`authorize_subscribe` rejects idempotent re-subscribes with `TooManyChannels`** — a peer at the cap re-subscribing to a channel it already holds is rejected even though `SubscriberRoster` is set-typed.
- **Routed-handshake `peers.get` → `peers.insert` not atomic** — concurrent routed handshakes for the same `peer_node_id` race the insert; the loser's `pending_handshakes` initiator state is wedged until `handshake_timeout`.
- **`publish_to_peer` does not propagate the reliable flag to the packet header** — every other sender (`send_to_peer`, `send_routed`, `send_on_stream`, etc.) computes `if reliable { PacketFlags::RELIABLE }` and threads it in. `publish_to_peer` hard-codes `PacketFlags::NONE`. Latent today (per-stream reliability is set on open) but the inconsistency will silently bite when a receiver-side path consults the packet flag.
- **`process_local_packet` migration loopback unbounded synchronous self-bounce** — a buggy / attacker-influenced "trusted" handler that always emits a self-bound message can spin the dispatch task synchronously, starving every other peer's packets.
- **`connect_via` does not refresh `addr_to_node` after a successful direct upgrade** — the upgraded session's dispatch fast path falls back to a linear `peers.iter().find(...)` per packet for exactly the sessions that benefit most from the addr → nid index. Performance only.

### Items deferred from the main audit

The following remain open from `BUG_AUDIT_2026_05_03.md` and are tracked for the next release: #1 (Windows `compact_to` non-atomic — `MoveFileExW`/`MOVEFILE_WRITE_THROUGH`), #6 / #7 / #8 (cortex watermark + checksum coverage), #13 (registry `replace` in-flight quiescing), #23 / #24 / #25 (cortex / mesh handle-lifetime contract on FFI), #39 (msg-id `sequence_start` monotonicity test), #56 (`origin_hash` u32 collision boundary; rename / wire bump), #64 (orchestrator `target_head` parent-hash `0`), #68 (`registry::unregister` in-flight Arc clones), #73 (per-shard cap clamps cursor advancement under filtered single-shard requests), #81 (`adapter/redis.rs` pipeline timeout duplicate hazard — depends on `RedisStreamDedup` wiring), #97 (`session.rs` racy `tx_bytes_sent` watermark — see notes about credit-window invariant), #102 (envelope v0/v1 prober), #118 (rule window reset), #121 (`select_power_of_two` degenerate on `len == 2`), #125 (`per_source.clear()` minute-boundary RPM cap exceedance), #127 (initiator handshake `HandshakePacer`), #128 (`router.rs` lost-wakeup window).

---

## Breaking changes

### Rust core (`net` crate)

#### `MigrationOrchestrator::start_migration` returns `Vec<MigrationMessage>`

`start_migration` now returns `Result<Vec<MigrationMessage>, MigrationError>` instead of `Result<MigrationMessage, MigrationError>`. The local-source path returns one or more `SnapshotReady` chunks (sized to `MAX_SNAPSHOT_CHUNK_SIZE = 7000`); the remote-source path returns a single-element `vec![TakeSnapshot { .. }]`.

**Why**: pre-fix the orchestrator emitted `chunk_index: 0, total_chunks: 1` regardless of payload size; the wire encoder rejected anything past 7 KB and locally-initiated migration of any stateful daemon with a non-trivial state vector simply could not be sent.

**Migrate**:
```rust
// Before
let msg: MigrationMessage = orchestrator.start_migration(origin, src, dst)?;
send_migration_message(dest_node, &msg).await?;

// After
let msgs: Vec<MigrationMessage> = orchestrator.start_migration(origin, src, dst)?;
for msg in &msgs {
    send_migration_message(dest_node, msg).await?;
}
```

If you opted into transport-identity sealing, reassemble all chunks → seal → `chunk_snapshot(daemon_origin, sealed, seq_through)` → re-dispatch in order. The SDK's `start_migration_with` and `MigrationHandle::reinitiate_attempt` route through a new `maybe_seal_chunked_snapshot` helper that does this for you.

#### `MigrationError::NoTargetAvailable` (variant addition)

`start_migration_auto` now returns `MigrationError::NoTargetAvailable` when the scheduler finds no candidate, instead of `TargetUnavailable(0)` (which surfaced "target node 0x0 unavailable" to operators).

**Migrate**: match arms over `MigrationError` need to add the new variant; with `#[non_exhaustive]` already in place this is forward-compatible, but exhaustive match-on-variant code will refuse to compile.

#### `ConsumeResponse::failed_shards`

A new `failed_shards: Vec<u16>` field reports per-shard adapter errors that previously were silently swallowed at `warn` level (in contrast to `stalled_shards`, which was already surfaced).

#### Config validation rejects zero in places it used to accept

- `retention_max_events = 0`, `retention_max_bytes = 0`, `retention_max_age_ms = 0` are now rejected at the JSON-config gate (matching the existing fsync zero-rejection). Set them to `null` or omit the field for "no limit."
- Net `heartbeat_interval_ms = 0`, `session_timeout_ms = 0`, mesh `heartbeat_ms = 0` are now rejected. A 0-ms heartbeat saturates a CPU; this was almost always an unintended config.
- `BatchConfig` `max_size > 1_000_000` is now rejected. Default is `10_000`; the cap closes the `current_batch_size * 3 + target` overflow path.
- `parse_config_json` errors on unknown `backpressure_mode` values instead of silently selecting `DropNewest`.

#### `BackpressureMode::Sample { rate }`

New variant; existing match arms must add a wildcard or the new arm.

#### `behavior::diff::to_bytes` deprecated

Returns `Vec::new()` on cap-violation, indistinguishable from a legitimate empty diff. Migrate to `try_to_bytes` which returns `Result`.

#### `WatermarkingFold` caps inputs at `u64::MAX - 1`

A peer publishing `seq_or_ts == u64::MAX` previously poisoned per-origin monotonicity. Inputs at the boundary are now rejected. Operators feeding the watermarking fold with a synthetic max-seq must pick `u64::MAX - 1`.

#### `consumer/merge::PollMerger` failed/stalled shard surfacing

`PollMerger::poll` now echoes back the caller's `from_id` when no shards make progress (instead of `None`, which callers were interpreting as "no events" and re-fetching from zero). Callers that relied on `None` as the stall signal need to switch to `next_id == request.from_id`.

#### Cross-backend cursor migration enforced

`compare_stream_ids`'s mixed-format lex fallback wedged the cursor across backend migrations (e.g. JetStream → Redis: `"1700-0"` < `"42"` lex-compared). The cursor format is now persisted alongside the cursor; cross-backend migration without explicit reset is refused.

#### `StoredEvent` serialization passes raw bytes through

Pre-fix `StoredEvent::Serialize` round-tripped `self.raw` through `serde_json::Value`, discarding original whitespace and key order, normalizing number formatting (`1.0` → `1`). Downstream signatures or hashes against the serialized form silently failed verification. Now uses `&serde_json::value::RawValue` passthrough — byte-equality is preserved.

### Rust SDK (`net-sdk`)

The SDK's public surface is unchanged. The migration kickoff paths (`DaemonRuntime::start_migration_with` and `MigrationHandle::reinitiate_attempt`) handle the new chunked `Vec<MigrationMessage>` internally; if you call the orchestrator directly via `DaemonRuntime::orchestrator_arc()` (or equivalent) you must update to the new return shape.

### FFI / bindings

| Binding | Change |
|---|---|
| **All** | Every `extern "C"` body is now wrapped in `catch_unwind`; the cdylib uses `panic = "abort"` so a Rust panic does not unwind across the FFI boundary. Behavior change for callers that depended on a Rust panic *partially* completing the call before unwinding. |
| **All** | `slice::from_raw_parts(ptr, len)` rejects `len > isize::MAX as usize`. C callers passing sign-extended `-1` previously hit immediate UB before any guard fired; they now hit a defined error return. |
| **All** | FFI handle accessors check alignment via `is_aligned_to::<HandleType>()`. A misaligned `*mut Handle` returned from a wrapper that allocated through a non-Rust allocator now returns an error instead of UB. |
| **All** | `net_ingest_raw_batch` surfaces dropped indices via two new out-parameters (`out_failed_indices`, `out_failed_len`). Bindings that called the function with `nullptr` for these still get the old "count returned" semantics. |
| **All** | `net_free_poll_result` is now idempotent. Callers that ran their own field-nulling defensively can drop it. |
| **All** | `parse_modality_cap` returns `NET_ERR_CHANNEL` on unknown modality strings instead of silently falling back to `Modality::Text`. Bindings that round-tripped capability announcements through arbitrary string fields will start surfacing errors at deploy time. |
| **C** | `net.h` now provides `net_generate_keypair` / `net_free_string` stubs in builds without `net`. Consumers linking against a `net`-less cdylib previously hit load-time missing-symbol errors despite the header. |

### Behavioral fixes that may surface as test breakage

These aren't strictly API-breaking, but tests that asserted the pre-fix behavior will need updating:

- **`MigrationError::NoTargetAvailable`**: tests asserting `TargetUnavailable(_)` from `start_migration_auto` need to switch.
- **`shutdown_was_lossy = false` on a clean deadline-triggered shutdown**: tests that asserted the false-positive behavior will fail.
- **`PollMerger::poll` echoes back `from_id` on stall**: tests that asserted `next_id == None` on stall will see the input cursor instead.
- **`active_count` cannot transiently exceed `max_shards`**: tests that relied on the budget overshoot to construct a degenerate state will need a different vector.
- **`flush()` Phase 2 barrier respects pre-flush ingest**: tests that satisfied the inequality with post-flush traffic will hang to the deadline.
- **Anti-replay `received == u64::MAX` is rejected**: tests that asserted the boundary was accepted will see the rejection.
- **`TokenScope::contains(NONE) == false`**: tests that asserted the old `true` will need to flip.
- **JetStream `Other` `PublishErrorKind` is fatal**: retry-loop tests that simulated `Other` and asserted retry will see the call return immediately.
- **Memories `STORED → DELETED → STORED` does not resurrect**: tests that asserted resurrection will see the post-tombstone behavior.
- **`gateway.rs::ParentVisible`** is now strictly upward; tests that asserted descendant-side leakage will fail.
- **`route.rs` route tie-break is strictly better, not equal-or-better**: tests that asserted equal-metric overwrite will see preserved routes.

---

## How to upgrade

1. Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.10 line.
2. Recompile. The signature changes (`start_migration` → `Vec`, `BackpressureMode::Sample`, `ConsumeResponse::failed_shards`, `MigrationError::NoTargetAvailable`) will surface as compile errors at the exact call sites that need updating — follow the **Migrate** snippets above.
3. Audit your config for fields that previously accepted zero where they shouldn't have (`retention_max_*`, `heartbeat_interval_ms`, `session_timeout_ms`, mesh `heartbeat_ms`). Replace zeros with `null` (or omit) for "no limit," or pick a small positive value for the heartbeat fields.
4. Cross-backend cursor migrations require an explicit reset. If your deployment is migrating from JetStream to Redis (or vice-versa), drop the persisted cursor and let the consumer re-tail from the explicit start position.
5. If you call `MigrationOrchestrator` directly (rather than through the SDK's `DaemonRuntime::start_migration_with`), update to the chunked `Vec<MigrationMessage>` return shape and reassemble + seal + rechunk on the transport-identity-sealing path.
6. If your test suite covers the items in *Behavioral fixes that may surface as test breakage*, update the assertions.
7. Re-run your full suite. The lib + binding suites run green; the FFI / bindings layer now uses `catch_unwind` + `panic = "abort"` so any unwind across the boundary that previously "worked" is now a hard failure pointing at an unhandled panic source.

The mesh-specific findings remain queued for the next release. If your deployment runs heavy heartbeat traffic at high peer counts, or operates on AArch64 / RISC-V hardware, the High items in the mesh audit are worth tracking.

---

Released 2026-05-03.

## License

See [LICENSE](../../LICENSE).
