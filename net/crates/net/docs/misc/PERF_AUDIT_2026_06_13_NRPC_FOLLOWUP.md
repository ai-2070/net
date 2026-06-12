# nRPC perf follow-up (2026-06-13)

Successor to `PERF_AUDIT_2026_05_19_NRPC.md`. That audit's Tier 1 landed
and was re-benched on 2026-05-26; this doc records where the budget
stands now, which of the original items have since landed, and ranks
the remaining opportunities. No new benchmark run was taken for this
doc — numbers cited are from the May 26 re-bench.

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
- **Payoff:** modest at c1; matters at c128 where every response for a
  service serializes through one fold lock.

### 3. De-serialize the per-service response drain (c128 ceiling)

- **What:** §8a funnels all responses for a service through a single
  bounded-mpsc drain task that does AEAD encrypt + `send_to`
  sequentially. 114 K QPS ≈ 8.7 µs/response — approximately that
  loop's per-item cost, i.e. the drain task is now the throughput
  ceiling.
- **Fix options:** (a) drain with a small worker pool; (b) drain the
  whole queue per wake and emit batched — pairs naturally with item 4.
- **Payoff:** lifts the c128 plateau toward the original ~150 K QPS
  target. Negligible effect on c1.

### 4. Syscall batching below nRPC

- **What:** 2 UDP syscalls per leg, ~5–10 µs of the 42.5 µs c1 budget.
- **Fix:** `sendmmsg`/`recvmmsg` + GSO/GRO on Linux; RIO (or at
  minimum multi-frame coalescing per `send_to`) on Windows.
- **Payoff:** the only path meaningfully below ~20 µs single-flight.
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

## Suggested order

1. **T2.3** (inline-ready dispatch) and **T2.1** (fold mutexes) — the
   two remaining audit items aimed at the wakeup signal.
2. **Re-bench `nrpc_qps`** (`cargo bench --bench nrpc_qps --features
   "net cortex"`). Original prediction for Tier 1 + Tier 2:
   c1/32B ≈ 35–45 µs is already met, so the refined target here is
   **c1/32B ≈ 30–35 µs, c128 ≥ 150 K QPS**.
3. If the measured delta diverges from prediction, take a `samply`
   flamegraph before the next change — this discipline served the
   Tier 1 work well (it's how the threshold-coalesce deadlock and the
   drainer redesign were caught early).
4. **Item 3** (response-drain parallelism) if c128 throughput is the
   priority after step 2.
5. **Item 4** (syscall batching) as a separate transport-layer track
   if sub-20 µs single-flight becomes a goal.

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
