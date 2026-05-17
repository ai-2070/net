# Phase 3 — Umbrella Findings (2026-05-18 audit)

**Crate:** `ai2070-net v0.18.0`
**Inputs:** eight module reviews + Phase 1 automated passes.

- [`PHASE1_REPORT.md`](./PHASE1_REPORT.md) — clippy / check / fmt sweep
- [`PHASE3_FFI_REVIEW.md`](./PHASE3_FFI_REVIEW.md) — `src/ffi/` + `include/`
- [`PHASE3_BUS_SHARD_REVIEW.md`](./PHASE3_BUS_SHARD_REVIEW.md) — `src/bus.rs` + `src/shard/`
- [`PHASE3_CONSUMER_REVIEW.md`](./PHASE3_CONSUMER_REVIEW.md) — `src/consumer/`
- [`PHASE3_REDEX_DISK_4GIB.md`](./PHASE3_REDEX_DISK_4GIB.md) — F2 follow-up
- [`PHASE3_WIRE_DECODERS.md`](./PHASE3_WIRE_DECODERS.md) — 5 fuzz-targeted decoders
- [`PHASE3_CAPABILITY_AUTH.md`](./PHASE3_CAPABILITY_AUTH.md) — capability, identity, token, channel auth
- [`PHASE3_ADAPTER_JETSTREAM_REDIS.md`](./PHASE3_ADAPTER_JETSTREAM_REDIS.md) — opt-in adapters
- [`PHASE3_CORTEX_RPC_DROP.md`](./PHASE3_CORTEX_RPC_DROP.md) — temp-Drop cluster drill

## Headline

The crate's hygiene is high — zero default-clippy code warnings, clean fmt, clean feature matrix, no `mem::transmute` in production, `catch_unwind` discipline at every FFI boundary, handle-quiescing protocol verified sound, SPSC ring buffer ordering correct, no locks held across `.await`, every `tokio::spawn` has its `JoinHandle` tracked, no `block_on` in production async paths, `unsafe` confined to FFI + one audited SPSC ring buffer. The single Phase-1 corruption hazard (F2 — `payload_offset as u32`) was a false alarm; `MAX_SEGMENT_BYTES = 3 GiB` and an upstream `offset_to_u32` guard make the cast lossless by construction.

That said, the deeper batch surfaced **three high-severity authorization bugs** in the capability layer that did not show up in the automated passes. These are the headline of the audit.

## Critical / high — fix promptly

### A-1 — `PermissionToken` keyed on 32-bit channel hash; collision → cross-channel auth grant
- **Source:** Capability/auth F-1.
- **File:line:** `PermissionToken` definition + `TokenCache::check` consulted from `channel/config.rs:111-153`.
- **What:** `ChannelHash = xxh3_64(name) as u32`. xxh3 is non-cryptographic; a u32 collision is reachable in ~2^32 work (offline, no rate limit). The hot-path cache lookup (`can_subscribe` / `can_publish`) keys *only* on the u32, so two distinct channel names with colliding hashes share token authorization. `AuthGuard::is_authorized_full` has a name-level backstop, but the cache path bypasses it.
- **Attack:** attacker grinds a channel name colliding with a victim channel they hold a delegated token for, registers it, and gains publish/subscribe on the victim channel via the cache fast path.
- **Fix sketch:** widen `ChannelHash` to `u64` (still xxh3 keyspace), or include the channel name in the cache key, or run the name-equality check on every cache hit (not just on `is_authorized_full`).

### A-2 — No token revocation; delegated children outlive parent revocation
- **Source:** Capability/auth F-2.
- **File:line:** `PermissionToken::delegate` and `TokenCache`.
- **What:** Delegation copies the parent's `not_after` verbatim with no parent-link chain. `TokenCache` exposes only `evict_expired`. There is no revocation list, no parent-link walk, no version bump. Once a token is delegated, "revoking" the parent does nothing to outstanding children.
- **Attack:** compromise of a long-lived parent token leaks indefinitely through children; rotation of a single tenant's keys cannot purge in-flight children.
- **Fix sketch:** add a per-issuer monotonic generation number to tokens; the cache rejects tokens whose generation is below the current issuer floor. Or: include a parent-id in children, walk on validate.

### A-3 — Announcement-dedup precedes TOFU pin → forwarder-poisoning DoS on channel auth
- **Source:** Capability/auth F-3.
- **File:line:** `src/adapter/net/mesh.rs:5145-5153` (dedup short-circuit) and `:5221` (TOFU pin).
- **What:** Announcement dedup keys on `(node_id, version)` and short-circuits *before* the TOFU pin runs. A forwarded (relayed) announcement primes the dedup table for `(victim_node_id, version)`. The victim's own direct announcement with the same key is then silently dropped, so `peer_entity_ids[victim_node_id]` never populates and any `require_token` channel the victim publishes to fails closed.
- **Attack:** any peer in the mesh that relays announcements can poison a victim's auth state by forwarding a forged-but-consistent announcement first.
- **Fix sketch:** swap the order — TOFU pin first, dedup second. Or: dedup key includes the immediate source-peer, so forwarder + direct don't collide. The lack of a regression test for "forwarder then direct" is a **dead invariant** — every existing TOFU test uses clean pairs.

### C-1 — `ShardMetricsCollector` silently inert on the production hot path
- **Source:** Bus/shard F-1.
- **File:line:** `src/shard/mod.rs:146-189` (`Shard::try_push_raw` / `Shard::try_push`).
- **What:** `record_push` / `record_buffer_len` are never called from production — only tests. Downstream:
  - `finalize_draining` (`mapper.rs:1185-1207`) sees `pushes_since_drain_start == 0` always; the "shard actually empty" predicate is a no-op and any Draining shard finalizes after the 100 ms timer regardless of contents. Only the bus's `remove_shard_internal` stranded-flush prevents event loss today.
  - `evaluate_scaling` reads `fill_ratio == 0`, `event_rate == 0` for every shard, so the "underutilized" autoscale trigger matches every Active shard every tick, masked only by warmup + cooldown.
- **Fix sketch:** wire `record_push` / `record_buffer_len` into the push paths (the field is already plumbed). Add a regression test that polls the collector after N pushes.

### H-1 — `slice::from_raw_parts` without `isize::MAX` guard at 14 FFI entry points
- **Source:** FFI F-1.
- **File:line:** `cortex.rs:1171, 2897`; `mesh.rs:1768, 2243, 2323, 2425, 2452, 2476, 2509`; `blob.rs:239, 307, 937, 945, 976, 1016`.
- **What:** `slice::from_raw_parts` requires `len ≤ isize::MAX`. The guard exists at 6 sibling sites (`mod.rs:737, 787, 873, 1637`; `mesh.rs:1354, 1923`); the 14 above missed it. A C caller forwarding `(size_t)-1` (e.g. a sign-extended Go `int = -1`) hits immediate UB. `include/README.md:1024-1027` falsely claims three of these are guarded — the doc is wrong.
- **Fix sketch:** add `if len > isize::MAX as usize { return NetError::InvalidJson.into(); }` before each `from_raw_parts` call; update / remove the README paragraph. ~15 minutes.

### A-4 — Redis adapter `dedup_id` round-trip broken end-to-end
- **Source:** Adapter F-1.
- **File:line:** `src/adapter/redis.rs:47-65` (producer contract documented) vs `parse_xrange_response`.
- **What:** Redis adapter writes `dedup_id` on every XADD. Its own `parse_xrange_response` discards the field on read. The documented producer-side dedup contract is silently broken for any bus consumer polling via the trait — events that *should* be deduped by id come through with no dedup data attached.
- **Fix sketch:** parse and propagate `dedup_id` in `parse_xrange_response`. Add a round-trip integration test (publish with id X, read back, assert id == X).

## Medium

### M-1 — `SnapshotReassembler::feed` allows total-chunks substitution
- **Source:** Wire decoders F-4.
- **File:line:** `src/adapter/net/.../orchestrator.rs:877-881`.
- **What:** The `total_chunks == 1` fast-path returns the new chunk as a completed snapshot without consulting existing `pending` state. A peer that shipped chunk 0/3 can follow up with chunk 0/1 and have the second payload accepted as the complete snapshot, dodging the `TotalChunksMismatch` guard.
- **Fix sketch:** check `pending` for a different `total_chunks` before taking the fast path.

### M-2 — Consumer `merge.rs` double-fetches on duplicate `shard_id`
- **Source:** Consumer F-1.
- **File:line:** `src/consumer/merge.rs:469-513`.
- **What:** `request.shards` consumed verbatim — `vec![0, 0, 1]` issues two `poll_shard(0, …)` calls; payload contains duplicate events (cursor stays correct).
- **Fix sketch:** `shards.sort_unstable(); shards.dedup();` before the empty-check.

### M-3 — `CompositeCursor::update_from_events` advertised but production `poll()` bypasses
- **Source:** Consumer F-2.
- **File:line:** `merge.rs:223-272` (def) vs `:558-560, :746-760` (prod cursor advance).
- **What:** Documented format-mismatch refusal lives in `update_from_events`. Production writes the cursor via direct `nc.set(...)`, skipping the guard. A JetStream→Redis mid-stream migration overwrites the cursor with the new-format id silently.
- **Fix sketch:** route `nc.set` through `update_from_events` or downgrade the public visibility on the latter.

### M-4 — Wall-clock `Instant` deadlines inside tokio-virtualized sleep loops
- **Source:** Bus/shard F-2/F-3.
- **File:line:** `src/bus.rs:1680, 1684` (manual_scale_down), `2270, 2272` (drain worker `finalize_deadline`). Same anti-pattern fixed at `bus.rs:1388-1392` was missed here.
- **Fix sketch:** swap `std::time::Instant::now()` for `tokio::time::Instant::now()`.

### M-5 — `EventBus::Drop` takes parking_lot mutexes
- **Source:** Bus/shard F-4.
- **File:line:** `src/bus.rs:1793` → `src/shard/mod.rs:696`.
- **What:** `total_pending_in_rings` takes each shard's mutex; on a single-thread runtime + panic during shutdown, drop can be invoked on a thread already holding a shard lock → deadlock.
- **Fix sketch:** `try_lock_for(short)` or use the lock-free atomic counters in `ShardManager::stats()`.

### M-6 — `net_blob_publish` / `net_blob_resolve` allocator-layout coupling fragile
- **Source:** FFI F-2.
- **File:line:** `src/ffi/blob.rs:259-263, 324-328`, freed at `:342-347`.
- **What:** `Vec → into_boxed_slice → Box::into_raw` paired with `Box::from_raw(slice_from_raw_parts_mut(...))`. Sound today (shrink-to-fit) but implicit; a refactor to `Vec::leak` would silently break dealloc.
- **Fix sketch:** route returned buffers through the explicit `std::alloc::Layout` path that `mesh.rs:alloc_bytes` / `net_free_bytes` already use.

### M-7 — `OpaqueCtx` carries C pointers across worker threads with no documented contract
- **Source:** FFI F-3.
- **File:line:** `src/ffi/blob.rs:449-450, 473-474`; entry `:694-735`.
- **What:** `unsafe impl Send + Sync` is sound at the type level, but `net_blob_register_callback_adapter` takes `ctx: *mut c_void` with no API affordance for declaring thread-safety. A C caller registering a non-thread-safe context (Python `PyObject*` without GIL, Go-routine-local pointer) races inside `spawn_blocking`.
- **Fix sketch:** document the cross-thread requirement on the C signature, or serialize vtable dispatch behind a per-adapter mutex.

### M-8 — JetStream init `drain().await` is unbounded
- **Source:** Adapter F-2.
- **File:line:** `src/adapter/jetstream.rs` (init path).
- **What:** unbounded drain can hang adapter init / shutdown indefinitely on a slow broker.
- **Fix sketch:** wrap in `tokio::time::timeout(...)`.

### M-9 — Redis `is_healthy` opens a fresh TCP+TLS connection per probe
- **Source:** Adapter F-3.
- **File:line:** `src/adapter/redis.rs`.
- **What:** health-check probes hammer the broker with full-handshake connections under load; on TLS this is expensive.
- **Fix sketch:** maintain a long-lived probe connection or use a `PING` on the working pool.

### M-10 — Post-shutdown `on_batch` returns non-retryable `Connection` error
- **Source:** Adapter F-4.
- **What:** after `shutdown`, batches return `Connection` (non-retryable in the bus's classification) → silent drops. Trait doesn't document this.
- **Fix sketch:** return a typed `Shutdown` error (retryable=false but distinguishable); document in the trait.

## Low

L-1: `String::from_utf8_unchecked` on serde-json output (FFI F-4, `predicate.rs:220`). Sound today; replace with checked form.
L-2: `alloc_bytes` writes via raw pointers without internal null check (FFI F-5, `mesh.rs:1968-1973`).
L-3: `blob.rs` uses `pub unsafe extern "C" fn`; other modules use `pub extern "C" fn` (FFI F-6) — style drift.
L-4: `Ordering::None` non-deterministic across shards but undocumented (Consumer F-3).
L-5: `failed_shards` recovery has no surfaced backlog hint (Consumer F-4).
L-6: Workspace `[profile]` sections silently ignored in 4 binding crates (Phase 1 F-1).
L-7: Bus/shard subtleties (Bus/shard F-5..F-8) — publish-then-spawn ordering, lossy-shutdown reconciliation off-by-one under specific interleave, transient `events_dropped` overcount in `DropOldest`, `collect_and_reset` cross-field non-atomicity.
L-8: Wire `RouteFlags::from_u8` masks `& 0x0F` — 16 wire bytes alias to same flags; future high-nibble bits silently disagree across upgrades (Wire F-3).
L-9: Wire `BufferedEvents` `payload_len + 8` can wrap on **32-bit** targets only (Wire F-1).
L-10: Wire — no per-event payload cap (Wire F-2).
L-11: Wire — zero-byte chunks bypass byte cap (bounded by 700 K chunk-count) (Wire F-5).
L-12: Capability `signed_payload` signs empty payload if `serde_json::to_vec` fails (Capability F-4).
L-13: Capability inbound metadata accepts well-known reserved exact-match keys (Capability F-5).
L-14: Capability case-folded channel names hash to different u32 (Capability F-6).
L-15: Capability `TokenCache::check` predicate missing subject re-check (Capability F-7).
L-16: Capability — no clock-skew tolerance in token validity / delegate (Capability F-8).
L-17: Capability wildcard-slot walk on every miss (Capability F-9) — DoS amplifier under cache-thrash.
L-18: Adapter JetStream `request_timeout` covers both phases → wall-clock is 2× configured (Adapter F-9).
L-19: Adapter credential-in-URL logged verbatim — may leak `redis://user:pass@host` to log sinks.

## Null results (explicitly clean)

These categories were searched and found clean:

- Production float comparisons (90 lint hits all in test code).
- `mem::transmute` in production — zero.
- `unsafe impl Send/Sync` — only on FFI handles; all sound except for the M-7 contract gap.
- `tokio::spawn` join-handle discipline — every spawn captured + awaited with bounded timeouts.
- `block_on` in production async — zero (only in tests / doc comments).
- Locks held across `.await` — none found.
- `unsafe` outside `src/ffi/` — only `shard/ring_buffer.rs` (audited, correct SPSC).
- Panic-across-FFI from `unwrap`/`expect`/`panic!` — every production unwrap traced is compile-time-safe, infallible-fallback, or wrapped in `catch_unwind`/`ffi_guard`.
- `tokio::select!` cancellation safety — zero `select!` blocks in `src/consumer/`; concurrency is `join_all`.
- Double-free of handle-internal allocations — `HandleGuard::begin_free` single-winner protocol verified.
- `Box::into_raw` / `Box::from_raw` pairing — every traced site has a matching free.
- `#[no_mangle]` collisions — feature-gated counterparts ensure exactly one definition per cdylib.
- NUL-termination / interior-NUL handling — typed error variant at every Rust→C string boundary.
- Handle quiescing protocol — Dekker-style SeqCst on `(active_ops, freeing)` correct.
- `RingBuffer` SPSC ordering — verified correct.
- Shutdown SeqCst handshake — correct.
- `redex/disk.rs` 4 GiB cast (Phase 1 F-2) — cap-bounded; cast lossless by construction.
- Filter recursion depth — bounded by serde-json default (128).
- Stream-id decade-rollover comparison — pinned by JetStream + Redis tests.
- Cortex `rpc.rs` Drop-temp cluster — all 19 hits are style; no real bugs.
- Wire decoders — `CapabilityAnnouncement::from_bytes` and `natpmp::decode_response` clean.
- Adapter reconnect — bus owns jittered exponential backoff; async-nats / redis-rs handle reconnect.
- Adapter unwrap/expect counts — all in `#[cfg(test)]`.
- `dedup_state.rs` + `redis_dedup.rs` correctness — versioned, atomic rename, OS-entropy mix; FIFO eviction with capacity clamp at 16.7M.
- `noop.rs` adapter — clean.
- `IdentityEnvelope` seal/open — sound.
- Schema-doc-guard — purely a CI doc-drift test, no auth surface.

## Suggested action order

1. **A-1, A-2, A-3** (capability/auth) — these are the highest-leverage finds. A-3 has a partial mitigation in place (auth fails closed rather than fails open) but is still a denial-of-service primitive a malicious mesh peer can wield. A-1 and A-2 are bypass primitives.
2. **C-1** (wire shard metrics into hot path) — 30 min + regression test.
3. **H-1** (14 `from_raw_parts` length guards + README fix) — ~15 min, mechanical.
4. **A-4** (Redis dedup_id round-trip) — fix + integration test.
5. **M-1** (snapshot reassembler total-chunks substitution) — pre-empt with `pending` check.
6. **M-4 / M-5** (tokio-time / drop-time mutex) — small.
7. **M-2 / M-3** (consumer dedup + cursor format-mismatch routing).
8. **M-6 / M-7** (FFI allocator layout, OpaqueCtx contract).
9. **M-8 / M-9 / M-10** (adapter hardening).
10. Lows can batch into a single cleanup commit, except **L-19** (credential-in-URL log leak) which deserves its own redaction commit.

## Coverage gaps carried forward

- **Phase 2** (Miri / ASan / TSan / fuzz): user-skipped. TSan + libfuzzer Linux-only; would need WSL or Linux runner. Existing `fuzz/fuzz_targets/` is wired and ready.
- **Cross-language conformance (Phase 4):** Rust/TS/Py/Go SDK round-trip property tests not started.
- **Dep audit:** `cargo-audit` / `cargo-machete` / `cargo-deny` / `cargo-udeps` not installed; needs user approval.
- **Adjacent surfaces not reviewed this round:** `src/adapter/net/dataforts/` (blob storage core beyond the FFI surface), `src/adapter/net/compute/` (compute orchestration), `src/adapter/net/redex/replication_*` (replication coordinator + state), `src/adapter/net/meshos/` (MeshOS daemon authoring), NetDB query layer (`netdb`). Each is a candidate for a future targeted review.

## Verdict

The crate is well-engineered — automated passes are clean, concurrency primitives are correct, FFI hygiene is sound, and the audit-history files under `docs/misc/` show the team has been doing this work consistently. The new finds break into two buckets:

1. **Three capability/auth bugs** (A-1, A-2, A-3) that the automated tooling cannot catch — they require understanding the trust model. These are the most important results of the audit.
2. **A handful of mechanical bugs** (C-1, H-1, A-4) that escaped clippy because they're "the right thing isn't called" rather than "the wrong thing is called."

Total: 4 highs + 6 mediums + 19 lows + 25 null-result categories. Highest leverage is the A-series; together with C-1, H-1, A-4 they're under a day's work to fix.
