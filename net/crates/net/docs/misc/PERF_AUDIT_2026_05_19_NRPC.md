# nRPC unary perf audit (2026-05-19)

Source data: `cargo bench --bench nrpc_qps --features "net cortex"` on the
post-merge `master` (tip `d873d52f`, post-`nrpc-streaming` merge).

| Configuration | Time      | Throughput          |
|---------------|-----------|---------------------|
| c1/32B        | ~69.6 µs  | ~14.4 K elem/s      |
| c1/1KiB       | ~71.9 µs  | ~13.9 K elem/s      |
| c16/32B       | ~300 µs   | ~53.3 K elem/s      |
| c16/1KiB      | ~312 µs   | ~51.2 K elem/s      |
| c128/32B      | ~1.84 ms  | ~69.7 K elem/s      |

**Headline:** payload size barely moves the needle (32 B ≈ 1 KiB), so the
pipeline is **CPU-overhead-bound, not bandwidth-bound**. Plateau at
~70 K QPS under c128 means per-call constant cost dominates batching.

**Target:** a well-tuned RPC over loopback UDP with AEAD should hit
~20–30 µs single-flight and ~150–200 K QPS at high concurrency. The audit
below ranks the changes that get us there.

The findings here are the synthesis of three parallel reviews — caller
path, transport/AEAD, server fold dispatch — each tracing its slice and
returning a ranked list with `file:line` refs. The reviews independently
identified overlapping costs, which increases confidence in the ranking
below.

The conclusions are further backed by **layer-baseline decomposition**
(see next section) using the lower-layer microbenches in
`benches/{net,cortex,mesh}.rs`, which establish the physics floor below
nRPC. Per Kyra's architectural framing (ring buffers → RedEX → CortEX →
nRPC), nRPC sits at the top of a stack, and most of the 70 µs lives
*below* nRPC's own code. The baselines pin where, and let us rule out
several initial suspects (notably the AEAD layer).

## Layer-baseline decomposition

Numbers from `benchmarks/BENCHMARK_RESULTS_14900K.md` (Intel 14900K, the
project's reference machine). Each row is the floor cost of the named
layer in isolation; the per-RT column scales it by the number of times
that op runs in a single unary RPC.

| Layer / op | Cost per op | Times per RT | Per-RT cost |
|---|---|---|---|
| Packet header ser/de | 1.2 ns | ~4 | ~5 ns |
| Event frame write_single (64 B) | 35.6 ns | 2 | ~70 ns |
| Event frame read_batch_10 (per event) | ~16 ns | 2 | ~30 ns |
| Packet pool get/return | 53 ns | ~4 | ~200 ns |
| Auth guard check_fast hit | 24.6 ns | ~2 | ~50 ns |
| **Build packet (1 event, w/ AEAD encrypt)** | **1.14 µs** | 2 legs | **~2.3 µs** |
| ChaCha20-Poly1305 decrypt (64 B) | ~1.14 µs (symmetric) | 2 legs | ~2.3 µs |
| CortEX ingest (tasks_create) | 192 ns | 1–2 | ~0.4 µs |
| CortEX fold_barrier (write→wait) | 1.85 µs | up to 1 | ≤1.8 µs |

**Physics floor for transport + crypto + framing: ~5 µs per RT.**

**Remaining ~65 µs (93% of the c1/32 B budget) lives above transport**
— in dispatch, scheduling, the `tokio::spawn` storm, the RedEX
publish→subscribe wake path, the mpsc bridge hops, and the nRPC layer's
own allocation + encoding overhead. Rough envelope of where the 65 µs
goes (to be refined by flamegraph if needed):

- UDP `send_to` / `recv_from` syscalls — ~5–10 µs (2 syscalls per leg
  on Windows, ~1–3 µs each including kernel transition)
- Tokio scheduling + spawns — ~10–20 µs (4–6 spawns per RT × ~2 µs)
- RedEX dispatcher → subscriber wake — ~5–10 µs (mpsc wake latency)
- nRPC encode/decode + alloc churn — ~10–15 µs (3 memcpys + ~8 allocs)
- Mesh routing — ~5–10 µs (peer lookup, `publish_to_peer` setup,
  response-leg roster fan-out)
- Bookkeeping — ~5 µs (metrics, observer fire, `CallMetricsGuard`)

### Cipher comparison data — AEAD is NOT the bottleneck

The `cipher_comparison` bench at every payload size:

| Payload | ChaCha20 (current `shared_pool` path) | `fast_chacha20` (unwrapped baseline) |
|---|---|---|
| 64 B | 1.12 µs | 1.14 µs |
| 256 B | 1.20 µs | 1.20 µs |
| 1 KiB | 1.58 µs | 1.55 µs |
| 4 KiB | 3.08 µs | 3.02 µs |

The two paths are statistically identical. ChaCha20 is already near-
optimal on this CPU and even switching to AES-GCM-NI could shave at most
~1–2 µs off the round-trip — well below the ~65 µs of dispatch
overhead. **T4.1 is dropped from the plan** (see Tier 4 below).

### Why the 32 B-vs-1 KiB flat curve

64 B encrypt = 1.14 µs; 1 KiB encrypt = 1.58 µs. A 16× payload bump
only adds 0.44 µs to the AEAD. At 32 B vs 1 KiB, that's under 0.5 µs
of the 70 µs budget — well within bench noise. The flat curve doesn't
say "AEAD is heavy"; it says **AEAD is so light that payload scaling
disappears under everything else**.

## What the audit found

Three cost categories dominate the per-call overhead:

1. **Spawn-storm.** Each round-trip currently does **4–6 `tokio::spawn`
   calls**, each ~1–3 µs:
   - 2× StreamWindow credit-grant spawns (one per inbound data packet
     on each leg — `mesh.rs:3987, 4212`)
   - 1× server-side handler spawn (`cortex/rpc.rs:1535`)
   - 1× server-side response-emit spawn (`mesh_rpc.rs:1530–1562`)
   - the existing dispatch-bridge spawn

   At 1–3 µs each, that's **5–15 µs of pure scheduling overhead per
   call**, before any user code runs.

2. **Allocation pressure.** ~8–10 avoidable allocations per call. The
   request body alone makes three memcpy passes:
   `payload.to_vec()` → `RpcRequestPayload::body` field →
   `encode()` `extend_from_slice` → outer-buf `extend_from_slice` →
   `Bytes::from(buf)`. Two `format!()` plus two `ChannelName::new()`
   per call derive channel names that are **constant for the lifetime
   of `(service, caller_origin)`**, but get rebuilt every call.

3. **Asymmetric send legs.** The request leg uses
   `MeshNode::publish_to_peer` (direct addressing, known target). The
   response leg uses `Mesh::publish` (roster fan-out, ACL check, subnet
   filter, per-peer `Vec<Bytes>` allocation). Both legs already know
   the peer node id — the response leg is paying 3–8 µs of extra work
   for nothing.

## Ranked plan

### Tier 1 — easy + big payoff (target: ~10–15 µs total)

#### T1.1 — Coalesce StreamWindow grants

- **What:** The receive path currently spawns one credit-grant publish
  per inbound data packet (`mesh.rs:3987` calls
  `spawn_stream_window_grant` at `mesh.rs:4212`), which AEAD-encrypts a
  control frame and `send_to`s it. Unary RPC pays this on both legs.
- **Cost today:** 4–12 µs / round-trip, plus 4 spawns + 2 extra UDP
  send_to + 2 extra AEAD encrypts.
- **Fix:** Batch / coalesce on a timer or threshold; skip entirely
  when `consumed < threshold`. `on_bytes_consumed` returns `Some`
  eagerly today.
- **Difficulty:** easy.
- **Verification needed:** confirm the unary path actually traverses
  this — it's a streaming-flow-control mechanism, and it would be
  surprising (though not impossible) if a unary REQUEST/RESPONSE rides
  through it. **This is the single biggest claimed win and the most
  surprising — verify before implementing.**

#### T1.2 — Response leg → `publish_to_peer` direct

- **What:** Both server-side response sites build a `ChannelPublisher`
  and call `Mesh::publish`, which runs `roster.dispatch_recipients` +
  subnet filter + `auth_guard.check_fast` + `peer_subnets.get` per
  recipient, allocates `events_owned: Vec<Bytes> = events.to_vec()`,
  then forwards to `publish_to_peer` anyway.
- **Cost today:** 3–8 µs / round-trip, 1 Vec alloc, N DashMap shard
  acquires.
- **Fix:** The response site already knows `caller_origin`; resolve
  once to `node_id` and call `publish_to_peer` directly, mirroring the
  request path.
- **Where:** `mesh_rpc.rs:1557` (unary response emit), `:1665`
  (streaming response emit).
- **Difficulty:** easy–medium.

#### T1.3 — Cache per-`(service, caller_origin)` route

- **What:** `format!("{service}.requests")` +
  `ChannelName::new(...)` + `format!("{service}.replies.{self_origin:016x}")` +
  second `ChannelName::new(...)` per call, then
  `ChannelId::new(request_channel.clone())` re-hashes the same name,
  plus `request_channel.clone()` happens three more times further down.
- **Cost today:** ~5 allocs/call, ~2 µs.
- **Fix:** Cache `(request_channel_id, request_channel_hash,
  request_stream_id, reply_channel, reply_hash)` in a
  `DashMap<&str, Arc<RpcRoute>>` on `MeshNode`. First call builds,
  subsequent calls clone the `Arc`. Pairs naturally with the existing
  `ensure_reply_subscription` registry (same key).
- **Where:** `mesh_rpc.rs:2461–2473` (the cold building), call-site
  refs at `:2487`, `:2560`, `:2615`, `:2775`.
- **Difficulty:** low–medium.
- **Status:** **landed** — `mesh.rs::RpcRoute` + `MeshNode::rpc_route_for_service`
  cover the unary, streaming, client-stream, and duplex caller paths.
  Cache is soft-capped at `RPC_ROUTE_CACHE_SOFT_CAP = 256` so callers
  passing high-cardinality `service: &str` can't grow the map without
  bound; insert path uses `entry().or_insert_with` so the build is
  race-free. See `route_cache_tests` in `mesh.rs` for the pinned
  invariants.
- **Follow-up (landed alongside T1.3):** `ChannelName(String)` →
  `ChannelName(Arc<str>)` (`channel/name.rs`). Pre-fix `Clone` was a
  heap alloc + memcpy, so even with the route cache the guards
  (`RpcStream` / `UnaryCallGuard` / `ClientStreamCallRaw` /
  `DuplexInner`) still allocated each time they took ownership of a
  cached `ChannelName`. With `Arc<str>` the per-call hand-off is a
  refcount bump and the cached-hit path is allocation-free for both
  the request and reply channel names. The change is contained in
  `channel/name.rs` (one field-type swap + one `Pattern` deref nudge
  in `is_prefix_of`); no public API or callsite changes were
  required since all consumers use `as_str()` or operate on the
  inner via `Deref<Target=str>`. Payoff is wider than nRPC since
  `ChannelName` is also cloned by `ChannelId` propagation through
  the pub/sub fan-out paths and `ChannelConfigMap` lookups.

### Tier 2 — medium refactors (target: ~5–10 µs)

#### T2.1 — Drop the fold `Mutex`, switch `in_flight` to `DashMap`

- **What:** Inbound REQUEST currently crosses **four mutex regions**:
  outer `Arc<Mutex<RpcServerFold>>` lock (`mesh_rpc.rs:1569, 1596`),
  three `in_flight.lock()` calls inside `apply` (`cortex/rpc.rs:1495,
  1513, 1629`).
- **Cost today:** ~20–50 ns/lock uncontended; much worse under
  contention. Cumulative 4 acquires + their cache-line bouncing.
- **Fix:** The fold's `&mut self` is only needed for `test_now_ns`;
  the real state is already `Arc`/internally synced. Make
  `apply(&self, ...)` and drop the outer mutex. Replace
  `in_flight: Arc<Mutex<HashMap>>` with
  `DashMap<(u64, u64), RpcCancellationToken>` — one lock-free insert +
  one lock-free remove.
- **Difficulty:** medium (touches `RedexFold` signature, or uses
  interior mutability).
- **Semantics:** unchanged.

#### T2.2 — `RpcRequestPayload::body: Vec<u8>` → `Bytes`

- **What:** Caller's `Bytes` payload gets `to_vec()`'d into the
  struct, then `encode()` does `extend_from_slice` onto it, then the
  outer wrapper does another `extend_from_slice` into `buf`, then
  `Bytes::from(buf)` consumes it. **3 mem copies + 3 allocations** for
  a body the caller already had as `Bytes`. On the server side
  `decode` does another `to_vec()` for the body and a
  `service.to_string()` that nobody reads.
- **Cost today:** ~3 allocs/call, ~5–10 µs at 1 KiB (smaller at 32 B
  but the alloc/free is still ~200 ns each).
- **Fix:** Change `RpcRequestPayload::body` (and `RpcResponsePayload::body`)
  to `Bytes`. Add `encode_into(&mut BytesMut)` that writes meta +
  header fields then appends `body` as a slice/chained `Bytes`. Add a
  `decode_for_server()` shape that returns
  `(deadline_ns, flags, headers, body: Bytes)` and skips
  `service` decoding entirely on the server (the fold already has the
  service bound at `serve_rpc` time — the wire `service` is never
  read).
- **Difficulty:** medium (touches codec + every encode/decode call
  site).
- **Semantics:** wire format unchanged.

#### T2.3 — Inline handler dispatch when handler future is Ready

- **What:** `tokio::spawn(handler.call(ctx, payload))` even when the
  handler is a synchronous closure that returns `Ready` on first poll.
  For benchmark echo handlers this is pure spawn overhead.
- **Cost today:** ~1–3 µs / call.
- **Fix:** Poll the future once before spawning; if `Ready`, take
  the result inline. Or expose a marker trait / `call_blocking()`
  fast path.
- **Where:** `cortex/rpc.rs:1535`.
- **Difficulty:** medium.
- **Semantics:** changes panic-recovery scope unless the inline path
  keeps `catch_unwind`. Currently `catch_unwind` adds ~100–300 ns on
  the success path — see T3.4.

### Tier 3 — quick cleanups (target: ~2–3 µs)

#### T3.1 — `ensure_reply_subscription` linear `Vec` scan under mutex

- **What:** `entries.iter().any(|(t, s)| *t == target_node_id && s == service)`
  is O(N) per call under `parking_lot::Mutex<Vec<(u64, String)>>`. At
  small N this is sub-microsecond, but it scales worse and forces a
  lock on the cached-hit path.
- **Where:** `mesh_rpc.rs:2747–2770`; field at `mesh.rs:1221`.
- **Fix:** Replace with `DashMap<(u64, u64 service-hash), ()>` keyed on
  the cached `reply_hash + target_node_id` (computed once via T1.3).
  Cached hit becomes one lock-free `contains_key`.
- **Difficulty:** low.

#### T3.2 — Make `ensure_reply_subscription` synchronous on cached hit

- **What:** Even on the cached-hit fast path the call `.await`s the
  function, which yields once and forces the future to be `!Unpin`
  heap-managed.
- **Where:** `mesh_rpc.rs:2486–2489`.
- **Fix:** Split into `fast_path_check(...) -> Option<()>` (sync
  DashMap probe, see T3.1) and `register_slow_path(...) -> impl Future`.
  `call` does the sync probe first, only awaits when missing.
- **Difficulty:** low.

#### T3.3 — Resolve `from_node` once in `process_local_packet`

- **What:** Per inbound data packet, `addr_to_node.get` + `peers.get`
  is called for the grant target, then again for `from_node`.
  Five DashMap shard-lock acquires per inbound data packet,
  ~50–150 ns each.
- **Cost today:** 0.3–0.8 µs / call. Three of the five disappear with
  T1.1.
- **Fix:** Resolve once at the top of `process_local_packet` and pass
  the resolved `from_node` down.
- **Difficulty:** easy.

#### T3.4 — Gate `catch_unwind` on a feature flag (or skip when handler is UnwindSafe)

- **What:** Every handler invocation wraps in
  `AssertUnwindSafe + catch_unwind` for panic recovery. On the
  success path this is ~100–300 ns of state-machine overhead with
  zero allocations.
- **Where:** `cortex/rpc.rs:1563`.
- **Fix:** Gate behind `rpc-catch-panics` feature (default on in
  release, off in microbench profiles), or skip when the handler is
  marked `UnwindSafe`.
- **Difficulty:** easy.
- **Semantics:** handler panics would unwind through the bridge
  task, taking down the bridge for that service. Acceptable in
  trusted in-proc deployments; document the trade-off.

#### T3.5 — `RpcCancellationToken` pooling

- **What:** Per call allocates an `Arc<RpcCancellationInner>` holding
  a `tokio::sync::Notify`. ~150–250 ns + ~100 B.
- **Where:** `cortex/rpc.rs:1158`.
- **Fix:** Pool tokens with a per-fold `crossbeam::queue::ArrayQueue`;
  reset (`fired` back to false, no waiters) and reuse.
- **Difficulty:** medium.
- **Semantics:** unchanged if reset drops stale waiters cleanly.

#### T3.6 — `decrypt_in_place` on receive path

- **What:** `rx_cipher.decrypt()` allocates a `Vec<u8>` for plaintext
  (`crypto.rs:677–689` uses `Aead::decrypt`, not `decrypt_in_place`)
  even though the in-place variant exists at `crypto.rs:695`.
- **Cost today:** ~2 allocs/round-trip, 0.5–1 µs.
- **Where:** `mesh.rs:3315`.
- **Fix:** Switch receive to `decrypt_in_place` and reuse the inbound
  `BytesMut`. The buffer is owned by `PacketReceiver`; needs ownership
  shuffle.
- **Difficulty:** medium.

### Tier 4 — wire/protocol changes (deferred)

#### T4.1 — AES-GCM-NI cipher option *(DROPPED)*

**Initial hypothesis:** the 32 B-vs-1 KiB flat curve suggested
ChaCha20-Poly1305 keystream init was wasting cycles on short payloads,
and feature-gating AES-GCM-NI on AES-capable hosts would be 3–5× faster.

**Why we dropped it:** the `cipher_comparison` bench at
`benches/net.rs:305` directly measures this and shows the ChaCha20 path
is already near-optimal on the reference 14900K — `shared_pool` and
`fast_chacha20` are statistically identical at every payload size. The
AEAD only accounts for ~2.3 µs of the 70 µs RT budget (under 4%); even
a hypothetical 3× cipher speedup buys at most ~1.5 µs. Not worth a wire
format negotiation. The flat curve isn't telling us AEAD is heavy —
it's telling us AEAD is so light that payload scaling vanishes under
everything else.

#### T4.2 — Shrink the per-frame AAD

64-byte fixed header is included as AAD on every frame (`pool.rs:264
header.aad()`), so Poly1305 absorbs header + payload (~122 B / ~2
blocks). Shrinking to a 16 B sub-header roughly halves Poly1305 work.
Wire-format change.

By the same logic as T4.1, the headroom here is also small — Poly1305
on 2 blocks vs 1 block at this payload size is sub-microsecond. Keep
deferred unless a future protocol revision touches the frame layout
for unrelated reasons.

## Non-findings (cleared by the audit)

These were initial suspicions that did not pan out:

- **`reliable=true` blocking on ACK.** Confirmed at
  `mesh.rs:6681–6685`: `publish_to_peer` returns immediately after
  `socket.send_to`. The flag only opens the stream in reliable mode
  and stamps `PacketFlags::RELIABLE`; no retransmit queue is touched
  on the send path. Reliability is currently best-effort on the wire.
  Not a current cost.
- **MTU padding / framing tax.** `MAX_PACKET_SIZE=8192`,
  `MAX_PAYLOAD_SIZE=8112` (`protocol.rs:24–27`). A 32 B RPC sends
  ~138 B on the wire — no padding to MTU.
- **Heartbeat interaction.** Heartbeats run on a 5 s timer
  (`mesh.rs:705, 4254`). No call-path interaction.
- **Per-call clones inside `apply`.** Per the
  `bug-audit-2026-05-18/PHASE3_CORTEX_RPC_DROP.md` review (line 86–90),
  the spawn-prologue clones are `Arc` refcount bumps, not deep copies.
  Verified — not a hot-path concern.

## Items flagged uncertain (verify before / during implementation)

- **StreamWindow grant cost on unary RPC** (T1.1). Single biggest
  claimed win, but it's a flow-control mechanism intended for the
  streaming path. Verify it actually fires on unary RPC before
  building a coalescer. Use a quick `samply` or `printf` probe before
  cutting code.
- ~~**AEAD vs syscall vs scheduling share.**~~ **RESOLVED** by the
  cipher_comparison data above — AEAD is ~5% of the budget, not the
  bottleneck. The remaining 65 µs is in scheduling + dispatch + nRPC
  layer.
- **Inbound bridge mpsc wake latency.** Could be 1 µs (same-thread
  hand-off) or 5 µs (cross-worker wake) depending on tokio runtime
  scheduler state. Localhost benchmarks under criterion may park
  workers between iters, biasing the result. Flamegraph would
  disambiguate if a measured Tier 1 delta diverges from prediction.

## Expected outcome

If Tier 1 lands as written: **c1/32B ≈ 50–55 µs** (vs 70 µs today),
**c128 ceiling lifted** roughly proportionally (the spawn-storm fix
scales with concurrency).

If Tier 1 + Tier 2 land: **c1/32B ≈ 35–45 µs**, **c128 in the
~150 K QPS range** — normal gRPC-loopback territory.

Tier 3 individually is small but cumulative — adds another ~2–3 µs
once the bigger wins are in.

## Recommended implementation order

The layer baselines (above) make the flamegraph step optional rather
than mandatory — we already know the AEAD/transport floor is ~5 µs and
the remaining ~65 µs is in dispatch/scheduling/nRPC. The flamegraph
would refine the breakdown of that 65 µs but isn't required to start
implementing the top-ranked wins.

1. **Tier 1.2** (response → `publish_to_peer` direct). Confirmed
   path, contained change, 3–8 µs win, three commits possible
   (unary, server-streaming, client-streaming reply emits).
2. **Tier 1.3** (per-service route cache). Independent of #1,
   ~2 µs + 5 allocs. Easy diff.
3. **Re-bench `nrpc_qps`.** Measure the c1/32 B + c128/32 B deltas
   so far.
4. **Tier 1.1** (StreamWindow coalesce). The biggest claimed single
   win but the most architecturally invasive. The layer baselines
   confirm the spawn-storm magnitude (4–6 spawns × ~2 µs ≈ 10–15 µs)
   matches the size of the gap that #1+#2 won't close, so this is
   the natural follow-up. Verify on the way in that the unary RPC
   path actually traverses the StreamWindow grant path — that's the
   one structural unknown remaining.
5. **Re-bench.** Decide whether Tier 2 is needed based on whether
   c1/32 B has reached the 30–35 µs target.

If at any point the deltas don't match the audit's predictions, drop
in a `samply` flamegraph run before the next change to pin attribution
on actual data.

Each Tier 1 item is independent and lands as one commit. Tier 2 items
are larger refactors and benefit from individual commits and re-benches.

## Source-of-truth file paths

- `net/crates/net/src/adapter/net/mesh_rpc.rs` — `MeshNode::call`
  (line 2445), `ensure_reply_subscription` (line 2747), response-emit
  closures (lines 1530, 1665)
- `net/crates/net/src/adapter/net/cortex/rpc.rs` — `RpcServerFold`
  (apply at line 1446), `RpcRequestPayload::encode/decode` (line 450 /
  484), `RpcCancellationToken::new` (line 1158)
- `net/crates/net/src/adapter/net/mesh.rs` — `publish_to_peer`
  (line 6570), `process_local_packet` (line 3297),
  `spawn_stream_window_grant` (line 4212), reply-subscriptions
  registry field (line 1221), `fire_rpc_observer_outbound`
  (line 4803)
- `net/crates/net/src/adapter/net/pool.rs` — `build_subprotocol`
  (line 231)
- `net/crates/net/src/adapter/net/crypto.rs` — AEAD path (lines
  578, 677, 695, 728)
- `net/crates/net/src/adapter/net/channel/name.rs` —
  `ChannelName::new` (line 47, always allocates via `to_string`)
- `net/crates/net/src/adapter/net/protocol.rs` — frame layout
  constants (lines 24–27)
