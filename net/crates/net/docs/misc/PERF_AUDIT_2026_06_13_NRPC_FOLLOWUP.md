# nRPC perf follow-up (2026-06-13)

Successor to `PERF_AUDIT_2026_05_19_NRPC.md`. That audit's Tier 1 landed
and was re-benched on 2026-05-26; this doc records where the budget
stands now, which of the original items have since landed, and ranks
the remaining opportunities. No new benchmark run was taken for this
doc — numbers cited are from the May 26 re-bench.

> **Course-correction (same day, before implementing):**
> [`../plans/NRPC_QPS_CONCURRENCY_SCALING_PLAN.md`](../plans/NRPC_QPS_CONCURRENCY_SCALING_PLAN.md)
> (Phase 0 findings, 2026-06-01) post-dates the May audit and
> experimentally disproves this doc's original c16/c128 framing for
> items #2 and #3: the worker-thread sweep is flat AND the channel-shard
> bench (own bridge + own fold mutex per channel) makes throughput
> *worse*, so the fold mutex is **not** the throughput ceiling. The
> ceiling is the **single recv loop's syscalls + wakeups** (one
> `recv_buf_from` per packet; a reliable unary RPC is 4 packets because
> the StreamWindow grant is the sole ACK). The real throughput levers
> are the ack-piggyback wire change
> ([`../plans/NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md`](../plans/NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md))
> and batched receive (item #4). Items #1/#2 below remain valid as
> **c1-latency / wake-reduction** work and were landed as such — their
> original "lifts the c128 ceiling" claims are struck.

## Where nRPC stands

| Configuration | Baseline (May 19) | After Tier 1 (May 26) | Δ |
|---------------|-------------------|-----------------------|------|
| c1/32B        | ~69.6 µs          | **42.5 µs**           | -39% |
| c128/32B      | ~1.84 ms          | **1.12 ms**           | -39% |
| c128/32B QPS  | 69.7 K elem/s     | **114.3 K elem/s**    | +64% |

Two anchor facts from the original audit still frame everything:

- **Physics floor** (transport + AEAD + framing) is ~5 µs per
  round-trip. AEAD was measured and ruled out as a bottleneck
  (cipher_comparison data; T4.1 dropped).
- The 2026-05-26 flamegraph shows **51% of CPU in
  `NtWaitForAlertByThreadId`** — tokio futex waits. The remaining
  cost is wakeups and task handoffs, not crypto or memcpy.

The cheap wins are gone. What's left is **wakeup reduction** (per-call
latency) and **de-serializing the response path** (c128 throughput).

## Status of the original audit items

Landed since (or alongside) the May 26 re-bench:

| Item | What | Evidence |
|------|------|----------|
| T1.1 | Stream-grant drainer (spawn-storm fix) | `mesh.rs::spawn_stream_grant_drainer_loop`, commit `b70e10290` |
| T1.2 | Response leg → `publish_to_peer` direct | `mesh_rpc.rs::publish_response_to_caller`, commit `813f951ba` |
| T1.3 | Per-service `RpcRoute` cache (+ `ChannelName(Arc<str>)`) | `mesh.rs::rpc_route_for_service` |
| T2.2 | `body: Vec<u8>` → `Bytes` zero-copy + `encode_into` | `cortex/rpc.rs` (`RpcRequestPayload::body` doc, perf #84) |
| T3.1 | Reply-subscription registry → `DashMap<(target, xxh3(service))>` | `mesh_rpc.rs::ensure_reply_subscription` (§3.5) |
| T3.6 | `decrypt_in_place` on the receive path | `crypto.rs`, `pool.rs` (perf #129) |
| §3.8 | Pooled call-id entropy (no per-call getrandom syscall) | `mesh_rpc.rs::mint_random_call_id` |
| §8a  | Response emit → bounded mpsc + drain task (no spawn-per-response) | `mesh_rpc.rs` serve_rpc emit closure |
| §3.10/§3.11 | Cached hash/stream_id on streaming paths; zero-alloc header append | `mesh_rpc.rs` |

Still open: **T2.1**, **T2.3**, **T3.2**, **T3.3** (verify still
relevant), **T3.4**, **T3.5**, **T4.2** (deferred).

## Ranked remaining opportunities

### 1. T2.3 — inline handler dispatch when the future is immediately ready

- **What:** Every REQUEST does `tokio::spawn(handler.call(ctx))`
  (`cortex/rpc.rs:1606`), even when the handler is synchronous and
  resolves on first poll. For echo/lookup-style handlers this is pure
  spawn + wake overhead.
- **Fix:** Poll the handler future once inline; only spawn if
  `Pending`. Keep `catch_unwind` on the inline path so panic-recovery
  semantics don't change.
- **Payoff:** ~1–3 µs/call plus one fewer wake per request — directly
  targets the futex-wait flamegraph signal.
- **Scope note:** the same spawn pattern repeats in all four folds
  (unary `RpcServerFold`, server-streaming, client-stream, duplex —
  `cortex/rpc.rs:1606, ~2220, ~2800, ~3000+`). Fix as a shared helper,
  land per-fold commits.
- **Status: landed (2026-06-13), unary fold only.**
  `cortex/rpc.rs::spawn_or_inline` Box-pins the handler task, polls it
  once with a noop waker, and spawns only on `Pending` — safe because
  tokio guarantees a fresh task an initial poll, which re-registers a
  real waker. Deliberately NOT applied to the streaming/client-stream/
  duplex folds: their handler tasks await a pump `JoinHandle` (and an
  async terminal emit), so they are always `Pending` at first poll and
  inline polling would buy nothing. Trade-off documented on the helper:
  a handler doing heavy synchronous work before its first await now
  runs that work head-of-line on the service's bridge task. Tests:
  `server_fold_sync_handler_emits_response_inline_without_yield`,
  `server_fold_pending_handler_is_spawned_and_completes_after_release`,
  `server_fold_sync_panic_is_caught_on_inline_path`.

### 2. T2.1 — drop the fold mutexes

- **What:** `RedexFold::apply(&mut self)` forces every fold behind a
  `Mutex`:
  - client side: the inbound dispatcher does
    `fold.lock().apply_inbound(&ev)` per RESPONSE
    (`mesh_rpc.rs:3740`);
  - server side: each REQUEST crosses 2–4 `in_flight.lock()` regions
    (`cortex/rpc.rs:1564, 1584, 1702, 1707`), and the
    `in_flight: Arc<Mutex<HashMap>>` pattern is duplicated across all
    four fold variants (`cortex/rpc.rs:1396, 2044, 2570, 2975`).
- **Fix:** Make `apply` take `&self` (or interior-mutability shim);
  replace `in_flight` with `DashMap<(u64, u64), RpcCancellationToken>`.
- **Payoff (corrected):** ~~matters at c128 where every response for a
  service serializes through one fold lock~~ — disproven by the
  scaling plan's shard test (the bridge is a single task per service,
  so the locks were uncontended; the ceiling is upstream). Real payoff:
  removes 3–5 uncontended lock crossings from the c1 path, and it is a
  **prerequisite for #1** — inline-polling the handler under the old
  `fold.lock()` would have held the fold mutex across user code.
- **Status: landed (2026-06-13), all four server folds + client fold.**
  Done WITHOUT touching the `RedexFold` trait (the wide-blast-radius
  churn the scaling plan warned against): each fold gains an inherent
  `apply_shared(&self)` carrying the real logic; the trait impl
  delegates, so `CortexAdapter`/test drivers still work. `in_flight`,
  the streaming fold's `flow_control`, and the chunk `senders` maps are
  all `DashMap` now; duplicate-REQUEST check + insert is a single
  atomic `entry()` op (shard guard provably dropped before the inline
  poll — holding it across the handler's self-clean `remove` would
  self-deadlock). `RpcClientFold::apply_inbound` takes `&self`, so the
  per-reply-channel dispatcher (`mesh_rpc.rs`) and all four bridge
  tasks drive their folds with no `Arc<Mutex<...>>` wrapper.

### 3. De-serialize the per-service response drain (c128 ceiling) — ⚠ demoted

- **What:** §8a funnels all responses for a service through a single
  bounded-mpsc drain task that does AEAD encrypt + `send_to`
  sequentially. 114 K QPS ≈ 8.7 µs/response — approximately that
  loop's per-item cost.
- **Demoted (2026-06-13):** the scaling plan's Phase 0 verdict places
  the ceiling in the **shared recv loop**, upstream of the drain — the
  same evidence that disproved the fold mutex (sharding channels, which
  also shards drain tasks, made throughput worse). Parallelizing the
  drain alone is unlikely to move the ceiling until the recv-loop
  packet count drops (ack-piggyback) or the recv syscall batches.
  Revisit only after item 4 / the ack-piggyback plan lands and a
  re-bench shows the drain as the new wall.
- **Fix options (if revisited):** (a) drain with a small worker pool;
  (b) drain the whole queue per wake and emit batched — pairs
  naturally with item 4.

### 4. Syscall batching below nRPC — now the primary throughput lever

- **What:** 2 UDP syscalls per leg, ~5–10 µs of the 42.5 µs c1 budget —
  and per the scaling plan, a reliable unary RPC is actually
  **4 packets** (REQUEST + RESPONSE + a StreamWindow grant in each
  direction, because the grant is the sole ACK), all funneling through
  one single-syscall recv loop per node.
- **Fix:** two complementary efforts, both already designed/scoped:
  - **Ack-piggyback wire change** — carry `ack_seq` on data packets so
    unary needs no standalone grant (4 packets → 2). Design-complete
    in `../plans/NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md`.
  - **Batched receive** — wire the existing `BatchedPacketReceiver`
    (recvmmsg) into `spawn_receive_loop` on Linux; RIO (or at minimum
    multi-frame coalescing per `send_to`) on Windows.
- **Payoff:** the only path meaningfully below ~20 µs single-flight,
  AND — post-correction — the only credible path past the ~90–115 K
  QPS ceiling.
- **Scope note:** transport-layer project, benefits everything on the
  mesh, not nRPC-local. Track separately from nRPC work.

### 5. Tier-3 leftovers (cumulative ~1–2 µs)

- **T3.4** — feature-gate `catch_unwind` (`cortex/rpc.rs:1634`),
  ~100–300 ns on the success path. Document the trade-off (handler
  panic takes down the bridge for that service when disabled).
- **T3.5** — pool `RpcCancellationToken` (`cortex/rpc.rs:1583`); one
  `Arc<Notify>` alloc per call today.
- **T3.2** — make the `ensure_reply_subscription` hit path fully sync.
  Less urgent now that §3.5's DashMap probe is cheap, but every call
  still builds an async state machine and re-hashes the service name
  (`mesh_rpc.rs:3693`). Better shape: carry the "already subscribed"
  state inside the cached `Arc<RpcRoute>` so the hot path is
  zero-lookup.
- **T3.3** — `from_node` double-resolution in `process_local_packet`.
  Partially obsoleted by the T1.1 drainer — verify it still fires
  before doing it.

### 6. Small caller-path alloc trims (opportunistic)

- Request buffer is a fresh `Vec` per call whose capacity hint
  (`EVENT_META_SIZE + body.len() + 32`, `mesh_rpc.rs:3416`) ignores
  header bytes — calls carrying trace/predicate headers realloc once.
  Either size it from `req.encoded_len()` or pool a `BytesMut`.
- `RpcReply.headers: Vec<(String, Vec<u8>)>` allocates per header on
  decode.

### Deferred (unchanged from the original audit)

- **T4.2** — shrink the per-frame AAD. Sub-microsecond headroom by the
  same reasoning that dropped T4.1; revisit only if a protocol
  revision touches the frame layout anyway.

## Measured outcome (2026-06-13, quick read)

A/B on this branch, same host (Win11, 24 logical cores) and settings
(criterion `--warm-up-time 1 --measurement-time 3 --sample-size 20`;
diagnostic quality, not the published-numbers protocol). Baseline =
the exact pre-change tree (changes stashed), comparison = #1 + #2
landed:

| bench | pre-change | with #1 + #2 | Δ |
|---|---:|---:|---|
| c1/32B | 34.31 µs | **33.73 µs** | **−1.6 %** (p = 0.00) |
| c16/32B | 134.1 µs (119.3 K/s) | 134.1 µs (119.4 K/s) | none (p = 0.82) |
| c128/32B | not measurable | not measurable | — |

Exactly as the course-corrected prediction said: a real but small
**c1-latency** trim (~0.6 µs ≈ one spawn+wake + a few uncontended lock
crossings) and **no movement at c16** — the ceiling is the recv loop,
which these items never targeted. c128 panics the bench harness's
saturation guard (`nrpc_common/mod.rs:91`, "this bar is not measurable
here — don't chase it") on this host **both with and without the
change** — pre-existing, verified by running the pre-change tree.

Note the absolute c1 number (≈34 µs) is already below the May 26
re-bench's 42.5 µs; that gap pre-dates this change (other work landed
on this branch / different quick-read settings) and is NOT attributable
to #1/#2.

## Suggested order (updated 2026-06-13 after landing #1/#2)

1. ~~**T2.3** (inline-ready dispatch) and **T2.1** (fold mutexes)~~ —
   **done** (see Status notes in items #1/#2). Tests: 78 cortex-rpc
   unit tests (3 new), 36 nRPC integration tests, SDK mesh_rpc suites
   + backpressure, full lib suite (4310) all green.
2. **Re-bench `nrpc_qps`** (`cargo bench --bench nrpc_qps`, sdk crate).
   Post-correction the honest expectation is a **c1-latency** trim
   (one spawn+wake + a handful of lock crossings, ~1–3 µs of 42.5),
   with the c16/c128 ceiling roughly unchanged — the ceiling is the
   recv loop, not dispatch. Treat the streaming benches
   (`nrpc_streaming.rs`) and `c128/16KiB` as the regression tripwires.
3. If the measured delta diverges from prediction, take a `samply`
   flamegraph before the next change — this discipline served the
   Tier 1 work well (it's how the threshold-coalesce deadlock and the
   drainer redesign were caught early).
4. **The ack-piggyback protocol plan + item 4** (batched receive) are
   the throughput track — that's where the c16/c128 ceiling actually
   moves. Item 3 (drain parallelism) only re-enters if a post-item-4
   re-bench names the drain as the new wall.

## Source-of-truth file paths

- `net/crates/net/src/adapter/net/cortex/rpc.rs` — fold `apply` +
  handler spawn (line 1606), `in_flight` mutexes (1396/2044/2570/2975),
  `catch_unwind` (1634), `RpcCancellationToken::new` (1583)
- `net/crates/net/src/adapter/net/mesh_rpc.rs` — `MeshNode::call`
  (line 3312), `ensure_reply_subscription` (3677), client fold
  dispatcher (3740), `publish_response_to_caller` (1738),
  `mint_random_call_id` (3836)
- `net/crates/net/src/adapter/net/mesh.rs` — `RpcRoute` cache,
  `spawn_stream_grant_drainer_loop` (5447), reply-subscription
  registry (1576)
- `net/crates/net/sdk/benches/` — `nrpc_qps`, `nrpc_churn`,
  `nrpc_common`
