# Phase 3 — Opt-in adapter audit (jetstream, redis, dedup, noop)

Scope: `src/adapter/{mod,noop,dedup_state,jetstream,redis,redis_dedup}.rs`.
Out of scope: `src/adapter/net/**`.

## Findings (severity-ordered)

### F-1 — `dedup_id` field is silently dropped on the bus poll path

- **File:line:** `src/adapter/redis.rs:205-278` (`parse_xrange_response`) and lines 398-421 (XADD writes both `d` and `dedup_id`)
- **Severity:** high
- **Bug class:** 3 (semantic confusion), 4 (header round-trip)
- **What:** Producer XADD attaches `dedup_id "{nonce:hex}:{shard}:{seq}:{i}"` to every entry — but `parse_xrange_response` only reads the `d` field and discards every other field. Any caller consuming via `bus.poll()` → `RedisAdapter::poll_shard` sees only the payload `r/t/s`, never the dedup id. The module docs (`redis.rs:47-65`) tell consumers they MUST filter on `dedup_id`, yet the adapter's own consumer path silently strips it. Only callers that bypass the trait and read XRANGE themselves (via `net_sdk::RedisStreamDedup`) get the contract; in-tree `bus.poll()` callers receive duplicates with no way to detect them.
- **Failure scenario:** producer hits `command_timeout` mid-EXEC (redis.rs:443), the EXEC still runs server-side, the bus retries, duplicate stream entries appear with the same `dedup_id`. A bus consumer (the in-tree path documented in `integration_redis.rs::test_redis_pagination`) receives both copies, no dedup possible, downstream processes the event twice.
- **Fix sketch:** Surface `dedup_id` on `StoredEvent` (new optional `dedup_id` field, or hide in metadata map) so trait callers can filter; or have the adapter dedup internally with a `RedisStreamDedup`-keyed LRU between XRANGE and the caller.

### F-2 — JetStream init drains prior client without timeout

- **File:line:** `src/adapter/jetstream.rs:290-302` (`init()` re-entry path)
- **Severity:** medium
- **Bug class:** 1 (reconnect storm), 8 (config error handling)
- **What:** On re-init `prior.drain().await` is unbounded. `drain()` waits for queued publishes to flush; if the broker is wedged the await blocks. The OUTER `EventBus::new` does wrap `adapter.init()` in `adapter_timeout`, so a fresh process bound at startup is safe, but any code that calls `JetStreamAdapter::init` directly after a previous successful init (e.g. a reconnect harness, not the bus) will hang on a dead broker. There is no `init` re-entry from the bus today, so the bus path is safe; the hazard is the direct-API contract.
- **Failure scenario:** operator wires `JetStreamAdapter::init` into a custom supervisor that re-inits after a NATS server restart while connections are still in `Reconnecting` state. `drain()` waits for pending publishes that will never ack against the half-dead client. Supervisor hangs.
- **Fix sketch:** wrap `prior.drain().await` in `tokio::time::timeout(self.config.connect_timeout, ...)`; on timeout, log warn and drop the prior handle.

### F-3 — `is_healthy` opens and discards a Redis connection per probe

- **File:line:** `src/adapter/redis.rs:538-573`
- **Severity:** medium
- **Bug class:** 6 (backpressure / flow control)
- **What:** Each `is_healthy` call invokes `self.client.get_multiplexed_async_connection().await` and drops the connection after PING. Orchestrator liveness probes that poll every 1-2 s create+tear down a TCP+TLS handshake per probe, which under Redis Cluster or `rediss://` TLS is dozens of ms and visible in `CLIENT LIST` churn. The comment explains why (avoiding multiplex-correlation leftovers from a cancelled PING), but the cost is real and the freshly-connected handle is also subject to the same multiplex correlation risk inside its single-shot lifetime.
- **Failure scenario:** k8s liveness/readiness probes at 1 s cadence × 20 replicas × 2 probe types = 40 connection setups/sec to Redis just for health checks. Visible in Redis's `connected_clients` metric and in TLS handshake CPU on the server.
- **Fix sketch:** keep a dedicated long-lived health-check `ConnectionManager` separate from the on_batch / poll_shard one; on PING failure tear it down once and re-establish on the next probe.

### F-4 — JetStream `is_transient_error` discards `BrokenPipe` distinction post-shutdown

- **File:line:** `src/adapter/jetstream.rs:338-340`, `src/adapter/redis.rs:190-197`
- **Severity:** medium
- **Bug class:** 3 (semantic confusion)
- **What:** Once `shutdown()` runs, `initialized = false`. Subsequent `on_batch` returns `AdapterError::Connection("adapter not initialized")`. `Connection(_)` is **non-retryable** (`error.rs:65-72`, exercised at `bus.rs:2027`). So a batch in flight that lands at the adapter after shutdown is dropped silently — but the bus's retry loop logs it as `reason = "non_retryable"` rather than as a shutdown race. There is no automatic re-init path: once shutdown has run, every subsequent batch is silently dropped until the operator constructs a new bus. No reconnect-after-down, by design — but undocumented in the trait contract.
- **Failure scenario:** orchestrator calls `bus.shutdown()` while in-flight batches are still queued in the ring buffer. BatchWorker dispatches one, adapter returns Connection, bus drops it. Data loss with only a warn log.
- **Fix sketch:** either (a) document on the `Adapter` trait that `shutdown` is one-way and queued batches will be dropped, or (b) introduce a distinct `AdapterError::Shutdown` variant so this drop cause is filterable from genuine config / auth Connection errors.

### F-5 — `dedup_state.rs` stack-marker shadowed by wall_nanos value

- **File:line:** `src/adapter/dedup_state.rs:154-155`
- **Severity:** low
- **Bug class:** 5 (dedup state correctness — entropy)
- **What:** `let stack_local: u64 = wall_nanos;` followed by `let stack_marker = (&stack_local as *const u64) as usize;`. The intent is to capture an ASLR-randomized stack address. By initializing the stack slot to `wall_nanos`, the *value* at that slot is predictable; only the *address* is randomized. In `hash_input` the address is at offset 16..24 and the value is repeated at offset 0..8 (via `wall_nanos`), so an attacker who controls clock skew sees the wall_nanos contribution twice. ASLR entropy is preserved, but the file's own claim of "~30 bits on 64-bit" is for the address alone. The OS-random samples at 48..64 dominate, so the net entropy is still adequate, but the in-line comment overstates the stack contribution.
- **Failure scenario:** academic; entropy from `RandomState::new()` covers the gap. Worth tightening for the next refactor.
- **Fix sketch:** use a fresh `let stack_local: u8 = 0;` so the slot value carries no wall-time correlation, or drop the stack-marker entirely and rely on the OS-random samples.

### F-6 — `RedisAdapter::on_batch` cancellation race documented but not closed

- **File:line:** `src/adapter/redis.rs:423-452`
- **Severity:** low (acknowledged; consumer-side mitigation in place)
- **Bug class:** 10 (async cancellation safety), 3 (semantic confusion)
- **What:** `tokio::time::timeout(self.config.command_timeout, pipe.query_async)` drops the future locally; the EXEC may still run server-side. The retry produces duplicate XADDs with new `*` ids. The adapter inserts `dedup_id` to make this filterable — but only via the SDK helper (see F-1), not via the bus's own poll path. So the inherent at-least-once-with-duplicates semantic of the producer path is exposed to bus consumers without the dedup contract being honored on the consumer side.
- **Failure scenario:** flaky Redis with command latency near `command_timeout` (default 1 s). Every slow EXEC produces a duplicate entry. Bus consumers see double-emit; SDK consumers using `RedisStreamDedup` are filtered.
- **Fix sketch:** see F-1 — surface `dedup_id` to the adapter's own consumer path, or wrap XRANGE in a `RedisStreamDedup`-keyed filter inside `parse_xrange_response`.

### F-7 — JetStream phase1/phase2 timeout maps to `Transient` regardless of cause

- **File:line:** `src/adapter/jetstream.rs:466-471, 491-495`
- **Severity:** low
- **Bug class:** 3 (semantic confusion)
- **What:** A `tokio::time::timeout` in phase1 OR phase2 returns `AdapterError::Transient("enqueue/ack phase timed out")`. The retry path then re-publishes the same batch. Dedup window (default 1 hour) discards duplicates, so semantically safe — but if the cause was actually a misconfigured `request_timeout` (too short for the batch size), the retry hits the same timeout forever until the bus exhausts retries. The error message doesn't distinguish "broker slow" from "request_timeout too low for batch size."
- **Failure scenario:** operator configures `request_timeout = 100ms` while sending 10k-event batches. Every batch times out in phase1, retries on the same timeout, exhausts retries, drops the batch.
- **Fix sketch:** include `batch.events.len()` and elapsed time in the timeout error message; consider sizing `request_timeout` per-batch as `base + per_event_us × len`.

### F-8 — JetStream cold-stream bail-gate test pins a stale invariant

- **File:line:** `src/adapter/jetstream.rs:955-979` (test) vs `src/adapter/jetstream.rs:597-705` (production code)
- **Severity:** low (test only)
- **Bug class:** 7 (correctness / dead invariant)
- **What:** `cold_stream_bail_gate_only_fires_when_first_seq_is_zero` references a `first_seq`-gated cold-stream bail that no longer exists in production code (the production path now uses `direct_get_next_for_subject` and the comments around lines 587-595 explicitly say "eliminates the cold-stream-bail and consecutive_not_found heuristics"). The test asserts numeric truths about `first_seq` values without exercising any production code — it's a constant-equality assertion masquerading as a regression test. Future readers will misread it as live coverage.
- **Failure scenario:** none (test passes trivially). The hazard is operational: a maintainer reads the test and believes the cold-stream-bail is still load-bearing.
- **Fix sketch:** delete the test or rewrite it to exercise `poll_shard` directly with an empty / sparse mocked stream.

### F-9 — Hardcoded `request_timeout` covers both publish and ack phases

- **File:line:** `src/adapter/jetstream.rs:454-456, 482-485`
- **Severity:** low
- **Bug class:** 6 (backpressure / flow control)
- **What:** Both phases share `self.config.request_timeout`. A 5 s default means the worst-case `on_batch` wall time is 10 s (phase1 timeout + phase2 timeout). The bus's `dispatch_batch` has its own outer `timeout` (`config.adapter_timeout`) — if `adapter_timeout < 2 × request_timeout` the bus cancels mid-phase2, dropping the phase2 future, leaving phase1's bytes on the wire (handled by dedup window) but losing the ack signal. Operationally fine; documentation-wise undocumented coupling.
- **Failure scenario:** operator sets `adapter_timeout = 5 s` and `request_timeout = 5 s` independently. A slow JetStream that takes 4 s in phase1 and 4 s in phase2 sees the bus cancel at 5 s wall; phase2 future is dropped. The bus retries, dedup window absorbs.
- **Fix sketch:** validate `adapter_timeout >= 2 × request_timeout` at config validation, or split into `publish_timeout` and `ack_timeout`.

---

## Bug-class checklist per file

**`src/adapter/mod.rs`** — clean for all bug classes. Trait surface defines `on_batch`/`poll_shard`/`init`/`shutdown` only; no transport state. Phase 1's "15 unwrap/expect" are entirely inside `#[cfg(test)]` (verified: every match for `\.unwrap\(\)|\.expect\(` is in the `mod tests` block — `mod.rs:200,208,209,212,215,256,265,271,273,275,278,284,294,296,297`). None reachable from network input.

**`src/adapter/noop.rs`** — clean. Confirmed.

**`src/adapter/dedup_state.rs`** — clean on 1/2/3/4/6/8/9. F-5 on class 5 (low). Phase 1's "17 unwrap/expect" are all in `#[cfg(test)]` (lines 339-624). The production code at 80-309 has no unwrap/expect (only `?` and `Result` returns).

**`src/adapter/jetstream.rs`** — clean on 1 (bus owns backoff + jitter at `bus.rs:1995-2012`, async-nats reconnects transparently), 2 (dedup window absorbs in-flight retries; cursor caller-maintained so no server-side advance race), 5 (per-process nonce file + JetStream `Nats-Msg-Id` works correctly), 9 (URL passed to `ConnectOptions`; nkeys/credentials not exposed yet — out-of-scope for current config surface). F-2/F-4/F-7/F-8/F-9 above. Phase 1's "8 unwrap/expect" all in tests (lines 804-998).

**`src/adapter/redis.rs`** — clean on 1 (bus backoff + redis-rs ConnectionManager auto-reconnect), 2 (XRANGE is server-side stateless; cursor caller-maintained), 5 (dedup_id stable across retries when `producer_nonce_path` set), 8 (URL validation at config; auth surfaces as Connection error → bus init fails hard), 9 (URL goes through `Client::open`; no logging of URL contents — but the URL CAN contain `redis://user:pass@host`, see below). F-1/F-3/F-4/F-6 above. Phase 1's "6 unwrap/expect" all in tests (lines 626-667).

**`src/adapter/redis_dedup.rs`** — clean on 1/2/4/8/9/10. Class 5 covered correctly (FIFO eviction documented, capacity clamp at `MAX_CAPACITY = 1<<24`). Class 6: the LRU is in-memory; a high-throughput consumer with capacity 4096 has ~0.4 s of dedup window at 10k events/sec (file calls this out at lines 137-151). Phase 1's "3 unwrap/expect" all in tests (lines 401, 414, 416).

## Additional note — credential handling in URLs

`Client::open(config.url)` (`redis.rs:101`) and `ConnectOptions::connect(&self.config.url)` (`jetstream.rs:307`) both accept URLs with embedded credentials (`redis://user:pass@host` or `nats://user:pass@host`). The `Debug` impl for `RedisAdapter` / `JetStreamAdapter` (lines 281-289 / 249-257) prints `config.url` verbatim. Any `tracing::debug!("{:?}", adapter)` exposes credentials in logs. The `tracing::info!` at init (`redis.rs:326-330`, `jetstream.rs:317-322`) also logs `url = %self.config.url`. **Severity: medium** for deployments where logs reach less-trusted observability stacks. Fix: redact the userinfo portion of the URL before emitting in any tracing or Debug output.
