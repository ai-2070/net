# BUG #56 + #57 — Cross-Process At-Least-Once Dedup

Resolves the paired deferred entries from
`BUG_AUDIT_2026_04_30_CORE.md:145, 150`. Both are duplicate-on-retry
hazards across process boundaries; both share the same fix shape
(stable, deterministic dedup ids derived from a durable producer
identity), but the *enforcement* point differs:

- **#56 (JetStream):** server-side dedup window does the work.
  Just need a stable `Nats-Msg-Id` across restarts.
- **#57 (Redis Streams):** no server-side dedup. Stable msg-ids
  flow through to a documented consumer-side dedup contract,
  with a reference helper provided.

## #56 — JetStream cross-process retry duplicates

`adapter/jetstream.rs:285-320` builds `Nats-Msg-Id` as
`{process_nonce}:{shard_id}:{sequence_start}:{i}`. The
`process_nonce` is sampled once at process start (`event.rs:322`)
and **does not survive restart**. A producer that crashes
mid-batch (server already accepted some events) and restarts gets
a fresh nonce; retransmits write *new* msg-ids; JetStream
persists the partial batch's accepted half twice.

**Fix:** make the nonce configurable per-bus and optionally
file-backed. New `PersistentProducerNonce` module
(`adapter/dedup_state.rs`) atomically loads-or-creates a u64
nonce at the configured path. `EventBusConfig` gains
`producer_nonce_path: Option<PathBuf>`. When set, the bus loads
the nonce on startup and threads it through every `Batch` (via
the existing `process_nonce` field, which keeps its name for
back-compat with the wire-level intent — it was always the
producer-identity slot, not literally a process-id). Same
`Nats-Msg-Id` format; just stable across restarts.

When `producer_nonce_path` is `None`, behavior is unchanged
(current per-process nonce). Documented as "at-most-once across
restarts" — same as today.

## #57 — Redis MULTI/EXEC timeout cancellation duplicates

`adapter/redis.rs:298-319` wraps the `EXEC` in
`tokio::time::timeout`. The local future is dropped on timeout,
but the bytes are already on the wire and the server may run the
EXEC anyway. The retry path issues another `MULTI/EXEC`,
producing two stream entries for each event. Each XADD gets a
server-generated `*` id, so consumers cannot dedupe at the
Redis-id level.

Redis Streams has no server-side dedup. The fix shifts dedup
responsibility to the consumer side, with the producer providing
a stable identity field that the consumer keys on.

**Fix:**

1. Each XADD entry gains a `dedup_id` field with value
   `{producer_nonce}:{shard_id}:{sequence_start}:{i}` — same
   string used for the JetStream `Nats-Msg-Id`. Stable across
   retries (deterministic from `(shard, seq, i)`) and across
   restarts (when `producer_nonce_path` is configured).
2. Keep the existing `MULTI/EXEC` wire shape — minimum churn.
   The duplicate-XADDs hazard remains on the producer side, but
   each duplicate carries the same `dedup_id`, so the consumer
   can filter.
3. Document the consumer-side dedup contract in `redis.rs` module
   docs. Provide a reference helper in `sdk/src/redis_dedup.rs`
   (`RedisStreamDedup`) that wraps stream reads and filters by
   `dedup_id` against an in-memory LRU.

The consumer contract:

- Read a stream entry, extract `dedup_id`.
- If the dedup_id is in the seen-set, skip.
- Otherwise process the event and add the id to the set.
- Set is LRU-bounded; sized to the worst-case dedup window the
  caller cares about (default: a few thousand ids, ~mins of
  flight at moderate throughput).

This addresses #57 in the sense the audit asked for — duplicates
no longer reach the application — while leaving the actual
duplicate XADD entries in the stream. The Redis storage cost is
unchanged.

## Files touched

Implementation:

1. **`src/adapter/dedup_state.rs`** (new module) —
   `PersistentProducerNonce` struct: `load_or_create(path)`,
   `nonce() -> u64`. Atomic write via `tempfile + rename`.
2. **`src/adapter/mod.rs`** — re-export.
3. **`src/config.rs`** — `EventBusConfig::producer_nonce_path:
   Option<PathBuf>`.
4. **`src/bus.rs::EventBus::new_with_adapter`** — load the nonce
   on startup; thread `producer_nonce: u64` through
   `BatchWorkerParams`.
5. **`src/shard/batch.rs::BatchWorker`** — gain
   `producer_nonce: u64` field; `flush()` stamps the nonce on
   the produced batch via a new `Batch::with_nonce`
   constructor.
6. **`src/event.rs`** — add `Batch::with_nonce(shard_id, events,
   sequence_start, nonce)`. `Batch::new` keeps the existing
   per-process-nonce behavior for tests / callers that don't
   thread a nonce through.
7. **`src/bus.rs::remove_shard_internal`** — stranded-flush uses
   `Batch::with_nonce` with the bus's `producer_nonce`.
8. **`src/adapter/redis.rs::on_batch`** — add
   `.arg("dedup_id").arg(...)` to each XADD command. Update the
   inline comment that documented the duplicate hazard.
9. **`src/adapter/redis.rs` module docs** — document the
   consumer-side dedup contract: how to read the field, how to
   key on it, the helper to use.
10. **`sdk/src/redis_dedup.rs`** (new) — `RedisStreamDedup`
    helper: in-memory LRU keyed on `dedup_id`. Optional, opt-in
    for callers that want consumer-side dedup.
11. **`sdk/src/lib.rs`** — re-export.

Tests:

12. Unit tests on `PersistentProducerNonce`:
    - first load creates the file with a random u64
    - second load (same path) returns the same nonce
    - corrupt file (non-u64 contents) → `io::Error`
    - parent directory must exist (clear error if not)
13. Integration test: bus restart with same `producer_nonce_path`
    produces the same nonce in `Batch::process_nonce`.
14. Integration test (Redis-feature-gated): bus produces
    duplicate XADDs (simulated via two consecutive `on_batch`
    calls with identical input) and `RedisStreamDedup` filters
    the second.
15. Negative test: without `producer_nonce_path`, two bus
    instances produce **different** nonces (documenting the
    documented at-most-once-across-restarts behavior).

Audit doc:

16. Mark **#56 FIXED**. Mark **#57 FIXED** as well — the
    duplicate is no longer user-visible after the consumer
    helper, even though the on-wire Redis stream may still carry
    duplicate entries. Add a note that the duplicate stream
    entries are an accepted trade-off (unchanged Redis storage
    cost; consumer-side filtering catches them).

## Phase 3 — Cross-language consumer helpers

The Rust `RedisStreamDedup` is the canonical implementation; the
Node and Python bindings ship thin wrappers so callers in those
languages get the same dedup semantic without rolling their own
LRU.

- **Node (`bindings/node/src/redis_dedup.rs` + TS surface in
  `index.d.ts`).** NAPI class wrapping the Rust LRU with the same
  `seen` / `mark` / `bounded_size` interface. Stream-entry
  inputs cross the FFI as `{ dedup_id: string, ... }` objects;
  `RedisStreamDedup.filter(entry) -> bool` returns whether the
  caller should process it.
- **Python (`bindings/python/src/redis_dedup.rs`).** Same shape
  via PyO3.
- **No SDK-level Redis client wrapping.** Callers bring their
  own `redis-rs` / `ioredis` / `redis-py` client; the helper
  takes `dedup_id: &str` (or equivalent) and answers
  yes/no. Keeps the helper transport-agnostic — works with any
  consumer wiring.

Tests:

- Both bindings get a small smoke test that constructs a helper,
  feeds it duplicate dedup_ids, and asserts the filter behavior.
  In-process testing is sufficient; we don't need a Redis
  fixture for the helper itself (the field is the contract).

## Non-goals

- Direction B (durable batch ledger) for full exactly-once across
  arbitrary crash windows. JetStream's 2 min dedup window covers
  the realistic case; durable ledgering is follow-up work.
- Switching the Redis adapter from `MULTI/EXEC` to one-XADD-per-event
  ("Path 1" in the design discussion). Heavier write change with
  more producer-side state; the dedup_id-field-plus-consumer-helper
  approach achieves the same user-visible semantic with much less
  churn.
- A Redis-backed seen-set (cross-worker dedup via a shared SET).
  In-memory LRU per consumer is fine for the documented contract;
  workers that need cross-process dedup can compose two helpers or
  share via their own state.

## Implementation order

**Phase 1 — `PersistentProducerNonce` + JetStream fix (#56)**
1. `PersistentProducerNonce` module + unit tests.
2. `EventBusConfig` + bus wire-up.
3. `Batch::with_nonce` + thread through `BatchWorkerParams` →
   `BatchWorker::flush` → stranded-flush.
4. JetStream adapter — no semantic change; just verifies the new
   nonce flows through.
5. Integration test for cross-restart nonce stability.

**Phase 2 — Redis dedup_id field + Rust helper (#57)**
6. Redis adapter — add `dedup_id` field, update doc.
7. `RedisStreamDedup` helper + unit tests.
8. Integration test for the helper.

**Phase 3 — Cross-language helpers**
9. NAPI binding (`bindings/node/src/redis_dedup.rs` + TS surface).
10. PyO3 binding (`bindings/python/src/redis_dedup.rs`).
11. Smoke tests in each binding.

**Wrap-up**
12. Audit doc — mark #56 and #57 fixed.
