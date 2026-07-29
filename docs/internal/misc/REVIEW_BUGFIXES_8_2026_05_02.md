# Code Review — `bugfixes-8` vs `master` (2026-05-02)

**Scope:** 79 commits, ~5000 lines across ~90 files. Branch addresses 74
audit items from `BUG_AUDIT_2026_05_02.md`: 51 code fixes + 13 docs + 4
rejected as non-bugs + 1 already-correct + 5 already-categorized. 36 new
regression tests; baseline went from 1391 → 1427 passing.

Three parallel review passes covered:

- **Pass A:** `bus.rs` / `consumer/` / `shard/` / `event.rs` / `timestamp.rs` / `config.rs`
- **Pass B:** `adapter/net/{redex, state, compute, identity, subnet, channel, behavior, cortex, pool, config}`
- **Pass C:** adapters (`jetstream`, `redis*`, `dedup_state`) / SDK / bindings (Node + Python) / FFI / integration tests

The most consequential findings were verified against the source after
the agent passes returned.

---

## Real issues (must fix before merge)

### 1. `num_shards` decremented on rollback that never incremented it

**Files:** `net/crates/net/src/bus.rs:504`, `net/crates/net/src/shard/mod.rs:746-893`

The activate-failure rollback path (`bus.rs:504`) calls
`shard_manager.remove_shard(new_id)`, which `fetch_sub(1)`s `num_shards`
whenever the table had the id (`shard/mod.rs:890-893`). But `add_shard`
deliberately does **not** bump `num_shards` — only `activate_shard`
does on transition (`shard/mod.rs:782-784, 805-809`).

On the activate-failure path:

1. `add_shard` runs (table grows, `num_shards` unchanged)
2. `activate_shard` errors (`num_shards` still unchanged)
3. Rollback `remove_shard` decrements anyway → `num_shards` ends 1 below actual table count.

The `#35` fix (gating `fetch_add` on `transitioned`) traded one
accounting bug for another. Fix: gate the rollback `fetch_sub`
symmetrically, or split `remove_shard` into "remove unmapped
provisioning" vs "remove active". Add a regression test pinning
`num_shards` invariance across an `add_shard` + failed `activate_shard`
+ rollback cycle.

### 2. `RedisStreamDedup` stripped from generated Node binding surface

**Files:** `net/crates/net/bindings/node/index.d.ts:893+`, `net/crates/net/bindings/node/index.js:594`

`bindings/node/index.d.ts` lost the entire `RedisStreamDedup` class
declaration; `bindings/node/index.js:594` lost the export line (commit
`9301cf8` "Fix cubic issues"). The Rust source
(`bindings/node/src/redis_dedup.rs`) is unchanged and `Cargo.toml` lists
`redis` as a default feature, so this looks like a regenerate-without-features
slip — the same regen picked up `testInjectSyntheticPeer` (added by
`--features test-helpers`) but dropped `redis`. npm consumers lose
`RedisStreamDedup` entirely.

Fix: re-run NAPI generation with `--features redis,…` so the published
TypeScript and JS surfaces match the Rust class.

---

## Concerns (worth addressing in this PR)

### 3. `RedexFile::Debug` deadlock footgun

**File:** `net/crates/net/src/adapter/net/redex/file.rs:1210-1218`

The `Debug` impl calls `self.len()` and `self.next_seq()`, both of
which take the non-reentrant `state.lock()` (parking_lot,
`file.rs:345-346, 369-371`). Two separate acquisitions in one Debug
print also produce torn reads. Any future `tracing::?(?file, …)`
inside an existing locked region will deadlock.

Fix: read `next_seq.load(Acquire)` directly and surface `len` from a
snapshot, or drop those fields from the Debug output.

### 4. `include_str!` of `go/net.h` escapes the crate root

**File:** `net/crates/net/src/ffi/mod.rs:1592`

Path was changed from `../../bindings/go/net/net.h` to
`../../../../../go/net.h`. The path resolves (verified —
`<repo-root>/go/net.h` exists), but the crate now has a compile-time
dependency on a file two levels above its own root, breaking
`cargo publish` and any out-of-repo vendoring of `net/crates/net/`.

Fix: restore the in-crate copy of `net.h`, or keep the in-crate copy
and have the parity test compare both.

### 5. `SnapshotStore::store` equal-seq race on `high_water`

**File:** `net/crates/net/src/adapter/net/state/snapshot.rs:589-605`

`store` reads `high_water` under one shard guard, then takes a
separate guard on the `snapshots` map. Two concurrent stores at the
same `seq` both see `high_water < seq`, both bump `high_water = seq`,
and the snapshots side arbitrates. The doc claim "we just verified
new_seq is > prev high_water" is false for the loser. Net impact is
benign here, but the doc/invariant should match reality.

### 6. Activate-failure rollback can hang under backpressure

**File:** `net/crates/net/src/bus.rs:482-540`

`workers.drain.await` and `workers.batch.await` are unconditional. If
the drain worker is parked in a `sender.send().await` against a full
channel or in `tokio::select!` waiting on `drain_finalize_ready`,
rollback blocks indefinitely. The pre-fix `.abort()` was wrong, but
unbounded await here can hang the rollback path.

Fix: bound the await with `tokio::time::timeout`, or drop the
bus-side sender clone before awaiting so the worker wakes when its
peer's channel closes.

### 7. `SdkError` enum is breaking and not `#[non_exhaustive]`

**File:** `net/crates/net/sdk/src/error.rs:11-35`

`#10` adds `Sampled` / `Unrouted` variants and changes
`From<IngestionError>` to no longer route everything through
`Ingestion(String)`. External callers matching on `SdkError` stop
compiling; callers matching on the *string content* of `Ingestion`
silently stop matching. Intentional per `#10` but the breaking
surface is larger than the audit item framed it.

Fix: bump SDK major version, add `#[non_exhaustive]` for future-proofing,
and call this out in CHANGELOG.

### 8. `Identity::issue_token` now panics on `Duration::ZERO`

**File:** `net/crates/net/sdk/src/identity.rs:152-153`

Wraps `try_issue_token(...).expect(...)`. Bindings route around it via
`try_issue`, but a pure-Rust caller passing `Duration::ZERO` now
process-aborts instead of getting a born-expired token. The `# Panics`
doc note at `:140` is the right mitigation, but a `debug_assert!` +
soft-clamp would be safer for an SDK surface.

### 9. `activate()` budget gate is load-then-store, not CAS

**File:** `net/crates/net/src/shard/mapper.rs:825-843`

Two concurrent activates on distinct provisioning ids both observe
`current < max_shards`, both `fetch_add(1)`, total exceeds
`max_shards`. The held write lock on `shards` serializes this in
practice today (the `#72` doc-only memory), but the comment doesn't
call this out. A `compare_exchange` would be defense-in-depth.

### 10. `dedup_state::create_new(true)` poison after crash

**File:** `net/crates/net/src/adapter/dedup_state.rs:241-249`

The tmp filename is derived from a counter; a crashed prior run
leaves a stale tmp that breaks every subsequent save. The comment
says "caller retries with a fresh tmp name" but no retry logic is
visible.

Fix: `remove_file(tmp).ok();` before `create_new`, or fall back to
`truncate(true)`.

### 11. `RedexIndex::clear()` on Lagged is silent

**File:** `net/crates/net/src/adapter/net/redex/redex/index.rs:151-172`

On `Lagged`, the index task wipes the entire `(K, V)` map and
re-tails live-only, with `tracing::warn!` as the only signal. For an
aggregating index this drops counts. For state-rebuild
(`MemoriesAdapter`), this is correct. Worth surfacing as a sentinel
item on the public index API for downstreams that need to react —
currently the only way a downstream learns is by polling `len()`.

### 12. `RedexIndex` saturation-resume can hot-loop

**File:** `net/crates/net/src/adapter/net/redex/redex/index.rs:181-211`

If the watcher repeatedly drops under sustained burst load with a
small `tail_buffer_size`, the loop pattern is
`tail.next() → None → clear → re-tail → None → …` with no backoff.
Every recovery emits `tracing::warn!`. Production deployments with
under-sized buffers will see log floods.

Fix: small backoff, or rate-limit the warn.

### 13. `compare_numbers` brittle with `serde_json/arbitrary_precision`

**File:** `net/crates/net/src/adapter/net/behavior/rules.rs:138-160`

Today's tree doesn't enable the `arbitrary_precision` feature, but if
a transitive dep ever does, `as_i64`/`as_u64`/`as_f64` all return
`None` for big-int-shaped values; the function returns `None` and
the rule silently fails to fire.

Fix: `debug_assert!(value.is_i64() || value.is_f64() || value.is_u64())`
or surface `None` distinctly from "not comparable".

### 14. `migration_source::start_snapshot` snapshot side-effects on race

**File:** `net/crates/net/src/adapter/net/compute/migration_source.rs:71-119`

The DashMap-guard fix (`#14`) is correct, but the comment only
acknowledges "wasted snapshot if two callers race". For a daemon
whose `MeshDaemon::snapshot()` is non-idempotent (counters, deferred
I/O), two concurrent migration starts now produce two snapshot
side-effects where the previous code produced one. Worth documenting
or constraining.

### 15. `MigrationTargetHandler::drain_pending` lacks regression test

**File:** `net/crates/net/src/adapter/net/compute/migration_target.rs:425-462`

No test pins the "second drain skips already-delivered prefix"
invariant. The dedup branch only drops events with
`seq <= replayed_through`. Through the current entry points this
can't double-deliver, but the invariant is load-bearing for `#1`.

Fix: add a regression test exercising mid-batch failure → restored
tail → second drain.

### 16. `add_events(empty)` regression coverage missing

**File:** `net/crates/net/src/shard/batch.rs:232-249`

`#66` was tagged doc-only, but the side effect (timeout-flush of
`current_batch`) load-bears for the activate-failure rollback path.
No test pins this; if a future refactor "fixes" the surprise by
making `add_events([])` a true no-op, the rollback silently loses
events.

Fix: unit test asserting `add_events(vec![])` may return
`Some(Batch)` after `max_delay`.

### 17. `drain_finalize_ready` deadline-break ordering doc

**File:** `net/crates/net/src/bus.rs:1212-1257`

On the deadline-break, `break` jumps to the SeqCst store without the
producer-completed happens-before. The new `lossy=true` flag and
`events_dropped` increment make this observable, but the comment
still implies "every observed-pre-shutdown push" is visible — which
is exactly what's *not* true on the deadline path.

Fix: tighten the doc.

### 18. `Cursor::encode` panic propagation

**File:** `net/crates/net/src/consumer/merge.rs:98-115`

Switching from `unwrap_or_default()` to `expect(...)` is correct (no
silent rewind). `serde_json::to_string` on `HashMap<u16, Arc<str>>`
is unreachable in practice (`Arc<str>` guarantees valid UTF-8), but
a panic in `encode` propagates from `poll()`, which is `async` and
may abort a runtime worker. Acceptable for a "BUG" panic, but
consider returning `Err` to keep `poll()` non-panicking.

### 19. `PER_SHARD_FETCH_CAP` made public

**File:** `net/crates/net/src/consumer/merge.rs:268`

Public const exposure is API surface — downstream code can match on
the value. If you want to tune later, mark `#[doc(hidden)]` or keep
it private and surface only the bool.

### 20. `retry_backoff` jitter test is statistically loose

**File:** `net/crates/net/src/bus.rs:2402-2450`

Test asserts `>= 8` unique values across 16 shards at attempt=2.
With `jitter_range = 200` this is empirically fine but not
guaranteed; `DefaultHasher` is not stable across Rust toolchains
and could flake.

Fix: deterministic hasher or relax the bound to `>= 4`.

### 21. `SnapshotStore::forget` is not gated

**File:** `net/crates/net/src/adapter/net/state/snapshot.rs:625-633`

Any caller can clear the high-water mark and re-store an older
snapshot. The doc says "Use this when the entity is being
legitimately rebound" but there's no authority check; an attacker
who can call `forget` arbitrarily defeats the entire point of `#8`.

Fix: `pub(crate)` if all callers are internal, or add an audit log.

### 22. `JetStream::deserialize_event` Fatal drops accumulated events

**File:** `net/crates/net/src/adapter/jetstream.rs:564-571`

A single bad record drops everything the loop had successfully read
in the same call. The cursor will re-walk those on retry. Probably
acceptable since `Fatal` is non-retryable, but document the
behavior.

### 23. No empty-batch test on Python bindings for `#27`

**File:** `net/crates/net/bindings/python/tests/test_redex.py`

Lacks an `append_batch([])` → `None` case. Node side has it
(`bindings/node/test/redex.test.ts:45-51`), Rust side has it. Not a
regression — non-empty path still asserts correctly.

---

## Notes (positive findings & context)

### Audit-item propagation verified clean

- **`#27` breaking change** propagates consistently across
  `RedexFile::append_batch`, `append_batch_ordered`, `OrderedAppender`,
  `TypedRedexFile`, Node binding (`bindings/node/src/cortex.rs:286,295`),
  Python binding (`bindings/python/src/cortex.rs:319,327`),
  `redex/typed.rs:60`, and integration tests
  (`tests/integration_redex.rs:232,1038,1046`,
  `bindings/node/test/redex.test.ts:45-51`,
  `redex/file.rs:1350`). No callers missed.
- **`#23` retention boundary** is unambiguously inclusive-on-cutoff
  (`retention.rs:84` flips `>` to `>=`; new test
  `bug23_entry_at_exact_cutoff_is_retained` pins the semantics).
- **`#56` Lemire mapping** is correct and well-tested (5% bias bound
  over 30k trials, `shard/mapper.rs:540-549`).
- **`#6` Python GIL release** is sound everywhere checked. Every
  `py.detach` block in `bindings/python/src/groups.rs` (lines 274,
  285, 290, 376, 380, 385, 487, 493, 497, 502) captures only owned
  primitives or `&Arc<…>` via `self.inner` — no `Py<PyAny>` is
  touched while detached. Daemon callbacks correctly re-attach via
  `Python::attach` (`bindings/python/src/compute.rs:963`).
- **`#4` u32→u8 truncation** is fully fixed in Node bindings;
  `bindings/node/src/groups.rs:672, 689` use `try_into`. The one
  remaining `as u8` (`subnets.rs:77`) is gated by an explicit
  `> 255` check at line 71.
- **`#11` `next_seq` lock acquisition** is correct given the
  rollback semantics — `fetch_add` + disk write + `fetch_sub` on
  failure are all under `state.lock()`.
- **`#14` `start_snapshot`** correctly removes the dashmap entry
  guard from across `daemon_registry.snapshot()`.
- **`#37` anti-replay log** at `crypto.rs:481-489` is a structured
  warn before zeroing — surfaces what was previously silent without
  changing correctness.
- **`#46` (membership trailing bytes)** correctly advances cursor
  past the channel name + token in all three message-type branches
  before checking `cur.remaining() == 0`.
- **`#67` `PollMerger` shard-id propagation** is consistent: both
  add and remove paths re-store via `shard_ids()`. The doc on
  `poll()` accurately describes topology-snapshot semantics.

### Hygiene & scope

- **Scope creep is minimal.** `#74` (jittered backoff) is a justified
  pairing with `#21`. The `Drop` doc block (`#49`), `last_scaling`
  doc (`#72`), and `poll()` topology doc (`#50`) are pure docs,
  well-targeted.
- **Test renaming/cleanup** of "BUG #N" references in public test
  names is consistent. Good hygiene.
- **`shutdown_was_lossy`** is `AtomicBool` with no reset — intentional
  one-shot lifecycle flag, but worth noting for callers holding
  long-lived stats refs across multiple buses.
- **`MAX_CAPACITY = 1<<24`** (`adapter/redis_dedup.rs:96`) is a
  sensible bound.
- **`consecutive_not_found_cap = max(64, fetch_limit/2)`**
  (`adapter/jetstream.rs:521`) is a reasonable heuristic.
- **`MIN_BACKOFF_INTERVAL = 1ns`** (`sdk/src/stream.rs:90`) is a
  tight, principled choice.
- **Doc-only `#70`** (`src/ffi/mod.rs:21-50`) accurately describes
  the `Runtime::block_on` reentrancy hazard.

---

## Bottom line

Findings **#1** (rollback decrement) and **#2** (NAPI binding regression)
are merge-blockers. The remaining 21 concerns are tractable cleanups;
many can ship as follow-ups. Overall the branch is high-quality —
fixes are tightly scoped, regressions are well-tested, and the audit
doc gives reviewers a fighting chance.
