# Code Review — `bugfixes-7` vs `master` (2026-05-01)

Synthesized from a six-agent parallel review pass over the ~17k LOC / 122 file diff between `master` and `bugfixes-7` HEAD (`da74187`). The branch implements ~60 numbered fixes from `BUG_AUDIT_2026_04_30_CORE.md`.

The fixes themselves are mostly high-quality and surgical. This document captures issues the audit doc does **not** track — failures of fixes to fully close their stated hazard, regressions introduced by fixes, and gaps the audit didn't enumerate.

Each item is tagged `[Critical | High | Medium | Low | Nit]`. Critical items are merge-blockers.

## Status (2026-05-01)

**Ship-blockers: ALL FIXED** (CR-1 through CR-5).
**High-severity: ALL FIXED** (CR-6 through CR-12).
**Medium-severity: ALL FIXED OR DOCUMENTED** (CR-13 through CR-35).

| ID | Status | Notes |
|----|--------|-------|
| CR-1 | ✅ Fixed | Structured-id comparator in `consumer/merge.rs`; 6 regression tests |
| CR-2 | ✅ Fixed | `Rc<str>` → `Arc<str>`; PyO3 binding now compiles; 2 regression tests |
| CR-3 | ✅ Fixed | Python `__init__.py` re-export + `sdk-ts/src/redis-dedup.ts` shim; tests in both languages |
| CR-4 | ✅ Fixed | `reopen_with_retries` helper; locals-first atomic-swap; 3 regression tests |
| CR-5 | ✅ Fixed | `IdentityEnvelope::open` v1→v0 AAD fallback; 3 rolling-upgrade tests |
| CR-6 | ✅ Fixed | `likely_restart` no longer overwrites `n.addr`; security-invariant pin test |
| CR-7 | ✅ Fixed | `accept()` errors when called after `start()`; integration test |
| CR-8 | ✅ Fixed | All 5 sister dispatch branches iterate events; source tripwire test |
| CR-9 | ✅ Fixed | `SchemaType::try_from_slice` with byte-level depth pre-scan; 4 regression tests |
| CR-10 | ✅ Fixed | `CapabilityDiff::try_to_bytes` returns `Result<_, DiffSizeError>`; 4 regression tests |
| CR-11 | ✅ Fixed | Per-file `sync_all` after rename + loud_log on Windows no-op; updated test |
| CR-12 | ✅ Fixed | `tx_key()` accessor and field both removed; source tripwire test |
| CR-14 | ✅ Fixed | `ContextStore::SlotReservation` RAII guard; sampling-skip + panic-rollback tests |
| CR-15 | ✅ Fixed | `#[deprecated]` on `DiffEngine::apply` + `apply_unchecked` for internals; source tripwire test |
| CR-17 | ✅ Documented | `SnapshotStore::remove` ABA limitation pinned with test (deliberate) |
| CR-19 | ✅ Fixed | `ProbeGuard` RAII type added for async callers; 3 panic-safety tests |
| CR-20 | ✅ Documented | Inclusive-bounds duplicate-delivery on paginate-by-last-seen-ns pinned (deliberate) |
| CR-21 | ✅ Fixed | `getrandom` failures abort instead of panic in `context.rs` and `loadbalance.rs`; 2 source tripwires |
| CR-22 | ✅ Fixed | `include/net.h` and `bindings/go/net/net.h` synced with `-9`/`-10`; parity test |
| CR-31 | ✅ Fixed | `from_registry` / `capability_tags` parity test now compares ordered Vec (was HashSet) |
| CR-13 | ✅ Fixed | `MAX_PROOF_VERIFY_SPAN` tightened from 1M to 100K; per-peer rate-limit doc added; tripwire test |
| CR-16 | ✅ Fixed | `CausalCone::is_concurrent_with` uses exact `contains_origin` when both horizons populated; bloom-saturation regression test |
| CR-18 | ✅ Documented | `MetadataStore` soft-cap race pinned with concurrent-upsert test (deliberate) |
| CR-23 | ✅ Fixed | Recording mock adapter test pinning `flush()` + `shutdown()` are each called exactly once |
| CR-25 | ✅ Fixed | `shutdown_via_ref` second-caller deadline elapsed → `AdapterError::Transient` instead of silent `Ok` |
| CR-26 | ✅ Fixed | `flush()` doc now states ~37s worst case (5s + 2s + adapter_timeout) instead of misleading 5s |
| CR-27 | ✅ Fixed | `manual_scale_down` partial-success now logs WARN with stuck-shard list |
| CR-28 | ✅ Fixed | `PersistentProducerNonce` v1 wire format with version prefix; v0 back-compat path; 4 regression tests |
| CR-29 | ✅ Fixed | `with_nonce` 0→1 coercion doc rewritten — JetStream sentinel claim was fiction; defense-in-depth rationale documented |
| CR-30 | ✅ Documented | Read-path `validate_bounds` invariant pinned with test |
| CR-33 | ✅ Documented | `SuperpositionState::continuity_proof` genesis-edge + seq=1 cases pinned |

| CR-24 | ✅ Documented | Investigation found no per-daemon migration-coupled state in StandbyGroup or CapabilityIndex today; source-level tripwire pins this so future coupling forces a migration-handler update |
| CR-32 | ✅ Fixed | `cfg(not(test))` returns `MigrationError::StateFailed` in production; cfg-test fallback preserved for unit tests; source tripwire |
| CR-34 | ✅ Fixed | `assess_continuity` returns `Unverifiable` (not `Forked`) when non-genesis snapshot has empty `head_payload`; `DaemonHost::from_snapshot` warns on fallback for non-genesis; 3 regression tests |
| CR-35 | ✅ Fixed (PyO3) / Documented (NAPI) | PyO3 `is_duplicate` and `clear` now use `py.detach(\|\|...)` to release the GIL; NAPI doesn't have a GIL (audit framing was PyO3-specific) — module-doc clarifies. Source tripwire pins the PyO3 invariant |

Total regression tests added across all 35 closed items: **56** across Rust (lib + integration), Python (pytest), and TypeScript (vitest). Full lib test suite: **1360 passing** (up from 1322 baseline at branch start).

---

## Ship-blockers

### CR-1. `CompositeCursor`/`PollMerger` lex-compare wedges shipped adapters — `[Critical]`
**Files:** `src/consumer/merge.rs:90-119`, `~378`

The BUG #66 fix gates cursor updates on `existing.as_ref() >= new_id` (lexicographic). The doc comment claims this matches "Redis `{ms}-{seq}`" and "JetStream zero-padded seqs," but:
- `adapter/jetstream.rs:113` emits plain `seq.to_string()` (unpadded — `"9" > "10"`)
- `adapter/redis.rs:166-171` propagates the raw `{ms}-{seq}` (also unpadded — `"9-0" > "10-0"`)

After ~10 ingested events the cursor permanently freezes at `"9"` / `"9-0"` because every later id compares smaller. The fix to BUG #66 introduces a new wedge bug for stream ids past every decade-rollover. The same lex-compare appears in `PollMerger::poll`'s `id` tiebreaker after `(insertion_ts, shard_id)`.

**Fix:** zero-pad ids in adapters on ingestion, OR provide a typed comparator the adapter supplies, OR widen all id formats to fixed-width hex.

### CR-2. `RedisStreamDedup` is `!Send + !Sync`; PyO3 binding does not compile — `[Critical]`
**Files:** `src/adapter/redis_dedup.rs:53-87`, `bindings/python/src/redis_dedup.rs:39`

`RedisStreamDedup` stores `Rc<str>`. The module doc claims `Send + !Sync` (line 48) — that is false; `Rc` is `!Send`. PyO3 0.28's `assert_pyclass_send_sync` rejects this, so `cargo check` of `bindings/python` fails. The C-FFI and NAPI wrappers compile only because raw `*mut` deref bypasses the Sync auto-trait check, but the `concurrent_threads_on_one_handle_serialize_safely` test (lines 299-355) is technically UB in the Rust abstract machine — Miri/TSan will flag it.

The doc comment claims "switching to `Arc<str>` would gain nothing" — this is wrong. Without it the entire feature is unbuildable from Python.

**Fix:** change `Rc<str>` to `Arc<str>` and remove the misleading comment.

### CR-3. Python and TS bindings don't actually export `RedisStreamDedup` — `[Critical]`
**Files:** `bindings/python/python/net/__init__.py`, `sdk-ts/src/*`

- `__init__.py` does not import `RedisStreamDedup` from `_net`. The PyO3 module registration (`bindings/python/src/lib.rs:2095`) adds it to the native module, but the public Python API never re-exports it. The `sdk-py/README.md:149` and `bindings/python/src/redis_dedup.rs:11` docstrings tell users to `from net import RedisStreamDedup` — that import will `ImportError` once CR-2 is fixed.
- `sdk-ts` README documents `import { RedisStreamDedup } from '@ai2070/net-sdk'`, but `grep` against `sdk-ts/src` returns nothing. No TS shim re-exports the NAPI class.

**Fix:** wire up the Python `__init__.py` re-export and add the TS implementation/shim.

### CR-4. `redex::compact_to` post-rename file-handle reopen is not transactional — `[Critical]`
**Files:** `src/adapter/net/redex/disk.rs:1275-1303`, `src/adapter/net/redex/file.rs:984-998`

After the three renames + dir-fsync succeed, `compact_to` re-opens the new files (lines 1283-1300) and `try_clone`s them for the worker slots. Each step is `?`-propagated. If any of those six fallible calls fails (transient ENFILE/EMFILE, antivirus open-handle, permission flap), `compact_to` returns `Err` while:

- The on-disk renames have **already committed** — disk is post-compact.
- The six cached file slots still hold the **placeholder File objects** parked into them at lines 1253-1258, pointing at `std::env::temp_dir()`.
- `sweep_retention` then sees `Err` and (per the BUG #95 fix) leaves in-memory state untouched, so memory says 5 entries while disk says 2.

Subsequent `append_entry_inner` calls take the cached locks and write to placeholders in `/tmp`. This is the BUG #95 failure mode inverted — the BUG #95 fix is structurally incomplete because the disk operation isn't transactional.

**Fix:** either (a) re-open the handles on placeholders BEFORE the renames so a placeholder hold is the *intended* state on partial failure and the file is force-marked closed/poisoned; (b) wrap rename + reopen in a closure and on inner failure repoint slots to the on-disk paths before bubbling Err.

### CR-5. `IdentityEnvelope::open` AAD binding is a wire-format break with no migration story — `[High]`
**File:** `src/adapter/net/identity/envelope.rs:228, 350`

The BUG #127 fix adds `chain_link.to_bytes()` as AAD on both `seal` and `open`. Any envelope produced by `master` and opened by HEAD will fail AEAD with `SealOpenFailed`. There is no version tag in the envelope wire format and no migration test for a rolling upgrade where one peer is on `master` and one is on HEAD. Identity transport will break **during** the upgrade window.

Worse: the `expected_signer_pub: Option<&[u8;32]>` parameter is on a public API; new callers will follow the test pattern (8 of 11 in-tree calls pass `None`). The doc comment says "New call sites should pass `Some` whenever the expected source identity is known" — but nothing enforces it.

**Fix:** add a 1-byte construction tag in the envelope wire layout BEFORE flipping the AEAD AAD. Consider renaming the public API so the `None` form is `unsafe` / explicitly named (`open_legacy_no_signer_check`).

---

## High-severity (should fix before merge)

### CR-6. Pingwave `likely_restart` heuristic enables off-path proximity poisoning — `[High security]`
**File:** `src/adapter/net/swarm.rs:594-600`

The BUG #120 fix accepts a sequence regression on **unauthenticated** UDP and overwrites the recorded address:

```rust
let likely_restart = n.last_seq > 1 && pw.seq < n.last_seq.saturating_div(2);
if pw.seq > n.last_seq || hops < n.hops || likely_restart {
    n.last_seq = pw.seq;
    n.hops = hops;
    n.addr = from;       // attacker-controlled
    n.touch();
}
```

Pingwaves are unauthenticated ("raw UDP — not encrypted, topology is public"). Any peer who has observed a pingwave for `origin_id` can spoof `(origin_id, seq=1, hops=0, from=attacker_addr)` — with `n.last_seq > 1`, the heuristic fires and the entry is rewritten. Proximity routing now points at the attacker. The pre-fix drop-on-regression was the safer policy.

**Fix:** authenticate the pingwave (signature over `(origin_id, seq, …)` from a known static key), or constrain restart-acceptance to addresses already hosting a verified peer.

### CR-7. `try_handshake_responder` still races dispatcher; no test pins the limitation — `[High]`
**File:** `src/adapter/net/mesh.rs:6368-6395`, `:2449`

BUG #86's fix only patched the initiator. The responder still polls `socket_arc.recv_from` directly, while the dispatcher unconditionally does `pending_direct_initiators.remove(&source)` for every handshake packet. Because the responder isn't registered (only initiators register), the dispatcher silently drops the msg1 (`return;` at line 2449).

Result: any caller that invokes `accept()` *after* `start()` cannot receive its msg1 — the dispatcher swallows it. `connect_post_start.rs` deliberately calls `accept` BEFORE `start` and explicitly notes the limitation. There is no failing/ignored test that pins this restriction; an SDK consumer who orders `start()` before `accept()` will hang forever.

**Fix:** at minimum, `accept()` should panic/error if `started == true`. Better: extend the registry symmetrically (a `pending_responders: DashMap<u64, oneshot::Sender<…>>` keyed by `peer_node_id`, populated before `accept` awaits).

### CR-8. BUG #147 fix is incomplete; five sister sites still drop multi-event frames — `[High]`
**File:** `src/adapter/net/mesh.rs:2766, 2859, 2943, 2964, 2988, 3068`

BUG #147 turned `events.into_iter().next()` into `for event in events` for `SUBPROTOCOL_STREAM_WINDOW`. Five other dispatch branches still have the same shape:

- 2943 — `SUBPROTOCOL_CHANNEL_MEMBERSHIP` (Subscribe/Unsubscribe/Ack)
- 2964 — `SUBPROTOCOL_CAPABILITY_ANN`
- 2988 — `SUBPROTOCOL_REFLEX`
- 2766 — migration handler
- 2859 / 3068 — other event types

The codec supports multi-event frames; a peer that batches subscriptions or capability updates loses every event past the first.

**Fix:** apply the same `for payload in events` pattern to all five sister sites.

### CR-9. `SchemaType::validate` recursion cap not enforced during `Deserialize` — `[High security]`
**File:** `src/adapter/net/behavior/api.rs:308`, deserialize path

The BUG #109 fix bounds recursion in `validate_with_depth` at `MAX_SCHEMA_DEPTH = 128`, but `SchemaType` is `#[derive(Deserialize)]` with no custom deserializer, no `serde_json::Deserializer::set_recursion_limit`, and no manual depth tracking on the deserialize path.

The audit comment names "untrusted JSON parsed into `SchemaType`" as the threat surface. An attacker shipping a 1000-deep `Array { items: ... }` JSON crashes the process during `serde_json::from_slice::<SchemaType>` BEFORE `validate` is called.

`serde_json` defaults to a recursion limit of 128 for `Value`, but `from_slice` directly into a typed struct uses a different path — the limit isn't applied unless `Deserializer::from_slice(...).set_recursion_limit(N)` is threaded through every callsite.

**Fix:** add a `Deserialize` impl that tracks depth manually, or thread `set_recursion_limit` through every callsite that parses untrusted JSON into `SchemaType`.

### CR-10. `CapabilityDiff::to_bytes` lacks size cap on the send side — `[High]`
**File:** `src/adapter/net/behavior/diff.rs:228`

BUG #138 added `MAX_DIFF_BYTES = 64 KiB` and `MAX_DIFF_OPS = 1024` on `from_bytes`, but `to_bytes` is `serde_json::to_vec(self).unwrap_or_default()` with no size check. Honest senders that legitimately accumulate >1024 ops or >64 KiB encoded bytes produce diffs every receiver silently rejects via `from_bytes → None`.

Symptoms: silent state divergence — sender thinks state was applied, receiver discarded it. Indistinguishable from a network drop, and the same kind of silent-success bug BUG #151 was specifically introduced to fight.

**Fix:** assert at `CapabilityDiff::new` (reject `ops.len() > MAX_DIFF_OPS`), or return `Result<Vec<u8>, _>` from `to_bytes`.

### CR-11. `fsync_dir` is silent no-op on Windows; recovery cannot detect rename-tear corruption — `[High]`
**Files:** `src/adapter/net/redex/disk.rs:1357-1370`, regression at `:2241-2256`, rename block at `:1262-1278`

The BUG #93 fix is honest about Windows ("Power-loss durability of `compact_to` on Windows is therefore best-effort"), but:
- The regression test asserts only `Ok(())` — does NOT distinguish a real fsync from a no-op.
- Given this branch was developed on Windows (`cwd: C:\...`), the most-tested deployment surface gets the weakest durability.
- No `FlushFileBuffers` on the directory handle, no `MoveFileExW(... MOVEFILE_WRITE_THROUGH)` fallback.

Compounded with the still-non-atomic three-rename sequence: a power-loss between rename N and N+1 leaves on-disk state where idx/dat/ts version-mismatch. Recovery (`disk.rs:240-302`) is per-file and has no cross-file consistency check. The post-compact idx records' rewritten-relative offsets may *pass* the `(offset + len) > dat_len` torn-tail check by accident against a pre-compact dat, returning silently corrupt bytes on read.

**Fix:** either move to a manifest-pointer scheme (the deferred follow-up the comment names), or call `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` on Windows. At minimum, `loud_log` once on first `compact_to` on Windows so operators aren't surprised.

### CR-12. `NetSession::tx_key()` is dead, public, and reintroduces the BUG #97/#106 hazard — `[High]`
**File:** `src/adapter/net/session.rs:185-205`

The accessor's doc-comment claims it "exists for the mesh heartbeat timer (`mesh.rs`)" — but the heartbeat timer in `mesh.rs:3345` calls `session.build_heartbeat()`, not `tx_key()`. `Grep` confirmed zero callers crate-wide.

The accessor is a `pub` footgun: any future `let mut b = PacketBuilder::new(session.tx_key(), session.session_id());` re-introduces the independent-counter ChaCha20-Poly1305 nonce-reuse hazard that BUG #106 was meant to seal off. The drift-check tripwire test only catches `.build_heartbeat(`-shaped regressions, not `.tx_key()` ones.

**Fix:** remove the accessor entirely, or demote to `pub(super)` with `#[allow(dead_code)]` and a clearer comment ("retained only as a tripwire — must never gain a caller").

---

## Medium-severity (track as follow-up)

### CR-13. `MAX_PROOF_VERIFY_SPAN = 1_000_000` is a peer-controlled CPU-burn — `[Medium]`
**File:** `src/adapter/net/continuity/chain.rs:80, 144-151`

The cap exists, but no rate limiter wraps `verify_against` — a peer can ship a proof with span `999_999` repeatedly, forcing ~100ms-CPU per dispatch (xxh3 over 1M events × ~100ns each at 1KB payloads). The cap should be 100K, or the docs need a "verifier MUST be rate-limited per peer" callout.

### CR-14. `ContextStore::try_reserve_slot` lacks a Drop guard — `[Medium]`
**File:** `src/adapter/net/behavior/context.rs:871-905, 908-966`

Between `try_reserve_slot` succeeding and `contexts.insert` returning, the code does `Context::new`, `clone`, `should_sample`, and the DashMap insert. None of these are panic-infallible. There is no `ReservedSlot` RAII guard; a panic anywhere in this window leaks the reservation permanently. On a long-lived process `active_count` monotonically drifts up under repeated panics until `CapacityExceeded` fires for everyone.

`continue_context`'s `prev.is_some() { release_slot() }` check at line 945-963 is also TOCTOU-racy: between the `contains_key` and the `insert`, a third thread can run `complete_trace`.

**Fix:** implement `struct SlotReservation<'s>` with `Drop` that releases, and `mem::forget` it on the success path.

### CR-15. `DiffEngine::apply` is still public with no `#[deprecated]` — `[Medium]`
**File:** `src/adapter/net/behavior/diff.rs:565`

BUG #125's fix is contract-only, not enforced. `apply` remains `pub` without `#[deprecated]`. No production callers exist in `src/` today, but `benches/net.rs:2671/2686/2695/2700/2836` still calls `apply`, and the public surface invites the bug to come back.

**Fix:** add `#[deprecated(note = "use apply_with_version; see BUG #125")]`, or downgrade to `pub(crate)`.

### CR-16. Horizon bloom (BUG #130) escape hatch unenforced in callers — `[Medium]`
**File:** `src/adapter/net/state/horizon.rs`

BUG #130's bloom (m=64, k=3) has a documented "n > 16 falls back to ObservedHorizon::has_observed" escape hatch, but no gate in the conflict-resolution path actually checks cardinality and switches over. `potentially_concurrent` is called blindly. On any mesh that grows past 16 origins, conflict resolution silently regresses to "every event has observed every other event" — the pre-fix behavior.

**Fix:** add a cardinality check in callers and explicit fallback.

### CR-17. `SnapshotStore::store` ABA via retention — `[Medium]`
**File:** `src/adapter/net/state/snapshot.rs:559-587`

BUG #122 added monotonicity on `store`, but `remove(entity_id)` at line 587 takes the entry without checking `through_seq`. A retention task that calls `remove` then a stale producer could re-`store(snap_at_seq_3)` after the legit producer cleared `seq_5`. The `Vacant` arm (line 563) accepts unconditionally — there is no global "max-ever-seen seq per entity" record. So `store → remove → store(older)` is a legitimate API rollback. If retention is ever wired in, this is exploitable.

### CR-18. `MetadataStore` capacity check sits outside the entry-lock — soft-cap, undocumented — `[Medium]`
**File:** `src/adapter/net/behavior/metadata.rs:1023`

BUG #123's entry-based serialization narrows the same-`node_id` race window, but the capacity check is OUTSIDE the entry. Two concurrent upserts of distinct `node_id`s both pass the check and both insert past `max_nodes`. Acknowledged in comments ("the soft-cap race window is acceptable"), mirroring `TokenCache::insert_unchecked`. But the public API does not document this — if any caller assumes hard-cap semantics this is an invisible regression.

### CR-19. `release_half_open_probe` is `store(false)`, not CAS; not panic-safe — `[Medium]`
**File:** `src/adapter/net/behavior/loadbalance.rs:418`

`release_half_open_probe` is a `store(false, Release)`, not a CAS. Trivially idempotent in the "two stores both write false" sense, but NOT safe against:
- Completion-then-release race: thread A wins probe → request completes → `record_completion(true)` clears slot → thread B clobbers state. Harmless today (closed breaker doesn't gate on the slot), but contractually fragile.
- Panic between `try_claim` and `release` (or future-cancel) — slot leaks.

**Fix:** use `compare_exchange(true, false, ...)` and wrap the claim in a `ProbeGuard { ... } impl Drop`.

### CR-20. BUG #142 inclusive-bound fix introduces symmetric duplicate-delivery — `[Medium]`
**Files:** `tasks/query.rs`, `memories/query.rs`, `tasks/filter.rs`, `memories/filter.rs`

BUG #142 made `created_after`/`updated_after`/etc. inclusive (`>=`). This fixes the fence-post-drop bug but creates a symmetric fence-post-DUPLICATE bug: a paginator that stores `last_seen = N` and on next sync polls `created_after(N)` gets the event at `N` re-delivered. Receiver dedup by `id` masks it, but a same-`created_ns` legitimate update (real secondary write) collides and may overwrite.

The audit doc doesn't acknowledge this side. Downstream dedup is now load-bearing.

**Fix:** the canonical pattern is "exclusive on after, inclusive on before" (or vice versa) — pick one boundary side per cutoff field.

### CR-21. Two `getrandom().expect(...)` panics still reachable from FFI — `[Medium]`
**Files:** `src/adapter/net/behavior/context.rs:21`, `src/adapter/net/behavior/loadbalance.rs:1231`

BUG #150's `eprintln + abort` pattern was applied to `entity.rs`, `envelope.rs`, and `token.rs:190/347` but missed these two. Both are reachable via the mesh transport hot path from `bindings/*` — a `getrandom` failure here unwinds across `extern "C"` exactly the way #150 was supposed to fix. (Trace IDs / load-balance random aren't auth-bearing, so loss-of-availability is the only impact; still UB across FFI.)

### CR-22. `include/net.h` missing error codes; two C headers diverged — `[Medium]`
**Files:** `include/net.h`, `bindings/go/net/net.h`, `src/ffi/mod.rs:206-235`

`src/ffi/mod.rs:206-235` defines `IntOverflow = -9` and `MismatchedHandles = -10`. Neither header lists these — both jump from `NET_ERR_SHUTTING_DOWN = -8` straight to `NET_ERR_UNKNOWN = -99`. C/Go consumers receiving `-9` / `-10` fall into an unknown-code branch and lose actionable distinction.

`bindings/go/net/net.h` is a separate copy of the public header — drift between the two is a guaranteed long-term bug.

**Fix:** sync the two headers and add the missing codes. Consider auto-generating from the Rust source.

### CR-23. `shutdown_regression.rs` only tests memory adapter — does not verify BUG #80/#81 contract — `[Medium]`
**File:** `sdk/tests/shutdown_regression.rs:19-38`

Uses `Net::builder().memory().build()` — the memory adapter's `flush()` is a no-op. The test only asserts that `node.shutdown()` returns `Ok` and `is_shutdown_completed()` becomes true. The actual BUG #80/#81 promise is "the adapter's `flush()` / `shutdown()` actually run." A regression where someone changed `EventBus::shutdown_via_ref` to set `shutdown_completed=true` without calling `self.adapter.flush()` would still pass.

**Fix:** use a mock adapter that records flush/shutdown calls; assert the calls happened.

### CR-24. `MigrationFailed` arm missing StandbyGroup / CapabilityIndex cleanup — `[Medium]`
**File:** `src/adapter/net/subprotocol/migration_handler.rs:653-686`

BUG #111's fix calls `source_handler.abort`, `target_handler.abort`, `orchestrator.abort_migration_with_reason`, and `reassemblers.remove`. But if the failed migration had a standby promotion mid-flight (StandbyGroup was about to receive the new active), or if the source had advertised a capability tied to the migrating daemon, those entries are not torn down. The audit only enumerated the reassembler leak.

### CR-25. `shutdown_via_ref` second-caller spin returns `Ok(())` while shutdown still running — `[Medium]`
**File:** `src/bus.rs:951-985`

When the first caller is still in `shutdown_via_ref` past 10s (e.g., adapter slow-flush of 30s `adapter_timeout`), the second caller's CAS-loss spin returns `Ok(())` after 10s while the bus is still mid-shutdown. `is_shutdown_completed()` would still report `false`. Caller order-dependence is invisible. `Ok(())` at the trait level reads as "shutdown done."

**Fix:** the spin should either (a) wait until `shutdown_completed` flips, or (b) return a distinguished status (`AlreadyInProgress` vs `Done`).

### CR-26. `flush()` doc claims "5s overall" but actual upper bound is ~37s — `[Low]`
**File:** `src/bus.rs:811`

Phase 1 `timeout = 5s`, Phase 2 deadline up to 2s, Phase 3 uses `adapter_timeout` (default 30s). Total = up to 37s. Easy doc fix, but callers wiring `flush()` into HTTP handlers will see surprising latency.

### CR-27. `manual_scale_down` silently returns partial success — `[Medium]`
**File:** `src/bus.rs:1160-1191`

Returns `Ok(finalized)` even when `finalized.len() < target.len()` after the 2s deadline. The `manual_scale_down_finalizes_and_removes_drained_shards` test asserts `removed.len() == 2`, so this is a flake source under load. For callers it's a silent partial success.

**Fix:** at minimum log at WARN; ideally distinguish "all draining started but not all finalized" in the return type.

### CR-28. `PersistentProducerNonce` file format is unversioned/unmagic'd — `[Medium]`
**File:** `src/adapter/dedup_state.rs:29-32, 65-93`

8 raw LE bytes; `length != 8 → InvalidData`. Any future format change (HMAC-keyed nonce, extend to 16 bytes for `(epoch, nonce)`) cannot read old files without an out-of-band migration. The roundtrip test deliberately pins the format. Module docs don't admit this is a one-way door.

**Fix:** add a 1-byte version prefix now — cheap insurance.

### CR-29. `with_nonce` 0→1 coercion justified by a false premise — `[Nit]`
**Files:** `src/event.rs:380-417`, test at `:792-820`

Comments claim "JetStream's `Nats-Msg-Id` codec treats `0` as a sentinel" — `jetstream.rs:299-303` just renders `{:x}` of whatever it receives. Coercion is harmless (saved by upstream non-zero invariants) but the doc/test rationale is fiction. Reviewer-trap.

### CR-30. `validate_bounds` not called on read paths (defense-in-depth gap) — `[Low]`
**File:** `src/adapter/net/behavior/metadata.rs:1003`

`upsert` and `update_versioned` call `validate_bounds`. But `MetadataStore::get`, `query`, `find_nearby`, `best_for_routing` return `Arc<NodeMetadata>` straight from the DashMap with no read-side bounds check. If an entry slips past a previous, less-strict `validate_bounds` (snapshot restore that bypasses upsert, or future refactor that drops the call), no defense-in-depth catches it. An attacker who ships a 100 MB `name` field forces a 100 MB allocation BEFORE `validate_bounds` rejects it (deserialize side).

**Fix:** thread `serde_json` size limits at the deserialize boundary, or perform a per-field cap during deserialization.

### CR-31. `capability_tags()` and `from_registry` test compares as `HashSet`; sort-order asymmetry not pinned — `[Low]`
**File:** `src/adapter/net/subprotocol/negotiation.rs:50`

BUG #131's fix filters `handler_present` AND BUG #149's fix sorts by id in `from_registry`. The cross-check test `from_registry_and_capability_tags_advertise_the_same_subprotocols` (~line 487) compares as `HashSet` — does NOT verify `capability_tags()` is also sorted. If `capability_tags()` is consumed downstream as an ordered byte stream, the two channels could diverge.

### CR-32. `start_migration` fallback path warns silently in production — `[Low]`
**File:** `src/adapter/net/compute/orchestrator.rs:957-980`

When `source_handler` is `None`, the orchestrator logs `tracing::warn!` and falls back to the pre-fix direct-snapshot path (BUG #104's hazard). Production wiring sets the handler via `MigrationDispatcher::new`, so unreachable in production — but a misconfigured operator deployment silently loses post-snapshot events.

**Fix:** gate the fallback with `#[cfg(test)]` or return `MigrationError::SourceHandlerNotWired` in non-test builds.

### CR-33. `SuperpositionState::continuity_proof` silent failure mode at genesis — `[Low]`
**File:** `src/adapter/net/continuity/superposition.rs:158-174`

`saturating_sub(1)` means a head at sequence 0 produces `from_seq=0, to_seq=0`. The doc warns but: a head at sequence 1 ALSO produces `from_seq=0`, and the verifier `log.range(0, 0)` returns the genesis event only if the log contains it. After ANY snapshot-prune the proof becomes unverifiable for any seq-1 head. The round-trip test only exercises seq-3/4, not the boundary.

### CR-34. `assess_continuity` `head_payload` contract enforced only by hash-mismatch — `[Medium]`
**File:** `src/adapter/net/continuity/chain.rs:284-369`

The new snapshot-anchor `parent_hash` check (cubic-ai P1) requires `head_payload` to be populated. Default is `Bytes::new()`. After `from_bytes` (snapshot deserialization at line 449), `head_payload` is always empty; the only path that populates it is `with_head_payload` on the source side AND a manual call after `from_bytes` on the receive side. **No production caller of `from_bytes` is grep-visible in this branch that calls `with_head_payload` afterward.** Likely silently broken on the actual cross-node migration restore path.

### CR-35. NAPI/PyO3 GIL-release patterns not used on `RedisStreamDedup::is_duplicate` — `[Low]`
**Files:** `bindings/python/src/redis_dedup.rs`, `bindings/node/src/redis_dedup.rs`

`is_duplicate` takes a `Mutex<RedisStreamDedup>`; under contention it can block. The Python wrapper does not use `Python::allow_threads`; the Node wrapper does not release Node's threadsafe-function context. Operations are short, but a long-held mutex on a contended hot path will block the runtime.

---

## Items the audit doc tracks correctly (verified by review)

These are recorded for posterity — the fixes pass review:

- BUG #58, #61, #62, #67, #74, #79, #121, #150 (FFI panic-hardening)
- BUG #80/#81 `Net::shutdown` via `shutdown_via_ref` flag-based signal
- BUG #98 `ContinuityProof::verify_against` walks full range
- BUG #99 `SuperpositionState::continuity_proof` lo/hi-by-seq pattern
- BUG #128 `try_to_bytes` production callers thread `MigrationError` cleanly
- BUG #129 `prune_through` gate on `last_pruned.is_some()`
- BUG #134 `CortexAdapter::open` rejection gate is watertight
- BUG #140 `ObservedHorizon::observe` saturating_add
- BUG #141 Decode/Encode classification at fold callsites
- BUG #156 RingBuffer SPSC tripwire moved to debug_assertions
- BUG #155 `events_dropped` double-count corrected
- The `PeerRegistrationGuard` Drop-rollback pattern (#87)

---

## Recommended path

1. **Fix the five ship-blockers (CR-1 through CR-5) in this PR** — all are tractable.
2. **File follow-up issues for the high-severity items (CR-6 through CR-12)** — most should land in a follow-up before this branch's first production rollout.
3. **Track the medium/low items (CR-13 through CR-35) in a follow-up audit** — none are exploitable in isolation, but several (CR-14, CR-16, CR-17, CR-19, CR-34) are places where a future refactor or a quiet-but-malicious peer can break correctness without the test suite noticing.

Verdict: **substantively good work, ship-able after the five blockers** are addressed and the high-severity items are at least filed.
