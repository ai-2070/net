# nRPC Code Review — 2026-05-05

Branch: `nrpc` vs `master`. 33 commits, ~12,400 LOC added across 31 files implementing the nRPC request/response convention layer (cortex folds, channel queue-group primitive, Mesh glue, SDK typed wrappers + resilience helpers, metrics, streaming + flow control).

Five parallel review passes:

1. `cortex/rpc.rs` (~2822 lines) — folds, codec, cancellation, trace context, streaming pump
2. `mesh_rpc.rs` (~1289 lines) + `mesh.rs` / `pool.rs` diffs — Mesh glue, ServeHandle, UnaryCallGuard, RpcStream
3. `channel/{roster,membership,config}.rs` + `mesh_rpc_metrics.rs` — queue-group primitive, prefix-match registry, Prometheus metrics
4. `sdk/src/{mesh_rpc,mesh_rpc_resilience}.rs` — typed wrappers, retry/hedge/breaker
5. Test coverage across 12 new test files

## Critical

1. **`Cancelled` status is defined but never produced** — `cortex/rpc.rs:1186-1193`. CANCEL flips the in-flight token and removes the entry, but the handler that's already past its last `await` checkpoint still emits an `Ok` payload. Caller can't tell whether their CANCEL won. Either suppress the spawned task's emit when the token fired, or emit a synthetic `RpcStatus::Cancelled` from the CANCEL arm.

2. **Encode/decode failures on the client are reified as `RpcError::ServerError(Internal)`** — `sdk/src/mesh_rpc.rs:245-257`. `default_retryable` then retries them (a permanent local bug like `f64::NAN` in serde becomes 3 attempts) **and** trips the circuit breaker. Add a distinct `RpcError::Codec` variant; have both predicates skip it.

3. **`FLAG_RPC_IDEMPOTENT` is dead documentation** — `cortex/rpc.rs:84-87`. Constant defined, doc-string promises replay-cache behavior, no `completed_idempotent` LRU exists anywhere on this branch. Phase 1 deliverable per design doc line 513. Either implement the LRU or remove the flag.

4. **`CircuitBreaker` HalfOpen leaks `probe_in_flight=true` on panic** — `sdk/src/mesh_rpc_resilience.rs:818-870`. If the inner future panics, the bookkeeping block is skipped and the breaker is wedged forever. Wrap the admission flag in an RAII guard.

5. **Streaming flow-control: duplicate REQUEST with same `(origin, call_id)` overwrites the semaphore Arc while the prior pump still holds a clone** — `cortex/rpc.rs:1380-1386`. Old pump waits forever on the orphaned semaphore. Refuse REQUEST when key is already in-flight.

6. **Channel-config prefix iteration order is DashMap-shard order, not longest-prefix-match** — `channel/config.rs:85-90`. With `foo.` and `foo.bar.` both registered, which wins is non-deterministic across runs. Either sort by length descending or panic on overlapping prefixes.

7. **Wire-format break: new SUBSCRIBE frames are rejected by pre-queue-group nodes** — `channel/membership.rs:113, 147-148`. New senders unconditionally append the qg byte; old binaries' strict-trailer guard rejects. Either omit the qg byte when `queue_group.is_none()` (preserving byte-equivalence) or document lockstep upgrade.

## High

8. **`ServeHandle::Drop` aborts the bridge task gratuitously** — `mesh_rpc.rs:221-229`. Mid-`fold.lock().apply()` events get killed without emitting RESPONSE; callers time out. The bridge would exit cleanly on its own once the dispatcher is unregistered (rx returns `None`). Drop the `abort()`.

9. **`rpc_reply_subscriptions` grows unbounded** — `mesh_rpc.rs:1147-1214`. No eviction on peer disconnect; hash collision silently overwrites the prior dispatcher (`tracing::warn!` only) and orphans pending oneshots. Use a stable map keyed on (target, service) and surface collisions as `NoRoute` for the new entry.

10. **Server-side `RpcMetricsRegistry` and roster `queue_groups` map are unbounded** — `mesh_rpc_metrics.rs:202-210`, `channel/roster.rs:114-120, 526-538`. Attacker-controllable `service` strings / queue-group names leak ~200 bytes each forever. Cap or evict.

11. **`as u8` / `as u16` truncations on encode produce undecodable wire** — `cortex/rpc.rs:308, 493-501, 524`; `channel/membership.rs:147-148`. `service` > 255 bytes silently truncates the length byte. Add asserts or return `Result` from `encode`.

12. **Status codes 0x4000 / 0x4001 sit in the reserved canonical band** — `sdk/src/mesh_rpc.rs:516, 526, 572, 585`. Will collide with future canonical statuses; also raw magic numbers. Move to `0x8001` / `0x8002` and lift to named `pub const`.

13. **Jitter PRNG isn't actually decorrelated across simultaneous callers** — `sdk/src/mesh_rpc_resilience.rs:566-575`. Windows `SystemTime` resolution is ~15 ms; two callers in the same tick get the same `frac` and retry in lockstep. NTP-step also produces a zero-seed all-callers-collide path. Mix in `thread::current().id()` + a per-call address, or take a `fastrand` dep.

14. **`RoutingPolicy::RoundRobin` reads the next call_id with `load`, not `fetch_add`** — `mesh_rpc.rs:898-902`. Two concurrent `call_service` calls can both observe the same counter and pick the same target. Allocate `call_id` inside `select_target`.

15. **`RpcServerStreamingFold` has zero direct unit tests** — `cortex/rpc.rs`. Most complex code in the file (chunk ordering, terminal frame, panic, cancel-mid-stream, GRANT, semaphore overflow cap) is untested at the unit level. Some integration coverage exists but the documented per-call ordering guarantee isn't pinned.

16. **5 of 15 documented Phase 1 claims have zero test coverage**: `PermissionToken.rpc_services` rejection, idempotency replay, crash recovery / rehydrate, backpressure overload → `RpcError::Backpressure`, identity guard (`caller_origin` is AEAD-verified, not payload-claimed).

## Medium

17. **Doc lies about hedge cancellation** — `mesh_rpc_resilience.rs:316-324` says losers run to completion; `UnaryCallGuard::Drop` (`mesh_rpc.rs:435-447`) actually fires CANCEL. Update the doc.

18. **`max_backoff` enforced before jitter, not after** — `mesh_rpc_resilience.rs:559-576`. Effective ceiling is `max_backoff`, not `min(max_backoff, jittered)`. Most users want a true ceiling.

19. **Hedge "last error" is non-deterministic** — `mesh_rpc_resilience.rs:531-541`. `select_all` overwrites `last_err` in completion order, so the surfaced error depends on which hedge lost the race. Prefer the primary's error or aggregate.

20. **Unbounded mpsc on streaming server pump** — `cortex/rpc.rs:1418, 1685`. If flow control is opt-in and caller doesn't use it, a fast handler balloons RAM. Add a bounded fallback (e.g. 1024) with a metric.

21. **Header-name comparison inconsistent** — `parse_stream_window_initial` uses `eq_ignore_ascii_case`; `extract_trace_context` uses exact match. Pick one (`cortex/rpc.rs:702-708, 446-468`).

22. **`publish_to_peer` "no session" maps to `Transport`, not `NoRoute`** — `mesh_rpc.rs:1064`. Users expect `NoRoute` for "I don't know this peer". Add a typed `AdapterError::NoSession` variant.

23. **In-flight gauge can briefly read negative; emitted as `-1` on the wire** — `mesh_rpc_metrics.rs:393, 466`. Prometheus rejects negative gauges. Clamp at 0 in the formatter.

24. **`serve_rpc` does not auto-register `ChannelConfig`** in `mesh_rpc.rs` despite the design-doc claim. Either implement or strike from the doc.

25. **Test flakiness pattern**: hard-coded latency thresholds (`mesh_rpc_hedge.rs:127` — 200 ms slack on a 600 ms budget; `integration_nrpc_streaming.rs:253-266` — sleep-then-assert on metrics), `wait_until` 25 ms polling without bounded slack, busy-poll-bool patterns in 3+ tests.

26. **Mutex `expect("breaker mutex poisoned")` panics propagate poison forever** — `mesh_rpc_resilience.rs:763, 770-774, 821, 877`. Use `parking_lot::Mutex` (already transitively a dep) or `unwrap_or_else(|p| p.into_inner())`.

27. **`RpcStreamTyped::Streaming::Unary` branch silently bridges a unary response to a streaming consumer** — `cortex/rpc.rs:1750`. Masks server bugs. At minimum `tracing::warn!`.

28. **Deadline check ignores clock skew** — `cortex/rpc.rs:1080-1087`. A peer with a slightly-fast clock gets prematurely timed out. Add a configurable skew tolerance (gRPC default ~10 s).

29. **`AssertUnwindSafe` comment misstates the danger** — `cortex/rpc.rs:1136-1148`. Real hazard is `parking_lot::Mutex` not poisoning on handler panic. Document.

30. **Random routing test could fail on bad-luck seed** — `integration_nrpc_service_discovery.rs:317-329`. xxh3-mod-2 over `call_id ∈ {1..40}` could hit all-even by chance.

## Low / Nit

- Setup duplication across 6 test files (`build_node`, `handshake_pair`, `wait_until`) — extract to `tests/common/mod.rs` (~250 lines saved).
- Hard-coded PSK `[0x42u8; 32]` in every test — per-test PSK would prevent any cross-test leak.
- `escape_label` doesn't handle `\r` (`mesh_rpc_metrics.rs:527-538`).
- Histogram `le` label uses `{}` Display on `f64` — `1.0` displays as `"1"`, breaking Grafana stable-label expectations (`mesh_rpc_metrics.rs:411-413`).
- `prometheus_text` is one ~150-line function — refactor to `write_counter`/`write_gauge`/`write_histogram` helpers.
- Phase 3 markers in doc comments for landed features (`cortex/rpc.rs:69-73, 94, 99`).
- `request_wire_size` re-encodes to compute size — add `encoded_len()`.
- Several `pub` items should be `pub(crate)` (`RpcInboundEvent`, `RpcInboundDispatcher`).
- `examples/nrpc_echo.rs` uses leading-underscore-named `_serve_*` bindings (footgun for copy-pasters; `let _ = ...` would silently drop the handle), and a 50 ms sleep to dodge a connect-before-accept race.

## Coverage matrix vs. design ✅ entries

| Design ✅ claim | Covered? |
|---|---|
| Queue-group one-of-N | Yes (queue_group_dispatch.rs) |
| Broadcast + queue-group coexistence | Yes |
| Correlation across concurrent calls | Yes (loopback) |
| Deadline → CANCEL emission | Partial — caller side observes `Timeout`, but no test asserts CANCEL event was actually published |
| Caller drop → CANCEL | Yes (`rpc_dropped_call_future_fires_cancel_to_server`) |
| **Idempotency replay** | **NO** |
| Server panic → Internal | Yes |
| **Backpressure** | **NO** |
| **Token-scope rejection** | **NO** |
| **Identity guard** | **NO** |
| **Crash recovery** | **NO** |
| Trace context propagation | Yes |
| Streaming + flow control + grants | Yes |
| Retry / hedge / breaker / metrics | Yes |
| Service discovery + RoutingPolicy | Yes |

**5 of 15 documented Phase 1 claims are unverified by tests.**

## Top action items

If only five things ship before merge:

1. **Wire compatibility (#7)** — decide lockstep upgrade vs preserving byte-equivalence; document either way.
2. **Codec-error retry/break (#2)** — the cleanest one-variant fix that prevents a misclassification cascade through retry + breaker + metrics.
3. **Cancelled status (#1)** — ship the documented status code; otherwise the API is misleading.
4. **HalfOpen panic leak (#4)** — wraps in 5 lines, prevents permanent breaker wedge.
5. **Coverage for the 5 missing Phase 1 claims (#16)** — at minimum identity-guard and token-scope, since both are security-load-bearing.

## Architectural notes

The shape is solid: the cortex-fold framing, queue-group primitive, asymmetric routing (REQUEST direct-unicast / RESPONSE roster-based), and `Bytes`-shared retry payload all read well. The bugs cluster in three areas:

- **Lifecycle edges** — drop, panic, in-flight cleanup, semaphore lifetime
- **Entropy + error classification** — jitter quality, codec-error misclassification, status-code reservation
- **Wire-format encode-side defenses** — silent truncation on oversize inputs, queue-group byte forward-compat asymmetry

No structural rework needed; the punchlist is concrete and most items are 5-50 line fixes.
